//! Shared colors keep controls, navigation and document surfaces consistent.
use gpui::{App, px, rgb};
use gpui_component::{Theme, ThemeMode};

pub const CANVAS: u32 = 0xf8f8f6;
pub const SURFACE: u32 = 0xffffff;
pub const SIDEBAR: u32 = 0xe8ebf0;
pub const INK: u32 = 0x252a32;
pub const MUTED: u32 = 0x485363;
pub const LINE: u32 = 0xe3e5e2;
pub const CONTROL: u32 = 0x7b8492;
pub const BLUE: u32 = 0x315bc5;
pub const TINT: u32 = 0xeaf0ff;
pub const SUCCESS: u32 = 0x237552;

/// Navigation has an explicit current-location treatment, separate from form toggles.
pub fn navigation(
    button: gpui_component::button::Button,
    selected: bool,
) -> gpui_component::button::Button {
    use gpui::{Styled, prelude::FluentBuilder};
    use gpui_component::{Selectable, button::ButtonVariants};
    button
        .ghost()
        .selected(selected)
        .toggled(selected)
        .text_color(rgb(if selected { SURFACE } else { INK }))
        .when(selected, |button| {
            button.bg(rgb(BLUE)).font_weight(gpui::FontWeight::SEMIBOLD)
        })
}

pub fn init(cx: &mut App) {
    Theme::change(ThemeMode::Light, None, cx);
    let theme = Theme::global_mut(cx);
    theme.font_size = px(14.);
    theme.radius = px(6.);
    theme.radius_lg = px(8.);
    let colors = &mut theme.colors;
    colors.background = rgb(CANVAS).into();
    colors.foreground = rgb(INK).into();
    colors.border = rgb(CONTROL).into();
    colors.input = rgb(CONTROL).into();
    colors.muted = rgb(SIDEBAR).into();
    colors.muted_foreground = rgb(MUTED).into();
    colors.accent = rgb(TINT).into();
    colors.accent_foreground = rgb(BLUE).into();
    colors.primary = rgb(BLUE).into();
    colors.progress_bar = rgb(BLUE).into();
    colors.primary_foreground = rgb(SURFACE).into();
    colors.primary_hover = rgb(0x284eae).into();
    colors.primary_active = rgb(0x214396).into();
    colors.button = rgb(SURFACE).into();
    colors.button_foreground = rgb(INK).into();
    colors.secondary_foreground = rgb(INK).into();
    colors.button_hover = rgb(0xf2f4f7).into();
    colors.button_primary = colors.primary;
    colors.button_primary_foreground = colors.primary_foreground;
    colors.button_primary_hover = colors.primary_hover;
    colors.button_primary_active = colors.primary_active;
    colors.button_active = rgb(TINT).into();
    colors.secondary_active = rgb(TINT).into();
    colors.ring = rgb(BLUE).into();
    colors.selection = rgb(0xcbdafa).into();
    theme.tokens = theme.colors.into();
    Theme::sync_base(cx);
}

/// Infrequent reveals only: source confirmation and expanded task options.
pub fn reveal(view: gpui::Div, id: impl Into<gpui::ElementId>, cx: &App) -> gpui::AnyElement {
    use gpui::{Animation, AnimationExt, IntoElement, Styled};
    if cx.reduce_motion() {
        return view.into_any_element();
    }
    view.with_animation(
        id,
        Animation::new(std::time::Duration::from_millis(180))
            .with_easing(|t| 1. - (1. - t).powi(3)),
        |view, t| view.relative().top(px(6. * (1. - t))).opacity(t),
    )
    .into_any_element()
}
