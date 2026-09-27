//! UI-independent process lifecycle and course library access.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Completed {
    pub out_dir: PathBuf,
    pub title: String,
    pub slides: usize,
    pub segments: usize,
    #[serde(default)]
    pub partial: Option<bool>,
    #[serde(default)]
    pub outcomes: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    Log {
        message: String,
    },
    Stage {
        stage: String,
        status: String,
    },
    Progress {
        stage: String,
        current: u64,
        total: u64,
        message: Option<String>,
    },
    Tokens {
        stage: String,
        prompt: u64,
        completion: u64,
    },
    Workers {
        stage: String,
        workers: usize,
    },
    Done(Completed),
    Error {
        message: String,
    },
    Blocked {
        reason: String,
        request_id: Option<String>,
        purpose: Option<String>,
        #[serde(default)]
        description: Option<String>,
        message: String,
    },
    #[serde(skip)]
    Exit {
        success: bool,
        cancelled: bool,
    },
}

pub struct Job {
    pub events: Receiver<Event>,
    cancel: mpsc::Sender<()>,
}
impl Job {
    pub fn start(args: Vec<String>) -> Result<Self> {
        Self::spawn_input(resolve_cli()?, args, None)
    }
    #[cfg(test)]
    fn spawn(bin: PathBuf, args: Vec<String>) -> Result<Self> {
        Self::spawn_input(bin, args, None)
    }
    pub fn start_task(request: &course2md::execution::Request) -> Result<Self> {
        Self::spawn_input(
            resolve_cli()?,
            vec!["run-task".into()],
            Some(serde_json::to_vec(request)?),
        )
    }
    fn spawn_input(bin: PathBuf, args: Vec<String>, mut input: Option<Vec<u8>>) -> Result<Self> {
        let redactions = std::sync::Arc::new(Redactions(
            input
                .as_ref()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
                .map(|value| {
                    [
                        value["config"]["llm"]["api_key"].as_str(),
                        value["config"]["asr_api"]["api_key"].as_str(),
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|key| !key.is_empty())
                    .map(str::to_owned)
                    .collect()
                })
                .unwrap_or_default(),
        ));
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
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("启动 {} 失败", bin.display()))?;
        if let Some(bytes) = input.as_mut() {
            let sent = child
                .stdin
                .take()
                .context("无法传递任务参数")
                .and_then(|mut pipe| pipe.write_all(bytes).context("传递任务参数失败"));
            use zeroize::Zeroize;
            bytes.zeroize();
            if let Err(error) = sent {
                terminate(&mut child);
                let _ = child.wait();
                return Err(error);
            }
        }
        let (tx, events) = mpsc::sync_channel(512);
        let (cancel, cancelled) = mpsc::channel();
        let stdout = child.stdout.take().context("缺少 stdout 管道")?;
        let stderr = child.stderr.take().context("缺少 stderr 管道")?;
        let readers = [
            reader(stdout, tx.clone(), true, redactions.clone()),
            reader(stderr, tx.clone(), false, redactions),
        ];
        thread::spawn(move || {
            let mut was_cancelled = false;
            let success = loop {
                // Reap and cancel on the same thread: a stale PID cannot be killed
                // after completion, and queued stdout is drained before Exit.
                match child.try_wait() {
                    Ok(Some(status)) => break status.success(),
                    Err(error) => {
                        let _ = tx.send(Event::Error {
                            message: error.to_string(),
                        });
                        terminate(&mut child);
                        let _ = child.wait();
                        break false;
                    }
                    Ok(None) => {}
                }
                if !matches!(cancelled.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    was_cancelled = true;
                    terminate(&mut child);
                    let _ = child.wait();
                    break false;
                }
                thread::sleep(Duration::from_millis(40));
            };
            // 有界排空：子进程已死则管道必将 EOF，但卡住的 reader 不得把 Exit 永远挡在
            // 门外（否则 job 永远存在、后续任务全部排队）。读不到的剩余输出允许丢失。
            let drain_deadline = Instant::now() + Duration::from_secs(3);
            for reader in readers {
                while !reader.is_finished() && Instant::now() < drain_deadline {
                    thread::sleep(Duration::from_millis(20));
                }
                // 仍未完成的 reader：进程已死，线程会随管道 EOF 自行结束
            }
            let _ = tx.send(Event::Exit {
                success,
                cancelled: was_cancelled,
            });
        });
        Ok(Self { events, cancel })
    }
    pub fn cancel(&self) {
        let _ = self.cancel.send(());
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}
struct Redactions(Vec<String>);
impl Redactions {
    fn text(&self, input: &str) -> String {
        let mut result = input.to_owned();
        // Long keys first: a shorter credential may be a prefix of another one.
        let mut keys: Vec<_> = self.0.iter().filter(|key| !key.is_empty()).collect();
        keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
        for key in keys {
            result = result.replace(key, "[已隐藏密钥]");
            // Tool diagnostics can contain JSON embedded in ordinary stderr text.
            let encoded = serde_json::to_string(key).unwrap_or_default();
            if encoded.len() > 2 {
                result = result.replace(&encoded[1..encoded.len() - 1], "[已隐藏密钥]");
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
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let event = match line {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => {
                    let line = redactions.line(&line);
                    if json {
                        serde_json::from_str(&line).unwrap_or(Event::Log { message: line })
                    } else {
                        Event::Log { message: line }
                    }
                }
                Err(error) => Event::Error {
                    message: format!("读取任务输出失败：{error}"),
                },
            };
            if tx.send(event).is_err() {
                break;
            }
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
            .creation_flags(0x08000000)
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

pub fn tool_path() -> std::ffi::OsString {
    if let Some(path) = std::env::var_os("COURSE2MD_TOOL_PATH").filter(|path| !path.is_empty()) {
        return path;
    }
    let mut dirs: Vec<_> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
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
            "COURSE2MD_BIN 指向的文件不存在：{}",
            path.display()
        );
        return Ok(path);
    }
    let name = format!("course2md{}", std::env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()?.parent() {
        candidates.push(dir.join(&name));
    }
    candidates.extend(std::env::split_paths(&tool_path()).map(|dir| dir.join(&name)));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .context("找不到 course2md 引擎。请将 CLI 放在应用旁，或设置 COURSE2MD_BIN。")
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
    pub fn detect() -> Self {
        let cli = resolve_cli().ok();
        // (字段名, 命令, 参数) 与回填写在一起：数组重排不会造成静默错配
        let checks = std::thread::scope(|scope| {
            let commands: Vec<(&'static str, &Path, &'static str)> = vec![
                (
                    "engine",
                    cli.as_deref().unwrap_or(Path::new("course2md")),
                    "--version",
                ),
                ("ffmpeg", Path::new("ffmpeg"), "-version"),
                ("ffprobe", Path::new("ffprobe"), "-version"),
                ("ytdlp", Path::new("yt-dlp"), "--version"),
                ("llama-devices", Path::new("llama-server"), "--list-devices"),
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
                    (name, scope.spawn(move || probe(bin, &[arg], timeout)))
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
                .as_ref()
                .and_then(|path| path.parent())
                .is_some_and(|dir| {
                    ["mlx.metallib", "default.metallib"]
                        .iter()
                        .any(|name| dir.join(name).is_file())
                });
        let npu_device = if cfg!(target_os = "linux") {
            std::fs::read_to_string("/sys/class/accel/accel0/device/vendor")
                .is_ok_and(|vendor| vendor.trim() == "0x8086")
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
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(25))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    output.rewind().ok()?;
    let mut text = String::new();
    output.take(32 * 1024).read_to_string(&mut text).ok()?;
    Some(text)
}

pub use crate::notes::{Course, Preview, read_preview, scan_library};

/// macOS「始终显示滚动条」系统偏好：探测一次并缓存（states-and-motion 滚动条契约要求尊重该偏好）。
/// 非 macOS 或读取失败按 Automatic（滚动时显示）。
pub fn scrollbars_always_visible() -> bool {
    static ALWAYS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ALWAYS.get_or_init(|| {
        if !cfg!(target_os = "macos") {
            return false;
        }
        std::process::Command::new("defaults")
            .args(["read", "-g", "AppleShowScrollBars"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|out| out.status.success())
            .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "Always")
    })
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
        let event: Event = serde_json::from_str(r#"{"type":"done","out_dir":"out/test","title":"课程","slides":3,"segments":4,"chars":50,"elapsed_secs":1.5,"outputs":["course.md"]}"#).unwrap();
        assert!(matches!(event, Event::Done(Completed { slides: 3, .. })));
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
             {{\"type\":\"blocked\",\"reason\":\"paused\",\"request_id\":null,\"purpose\":null,\"description\":null,\"message\":\"paused\"}}\n"
        );
        let (tx, rx) = mpsc::sync_channel(8);
        let handle = reader(std::io::Cursor::new(source), tx, true, redactions.clone());
        handle.join().unwrap();
        match rx.recv().unwrap() {
            Event::Error { message } => assert_eq!(message, "failed [已隐藏密钥]"),
            _ => panic!("protocol was lost during redaction"),
        }
        assert!(matches!(rx.recv().unwrap(), Event::Blocked { .. }));
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
        let manifest = smol::block_on(course2md::artifact::publish(
            &target,
            &work,
            &meta,
            &sections,
            None,
            &[],
            Default::default(),
        ))
        .unwrap();
        let preview = read_preview(Course {
            dir: target.version_dir(),
            title: "course".into(),
            modified: SystemTime::now(),
            slides: 0,
            segments: 1,
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
        smol::block_on(course2md::artifact::publish(
            &target,
            &work,
            &meta,
            &sections,
            None,
            &[],
            Default::default(),
        ))
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
            segments: 0,
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
        assert!(
            !preview
                .frames
                .iter()
                .any(|path| path.ends_with("outside.jpg"))
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
                Event::Log { .. } => lines += 1,
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
    #[cfg(unix)]
    fn cancellation_terminates_running_process_tree() {
        let job = Job::spawn(
            "/bin/sh".into(),
            vec!["-c".into(), "sleep 60 & echo ready; wait".into()],
        )
        .unwrap();
        assert!(matches!(
            job.events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Log { .. }
        ));
        job.cancel();
        // Exit is emitted only after both inherited child pipes close, so this
        // also checks that the descendant (sleep) was terminated.
        assert!(matches!(
            job.events.recv_timeout(Duration::from_secs(5)).unwrap(),
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
