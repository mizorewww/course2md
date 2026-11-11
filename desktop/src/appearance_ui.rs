//! Appearance preferences with real palette previews and immediate, durable selection.
use crate::preferences::{InterfaceLanguage, PreferenceGroup};
use crate::{Desktop, symbols, ui};
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{
    color::{Scheme, palette::ALL},
    prelude::*,
    theme::{is_dark, rgba},
    tokens::shape::OuterCorners,
};

/// Between palette cards, and the narrowest a card gets, in design px.
const CARD_GAP: f32 = 16.;
const CARD_MIN_WIDTH: f32 = 150.;

/// As many columns as `width` holds, each following the text size so a longer name
/// or larger text reduces the columns before the names collide.
/// The picker's widest, in dp: four previews across with their names on one line.
const PICKER_WIDTH: f32 = 920.;

fn palette_columns(width: f32, rem_size: f32) -> u16 {
    let unit = rem_size / 14.;
    (((width + CARD_GAP * unit) / ((CARD_MIN_WIDTH + CARD_GAP) * unit)).floor() as u16).clamp(1, 4)
}

/// A miniature window in the palette's own colors, not application chrome: window
/// controls, a sidebar and a page with a title, text and a button.
fn sample(palette: PaletteId, height: Rems) -> Div {
    let scheme = Scheme::from_palette(palette);
    let paint = |role| rgba(scheme.get(role));
    let bar = |height: f32, width: f32, role| {
        div()
            .h(rem(height))
            .w(relative(width))
            .flex_shrink_0()
            .rounded_full()
            .bg(paint(role))
    };
    v_flex()
        .w_full()
        .h(height)
        .flex_shrink_0()
        .overflow_hidden()
        .bg(paint(ColorRole::SURFACE_CONTAINER))
        .p(rem(10.))
        .gap(rem(8.))
        .child(
            h_flex().gap(rem(4.)).children(
                [ColorRole::ERROR, ColorRole::WARNING, ColorRole::SUCCESS]
                    .map(|role| div().size(rem(6.)).rounded_full().bg(paint(role))),
            ),
        )
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .gap(rem(8.))
                .items_stretch()
                .child(
                    v_flex()
                        .w(relative(0.26))
                        .rounded(RADIUS_SMALL)
                        .bg(paint(ColorRole::INSET))
                        .p(rem(8.))
                        .gap(rem(6.))
                        .child(bar(6., 1., ColorRole::PRIMARY))
                        .child(bar(6., 1., ColorRole::OUTLINE_VARIANT))
                        .child(bar(6., 1., ColorRole::OUTLINE_VARIANT)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .rounded(RADIUS_SMALL)
                        .bg(paint(ColorRole::SURFACE))
                        .p(rem(10.))
                        .gap(rem(7.))
                        .child(bar(7., 0.7, ColorRole::ON_SURFACE))
                        .child(bar(5., 0.9, ColorRole::OUTLINE_VARIANT))
                        .child(bar(5., 0.8, ColorRole::OUTLINE_VARIANT))
                        .child(div().flex_1())
                        .child(
                            div()
                                .h(rem(16.))
                                .w(rem(40.))
                                .rounded(RADIUS_SMALL)
                                .bg(paint(ColorRole::PRIMARY)),
                        ),
                ),
        )
}

/// The label a palette preview goes under: which mode it serves.
fn mode_label(dark: bool) -> &'static str {
    if dark {
        course2md::tr!("深色主题")
    } else {
        course2md::tr!("浅色主题")
    }
}

impl Desktop {
    /// The appearance this page shows: the saved one, or in capture builds the palette
    /// a capture paints instead, so the page agrees with the colors on screen.
    fn displayed_appearance(&self) -> ThemePreferences {
        #[allow(unused_mut)]
        let mut appearance = self.preferences.application().appearance.clone();
        #[cfg(feature = "capture")]
        crate::capture::override_appearance(&mut appearance);
        appearance
    }

    /// One palette as a selectable card: the kit draws its selection (outline, tint and
    /// check) and the group announces it; the caption is tonal while chosen (S11).
    fn palette_card(palette: PaletteId, selected: bool, preview: Rems) -> MaterialCard {
        MaterialCard::new(("palette", palette as usize), CardVariant::Outlined)
            .rounded(RADIUS_LARGE)
            .w_full()
            .accessibility_label(course2md::tr!(
                "{}，{}",
                palette.name(),
                mode_label(palette.is_dark())
            ))
            .selected(selected)
            .child(sample(palette, preview).rounded_t(RADIUS_LARGE))
            .child(
                h_flex()
                    .w_full()
                    .min_h(rem(48.))
                    .gap(rem(8.))
                    .p(rem(12.))
                    .items_center()
                    .when(selected, |caption| {
                        caption.bg(color(ColorRole::SECONDARY_CONTAINER))
                    })
                    .child(
                        TileScale::Regular
                            .label(("palette-name", palette as usize), palette.name())
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal(),
                    ),
            )
    }

    /// The palette chooser: every palette of one mode as a real preview. A choice
    /// applies at once; a failed save stays visible here with its retry.
    pub(crate) fn palette_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (dark, open) = self.settings_ui.palette_picker;
        let appearance = self.displayed_appearance();
        let selected = if dark {
            appearance.dark
        } else {
            appearance.light
        };
        let inner_width = (f32::from(window.viewport_size().width) - 48.)
            .min(rem(PICKER_WIDTH).to_pixels(window.rem_size()).as_f32())
            - rem(48.).to_pixels(window.rem_size()).as_f32();
        let columns = palette_columns(inner_width, f32::from(window.rem_size()));
        // A row of cards fits the body however large the text: the preview gives up
        // height before its name is cut off.
        let rem_size = f32::from(window.rem_size());
        let unit = rem_size / 14.;
        let body = f32::from(crate::settings_ui::dialog_body_height(window));
        let preview = rem(((body / unit) - 48. - 8.).clamp(72., 120.));
        let failed = self
            .settings_group_notice(PreferenceGroup::Application)
            .is_some_and(|(_, error)| error);
        let scroll = self.settings_ui.palette_scroll.clone();
        let palettes: Vec<PaletteId> = ALL
            .into_iter()
            .filter(|palette| palette.is_dark() == dark)
            .collect();
        let choices = palettes.clone();
        let grid = palettes
            .iter()
            .fold(
                CardGroup::new("palette-group", mode_label(dark))
                    .grid()
                    .grid_cols(columns)
                    .w_full()
                    .min_w_0()
                    .gap(rem(CARD_GAP)),
                |group, &palette| {
                    group.card(Self::palette_card(palette, palette == selected, preview))
                },
            )
            .on_change(cx.listener(move |this, index: &usize, _, cx| {
                let Some(&palette) = choices.get(*index) else {
                    return;
                };
                let mut next = this.application_edit_base();
                next.appearance.select(palette);
                this.commit_application(next, cx);
            }));
        let close = cx.listener(|this, _: &ClickEvent, _, cx| this.close_palette_picker(cx));
        MaterialDialog::new("palette-dialog")
            .container(ColorRole::SURFACE)
            .headline(mode_label(dark))
            // Four previews across on a large window, as before the migration (S11).
            .size(DialogSize::FullWidth)
            .max_width(PICKER_WIDTH)
            .content(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .max_h(crate::settings_ui::dialog_body_height(window))
                    .px(rem(24.))
                    .gap(rem(16.))
                    .when(failed, |view| {
                        view.child(self.group_feedback(PreferenceGroup::Application, cx))
                    })
                    .child(
                        div()
                            .id("palette-scroll")
                            .min_w_0()
                            .min_h_0()
                            // Room for a focused card's ring, which the scroll would cut.
                            .m(-ui::FOCUS_RING_ROOM)
                            .p(ui::FOCUS_RING_ROOM)
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .child(grid),
                    ),
            )
            .action(
                material_button(
                    "finish-palette-picker",
                    if failed {
                        ButtonVariant::Outlined
                    } else {
                        ButtonVariant::Filled
                    },
                )
                .when(failed, |button| button.tone(ButtonTone::Accent))
                .icon(if failed {
                    symbols::close()
                } else {
                    symbols::check_circle()
                })
                .child(if failed {
                    course2md::tr!("关闭")
                } else {
                    course2md::tr!("完成")
                })
                .on_click(close),
            )
            .modal(open)
            .on_dismiss({
                let this = cx.weak_entity();
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.close_palette_picker(cx));
                }
            })
            .into_any_element()
    }

    pub(crate) fn open_palette_picker(&mut self, dark: bool, cx: &mut Context<Self>) {
        self.settings_ui.palette_picker = (dark, true);
        cx.notify();
    }

    pub(crate) fn close_palette_picker(&mut self, cx: &mut Context<Self>) {
        self.settings_ui.palette_picker.1 = false;
        cx.notify();
    }

    /// One saved palette as a tile: its real preview, the mode it serves and its name.
    /// The palette painted now carries 使用中. The tile opens the chooser.
    fn palette_preset(
        &self,
        dark: bool,
        palette: PaletteId,
        outer: OuterCorners,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scale = TileScale::Regular;
        scale
            .button(("palette-preset", usize::from(dark)), outer)
            .accessibility_label(course2md::tr!("{}：{}", mode_label(dark), palette.name()))
            .on_click(cx.listener(move |this, _, _, cx| this.open_palette_picker(dark, cx)))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(14.))
                    .child(
                        div()
                            .w_full()
                            .rounded(RADIUS_LARGE)
                            .overflow_hidden()
                            .child(sample(palette, rem(128.))),
                    )
                    .child(
                        scale
                            .row()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(scale.support(
                                        ("palette-preset-mode", usize::from(dark)),
                                        mode_label(dark),
                                    ))
                                    .child(scale.label(
                                        ("palette-preset-name", usize::from(dark)),
                                        palette.name(),
                                    )),
                            )
                            .when(is_dark() == dark, |row| {
                                row.child(
                                    status_chip(StatusKind::Tonal).child(course2md::tr!("使用中")),
                                )
                            })
                            .child(tile_chevron()),
                    ),
            )
            .into_any_element()
    }

    fn interface_language_field(&self, cx: &mut Context<Self>) -> ui::Field {
        let current = self.application_edit_base().language;
        let label = |language: InterfaceLanguage| match language {
            InterfaceLanguage::System => course2md::tr!("跟随系统"),
            other => other.resolve().native_name(),
        };
        let value: SharedString = if current == InterfaceLanguage::System {
            format!("{} · {}", label(current), current.resolve().native_name()).into()
        } else {
            label(current).into()
        };
        // 跟随系统 stands apart from the fixed languages.
        let items = ui::choice_items(
            InterfaceLanguage::ALL
                .into_iter()
                .map(|language| (language, label(language))),
            &current,
            cx.listener(|this, language: &InterfaceLanguage, window, cx| {
                let mut next = this.application_edit_base();
                next.language = *language;
                if this.commit_application(next, cx) {
                    this.apply_interface_language(*language, window, cx);
                }
            }),
        )
        .enumerate()
        .map(|(index, item)| item.divider(index == 0));
        ui::Field::menu(
            "app-language",
            course2md::tr!("界面语言"),
            value,
            MaterialMenu::new("app-language-items")
                .width(240.)
                .items(items),
        )
    }

    pub(crate) fn appearance_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let appearance = self.displayed_appearance();
        let modes = ConnectedButtonGroup::new("appearance-mode", course2md::tr!("外观模式"))
            .tone(ButtonTone::Neutral)
            .segmented()
            .button_size(ButtonSize::Small)
            .options([
                ("system", course2md::tr!("跟随系统")),
                ("light", course2md::tr!("浅色")),
                ("dark", course2md::tr!("深色")),
            ])
            .icon("system", symbols::computer())
            .icon("light", symbols::light_mode())
            .icon("dark", symbols::dark_mode())
            .selected(match appearance.mode {
                Appearance::System => "system",
                Appearance::Light => "light",
                Appearance::Dark => "dark",
            })
            .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                let mode = match selected.as_ref() {
                    "light" => Appearance::Light,
                    "dark" => Appearance::Dark,
                    _ => Appearance::System,
                };
                let mut next = this.application_edit_base();
                if next.appearance.mode != mode {
                    next.appearance.mode = mode;
                    this.commit_application(next, cx);
                }
            }));
        // Both previews always stay in place, whatever the mode, so a mode change never
        // moves the preference groups below them. They stack when the pane is narrow.
        let side_by_side = !is_compact(window);
        let light_corners = [false, false, false, side_by_side];
        let dark_corners = [false, false, true, !side_by_side];
        let light = self.palette_preset(false, appearance.light, light_corners, cx);
        let dark = self.palette_preset(true, appearance.dark, dark_corners, cx);
        let application = self.preferences.application();
        let scales = crate::preferences::FONT_SCALES;
        let font_scale =
            ConnectedButtonGroup::new("app-font-scale", course2md::tr!("界面文字大小"))
                .tone(ButtonTone::Neutral)
                .segmented()
                .button_size(ButtonSize::Small)
                .options(scales.into_iter().map(|scale| {
                    let percent = (scale * 100.).round() as u32;
                    (percent.to_string(), format!("{percent}%"))
                }))
                .selected(((application.font_scale * 100.).round() as u32).to_string())
                .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                    let Ok(percent) = selected.parse::<f32>() else {
                        return;
                    };
                    let mut next = this.application_edit_base();
                    next.font_scale = percent / 100.;
                    this.commit_application(next, cx);
                }));
        let previews = ui::Field::tile("palette-previews", move |_| {
            div()
                .grid()
                .grid_cols(if side_by_side { 2 } else { 1 })
                .w_full()
                .min_w_0()
                .gap(TILE_GAP)
                .child(light)
                .child(dark)
                .into_any_element()
        });
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(28.))
            .child(self.settings_fields(
                "appearance-theme",
                course2md::tr!("主题"),
                vec![
                    ui::Field::below("appearance-mode-row", course2md::tr!("外观模式"), modes),
                    previews,
                ],
            ))
            .child(self.settings_fields(
                "appearance-motion",
                course2md::tr!("界面偏好"),
                vec![
                    self.interface_language_field(cx),
                    ui::Field::below(
                        "app-font-scale-row",
                        course2md::tr!("界面文字大小"),
                        font_scale,
                    ),
                    ui::Field::switch(
                        "app-reduce-motion",
                        course2md::tr!("减少动态效果"),
                        application.desktop.reduce_motion,
                        cx.listener(|this, on: &bool, _, cx| {
                            let mut next = this.application_edit_base();
                            next.desktop.reduce_motion = *on;
                            this.commit_application(next, cx);
                        }),
                    )
                    .hint(course2md::tr!("关闭过渡动画和进度条的流动效果")),
                ],
            ))
            .child(self.settings_fields(
                "appearance-home",
                course2md::tr!("主页"),
                vec![
                    ui::Field::switch(
                        "app-home-recent-notes",
                        course2md::tr!("显示最近的笔记"),
                        application.desktop.home_recent_notes,
                        cx.listener(|this, on: &bool, _, cx| {
                            let mut next = this.application_edit_base();
                            next.desktop.home_recent_notes = *on;
                            this.commit_application(next, cx);
                        }),
                    )
                    .hint(course2md::tr!("关闭后主页不显示已完成的笔记")),
                ],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::palette_columns;

    #[test]
    fn larger_text_and_narrower_panes_reduce_the_palette_columns() {
        assert_eq!(palette_columns(760., 14.), 4);
        assert!(palette_columns(760., 28.) < palette_columns(760., 14.));
        assert!(palette_columns(520., 28.) <= palette_columns(760., 28.));
        assert_eq!(palette_columns(100., 28.), 1);
    }
}
