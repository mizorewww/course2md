//! Native pages: stable navigation and actions surround independently scrolling content.
use super::*;
use crate::theme::*;
use gpui_component::button::*;

const SHELL_GUTTER: f32 = 24.;
const SETTINGS_SHELL_WIDTH: f32 = 1108.;

/// The app bar marks the current destination with a tonal pill; TitleBar's
/// default bottom hairline would be unused chrome.
pub(crate) fn apply_shell_title_bar_chrome(bar: TitleBar) -> TitleBar {
    bar.pl_0().bg(color(CANVAS)).border_b_0()
}

fn shell_column_for(page: Page) -> Div {
    shell_column_at(shell_column_width(page))
}

fn shell_column_width(page: Page) -> AbsoluteLength {
    match page {
        // Each task gets a useful content width: a gallery needs room for covers.
        Page::Settings => rems(SETTINGS_SHELL_WIDTH / 14.).into(),
        Page::Library => rems(1280. / 14.).into(),
        Page::New => rems(1200. / 14.).into(),
        // Sections set the screenshot beside the text, as on the canvas.
        Page::Result => rems(1180. / 14.).into(),
        _ => COLUMN.into(),
    }
}

/// The max width includes both gutters; layout calculations use the same box.
pub(super) fn shell_content_width(page: Page, window: &Window) -> f32 {
    let width = f32::from(window.bounds().size.width);
    let gutter = if matches!(page, Page::New | Page::Settings) && is_compact(window) {
        16.
    } else {
        SHELL_GUTTER
    };
    (width.min(f32::from(
        shell_column_width(page).to_pixels(window.rem_size()),
    )) - gutter * 2.)
        .max(0.)
}

/// Entrance motion replays when the notice text changes, not on every frame.
fn notice_key(text: &str) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish() as usize
}

/// Whether the app bar holds the brand and every destination with its label,
/// from the shaped text at the current scale: the mark, wordmark and each
/// destination's padding, icon, gap and label, plus a little open space.
fn app_bar_labels_fit(leading: f32, tasks: Option<&str>, window: &Window) -> bool {
    let rem = f32::from(window.rem_size());
    let measure = |text: &str, size: f32| {
        let mut style = window.text_style();
        style.font_weight = FontWeight::MEDIUM;
        let text = SharedString::from(text.to_owned());
        f32::from(
            window
                .text_system()
                .shape_line(text.clone(), px(size), &[style.to_run(text.len())], None)
                .width,
        )
    };
    let scaled = |value: f32| value * rem / 14.;
    let brand = scaled(4. + 32. + 10. + 12.) + measure("course2md", scaled(20.));
    let destination = |label: &str| scaled(12. + 20. + 8. + 16.) + measure(label, scaled(15.));
    let destinations = [
        Some(course2md::tr!("笔记库")),
        tasks,
        Some(course2md::tr!("设置")),
    ]
    .into_iter()
    .flatten()
    .map(|label| destination(label) + 4.)
    .sum::<f32>();
    let open_space = scaled(24.);
    leading + brand + destinations + open_space + 16. <= f32::from(window.viewport_size().width)
}

fn shell_column_at(width: AbsoluteLength) -> Div {
    div()
        .w_full()
        .min_w_0()
        .max_w(width)
        .mx_auto()
        .px(px(SHELL_GUTTER))
}

impl Desktop {
    /// Keep task results reachable while the user is on another page.
    fn shell_topbar(
        &self,
        reader_tools: Option<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TitleBar {
        let (task_count, tasks_need_attention) =
            self.workspace.as_ref().map_or((0, false), |workspace| {
                crate::task_ui::task_navigation_summary(&workspace.state.tasks)
            });
        // macOS draws the traffic lights inside the title bar's leading edge.
        let leading = if cfg!(target_os = "macos") { 88. } else { 12. };
        let page = self.page;
        // TitleBar wraps its children in a box that grows to their content, so
        // the bar takes the window's width explicitly; otherwise nothing in it
        // (the note title, the reader's search) would ever shrink.
        let bar = h_flex()
            .w(window.viewport_size().width)
            .min_w_0()
            .h_full()
            .pl(px(leading))
            .pr(px(16.))
            .gap(px(4.))
            .items_center();
        if page == Page::Settings {
            return self.settings_title_bar(bar, window, cx);
        }
        if let (Page::Result, Some(tools)) = (page, reader_tools) {
            return self.reader_title_bar(bar, tools, window, cx);
        }
        // The brand leads the bar and returns to the workbench, as on the M3
        // canvas; destinations follow on the trailing edge with icon and label.
        let task_label = if task_count > 0 {
            course2md::tr!("任务 · {}", task_count)
        } else {
            course2md::tr!("任务").to_owned()
        };
        let show_tasks = task_count > 0 || tasks_need_attention || page == Page::Task;
        // Destinations keep their labels; they become icons only when the
        // bar, measured with the current text, can no longer hold them.
        let compact =
            !app_bar_labels_fit(leading, show_tasks.then_some(task_label.as_str()), window);
        let home = nav_action("nav-home", false, cx)
            .pl(px(4.))
            .pr(px(12.))
            .accessibility_label(course2md::tr!("主页"))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        // The mark scales with the wordmark beside it.
                        img("images/course2md.png")
                            .size(rems(32. / 14.))
                            .rounded(rems(10. / 14.))
                            .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .text_size(rems(20. / 14.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color(INK))
                            .child("course2md"),
                    ),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                if this.prepare_workbench_input(window, cx) {
                    this.navigate(Page::New, cx);
                }
            }));
        let library = nav_action(
            "nav-library",
            matches!(page, Page::Library | Page::Result),
            cx,
        )
        .icon(icons::library())
        .when(!compact, |button| button.label(course2md::tr!("笔记库")))
        .accessibility_label(course2md::tr!("笔记库"))
        .on_click(cx.listener(|this, _, _, cx| {
            this.folder_filter = crate::FolderFilter::All;
            this.navigate(Page::Library, cx);
        }));
        // Tasks stay reachable, but only claim space once there is work to show.
        let tasks = show_tasks.then(|| {
            nav_action("nav-tasks", page == Page::Task, cx)
                .icon(if tasks_need_attention {
                    icons::warning()
                } else {
                    icons::list_checks()
                })
                .label(task_label)
                .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::Task, cx)))
        });
        let settings = nav_action("nav-settings", page == Page::Settings, cx)
            .icon(if self.settings_have_problem() {
                icons::warning()
            } else {
                icons::settings()
            })
            .when(!compact, |button| button.label(course2md::tr!("设置")))
            .accessibility_label(course2md::tr!("设置"))
            .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)));
        let bar = bar
            .child(home)
            .child(div().flex_1())
            .child(library)
            .children(tasks)
            .child(settings);
        apply_shell_title_bar_chrome(TitleBar::new().h(title_bar_height(window))).child(bar)
    }

    fn task_result_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // A new result can briefly announce itself. Historical unresolved work
        // remains in Tasks; it must not consume every unrelated page's height.
        let (announced_id, shown) = self.transient_task_result.as_ref()?;
        if shown.elapsed() >= std::time::Duration::from_secs(8) {
            return None;
        }
        let visible_task = if self.page == Page::New {
            self.current_input_task(cx).map(|task| task.id.as_str())
        } else {
            None
        };
        let task = self
            .workspace
            .as_ref()?
            .state
            .tasks
            .iter()
            .filter(|task| {
                &task.id == announced_id
                    && task.unread
                    && task.handled_by.is_none()
                    && visible_task != Some(task.id.as_str())
                    && !(self.reading
                        && self.page == Page::New
                        && self.following_conversion.as_ref().is_some_and(|follow| {
                            follow.follows(&task.id, self.preview_generation)
                        }))
            })
            .max_by_key(|task| task.updated)?;
        let id = task.id.clone();
        let dismiss_id = id.clone();
        let exports_only = task.exports_only() && task.state == workspace::TaskState::Complete;
        let result = if matches!(
            task.state,
            workspace::TaskState::Complete | workspace::TaskState::Partial
        ) {
            task.artifact.clone().map(|out_dir| crate::backend::Done {
                out_dir,
                title: task.plan.title.clone(),
                ..Default::default()
            })
        } else {
            None
        };
        let action = match task.state {
            workspace::TaskState::Complete if exports_only => course2md::tr!("打开导出位置"),
            workspace::TaskState::Complete => course2md::tr!("阅读笔记"),
            workspace::TaskState::Partial if result.is_some() => course2md::tr!("阅读笔记"),
            workspace::TaskState::Partial => course2md::tr!("查看未完成部分"),
            workspace::TaskState::Uncertain => course2md::tr!("查看结果"),
            _ => course2md::tr!("查看任务"),
        };
        Some(crate::motion::enter(
            SharedString::from(format!("task-result-notice-{}", task.id)),
            shell_column_for(self.page).py_2().flex_shrink_0().child(
                notice_banner(
                    "background-task-result",
                    NoticeKind::Info,
                    course2md::tr!(
                        "《{}》：{}",
                        task.plan.title,
                        if exports_only {
                            course2md::tr!("文件已导出")
                        } else {
                            task.state.label()
                        },
                    ),
                )
                .child(
                    control("open-task-result")
                        .icon(icons::arrow_forward())
                        .label(action)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if exports_only {
                                this.open_task_export_location(id.clone(), cx);
                            } else if let Some(done) = &result {
                                this.open_course(Course::from_completed(done), cx);
                            } else {
                                this.select_task(&id, cx);
                                this.navigate(Page::Task, cx);
                            }
                        })),
                )
                .child(
                    control("dismiss-task-result")
                        .ghost()
                        .icon(crate::icons::close())
                        .accessibility_label(course2md::tr!("关闭提示"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.transient_task_result = None;
                            if let Some(workspace) = &mut this.workspace
                                && let Err(error) = workspace.transaction(|state| {
                                    if let Some(task) = state.task_mut(&dismiss_id) {
                                        task.unread = false;
                                    }
                                    Ok(())
                                })
                            {
                                this.workspace_error = Some(course2md::tr!(
                                    "提示关闭状态未能保存：{}",
                                    format!("{error:#}")
                                ));
                            }
                            cx.notify();
                        })),
                ),
            ),
        ))
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[allow(unused_mut)]
        let mut appearance = self.preferences.application().appearance.clone();
        #[allow(unused_mut)]
        let mut font_scale = self.preferences.application().font_scale;
        #[cfg(feature = "capture")]
        {
            if let Some(palette) = crate::capture::palette_override() {
                appearance.select(palette);
                appearance.mode = if palette.is_dark() {
                    crate::palettes::Appearance::Dark
                } else {
                    crate::palettes::Appearance::Light
                };
            }
            font_scale = crate::capture::scale_override().unwrap_or(font_scale);
        }
        theme::apply_preference(&appearance, window, cx);
        theme::apply_scale(font_scale, window, cx);
        if self.onboarding.active {
            let content = self.onboarding_page(window, cx);
            return div()
                .id("onboarding-action-root")
                .track_focus(&self.root_focus)
                .tab_stop(false)
                .size_full()
                .child(a11y::ModalBackground::new(
                    v_flex()
                        .size_full()
                        .bg(color(CANVAS))
                        .text_color(color(INK))
                        .text_size(TEXT_BODY)
                        // The shell's brand, without its destinations: setup is
                        // modal, so the mark is not a way home here.
                        .child(
                            apply_shell_title_bar_chrome(
                                TitleBar::new().h(title_bar_height(window)),
                            )
                            .child(
                                h_flex()
                                    .w(window.viewport_size().width)
                                    .h_full()
                                    // The traffic lights, then the home action's inset.
                                    .pl(px(if cfg!(target_os = "macos") { 92. } else { 16. }))
                                    .gap(px(10.))
                                    .items_center()
                                    .child(
                                        img("images/course2md.png")
                                            .size(px(32.))
                                            .rounded(px(10.))
                                            .flex_shrink_0(),
                                    )
                                    .child(
                                        div()
                                            .text_size(rems(20. / 14.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(color(INK))
                                            .child("course2md"),
                                    ),
                            ),
                        )
                        .child(content),
                    window.has_active_dialog(cx),
                ))
                .children(Root::render_dialog_layer(window, cx))
                .into_any_element();
        }
        let mut reader_tools = None;
        let content = match self.page {
            Page::New => self.new_page(window, cx),
            Page::Task => self.queue_page(window, cx),
            Page::Library => self.library_page(window, cx),
            Page::Settings => self.settings_page(window, cx),
            Page::Result => {
                let (page, tools) = self.reader_page(window, cx);
                reader_tools = tools;
                page
            }
        };
        // Task and library content virtualize their long lists: the list owns
        // scrolling for the page instead of the shared page-scroll container.
        let self_scrolling = matches!(self.page, Page::Task | Page::Library);
        let content = v_flex()
            .gap_4()
            .when(
                matches!(self.page, Page::Result | Page::Settings) || self_scrolling,
                |v| v.h_full().min_h_0(),
            )
            .when(
                // The workbench card already shows a followed result opening.
                self.reading
                    && !matches!(self.page, Page::Library | Page::Result)
                    && !(self.page == Page::New && self.following_conversion.is_some()),
                |v| {
                    v.child(
                        h_flex()
                            .items_center()
                            .gap(px(8.))
                            .child(crate::motion::spinner("opening-note-spinner", cx))
                            .child(
                                accessible_text("opening-note", course2md::tr!("正在打开笔记…"))
                                    .role(Role::Status)
                                    .text_color(color(GRAY)),
                            ),
                    )
                },
            )
            .child(content);
        // Keyed by destination, so arriving at a page plays once and later
        // renders of the same page stay still. Settings animates its category
        // panel instead, so its navigation stays a landmark.
        let content = if self.page == Page::Settings {
            content.into_any_element()
        } else {
            crate::motion::page_enter(("page-content", self.page as usize), content)
        };
        let topbar = self.shell_topbar(reader_tools, window, cx);
        let task_notice = self.task_result_notice(cx);
        let model_notice = self.onboarding_background_notice(cx);
        let body = v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .w_full()
            .when_some(task_notice, |body, notice| body.child(notice))
            .when_some(model_notice, |body, notice| {
                body.child(shell_column_for(self.page).py_2().child(notice))
            })
            .when(
                self.page == Page::Library
                    && self
                        .settings_group_notice(crate::preferences::PreferenceGroup::Application)
                        .is_some_and(|(_, error)| error),
                |v| {
                    v.child(
                        shell_column_for(self.page).pb_3().flex_shrink_0().child(
                            div()
                                .id("library-settings-feedback")
                                .w_full()
                                .min_w_0()
                                .p_3()
                                .rounded(RADIUS_CARD)
                                .bg(color(DANGER_BG))
                                .child(self.group_feedback(
                                    crate::preferences::PreferenceGroup::Application,
                                    cx,
                                )),
                        ),
                    )
                },
            )
            .when_some(self.workspace_error.clone(), |v, message| {
                v.child(crate::motion::enter(
                    ("workspace-error", notice_key(&message)),
                    shell_column_for(self.page).pb_3().child(
                        notice_banner("workspace-error", NoticeKind::Danger, message)
                            .child(
                                control("retry-workspace-records")
                                    .icon(icons::refresh())
                                    .label(course2md::tr!("重试"))
                                    .disabled(self.job.is_some() || self.storage_ui.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.retry_workspace_records(window, cx)
                                    })),
                            )
                            .when(self.workspace.is_none(), |row| {
                                row.child(
                                    control("rebuild-workspace-records")
                                        .icon(icons::restart())
                                        .label(course2md::tr!("备份并重建任务记录"))
                                        .disabled(self.job.is_some() || self.storage_ui.busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.rebuild_workspace_records(window, cx)
                                        })),
                                )
                            }),
                    ),
                ))
            })
            .when_some(self.message.clone(), |v, message| {
                v.child(crate::motion::state_enter(
                    ("app-message", notice_key(&message)),
                    shell_column_for(self.page).pb_3().child(
                        notice_banner("app-message", NoticeKind::Info, message).child(
                            control("dismiss-message")
                                .ghost()
                                .icon(crate::icons::close())
                                .accessibility_label(course2md::tr!("关闭提示"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.message = None;
                                    cx.notify();
                                })),
                        ),
                    ),
                ))
            })
            .child(
                div()
                    .id(("page-scroll", self.page as usize))
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .when(
                        !matches!(self.page, Page::Result | Page::Settings) && !self_scrolling,
                        |view| {
                            view.overflow_y_scroll()
                                .track_scroll(&self.scrolls[self.page as usize])
                        },
                    )
                    .child(
                        shell_column_for(self.page)
                            .when(self.page == Page::Settings, |v| v.max_w_full())
                            .when(
                                matches!(self.page, Page::New | Page::Settings)
                                    && is_compact(window),
                                |v| v.px(px(16.)),
                            )
                            .when(
                                matches!(self.page, Page::Result | Page::Settings)
                                    || self_scrolling,
                                |v| v.h_full().min_h_0(),
                            )
                            // Lists that scroll themselves keep their trailing
                            // space inside the list, so their last surface is
                            // never cut off above the window edge.
                            .when(
                                !matches!(self.page, Page::Result | Page::Settings)
                                    && !self_scrolling,
                                |v| v.pb_6(),
                            )
                            .child(content),
                    ),
            );
        let background = v_flex()
            .size_full()
            .bg(color(CANVAS))
            .text_color(color(INK))
            .text_size(rems(1.))
            .child(topbar)
            .child(body);
        div()
            .id("desktop-action-root")
            .relative()
            .group("workbench-drop")
            .track_focus(&self.root_focus)
            .tab_stop(false)
            .size_full()
            .on_action(cx.listener(|this, _: &ImportVideo, window, cx| {
                if !window.has_active_dialog(cx) {
                    this.import_video_from_action(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SearchContent, window, cx| {
                if !window.has_active_dialog(cx) {
                    this.search_content(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                if !window.has_active_dialog(cx) {
                    this.open_settings(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenAbout, window, cx| {
                if !window.has_active_dialog(cx) {
                    this.open_about(window, cx);
                }
            }))
            .child(a11y::ModalBackground::new(
                background,
                window.has_active_dialog(cx),
            ))
            .when(self.page == Page::New, |view| {
                view.child(self.workbench_drop_overlay(cx))
            })
            .children(Root::render_dialog_layer(window, cx))
            .into_any_element()
    }
}

/// Display name of a known video site, shared by the workbench preview and the
/// reader header.
pub fn video_site_name(url: &str) -> Option<&'static str> {
    match course2md::config::known_video_site(url) {
        Some("bilibili") => Some("Bilibili"),
        Some("youtube") => Some("YouTube"),
        _ => None,
    }
}

/// A dialog part drawn from the desktop's state. Dialog contents live in
/// their own entity, so each one re-enters the desktop to render.
pub(crate) struct DesktopView<F> {
    desktop: Entity<Desktop>,
    render: F,
    _observation: Option<Subscription>,
}

impl<F> DesktopView<F>
where
    F: FnMut(&mut Desktop, &mut Window, &mut Context<Desktop>) -> AnyElement + 'static,
{
    /// Repaints whenever the desktop changes.
    pub(crate) fn observing(desktop: Entity<Desktop>, cx: &mut App, render: F) -> Entity<Self> {
        cx.new(|cx| Self {
            _observation: Some(cx.observe(&desktop, |_, _, cx| cx.notify())),
            desktop,
            render,
        })
    }

    /// Repaints only when notified directly, for parts whose state changes
    /// far less often than the desktop.
    pub(crate) fn notified(desktop: Entity<Desktop>, cx: &mut App, render: F) -> Entity<Self> {
        cx.new(|_| Self {
            _observation: None,
            desktop,
            render,
        })
    }
}

impl<F> Render for DesktopView<F>
where
    F: FnMut(&mut Desktop, &mut Window, &mut Context<Desktop>) -> AnyElement + 'static,
{
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Self {
            desktop, render, ..
        } = self;
        desktop.update(cx, |desktop, cx| render(desktop, window, cx))
    }
}
