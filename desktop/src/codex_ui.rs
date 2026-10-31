//! Codex (ChatGPT subscription) account connection: import the codex CLI login or authorize with PKCE in the browser.
//! Credentials live only in the CLI token file ({config_dir}/auth/codex.json); desktop settings, logs and
//! interface text never touch the token. The settings service editor and first-use AI step share this state.
use crate::preferences::{ServiceProtocol, ServicePurpose};
use crate::settings_ui::Resource;
use crate::{Desktop, symbols};
use course2md::login::codex::LoginStatus;
use gpui::{prelude::*, *};
use gpui_kit::component::input::InputState;
use gpui_kit_material::prelude::*;

/// Recognize the engine's missing/expired login marker and replace it with in-app connection guidance.
fn login_required(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<course2md::login::codex::CodexLoginRequired>()
            .is_some()
    })
}

/// The surface a Codex action belongs to: asynchronous results return only to the surface that started it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CodexSurface {
    Editor,
    Onboarding,
    /// The account card on the services and accounts page: connection state only, no model input.
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CodexWork {
    Checking,
    Importing,
    Authorizing,
    Models,
    LoggingOut,
}

#[derive(Default)]
pub(crate) struct CodexUi {
    generation: u64,
    pub(crate) status: Option<LoginStatus>,
    working: Option<CodexWork>,
    /// The visible result of the last action; the bool marks a failure (shown in the danger color).
    pub(crate) notice: Option<(String, bool)>,
    surface: Option<CodexSurface>,
    authorize_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl CodexUi {
    fn begin(&mut self, surface: CodexSurface, work: CodexWork) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.surface = Some(surface);
        self.working = Some(work);
        // A status check follows other actions and must not hide their result.
        if work != CodexWork::Checking {
            self.notice = None;
        }
        self.generation
    }
    fn current(&self, generation: u64, work: CodexWork) -> bool {
        self.generation == generation && self.working == Some(work)
    }
    pub(crate) fn busy(&self) -> bool {
        self.working.is_some()
    }

    /// The account's one status badge, shared by every place that shows it.
    pub(crate) fn badge(&self) -> (StatusKind, &'static str) {
        match (self.working, &self.status) {
            (Some(CodexWork::Checking), _) => (StatusKind::Progress, course2md::tr!("正在检查")),
            (Some(CodexWork::LoggingOut), _) => (StatusKind::Progress, course2md::tr!("正在退出")),
            (Some(_), _) => (StatusKind::Progress, course2md::tr!("正在连接")),
            (_, Some(LoginStatus::Connected)) => (StatusKind::Success, course2md::tr!("已连接")),
            (_, Some(LoginStatus::Expired)) => (StatusKind::Warning, course2md::tr!("登录已失效")),
            _ => (StatusKind::Neutral, course2md::tr!("未连接")),
        }
    }

    /// The last action's result, shown only where that action started.
    pub(crate) fn notice_on(&self, surface: CodexSurface) -> Option<(String, bool)> {
        self.notice
            .clone()
            .filter(|_| self.surface == Some(surface))
    }

    pub(crate) fn cancel_authorization(&self) {
        if let Some(cancel) = &self.authorize_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    fn fail(&mut self, message: String) {
        self.notice = Some((message, true));
    }
    fn inform(&mut self, message: String) {
        self.notice = Some((message, false));
    }
}

/// When the catalog arrives, an empty model input takes the first candidate (a supported default, still editable).
/// set_value needs the Window, so it returns to the window through the shared defer + update_window path.
pub(crate) fn autofill_input(
    handle: AnyWindowHandle,
    input: Entity<InputState>,
    value: Option<String>,
    cx: &mut Context<Desktop>,
) {
    let Some(value) = value else { return };
    if !input.read(cx).value().trim().is_empty() {
        return;
    }
    cx.defer(move |cx| {
        let _ = cx.update_window(handle, |_, window, cx| {
            input.update(cx, |input, cx| {
                // The person may have typed since the action finished; fill in the default only when empty.
                if input.value().trim().is_empty() {
                    input.set_value(value, window, cx);
                }
            });
        });
    });
}

impl Desktop {
    /// Runs one account action off the UI thread. Its result returns only while it is
    /// still the current action; a busy account starts nothing.
    fn codex_work<T: Send + 'static>(
        &mut self,
        surface: CodexSurface,
        work: CodexWork,
        action: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
        then: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.codex.busy() {
            return;
        }
        let generation = self.codex.begin(surface, work);
        let task = crate::spawn_blocking_io(action);
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update(cx, |this, cx| {
                if !this.codex.current(generation, work) {
                    return;
                }
                this.codex.working = None;
                this.codex.authorize_cancel = None;
                then(this, result, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Read the account status from the local token file (no network), still off the render thread.
    pub(crate) fn codex_refresh_status(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        self.codex_work(
            surface,
            CodexWork::Checking,
            course2md::login::codex::status,
            move |this, result, cx| {
                match result {
                    Ok(status) => this.codex.status = Some(status),
                    Err(error) => this
                        .codex
                        .fail(course2md::tr!("暂时无法读取 Codex 登录状态：{}", error)),
                }
                // A connected Codex surface without a catalog fetches it next (an empty model input takes the first candidate).
                if matches!(this.codex.status, Some(LoginStatus::Connected))
                    && this.codex_form_needs_catalog(surface)
                {
                    this.codex_refresh_models(surface, cx);
                }
            },
            cx,
        );
    }

    pub(crate) fn codex_import(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        self.codex_login(
            surface,
            CodexWork::Importing,
            course2md::login::codex::import_for_desktop,
            cx,
        );
    }

    /// Browser PKCE authorization blocks up to 180 seconds and opens the system browser, so it runs off the UI thread.
    pub(crate) fn codex_authorize(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.codex.authorize_cancel = Some(cancel.clone());
        self.codex_login(
            surface,
            CodexWork::Authorizing,
            move || course2md::login::codex::authorize_for_desktop(cancel),
            cx,
        );
    }

    fn codex_login(
        &mut self,
        surface: CodexSurface,
        work: CodexWork,
        action: impl FnOnce() -> anyhow::Result<course2md::login::codex::DesktopCatalog>
        + Send
        + 'static,
        cx: &mut Context<Self>,
    ) {
        self.codex_work(
            surface,
            work,
            action,
            move |this, result, cx| match result {
                Ok(catalog) => {
                    this.codex.status = Some(LoginStatus::Connected);
                    if catalog.catalog_is_fallback {
                        this.codex
                            .inform(course2md::tr!("模型目录暂不可用，已使用默认模型").into());
                    } else {
                        this.codex.notice = None;
                    }
                    this.codex_apply_catalog(surface, catalog.models, cx);
                    // Re-read the login so the account details show instead of a generic placeholder.
                    this.codex_refresh_status(surface, cx);
                }
                Err(error) => {
                    let importing = work == CodexWork::Importing;
                    this.codex.fail(if !login_required(&error) {
                        if importing {
                            course2md::tr!("导入 Codex 登录未完成：{}", format!("{error:#}"))
                        } else {
                            course2md::tr!("浏览器授权未完成：{}", format!("{error:#}"))
                        }
                    } else if importing {
                        // Missing or expired login: point to the in-app connect action, not a CLI command.
                        course2md::tr!(
                            "命令行 Codex 的登录已失效，请先在 Codex 中重新登录再导入，或改用浏览器授权"
                        )
                        .into()
                    } else {
                        course2md::tr!("Codex 登录已失效，请重新连接").into()
                    });
                }
            },
            cx,
        );
    }

    /// Fetch the account's model catalog again with the saved login.
    pub(crate) fn codex_refresh_models(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        // Put the model field into its loading state first, so a click stopped by the busy guard is not silent.
        // The settings account card has no model input and no catalog to refresh.
        if !self.codex_form_models_loading(surface) {
            return;
        }
        if self.codex.busy() {
            let message = course2md::tr!("上一个 Codex 请求尚未完成").to_string();
            self.codex_form_models_failed(surface, message);
            cx.notify();
            return;
        }
        self.codex_work(
            surface,
            CodexWork::Models,
            course2md::login::codex::refresh_models_for_desktop,
            move |this, result, cx| match result {
                Ok(catalog) => {
                    this.codex.notice = None;
                    this.codex_apply_catalog(surface, catalog, cx);
                }
                Err(error) => {
                    let message = if login_required(&error) {
                        // The service refused the saved login: the account
                        // state follows, so sign-in is offered again.
                        this.codex.status = Some(LoginStatus::Expired);
                        course2md::tr!("ChatGPT 登录已失效，请重新登录").to_string()
                    } else {
                        course2md::tr!(
                            "暂时无法获取模型列表，可以直接填写模型名称（{}）",
                            error.root_cause().to_string()
                        )
                    };
                    this.codex_form_models_failed(surface, message);
                }
            },
            cx,
        );
    }

    /// Sign out of Codex: delete the CLI token copy and unbind defaults that point at the Codex service.
    pub(crate) fn codex_logout(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        self.codex_work(
            surface,
            CodexWork::LoggingOut,
            course2md::login::codex::remove_credentials,
            |this, result, _| match result {
                Ok(_) => {
                    this.codex.status = Some(LoginStatus::Disconnected);
                    if this.codex_detach_default_service() {
                        this.codex.inform(
                            course2md::tr!("已退出 Codex 登录；默认 AI 服务已解除绑定").into(),
                        );
                    }
                    this.codex_forms_logged_out();
                }
                Err(error) => {
                    this.codex
                        .fail(course2md::tr!("退出登录尚未完成：{}", format!("{error:#}")));
                }
            },
            cx,
        );
    }

    /// Unbind the default AI service when it points at Codex; returns whether anything changed.
    fn codex_detach_default_service(&mut self) -> bool {
        let is_codex = self
            .preferences
            .default_refs()
            .llm
            .as_deref()
            .and_then(|id| self.preferences.version(id))
            .is_some_and(|version| version.config.protocol == ServiceProtocol::CodexResponses);
        if !is_codex {
            return false;
        }
        match self
            .preferences
            .set_default_service(ServicePurpose::Ai, None)
        {
            Ok(_) => true,
            Err(error) => {
                self.codex.fail(course2md::tr!(
                    "已退出 Codex 登录，默认 AI 服务尚未解除绑定：{}",
                    format!("{error:#}")
                ));
                false
            }
        }
    }

    /// The account catalog reaches only the originating surface while it still edits a Codex service.
    fn codex_apply_catalog(
        &mut self,
        surface: CodexSurface,
        catalog: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let models: Vec<String> = catalog.into_iter().map(|(slug, _)| slug).collect();
        let suggested = models.first().cloned();
        self.codex_form_catalog(surface, models, suggested, cx);
    }

    /// The Codex account section shared by the editor and first use: status, explanation and sign-in actions.
    pub(crate) fn codex_account_section(
        &self,
        surface: CodexSurface,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = match surface {
            CodexSurface::Editor => "codex-account",
            _ => "setup-codex-account",
        };
        let child = |name: &str| SharedString::from(format!("{id}-{name}"));
        let working = self.codex.working;
        let busy = working.is_some();
        let connected = matches!(self.codex.status, Some(LoginStatus::Connected))
            && !matches!(working, Some(CodexWork::LoggingOut));
        let authorizing = working == Some(CodexWork::Authorizing);
        let mut lines = vec![(course2md::tr!("使用你的 ChatGPT 订阅").to_owned(), false)];
        if authorizing {
            lines.push((
                course2md::tr!("请在浏览器中完成授权（约 3 分钟内）").into(),
                false,
            ));
        }
        lines.extend(self.codex.notice_on(surface));
        let mut actions = Vec::new();
        if authorizing {
            actions.push(
                material_button(child("cancel-authorize"), ButtonVariant::Text)
                    .icon(symbols::close())
                    .child(course2md::tr!("取消授权"))
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.codex.cancel_authorization();
                        this.codex
                            .inform(course2md::tr!("正在取消浏览器授权…").into());
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if connected {
            // The same order as the Bilibili account: check again first, sign out last.
            actions.push(
                material_button(child("recheck"), ButtonVariant::Text)
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重新检查"))
                    .disabled(disabled || busy)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.codex_refresh_status(surface, cx)),
                    )
                    .into_any_element(),
            );
            actions.push(
                material_button(child("logout"), ButtonVariant::Text)
                    .icon(symbols::logout())
                    .child(course2md::tr!("退出登录"))
                    .disabled(disabled || busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.codex_logout(surface, cx)))
                    .into_any_element(),
            );
        } else {
            // Importing an existing login is the quiet alternative; signing in through
            // the browser is the primary way.
            actions.push(
                material_button(child("import"), ButtonVariant::Text)
                    .child(if matches!(self.codex.status, Some(LoginStatus::Expired)) {
                        course2md::tr!("重新导入 Codex 登录")
                    } else {
                        course2md::tr!("导入 Codex 登录")
                    })
                    .disabled(disabled || busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.codex_import(surface, cx)))
                    .into_any_element(),
            );
            actions.push(
                material_button(child("authorize"), ButtonVariant::Filled)
                    .icon(symbols::open_in_new())
                    .child(course2md::tr!("在浏览器中登录"))
                    .disabled(disabled || busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.codex_authorize(surface, cx)))
                    .into_any_element(),
            );
        }
        Resource {
            id: id.into(),
            icon: symbols::auto_awesome().into(),
            title: course2md::tr!("ChatGPT 账号").into(),
            status: vec![self.codex.badge()],
            busy,
            lines,
            actions,
        }
        .row(cx)
    }
}
