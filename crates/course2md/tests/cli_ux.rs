//! Exercise the public CLI contract without network access or real credentials.
mod common;

use std::process::{Command, Output};

struct CliTest {
    dir: tempfile::TempDir,
}
impl CliTest {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        self.run_with_lang(args, "zh-Hans")
    }
    fn run_with_lang(&self, args: &[&str], lang: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_course2md"))
            .args(args)
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_CACHE_HOME", self.dir.path().join("cache"))
            .env("COURSE2MD_LANG", lang)
            .env_remove("RUST_LOG")
            .env_remove("COURSE2MD_ASR_API_KEY")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }
    fn config(&self, contents: &str) {
        let dir = self.dir.path().join("config/course2md");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), contents).unwrap();
    }
}
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
fn json_error(output: &Output) -> serde_json::Value {
    assert!(!output.status.success());
    assert!(output.stderr.is_empty(), "{}", stderr(output));
    let lines: Vec<_> = output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines.len(), 1, "{}", stdout(output));
    let value: serde_json::Value = serde_json::from_slice(lines[0]).unwrap();
    assert_eq!(value["type"], "error");
    value
}

/// The suite's one check that human-readable output follows the language;
/// the other tests assert language-independent facts.
#[test]
fn no_arguments_or_help_succeed_with_localized_guidance() {
    let cli = CliTest::new();
    for args in [vec![], vec!["--help"]] {
        let result = cli.run(&args);
        assert!(result.status.success());
        let text = stdout(&result);
        assert!(text.contains("快速开始"), "{text}");
        assert!(text.contains("语音识别"), "{text}");
    }
    let english = cli.run_with_lang(&["--help"], "en");
    assert!(english.status.success());
    let text = stdout(&english);
    assert!(text.contains("Quick start"), "{text}");
    assert!(text.contains("Speech recognition"), "{text}");
    let traditional = cli.run_with_lang(&["--help"], "zh-Hant");
    assert!(traditional.status.success());
    assert!(!stdout(&traditional).is_empty());
    for cmd in ["models", "llm", "config", "summarize", "remove", "doctor"] {
        assert!(cli.run(&[cmd, "--help"]).status.success(), "{cmd}");
    }
}

#[test]
fn missing_or_invalid_sources_fail_with_recovery() {
    let cli = CliTest::new();
    // The recovery is an example command, and the rejected path is echoed back.
    let missing = cli.run(&["--provider", "api"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("course2md ./lecture.mp4"),
        "{}",
        stderr(&missing)
    );
    assert!(missing.stdout.is_empty());
    let invalid = cli.run(&["./missing lecture.mp4"]);
    assert!(!invalid.status.success());
    assert!(
        stderr(&invalid).contains("./missing lecture.mp4"),
        "{}",
        stderr(&invalid)
    );
    assert!(invalid.stdout.is_empty());
}

#[test]
fn json_covers_missing_source_parse_and_config_errors() {
    let cli = CliTest::new();
    // Scripts read the stable code, never the message.
    for args in [
        &["--json"][..],
        &["./missing.mp4", "--json"],
        &["./missing.mp4", "--json", "--provider", "invalid"],
    ] {
        assert_eq!(
            json_error(&cli.run(args))["code"],
            "invalid_arguments",
            "{args:?}"
        );
    }
    std::fs::write(cli.dir.path().join("lecture.mp4"), []).unwrap();
    cli.config("[defaults\n");
    let error = json_error(&cli.run(&["./lecture.mp4", "--json"]));
    assert_eq!(error["code"], "invalid_configuration");
    assert!(error["message"].as_str().unwrap().contains("configuration"));
}

#[test]
fn validation_precedes_dependency_checks_and_downloads() {
    let cli = CliTest::new();
    std::fs::write(cli.dir.path().join("lecture.mp4"), []).unwrap();
    let result = cli.run(&["./lecture.mp4", "--sample-interval", "0", "--json"]);
    let error = json_error(&result);
    assert_eq!(error["code"], "invalid_arguments");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("--sample-interval")
    );
    assert!(!cli.dir.path().join("cache").exists());
}

#[test]
fn noninteractive_setup_explains_missing_values_without_saving() {
    let cli = CliTest::new();
    let result = cli.run(&["llm", "setup"]);
    assert!(!result.status.success());
    assert!(stderr(&result).contains("--base-url"));
    assert!(!cli.dir.path().join("config/course2md/config.toml").exists());
}

#[test]
fn model_listing_uses_configured_directory_and_rejects_partial_files() {
    let cli = CliTest::new();
    cli.config("[defaults]\nmodel_dir = 'custom models'\n");
    let dir = cli.dir.path().join("custom models/llama-qwen3-1.7b");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Qwen3-ASR-1.7B-Q8_0.gguf"), b"partial").unwrap();
    let result = cli.run_with_lang(&["models", "list"], "en");
    assert!(result.status.success());
    let text = stdout(&result);
    assert!(text.contains("custom models"));
    assert!(text.contains("Missing or incomplete"));
    assert!(text.contains("models download --dir"));
}

#[test]
fn model_inspection_accepts_aliases_and_uses_the_configured_directory() {
    let cli = CliTest::new();
    cli.config("[defaults]\nmodel_dir = 'custom models'\n");
    let result = cli.run(&[
        "models",
        "inspect",
        "--provider",
        "gpu",
        "--model",
        "qwen3",
        "--json",
    ]);
    assert!(result.status.success(), "{}", stderr(&result));
    let status: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(status["model"], "qwen3-1.7b");
    assert_eq!(status["can_prepare"], true);
    let part = status["parts"][0]["path"].as_str().unwrap();
    assert!(part.contains("custom models"), "{part}");
}

#[test]
fn config_show_reports_the_defaults_conversion_uses() {
    let cli = CliTest::new();
    let result = cli.run(&["config", "show"]);
    assert!(result.status.success());
    let text = stdout(&result);
    let value = |key: &str| {
        text.lines()
            .find(|line| line.trim_start().starts_with(key))
            .and_then(|line| line.split(':').nth(1))
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(value("resume "), "true");
    assert_eq!(value("stable_secs "), "0.8");
}

#[test]
fn settings_and_status_never_print_saved_keys() {
    let cli = CliTest::new();
    cli.config(
        "[llm]\napi_key = 'secret-llm-test-key'\n[asr_api]\napi_key = 'secret-asr-test-key'\n",
    );
    for args in [["config", "show"], ["llm", "status"]] {
        let result = cli.run(&args);
        assert!(result.status.success());
        assert!(!stdout(&result).contains("secret-"));
    }
}

#[test]
fn generated_template_parses_and_does_not_overwrite_existing_config() {
    let cli = CliTest::new();
    assert!(cli.run(&["config", "init"]).status.success());
    assert!(cli.run(&["config", "show"]).status.success());
    cli.config("[defaults]\nthreads = 7\n");
    assert!(!cli.run(&["config", "init"]).status.success());
    let file =
        std::fs::read_to_string(cli.dir.path().join("config/course2md/config.toml")).unwrap();
    assert!(file.contains("threads = 7"));

    // --force replaces it, and the hand-edited file stays beside the new one.
    assert!(cli.run(&["config", "init", "--force"]).status.success());
    let dir = cli.dir.path().join("config/course2md");
    let file = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(!file.contains("threads = 7"));
    let backup = std::fs::read_to_string(dir.join("config.toml.bak")).unwrap();
    assert!(backup.contains("threads = 7"));
}

#[test]
fn doctor_reports_required_failures_with_nonzero_exit() {
    let cli = CliTest::new();
    let result = Command::new(env!("CARGO_BIN_EXE_course2md"))
        .env("COURSE2MD_LANG", "zh-Hans")
        .arg("doctor")
        .env("PATH", "")
        .env("XDG_CONFIG_HOME", cli.dir.path().join("config"))
        .env("XDG_CACHE_HOME", cli.dir.path().join("cache"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!result.status.success());
    // With an empty PATH the required tools are listed as failed.
    for tool in ["ffmpeg", "ffprobe"] {
        assert!(
            stdout(&result).contains(&format!("✗ {tool}")),
            "{}",
            stdout(&result)
        );
    }
    assert!(!result.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn remote_download_output_does_not_leak_into_json_or_quiet_mode() {
    use std::os::unix::fs::PermissionsExt;
    if !common::ffmpeg_available() {
        return;
    }
    let cli = CliTest::new();
    // Use real ffmpeg/ffprobe, with an offline yt-dlp fixture that emits noisy output.
    let video = cli.dir.path().join("fixture.mp4");
    common::fixture_video(&video, 3);
    let bin = cli.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("yt-dlp");
    std::fs::write(&script, r#"#!/bin/sh
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
  printf '1\n00:00:00,000 --> 00:00:02,500\nWelcome to the lecture.\n' > "$output.en.srt"
else
  /bin/cp "$CLI_TEST_VIDEO" "$output"
fi
if [ "$json" -eq 1 ]; then
  printf '%s\n' '{"title":"Test lecture","id":"test","webpage_url":"https://example.test/video","duration":3,"extractor":"test","subtitles":{"en":[{"ext":"srt","url":"https://example.test/en.srt"}]},"automatic_captions":{}}'
else
  printf 'raw downloader output\n'
fi
printf 'raw downloader stderr\n' >&2
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    for flag in ["--json", "--quiet"] {
        let output_dir = cli.dir.path().join(flag.trim_start_matches('-'));
        let result = Command::new(env!("CARGO_BIN_EXE_course2md"))
            .env("COURSE2MD_LANG", "zh-Hans")
            .args([
                "https://example.test/video",
                "--transcript-source",
                "subtitle",
                flag,
                "-o",
            ])
            .arg(&output_dir)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("CLI_TEST_VIDEO", &video)
            .env("XDG_CONFIG_HOME", cli.dir.path().join("config"))
            .env("XDG_CACHE_HOME", cli.dir.path().join("cache"))
            .env("RUST_LOG", "info")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{} {}",
            stdout(&result),
            stderr(&result)
        );
        assert!(result.stderr.is_empty(), "{}", stderr(&result));
        if flag == "--quiet" {
            assert!(result.stdout.is_empty(), "{}", stdout(&result));
        } else {
            let events: Vec<serde_json::Value> = stdout(&result)
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let done = events.last().unwrap();
            assert_eq!(done["type"], "done");
            let notes = std::path::Path::new(done["out_dir"].as_str().unwrap()).join("course.md");
            assert!(
                std::fs::read_to_string(notes)
                    .unwrap()
                    .contains("Welcome to the lecture.")
            );
        }
    }
    assert!(video.is_file());
}

#[test]
fn version_flags_include_the_build_commit() {
    let cli = CliTest::new();
    let expected = format!(
        "course2md {} ({})\n",
        env!("CARGO_PKG_VERSION"),
        env!("COURSE2MD_COMMIT_HASH")
    );
    for flag in ["-V", "--version"] {
        let result = cli.run(&[flag]);
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        assert_eq!(stdout(&result), expected);
    }
}

#[test]
fn a_qwen3_npu_choice_from_2_0_0_falls_back_to_whisper_with_a_notice() {
    let cli = CliTest::new();
    cli.config("[defaults]\nprovider = 'npu'\nasr_model = 'qwen3-1.7b'\n");
    let result = cli.run_with_lang(&["config", "show"], "en");
    assert!(result.status.success(), "{}", stderr(&result));
    assert!(
        stderr(&result).contains("Qwen3 is no longer available on Intel NPU"),
        "{}",
        stderr(&result)
    );
    assert!(
        !stdout(&result).contains("qwen3-1.7b"),
        "{}",
        stdout(&result)
    );
    // Asking for it explicitly is refused in the interface language.
    let result = cli.run_with_lang(
        &[
            "models",
            "prepare",
            "--provider",
            "npu",
            "--model",
            "qwen3-1.7b",
        ],
        "ja",
    );
    assert!(!result.status.success());
    assert!(
        stderr(&result).contains("Qwen3 モデル"),
        "{}",
        stderr(&result)
    );
}

#[test]
fn an_unsupported_model_is_quoted_in_every_language() {
    let cli = CliTest::new();
    for (lang, quoted) in [
        ("en", "\"whisper-x\""),
        ("zh-Hans", "「whisper-x」"),
        ("zh-Hant", "「whisper-x」"),
        ("ja", "「whisper-x」"),
    ] {
        let result = cli.run_with_lang(
            &[
                "models",
                "prepare",
                "--provider",
                "cpu",
                "--model",
                "whisper-x",
            ],
            lang,
        );
        assert!(!result.status.success());
        assert!(
            stderr(&result).contains(quoted),
            "{lang}: {}",
            stderr(&result)
        );
    }
}

/// Transport crates dump what a request sends at trace level, `Authorization`
/// header included; however `RUST_LOG` is set, the key never reaches a log.
#[test]
fn trace_logging_never_shows_the_api_key() {
    const KEY: &str = "sk-Zq8Xv3Lp9Wm2Tn7Rb4Yc6Hd5";
    for spec in ["trace", "ureq_proto=trace,ureq=trace,rustls=trace"] {
        let cli = CliTest::new();
        let (url, server) = c2m_test_http::serve(vec![c2m_test_http::json_response(
            r#"{"choices":[{"message":{"content":"ok"}}]}"#,
        )]);
        let output = Command::new(env!("CARGO_BIN_EXE_course2md"))
            .args(["llm", "setup", "--base-url", &url, "--model", "m"])
            .args(["--api-key", KEY])
            .current_dir(cli.dir.path())
            .env("XDG_CONFIG_HOME", cli.dir.path().join("config"))
            .env("XDG_CACHE_HOME", cli.dir.path().join("cache"))
            .env("COURSE2MD_LANG", "en")
            .env("RUST_LOG", spec)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let requests = server.join().unwrap();
        assert!(
            requests[0].head.contains(KEY),
            "the request must carry the key for this test to mean anything"
        );
        assert!(output.status.success(), "{}", stderr(&output));
        for text in [stdout(&output), stderr(&output)] {
            assert!(!common::shows_part_of(&text, KEY), "{spec}: {text}");
        }
    }
}

/// An online run picks its subtitle track in the interface language, as the
/// desktop does, in plain and `--json` output alike. A task keeps its first
/// choice when it continues in another language, and a work folder from a
/// version that preferred Chinese keeps Chinese, so paid requests made from
/// the transcript keep their ids.
#[cfg(unix)]
#[test]
fn online_subtitles_follow_the_interface_language_and_stay_with_the_task() {
    use std::os::unix::fs::PermissionsExt;
    let cli = CliTest::new();
    let log = cli.dir.path().join("yt-dlp.log");
    let fake = cli.dir.path().join("yt-dlp");
    // Offers Simplified Chinese and English tracks of a French video; records
    // the chosen track, then refuses the video download, which ends the run.
    std::fs::write(
        &fake,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo 2026.08.19; exit 0; fi
json='{{"id":"abcdefghijk","title":"Fake lecture","duration":240,"webpage_url":"https://www.youtube.com/watch?v=abcdefghijk","extractor":"youtube","extractor_key":"Youtube","_type":"video","language":"fr","subtitles":{{"zh-Hans":[{{"ext":"vtt","url":"http://127.0.0.1:1/zh.vtt"}}],"en":[{{"ext":"vtt","url":"http://127.0.0.1:1/en.vtt"}}]}},"automatic_captions":{{}}}}'
prev=""; langs=""; out=""; skip=""
for arg in "$@"; do
  [ "$prev" = "--sub-langs" ] && langs="$arg"
  [ "$prev" = "-o" ] && out="$arg"
  [ "$arg" = "--skip-download" ] && skip=1
  prev="$arg"
done
if [ -n "$skip" ]; then
  lang="${{langs#^}}"; lang="${{lang%$}}"
  echo "$lang" >> "{log}"
  printf '1\n00:00:00,000 --> 00:00:02,000\nsubtitle in %s\n' "$lang" > "$out.$lang.srt"
  echo "$json"; exit 0
fi
for arg in "$@"; do [ "$arg" = "--dump-single-json" ] && {{ echo "$json"; exit 0; }}; done
echo "ERROR: [youtube] abcdefghijk: Private video" >&2
exit 1
"#,
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |lang: &str, extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_course2md"))
            .args(["https://www.youtube.com/watch?v=abcdefghijk"])
            .args(["--transcript-source", "subtitle", "-o"])
            .arg(cli.dir.path().join("out"))
            .args(extra)
            .current_dir(cli.dir.path())
            .env("HOME", cli.dir.path())
            .env("XDG_CONFIG_HOME", cli.dir.path().join("config"))
            .env("XDG_CACHE_HOME", cli.dir.path().join("cache"))
            .env("COURSE2MD_LANG", lang)
            .env("COURSE2MD_YTDLP", &fake)
            .env_remove("RUST_LOG")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let chosen = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        (chosen.trim().to_owned(), output)
    };
    let fresh = ["--no-resume"];
    for (lang, extra, track) in [
        ("en", &fresh[..], "en"),
        ("en", &["--no-resume", "--json"][..], "en"),
        ("zh-Hans", &fresh[..], "zh-Hans"),
        ("zh-Hans", &["--no-resume", "--json"][..], "zh-Hans"),
    ] {
        let (chosen, output) = run(lang, extra);
        assert_eq!(chosen, track, "{lang} {extra:?}: {}", stderr(&output));
    }
    // A task keeps its first choice when it continues in another language.
    assert_eq!(run("en", &[]).0, "en");
    assert_eq!(run("zh-Hans", &[]).0, "en");
    // A work folder from before the choice was recorded preferred Chinese.
    let work = std::fs::read_dir(cli.dir.path().join("out/youtube"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join(".work");
    let mut records = 0;
    for task in std::fs::read_dir(&work).unwrap() {
        let record = task.unwrap().path().join(".subs/language");
        if record.is_file() {
            std::fs::remove_file(record).unwrap();
            records += 1;
        }
    }
    assert!(records > 0);
    assert_eq!(run("en", &[]).0, "zh-Hans");
}

/// A machine-translated track never comes before the original (#25). YouTube
/// lists an English video's translations beside its captions (yt-dlp marks them
/// with `tlang`) and answered 429 for them, which ended the run. A track that
/// cannot be read is followed by the next ones, at most three in all.
#[cfg(unix)]
#[test]
fn online_subtitles_prefer_the_original_and_fall_back_when_one_fails() {
    use std::os::unix::fs::PermissionsExt;
    let cli = CliTest::new();
    let log = cli.dir.path().join("yt-dlp.log");
    let fail = cli.dir.path().join("fail");
    let fake = cli.dir.path().join("yt-dlp");
    // yt-dlp's YouTube track list for an English video with English subtitles:
    // their translations (`<lang>-en`), the captions' translations, and the
    // captions themselves as `en-orig` and `en`. Translations and the keys in
    // `fail` answer 429; the video download is refused, which ends the run.
    let track = |name: &str, tlang: &str| {
        format!(
            r#"[{{"ext":"vtt","name":"{name}","url":"https://www.youtube.com/api/timedtext?v=abcdefghijk&lang=en{tlang}&fmt=vtt"}}]"#
        )
    };
    let json = format!(
        r#"{{"id":"abcdefghijk","title":"Fake lecture","duration":240,"webpage_url":"https://www.youtube.com/watch?v=abcdefghijk","extractor":"youtube","extractor_key":"Youtube","_type":"video","language":"en","subtitles":{{"en":{}}},"automatic_captions":{{"zh-Hans-en":{},"ja-en":{},"zh-Hans":{},"en-orig":{},"en":{},"ja":{}}}}}"#,
        track("English", ""),
        track("Chinese (Simplified) from English", "&tlang=zh-Hans"),
        track("Japanese from English", "&tlang=ja"),
        track("Chinese (Simplified)", "&tlang=zh-Hans"),
        track("English (Original)", ""),
        track("English", ""),
        track("Japanese", "&tlang=ja"),
    );
    std::fs::write(
        &fake,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo 2026.08.19; exit 0; fi
json='{json}'
prev=""; langs=""; out=""; skip=""; channel=""
for arg in "$@"; do
  [ "$prev" = "--sub-langs" ] && langs="$arg"
  [ "$prev" = "-o" ] && out="$arg"
  [ "$arg" = "--skip-download" ] && skip=1
  [ "$arg" = "--write-subs" ] && channel=manual
  [ "$arg" = "--write-auto-subs" ] && channel=auto
  prev="$arg"
done
if [ -n "$skip" ]; then
  lang=$(printf '%s' "$langs" | sed -e 's/^\^//' -e 's/\$$//' -e 's/\\//g')
  echo "$channel $lang" >> "{log}"
  case "$channel $lang" in
    "manual en"|"auto en"|"auto en-orig") grep -qx "$channel $lang" "{fail}" 2>/dev/null || ok=1 ;;
  esac
  if [ -z "$ok" ]; then
    echo "ERROR: Unable to download video subtitles for '$lang': HTTP Error 429: Too Many Requests" >&2
    exit 1
  fi
  printf '1\n00:00:00,000 --> 00:00:02,000\nsubtitle %s\n' "$channel $lang" > "$out.$lang.srt"
  echo "$json"; exit 0
fi
for arg in "$@"; do [ "$arg" = "--dump-single-json" ] && {{ echo "$json"; exit 0; }}; done
echo "ERROR: [youtube] abcdefghijk: Private video" >&2
exit 1
"#,
            log = log.display(),
            fail = fail.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |lang: &str, failing: &[&str]| {
        std::fs::write(&fail, failing.join("\n") + "\n").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_course2md"))
            .args(["https://www.youtube.com/watch?v=abcdefghijk"])
            .args(["--transcript-source", "subtitle", "--no-resume", "-o"])
            .arg(cli.dir.path().join("out"))
            .current_dir(cli.dir.path())
            .env("HOME", cli.dir.path())
            .env("XDG_CONFIG_HOME", cli.dir.path().join("config"))
            .env("XDG_CACHE_HOME", cli.dir.path().join("cache"))
            .env("COURSE2MD_LANG", lang)
            .env("COURSE2MD_YTDLP", &fake)
            .env_remove("RUST_LOG")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let tried = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        (
            tried.lines().map(str::to_owned).collect::<Vec<_>>(),
            stderr(&output),
        )
    };
    // Whatever the interface language, the English subtitles come first.
    for lang in ["zh-Hans", "zh-Hant", "ja", "en"] {
        let (tried, text) = run(lang, &[]);
        assert_eq!(tried, ["manual en"], "{lang}: {text}");
    }
    // The subtitles fail: the captions are next; `en-orig` is `en` again.
    let (tried, text) = run("zh-Hans", &["manual en"]);
    assert_eq!(tried, ["manual en", "auto en"], "{text}");
    assert!(text.contains("429"), "{text}");
    assert!(text.contains("下一条字幕"), "{text}");
    assert!(!text.contains("读取视频字幕失败"), "{text}");
    // Nothing readable in three tries: the first failure is the one reported.
    let (tried, text) = run("zh-Hans", &["manual en", "auto en"]);
    assert_eq!(tried, ["manual en", "auto en", "auto zh-Hans"], "{text}");
    assert!(text.contains("读取视频字幕失败"), "{text}");
    assert!(text.contains("for 'en'"), "{text}");
    // In English nothing else is the video's or the person's language: no
    // translation into another language is read in their place.
    let (tried, text) = run("en", &["manual en", "auto en"]);
    assert_eq!(tried, ["manual en", "auto en"], "{text}");
    assert!(text.contains("for 'en'"), "{text}");
}

/// Password and SMS login type into a terminal; without one the error says what
/// to do instead, in the interface language, before anything is sent.
#[test]
fn password_login_without_a_terminal_explains_what_to_do() {
    let cli = CliTest::new();
    for method in ["password", "sms"] {
        for (lang, expected) in [
            ("en", "Password and SMS login need a terminal to type in."),
            (
                "ja",
                "パスワードと SMS のログインにはターミナルでの入力が必要です。",
            ),
        ] {
            let output = cli.run_with_lang(&["--login", "bilibili", "--method", method], lang);
            assert!(!output.status.success());
            let text = stderr(&output);
            assert!(text.contains(expected), "{method} {lang}: {text}");
            assert!(!text.contains("not a terminal"), "{text}");
        }
    }
}

/// The values' descriptions in `--help` follow the interface language too.
#[test]
fn value_descriptions_in_help_are_localized() {
    let cli = CliTest::new();
    let help = stdout(&cli.run_with_lang(&["--help"], "en"));
    assert!(
        help.contains("- qr:       Scan a QR code (default)"),
        "{help}"
    );
    assert!(help.contains("Prefer subtitles; transcribe speech if unavailable"));
    let help = stdout(&cli.run_with_lang(&["--help"], "ja"));
    assert!(help.contains("QR コードをスキャン（既定）"), "{help}");
    assert!(help.contains("字幕を優先し、なければ音声を認識"), "{help}");
    assert!(!help.contains("Scan a QR code"), "{help}");
    assert!(help.contains("使用できる値："), "{help}");
    assert!(help.contains("[使用できる値：bilibili, youtube"), "{help}");
    assert!(!help.to_lowercase().contains("possible values"), "{help}");
}

/// A video command that needs a local speech model it lacks: a local video, CPU speech
/// recognition, a stand-in `llama-server`, and a fake model host that records each
/// request and answers 404, so nothing is ever fetched from the network.
#[cfg(unix)]
struct MissingModel {
    cli: CliTest,
    video: std::path::PathBuf,
    host: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(unix)]
impl MissingModel {
    fn new() -> Option<Self> {
        use std::io::{BufRead, Write};
        use std::os::unix::fs::PermissionsExt;
        if !common::ffmpeg_available() {
            return None;
        }
        let cli = CliTest::new();
        let video = cli.dir.path().join("lecture.mp4");
        common::fixture_video(&video, 2);
        let bin = cli.dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let llama = bin.join("llama-server");
        std::fs::write(&llama, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&llama, std::fs::Permissions::from_mode(0o755)).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut line = String::new();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let _ = reader.read_line(&mut line);
                seen.lock().unwrap().push(line.trim().to_owned());
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        Some(Self {
            cli,
            video,
            host,
            requests,
        })
    }

    /// `program` in an isolated environment.
    fn command(&self, program: &str, lang: &str) -> Command {
        let dir = self.cli.dir.path();
        let mut command = Command::new(program);
        command
            .current_dir(dir)
            .env("HOME", dir.join("home"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("QWEN3_CACHE_DIR", dir.join("apple"))
            .env("HF_HOME", dir.join("hf"))
            .env("HF_ENDPOINT", &self.host)
            .env("COURSE2MD_NO_ARIA2", "1")
            .env("COURSE2MD_LANG", lang)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    dir.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("COURSE2MD_DOWNLOAD_MODELS")
            .env_remove("COURSE2MD_ASR_API_KEY")
            .env_remove("RUST_LOG");
        for proxy in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
        ] {
            command.env_remove(proxy);
        }
        command
    }

    fn args<'a>(&'a self, extra: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec![
            self.video.to_str().unwrap(),
            "--transcript-source",
            "asr",
            "--provider",
            "cpu",
            "-o",
            "notes",
        ];
        args.extend_from_slice(extra);
        args
    }

    fn run(&self, extra: &[&str], lang: &str) -> Output {
        self.command(env!("CARGO_BIN_EXE_course2md"), lang)
            .args(self.args(extra))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    /// Runs the video command on a pseudo-terminal through `script`, typing `answer`.
    fn run_at_terminal(&self, answer: &str) -> Option<(std::process::ExitStatus, String)> {
        use std::io::Write;
        if !std::path::Path::new("/usr/bin/script").exists() {
            eprintln!("skip: script not found");
            return None;
        }
        let binary = env!("CARGO_BIN_EXE_course2md");
        let mut command = self.command("/usr/bin/script", "en");
        if cfg!(target_os = "macos") {
            command
                .args(["-q", "/dev/null", binary])
                .args(self.args(&[]));
        } else {
            let line = std::iter::once(binary)
                .chain(self.args(&[]))
                .map(|arg| format!("'{arg}'"))
                .collect::<Vec<_>>()
                .join(" ");
            command.args(["-qec", &line, "/dev/null"]);
        }
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Answer once the question is on the screen, then close the input.
        let mut screen = child.stdout.take().unwrap();
        let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let reader = {
            let shown = shown.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut chunk = [0u8; 4096];
                while let Ok(n) = screen.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    shown.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !String::from_utf8_lossy(&shown.lock().unwrap()).contains("[y/N]") {
            assert!(
                std::time::Instant::now() < deadline,
                "no question: {}",
                String::from_utf8_lossy(&shown.lock().unwrap())
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(answer.as_bytes()).unwrap();
        stdin.flush().unwrap();
        let status = child.wait().unwrap();
        drop(stdin);
        reader.join().unwrap();
        let text = String::from_utf8_lossy(&shown.lock().unwrap()).into_owned();
        Some((status, text))
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn assert_nothing_fetched(&self) {
        assert_eq!(self.requests(), Vec::<String>::new());
        let models = self.cli.dir.path().join("cache/course2md/models");
        let files: Vec<_> = walk(&models);
        assert!(
            files.iter().all(|file| !file.ends_with(".gguf")),
            "{files:?}"
        );
    }

    /// Waits briefly for the fake host to log a request for the speech model.
    fn assert_download_attempted(&self) {
        for _ in 0..50 {
            if self
                .requests()
                .iter()
                .any(|line| line.contains("/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main/"))
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("no model request: {:?}", self.requests());
    }
}

#[cfg(unix)]
fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path.display().to_string());
        }
    }
    out
}

/// Without a terminal, a missing speech model stops the command before the video is
/// processed; nothing is downloaded, and the message says how to prepare it.
#[cfg(unix)]
#[test]
fn a_missing_speech_model_is_not_downloaded_without_a_terminal() {
    let Some(scene) = MissingModel::new() else {
        return;
    };
    for (lang, words) in [
        ("en", "is not downloaded without your consent"),
        ("zh-Hans", "未经同意不会下载"),
        ("zh-Hant", "未經同意不會下載"),
        ("ja", "同意なしにはダウンロードしません"),
    ] {
        let result = scene.run(&[], lang);
        assert_eq!(result.status.code(), Some(1), "{}", stderr(&result));
        let text = stderr(&result);
        assert!(text.contains(words), "{lang}: {text}");
        assert!(
            text.contains("course2md models prepare --provider cpu --model qwen3-1.7b"),
            "{text}"
        );
        assert!(text.contains("--download-models"), "{text}");
    }
    // --quiet is no terminal conversation either.
    let result = scene.run(&["--quiet"], "en");
    assert_eq!(result.status.code(), Some(1));
    assert!(stderr(&result).contains("--download-models"));
    scene.assert_nothing_fetched();
}

/// `--json` ends with a stable error event in Simplified Chinese, coded
/// `model_required`, whatever the interface language.
#[cfg(unix)]
#[test]
fn json_reports_a_missing_speech_model_as_model_required() {
    let Some(scene) = MissingModel::new() else {
        return;
    };
    let result = scene.run(&["--json"], "en");
    assert_eq!(result.status.code(), Some(1));
    let text = stdout(&result);
    let last: serde_json::Value =
        serde_json::from_str(text.lines().last().expect("events")).unwrap();
    assert_eq!(last["type"], "error", "{text}");
    assert_eq!(last["code"], "model_required", "{text}");
    let message = last["message"].as_str().unwrap();
    assert!(message.contains("未经同意不会下载"), "{message}");
    assert!(message.contains("--download-models"), "{message}");
    scene.assert_nothing_fetched();
}

/// `--download-models` and `COURSE2MD_DOWNLOAD_MODELS=1` restore the download for
/// scripts, without a question.
#[cfg(unix)]
#[test]
fn download_models_lets_a_script_download_the_model() {
    let Some(scene) = MissingModel::new() else {
        return;
    };
    let result = scene.run(&["--download-models", "--json"], "en");
    assert!(!result.status.success());
    assert!(
        !stdout(&result).contains("model_required"),
        "{}",
        stdout(&result)
    );
    scene.assert_download_attempted();

    let Some(scene) = MissingModel::new() else {
        return;
    };
    let result = scene
        .command(env!("CARGO_BIN_EXE_course2md"), "en")
        .args(scene.args(&[]))
        .env("COURSE2MD_DOWNLOAD_MODELS", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        !stderr(&result).contains("--download-models"),
        "{}",
        stderr(&result)
    );
    scene.assert_download_attempted();
}

/// `models prepare` is the explicit command: it downloads without asking.
#[cfg(unix)]
#[test]
fn models_prepare_downloads_without_asking() {
    let Some(scene) = MissingModel::new() else {
        return;
    };
    let result = scene
        .command(env!("CARGO_BIN_EXE_course2md"), "en")
        .args(["models", "prepare", "--provider", "cpu"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!stderr(&result).contains("[y/N]"), "{}", stderr(&result));
    scene.assert_download_attempted();
}

/// At a terminal the command asks, naming the model, its size and where it goes;
/// no (the default) stops with how to prepare it later, yes downloads.
#[cfg(unix)]
#[test]
fn a_terminal_is_asked_before_the_model_is_downloaded() {
    let Some(scene) = MissingModel::new() else {
        return;
    };
    let Some((status, text)) = scene.run_at_terminal("n\n") else {
        return;
    };
    assert_eq!(status.code(), Some(1), "{text}");
    assert!(
        text.contains("The speech model qwen3-1.7b (about 2.3 GB)"),
        "{text}"
    );
    assert!(text.contains("llama-qwen3-1.7b"), "{text}");
    assert!(text.contains("Download it now? [y/N]"), "{text}");
    assert!(text.contains("was not downloaded. To prepare it later, run course2md models prepare --provider cpu --model qwen3-1.7b"), "{text}");
    scene.assert_nothing_fetched();

    let (status, text) = scene.run_at_terminal("\n").unwrap();
    assert_eq!(status.code(), Some(1), "{text}");
    assert!(text.contains("was not downloaded"), "{text}");
    scene.assert_nothing_fetched();

    let (status, text) = scene.run_at_terminal("y\n").unwrap();
    assert!(!status.success(), "{text}");
    assert!(!text.contains("was not downloaded"), "{text}");
    scene.assert_download_attempted();
}

/// The opt-in is described in every interface language.
#[test]
fn download_models_help_is_localized() {
    let cli = CliTest::new();
    for (lang, words) in [
        ("en", "Download a missing speech model without asking"),
        ("zh-Hans", "缺少识别模型时直接下载，不再询问"),
        ("zh-Hant", "缺少辨識模型時直接下載，不再詢問"),
        ("ja", "音声認識モデルがなければ確認せずにダウンロード"),
    ] {
        let help = stdout(&cli.run_with_lang(&["--help"], lang));
        assert!(help.contains("--download-models"), "{lang}: {help}");
        assert!(help.contains(words), "{lang}: {help}");
    }
}
