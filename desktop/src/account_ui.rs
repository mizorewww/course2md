//! Bilibili account settings and its login dialog: the cancellable QR login here, the
//! password and SMS forms in `login_ui`. No ticket is logged or stored in UI text.
use crate::ui::Resource;
use crate::{symbols, ui};
use course2md::auth::{AccountStatus, QrPoll, QrSession};
use desktop_accounts::account::*;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{color::ColorRole, icons::Icon, prelude::*};
use std::{sync::Arc, time::Duration};

/// The login dialog's width in design px.
const DIALOG_LOGIN_WIDTH: f32 = 448.;

/// What the sign-ins ask of the window.
pub(crate) enum AccountsEvent {
    /// Open the Bilibili sign-in, deciding what it may retry.
    OpenAccount,
    /// Open the YouTube connection, deciding what it may retry.
    OpenYoutube,
    /// Signed in to Bilibili from a source preview that failed: retry it, if it is
    /// still the one shown and still needs it.
    SignedIn { source: (u64, String) },
    /// Connected YouTube from a source preview that failed: retry it, if it is still
    /// the one shown.
    YoutubeConnected { source: (u64, String) },
    /// Quit and open the app again, so macOS applies the Full Disk Access just
    /// given; the guide opens again after the reopen.
    Relaunch,
}

/// The sign-ins: the Bilibili account and its login dialog, and the YouTube
/// connection. Pages draw its rows and dialogs; what the sign-ins change about the
/// rest of the window goes out as `AccountsEvent`.
pub(crate) struct AccountsView {
    pub(crate) account: AccountUi,
    /// The password and SMS login fields, made when the login dialog first opens.
    pub(crate) login_inputs: Option<crate::login_ui::LoginInputs>,
    pub(crate) youtube: desktop_accounts::youtube::YoutubeUi,
    /// The window the dialogs open in; deferred input changes return to it.
    pub(crate) window: AnyWindowHandle,
}

impl EventEmitter<AccountsEvent> for AccountsView {}

impl AccountsView {
    pub(crate) fn new(window: &Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            account: AccountUi::default(),
            login_inputs: None,
            youtube: Default::default(),
            window: window.window_handle(),
        };
        this.refresh_account(cx);
        this.load_youtube(cx);
        this
    }
}

impl AccountsView {
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
            let received = task.await;
            let _ = this.update(cx, |this, cx| {
                let (saved, result) = received.unwrap_or_else(|| {
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

    /// The account's row, for settings and first use alike.
    pub(crate) fn account_row(
        &self,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let saved = self.account.has_saved_login;
        let status = if self.account.checking {
            (StatusKind::Progress, course2md::tr!("验证中"))
        } else if self.account.status_error.is_some() {
            (StatusKind::Warning, course2md::tr!("暂时无法验证"))
        } else {
            match &self.account.status {
                Some(AccountStatus::Expired) => (StatusKind::Warning, course2md::tr!("登录已失效")),
                Some(AccountStatus::Connected(_)) => {
                    (StatusKind::Success, course2md::tr!("已登录"))
                }
                _ if saved => (StatusKind::Neutral, course2md::tr!("已保存登录信息")),
                _ => (StatusKind::Neutral, course2md::tr!("未登录")),
            }
        };
        let policy = if saved {
            course2md::tr!("退出登录不会删除笔记")
        } else {
            course2md::tr!("登录后可读取账号有权访问的视频与字幕")
        };
        let mut actions = Vec::new();
        if let Some(login) = self.account.login_action() {
            actions.push(
                material_button("account-login", ButtonVariant::Outlined)
                    .tone(ButtonTone::Accent)
                    .icon(symbols::bilibili())
                    .child(login)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::OpenAccount)))
                    .into_any_element(),
            );
        }
        // Only a saved login has anything to check again or sign out of.
        if saved {
            let menu_id = ElementId::from("account-actions");
            let trigger = MaterialTooltip::plain("account-actions-tip", course2md::tr!("账号操作"))
                .trigger(
                    IconButton::new(
                        "account-actions-button",
                        symbols::more_vert(),
                        course2md::tr!("{} 操作", course2md::tr!("Bilibili 账号")),
                    )
                    .on_click(ui::menu_key_toggle(&menu_id, window, cx)),
                );
            let items = MaterialMenu::new("account-action-items")
                .width(240.)
                .item(
                    MaterialMenuItem::new(course2md::tr!("重新检查"))
                        .leading(Icon::from(symbols::refresh()))
                        .disabled(self.account.checking)
                        // Signing out stands apart, below the line.
                        .divider(true)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_account(cx))),
                )
                .item(
                    MaterialMenuItem::new(course2md::tr!("退出登录"))
                        .leading(Icon::from(symbols::logout()))
                        .on_click(cx.listener(|this, _, _, cx| this.clear_account(cx))),
                );
            actions.push(
                ui::menu(menu_id, trigger, items, window, cx)
                    .anchor(Anchor::TopRight)
                    .into_any_element(),
            );
        }
        Resource {
            id: "bilibili-account".into(),
            icon: symbols::tv().into(),
            tint: Tint::Pink,
            title: title.into(),
            status: vec![status],
            busy: self.account.checking,
            lines: self
                .account_detail()
                .map(|detail| (detail, false))
                .into_iter()
                .chain([(policy.to_owned(), false)])
                .collect(),
            mono: false,
            below: Vec::new(),
            actions,
        }
        .row(cx)
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
        h_flex()
            .w_full()
            .items_center()
            .gap(rem(12.))
            .flex_wrap()
            .child(
                TileScale::Regular
                    .support("source-bilibili-account-status", status)
                    .flex_1(),
            )
            .child(
                material_button("source-account-refresh", ButtonVariant::Text)
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重新检查"))
                    .disabled(self.account.checking)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_account(cx))),
            )
            .when_some(self.account.login_action(), |view, label| {
                view.child(
                    material_button("source-account-login", ButtonVariant::Text)
                        .icon(symbols::login())
                        .child(label)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::OpenAccount))),
                )
            })
    }

    fn clear_account(&mut self, cx: &mut Context<Self>) {
        self.account.invalidate();
        self.account.status_generation = self.account.status_generation.wrapping_add(1);
        self.account.checking = false;
        let generation = self.account.status_generation;
        // Removing the keychain/file credential can block; keep it off the UI thread.
        let task = crate::spawn_blocking_io(course2md::auth::clear_bilibili_login);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.account.status_generation != generation {
                    return;
                }
                match result {
                    Some(Ok(_)) => {
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
        self.clear_login_secrets(cx);
        cx.notify();
    }

    /// Opens the sign-in dialog. `retry_source` is the source preview to try again once
    /// signed in, as the window saw it when the dialog opened.
    pub fn open_account_dialog(
        &mut self,
        retry_source: Option<(u64, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.account.dialog.is_some() {
            return;
        }
        self.account.retry_source = retry_source;
        self.ensure_login_inputs(window, cx);
        if self.account.method == LoginMethod::Qr {
            self.start_account_qr(cx);
        } else {
            self.account.invalidate();
            self.account.dialog = Some(QrDialogState::Generating);
        }
        window.refresh();
    }

    pub(crate) fn start_account_qr(&mut self, cx: &mut Context<Self>) {
        self.account.invalidate();
        self.account.dialog = Some(QrDialogState::Generating);
        let generation = self.account.generation;
        let task = crate::spawn_blocking_io(QrSession::generate);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let mut session = match result {
                Some(Ok(session)) => session,
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
                let Some((next, result)) = task.await else {
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
                        let save = crate::spawn_blocking_result(move || login.save());
                        QrPollResult::Authenticated(save.await)
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
                                this.account.signed_in(profile.clone());
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

    /// The login dialog, while it is open or playing its exit.
    /// The sign-in dialog. `source_ready` says the source preview it was opened from can
    /// be tried again once signed in.
    pub(crate) fn account_dialog(
        &self,
        source_ready: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.account.dialog.is_some();
        let Some(state) = self
            .account
            .dialog
            .as_ref()
            .or(self.account.closing.as_ref())
        else {
            return div().into_any_element();
        };
        let success = matches!(state, QrDialogState::Success(_));
        let qr = self.account.method == LoginMethod::Qr || success;
        let retry = qr && matches!(state, QrDialogState::Expired | QrDialogState::Error(_));
        let retry_source = success && source_ready;
        let close = cx.listener(|this, _: &ClickEvent, _, cx| {
            let retry = matches!(this.account.dialog, Some(QrDialogState::Success(_)))
                .then(|| this.account.retry_source.clone())
                .flatten();
            this.close_account_dialog(cx);
            if let Some(source) = retry {
                cx.emit(AccountsEvent::SignedIn { source });
            }
        });
        let close = material_button(
            "account-qr-close",
            if success {
                ButtonVariant::Filled
            } else {
                ButtonVariant::Text
            },
        )
        .when(success, |button| button.icon(symbols::check_circle()))
        .child(if retry_source {
            course2md::tr!("重新预览")
        } else if success {
            course2md::tr!("完成")
        } else {
            course2md::tr!("取消")
        })
        .on_click(close);
        MaterialDialog::new("bilibili-dialog")
            .container(ColorRole::SURFACE)
            .headline(course2md::tr!("登录 Bilibili"))
            .width(DIALOG_LOGIN_WIDTH)
            .content(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(20.))
                    // A phone check continues the login it interrupted; no method to switch.
                    .when(!success && self.account.risk.is_none(), |content| {
                        content.child(div().px(rem(24.)).child(self.login_method_group(cx)))
                    })
                    .child(if qr {
                        self.account_dialog_content(state, window, cx)
                    } else {
                        self.login_form(window, cx)
                    }),
            )
            .action(close)
            .when_some(
                (!qr).then(|| self.login_form_action(cx)).flatten(),
                |dialog, action| dialog.action(action),
            )
            .when(retry, |dialog| {
                dialog.action(
                    material_button("account-qr-retry", ButtonVariant::Filled)
                        .icon(symbols::refresh())
                        .child(course2md::tr!("刷新二维码"))
                        .on_click(cx.listener(|this, _, _, cx| this.start_account_qr(cx))),
                )
            })
            .dismiss_on_scrim(false)
            .modal(open)
            .on_dismiss({
                let this = cx.weak_entity();
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.close_account_dialog(cx));
                }
            })
            .into_any_element()
    }

    fn account_dialog_content(
        &self,
        state: &QrDialogState,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let rem_size = f32::from(window.rem_size());
        let width = (DIALOG_LOGIN_WIDTH / 14. * rem_size)
            .min(f32::from(window.viewport_size().width) - 48.)
            - rem_size * (48. / 14.);
        let body = f32::from(crate::settings_ui::dialog_body_height(window));
        let (code_size, beside) = qr_layout(width, body, rem_size);
        let (message, hint): (String, String) = match state {
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
        let confirming = matches!(state, QrDialogState::Confirming(_));
        let (status, tone) = match state {
            QrDialogState::Success(_) => (
                Some(symbols::check_circle()),
                (ColorRole::SUCCESS, ColorRole::SUCCESS_BG),
            ),
            QrDialogState::Expired | QrDialogState::Error(_) => (
                Some(symbols::warning()),
                (ColorRole::WARNING, ColorRole::WARNING_BG),
            ),
            _ => (None, (ColorRole::PRIMARY, ColorRole::INSET)),
        };
        let visual = h_flex()
            .size(px(code_size))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded(RADIUS_LARGE)
            .bg(color(tone.1));
        let visual = if let Some(modules) = self.account.modules.clone() {
            // A real QR code keeps its high-contrast white scanning surface.
            visual
                .rounded(px(0.))
                .bg(gpui::rgb(0xffffff))
                .child(qr_canvas(modules))
        } else if let Some(icon) = status {
            visual.child(Icon::from(icon).size(rem(32.)).color(color(tone.0)))
        } else {
            visual.child(spinner("qr-code-generating", message.clone(), cx))
        };
        v_flex()
            .w_full()
            .min_w_0()
            .px(rem(24.))
            .gap(rem(16.))
            .items_center()
            .when(beside, |view| view.flex_row())
            .child(visual)
            .child(
                v_flex()
                    .min_w_0()
                    .gap(rem(8.))
                    .when(beside, |view| view.flex_1())
                    .when(!beside, |view| view.w_full().items_center())
                    .child(
                        h_flex()
                            .gap(rem(8.))
                            .items_center()
                            .when(confirming, |row| {
                                row.child(spinner("qr-awaiting-confirmation", message.clone(), cx))
                            })
                            .child(
                                accessible_text("bilibili-login-step", message.clone())
                                    .min_w_0()
                                    .whitespace_normal()
                                    .when(!beside, |text| text.text_center())
                                    .font_weight(FontWeight::MEDIUM),
                            ),
                    )
                    .when(!hint.is_empty(), |view| {
                        view.child(
                            TileScale::Regular
                                .support("bilibili-login-detail", hint)
                                .when(!beside, |text| text.text_center()),
                        )
                    }),
            )
    }
}

/// Whole physical-pixel modules and a four-module quiet zone keep the edges crisp.
fn qr_canvas(modules: Arc<Vec<Vec<bool>>>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let scale = window.scale_factor();
            let (unit, offset) = qr_raster(
                bounds.size.width.as_f32().min(bounds.size.height.as_f32()),
                modules.len(),
                scale,
            );
            let snap = |position: f32| px((position * scale).round() / scale);
            for (y, row) in modules.iter().enumerate() {
                for (x, dark) in row.iter().enumerate() {
                    if *dark {
                        let origin = point(
                            snap(bounds.origin.x.as_f32() + offset + x as f32 * unit),
                            snap(bounds.origin.y.as_f32() + offset + y as f32 * unit),
                        );
                        window.paint_quad(fill(
                            Bounds::new(origin, size(px(unit), px(unit))),
                            gpui::rgb(0x000000),
                        ));
                    }
                }
            }
        },
    )
    .w_full()
    .h_full()
}
