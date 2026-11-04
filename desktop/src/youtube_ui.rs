//! The YouTube connection: its row in Settings › 来源账号 and the dialog that
//! connects a browser's YouTube session. yt-dlp reads the browser's cookies; this
//! app keeps only which browser to use.
//!
//! On macOS a browser's data folder is closed to apps without Full Disk Access, and
//! yt-dlp then reports no cookie database. The dialog looks before the person
//! connects and, while the folder is closed, guides them through System Settings,
//! looking again every [`ACCESS_POLL`] until the folder opens.
use crate::{
    account_ui::{AccountsEvent, AccountsView},
    symbols,
    ui::Resource,
};
use course2md::auth::youtube::{self, Access, Browser, BrowserChoice, Reader, Session};
use desktop_accounts::youtube::*;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{color::ColorRole, icons::Icon, prelude::*};
use std::{path::PathBuf, time::Instant};

const YOUTUBE_DIALOG_WIDTH: f32 = 480.;

/// One look at `browser`'s cookie folder. Only macOS has a switch to guide the
/// person to; elsewhere a refusal is the check's to report.
fn look(browser: Browser) -> Access {
    match browser.access() {
        Access::Blocked if !cfg!(target_os = "macos") => Access::Absent,
        access => access,
    }
}

/// The app bundle this process runs from, as System Settings lists it. Found
/// once: drawing the guide asks for it on every frame.
fn app_bundle() -> Option<&'static PathBuf> {
    static BUNDLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    BUNDLE
        .get_or_init(|| app_bundle_of(&std::env::current_exe().ok()?))
        .as_ref()
}

fn resume_marker_path() -> PathBuf {
    resume_marker(&course2md::config::cache_dir())
}

/// Capture runs report a system action instead of taking it: they open no System
/// Settings or Finder window and do not quit or reopen the app.
fn stubbed(action: &str) -> bool {
    #[cfg(feature = "capture")]
    if std::env::var_os("COURSE2MD_CAPTURE").is_some() {
        eprintln!("capture: {action} (not taken)");
        return true;
    }
    let _ = action;
    false
}

/// The note that brings the person back to the guide, on `browser`, after the
/// app reopens.
fn leave_resume_marker(browser: Option<Browser>) {
    drop(crate::spawn_blocking_io(move || {
        let marker = resume_marker_path();
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(marker, browser.map_or("", Browser::id));
    }));
}

impl AccountsView {
    /// Reads the saved browser choice and whether its folder is open; no cookie
    /// is read until the person asks. A note left by a guide the person stepped
    /// away from (to give access, which may reopen the app) opens it again.
    pub(crate) fn load_youtube(&mut self, cx: &mut Context<Self>) {
        let task = crate::spawn_blocking_result(|| {
            let choice = youtube::saved_choice()?;
            let access = choice.as_ref().map(|choice| look(choice.browser));
            let marker = resume_marker_path();
            let resume = resume_due(&marker, std::time::SystemTime::now());
            if resume.is_some() {
                let _ = std::fs::remove_file(&marker);
            }
            anyhow::Ok((choice, access, resume))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((choice, access, resume)) => {
                        this.youtube.choice = choice;
                        this.youtube.access = access;
                        this.youtube.load_error = None;
                        if resume.is_some() && this.youtube.dialog.is_none() {
                            this.youtube.resume_guide = resume;
                            cx.emit(AccountsEvent::OpenYoutube);
                        }
                    }
                    Err(error) => this.youtube.load_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the connection dialog. `retry_source` is the source preview to try again
    /// once connected, as the window saw it when the dialog opened.
    pub(crate) fn open_youtube_dialog(
        &mut self,
        retry_source: Option<(u64, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.youtube.dialog.is_some() {
            return;
        }
        let resumed = self.youtube.resume_guide.take();
        self.youtube.retry_source = retry_source;
        self.youtube.generation = self.youtube.generation.wrapping_add(1);
        let mut dialog = YoutubeDialog::new(Browser::installed(), self.youtube.choice.as_ref());
        // Back from giving access: on the browser the guide was for.
        if let Some(index) = resumed
            .flatten()
            .and_then(|browser| dialog.browsers.iter().position(|b| *b == browser))
        {
            dialog.select_browser(Some(index), None);
        }
        if let Some(browser) = dialog.selected_browser() {
            // One folder listing per look, as cheap as finding the installed
            // browsers above; a protected folder answers at once.
            let access = look(browser);
            if resumed.is_some() {
                dialog.guide = AccessGuide::resumed(browser, access, Instant::now());
            } else {
                dialog.probed(browser, access, true, Instant::now());
            }
        }
        self.youtube.dialog = Some(dialog);
        self.watch_youtube_access(cx);
        window.refresh();
        cx.notify();
    }

    fn close_youtube_dialog(&mut self, cx: &mut Context<Self>) {
        let resume = matches!(
            self.youtube.dialog.as_ref().map(|dialog| &dialog.step),
            Some(YoutubeStep::Connected)
        )
        .then(|| self.youtube.retry_source.clone())
        .flatten();
        self.youtube.close();
        // Closed on purpose: nothing to come back to on the next launch, and the
        // saved browser's folder may have opened meanwhile.
        let browser = self.youtube.choice.as_ref().map(|choice| choice.browser);
        let task = crate::spawn_blocking_io(move || {
            let _ = std::fs::remove_file(resume_marker_path());
            browser.map(look)
        });
        cx.spawn(async move |this, cx| {
            if let Some(access) = task.await {
                let _ = this.update(cx, |this, cx| {
                    if this.youtube.choice.as_ref().map(|choice| choice.browser) == browser {
                        this.youtube.access = access;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        if let Some(source) = resume {
            cx.emit(AccountsEvent::YoutubeConnected { source });
        }
        cx.notify();
    }

    /// Looks at the chosen browser's folder again every [`ACCESS_POLL`] while the
    /// guide waits for access, until it opens or the dialog closes.
    fn watch_youtube_access(&mut self, cx: &mut Context<Self>) {
        let polls = |ui: &YoutubeUi| {
            ui.dialog
                .as_ref()
                .filter(|dialog| dialog.step == YoutubeStep::Choose)
                .and_then(|dialog| {
                    dialog
                        .guide
                        .as_ref()
                        .filter(|guide| guide.polls())
                        .map(|guide| guide.browser)
                })
        };
        if self.youtube.polling || polls(&self.youtube).is_none() {
            return;
        }
        self.youtube.polling = true;
        let window = self.window;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ACCESS_POLL).await;
                let Ok(Some(browser)) = this.update(cx, |this, _| polls(&this.youtube)) else {
                    break;
                };
                let Some(access) = crate::spawn_blocking_io(move || look(browser)).await else {
                    continue;
                };
                // Whether the person is in the app, or away (in System Settings).
                let active = cx
                    .update_window(window, |_, window, _| window.is_window_active())
                    .unwrap_or(false);
                let _ = this.update(cx, |this, cx| {
                    if let Some(dialog) = &mut this.youtube.dialog {
                        dialog.probed(browser, access, active, Instant::now());
                    }
                    if this.youtube.choice.as_ref().map(|choice| choice.browser) == Some(browser) {
                        this.youtube.access = Some(access);
                    }
                    cx.notify();
                });
            }
            let _ = this.update(cx, |this, _| this.youtube.polling = false);
        })
        .detach();
    }

    /// Step 1: System Settings › Privacy & Security › Full Disk Access. A note
    /// left for the next launch brings the person back here if macOS reopens the app.
    fn open_access_settings(&mut self, cx: &mut Context<Self>) {
        if !stubbed(&format!("open {}", youtube::FULL_DISK_ACCESS_SETTINGS)) {
            cx.open_url(youtube::FULL_DISK_ACCESS_SETTINGS);
        }
        let mut browser = None;
        if let Some(guide) = self
            .youtube
            .dialog
            .as_mut()
            .and_then(|dialog| dialog.guide.as_mut())
        {
            guide.opened_settings(Instant::now());
            browser = Some(guide.browser);
        }
        leave_resume_marker(browser);
        self.watch_youtube_access(cx);
        cx.notify();
    }

    /// Step 2, when course2md is not in the list: its bundle in Finder, to drag in.
    fn reveal_app(&mut self, cx: &mut Context<Self>) {
        let Some(bundle) = app_bundle() else {
            return;
        };
        if !stubbed(&format!("reveal {}", bundle.display())) {
            cx.reveal_path(bundle);
        }
    }

    /// Has yt-dlp read the chosen browser's session; connects it when signed in.
    fn check_youtube(&mut self, choice: BrowserChoice, save: bool, cx: &mut Context<Self>) {
        let generation = self.youtube.generation;
        if save {
            if let Some(dialog) = &mut self.youtube.dialog {
                dialog.step = YoutubeStep::Checking;
            }
        } else {
            self.youtube.checking = true;
            self.youtube.recheck = Some(generation);
        }
        let task = crate::spawn_blocking_result(move || {
            // A browser folder macOS keeps from the app needs no yt-dlp to report.
            youtube::ensure_readable(&choice, Reader::App)?;
            let program = course2md::ytdlp::resolve_in(Some(&crate::environment::tool_path()))
                .ok_or_else(|| {
                    anyhow::anyhow!(course2md::tr!(
                        "缺少读取视频所需的组件，请在设置中检查运行环境"
                    ))
                })?;
            // Only the check runs here: the choice is saved below, and only
            // while the dialog that asked for it is still open.
            let session = youtube::check_session(&program, &choice, Reader::App)?;
            anyhow::Ok((choice, session))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if !save {
                    // Even a stale re-check ends its own busy state.
                    this.youtube.finish_recheck(generation);
                    cx.notify();
                }
                if !this.youtube.current(generation) {
                    return;
                }
                let result = match result {
                    // A small atomic write, once the person is still waiting for it.
                    Ok((choice, Session::SignedIn)) if save => {
                        youtube::save_choice(&choice).map(|()| (choice, Session::SignedIn))
                    }
                    other => other,
                };
                let step = match result {
                    Ok((choice, Session::SignedIn)) => {
                        if save {
                            this.youtube.choice = Some(choice);
                        }
                        YoutubeStep::Connected
                    }
                    Ok((_, Session::SignedOut)) => YoutubeStep::SignedOut,
                    Err(error) => match error.downcast::<youtube::BrowserReadError>() {
                        Ok(read) => YoutubeStep::Failed {
                            summary: read.summary,
                            detail: read.detail,
                            blocked: read.blocked,
                        },
                        Err(error) => YoutubeStep::Failed {
                            summary: format!("{error:#}"),
                            detail: String::new(),
                            blocked: false,
                        },
                    },
                };
                let refused = matches!(step, YoutubeStep::Failed { blocked: true, .. });
                if refused && this.youtube.choice.is_some() && !save {
                    this.youtube.access = Some(Access::Blocked);
                }
                if save && let Some(dialog) = &mut this.youtube.dialog {
                    if refused {
                        // The app's look found the folder open but yt-dlp did not:
                        // the grant applies once the app reopens.
                        let access = dialog.selected_browser().map_or(Access::Blocked, look);
                        dialog.check_blocked(access, Instant::now());
                    } else {
                        dialog.step = step.clone();
                    }
                }
                this.youtube.status = Some(step);
                this.watch_youtube_access(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn disconnect_youtube(&mut self, cx: &mut Context<Self>) {
        self.youtube.generation = self.youtube.generation.wrapping_add(1);
        let task = crate::spawn_blocking_io(youtube::clear_youtube_login);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Some(Ok(_)) => {
                        this.youtube.choice = None;
                        this.youtube.status = None;
                        this.youtube.load_error = None;
                    }
                    _ => {
                        this.youtube.load_error =
                            Some(course2md::tr!("断开未完成，原连接已保留").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The YouTube row in Settings › 来源账号.
    pub(crate) fn youtube_row(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let connected = self.youtube.choice.is_some();
        // The saved browser's folder is closed to the app: every YouTube run would
        // fail to read it, so the row says so before one does.
        let blocked = connected && self.youtube.access == Some(Access::Blocked);
        let status = if self.youtube.checking {
            (StatusKind::Progress, course2md::tr!("验证中"))
        } else if blocked {
            (StatusKind::Warning, course2md::tr!("需要完全磁盘访问权限"))
        } else {
            match (&self.youtube.status, connected) {
                (Some(YoutubeStep::SignedOut), true) => {
                    (StatusKind::Warning, course2md::tr!("浏览器未登录"))
                }
                (Some(YoutubeStep::Failed { .. }), true) => {
                    (StatusKind::Warning, course2md::tr!("暂时无法验证"))
                }
                (_, true) => (StatusKind::Success, course2md::tr!("已连接")),
                (_, false) => (StatusKind::Neutral, course2md::tr!("未连接")),
            }
        };
        let mut lines = Vec::new();
        if let Some(choice) = &self.youtube.choice {
            lines.push((course2md::tr!("使用 {} 中的登录", choice.label()), false));
        }
        if let Some(error) = &self.youtube.load_error {
            lines.push((error.clone(), true));
        } else if blocked {
            lines.push((
                course2md::tr!("course2md 获得完全磁盘访问权限前，YouTube 视频无法使用这个登录")
                    .into(),
                false,
            ));
        } else if let Some(YoutubeStep::Failed { summary, .. }) = &self.youtube.status {
            lines.push((summary.clone(), true));
        }
        if !connected {
            lines.push((
                course2md::tr!("YouTube 要求登录时，可使用浏览器中的 YouTube 登录").into(),
                false,
            ));
        }
        let mut actions = Vec::new();
        if blocked {
            actions.push(
                material_button("youtube-allow-access", ButtonVariant::Outlined)
                    .tone(ButtonTone::Accent)
                    .icon(symbols::lock_open())
                    .child(course2md::tr!("设置权限"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::OpenYoutube)))
                    .into_any_element(),
            );
        } else if !connected || matches!(self.youtube.status, Some(YoutubeStep::SignedOut)) {
            actions.push(
                material_button("youtube-connect", ButtonVariant::Outlined)
                    .tone(ButtonTone::Accent)
                    .icon(symbols::smart_display())
                    .child(if connected {
                        course2md::tr!("重新连接 YouTube")
                    } else {
                        course2md::tr!("连接 YouTube")
                    })
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::OpenYoutube)))
                    .into_any_element(),
            );
        }
        if let Some(choice) = self.youtube.choice.clone() {
            let menu_id = ElementId::from("youtube-actions");
            let trigger = MaterialTooltip::plain("youtube-actions-tip", course2md::tr!("账号操作"))
                .trigger(
                    IconButton::new(
                        "youtube-actions-button",
                        symbols::more_vert(),
                        course2md::tr!("{} 操作", course2md::tr!("YouTube 账号")),
                    )
                    .on_click(crate::ui::menu_key_toggle(&menu_id, window, cx)),
                );
            let items = MaterialMenu::new("youtube-action-items")
                .width(240.)
                .item(
                    MaterialMenuItem::new(course2md::tr!("重新检查"))
                        .leading(Icon::from(symbols::refresh()))
                        .disabled(self.youtube.checking)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.check_youtube(choice.clone(), false, cx)
                        })),
                )
                .item(
                    MaterialMenuItem::new(course2md::tr!("更换浏览器"))
                        .leading(Icon::from(symbols::web()))
                        // Disconnecting stands apart, below the line.
                        .divider(true)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::OpenYoutube))),
                )
                .item(
                    MaterialMenuItem::new(course2md::tr!("断开连接"))
                        .leading(Icon::from(symbols::logout()))
                        .on_click(cx.listener(|this, _, _, cx| this.disconnect_youtube(cx))),
                );
            actions.push(
                crate::ui::menu(menu_id, trigger, items, window, cx)
                    .anchor(Anchor::TopRight)
                    .into_any_element(),
            );
        }
        Resource {
            id: "youtube-account".into(),
            icon: symbols::smart_display().into(),
            tint: Tint::Orange,
            title: course2md::tr!("YouTube 账号").into(),
            status: vec![status],
            busy: self.youtube.checking,
            lines,
            mono: false,
            below: Vec::new(),
            actions,
        }
        .row(cx)
    }

    /// Connecting a browser's YouTube session.
    pub(crate) fn youtube_dialog(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let open = self.youtube.dialog.is_some();
        let Some(dialog) = self
            .youtube
            .dialog
            .as_ref()
            .or(self.youtube.closing.as_ref())
        else {
            return div().into_any_element();
        };
        let checking = dialog.step == YoutubeStep::Checking;
        let connected = dialog.step == YoutubeStep::Connected;
        let names: Vec<_> = dialog
            .browsers
            .iter()
            .map(|browser| browser.name().to_owned())
            .collect();
        let browser = dialog.selected_browser();
        // The Full Disk Access guide, while it is what stands in the way (or has
        // just been cleared): only before a check, which reports for itself.
        let guide = dialog
            .guide
            .clone()
            .filter(|_| dialog.step == YoutubeStep::Choose);
        let guiding = guide
            .as_ref()
            .is_some_and(|guide| guide.step != AccessStep::Granted);
        let mut body = v_flex().w_full().min_w_0().px(rem(24.)).gap(rem(16.));
        if connected {
            let label = dialog
                .choice()
                .map(|choice| choice.label())
                .unwrap_or_default();
            body = body.child(
                h_flex()
                    .gap(rem(12.))
                    .items_center()
                    .child(
                        Icon::from(symbols::check_circle())
                            .size(rem(24.))
                            .color(color(ColorRole::SUCCESS)),
                    )
                    .child(
                        TileScale::Regular
                            .support(
                                "youtube-connected",
                                course2md::tr!(
                                    "已连接 {} 中的 YouTube 登录。打开 YouTube 视频时会使用它。",
                                    label
                                ),
                            )
                            .whitespace_normal()
                            .flex_1()
                            .min_w_0(),
                    ),
            );
        } else {
            // While macOS keeps the folder closed, why it does and what course2md
            // will and will not do with the sign-in take the explanation's place.
            body = body.child(match (&guide, guiding) {
                (Some(guide), true) => supporting_info(
                    "youtube-access-why",
                    course2md::tr!(
                        "macOS 只允许有“完全磁盘访问权限”的应用读取 {} 的数据。获得权限后，yt-dlp 会在这台 Mac 上读取这个浏览器的 Cookie，只在打开 YouTube 视频时用来访问 YouTube；course2md 不保存它们（连接时的检查会把它们短暂写入私有临时文件，检查结束即删除）。这项权限也让 course2md 能读取其他受保护的文件，你可以随时在系统设置中关闭。",
                        guide.browser.name()
                    ),
                ),
                _ => supporting_info(
                    "youtube-explain",
                    course2md::tr!(
                        "YouTube 不允许在其他应用中用密码登录。course2md 会让 yt-dlp 读取所选浏览器中的 YouTube 登录（Cookie），只在打开 YouTube 视频时使用，不保存这些 Cookie。连接时的检查会让 yt-dlp 把浏览器的 Cookie 短暂写入本机的私有临时文件，检查结束即删除。"
                    ),
                ),
            });
            // The standing risk comes before the choice, so it stays in view above
            // a long check result and beside the action it is about. While the
            // guide stands in the way there is no connecting to warn about, and
            // its steps keep the room.
            body = body.when(!guiding, |body| {
                body.child(notice_banner(
                    "youtube-risk",
                    NoticeKind::Warning,
                    course2md::tr!(
                        "用账号下载可能让 YouTube 限制或封禁这个账号。请适度使用，或使用不常用的账号。"
                    ),
                ))
            });
            if names.is_empty() {
                body = body.child(notice_banner(
                    "youtube-no-browser",
                    NoticeKind::Warning,
                    course2md::tr!("没有找到可读取的浏览器。请安装并登录 Chrome、Edge、Firefox 或 Safari 后重试。"),
                ));
            } else {
                body = body.child(
                    ExposedDropdown::new("youtube-browser", course2md::tr!("浏览器"))
                        .variant(TextFieldVariant::Outlined)
                        .shape(TextFieldShape::Rounded)
                        .leading_icon(Icon::from(symbols::web()))
                        .options(names)
                        .selected(dialog.browser)
                        .full_width()
                        .open(dialog.browser_open)
                        .disabled(checking)
                        .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                            if let Some(dialog) = &mut this.youtube.dialog {
                                dialog.browser_open = *open;
                            }
                            cx.notify();
                        }))
                        .on_select(cx.listener(|this, index: &usize, _, cx| {
                            if let Some(dialog) = &mut this.youtube.dialog {
                                dialog.browser_open = false;
                                dialog.select_browser(Some(*index), None);
                                // The newly chosen browser's folder, looked at once now.
                                if let Some(browser) = dialog.selected_browser() {
                                    dialog.probed(browser, look(browser), false, Instant::now());
                                }
                            }
                            this.watch_youtube_access(cx);
                            cx.notify();
                        })),
                );
                if !dialog.profiles.is_empty() {
                    body = body.child(
                        ExposedDropdown::new("youtube-profile", course2md::tr!("浏览器个人资料"))
                            .variant(TextFieldVariant::Outlined)
                            .shape(TextFieldShape::Rounded)
                            .leading_icon(Icon::from(symbols::person()))
                            .options(dialog.profiles.iter().map(|profile| profile.name.clone()))
                            .selected(dialog.profile)
                            .full_width()
                            .open(dialog.profile_open)
                            .disabled(checking)
                            .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                                if let Some(dialog) = &mut this.youtube.dialog {
                                    dialog.profile_open = *open;
                                }
                                cx.notify();
                            }))
                            .on_select(cx.listener(|this, index: &usize, _, cx| {
                                if let Some(dialog) = &mut this.youtube.dialog {
                                    dialog.profile_open = false;
                                    dialog.profile = Some(*index);
                                    dialog.step = YoutubeStep::Choose;
                                }
                                cx.notify();
                            })),
                    );
                }
                // A keychain prompt is not what stands in the way of a closed folder,
                // and Safari's note is the guide itself.
                if let Some(note) = browser.and_then(browser_note).filter(|_| guide.is_none()) {
                    body = body.child(supporting_info("youtube-browser-note", note));
                }
            }
            if let Some(guide) = &guide {
                body = body.child(self.access_guide(guide, cx));
            }
            // The check's result sits under the choice it is about, next to the action.
            match &dialog.step {
                YoutubeStep::Checking => {
                    body = body.child(
                        h_flex()
                            .gap(rem(12.))
                            .items_center()
                            .child(spinner(
                                "youtube-checking",
                                course2md::tr!("正在读取浏览器中的登录"),
                                cx,
                            ))
                            .child(TileScale::Regular.support(
                                "youtube-checking-text",
                                course2md::tr!("正在读取浏览器中的登录"),
                            )),
                    );
                }
                YoutubeStep::SignedOut => {
                    body = body.child(notice_banner(
                        "youtube-signed-out",
                        NoticeKind::Danger,
                        course2md::tr!(
                            "这个浏览器中没有登录 YouTube。请先在浏览器中登录 YouTube，再重新检查。"
                        ),
                    ));
                }
                YoutubeStep::Failed {
                    summary, detail, ..
                } => {
                    let text = if detail.is_empty() {
                        summary.clone()
                    } else {
                        format!("{summary}\n{detail}")
                    };
                    body = body.child(notice_banner("youtube-failed", NoticeKind::Danger, text));
                }
                _ => {}
            }
        }
        let close = material_button(
            "youtube-close",
            if connected {
                ButtonVariant::Filled
            } else {
                ButtonVariant::Text
            },
        )
        .when(connected, |button| button.icon(symbols::check_circle()))
        .child(if connected {
            course2md::tr!("完成")
        } else {
            course2md::tr!("取消")
        })
        .on_click(cx.listener(|this, _, _, cx| this.close_youtube_dialog(cx)));
        let choice = dialog.choice();
        let _ = window;
        // The dialog's one main action is the guide's next step while it guides;
        // otherwise (and once access is given) the check.
        let check = || {
            material_button("youtube-check", ButtonVariant::Filled)
                .icon(symbols::login())
                .child(course2md::tr!("检查并连接"))
                .disabled(checking || choice.is_none() || guiding)
                .on_click(cx.listener({
                    let choice = choice.clone();
                    move |this, _, _, cx| {
                        if let Some(choice) = choice.clone() {
                            if let Some(dialog) = &mut this.youtube.dialog {
                                dialog.guide = None;
                            }
                            this.check_youtube(choice, true, cx);
                        }
                    }
                }))
        };
        let primary = match guide.as_ref().map(|guide| guide.step) {
            _ if connected => None,
            Some(AccessStep::Blocked) => Some(
                material_button("youtube-open-privacy", ButtonVariant::Filled)
                    .icon(symbols::settings())
                    .child(course2md::tr!("打开系统设置"))
                    .on_click(cx.listener(|this, _, _, cx| this.open_access_settings(cx))),
            ),
            Some(AccessStep::Restart) if app_bundle().is_some() => Some(
                material_button("youtube-relaunch", ButtonVariant::Filled)
                    .icon(symbols::restart_alt())
                    .child(course2md::tr!("重新打开 course2md"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::Relaunch))),
            ),
            _ => Some(check()),
        };
        MaterialDialog::new("youtube-dialog")
            .container(ColorRole::SURFACE)
            .headline(course2md::tr!("连接 YouTube"))
            .width(YOUTUBE_DIALOG_WIDTH)
            .content(body)
            .action(close)
            .when_some(primary, |dialog, primary| dialog.action(primary))
            .dismiss_on_scrim(false)
            .modal(open)
            .on_dismiss({
                let this = cx.weak_entity();
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.close_youtube_dialog(cx));
                }
            })
            .into_any_element()
    }

    /// The Full Disk Access guide: where the person stands in three steps, what to
    /// do now, and the actions that help with it. The dialog's main action takes
    /// the step forward; these are the step's own helpers.
    fn access_guide(&self, guide: &AccessGuide, cx: &mut Context<Self>) -> Div {
        let name = guide.browser.name();
        let bundle = app_bundle().is_some();
        let stepper = Stepper::new(
            "youtube-access-steps",
            course2md::tr!("允许 course2md 读取浏览器数据的步骤"),
        )
        .steps([
            course2md::tr!("系统设置"),
            course2md::tr!("允许 course2md"),
            course2md::tr!("重新打开"),
        ])
        .current(guide.position());
        let text = |id: &'static str, text: String| {
            TileScale::Regular
                .support(id, text)
                .whitespace_normal()
                .min_w_0()
        };
        // The step's helpers, always in this order: back to System Settings,
        // then the app in Finder, to drag into the list.
        let helpers = |settings: bool, reveal: bool| {
            h_flex()
                .w_full()
                .min_w_0()
                .flex_wrap()
                .gap(rem(8.))
                .when(settings, |row| {
                    row.child(
                        material_button("youtube-open-privacy-again", ButtonVariant::Text)
                            .icon(symbols::settings())
                            .child(course2md::tr!("打开系统设置"))
                            .on_click(cx.listener(|this, _, _, cx| this.open_access_settings(cx))),
                    )
                })
                .when(reveal && bundle, |row| {
                    row.child(
                        material_button("youtube-reveal-app", ButtonVariant::Text)
                            .icon(symbols::folder_open())
                            .child(course2md::tr!("在访达中显示 course2md"))
                            .on_click(cx.listener(|this, _, _, cx| this.reveal_app(cx))),
                    )
                })
        };
        let content = v_flex().w_full().min_w_0().gap(rem(12.));
        let content = match guide.step {
            // Reopened for the access and still closed: the switch is off, or on
            // for another copy of the app than this one.
            AccessStep::Blocked if guide.reopened => content
                .child(text(
                    "youtube-access-step",
                    course2md::tr!(
                        "重新打开后，course2md 仍然无法读取 {} 的数据。请在“完全磁盘访问权限”中确认 course2md 的开关已打开；如果列表中有不止一个 course2md，请打开这一个（可在访达中显示）。",
                        name
                    ),
                ))
                .child(helpers(false, true)),
            AccessStep::Blocked => content.child(text(
                "youtube-access-step",
                course2md::tr!(
                    "点按“打开系统设置”，前往“隐私与安全性 › 完全磁盘访问权限”，在列表中打开 course2md。"
                )
                .into(),
            )),
            AccessStep::Waiting => content
                .child(text(
                    "youtube-access-step",
                    course2md::tr!(
                        "在列表中打开 course2md 的开关。如果列表中没有 course2md，点按列表下方的“+”添加它，或把它从访达拖进列表。"
                    )
                    .into(),
                ))
                .children(crate::ui::async_status(
                    "youtube-access-waiting",
                    &crate::ui::AsyncStatus::Busy(
                        course2md::tr!("正在等待权限，打开开关后这里会自动更新").into(),
                    ),
                    None,
                    cx,
                ))
                .child(helpers(true, true)),
            AccessStep::Restart => content
                .child(text(
                    "youtube-access-step",
                    if bundle {
                        course2md::tr!(
                            "如果已在列表中打开 course2md，还要重新打开 course2md，macOS 才会应用这项权限。系统询问时选择“退出并重新打开”，或点按“重新打开 course2md”；之后会回到这里。"
                        )
                    } else {
                        course2md::tr!(
                            "如果已在列表中打开 course2md，还要退出并重新打开 course2md，macOS 才会应用这项权限。"
                        )
                    }
                    .into(),
                ))
                .child(helpers(true, true)),
            AccessStep::Granted => content.children(crate::ui::async_status(
                "youtube-access-granted",
                &crate::ui::AsyncStatus::Success(
                    course2md::tr!("course2md 现在可以读取 {} 的数据了", name).into(),
                ),
                None,
                cx,
            )),
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(16.))
            .child(stepper)
            .child(content)
    }
}

impl crate::Desktop {
    /// Quits the way the menu's Quit does (saving drafts, settings and the
    /// workspace, letting a running task stop cleanly) and has a small helper open
    /// the bundle again once this process has exited. The helper gives up if the
    /// app is still running after a minute (the quit was refused), so it never
    /// opens a second copy.
    pub(crate) fn relaunch_for_access(&mut self, cx: &mut Context<Self>) {
        let Some(bundle) = app_bundle() else {
            return;
        };
        let browser = self
            .accounts
            .read(cx)
            .youtube
            .dialog
            .as_ref()
            .and_then(|dialog| dialog.guide.as_ref().map(|guide| guide.browser));
        leave_resume_marker(browser);
        if stubbed(&format!("relaunch {}", bundle.display())) {
            return;
        }
        if let Err(error) = relaunch_when_exited(bundle) {
            self.message = Some(course2md::tr!(
                "无法重新打开 course2md：{}。请退出后手动打开。",
                format!("{error:#}")
            ));
            cx.notify();
            return;
        }
        if self.request_close(cx) {
            cx.quit();
        }
    }
}

#[cfg(not(unix))]
fn relaunch_when_exited(_: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Starts `/bin/sh` to wait for this process to exit, then `open` the bundle.
#[cfg(unix)]
fn relaunch_when_exited(bundle: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(
            r#"i=0; while /bin/kill -0 "$1" 2>/dev/null; do i=$((i+1)); [ "$i" -gt 300 ] && exit 0; /bin/sleep 0.2; done; exec /usr/bin/open "$2""#,
        )
        .arg("course2md-relaunch")
        .arg(std::process::id().to_string())
        .arg(bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
}

/// What the person may see from their system when this browser is read. Safari
/// needs Full Disk Access, which the guide covers when its folder is closed.
fn browser_note(browser: Browser) -> Option<&'static str> {
    match browser {
        Browser::Safari | Browser::Firefox => None,
        _ if cfg!(target_os = "macos") => Some(course2md::tr!(
            "macOS 可能询问是否允许访问浏览器的“Safe Storage”钥匙串项目。选择“始终允许”，之后打开 YouTube 视频时就不会再问。"
        )),
        _ if cfg!(windows) => Some(course2md::tr!(
            "Windows 上的新版 Chrome 和 Edge 可能不允许读取登录信息；如果失败，请改用 Firefox。读取前请先退出该浏览器。"
        )),
        _ => None,
    }
}

/// Capture builds: the connection dialog in each state, without reading a browser.
#[cfg(feature = "capture")]
impl AccountsView {
    pub(crate) fn capture_youtube_state(&mut self, state: &str, cx: &mut Context<Self>) {
        // A real check of the first installed browser, for a fixture home.
        if state == "check" {
            if let Some(choice) = self.youtube.dialog.as_ref().and_then(|d| d.choice()) {
                self.check_youtube(choice, true, cx);
            }
            return;
        }
        // The guide's states for a fixture browser, whatever its folder says.
        if let Some(step) = match state {
            "access-blocked" | "access-reopened" => Some(AccessStep::Blocked),
            "access-waiting" => Some(AccessStep::Waiting),
            "access-restart" => Some(AccessStep::Restart),
            "access-granted" => Some(AccessStep::Granted),
            _ => None,
        } {
            if let Some(dialog) = &mut self.youtube.dialog {
                if dialog.browsers.is_empty() {
                    *dialog = YoutubeDialog::new(vec![Browser::Chrome, Browser::Safari], None);
                }
                dialog.step = YoutubeStep::Choose;
                dialog.guide = dialog
                    .selected_browser()
                    .map(|browser| AccessGuide::new(browser, step, Instant::now()));
                if state == "access-reopened"
                    && let Some(guide) = &mut dialog.guide
                {
                    guide.reopened = true;
                }
            }
            cx.notify();
            return;
        }
        // A real look at the fixture folder, as the dialog opened it, then the
        // person's first action: System Settings (stubbed in capture runs).
        if state == "access-open-settings" {
            self.open_access_settings(cx);
            return;
        }
        if let Some(dialog) = &mut self.youtube.dialog {
            dialog.step = match state {
                "checking" => YoutubeStep::Checking,
                "signedout" => YoutubeStep::SignedOut,
                "failed" => YoutubeStep::Failed {
                    summary: course2md::tr!(
                        "{} 正在使用 Cookie 数据库。请退出 {} 后重试。",
                        "Google Chrome",
                        "Google Chrome"
                    ),
                    detail: "database is locked".into(),
                    blocked: false,
                },
                "connected" => YoutubeStep::Connected,
                _ => YoutubeStep::Choose,
            };
            if dialog.browsers.is_empty() && state != "empty" {
                *dialog = YoutubeDialog::new(
                    vec![Browser::Chrome, Browser::Safari, Browser::Firefox],
                    None,
                );
                dialog.step = YoutubeStep::Choose;
            }
        }
        cx.notify();
    }
}
