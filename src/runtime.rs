//! 子进程与本地服务的共用原语（llama-server / NPU worker / ffmpeg）：
//! - `ManagedChild`：Drop 保证 kill+wait，任何错误路径（含 `?` 早退）不泄漏进程
//! - `run_bounded`：短命令的超时与输出捕获
//! - `wait_ready`：健康轮询期间监视子进程，秒退立即报错而不是等满超时
//! - `retry_backoff` / `interruptible_sleep`：可取消的重试退避

use crate::tr;
use anyhow::{Context, Result};
use std::net::TcpListener;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// 获取互斥锁；若锁已中毒（某线程在持锁期间 panic），直接取走数据继续用。
/// 语义：本进程自愿接受中毒锁——所有持锁临界区只做内存内状态更新，
/// 数据本身不会被 panic 路径写坏，传播 PoisonError 反而会让无关线程陪葬。
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Runs an async function from the CLI's synchronous entry points (main,
/// wizard) on one process-wide runtime, created on first use, so a command
/// that loops over notes does not start a worker pool per call.
pub fn block_on<F: std::future::Future>(f: F) -> Result<F::Output> {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime.block_on(f));
    }
    let runtime = tokio::runtime::Runtime::new()?;
    Ok(RUNTIME.get_or_init(|| runtime).block_on(f))
}

/// kill-on-drop 的子进程句柄。
pub struct ManagedChild {
    child: Child,
    name: &'static str,
}

impl ManagedChild {
    pub fn spawn(name: &'static str, cmd: &mut Command) -> Result<Self> {
        let child = cmd
            .spawn()
            .with_context(|| tr!("无法启动 {} / Could not start {}", name, name))?;
        Ok(Self { child, name })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// 取出 piped 的 stderr 句柄（配合 [`drain_stderr`] 使用）。
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.stderr.take()
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 非阻塞收割；Some = 已退出。
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// 轮询等待进程退出，最多 `timeout`；Err = 超时（进程仍由 Drop 兜底 kill+wait）。
    pub fn wait_within(&mut self, timeout: Duration) -> Result<ExitStatus> {
        let t0 = Instant::now();
        loop {
            if let Some(status) = self.try_wait() {
                return Ok(status);
            }
            anyhow::ensure!(
                t0.elapsed() < timeout,
                "{} 未在 {:.1}s 内退出 / did not exit within the time limit",
                self.name,
                timeout.as_secs_f64()
            );
            std::thread::sleep(WAIT_POLL_INTERVAL);
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        // 双 kill 无害（对已退出进程返回 Err 被忽略）
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 后台读取子进程 stderr：避免 pipe 写满阻塞子进程；保留尾部若干行
/// 供失败诊断；verbose(debug) 时逐行转发到 tracing。
/// 返回的尾部缓存与会话同寿命，随时可读取。
#[derive(Clone, Default)]
pub struct StderrTail {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// How many non-empty lines (progress redraws included) the child has
    /// written: a caller can tell a child that is still downloading from one
    /// that has gone quiet.
    seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

const STDERR_TAIL_MAX: usize = 100;
/// wait_within 的轮询间隔：50ms 足够及时，又不至于空转 CPU
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

impl StderrTail {
    /// How many lines the child has written so far.
    pub fn seen(&self) -> usize {
        self.seen.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn tail(&self) -> String {
        self.lines.lock().map(|v| v.join("\n")).unwrap_or_default()
    }

    /// 给错误附上 stderr 尾部作为 context（尾部为空则原样返回）。
    /// 收拢 asr.rs / npu.rs 同构的「失败时附加子进程 stderr 尾部」骨架。
    pub fn attach(&self, e: anyhow::Error, label: &str) -> anyhow::Error {
        let tail = self.tail();
        if tail.is_empty() {
            e
        } else {
            e.context(format!("{label}:\n{tail}"))
        }
    }
}

/// 为一个已 spawn 的 stderr pipe 启动 drain 线程。
/// `target` 是 tracing target（如 "llama_server" / "npu_worker"），用于区分日志来源。
pub fn drain_stderr(stderr: std::process::ChildStderr, target: &'static str) -> StderrTail {
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let shared = lines.clone();
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = seen.clone();
    std::thread::spawn(move || {
        for_each_output_line(stderr, |line| {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // tracing 的 target: 只接受常量，来源名放消息前缀
            tracing::debug!("[{target}] {line}");
            if let Ok(mut v) = shared.lock()
                && !line.trim().is_empty()
            {
                v.push(line);
                let overflow = v.len().saturating_sub(STDERR_TAIL_MAX);
                if overflow > 0 {
                    v.drain(..overflow);
                }
            }
        });
    });
    StderrTail { lines, seen }
}

/// Reads a child's output to its end, one line at a time. Bytes that are not
/// UTF-8 (a Windows tool writing in the ANSI code page) are replaced instead of
/// ending the read: a reader that stops would close the pipe and make the
/// child's next write fail. Carriage returns end a line too, so a progress bar
/// that redraws with `\r` does not grow one unbounded line.
fn for_each_output_line(reader: impl std::io::Read, mut line: impl FnMut(String)) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        let chunk = match reader.fill_buf() {
            Ok([]) => break,
            Ok(chunk) => chunk,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let (used, end) = match chunk.iter().position(|b| matches!(b, b'\n' | b'\r')) {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        buf.extend_from_slice(&chunk[..used - usize::from(end)]);
        reader.consume(used);
        if end && !buf.is_empty() {
            line(String::from_utf8_lossy(&buf).into_owned());
            buf.clear();
        }
    }
    if !buf.is_empty() {
        line(String::from_utf8_lossy(&buf).into_owned());
    }
}

/// 有界命令输出（stdout/stderr 经临时文件捕获，不受 pipe 缓冲与 GUI stdin 影响）。
pub struct BoundedOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// 短命令执行：stdin 关闭（ffmpeg 等工具在 GUI/管道 stdin 上会挂死）、
/// 超时强制 kill（ManagedChild 兜底）、输出经临时文件捕获（防 pipe 写满再超时）。
pub fn run_bounded(
    name: &'static str,
    cmd: &mut Command,
    timeout: Duration,
) -> Result<BoundedOutput> {
    use std::io::{Read, Seek};
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    cmd.stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = ManagedChild::spawn(name, cmd)?;
    let status = child.wait_within(timeout).map_err(|_| {
        anyhow::anyhow!(
            "{name} 超过 {0:.0} 秒未完成，已终止 / {name} did not finish within {0:.0}s and was stopped",
            timeout.as_secs_f64()
        )
    })?;
    stdout.rewind()?;
    stderr.rewind()?;
    let capture = |file: &std::fs::File| -> Result<String> {
        let mut bytes = Vec::new();
        file.take(64 * 1024 * 1024).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    Ok(BoundedOutput {
        status,
        stdout: capture(&stdout)?,
        stderr: capture(&stderr)?,
    })
}

/// 轮询 `{base}/health` 直到其 status 为 ok（端口可能被无关服务抢占，free_port 有
/// TOCTOU 窗口）；子进程中途退出立即失败，不等满超时。`control` 每轮调用一次：
/// 返回 Err 表示外部要求中止（暂停/取消），原样上抛，由调用方区分于启动失败。
pub fn wait_ready(
    base: &str,
    timeout: Duration,
    child: &mut ManagedChild,
    control: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let t0 = Instant::now();
    let url = format!("{base}/health");
    let agent = http_agent(Duration::from_secs(2));
    loop {
        control()?;
        if let Some(st) = child.try_wait() {
            anyhow::bail!(
                "{} 启动失败（{st}） / {} exited during startup ({st})",
                child.name(),
                child.name()
            );
        }
        if t0.elapsed() > timeout {
            anyhow::bail!(
                "{} 启动超时（{:.0} 秒） / {} startup timed out after {:.0}s",
                child.name(),
                timeout.as_secs_f64(),
                child.name(),
                timeout.as_secs_f64()
            );
        }
        if let Ok(mut resp) = agent.get(&url).call()
            && resp
                .body_mut()
                .read_json::<serde_json::Value>()
                .is_ok_and(|body| body["status"] == "ok")
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 无重定向的 HTTP agent：整体超时参数化，内置 `.redirects(0)`。
///
/// 安全纪律（与 dispatch.rs 请求账本注释一致）：请求经 3xx 重定向后失败，
/// 无法证明服务端「未提交」——可能已产生费用/副作用，因此凡走账本的调用
/// 必须禁止跟随重定向。这里在构造处统一内置，不靠每个调用点自觉。
/// 需要 connect/read 细分超时的场景（fetch.rs 元数据、download.rs 大文件下载）
/// 从 [`agent_builder`] 自行配置。
pub(crate) fn http_agent(timeout: Duration) -> ureq::Agent {
    agent_builder()
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}

/// [`http_agent`] whose error statuses arrive as responses, so that a caller can
/// read the body. Whoever uses it checks the status; `dispatch::receive` does.
pub(crate) fn http_agent_keeping_errors(timeout: Duration) -> ureq::Agent {
    let agent = agent_builder().timeout_global(Some(timeout));
    agent.http_status_as_error(false).build().new_agent()
}

/// A menu that Esc cancels with `cancelled` as the error, not accepting the default.
pub(crate) fn choose(
    prompt: &str,
    items: &[impl ToString],
    default: usize,
    cancelled: &str,
) -> Result<usize> {
    dialoguer::Select::new()
        .with_prompt(prompt)
        .items(items.iter().map(ToString::to_string))
        .default(default)
        .interact_opt()?
        .ok_or_else(|| anyhow::anyhow!("{cancelled}"))
}

/// Whether a person is at both ends of this terminal, so a prompt can be answered.
pub(crate) fn interactive() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// What every agent starts from: no redirects (see [`http_agent`]), and no proxy from
/// the environment, which would otherwise also catch local services on loopback.
pub(crate) fn agent_builder() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder().proxy(None).max_redirects(0)
}

/// Windows: start a console helper without opening a console window. The
/// engine runs without a console under the desktop app, and a console child
/// would otherwise open one of its own. No effect elsewhere.
pub fn hide_console(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Ask a still-running child and everything it started to stop. Unix sends
/// SIGTERM to the process group the child leads (spawned with
/// `process_group(0)`); Windows has no groups, so `taskkill /T` walks the
/// process tree. Launchers such as `uv run` start the real worker as a
/// grandchild, which killing the direct child alone would leave running.
pub fn terminate_tree(child: &mut ManagedChild) {
    if child.try_wait().is_some() {
        // Reaped: its id may already belong to another process.
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new(system32_program("taskkill.exe"));
        cmd.args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = hide_console(&mut cmd).status();
    }
}

/// A Windows system tool by absolute path, so a same-named program earlier on
/// PATH (or in the working directory) is never run in its place.
#[cfg(windows)]
pub fn system32_program(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32").join(name))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Own an OS lock separately from the lifetime of duplicated file descriptors.
pub struct FileLock {
    file: std::fs::File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Unix flock locks belong to the open file description. A concurrently
        // forked child may still hold that description until exec, so closing
        // our descriptor alone need not release the lock at the end of its scope.
        let _ = self.file.unlock();
    }
}

fn open_lock_file(path: &Path) -> Result<std::fs::File> {
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

/// Hold an OS file lock until the returned guard is dropped. The lock file
/// remains in place so another process cannot open a different inode at its path.
pub fn lock_file(path: &Path) -> Result<FileLock> {
    let file = open_lock_file(path)?;
    file.try_lock().with_context(|| {
        tr!("目录正被另一任务使用：{}。请等待任务结束", path.display()).to_string()
    })?;
    Ok(FileLock { file })
}

/// [`lock_file`] for a short critical section: waits up to `timeout` for the holder,
/// in this process or another, instead of failing at once.
pub fn lock_file_within(path: &Path, timeout: Duration) -> Result<FileLock> {
    let file = open_lock_file(path)?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(FileLock { file }),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    tr!(
                        "等待文件锁 {} 超时 / Timed out waiting for the file lock {}",
                        path.display(),
                        path.display()
                    )
                });
            }
        }
    }
}

/// 让 OS 分配一个空闲端口。
/// 注意 TOCTOU：返回后到真正 bind 之间端口可能被抢占，
/// 调用方需用 `wait_ready` 的 `/health` status 校验兜底。
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// 临时工作目录（chunk / worker 脚本等中间产物）：创建失败立即报错，
/// Drop 时尽力清理整个目录。目录名带 tag 与随机后缀，避免并发实例冲突。
pub(crate) fn temp_work_dir(tag: &str) -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(&format!("course2md-{tag}-"))
        .tempdir()
        .context(tr!(
            "无法创建临时工作目录 / Could not create a temporary working directory"
        ))
}

/// 操作系统随机源生成的 URL-safe base64 串（会话令牌、PKCE 参数）。
pub fn random_b64url(bytes: usize) -> anyhow::Result<String> {
    use base64::Engine as _;
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|_| {
        anyhow::anyhow!(crate::tr!(
            "无法生成安全随机数 / Cannot generate secure random bytes"
        ))
    })?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lines_survive_bytes_that_are_not_utf8() {
        // GBK "错误" followed by more output: the reader must keep going.
        let input: &[u8] = b"\xb4\xed\xce\xf3 one\nprogress 10%\rprogress 20%\r\nlast";
        let mut lines = Vec::new();
        for_each_output_line(input, |line| lines.push(line));
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].ends_with(" one"));
        assert_eq!(lines[1..], ["progress 10%", "progress 20%", "last"]);
    }

    #[test]
    fn a_short_lock_waits_for_its_holder_and_gives_up_on_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let holder = lock_file(&path).unwrap();
        assert!(lock_file_within(&path, Duration::from_millis(100)).is_err());
        let waiter = {
            let path = path.clone();
            std::thread::spawn(move || lock_file_within(&path, Duration::from_secs(20)).is_ok())
        };
        std::thread::sleep(Duration::from_millis(150));
        drop(holder);
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn workspace_lock_rejects_a_second_writer_and_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let first = lock_file(&path).unwrap();
        assert!(lock_file(&path).is_err());
        drop(first);
        assert!(lock_file(&path).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn workspace_lock_releases_while_an_inherited_descriptor_remains_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let first = lock_file(&path).unwrap();
        // dup and fork both retain the same open file description. Keeping a
        // duplicate makes the fork-before-exec window deterministic, without
        // forking the multithreaded test runner or depending on scheduler timing.
        let inherited = first.file.try_clone().unwrap();
        assert!(lock_file(&path).is_err());
        drop(first);
        let second = lock_file(&path).expect("scope exit must release the original lock");
        drop(inherited);
        assert!(
            lock_file(&path).is_err(),
            "the new owner must keep its lock"
        );
        drop(second);
        assert!(lock_file(&path).is_ok());
    }

    /// 取消任务后，模型加载（最长 300s）不得继续空等：control 返回 Err 立即中止。
    #[cfg(unix)]
    #[test]
    fn wait_ready_aborts_when_control_hook_fails() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let mut child = ManagedChild::spawn("sleep", &mut cmd).unwrap();
        let t0 = Instant::now();
        // 永远不就绪的端口 + 60s 超时；control 第二轮即报错，应远早于超时返回
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = wait_ready(
            "http://127.0.0.1:9",
            Duration::from_secs(60),
            &mut child,
            &|| {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1 {
                    anyhow::bail!("测试取消 / test cancellation")
                } else {
                    Ok(())
                }
            },
        );
        let err = result.expect_err("control 报错必须中止 wait_ready");
        assert_eq!(
            err.to_string(),
            "测试取消 / test cancellation",
            "控制错误须原样返回"
        );
        assert!(t0.elapsed() < Duration::from_secs(10), "取消不应等到超时");
    }

    /// 错误路径/panic unwind 也必须 kill+wait 子进程（孤儿 llama-server 会占住 GPU 设备）。
    #[cfg(unix)]
    #[test]
    fn managed_child_drop_kills_and_reaps() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let child = ManagedChild::spawn("sleep", &mut cmd).unwrap();
        let pid = child.id().to_string();
        drop(child);
        // kill -0 仅探测存在性：Drop 已 kill+wait，进程应不复存在（zombie 也被收割）
        let st = Command::new("kill").args(["-0", &pid]).status().unwrap();
        assert!(!st.success(), "pid {pid} 应已被 Drop 清理");
    }
}
