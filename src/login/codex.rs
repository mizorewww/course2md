//! Codex 登录：导入 codex CLI 的 ChatGPT OAuth 登录态（~/.codex/auth.json）。
//!
//! 凭据存 course2md 自有副本 `{config_dir}/auth/codex.json`（0600 原子写），
//! 绝不改写 ~/.codex/auth.json。access token 过期前自动刷新（refresh token
//! 会轮换，刷新后立即写回）。
//!
//! 协议常量对照 openai/codex 源码（codex-rs/login/src/auth/manager.rs、
//! codex-rs/model-provider-info/src/lib.rs），见各常量注释。

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::LlmProvider;

/// codex CLI 的公开 OAuth client_id（login/src/auth/manager.rs `CLIENT_ID`）。
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// ChatGPT refresh 走 JSON POST（authorization-code 才是 form 编码）。
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// access token 提前刷新窗口（manager.rs CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES = 5）。
const REFRESH_WINDOW_SECS: i64 = 300;
/// JWT 无法解析 exp 时的兜底刷新间隔（manager.rs TOKEN_REFRESH_INTERVAL = 8 天）。
const REFRESH_FALLBACK_SECS: i64 = 8 * 24 * 3600;
/// 登录后默认模型（拉取模型目录失败时的兜底；连接测试会验证 slug 是否可用）。
pub(super) const DEFAULT_MODEL: &str = "gpt-5.5";
/// 拉取模型目录用的客户端版本（目录按 client_version 门控，过低会返回空列表）。
const CLIENT_VERSION: &str = "1.0.0";

/// 拉取该账号可用的模型目录（slug + 展示名）；失败由调用方回落默认模型。
fn fetch_models(tokens: &CodexTokens) -> Result<Vec<(String, String)>> {
    let agent = crate::runtime::http_agent(Duration::from_secs(15));
    let mut request = agent
        .get(&format!(
            "{}/models?client_version={CLIENT_VERSION}",
            crate::provider::CODEX_BASE_URL
        ))
        .header("Authorization", &format!("Bearer {}", tokens.access_token))
        .header("originator", "codex_cli_rs");
    if let Some(account_id) = &tokens.account_id {
        request = request.header("ChatGPT-Account-ID", account_id);
    }
    // The root cause stays specific (status or transport failure); the
    // context names what failed.
    let mut response = request.call().map_err(|e| {
        let cause = match e {
            ureq::Error::StatusCode(401 | 403) => return anyhow::Error::new(CodexLoginRequired),
            ureq::Error::StatusCode(status) => anyhow::anyhow!("HTTP {status}"),
            other => anyhow::anyhow!("{other}"),
        };
        cause.context(crate::tr!(
            "无法获取 Codex 模型目录 / Cannot fetch the Codex model catalog"
        ))
    })?;
    let value: Value = response.body_mut().read_json().context(crate::tr!(
        "Codex 模型目录无法解析 / Cannot parse the Codex model catalog"
    ))?;
    let Some(models) = value["models"].as_array() else {
        bail!(crate::tr!(
            "Codex 模型目录缺少 models / Codex model catalog is missing models"
        ));
    };
    Ok(models
        .iter()
        .filter_map(|m| {
            let slug = m["slug"].as_str()?.to_string();
            let label = m["display_name"].as_str().unwrap_or(&slug).to_string();
            Some((slug, label))
        })
        .collect())
}

/// 需要（重新）登录 Codex 的标记错误：桌面端在错误链中识别它并替换为
/// 界面内的连接引导，CLI 直接展示其双语说明。不含任何凭据内容。
#[derive(Debug)]
pub struct CodexLoginRequired;

impl std::fmt::Display for CodexLoginRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(crate::tr!("Codex 登录缺失或已失效，请重新登录 / Codex login is missing or has expired; log in again"))
    }
}

impl std::error::Error for CodexLoginRequired {}

/// 桌面端登录动作的模型目录结果。`catalog_is_fallback = true` 表示目录拉取失败、
/// 已回落默认模型——界面应如实说明，而不是把兜底冒充为账号目录。
#[derive(Debug, Clone)]
pub struct DesktopCatalog {
    pub models: Vec<(String, String)>,
    pub catalog_is_fallback: bool,
}

/// 登录成功后的目录：失败回落默认模型并标记，由调用方如实提示。
fn desktop_catalog(tokens: &CodexTokens) -> DesktopCatalog {
    match fetch_models(tokens) {
        Ok(models) => DesktopCatalog {
            models,
            catalog_is_fallback: false,
        },
        Err(e) => {
            tracing::warn!("{e:#}");
            DesktopCatalog {
                models: vec![(DEFAULT_MODEL.into(), DEFAULT_MODEL.into())],
                catalog_is_fallback: true,
            }
        }
    }
}

/// course2md 自有的 codex 凭据副本。
#[derive(Clone, Serialize, Deserialize)]
pub struct CodexTokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub account_id: Option<String>,
    /// 本副本获取/刷新时间（unix 秒）；JWT exp 解析失败时的兜底依据
    pub obtained_at: i64,
}

/// 审计红线：凭据不出现在日志/错误里。
impl std::fmt::Debug for CodexTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexTokens")
            .field("account_id", &self.account_id)
            .field("obtained_at", &self.obtained_at)
            .finish_non_exhaustive()
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn credential_path() -> PathBuf {
    crate::config::config_dir().join("auth/codex.json")
}

/// codex CLI 的登录态文件（~/.codex/auth.json）。
fn codex_cli_auth_path() -> PathBuf {
    crate::config::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
        .join("auth.json")
}

pub fn load_tokens() -> Result<Option<CodexTokens>> {
    let path = credential_path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).context(crate::tr!(
                "无法读取 Codex 登录状态 / Cannot read Codex login state"
            ));
        }
    };
    let tokens: CodexTokens = serde_json::from_slice(&bytes).context(crate::tr!(
        "Codex 登录状态损坏，请重新登录 / Codex login state is damaged; log in again"
    ))?;
    Ok(Some(tokens))
}

fn save_tokens(tokens: &CodexTokens) -> Result<()> {
    crate::checkpoint::atomic_write(&credential_path(), &serde_json::to_vec_pretty(tokens)?)
        .context(crate::tr!(
            "保存 Codex 登录状态失败 / Failed to save Codex login state"
        ))
}

/// JWT payload 解析（只读 claim，不做签名校验——令牌由本机持有）。
fn jwt_payload(token: &str) -> Option<Value> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// JWT payload 的 exp（unix 秒）。
fn jwt_exp(token: &str) -> Option<i64> {
    jwt_payload(token)?["exp"].as_i64()
}

fn needs_refresh(tokens: &CodexTokens) -> bool {
    match jwt_exp(&tokens.access_token) {
        Some(exp) => exp <= now_secs() + REFRESH_WINDOW_SECS,
        None => tokens.obtained_at + REFRESH_FALLBACK_SECS <= now_secs(),
    }
}

/// 刷新（refresh token 单次使用且会轮换：成功必须写回；4xx = 登录已失效）。
fn refresh(tokens: &CodexTokens) -> Result<CodexTokens> {
    let agent = crate::runtime::http_agent(Duration::from_secs(30));
    let mut response = agent
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .send_json(serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": tokens.refresh_token,
        }))
        .map_err(|e| match e {
            ureq::Error::StatusCode(status) if (400..500).contains(&status) => {
                anyhow::Error::new(CodexLoginRequired)
            }
            _ => anyhow::anyhow!(
                crate::tr!("无法连接 OpenAI 认证服务，稍后重试 / Cannot reach the OpenAI auth service; retry later")
            ),
        })?;
    let value: Value = response.body_mut().read_json().context(crate::tr!(
        "无法解析刷新响应 / Cannot parse the token refresh response"
    ))?;
    let refreshed = CodexTokens {
        access_token: value["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&tokens.access_token)
            .to_string(),
        // refresh token 轮换：返回新值才覆盖，否则保留旧值
        refresh_token: value["refresh_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&tokens.refresh_token)
            .to_string(),
        account_id: tokens.account_id.clone(),
        obtained_at: now_secs(),
    };
    save_tokens(&refreshed)?;
    Ok(refreshed)
}

/// Longer than one refresh request, so a waiter normally finds the holder's result.
const REFRESH_LOCK_WAIT: Duration = Duration::from_secs(45);

/// 获取可用凭据：临期/过期则在锁内复查后刷新并写回。
///
/// The refresh token is single-use, and proofreading workers, task processes and the
/// desktop app all ask for tokens: the lock is a file, so only one of them refreshes.
fn fresh_tokens() -> Result<CodexTokens> {
    let Some(tokens) = load_tokens()? else {
        return Err(CodexLoginRequired.into());
    };
    if !needs_refresh(&tokens) {
        return Ok(tokens);
    }
    let path = credential_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _guard = crate::runtime::lock_file_within(&path.with_extension("lock"), REFRESH_LOCK_WAIT)?;
    // 锁内复查：另一线程或进程可能刚刷新完
    let Some(current) = load_tokens()? else {
        return Err(CodexLoginRequired.into());
    };
    if !needs_refresh(&current) {
        return Ok(current);
    }
    refresh(&current)
}

/// 生产请求注入点（provider::auth_headers 的 Codex 分支）。
pub(crate) fn auth_headers() -> Result<Vec<(String, String)>> {
    let tokens = fresh_tokens()?;
    let mut headers = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", tokens.access_token),
        ),
        ("originator".to_string(), "codex_cli_rs".to_string()),
        ("Accept".to_string(), "text/event-stream".to_string()),
    ];
    if let Some(account_id) = tokens.account_id.filter(|id| !id.is_empty()) {
        headers.push(("ChatGPT-Account-ID".to_string(), account_id));
    }
    Ok(headers)
}

/// 从 codex CLI 的 auth.json 导入登录态（只读原文件）。
fn import_from_codex_cli() -> Result<CodexTokens> {
    let path = codex_cli_auth_path();
    let bytes = std::fs::read(&path).with_context(|| {
        format!(
            "未找到 codex CLI 的登录态（{}）；请先运行 codex 登录 ChatGPT 账号 / Codex CLI login not found ({}); log in with the codex CLI first",
            path.display(),
            path.display()
        )
    })?;
    let value: Value = serde_json::from_slice(&bytes).context(crate::tr!(
        "codex CLI 登录态无法解析 / Cannot parse the codex CLI login state"
    ))?;
    if value["auth_mode"].as_str() == Some("apikey") {
        bail!(crate::tr!(
            "codex CLI 当前是 API key 模式；请在 codex 中用 ChatGPT 账号登录后重试 / The codex CLI is in API-key mode; log in with a ChatGPT account in codex and retry"
        ));
    }
    let tokens = &value["tokens"];
    let access = tokens["access_token"].as_str().unwrap_or_default();
    let refresh = tokens["refresh_token"].as_str().unwrap_or_default();
    ensure!(
        !access.is_empty() && !refresh.is_empty(),
        crate::tr!(
            "codex CLI 登录态缺少令牌，请在 codex 中重新登录 / The codex CLI login state is missing tokens; log in again in codex"
        )
    );
    Ok(CodexTokens {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        account_id: tokens["account_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        obtained_at: now_secs(),
    })
}

// ---------- PKCE 浏览器授权（无 codex CLI 时的兜底；对照 codex-rs/login/src/server.rs） ----------

const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// 与 codex CLI 的 redirect allow-list 保持一致（server.rs DEFAULT_PORT/FALLBACK_PORT）。
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const OAUTH_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
/// 等待用户完成浏览器授权的总时限（与扫码登录一致）。
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);

/// PKCE S256：BASE64URL-NO-PAD(SHA256(verifier))。
fn pkce_challenge(verifier: &str) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// The authorization server answered this login with a refusal; waiting longer
/// cannot change that. Holds what the server said, which may name the cause.
#[derive(Debug)]
struct AuthorizationDenied(String);

impl std::fmt::Display for AuthorizationDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Text from a URL reaches a terminal: no control characters, bounded length.
        let cause: String = self
            .0
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        f.write_str(&crate::tr!(
            "授权被拒绝：{} / Authorization denied: {}",
            cause,
            cause
        ))
    }
}

impl std::error::Error for AuthorizationDenied {}

/// 从回调请求行解析授权码并校验 state（防 CSRF/错会话）。
fn parse_callback_request(request_line: &str, expected_state: &str) -> Result<String> {
    let path = request_line.split_whitespace().nth(1).context(crate::tr!(
        "授权回调请求无效 / Invalid authorization callback request"
    ))?;
    let url = url::Url::parse(&format!("http://localhost{path}")).context(crate::tr!(
        "授权回调地址无效 / Invalid authorization callback URL"
    ))?;
    ensure!(
        url.path() == "/auth/callback",
        crate::tr!("授权回调路径不受支持 / Unsupported authorization callback path")
    );
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    // State first: anything without it is not an answer to this login, whatever it says.
    ensure!(
        param("state").as_deref() == Some(expected_state),
        crate::tr!("授权回调状态不匹配，请重试 / Authorization state mismatch; retry")
    );
    if let Some(error) = param("error") {
        let description = param("error_description").unwrap_or_default();
        return Err(AuthorizationDenied(format!("{error} {description}")).into());
    }
    param("code")
        .filter(|code| !code.is_empty())
        .context(crate::tr!(
            "授权回调缺少授权码 / Authorization callback is missing the code"
        ))
}

/// 打开系统浏览器；失败时用户可手动复制 URL。
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(url).spawn().is_ok();
    #[cfg(target_os = "linux")]
    let opened = std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .is_ok();
    #[cfg(target_os = "windows")]
    let opened = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn()
        .is_ok();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let opened = false;
    if !opened {
        println!("请手动在浏览器打开 / Open this URL in your browser:\n{url}");
    }
}

/// 授权码换取令牌（form 编码；对照 login/src/oauth/client.rs TokenEncoding::Form）。
fn exchange_code(code: &str, verifier: &str, port: u16) -> Result<CodexTokens> {
    let agent = crate::runtime::http_agent(Duration::from_secs(30));
    let mut response = agent
        .post(TOKEN_URL)
        .send_form([
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("redirect_uri", callback_url(port).as_str()),
            ("code_verifier", verifier),
        ])
        .map_err(|e| {
            let status = match e {
                ureq::Error::StatusCode(code) => format!(" (HTTP {code})"),
                _ => String::new(),
            };
            anyhow::anyhow!(
                "授权码交换失败，请重试登录{status} / Authorization code exchange failed; retry the login{status}"
            )
        })?;
    let value: Value = response.body_mut().read_json().context(crate::tr!(
        "无法解析令牌响应 / Cannot parse the token response"
    ))?;
    let access = value["access_token"].as_str().unwrap_or_default();
    let refresh = value["refresh_token"].as_str().unwrap_or_default();
    ensure!(
        !access.is_empty() && !refresh.is_empty(),
        crate::tr!("令牌响应不完整 / The token response is incomplete")
    );
    // account_id 来自 id_token 的 chatgpt_account_id claim（token_data.rs）
    let account_id = value["id_token"]
        .as_str()
        .and_then(jwt_payload)
        .and_then(|claims| {
            claims["https://api.openai.com/auth"]["chatgpt_account_id"]
                .as_str()
                .or_else(|| claims["chatgpt_account_id"].as_str())
                .map(str::to_string)
        });
    Ok(CodexTokens {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        account_id,
        obtained_at: now_secs(),
    })
}

fn callback_url(port: u16) -> String {
    format!("http://localhost:{port}/auth/callback")
}

/// The authorization page's address, with every value percent-encoded. Spaces are
/// `%20`, not the form encoding's `+`, as in the codex CLI's own request.
fn authorize_url(port: u16, challenge: &str, state: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", &callback_url(port)),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("scope", OAUTH_SCOPE),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "codex_cli_rs"),
        ])
        .finish()
        .replace('+', "%20");
    format!("{AUTHORIZE_URL}?{query}")
}

/// 浏览器授权全流程：本地回调服务器 + PKCE + 令牌交换。
fn browser_authorization(cancel: Option<&std::sync::atomic::AtomicBool>) -> Result<CodexTokens> {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = CALLBACK_PORTS
        .iter()
        .find_map(|port| TcpListener::bind(("127.0.0.1", *port)).ok())
        .context(crate::tr!("无法启动本地授权回调服务（端口 1455/1457 均被占用）/ Cannot start the local authorization callback (ports 1455/1457 are both in use)"))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;

    let verifier = crate::random_b64url(64)?;
    let state = crate::random_b64url(32)?;
    let url = authorize_url(port, &pkce_challenge(&verifier), &state);
    crate::println_tr!(
        "正在打开浏览器完成 ChatGPT 授权（{} 分钟内有效，Ctrl+C 取消）…",
        CALLBACK_TIMEOUT.as_secs() / 60
    );
    open_browser(&url);

    let deadline = std::time::Instant::now() + CALLBACK_TIMEOUT;
    let code = loop {
        if cancel.is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Relaxed)) {
            bail!(crate::tr!(
                "已取消浏览器授权 / Browser authorization cancelled"
            ));
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                let mut buf = [0u8; 8192];
                let size = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..size]);
                let line = request.lines().next().unwrap_or_default();
                let parsed = parse_callback_request(line, &state);
                let body = if parsed.is_ok() {
                    crate::tr!(
                        "Codex 登录成功，可以关闭此页。/ Login successful; you may close this page."
                    )
                } else {
                    crate::tr!(
                        "登录失败，请回终端重试。/ Login failed; return to the terminal and retry."
                    )
                };
                let body = format!(
                    "<html><body style=\"font-family:sans-serif;text-align:center;padding:4em\">{body}</body></html>"
                );
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                match parsed {
                    Ok(code) => break code,
                    // The refusal ends the wait; any other request (a stray tab, a
                    // favicon) was not an answer, so the real one can still arrive.
                    Err(error) if error.is::<AuthorizationDenied>() => return Err(error),
                    Err(_) => {}
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                ensure!(
                    std::time::Instant::now() < deadline,
                    crate::tr!(
                        "浏览器授权超时，请重试 / Browser authorization timed out; please retry"
                    )
                );
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                return Err(e).context(crate::tr!(
                    "授权回调服务出错 / Authorization callback error"
                ));
            }
        }
    };
    exchange_code(&code, &verifier, port)
}

/// 桌面端使用：导入 codex CLI 登录态并保存 course2md 自有副本，返回账号可用
/// 模型目录（目录拉取失败时回落默认模型并标记，见 [`DesktopCatalog`]）；导入失败不写任何文件。
pub fn import_for_desktop() -> Result<DesktopCatalog> {
    let mut tokens = import_from_codex_cli()?;
    if needs_refresh(&tokens) {
        tokens = refresh(&tokens)?;
    }
    save_tokens(&tokens)?;
    Ok(desktop_catalog(&tokens))
}

/// 桌面端使用：浏览器 PKCE 授权（无 codex CLI 时），成功后保存并返回模型目录。
/// 会打开系统浏览器并阻塞等待回调（最长 180 秒）；须在非 UI 线程调用。
pub fn authorize_for_desktop(
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<DesktopCatalog> {
    let tokens = browser_authorization(Some(&cancel))?;
    save_tokens(&tokens)?;
    Ok(desktop_catalog(&tokens))
}

/// 桌面端使用：用已保存的登录态重新拉取账号可用的模型目录（临期先刷新令牌）。
/// 与导入来源无关（codex CLI 导入或浏览器授权均可）；未登录时报错。
pub fn refresh_models_for_desktop() -> Result<Vec<(String, String)>> {
    let tokens = fresh_tokens()?;
    fetch_models(&tokens)
}

/// 只删除 course2md 自有的 Codex 凭据副本，不改写配置。返回是否确有凭据被删除。
pub fn remove_credentials() -> Result<bool> {
    match std::fs::remove_file(credential_path()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).context(crate::tr!(
            "清除 Codex 登录状态失败 / Could not remove Codex login"
        )),
    }
}

fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn confirm_browser(prompt: &str) -> Result<CodexTokens> {
    let browser = dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(true)
        .interact_opt()?
        .unwrap_or(false);
    if !browser {
        bail!(crate::tr!("已取消登录。/ Login cancelled."));
    }
    browser_authorization(None)
}

/// `course2md --login codex`：导入 codex CLI 登录态（失败时可改用浏览器授权），选模型并启用 AI 校对。
pub(super) fn login() -> Result<()> {
    let interactive = interactive();
    let mut tokens = match import_from_codex_cli() {
        Ok(tokens) => tokens,
        Err(import_err) if interactive => {
            println!("{import_err:#}");
            confirm_browser(crate::tr!(
                "改用浏览器授权登录（ChatGPT 账号）？/ Authorize with a ChatGPT account in the browser instead?"
            ))?
        }
        Err(import_err) => return Err(import_err),
    };
    // 导入即验证：过期令牌现场刷新（同时验证 refresh token 可用），成功后才落盘
    if needs_refresh(&tokens) {
        match refresh(&tokens) {
            Ok(refreshed) => tokens = refreshed,
            Err(e)
                if interactive
                    && e.chain()
                        .any(|c| c.downcast_ref::<CodexLoginRequired>().is_some()) =>
            {
                println!("{e:#}");
                tokens = confirm_browser(crate::tr!(
                    "codex CLI 的登录已失效。改用浏览器授权登录（ChatGPT 账号）？/ The codex CLI login expired. Authorize in the browser instead?"
                ))?;
            }
            Err(e) => return Err(e),
        }
    }
    save_tokens(&tokens)?;

    let mut cfg = crate::settings::load()?;
    cfg.llm.provider = LlmProvider::Codex;
    cfg.llm.base_url.clear();
    cfg.llm.api_key.clear();
    let catalog = desktop_catalog(&tokens);
    if catalog.catalog_is_fallback {
        crate::println_tr!(
            "无法获取账号可用的模型列表，已使用默认模型 {}。",
            DEFAULT_MODEL
        );
    }
    let catalog = catalog.models;
    cfg.llm.model = if !interactive {
        catalog
            .first()
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string())
    } else if let Some(labels) = (!catalog.is_empty()).then(|| {
        catalog
            .iter()
            .map(|(_, label)| label.as_str())
            .collect::<Vec<_>>()
    }) {
        let pick = dialoguer::Select::new()
            .with_prompt(crate::tr!("选择 Codex 模型 / Choose a Codex model"))
            .items(&labels)
            .default(0)
            .interact_opt()?
            .ok_or_else(|| {
                anyhow::anyhow!(crate::tr!(
                    "已取消登录，未保存配置。 / Login cancelled; no configuration saved."
                ))
            })?;
        catalog[pick].0.clone()
    } else {
        DEFAULT_MODEL.to_string()
    };
    cfg.llm.enabled = true;
    crate::llm::validate(&cfg.llm)?;
    crate::settings::save(&cfg)?;
    crate::println_tr!(
        "已导入 Codex 登录态并启用 AI 校对（模型 {}）。",
        cfg.llm.model
    );
    match crate::llm::test_connection(&cfg.llm) {
        Ok(()) => crate::println_tr!("连接测试通过 / Connection test passed."),
        Err(e) => bail!(
            "登录态已保存，但连接测试失败；可用 course2md llm setup 调整模型 / Login saved, but the connection test failed; adjust the model with course2md llm setup: {e:#}"
        ),
    }
    Ok(())
}

/// Codex 登录状态（桌面账号卡片用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginStatus {
    Disconnected,
    /// 已连接：展示用说明
    Connected,
    /// 凭据存在但无法再刷新，需要重新登录
    Expired,
}

/// 本地判断登录状态（不触网）。access token 过期不算失效：只要有 refresh token，
/// 下次请求会自动刷新；刷新被拒时请求路径会返回 [`CodexLoginRequired`]。
pub fn status() -> Result<LoginStatus> {
    Ok(match load_tokens()? {
        None => LoginStatus::Disconnected,
        Some(tokens) if tokens.refresh_token.is_empty() => LoginStatus::Expired,
        Some(_) => LoginStatus::Connected,
    })
}

/// `course2md --logout codex`：删除凭据；当前 AI 校对使用 Codex 时一并重置。
pub(super) fn logout() -> Result<()> {
    if remove_credentials()? {
        crate::println_tr!("已清除 Codex 登录状态。/ Codex login removed.");
    } else {
        crate::println_tr!("尚未登录 Codex。/ No Codex login saved.");
    }
    let mut cfg = crate::settings::load()?;
    if cfg.llm.provider == LlmProvider::Codex {
        cfg.llm = crate::llm::LlmSettings::default();
        crate::settings::save(&cfg)?;
        crate::println_tr!(
            "已关闭 AI 校对并重置 LLM 配置。/ AI proofreading disabled and LLM settings reset."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_exp_reads_payload_without_verifying() {
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"exp":1893456000,"sub":"u"}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(jwt_exp(&token), Some(1_893_456_000));
        assert_eq!(jwt_exp("not-a-jwt"), None);
        assert_eq!(jwt_exp("a.@@#.b"), None);
    }

    #[test]
    fn refresh_window_and_fallback() {
        let fresh = CodexTokens {
            access_token: String::new(),
            refresh_token: String::new(),
            account_id: None,
            obtained_at: now_secs(),
        };
        // JWT 无法解析 exp → 以 obtained_at + 8 天兜底
        assert!(!needs_refresh(&fresh));
        let stale = CodexTokens {
            obtained_at: now_secs() - REFRESH_FALLBACK_SECS - 1,
            ..fresh
        };
        assert!(needs_refresh(&stale));
    }

    #[test]
    fn debug_never_contains_tokens() {
        let tokens = CodexTokens {
            access_token: "secret-access".into(),
            refresh_token: "secret-refresh".into(),
            account_id: Some("acct".into()),
            obtained_at: 1,
        };
        let debug = format!("{tokens:?}");
        assert!(!debug.contains("secret-access") && !debug.contains("secret-refresh"));
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_refusal_cannot_write_control_characters_to_the_terminal() {
        let shown = AuthorizationDenied("bad\u{1b}[2Jnews\n".repeat(100)).to_string();
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert!(shown.chars().count() < 450);
    }

    #[test]
    fn authorize_url_encodes_every_value() {
        let raw = authorize_url(1455, "chal-lenge_", "st&ate=1");
        assert!(
            raw.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"),
            "{raw}"
        );
        assert!(raw.contains("scope=openid%20profile%20email"), "{raw}");
        assert!(!raw.contains('+'), "{raw}");
        let url = url::Url::parse(&raw).unwrap();
        let param = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .unwrap()
        };
        assert_eq!(param("redirect_uri"), "http://localhost:1455/auth/callback");
        assert_eq!(param("scope"), OAUTH_SCOPE);
        assert_eq!(param("state"), "st&ate=1");
        assert_eq!(param("client_id"), CLIENT_ID);
    }

    #[test]
    fn callback_parsing_validates_state_path_and_errors() {
        let good = "GET /auth/callback?code=abc123&state=st HTTP/1.1";
        assert_eq!(parse_callback_request(good, "st").unwrap(), "abc123");
        // state 不匹配（CSRF/错会话）
        assert!(parse_callback_request(good, "other").is_err());
        // 授权被拒绝：原因保留，且可与无关请求区分
        let denied =
            "GET /auth/callback?error=access_denied&error_description=no%20plan&state=st HTTP/1.1";
        let error = parse_callback_request(denied, "st").unwrap_err();
        assert!(error.is::<AuthorizationDenied>());
        assert!(
            error.to_string().contains("access_denied no plan"),
            "{error}"
        );
        // 没有正确 state 的拒绝不是对本次登录的回答
        let forged = "GET /auth/callback?error=access_denied&state=other HTTP/1.1";
        assert!(
            !parse_callback_request(forged, "st")
                .unwrap_err()
                .is::<AuthorizationDenied>()
        );
        // 路径不受支持
        let wrong_path = "GET /other?code=x&state=st HTTP/1.1";
        assert!(parse_callback_request(wrong_path, "st").is_err());
        // 缺 code
        let no_code = "GET /auth/callback?state=st HTTP/1.1";
        assert!(parse_callback_request(no_code, "st").is_err());
    }
}
