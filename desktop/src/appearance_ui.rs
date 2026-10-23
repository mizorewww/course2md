//! Appearance preferences with real palette previews and immediate, durable selection.
use super::*;
use crate::palettes::{ALL, Appearance, PaletteId};
use crate::preferences::PreferenceGroup;
use crate::settings_ui::{field_label, settings_stacked_row};
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

fn sample(palette: PaletteId) -> Div {
    let p = palette.colors();
    // These are the selected palette's actual preview colors, not application chrome.
    div()
        .w_full()
        .h(px(120.))
        .flex_shrink_0()
        .rounded_t(RADIUS_CARD)
        .overflow_hidden()
        .bg(rgb(p.canvas))
        .p(px(12.))
        .child(
            h_flex()
                .h(px(16.))
                .items_center()
                .gap(px(4.))
                .children(
                    [p.accent, p.success, p.warning]
                        .into_iter()
                        .map(|c| div().size(px(5.)).rounded_full().bg(rgb(c))),
                )
                .child(
                    div()
                        .ml(px(8.))
                        .h(px(4.))
                        .w(px(36.))
                        .rounded_full()
                        .bg(rgb(p.muted)),
                ),
        )
        .child(
            h_flex()
                .mt(px(8.))
                .gap(px(8.))
                .items_start()
                .child(
                    div()
                        .w(px(36.))
                        .h(px(80.))
                        .rounded(px(5.))
                        .bg(rgb(p.inset))
                        .child(
                            div()
                                .mx(px(5.))
                                .mt(px(7.))
                                .h(px(5.))
                                .rounded(px(2.))
                                .bg(rgb(p.accent)),
                        ),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .h(px(80.))
                        .p(px(6.))
                        .gap(px(4.))
                        .rounded(px(6.))
                        .bg(rgb(p.surface))
                        .child(
                            div()
                                .h(px(5.))
                                .w(relative(0.65))
                                .rounded_full()
                                .bg(rgb(p.text)),
                        )
                        .child(
                            div()
                                .h(px(4.))
                                .w(relative(0.9))
                                .rounded_full()
                                .bg(rgb(p.border)),
                        )
                        .child(
                            div()
                                .h(px(12.))
                                .w(px(32.))
                                .rounded(px(3.))
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
                .child(sample(palette))
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
            outline_pill("finish-palette-picker")
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
        let content = crate::views::DesktopView::observing(desktop, cx, move |desktop, window, cx| {
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

    fn palette_preset(&self, dark: bool, palette: PaletteId, cx: &mut Context<Self>) -> Div {
        let label = if dark {
            course2md::tr!("深色主题")
        } else {
            course2md::tr!("浅色主题")
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(px(8.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .items_center()
                    .child(
                        if dark { icons::moon() } else { icons::sun() }
                            .size(px(18.))
                            .text_color(color(GRAY)),
                    )
                    .child(field_label(
                        ("palette-preset-label", usize::from(dark)),
                        label,
                    )),
            )
            .child(
                div()
                    .rounded(RADIUS_SMALL)
                    .overflow_hidden()
                    .child(sample(palette).rounded(RADIUS_CARD)),
            )
            .child(outlined_field_label(
                ("palette-preset-field", usize::from(dark)),
                course2md::tr!("主题"),
                TEXT_SETTINGS_AUX,
                color(GRAY),
                control(("palette-preset", usize::from(dark)))
                    .settings_text()
                    .w_full()
                    .h(px(56.))
                    .rounded(FIELD_RADIUS)
                    .text_size(rems(16. / 14.))
                    .font_weight(FontWeight::NORMAL)
                    .accessibility_label(course2md::tr!("{}：{}", label, palette.name()))
                    .label(palette.name())
                    .child(div().flex_1())
                    .child(icons::chevron_down().size(px(18.)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_palette_picker(dark, window, cx);
                    })),
            ))
    }

    pub(crate) fn appearance_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preference = self.preferences.application().appearance.clone();
        let width = crate::views::settings_content_width(window);
        let modes = SingleChoiceGroup::new("appearance-mode", course2md::tr!("外观模式"))
            .segmented()
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
        // Both stored values remain in place in every appearance mode. Measure
        // against every supported name so selecting a longer one cannot move
        // the preference rows below these two fields.
        // The palette grid sits inside the group's card padding.
        let columns = palette_columns(
            (width - 42. * f32::from(window.rem_size()) / 14.).max(0.),
            palette_minimum_width(&ALL, window),
            2,
        );
        v_flex()
            .w_full()
            .min_w_0()
            .gap(px(23.))
            .child(
                crate::settings_ui::SettingsGroup::new("appearance-theme", course2md::tr!("主题"))
                    .child(settings_stacked_row(
                        "appearance-mode-row",
                        course2md::tr!("外观模式"),
                        "",
                        modes,
                    ))
                    .child(
                        crate::settings_ui::setting_surface()
                            .pt(rems(11. / 14.))
                            .pb(rems(24. / 14.))
                            .gap(px(12.))
                            .child(
                                div()
                                    .grid()
                                    .grid_cols(columns)
                                    .w_full()
                                    .min_w_0()
                                    .gap(px(PALETTE_GAP))
                                    .child(self.palette_preset(false, preference.light, cx))
                                    .child(self.palette_preset(true, preference.dark, cx)),
                            ),
                    ),
            )
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
