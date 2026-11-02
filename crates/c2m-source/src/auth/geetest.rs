//! Geetest's check runs only in a browser. The app serves one page on the
//! loopback interface that loads Geetest's official script with Bilibili's
//! `gt` and `challenge`, opens it in the system browser, and takes back the
//! `validate`/`seccode` the check produces. Nothing else is served or accepted:
//! the page and its result live under a random path, requests must name this
//! server as their host (no DNS rebinding), and the first valid result ends it.
use super::passport::{Captcha, GeetestSolution};
use crate::tr;
use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Geetest's own loader, pinned; it fetches the rest of the check from Geetest.
const GEETEST_SCRIPT: &str = "https://static.geetest.com/static/js/gt.0.5.0.js";
/// How long a person has to finish the check before it is given up.
pub const CAPTCHA_TIMEOUT: Duration = Duration::from_secs(300);

/// A served check page. Dropping it closes the server.
pub struct CaptchaPage {
    listener: TcpListener,
    secret: String,
    port: u16,
    page: String,
}

impl CaptchaPage {
    pub fn start(captcha: &Captcha) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context(tr!("无法启动本机验证页面"))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!(tr!("无法启动本机验证页面")))?;
        let secret: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| anyhow::anyhow!(tr!("无法启动本机验证页面")))?;
        let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            listener,
            page: page(captcha, &nonce),
            secret,
            port,
        })
    }

    /// The page to open in the browser.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/{}/", self.port, self.secret)
    }

    /// Serves the page until the check is passed, `cancel` is set or the time
    /// runs out. Blocking; run it off the UI thread.
    pub fn wait(&self, cancel: &AtomicBool, timeout: Duration) -> Result<GeetestSolution> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!(tr!("已取消验证"));
            }
            if Instant::now() >= deadline {
                bail!(tr!("人机验证超时，请重新开始"));
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(solution) = self.answer(stream, cancel) {
                        return Ok(solution);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error).context(tr!("本机验证页面已停止")),
            }
        }
    }

    /// One request. Returns the solution when this request delivered a valid one.
    fn answer(&self, mut stream: TcpStream, cancel: &AtomicBool) -> Option<GeetestSolution> {
        let _ = stream.set_nonblocking(false);
        let request = read_request(&mut stream, Instant::now() + REQUEST_TIMEOUT, cancel)?;
        let host = format!("127.0.0.1:{}", self.port);
        let origin_ok = request
            .header("origin")
            .is_none_or(|origin| origin == format!("http://{host}"));
        if request.header("host") != Some(host.as_str()) || !origin_ok {
            respond(&mut stream, "403 Forbidden", "text/plain", "");
            return None;
        }
        let root = format!("/{}/", self.secret);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", path) if path == root => {
                respond(
                    &mut stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    &self.page,
                );
                None
            }
            ("POST", path) if path == format!("{root}result") => {
                match serde_json::from_slice::<GeetestSolution>(&request.body) {
                    Ok(solution)
                        if !solution.validate.is_empty() && !solution.seccode.is_empty() =>
                    {
                        respond(&mut stream, "200 OK", "text/plain", "ok");
                        Some(solution)
                    }
                    _ => {
                        respond(&mut stream, "400 Bad Request", "text/plain", "");
                        None
                    }
                }
            }
            _ => {
                respond(&mut stream, "404 Not Found", "text/plain", "");
                None
            }
        }
    }
}

/// Opens the check in the system browser. Returns whether one was started;
/// either way the address stays visible for the person to open by hand.
pub fn open_in_browser(url: &str) -> bool {
    open::that_detached(url).is_ok()
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// How long one connection may take to deliver its request. Connections are
/// served one at a time, so a silent one (a browser's preconnect, a slow local
/// client) holds up the next only this long.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Reads at most until `deadline`, in short waits so `cancel` is seen promptly.
/// `None` once the deadline passes, the check is cancelled or the peer closes.
fn read_some(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    deadline: Instant,
    cancel: &AtomicBool,
) -> Option<usize> {
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        if left.is_zero() || cancel.load(Ordering::Relaxed) {
            return None;
        }
        let _ = stream.set_read_timeout(Some(left.min(Duration::from_millis(100))));
        match stream.read(buffer) {
            Ok(0) => return None,
            Ok(read) => return Some(read),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return None,
        }
    }
}

/// A bounded HTTP/1.1 request: 16 KiB of head, 4 KiB of body, by `deadline`.
fn read_request(stream: &mut TcpStream, deadline: Instant, cancel: &AtomicBool) -> Option<Request> {
    let mut data = Vec::new();
    let mut buffer = [0u8; 2048];
    let head_end = loop {
        if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if data.len() > 16 * 1024 {
            return None;
        }
        let read = read_some(stream, &mut buffer, deadline, cancel)?;
        data.extend_from_slice(&buffer[..read]);
    };
    let head = std::str::from_utf8(&data[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: Vec<_> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    if length > 4096 {
        return None;
    }
    let mut body = data[head_end + 4..].to_vec();
    while body.len() < length {
        let read = read_some(stream, &mut buffer, deadline, cancel)?;
        body.extend_from_slice(&buffer[..read]);
    }
    body.truncate(length);
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// The check page: Geetest's widget, a line of status and nothing else. Values
/// are JSON-encoded into the script; scripts run only from Geetest or this page.
fn page(captcha: &Captcha, nonce: &str) -> String {
    // `<` is escaped so no value can close the script element it sits in.
    let json = |text: &str| {
        serde_json::to_string(text)
            .unwrap_or_default()
            .replace('<', "\\u003c")
    };
    let lang = match c2m_i18n::i18n::lang().code() {
        "zh-Hant" => "zh-tw",
        "en" => "en",
        "ja" => "ja",
        _ => "zh-cn",
    };
    let geetest = "https://*.geetest.com https://*.geevisit.com";
    format!(
        r#"<!doctype html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'nonce-{nonce}' {geetest}; style-src 'unsafe-inline' {geetest}; img-src https: data:; connect-src 'self' {geetest}; frame-src {geetest}; font-src {geetest}">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; }}
body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: Canvas; color: CanvasText; }}
main {{ max-width: 28rem; padding: 2rem; text-align: center; line-height: 1.5; }}
h1 {{ font-size: 1.25rem; font-weight: 600; margin: 0 0 .5rem; }}
p {{ margin: 0; opacity: .8; }}
button {{ margin-top: 1rem; font: inherit; padding: .5rem 1.25rem; border-radius: 999px; border: 1px solid currentColor; background: transparent; color: inherit; cursor: pointer; }}
</style>
</head>
<body>
<main>
<h1>{title}</h1>
<p id="status">{loading}</p>
<button id="again" type="button" hidden>{again}</button>
</main>
<script nonce="{nonce}" src="{GEETEST_SCRIPT}"></script>
<script nonce="{nonce}">
(function () {{
  var status = document.getElementById("status");
  var again = document.getElementById("again");
  function say(text) {{ status.textContent = text; }}
  if (typeof initGeetest !== "function") {{ say({failed}); return; }}
  initGeetest({{ gt: {gt}, challenge: {challenge}, offline: false, new_captcha: true, product: "bind", lang: "{lang}", https: true }}, function (captcha) {{
    captcha.onReady(function () {{ say({prompt}); captcha.verify(); }});
    captcha.onSuccess(function () {{
      var result = captcha.getValidate();
      if (!result) {{ say({failed}); return; }}
      fetch("result", {{ method: "POST", headers: {{ "Content-Type": "application/json" }}, body: JSON.stringify({{ challenge: result.geetest_challenge, validate: result.geetest_validate, seccode: result.geetest_seccode }}) }})
        .then(function (response) {{ say(response.ok ? {done} : {failed}); }})
        .catch(function () {{ say({failed}); }});
    }});
    captcha.onClose(function () {{
      say({closed});
      again.hidden = false;
      again.onclick = function () {{ again.hidden = true; say({prompt}); captcha.verify(); }};
    }});
    captcha.onError(function () {{ say({failed}); }});
  }});
}})();
</script>
</body>
</html>
"#,
        title = tr!("course2md 人机验证"),
        loading = tr!("正在加载验证…"),
        prompt = json(tr!("请完成验证。完成后回到 course2md。")),
        done = json(tr!("验证完成，可以关闭此页面并回到 course2md。")),
        failed = json(tr!("验证未能加载或未通过，请回到 course2md 重新开始。")),
        closed = json(tr!("验证已关闭。")),
        again = tr!("重新验证"),
        gt = json(&captcha.gt),
        challenge = json(&captcha.challenge),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captcha() -> Captcha {
        Captcha {
            token: "tok".into(),
            gt: "g\"</script>".into(),
            challenge: "c".into(),
        }
    }

    fn send(port: u16, raw: String) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        out
    }

    #[test]
    fn the_page_and_its_result_live_only_under_the_secret_path_and_host() {
        let page = CaptchaPage::start(&captcha()).unwrap();
        let port = page.port;
        let root = format!("/{}/", page.secret);
        assert!(page.url().ends_with(&root));
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let client = std::thread::spawn({
            let root = root.clone();
            move || {
                let host = format!("127.0.0.1:{port}");
                let html = send(port, format!("GET {root} HTTP/1.1\r\nHost: {host}\r\n\r\n"));
                let wrong_path = send(port, format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"));
                let rebound = send(
                    port,
                    format!("GET {root} HTTP/1.1\r\nHost: attacker.example:{port}\r\n\r\n"),
                );
                let body = r#"{"challenge":"c","validate":"v","seccode":"v|jordan"}"#;
                let foreign = send(
                    port,
                    format!(
                        "POST {root}result HTTP/1.1\r\nHost: {host}\r\nOrigin: https://evil.example\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    ),
                );
                let empty = send(
                    port,
                    format!(
                        "POST {root}result HTTP/1.1\r\nHost: {host}\r\nContent-Length: 2\r\n\r\n{{}}"
                    ),
                );
                let accepted = send(
                    port,
                    format!(
                        "POST {root}result HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    ),
                );
                (html, wrong_path, rebound, foreign, empty, accepted)
            }
        });
        let solution = page.wait(&cancel, Duration::from_secs(20)).unwrap();
        let (html, wrong_path, rebound, foreign, empty, accepted) = client.join().unwrap();
        assert!(html.starts_with("HTTP/1.1 200"));
        assert!(html.contains(GEETEST_SCRIPT));
        assert!(html.contains(r#"gt: "g\"\u003c/script>""#), "{html}");
        assert!(wrong_path.starts_with("HTTP/1.1 404"));
        assert!(rebound.starts_with("HTTP/1.1 403"));
        assert!(foreign.starts_with("HTTP/1.1 403"));
        assert!(empty.starts_with("HTTP/1.1 400"));
        assert!(accepted.starts_with("HTTP/1.1 200"));
        assert_eq!(solution.seccode, "v|jordan");
    }

    #[test]
    fn waiting_stops_on_cancel_and_on_time() {
        let page = CaptchaPage::start(&captcha()).unwrap();
        let started = Instant::now();
        assert!(page.wait(&AtomicBool::new(true), CAPTCHA_TIMEOUT).is_err());
        assert!(
            page.wait(&AtomicBool::new(false), Duration::from_millis(200))
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_silent_connection_holds_up_the_result_only_briefly_and_never_cancel() {
        let page = CaptchaPage::start(&captcha()).unwrap();
        let port = page.port;
        let root = format!("/{}/", page.secret);
        // A preconnect that never sends, then the real result behind it.
        let silent = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let client = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let host = format!("127.0.0.1:{port}");
            let body = r#"{"challenge":"c","validate":"v","seccode":"v|jordan"}"#;
            send(
                port,
                format!(
                    "POST {root}result HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                ),
            )
        });
        let started = Instant::now();
        let solution = page.wait(&AtomicBool::new(false), Duration::from_secs(20));
        assert_eq!(solution.unwrap().validate, "v");
        assert!(started.elapsed() < REQUEST_TIMEOUT + Duration::from_secs(2));
        assert!(client.join().unwrap().starts_with("HTTP/1.1 200"));
        drop(silent);

        // Cancelling while a silent connection is being read ends the wait at once.
        let page = CaptchaPage::start(&captcha()).unwrap();
        let _silent = TcpStream::connect(("127.0.0.1", page.port)).unwrap();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let cancel = cancel.clone();
            move || {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::Relaxed);
            }
        });
        let started = Instant::now();
        assert!(page.wait(&cancel, Duration::from_secs(20)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// Serves a live Bilibili captcha for a minute and writes the page's URL to
    /// `COURSE2MD_TEST_CAPTCHA_URL`, so a headless browser with a throwaway
    /// profile can confirm Geetest's widget loads under the page's CSP.
    #[test]
    #[ignore = "requires Bilibili and Geetest network access; submits nothing"]
    fn live_page_for_a_browser() {
        let captcha = crate::auth::passport::Passport::new().captcha().unwrap();
        let page = CaptchaPage::start(&captcha).unwrap();
        std::fs::write(
            std::env::var_os("COURSE2MD_TEST_CAPTCHA_URL").unwrap(),
            page.url(),
        )
        .unwrap();
        let _ = page.wait(&AtomicBool::new(false), Duration::from_secs(60));
    }
}
