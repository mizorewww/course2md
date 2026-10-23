//! UI-independent process lifecycle and course library access.
use anyhow::{Context, Result};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

pub use course2md::progress::{Done, Event as EngineEvent, LogLevel};

/// One line of engine output, or the end of the process.
#[derive(Debug)]
pub enum Event {
    Engine(EngineEvent),
    Exit { success: bool, cancelled: bool },
}

impl Event {
    fn log(message: String) -> Self {
        Self::Engine(EngineEvent::Log {
            level: LogLevel::Info,
            message,
        })
    }
    fn error(message: String) -> Self {
        Self::Engine(EngineEvent::Error { message })
    }
}

/// `CREATE_NO_WINDOW`: helper processes never flash a console window.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A child process reaped on its own thread. `kill` cannot reach a recycled
/// PID: on Unix the reaper first waits without reaping (`WNOWAIT`), so the
/// zombie keeps its PID reserved until the reaper marks it reaped under the
/// same lock `kill` takes; on Windows the open process handle reserves it.
pub(crate) struct Watched {
    shared: std::sync::Arc<WatchedProcess>,
}
struct WatchedProcess {
    pid: u32,
    /// Kill the whole process group (Unix) / tree (Windows).
    group: bool,
    state: std::sync::Mutex<WatchState>,
}
#[derive(Default)]
struct WatchState {
    reaped: bool,
    killed: bool,
}
impl Watched {
    /// Reaps `child` on a dedicated thread and then calls `on_exit` there with
    /// its status and whether `kill` stopped it.
    pub(crate) fn spawn(
        mut child: std::process::Child,
        group: bool,
        on_exit: impl FnOnce(std::io::Result<std::process::ExitStatus>, bool) + Send + 'static,
    ) -> Self {
        let shared = std::sync::Arc::new(WatchedProcess {
            pid: child.id(),
            group,
            state: Default::default(),
        });
        let reaper = std::sync::Arc::clone(&shared);
        thread::spawn(move || {
            #[cfg(unix)]
            loop {
                // SAFETY: waitid only writes the zeroed siginfo it is given.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let waited = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        reaper.pid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT,
                    )
                };
                if waited == 0
                    || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    break;
                }
            }
            let (status, killed) = {
                let mut state = course2md::runtime::lock_recover(&reaper.state);
                let status = child.wait();
                state.reaped = true;
                (status, state.killed)
            };
            drop(child);
            on_exit(status, killed);
        });
        Self { shared }
    }

    /// Stops the process unless it has already been reaped. Returns whether
    /// it was still running.
    pub(crate) fn kill(&self) -> bool {
        let mut state = course2md::runtime::lock_recover(&self.shared.state);
        if state.reaped {
            return false;
        }
        state.killed = true;
        kill_pid(self.shared.pid, self.shared.group);
        true
    }
}

fn kill_pid(pid: u32, group: bool) {
    #[cfg(unix)]
    unsafe {
        if group {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let pid = pid.to_string();
        let mut args = vec!["/PID", pid.as_str(), "/F"];
        if group {
            args.push("/T");
        }
        let _ = Command::new("taskkill")
            .creation_flags(CREATE_NO_WINDOW)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub struct Job {
    pub events: Receiver<Event>,
    process: Watched,
}
impl Job {
    pub fn start(args: Vec<String>, wake: smol::channel::Sender<()>) -> Result<Self> {
        Self::spawn_input(resolve_cli()?, args, None, Redactions(vec![]), wake)
    }
    #[cfg(test)]
    fn spawn(bin: PathBuf, args: Vec<String>) -> Result<Self> {
        let (wake, _) = smol::channel::bounded(1);
        Self::spawn_input(bin, args, None, Redactions(vec![]), wake)
    }
    pub fn start_task(
        request: &course2md::execution::Request,
        wake: smol::channel::Sender<()>,
    ) -> Result<Self> {
        Self::spawn_input(
            resolve_cli()?,
            vec!["run-task".into()],
            Some(serde_json::to_vec(request)?),
            Redactions::for_request(request),
            wake,
        )
    }
    fn spawn_input(
        bin: PathBuf,
        args: Vec<String>,
        input: Option<Vec<u8>>,
        redactions: Redactions,
        wake: smol::channel::Sender<()>,
    ) -> Result<Self> {
        let redactions = std::sync::Arc::new(redactions);
        let mut command = Command::new(&bin);
        command
            .args(args)
            .env("PATH", tool_path())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .with_context(|| course2md::tr!("启动 {} 失败", bin.display()))?;
        let pipes = (|| {
            Ok::<_, anyhow::Error>((
                child
                    .stdout
                    .take()
                    .context(course2md::tr!("缺少 stdout 管道"))?,
                child
                    .stderr
                    .take()
                    .context(course2md::tr!("缺少 stderr 管道"))?,
                match &input {
                    Some(_) => Some(
                        child
                            .stdin
                            .take()
                            .context(course2md::tr!("无法传递任务参数"))?,
                    ),
                    None => None,
                },
            ))
        })();
        let (stdout, stderr, stdin) = match pipes {
            Ok(pipes) => pipes,
            Err(error) => {
                terminate(&mut child);
                let _ = child.wait();
                return Err(error);
            }
        };
        let (tx, events) = mpsc::sync_channel(512);
        // Readers start before the request is written: a request larger than the
        // pipe buffer must not wait on an engine that is blocked writing output.
        let (finished, drained) = mpsc::channel();
        reader(
            stdout,
            tx.clone(),
            true,
            redactions.clone(),
            wake.clone(),
            finished.clone(),
        );
        reader(
            stderr,
            tx.clone(),
            false,
            redactions,
            wake.clone(),
            finished,
        );
        let process = Watched::spawn(child, true, {
            let tx = tx.clone();
            let wake = wake.clone();
            move |status, cancelled| {
                let success = match status {
                    Ok(status) => !cancelled && status.success(),
                    Err(error) => {
                        let _ = tx.send(Event::error(error.to_string()));
                        false
                    }
                };
                // 有界排空：子进程已死则管道必将 EOF，但卡住的 reader（例如管道被
                // 孙进程继承）不得把 Exit 永远挡在门外。读不到的剩余输出允许丢失。
                let deadline = Instant::now() + Duration::from_secs(3);
                for _ in 0..2 {
                    if drained
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .is_err()
                    {
                        break;
                    }
                }
                let _ = tx.send(Event::Exit { success, cancelled });
                let _ = wake.try_send(());
            }
        });
        if let (Some(mut bytes), Some(mut pipe)) = (input, stdin) {
            let tx = tx.clone();
            thread::spawn(move || {
                let sent = pipe.write_all(&bytes);
                drop(pipe);
                use zeroize::Zeroize;
                bytes.zeroize();
                if let Err(error) = sent {
                    // The engine closed its input early; its own exit reports why.
                    let _ = tx.send(Event::error(course2md::tr!("传递任务参数失败：{}", error)));
                    let _ = wake.try_send(());
                }
            });
        }
        Ok(Self { events, process })
    }
    pub fn cancel(&self) {
        self.process.kill();
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}
struct Redactions(Vec<String>);
impl Redactions {
    /// Every credential the request carries, read from its typed fields so a
    /// renamed field fails to compile instead of silently disabling redaction.
    fn for_request(request: &course2md::execution::Request) -> Self {
        Self(
            [&request.config.llm.api_key, &request.config.asr_api.api_key]
                .into_iter()
                .filter(|key| !key.is_empty())
                .cloned()
                .collect(),
        )
    }
    fn text(&self, input: &str) -> String {
        let mut result = input.to_owned();
        // Long keys first: a shorter credential may be a prefix of another one.
        let mut keys: Vec<_> = self.0.iter().filter(|key| !key.is_empty()).collect();
        keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
        for key in keys {
            result = result.replace(key, course2md::tr!("[已隐藏密钥]"));
            // Tool diagnostics can contain JSON embedded in ordinary stderr text.
            let encoded = serde_json::to_string(key).unwrap_or_default();
            if encoded.len() > 2 {
                result = result.replace(
                    &encoded[1..encoded.len() - 1],
                    course2md::tr!("[已隐藏密钥]"),
                );
            }
        }
        result
    }

    fn value(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => *text = self.line(text),
            serde_json::Value::Array(values) => values.iter_mut().for_each(|v| self.value(v)),
            serde_json::Value::Object(values) => {
                let original = std::mem::take(values);
                for (key, mut value) in original {
                    self.value(&mut value);
                    values.insert(self.text(&key), value);
                }
            }
            _ => {}
        }
    }

    fn line(&self, line: &str) -> String {
        if self.0.is_empty() {
            return line.to_owned();
        }
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) {
            // Decode JSON before filtering, including \uXXXX and escaped quotes.
            self.value(&mut value);
            return serde_json::to_string(&value).unwrap_or_default();
        }
        // A prefixed diagnostic is not a JSON document, but each valid quoted token
        // can still be decoded. Never retain a secret only because its spelling is escaped.
        let mut result = String::new();
        let bytes = line.as_bytes();
        let mut cursor = 0;
        let mut copied = 0;
        while cursor < bytes.len() {
            if bytes[cursor] != b'"' {
                cursor += 1;
                continue;
            }
            let start = cursor;
            cursor += 1;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'\\' => cursor = (cursor + 2).min(bytes.len()),
                    b'"' => {
                        cursor += 1;
                        if let Ok(token) = serde_json::from_str::<String>(&line[start..cursor]) {
                            result.push_str(&self.text(&line[copied..start]));
                            result.push_str(&serde_json::to_string(&self.text(&token)).unwrap());
                            copied = cursor;
                        }
                        break;
                    }
                    _ => cursor += 1,
                }
            }
        }
        result.push_str(&self.text(&line[copied..]));
        result
    }
}
impl Drop for Redactions {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}
fn reader(
    stream: impl std::io::Read + Send + 'static,
    tx: SyncSender<Event>,
    json: bool,
    redactions: std::sync::Arc<Redactions>,
    wake: smol::channel::Sender<()>,
    finished: mpsc::Sender<()>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Reports the end of this stream however the loop exits.
        struct Finished(mpsc::Sender<()>);
        impl Drop for Finished {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let _finished = Finished(finished);
        for line in BufReader::new(stream).lines() {
            let event = match line {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => {
                    let line = redactions.line(&line);
                    if json {
                        match serde_json::from_str(&line) {
                            Ok(event) => Event::Engine(event),
                            // A JSON object this build does not recognize is a
                            // protocol drift, not a log line; keep it visible.
                            Err(error) if line.starts_with('{') => Event::log(course2md::tr!(
                                "无法识别的任务事件（{}）：{}",
                                error,
                                line
                            )),
                            Err(_) => Event::log(line),
                        }
                    } else {
                        Event::log(line)
                    }
                }
                Err(error) => Event::error(course2md::tr!("读取任务输出失败：{}", error)),
            };
            if tx.send(event).is_err() {
                break;
            }
            let _ = wake.try_send(());
        }
    })
}
pub(crate) fn terminate(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::killpg(child.id() as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// The search path for engine and helper tools. The environment it is built
/// from does not change while the app runs, so it is computed once.
pub fn tool_path() -> std::ffi::OsString {
    static PATH: std::sync::OnceLock<std::ffi::OsString> = std::sync::OnceLock::new();
    PATH.get_or_init(compute_tool_path).clone()
}

fn compute_tool_path() -> std::ffi::OsString {
    if let Some(path) = std::env::var_os("COURSE2MD_TOOL_PATH").filter(|path| !path.is_empty()) {
        return path;
    }
    // 随包自带的工具（yt-dlp 等）与可执行文件同目录，排在最前：安装包行为
    // 不随用户 PATH 漂移。`.app` 布局额外看 ../Resources/bin。
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        dirs.push(dir.clone());
        dirs.push(dir.join("../Resources/bin"));
    }
    dirs.extend(
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    #[cfg(unix)]
    {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(&home).join(".local/bin"));
            dirs.push(PathBuf::from(home).join(".cargo/bin"));
        }
        dirs.extend([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
    }
    std::env::join_paths(dirs).unwrap_or_default()
}
/// Presence of a llama.cpp runtime is independent of `--list-devices` succeeding.
/// That flag can fail on CPU-only builds or older binaries that are still installed.
pub(crate) fn llama_binary_on_path(path: &std::ffi::OsStr) -> bool {
    ["llama-server", "llama-cli"].iter().any(|name| {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        std::env::split_paths(path).any(|dir| dir.join(&file).is_file())
    })
}

pub fn resolve_cli() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("COURSE2MD_BIN") {
        let path = PathBuf::from(path);
        anyhow::ensure!(
            path.is_file(),
            course2md::tr!("COURSE2MD_BIN 指向的文件不存在：{}", path.display())
        );
        return Ok(path);
    }
    let name = format!("course2md{}", std::env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()?.parent() {
        candidates.push(dir.join(&name));
    }
    // A development build runs beside the workspace's own engine build; an
    // older engine on PATH cannot read today's task format.
    if cfg!(debug_assertions) {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
        candidates.extend(["debug", "release"].map(|profile| workspace.join(profile).join(&name)));
    }
    candidates.extend(std::env::split_paths(&tool_path()).map(|dir| dir.join(&name)));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .context(course2md::tr!(
            "找不到 course2md 引擎。请将 CLI 放在应用旁，或设置 COURSE2MD_BIN。"
        ))
}

#[derive(Clone)]
pub struct Environment {
    pub engine: bool,
    pub ffmpeg: bool,
    pub ffprobe: bool,
    pub ytdlp: bool,
    pub llama: bool,
    pub apple: bool,
    pub gpu: Option<String>,
    pub npu: bool,
    pub npu_device: bool,
    pub npu_runtime: bool,
}
impl Environment {
    /// 本机环境是否支持指定的识别方式。NPU 要求设备与运行器同时可用——
    /// `npu` 字段在 detect() 中即 `npu_device && npu_runtime` 的合一，
    /// 两处写法等价，以此处为准。Api 不依赖本机组件。
    pub fn supports(&self, provider: course2md::config::AsrProvider) -> bool {
        use course2md::config::AsrProvider;
        match provider {
            AsrProvider::Coreml => self.apple,
            AsrProvider::Gpu => self.llama && self.gpu.is_some(),
            AsrProvider::Cpu => self.llama,
            AsrProvider::Npu => self.npu,
            AsrProvider::Api => true,
        }
    }

    pub fn detect() -> Self {
        let cli = resolve_cli().ok();
        // yt-dlp 走共享搜索清单（随包自带 → PATH → 常见安装目录），定位到
        // 绝对路径再探测；找不到时退回裸名，probe 失败与「未安装」同义。
        let ytdlp = course2md::ytdlp::resolve_in(Some(&tool_path()))
            .unwrap_or_else(|| PathBuf::from("yt-dlp"));
        // (字段名, 命令, 参数) 与回填写在一起：数组重排不会造成静默错配
        let checks = std::thread::scope(|scope| {
            let commands: Vec<(&'static str, PathBuf, &'static str)> = vec![
                (
                    "engine",
                    cli.clone().unwrap_or_else(|| PathBuf::from("course2md")),
                    "--version",
                ),
                ("ffmpeg", PathBuf::from("ffmpeg"), "-version"),
                ("ffprobe", PathBuf::from("ffprobe"), "-version"),
                ("ytdlp", ytdlp, "--version"),
                (
                    "llama-devices",
                    PathBuf::from("llama-server"),
                    "--list-devices",
                ),
            ];
            let handles: Vec<_> = commands
                .into_iter()
                .map(|(name, bin, arg)| {
                    // GPU 首次枚举可能触发驱动初始化（实测偶发 17s），给足与引擎一致的余量
                    let timeout = if name == "llama-devices" {
                        Duration::from_secs(20)
                    } else {
                        Duration::from_secs(5)
                    };
                    (name, scope.spawn(move || probe(&bin, &[arg], timeout)))
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|(name, task)| task.join().unwrap_or(None).map(|out| (name, out)))
                .collect::<std::collections::BTreeMap<_, _>>()
        });
        let llama = llama_binary_on_path(&tool_path());
        let gpu = checks.get("llama-devices").and_then(|output| {
            output.lines().find_map(|line| {
                let (id, description) = line.trim().split_once(':')?;
                course2md::asr::is_gpu_device_id(id).then(|| {
                    description
                        .split(" (")
                        .next()
                        .unwrap_or(description)
                        .trim()
                        .to_owned()
                })
            })
        });
        let apple = course2md::config::apple_native_available()
            && cli
                .as_deref()
                .and_then(course2md::config::find_metallib)
                .is_some();
        let npu_device = if cfg!(target_os = "linux") {
            Path::new(course2md::npu::NPU_DEVICE_PATH).exists()
        } else if cfg!(target_os = "windows") {
            probe(Path::new("powershell.exe"), &["-NoProfile", "-NonInteractive", "-Command",
                "Get-CimInstance Win32_PnPEntity | Where-Object { $_.Name -match 'Intel.*(AI Boost|NPU)' -and $_.Status -eq 'OK' } | Select-Object -ExpandProperty Name"], Duration::from_secs(10))
                .is_some_and(|name| !name.trim().is_empty())
        } else {
            false
        };
        let npu_runtime = ["uv", "python3", "python"].iter().any(|name| {
            let executable = format!("{name}{}", std::env::consts::EXE_SUFFIX);
            std::env::split_paths(&tool_path()).any(|dir| dir.join(&executable).is_file())
        });
        let npu = npu_device && npu_runtime;
        Self {
            engine: cli.is_some() && checks.contains_key("engine"),
            ffmpeg: checks.contains_key("ffmpeg"),
            ffprobe: checks.contains_key("ffprobe"),
            ytdlp: checks.contains_key("ytdlp"),
            llama,
            apple,
            gpu,
            npu,
            npu_device,
            npu_runtime,
        }
    }
}

/// Bounded, executable checks. Redirect output to a file so a verbose tool cannot
/// fill a pipe while the detector waits; timed-out children are always reaped.
fn probe(bin: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    use std::io::{Read, Seek};
    let mut output = tempfile::tempfile().ok()?;
    let mut command = Command::new(bin);
    command
        .args(args)
        .env("PATH", tool_path())
        .stdin(Stdio::null())
        .stdout(output.try_clone().ok()?)
        .stderr(output.try_clone().ok()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let child = command.spawn().ok()?;
    let (exited, exit) = mpsc::channel();
    let process = Watched::spawn(child, false, move |status, _| {
        let _ = exited.send(status);
    });
    let status = match exit.recv_timeout(timeout) {
        Ok(status) => status.ok()?,
        Err(_) => {
            process.kill();
            // The reaper reports once the killed child is gone.
            let _ = exit.recv();
            return None;
        }
    };
    if !status.success() {
        return None;
    }
    output.rewind().ok()?;
    let mut text = String::new();
    output.take(32 * 1024).read_to_string(&mut text).ok()?;
    Some(text)
}

pub use crate::notes::{Course, Preview, read_preview, scan_library};

/// macOS「始终显示滚动条」系统偏好（states-and-motion 滚动条契约要求尊重该偏好）。
/// 首次调用在后台线程读取 `defaults`，渲染期只读缓存值；读到之前与非 macOS 均按
/// Automatic（滚动时显示）。
pub fn scrollbars_always_visible() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ALWAYS: AtomicBool = AtomicBool::new(false);
    static PROBE: std::sync::Once = std::sync::Once::new();
    if cfg!(target_os = "macos") {
        PROBE.call_once(|| {
            thread::spawn(|| {
                let always = Command::new("defaults")
                    .args(["read", "-g", "AppleShowScrollBars"])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()
                    .filter(|out| out.status.success())
                    .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "Always");
                ALWAYS.store(always, Ordering::Relaxed);
            });
        });
    }
    ALWAYS.load(Ordering::Relaxed)
}

/// 滚动条模式：常显偏好→Always；其余滚动时显示、闲置淡出。
pub(crate) fn vertical_scrollbar(handle: &gpui::ScrollHandle) -> gpui_component::scroll::Scrollbar {
    vertical_scrollbar_for(handle)
}

/// 任意 ScrollbarHandle（含 gpui::ListState 虚拟列表）都走同一模式决策：
/// 系统「始终显示滚动条」偏好开启时常驻，否则滚动时显示。
pub(crate) fn vertical_scrollbar_for<H: gpui_component::scroll::ScrollbarHandle + Clone>(
    handle: &H,
) -> gpui_component::scroll::Scrollbar {
    use gpui_component::scroll::{Scrollbar, ScrollbarMode};
    Scrollbar::vertical(handle).mode(if scrollbars_always_visible() {
        ScrollbarMode::Always
    } else {
        ScrollbarMode::Scrolling
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::PreviewBlock;
    use std::time::SystemTime;

    #[test]
    fn done_event_uses_the_cli_protocol() {
        let event: EngineEvent = serde_json::from_str(r#"{"type":"done","out_dir":"out/test","title":"课程","slides":3,"segments":4,"chars":50,"elapsed_secs":1.5,"outputs":["course.md"]}"#).unwrap();
        assert!(matches!(event, EngineEvent::Done(done) if done.slides == 3));
        let event: EngineEvent = serde_json::from_str(
            r#"{"type":"model_ready","operation":"prepare","dir":"/tmp/models","model":"qwen3-1.7b"}"#,
        )
        .unwrap();
        assert!(matches!(
            event,
            EngineEvent::ModelReady { model, .. } if model.as_deref() == Some("qwen3-1.7b")
        ));
        let (tx, rx) = mpsc::sync_channel(8);
        let (wake, _) = smol::channel::bounded(1);
        reader(
            std::io::Cursor::new("{\"type\":\"from_the_future\"}\nplain text\n"),
            tx,
            true,
            std::sync::Arc::new(Redactions(vec![])),
            wake,
            mpsc::channel().0,
        )
        .join()
        .unwrap();
        match rx.recv().unwrap() {
            Event::Engine(EngineEvent::Log { message, .. }) => {
                assert!(message.contains("from_the_future") && message.contains("无法识别"))
            }
            _ => panic!("unknown events must stay visible"),
        }
        assert!(matches!(
            rx.recv().unwrap(),
            Event::Engine(EngineEvent::Log { message, .. }) if message == "plain text"
        ));
    }

    #[test]
    fn child_output_redacts_json_escaped_unicode_and_prefixed_credentials() {
        let key = "fake\"\\秘密\ncredential";
        let redactions = std::sync::Arc::new(Redactions(vec![key.into()]));
        let escaped: String = key
            .encode_utf16()
            .map(|unit| format!("\\u{unit:04x}"))
            .collect();
        let source = format!(
            "{{\"type\":\"error\",\"message\":\"failed {escaped}\"}}\n\
             {{\"type\":\"blocked\",\"reason\":\"paused\",\"request_id\":null,\"purpose\":null,\"description\":null,\"kind\":\"other\",\"message\":\"paused\"}}\n"
        );
        let (tx, rx) = mpsc::sync_channel(8);
        let (wake, _) = smol::channel::bounded(1);
        let handle = reader(
            std::io::Cursor::new(source),
            tx,
            true,
            redactions.clone(),
            wake,
            mpsc::channel().0,
        );
        handle.join().unwrap();
        match rx.recv().unwrap() {
            Event::Engine(EngineEvent::Error { message }) => {
                assert_eq!(message, "failed [已隐藏密钥]")
            }
            _ => panic!("protocol was lost during redaction"),
        }
        assert!(matches!(
            rx.recv().unwrap(),
            Event::Engine(EngineEvent::Blocked { .. })
        ));
        let stderr = format!("HTTP response: {{\"echo\":\"{escaped}\"}}");
        let clean = redactions.line(&stderr);
        assert!(clean.contains("[已隐藏密钥]"));
        assert!(!clean.contains("credential") && !clean.contains("\\u0063"));
        let nested =
            serde_json::json!({"type":"log","message":format!("{{\"echo\":\"{escaped}\"}}")});
        let clean: serde_json::Value =
            serde_json::from_str(&redactions.line(&nested.to_string())).unwrap();
        assert!(clean["message"].as_str().unwrap().contains("[已隐藏密钥]"));
    }

    #[test]
    fn notes_without_requested_file_exports_stay_readable() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let sections = vec![course2md::timeline::Section {
            t: 0.,
            end: 1.,
            image: String::new(),
            speech: vec![course2md::timeline::TranscriptEvent {
                start: 0.,
                end: 1.,
                text: "Manually added explanation.".into(),
                raw: None,
                translation: None,
            }],
        }];
        let target = course2md::artifact::Target {
            task_id: "task-1".into(),
            course_id: "course-1".into(),
            source_id: "source-1".into(),
            version_id: "v1".into(),
            course_dir: root.path().join("note"),
        };
        let meta = course2md::fetch::VideoMeta {
            title: "course".into(),
            uploader: String::new(),
            duration: 0.,
            webpage_url: String::new(),
            extractor: "local".into(),
            id: "source".into(),
        };
        let manifest = course2md::artifact::publish_blocking(
            &target,
            &work,
            &meta,
            &sections,
            None,
            &[],
            Default::default(),
        )
        .unwrap();
        let preview = read_preview(Course {
            dir: target.version_dir(),
            title: "course".into(),
            modified: SystemTime::now(),
            slides: 0,
            thumbnail: None,
            manifest: Some(manifest),
            warning: None,
        })
        .unwrap();
        assert!(preview.outputs.is_empty());
        assert!(preview.plain_text.contains("Manually added explanation."));
    }

    #[test]
    fn preview_resolves_only_images_inside_the_course() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("frames")).unwrap();
        image::RgbImage::new(2, 2)
            .save(work.join("frames/slide.jpg"))
            .unwrap();
        std::fs::write(dir.path().join("outside.jpg"), b"private").unwrap();
        let sections = vec![course2md::timeline::Section {
            t: 0.,
            end: 1.,
            image: "frames/slide.jpg".into(),
            speech: vec![course2md::timeline::TranscriptEvent {
                start: 0.,
                end: 1.,
                text: "Readable explanation.".into(),
                raw: None,
                translation: None,
            }],
        }];
        let target = course2md::artifact::Target {
            task_id: "task-1".into(),
            course_id: "course-1".into(),
            source_id: "source-1".into(),
            version_id: "v1".into(),
            course_dir: dir.path().join("course"),
        };
        let meta = course2md::fetch::VideoMeta {
            title: "Course".into(),
            uploader: String::new(),
            duration: 0.,
            webpage_url: String::new(),
            extractor: "local".into(),
            id: "source".into(),
        };
        course2md::artifact::publish_blocking(
            &target,
            &work,
            &meta,
            &sections,
            None,
            &[],
            Default::default(),
        )
        .unwrap();
        let version = target.version_dir();
        // Tamper with the published records: the frame reference now escapes the note.
        let mut manifest: course2md::artifact::Manifest =
            serde_json::from_slice(&std::fs::read(version.join("manifest.json")).unwrap()).unwrap();
        manifest.frames[0].image = "../../outside.jpg".into();
        std::fs::write(
            version.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let mut document: course2md::artifact::Document =
            serde_json::from_slice(&std::fs::read(version.join("document.json")).unwrap()).unwrap();
        document.sections[0].image = "../../outside.jpg".into();
        std::fs::write(
            version.join("document.json"),
            serde_json::to_vec_pretty(&document).unwrap(),
        )
        .unwrap();
        let preview = read_preview(Course {
            dir: version,
            title: "Course".into(),
            modified: SystemTime::now(),
            slides: 0,
            thumbnail: None,
            manifest: None,
            warning: None,
        })
        .unwrap();
        assert_eq!(
            preview
                .blocks
                .iter()
                .filter(|b| matches!(b, PreviewBlock::Image(_)))
                .count(),
            0
        );
        assert!(!preview.issues.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn process_exit_follows_all_buffered_output() {
        let job = Job::spawn("/bin/sh".into(), vec!["-c".into(), "i=0; while [ $i -lt 900 ]; do echo log-$i; i=$((i+1)); done; echo final-message >&2".into()]).unwrap();
        let mut lines = 0;
        loop {
            match job.events.recv_timeout(Duration::from_secs(10)).unwrap() {
                Event::Engine(EngineEvent::Log { .. }) => lines += 1,
                Event::Exit { success, cancelled } => {
                    assert!(success && !cancelled);
                    break;
                }
                _ => panic!("unexpected event"),
            }
        }
        assert_eq!(lines, 901);
    }

    #[test]
    fn task_redaction_covers_every_typed_credential() {
        let mut request: course2md::execution::Request =
            serde_json::from_value(serde_json::json!({
                "schema": 1,
                "task_id": "task",
                "course_id": "course",
                "version_id": "version",
                "source": "lecture.mp4",
                "source_id": "source",
                "title": "Lecture",
                "work_dir": "/tmp/work",
                "course_dir": "/tmp/course",
                "config": course2md::settings::ConfigFile::default(),
            }))
            .unwrap();
        request.config.llm.api_key = "llm-only-secret".into();
        request.config.asr_api.api_key = "asr-only-secret".into();
        let clean = Redactions::for_request(&request)
            .line("proofreading llm-only-secret, speech asr-only-secret");
        assert!(!clean.contains("only-secret"), "{clean}");
        assert_eq!(clean.matches("[已隐藏密钥]").count(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn an_oversized_request_cannot_deadlock_against_engine_output() {
        // cat echoes its input: written before the readers, this request would
        // fill both pipes and block forever.
        let line = "x".repeat(63);
        let lines = 64 * 1024;
        let input = format!("{line}\n").repeat(lines).into_bytes();
        let (wake, _) = smol::channel::bounded(1);
        let job = Job::spawn_input(
            "/bin/cat".into(),
            vec![],
            Some(input),
            Redactions(vec![]),
            wake,
        )
        .unwrap();
        let mut echoed = 0;
        loop {
            match job.events.recv_timeout(Duration::from_secs(20)).unwrap() {
                Event::Engine(EngineEvent::Log { .. }) => echoed += 1,
                Event::Exit { success, cancelled } => {
                    assert!(success && !cancelled);
                    break;
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert_eq!(echoed, lines);
    }

    #[test]
    fn probe_reports_output_and_times_out_without_polling() {
        #[cfg(unix)]
        {
            let started = Instant::now();
            assert_eq!(
                probe(Path::new("/bin/echo"), &["ready"], Duration::from_secs(5)).as_deref(),
                Some("ready\n")
            );
            assert!(probe(Path::new("/bin/sleep"), &["5"], Duration::from_millis(200)).is_none());
            assert!(started.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    #[cfg(unix)]
    fn cancellation_terminates_running_process_tree() {
        let job = Job::spawn(
            "/bin/sh".into(),
            vec!["-c".into(), "sleep 60 & echo ready; wait".into()],
        )
        .unwrap();
        assert!(matches!(
            job.events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Engine(EngineEvent::Log { .. })
        ));
        job.cancel();
        // Exit waits up to 3s for both inherited pipes to close; arriving sooner
        // means the descendant (sleep) released them, i.e. it was terminated too.
        assert!(matches!(
            job.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Exit {
                cancelled: true,
                ..
            }
        ));
    }

    #[test]
    fn llama_runtime_is_present_when_the_binary_exists_even_if_device_listing_fails() {
        let missing = tempfile::tempdir().unwrap();
        assert!(!llama_binary_on_path(missing.path().as_os_str()));
        let dir = tempfile::tempdir().unwrap();
        let bin = dir
            .path()
            .join(format!("llama-server{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&bin, b"not-executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(llama_binary_on_path(dir.path().as_os_str()));
    }
}
