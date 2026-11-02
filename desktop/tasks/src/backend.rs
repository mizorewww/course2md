//! UI-independent process lifecycle and course library access.
use anyhow::{Context, Result};
use desktop_prefs::environment::{resolve_cli, tool_path};
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
        Self::Engine(EngineEvent::Error {
            message,
            code: Default::default(),
        })
    }
}

/// A child process reaped on its own thread. `kill` cannot reach a recycled
/// PID: on Unix the reaper first waits without reaping (`WNOWAIT`), so the
/// zombie keeps its PID reserved until the reaper marks it reaped under the
/// same lock `kill` takes; on Windows the open process handle reserves it.
pub struct Watched {
    shared: std::sync::Arc<WatchedProcess>,
}
struct WatchedProcess {
    pid: u32,
    state: std::sync::Mutex<WatchState>,
}
#[derive(Default)]
struct WatchState {
    reaped: bool,
    killed: bool,
}
impl Watched {
    /// Reaps `child`, the leader of its own process group, on a dedicated
    /// thread and then calls `on_exit` there with its status and whether `kill`
    /// stopped it.
    pub fn spawn(
        mut child: std::process::Child,
        on_exit: impl FnOnce(std::io::Result<std::process::ExitStatus>, bool) + Send + 'static,
    ) -> Self {
        let shared = std::sync::Arc::new(WatchedProcess {
            pid: child.id(),
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

    /// Stops the process and its group (Unix) / tree (Windows) unless it has
    /// already been reaped. Returns whether it was still running.
    pub fn kill(&self) -> bool {
        let mut state = course2md::runtime::lock_recover(&self.shared.state);
        if state.reaped {
            return false;
        }
        state.killed = true;
        course2md::runtime::kill_process(self.shared.pid, true, true);
        true
    }
}

pub struct Job {
    pub events: Receiver<Event>,
    process: Watched,
}
impl Job {
    pub fn start(args: Vec<String>, wake: smol::channel::Sender<()>) -> Result<Self> {
        Self::spawn_input(resolve_cli()?, args, None, Redactions(vec![]), None, wake)
    }
    #[cfg(all(test, unix))]
    fn spawn(bin: PathBuf, args: Vec<String>) -> Result<Self> {
        let (wake, _) = smol::channel::bounded(1);
        Self::spawn_input(bin, args, None, Redactions(vec![]), None, wake)
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
            Some(EventLog::open(
                &request
                    .work_dir
                    .join(desktop_store::workspace::TASK_EVENTS_FILE),
            )?),
            wake,
        )
    }
    fn spawn_input(
        bin: PathBuf,
        args: Vec<String>,
        input: Option<Vec<u8>>,
        redactions: Redactions,
        log: Option<EventLog>,
        wake: smol::channel::Sender<()>,
    ) -> Result<Self> {
        let redactions = std::sync::Arc::new(redactions);
        let log = log.map(std::sync::Arc::new);
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
        let mut child = course2md::runtime::hide_console(&mut command)
            .spawn()
            .with_context(|| course2md::tr!("启动 {} 失败", bin.display()))?;
        // All three were set up above: stdin only with an input.
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let stdin = child.stdin.take();
        let (tx, events) = mpsc::sync_channel(512);
        // Readers start before the request is written: a request larger than the
        // pipe buffer must not wait on an engine that is blocked writing output.
        let (finished, drained) = mpsc::channel();
        reader(
            stdout,
            tx.clone(),
            true,
            redactions.clone(),
            log.clone(),
            wake.clone(),
            finished.clone(),
        );
        reader(
            stderr,
            tx.clone(),
            false,
            redactions,
            log,
            wake.clone(),
            finished,
        );
        let process = Watched::spawn(child, {
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
/// A task's `events.jsonl`: what it said, after redaction, so its card reads the same after
/// a restart and in another copy of the library. Steady progress is thinned to one sample
/// a second; the newest skipped sample goes out before anything that is not progress.
struct EventLog(std::sync::Mutex<EventLogState>);

struct EventLogState {
    file: std::fs::File,
    sampled: Option<Instant>,
    skipped: Option<EngineEvent>,
}

impl EventLog {
    fn open(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| course2md::tr!("无法读取 {}", path.display()))?;
        Ok(Self(std::sync::Mutex::new(EventLogState {
            file,
            sampled: None,
            skipped: None,
        })))
    }

    /// A failed write costs only detail in a task's history, never its state.
    fn record(&self, event: &EngineEvent) {
        let mut log = course2md::runtime::lock_recover(&self.0);
        if matches!(event, EngineEvent::Progress { .. }) {
            if log
                .sampled
                .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
            {
                log.skipped = Some(event.clone());
                return;
            }
            log.sampled = Some(Instant::now());
        }
        let mut lines = Vec::new();
        for event in log.skipped.take().iter().chain([event]) {
            if serde_json::to_writer(&mut lines, event).is_ok() {
                lines.push(b'\n');
            }
        }
        let _ = log.file.write_all(&lines);
    }
}

fn reader(
    stream: impl std::io::Read + Send + 'static,
    tx: SyncSender<Event>,
    json: bool,
    redactions: std::sync::Arc<Redactions>,
    log: Option<std::sync::Arc<EventLog>>,
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
        let mut stream = BufReader::new(stream);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            let event = match stream.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                // One undecodable byte must not cost the rest of its line.
                Ok(_) => {
                    let line = String::from_utf8_lossy(&bytes);
                    let line = line.trim_end_matches(['\n', '\r']);
                    if line.trim().is_empty() {
                        continue;
                    }
                    line_event(line, json, &redactions)
                }
                // A failed stream stays failed; reading it again would only repeat this.
                Err(error) => {
                    let _ = tx.send(Event::error(course2md::tr!("读取任务输出失败：{}", error)));
                    let _ = wake.try_send(());
                    break;
                }
            };
            if let (Some(log), Event::Engine(event)) = (&log, &event) {
                log.record(event);
            }
            if tx.send(event).is_err() {
                break;
            }
            let _ = wake.try_send(());
        }
    })
}

fn line_event(line: &str, json: bool, redactions: &Redactions) -> Event {
    let line = redactions.line(line);
    if !json {
        return Event::log(line);
    }
    match serde_json::from_str(&line) {
        Ok(event) => Event::Engine(event),
        // A JSON object this build does not recognize is a
        // protocol drift, not a log line; keep it visible.
        Err(error) if line.starts_with('{') => {
            Event::log(course2md::tr!("无法识别的任务事件（{}）：{}", error, line))
        }
        Err(_) => Event::log(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_store::notes::{Course, PreviewBlock, read_preview};
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
            None,
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
            None,
            wake,
            mpsc::channel().0,
        );
        handle.join().unwrap();
        match rx.recv().unwrap() {
            Event::Engine(EngineEvent::Error { message, .. }) => {
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
            None,
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
    fn a_task_log_keeps_what_was_said_thins_progress_and_saves_after_redaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let log = std::sync::Arc::new(EventLog::open(&path).unwrap());
        let progress = |current| {
            format!(
                "{{\"type\":\"progress\",\"stage\":\"transcribe\",\"current\":{current},\"total\":9}}\n"
            )
        };
        let mut source = String::new();
        for current in 1..=9 {
            source.push_str(&progress(current));
        }
        source.push_str("{\"type\":\"log\",\"level\":\"info\",\"message\":\"key sk-secret\"}\n");
        let (tx, rx) = mpsc::sync_channel(32);
        let (wake, _) = smol::channel::bounded(1);
        reader(
            std::io::Cursor::new(source),
            tx,
            true,
            std::sync::Arc::new(Redactions(vec!["sk-secret".into()])),
            Some(log),
            wake,
            mpsc::channel().0,
        )
        .join()
        .unwrap();
        // The live stream misses nothing; the saved one only skips steady progress.
        assert_eq!(rx.iter().count(), 10);
        let saved = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&saved);
        assert!(!text.contains("sk-secret"));
        assert!(text.lines().count() < 10);
        let mut replayed = desktop_store::activity::JobEvents::default();
        replayed.replay(&saved);
        let stage = &replayed.stages[&course2md::progress::Stage::Transcribe];
        assert_eq!((stage.current, stage.total), (9, 9));
        assert_eq!(replayed.logs.len(), 1);
    }

    fn read_all(stream: impl std::io::Read + Send + 'static, json: bool) -> Vec<Event> {
        let (tx, rx) = mpsc::sync_channel(8);
        let (wake, _) = smol::channel::bounded(1);
        let reading = reader(
            stream,
            tx,
            json,
            std::sync::Arc::new(Redactions(vec![])),
            None,
            wake,
            mpsc::channel().0,
        );
        let events = rx.iter().collect();
        reading.join().unwrap();
        events
    }

    #[test]
    fn a_line_with_invalid_utf8_keeps_its_text_and_the_lines_after_it() {
        let events = read_all(
            std::io::Cursor::new(b"before\nbad \xff byte\r\nafter".to_vec()),
            false,
        );
        let lines: Vec<_> = events
            .iter()
            .map(|event| match event {
                Event::Engine(EngineEvent::Log { message, .. }) => message.as_str(),
                other => panic!("expected a log line, got {other:?}"),
            })
            .collect();
        assert_eq!(lines, ["before", "bad \u{fffd} byte", "after"]);
    }

    #[test]
    fn a_stream_that_keeps_failing_reports_once_and_ends() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("gone"))
            }
        }
        let events = read_all(Broken, true);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            Event::Engine(EngineEvent::Error { message, .. }) if message.contains("gone")
        ));
    }
}
