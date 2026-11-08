//! End-to-end stdin task protocol, using real ffmpeg and isolated local files only.
mod common;

use course2md::execution::Component;
use course2md::{
    artifact,
    config::{AsrProvider, SlideMode, TranscriptSource},
    execution::Request,
    settings::ConfigFile,
    timeline::TranscriptEvent,
};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

fn request(root: &Path, source: &Path) -> Request {
    let mut config = ConfigFile::default();
    config.defaults.formats = Some(vec![]);
    config.defaults.provider = Some(AsrProvider::Api);
    config.defaults.transcript_source = Some(TranscriptSource::Subtitle);
    config.defaults.slide_mode = Some(SlideMode::First);
    Request {
        schema: 1,
        operation: Default::default(),
        task_id: "task-one".into(),
        course_id: "course-one".into(),
        version_id: None,
        source: source.display().to_string(),
        source_id: format!(
            "local:sha256:{}",
            course2md::execution::file_digest(source).unwrap()
        ),
        title: "我确认的笔记名称".into(),
        author: String::new(),
        duration: 1.,
        subtitle: None,
        subtitle_events: Some(vec![TranscriptEvent {
            start: 0.,
            end: 1.,
            text: "这里是已选字幕中的实际正文。".into(),
            raw: None,
            translation: None,
        }]),
        config,
        allow_unauthenticated_asr: false,
        work_dir: root.join("work"),
        course_dir: root.join("course"),
        control_path: None,
        service_versions: Default::default(),
    }
}
/// Start `course2md run-task` with the request on stdin, isolated from the
/// developer's configuration, cache and environment credentials. A broken user
/// config file proves the task never reads it; the environment key must never
/// reach a task. `configure` adjusts the command before it starts.
fn spawn_task(root: &Path, request: &Request, configure: impl FnOnce(&mut Command)) -> Child {
    let config = root.join("config/course2md");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), "[broken").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_course2md"));
    command
        .env("COURSE2MD_LANG", "zh-Hans")
        .arg("run-task")
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        // Nothing the task or a tool it starts (the python3 wrapper below) keeps
        // per user may land in the person's home: Apple's python3 caches bytecode
        // under ~/Library/Caches.
        .env("HOME", root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("COURSE2MD_ASR_API_KEY", "environment-key-must-not-be-used")
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut command);
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(request).unwrap())
        .unwrap();
    child
}
fn run(root: &Path, request: &Request) -> Output {
    spawn_task(root, request, |_| {})
        .wait_with_output()
        .unwrap()
}
fn events(output: &Output) -> Vec<serde_json::Value> {
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
#[cfg(unix)]
fn scanning_finishes_before_a_slow_screenshot_reports_progress_with_estimated_total() {
    use std::{
        io::{BufRead, BufReader},
        os::unix::fs::PermissionsExt,
        sync::mpsc,
        time::{Duration, Instant},
    };
    if !common::tools_available(&["ffmpeg", "ffprobe", "python3"]) {
        return;
    }
    let ffmpeg = which::which("ffmpeg").ok().unwrap();
    let python = which::which("python3").ok().unwrap();
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("flat-video.mp4");
    common::fixture_video(&video, 18);
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let gate = root.path().join("extract-started");
    let release = root.path().join("extract-release");
    let wrapper = bin.join("ffmpeg");
    let py_literal = |path: &Path| serde_json::to_string(path.to_str().unwrap()).unwrap();
    std::fs::write(&wrapper,format!("#!{}\nimport os,sys,time,pathlib\nif '-frames:v' in sys.argv:\n    pathlib.Path({}).write_text('started')\n    deadline=time.monotonic()+10\n    while not pathlib.Path({}).exists() and time.monotonic()<deadline:\n        time.sleep(0.01)\nos.execv({},[{},*sys.argv[1:]])\n",python.display(),py_literal(&gate),py_literal(&release),py_literal(&ffmpeg),py_literal(&ffmpeg))).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut request = request(root.path(), &video);
    request.duration = 18.;
    request.config.defaults.sample_interval = Some(1.);
    let mut child = spawn_task(root.path(), &request, |command| {
        command.env("PATH", std::env::join_paths(paths).unwrap());
    });
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let value: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    let mut observed = Vec::new();
    loop {
        let event = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("new screenshot progress must be emitted before ffmpeg finishes");
        let initial = event["type"] == "progress" && event["stage"] == "scenes/extract";
        if initial {
            assert_eq!(event["current"], 0);
            assert_eq!(event["total"], 1);
        }
        observed.push(event);
        if initial {
            break;
        }
    }
    assert!(
        observed
            .iter()
            .any(|event| event["stage"] == "scenes/scan" && event["status"] == "done")
    );
    let scan = observed
        .iter()
        .rev()
        .find(|event| event["stage"] == "scenes/scan" && event["type"] == "progress")
        .unwrap();
    assert_eq!(scan["current"], 18);
    assert_eq!(
        scan["total"], 18,
        "total is the duration/interval estimate; CFR fixtures match exactly"
    );
    // The gate is the synchronization point: once the wrapper has written it,
    // the real screenshot ffmpeg is blocked and cannot have produced a file, so
    // every event received so far was emitted while no screenshot existed.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !gate.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        gate.exists(),
        "real screenshot subprocess should now be blocked at the gate"
    );
    observed.extend(rx.try_iter());
    let released_at = observed.len();
    std::fs::write(&release, b"continue").unwrap();
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    observed.extend(rx.try_iter());
    assert!(
        output.status.success(),
        "{observed:#?}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let extract_finished = |event: &serde_json::Value| {
        event["stage"] == "scenes/extract"
            && ((event["current"] == 1 && event["total"] == 1) || event["status"] == "done")
    };
    let saved = observed
        .iter()
        .position(extract_finished)
        .expect("the screenshot is reported saved");
    assert!(
        saved >= released_at,
        "unfinished ffmpeg output must not count as a saved screenshot: {observed:#?}"
    );
    assert!(
        observed
            .iter()
            .any(|event| event["stage"] == "scenes/extract" && event["status"] == "done")
    );
    assert!(!observed.iter().any(|event| event["stage"] == "scenes"));
}

#[test]
fn explicit_subtitles_zero_exports_and_confirmed_title_survive_real_conversion_and_resume() {
    if !common::ffmpeg_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("different-original-name.mp4");
    common::fixture_video(&video, 1);
    let mut request = request(root.path(), &video);
    let first = run(root.path(), &request);
    let first_events = events(&first);
    assert!(first.status.success(), "{first_events:#?}");
    let done = first_events
        .iter()
        .find(|event| event["type"] == "done")
        .unwrap();
    assert_eq!(done["title"], request.title);
    assert_eq!(done["outputs"], serde_json::json!([]));
    assert!(
        !first_events
            .iter()
            .any(|event| event["stage"] == "transcribe" || event["stage"] == "audio")
    );
    let version = request.course_dir.clone();
    let manifest = artifact::read_manifest(&version.join("manifest.json")).unwrap();
    assert!(!manifest.frames.is_empty());
    assert_eq!(manifest.task_id, request.task_id);
    artifact::validate_note(&version, &manifest).unwrap();
    let original = std::fs::read(version.join("document.json")).unwrap();
    assert!(String::from_utf8_lossy(&original).contains("这里是已选字幕中的实际正文"));
    let resumed = run(root.path(), &request);
    assert!(resumed.status.success(), "{:?}", events(&resumed));
    assert_eq!(
        std::fs::read(version.join("document.json")).unwrap(),
        original
    );
    request.title = "后来在添加页填写的不同标题".into();
    assert!(!run(root.path(), &request).status.success());
    assert_eq!(
        std::fs::read(version.join("document.json")).unwrap(),
        original
    );
    let persisted = std::fs::read_to_string(request.work_dir.join("task-identity.json")).unwrap();
    assert!(!persisted.contains("environment-key-must-not-be-used"));
}

/// A sidecar saved by a Windows subtitle editor: UTF-16 with a BOM.
#[test]
fn a_selected_subtitle_file_is_decoded_like_any_other_sidecar() {
    if !common::ffmpeg_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("lesson.mp4");
    common::fixture_video(&video, 1);
    let mut request = request(root.path(), &video);
    let subtitle = root.path().join("selected.srt");
    let mut bytes = vec![0xff, 0xfe];
    "1\n00:00:00,000 --> 00:00:01,000\n来自 UTF-16 字幕的正文\n"
        .encode_utf16()
        .for_each(|unit| bytes.extend_from_slice(&unit.to_le_bytes()));
    std::fs::write(&subtitle, bytes).unwrap();
    request.subtitle_events = None;
    request.subtitle = Some(subtitle);
    let output = run(root.path(), &request);
    assert!(output.status.success(), "{:#?}", events(&output));
    let document = request.course_dir.join("document.json");
    let text = std::fs::read_to_string(document).unwrap();
    assert!(text.contains("来自 UTF-16 字幕的正文"), "{text}");
}

#[test]
fn broken_video_preserves_readable_subtitles_as_partial_note_without_phantom_images() {
    if !common::ffmpeg_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("broken.mp4");
    std::fs::write(&video, b"broken media").unwrap();
    let request = request(root.path(), &video);
    let output = run(root.path(), &request);
    let observed = events(&output);
    assert!(output.status.success(), "{observed:#?}");
    let manifest = artifact::read_manifest(&request.course_dir.join("manifest.json")).unwrap();
    assert!(manifest.partial);
    assert!(manifest.frames.is_empty());
    assert_eq!(
        manifest.outcomes.screenshots.status,
        artifact::Status::Failed
    );
    let body = std::fs::read_to_string(request.course_dir.join("course.md")).unwrap();
    assert!(body.contains("这里是已选字幕"));
    assert!(!body.contains("![]"));
    assert!(!body.contains("本段无语音"));
}

#[test]
fn missing_frozen_cloud_credentials_fail_before_download_and_ignore_environment() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("local.mp4");
    std::fs::write(&source, b"input").unwrap();
    let mut request = request(root.path(), &source);
    request.source = "https://example.invalid/video".into();
    request.source_id = "online:example:video".into();
    request.subtitle_events = None;
    request.config.defaults.transcript_source = Some(TranscriptSource::Asr);
    let output = run(root.path(), &request);
    let observed = events(&output);
    assert!(!output.status.success());
    assert!(!observed.iter().any(|event| event["stage"] == "download"));
    assert!(observed.iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .unwrap_or_default()
                .contains("API Key")
    }));
    assert!(!request.work_dir.join("media.mp4").exists());
    assert!(!request.course_dir.join("manifest.json").exists());
}

#[test]
fn changed_local_source_is_rejected_before_using_the_confirmed_subtitles() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("changed.mp4");
    std::fs::write(&source, b"confirmed content").unwrap();
    let request = request(root.path(), &source);
    std::fs::write(&source, b"replacement content").unwrap();
    let result = run(root.path(), &request);
    assert!(!result.status.success());
    let observed = events(&result);
    // run-task messages stay in Simplified Chinese for the desktop app; this is
    // the suite's one check of that language, the others use stable fields.
    assert!(observed.iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .is_some_and(|s| s.contains("视频文件已在读取后发生变化"))
    }));
    assert!(!observed.iter().any(|event| event["type"] == "stage"));
    assert!(!request.course_dir.join("manifest.json").exists());
}

/// An OpenAI-compatible chat endpoint on a loopback port. `respond` receives
/// the zero-based request number and the parsed body; `None` closes the
/// connection without any response. Server-side failures are collected and
/// reported as the test's panic message when the mock is dropped, instead of
/// panicking on the worker thread.
struct MockAi {
    url: String,
    address: std::net::SocketAddr,
    calls: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    failures: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl MockAi {
    /// Loses the response to the first request, then answers proofreading and
    /// summary requests with the shared fixture samples.
    fn new() -> Self {
        let samples = common::fake_ai();
        let proofread_marker = samples["markers"]["proofread"].as_str().unwrap().to_owned();
        let proofread = samples["responses"]["proofread"].clone();
        let summary = samples["responses"]["summary"].clone();
        Self::respond_with(move |number, body| {
            if number == 0 {
                return None; // Request was received, but its response is lost.
            }
            let is_polish = body["messages"][0]["content"]
                .as_str()
                .unwrap_or_default()
                .contains(&proofread_marker);
            let content = if is_polish { &proofread } else { &summary };
            Some((200, chat_completion(content)))
        })
    }
    fn respond_with(
        respond: impl Fn(usize, &serde_json::Value) -> Option<(u16, serde_json::Value)> + Send + 'static,
    ) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, received, failed, shutdown) = (
            calls.clone(),
            bodies.clone(),
            failures.clone(),
            stop.clone(),
        );
        let worker = std::thread::spawn(move || {
            // Blocking accept; Drop sets `stop` and connects once to wake it.
            for stream in listener.incoming() {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let result = stream
                    .map_err(|error| format!("accepting a connection: {error}"))
                    .and_then(|mut stream| {
                        let body = read_json_request(&mut stream)?;
                        let number = count.fetch_add(1, Ordering::SeqCst);
                        received.lock().unwrap().push(body.clone());
                        // Returning without writing closes the connection unanswered.
                        let Some((status, json)) = respond(number, &body) else {
                            return Ok(());
                        };
                        let payload = serde_json::to_vec(&json).unwrap();
                        write!(
                            stream,
                            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        )
                        .and_then(|()| stream.write_all(&payload))
                        .map_err(|error| format!("writing response {number}: {error}"))
                    });
                if let Err(message) = result {
                    failed.lock().unwrap().push(message);
                }
            }
        });
        Self {
            url: format!("http://{address}"),
            address,
            calls,
            bodies,
            failures,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for MockAi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let failures = self.failures.lock().unwrap();
        if failures.is_empty() {
            return;
        }
        let report = format!("mock AI server failures:\n{}", failures.join("\n"));
        if std::thread::panicking() {
            eprintln!("{report}");
        } else {
            panic!("{report}");
        }
    }
}

/// Read one HTTP/1.1 request with a Content-Length body and parse the body as JSON.
fn read_json_request(stream: &mut std::net::TcpStream) -> Result<serde_json::Value, String> {
    // BSD may inherit the listener's mode; reads below block with a bound.
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut read = |data: &mut Vec<u8>| -> Result<(), String> {
        let size = stream
            .read(&mut buffer)
            .map_err(|error| format!("reading the request: {error}"))?;
        if size == 0 {
            return Err(format!(
                "connection closed mid-request after {} bytes",
                data.len()
            ));
        }
        data.extend_from_slice(&buffer[..size]);
        Ok(())
    };
    let header_end = loop {
        read(&mut data)?;
        if let Some(index) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .ok_or_else(|| format!("request without Content-Length:\n{header}"))?;
    while data.len() < header_end + length {
        read(&mut data)?;
    }
    let body = &data[header_end..header_end + length];
    serde_json::from_slice(body).map_err(|error| {
        format!(
            "request body is not JSON ({error}): {}",
            String::from_utf8_lossy(body)
        )
    })
}

/// Wrap a JSON content sample the way a chat completion carries it.
fn chat_completion(content: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"choices":[{"message":{"content":content.to_string()}}]})
}

fn summary_response() -> serde_json::Value {
    chat_completion(&common::fake_ai()["responses"]["summary"])
}

#[test]
fn proofreading_images_and_custom_rules_match_the_requested_outputs() {
    if !common::ffmpeg_available() {
        return;
    }
    let markers = common::fake_ai()["markers"].clone();
    let summary_marker = markers["summary"].as_str().unwrap().to_owned();
    let mock = MockAi::respond_with(move |_, body| {
        if body.to_string().contains(&summary_marker) {
            return Some((200, summary_response()));
        }
        // Echo the segments back as the proofread result.
        let user = &body["messages"][1]["content"];
        let text = user.as_str().map(str::to_owned).unwrap_or_else(|| {
            user.as_array()
                .unwrap()
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        });
        let segments: serde_json::Value = serde_json::from_str(&text).unwrap();
        Some((
            200,
            chat_completion(&serde_json::json!({"segments": segments})),
        ))
    });
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("slides.mp4");
    common::fixture_video(&video, 1);
    let mut initial = request(root.path(), &video);
    initial.config.llm.enabled = true;
    initial.config.llm.summarize = true;
    initial.config.llm.vision = true;
    let rule = markers["custom_rule"].as_str().unwrap();
    initial.config.llm.prompt = Some(format!("{rule}: preserve terminology."));
    initial.config.llm.base_url = mock.url.clone();
    initial.config.llm.model = "proofread-vision".into();
    initial.config.llm.api_key = "private-task-key".into();
    initial.config.defaults.formats = Some(vec![course2md::config::OutputFormat::Json]);
    initial
        .service_versions
        .insert("llm".into(), "service-v1".into());
    let output = run(root.path(), &initial);
    assert!(output.status.success(), "{:?}", events(&output));
    let manifest = artifact::read_manifest(&initial.course_dir.join("manifest.json")).unwrap();
    assert!(!manifest.partial, "{manifest:?}");
    assert_eq!(
        manifest.outcomes.proofreading.status,
        artifact::Status::Succeeded
    );
    assert_eq!(
        manifest.outcomes.summary.status,
        artifact::Status::Succeeded
    );
    assert_eq!(manifest.outputs, vec!["exports/structured.json"]);
    assert!(!manifest.frames.is_empty());
    let bodies = mock.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].to_string().contains(rule));
    assert!(bodies[0].to_string().contains("image_url"));
    assert!(!bodies[1].to_string().contains("image_url"));
    assert!(bodies.iter().all(
        |body| body["model"] == "proofread-vision" && !body.to_string().contains("input_audio")
    ));
}

#[test]
fn summary_only_reprocessing_retries_known_failure_once_without_repeating_source_work() {
    if !common::ffmpeg_available() {
        return;
    }
    let mock = MockAi::respond_with(|number, _| {
        if number == 0 {
            // An explicit response_format rejection sanctions exactly one relaxed resend.
            Some((
                422,
                serde_json::json!({"error":{"param":"response_format","code":"unsupported_parameter","message":"Unsupported response_format; private-task-key"}}),
            ))
        } else if number < 3 {
            Some((
                400,
                serde_json::json!({"error":{"message":"Invalid model; private-task-key must not be copied"}}),
            ))
        } else {
            Some((200, summary_response()))
        }
    });
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(&source, b"broken video, readable selected subtitles").unwrap();
    let mut initial = request(root.path(), &source);
    initial.config.defaults.formats = Some(vec![course2md::config::OutputFormat::Html]);
    initial.config.llm.enabled = false;
    initial.config.llm.summarize = true;
    initial.config.llm.base_url = mock.url.clone();
    initial.config.llm.model = "summary-model".into();
    initial.config.llm.api_key = "private-task-key".into();
    initial
        .service_versions
        .insert("llm".into(), "service-v1".into());
    let first = run(root.path(), &initial);
    assert!(first.status.success(), "{:?}", events(&first));
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        2,
        "an explicit response_format rejection allows one resend without it; the following arbitrary HTTP 400 must not change and resend the payload"
    );
    let base = initial.course_dir.clone();
    let first_manifest = artifact::read_manifest(&base.join("manifest.json")).unwrap();
    assert_eq!(
        first_manifest.outcomes.summary.status,
        artifact::Status::Failed
    );
    assert_eq!(
        first_manifest.outcomes.exports["html"].status,
        artifact::Status::Succeeded
    );
    let reason = first_manifest.outcomes.summary.message.as_deref().unwrap();
    assert!(reason.contains("HTTP 400"));
    assert!(!reason.contains(" / ") && !reason.contains("private-task-key"));
    let edit = "# 我的人工补充\n\n补摘要不能丢掉这份修改。\n";
    std::fs::write(base.join("course.md"), edit).unwrap();
    let note_files = || {
        std::fs::read_dir(&base)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.is_file())
            .map(|p| {
                let bytes = std::fs::read(&p).unwrap();
                (p, bytes)
            })
            .collect::<Vec<_>>()
    };
    let original_files = note_files();
    std::fs::remove_file(&source).unwrap();
    let mut retry = initial.clone();
    retry.task_id = "summary-retry".into();
    retry.work_dir = root.path().join("retry-work");
    retry.operation = course2md::execution::Operation::Reprocess {
        note_dir: base.clone(),
        base_task_id: initial.task_id.clone(),
        components: vec![Component::Summary],
        prior_work_dir: Some(initial.work_dir.clone()),
    };
    let second = run(root.path(), &retry);
    let second_events = events(&second);
    // The summary was the only thing asked for and failed again, so the note
    // stays as it was; the error says why.
    assert!(!second.status.success(), "{second_events:#?}");
    assert!(second_events.iter().any(|event| {
        event["type"] == "error"
            && event["code"] == "parameters"
            && event["message"]
                .as_str()
                .is_some_and(|m| m.contains("HTTP 400"))
    }));
    assert_eq!(note_files(), original_files);
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        3,
        "the selected known-failed summary must be attempted again"
    );
    assert!(
        second_events
            .iter()
            .any(|event| event["type"] == "stage" && event["stage"] == "summary")
    );
    assert!(!second_events.iter().any(|event| {
        ["audio", "transcribe", "llm", "download", "scenes"]
            .iter()
            .any(|stage| event["stage"] == *stage)
            || event["stage"]
                .as_str()
                .is_some_and(|stage| stage.starts_with("scenes/"))
    }));
    // 首轮降级重发留下过两张 receipt（原负载与去 response_format 的负载各一张）；
    // 重新尝试只针对与当前负载匹配的那张。
    let receipts = course2md::dispatch::receipts(&retry.work_dir).unwrap();
    let receipt = receipts.iter().find(|r| r.attempt == 2).unwrap();
    assert!(
        receipt.retry_authorized.is_none(),
        "a consumed authorization must not survive the retried attempt"
    );
    let resumed = run(root.path(), &retry);
    assert!(!resumed.status.success(), "{:?}", events(&resumed));
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        3,
        "normal continuation must not renew an explicit retry"
    );
    let mut final_retry = retry.clone();
    final_retry.task_id = "summary-final".into();
    final_retry.work_dir = root.path().join("final-work");
    final_retry.operation = course2md::execution::Operation::Reprocess {
        note_dir: base.clone(),
        base_task_id: initial.task_id.clone(),
        components: vec![Component::Summary],
        // A failed retry left the note as it was; the next repair starts from
        // the note's own task, as the desktop's repair does.
        prior_work_dir: Some(initial.work_dir.clone()),
    };
    let third = run(root.path(), &final_retry);
    let third_events = events(&third);
    assert!(third.status.success(), "{third_events:#?}");
    assert_eq!(mock.calls.load(Ordering::SeqCst), 4);
    let done = third_events.iter().find(|e| e["type"] == "done").unwrap();
    assert_eq!(done["kept_edits"], serde_json::json!(["course.edited.md"]));
    assert!(done.get("version_id").is_none());
    let final_dir = initial.course_dir.clone();
    let manifest = artifact::read_manifest(&final_dir.join("manifest.json")).unwrap();
    assert_eq!(manifest.task_id, "summary-final");
    assert_eq!(
        manifest.outcomes.summary.status,
        artifact::Status::Succeeded
    );
    let document: artifact::Document =
        serde_json::from_slice(&std::fs::read(final_dir.join("document.json")).unwrap()).unwrap();
    assert_eq!(
        document.summary.unwrap().tldr,
        common::fake_ai()["responses"]["summary"]["tldr"]
    );
    // The summary is in the note, and the hand edit is kept beside it.
    assert_eq!(
        std::fs::read_to_string(final_dir.join("course.edited.md")).unwrap(),
        edit
    );
    assert!(
        std::fs::read_to_string(final_dir.join("course.md"))
            .unwrap()
            .contains(
                common::fake_ai()["responses"]["summary"]["tldr"]
                    .as_str()
                    .unwrap()
            )
    );
    // A restart after the note was replaced, before the task recorded it, sends
    // nothing and leaves the note alone.
    let replaced = std::fs::read(final_dir.join("manifest.json")).unwrap();
    let restarted = run(root.path(), &final_retry);
    assert!(restarted.status.success(), "{:?}", events(&restarted));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        std::fs::read(final_dir.join("manifest.json")).unwrap(),
        replaced
    );
    // The older repair, planned from the note it replaced, no longer applies.
    let stale = run(root.path(), &retry);
    assert!(!stale.status.success());
    assert!(events(&stale).iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .is_some_and(|m| m.contains("计划补做后笔记已被更改"))
    }));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 4);
    let bodies = mock.bodies.lock().unwrap();
    // The single sanctioned resend drops response_format and changes nothing else.
    assert!(bodies[0].get("response_format").is_some());
    assert!(bodies[1].get("response_format").is_none());
    let mut relaxed = bodies[0].clone();
    relaxed.as_object_mut().unwrap().remove("response_format");
    assert_eq!(bodies[1], relaxed);
    // Later tasks resend the full payload, byte-identical to the first request.
    assert_eq!(bodies[0], bodies[2]);
    assert_eq!(bodies[2], bodies[3]);
}

/// The desktop reads a task's log events: at trace level they must not carry
/// the request bytes transport crates dump, the summary key among them.
#[test]
fn trace_log_events_never_carry_the_api_key() {
    if !common::ffmpeg_available() {
        return;
    }
    const KEY: &str = "sk-Zq8Xv3Lp9Wm2Tn7Rb4Yc6Hd5";
    let mock = MockAi::respond_with(|_, _| Some((200, summary_response())));
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(&source, b"broken video, readable selected subtitles").unwrap();
    let mut task = request(root.path(), &source);
    task.config.llm.summarize = true;
    task.config.llm.base_url = mock.url.clone();
    task.config.llm.model = "summary-model".into();
    task.config.llm.api_key = KEY.into();
    let output = spawn_task(root.path(), &task, |command| {
        command.env("RUST_LOG", "trace");
    })
    .wait_with_output()
    .unwrap();
    let events = events(&output);
    assert!(output.status.success(), "{events:#?}");
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!common::shows_part_of(&stdout, KEY), "{stdout}");
}

#[test]
fn summarize_command_adds_a_summary_to_the_note() {
    if !common::ffmpeg_available() {
        return;
    }
    let mock = MockAi::respond_with(|_, _| Some((200, summary_response())));
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(
        &source,
        b"unreadable video; the selected subtitles carry the text",
    )
    .unwrap();
    let initial = request(root.path(), &source);
    let first = run(root.path(), &initial);
    assert!(first.status.success(), "{:?}", events(&first));
    let base = initial.course_dir.clone();
    let config = root.path().join("cli-config/course2md");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[llm]\nbase_url = {:?}\nmodel = \"summary-model\"\napi_key = \"cli-key\"\n",
            mock.url
        ),
    )
    .unwrap();
    let exported = root.path().join("exported");
    std::fs::create_dir_all(&exported).unwrap();
    let summarize = || {
        Command::new(env!("CARGO_BIN_EXE_course2md"))
            .env("COURSE2MD_LANG", "zh-Hans")
            .env("XDG_CONFIG_HOME", root.path().join("cli-config"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("HOME", root.path())
            .env_remove("RUST_LOG")
            .args(["summarize", "--quiet", "--out"])
            .arg(&exported)
            .arg(&base)
            .output()
            .unwrap()
    };
    let output = summarize();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let manifest = artifact::read_manifest(&base.join("manifest.json")).unwrap();
    assert!(
        manifest.task_id.starts_with("summary-"),
        "{}",
        manifest.task_id
    );
    assert_eq!(
        manifest.outcomes.summary.status,
        artifact::Status::Succeeded
    );
    // Its work stays outside the note.
    assert!(std::fs::read_dir(&base).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')
    }));
    assert_eq!(manifest.source_id, initial.source_id);
    let markdown = std::fs::read_dir(&exported)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(markdown.to_string_lossy().ends_with(".summary.md"));
    // The same note and settings are already summarized: nothing is sent again.
    let again = summarize();
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}

/// A course an older app saved as versions is flattened, its other versions
/// deleted, and summarized like any note, whether the command names the course
/// or one of its version folders.
#[test]
fn summarize_command_flattens_a_course_saved_as_versions() {
    if !common::ffmpeg_available() {
        return;
    }
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &to.join(entry.file_name()));
            } else {
                std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
            }
        }
    }
    let mock = MockAi::respond_with(|_, _| Some((200, summary_response())));
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(
        &source,
        b"unreadable video; the selected subtitles carry the text",
    )
    .unwrap();
    let initial = request(root.path(), &source);
    assert!(run(root.path(), &initial).status.success());
    // The layout an older app wrote: two versions, a pointer at the newer one.
    let course = initial.course_dir.clone();
    let flat = root.path().join("flat-copy");
    std::fs::rename(&course, &flat).unwrap();
    for (version, revision) in [("older", 1), ("task-one", 2)] {
        let dir = course.join("versions").join(version);
        copy_tree(&flat, &dir);
        let path = dir.join("manifest.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["schema"] = 1.into();
        value["task_id"] = version.into();
        value["version_id"] = version.into();
        value["revision"] = revision.into();
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }
    std::fs::write(
        course.join("current.json"),
        br#"{"schema":1,"course_id":"course-one","version_id":"task-one","manifest":"versions/task-one/manifest.json"}"#,
    )
    .unwrap();
    let config = root.path().join("cli-config/course2md");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[llm]\nbase_url = {:?}\nmodel = \"summary-model\"\napi_key = \"cli-key\"\n",
            mock.url
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_course2md"))
        .env("COURSE2MD_LANG", "zh-Hans")
        .env("XDG_CONFIG_HOME", root.path().join("cli-config"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env_remove("RUST_LOG")
        .args(["summarize"])
        .arg(course.join("versions/task-one"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("摘要已加入笔记"), "{stdout}");
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert!(!course.join("versions").exists());
    assert!(!course.join("current.json").exists());
    let manifest = artifact::read_manifest(&course.join("manifest.json")).unwrap();
    assert!(
        manifest.task_id.starts_with("summary-"),
        "{}",
        manifest.task_id
    );
    assert_eq!(manifest.schema, artifact::MANIFEST_SCHEMA);
}

#[test]
fn proofreading_resend_continues_unsent_summary_without_repeating_source_work() {
    if !common::ffmpeg_available() {
        return;
    }
    let mock = MockAi::new();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("unreadable-video.mp4");
    std::fs::write(&source, b"broken").unwrap();
    let mut original = request(root.path(), &source);
    original.duration = 10.;
    original.subtitle_events = Some(vec![
        TranscriptEvent {
            start: 0.,
            end: 1.,
            text: "第一段原文。".into(),
            raw: None,
            translation: None,
        },
        TranscriptEvent {
            start: 6.,
            end: 7.,
            text: "第二段原文。".into(),
            raw: None,
            translation: None,
        },
    ]);
    original.config.llm.enabled = true;
    original.config.llm.summarize = true;
    original.config.llm.base_url = mock.url.clone();
    original.config.llm.model = "test-model".into();
    original.config.llm.api_key = "private-task-key".into();
    original
        .service_versions
        .insert("llm".into(), "ai-v1".into());
    let first = run(root.path(), &original);
    let first_events = events(&first);
    assert!(first.status.success(), "{first_events:#?}");
    assert!(
        first_events
            .iter()
            .any(|event| event["type"] == "blocked" && event["reason"] == "uncertain")
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let mut receipts = course2md::dispatch::receipts(&original.work_dir).unwrap();
    assert_eq!(
        receipts.len(),
        1,
        "blocked summary must not create an attempt"
    );
    let receipt = receipts.remove(0);
    assert_eq!(receipt.state, course2md::dispatch::State::Uncertain);
    assert_eq!(receipt.purpose, "proofreading");
    assert!(receipt.description.contains("00:00–00:07"));
    assert!(!String::from_utf8_lossy(&first.stdout).contains("private-task-key"));
    let base = original.course_dir.clone();
    let manifest = artifact::read_manifest(&base.join("manifest.json")).unwrap();
    assert_eq!(manifest.outcomes.summary.status, artifact::Status::Failed);
    std::fs::remove_file(&source).unwrap();
    let mut retry = original.clone();
    retry.task_id = "retry-polish".into();
    retry.work_dir = root.path().join("retry-work");
    retry.control_path = Some(retry.work_dir.join("control.json"));
    retry.operation = course2md::execution::Operation::Reprocess {
        note_dir: base.clone(),
        base_task_id: original.task_id.clone(),
        components: vec![Component::Proofreading, Component::Summary],
        prior_work_dir: Some(original.work_dir.clone()),
    };
    std::fs::create_dir_all(&retry.work_dir).unwrap();
    std::fs::write(
        retry.control_path.as_ref().unwrap(),
        serde_json::to_vec(&serde_json::json!({"intent":"run","resend":[receipt.request_id]}))
            .unwrap(),
    )
    .unwrap();
    let second = run(root.path(), &retry);
    let observed = events(&second);
    assert!(second.status.success(), "{observed:#?}");
    assert_eq!(mock.calls.load(Ordering::SeqCst), 3);
    assert!(
        !observed
            .iter()
            .any(|event| event["stage"] == "transcribe" || event["stage"] == "download")
    );
    let updated = artifact::read_manifest(&base.join("manifest.json")).unwrap();
    assert_eq!(updated.task_id, "retry-polish");
    assert_eq!(
        updated.outcomes.proofreading.status,
        artifact::Status::Succeeded
    );
    assert_eq!(updated.outcomes.summary.status, artifact::Status::Succeeded);
    let receipts = course2md::dispatch::receipts(&retry.work_dir).unwrap();
    assert_eq!(receipts.len(), 2);
    for (purpose, attempt) in [("proofreading", 2), ("summary", 1)] {
        let receipt = receipts.iter().find(|r| r.purpose == purpose).unwrap();
        assert_eq!(receipt.state, course2md::dispatch::State::Completed);
        assert_eq!(receipt.attempt, attempt);
    }
}

/// Answers a proofreading or translation batch item for item, so the note keeps
/// its text: proofreading echoes it, translation marks it.
fn batch_reply(body: &serde_json::Value, translated: bool) -> serde_json::Value {
    let user = &body["messages"][1]["content"];
    let payload = user.as_str().or_else(|| user[0]["text"].as_str()).unwrap();
    let items: Vec<serde_json::Value> = serde_json::from_str(payload).unwrap();
    let segments: Vec<_> = items
        .iter()
        .map(|item| {
            let text = item["text"].as_str().unwrap();
            let text = if translated {
                format!("译：{text}")
            } else {
                text.to_owned()
            };
            serde_json::json!({"id": item["id"], "text": text})
        })
        .collect();
    chat_completion(&serde_json::json!({ "segments": segments }))
}

fn is_translation(body: &serde_json::Value) -> bool {
    body["messages"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .starts_with("You translate lecture transcripts")
}

/// The owner's deadlock in the engine: a translation batch lost its response after
/// the note was saved. Redoing the translation alone sends exactly that batch (as
/// authorized) and the batch it held back, plus the summary the uncertain step kept
/// from starting. The finished batch comes back from its receipt, and proofreading
/// is never sent again.
#[test]
fn translation_resend_sends_only_unfinished_batches_and_the_held_back_summary() {
    if common::ffmpeg_available() {
        translation_resend(false, false);
    }
}

/// The saved note says which batches are translated: a batch whose receipt is gone
/// is still never sent again.
#[test]
fn a_translated_batch_without_its_receipt_is_not_sent_again() {
    if common::ffmpeg_available() {
        translation_resend(true, false);
    }
}

/// A repair whose resend is lost again finishes nothing new: it sends the one
/// authorized batch, holds the rest back, and leaves the note as it was, even
/// with the summary riding along.
#[test]
fn a_repair_that_finishes_nothing_new_leaves_the_note_unchanged() {
    if common::ffmpeg_available() {
        translation_resend(false, true);
    }
}

fn translation_resend(missing_receipt: bool, lose_again: bool) {
    let translations = Arc::new(AtomicUsize::new(0));
    let counted = translations.clone();
    let lose_all = Arc::new(AtomicBool::new(false));
    let losing = lose_all.clone();
    let summary = common::fake_ai()["responses"]["summary"].clone();
    let mock = MockAi::respond_with(move |_, body| {
        if losing.load(Ordering::SeqCst) {
            return None;
        }
        if is_translation(body) {
            // The second translation batch is received, but its response is lost.
            if counted.fetch_add(1, Ordering::SeqCst) == 1 {
                return None;
            }
            return Some((200, batch_reply(body, true)));
        }
        let proofreading = body["messages"][1]["content"].is_array();
        Some((
            200,
            if proofreading {
                batch_reply(body, false)
            } else {
                chat_completion(&summary)
            },
        ))
    });
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("unreadable-video.mp4");
    std::fs::write(&source, b"broken").unwrap();
    let mut original = request(root.path(), &source);
    // Pauses between sentences keep each one its own paragraph.
    original.duration = 450.;
    original.subtitle_events = Some(
        (0..45)
            .map(|index| TranscriptEvent {
                start: index as f64 * 10.,
                end: index as f64 * 10. + 2.,
                text: format!("This is spoken sentence number {index} of the lecture."),
                raw: None,
                translation: None,
            })
            .collect(),
    );
    let llm = &mut original.config.llm;
    llm.enabled = true;
    llm.summarize = true;
    llm.translate = Some("zh-Hans".into());
    llm.concurrency = 1;
    llm.base_url = mock.url.clone();
    llm.model = "test-model".into();
    llm.api_key = "private-task-key".into();
    original
        .service_versions
        .insert("llm".into(), "ai-v1".into());
    let first = run(root.path(), &original);
    let first_events = events(&first);
    assert!(first.status.success(), "{first_events:#?}");
    // Three proofreading batches, then translation: the first answered, the second lost.
    assert_eq!(mock.calls.load(Ordering::SeqCst), 5);
    let receipts = course2md::dispatch::receipts(&original.work_dir).unwrap();
    let uncertain: Vec<_> = receipts
        .iter()
        .filter(|r| r.state == course2md::dispatch::State::Uncertain)
        .collect();
    assert_eq!(uncertain.len(), 1);
    assert_eq!(uncertain[0].purpose, Component::Translation.as_str());
    let base = original.course_dir.clone();
    let manifest = artifact::read_manifest(&base.join("manifest.json")).unwrap();
    assert_eq!(
        manifest.outcomes.proofreading.status,
        artifact::Status::Succeeded
    );
    assert_ne!(
        manifest.outcomes.translation.status,
        artifact::Status::Succeeded
    );
    assert_eq!(manifest.outcomes.summary.status, artifact::Status::Failed);
    let untouched = std::fs::read(base.join("document.json")).unwrap();
    let untouched_manifest = std::fs::read(base.join("manifest.json")).unwrap();
    std::fs::remove_file(&source).unwrap();
    if missing_receipt {
        let finished = receipts
            .iter()
            .find(|r| {
                r.purpose == Component::Translation.as_str()
                    && r.state == course2md::dispatch::State::Completed
            })
            .unwrap();
        std::fs::remove_file(
            original
                .work_dir
                .join(format!("requests/{}.json", finished.stable_id)),
        )
        .unwrap();
    }
    lose_all.store(lose_again, Ordering::SeqCst);

    let mut retry = original.clone();
    retry.task_id = "retry-translation".into();
    retry.work_dir = root.path().join("retry-work");
    retry.control_path = Some(retry.work_dir.join("control.json"));
    // As the desktop asks for it: no proofreading, translation and the summary.
    retry.config.llm.enabled = false;
    retry.operation = course2md::execution::Operation::Reprocess {
        note_dir: base.clone(),
        base_task_id: original.task_id.clone(),
        components: vec![Component::Translation, Component::Summary],
        prior_work_dir: Some(original.work_dir.clone()),
    };
    std::fs::create_dir_all(&retry.work_dir).unwrap();
    std::fs::write(
        retry.control_path.as_ref().unwrap(),
        serde_json::to_vec(&serde_json::json!({"intent":"run","resend":[uncertain[0].request_id]}))
            .unwrap(),
    )
    .unwrap();
    let sent_before = mock.bodies.lock().unwrap().len();
    let second = run(root.path(), &retry);
    let observed = events(&second);
    let bodies = mock.bodies.lock().unwrap()[sent_before..].to_vec();
    let translated: Vec<_> = bodies.iter().filter(|body| is_translation(body)).collect();
    if lose_again {
        assert!(!second.status.success() || observed.iter().any(|e| e["type"] == "error"));
        // The authorized batch once; lost again, it holds the rest and the summary back.
        assert_eq!(translated.len(), 1, "{bodies:#?}");
        assert_eq!(bodies.len(), 1, "{bodies:#?}");
        assert_eq!(
            std::fs::read(base.join("manifest.json")).unwrap(),
            untouched_manifest,
            "the note was replaced by an identical one"
        );
        assert_eq!(
            std::fs::read(base.join("document.json")).unwrap(),
            untouched
        );
        return;
    }
    assert!(second.status.success(), "{observed:#?}");
    // The lost batch once and the third batch it held back; the summary once.
    assert_eq!(translated.len(), 2, "{bodies:#?}");
    assert_eq!(bodies.len(), 3, "{bodies:#?}");
    let first_sent = mock.bodies.lock().unwrap()[..sent_before].to_vec();
    let answered = first_sent.iter().find(|body| is_translation(body)).unwrap();
    assert!(
        !translated.contains(&answered),
        "a finished batch was sent again"
    );
    assert!(
        !observed
            .iter()
            .any(|event| event["stage"] == "llm" || event["stage"] == "transcribe")
    );
    let updated = artifact::read_manifest(&base.join("manifest.json")).unwrap();
    assert_eq!(updated.task_id, "retry-translation");
    assert_eq!(
        updated.outcomes.translation.status,
        artifact::Status::Succeeded
    );
    assert_eq!(
        updated.outcomes.proofreading.status,
        artifact::Status::Succeeded
    );
    assert_eq!(updated.outcomes.summary.status, artifact::Status::Succeeded);
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(base.join(&updated.document)).unwrap()).unwrap();
    let speech: Vec<_> = document["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|section| section["speech"].as_array().unwrap())
        .collect();
    assert_eq!(speech.len(), 45);
    assert!(speech.iter().all(|event| {
        event["translation"]
            .as_str()
            .is_some_and(|text| text.starts_with("译："))
    }));
    let receipts = course2md::dispatch::receipts(&retry.work_dir).unwrap();
    let resent = receipts
        .iter()
        .find(|r| r.stable_id == uncertain[0].stable_id)
        .unwrap();
    assert_eq!(resent.state, course2md::dispatch::State::Completed);
    assert_eq!(resent.attempt, uncertain[0].attempt + 1);
}

/// Several paid batches lose their responses while in flight together, so each is
/// uncertain and everything after them is held back. Resending all of them sends each
/// once, with the batches they held back and the summary, and the repair finishes in
/// one run even with concurrent batches: an authorized request never holds back
/// another (it did, and nothing was sent).
#[test]
fn resending_several_uncertain_paid_batches_sends_each_once_and_finishes() {
    if !common::ffmpeg_available() {
        return;
    }
    for lost_kind in ["translation", "proofreading"] {
        for lost in [2, 3] {
            several_uncertain_batches(lost_kind, lost);
        }
    }
}

fn several_uncertain_batches(lost_kind: &'static str, lost: usize) {
    let losing = Arc::new(AtomicBool::new(true));
    let (losses, still_losing) = (Arc::new(AtomicUsize::new(0)), losing.clone());
    let summary = common::fake_ai()["responses"]["summary"].clone();
    let mock = MockAi::respond_with(move |_, body| {
        let translation = is_translation(body);
        let proofreading = !translation && body["messages"][1]["content"].is_array();
        let kind = if translation {
            "translation"
        } else if proofreading {
            "proofreading"
        } else {
            "summary"
        };
        if kind == lost_kind
            && still_losing.load(Ordering::SeqCst)
            && losses.fetch_add(1, Ordering::SeqCst) < lost
        {
            // Received, then lost late enough that the batches beside it are already sent.
            std::thread::sleep(std::time::Duration::from_millis(400));
            return None;
        }
        Some((
            200,
            match kind {
                "summary" => chat_completion(&summary),
                _ => batch_reply(body, translation),
            },
        ))
    });
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("unreadable-video.mp4");
    std::fs::write(&source, b"broken").unwrap();
    let mut original = request(root.path(), &source);
    // Four batches of each kind.
    original.duration = 800.;
    original.subtitle_events = Some(
        (0..80)
            .map(|index| TranscriptEvent {
                start: index as f64 * 10.,
                end: index as f64 * 10. + 2.,
                text: format!("This is spoken sentence number {index} of the lecture."),
                raw: None,
                translation: None,
            })
            .collect(),
    );
    let llm = &mut original.config.llm;
    llm.enabled = true;
    llm.summarize = true;
    llm.translate = Some("zh-Hans".into());
    llm.concurrency = lost;
    llm.base_url = mock.url.clone();
    llm.model = "test-model".into();
    llm.api_key = "private-task-key".into();
    original
        .service_versions
        .insert("llm".into(), "ai-v1".into());
    let first = run(root.path(), &original);
    assert!(first.status.success(), "{:#?}", events(&first));
    let receipts = course2md::dispatch::receipts(&original.work_dir).unwrap();
    let uncertain: Vec<_> = receipts
        .iter()
        .filter(|r| r.state == course2md::dispatch::State::Uncertain)
        .collect();
    assert_eq!(uncertain.len(), lost, "{lost_kind} {lost}: {receipts:#?}");
    assert!(uncertain.iter().all(|r| r.purpose == lost_kind));
    let answered_before: Vec<_> = mock.bodies.lock().unwrap().clone();
    losing.store(false, Ordering::SeqCst);

    let base = original.course_dir.clone();
    let mut retry = original.clone();
    retry.task_id = "retry".into();
    retry.work_dir = root.path().join("retry-work");
    retry.control_path = Some(retry.work_dir.join("control.json"));
    // As the desktop asks for it: the uncertain part and the summary it held back.
    let part = if lost_kind == "translation" {
        retry.config.llm.enabled = false;
        Component::Translation
    } else {
        Component::Proofreading
    };
    retry.operation = course2md::execution::Operation::Reprocess {
        note_dir: base,
        base_task_id: original.task_id.clone(),
        components: vec![part, Component::Summary],
        prior_work_dir: Some(original.work_dir.clone()),
    };
    std::fs::create_dir_all(&retry.work_dir).unwrap();
    let ids: Vec<_> = uncertain.iter().map(|r| r.request_id.clone()).collect();
    std::fs::write(
        retry.control_path.as_ref().unwrap(),
        serde_json::to_vec(&serde_json::json!({"intent":"run","resend":ids})).unwrap(),
    )
    .unwrap();
    let second = run(root.path(), &retry);
    let observed = events(&second);
    assert!(second.status.success(), "{observed:#?}");
    let sent = mock.bodies.lock().unwrap()[answered_before.len()..].to_vec();
    let kinds = |kind: &str| {
        sent.iter()
            .filter(|body| match kind {
                "translation" => is_translation(body),
                "proofreading" => {
                    !is_translation(body) && body["messages"][1]["content"].is_array()
                }
                _ => !is_translation(body) && !body["messages"][1]["content"].is_array(),
            })
            .count()
    };
    // Every batch of the redone parts once (none had finished), the summary once,
    // and nothing that was answered before.
    let expected_proofreading = if lost_kind == "proofreading" { 4 } else { 0 };
    assert_eq!(
        kinds("proofreading"),
        expected_proofreading,
        "{lost_kind} {lost}"
    );
    assert_eq!(kinds("translation"), 4, "{lost_kind} {lost}");
    assert_eq!(kinds("summary"), 1, "{lost_kind} {lost}");
    for (index, body) in sent.iter().enumerate() {
        assert!(
            !sent[..index].contains(body),
            "{lost_kind} {lost}: a request was sent twice"
        );
    }
    // Before, only the lost batches of that kind went out; everything else was answered.
    let answered: Vec<_> = answered_before
        .iter()
        .filter(|body| {
            if lost_kind == "translation" {
                !is_translation(body)
            } else {
                is_translation(body) || !body["messages"][1]["content"].is_array()
            }
        })
        .collect();
    assert!(
        sent.iter().all(|body| !answered.contains(&body)),
        "{lost_kind} {lost}: an answered request was sent again"
    );
    let updated = artifact::read_manifest(&retry.course_dir.join("manifest.json")).unwrap();
    assert_eq!(updated.task_id, "retry");
    for outcome in [
        &updated.outcomes.proofreading,
        &updated.outcomes.translation,
        &updated.outcomes.summary,
    ] {
        assert_eq!(
            outcome.status,
            artifact::Status::Succeeded,
            "{lost_kind} {lost}"
        );
    }
}

#[test]
fn export_only_recovery_uses_the_saved_note_after_source_and_subtitle_are_gone() {
    use course2md::config::OutputFormat;
    if !common::ffmpeg_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(&source, b"broken").unwrap();
    let original = request(root.path(), &source);
    assert!(run(root.path(), &original).status.success());
    let note = original.course_dir.clone();
    let document = std::fs::read(note.join("document.json")).unwrap();
    let mut export = original.clone();
    export.task_id = "exports".into();
    export.work_dir = root.path().join("export-work");
    export.subtitle_events = None;
    export.subtitle = Some(root.path().join("deleted-subtitles.srt"));
    export.operation = course2md::execution::Operation::Reprocess {
        note_dir: note.clone(),
        base_task_id: original.task_id.clone(),
        components: vec![Component::Exports],
        prior_work_dir: None,
    };
    export.config.defaults.formats = Some(vec![
        OutputFormat::Md,
        OutputFormat::Html,
        OutputFormat::Json,
    ]);
    for attempt in 0..2 {
        if attempt == 1 {
            std::fs::remove_file(&source).unwrap();
        }
        let result = run(root.path(), &export);
        let events = events(&result);
        assert!(result.status.success(), "{events:#?}");
        let done = events.iter().find(|event| event["type"] == "done").unwrap();
        assert_eq!(done["partial"], false);
        assert_eq!(done["outputs"].as_array().unwrap().len(), 3);
        assert!(done.get("kept_edits").is_none(), "{done:#}");
    }
    // The exports are the note's own files; the rest of the note is as it was.
    let manifest = artifact::read_manifest(&note.join("manifest.json")).unwrap();
    assert_eq!(manifest.task_id, original.task_id);
    for name in ["course.zip", "course.html", "structured.json"] {
        assert!(note.join("exports").join(name).is_file(), "{name}");
        assert!(manifest.outputs.contains(&format!("exports/{name}")));
    }
    assert_eq!(std::fs::read(note.join("document.json")).unwrap(), document);
    artifact::validate_note(&note, &manifest).unwrap();
}

/// A repair of a summary and the exports in which the summary fails still makes
/// the exports, and keeps the summary the note had rather than clear it.
#[test]
fn a_failed_part_of_a_mixed_repair_keeps_what_the_note_had() {
    use course2md::config::OutputFormat;
    if !common::ffmpeg_available() {
        return;
    }
    let failing = Arc::new(AtomicBool::new(false));
    let fail = failing.clone();
    let mock = MockAi::respond_with(move |_, _| {
        Some(if fail.load(Ordering::SeqCst) {
            (
                400,
                serde_json::json!({"error":{"message":"Invalid model"}}),
            )
        } else {
            (200, summary_response())
        })
    });
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(&source, b"broken video, readable selected subtitles").unwrap();
    let mut original = request(root.path(), &source);
    original.config.llm.summarize = true;
    original.config.llm.base_url = mock.url.clone();
    original.config.llm.model = "summary-model".into();
    original.config.llm.api_key = "private-task-key".into();
    original
        .service_versions
        .insert("llm".into(), "service-v1".into());
    assert!(run(root.path(), &original).status.success());
    let note = original.course_dir.clone();
    let summarized: artifact::Document =
        serde_json::from_slice(&std::fs::read(note.join("document.json")).unwrap()).unwrap();
    let summary = serde_json::to_value(summarized.summary.as_ref().unwrap()).unwrap();
    failing.store(true, Ordering::SeqCst);
    let mut repair = original.clone();
    repair.task_id = "repair".into();
    repair.work_dir = root.path().join("repair-work");
    repair.config.defaults.formats = Some(vec![OutputFormat::Json]);
    repair.operation = course2md::execution::Operation::Reprocess {
        note_dir: note.clone(),
        base_task_id: original.task_id.clone(),
        components: vec![Component::Summary, Component::Exports],
        // Without the original's receipts the summary is asked for again, and fails.
        prior_work_dir: None,
    };
    let output = run(root.path(), &repair);
    assert!(output.status.success(), "{:#?}", events(&output));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    let manifest = artifact::read_manifest(&note.join("manifest.json")).unwrap();
    assert_eq!(manifest.task_id, "repair");
    assert_eq!(manifest.outputs, vec!["exports/structured.json"]);
    assert_eq!(
        manifest.outcomes.summary.status,
        artifact::Status::Succeeded
    );
    let repaired: artifact::Document =
        serde_json::from_slice(&std::fs::read(note.join("document.json")).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(repaired.summary.unwrap()).unwrap(),
        summary
    );
}

/// A regenerate whose AI service is unreachable ends failed and leaves the
/// complete note, its summary and its exports as they were; a later repair that
/// asks for no export still keeps every export the note had.
#[test]
fn a_regenerate_that_loses_paid_work_leaves_the_note() {
    use course2md::config::OutputFormat;
    if !common::ffmpeg_available() {
        return;
    }
    let mock = MockAi::respond_with(|_, _| Some((200, summary_response())));
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(&source, b"broken video, readable selected subtitles").unwrap();
    let mut original = request(root.path(), &source);
    original.config.defaults.formats = Some(vec![OutputFormat::Md]);
    original.config.llm.summarize = true;
    original.config.llm.base_url = mock.url.clone();
    original.config.llm.model = "summary-model".into();
    original.config.llm.api_key = "private-task-key".into();
    assert!(run(root.path(), &original).status.success());
    let note = original.course_dir.clone();
    let before: Vec<_> = ["manifest.json", "document.json", "exports/course.zip"]
        .iter()
        .map(|name| std::fs::read(note.join(name)).unwrap())
        .collect();
    // The service is gone: nothing listens there any more.
    let mut regenerate = original.clone();
    regenerate.task_id = "regenerate".into();
    regenerate.work_dir = root.path().join("regenerate-work");
    regenerate.config.llm.base_url = "http://127.0.0.1:9".into();
    let output = run(root.path(), &regenerate);
    let observed = events(&output);
    assert!(!output.status.success(), "{observed:#?}");
    assert!(observed.iter().any(|event| {
        event["type"] == "error"
            && event["code"] == "note_kept"
            && event["message"].as_str().is_some_and(|m| {
                m.contains("不如现有笔记") && m.contains("摘要") && m.contains("（")
            })
    }));
    let after: Vec<_> = ["manifest.json", "document.json", "exports/course.zip"]
        .iter()
        .map(|name| std::fs::read(note.join(name)).unwrap())
        .collect();
    assert_eq!(after, before);
    // A summary-only repair asks for no export; the note keeps its archive.
    let mut repair = original.clone();
    repair.task_id = "repair".into();
    repair.work_dir = root.path().join("repair-work");
    repair.config.defaults.formats = Some(vec![]);
    repair.operation = course2md::execution::Operation::Reprocess {
        note_dir: note.clone(),
        base_task_id: original.task_id.clone(),
        components: vec![Component::Summary],
        prior_work_dir: None,
    };
    let output = run(root.path(), &repair);
    assert!(output.status.success(), "{:#?}", events(&output));
    let manifest = artifact::read_manifest(&note.join("manifest.json")).unwrap();
    assert_eq!(manifest.task_id, "repair");
    assert_eq!(manifest.outputs, vec!["exports/course.zip"]);
    assert!(note.join("exports/course.zip").is_file());
}

/// A note a newer app wrote is refused before anything is sent.
#[test]
fn a_newer_apps_note_is_refused_before_any_request() {
    if !common::ffmpeg_available() {
        return;
    }
    let mock = MockAi::respond_with(|_, _| Some((200, summary_response())));
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(&source, b"broken video, readable selected subtitles").unwrap();
    let original = request(root.path(), &source);
    assert!(run(root.path(), &original).status.success());
    let note = original.course_dir.clone();
    let path = note.join("manifest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["schema"] = 3.into();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let newer = std::fs::read(&path).unwrap();
    let mut repair = original.clone();
    repair.task_id = "repair".into();
    repair.work_dir = root.path().join("repair-work");
    repair.config.llm.summarize = true;
    repair.config.llm.base_url = mock.url.clone();
    repair.config.llm.model = "summary-model".into();
    repair.config.llm.api_key = "private-task-key".into();
    repair.operation = course2md::execution::Operation::Reprocess {
        note_dir: note.clone(),
        base_task_id: original.task_id.clone(),
        components: vec![Component::Summary],
        prior_work_dir: None,
    };
    let mut regenerate = original.clone();
    regenerate.task_id = "regenerate".into();
    regenerate.work_dir = root.path().join("regenerate-work");
    regenerate.config = repair.config.clone();
    for task in [&repair, &regenerate] {
        let output = run(root.path(), task);
        assert!(!output.status.success(), "{}", task.task_id);
        assert!(events(&output).iter().any(|event| {
            event["type"] == "error"
                && event["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("此笔记需要更新版本的软件"))
        }));
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&path).unwrap(), newer);
}

#[test]
fn relocation_updates_owned_paths_only_after_verifying_the_copy() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old");
    let new = root.path().join("new");
    std::fs::create_dir_all(old.join("work")).unwrap();
    std::fs::create_dir_all(new.join("work")).unwrap();
    let identity = serde_json::json!({"source":old.join("source.mp4"),"config":{"url":old.join("source.mp4"),"out_root":old.join("notes"),"out_dir":old.join("work"),"model_dir":old.join("models")}});
    for path in [&old, &new] {
        std::fs::write(
            path.join("work/task-identity.json"),
            serde_json::to_vec(&identity).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        course2md::execution::relocate_work_bindings(&old, &new).unwrap(),
        1
    );
    let changed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(new.join("work/task-identity.json")).unwrap())
            .unwrap();
    assert_eq!(changed["source"], identity["source"]);
    assert_eq!(changed["config"]["url"], identity["config"]["url"]);
    assert_eq!(
        changed["config"]["out_dir"],
        serde_json::json!(new.join("work"))
    );
    assert_eq!(
        changed["config"]["model_dir"],
        serde_json::json!(new.join("models"))
    );
    assert_eq!(
        course2md::execution::relocate_work_bindings(&old, &new).unwrap(),
        0
    );
    std::fs::write(new.join("work/task-identity.json"), b"{\"different\":true}").unwrap();
    assert!(course2md::execution::relocate_work_bindings(&old, &new).is_err());
}

/// Starts the command line on `source` with AI proofreading and a summary from `ai`,
/// isolated like [`spawn_task`]: only the options given here configure it.
fn spawn_cli(
    root: &Path,
    source: &str,
    ai: &MockAi,
    configure: impl FnOnce(&mut Command),
) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_course2md"));
    command
        .current_dir(root)
        .env("COURSE2MD_LANG", "zh-Hans")
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("HOME", root)
        .env_remove("RUST_LOG")
        .env_remove("COURSE2MD_ASR_API_KEY")
        .arg(source)
        .args(CLI_OPTIONS)
        .args(["--model-dir"])
        .arg(root.join("models"))
        .args([
            "--llm-api-key",
            "private-cli-key",
            "--llm-base-url",
            &ai.url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut command);
    command.spawn().unwrap()
}

/// What [`spawn_cli`] chooses, besides the model folder and the AI service.
const CLI_OPTIONS: [&str; 14] = [
    "--json",
    "-o",
    "out",
    "--transcript-source",
    "subtitle",
    "--formats",
    "md",
    "--slide-mode",
    "first",
    "--llm",
    "--summarize",
    "--llm-model",
    "test-model",
    "--no-llm-hint",
];

const TWO_CUES: &str = "1\n00:00:00,000 --> 00:00:01,000\n第一段原文。\n\n2\n00:00:02,000 --> 00:00:03,000\n第二段原文。\n";

/// An AI service that answers proofreading item for item and holds the first summary
/// request until the run that sent it is killed ([`interrupt_at_summary`]).
fn holding_ai() -> (
    MockAi,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let samples = common::fake_ai();
    let marker = samples["markers"]["proofread"].as_str().unwrap().to_owned();
    let summary = samples["responses"]["summary"].clone();
    let (arrived, summary_arrived) = std::sync::mpsc::channel::<()>();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let held = Mutex::new(Some((arrived, released)));
    let ai = MockAi::respond_with(move |_, body| {
        let polish = body["messages"][0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains(&marker);
        if !polish && let Some((arrived, released)) = held.lock().unwrap().take() {
            arrived.send(()).unwrap();
            let _ = released.recv();
            return None;
        }
        let reply = if polish {
            batch_reply(body, false)
        } else {
            chat_completion(&summary)
        };
        Some((200, reply))
    });
    (ai, summary_arrived, release)
}

/// Kills `run` while its summary request is in flight: proofreading was paid for and
/// the summary's response is lost. Returns the task's one work folder under `library`.
fn interrupt_at_summary(
    mut run: Child,
    ai: &MockAi,
    arrived: &std::sync::mpsc::Receiver<()>,
    release: &std::sync::mpsc::Sender<()>,
    library: &Path,
) -> std::path::PathBuf {
    arrived
        .recv_timeout(std::time::Duration::from_secs(120))
        .expect("the first run reaches its summary");
    run.kill().unwrap();
    run.wait().unwrap();
    release.send(()).unwrap();
    assert_eq!(ai.calls.load(Ordering::SeqCst), 2);
    let mut works: Vec<_> = std::fs::read_dir(library.join(".course2md/work"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(works.len(), 1, "{works:?}");
    works.remove(0)
}

fn identity(work: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(work.join("task-identity.json")).unwrap()).unwrap()
}

/// A run after the interruption: it continues `work` and sends nothing, holding back the
/// summary whose response was lost, and publishes the proofread note.
fn assert_continued_without_paying(output: &Output, ai: &MockAi, work: &Path) {
    let observed = events(output);
    assert!(output.status.success(), "{observed:#?}");
    assert_eq!(ai.calls.load(Ordering::SeqCst), 2, "{observed:#?}");
    assert!(observed.iter().any(|event| event["type"] == "blocked"
        && event["reason"] == "uncertain"
        && event["purpose"] == "summary"));
    let done = observed.last().unwrap();
    assert_eq!(done["type"], "done");
    assert_eq!(done["task_id"].as_str(), work.file_name().unwrap().to_str());
    assert_eq!(
        done["outcomes"]["proofreading"]["status"], "succeeded",
        "{done:#}"
    );
    let note = Path::new(done["out_dir"].as_str().unwrap());
    assert!(note.is_absolute());
    assert!(
        std::fs::read_to_string(note.join("course.md"))
            .unwrap()
            .contains("第二段原文。")
    );
    let receipts = course2md::dispatch::receipts(work).unwrap();
    assert_eq!(receipts.len(), 2);
    use course2md::dispatch::State;
    let receipt = |purpose| receipts.iter().find(|r| r.purpose == purpose).unwrap();
    assert_eq!(receipt("proofreading").state, State::Completed);
    // Sent once; its outcome is unknown whether or not the run that sent it got to say so.
    assert!(matches!(
        receipt("summary").state,
        State::Sending | State::Uncertain
    ));
    assert!(receipts.iter().all(|receipt| receipt.attempt == 1));
    assert_eq!(identity(work)["operation"]["kind"], "generate");
}

fn local_lecture(root: &Path) -> std::path::PathBuf {
    let video = root.join("lecture.mp4");
    common::fixture_video(&video, 3);
    std::fs::write(root.join("lecture.srt"), TWO_CUES).unwrap();
    video
}

#[test]
fn the_command_line_runs_a_task_and_resumes_it_without_paying_twice() {
    if !common::ffmpeg_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let video = local_lecture(root.path());
    let source = video.display().to_string();
    let (ai, arrived, release) = holding_ai();

    // A fresh run binds its work directory as a task, then is interrupted mid-request.
    let first = spawn_cli(root.path(), &source, &ai, |_| {});
    let library = root.path().join("out/local");
    let work = interrupt_at_summary(first, &ai, &arrived, &release, &library);
    let bound = identity(&work);
    assert_eq!(bound["operation"]["kind"], "generate");
    assert_eq!(bound["schema"], course2md::execution::REQUEST_SCHEMA);
    assert_eq!(
        bound["task_id"].as_str(),
        work.file_name().unwrap().to_str()
    );
    assert!(!bound.to_string().contains("private-cli-key"));

    // The same command continues that task: the proofreading it paid for is reused,
    // and the summary whose response was lost is not sent again.
    let second = spawn_cli(root.path(), &source, &ai, |_| {})
        .wait_with_output()
        .unwrap();
    assert_continued_without_paying(&second, &ai, &work);
    assert_eq!(identity(&work), bound);

    // Once the task has published its note, running it again sends nothing.
    let third = spawn_cli(root.path(), &source, &ai, |_| {})
        .wait_with_output()
        .unwrap();
    let observed = events(&third);
    assert!(third.status.success(), "{observed:#?}");
    assert_eq!(observed.last().unwrap()["type"], "done");
    assert_eq!(ai.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_dir(library.join(".course2md/work"))
            .unwrap()
            .count(),
        1
    );
}

/// A site may report another uploader or duration on the next probe. The task's
/// identity leaves both out, so the command still continues the task it began.
/// The stand-in yt-dlp is a shell script.
#[cfg(unix)]
#[test]
fn a_changed_uploader_or_duration_still_continues_the_command_line_task() {
    use std::os::unix::fs::PermissionsExt;
    if !common::ffmpeg_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("fixture.mp4");
    common::fixture_video(&video, 3);
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("yt-dlp");
    std::fs::write(
        &script,
        r#"#!/bin/sh
output=''
subtitles=0
simulate=0
json=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -J|--dump-single-json) json=1 ;;
    --simulate) simulate=1 ;;
    --no-simulate) simulate=0 ;;
    -o) shift; output="$1" ;;
    --skip-download) subtitles=1 ;;
  esac
  shift
done
if [ "$simulate" -eq 1 ]; then
  :
elif [ "$subtitles" -eq 1 ]; then
  printf '%s' "$CLI_TEST_CUES" > "$output.en.srt"
else
  /bin/cp "$CLI_TEST_VIDEO" "$output"
fi
if [ "$json" -eq 1 ]; then
  printf '{"title":"Test lecture","id":"test","uploader":"%s","webpage_url":"https://example.test/video","duration":%s,"extractor":"test","subtitles":{"en":[{"ext":"srt","url":"https://example.test/en.srt"}]},"automatic_captions":{}}\n' "$CLI_TEST_UPLOADER" "$CLI_TEST_DURATION"
fi
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let site = |uploader: &'static str, duration: &'static str| {
        let (bin, video) = (bin.clone(), video.clone());
        move |command: &mut Command| {
            let path = std::env::var("PATH").unwrap_or_default();
            command
                .env("PATH", format!("{}:{path}", bin.display()))
                .env("CLI_TEST_VIDEO", &video)
                .env("CLI_TEST_CUES", TWO_CUES)
                .env("CLI_TEST_UPLOADER", uploader)
                .env("CLI_TEST_DURATION", duration);
        }
    };
    let (ai, arrived, release) = holding_ai();
    let source = "https://example.test/video";
    let first = spawn_cli(root.path(), source, &ai, site("First Teacher", "3"));
    let library = root.path().join("out/test");
    let work = interrupt_at_summary(first, &ai, &arrived, &release, &library);
    let bound = identity(&work);

    let second = spawn_cli(root.path(), source, &ai, site("Renamed Teacher", "3.5"))
        .wait_with_output()
        .unwrap();
    assert_continued_without_paying(&second, &ai, &work);
    assert_eq!(identity(&work), bound);
    let done = events(&second).pop().unwrap();
    let note =
        std::fs::read_to_string(Path::new(done["out_dir"].as_str().unwrap()).join("course.md"))
            .unwrap();
    assert!(note.contains("Renamed Teacher"), "{note}");
}

/// A command-line task interrupted before the command line ran tasks bound its work
/// folder to the identity it chose then. It continues there under the task's identity,
/// and a request whose outcome its receipts leave unknown stays held, not sent again.
#[test]
fn a_task_from_before_the_command_line_ran_tasks_keeps_its_uncertain_request_held() {
    if !common::ffmpeg_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let video = local_lecture(root.path());
    let source = video.display().to_string();
    let (ai, arrived, release) = holding_ai();
    let first = spawn_cli(root.path(), &source, &ai, |_| {});
    let library = root.path().join("out/local");
    let work = interrupt_at_summary(first, &ai, &arrived, &release, &library);

    // The identity the command line bound before, for the same options: its digest is
    // the task's id, which names the folder.
    let opts = course2md::options::RunOpts {
        json: true,
        out: Some("out".into()),
        transcript_source: Some(TranscriptSource::Subtitle),
        formats: Some(vec![course2md::config::OutputFormat::Md]),
        slide_mode: Some(SlideMode::First),
        llm: true,
        summarize: true,
        llm_model: Some("test-model".into()),
        no_llm_hint: true,
        model_dir: Some(root.path().join("models")),
        llm_api_key: Some("private-cli-key".into()),
        llm_base_url: Some(ai.url.clone()),
        ..Default::default()
    };
    let cfg = course2md::options::resolve(source.clone(), &opts, &ConfigFile::default()).unwrap();
    let source_id = format!(
        "local:sha256:{}",
        course2md::execution::file_digest(&video).unwrap()
    );
    let former = serde_json::json!({
        "source_id": source_id,
        "config": cfg.redacted_for_identity(&cfg.out_root),
        "title": "lecture",
    });
    assert_eq!(
        course2md::digest::json_digest(&former).unwrap().as_str(),
        work.file_name().unwrap().to_str().unwrap()
    );
    std::fs::write(
        work.join("task-identity.json"),
        serde_json::to_vec_pretty(&former).unwrap(),
    )
    .unwrap();
    // Its summary receipt says the outcome is unknown, as a run that saw the
    // connection drop would have recorded.
    let summary = course2md::dispatch::receipts(&work)
        .unwrap()
        .into_iter()
        .find(|receipt| receipt.purpose == "summary")
        .unwrap();
    let path = work
        .join("requests")
        .join(format!("{}.json", summary.stable_id));
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    saved["state"] = "uncertain".into();
    std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();

    let second = spawn_cli(root.path(), &source, &ai, |_| {})
        .wait_with_output()
        .unwrap();
    assert_continued_without_paying(&second, &ai, &work);
    assert_ne!(identity(&work), former);
}
