//! `course2md --login bilibili --method password|sms` and `--login youtube`: the
//! terminal side of the source accounts in [`crate::auth`].
use crate::auth::{
    geetest::{CAPTCHA_TIMEOUT, CaptchaPage, open_in_browser},
    passport::{Captcha, GeetestSolution, LoginOutcome, Passport, Password},
    youtube::{Browser, BrowserChoice},
};
use crate::{println_tr, tr};
use anyhow::{Context, Result, bail};
use std::sync::atomic::AtomicBool;

/// How to sign in to Bilibili.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum LoginMethod {
    /// 扫码（默认）/ Scan a QR code (default)
    #[default]
    Qr,
    /// 账号和密码 / Account and password
    Password,
    /// 手机短信验证码 / SMS code
    Sms,
}

pub fn login_bilibili(method: LoginMethod) -> Result<()> {
    match method {
        LoginMethod::Qr => crate::auth::login_bilibili(),
        LoginMethod::Password => {
            let account: String = dialoguer::Input::new()
                .with_prompt(tr!(
                    "账号（手机号、邮箱或用户名）/ Account (phone, email or username)"
                ))
                .interact_text()?;
            // dialoguer hands the text over once; it is zeroized from here on.
            let password = Password::new(
                dialoguer::Password::new()
                    .with_prompt(tr!("密码 / Password"))
                    .interact()?,
            );
            let mut passport = Passport::new();
            let captcha = passport.captcha()?;
            let solution = solve(&captcha)?;
            let outcome = passport.login_password(&account, password, &captcha, &solution)?;
            finish(passport, outcome)
        }
        LoginMethod::Sms => {
            let cid: String = dialoguer::Input::new()
                .with_prompt(tr!("国家或地区区号 / Country calling code"))
                .default("86".into())
                .interact_text()?;
            let tel: String = dialoguer::Input::new()
                .with_prompt(tr!("手机号 / Phone number"))
                .interact_text()?;
            let mut passport = Passport::new();
            let captcha = passport.captcha()?;
            let solution = solve(&captcha)?;
            let ticket = passport.send_sms(&cid, &tel, &captcha, &solution)?;
            println_tr!("验证码已发送。/ Code sent.");
            let code = code_prompt()?;
            let outcome = passport.login_sms(&ticket, &code)?;
            finish(passport, outcome)
        }
    }
}

/// Serves Geetest's check on this machine and waits for the person to pass it.
fn solve(captcha: &Captcha) -> Result<GeetestSolution> {
    let page = CaptchaPage::start(captcha)?;
    let url = page.url();
    println_tr!(
        "请在浏览器中完成人机验证（Ctrl+C 取消）：{} / Complete the human check in your browser (Ctrl+C to cancel): {}",
        url,
        url
    );
    open_in_browser(&url);
    page.wait(&AtomicBool::new(false), CAPTCHA_TIMEOUT)
}

fn code_prompt() -> Result<String> {
    Ok(dialoguer::Input::new()
        .with_prompt(tr!("短信验证码 / SMS code"))
        .interact_text()?)
}

/// Saves a session, after the phone check Bilibili may ask for first.
fn finish(mut passport: Passport, outcome: LoginOutcome) -> Result<()> {
    let login = match outcome {
        LoginOutcome::Authenticated(login) => login,
        LoginOutcome::VerifyPhone(mut check) => {
            let phone = passport.risk_phone(&mut check)?.unwrap_or_default();
            println_tr!(
                "Bilibili 要求验证账号绑定的手机 {}。/ Bilibili asks to verify the account's phone {}.",
                phone,
                phone
            );
            let captcha = passport.risk_captcha()?;
            let solution = solve(&captcha)?;
            passport.risk_send_sms(&mut check, &captcha, &solution)?;
            println_tr!("验证码已发送。/ Code sent.");
            let code = code_prompt()?;
            passport.risk_verify(&check, &code)?
        }
    };
    login.save()?;
    println_tr!(
        "Bilibili 登录成功。预览、字幕和视频下载将自动使用此登录状态。/ Bilibili login successful. Previews, subtitles and video downloads will use it automatically."
    );
    Ok(())
}

/// `--browser chrome[:profile]`, or a choice among the installed browsers.
pub fn login_youtube(browser: Option<&str>) -> Result<()> {
    let choice = match browser {
        Some(spec) => BrowserChoice::parse(spec).with_context(|| {
            tr!(
                "不支持的浏览器：{}。可用：{} / Unsupported browser: {}. Available: {}",
                spec,
                names(),
                spec,
                names()
            )
        })?,
        None => {
            let installed = Browser::installed();
            let browser = match installed.as_slice() {
                [] => bail!(tr!(
                    "没有找到可读取的浏览器。请用 --browser 指定，例如 --browser firefox / No browser found; name one with --browser, for example --browser firefox"
                )),
                [only] => *only,
                many => {
                    let labels: Vec<_> = many.iter().map(|browser| browser.name()).collect();
                    many[dialoguer::Select::new()
                        .with_prompt(tr!(
                            "选择已登录 YouTube 的浏览器 / Choose the browser where you are signed in to YouTube"
                        ))
                        .items(&labels)
                        .default(0)
                        .interact()?]
                }
            };
            BrowserChoice {
                browser,
                profile: None,
            }
        }
    };
    crate::auth::youtube::login_youtube(choice)
}

fn names() -> String {
    Browser::ALL
        .iter()
        .map(|browser| browser.id())
        .collect::<Vec<_>>()
        .join(", ")
}
