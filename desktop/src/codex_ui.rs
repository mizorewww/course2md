//! Codex（ChatGPT 订阅）账号连接：导入 codex CLI 登录态或浏览器 PKCE 授权。
//! 凭据只存在于 CLI 令牌文件（{config_dir}/auth/codex.json）；桌面设置、日志与
//! 界面文本不接触令牌。设置的服务编辑器与首次引导的 AI 步骤共用此状态与组合。
use super::*;
use crate::preferences::{ServiceProtocol, ServicePurpose};
use crate::theme::*;
use course2md::login::LoginStatus;
use gpui_component::button::ButtonVariants as _;

/// Codex 固定端点说明：设置编辑器与首次引导共用同一文案。
pub(crate) const ENDPOINT_NOTE: &str =
    "请求固定发往 OpenAI Codex 后端；无需服务地址与 API Key。";

/// 识别根 crate 的登录缺失/失效标记，替换为界面内的连接引导。
fn login_required(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<course2md::login::codex::CodexLoginRequired>().is_some())
}

/// Codex 动作的归属界面：异步结果只落回发起时的界面。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CodexSurface {
    Editor,
    Onboarding,
    /// 设置「服务与账号」页的账号卡片：只呈现连接状态，不填任何模型输入
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
    /// 最近一次动作的可见结果；bool = 是否为失败（失败用危险色呈现）
    pub(crate) notice: Option<(String, bool)>,
    surface: Option<CodexSurface>,
}

impl CodexUi {
    fn begin(&mut self, surface: CodexSurface, work: CodexWork) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.surface = Some(surface);
        self.working = Some(work);
        self.notice = None;
        self.generation
    }
    fn current(&self, generation: u64, work: CodexWork) -> bool {
        self.generation == generation && self.working == Some(work)
    }
    pub(crate) fn busy(&self) -> bool {
        self.working.is_some()
    }
    fn fail(&mut self, message: String) {
        self.notice = Some((message, true));
    }
    fn inform(&mut self, message: String) {
        self.notice = Some((message, false));
    }
}

/// 目录到达时为空的模型输入填入首个候选（受支持默认值，仍可修改）。
/// set_value 需要 Window：经共享的 defer + update_window 途径回到窗口上下文。
pub(crate) fn autofill_input(
    input: Entity<InputState>,
    value: Option<String>,
    cx: &mut Context<Desktop>,
) {
    let Some(value) = value else { return };
    if !input.read(cx).value().trim().is_empty() {
        return;
    }
    let Some(handle) = cx.windows().first().copied() else {
        return;
    };
    cx.defer(move |cx| {
        let _ = cx.update_window(handle, |_, window, cx| {
            input.update(cx, |input, cx| {
                // 动作完成到落窗之间用户可能已开始输入；空输入才填默认值
                if input.value().trim().is_empty() {
                    input.set_value(value, window, cx);
                }
            });
        });
    });
}

impl Desktop {
    /// 读本地令牌文件的账号状态（不触网）；仍在后台线程执行，不占用渲染。
    pub(crate) fn codex_refresh_status(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        if self.codex.busy() {
            return;
        }
        let generation = self.codex.begin(surface, CodexWork::Checking);
        let task = crate::spawn_blocking_io(|| {
            course2md::login::for_platform(course2md::cli::LoginPlatform::Codex)
                .status()
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.recv().await else { return };
            let _ = this.update(cx, |this, cx| {
                if !this.codex.current(generation, CodexWork::Checking) {
                    return;
                }
                this.codex.working = None;
                match result {
                    Ok(status) => this.codex.status = Some(status),
                    Err(error) => {
                        this.codex.fail(format!("暂时无法读取 Codex 登录状态：{error}"))
                    }
                }
                // 已连接的 Codex 界面没有目录时，顺接拉取账号目录（空模型输入自动填首个候选）
                let needs_catalog = matches!(this.codex.status, Some(LoginStatus::Connected(_)))
                    && match surface {
                        CodexSurface::Editor => this.editor_codex_needs_catalog(),
                        CodexSurface::Onboarding => this.onboarding_codex_needs_catalog(),
                        CodexSurface::Settings => false,
                    };
                if needs_catalog {
                    this.codex_refresh_models(surface, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn codex_import(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        self.codex_login(
            surface,
            CodexWork::Importing,
            course2md::login::codex::import_for_desktop,
            cx,
        );
    }

    /// 浏览器 PKCE 授权：阻塞最长 180 秒并打开系统浏览器，必须离开 UI 线程。
    pub(crate) fn codex_authorize(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        self.codex_login(
            surface,
            CodexWork::Authorizing,
            course2md::login::codex::authorize_for_desktop,
            cx,
        );
    }

    fn codex_login(
        &mut self,
        surface: CodexSurface,
        work: CodexWork,
        action: fn() -> anyhow::Result<course2md::login::codex::DesktopCatalog>,
        cx: &mut Context<Self>,
    ) {
        if self.codex.busy() {
            return;
        }
        let generation = self.codex.begin(surface, work);
        let task = crate::spawn_blocking_io(action);
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.recv().await else { return };
            let _ = this.update(cx, |this, cx| {
                if !this.codex.current(generation, work) {
                    return;
                }
                this.codex.working = None;
                match result {
                    Ok(catalog) => {
                        this.codex.status = Some(LoginStatus::Connected("Codex".into()));
                        this.codex.inform(if catalog.catalog_is_fallback {
                            "已连接 Codex。暂时无法获取账号的模型目录，已填入默认模型，可稍后刷新。".into()
                        } else {
                            "已连接 Codex，模型目录已更新。".into()
                        });
                        this.codex_apply_catalog(surface, catalog.models, cx);
                        // 重读登录状态，展示账号详情而非通用占位
                        this.codex_refresh_status(surface, cx);
                    }
                    Err(error) => {
                        let message = if login_required(&error) {
                            // 登录缺失/失效：引导使用界面内的连接动作，而非 CLI 命令
                            match work {
                                CodexWork::Importing => {
                                    "codex CLI 的登录已失效：请在 codex CLI 重新登录后再导入，或改用浏览器授权。"
                                        .into()
                                }
                                _ => "Codex 登录已失效，请重新连接。".into(),
                            }
                        } else {
                            match work {
                                CodexWork::Importing => format!("导入 Codex 登录未完成：{error:#}"),
                                _ => format!("浏览器授权未完成：{error:#}"),
                            }
                        };
                        this.codex.fail(message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 用已保存的登录态重新拉取账号可用的模型目录。
    pub(crate) fn codex_refresh_models(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        // 先让模型字段进入加载状态：即使随后被忙碌守卫拦下，点击也不是静默无响应
        let armed = match surface {
            CodexSurface::Editor => self.editor_codex_models_loading(),
            CodexSurface::Onboarding => self.onboarding_codex_models_loading(),
            // 设置页账号卡片没有模型输入，无目录可刷新
            CodexSurface::Settings => false,
        };
        if !armed {
            return;
        }
        if self.codex.busy() {
            let message = "正在处理上一个 Codex 请求，完成后请重试获取模型。".to_string();
            match surface {
                CodexSurface::Editor => self.editor_codex_models_failed(message),
                CodexSurface::Onboarding => self.onboarding_codex_models_failed(message),
                CodexSurface::Settings => {}
            }
            cx.notify();
            return;
        }
        let generation = self.codex.begin(surface, CodexWork::Models);
        let task = crate::spawn_blocking_io(course2md::login::codex::refresh_models_for_desktop);
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.recv().await else { return };
            let _ = this.update(cx, |this, cx| {
                if !this.codex.current(generation, CodexWork::Models) {
                    return;
                }
                this.codex.working = None;
                match result {
                    Ok(catalog) => {
                        this.codex.notice = None;
                        this.codex_apply_catalog(surface, catalog, cx);
                    }
                    Err(error) => {
                        let message = if login_required(&error) {
                            "Codex 登录已失效，请先连接 Codex 账号。".to_string()
                        } else {
                            format!("暂时无法获取 Codex 模型目录：{error:#}")
                        };
                        match surface {
                            CodexSurface::Editor => this.editor_codex_models_failed(message),
                            CodexSurface::Onboarding => {
                                this.onboarding_codex_models_failed(message)
                            }
                            CodexSurface::Settings => {}
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 退出 Codex 登录：删除 CLI 令牌副本，并解除指向 Codex 服务的默认绑定。
    pub(crate) fn codex_logout(&mut self, surface: CodexSurface, cx: &mut Context<Self>) {
        if self.codex.busy() {
            return;
        }
        let generation = self.codex.begin(surface, CodexWork::LoggingOut);
        let task = crate::spawn_blocking_io(course2md::login::codex::logout_for_desktop);
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.recv().await else { return };
            let _ = this.update(cx, |this, cx| {
                if !this.codex.current(generation, CodexWork::LoggingOut) {
                    return;
                }
                this.codex.working = None;
                match result {
                    Ok(_) => {
                        this.codex.status = Some(LoginStatus::Disconnected);
                        let detached = this.codex_detach_default_service();
                        this.codex.inform(if detached {
                            "已退出 Codex 登录；默认 AI 服务已解除绑定。".into()
                        } else {
                            "已退出 Codex 登录。".into()
                        });
                        this.editor_codex_logged_out();
                        this.onboarding_codex_logged_out();
                    }
                    Err(error) => {
                        this.codex.fail(format!("退出登录尚未完成：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 默认 AI 服务绑定指向 Codex 服务时解除绑定；返回是否发生了变更。
    fn codex_detach_default_service(&mut self) -> bool {
        let is_codex = self
            .preferences
            .default_refs()
            .llm
            .as_deref()
            .and_then(|id| self.preferences.version(id))
            .is_some_and(|version| version.config.protocol == ServiceProtocol::CodexResponses);
        is_codex && self.preferences.set_default_service(ServicePurpose::Ai, None).is_ok()
    }

    /// 账号目录只落到仍在编辑 Codex 服务的发起界面。
    fn codex_apply_catalog(
        &mut self,
        surface: CodexSurface,
        catalog: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let models: Vec<String> = catalog.into_iter().map(|(slug, _)| slug).collect();
        let suggested = models.first().cloned();
        match surface {
            CodexSurface::Editor => self.editor_codex_catalog(models, suggested, cx),
            CodexSurface::Onboarding => self.onboarding_codex_catalog(models, suggested, cx),
            CodexSurface::Settings => {}
        }
    }

    /// 编辑器/引导共用的 Codex 账号区：状态、说明与登录动作。
    pub(crate) fn codex_account_section(
        &self,
        surface: CodexSurface,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = match surface {
            CodexSurface::Editor => "codex-account",
            CodexSurface::Onboarding => "setup-codex-account",
            CodexSurface::Settings => "settings-codex-account",
        };
        let working = self.codex.working;
        let busy = working.is_some();
        let (kind, label) = match (working, &self.codex.status) {
            (Some(CodexWork::Checking), _) => (BadgeKind::Progress, "正在检查"),
            (Some(CodexWork::LoggingOut), _) => (BadgeKind::Progress, "正在退出"),
            (Some(_), _) => (BadgeKind::Progress, "正在连接"),
            (_, Some(LoginStatus::Connected(_))) => (BadgeKind::Success, "已连接"),
            (_, Some(LoginStatus::Expired)) => (BadgeKind::Warning, "登录已失效"),
            _ => (BadgeKind::Neutral, "未连接"),
        };
        let detail = match &self.codex.status {
            Some(LoginStatus::Connected(detail)) if !detail.trim().is_empty() => Some(detail.clone()),
            _ => None,
        };
        let connected = matches!(self.codex.status, Some(LoginStatus::Connected(_)))
            && !matches!(working, Some(CodexWork::LoggingOut));
        let notice = self.codex.notice.clone();
        let authorizing = working == Some(CodexWork::Authorizing);
        crate::settings_ui::settings_detail_group(
            SharedString::from(format!("{id}-heading")),
            icons::shield(),
            "Codex 账号",
        )
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(badge(kind).child(label))
                .when_some(detail, |row, detail| {
                    row.child(
                        accessible_text(SharedString::from(format!("{id}-detail")), detail)
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(TEXT_AUX)
                            .text_color(color(MUTED)),
                    )
                })
                .when(busy, |row| {
                    row.child(crate::motion::spinner(
                        SharedString::from(format!("{id}-busy")),
                        cx,
                    ))
                }),
        )
        .child(
            supporting_info(
                SharedString::from(format!("{id}-policy")),
                "Codex 使用 ChatGPT 订阅登录，请求固定发往 OpenAI Codex 后端；令牌保存在本机登录文件中，转换时自动刷新，无需 API Key。",
            )
            .w_full()
            .min_w_0()
            .whitespace_normal(),
        )
        .when(authorizing, |view| {
            view.child(
                supporting_info(
                    SharedString::from(format!("{id}-authorizing")),
                    "请在浏览器中完成 ChatGPT 授权，最长等待约 3 分钟。",
                )
                .w_full()
                .min_w_0()
                .whitespace_normal(),
            )
        })
        .when_some(notice, |view, (message, is_error)| {
            view.child(
                accessible_text(SharedString::from(format!("{id}-notice")), message)
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(TEXT_AUX)
                    .text_color(color(if is_error { DANGER } else { MUTED })),
            )
        })
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .flex_wrap()
                .justify_end()
                .when(!connected, |row| {
                    row.child(
                        outline_pill(SharedString::from(format!("{id}-import")))
                            .icon(icons::login())
                            .label(if matches!(self.codex.status, Some(LoginStatus::Expired)) {
                                "重新导入 Codex 登录"
                            } else {
                                "导入 Codex 登录"
                            })
                            .disabled(disabled || busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.codex_import(surface, cx)
                            })),
                    )
                    .child(
                        quiet(SharedString::from(format!("{id}-authorize")))
                            .icon(icons::external_link())
                            .label("浏览器授权…")
                            .disabled(disabled || busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.codex_authorize(surface, cx)
                            })),
                    )
                })
                .when(connected, |row| {
                    row.child(
                        quiet(SharedString::from(format!("{id}-logout")))
                            .icon(icons::logout())
                            .label("退出登录")
                            .disabled(disabled || busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.codex_logout(surface, cx)
                            })),
                    )
                    .child(
                        control(SharedString::from(format!("{id}-recheck")))
                            .ghost()
                            .icon(icons::refresh())
                            .label("重新检查")
                            .disabled(disabled || busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.codex_refresh_status(surface, cx)
                            })),
                    )
                }),
        )
    }
}
