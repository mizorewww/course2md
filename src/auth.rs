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

pub const BILIBILI_SETUP_TIP: &str = "使用 Bilibili 视频时，推荐先运行 course2md --login bilibili 扫码登录，可下载账号权限范围内更高清晰度的视频。";

/// Keep the original failure visible; login is a suggested next step, not a diagnosis.
pub fn with_bilibili_login_tip(url: &str, error: anyhow::Error) -> anyhow::Error {
    let message = format!("{error:#}");
    if is_bilibili_url(url) && !message.contains("--login bilibili") {
        anyhow::anyhow!(
            "{message}\n提示：可运行 course2md --login bilibili 扫码登录后重试；已登录时可重新登录。"
        )
    } else {
        error
    }
}

pub fn cookie_path() -> PathBuf {
    crate::config::config_dir().join("auth/bilibili.cookies.txt")
}

pub fn is_bilibili_url(input: &str) -> bool {
    let parsed = url::Url::parse(input).or_else(|_| url::Url::parse(&format!("https://{input}")));
    parsed.is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| {
                host == "bilibili.com" || host.ends_with(".bilibili.com") || host == "b23.tv"
            })
    })
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
                .context("无法读取 Bilibili 登录状态，请重新运行 course2md --login bilibili");
        }
    };
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(&bytes)?;
    file.flush()?;
    command.arg("--cookies").arg(file.path());
    Ok(Some(file))
}

pub fn logout_bilibili() -> Result<()> {
    match std::fs::remove_file(cookie_path()) {
        Ok(()) => println!("已清除 Bilibili 本地登录状态。"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("尚未登录 Bilibili。"),
        Err(e) => return Err(e).context("清除 Bilibili 登录状态失败"),
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

fn read_cookies(response: &ureq::Response, cookies: &mut Cookies) {
    for header in response.all("set-cookie") {
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
    ensure!(complete(cookies), "登录响应未包含完整凭据，请重新扫码");
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
    let parent = path.parent().context("登录状态路径无效")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?; // mode 0600 on Unix
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|e| e.error)
        .context("保存 Bilibili 登录状态失败")?;
    Ok(())
}

fn request(agent: &ureq::Agent, url: &str, cookies: &Cookies) -> Result<ureq::Response> {
    // Transport errors may contain the URL, including a one-use QR ticket.
    agent
        .get(url)
        .set("Referer", "https://passport.bilibili.com/")
        .set("Cookie", &cookie_header(cookies))
        .call()
        .map_err(|_| anyhow::anyhow!("Bilibili 登录请求失败，请检查网络后重试"))
}

fn data(response: ureq::Response) -> Result<Value> {
    let value: Value = response.into_json().context("无法解析 Bilibili 登录响应")?;
    ensure!(
        value["code"].as_i64() == Some(0),
        "Bilibili 登录接口返回错误"
    );
    ensure!(value["data"].is_object(), "Bilibili 登录响应缺少 data");
    Ok(value["data"].clone())
}

fn trusted_ticket_url(raw: &str) -> Result<url::Url> {
    let url = url::Url::parse(raw).map_err(|_| anyhow::anyhow!("Bilibili 登录跳转地址无效"))?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && url
                .host_str()
                .is_some_and(|host| host == "bilibili.com" || host.ends_with(".bilibili.com")),
        "Bilibili 登录跳转地址不受支持"
    );
    Ok(url)
}

fn finish_login(agent: &ureq::Agent, login: &Value, cookies: &mut Cookies) -> Result<()> {
    finish_with(login, cookies, |url, cookies| request(agent, url, cookies))
}

fn finish_with(
    login: &Value,
    cookies: &mut Cookies,
    mut fetch: impl FnMut(&str, &Cookies) -> Result<ureq::Response>,
) -> Result<()> {
    if let Some(raw) = login["url"].as_str().filter(|s| !s.is_empty()) {
        // Older responses embed cookies in the callback query; newer responses
        // provide a crossDomain ticket whose response sets the real cookies.
        let url = url::Url::parse(raw).map_err(|_| anyhow::anyhow!("登录回调地址无效"))?;
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
                if !(300..400).contains(&response.status()) {
                    break;
                }
                let location = response.header("location").context("登录跳转缺少地址")?;
                let joined = next
                    .join(location)
                    .map_err(|_| anyhow::anyhow!("登录跳转地址无效"))?;
                next = trusted_ticket_url(joined.as_str())?;
            }
        }
    }
    ensure!(
        complete(cookies),
        "登录响应缺少凭据，请重新运行 course2md --login bilibili"
    );
    let profile = data(fetch(
        "https://api.bilibili.com/x/web-interface/nav",
        cookies,
    )?)?;
    ensure!(
        profile["isLogin"].as_bool() == Some(true),
        "Bilibili 未确认登录，请重新扫码"
    );
    Ok(())
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
        _ => bail!("Bilibili 返回未知扫码状态，请重试"),
    }
}

pub fn login_bilibili() -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .redirects(0)
        .user_agent(USER_AGENT)
        .build();
    let mut cookies = Cookies::new();
    let response = request(&agent, &format!("{PASSPORT}/generate"), &cookies)?;
    read_cookies(&response, &mut cookies);
    let qr = data(response)?;
    let key = qr["qrcode_key"].as_str().context("二维码响应缺少 key")?;
    let link = qr["url"].as_str().context("二维码响应缺少 url")?;
    let code = qrcode::QrCode::new(link.as_bytes()).context("生成二维码失败")?;
    println!("请用哔哩哔哩 App 扫描二维码，并在手机上确认登录（Ctrl+C 取消）：");
    println!(
        "{}",
        code.render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Light)
            .light_color(qrcode::render::unicode::Dense1x2::Dark)
            .build()
    );
    let mut poll = url::Url::parse(&format!("{PASSPORT}/poll"))?;
    poll.query_pairs_mut().append_pair("qrcode_key", key);
    let start = Instant::now();
    let mut confirmed = false;
    while start.elapsed() < Duration::from_secs(180) {
        std::thread::sleep(Duration::from_secs(2));
        let response = request(&agent, poll.as_str(), &cookies)?;
        read_cookies(&response, &mut cookies);
        let login = data(response)?;
        match qr_state(&login)? {
            QrState::Waiting => {}
            QrState::Confirm => {
                if !confirmed {
                    println!("已扫码，请在手机上确认登录。");
                    confirmed = true;
                }
            }
            QrState::Expired => break,
            QrState::Done => {
                finish_login(&agent, &login, &mut cookies)?;
                save_cookies(&cookie_path(), &cookies)?;
                println!("Bilibili 登录成功。预览、字幕和视频下载将自动使用此登录状态。");
                return Ok(());
            }
        }
    }
    bail!("二维码已过期，请重新运行 course2md --login bilibili")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn login_tip_preserves_failure_and_only_applies_to_bilibili_once() {
        let error = anyhow::anyhow!("HTTP Error 403").context("无法读取视频");
        let error = with_bilibili_login_tip("https://b23.tv/example", error);
        let error = with_bilibili_login_tip("https://b23.tv/example", error);
        let message = error.to_string();
        assert!(message.contains("无法读取视频: HTTP Error 403"));
        assert_eq!(message.matches("--login bilibili").count(), 1);
        let error = with_bilibili_login_tip(
            "https://youtube.com/watch?v=bilibili.com",
            anyhow::anyhow!("HTTP Error 403"),
        );
        assert_eq!(error.to_string(), "HTTP Error 403");
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
        let response: ureq::Response = "HTTP/1.1 200 OK\r\nSet-Cookie: SESSDATA=abc%2C123; Domain=.bilibili.com; HttpOnly; Secure\r\nSet-Cookie: bili_jct=csrf; Path=/\r\nSet-Cookie: DedeUserID=123; Path=/\r\n\r\n".parse().unwrap();
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
            finish_with(&serde_json::json!({"url":callback}), &mut cookies, |url, jar| {
                calls.push(url.to_owned());
                if url.contains("/nav") {
                    assert!(complete(jar));
                    assert_eq!(jar["SESSDATA"], "abc%2C123");
                    Ok(ureq::Response::new(200, "OK", r#"{"code":0,"data":{"isLogin":true}}"#).unwrap())
                } else {
                    Ok("HTTP/1.1 302 Found\r\nSet-Cookie: SESSDATA=abc%2C123; Secure\r\nSet-Cookie: bili_jct=csrf\r\nSet-Cookie: DedeUserID=123\r\nLocation: https://www.bilibili.com/\r\n\r\n".parse().unwrap())
                }
            }).unwrap();
            assert_eq!(
                calls.last().unwrap(),
                "https://api.bilibili.com/x/web-interface/nav"
            );
            assert_eq!(
                calls.len(),
                if callback.contains("ticket=") { 2 } else { 1 }
            );
            assert!(
                finish_with(&serde_json::json!({}), &mut cookies, |_, _| {
                    Ok(
                        ureq::Response::new(200, "OK", r#"{"code":0,"data":{"isLogin":false}}"#)
                            .unwrap(),
                    )
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
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(12))
            .user_agent(USER_AGENT)
            .build();
        let cookies = Cookies::new();
        let qr = data(request(&agent, &format!("{PASSPORT}/generate"), &cookies).unwrap()).unwrap();
        let key = qr["qrcode_key"].as_str().unwrap();
        let link = qr["url"].as_str().unwrap();
        assert!(qrcode::QrCode::new(link.as_bytes()).is_ok());
        let mut url = url::Url::parse(&format!("{PASSPORT}/poll")).unwrap();
        url.query_pairs_mut().append_pair("qrcode_key", key);
        let poll = data(request(&agent, url.as_str(), &cookies).unwrap()).unwrap();
        assert_eq!(qr_state(&poll).unwrap(), QrState::Waiting);
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
