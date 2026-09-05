use clap::Parser;
use course2md::cli::Cli;

#[test]
fn login_is_standalone_or_precedes_video_processing() {
    let login = Cli::try_parse_from(["course2md", "--login", "bilibili"]).unwrap();
    assert!(login.login.is_some());
    assert!(login.source.is_none());
    let login = Cli::try_parse_from([
        "course2md",
        "--login",
        "bilibili",
        "https://www.bilibili.com/video/BV1",
    ])
    .unwrap();
    assert!(login.source.is_some());
    assert!(
        Cli::try_parse_from(["course2md", "--logout", "bilibili"])
            .unwrap()
            .logout
            .is_some()
    );
    for args in [
        vec!["course2md", "--login", "youtube"],
        vec!["course2md", "--login", "bilibili", "--logout", "bilibili"],
        vec!["course2md", "--login", "bilibili", "--json"],
        vec!["course2md", "--logout", "bilibili", "https://b23.tv/test"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "accepted {args:?}");
    }
}

#[test]
fn invalid_source_does_not_start_login() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_course2md"))
        .args(["--login", "bilibili", "doctor"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("只能跟视频链接"));
    assert!(out.stdout.is_empty());
}

#[test]
fn initial_config_recommends_bilibili_login_without_creating_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_course2md"))
        .env("XDG_CONFIG_HOME", dir.path())
        .args(["config", "init"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let message = String::from_utf8_lossy(&out.stdout);
    assert!(message.contains("course2md --login bilibili"));
    assert!(message.contains("账号权限范围内更高清晰度"));
    assert!(dir.path().join("course2md/config.toml").is_file());
    assert!(!dir.path().join("course2md/auth").exists());
}
