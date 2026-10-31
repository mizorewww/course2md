//! Bilibili QR login and private yt-dlp cookie snapshots.
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const PASSPORT: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode";
const NAV: &str = "https://api.bilibili.com/x/web-interface/nav";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
const COOKIE_NAMES: &[&str] = &[
    "SESSDATA",
    "bili_jct",
    "DedeUserID",
    "DedeUserID__ckMd5",
    "buvid3",
    "buvid4",
    "b_nut",
];
type Cookies = BTreeMap<String, String>;
type Response = ureq::http::Response<ureq::Body>;

pub const BILIBILI_SETUP_TIP: &str = "使用 Bilibili 视频时，推荐先运行 course2md --login bilibili 扫码登录，可下载账号权限范围内更高清晰度的视频。 / For Bilibili videos, run course2md --login bilibili first to scan a QR code; downloads can then use higher resolutions available to your account.";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct LoginTip(&'static str);

/// Keep the original failure visible and recognisable: login is a suggested next
/// step, not a diagnosis, so markers in `error` still answer to `downcast_ref`.
pub fn with_bilibili_login_tip(url: &str, error: anyhow::Error) -> anyhow::Error {
    if !is_bilibili_url(url) || error.downcast_ref::<LoginTip>().is_some() {
        return error;
    }
    let hint = if crate::progress::is_json() {
        "提示：请在设置的账号中重新登录 B 站后重试。 / Hint: sign in to Bilibili again in Settings and retry."
    } else {
        "提示：可运行 course2md --login bilibili 扫码登录后重试；已登录时可重新登录。 / Hint: run course2md --login bilibili and scan the QR code to retry; if already logged in, log in again."
    };
    error.context(LoginTip(hint))
}

pub fn cookie_path() -> PathBuf {
    crate::config::config_dir().join("auth/bilibili.cookies.txt")
}

pub fn is_bilibili_url(input: &str) -> bool {
    crate::config::known_video_site(input) == Some("bilibili")
}

/// yt-dlp rewrites cookie files on exit. Give each subprocess a private snapshot
/// so simultaneous preview/subtitle/download requests cannot corrupt saved login.
/// The caller must keep the returned file alive until the subprocess exits.
pub fn configure_ytdlp(
    command: &mut std::process::Command,
    url: &str,
) -> Result<Option<tempfile::NamedTempFile>> {
    configure_with_path(command, url, &cookie_path())
}

fn configure_with_path(
    command: &mut std::process::Command,
    url: &str,
    path: &Path,
) -> Result<Option<tempfile::NamedTempFile>> {
    if !is_bilibili_url(url) {
        return Ok(None);
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e)
                .context(crate::tr!("无法读取 Bilibili 登录状态，请重新运行 course2md --login bilibili / Cannot read Bilibili login state; run course2md --login bilibili again"));
        }
    };
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(&bytes)?;
    file.flush()?;
    command.arg("--cookies").arg(file.path());
    Ok(Some(file))
}

/// Remove the saved session. Returns whether a saved login existed.
pub fn clear_bilibili_login() -> Result<bool> {
    match std::fs::remove_file(cookie_path()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).context(crate::tr!(
            "清除 Bilibili 登录状态失败 / Could not remove Bilibili login"
        )),
    }
}

pub fn logout_bilibili() -> Result<()> {
    if clear_bilibili_login()? {
        crate::println_tr!("已清除 Bilibili 本地登录状态。/ Bilibili login removed.");
    } else {
        crate::println_tr!("尚未登录 Bilibili。/ No Bilibili login saved.");
    }
    Ok(())
}

fn insert_cookie(cookies: &mut Cookies, name: &str, value: &str) {
    if COOKIE_NAMES.contains(&name)
        && !value.is_empty()
        && !value.chars().any(|c| c.is_control() || c == ';')
    {
        cookies.insert(name.into(), value.into());
    }
}

fn read_cookies(response: &Response, cookies: &mut Cookies) {
    for header in response.headers().get_all("set-cookie") {
        let Ok(header) = header.to_str() else {
            continue;
        };
        if let Some((name, value)) = header
            .split(';')
            .next()
            .and_then(|pair| pair.split_once('='))
        {
            insert_cookie(cookies, name.trim(), value.trim());
        }
    }
}

fn complete(cookies: &Cookies) -> bool {
    ["SESSDATA", "bili_jct", "DedeUserID"]
        .iter()
        .all(|key| cookies.contains_key(*key))
}

fn cookie_header(cookies: &Cookies) -> String {
    cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn netscape(cookies: &Cookies) -> Result<String> {
    ensure!(
        complete(cookies),
        crate::tr!(
            "登录响应未包含完整凭据，请重新扫码 / Login response did not include complete credentials; scan the QR code again"
        )
    );
    let mut text = String::from(
        "# Netscape HTTP Cookie File\n# course2md Bilibili login; do not share this file.\n",
    );
    for (name, value) in cookies {
        // Session cookies (expiry 0) remain valid until the server expires them.
        text.push_str(&format!(
            ".bilibili.com\tTRUE\t/\tTRUE\t0\t{name}\t{value}\n"
        ));
    }
    Ok(text)
}

fn save_cookies(path: &Path, cookies: &Cookies) -> Result<()> {
    let text = netscape(cookies)?;
    // atomic_write 经 tempfile 落盘，Unix 上权限为 0600
    crate::checkpoint::atomic_write(path, text.as_bytes()).context(crate::tr!(
        "保存 Bilibili 登录状态失败 / Failed to save Bilibili login state"
    ))
}

fn request(agent: &ureq::Agent, url: &str, cookies: &Cookies) -> Result<Response> {
    // Transport errors may contain the URL, including a one-use QR ticket.
    agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", "https://passport.bilibili.com/")
        .header("Cookie", &cookie_header(cookies))
        .call()
        .map_err(|_| anyhow::anyhow!(crate::tr!("Bilibili 登录请求失败，请检查网络后重试 / Bilibili login request failed; check the network and retry")))
}

fn data(mut response: Response) -> Result<Value> {
    let value: Value = response.body_mut().read_json().context(crate::tr!(
        "无法解析 Bilibili 登录响应 / Cannot parse Bilibili login response"
    ))?;
    ensure!(
        value["code"].as_i64() == Some(0),
        crate::tr!("Bilibili 登录接口返回错误 / Bilibili login API returned an error")
    );
    ensure!(
        value["data"].is_object(),
        crate::tr!("Bilibili 登录响应缺少 data / Bilibili login response is missing data")
    );
    Ok(value["data"].clone())
}

fn trusted_ticket_url(raw: &str) -> Result<url::Url> {
    let url = url::Url::parse(raw).map_err(|_| {
        anyhow::anyhow!(crate::tr!(
            "Bilibili 登录跳转地址无效 / Invalid Bilibili login redirect URL"
        ))
    })?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && url
                .host_str()
                .is_some_and(|host| host == "bilibili.com" || host.ends_with(".bilibili.com")),
        crate::tr!("Bilibili 登录跳转地址不受支持 / Unsupported Bilibili login redirect URL")
    );
    Ok(url)
}

fn finish_login(
    agent: &ureq::Agent,
    login: &Value,
    cookies: &mut Cookies,
) -> Result<AccountProfile> {
    finish_with(login, cookies, |url, cookies| request(agent, url, cookies))
}

fn finish_with(
    login: &Value,
    cookies: &mut Cookies,
    mut fetch: impl FnMut(&str, &Cookies) -> Result<Response>,
) -> Result<AccountProfile> {
    if let Some(raw) = login["url"].as_str().filter(|s| !s.is_empty()) {
        // Older responses embed cookies in the callback query; newer responses
        // provide a crossDomain ticket whose response sets the real cookies.
        let url = url::Url::parse(raw).map_err(|_| {
            anyhow::anyhow!(crate::tr!("登录回调地址无效 / Invalid login callback URL"))
        })?;
        for pair in url.query().unwrap_or_default().split('&') {
            if let Some((name, value)) = pair.split_once('=')
                && !cookies.contains_key(name)
            {
                insert_cookie(cookies, name, value);
            }
        }
        if !complete(cookies) {
            let mut next = trusted_ticket_url(raw)?;
            for _ in 0..5 {
                let response = fetch(next.as_str(), cookies)?;
                read_cookies(&response, cookies);
                if complete(cookies) {
                    break;
                }
                if !response.status().is_redirection() {
                    break;
                }
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|location| location.to_str().ok())
                    .context(crate::tr!(
                        "登录跳转缺少地址 / Login redirect is missing an address"
                    ))?;
                let joined = next.join(location).map_err(|_| {
                    anyhow::anyhow!(crate::tr!("登录跳转地址无效 / Invalid login redirect URL"))
                })?;
                next = trusted_ticket_url(joined.as_str())?;
            }
        }
    }
    ensure!(
        complete(cookies),
        crate::tr!(
            "登录响应缺少凭据，请重新运行 course2md --login bilibili / Login response is missing credentials; run course2md --login bilibili again"
        )
    );
    let value: Value = fetch(NAV, cookies)?
        .body_mut()
        .read_json()
        .context(crate::tr!(
            "无法解析 Bilibili 登录响应 / Cannot parse Bilibili login response"
        ))?;
    match nav_status(&value)? {
        AccountStatus::Connected(profile) => Ok(profile),
        _ => bail!(crate::tr!(
            "Bilibili 未确认登录，请重新扫码 / Bilibili did not confirm the login; scan the QR code again"
        )),
    }
}

/// 解析 nav 接口：-101 / isLogin=false 为失效，其余异常是暂时错误（不能当作已登出）。
fn nav_status(value: &Value) -> Result<AccountStatus> {
    if value["code"].as_i64() == Some(-101) {
        return Ok(AccountStatus::Expired);
    }
    ensure!(
        value["code"].as_i64() == Some(0),
        crate::tr!("无法验证账号状态，请稍后重试 / Cannot verify account status; retry later")
    );
    let profile = &value["data"];
    match profile["isLogin"].as_bool() {
        Some(true) => Ok(AccountStatus::Connected(AccountProfile {
            name: profile["uname"]
                .as_str()
                .unwrap_or(crate::tr!("Bilibili 用户"))
                .to_owned(),
        })),
        Some(false) => Ok(AccountStatus::Expired),
        None => bail!(crate::tr!(
            "账号状态响应不完整，请稍后重试 / Account status response is incomplete; retry later"
        )),
    }
}

#[derive(Debug, PartialEq)]
enum QrState {
    Waiting,
    Confirm,
    Expired,
    Done,
}
fn qr_state(value: &Value) -> Result<QrState> {
    match value["code"].as_i64() {
        Some(86101) => Ok(QrState::Waiting),
        Some(86090) => Ok(QrState::Confirm),
        Some(86038) => Ok(QrState::Expired),
        Some(0) => Ok(QrState::Done),
        _ => bail!(crate::tr!(
            "Bilibili 返回未知扫码状态，请重试 / Bilibili returned an unknown QR scan status; retry"
        )),
    }
}

/// Verified public account information. Credentials are never exposed or Debug-printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountProfile {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountStatus {
    Disconnected,
    Connected(AccountProfile),
    Expired,
}

fn login_agent() -> ureq::Agent {
    crate::runtime::http_agent(Duration::from_secs(12))
}

/// Check saved credentials against Bilibili. A network failure is an error, not logout.
pub fn bilibili_account_status() -> Result<AccountStatus> {
    let text = match std::fs::read_to_string(cookie_path()) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AccountStatus::Disconnected);
        }
        Err(_) => bail!(crate::tr!(
            "无法读取本地登录状态，请重新登录 / Cannot read the saved login state; log in again"
        )),
    };
    let mut cookies = Cookies::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let columns: Vec<_> = line.split('\t').collect();
        if columns.len() == 7 && columns[0] == ".bilibili.com" {
            insert_cookie(&mut cookies, columns[5], columns[6]);
        }
    }
    if !complete(&cookies) {
        return Ok(AccountStatus::Expired);
    }
    let value: Value = request(&login_agent(), NAV, &cookies)?
        .body_mut()
        .read_json()
        .map_err(|_| {
            anyhow::anyhow!(crate::tr!("无法解析账号状态 / Cannot parse account status"))
        })?;
    nav_status(&value)
}

/// A successful poll returns credentials in memory. The caller explicitly commits
/// only after checking that its GUI session is still current.
pub struct VerifiedLogin {
    cookies: Cookies,
    pub profile: AccountProfile,
}
impl VerifiedLogin {
    pub fn save(self) -> Result<AccountProfile> {
        save_cookies(&cookie_path(), &self.cookies)?;
        Ok(self.profile)
    }
}

pub enum QrPoll {
    Waiting,
    AwaitingConfirmation,
    Expired,
    Authenticated(VerifiedLogin),
}

/// Blocking, reusable session API. Run generation and polling off the UI thread.
/// Intentionally does not implement Debug: it owns one-use login tickets.
pub struct QrSession {
    agent: ureq::Agent,
    cookies: Cookies,
    poll_url: url::Url,
    code: qrcode::QrCode,
    started: Instant,
}
impl QrSession {
    pub fn generate() -> Result<Self> {
        let agent = login_agent();
        let mut cookies = Cookies::new();
        let response = request(&agent, &format!("{PASSPORT}/generate"), &cookies)?;
        read_cookies(&response, &mut cookies);
        let qr = data(response)?;
        let key = qr["qrcode_key"].as_str().context(crate::tr!(
            "二维码响应缺少 key / QR code response is missing key"
        ))?;
        let link = qr["url"].as_str().context(crate::tr!(
            "二维码响应缺少 url / QR code response is missing url"
        ))?;
        let code = qrcode::QrCode::new(link.as_bytes())
            .context(crate::tr!("生成二维码失败 / Failed to generate QR code"))?;
        let mut poll_url = url::Url::parse(&format!("{PASSPORT}/poll"))?;
        poll_url.query_pairs_mut().append_pair("qrcode_key", key);
        Ok(Self {
            agent,
            cookies,
            poll_url,
            code,
            started: Instant::now(),
        })
    }

    /// Renderable QR modules only; callers must add a four-module white quiet zone.
    pub fn modules(&self) -> Vec<Vec<bool>> {
        (0..self.code.width())
            .map(|y| {
                (0..self.code.width())
                    .map(|x| self.code[(x, y)] == qrcode::Color::Dark)
                    .collect()
            })
            .collect()
    }

    pub fn remaining(&self) -> Duration {
        Duration::from_secs(180).saturating_sub(self.started.elapsed())
    }

    pub fn poll(&mut self) -> Result<QrPoll> {
        if self.remaining().is_zero() {
            return Ok(QrPoll::Expired);
        }
        let response = request(&self.agent, self.poll_url.as_str(), &self.cookies)?;
        read_cookies(&response, &mut self.cookies);
        let login = data(response)?;
        match qr_state(&login)? {
            QrState::Waiting => Ok(QrPoll::Waiting),
            QrState::Confirm => Ok(QrPoll::AwaitingConfirmation),
            QrState::Expired => Ok(QrPoll::Expired),
            QrState::Done => {
                let profile = finish_login(&self.agent, &login, &mut self.cookies)?;
                Ok(QrPoll::Authenticated(VerifiedLogin {
                    cookies: std::mem::take(&mut self.cookies),
                    profile,
                }))
            }
        }
    }
}

pub fn login_bilibili() -> Result<()> {
    let mut session = QrSession::generate()?;
    crate::println_tr!(
        "请用哔哩哔哩 App 扫描二维码，并在手机上确认登录（Ctrl+C 取消）：/ Scan with the Bilibili app and confirm the login on your phone (Ctrl+C to cancel):"
    );
    println!(
        "{}",
        session
            .code
            .render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Light)
            .light_color(qrcode::render::unicode::Dense1x2::Dark)
            .build()
    );
    let mut confirmed = false;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        match session.poll()? {
            QrPoll::Waiting => {}
            QrPoll::AwaitingConfirmation => {
                if !confirmed {
                    crate::println_tr!(
                        "已扫码，请在手机上确认登录。/ Code scanned; confirm the login on your phone."
                    );
                    confirmed = true;
                }
            }
            QrPoll::Expired => bail!(crate::tr!(
                "二维码已过期，请重新运行 course2md --login bilibili / QR code expired; run course2md --login bilibili again"
            )),
            QrPoll::Authenticated(login) => {
                login.save()?;
                crate::println_tr!(
                    "Bilibili 登录成功。预览、字幕和视频下载将自动使用此登录状态。/ Bilibili login successful. Previews, subtitles and video downloads will use it automatically."
                );
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(status: u16, cookies: &[&str], body: &str) -> Response {
        let mut response = ureq::http::Response::builder().status(status);
        for cookie in cookies {
            response = response.header("Set-Cookie", *cookie);
        }
        if (300..400).contains(&status) {
            response = response.header("Location", "https://www.bilibili.com/");
        }
        response.body(ureq::Body::builder().data(body)).unwrap()
    }

    #[test]
    fn login_tip_preserves_failure_and_only_applies_to_bilibili_once() {
        let error = anyhow::anyhow!("HTTP Error 403").context("无法读取视频");
        let error = with_bilibili_login_tip("https://b23.tv/example", error);
        let error = with_bilibili_login_tip("https://b23.tv/example", error);
        let message = format!("{error:#}");
        assert!(message.contains("无法读取视频: HTTP Error 403"));
        assert_eq!(message.matches("--login bilibili").count(), 2);
        let error = with_bilibili_login_tip(
            "https://youtube.com/watch?v=bilibili.com",
            anyhow::anyhow!("HTTP Error 403"),
        );
        assert_eq!(error.to_string(), "HTTP Error 403");
    }

    #[test]
    fn login_tip_leaves_typed_causes_reachable() {
        #[derive(Debug)]
        struct Marker;
        impl std::fmt::Display for Marker {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("marker")
            }
        }
        impl std::error::Error for Marker {}
        let error = with_bilibili_login_tip("https://b23.tv/example", Marker.into());
        assert!(error.downcast_ref::<Marker>().is_some());
    }

    #[test]
    fn cookie_scope_excludes_lookalike_hosts() {
        for url in [
            "https://www.bilibili.com/video/BV1",
            "https://b23.tv/test",
            "www.bilibili.com/video/BV1",
        ] {
            assert!(is_bilibili_url(url));
        }
        for url in [
            "https://youtube.com/?bilibili.com",
            "https://bilibili.com.evil.test",
            "https://bilibili.com@evil.test",
            "file:///bilibili.com",
        ] {
            assert!(!is_bilibili_url(url));
        }
    }
    #[test]
    fn qr_states_are_explicit() {
        for (code, state) in [
            (86101, QrState::Waiting),
            (86090, QrState::Confirm),
            (86038, QrState::Expired),
            (0, QrState::Done),
        ] {
            assert_eq!(qr_state(&serde_json::json!({"code":code})).unwrap(), state);
        }
        assert!(qr_state(&serde_json::json!({})).is_err());
        assert!(qr_state(&serde_json::json!({"code":-1})).is_err());
    }
    #[test]
    fn cookies_are_private_complete_and_in_netscape_format() {
        let mut cookies = Cookies::new();
        let response = answer(
            200,
            &[
                "SESSDATA=abc%2C123; Domain=.bilibili.com; HttpOnly; Secure",
                "bili_jct=csrf; Path=/",
                "DedeUserID=123; Path=/",
            ],
            "",
        );
        read_cookies(&response, &mut cookies);
        insert_cookie(&mut cookies, "evil", "ignored");
        insert_cookie(&mut cookies, "buvid3", "bad\ninjection");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.txt");
        save_cookies(&path, &cookies).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(".bilibili.com\tTRUE\t/\tTRUE\t0\tSESSDATA\tabc%2C123\n"));
        assert!(!text.contains("evil"));
        assert!(!text.contains("injection"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(save_cookies(&path, &Cookies::new()).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }
    #[test]
    fn legacy_and_crossdomain_login_validate_before_saving() {
        for callback in [
            "https://passport.biligame.com/crossDomain?SESSDATA=abc%2C123&bili_jct=csrf&DedeUserID=123",
            "https://passport.bilibili.com/x/passport-login/web/crossDomain?ticket=one-use",
        ] {
            let mut cookies = Cookies::new();
            let mut calls = Vec::new();
            finish_with(
                &serde_json::json!({"url":callback}),
                &mut cookies,
                |url, jar| {
                    calls.push(url.to_owned());
                    if url.contains("/nav") {
                        assert!(complete(jar));
                        assert_eq!(jar["SESSDATA"], "abc%2C123");
                        Ok(answer(200, &[], r#"{"code":0,"data":{"isLogin":true}}"#))
                    } else {
                        Ok(answer(
                            302,
                            &[
                                "SESSDATA=abc%2C123; Secure",
                                "bili_jct=csrf",
                                "DedeUserID=123",
                            ],
                            "",
                        ))
                    }
                },
            )
            .unwrap();
            assert_eq!(calls.last().unwrap(), NAV);
            assert_eq!(
                calls.len(),
                if callback.contains("ticket=") { 2 } else { 1 }
            );
            assert!(
                finish_with(&serde_json::json!({}), &mut cookies, |_, _| {
                    Ok(answer(200, &[], r#"{"code":0,"data":{"isLogin":false}}"#))
                })
                .is_err()
            );
        }
    }

    #[test]
    fn downloader_snapshots_do_not_modify_saved_login() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.txt");
        std::fs::write(&path, "saved session").unwrap();
        let mut first = std::process::Command::new("yt-dlp");
        let mut second = std::process::Command::new("yt-dlp");
        let a = configure_with_path(&mut first, "https://www.bilibili.com/video/BV1", &path)
            .unwrap()
            .unwrap();
        let b = configure_with_path(&mut second, "https://b23.tv/test", &path)
            .unwrap()
            .unwrap();
        assert_ne!(a.path(), b.path());
        std::fs::write(a.path(), "rewritten by yt-dlp").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "saved session");
        assert_eq!(std::fs::read_to_string(b.path()).unwrap(), "saved session");
        let mut other = std::process::Command::new("yt-dlp");
        assert!(
            configure_with_path(&mut other, "https://youtube.com/", &path)
                .unwrap()
                .is_none()
        );
        assert_eq!(other.get_args().count(), 0);
        assert!(
            configure_with_path(
                &mut other,
                "https://b23.tv/test",
                &dir.path().join("missing")
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    #[ignore = "requires Bilibili network access; generates a QR session without logging in"]
    fn live_qr_generation_and_waiting_state() {
        let mut session = QrSession::generate().unwrap();
        let modules = session.modules();
        assert!(!modules.is_empty());
        assert!(modules.iter().all(|row| row.len() == modules.len()));
        assert!(matches!(session.poll().unwrap(), QrPoll::Waiting));
        // Local deadline terminates without a further network request.
        session.started = Instant::now() - Duration::from_secs(181);
        assert!(matches!(session.poll().unwrap(), QrPoll::Expired));
    }

    #[test]
    fn ticket_redirects_cannot_leave_bilibili() {
        assert!(
            trusted_ticket_url(
                "https://passport.bilibili.com/x/passport-login/web/crossDomain?ticket=test"
            )
            .is_ok()
        );
        for url in [
            "http://passport.bilibili.com/",
            "https://bilibili.com.evil.test/",
            "https://evil.test/",
            "https://user@bilibili.com/",
        ] {
            assert!(trusted_ticket_url(url).is_err());
        }
    }
}
