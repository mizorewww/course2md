//! Bilibili account settings and cancellable QR login. No ticket is logged or stored in UI text.
use super::*;
use crate::theme::*;
use course2md::auth::{AccountProfile, AccountStatus, QrPoll, QrSession};
use gpui_component::{
    button::*,
    menu::{DropdownMenu, PopupMenuItem},
};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct AccountUi {
    generation: u64,
    status_generation: u64,
    dialog: Option<QrDialogState>,
    modules: Option<Arc<Vec<Vec<bool>>>>,
    status: Option<AccountStatus>,
    checking: bool,
    has_saved_login: bool,
    status_error: Option<String>,
    /// The current status_error is the logout-incomplete message; a flag so a
    /// copy change cannot silently break the status derivation.
    logout_failed: bool,
    retry_source: Option<(u64, String)>,
    return_focus: Option<FocusHandle>,
}
#[derive(Clone)]
enum QrDialogState {
    Generating,
    Waiting(u64),
    Confirming(u64),
    Expired,
    Error(String),
    Success(AccountProfile),
}

enum QrPollResult {
    Waiting,
    Confirming,
    Expired,
    Authenticated(Result<AccountProfile>),
    Failed,
}

#[derive(Clone, Copy, Debug)]
struct QrLayout {
    dialog_width: f32,
    content_width: f32,
    content_height: f32,
    side_by_side: bool,
    code_size: f32,
    text_height: f32,
    gap: f32,
}

/// Match the dialog's 16px padding and 1px border, reserving its existing
/// title/footer budget. The QR itself never belongs to a scrolling region.
fn qr_layout(viewport_width: f32, viewport_height: f32, rem_size: f32) -> QrLayout {
    let scale = rem_size / 14.;
    let dialog_width = (420. * scale).min((viewport_width - 48.).max(0.));
    let content_width = (dialog_width - 34.).max(0.);
    let content_height = (viewport_height - rem_size * (180. / 14.) - 64.).max(80.);
    let gap = 16. * scale;
    let preferred_code = 260_f32.min(content_width);
    let text_reserve = rem_size * 4.5;
    let side_by_side = content_height < preferred_code + gap + text_reserve
        && content_width >= preferred_code.min(content_height) + gap + rem_size * 12.;
    let code_size = if side_by_side {
        preferred_code.min(content_height)
    } else {
        preferred_code.min((content_height - gap - text_reserve).max(0.))
    }
    .floor();
    let text_height = if side_by_side {
        content_height
    } else {
        (content_height - code_size - gap).max(0.)
    };
    QrLayout {
        dialog_width,
        content_width,
        content_height,
        side_by_side,
        code_size,
        text_height,
        gap,
    }
}

/// Whole physical pixels avoid blurry modules. Centering includes at least
/// four white modules on every side, even when the available size is fractional.
fn qr_raster(code_size: f32, module_count: usize, scale: f32) -> (f32, f32) {
    let total = (module_count + 8) as f32;
    // Keep a pixel of slack at each edge so snapping a fractional window origin
    // cannot take part of the four-module quiet zone away.
    let unit = ((code_size * scale - 2.).max(0.) / total).floor() / scale;
    let offset = (code_size - unit * total) / 2. + unit * 4.;
    (unit, offset)
}

impl AccountUi {
    fn current(&self, generation: u64) -> bool {
        self.generation == generation && self.dialog.is_some()
    }
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.dialog = None;
        self.modules = None;
    }

    fn close(&mut self) {
        self.invalidate();
        self.retry_source = None;
    }

    fn apply_status(
        &mut self,
        generation: u64,
        has_saved_login: bool,
        result: Result<AccountStatus, String>,
    ) -> bool {
        if self.status_generation != generation {
            return false;
        }
        self.checking = false;
        self.has_saved_login = has_saved_login;
        match result {
            Ok(status) => {
                // A removed credential file must not keep the previous "saved" badge.
                self.has_saved_login = !matches!(status, AccountStatus::Disconnected);
                self.status = Some(status);
                self.status_error = None;
                self.logout_failed = false;
            }
            Err(error) => {
                self.status_error = Some(error);
                self.logout_failed = false;
            }
        }
        true
    }

    fn login_action(&self) -> Option<&'static str> {
        if matches!(self.status, Some(AccountStatus::Connected(_)))
            && self.has_saved_login
            && self.status_error.is_none()
        {
            None
        } else if self.has_saved_login || matches!(self.status, Some(AccountStatus::Expired)) {
            Some(course2md::tr!("重新登录 Bilibili"))
        } else {
            Some(course2md::tr!("登录 Bilibili"))
        }
    }

    fn can_resume_source(&self, generation: u64, input: &str, in_source: bool) -> bool {
        in_source
            && self
                .retry_source
                .as_ref()
                .is_some_and(|(saved_generation, saved_input)| {
                    *saved_generation == generation && saved_input == input
                })
    }
}

impl Desktop {
    pub(crate) fn account_connected(&self) -> bool {
        matches!(self.account.status, Some(AccountStatus::Connected(_)))
    }

    pub fn refresh_account(&mut self, cx: &mut Context<Self>) {
        if self.account.checking {
            return;
        }
        self.account.status_generation = self.account.status_generation.wrapping_add(1);
        let generation = self.account.status_generation;
        self.account.checking = true;
        self.account.status_error = None;
        self.account.logout_failed = false;
        // The account status is a blocking network request; see crate::spawn_blocking_io.
        let task = crate::spawn_blocking_io(|| {
            let saved = course2md::auth::cookie_path().is_file();
            let status =
                course2md::auth::bilibili_account_status().map_err(|error| error.to_string());
            (saved, status)
        });
        cx.spawn(async move |this, cx| {
            let received = task.recv().await;
            let _ = this.update(cx, |this, cx| {
                let (saved, result) = received.unwrap_or_else(|_| {
                    (
                        this.account.has_saved_login,
                        Err(crate::interrupted().to_owned()),
                    )
                });
                if this.account.apply_status(generation, saved, result) {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn account_settings_page(&self, cx: &mut Context<Self>) -> AnyElement {
        self.account_summary(false, cx).into_any_element()
    }

    /// Shared account state and actions only; the guide owns its heading and footer.
    pub(crate) fn account_onboarding_page(&self, cx: &mut Context<Self>) -> Div {
        self.account_summary(true, cx)
    }

    /// The single detail source shared by the summary card and status text.
    fn account_detail(&self) -> Option<String> {
        if self.account.checking {
            None
        } else if self.account.logout_failed {
            Some(course2md::tr!("退出登录尚未完成，原登录状态已保留").to_owned())
        } else if self.account.status_error.is_some() {
            self.account
                .has_saved_login
                .then(|| course2md::tr!("已保存登录信息").to_owned())
        } else {
            match &self.account.status {
                Some(AccountStatus::Connected(profile)) if !profile.name.trim().is_empty() => {
                    Some(profile.name.clone())
                }
                None if self.account.has_saved_login => Some(course2md::tr!("尚未验证").to_owned()),
                _ => None,
            }
        }
    }

    fn account_summary(&self, onboarding: bool, cx: &mut Context<Self>) -> Div {
        let saved = self.account.has_saved_login;
        let (kind, label) = if self.account.checking {
            (BadgeKind::Progress, course2md::tr!("验证中"))
        } else if self.account.status_error.is_some() {
            (BadgeKind::Warning, course2md::tr!("暂时无法验证"))
        } else {
            match &self.account.status {
                Some(AccountStatus::Expired) => (BadgeKind::Warning, course2md::tr!("登录已失效")),
                Some(AccountStatus::Connected(_)) => (BadgeKind::Success, course2md::tr!("已登录")),
                _ if saved => (BadgeKind::Neutral, course2md::tr!("已保存登录信息")),
                _ => (BadgeKind::Neutral, course2md::tr!("未登录")),
            }
        };
        // The badge carries the state; the line adds only what it does not say.
        let detail = self.account_detail();
        let mut actions = Vec::new();
        if let Some(login) = self.account.login_action() {
            actions.push(
                outline_pill("account-login", cx)
                    .settings_text()
                    .icon(icons::bilibili())
                    .label(login)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_account_dialog(window, cx)),
                    )
                    .into_any_element(),
            );
        }
        // Only a saved login has anything to re-verify or sign out of; like
        // every resource row, its commands sit in one ⋯ menu.
        if saved {
            let entity = cx.weak_entity();
            let checking = self.account.checking;
            actions.push(
                crate::settings_ui::resource_menu("account-actions", cx)
                    .accessibility_label(course2md::tr!("{} 操作", course2md::tr!("Bilibili 账号")))
                    .tooltip(course2md::tr!("账号操作"))
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                        let refresh = entity.clone();
                        let logout = entity.clone();
                        menu.item(
                            PopupMenuItem::new(course2md::tr!("重新检查"))
                                .icon(icons::refresh())
                                .disabled(checking)
                                .on_click(move |_, _, cx| {
                                    let _ = refresh.update(cx, |this, cx| this.refresh_account(cx));
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new(course2md::tr!("退出登录"))
                                .icon(icons::logout())
                                .on_click(move |_, _, cx| {
                                    let _ = logout.update(cx, |this, cx| this.clear_account(cx));
                                }),
                        )
                    })
                    .into_any_element(),
            );
        }
        crate::settings_ui::AccountCard {
            id: "bilibili-account",
            icon: icons::tv(),
            tint: Tint::Pink,
            title: if onboarding {
                course2md::tr!("账号状态")
            } else {
                course2md::tr!("Bilibili 账号")
            },
            status: (kind, label),
            detail,
            busy: self.account.checking,
            lines: vec![(
                "policy",
                if saved {
                    course2md::tr!("退出登录不会删除笔记")
                } else {
                    course2md::tr!("登录后可读取账号有权访问的视频与字幕")
                }
                .into(),
                false,
            )],
            actions,
        }
        .render(cx)
    }

    /// Status text without the「Bilibili：」prefix and the「已登录 · 」name
    /// separator; the single source for the summary row, the compact source
    /// row and the detail line, which each format their own wrapper.
    fn account_status_text(&self) -> String {
        if self.account.checking {
            return course2md::tr!("正在验证登录状态…").into();
        }
        if self.account.logout_failed {
            return course2md::tr!("退出登录尚未完成，原登录状态已保留").into();
        }
        if self.account.status_error.is_some() {
            return if self.account.has_saved_login {
                course2md::tr!("暂时无法验证，已保存登录信息").into()
            } else {
                course2md::tr!("暂时无法检查登录状态").into()
            };
        }
        match &self.account.status {
            Some(AccountStatus::Connected(profile)) if !profile.name.trim().is_empty() => {
                course2md::tr!("已登录 · {}", profile.name)
            }
            Some(AccountStatus::Connected(_)) => course2md::tr!("已登录").into(),
            Some(AccountStatus::Expired) => course2md::tr!("登录已失效").into(),
            Some(AccountStatus::Disconnected) => course2md::tr!("未登录").into(),
            None if self.account.has_saved_login => {
                course2md::tr!("已保存登录信息，尚未验证").into()
            }
            None => course2md::tr!("未登录").into(),
        }
    }

    /// Only rendered inside source details or a permission repair, never a permanent status row.
    pub fn source_account_row(&self, cx: &mut Context<Self>) -> Div {
        let status = course2md::tr!("Bilibili：{}", self.account_status_text());
        let login_action = self.account.login_action();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .flex_wrap()
            .child(
                div()
                    .id("source-bilibili-account-status")
                    .role(Role::Label)
                    .aria_label(status.clone())
                    .child(status)
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .text_sm()
                    .text_color(color(GRAY)),
            )
            .child(
                control("source-account-refresh")
                    .ghost()
                    .icon(icons::refresh())
                    .label(course2md::tr!("重新检查"))
                    .disabled(self.account.checking)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_account(cx))),
            )
            .when_some(login_action, |view, label| {
                view.child(
                    control("source-account-login")
                        .icon(icons::login())
                        .ghost()
                        .label(label)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_account_dialog(window, cx)),
                        ),
                )
            })
    }

    fn can_retry_account_source(&self, cx: &App) -> bool {
        self.account.can_resume_source(
            self.preview_generation,
            &self.value(Field::Source, cx),
            !self.onboarding.active
                && matches!(self.page, Page::New)
                && self.online()
                && (self.preview_error.is_some() || self.subtitle_attention_required()),
        )
    }

    fn clear_account(&mut self, cx: &mut Context<Self>) {
        self.account.invalidate();
        self.account.status_generation = self.account.status_generation.wrapping_add(1);
        self.account.checking = false;
        let generation = self.account.status_generation;
        // Removing the keychain/file credential can block; keep it off the UI thread.
        let task = crate::spawn_blocking_io(course2md::auth::clear_bilibili_login);
        cx.spawn(async move |this, cx| {
            let result = task.recv().await;
            let _ = this.update(cx, |this, cx| {
                if this.account.status_generation != generation {
                    return;
                }
                match result {
                    Ok(Ok(_)) => {
                        this.account.has_saved_login = false;
                        this.account.status = Some(AccountStatus::Disconnected);
                        this.account.status_error = None;
                        this.account.logout_failed = false;
                    }
                    _ => {
                        this.account.status_error =
                            Some(course2md::tr!("退出登录尚未完成，原登录状态已保留").into());
                        this.account.logout_failed = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn close_account_dialog(&mut self, cx: &mut Context<Self>) {
        self.account.close();
        cx.notify();
    }

    pub fn open_account_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account.dialog.is_some() {
            return;
        }
        self.account.retry_source = if !self.onboarding.active
            && matches!(self.page, Page::New)
            && self.online()
            && (self.preview_error.is_some() || self.subtitle_attention_required())
            && course2md::auth::is_bilibili_url(&self.value(Field::Source, cx))
        {
            Some((self.preview_generation, self.value(Field::Source, cx)))
        } else {
            None
        };
        self.account.return_focus = window.focused(cx);
        self.start_account_qr(cx);
        let desktop = cx.entity();
        let content =
            crate::views::DesktopView::observing(desktop.clone(), cx, |desktop, window, cx| {
                desktop
                    .account_dialog_content(window, cx)
                    .into_any_element()
            });
        let footer = crate::views::DesktopView::observing(desktop, cx, |desktop, _, cx| {
            desktop.account_dialog_actions(cx).into_any_element()
        });
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, _| {
            let weak = weak.clone();
            let closed = weak.clone();
            let layout = qr_layout(
                f32::from(window.bounds().size.width),
                f32::from(window.bounds().size.height),
                f32::from(window.rem_size()),
            );
            dialog
                .material("bilibili-dialog-title", course2md::tr!("登录 Bilibili"))
                .close_button(false)
                .w(px(layout.dialog_width))
                .margin_top(task_dialog_top(window))
                .overlay_closable(false)
                .child(content.clone())
                .footer(footer.clone())
                .on_close(move |_, window, cx| {
                    let _ = closed.update(cx, |this, cx| {
                        this.close_account_dialog(cx);
                        if let Some(focus) = this.account.return_focus.take() {
                            focus.focus(window, cx);
                        }
                    });
                })
                .on_cancel(move |_, _, cx| {
                    let _ = weak.update(cx, |this, cx| this.close_account_dialog(cx));
                    true
                })
        });
    }

    fn start_account_qr(&mut self, cx: &mut Context<Self>) {
        self.account.invalidate();
        self.account.dialog = Some(QrDialogState::Generating);
        let generation = self.account.generation;
        let task = crate::spawn_blocking_io(QrSession::generate);
        cx.spawn(async move |this, cx| {
            let result = task.recv().await;
            let mut session = match result {
                Ok(Ok(session)) => session,
                _ => {
                    let _ = this.update(cx, |this, cx| {
                        if this.account.current(generation) {
                            this.account.dialog = Some(QrDialogState::Error(
                                course2md::tr!("获取二维码失败，请检查网络后重试").into(),
                            ));
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let modules = Arc::new(session.modules());
            let active = this
                .update(cx, |this, cx| {
                    if !this.account.current(generation) {
                        return false;
                    }
                    this.account.modules = Some(modules);
                    this.account.dialog =
                        Some(QrDialogState::Waiting(session.remaining().as_secs()));
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !active {
                return;
            }
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if !this
                    .update(cx, |this, _| this.account.current(generation))
                    .unwrap_or(false)
                {
                    break;
                }
                let task = crate::spawn_blocking_io(move || {
                    let result = session.poll();
                    (session, result)
                });
                let Ok((next, result)) = task.recv().await else {
                    let _ = this.update(cx, |this, cx| {
                        if this.account.current(generation) {
                            this.account.dialog =
                                Some(QrDialogState::Error(crate::interrupted().into()));
                            cx.notify();
                        }
                    });
                    break;
                };
                session = next;
                let remaining = session.remaining().as_secs();
                let result = match result {
                    Ok(QrPoll::Waiting) => QrPollResult::Waiting,
                    Ok(QrPoll::AwaitingConfirmation) => QrPollResult::Confirming,
                    Ok(QrPoll::Expired) => QrPollResult::Expired,
                    Ok(QrPoll::Authenticated(login)) => {
                        // The credential commit touches the keychain/file store; do it
                        // off the UI thread, then re-check the generation below.
                        let save = crate::spawn_blocking_io(move || login.save());
                        QrPollResult::Authenticated(match save.recv().await {
                            Ok(result) => result,
                            Err(_) => Err(anyhow::anyhow!(crate::interrupted())),
                        })
                    }
                    Err(_) => QrPollResult::Failed,
                };
                let keep_polling = this
                    .update(cx, |this, cx| {
                        if !this.account.current(generation) {
                            return false;
                        }
                        let (state, keep_polling) = match result {
                            QrPollResult::Waiting => (QrDialogState::Waiting(remaining), true),
                            QrPollResult::Confirming => {
                                (QrDialogState::Confirming(remaining), true)
                            }
                            QrPollResult::Expired => (QrDialogState::Expired, false),
                            QrPollResult::Authenticated(Ok(profile)) => {
                                this.account.status_generation =
                                    this.account.status_generation.wrapping_add(1);
                                this.account.checking = false;
                                this.account.has_saved_login = true;
                                this.account.status =
                                    Some(AccountStatus::Connected(profile.clone()));
                                this.account.status_error = None;
                                this.account.logout_failed = false;
                                (QrDialogState::Success(profile), false)
                            }
                            QrPollResult::Authenticated(Err(_)) => (
                                QrDialogState::Error(
                                    course2md::tr!(
                                        "账号已确认，但保存登录状态失败。请检查本地权限后重新扫码。"
                                    )
                                    .into(),
                                ),
                                false,
                            ),
                            QrPollResult::Failed => (
                                QrDialogState::Error(
                                    course2md::tr!("登录请求失败，请检查网络后重新获取二维码")
                                        .into(),
                                ),
                                false,
                            ),
                        };
                        if !keep_polling {
                            this.account.modules = None;
                        }
                        this.account.dialog = Some(state);
                        cx.notify();
                        keep_polling
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn account_dialog_content(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let layout = qr_layout(
            f32::from(window.bounds().size.width),
            f32::from(window.bounds().size.height),
            f32::from(window.rem_size()),
        );
        let state = self
            .account
            .dialog
            .clone()
            .unwrap_or(QrDialogState::Generating);
        let (message, hint): (String, String) = match &state {
            QrDialogState::Generating => (course2md::tr!("正在获取二维码").into(), String::new()),
            QrDialogState::Waiting(seconds) => (
                course2md::tr!("请使用 Bilibili App 扫码").into(),
                course2md::tr!("二维码约 {} 秒后过期", seconds),
            ),
            QrDialogState::Confirming(seconds) => (
                course2md::tr!("已扫码，请在手机上确认登录").into(),
                course2md::tr!("请在约 {} 秒内确认", seconds),
            ),
            QrDialogState::Expired => (course2md::tr!("二维码已过期").into(), String::new()),
            QrDialogState::Error(error) => (course2md::tr!("登录未完成").into(), error.clone()),
            QrDialogState::Success(profile) => (
                course2md::tr!("已登录 Bilibili").into(),
                profile.name.trim().to_owned(),
            ),
        };
        let retry = matches!(state, QrDialogState::Expired | QrDialogState::Error(_));
        let success = matches!(state, QrDialogState::Success(_));
        let mut visual = v_flex()
            .w(px(layout.code_size))
            .h(px(layout.code_size))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded(RADIUS_CARD)
            .bg(color(if success {
                SUCCESS_BG
            } else if retry {
                WARNING_BG
            } else {
                INSET
            }));
        if let Some(modules) = &self.account.modules {
            let modules = modules.clone();
            // A real QR code intentionally keeps its high-contrast white scanning surface.
            visual = visual.rounded(px(0.)).bg(gpui::rgb(0xffffff)).child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        // Whole physical-pixel modules and a >=4-module quiet zone keep QR edges crisp.
                        let scale = window.scale_factor();
                        let (unit, offset) = qr_raster(
                            bounds.size.width.as_f32().min(bounds.size.height.as_f32()),
                            modules.len(),
                            scale,
                        );
                        for (y, row) in modules.iter().enumerate() {
                            for (x, dark) in row.iter().enumerate() {
                                if *dark {
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(
                                                px(((bounds.origin.x.as_f32()
                                                    + offset
                                                    + x as f32 * unit)
                                                    * scale)
                                                    .round()
                                                    / scale),
                                                px(((bounds.origin.y.as_f32()
                                                    + offset
                                                    + y as f32 * unit)
                                                    * scale)
                                                    .round()
                                                    / scale),
                                            ),
                                            size(px(unit), px(unit)),
                                        ),
                                        gpui::rgb(0x000000),
                                    ));
                                }
                            }
                        }
                    },
                )
                .w_full()
                .h_full(),
            );
        } else if success || retry {
            visual = visual.child(
                if success {
                    icons::check_circle()
                } else {
                    icons::warning()
                }
                .size_8()
                .text_color(color(if success { SUCCESS } else { WARNING })),
            );
        } else {
            visual = visual.child(crate::motion::spinner("qr-code-generating", cx));
        }
        let phase = match state {
            QrDialogState::Generating => 0,
            QrDialogState::Waiting(_) => 1,
            QrDialogState::Confirming(_) => 2,
            QrDialogState::Expired => 3,
            QrDialogState::Error(_) => 4,
            QrDialogState::Success(_) => 5,
        };
        let explanation = v_flex()
            .id("account-login-explanation")
            .min_w_0()
            .min_h_0()
            .max_h(px(layout.text_height))
            .overflow_y_scroll()
            .gap_2()
            .when(layout.side_by_side, |view| view.flex_1())
            .when(!layout.side_by_side, |view| view.w_full().items_center())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(matches!(state, QrDialogState::Confirming(_)), |row| {
                        row.child(crate::motion::spinner("qr-awaiting-confirmation", cx))
                    })
                    .child(
                        accessible_text("bilibili-login-step", message)
                            .min_w_0()
                            .whitespace_normal()
                            .when(!layout.side_by_side, |text| text.text_center())
                            .font_weight(FontWeight::SEMIBOLD),
                    ),
            )
            .when(!hint.is_empty(), |view| {
                view.child(
                    accessible_text("bilibili-login-detail", hint)
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .when(!layout.side_by_side, |text| text.text_center())
                        .text_sm()
                        .text_color(color(GRAY)),
                )
            });
        v_flex()
            .id("account-login-body")
            .w_full()
            .max_w(px(layout.content_width))
            .min_w_0()
            .min_h_0()
            .max_h(px(layout.content_height))
            .gap(px(layout.gap))
            .items_center()
            .when(layout.side_by_side, |view| view.flex_row())
            .child(crate::motion::enter(
                SharedString::from(format!("qr-visual-{}-{phase}", self.account.generation)),
                visual,
            ))
            .child(explanation)
            .into_any_element()
    }

    fn account_dialog_actions(&self, cx: &mut Context<Self>) -> Div {
        let retry = matches!(
            self.account.dialog,
            Some(QrDialogState::Expired | QrDialogState::Error(_))
        );
        let success = matches!(self.account.dialog, Some(QrDialogState::Success(_)));
        let retry_source = success && self.can_retry_account_source(cx);
        let close = if success {
            primary_pill("account-qr-close").icon(icons::check_circle())
        } else {
            text_button("account-qr-close", cx)
        };
        dialog_actions()
            .child(
                close
                    .label(if retry_source {
                        course2md::tr!("重新预览")
                    } else if success {
                        course2md::tr!("完成")
                    } else {
                        course2md::tr!("取消")
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        let retry = matches!(this.account.dialog, Some(QrDialogState::Success(_)))
                            && this.can_retry_account_source(cx);
                        this.close_account_dialog(cx);
                        window.close_dialog(cx);
                        if retry {
                            this.inspect_source(window, cx);
                        }
                    })),
            )
            .when(retry, |view| {
                view.child(
                    primary_pill("account-qr-retry")
                        .icon(icons::refresh())
                        .label(course2md::tr!("刷新二维码"))
                        .on_click(cx.listener(|this, _, _, cx| this.start_account_qr(cx))),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{AccountUi, QrDialogState, qr_layout, qr_raster};
    use course2md::auth::{AccountProfile, AccountStatus};

    #[test]
    fn qr_layout_keeps_the_complete_code_inside_short_and_regular_dialogs() {
        // The rejected native frame had 196px of body height and a 260px code.
        let short = qr_layout(860., 620., 28.);
        assert!(short.side_by_side);
        assert!(short.code_size <= 196.);
        assert!(short.code_size + short.gap + 28. * 12. <= short.content_width);

        let regular = qr_layout(1068., 768., 14.);
        assert!(!regular.side_by_side);
        assert_eq!(regular.dialog_width, 420.);
        assert!(regular.code_size >= 240.);

        for (width, height) in [(860., 620.), (1068., 768.), (1600., 1000.)] {
            for scale in crate::preferences::FONT_SCALES {
                let layout = qr_layout(width, height, 14. * scale);
                assert!(layout.code_size > 0.);
                assert!(layout.code_size <= layout.content_width);
                assert!(layout.code_size <= layout.content_height);
                if layout.side_by_side {
                    assert!(layout.code_size + layout.gap < layout.content_width);
                    assert!(layout.text_height <= layout.content_height);
                } else {
                    assert!(
                        layout.code_size + layout.gap + layout.text_height
                            <= layout.content_height + 0.001,
                    );
                }
            }
        }
    }

    #[test]
    fn qr_physical_pixels_retain_four_white_modules_after_origin_snapping() {
        for code_size in [196., 195., 260.] {
            for module_count in [21, 41, 57, 77] {
                for scale in [1., 2.] {
                    let (unit, offset) = qr_raster(code_size, module_count, scale);
                    assert!(unit > 0.);
                    assert_eq!((unit * scale).fract(), 0.);
                    for origin in [0., 0.125, 0.25, 0.5, 0.75] {
                        let first = ((origin + offset) * scale).round() / scale;
                        let last = ((origin + offset + (module_count - 1) as f32 * unit) * scale)
                            .round()
                            / scale
                            + unit;
                        assert!(first - origin >= unit * 4. - 0.001);
                        assert!(origin + code_size - last >= unit * 4. - 0.001);
                    }
                }
            }
        }
    }

    #[test]
    fn closing_and_refreshing_reject_late_qr_results() {
        let mut account = AccountUi {
            dialog: Some(QrDialogState::Generating),
            ..Default::default()
        };
        let first = account.generation;
        assert!(account.current(first));
        account.invalidate();
        assert!(!account.current(first));
        account.dialog = Some(QrDialogState::Generating);
        assert!(!account.current(first));
        assert!(account.current(account.generation));
    }

    #[test]
    fn temporary_validation_failure_keeps_login_and_allows_repair() {
        let connected = AccountStatus::Connected(AccountProfile {
            name: "Fixture account".into(),
        });
        let mut account = AccountUi {
            status_generation: 7,
            status: Some(connected.clone()),
            has_saved_login: true,
            checking: true,
            ..Default::default()
        };
        assert!(account.apply_status(7, true, Err("Network unavailable".into())));
        assert_eq!(account.status, Some(connected.clone()));
        assert!(account.has_saved_login);
        assert!(!account.checking);
        assert_eq!(account.login_action(), Some("重新登录 Bilibili"));

        assert!(account.apply_status(7, true, Ok(connected)));
        assert!(account.status_error.is_none());
        assert_eq!(account.login_action(), None);

        assert!(account.apply_status(7, true, Ok(AccountStatus::Expired)));
        assert_eq!(account.login_action(), Some("重新登录 Bilibili"));
        assert!(account.apply_status(7, true, Ok(AccountStatus::Disconnected)));
        assert!(!account.has_saved_login);
        assert_eq!(account.login_action(), Some("登录 Bilibili"));
    }

    #[test]
    fn late_account_check_cannot_replace_a_newly_authenticated_account() {
        let connected = AccountStatus::Connected(AccountProfile {
            name: "New account".into(),
        });
        let mut account = AccountUi {
            status_generation: 4,
            status: Some(connected.clone()),
            has_saved_login: true,
            ..Default::default()
        };
        assert!(!account.apply_status(3, false, Ok(AccountStatus::Disconnected)));
        assert_eq!(account.status, Some(connected));
        assert!(account.has_saved_login);
        assert!(account.status_error.is_none());
    }

    #[test]
    fn source_continuation_requires_the_same_input_and_origin_after_login() {
        let input = "https://www.bilibili.com/video/BVfixture";
        let mut account = AccountUi {
            retry_source: Some((5, input.into())),
            dialog: Some(QrDialogState::Waiting(120)),
            ..Default::default()
        };
        assert!(account.can_resume_source(5, input, true));
        assert!(!account.can_resume_source(6, input, true));
        assert!(!account.can_resume_source(5, "https://b23.tv/changed", true));
        // Settings and an active guide are not the originating source view.
        assert!(!account.can_resume_source(5, input, false));
        account.invalidate();
        account.dialog = Some(QrDialogState::Waiting(180));
        assert!(account.can_resume_source(5, input, true));
        account.close();
        assert!(!account.can_resume_source(5, input, true));
    }
}
