//! The fields of Kit's theme the kit's bridge does not project. See `sync_unprojected`.
use gpui::{App, Rgba, px};
use gpui_kit::component::Theme;
use gpui_kit_material::{
    color::ColorRole,
    tokens::shape::{RADIUS_LARGE, RADIUS_SMALL},
};

macro_rules! tokens {
    ($($name:ident => $role:ident),+ $(,)?) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum ColorToken { $($name),+ }
        $(pub const $name: ColorToken = ColorToken::$name;)+
        impl ColorToken {
            fn role(self) -> ColorRole {
                match self { $(Self::$name => ColorRole::$role),+ }
            }
        }
    }
}
// Each old token is the kit's role of the same color (`Scheme::from_palette`), so
// pages on either side of the migration paint alike. See docs/split/MIGRATION.md.
tokens!(
    CANVAS => SURFACE_CONTAINER,
    SURFACE => SURFACE,
    SURFACE_HIGH => SURFACE_CONTAINER_HIGHEST,
    INSET => INSET,
    INK => ON_SURFACE,
    GRAY => ON_SURFACE_VARIANT,
    FAINT => FAINT,
    HAIRLINE => OUTLINE_VARIANT,
    CONTROL => OUTLINE,
    ACCENT => PRIMARY,
    ON_PRIMARY => ON_PRIMARY,
    SECONDARY_CONTAINER => SECONDARY_CONTAINER,
    SUCCESS => SUCCESS,
    SUCCESS_BG => SUCCESS_BG,
    WARNING => WARNING,
    WARNING_BG => WARNING_BG,
    DANGER => ERROR,
    DANGER_BG => ERROR_CONTAINER,
);

pub use gpui_kit_material::theme::blend;

pub fn color(token: ColorToken) -> Rgba {
    gpui_kit_material::theme::color(token.role())
}

/// Kit's theme fields the kit's bridge does not project, painted as they were: scrollbars,
/// lists, the radii of the context menus, and text in monospace. The bridge owns the rest
/// (`gpui_kit_material::theme::advance_component_theme`). Call after the bridge has moved,
/// since a change of mode reloads these fields. Never from `render`.
pub fn sync_unprojected(scale: f32, cx: &mut App) {
    let dark = gpui_kit_material::theme::is_dark();
    let theme = Theme::global_mut(cx);
    theme.radius = RADIUS_SMALL;
    theme.radius_lg = RADIUS_LARGE;
    theme.mono_font_size = px(13.0 * scale);
    let colors = &mut theme.colors;
    macro_rules! set {
        ($value:expr => $($field:ident),+ $(,)?) => {{ let value = $value; $(colors.$field = value.into();)+ }};
    }
    set!(color(CANVAS) => title_bar, tab_bar, status_bar);
    set!(color(SURFACE) => button, list, table, accordion, group_box, tab_active, switch_thumb, slider_thumb);
    set!(color(INSET) => sidebar, table_head, table_foot, description_list_label, list_head, list_even, table_even, tab, button_secondary);
    set!(color(INK) => button_foreground, button_secondary_foreground, sidebar_foreground, group_box_foreground, table_head_foreground, table_foot_foreground, tab_active_foreground);
    set!(color(GRAY) => description_list_label_foreground, tab_foreground);
    set!(color(FAINT) => scrollbar_thumb_hover);
    set!(color(HAIRLINE) => title_bar_border, sidebar_border, status_bar_border, table_row_border, window_border, skeleton, tab_bar_segmented, button_active, secondary_active, button_secondary_active);
    set!(color(CONTROL) => scrollbar_thumb);
    set!(color(SURFACE_HIGH) => switch);
    set!(color(ACCENT) => button_primary, link, progress_bar, slider_bar, drag_border, sidebar_primary, list_active_border, table_active_border, sidebar_accent_foreground, info, info_hover, info_active, button_info_foreground);
    set!(color(ON_PRIMARY) => button_primary_foreground, sidebar_primary_foreground, success_foreground, warning_foreground, info_foreground);
    set!(blend(color(ACCENT), color(INK), 0.10) => primary_hover, button_primary_hover, link_hover);
    set!(blend(color(ACCENT), color(INK), 0.20) => primary_active, button_primary_active, link_active);
    set!(color(SECONDARY_CONTAINER) => drop_target, list_active, sidebar_accent, table_active, button_info, button_info_hover, button_info_active);
    set!(blend(color(SURFACE), color(INK), 0.05) => button_hover, secondary_hover, button_secondary_hover, list_hover, table_hover);
    set!(color(DANGER) => button_danger_foreground, danger_hover, danger_active);
    set!(color(DANGER_BG) => button_danger, button_danger_hover, button_danger_active);
    set!(color(SUCCESS) => success, button_success_foreground, success_hover, success_active);
    set!(color(SUCCESS_BG) => button_success, button_success_hover, button_success_active);
    set!(color(WARNING) => warning, button_warning_foreground, warning_hover, warning_active);
    set!(color(WARNING_BG) => button_warning, button_warning_hover, button_warning_active);
    colors.overlay = gpui::hsla(0., 0., 0., if dark { 0.58 } else { 0.28 });
    colors.scrollbar = gpui::hsla(0., 0., 0., 0.);
    theme.tokens = theme.colors.into();
    Theme::sync_base(cx);
}
