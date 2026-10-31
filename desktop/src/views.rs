//! Native pages: stable navigation and actions surround independently scrolling content.
use super::*;
use crate::symbols;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit_material::{
    color::{ColorRole, Scheme},
    components::{
        ButtonVariant, CaptionLabels, IconButton, MaterialTooltip, NoticeKind, Snackbar,
        SnackbarPosition, TitleBar as AppBar, material_button, notice_banner, spinner,
        title_bar_control,
    },
    icons::SymbolIcon,
    motion,
    prelude::{ButtonSize, CaptionButtons, accessible_text, color, is_compact, rem},
    theme::{advance_component_theme, init_component_theme, scheme as current_scheme, system_dark},
};

const SHELL_GUTTER: f32 = 24.;
const SETTINGS_SHELL_WIDTH: f32 = 1108.;
/// The reading column of a page without a wider one of its own.
const COLUMN: f32 = 920.;

/// The height the window opens for: the traffic lights sit centred on it.
pub(crate) const APP_BAR_HEIGHT: Pixels = px(56.);

/// Where the title bar's content starts, after the macOS traffic lights.
pub(crate) fn title_bar_leading(window: &Window) -> f32 {
    gpui_kit_material::components::title_bar_leading(APP_BAR_HEIGHT, window)
}

/// Where the title bar's content ends, before any caption buttons.
pub(crate) fn title_bar_trailing(window: &Window) -> f32 {
    CaptionButtons::trailing(window)
}

/// The window's title bar on the page's ground. It moves the window and draws what
/// caption buttons the platform lacks; children call `title_bar_control` to take their
/// presses. The Linux close button asks first, as the window manager's close does.
pub(crate) fn app_bar(height: Pixels, window: &Window, cx: &mut Context<Desktop>) -> AppBar {
    let view = cx.weak_entity();
    let bar = AppBar::new(
        "app-bar",
        height,
        CaptionLabels {
            minimize: course2md::tr!("最小化").into(),
            maximize: course2md::tr!("最大化").into(),
            restore: course2md::tr!("还原").into(),
            close: course2md::tr!("关闭").into(),
        },
    )
    .w_full()
    .bg(color(ColorRole::SURFACE_CONTAINER))
    .on_close(move |_, cx| {
        view.update(cx, |this, cx| this.request_close(cx))
            .unwrap_or(true)
    });
    // A capture can ask for another platform's caption buttons.
    #[cfg(feature = "capture")]
    let bar = bar.caption(
        crate::capture::caption_override()
            .unwrap_or_else(|| gpui_kit_material::components::Caption::detect(window)),
    );
    #[cfg(not(feature = "capture"))]
    let _ = window;
    bar
}

/// The course2md mark, with the wordmark when there is room.
fn brand(wordmark: bool) -> Div {
    h_flex()
        .items_center()
        .gap(rem(10.))
        .child(
            img("images/course2md.png")
                .size(rem(32.))
                .rounded(rem(10.))
                .flex_shrink_0(),
        )
        .when(wordmark, |brand| {
            brand.child(
                div()
                    .text_size(rem(20.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color(ColorRole::ON_SURFACE))
                    .child("course2md"),
            )
        })
}

/// One app bar destination, tonal when current. `shown` is its label, or a shorter
/// form (a task count) or nothing when the bar cannot hold labels; then the full
/// label is its tooltip. It always names the button.
fn nav_destination(
    id: &'static str,
    selected: bool,
    icon: SymbolIcon,
    label: impl Into<SharedString>,
    shown: Option<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let label = label.into();
    let collapsed = shown.as_ref() != Some(&label);
    let button = title_bar_control(
        material_button(
            id,
            if selected {
                ButtonVariant::Tonal
            } else {
                ButtonVariant::Text
            },
        )
        .button_size(ButtonSize::Small)
        .icon(icon)
        .accessibility_label(label.clone())
        .children(shown)
        .on_click(on_click),
    );
    if collapsed {
        MaterialTooltip::plain(SharedString::from(format!("{id}-tip")), label)
            .trigger(button)
            .into_any_element()
    } else {
        button.into_any_element()
    }
}

/// The width of `text` as the window would shape it.
fn shaped_width(text: &str, size: f32, weight: FontWeight, window: &Window) -> f32 {
    let mut style = window.text_style();
    style.font_weight = weight;
    let text = SharedString::from(text.to_owned());
    f32::from(
        window
            .text_system()
            .shape_line(text.clone(), px(size), &[style.to_run(text.len())], None)
            .width,
    )
}

/// The height of the app bar at the current text scale.
pub(crate) fn title_bar_height(window: &Window) -> Pixels {
    rem(40.).to_pixels(window.rem_size()) + px(16.)
}

/// The width of a `nav_destination` showing `shown`, from `ButtonSize::Small`'s
/// geometry and the shaped text at the current scale.
fn nav_destination_width(shown: Option<&str>, window: &Window) -> f32 {
    let scaled = |value: f32| value * f32::from(window.rem_size()) / 14.;
    let padding_and_icon = scaled(16. + 20. + 16.);
    match shown {
        None => padding_and_icon,
        Some(text) => {
            padding_and_icon
                + scaled(8.)
                + shaped_width(text, scaled(14.), FontWeight::MEDIUM, window)
        }
    }
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
        _ => rem(COLUMN).into(),
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

/// How much text the app bar can hold, from most to least.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppBarDensity {
    /// The wordmark and every destination's label.
    Labels,
    /// Destinations as icons (the task count stays), with the wordmark.
    Icons,
    /// The mark alone and icon destinations.
    Mark,
}

/// The fullest app bar that fits, measured from the shaped text at the current scale:
/// the brand, each destination, the gaps, and a little open space. `tasks` is the task
/// destination's label and its count, when it is shown.
fn app_bar_density(tasks: Option<(&str, Option<&str>)>, window: &Window) -> AppBarDensity {
    let scaled = |value: f32| value * f32::from(window.rem_size()) / 14.;
    let available = f32::from(window.viewport_size().width)
        - title_bar_leading(window)
        - title_bar_trailing(window);
    let fits = |density: AppBarDensity| {
        let brand = match density {
            AppBarDensity::Mark => scaled(4. + 32. + 4.),
            _ => {
                scaled(4. + 32. + 10. + 12.)
                    + shaped_width("course2md", scaled(20.), FontWeight::MEDIUM, window)
            }
        };
        let labels = density == AppBarDensity::Labels;
        let destinations = [
            Some(labels.then_some(course2md::tr!("笔记库"))),
            tasks.map(|(label, count)| if labels { Some(label) } else { count }),
            Some(labels.then_some(course2md::tr!("设置"))),
        ]
        .into_iter()
        .flatten()
        .map(|shown| nav_destination_width(shown, window) + 4.)
        .sum::<f32>();
        brand + destinations + scaled(24.) <= available
    };
    [AppBarDensity::Labels, AppBarDensity::Icons]
        .into_iter()
        .find(|density| fits(*density))
        .unwrap_or(AppBarDensity::Mark)
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
    ) -> AppBar {
        let (task_count, tasks_need_attention) =
            self.workspace.as_ref().map_or((0, false), |workspace| {
                crate::task_ui::task_navigation_summary(&workspace.state.tasks)
            });
        let page = self.page;
        let bar = h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(px(4.))
            .items_center();
        if page == Page::Settings {
            // Settings is a place of its own: window controls and dragging only; the way
            // back and its title lead its navigation pane instead.
            return app_bar(title_bar_height(window), window, cx).child(bar);
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
        let task_count_text = (task_count > 0).then(|| task_count.to_string());
        let show_tasks = task_count > 0 || tasks_need_attention || page == Page::Task;
        // Destinations keep their labels; once the bar, measured with the
        // current text, cannot hold them, every destination becomes its icon
        // (the label as its tooltip), and then the wordmark goes.
        let density = app_bar_density(
            show_tasks.then_some((task_label.as_str(), task_count_text.as_deref())),
            window,
        );
        let labels = density == AppBarDensity::Labels;
        let wordmark = density != AppBarDensity::Mark;
        let shown = |label: &str| labels.then(|| SharedString::from(label.to_owned()));
        let home_button = title_bar_control(
            material_button("nav-home", ButtonVariant::Text)
                .button_size(ButtonSize::Small)
                .px(rem(4.))
                .pr(rem(if wordmark { 12. } else { 4. }))
                .accessibility_label(course2md::tr!("主页"))
                .child(brand(wordmark))
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.prepare_workbench_input(window, cx) {
                        this.navigate(Page::New, cx);
                    }
                })),
        );
        let home = if wordmark {
            home_button.into_any_element()
        } else {
            MaterialTooltip::plain("nav-home-tip", course2md::tr!("主页"))
                .trigger(home_button)
                .into_any_element()
        };
        let library = nav_destination(
            "nav-library",
            matches!(page, Page::Library | Page::Result),
            symbols::library_books(),
            course2md::tr!("笔记库"),
            shown(course2md::tr!("笔记库")),
            cx.listener(|this, _, _, cx| {
                this.folder_filter = crate::FolderFilter::All;
                this.navigate(Page::Library, cx);
            }),
        );
        // Tasks stay reachable, but only claim space once there is work to
        // show. Without room for labels the count stays beside the icon.
        let tasks = show_tasks.then(|| {
            let shown = if labels {
                Some(SharedString::from(task_label.clone()))
            } else {
                task_count_text.clone().map(SharedString::from)
            };
            nav_destination(
                "nav-tasks",
                page == Page::Task,
                if tasks_need_attention {
                    symbols::warning()
                } else {
                    symbols::checklist()
                },
                task_label.clone(),
                shown,
                cx.listener(|this, _, _, cx| this.navigate(Page::Task, cx)),
            )
        });
        let settings = nav_destination(
            "nav-settings",
            page == Page::Settings,
            if self.settings_have_problem() {
                symbols::warning()
            } else {
                symbols::settings()
            },
            course2md::tr!("设置"),
            shown(course2md::tr!("设置")),
            cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
        );
        // The bar between them stays a place to move the window from.
        app_bar(title_bar_height(window), window, cx).child(
            bar.child(home)
                .child(div().flex_1())
                .child(library)
                .children(tasks)
                .child(settings),
        )
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
        Some(motion::enter(
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
                    material_button("open-task-result", ButtonVariant::Tonal)
                        .icon(symbols::arrow_forward())
                        .child(action)
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
                    IconButton::new(
                        "dismiss-task-result",
                        symbols::close(),
                        course2md::tr!("关闭提示"),
                    )
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

impl Desktop {
    /// What just happened, told in passing; it goes by itself or with its close button.
    fn message_snackbar(&self, message: String, cx: &mut Context<Self>) -> impl IntoElement {
        let desktop = cx.weak_entity();
        let dismiss = move |_: &mut Window, cx: &mut App| {
            let _ = desktop.update(cx, |this, cx| {
                this.message = None;
                cx.notify();
            });
        };
        let close = dismiss.clone();
        Snackbar::new(("app-message", notice_key(&message)), message)
            .position(SnackbarPosition::default())
            .close(course2md::tr!("关闭提示"), move |_, window, cx| {
                close(window, cx)
            })
            .on_dismiss(dismiss)
    }
}

impl Desktop {
    /// What the window should paint: the saved appearance against the system's, or a capture's.
    fn theme_target(&self, cx: &App) -> (Scheme, f32) {
        let application = self.preferences.application();
        #[allow(unused_mut)]
        let (mut appearance, mut scale) = (application.appearance.clone(), application.font_scale);
        #[cfg(feature = "capture")]
        {
            crate::capture::override_appearance(&mut appearance);
            scale = crate::capture::scale_override().unwrap_or(scale);
        }
        (
            Scheme::from_palette(appearance.resolve(system_dark(cx))),
            scale,
        )
    }

    /// Starts the theme at its target, with no transition, before Root's first render.
    pub(crate) fn init_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (scheme, scale) = self.theme_target(cx);
        init_component_theme(scheme, scale, cx);
        self.prepare_theme(window, cx);
    }

    /// Moves the theme toward its target. One next-frame callback carries a running
    /// transition; the kit refreshes the window for each step.
    pub(crate) fn prepare_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (scheme, scale) = self.theme_target(cx);
        let running = advance_component_theme(scheme, scale, window, cx);
        let painted = (current_scheme(), scale);
        if self.unprojected_paint != Some(painted) {
            self.unprojected_paint = Some(painted);
            theme::sync_unprojected(scale, cx);
        }
        if running && !self.theme_frame_pending {
            self.theme_frame_pending = true;
            let view = cx.weak_entity();
            window.on_next_frame(move |window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.theme_frame_pending = false;
                    this.prepare_theme(window, cx);
                });
            });
        }
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.onboarding.active {
            let content = self.onboarding_page(window, cx);
            // The shell's brand, without its destinations: setup is modal, so the
            // mark is not a way home here.
            let bar = app_bar(title_bar_height(window), window, cx).child(brand(true));
            return div()
                .id("onboarding-action-root")
                .track_focus(&self.root_focus)
                .tab_stop(false)
                .size_full()
                .child(
                    v_flex()
                        .size_full()
                        .bg(color(ColorRole::SURFACE_CONTAINER))
                        .text_color(color(ColorRole::ON_SURFACE))
                        .text_size(rems(1.))
                        .child(bar)
                        .child(content),
                )
                .children(self.settings_dialogs(window, cx))
                .children(crate::ui::confirm_host(cx))
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
        let page_scrolls = !matches!(self.page, Page::Result | Page::Settings) && !self_scrolling;
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
                            .child(spinner(
                                "opening-note-spinner",
                                course2md::tr!("正在打开笔记…"),
                                cx,
                            ))
                            .child(
                                accessible_text("opening-note", course2md::tr!("正在打开笔记…"))
                                    .role(Role::Status)
                                    .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
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
            motion::page_enter(("page-content", self.page as usize), content)
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
                    v.child(shell_column_for(self.page).pb_3().flex_shrink_0().child(
                        self.group_feedback(crate::preferences::PreferenceGroup::Application, cx),
                    ))
                },
            )
            .when_some(self.workspace_error.clone(), |v, message| {
                v.child(motion::enter(
                    ("workspace-error", notice_key(&message)),
                    shell_column_for(self.page).pb_3().child(
                        notice_banner("workspace-error", NoticeKind::Danger, message)
                            .child(
                                material_button("retry-workspace-records", ButtonVariant::Tonal)
                                    .icon(symbols::refresh())
                                    .child(course2md::tr!("重试"))
                                    .disabled(self.job.is_some() || self.storage_ui.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.retry_workspace_records(window, cx)
                                    })),
                            )
                            .when(self.workspace.is_none(), |row| {
                                row.child(
                                    material_button(
                                        "rebuild-workspace-records",
                                        ButtonVariant::Text,
                                    )
                                    .icon(symbols::restart_alt())
                                    .child(course2md::tr!("备份并重建任务记录"))
                                    .disabled(self.job.is_some() || self.storage_ui.busy)
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.rebuild_workspace_records(window, cx)
                                        },
                                    )),
                                )
                            }),
                    ),
                ))
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .child(
                        div()
                            .id(("page-scroll", self.page as usize))
                            .size_full()
                            .when(page_scrolls, |view| {
                                view.overflow_y_scroll()
                                    .track_scroll(&self.scrolls[self.page as usize])
                            })
                            .child(
                                shell_column_for(self.page)
                                    .when(self.page == Page::Settings, |v| v.max_w_full())
                                    .when(
                                        matches!(self.page, Page::New | Page::Settings)
                                            && is_compact(window),
                                        |v| v.px(px(16.)),
                                    )
                                    .when(!page_scrolls, |v| v.h_full().min_h_0())
                                    // Lists that scroll themselves keep their trailing
                                    // space inside the list, so their last surface is
                                    // never cut off above the window edge.
                                    .when(page_scrolls, |v| v.pb(rem(24.)))
                                    .child(content),
                            ),
                    )
                    .when(page_scrolls, |view| {
                        view.child(Scrollbar::vertical(&self.scrolls[self.page as usize]))
                    }),
            );
        let background = v_flex()
            .size_full()
            .bg(color(ColorRole::SURFACE_CONTAINER))
            .text_color(color(ColorRole::ON_SURFACE))
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
                if !crate::ui::modal_open(window, cx) {
                    this.import_video_from_action(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SearchContent, window, cx| {
                if !crate::ui::modal_open(window, cx) {
                    this.search_content(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                if !crate::ui::modal_open(window, cx) {
                    this.open_settings(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenAbout, window, cx| {
                if !crate::ui::modal_open(window, cx) {
                    this.open_about(window, cx);
                }
            }))
            .child(background)
            .when(self.page == Page::New, |view| {
                view.child(self.workbench_drop_overlay(cx))
            })
            .when_some(self.message.clone(), |view, message| {
                view.child(self.message_snackbar(message, cx))
            })
            .children(self.name_dialog_view(window, cx))
            .children(self.move_dialog_view(cx))
            .children(self.settings_dialogs(window, cx))
            .children(crate::ui::confirm_host(cx))
            .child(self.storage_dialog(window, cx))
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
