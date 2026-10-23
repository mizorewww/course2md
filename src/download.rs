//! Resumable, verified downloads of single model files.
//!
//! Each host from [`crate::hub::endpoints`] is tried in turn. aria2 fetches
//! over several connections when it is installed (bundled beside the
//! executable or on PATH); otherwise a built-in client resumes with HTTP
//! ranges. Either way a host that stops sending data is abandoned within a
//! minute, and the bytes must match the published SHA-256 before they replace
//! anything in the model directory.

use anyhow::{Context, Result};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::hub::Host;
use crate::progress::{Bar, Phase, Stage};

/// A file published in a model repository, with its size and SHA-256.
pub struct Remote {
    pub repo: &'static str,
    pub file: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// A host that sends nothing for this long is treated as stalled.
const STALL: Duration = Duration::from_secs(60);
/// Built-in attempts on one host that may fail without progress.
const IDLE_ATTEMPTS: u32 = 3;

/// Deterministic HTTP failures (4xx other than 429): the host lacks the file.
#[derive(Debug)]
struct PermanentHttp(String);

impl std::fmt::Display for PermanentHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PermanentHttp {}

fn is_permanent_status(code: u16) -> bool {
    code != 429 && (400..500).contains(&code)
}

/// aria2 could not run at all, as opposed to failing a transfer.
#[derive(Debug)]
struct Aria2Unavailable(String);

impl std::fmt::Display for Aria2Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Aria2Unavailable {}

fn cancelled() -> bool {
    crate::dispatch::check_control().is_err()
}

/// Downloads `remote` to `dest` unless a complete copy is already there.
/// `.part` keeps the bytes received so far, so a later run resumes.
pub fn fetch(remote: &Remote, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension("part");
    let stage = Stage::ModelFile(remote.file.to_owned());
    crate::progress::start(stage.clone());
    let bar = Bar::new(stage.clone(), remote.size)
        .with_template("{spinner:.green} {msg} [{bar:32.cyan/blue}] {bytes}/{total_bytes} ({eta})");
    let mut failures = Vec::new();
    for host in &crate::hub::endpoints().hosts {
        crate::dispatch::check_control()?;
        let url = host.file_url(remote.repo, remote.file);
        match transfer(&url, host, &part, remote, &bar) {
            Ok(()) => {}
            Err(error) => {
                if cancelled() {
                    return Err(error);
                }
                tracing::warn!(
                    file = remote.file,
                    host = %host.name(),
                    "模型下载失败，换下一个地址 / Model download failed; trying the next host: {error:#}"
                );
                failures.push(format!("{}: {error:#}", host.name()));
                continue;
            }
        }
        if verify(&part, remote, &bar)? {
            fs::rename(&part, dest)?;
            write_manifest(dest, remote);
            bar.finish();
            crate::progress::done(stage);
            tracing::info!(file = remote.file, host = %host.name(), "downloaded");
            return Ok(());
        }
        let _ = fs::remove_file(&part);
        failures.push(format!(
            "{}: {}",
            host.name(),
            crate::tr!("文件校验不符 / Checksum mismatch")
        ));
    }
    bar.finish();
    anyhow::bail!(
        "{}\n{}",
        crate::tr!("所有下载地址均失败 / Every download host failed"),
        failures.join("\n")
    )
}

fn transfer(url: &str, host: &Host, part: &Path, remote: &Remote, bar: &Bar) -> Result<()> {
    if let Some(program) = aria2_program() {
        match aria2(&program, url, host, part, remote, bar) {
            Err(error) if error.downcast_ref::<Aria2Unavailable>().is_some() => {
                tracing::warn!(
                    "aria2 无法使用，改用内置下载 / aria2 unavailable; using the built-in downloader: {error:#}"
                );
                ARIA2_BROKEN.store(true, Ordering::Relaxed);
            }
            result => return result,
        }
    }
    native(url, host, part, remote, bar)
}

/// "下载 Qwen3-ASR-1.7B-Q8_0.gguf · modelscope.cn" for the terminal bar.
fn human(remote: &Remote, host: &Host, phase: Phase) -> String {
    match phase {
        Phase::Connect => crate::tr!("连接 {} · {}", host.name(), remote.file),
        Phase::Retry => crate::tr!("重新连接 {} · {}", host.name(), remote.file),
        Phase::Verify => crate::tr!("校验 {}", remote.file),
        Phase::Download | Phase::Load => format!("{} · {}", remote.file, host.name()),
    }
}

fn set_phase(bar: &Bar, remote: &Remote, host: &Host, phase: Phase) {
    bar.set_phase(phase, &host.name(), human(remote, host, phase));
}

// ── built-in client ─────────────────────────────────────────────────────

fn native(url: &str, host: &Host, part: &Path, remote: &Remote, bar: &Bar) -> Result<()> {
    // An aria2 partial file is sparse; its length is not a contiguous prefix.
    let control = aria2_control(part);
    if control.exists() {
        let _ = fs::remove_file(part);
        let _ = fs::remove_file(control);
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(STALL)
        .build();
    let mut idle = 0;
    let mut phase = Phase::Connect;
    loop {
        crate::dispatch::check_control()?;
        let have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if have == remote.size {
            return Ok(());
        }
        if have > remote.size {
            fs::remove_file(part)?;
            continue;
        }
        bar.set_position(have);
        set_phase(bar, remote, host, phase);
        let result = native_attempt(&agent, url, part, have, remote, host, bar);
        let after = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        match result {
            Ok(()) => idle = 0,
            Err(error) => {
                if error.downcast_ref::<PermanentHttp>().is_some() || cancelled() {
                    return Err(error);
                }
                if after > have {
                    idle = 0;
                } else {
                    idle += 1;
                }
                if idle >= IDLE_ATTEMPTS {
                    return Err(error);
                }
                tracing::warn!(
                    file = remote.file,
                    host = %host.name(),
                    received = after,
                    "下载中断，正在续传 / Download interrupted; resuming: {error:#}"
                );
                phase = Phase::Retry;
                crate::runtime::interruptible_sleep(Duration::from_secs(
                    2 * u64::from(idle.max(1)),
                ))?;
            }
        }
    }
}

/// One request from byte `have`; appends to `part`.
fn native_attempt(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    have: u64,
    remote: &Remote,
    host: &Host,
    bar: &Bar,
) -> Result<()> {
    tracing::info!(url, from = have, "download");
    let mut request = agent.get(url);
    if have > 0 {
        request = request.set("Range", &format!("bytes={have}-"));
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            // The server's error page or JSON usually names the cause.
            let body = response.into_string().unwrap_or_default();
            let n = body.chars().count();
            let tail: String = body.chars().skip(n.saturating_sub(200)).collect();
            let message = format!("HTTP {code}: {tail}");
            if code == 416 && have > 0 {
                // The range no longer fits this file; start over.
                fs::remove_file(part)?;
                anyhow::bail!(message);
            }
            if is_permanent_status(code) {
                return Err(anyhow::Error::new(PermanentHttp(message)));
            }
            anyhow::bail!(message);
        }
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(crate::tr!("模型请求失败 / Model request failed")));
        }
    };
    let mut done = if response.status() == 206 { have } else { 0 };
    let mut out = if done > 0 {
        fs::OpenOptions::new().append(true).open(part)?
    } else {
        fs::File::create(part)?
    };
    set_phase(bar, remote, host, Phase::Download);
    bar.set_position(done);
    let mut reader = response.into_reader();
    let mut buffer = vec![0u8; 512 * 1024];
    let mut since_check = 0u64;
    loop {
        // Checking every 32MB lets the desktop cancel without waiting for 2GB.
        if since_check >= 32 << 20 {
            crate::dispatch::check_control()?;
            since_check = 0;
        }
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        out.write_all(&buffer[..n])?;
        done += n as u64;
        since_check += n as u64;
        bar.set_position(done);
    }
    out.sync_all()?;
    anyhow::ensure!(
        done == remote.size,
        "下载不完整 / Incomplete download (expected {} bytes, received {done})",
        remote.size
    );
    Ok(())
}

// ── aria2 ───────────────────────────────────────────────────────────────

static ARIA2_BROKEN: AtomicBool = AtomicBool::new(false);

/// aria2c beside the executable (desktop packages) or on PATH.
fn aria2_program() -> Option<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    if ARIA2_BROKEN.load(Ordering::Relaxed) || std::env::var_os("COURSE2MD_NO_ARIA2").is_some() {
        return None;
    }
    FOUND
        .get_or_init(|| {
            let name = format!("aria2c{}", std::env::consts::EXE_SUFFIX);
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
                .filter(|path| path.is_file())
                .or_else(|| crate::runtime::which("aria2c"))
        })
        .clone()
}

fn aria2_control(part: &Path) -> PathBuf {
    let mut name = part.as_os_str().to_owned();
    name.push(".aria2");
    PathBuf::from(name)
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::getrandom(&mut buffer).map_err(|_| anyhow::anyhow!("getrandom"))?;
    Ok(buffer.iter().map(|b| format!("{b:02x}")).collect())
}

/// Runs aria2c for one file and follows it over JSON-RPC, which reports exact
/// byte counts (its console output rounds to MiB).
fn aria2(
    program: &Path,
    url: &str,
    host: &Host,
    part: &Path,
    remote: &Remote,
    bar: &Bar,
) -> Result<()> {
    let unavailable = |message: String| anyhow::Error::new(Aria2Unavailable(message));
    let dir = part.parent().context("part path")?;
    let out = part.file_name().context("part path")?;
    let port = crate::runtime::free_port()?;
    let secret = random_hex(16)?;
    let gid = random_hex(8)?;
    // The secret stays out of the process list; the file also replaces any
    // user aria2.conf, whose options could change where bytes land.
    let mut conf = tempfile::NamedTempFile::new()?;
    writeln!(conf, "rpc-secret={secret}")?;
    conf.flush()?;
    let mut command = std::process::Command::new(program);
    command
        .arg(format!("--conf-path={}", conf.path().display()))
        .arg("--quiet=true")
        .arg("--enable-rpc=true")
        .arg("--rpc-listen-all=false")
        .arg(format!("--rpc-listen-port={port}"))
        .arg(format!("--stop-with-process={}", std::process::id()))
        .arg(format!("--gid={gid}"))
        .arg("--dir")
        .arg(dir)
        .arg("--out")
        .arg(out)
        .args([
            "--continue=true",
            "--allow-overwrite=true",
            "--auto-file-renaming=false",
            "--file-allocation=none",
            "--max-connection-per-server=8",
            "--split=8",
            "--min-split-size=8M",
            "--connect-timeout=15",
            "--timeout=30",
            "--max-tries=5",
            "--retry-wait=3",
            "--max-file-not-found=2",
        ])
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = crate::runtime::ManagedChild::spawn("aria2c", &mut command)
        .map_err(|error| unavailable(format!("{error:#}")))?;
    let stderr = child
        .take_stderr()
        .map(|stderr| crate::runtime::drain_stderr(stderr, "aria2c"));
    let rpc = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    let endpoint = format!("http://127.0.0.1:{port}/jsonrpc");
    let call = |method: &str, params: serde_json::Value| -> Result<serde_json::Value> {
        let mut all = vec![serde_json::json!(format!("token:{secret}"))];
        if let serde_json::Value::Array(rest) = params {
            all.extend(rest);
        }
        let response: serde_json::Value = rpc
            .post(&endpoint)
            .send_json(
                serde_json::json!({"jsonrpc":"2.0","id":"c2m","method":method,"params":all}),
            )?
            .into_json()?;
        response
            .get("result")
            .cloned()
            .with_context(|| response.to_string())
    };
    set_phase(bar, remote, host, Phase::Connect);
    let started = Instant::now();
    let mut answered = false;
    let mut downloading = false;
    let mut last = (0u64, Instant::now());
    loop {
        std::thread::sleep(Duration::from_millis(500));
        // Dropping the child stops aria2; its control file keeps the progress.
        crate::dispatch::check_control()?;
        if let Some(status) = child.try_wait() {
            let tail = stderr.as_ref().map(|tail| tail.tail()).unwrap_or_default();
            let message = format!("aria2c {status}: {tail}");
            return Err(if answered {
                anyhow::anyhow!(message)
            } else {
                unavailable(message)
            });
        }
        let status = match call(
            "aria2.tellStatus",
            serde_json::json!([
                gid,
                ["status", "completedLength", "errorCode", "errorMessage"]
            ]),
        ) {
            Ok(status) => status,
            Err(error) if !answered && started.elapsed() < Duration::from_secs(10) => {
                tracing::debug!("aria2 RPC not ready: {error:#}");
                continue;
            }
            Err(error) if !answered => return Err(unavailable(format!("{error:#}"))),
            Err(error) => return Err(error.context("aria2 RPC")),
        };
        answered = true;
        let completed: u64 = status["completedLength"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        if completed != last.0 {
            last = (completed, Instant::now());
            if !downloading && completed > 0 {
                downloading = true;
                set_phase(bar, remote, host, Phase::Download);
            }
        }
        bar.set_position(completed);
        match status["status"].as_str().unwrap_or_default() {
            "complete" => {
                let _ = call("aria2.shutdown", serde_json::json!([]));
                let _ = child.wait_within(Duration::from_secs(5));
                return Ok(());
            }
            "error" | "removed" => {
                anyhow::bail!(
                    "aria2 {}: {}",
                    status["errorCode"].as_str().unwrap_or("?"),
                    status["errorMessage"].as_str().unwrap_or_default()
                );
            }
            _ => {}
        }
        if last.1.elapsed() >= STALL {
            anyhow::bail!(
                "{}",
                crate::tr!("一分钟没有收到数据 / No data for a minute")
            );
        }
    }
}

// ── verification ────────────────────────────────────────────────────────

/// Whether `part` has the published size and SHA-256; reports hashing progress.
fn verify(part: &Path, remote: &Remote, bar: &Bar) -> Result<bool> {
    use sha2::{Digest, Sha256};
    if fs::metadata(part).map(|m| m.len()).unwrap_or(0) != remote.size {
        return Ok(false);
    }
    bar.set_phase(Phase::Verify, "", crate::tr!("校验 {}", remote.file));
    bar.set_position(0);
    let mut file = fs::File::open(part)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut done = 0u64;
    loop {
        if done.is_multiple_of(64 << 20) {
            crate::dispatch::check_control()?;
        }
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        done += n as u64;
        bar.set_position(done);
    }
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if digest != remote.sha256 {
        tracing::warn!(file = remote.file, expected = remote.sha256, actual = %digest, "checksum mismatch");
    }
    Ok(digest == remote.sha256)
}

/// Records the verified size; later startups compare against it.
fn write_manifest(dest: &Path, remote: &Remote) {
    let manifest = serde_json::json!({"size": remote.size, "sha256": remote.sha256});
    if let Err(error) = crate::checkpoint::atomic_write(
        &dest.with_extension("manifest.json"),
        manifest.to_string().as_bytes(),
    ) {
        tracing::warn!(path = %dest.display(), "模型清单未能写入 / Could not write the model manifest: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(body: &[u8]) -> Remote {
        use sha2::{Digest, Sha256};
        let digest: String = Sha256::digest(body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Remote {
            repo: "test/repo",
            file: "synthetic.bin",
            size: body.len() as u64,
            sha256: Box::leak(digest.into_boxed_str()),
        }
    }

    /// Serves canned responses in order and returns each request's head.
    fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if stream.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        (format!("http://{address}/model"), server)
    }

    #[test]
    fn interrupted_downloads_resume_with_a_range_and_verify() {
        let body = b"abcdef";
        let remote = remote(body);
        let (url, server) = serve(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc".into(),
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef".into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let part = root.path().join("synthetic.part");
        let host = Host::Hub("http://127.0.0.1".into());
        let bar = Bar::new(Stage::ModelFile("synthetic.bin".into()), remote.size);
        native(&url, &host, &part, &remote, &bar).unwrap();
        let requests = server.join().unwrap();
        assert!(!requests[0].to_ascii_lowercase().contains("range:"));
        assert!(requests[1].to_ascii_lowercase().contains("range: bytes=3-"));
        assert_eq!(fs::read(&part).unwrap(), body);
        assert!(verify(&part, &remote, &bar).unwrap());
    }

    #[test]
    fn a_server_ignoring_the_range_restarts_the_file() {
        let remote = remote(b"12345");
        let (url, server) = serve(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345".into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let part = root.path().join("synthetic.part");
        fs::write(&part, b"xx").unwrap();
        let host = Host::Hub("http://127.0.0.1".into());
        let bar = Bar::new(Stage::ModelFile("synthetic.bin".into()), remote.size);
        native(&url, &host, &part, &remote, &bar).unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&part).unwrap(), b"12345");
    }

    #[test]
    fn missing_files_are_not_retried() {
        let remote = remote(b"12345");
        let (url, server) = serve(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found"
                .into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let part = root.path().join("synthetic.part");
        let host = Host::Hub("http://127.0.0.1".into());
        let bar = Bar::new(Stage::ModelFile("synthetic.bin".into()), remote.size);
        let error = native(&url, &host, &part, &remote, &bar).unwrap_err();
        assert!(error.downcast_ref::<PermanentHttp>().is_some());
        server.join().unwrap();
    }

    #[test]
    fn corrupted_bytes_fail_verification() {
        let remote = remote(b"12345");
        let root = tempfile::tempdir().unwrap();
        let part = root.path().join("synthetic.part");
        let bar = Bar::new(Stage::ModelFile("synthetic.bin".into()), remote.size);
        fs::write(&part, b"12346").unwrap();
        assert!(!verify(&part, &remote, &bar).unwrap());
        fs::write(&part, b"1234").unwrap();
        assert!(!verify(&part, &remote, &bar).unwrap());
    }

    #[test]
    fn permanent_status_classification() {
        for code in [400, 401, 403, 404, 422] {
            assert!(is_permanent_status(code), "{code}");
        }
        for code in [429, 500, 502, 503] {
            assert!(!is_permanent_status(code), "{code}");
        }
    }
}
