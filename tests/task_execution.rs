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
        version_id: "version-one".into(),
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
    let ffmpeg = course2md::runtime::which("ffmpeg").unwrap();
    let python = course2md::runtime::which("python3").unwrap();
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
    let version = request.course_dir.join("versions/version-one");
    let manifest = artifact::read_manifest(&version.join("manifest.json")).unwrap();
    assert!(!manifest.frames.is_empty());
    artifact::validate_version(&version, &manifest).unwrap();
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
    let manifest = artifact::read_manifest(
        &request
            .course_dir
            .join("versions/version-one/manifest.json"),
    )
    .unwrap();
    assert!(manifest.partial);
    assert!(manifest.frames.is_empty());
    assert_eq!(
        manifest.outcomes.screenshots.status,
        artifact::Status::Failed
    );
    let body =
        std::fs::read_to_string(request.course_dir.join("versions/version-one/course.md")).unwrap();
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
    assert!(!request.course_dir.join("current.json").exists());
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
    assert!(!request.course_dir.join("current.json").exists());
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
    let manifest = artifact::read_manifest(
        &initial
            .course_dir
            .join("versions/version-one/manifest.json"),
    )
    .unwrap();
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
    let base = initial.course_dir.join("versions/version-one");
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
    std::fs::write(
        base.join("course.md"),
        "# 我的人工补充\n\n旧版本不能被补摘要覆盖。\n",
    )
    .unwrap();
    let original_files = std::fs::read_dir(&base)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect::<Vec<_>>();
    std::fs::remove_file(&source).unwrap();
    let mut retry = initial.clone();
    retry.task_id = "summary-retry".into();
    retry.version_id = "summary-retry".into();
    retry.work_dir = root.path().join("retry-work");
    retry.operation = course2md::execution::Operation::Reprocess {
        base_version_dir: base.clone(),
        components: vec![Component::Summary],
        prior_work_dir: Some(initial.work_dir.clone()),
    };
    let second = run(root.path(), &retry);
    let second_events = events(&second);
    // The summary was the only thing asked for and failed again, so no new
    // version repeats the old one; the error says why.
    assert!(!second.status.success(), "{second_events:#?}");
    assert!(second_events.iter().any(|event| {
        event["type"] == "error"
            && event["code"] == "parameters"
            && event["message"]
                .as_str()
                .is_some_and(|m| m.contains("HTTP 400"))
    }));
    assert!(!initial.course_dir.join("versions/summary-retry").exists());
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
    final_retry.version_id = "summary-final".into();
    final_retry.work_dir = root.path().join("final-work");
    final_retry.operation = course2md::execution::Operation::Reprocess {
        base_version_dir: base.clone(),
        components: vec![Component::Summary],
        // A failed retry left no version; the next repair starts from the
        // base version's own task, as the desktop's repair does.
        prior_work_dir: Some(initial.work_dir.clone()),
    };
    let third = run(root.path(), &final_retry);
    assert!(third.status.success(), "{:?}", events(&third));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 4);
    let final_dir = initial.course_dir.join("versions/summary-final");
    let manifest = artifact::read_manifest(&final_dir.join("manifest.json")).unwrap();
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
    for (path, bytes) in original_files {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
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

#[test]
fn summarize_command_adds_a_summary_as_a_new_version() {
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
    let base = initial.course_dir.join("versions/version-one");
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
    let versions: Vec<_> = std::fs::read_dir(initial.course_dir.join("versions"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("summary-"))
        .collect();
    assert_eq!(versions.len(), 1, "{versions:?}");
    let version = initial.course_dir.join("versions").join(&versions[0]);
    let manifest = artifact::read_manifest(&version.join("manifest.json")).unwrap();
    assert_eq!(
        manifest.outcomes.summary.status,
        artifact::Status::Succeeded
    );
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
    let base = original.course_dir.join("versions/version-one");
    let manifest = artifact::read_manifest(&base.join("manifest.json")).unwrap();
    assert_eq!(manifest.outcomes.summary.status, artifact::Status::Failed);
    let untouched = std::fs::read(base.join("document.json")).unwrap();
    std::fs::remove_file(&source).unwrap();
    let mut retry = original.clone();
    retry.task_id = "retry-polish".into();
    retry.version_id = "retry-polish".into();
    retry.work_dir = root.path().join("retry-work");
    retry.control_path = Some(retry.work_dir.join("control.json"));
    retry.operation = course2md::execution::Operation::Reprocess {
        base_version_dir: base.clone(),
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
    let updated =
        artifact::read_manifest(&retry.course_dir.join("versions/retry-polish/manifest.json"))
            .unwrap();
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
    assert_eq!(
        std::fs::read(base.join("document.json")).unwrap(),
        untouched
    );
}

#[test]
fn export_only_recovery_uses_the_saved_version_after_source_and_subtitle_are_gone() {
    use course2md::config::OutputFormat;
    if !common::ffmpeg_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    std::fs::write(&source, b"broken").unwrap();
    let original = request(root.path(), &source);
    assert!(run(root.path(), &original).status.success());
    let base = original.course_dir.join("versions/version-one");
    let pointer = std::fs::read(original.course_dir.join("current.json")).unwrap();
    let mut export = original.clone();
    export.task_id = "exports".into();
    export.version_id = "unused-new-body".into();
    export.work_dir = root.path().join("export-work");
    export.subtitle_events = None;
    export.subtitle = Some(root.path().join("deleted-subtitles.srt"));
    export.operation = course2md::execution::Operation::Reprocess {
        base_version_dir: base,
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
    }
    assert_eq!(
        std::fs::read(original.course_dir.join("current.json")).unwrap(),
        pointer
    );
    assert!(
        !original
            .course_dir
            .join("versions/unused-new-body")
            .exists()
    );
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
