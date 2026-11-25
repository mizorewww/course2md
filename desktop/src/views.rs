//! Native pages: stable navigation and actions surround independently scrolling content.
use super::*;
use crate::{layout, symbols};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit_material::{
    color::{ColorRole, Scheme},
    components::{
        Badge, ButtonTone, ButtonVariant, Caption, CaptionLabels, MaterialTooltip, NoticeKind,
        Snackbar, SnackbarDuration, SnackbarPosition, TitleBar as AppBar, material_button,
        notice_banner, spinner, title_bar_control, title_bar_room,
    },
    icons::SymbolIcon,
    motion,
    prelude::{CaptionButtons, accessible_text, color, is_compact, rem},
    theme::{advance_component_theme, init_component_theme, system_dark},
};

const SETTINGS_SHELL_WIDTH: f32 = 1108.;

fn app_bar_caption(window: &Window) -> Caption {
    #[cfg(feature = "capture")]
    if let Some(caption) = crate::capture::caption_override() {
        return caption;
    }
    Caption::detect(window)
}

/// The window's title bar on the page's ground. It moves the window and draws what
/// caption buttons the platform lacks; children call `title_bar_control` to take their
/// presses. The Linux close button asks first, as the window manager's close does.
pub(crate) fn app_bar(height: Pixels, window: &Window, cx: &mut Context<Desktop>) -> AppBar {
    let view = cx.weak_entity();
    AppBar::new(
        "app-bar",
        height,
        CaptionLabels {
            minimize: course2md::tr!("最小化").into(),
            maximize: course2md::tr!("最大化").into(),
            restore: course2md::tr!("还原").into(),
            close: course2md::tr!("关闭").into(),
        },
    )
    .caption(app_bar_caption(window))
    .w_full()
    .bg(color(ColorRole::SURFACE_CONTAINER))
    .on_close(move |_, cx| {
        view.update(cx, |this, cx| this.request_close(cx))
            .unwrap_or(true)
    })
}

/// The course2md mark, with the wordmark when there is room.
fn brand(wordmark: bool) -> Div {
    h_flex()
        .items_center()
        .gap(rem(10.))
        // The mark carries its own rounded square, as on 关于, so nothing clips it.
        .child(img("images/course2md.png").size(rem(32.)).flex_shrink_0())
        .when(wordmark, |brand| {
            brand.child(
                div()
                    // 20 between title medium and title large: the existing wordmark,
                    // which no type role matches.
                    .text_size(rem(20.))
                    .text_color(color(ColorRole::ON_SURFACE))
                    .font_weight(FontWeight::MEDIUM)
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
    attention: bool,
    icon: SymbolIcon,
    label: impl Into<SharedString>,
    shown: Option<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let label = label.into();
    let collapsed = shown.as_ref() != Some(&label);
    let button = Badge::small(SharedString::from(format!("{id}-badge")))
        .visible(attention)
        .inset(6.)
        .anchor(title_bar_control(
            material_button(
                id,
                if selected {
                    ButtonVariant::Tonal
                } else {
                    ButtonVariant::Text
                },
            )
            // Neutral at rest, so the current destination's secondary container is the one
            // tint in the bar.
            .tone(if selected {
                ButtonTone::Standard
            } else {
                ButtonTone::Neutral
            })
            .button_size(layout::CONTROL_SIZE)
            .icon(icon)
            .accessibility_label(label.clone())
            .children(shown)
            .on_click(on_click),
        ));
    if collapsed {
        MaterialTooltip::plain(SharedString::from(format!("{id}-tip")), label)
            .trigger(button)
            .into_any_element()
    } else {
        button.into_any_element()
    }
}

pub(crate) use crate::ui::shaped_width;

/// The height of the app bar at the current text scale.
pub(crate) fn title_bar_height(window: &Window) -> Pixels {
    crate::reader_ui::title_bar_height_for_rem(window.rem_size())
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
        Page::Library => rem(layout::LIBRARY_WIDTH).into(),
        Page::New => rems(1200. / 14.).into(),
        // Tasks read at home's column, centred on the same axis.
        Page::Task => rems((760. + layout::SHELL_GUTTER * 2.) / 14.).into(),
        // Sections set the screenshot beside the text, as on the canvas.
        Page::Result => rems(1180. / 14.).into(),
    }
}

/// The max width includes both gutters; layout calculations use the same box.
pub(super) fn shell_content_width(page: Page, window: &Window) -> f32 {
    let width = f32::from(window.bounds().size.width);
    let gutter = if matches!(page, Page::New | Page::Settings) && is_compact(window) {
        16.
    } else {
        layout::SHELL_GUTTER
    };
    (width.min(f32::from(
        shell_column_width(page).to_pixels(window.rem_size()),
    )) - gutter * 2.)
        .max(0.)
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
fn app_bar_density(
    tasks: Option<(&str, Option<&str>)>,
    available: f32,
    window: &Window,
) -> AppBarDensity {
    let scaled = |value: f32| value * f32::from(window.rem_size()) / 14.;
    let gap = f32::from(layout::CONTROL_CLUSTER_SPACING.to_pixels(window.rem_size()));
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
        .map(|shown| nav_destination_width(shown, window) + gap)
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
        .px(px(layout::SHELL_GUTTER))
}

impl Desktop {
    /// Keep task results reachable while the user is on another page.
    fn shell_topbar(&self, window: &mut Window, cx: &mut Context<Self>) -> AppBar {
        let (task_count, tasks_need_attention) =
            self.workspace(cx).map_or((0, false), |workspace| {
                desktop_tasks::task_view::task_navigation_summary(&workspace.state.tasks)
            });
        let page = self.page;
        let height = title_bar_height(window);
        let bar = h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(layout::CONTROL_CLUSTER_SPACING)
            .items_center();
        if page == Page::Settings {
            // Settings is a place of its own: window controls and dragging only; the way
            // back and its title lead its navigation pane instead.
            return app_bar(height, window, cx).child(bar);
        }
        if page == Page::Result {
            // The reader draws its own part, so the shell does not read it. Drawn cached:
            // it is built again when it, the reader, the records or the exports change,
            // not with every repaint of a task's progress in the shell around it.
            return app_bar(height, window, cx).child(
                self.reader_bar.clone().cached(
                    StyleRefinement::default()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .h_full(),
                ),
            );
        }
        // The brand leads the bar and returns to the workbench, as on the M3
        // canvas; destinations follow on the trailing edge with icon and label.
        let has_any_tasks = self
            .workspace(cx)
            .is_some_and(|workspace| !workspace.state.tasks.is_empty());
        let task_label = if task_count > 0 {
            course2md::tr!("任务 · {}", task_count)
        } else {
            course2md::tr!("任务").to_owned()
        };
        let task_count_text = (task_count > 0).then(|| task_count.to_string());
        let show_tasks = has_any_tasks || tasks_need_attention || page == Page::Task;
        let width = window.viewport_size().width;
        let caption = app_bar_caption(window);
        let room = f32::from(title_bar_room(width, height, Some(caption), window));
        let (start, end) = if page == Page::Library {
            let edge = (f32::from(width) - shell_content_width(page, window)) / 2.;
            let trailing = CaptionButtons::trailing_for_style(caption, window);
            let leading = f32::from(width) - room - trailing;
            ((edge - leading).max(0.), (edge - trailing).max(0.))
        } else {
            (0., 0.)
        };
        // Destinations keep their labels; once the bar, measured with the
        // current text, cannot hold them, every destination becomes its icon
        // (the label as its tooltip), and then the wordmark goes.
        let density = app_bar_density(
            show_tasks.then_some((task_label.as_str(), task_count_text.as_deref())),
            room - start - end,
            window,
        );
        let labels = density == AppBarDensity::Labels;
        let wordmark = density != AppBarDensity::Mark;
        let shown = |label: &str| labels.then(|| SharedString::from(label.to_owned()));
        let home_button = title_bar_control(
            material_button("nav-home", ButtonVariant::Text)
                .button_size(layout::CONTROL_SIZE)
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
            false,
            symbols::library_books(),
            course2md::tr!("笔记库"),
            shown(course2md::tr!("笔记库")),
            cx.listener(|this, _, _, cx| {
                this.records.change(cx, |records, _| {
                    records.library.folder_filter = crate::FolderFilter::All
                });
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
                tasks_need_attention,
                symbols::checklist(),
                task_label.clone(),
                shown,
                cx.listener(|this, _, _, cx| this.navigate(Page::Task, cx)),
            )
        });
        let settings = nav_destination(
            "nav-settings",
            page == Page::Settings,
            self.settings_have_problem(),
            symbols::settings(),
            course2md::tr!("设置"),
            shown(course2md::tr!("设置")),
            cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
        );
        // The bar between them stays a place to move the window from.
        app_bar(height, window, cx).child(
            bar.pl(px(start))
                .pr(px(end))
                .child(home)
                .child(div().flex_1())
                .child(library)
                .children(tasks)
                .child(settings),
        )
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
        // Keyed by its text, so entrance motion replays when the text changes.
        let snackbar = Snackbar::new(
            SharedString::from(format!("app-message-{message}")),
            message,
        )
        .duration(SnackbarDuration::Short)
        .position(SnackbarPosition::default())
        .close(course2md::tr!("关闭提示"), move |_, window, cx| {
            close(window, cx)
        })
        .on_dismiss(dismiss);
        // Over the screenshot viewer it rises above the viewer's action row,
        // whose stepping and zoom controls sit where it would otherwise be.
        let clearance = self.reader.read(cx).viewer_snackbar_clearance();
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom(clearance.unwrap_or_default())
            .child(snackbar)
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
        let percent = (self.settings_ui.scale_preview.unwrap_or(scale) * 100.).round();
        if self.settings_ui.font_scale.read(cx).value().end() != percent {
            self.settings_ui.font_scale.update(cx, |state, cx| {
                state.set_value(percent, window, cx);
            });
        }
        let running = advance_component_theme(scheme, scale, window, cx);
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
        #[cfg(feature = "capture")]
        crate::capture::count_render(crate::capture::Counted::Desktop);
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
        // The task cards show on Tasks, the workbench and the library.
        let facts = self.shell_facts(cx);
        self.tasks.update(cx, |page, cx| page.set_facts(facts, cx));
        let content = match self.page {
            Page::New => self.new_page(window, cx),
            Page::Task => self.tasks.clone().into_any_element(),
            Page::Library => self.library_page(window, cx),
            Page::Settings => self.settings_page(window, cx),
            // Drawn cached: it is built again when it, the records or the exports
            // change, not with every frame of the shell around it.
            Page::Result => self
                .reader
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
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
                !matches!(self.page, Page::Library | Page::Result) && self.opened(cx).reading,
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
        let topbar = self.shell_topbar(window, cx);
        let model_notice = self.onboarding_background_notice(cx);
        let body = v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .w_full()
            .when_some(model_notice, |body, notice| {
                body.child(shell_column_for(self.page).py_2().child(notice))
            })
            .when_some(self.workspace_error.clone(), |v, message| {
                v.child(motion::enter(
                    SharedString::from(format!("workspace-error-{message}")),
                    shell_column_for(self.page).pb_3().child(
                        notice_banner("workspace-error", NoticeKind::Danger, message)
                            .child(
                                material_button("retry-workspace-records", ButtonVariant::Tonal)
                                    .icon(symbols::refresh())
                                    .child(course2md::tr!("重试"))
                                    .disabled(
                                        self.work.read(cx).job.is_some() || self.storage_ui.busy,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.retry_workspace_records(window, cx)
                                    })),
                            )
                            .when(self.records.read(cx).workspace.is_none(), |row| {
                                row.child(
                                    material_button(
                                        "rebuild-workspace-records",
                                        ButtonVariant::Text,
                                    )
                                    .icon(symbols::restart_alt())
                                    .child(course2md::tr!("备份并重建任务记录"))
                                    .disabled(
                                        self.work.read(cx).job.is_some() || self.storage_ui.busy,
                                    )
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
            .children((self.page == Page::Library).then(layout::library_app_bar_divider))
            .child(body);
        crate::clipboard::intercept_view_copies(div().id("desktop-action-root"))
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
