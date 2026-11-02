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
    for platform in ["bilibili", "youtube", "ollama", "codex"] {
        assert!(
            Cli::try_parse_from(["course2md", "--login", platform])
                .unwrap()
                .login
                .is_some(),
            "rejected --login {platform}"
        );
        assert!(
            Cli::try_parse_from(["course2md", "--logout", platform])
                .unwrap()
                .logout
                .is_some(),
            "rejected --logout {platform}"
        );
    }
    for method in ["qr", "password", "sms"] {
        let cli = Cli::try_parse_from(["course2md", "--login", "bilibili", "--method", method]);
        assert!(cli.unwrap().method.is_some(), "rejected --method {method}");
    }
    let youtube = Cli::try_parse_from([
        "course2md",
        "--login",
        "youtube",
        "--browser",
        "chrome:Profile 1",
    ])
    .unwrap();
    assert_eq!(youtube.browser.as_deref(), Some("chrome:Profile 1"));
    for args in [
        vec!["course2md", "--method", "sms"],
        vec!["course2md", "--browser", "chrome"],
        vec![
            "course2md",
            "--login",
            "bilibili",
            "--method",
            "fingerprint",
        ],
        vec!["course2md", "--login", "bilibili", "--logout", "bilibili"],
        vec!["course2md", "--login", "bilibili", "--json"],
        vec!["course2md", "--logout", "bilibili", "https://b23.tv/test"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "accepted {args:?}");
    }
}

/// The suite's one check that error output follows the language.
#[test]
fn invalid_source_does_not_start_login() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_course2md"))
        .env("COURSE2MD_LANG", "zh-Hans")
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
        .env("COURSE2MD_LANG", "zh-Hans")
        .args(["config", "init"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let message = String::from_utf8_lossy(&out.stdout);
    assert!(message.contains("course2md --login bilibili"));
    assert!(dir.path().join("course2md/config.toml").is_file());
    assert!(!dir.path().join("course2md/auth").exists());
}

/// A method or browser meant for another site stops before any request.
#[test]
fn login_options_belong_to_their_site() {
    for (args, expected) in [
        (["--login", "bilibili", "--browser", "chrome"], "--browser"),
        (["--login", "youtube", "--method", "sms"], "--method"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_course2md"))
            .env("XDG_CONFIG_HOME", dir.path())
            .env("COURSE2MD_LANG", "en")
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(expected), "{stderr}");
        assert!(!dir.path().join("course2md/auth").exists());
    }
}

/// A saved login reaches yt-dlp: each run gets `--cookies` with a private copy of
/// the saved Netscape file, which yt-dlp may rewrite without touching the saved
/// login, and a refusal shows yt-dlp's own reason first. YouTube runs read the
/// chosen browser instead. A fake yt-dlp records what it was given.
#[cfg(unix)]
#[test]
fn a_saved_login_reaches_yt_dlp() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("config/course2md/auth");
    std::fs::create_dir_all(&auth).unwrap();
    let saved = "# Netscape HTTP Cookie File\n\
        .bilibili.com\tTRUE\t/\tTRUE\t0\tDedeUserID\t42\n\
        .bilibili.com\tTRUE\t/\tTRUE\t0\tSESSDATA\tfake%2C1%2Cs\n\
        .bilibili.com\tTRUE\t/\tTRUE\t0\tbili_jct\tfakejct\n";
    std::fs::write(auth.join("bilibili.cookies.txt"), saved).unwrap();
    std::fs::write(auth.join("youtube.json"), r#"{"browser":"firefox"}"#).unwrap();
    let log = dir.path().join("log");
    let fake = dir.path().join("yt-dlp");
    std::fs::write(
        &fake,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo 2026.08.19; exit 0; fi
printf '%s\n' "$@" > "{log}.args"
prev=""
for arg in "$@"; do
  if [ "$prev" = "--cookies" ]; then
    printf '%s\n' "$arg" > "{log}.path"
    cat "$arg" > "{log}.cookies"
    echo "rewritten by yt-dlp" >> "$arg"
  fi
  prev="$arg"
done
echo "ERROR: [site] fake: Requested format is not available" >&2
exit 1
"#,
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |url: &str| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_course2md"))
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .env("QWEN3_CACHE_DIR", dir.path().join("qwen3"))
            .env("COURSE2MD_LANG", "en")
            .env("COURSE2MD_YTDLP", &fake)
            .current_dir(dir.path())
            .arg(url)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let args = std::fs::read_to_string(log.with_extension("args")).unwrap();
        (String::from_utf8_lossy(&out.stderr).into_owned(), args)
    };

    let (stderr, args) = run("https://www.bilibili.com/video/BV1fake");
    let args: Vec<_> = args.lines().collect();
    assert_eq!(args.first(), Some(&"--cookies"), "{args:?}");
    assert_eq!(args.last(), Some(&"https://www.bilibili.com/video/BV1fake"));
    assert_eq!(
        std::fs::read_to_string(log.with_extension("cookies")).unwrap(),
        saved
    );
    let snapshot = std::fs::read_to_string(log.with_extension("path")).unwrap();
    assert!(!std::path::Path::new(snapshot.trim()).exists());
    assert_eq!(
        std::fs::read_to_string(auth.join("bilibili.cookies.txt")).unwrap(),
        saved
    );
    assert!(
        stderr.contains("yt-dlp: [site] fake: Requested format is not available"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Hint: if the video needs a login"),
        "{stderr}"
    );

    let _ = std::fs::remove_file(log.with_extension("cookies"));
    let (_, args) = run("https://www.youtube.com/watch?v=fake");
    let args: Vec<_> = args.lines().collect();
    assert_eq!(args[..2], ["--cookies-from-browser", "firefox"], "{args:?}");
    assert!(!args.contains(&"--cookies"));
    assert!(!log.with_extension("cookies").exists());
}
