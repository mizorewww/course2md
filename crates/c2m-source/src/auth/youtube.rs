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

    /// The folder yt-dlp searches for this browser's cookies: the data folder, or
    /// for Safari its cookie folder (in Safari's container on current macOS), which
    /// is not the app that marks Safari installed.
    fn cookie_dir(self, home: &Path) -> Option<PathBuf> {
        if self != Self::Safari {
            return self.data_dir(home);
        }
        if !cfg!(target_os = "macos") {
            return None;
        }
        // Safari's container first: where current Safari keeps its cookies. A
        // folder the system will not even look into counts as there (and closed),
        // not as absent: `exists` would read that refusal as a missing folder
        // and fall back to a listable legacy one, so a blocked Safari would pass.
        [
            home.join("Library/Containers/com.apple.Safari/Data/Library/Cookies"),
            home.join("Library/Cookies"),
        ]
        .into_iter()
        .find(|dir| !matches!(dir.try_exists(), Ok(false)))
    }

    /// Whether this app may read the browser's cookie folder now. macOS 27 keeps
    /// the folders of Chrome and other browsers (and Safari's container) to the
    /// browser itself unless the reading app has Full Disk Access; the folder still
    /// exists (so the browser is offered), but yt-dlp cannot search it and reports
    /// no cookie database. One folder listing, no cookie is read: cheap enough to
    /// ask again every few seconds while the person grants access.
    pub fn access(self) -> Access {
        c2m_core::paths::home_dir()
            .and_then(|home| self.cookie_dir(&home))
            .map_or(Access::Absent, |dir| access_to(&dir))
    }

    /// Whether the system refuses to list this browser's cookie folder.
    pub fn blocked(self) -> bool {
        self.access() == Access::Blocked
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

/// What the system answers this app about a browser's cookie folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The folder lists: yt-dlp can search it.
    Open,
    /// The folder exists but the system refuses to list it (on macOS, until the
    /// app has Full Disk Access).
    Blocked,
    /// No such folder, or another failure that access would not change.
    Absent,
}

/// Lists `dir` once. Both refusals (`EPERM`, as macOS privacy answers, and
/// `EACCES`, as file modes do) read as blocked.
pub fn access_to(dir: &Path) -> Access {
    match std::fs::read_dir(dir) {
        Ok(_) => Access::Open,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Access::Blocked,
        Err(_) => Access::Absent,
    }
}

/// Whether listing `dir` is refused, as opposed to the folder being absent.
fn listing_denied(dir: &Path) -> bool {
    access_to(dir) == Access::Blocked
}

fn chromium_profiles(data_dir: &Path) -> Vec<Profile> {
    let names: serde_json::Value = std::fs::read(data_dir.join("Local State"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let name_of = |dir: String| {
        let name = names["profile"]["info_cache"][&dir]["name"]
            .as_str()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&dir)
            .to_owned();
        Profile { dir, name }
    };
    let mut profiles: Vec<_> = match std::fs::read_dir(data_dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|entry| {
                let path = entry.path();
                path.join("Cookies").is_file() || path.join("Network/Cookies").is_file()
            })
            .filter_map(|entry| entry.file_name().into_string().ok())
            .map(name_of)
            .collect(),
        // A folder that cannot be listed may still let its "Local State" be
        // opened by name: it names every profile the browser knows.
        Err(_) => names["profile"]["info_cache"]
            .as_object()
            .into_iter()
            .flat_map(|cache| cache.keys().cloned())
            .map(name_of)
            .collect(),
    };
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

/// A run failed reading the connected browser's cookies because the system keeps
/// its folder from this app (yt-dlp words that as a missing database).
pub fn cookies_blocked(cause: &str) -> bool {
    cookie_read_refused(cause)
        && saved_choice()
            .ok()
            .flatten()
            .is_some_and(|choice| choice.browser.blocked())
}

fn cookie_read_refused(cause: &str) -> bool {
    let lower = cause.to_lowercase();
    (lower.contains("could not find") && lower.contains("cookies database"))
        || lower.contains("operation not permitted")
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
    /// The system kept the browser's data from this app: Full Disk Access, in
    /// System Settings, lets it through ([`FULL_DISK_ACCESS_SETTINGS`]).
    pub blocked: bool,
}

/// Who asks to read the browser, and so who the system lets through: the app,
/// or for the command line the terminal app it runs in (macOS holds the app
/// that started a program responsible for what it reads).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    App,
    Terminal,
}

/// System Settings › Privacy & Security › Full Disk Access (macOS 13 and later).
pub const FULL_DISK_ACCESS_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// What to do when the system keeps `browser`'s data from `reader`.
pub fn blocked_summary(browser: Browser, reader: Reader) -> String {
    let name = browser.name();
    if !cfg!(target_os = "macos") {
        return tr!("系统不允许读取 {} 的 Cookie，请检查权限后重试。", name);
    }
    match reader {
        Reader::App => tr!(
            "macOS 不允许 course2md 读取 {} 的数据。请在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中打开 course2md，然后退出并重新打开 course2md，再试一次。",
            name
        ),
        Reader::Terminal => tr!(
            "macOS 不允许终端读取 {} 的数据。请在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中打开你使用的终端应用（如“终端”），然后退出并重新打开它，再试一次。可运行 open \"{}\" 打开该设置。",
            name,
            FULL_DISK_ACCESS_SETTINGS
        ),
    }
}

/// The folder yt-dlp searches for `choice`'s cookies, when the system refuses to
/// list it: the browser's data folder, or a profile given as a path.
fn blocked_dir(choice: &BrowserChoice, home: &Path) -> Option<PathBuf> {
    let profile_path = choice
        .profile
        .as_deref()
        .map(Path::new)
        .filter(|profile| profile.components().count() > 1);
    let dirs = [
        profile_path.map(Path::to_path_buf),
        choice.browser.cookie_dir(home),
    ];
    dirs.into_iter().flatten().find(|dir| listing_denied(dir))
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
pub fn check_session(program: &Path, choice: &BrowserChoice, reader: Reader) -> Result<Session> {
    ensure_readable(choice, reader)?;
    let home = c2m_core::paths::home_dir();
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
        let denied = home
            .as_deref()
            .and_then(|home| blocked_dir(choice, home))
            .is_some();
        bail!(read_error(choice.browser, &output.stderr, reader, denied));
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

/// A folder the system will not let yt-dlp search reads to it as "no cookie
/// database"; this says what is really wrong without starting it.
pub fn ensure_readable(choice: &BrowserChoice, reader: Reader) -> Result<(), BrowserReadError> {
    match c2m_core::paths::home_dir().and_then(|home| blocked_dir(choice, &home)) {
        Some(dir) => Err(blocked_error(choice.browser, reader, &dir)),
        None => Ok(()),
    }
}

fn blocked_error(browser: Browser, reader: Reader, dir: &Path) -> BrowserReadError {
    BrowserReadError {
        summary: blocked_summary(browser, reader),
        detail: format!("Operation not permitted: {}", dir.display()),
        blocked: cfg!(target_os = "macos"),
    }
}

/// `denied`: the folder yt-dlp searched cannot be listed, which yt-dlp reports
/// as a missing cookie database.
fn read_error(browser: Browser, stderr: &str, reader: Reader, denied: bool) -> BrowserReadError {
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
    let refused = lower.contains("operation not permitted") || lower.contains("permission denied");
    if denied || refused {
        return BrowserReadError {
            summary: blocked_summary(browser, reader),
            detail,
            blocked: cfg!(target_os = "macos"),
        };
    }
    let summary = if lower.contains("could not find") && lower.contains("database") {
        tr!(
            "找不到 {} 的 Cookie。请确认已安装它，并至少打开过一次。",
            name
        )
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
    BrowserReadError {
        summary,
        detail,
        blocked: false,
    }
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
    match check_session(&program, &choice, Reader::Terminal)? {
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
        let missing = "Extracting cookies from chrome\nERROR: could not find chrome cookies database in \"/x\"";
        let error = read_error(Browser::Chrome, missing, Reader::App, false);
        assert!(error.summary.contains("找不到 Google Chrome 的 Cookie"));
        assert!(error.detail.starts_with("could not find chrome"));
        assert!(!error.blocked);
        let locked = read_error(
            Browser::Edge,
            "ERROR: database is locked",
            Reader::App,
            false,
        );
        assert!(locked.summary.contains("退出 Microsoft Edge"));
        // A folder that cannot be listed is what yt-dlp calls a missing database.
        let hidden = read_error(Browser::Chrome, missing, Reader::App, true);
        assert_eq!(
            hidden.summary,
            blocked_summary(Browser::Chrome, Reader::App)
        );
        assert!(hidden.detail.starts_with("could not find chrome"));
        let safari = read_error(
            Browser::Safari,
            "ERROR: [Errno 1] Operation not permitted: '/Users/x/Library/Cookies'",
            Reader::Terminal,
            false,
        );
        assert_eq!(
            safari.summary,
            blocked_summary(Browser::Safari, Reader::Terminal)
        );
        assert_eq!(safari.blocked, hidden.blocked);
        if cfg!(target_os = "macos") {
            assert!(hidden.blocked);
            for (reader, who) in [(Reader::App, "course2md"), (Reader::Terminal, "终端")] {
                let summary = blocked_summary(Browser::Chrome, reader);
                assert!(summary.contains("Google Chrome"), "{summary}");
                assert!(summary.contains("完全磁盘访问权限"), "{summary}");
                assert!(summary.contains(who), "{summary}");
            }
            assert!(
                blocked_summary(Browser::Chrome, Reader::Terminal)
                    .contains(FULL_DISK_ACCESS_SETTINGS)
            );
        } else {
            assert!(!hidden.blocked);
        }
    }

    /// A folder that exists but cannot be listed, as macOS shows another app's
    /// protected data. `None` where permissions do not bind (running as root).
    #[cfg(unix)]
    fn unlistable(dir: &Path, mode: u32) -> Option<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
        std::fs::read_dir(dir).is_err().then_some(())
    }

    #[cfg(unix)]
    fn restore(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_listed_is_blocked_not_missing() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        let choice = BrowserChoice {
            browser: Browser::Firefox,
            profile: Some(profile.display().to_string()),
        };
        assert!(!listing_denied(&profile));
        assert!(!listing_denied(&home.path().join("absent")));
        assert_eq!(blocked_dir(&choice, home.path()), None);
        if unlistable(&profile, 0o000).is_some() {
            assert!(listing_denied(&profile));
            assert_eq!(blocked_dir(&choice, home.path()), Some(profile.clone()));
            let error = blocked_error(Browser::Firefox, Reader::App, &profile);
            assert_eq!(
                error.summary,
                blocked_summary(Browser::Firefox, Reader::App)
            );
            assert!(error.detail.contains("Operation not permitted"));
        }
        restore(&profile);
    }

    /// macOS 27 refuses to list Chrome's folder; Chrome is still offered, and the
    /// check says what to grant instead of running yt-dlp into "could not find".
    #[cfg(target_os = "macos")]
    #[test]
    fn a_protected_chrome_folder_is_installed_but_blocked() {
        let home = tempfile::tempdir().unwrap();
        let data = Browser::Chrome.data_dir(home.path()).unwrap();
        std::fs::create_dir_all(data.join("Default")).unwrap();
        let choice = BrowserChoice {
            browser: Browser::Chrome,
            profile: Some("Default".into()),
        };
        assert_eq!(blocked_dir(&choice, home.path()), None);
        if unlistable(&data, 0o000).is_some() {
            assert!(data.exists());
            assert_eq!(blocked_dir(&choice, home.path()), Some(data.clone()));
        }
        restore(&data);
    }

    /// The probe the desktop repeats while the person grants access: a folder the
    /// system will not list is blocked, a listed one open, and one that is not there
    /// absent; giving access back (here, the folder's mode) opens it again.
    #[cfg(unix)]
    #[test]
    fn access_follows_the_folder_as_it_is_refused_and_granted() {
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join("Chrome");
        assert_eq!(access_to(&data), Access::Absent);
        std::fs::create_dir(&data).unwrap();
        assert_eq!(access_to(&data), Access::Open);
        if unlistable(&data, 0o000).is_some() {
            assert_eq!(access_to(&data), Access::Blocked);
            assert!(listing_denied(&data));
            // Search without read is still a refusal to list.
            unlistable(&data, 0o311);
            assert_eq!(access_to(&data), Access::Blocked);
        }
        restore(&data);
        assert_eq!(access_to(&data), Access::Open);
        // A file where the folder should be is not something access would change.
        let file = home.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert_eq!(access_to(&file), Access::Absent);
    }

    /// Safari's cookies live in its container, not in the app that marks it
    /// installed; without either folder there is nothing to grant.
    #[cfg(target_os = "macos")]
    #[test]
    fn safari_is_probed_at_its_cookie_folder() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(Browser::Safari.cookie_dir(home.path()), None);
        let legacy = home.path().join("Library/Cookies");
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(
            Browser::Safari.cookie_dir(home.path()),
            Some(legacy.clone())
        );
        let container = home
            .path()
            .join("Library/Containers/com.apple.Safari/Data/Library/Cookies");
        std::fs::create_dir_all(&container).unwrap();
        assert_eq!(
            Browser::Safari.cookie_dir(home.path()),
            Some(container.clone())
        );
        let choice = BrowserChoice {
            browser: Browser::Safari,
            profile: None,
        };
        assert_eq!(blocked_dir(&choice, home.path()), None);
        if unlistable(&container, 0o000).is_some() {
            assert_eq!(blocked_dir(&choice, home.path()), Some(container.clone()));
        }
        restore(&container);
        // Safari's whole container closed, as macOS keeps it, while the legacy
        // folder lists: Safari is blocked, not read from the legacy folder.
        let safari = home.path().join("Library/Containers/com.apple.Safari");
        if unlistable(&safari, 0o000).is_some() {
            assert_eq!(
                Browser::Safari.cookie_dir(home.path()),
                Some(container.clone())
            );
            assert_eq!(access_to(&container), Access::Blocked);
            assert_eq!(blocked_dir(&choice, home.path()), Some(container.clone()));
        }
        restore(&safari);
        // No container at all: the legacy folder is the one.
        std::fs::remove_dir_all(&safari).unwrap();
        assert_eq!(Browser::Safari.cookie_dir(home.path()), Some(legacy));
    }

    /// When the folder cannot be listed but "Local State" opens by name (execute
    /// without read), the profiles still come from it rather than vanishing.
    #[cfg(unix)]
    #[test]
    fn profiles_fall_back_to_local_state_when_the_folder_cannot_be_listed() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Chrome");
        std::fs::create_dir_all(data.join("Default/Network")).unwrap();
        std::fs::write(data.join("Default/Network/Cookies"), "").unwrap();
        std::fs::write(
            data.join("Local State"),
            r#"{"profile":{"info_cache":{"Default":{"name":"Personal"},"Profile 3":{"name":"Work"}}}}"#,
        )
        .unwrap();
        if unlistable(&data, 0o311).is_some() {
            assert_eq!(
                chromium_profiles(&data),
                [
                    Profile {
                        dir: "Default".into(),
                        name: "Personal".into()
                    },
                    Profile {
                        dir: "Profile 3".into(),
                        name: "Work".into()
                    }
                ]
            );
            // Nothing opens at all: no profiles, and no error either.
            unlistable(&data, 0o000);
            assert_eq!(chromium_profiles(&data), []);
        }
        restore(&data);
    }

    #[test]
    fn cookie_read_refusals_are_recognised() {
        assert!(cookie_read_refused(
            "could not find chrome cookies database in \"/Users/x/Library/Application Support/Google/Chrome\""
        ));
        assert!(cookie_read_refused(
            "[Errno 1] Operation not permitted: '/Users/x/Library/Application Support/Microsoft Edge/Default/Cookies'"
        ));
        assert!(!cookie_read_refused("[youtube] abc: Video unavailable"));
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
            check_session(&program, &choice, Reader::App).unwrap(),
            Session::SignedOut
        );
        insert(
            "INSERT INTO moz_cookies (name,value,host,path,expiry,isSecure,isHttpOnly) VALUES ('SAPISID','fake','.youtube.com','/',2000000000,1,0);",
        );
        assert_eq!(
            check_session(&program, &choice, Reader::App).unwrap(),
            Session::SignedIn
        );
        let missing = BrowserChoice {
            browser: Browser::Firefox,
            profile: Some(profile.path().join("absent").display().to_string()),
        };
        let error = check_session(&program, &missing, Reader::App).unwrap_err();
        let error = error.downcast_ref::<BrowserReadError>().unwrap();
        assert!(error.summary.contains("Firefox"), "{error}");
    }
}
