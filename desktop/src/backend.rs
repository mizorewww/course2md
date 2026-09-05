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
    pub elapsed_secs: f64,
    pub outputs: Vec<String>,
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
fn terminate(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::killpg(child.id() as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
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
pub struct Course {
    pub dir: PathBuf,
    pub title: String,
    pub modified: SystemTime,
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
            courses.push(Course {
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
}

pub fn read_preview(course: Course) -> Result<Preview> {
    let path = course.dir.join("course.md");
    let markdown = if path.is_file() {
        std::fs::read_to_string(path)?
    } else {
        "这份课程未导出 Markdown。请使用上方按钮打开 HTML 或 JSON 产物。".into()
    };
    let root = course.dir.canonicalize()?;
    let mut blocks = Vec::new();
    let mut text = String::new();
    for line in markdown.lines() {
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
