//! UI-independent process lifecycle and course library access.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, SystemTime},
};

#[derive(Clone, Debug, Deserialize)]
pub struct Completed {
    pub out_dir: PathBuf,
    pub title: String,
    pub slides: usize,
    pub segments: usize,
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
    Workers {
        stage: String,
        workers: usize,
    },
    Done(Completed),
    Error {
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
        Self::spawn(resolve_cli()?, args)
    }
    fn spawn(bin: PathBuf, args: Vec<String>) -> Result<Self> {
        let mut command = Command::new(&bin);
        command
            .args(args)
            .env("PATH", tool_path())
            .stdin(Stdio::null())
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
        let (tx, events) = mpsc::sync_channel(512);
        let (cancel, cancelled) = mpsc::channel();
        let stdout = child.stdout.take().context("缺少 stdout 管道")?;
        let stderr = child.stderr.take().context("缺少 stderr 管道")?;
        let readers = [
            reader(stdout, tx.clone(), true),
            reader(stderr, tx.clone(), false),
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
            for reader in readers {
                let _ = reader.join();
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
fn reader(
    stream: impl std::io::Read + Send + 'static,
    tx: SyncSender<Event>,
    json: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let event = match line {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => {
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
}
impl Environment {
    pub fn detect() -> Self {
        let cli = resolve_cli().ok();
        let checks = std::thread::scope(|scope| {
            let commands = [
                (
                    cli.as_deref().unwrap_or(Path::new("course2md")),
                    "--version",
                ),
                (Path::new("ffmpeg"), "-version"),
                (Path::new("ffprobe"), "-version"),
                (Path::new("yt-dlp"), "--version"),
                (Path::new("llama-server"), "--list-devices"),
            ];
            commands
                .map(|(bin, arg)| scope.spawn(move || probe(bin, &[arg])))
                .map(|task| task.join().unwrap_or(None))
        });
        let gpu = checks[4].as_deref().and_then(|output| {
            output.lines().find_map(|line| {
                let (id, description) = line.trim().split_once(':')?;
                (id.starts_with("MTL")
                    || id.starts_with("CUDA")
                    || id.starts_with("Vulkan")
                    || id.starts_with("SYCL")
                    || id.starts_with("ROCm"))
                .then(|| {
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
                "Get-CimInstance Win32_PnPEntity | Where-Object { $_.Name -match 'Intel.*(AI Boost|NPU)' -and $_.Status -eq 'OK' } | Select-Object -ExpandProperty Name"])
                .is_some_and(|name| !name.trim().is_empty())
        } else {
            false
        };
        let npu = npu_device
            && ["uv", "python3", "python"].iter().any(|name| {
                let executable = format!("{name}{}", std::env::consts::EXE_SUFFIX);
                std::env::split_paths(&tool_path()).any(|dir| dir.join(&executable).is_file())
            });
        Self {
            engine: cli.is_some() && checks[0].is_some(),
            ffmpeg: checks[1].is_some(),
            ffprobe: checks[2].is_some(),
            ytdlp: checks[3].is_some(),
            llama: checks[4].is_some(),
            apple,
            gpu,
            npu,
        }
    }
    pub fn ready(&self) -> bool {
        self.engine && self.ffmpeg && self.ffprobe
    }
}

/// Bounded, executable checks. Redirect output to a file so a verbose tool cannot
/// fill a pipe while the detector waits; timed-out children are always reaped.
fn probe(bin: &Path, args: &[&str]) -> Option<String> {
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
            Ok(None) if started.elapsed() < Duration::from_secs(5) => {
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

#[derive(Clone)]
pub struct Course {
    pub dir: PathBuf,
    pub title: String,
    pub modified: SystemTime,
    pub slides: usize,
    pub segments: usize,
    pub thumbnail: Option<PathBuf>,
}
impl Course {
    pub fn from_completed(done: &Completed) -> Self {
        Self {
            dir: done.out_dir.clone(),
            title: done.title.clone(),
            modified: SystemTime::now(),
            slides: done.slides,
            segments: done.segments,
            thumbnail: None,
        }
    }
}
pub fn library(root: &Path) -> Result<Vec<Course>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut courses = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0)]);
    while let Some((dir, depth)) = queue.pop_front() {
        if dir.join("run.json").is_file() {
            let title = std::fs::read(dir.join("meta.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|v| v["title"].as_str().map(str::to_owned))
                .unwrap_or_else(|| {
                    dir.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            let modified = dir.join("run.json").metadata()?.modified()?;
            let run: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.join("run.json"))?).unwrap_or_default();
            let thumbnail = if dir.join("cover.jpg").is_file() {
                Some(dir.join("cover.jpg"))
            } else {
                frame_paths(&dir).into_iter().next()
            };
            courses.push(Course {
                slides: run["sections"].as_u64().unwrap_or(0) as usize,
                segments: run["speech_segments"].as_u64().unwrap_or(0) as usize,
                thumbnail,
                dir,
                title,
                modified,
            });
        } else if depth < 4 {
            for entry in
                std::fs::read_dir(&dir).with_context(|| format!("读取 {}", dir.display()))?
            {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    queue.push_back((entry.path(), depth + 1));
                }
            }
        }
    }
    courses.sort_by_key(|course| std::cmp::Reverse(course.modified));
    Ok(courses)
}

pub fn default_output() -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    PathBuf::from(home.unwrap_or_else(|| ".".into())).join("Documents/course2md")
}

#[derive(Clone)]
pub enum PreviewBlock {
    Markdown(String),
    Image(PathBuf),
}
#[derive(Clone)]
pub struct Preview {
    pub course: Course,
    pub markdown: String,
    pub blocks: Vec<PreviewBlock>,
    pub frames: Vec<PathBuf>,
    pub has_markdown: bool,
    pub outputs: Vec<String>,
}

fn frame_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(root) = dir.canonicalize() else {
        return Vec::new();
    };
    let mut paths = std::fs::read_dir(root.join("frames"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.path().canonicalize().ok())
        .filter(|path| {
            path.starts_with(&root)
                && path.is_file()
                && matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("jpg" | "jpeg" | "png")
                )
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

pub fn read_preview(course: Course) -> Result<Preview> {
    let path = course.dir.join("course.md");
    let run = std::fs::read(course.dir.join("run.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let formats = run.as_ref().and_then(|run| run["formats"].as_array());
    let outputs = [
        ("md", "course.md"),
        ("html", "course.html"),
        ("json", "structured.json"),
    ]
    .into_iter()
    .filter(|(format, name)| {
        course.dir.join(name).is_file()
            && formats
                .is_none_or(|formats| formats.iter().any(|value| value.as_str() == Some(*format)))
    })
    .map(|(_, name)| name.to_owned())
    .collect::<Vec<_>>();
    let has_markdown = outputs.iter().any(|name| name == "course.md");
    let markdown = if has_markdown {
        std::fs::read_to_string(path)?
    } else {
        "这份课程未导出 Markdown。请使用上方按钮打开 HTML 或 JSON 产物。".into()
    };
    let root = course.dir.canonicalize()?;
    let mut blocks = Vec::new();
    let mut text = String::new();
    // The page header already presents the title and statistics. Keep the full
    // original document for copying, but start the reader at its first section.
    let body = if markdown.contains("- 由 course2md 生成") {
        markdown
            .split_once("\n## ")
            .map(|(_, body)| format!("## {body}"))
            .unwrap_or_else(|| markdown.clone())
    } else {
        markdown.clone()
    };
    for line in body.lines() {
        let image = line
            .strip_prefix("![")
            .and_then(|line| line.split_once("]("))
            .and_then(|(_, path)| path.strip_suffix(')'))
            .filter(|path| path.starts_with("frames/"))
            .and_then(|path| root.join(path).canonicalize().ok())
            .filter(|path| path.starts_with(&root) && path.is_file());
        if let Some(image) = image {
            if !text.is_empty() {
                blocks.push(PreviewBlock::Markdown(std::mem::take(&mut text)));
            }
            blocks.push(PreviewBlock::Image(image));
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    if !text.is_empty() {
        blocks.push(PreviewBlock::Markdown(text));
    }
    Ok(Preview {
        frames: frame_paths(&course.dir),
        has_markdown,
        outputs,
        course,
        markdown,
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_event_uses_the_cli_protocol() {
        let event: Event = serde_json::from_str(r#"{"type":"done","out_dir":"out/test","title":"课程","slides":3,"segments":4,"chars":50,"elapsed_secs":1.5,"outputs":["course.md"]}"#).unwrap();
        assert!(matches!(event, Event::Done(Completed { slides: 3, .. })));
    }

    #[test]
    fn preview_hides_exports_left_over_from_a_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("course.md"), "# Updated notes").unwrap();
        std::fs::write(dir.path().join("structured.json"), "{}").unwrap();
        std::fs::write(dir.path().join("run.json"), r#"{"formats":["md"]}"#).unwrap();
        let preview = read_preview(Course {
            dir: dir.path().into(),
            title: "course".into(),
            modified: SystemTime::now(),
            slides: 0,
            segments: 0,
            thumbnail: None,
        })
        .unwrap();
        assert_eq!(preview.outputs, ["course.md"]);
        assert!(preview.has_markdown);
    }

    #[test]
    fn preview_resolves_only_images_inside_the_course() {
        let dir = tempfile::tempdir().unwrap();
        let course_dir = dir.path().join("course");
        std::fs::create_dir_all(course_dir.join("frames")).unwrap();
        std::fs::write(course_dir.join("frames/slide.jpg"), b"image").unwrap();
        std::fs::write(dir.path().join("outside.jpg"), b"private").unwrap();
        std::fs::write(
            course_dir.join("course.md"),
            "# Course\n![slide](frames/slide.jpg)\n![escape](frames/../../outside.jpg)\n",
        )
        .unwrap();
        let preview = read_preview(Course {
            dir: course_dir,
            title: "Course".into(),
            modified: SystemTime::now(),
            slides: 0,
            segments: 0,
            thumbnail: None,
        })
        .unwrap();
        assert_eq!(
            preview
                .blocks
                .iter()
                .filter(|b| matches!(b, PreviewBlock::Image(_)))
                .count(),
            1
        );
        assert!(preview.markdown.contains("outside.jpg"));
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
}
