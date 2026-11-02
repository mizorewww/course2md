//! YouTube: the person's own browser session, read by yt-dlp.
//!
//! Google does not let a program outside a browser sign in with a password, and
//! YouTube no longer accepts yt-dlp's OAuth sign-in. What yt-dlp supports, and
//! recommends for "Sign in to confirm you're not a bot", is the session cookies
//! of a browser where the person is already signed in to YouTube
//! (`--cookies-from-browser`). This app saves only which browser (and profile)
//! to use. yt-dlp reads the browser's cookies afresh on every run, so a session
//! the browser rotates keeps working, and nothing is copied into the app.
use crate::tr;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// The browsers yt-dlp can read, by its names for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Browser {
    Chrome,
    Edge,
    Firefox,
    Safari,
    Brave,
    Chromium,
    Opera,
    Vivaldi,
    Whale,
}

impl Browser {
    pub const ALL: [Self; 9] = [
        Self::Chrome,
        Self::Edge,
        Self::Firefox,
        Self::Safari,
        Self::Brave,
        Self::Chromium,
        Self::Opera,
        Self::Vivaldi,
        Self::Whale,
    ];

    /// yt-dlp's name, as `--cookies-from-browser` takes it.
    pub fn id(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Firefox => "firefox",
            Self::Safari => "safari",
            Self::Brave => "brave",
            Self::Chromium => "chromium",
            Self::Opera => "opera",
            Self::Vivaldi => "vivaldi",
            Self::Whale => "whale",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|browser| browser.id().eq_ignore_ascii_case(id.trim()))
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Chrome => "Google Chrome",
            Self::Edge => "Microsoft Edge",
            Self::Firefox => "Firefox",
            Self::Safari => "Safari",
            Self::Brave => "Brave",
            Self::Chromium => "Chromium",
            Self::Opera => "Opera",
            Self::Vivaldi => "Vivaldi",
            Self::Whale => "Naver Whale",
        }
    }

    fn chromium(self) -> bool {
        !matches!(self, Self::Firefox | Self::Safari)
    }

    /// Where the browser keeps its profiles, as yt-dlp looks for them.
    fn data_dir(self, home: &Path) -> Option<PathBuf> {
        if cfg!(target_os = "macos") {
            let support = home.join("Library/Application Support");
            Some(match self {
                Self::Chrome => support.join("Google/Chrome"),
                Self::Edge => support.join("Microsoft Edge"),
                Self::Firefox => support.join("Firefox/Profiles"),
                Self::Safari => return Some(PathBuf::from("/Applications/Safari.app")),
                Self::Brave => support.join("BraveSoftware/Brave-Browser"),
                Self::Chromium => support.join("Chromium"),
                Self::Opera => support.join("com.operasoftware.Opera"),
                Self::Vivaldi => support.join("Vivaldi"),
                Self::Whale => support.join("Naver/Whale"),
            })
        } else if cfg!(windows) {
            let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
            let roaming = std::env::var_os("APPDATA").map(PathBuf::from)?;
            Some(match self {
                Self::Chrome => local.join(r"Google\Chrome\User Data"),
                Self::Edge => local.join(r"Microsoft\Edge\User Data"),
                Self::Firefox => roaming.join(r"Mozilla\Firefox\Profiles"),
                Self::Safari => return None,
                Self::Brave => local.join(r"BraveSoftware\Brave-Browser\User Data"),
                Self::Chromium => local.join(r"Chromium\User Data"),
                Self::Opera => roaming.join(r"Opera Software\Opera Stable"),
                Self::Vivaldi => local.join(r"Vivaldi\User Data"),
                Self::Whale => local.join(r"Naver\Naver Whale\User Data"),
            })
        } else {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            Some(match self {
                Self::Chrome => config.join("google-chrome"),
                Self::Edge => config.join("microsoft-edge"),
                Self::Firefox => [
                    home.join(".mozilla/firefox"),
                    home.join("snap/firefox/common/.mozilla/firefox"),
                    home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
                ]
                .into_iter()
                .find(|dir| dir.is_dir())
                .unwrap_or_else(|| home.join(".mozilla/firefox")),
                Self::Safari => return None,
                Self::Brave => config.join("BraveSoftware/Brave-Browser"),
                Self::Chromium => config.join("chromium"),
                Self::Opera => config.join("opera"),
                Self::Vivaldi => config.join("vivaldi"),
                Self::Whale => config.join("naver-whale"),
            })
        }
    }

    /// The browsers installed for this user, in [`Browser::ALL`] order.
    pub fn installed() -> Vec<Self> {
        let Some(home) = c2m_core::paths::home_dir() else {
            return Vec::new();
        };
        Self::ALL
            .into_iter()
            .filter(|browser| browser.data_dir(&home).is_some_and(|dir| dir.exists()))
            .collect()
    }

    /// A Chromium browser's profiles by the names the person gave them. Others
    /// (and a browser with one profile) need no choice: yt-dlp finds the profile.
    pub fn profiles(self) -> Vec<Profile> {
        let Some(dir) = c2m_core::paths::home_dir().and_then(|home| self.data_dir(&home)) else {
            return Vec::new();
        };
        if !self.chromium() {
            return Vec::new();
        }
        chromium_profiles(&dir)
    }
}

/// One browser profile: `dir` is what yt-dlp takes, `name` what the person sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub dir: String,
    pub name: String,
}

fn chromium_profiles(data_dir: &Path) -> Vec<Profile> {
    let names: serde_json::Value = std::fs::read(data_dir.join("Local State"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut profiles: Vec<_> = std::fs::read_dir(data_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let path = entry.path();
            path.join("Cookies").is_file() || path.join("Network/Cookies").is_file()
        })
        .filter_map(|entry| {
            let dir = entry.file_name().into_string().ok()?;
            let name = names["profile"]["info_cache"][&dir]["name"]
                .as_str()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(&dir)
                .to_owned();
            Some(Profile { dir, name })
        })
        .collect();
    // The default profile first, then as the browser lists them.
    profiles.sort_by_key(|profile| (profile.dir != "Default", profile.dir.clone()));
    profiles
}

/// Which browser session YouTube runs use. Holds no cookie.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserChoice {
    pub browser: Browser,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl BrowserChoice {
    /// `--cookies-from-browser` value: `chrome` or `chrome:Profile 1`.
    pub fn spec(&self) -> String {
        match self
            .profile
            .as_deref()
            .filter(|profile| !profile.trim().is_empty())
        {
            Some(profile) => format!("{}:{profile}", self.browser.id()),
            None => self.browser.id().to_owned(),
        }
    }

    /// Parses `chrome`, `chrome:Profile 1` or `firefox:/path/to/profile`.
    pub fn parse(spec: &str) -> Option<Self> {
        let (browser, profile) = match spec.split_once(':') {
            Some((browser, profile)) => (browser, Some(profile.trim().to_owned())),
            None => (spec, None),
        };
        Some(Self {
            browser: Browser::from_id(browser)?,
            profile: profile.filter(|profile| !profile.is_empty()),
        })
    }

    /// How the choice reads to the person: `Google Chrome` or `Google Chrome · Work`.
    pub fn label(&self) -> String {
        let Some(profile) = self.profile.as_deref() else {
            return self.browser.name().to_owned();
        };
        let name = self
            .browser
            .profiles()
            .into_iter()
            .find(|candidate| candidate.dir == profile)
            .map_or_else(|| profile.to_owned(), |candidate| candidate.name);
        format!("{} · {name}", self.browser.name())
    }
}

pub fn choice_path() -> PathBuf {
    crate::config::config_dir().join("auth/youtube.json")
}

/// The saved choice. A missing file is no choice; an unreadable one is an error.
pub fn saved_choice() -> Result<Option<BrowserChoice>> {
    read_choice(&choice_path())
}

fn read_choice(path: &Path) -> Result<Option<BrowserChoice>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .context(tr!("YouTube 登录设置已损坏，请重新连接浏览器")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context(tr!("无法读取 YouTube 登录设置")),
    }
}

pub fn save_choice(choice: &BrowserChoice) -> Result<()> {
    crate::fsio::atomic_write(&choice_path(), &serde_json::to_vec_pretty(choice)?)
        .context(tr!("保存 YouTube 登录设置失败"))
}

/// Forgets the choice; the browser's own session is not touched.
pub fn clear_youtube_login() -> Result<bool> {
    match std::fs::remove_file(choice_path()) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context(tr!("清除 YouTube 登录设置失败")),
    }
}

/// A YouTube run reads the chosen browser's session, and has a JavaScript
/// runtime for YouTube's player challenges when one is installed.
pub(crate) fn configure(command: &mut std::process::Command) -> Result<()> {
    if let Some(choice) = saved_choice()? {
        command.arg("--cookies-from-browser").arg(choice.spec());
    }
    if let Some(runtime) = js_runtime(Path::new(command.get_program())) {
        command.arg("--js-runtimes").arg(runtime);
    }
    Ok(())
}

/// yt-dlp solves YouTube's player challenges in JavaScript and enables only Deno
/// by default; without a runtime some formats go missing. When Deno is absent
/// but Node is installed, enable Node, if this yt-dlp knows the option.
fn js_runtime(program: &Path) -> Option<String> {
    if crate::tools::locate("deno").is_some() {
        return None;
    }
    let node = crate::tools::locate("node")?;
    supports_js_runtimes(program).then(|| format!("node:{}", node.path.display()))
}

/// `--js-runtimes` arrived in yt-dlp 2025.11.12. Asked once per program.
fn supports_js_runtimes(program: &Path) -> bool {
    static SEEN: std::sync::Mutex<Vec<(PathBuf, bool)>> = std::sync::Mutex::new(Vec::new());
    if let Some((_, known)) = SEEN
        .lock()
        .ok()
        .and_then(|seen| seen.iter().find(|(path, _)| path == program).cloned())
    {
        return known;
    }
    let mut command = std::process::Command::new(program);
    command.arg("--version");
    let supported = crate::runtime::run_ok(&mut command, Duration::from_secs(30))
        .is_some_and(|output| version_has_js_runtimes(output.stdout.trim()));
    if let Ok(mut seen) = SEEN.lock() {
        seen.push((program.to_path_buf(), supported));
    }
    supported
}

fn version_has_js_runtimes(version: &str) -> bool {
    let parts: Vec<u32> = version
        .split(['.', '-'])
        .take(3)
        .filter_map(|part| part.parse().ok())
        .collect();
    parts.len() == 3 && (parts[0], parts[1], parts[2]) >= (2025, 11, 12)
}

/// A failure that signing in would answer: the bot check, age or membership gates.
pub fn wants_sign_in(cause: &str) -> bool {
    let lower = cause.to_lowercase();
    [
        "sign in to confirm",
        "not a bot",
        "confirm your age",
        "members-only",
        "join this channel",
        "private video",
        "login required",
        "use --cookies",
        "cookies-from-browser",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Whether the chosen browser holds a YouTube session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    SignedIn,
    SignedOut,
}

/// Why the browser's session could not be read, in words the person can act on.
#[derive(Debug, thiserror::Error)]
#[error("{summary}\n{detail}")]
pub struct BrowserReadError {
    pub summary: String,
    /// yt-dlp's own words.
    pub detail: String,
}

/// Where a check's private directories live, so one a crash left behind is
/// found and removed ([`remove_stale_checks`]).
fn checks_dir() -> PathBuf {
    crate::config::cache_dir().join("browser-check")
}

/// A check lasts at most [`CHECK_TIMEOUT`]; anything older was left by a crash
/// or a force quit and still holds a browser's cookies.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// Removes check directories older than any running check. Called before each
/// check and when the app starts.
pub fn remove_stale_checks() {
    remove_stale_in(&checks_dir(), CHECK_TIMEOUT + Duration::from_secs(60));
}

fn remove_stale_in(dir: &Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            // A future time (the clock was set back) is not proof of age, and an
            // unreadable one could be a running check's: both are kept.
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed >= age);
        if stale && entry.file_name().to_string_lossy().starts_with("check-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Has yt-dlp read the chosen browser's cookies, the way every YouTube run will,
/// and reports whether they include a signed-in YouTube session. No network
/// request is made. yt-dlp can only hand over what it read as a cookie file, so
/// it writes the browser's cookies into a private (0700) directory in the app's
/// cache; only cookie names are looked at, the directory is removed before this
/// returns, and one a crash left behind is removed by the next check or start.
/// Blocking: on macOS the system may ask the person to allow access to the
/// browser's keychain item.
pub fn check_session(program: &Path, choice: &BrowserChoice) -> Result<Session> {
    remove_stale_checks();
    let parent = checks_dir();
    std::fs::create_dir_all(&parent)?;
    let dir = tempfile::Builder::new()
        .prefix("check-")
        .tempdir_in(&parent)?;
    let jar = dir.path().join("cookies.txt");
    let mut command = std::process::Command::new(program);
    command
        .args(["--ignore-config", "--cookies-from-browser"])
        .arg(choice.spec())
        .arg("--cookies")
        .arg(&jar);
    // With no URL yt-dlp loads the browser's cookies, saves them and stops
    // before any request. Two minutes leave time to answer a keychain prompt.
    let output = crate::runtime::run_bounded("yt-dlp", &mut command, CHECK_TIMEOUT)?;
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(&jar).unwrap_or_default());
    drop(dir);
    if !text.starts_with("# Netscape HTTP Cookie File") {
        bail!(read_error(choice.browser, &output.stderr));
    }
    Ok(if signed_in(&text) {
        Session::SignedIn
    } else {
        Session::SignedOut
    })
}

/// yt-dlp's own test: a YouTube SAPISID cookie (any of its three forms).
fn signed_in(jar: &str) -> bool {
    jar.lines()
        .filter(|line| !line.starts_with('#') || line.starts_with("#HttpOnly_"))
        .filter_map(|line| {
            let columns: Vec<_> = line.split('\t').collect();
            (columns.len() == 7).then(|| (columns[0], columns[5]))
        })
        .any(|(domain, name)| {
            let domain = domain
                .trim_start_matches("#HttpOnly_")
                .trim_start_matches('.');
            (domain == "youtube.com" || domain.ends_with(".youtube.com"))
                && matches!(name, "SAPISID" | "__Secure-1PAPISID" | "__Secure-3PAPISID")
        })
}

fn read_error(browser: Browser, stderr: &str) -> BrowserReadError {
    let detail = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with("ERROR:"))
        .or_else(|| {
            stderr
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
        })
        .unwrap_or_default()
        .trim_start_matches("ERROR:")
        .trim()
        .to_owned();
    let lower = detail.to_lowercase();
    let name = browser.name();
    let summary = if lower.contains("could not find") && lower.contains("database") {
        tr!(
            "找不到 {} 的 Cookie。请确认已安装它，并至少打开过一次。",
            name
        )
    } else if lower.contains("operation not permitted") || lower.contains("permission denied") {
        if browser == Browser::Safari {
            tr!("macOS 不允许读取 Safari 的 Cookie。请在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许 course2md，或改用其他浏览器。").to_owned()
        } else {
            tr!("系统不允许读取 {} 的 Cookie，请检查权限后重试。", name)
        }
    } else if lower.contains("database is locked") || lower.contains("could not copy") {
        tr!("{} 正在使用 Cookie 数据库。请退出 {} 后重试。", name, name)
    } else if lower.contains("decrypt") || lower.contains("keyring") || lower.contains("key") {
        if cfg!(windows) && browser.chromium() {
            tr!(
                "无法解密 {} 的 Cookie：Windows 上的新版 Chromium 浏览器不允许其他程序读取。请改用 Firefox。",
                name
            )
        } else {
            tr!(
                "无法解密 {} 的 Cookie。若系统询问是否允许访问钥匙串，请选择允许。",
                name
            )
        }
    } else {
        tr!("无法读取 {} 的 Cookie。", name)
    };
    BrowserReadError { summary, detail }
}

/// CLI: `course2md --login youtube [--browser chrome[:profile]]`; the CLI picks
/// the browser.
pub fn login_youtube(choice: BrowserChoice) -> Result<()> {
    use crate::println_tr;
    println_tr!(
        "将由 yt-dlp 读取 {} 的 Cookie，仅用于访问 YouTube；course2md 不保存这些 Cookie（检查时会短暂写入私有临时文件，结束即删除）。使用账号下载存在被 YouTube 限制或封禁的风险，请适度使用。 / yt-dlp will read {}'s cookies and use them only for YouTube; course2md does not keep them (this check briefly writes them to a private temporary file, deleted when it ends). Downloading with an account risks YouTube restricting or banning it; use it sparingly.",
        choice.label(),
        choice.label()
    );
    let program = crate::ytdlp::program();
    match check_session(&program, &choice)? {
        Session::SignedIn => {
            save_choice(&choice)?;
            println_tr!(
                "已连接 {} 中的 YouTube 登录。YouTube 视频将使用此登录。 / Connected to the YouTube session in {}. YouTube videos will use it.",
                choice.label(),
                choice.label()
            );
            Ok(())
        }
        Session::SignedOut => bail!(tr!(
            "{} 中没有登录 YouTube。请先在浏览器中登录 YouTube，再重新运行 / {} is not signed in to YouTube. Sign in there first, then run this again",
            choice.label(),
            choice.label()
        )),
    }
}

pub fn logout_youtube() -> Result<()> {
    use crate::println_tr;
    if clear_youtube_login()? {
        println_tr!(
            "已断开 YouTube 登录；浏览器中的登录不受影响。/ YouTube session disconnected; your browser stays signed in."
        );
    } else {
        println_tr!("尚未连接 YouTube 登录。/ No YouTube session connected.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_left_by_a_crash_is_removed_and_a_running_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("check-old");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("cookies.txt"), "secret").unwrap();
        let other = dir.path().join("unrelated");
        std::fs::create_dir(&other).unwrap();
        remove_stale_in(dir.path(), Duration::from_secs(3600));
        assert!(old.exists());
        remove_stale_in(dir.path(), Duration::ZERO);
        assert!(!old.exists());
        // A folder dated in the future (the clock went back) is kept.
        let ahead = dir.path().join("check-ahead");
        std::fs::create_dir(&ahead).unwrap();
        // Windows opens a folder only with backup semantics, and dating it needs
        // the write-attributes right.
        let mut options = std::fs::OpenOptions::new();
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::custom_flags(
            std::os::windows::fs::OpenOptionsExt::access_mode(&mut options, 0x0100),
            0x0200_0000,
        );
        #[cfg(not(windows))]
        options.read(true);
        options
            .open(&ahead)
            .unwrap()
            .set_modified(std::time::SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        remove_stale_in(dir.path(), Duration::ZERO);
        assert!(ahead.exists());
        assert!(other.exists());
    }

    #[test]
    fn choices_round_trip_as_yt_dlp_specs_and_hold_no_cookie() {
        let choice = BrowserChoice::parse("Chrome:Profile 1").unwrap();
        assert_eq!(choice.browser, Browser::Chrome);
        assert_eq!(choice.spec(), "chrome:Profile 1");
        assert_eq!(BrowserChoice::parse("firefox").unwrap().spec(), "firefox");
        assert!(BrowserChoice::parse("netscape").is_none());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("youtube.json");
        crate::fsio::atomic_write(&path, &serde_json::to_vec(&choice).unwrap()).unwrap();
        assert_eq!(read_choice(&path).unwrap(), Some(choice));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"browser":"chrome","profile":"Profile 1"}"#
        );
        assert_eq!(read_choice(&dir.path().join("missing")).unwrap(), None);
        std::fs::write(&path, "{").unwrap();
        assert!(read_choice(&path).is_err());
    }

    #[test]
    fn a_signed_in_session_is_a_youtube_sapisid_and_nothing_else() {
        let jar = "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tPREF\tf6=1\n";
        assert!(!signed_in(jar));
        assert!(!signed_in(
            "# Netscape HTTP Cookie File\n.google.com\tTRUE\t/\tTRUE\t0\tSAPISID\tx\n"
        ));
        assert!(!signed_in(
            "# Netscape HTTP Cookie File\n.notyoutube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tx\n"
        ));
        for name in ["SAPISID", "__Secure-3PAPISID"] {
            assert!(signed_in(&format!(
                "{jar}.youtube.com\tTRUE\t/\tTRUE\t0\t{name}\tx\n"
            )));
        }
        assert!(signed_in(&format!(
            "{jar}#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\t__Secure-1PAPISID\tx\n"
        )));
    }

    #[test]
    fn read_failures_say_what_to_do() {
        let error = read_error(
            Browser::Chrome,
            "Extracting cookies from chrome\nERROR: could not find chrome cookies database in \"/x\"",
        );
        assert!(error.summary.contains("找不到 Google Chrome 的 Cookie"));
        assert!(error.detail.starts_with("could not find chrome"));
        let locked = read_error(Browser::Edge, "ERROR: database is locked");
        assert!(locked.summary.contains("退出 Microsoft Edge"));
        let safari = read_error(
            Browser::Safari,
            "ERROR: [Errno 1] Operation not permitted: '/Users/x/Library/Cookies'",
        );
        assert!(safari.summary.contains("完全磁盘访问权限"));
    }

    #[test]
    fn sign_in_prompts_are_recognised_and_other_failures_are_not() {
        assert!(wants_sign_in(
            "[youtube] abc: Sign in to confirm you’re not a bot. This helps protect our community."
        ));
        assert!(wants_sign_in("[youtube] abc: Sign in to confirm your age"));
        assert!(!wants_sign_in("[youtube] abc: Video unavailable"));
        assert!(!wants_sign_in("HTTP Error 404: Not Found"));
    }

    #[test]
    fn js_runtime_option_follows_the_yt_dlp_version() {
        assert!(version_has_js_runtimes("2026.10.26"));
        assert!(version_has_js_runtimes("2025.11.12"));
        assert!(!version_has_js_runtimes("2025.10.22"));
        assert!(!version_has_js_runtimes("2024.12.06.232811"));
        assert!(!version_has_js_runtimes("garbage"));
    }

    #[test]
    fn chromium_profiles_use_the_names_the_person_gave_them() {
        let dir = tempfile::tempdir().unwrap();
        for profile in ["Default", "Profile 2", "System Profile"] {
            std::fs::create_dir_all(dir.path().join(profile).join("Network")).unwrap();
        }
        std::fs::write(dir.path().join("Default/Network/Cookies"), "").unwrap();
        std::fs::write(dir.path().join("Profile 2/Cookies"), "").unwrap();
        std::fs::write(
            dir.path().join("Local State"),
            r#"{"profile":{"info_cache":{"Profile 2":{"name":"Work"}}}}"#,
        )
        .unwrap();
        assert_eq!(
            chromium_profiles(dir.path()),
            [
                Profile {
                    dir: "Default".into(),
                    name: "Default".into()
                },
                Profile {
                    dir: "Profile 2".into(),
                    name: "Work".into()
                }
            ]
        );
    }

    /// A throwaway Firefox profile with fake cookies, read by the real yt-dlp.
    /// Set `COURSE2MD_TEST_YTDLP` to a yt-dlp binary; needs `sqlite3`.
    #[test]
    #[ignore = "requires yt-dlp and sqlite3"]
    fn live_session_check_reads_a_throwaway_firefox_profile() {
        let program = PathBuf::from(
            std::env::var_os("COURSE2MD_TEST_YTDLP").unwrap_or_else(|| "yt-dlp".into()),
        );
        let profile = tempfile::tempdir().unwrap();
        let db = profile.path().join("cookies.sqlite");
        let insert = |cookies: &str| {
            let _ = std::fs::remove_file(&db);
            let status = std::process::Command::new("sqlite3")
                .arg(&db)
                .arg(format!(
                    "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '', name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, lastAccessed INTEGER, creationTime INTEGER, isSecure INTEGER, isHttpOnly INTEGER, inBrowserElement INTEGER DEFAULT 0, sameSite INTEGER DEFAULT 0, rawSameSite INTEGER DEFAULT 0, schemeMap INTEGER DEFAULT 0); {cookies}"
                ))
                .status()
                .unwrap();
            assert!(status.success());
        };
        let choice = BrowserChoice {
            browser: Browser::Firefox,
            profile: Some(profile.path().display().to_string()),
        };
        insert(
            "INSERT INTO moz_cookies (name,value,host,path,expiry,isSecure,isHttpOnly) VALUES ('PREF','x','.youtube.com','/',2000000000,1,0);",
        );
        assert_eq!(
            check_session(&program, &choice).unwrap(),
            Session::SignedOut
        );
        insert(
            "INSERT INTO moz_cookies (name,value,host,path,expiry,isSecure,isHttpOnly) VALUES ('SAPISID','fake','.youtube.com','/',2000000000,1,0);",
        );
        assert_eq!(check_session(&program, &choice).unwrap(), Session::SignedIn);
        let missing = BrowserChoice {
            browser: Browser::Firefox,
            profile: Some(profile.path().join("absent").display().to_string()),
        };
        let error = check_session(&program, &missing).unwrap_err();
        let error = error.downcast_ref::<BrowserReadError>().unwrap();
        assert!(error.summary.contains("Firefox"), "{error}");
    }
}
