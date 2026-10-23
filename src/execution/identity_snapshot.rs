//! Snapshots of task identity bindings. A saved task compares its stored
//! identity with a fresh one, and any difference rejects the work directory as
//! "Task inputs changed": these bytes must stay identical across refactors.
//! The fixtures were produced by the code that existing users ran.

use crate::{config, execution, settings::ConfigFile};
use std::path::{Path, PathBuf};

fn file() -> ConfigFile {
    let mut file = ConfigFile::default();
    file.defaults.out = Some("/library".into());
    file.defaults.model_dir = Some("/models".into());
    file.defaults.provider = Some(config::AsrProvider::Api);
    file.llm.api_key = "secret-llm".into();
    file.llm.enabled = true;
    file.asr_api.api_key = "secret-asr".into();
    file.asr_api.model = "asr-model".into();
    file
}

fn request(source: &str, source_id: &str, config: ConfigFile) -> execution::Request {
    execution::Request {
        schema: execution::REQUEST_SCHEMA,
        operation: execution::Operation::Generate,
        task_id: "task-1".into(),
        course_id: "course-1".into(),
        version_id: "version-1".into(),
        source: source.into(),
        source_id: source_id.into(),
        title: "Lecture".into(),
        author: "Teacher".into(),
        duration: 61.5,
        subtitle: None,
        subtitle_events: None,
        config,
        allow_unauthenticated_asr: false,
        work_dir: "/library/bilibili/course-1/.work/task-1".into(),
        course_dir: "/library/bilibili/course-1".into(),
        control_path: None,
        service_versions: [("llm".to_string(), "service-v1".to_string())].into(),
    }
}

/// (fixture name, binding) for each entry point that binds a work directory.
fn bindings() -> Vec<(&'static str, serde_json::Value)> {
    let online = request(
        "https://www.bilibili.com/video/BV1xx",
        "bilibili:BV1xx",
        file(),
    );
    let mut local_file = file();
    local_file.defaults.provider = Some(config::AsrProvider::Cpu);
    local_file.defaults.resume = Some(false);
    let local = request("/nonexistent/lecture.mp4", "local:sha256:0123", local_file);
    let desktop = |request: &execution::Request| {
        let cfg = request.resolve().unwrap();
        request.binding(&cfg).unwrap()
    };
    let cli = |opts: crate::cli::RunOpts| {
        let cfg = crate::options::resolve(
            "https://www.bilibili.com/video/BV1xx".into(),
            &opts,
            &file(),
        )
        .unwrap();
        crate::pipeline::cli_binding("bilibili:BV1xx", &cfg, "Lecture")
    };
    vec![
        ("desktop-online", desktop(&online)),
        ("desktop-local", desktop(&local)),
        ("cli-default", cli(Default::default())),
        (
            "cli-no-resume",
            cli(crate::cli::RunOpts {
                out: Some("/elsewhere".into()),
                no_resume: true,
                asr_api_key: Some("secret-cli".into()),
                ..Default::default()
            }),
        ),
    ]
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/identity")
        .join(format!("{name}.json"))
}

// The snapshots hold the absolute Unix paths a saved task records; Windows
// builds the same JSON from Windows paths, so it is checked on Unix only.
#[cfg(unix)]
#[test]
fn task_identity_bindings_match_saved_snapshots() {
    for (name, binding) in bindings() {
        let text = String::from_utf8(serde_json::to_vec_pretty(&binding).unwrap()).unwrap();
        assert!(!text.contains("secret"), "{name} carries a credential");
        if std::env::var_os("COURSE2MD_WRITE_IDENTITY_FIXTURES").is_some() {
            std::fs::write(fixture(name), &text).unwrap();
        }
        assert_eq!(
            text,
            std::fs::read_to_string(fixture(name)).unwrap(),
            "{name}"
        );
    }
}
