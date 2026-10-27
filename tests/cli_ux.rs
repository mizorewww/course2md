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
