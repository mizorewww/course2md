//! Bilibili's password and SMS logins. The QR login and the shared dialog frame
//! are in `account_ui`.
//!
//! A typed password is read out of its field once, cleared from the field, and
//! handed to the engine as a zeroizing `Password`; it is never logged, stored or
//! shown. Every reply is checked against the attempt that asked for it, so a
//! closed dialog or a switched method ignores late answers.
use crate::{Desktop, symbols};
use course2md::auth::{
    geetest::{CAPTCHA_TIMEOUT, CaptchaPage},
    passport::{Captcha, GeetestSolution, LoginOutcome, Passport, Password, Zeroizing},
};
use desktop_accounts::account::*;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit::component::input::InputState;
use gpui_kit_material::{color::ColorRole, icons::Icon, prelude::*};

/// The fields of the password and SMS forms.
pub(crate) struct LoginInputs {
    account: Entity<InputState>,
    password: Entity<InputState>,
    cid: Entity<InputState>,
    phone: Entity<InputState>,
    code: Entity<InputState>,
}

impl LoginInputs {
    pub(crate) fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            account: cx.new(|cx| InputState::new(window, cx)),
            password: cx.new(|cx| InputState::new(window, cx).masked(true)),
            cid: cx.new(|cx| InputState::new(window, cx).default_value("86")),
            phone: cx.new(|cx| InputState::new(window, cx)),
            code: cx.new(|cx| InputState::new(window, cx)),
        }
    }
}

impl Desktop {
    fn login_field(&self, pick: fn(&LoginInputs) -> &Entity<InputState>, cx: &App) -> String {
        self.login_inputs
            .as_ref()
            .map(|inputs| pick(inputs).read(cx).value().trim().to_owned())
            .unwrap_or_default()
    }

    /// A one-time code as typed, in a buffer zeroized when the login drops it.
    fn login_code(&self, cx: &App) -> Zeroizing<String> {
        Zeroizing::new(self.login_field(|inputs| &inputs.code, cx))
    }

    fn clear_login_field(
        &self,
        pick: fn(&LoginInputs) -> &Entity<InputState>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(inputs) = &self.login_inputs {
            pick(inputs).update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Empties the password and code fields, so nothing typed outlives the attempt
    /// it was typed for. `set_value` needs the window, which a close does not have,
    /// so this returns to it through `defer` + `update_window`.
    pub(crate) fn clear_login_secrets(&self, cx: &mut Context<Self>) {
        let Some(inputs) = &self.login_inputs else {
            return;
        };
        let fields = [inputs.password.clone(), inputs.code.clone()];
        let handle = self.window;
        cx.defer(move |cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                for field in &fields {
                    field.update(cx, |input, cx| input.set_value("", window, cx));
                }
            });
        });
    }

    pub(crate) fn ensure_login_inputs(&mut self, window: &mut Window, cx: &mut App) {
        if self.login_inputs.is_none() {
            self.login_inputs = Some(LoginInputs::new(window, cx));
        }
    }

    /// Switches the Bilibili dialog's method; the QR code only runs while shown.
    pub(crate) fn choose_login_method(
        &mut self,
        method: LoginMethod,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.account.method == method {
            return;
        }
        self.account.choose_method(method);
        self.clear_login_secrets(cx);
        self.ensure_login_inputs(window, cx);
        if method == LoginMethod::Qr {
            self.start_account_qr(cx);
        } else {
            // The dialog stays open on a neutral state that is not a QR session.
            self.account.dialog = Some(QrDialogState::Generating);
        }
        cx.notify();
    }

    /// Fetches a human check, serves it to the browser, and once it is passed runs
    /// `step` with the same connection. `captcha` chooses which check to fetch.
    fn with_captcha<R: Send + 'static>(
        &mut self,
        captcha: fn(&mut Passport) -> anyhow::Result<Captcha>,
        step: impl FnOnce(&mut Passport, &Captcha, &GeetestSolution) -> anyhow::Result<R>
        + Send
        + 'static,
        done: impl FnOnce(&mut Self, anyhow::Result<R>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let generation = self.account.generation;
        let cancel = self.account.form.watch();
        self.account.form.step = FormStep::Captcha { url: None };
        self.account.form.error = None;
        let passport = self.account.passport.take();
        let (url_tx, url_rx) = smol::channel::bounded::<String>(1);
        let task = crate::spawn_blocking_io(move || {
            let mut passport = passport.unwrap_or_else(Passport::new);
            let result = (|| {
                let captcha = captcha(&mut passport)?;
                let page = CaptchaPage::start(&captcha)?;
                let _ = url_tx.send_blocking(page.url());
                let solution = page.wait(&cancel, CAPTCHA_TIMEOUT)?;
                drop(page);
                step(&mut passport, &captcha, &solution)
            })();
            (passport, result)
        });
        cx.spawn(async move |this, cx| {
            if let Ok(url) = url_rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if this.account.current(generation) && this.account.form.busy() {
                        cx.open_url(&url);
                        this.account.form.step = FormStep::Captcha { url: Some(url) };
                        cx.notify();
                    }
                });
            }
            let received = task.recv().await;
            let _ = this.update(cx, |this, cx| {
                if !this.account.current(generation) {
                    return;
                }
                let result = match received {
                    Ok((passport, result)) => {
                        this.account.passport = Some(passport);
                        result
                    }
                    Err(_) => Err(anyhow::anyhow!(crate::interrupted())),
                };
                this.account.form.step = FormStep::Entry;
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Runs one request with the attempt's connection, off the UI thread.
    fn with_passport<R: Send + 'static>(
        &mut self,
        step: impl FnOnce(&mut Passport) -> anyhow::Result<R> + Send + 'static,
        done: impl FnOnce(&mut Self, anyhow::Result<R>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let generation = self.account.generation;
        self.account.form.step = FormStep::Working;
        self.account.form.error = None;
        let passport = self.account.passport.take();
        let task = crate::spawn_blocking_io(move || {
            let mut passport = passport.unwrap_or_else(Passport::new);
            let result = step(&mut passport);
            (passport, result)
        });
        cx.spawn(async move |this, cx| {
            let received = task.recv().await;
            let _ = this.update(cx, |this, cx| {
                if !this.account.current(generation) {
                    return;
                }
                let result = match received {
                    Ok((passport, result)) => {
                        this.account.passport = Some(passport);
                        result
                    }
                    Err(_) => Err(anyhow::anyhow!(crate::interrupted())),
                };
                this.account.form.step = FormStep::Entry;
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn login_failed(&mut self, error: anyhow::Error) {
        // A spent check means the next try starts with a new connection too.
        if error
            .downcast_ref::<course2md::auth::passport::PassportError>()
            .is_some_and(|refusal| refusal.needs_new_captcha())
        {
            self.account.passport = None;
        }
        self.account.form.error = Some(format!("{error:#}"));
    }

    /// The login reply: a session to save, or the phone check Bilibili asks for.
    fn login_reply(&mut self, result: anyhow::Result<LoginOutcome>, cx: &mut Context<Self>) {
        match result {
            Ok(LoginOutcome::Authenticated(login)) => {
                let generation = self.account.generation;
                self.account.form.step = FormStep::Working;
                let task = crate::spawn_blocking_io(move || login.save());
                cx.spawn(async move |this, cx| {
                    let saved = task.recv().await;
                    let _ = this.update(cx, |this, cx| {
                        if !this.account.current(generation) {
                            return;
                        }
                        match saved {
                            Ok(Ok(profile)) => this.account_signed_in(profile),
                            _ => {
                                this.account.form.step = FormStep::Entry;
                                this.account.form.error = Some(
                                    course2md::tr!(
                                        "账号已确认，但保存登录状态失败。请检查本地权限后重试。"
                                    )
                                    .into(),
                                );
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            Ok(LoginOutcome::VerifyPhone(check)) => {
                self.account.form.code_sent = false;
                self.account.risk = Some(check);
            }
            Err(error) => self.login_failed(error),
        }
    }

    /// A saved session from any method: the row and the dialog show it.
    pub(crate) fn account_signed_in(&mut self, profile: course2md::auth::AccountProfile) {
        self.account.status_generation = self.account.status_generation.wrapping_add(1);
        self.account.checking = false;
        self.account.has_saved_login = true;
        self.account.status = Some(course2md::auth::AccountStatus::Connected(profile.clone()));
        self.account.status_error = None;
        self.account.logout_failed = false;
        self.account.reset_form();
        self.account.dialog = Some(QrDialogState::Success(profile));
    }

    fn submit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let account = self.login_field(|inputs| &inputs.account, cx);
        let password = self
            .login_inputs
            .as_ref()
            .map(|inputs| Password::new(inputs.password.read(cx).value().to_string()))
            .filter(|password| !password.is_empty());
        let Some(password) = password.filter(|_| !account.is_empty()) else {
            self.account.form.error = Some(course2md::tr!("请填写账号和密码").into());
            cx.notify();
            return;
        };
        self.clear_login_field(|inputs| &inputs.password, window, cx);
        self.with_captcha(
            |passport| passport.captcha(),
            move |passport, captcha, solution| {
                let mut outcome = passport.login_password(&account, password, captcha, solution)?;
                if let LoginOutcome::VerifyPhone(check) = &mut outcome {
                    passport.risk_phone(check)?;
                }
                Ok(outcome)
            },
            |this, result, cx| this.login_reply(result, cx),
            cx,
        );
    }

    fn send_login_code(&mut self, cx: &mut Context<Self>) {
        if let Some(mut check) = self.account.risk.clone() {
            self.with_captcha(
                |passport| passport.risk_captcha(),
                move |passport, captcha, solution| {
                    passport.risk_send_sms(&mut check, captcha, solution)?;
                    Ok(check)
                },
                move |this, result: anyhow::Result<_>, _| match result {
                    Ok(check) => {
                        this.account.risk = Some(check);
                        this.account.form.code_sent = true;
                    }
                    Err(error) => this.login_failed(error),
                },
                cx,
            );
            return;
        }
        let cid = self.login_field(|inputs| &inputs.cid, cx);
        let phone = self.login_field(|inputs| &inputs.phone, cx);
        if phone.is_empty() {
            self.account.form.error = Some(course2md::tr!("手机号格式不正确").into());
            cx.notify();
            return;
        }
        self.with_captcha(
            |passport| passport.captcha(),
            move |passport, captcha, solution| passport.send_sms(&cid, &phone, captcha, solution),
            |this, result, _| match result {
                Ok(ticket) => {
                    this.account.sms = Some(ticket);
                    this.account.form.code_sent = true;
                }
                Err(error) => this.login_failed(error),
            },
            cx,
        );
    }

    fn submit_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code = self.login_code(cx);
        if code.is_empty() {
            self.account.form.error = Some(course2md::tr!("短信验证码不正确").into());
            cx.notify();
            return;
        }
        self.clear_login_field(|inputs| &inputs.code, window, cx);
        if let Some(check) = self.account.risk.clone() {
            self.with_passport(
                move |passport| passport.risk_verify(&check, &code),
                |this, result, cx| match result {
                    Ok(login) => this.login_reply(Ok(LoginOutcome::Authenticated(login)), cx),
                    Err(error) => this.login_failed(error),
                },
                cx,
            );
            return;
        }
        let Some(ticket) = self.account.sms.clone() else {
            self.account.form.error = Some(course2md::tr!("请先获取验证码").into());
            cx.notify();
            return;
        };
        self.with_passport(
            move |passport| {
                let mut outcome = passport.login_sms(&ticket, &code)?;
                if let LoginOutcome::VerifyPhone(check) = &mut outcome {
                    passport.risk_phone(check)?;
                }
                Ok(outcome)
            },
            |this, result, cx| this.login_reply(result, cx),
            cx,
        );
    }

    /// The method choice and the chosen method's form; the QR content is `account_ui`'s.
    pub(crate) fn login_method_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut group =
            ConnectedButtonGroup::new("bilibili-login-method", course2md::tr!("登录方式"))
                .tone(ButtonTone::Neutral)
                .full_width()
                .options(LoginMethod::ALL.map(|method| (method.id(), method.label())))
                .icon(LoginMethod::Qr.id(), symbols::qr_code_2())
                .icon(LoginMethod::Password.id(), symbols::password())
                .icon(LoginMethod::Sms.id(), symbols::sms())
                .selected(self.account.method.id())
                .disabled(self.account.form.busy())
                .on_change(cx.listener(|this, value: &SharedString, window, cx| {
                    if let Some(method) = LoginMethod::from_id(value) {
                        this.choose_login_method(method, window, cx);
                    }
                }));
        group = group.equal_widths(true);
        group.into_any_element()
    }

    pub(crate) fn login_form(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let Some(inputs) = &self.login_inputs else {
            return div();
        };
        let form = &self.account.form;
        let busy = form.busy();
        let field = |id: &'static str, input: &Entity<InputState>, label: &'static str| {
            TextField::new(id, input)
                .outlined()
                .shape(TextFieldShape::Rounded)
                .w_full()
                .label(label)
                .float_label(true)
                .disabled(busy)
        };
        let mut body = v_flex().w_full().min_w_0().px(rem(24.)).gap(rem(16.));
        let code_row = |sent: bool, cx: &mut Context<Self>| {
            h_flex()
                .w_full()
                .min_w_0()
                .gap(rem(12.))
                .items_center()
                .child(
                    div().flex_1().min_w_0().child(
                        field("login-code", &inputs.code, course2md::tr!("短信验证码"))
                            .input_type(TextFieldInputType::Number)
                            .disabled(busy || !sent),
                    ),
                )
                .child(
                    material_button("login-send-code", ButtonVariant::Tonal)
                        .icon(symbols::sms())
                        .child(if sent {
                            course2md::tr!("重新获取")
                        } else {
                            course2md::tr!("获取验证码")
                        })
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.send_login_code(cx))),
                )
        };
        match (&self.account.risk, self.account.method) {
            (Some(check), _) => {
                body = body
                    .child(
                        TileScale::Regular
                            .support(
                                "bilibili-phone-check",
                                course2md::tr!(
                                    "Bilibili 认为这次登录有风险，需要用账号绑定的手机确认。"
                                ),
                            )
                            .whitespace_normal(),
                    )
                    .when_some(check.phone.clone(), |body, phone| {
                        body.child(
                            h_flex()
                                .gap(rem(12.))
                                .items_center()
                                .child(
                                    Icon::from(symbols::phone_iphone())
                                        .size(rem(20.))
                                        .color(color(ColorRole::ON_SURFACE_VARIANT)),
                                )
                                .child(TileScale::Regular.label("bilibili-phone", phone)),
                        )
                    })
                    .child(code_row(form.code_sent, cx));
            }
            (_, LoginMethod::Password) => {
                body = body
                    .child(field(
                        "login-account",
                        &inputs.account,
                        course2md::tr!("手机号、邮箱或用户名"),
                    ))
                    .child(
                        field("login-password", &inputs.password, course2md::tr!("密码"))
                            .input_type(TextFieldInputType::Password),
                    )
                    .child(supporting_info(
                        "login-password-note",
                        course2md::tr!(
                            "密码只发送给 Bilibili 用于这次登录，不会保存。登录前会在浏览器中打开人机验证。"
                        ),
                    ));
            }
            _ => {
                let sent = form.code_sent;
                body = body
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap(rem(12.))
                            .child(
                                div().w(rem(104.)).flex_shrink_0().child(
                                    field("login-cid", &inputs.cid, course2md::tr!("区号"))
                                        .prefix("+")
                                        .input_type(TextFieldInputType::Tel),
                                ),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    field("login-phone", &inputs.phone, course2md::tr!("手机号"))
                                        .input_type(TextFieldInputType::Tel),
                                ),
                            ),
                    )
                    .child(code_row(sent, cx))
                    .child(supporting_info(
                        "login-sms-note",
                        course2md::tr!("获取验证码前会在浏览器中打开人机验证。"),
                    ));
            }
        }
        if let FormStep::Captcha { url } = &form.step {
            let reopen = url.clone();
            body = body.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(12.))
                    .items_center()
                    .flex_wrap()
                    .child(spinner(
                        "login-captcha-waiting",
                        course2md::tr!("请在浏览器中完成人机验证"),
                        cx,
                    ))
                    .child(
                        TileScale::Regular
                            .support(
                                "login-captcha-status",
                                course2md::tr!("请在浏览器中完成人机验证"),
                            )
                            .flex_1()
                            .min_w_0(),
                    )
                    .when_some(reopen, |row, url| {
                        row.child(
                            material_button("login-captcha-reopen", ButtonVariant::Text)
                                .tone(ButtonTone::Neutral)
                                .icon(symbols::open_in_new())
                                .child(course2md::tr!("重新打开"))
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            );
        } else if form.step == FormStep::Working {
            body = body.child(
                h_flex()
                    .gap(rem(12.))
                    .items_center()
                    .child(spinner("login-working", course2md::tr!("正在登录"), cx))
                    .child(
                        TileScale::Regular
                            .support("login-working-text", course2md::tr!("正在登录")),
                    ),
            );
        }
        let _ = window;
        body.when_some(form.error.clone(), |body, error| {
            body.child(notice_banner("login-error", NoticeKind::Danger, error))
        })
    }

    /// The Bilibili dialog's main action for the password and SMS methods.
    pub(crate) fn login_form_action(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let form = &self.account.form;
        let (label, icon, code) = match (&self.account.risk, self.account.method) {
            (Some(_), _) => (course2md::tr!("验证并登录"), symbols::login(), true),
            (_, LoginMethod::Password) => (course2md::tr!("登录"), symbols::login(), false),
            (_, LoginMethod::Sms) => (course2md::tr!("登录"), symbols::login(), true),
            (_, LoginMethod::Qr) => return None,
        };
        let ready = !form.busy() && (!code || form.code_sent);
        Some(
            material_button("login-submit", ButtonVariant::Filled)
                .icon(icon)
                .child(label)
                .disabled(!ready)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if code {
                        this.submit_code(window, cx);
                    } else {
                        this.submit_password(window, cx);
                    }
                }))
                .into_any_element(),
        )
    }
}

/// Capture builds: the login dialogs in each state, without a network request.
#[cfg(feature = "capture")]
impl Desktop {
    pub(crate) fn capture_login_state(
        &mut self,
        state: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (method, step) = state.split_once('-').unwrap_or((state, ""));
        let method = match method {
            "password" => LoginMethod::Password,
            "sms" | "phone" => LoginMethod::Sms,
            "success" => {
                self.account.dialog =
                    Some(QrDialogState::Success(course2md::auth::AccountProfile {
                        name: "course2md 示例账号".into(),
                    }));
                cx.notify();
                return;
            }
            _ => return,
        };
        self.choose_login_method(method, window, cx);
        if let Some(inputs) = &self.login_inputs {
            // After the switch's own clearing of the password, which is deferred.
            let fields = [
                (inputs.account.clone(), "someone@example.com"),
                (inputs.password.clone(), "preview-only"),
                (inputs.phone.clone(), "13800000000"),
            ];
            let handle = self.window;
            cx.defer(move |cx| {
                let _ = cx.update_window(handle, |_, window, cx| {
                    for (input, value) in &fields {
                        input.update(cx, |input, cx| input.set_value(*value, window, cx));
                    }
                });
            });
        }
        let form = &mut self.account.form;
        match step {
            "captcha" => {
                form.step = FormStep::Captcha {
                    url: Some("http://127.0.0.1:1/preview/".into()),
                }
            }
            "working" => form.step = FormStep::Working,
            "error" => {
                // As the passport's refusals read: a known code is its summary.
                form.error = Some(
                    if method == LoginMethod::Password {
                        course2md::tr!("账号或密码错误")
                    } else {
                        course2md::tr!("短信验证码不正确")
                    }
                    .into(),
                )
            }
            "sent" => form.code_sent = true,
            _ => {}
        }
        if state.starts_with("phone") {
            self.account.risk = Some(course2md::auth::passport::RiskCheck::preview("138*****000"));
            self.account.form.code_sent = step == "sent";
        }
        cx.notify();
    }
}
