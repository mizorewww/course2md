//! Exercise the public CLI contract without network access or real credentials.
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
        Command::new(env!("CARGO_BIN_EXE_course2md"))
            .args(args)
            .current_dir(self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_CACHE_HOME", self.dir.path().join("cache"))
            .env_remove("RUST_LOG")
            .env_remove("COURSE2MD_ASR_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
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

#[test]
fn no_arguments_or_help_succeed_with_bilingual_guidance() {
    let cli = CliTest::new();
    for args in [vec![], vec!["--help"]] {
        let result = cli.run(&args);
        assert!(result.status.success());
        let text = stdout(&result);
        assert!(text.contains("快速开始 / Quick start"));
        assert!(text.contains("Speech recognition"));
        assert!(!text.contains("提交代码"));
    }
    for cmd in ["models", "llm", "config", "summarize", "remove", "doctor"] {
        assert!(cli.run(&[cmd, "--help"]).status.success(), "{cmd}");
    }
}

#[test]
fn missing_or_invalid_sources_fail_with_recovery() {
    let cli = CliTest::new();
    let missing = cli.run(&["--provider", "api"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("Missing video source"));
    let invalid = cli.run(&["./missing lecture.mp4"]);
    assert!(!invalid.status.success());
    assert!(stderr(&invalid).contains("Check the path"));
    assert!(invalid.stdout.is_empty());
}

#[test]
fn json_covers_missing_source_parse_and_config_errors() {
    let cli = CliTest::new();
    json_error(&cli.run(&["--json"]));
    json_error(&cli.run(&["./missing.mp4", "--json"]));
    json_error(&cli.run(&["./missing.mp4", "--json", "--provider", "invalid"]));
    std::fs::write(cli.dir.path().join("lecture.mp4"), []).unwrap();
    cli.config("[defaults\n");
    let error = json_error(&cli.run(&["./lecture.mp4", "--json"]));
    assert!(error["message"].as_str().unwrap().contains("configuration"));
}

#[test]
fn validation_precedes_dependency_checks_and_downloads() {
    let cli = CliTest::new();
    std::fs::write(cli.dir.path().join("lecture.mp4"), []).unwrap();
    let result = cli.run(&["./lecture.mp4", "--sample-interval", "0", "--json"]);
    let error = json_error(&result);
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
    let result = cli.run(&["models", "list"]);
    assert!(result.status.success());
    let text = stdout(&result);
    assert!(text.contains("custom models"));
    assert!(text.contains("Missing or incomplete"));
    assert!(text.contains("models download --dir"));
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
        .arg("doctor")
        .env("PATH", "")
        .env("XDG_CONFIG_HOME", cli.dir.path().join("config"))
        .env("XDG_CACHE_HOME", cli.dir.path().join("cache"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(stdout(&result).contains("Required tools"));
    assert!(stderr(&result).contains("required-tool or configuration issues"));
}

#[cfg(all(unix, feature = "integration"))]
#[test]
fn remote_download_output_does_not_leak_into_json_or_quiet_mode() {
    use std::os::unix::fs::PermissionsExt;
    let cli = CliTest::new();
    // Use real ffmpeg/ffprobe, with an offline yt-dlp fixture that emits noisy output.
    let video = cli.dir.path().join("fixture.mp4");
    let generated = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=320x180:d=3",
            "-c:v",
            "libx264",
            "-y",
        ])
        .arg(&video)
        .output()
        .unwrap();
    assert!(generated.status.success(), "{}", stderr(&generated));
    let bin = cli.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("yt-dlp");
    std::fs::write(&script, r#"#!/bin/sh
output=''
subtitles=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -J) printf '%s\n' '{"title":"Test lecture","id":"test","webpage_url":"https://example.test/video","duration":3,"extractor":"test"}'; exit 0 ;;
    -o) shift; output="$1" ;;
    --skip-download) subtitles=1 ;;
  esac
  shift
done
if [ "$subtitles" -eq 1 ]; then
  printf '1\n00:00:00,000 --> 00:00:02,500\nWelcome to the lecture.\n' > "$output.en.srt"
else
  /bin/cp "$CLI_TEST_VIDEO" "$output"
fi
printf 'raw downloader output\n'
printf 'raw downloader stderr\n' >&2
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    for flag in ["--json", "--quiet"] {
        let output_dir = cli.dir.path().join(flag.trim_start_matches('-'));
        let result = Command::new(env!("CARGO_BIN_EXE_course2md"))
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
