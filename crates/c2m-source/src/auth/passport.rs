//! Bilibili's web passport: account-and-password and SMS login, both behind a
//! Geetest check, and the phone verification Bilibili asks for when it judges a
//! login risky. The session it hands out is saved like a QR login's.
//!
//! The password exists only as a [`Password`]: it is zeroized when dropped, has
//! no `Debug` text, is never logged, and leaves this module only RSA-encrypted
//! with Bilibili's one-time key, in a request to Bilibili's HTTPS passport.
use super::{AccountProfile, Cookies, Response, USER_AGENT, VerifiedLogin};
use crate::tr;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::time::Duration;
/// Holds a password or a one-time code; zeroized when dropped.
pub use zeroize::Zeroizing;

/// A password typed by the person. Zeroized on drop; deliberately not `Debug`,
/// `Clone`, `Display` or serializable.
pub struct Password(Zeroizing<String>);

impl Password {
    /// Takes ownership of the typed text, so no other copy outlives this one.
    pub fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

/// A Geetest v3 check Bilibili issued: `token` ties it to the login request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captcha {
    pub token: String,
    pub gt: String,
    pub challenge: String,
}

/// What Geetest returns once the person has passed its check.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct GeetestSolution {
    pub challenge: String,
    pub validate: String,
    pub seccode: String,
}

/// A sent SMS code, to be entered with [`Passport::login_sms`].
#[derive(Clone, Debug)]
pub struct SmsTicket {
    captcha_key: String,
    cid: String,
    tel: String,
}

/// Bilibili accepted the credentials but wants the account's phone to confirm.
#[derive(Clone)]
pub struct RiskCheck {
    tmp_code: String,
    request_id: String,
    /// The bound phone as Bilibili masks it, e.g. `138*****000`.
    pub phone: Option<String>,
    captcha_key: Option<String>,
}

impl std::fmt::Debug for RiskCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RiskCheck")
            .field("phone", &self.phone)
            .finish_non_exhaustive()
    }
}

pub enum LoginOutcome {
    Authenticated(VerifiedLogin),
    /// Send a code to the bound phone ([`Passport::risk_captcha`],
    /// [`Passport::risk_send_sms`]) and confirm it ([`Passport::risk_verify`]).
    VerifyPhone(RiskCheck),
}

/// A refusal from the passport, in Bilibili's own words with a readable summary first.
#[derive(Debug, thiserror::Error)]
#[error("{}", self.text())]
pub struct PassportError {
    pub code: i64,
    /// Bilibili's own message, shown so the person sees the real reason.
    pub message: String,
    summary: String,
    known: bool,
}

impl PassportError {
    /// A known refusal reads as its summary; anything else keeps Bilibili's own
    /// words and code, so the real reason is never hidden behind a generic one.
    fn text(&self) -> String {
        let message = self.message.trim();
        if self.known || message.is_empty() {
            self.summary.clone()
        } else {
            format!("{}：{message}（{}）", self.summary, self.code)
        }
    }

    fn new(code: i64, message: &str) -> Self {
        let summary = match code {
            -629 | -2002 => Some(tr!("账号或密码错误")),
            -653 => Some(tr!("请填写账号和密码")),
            -105 | 2406 | -662 => Some(tr!("人机验证未通过或已过期，请重新验证")),
            2400 | 86000 => Some(tr!("登录密钥已过期，请重新登录")),
            86203 | 1004 => Some(tr!("短信发送次数已达上限，请稍后再试")),
            1002 | -400 => Some(tr!("手机号格式不正确")),
            1003 | 86206 => Some(tr!("验证码已发送，请稍后再试")),
            1006 | 86211 => Some(tr!("短信验证码不正确")),
            1007 | 86212 => Some(tr!("短信验证码已过期，请重新获取")),
            1025 => Some(tr!("该手机号无法用于登录 Bilibili")),
            // These keep Bilibili's words: they say why, the code does not.
            -2100 => None,
            -412 => Some(tr!("Bilibili 暂时限制了登录请求，请稍后再试")),
            _ => None,
        };
        let known = summary.is_some();
        let summary = summary
            .unwrap_or(if code == -2100 {
                tr!("Bilibili 要求验证手机号")
            } else {
                tr!("Bilibili 拒绝了登录请求")
            })
            .to_owned();
        Self {
            code,
            message: message.to_owned(),
            summary,
            known,
        }
    }

    /// The check this request rode on is spent; the person must pass a new one.
    pub fn needs_new_captcha(&self) -> bool {
        matches!(self.code, -105 | 2406 | -662 | 2400 | 86000)
    }
}

/// Where the passport lives; tests point it at a local server.
#[derive(Clone)]
pub(crate) struct Endpoints {
    pub(crate) passport: String,
    pub(crate) api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            passport: "https://passport.bilibili.com".into(),
            api: "https://api.bilibili.com".into(),
        }
    }
}

/// One login attempt's connection and cookie jar. Blocking; run it off the UI thread.
/// Not `Debug`: it holds session cookies once a login succeeds.
pub struct Passport {
    agent: ureq::Agent,
    endpoints: Endpoints,
    cookies: Cookies,
}

impl Passport {
    /// Starts an attempt with a device identity (`buvid3`/`buvid4`), which the
    /// passport's risk control expects from a browser. Missing identity is not
    /// fatal. Makes a request: call it off the UI thread.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::with(
            crate::bilibili_agent_builder()
                .timeout_global(Some(Duration::from_secs(15)))
                .build()
                .new_agent(),
            Endpoints::default(),
        )
    }

    pub(crate) fn with(agent: ureq::Agent, endpoints: Endpoints) -> Self {
        let mut passport = Self {
            agent,
            endpoints,
            cookies: Cookies::new(),
        };
        if let Err(error) = passport.device_identity() {
            tracing::debug!("buvid unavailable: {error:#}");
        }
        passport
    }

    fn device_identity(&mut self) -> Result<()> {
        let url = format!("{}/x/frontend/finger/spi", self.endpoints.api);
        let response = self.get(&url)?;
        let data = data(response)?;
        for (key, name) in [("b_3", "buvid3"), ("b_4", "buvid4")] {
            if let Some(value) = data[key].as_str() {
                super::insert_cookie(&mut self.cookies, name, value);
            }
        }
        Ok(())
    }

    fn get(&mut self, url: &str) -> Result<Response> {
        let response = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", "https://www.bilibili.com/")
            .header("Cookie", &super::cookie_header(&self.cookies))
            .call()
            .map_err(transport)?;
        super::read_cookies(&response, &mut self.cookies);
        Ok(response)
    }

    fn post(&mut self, url: &str, form: &[(&str, &str)]) -> Result<Response> {
        // Transport errors may echo the request; never let one carry the form.
        let response = self
            .agent
            .post(url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", "https://passport.bilibili.com/login")
            .header("Origin", "https://passport.bilibili.com")
            .header("Cookie", &super::cookie_header(&self.cookies))
            .send_form(form.iter().copied())
            .map_err(transport)?;
        super::read_cookies(&response, &mut self.cookies);
        Ok(response)
    }

    /// A fresh Geetest check for a password login or an SMS code.
    pub fn captcha(&mut self) -> Result<Captcha> {
        let url = format!(
            "{}/x/passport-login/captcha?source=main_web",
            self.endpoints.passport
        );
        let data = data(self.get(&url)?)?;
        let field = |value: &Value| {
            value
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        ensure!(
            data["type"].as_str().is_none_or(|kind| kind == "geetest"),
            tr!("Bilibili 要求了暂不支持的验证方式，请改用扫码登录")
        );
        Ok(Captcha {
            token: field(&data["token"]).context(incomplete())?,
            gt: field(&data["geetest"]["gt"]).context(incomplete())?,
            challenge: field(&data["geetest"]["challenge"]).context(incomplete())?,
        })
    }

    /// Account (phone, email or username) and password. The password is
    /// encrypted with the passport's one-time key and dropped (zeroized) here.
    pub fn login_password(
        &mut self,
        account: &str,
        password: Password,
        captcha: &Captcha,
        solution: &GeetestSolution,
    ) -> Result<LoginOutcome> {
        ensure!(
            !account.trim().is_empty() && !password.is_empty(),
            PassportError::new(-653, "")
        );
        let key = data(self.get(&format!(
            "{}/x/passport-login/web/key",
            self.endpoints.passport
        ))?)?;
        let encrypted = encrypt_password(
            key["hash"].as_str().context(incomplete())?,
            key["key"].as_str().context(incomplete())?,
            password,
        )?;
        let url = format!("{}/x/passport-login/web/login", self.endpoints.passport);
        let response = self.post(
            &url,
            &[
                ("username", account.trim()),
                ("password", &encrypted),
                ("keep", "0"),
                ("token", &captcha.token),
                ("challenge", &solution.challenge),
                ("validate", &solution.validate),
                ("seccode", &solution.seccode),
                ("go_url", "https://www.bilibili.com"),
                ("source", "main-fe-header"),
            ],
        )?;
        self.outcome(response)
    }

    /// Sends a login code to `tel` (`cid` is the country calling code, `86` for China).
    pub fn send_sms(
        &mut self,
        cid: &str,
        tel: &str,
        captcha: &Captcha,
        solution: &GeetestSolution,
    ) -> Result<SmsTicket> {
        let (cid, tel) = (digits(cid), digits(tel));
        ensure!(!tel.is_empty(), PassportError::new(1002, ""));
        let cid = if cid.is_empty() { "86".to_owned() } else { cid };
        let url = format!("{}/x/passport-login/web/sms/send", self.endpoints.passport);
        let response = self.post(
            &url,
            &[
                ("cid", &cid),
                ("tel", &tel),
                ("source", "main_web"),
                ("token", &captcha.token),
                ("challenge", &solution.challenge),
                ("validate", &solution.validate),
                ("seccode", &solution.seccode),
            ],
        )?;
        let data = data(response)?;
        Ok(SmsTicket {
            captcha_key: data["captcha_key"]
                .as_str()
                .filter(|key| !key.is_empty())
                .context(incomplete())?
                .to_owned(),
            cid,
            tel,
        })
    }

    /// `code` is the person's one-time code; keep it in a [`Zeroizing`] buffer.
    pub fn login_sms(&mut self, ticket: &SmsTicket, code: &str) -> Result<LoginOutcome> {
        let code = Zeroizing::new(digits(code));
        ensure!(!code.is_empty(), PassportError::new(1006, ""));
        let url = format!("{}/x/passport-login/web/login/sms", self.endpoints.passport);
        let response = self.post(
            &url,
            &[
                ("cid", &ticket.cid),
                ("tel", &ticket.tel),
                ("code", &code),
                ("source", "main_web"),
                ("captcha_key", &ticket.captcha_key),
                ("keep", "0"),
                ("go_url", "https://www.bilibili.com"),
            ],
        )?;
        self.outcome(response)
    }

    /// Reads a login reply: a session, a phone check, or Bilibili's refusal.
    fn outcome(&mut self, response: Response) -> Result<LoginOutcome> {
        let data = data(response)?;
        let url = data["url"].as_str().unwrap_or_default();
        let status = data["status"].as_i64().unwrap_or(0);
        if status != 0 || url.contains("tmp_token=") {
            if let Some(check) = risk_check(url) {
                return Ok(LoginOutcome::VerifyPhone(check));
            }
            bail!(PassportError::new(
                -2100,
                data["message"].as_str().unwrap_or_default()
            ));
        }
        self.authenticated(&data).map(LoginOutcome::Authenticated)
    }

    fn authenticated(&mut self, login: &Value) -> Result<VerifiedLogin> {
        let nav = format!("{}/x/web-interface/nav", self.endpoints.api);
        let agent = self.agent.clone();
        let profile: AccountProfile =
            super::finish_at(login, &mut self.cookies, &nav, |url, cookies| {
                super::request(&agent, url, cookies)
            })?;
        Ok(VerifiedLogin {
            cookies: std::mem::take(&mut self.cookies),
            profile,
        })
    }

    /// The bound phone, masked, so the person knows where the code goes.
    pub fn risk_phone(&mut self, check: &mut RiskCheck) -> Result<Option<String>> {
        let mut url = url::Url::parse(&format!(
            "{}/x/safecenter/user/info",
            self.endpoints.passport
        ))?;
        url.query_pairs_mut()
            .append_pair("tmp_code", &check.tmp_code);
        let data = data(self.get(url.as_str())?)?;
        let info = &data["account_info"];
        ensure!(
            info["bind_tel"].as_bool() != Some(false),
            tr!("Bilibili 要求验证手机号，但账号没有绑定手机。请改用扫码登录")
        );
        check.phone = info["hide_tel"]
            .as_str()
            .filter(|phone| !phone.is_empty())
            .map(str::to_owned);
        Ok(check.phone.clone())
    }

    /// The Geetest check that guards the verification SMS.
    pub fn risk_captcha(&mut self) -> Result<Captcha> {
        let url = format!("{}/x/safecenter/captcha/pre", self.endpoints.passport);
        let data = data(self.post(&url, &[("source", "risk")])?)?;
        let field = |key: &str| {
            data[key]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .context(incomplete())
        };
        Ok(Captcha {
            token: field("recaptcha_token")?,
            gt: field("gee_gt")?,
            challenge: field("gee_challenge")?,
        })
    }

    pub fn risk_send_sms(
        &mut self,
        check: &mut RiskCheck,
        captcha: &Captcha,
        solution: &GeetestSolution,
    ) -> Result<()> {
        let url = format!("{}/x/safecenter/common/sms/send", self.endpoints.passport);
        let tmp_code = check.tmp_code.clone();
        let data = data(self.post(
            &url,
            &[
                ("tmp_code", &tmp_code),
                ("sms_type", "loginTelCheck"),
                ("recaptcha_token", &captcha.token),
                ("gee_challenge", &solution.challenge),
                ("gee_validate", &solution.validate),
                ("gee_seccode", &solution.seccode),
            ],
        )?)?;
        check.captcha_key = Some(
            data["captcha_key"]
                .as_str()
                .filter(|key| !key.is_empty())
                .context(incomplete())?
                .to_owned(),
        );
        Ok(())
    }

    /// `code` is the person's one-time code; keep it in a [`Zeroizing`] buffer.
    /// Confirms the code sent to the bound phone and exchanges it for the session.
    pub fn risk_verify(&mut self, check: &RiskCheck, code: &str) -> Result<VerifiedLogin> {
        let code = Zeroizing::new(digits(code));
        ensure!(!code.is_empty(), PassportError::new(1006, ""));
        let captcha_key = check.captcha_key.clone().context(tr!("请先获取验证码"))?;
        let url = format!("{}/x/safecenter/login/tel/verify", self.endpoints.passport);
        let data = data(self.post(
            &url,
            &[
                ("tmp_code", &check.tmp_code),
                ("captcha_key", &captcha_key),
                ("type", "loginTelCheck"),
                ("code", &code),
                ("request_id", &check.request_id),
                ("source", "risk"),
            ],
        )?)?;
        let exchange = data["code"]
            .as_str()
            .filter(|code| !code.is_empty())
            .context(incomplete())?
            .to_owned();
        let url = format!(
            "{}/x/passport-login/web/exchange_result",
            self.endpoints.passport
        );
        let login = data_of(self.post(&url, &[("code", &exchange), ("source", "risk")])?)?;
        self.authenticated(&login)
    }
}

impl RiskCheck {
    /// A check with nothing behind it, for interface previews.
    #[doc(hidden)]
    pub fn preview(phone: &str) -> Self {
        Self {
            tmp_code: String::new(),
            request_id: String::new(),
            phone: Some(phone.to_owned()),
            captcha_key: None,
        }
    }
}

/// The phone check a risky login reply points at: `…/risk/…?tmp_token=…&request_id=…`.
fn risk_check(url: &str) -> Option<RiskCheck> {
    let url = url::Url::parse(url).ok()?;
    let query = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
    };
    Some(RiskCheck {
        tmp_code: query("tmp_token")?,
        request_id: query("request_id").unwrap_or_default(),
        phone: None,
        captcha_key: None,
    })
}

/// RSA-PKCS#1 v1.5 of `hash + password` under the passport's key, base64. The
/// joined plaintext lives in a zeroizing buffer; `password` is dropped (zeroized) here.
fn encrypt_password(hash: &str, pem: &str, password: Password) -> Result<String> {
    use base64::Engine as _;
    use rsa::pkcs8::DecodePublicKey as _;
    let key = rsa::RsaPublicKey::from_public_key_pem(pem.trim())
        .map_err(|_| anyhow::anyhow!(tr!("Bilibili 返回的登录密钥无效，请重试")))?;
    let mut plain = Zeroizing::new(Vec::with_capacity(hash.len() + password.0.len()));
    plain.extend_from_slice(hash.as_bytes());
    plain.extend_from_slice(password.0.as_bytes());
    drop(password);
    let sealed = key
        .encrypt(&mut rsa::rand_core::OsRng, rsa::Pkcs1v15Encrypt, &plain)
        .map_err(|_| anyhow::anyhow!(tr!("无法加密密码，请重试")))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(sealed))
}

fn digits(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).collect()
}

/// A failed exchange. Only the status is kept: a transport error may echo the
/// request, and the request may carry the encrypted password or a code.
fn transport(error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::StatusCode(status) => {
            PassportError::new(-i64::from(status), &format!("HTTP {status}")).into()
        }
        _ => anyhow::anyhow!(tr!(
            "Bilibili 登录请求失败，请检查网络后重试 / Bilibili login request failed; check the network and retry"
        )),
    }
}

fn incomplete() -> &'static str {
    tr!("Bilibili 登录响应不完整，请重试")
}

/// The reply's `data`, or the passport's refusal as a [`PassportError`].
fn data(response: Response) -> Result<Value> {
    let value = data_of(response)?;
    ensure!(value.is_object(), incomplete());
    Ok(value)
}

fn data_of(mut response: Response) -> Result<Value> {
    let value: Value = response.body_mut().read_json().map_err(|_| {
        anyhow::anyhow!(tr!(
            "无法解析 Bilibili 登录响应 / Cannot parse Bilibili login response"
        ))
    })?;
    let code = value["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        bail!(PassportError::new(
            code,
            value["message"].as_str().unwrap_or_default()
        ));
    }
    Ok(value["data"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use c2m_test_http::{json_response, serve};

    const KEY: &str = "-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDMpvIbDfLJEo3GDIrh8dyPKGeX\n5kdTi/EV7bp0MRV0j9BuTEyVyxpOx2aiZXqNQIuu4O0p8XvgpCJDgwHKnHPcNMPn\nVpIO9iBM1zSQRHuNUBrwM3pLw66Qeg+yjO1wjcecWgIGVQyrwQ46r7OYgg6yQLAn\nuLd+lf6H6hdLieQt7QIDAQAB\n-----END PUBLIC KEY-----\n";

    fn local(base: &str) -> Passport {
        Passport::with(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(5)))
                .build()
                .new_agent(),
            Endpoints {
                passport: base.into(),
                api: base.into(),
            },
        )
    }

    fn solution() -> GeetestSolution {
        GeetestSolution {
            challenge: "ch".into(),
            validate: "va".into(),
            seccode: "va|jordan".into(),
        }
    }

    fn captcha() -> Captcha {
        Captcha {
            token: "tok".into(),
            gt: "gt".into(),
            challenge: "ch".into(),
        }
    }

    fn form(body: &[u8]) -> std::collections::BTreeMap<String, String> {
        url::form_urlencoded::parse(body).into_owned().collect()
    }

    fn with_cookies(body: &str, cookies: &[&str]) -> String {
        let mut head =
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n".to_owned();
        for cookie in cookies {
            head.push_str(&format!("Set-Cookie: {cookie}\r\n"));
        }
        format!("{head}Content-Length: {}\r\n\r\n{body}", body.len())
    }

    #[test]
    fn password_is_encrypted_with_the_one_time_key_and_never_sent_plain() {
        let sealed = encrypt_password("salt", KEY, Password::new("hunter2-secret".into())).unwrap();
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&sealed)
            .unwrap();
        assert_eq!(bytes.len(), 128, "RSA-1024 block");
        // PKCS#1 v1.5 is randomized: the same input never repeats on the wire.
        assert_ne!(
            sealed,
            encrypt_password("salt", KEY, Password::new("hunter2-secret".into())).unwrap()
        );
        assert!(encrypt_password("salt", "not a key", Password::new("x".into())).is_err());
    }

    #[test]
    fn password_login_sends_the_expected_form_and_saves_the_session() {
        let (base, requests) = serve(vec![
            json_response(r#"{"code":0,"data":{"b_3":"B3-fixture","b_4":"B4-fixture"}}"#),
            json_response(&format!(
                r#"{{"code":0,"data":{{"hash":"salt","key":{}}}}}"#,
                serde_json::to_string(KEY).unwrap()
            )),
            with_cookies(
                r#"{"code":0,"data":{"status":0,"message":"","url":"https://www.bilibili.com"}}"#,
                &[
                    "SESSDATA=sess%2C1; Path=/; Domain=bilibili.com; HttpOnly",
                    "bili_jct=csrf; Path=/",
                    "DedeUserID=42; Path=/",
                    "DedeUserID__ckMd5=md5; Path=/",
                ],
            ),
            json_response(r#"{"code":0,"data":{"isLogin":true,"uname":"测试用户"}}"#),
        ]);
        let mut passport = local(&base);
        let LoginOutcome::Authenticated(login) = passport
            .login_password(
                " someone@example.com ",
                Password::new("hunter2-secret".into()),
                &captcha(),
                &solution(),
            )
            .unwrap()
        else {
            panic!("expected a session");
        };
        assert_eq!(login.profile.name, "测试用户");
        assert_eq!(login.cookies["SESSDATA"], "sess%2C1");
        assert_eq!(login.cookies["buvid3"], "B3-fixture");
        let requests = requests.join().unwrap();
        assert!(requests[0].head.starts_with("GET /x/frontend/finger/spi"));
        assert!(
            requests[1]
                .head
                .starts_with("GET /x/passport-login/web/key")
        );
        let login = &requests[2];
        assert!(login.head.starts_with("POST /x/passport-login/web/login "));
        assert!(login.head.contains("buvid3=B3-fixture"));
        let sent = form(&login.body);
        assert_eq!(sent["username"], "someone@example.com");
        assert_eq!(sent["token"], "tok");
        assert_eq!(sent["challenge"], "ch");
        assert_eq!(sent["validate"], "va");
        assert_eq!(sent["seccode"], "va|jordan");
        assert_ne!(sent["password"], "hunter2-secret");
        for request in &requests {
            let text = format!("{}{}", request.head, String::from_utf8_lossy(&request.body));
            assert!(!text.contains("hunter2"), "plain password on the wire");
        }
        assert!(requests[3].head.contains("SESSDATA=sess%2C1"));
    }

    #[test]
    fn a_risky_login_asks_for_the_bound_phone_then_exchanges_the_code() {
        let risk = "https://passport.bilibili.com/h5-app/passport/risk/verify?tmp_token=TMP&request_id=REQ&source=risk";
        let (base, requests) = serve(vec![
            json_response(r#"{"code":0,"data":{}}"#),
            json_response(&format!(
                r#"{{"code":0,"data":{{"hash":"salt","key":{}}}}}"#,
                serde_json::to_string(KEY).unwrap()
            )),
            json_response(&format!(
                r#"{{"code":0,"data":{{"status":2,"message":"本次登录环境存在风险","url":"{risk}"}}}}"#
            )),
            json_response(
                r#"{"code":0,"data":{"account_info":{"hide_tel":"138*****000","bind_tel":true}}}"#,
            ),
            json_response(
                r#"{"code":0,"data":{"recaptcha_token":"RT","gee_gt":"GT","gee_challenge":"GC"}}"#,
            ),
            json_response(r#"{"code":0,"data":{"captcha_key":"CK"}}"#),
            json_response(r#"{"code":0,"data":{"code":"EXCHANGE"}}"#),
            with_cookies(
                r#"{"code":0,"data":{"url":"https://www.bilibili.com"}}"#,
                &["SESSDATA=s", "bili_jct=j", "DedeUserID=1"],
            ),
            json_response(r#"{"code":0,"data":{"isLogin":true,"uname":"u"}}"#),
        ]);
        let mut passport = local(&base);
        let LoginOutcome::VerifyPhone(mut check) = passport
            .login_password("a", Password::new("b".into()), &captcha(), &solution())
            .unwrap()
        else {
            panic!("expected a phone check");
        };
        assert_eq!(
            passport.risk_phone(&mut check).unwrap().as_deref(),
            Some("138*****000")
        );
        let risk_captcha = passport.risk_captcha().unwrap();
        assert_eq!(risk_captcha.gt, "GT");
        passport
            .risk_send_sms(&mut check, &risk_captcha, &solution())
            .unwrap();
        let login = passport.risk_verify(&check, " 123 456 ").unwrap();
        assert_eq!(login.profile.name, "u");
        let requests = requests.join().unwrap();
        assert!(requests[3].head.contains("tmp_code=TMP"));
        let send = form(&requests[5].body);
        assert_eq!(send["tmp_code"], "TMP");
        assert_eq!(send["sms_type"], "loginTelCheck");
        assert_eq!(send["recaptcha_token"], "RT");
        assert_eq!(send["gee_seccode"], "va|jordan");
        let verify = form(&requests[6].body);
        assert_eq!(verify["code"], "123456");
        assert_eq!(verify["captcha_key"], "CK");
        assert_eq!(verify["request_id"], "REQ");
        assert_eq!(form(&requests[7].body)["code"], "EXCHANGE");
    }

    #[test]
    fn sms_login_sends_the_code_with_its_key() {
        let (base, requests) = serve(vec![
            json_response(r#"{"code":0,"data":{}}"#),
            json_response(r#"{"code":0,"data":{"captcha_key":"KEY"}}"#),
            json_response(r#"{"code":1006,"message":"请输入正确的短信验证码","data":{}}"#),
        ]);
        let mut passport = local(&base);
        let ticket = passport
            .send_sms("+86", "138 0000 0000", &captcha(), &solution())
            .unwrap();
        let error = match passport.login_sms(&ticket, "000000") {
            Err(error) => error,
            Ok(_) => panic!("a wrong code must fail"),
        };
        let refusal = error.downcast_ref::<PassportError>().unwrap();
        assert_eq!(refusal.code, 1006);
        assert_eq!(error.to_string(), "短信验证码不正确");
        assert_eq!(refusal.message, "请输入正确的短信验证码");
        let requests = requests.join().unwrap();
        let send = form(&requests[1].body);
        assert_eq!(send["cid"], "86");
        assert_eq!(send["tel"], "13800000000");
        assert_eq!(send["source"], "main_web");
        let login = form(&requests[2].body);
        assert_eq!(login["captcha_key"], "KEY");
        assert_eq!(login["code"], "000000");
    }

    #[test]
    fn refusals_keep_bilibilis_words_and_say_when_to_verify_again() {
        let error = PassportError::new(-629, "账号或者密码错误");
        assert_eq!(error.to_string(), "账号或密码错误");
        let unknown = PassportError::new(-3, "本次登录环境存在风险");
        assert_eq!(
            unknown.to_string(),
            "Bilibili 拒绝了登录请求：本次登录环境存在风险（-3）"
        );
        assert!(!error.needs_new_captcha());
        assert!(PassportError::new(-105, "验证码错误").needs_new_captcha());
        assert!(
            PassportError::new(99999, "")
                .to_string()
                .starts_with("Bilibili 拒绝了登录请求")
        );
        assert!(risk_check("https://www.bilibili.com").is_none());
    }

    #[test]
    #[ignore = "requires Bilibili network access; reads a captcha, submits nothing"]
    fn live_captcha_has_the_expected_shape() {
        let captcha = Passport::new().captcha().unwrap();
        assert_eq!(captcha.gt.len(), 32);
        assert!(!captcha.challenge.is_empty() && !captcha.token.is_empty());
    }
}
