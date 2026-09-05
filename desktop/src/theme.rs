//! Shared colors keep controls, navigation and document surfaces consistent.
use gpui::{App, px, rgb};
use gpui_component::{Theme, ThemeMode};

pub const CANVAS: u32 = 0xf8f8f6;
pub const SURFACE: u32 = 0xffffff;
pub const SIDEBAR: u32 = 0xf0f1ee;
pub const INK: u32 = 0x252a32;
pub const MUTED: u32 = 0x68707b;
pub const LINE: u32 = 0xe3e5e2;
pub const BLUE: u32 = 0x315bc5;
pub const TINT: u32 = 0xeaf0ff;
pub const SUCCESS: u32 = 0x237552;

pub fn init(cx: &mut App) {
    Theme::change(ThemeMode::Light, None, cx);
    let theme = Theme::global_mut(cx);
    theme.font_size = px(16.);
    theme.radius = px(8.);
    theme.radius_lg = px(12.);
    let colors = &mut theme.colors;
    colors.background = rgb(CANVAS).into();
    colors.foreground = rgb(INK).into();
    colors.border = rgb(LINE).into();
    colors.input = rgb(LINE).into();
    colors.muted = rgb(SIDEBAR).into();
    colors.muted_foreground = rgb(MUTED).into();
    colors.accent = rgb(TINT).into();
    colors.accent_foreground = rgb(BLUE).into();
    colors.primary = rgb(BLUE).into();
    colors.primary_foreground = rgb(SURFACE).into();
    colors.primary_hover = rgb(0x284eae).into();
    colors.primary_active = rgb(0x214396).into();
    colors.button = rgb(SURFACE).into();
    colors.button_foreground = rgb(INK).into();
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
