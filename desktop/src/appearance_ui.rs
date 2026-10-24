//! Appearance preferences with real palette previews and immediate, durable selection.
use super::*;
use crate::palettes::{ALL, Appearance, PaletteId};
use crate::preferences::PreferenceGroup;
use crate::theme::*;

const PALETTE_GAP: f32 = 16.;
const PICKER_GUTTER: f32 = 24.;
const PICKER_PADDING: f32 = 24.;
const PICKER_BORDER: f32 = 1.;
const PICKER_MAX_WIDTH: f32 = 920.;

fn picker_width(window: &Window) -> f32 {
    (f32::from(window.viewport_size().width) - PICKER_GUTTER * 2.).clamp(0., PICKER_MAX_WIDTH)
}

fn picker_inner_width(window: &Window) -> f32 {
    // The dialog below owns this width and padding. Its scrollbar overlays the
    // body, so there is no second page-width estimate or scrollbar deduction.
    (picker_width(window) - (PICKER_PADDING + PICKER_BORDER) * 2.).max(0.)
}

fn palette_minimum_width(palettes: &[PaletteId], window: &Window) -> f32 {
    let mut style = window.text_style();
    style.font_weight = FontWeight::SEMIBOLD;
    let label_width = palettes
        .iter()
        .map(|palette| {
            let label = SharedString::from(palette.name());
            f32::from(
                window
                    .text_system()
                    .shape_line(
                        label.clone(),
                        TEXT_SETTINGS_BODY.to_pixels(window.rem_size()),
                        &[style.to_run(label.len())],
                        None,
                    )
                    .width,
            )
        })
        .fold(0., f32::max);
    // Match the actual footer: 12px padding, 8px gap, an 18px marker and the
    // card's 2px border. A larger font changes the required width immediately.
    (label_width + 24. + 8. + 18. + 4.).max(168.)
}

fn palette_columns(width: f32, minimum_width: f32, maximum: u16) -> u16 {
    (((width + PALETTE_GAP) / (minimum_width + PALETTE_GAP)).floor() as u16).clamp(1, maximum)
}

/// A miniature window in the palette's own colors (not application chrome):
/// window controls, a sidebar and a page with a title, text and a button.
fn sample(palette: PaletteId, height: f32) -> Div {
    let p = palette.colors();
    let bar = |height: f32, width: gpui::DefiniteLength, fill: u32| {
        div()
            .h(px(height))
            .w(width)
            .flex_shrink_0()
            .rounded_full()
            .bg(rgb(fill))
    };
    v_flex()
        .w_full()
        .h(px(height))
        .flex_shrink_0()
        .overflow_hidden()
        .bg(rgb(p.canvas))
        .p(px(10.))
        .gap(px(8.))
        .child(
            h_flex().gap(px(4.)).children(
                [p.danger, p.warning, p.success]
                    .into_iter()
                    .map(|c| div().size(px(6.)).rounded_full().bg(rgb(c))),
            ),
        )
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .gap(px(8.))
                .items_stretch()
                .child(
                    v_flex()
                        .w(relative(0.26))
                        .rounded(px(8.))
                        .bg(rgb(p.inset))
                        .p(px(8.))
                        .gap(px(6.))
                        .child(bar(6., relative(1.), p.accent))
                        .child(bar(6., relative(1.), p.subtle))
                        .child(bar(6., relative(1.), p.subtle)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .rounded(px(8.))
                        .bg(rgb(p.surface))
                        .p(px(10.))
                        .gap(px(7.))
                        .child(bar(7., relative(0.7), p.text))
                        .child(bar(5., relative(0.9), p.border))
                        .child(bar(5., relative(0.8), p.border))
                        .child(div().flex_1())
                        .child(
                            div()
                                .h(px(16.))
                                .w(px(40.))
                                .rounded(px(8.))
                                .bg(rgb(p.accent)),
                        ),
                ),
        )
}

fn palette_choice(palette: PaletteId, selected: bool, amount: f32) -> gpui_base::Button {
    selection_card(("palette", palette as usize), selected, amount)
        .w_full()
        .p(px(0.))
        .overflow_hidden()
        .accessibility_label(course2md::tr!(
            "{}，{}{}",
            palette.name(),
            if palette.is_dark() {
                course2md::tr!("深色主题")
            } else {
                course2md::tr!("浅色主题")
            },
            if selected {
                course2md::tr!("，已选择")
            } else {
                ""
            }
        ))
        .when(cfg!(test), |card| {
            card.debug_selector(move || format!("palette-card-{}", palette as usize))
        })
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .text_left()
                .child(sample(palette, 120.).rounded_t(RADIUS_CARD))
                .child(
                    h_flex()
                        .w_full()
                        .min_h(rems(3.43))
                        .flex_1()
                        .gap(px(8.))
                        .p(px(12.))
                        .items_center()
                        .bg(theme::blend(
                            color(SURFACE),
                            color(SECONDARY_CONTAINER),
                            amount,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(TEXT_SETTINGS_BODY)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color(INK))
                                .when(cfg!(test), |label| {
                                    label.debug_selector(move || {
                                        format!("palette-label-{}", palette as usize)
                                    })
                                })
                                .child(palette.name()),
                        )
                        .child(
                            icons::check_circle()
                                .text_color(color(ACCENT))
                                .size(px(18.))
                                .opacity(amount)
                                .flex_shrink_0(),
                        ),
                ),
        )
}

impl Desktop {
    fn palette_grid(
        &self,
        dark: bool,
        selected_palette: PaletteId,
        width: f32,
        scroll: ScrollHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let cards = ALL
            .into_iter()
            .filter(|id| id.is_dark() == dark)
            .collect::<Vec<_>>();
        let columns = palette_columns(width, palette_minimum_width(&cards, window), 4);
        div()
            .grid()
            .grid_cols(columns)
            .w_full()
            .min_w_0()
            .gap(px(PALETTE_GAP))
            .children(cards.into_iter().map(|palette| {
                let selected = palette == selected_palette;
                let amount = crate::motion::value(
                    ("palette-selection", palette as usize),
                    if selected { 1. } else { 0. },
                    window,
                    cx,
                );
                crate::focus_scroll::RevealFocus::new(
                    ("palette-choice-focus", palette as usize),
                    palette_choice(palette, selected, amount).on_click(cx.listener(
                        move |this, _, window, cx| {
                            let previous = if dark {
                                this.preferences.application().appearance.dark
                            } else {
                                this.preferences.application().appearance.light
                            };
                            if previous == palette {
                                return;
                            }
                            let mut next = this.application_edit_base();
                            next.appearance.select(palette);
                            if this.commit_application(next, cx) {
                                window.refresh();
                            }
                        },
                    )),
                    scroll.clone(),
                )
            }))
    }

    fn palette_picker_content(
        &self,
        dark: bool,
        scroll: ScrollHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let preference = &self.preferences.application().appearance;
        let grid = self.palette_grid(
            dark,
            if dark {
                preference.dark
            } else {
                preference.light
            },
            picker_inner_width(window),
            scroll.clone(),
            window,
            cx,
        );
        let available_height = (f32::from(window.viewport_size().height)
            - f32::from(task_dialog_top(window))
            - PICKER_GUTTER
            - PICKER_PADDING * 2.
            - PICKER_BORDER * 2.
            - f32::from(window.rem_size()) * 3.
            - 16.)
            .max(120.);
        let save_failed = self
            .settings_group_notice(PreferenceGroup::Application)
            .is_some_and(|(_, error)| error);
        let finish = if save_failed {
            outline_pill("finish-palette-picker", cx)
                .settings_text()
                .icon(icons::close())
                .label(course2md::tr!("关闭"))
        } else {
            primary_pill("finish-palette-picker")
                .icon(icons::check_circle())
                .label(course2md::tr!("完成"))
        };
        v_flex()
            .w_full()
            .min_w_0()
            .max_h(px(available_height))
            .gap(px(16.))
            .when(save_failed, |view| {
                view.child(
                    self.group_feedback_with_retry_emphasis(PreferenceGroup::Application, true, cx)
                        .flex_shrink_0(),
                )
            })
            .child(
                div()
                    .id(("palette-picker-scroll", usize::from(dark)))
                    .w_full()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .child(grid),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .flex_shrink_0()
                    .child(finish.on_click(|_, window, cx| window.close_dialog(cx))),
            )
    }

    fn open_palette_picker(&mut self, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        let desktop = cx.entity();
        let scroll = ScrollHandle::new();
        let content =
            crate::views::DesktopView::observing(desktop, cx, move |desktop, window, cx| {
                desktop
                    .palette_picker_content(dark, scroll.clone(), window, cx)
                    .into_any_element()
            });
        window.open_dialog(cx, move |dialog, window, _| {
            dialog
                .title(
                    crate::settings_ui::settings_value(
                        "palette-dialog-title",
                        if dark {
                            course2md::tr!("深色主题")
                        } else {
                            course2md::tr!("浅色主题")
                        },
                    )
                    .text_size(TEXT_TITLE)
                    .font_weight(FontWeight::SEMIBOLD),
                )
                .w(px(picker_width(window)))
                .p(px(PICKER_PADDING))
                .margin_top(task_dialog_top(window))
                .child(content.clone())
        });
    }

    /// One saved palette as a tile of the 主题 group: its real preview, which
    /// mode it serves and its name. The palette in use carries 使用中 and an
    /// accent ring; the tile opens the palette chooser.
    fn palette_preset(
        &self,
        dark: bool,
        palette: PaletteId,
        active: bool,
        outer: [bool; 4],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let caption = if dark {
            course2md::tr!("深色主题")
        } else {
            course2md::tr!("浅色主题")
        };
        crate::settings_ui::tile_button(("palette-preset", usize::from(dark)), outer, cx)
            .p(rems(16. / 14.))
            .border_2()
            .border_color(gpui::Rgba {
                a: if active { 1. } else { 0. },
                ..color(ACCENT)
            })
            .accessibility_label(course2md::tr!("{}：{}", caption, palette.name()))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rems(1.))
                    .child(
                        div()
                            .w_full()
                            .rounded(px(16.))
                            .overflow_hidden()
                            .child(sample(palette, 128.).rounded(px(16.))),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap(rems(12. / 14.))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(rems(2. / 14.))
                                    .child(
                                        div()
                                            .text_size(rems(13. / 14.))
                                            .line_height(rems(18. / 14.))
                                            .text_color(color(GRAY))
                                            .child(caption),
                                    )
                                    .child(
                                        div()
                                            .whitespace_normal()
                                            .text_size(TEXT_SETTINGS_BODY)
                                            .line_height(rems(22. / 14.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(color(INK))
                                            .child(palette.name()),
                                    ),
                            )
                            .when(active, |row| {
                                row.child(badge(BadgeKind::Tonal).child(course2md::tr!("使用中")))
                            })
                            .child(crate::settings_ui::tile_chevron()),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_palette_picker(dark, window, cx);
            }))
            .into_any_element()
    }

    pub(crate) fn appearance_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preference = self.preferences.application().appearance.clone();
        let modes = SingleChoiceGroup::new("appearance-mode", course2md::tr!("外观模式"))
            .segmented()
            .full_width()
            .settings_text()
            .options([
                ("system", course2md::tr!("跟随系统")),
                ("light", course2md::tr!("浅色")),
                ("dark", course2md::tr!("深色")),
            ])
            .icon("system", icons::computer())
            .icon("light", icons::sun())
            .icon("dark", icons::moon())
            .selected(match preference.mode {
                Appearance::System => "system",
                Appearance::Light => "light",
                Appearance::Dark => "dark",
            })
            .reveal_in(self.scrolls[Page::Settings as usize].clone())
            .on_change(cx.listener(|this, selected: &SharedString, window, cx| {
                let mode = match selected.as_ref() {
                    "light" => Appearance::Light,
                    "dark" => Appearance::Dark,
                    _ => Appearance::System,
                };
                let mut next = this.application_edit_base();
                if next.appearance.mode == mode {
                    return;
                }
                next.appearance.mode = mode;
                if this.commit_application(next, cx) {
                    window.refresh();
                }
            }));
        // The palette in use is the one painted now: the mode's choice, or
        // the system's when following it.
        let dark_active = theme::is_dark_paint();
        // Both tiles always stay in place, whatever the mode, so a mode change
        // never moves the preference groups below them. They sit side by side
        // below the mode tile, or stack when the pane is narrow.
        let side_by_side = crate::settings_ui::settings_layout(window).content
            >= 440. * f32::from(window.rem_size()) / 14.;
        let (light_corners, dark_corners) = if side_by_side {
            ([false, false, false, true], [false, false, true, false])
        } else {
            ([false; 4], [false, false, true, true])
        };
        let light = self.palette_preset(false, preference.light, !dark_active, light_corners, cx);
        let dark = self.palette_preset(true, preference.dark, dark_active, dark_corners, cx);
        let group =
            crate::settings_ui::SettingsGroup::new("appearance-theme", course2md::tr!("主题"))
                .child(crate::settings_ui::settings_stacked_row(
                    "appearance-mode-row",
                    course2md::tr!("外观模式"),
                    "",
                    modes,
                ));
        let presets = if side_by_side {
            group.pair(move |_| light, move |_| dark)
        } else {
            group.tile(move |_| light).tile(move |_| dark)
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(px(28.))
            .child(presets)
            .child(self.appearance_controls(cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{PALETTE_GAP, palette_choice, palette_columns, palette_minimum_width};
    use crate::palettes::ALL;
    use gpui::{
        Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
        TestAppContext, VisualTestContext, Window, div, px,
    };

    struct PaletteLayoutHarness {
        width: f32,
        rem: f32,
    }

    impl Render for PaletteLayoutHarness {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(px(self.rem));
            let cards = ALL
                .into_iter()
                .filter(|id| id.is_dark())
                .collect::<Vec<_>>();
            let columns = palette_columns(self.width, palette_minimum_width(&cards, window), 4);
            div()
                .w(px(self.width))
                .grid()
                .grid_cols(columns)
                .gap(px(PALETTE_GAP))
                .debug_selector(|| "palette-layout-grid".into())
                .children(
                    cards
                        .into_iter()
                        .map(|palette| palette_choice(palette, false, 0.)),
                )
        }
    }

    fn inspect_palette_layout(cx: &mut VisualTestContext) -> usize {
        let grid = cx.debug_bounds("palette-layout-grid").unwrap();
        let mut first_row = 0;
        for palette in ALL.into_iter().filter(|id| id.is_dark()) {
            let card = cx
                .debug_bounds(Box::leak(
                    format!("palette-card-{}", palette as usize).into_boxed_str(),
                ))
                .unwrap();
            let label = cx
                .debug_bounds(Box::leak(
                    format!("palette-label-{}", palette as usize).into_boxed_str(),
                ))
                .unwrap();
            assert!(card.left() >= grid.left() && card.right() <= grid.right() + px(0.5));
            assert!(
                card.size.height >= px(144.),
                "the preview keeps its natural height"
            );
            let required = cx.update(|window, _| palette_minimum_width(&[palette], window) - 54.);
            assert!(
                label.size.width + px(0.5) >= px(required),
                "{} needs room for its name",
                palette.name()
            );
            if card.top() == grid.top() {
                first_row += 1;
            }
        }
        first_row
    }

    #[gpui::test]
    fn palette_names_reflow_on_the_first_font_and_width_change(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| PaletteLayoutHarness {
            width: 760.,
            rem: 14.,
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let original_columns = inspect_palette_layout(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.rem = 28.;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        let large_columns = inspect_palette_layout(cx);
        assert!(
            large_columns < original_columns,
            "200% text must reduce columns before labels collide"
        );
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.width = 520.;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert!(inspect_palette_layout(cx) <= large_columns);
    }
}
