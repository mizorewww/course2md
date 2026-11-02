//! The YouTube connection: its row in Settings › 来源账号 and the dialog that
//! connects a browser's YouTube session. yt-dlp reads the browser's cookies; this
//! app keeps only which browser to use.
use crate::{Desktop, Field, Page, symbols, ui::Resource};
use course2md::auth::youtube::{self, Browser, BrowserChoice, Session};
use desktop_accounts::youtube::*;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{color::ColorRole, icons::Icon, prelude::*};

const YOUTUBE_DIALOG_WIDTH: f32 = 480.;

impl Desktop {
    /// Reads the saved browser choice; no browser is touched until the person asks.
    pub(crate) fn load_youtube(&mut self, cx: &mut Context<Self>) {
        let task = crate::spawn_blocking_result(youtube::saved_choice);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(choice) => {
                        this.youtube.choice = choice;
                        this.youtube.load_error = None;
                    }
                    Err(error) => this.youtube.load_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn open_youtube_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.youtube.dialog.is_some() {
            return;
        }
        self.youtube.retry_source = (!self.onboarding.active
            && matches!(self.page, Page::New)
            && self.online(cx)
            && self.import.preview_error.is_some()
            && course2md::auth::is_youtube_url(&self.value(Field::Source, cx)))
        .then(|| {
            (
                self.import.preview_generation,
                self.value(Field::Source, cx),
            )
        });
        self.youtube.generation = self.youtube.generation.wrapping_add(1);
        self.youtube.dialog = Some(YoutubeDialog::new(
            Browser::installed(),
            self.youtube.choice.as_ref(),
        ));
        window.refresh();
        cx.notify();
    }

    fn close_youtube_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resume = matches!(
            self.youtube.dialog.as_ref().map(|dialog| &dialog.step),
            Some(YoutubeStep::Connected)
        ) && self
            .youtube
            .retry_source
            .as_ref()
            .is_some_and(|(generation, input)| {
                *generation == self.import.preview_generation
                    && *input == self.value(Field::Source, cx)
            });
        self.youtube.close();
        if resume {
            self.inspect_source(window, cx);
        }
        cx.notify();
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
            let program = course2md::ytdlp::resolve_in(Some(&crate::environment::tool_path()))
                .ok_or_else(|| {
                    anyhow::anyhow!(course2md::tr!(
                        "缺少读取视频所需的组件，请在设置中检查运行环境"
                    ))
                })?;
            // Only the check runs here: the choice is saved below, and only
            // while the dialog that asked for it is still open.
            let session = youtube::check_session(&program, &choice)?;
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
                        },
                        Err(error) => YoutubeStep::Failed {
                            summary: format!("{error:#}"),
                            detail: String::new(),
                        },
                    },
                };
                if save && let Some(dialog) = &mut this.youtube.dialog {
                    dialog.step = step.clone();
                }
                this.youtube.status = Some(step);
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
        let status = if self.youtube.checking {
            (StatusKind::Progress, course2md::tr!("验证中"))
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
        if !connected || matches!(self.youtube.status, Some(YoutubeStep::SignedOut)) {
            actions.push(
                material_button("youtube-connect", ButtonVariant::Outlined)
                    .tone(ButtonTone::Accent)
                    .icon(symbols::smart_display())
                    .child(if connected {
                        course2md::tr!("重新连接 YouTube")
                    } else {
                        course2md::tr!("连接 YouTube")
                    })
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_youtube_dialog(window, cx)),
                    )
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
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_youtube_dialog(window, cx)),
                        ),
                )
                .item(
                    MaterialMenuItem::new(course2md::tr!("断开连接"))
                        .leading(Icon::from(symbols::logout()))
                        .divider(true)
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
        let browser = dialog
            .browser
            .and_then(|index| dialog.browsers.get(index).copied());
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
            body = body.child(supporting_info(
                "youtube-explain",
                course2md::tr!(
                    "YouTube 不允许在其他应用中用密码登录。course2md 会让 yt-dlp 读取所选浏览器中的 YouTube 登录（Cookie），只在打开 YouTube 视频时使用，不保存这些 Cookie。连接时的检查会让 yt-dlp 把浏览器的 Cookie 短暂写入本机的私有临时文件，检查结束即删除。"
                ),
            ));
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
                            }
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
                if let Some(note) = browser.and_then(browser_note) {
                    body = body.child(supporting_info("youtube-browser-note", note));
                }
            }
            body = body.child(notice_banner(
                "youtube-risk",
                NoticeKind::Warning,
                course2md::tr!(
                    "用账号下载可能让 YouTube 限制或封禁这个账号。请适度使用，或使用不常用的账号。"
                ),
            ));
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
                YoutubeStep::Failed { summary, detail } => {
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
        .on_click(cx.listener(|this, _, window, cx| this.close_youtube_dialog(window, cx)));
        let choice = dialog.choice();
        let _ = window;
        MaterialDialog::new("youtube-dialog")
            .container(ColorRole::SURFACE)
            .headline(course2md::tr!("连接 YouTube"))
            .width(YOUTUBE_DIALOG_WIDTH)
            .content(body)
            .action(close)
            .when(!connected, |dialog| {
                dialog.action(
                    material_button("youtube-check", ButtonVariant::Filled)
                        .icon(symbols::login())
                        .child(course2md::tr!("检查并连接"))
                        .disabled(checking || choice.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(choice) = choice.clone() {
                                this.check_youtube(choice, true, cx);
                            }
                        })),
                )
            })
            .dismiss_on_scrim(false)
            .modal(open)
            .on_dismiss({
                let this = cx.weak_entity();
                move |window, cx| {
                    let _ = this.update(cx, |this, cx| this.close_youtube_dialog(window, cx));
                }
            })
            .into_any_element()
    }
}

/// What the person may see from their system when this browser is read.
fn browser_note(browser: Browser) -> Option<&'static str> {
    match browser {
        Browser::Safari => Some(course2md::tr!(
            "读取 Safari 需要在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许 course2md。"
        )),
        Browser::Firefox => None,
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
impl Desktop {
    pub(crate) fn capture_youtube_state(&mut self, state: &str, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.youtube.dialog {
            dialog.step = match state {
                "checking" => YoutubeStep::Checking,
                "signedout" => YoutubeStep::SignedOut,
                "failed" => YoutubeStep::Failed {
                    summary: course2md::tr!("macOS 不允许读取 Safari 的 Cookie。请在“系统设置 › 隐私与安全性 › 完全磁盘访问权限”中允许 course2md，或改用其他浏览器。").into(),
                    detail: "[Errno 1] Operation not permitted: '~/Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies'".into(),
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
