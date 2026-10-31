//! Bilibili account settings and cancellable QR login. No ticket is logged or stored in UI text.
use crate::settings_ui::Resource;
use crate::{Desktop, Field, Page, symbols, ui};
use anyhow::Result;
use course2md::auth::{AccountProfile, AccountStatus, QrPoll, QrSession};
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{color::ColorRole, icons::Icon, prelude::*};
use std::{sync::Arc, time::Duration};

#[derive(Default)]
pub(crate) struct AccountUi {
    generation: u64,
    status_generation: u64,
    dialog: Option<QrDialogState>,
    /// The closed dialog's last state, so it can play its exit.
    closing: Option<QrDialogState>,
    modules: Option<Arc<Vec<Vec<bool>>>>,
    status: Option<AccountStatus>,
    checking: bool,
    has_saved_login: bool,
    status_error: Option<String>,
    /// The current status_error is the logout-incomplete message; a flag so a
    /// copy change cannot silently break the status derivation.
    logout_failed: bool,
    retry_source: Option<(u64, String)>,
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

/// The QR code's size and whether the text sits beside it. A short window keeps the
/// code whole: the text moves beside it before the code shrinks. The code never
/// scrolls, and `body` is the height the dialog leaves for its content.
fn qr_layout(content_width: f32, body: f32, rem_size: f32) -> (f32, bool) {
    let gap = rem_size * (16. / 14.);
    let preferred = 260_f32.min(content_width);
    let text = rem_size * 4.5;
    let beside = body < preferred + gap + text
        && content_width >= preferred.min(body) + gap + rem_size * 12.;
    let code = if beside {
        preferred.min(body)
    } else {
        preferred.min((body - gap - text).max(0.))
    };
    (code.floor(), beside)
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

/// The login dialog's width in design px.
const DIALOG_LOGIN_WIDTH: f32 = 448.;

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
        self.generation = self.generation.wrapping_add(1);
        self.closing = self.dialog.take().or(self.closing.take());
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
                    .icon(symbols::login())
                    .child(login)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_account_dialog(window, cx)),
                    )
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
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_account(cx))),
                )
                .item(
                    MaterialMenuItem::new(course2md::tr!("退出登录"))
                        .leading(Icon::from(symbols::logout()))
                        .divider(true)
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
            title: title.into(),
            status: vec![status],
            busy: self.account.checking,
            lines: self
                .account_detail()
                .map(|detail| (detail, false))
                .into_iter()
                .chain([(policy.to_owned(), false)])
                .collect(),
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
        self.account.retry_source = (!self.onboarding.active
            && matches!(self.page, Page::New)
            && self.online()
            && (self.preview_error.is_some() || self.subtitle_attention_required())
            && course2md::auth::is_bilibili_url(&self.value(Field::Source, cx)))
        .then(|| (self.preview_generation, self.value(Field::Source, cx)));
        self.start_account_qr(cx);
        window.refresh();
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

    /// The login dialog, while it is open or playing its exit.
    pub(crate) fn account_dialog(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let open = self.account.dialog.is_some();
        let Some(state) = self
            .account
            .dialog
            .as_ref()
            .or(self.account.closing.as_ref())
        else {
            return div().into_any_element();
        };
        let retry = matches!(state, QrDialogState::Expired | QrDialogState::Error(_));
        let success = matches!(state, QrDialogState::Success(_));
        let retry_source = success && self.can_retry_account_source(cx);
        let close = cx.listener(|this, _: &ClickEvent, window, cx| {
            let retry = matches!(this.account.dialog, Some(QrDialogState::Success(_)))
                && this.can_retry_account_source(cx);
            this.close_account_dialog(cx);
            if retry {
                this.inspect_source(window, cx);
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
            .headline(course2md::tr!("登录 Bilibili"))
            .width(DIALOG_LOGIN_WIDTH)
            .content(self.account_dialog_content(state, window, cx))
            .action(close)
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
            .rounded(RADIUS_CARD)
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

#[cfg(test)]
mod tests {
    use super::{AccountUi, QrDialogState, qr_layout, qr_raster};
    use course2md::auth::{AccountProfile, AccountStatus};

    #[test]
    fn qr_layout_keeps_the_complete_code_inside_short_and_regular_dialogs() {
        // A short window at 200% text: the text moves beside a whole code.
        let (code, beside) = qr_layout(812., 268., 28.);
        assert!(beside);
        assert!(code <= 268.);
        assert!(code >= 196.);

        let (code, beside) = qr_layout(400., 500., 14.);
        assert!(!beside);
        assert_eq!(code, 260.);

        for (width, height) in [(400., 300.), (812., 268.), (560., 800.)] {
            for scale in crate::preferences::FONT_SCALES {
                let (code, _) = qr_layout(width, height, 14. * scale);
                assert!(code > 0. && code <= width && code <= height);
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
