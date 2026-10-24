//! Semantic design tokens and animated appearance for all desktop pages.
use crate::palettes::{PaletteId, ThemePreferences};
use gpui::{App, Pixels, Rems, Rgba, Window, px, rems, rgb};
use gpui_component::{Theme, ThemeMode};
use std::{cell::RefCell, time::Instant};
#[path = "choice_group.rs"]
mod choice_group;
pub use choice_group::SingleChoiceGroup;
#[path = "select_field.rs"]
mod select_field;
pub use select_field::{MenuBuilder, SelectField};

macro_rules! tokens {
    ($($name:ident),+ $(,)?) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(usize)]
        pub enum ColorToken { $($name),+ }
        $(pub const $name: ColorToken = ColorToken::$name;)+
        const ALL_TOKENS: &[ColorToken] = &[$(ColorToken::$name),+];
    }
}
tokens!(
    CANVAS,
    SURFACE,
    SURFACE_HIGH,
    INSET,
    INK,
    GRAY,
    FAINT,
    HAIRLINE,
    CARD_LINE,
    CONTROL,
    ACCENT,
    ON_PRIMARY,
    SECONDARY_CONTAINER,
    ON_SECONDARY_CONTAINER,
    SUCCESS,
    SUCCESS_BG,
    WARNING,
    WARNING_BG,
    DANGER,
    DANGER_BG,
    FIND_HIGHLIGHT,
    FIND_CURRENT,
    FIND_CURRENT_LINE,
    SELECTION,
);
const COLOR_COUNT: usize = ALL_TOKENS.len();

pub fn blend(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0., 1.);
    Rgba {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

fn palette_colors(id: PaletteId) -> [Rgba; COLOR_COUNT] {
    let p = id.colors();
    let tint = |foreground, amount| blend(rgb(p.surface), rgb(foreground), amount);
    // The window ground is M3's surface container: the palette's canvas with
    // a trace of its accent, so surface tiles read as a layer above it.
    let canvas = blend(
        rgb(p.canvas),
        rgb(p.accent),
        if id.is_dark() { 0.03 } else { 0.045 },
    );
    std::array::from_fn(|index| match ALL_TOKENS[index] {
        CANVAS => canvas,
        // Dark palettes' surfaces sit only a step above their canvas; lift
        // them as M3 lifts dark surface containers, so tiles and cards read
        // as layers.
        SURFACE => {
            if id.is_dark() {
                blend(rgb(p.surface), rgb(p.text), 0.045)
            } else {
                rgb(p.surface)
            }
        }
        // Surface container highest: unselected segments, filled search bars
        // and tonal icon buttons.
        // Dark palettes raise surfaces by lightening them, so their highest
        // container builds on the surface rather than the darker ground.
        SURFACE_HIGH => {
            if id.is_dark() {
                blend(rgb(p.surface), rgb(p.text), 0.13)
            } else {
                blend(canvas, rgb(p.text), 0.06)
            }
        }
        INSET => rgb(p.inset),
        INK => rgb(p.text),
        GRAY => rgb(p.muted),
        FAINT => rgb(p.subtle),
        HAIRLINE => rgb(p.border),
        // M3 cards are separated by tone rather than a full-strength outline.
        CARD_LINE => blend(rgb(p.surface), rgb(p.border), 0.55),
        CONTROL => rgb(p.control),
        ACCENT => rgb(p.accent),
        ON_PRIMARY => rgb(p.on_accent),
        // M3 container roles derived from each palette's own accent and text, so
        // every preset keeps its identity (assets/themes/SOURCES.md).
        SECONDARY_CONTAINER => tint(p.accent, if id.is_dark() { 0.26 } else { 0.24 }),
        ON_SECONDARY_CONTAINER => blend(
            rgb(p.text),
            rgb(p.accent),
            if id.is_dark() { 0.30 } else { 0.40 },
        ),
        SUCCESS => rgb(p.success),
        SUCCESS_BG => tint(p.success, 0.12),
        WARNING => rgb(p.warning),
        WARNING_BG => tint(p.warning, 0.12),
        DANGER => rgb(p.danger),
        DANGER_BG => tint(p.danger, 0.12),
        SELECTION => rgb(p.selection),
        FIND_HIGHLIGHT => tint(p.warning, 0.16),
        FIND_CURRENT => tint(p.warning, 0.32),
        FIND_CURRENT_LINE => rgb(p.warning),
    })
}

struct PaintState {
    target: Option<PaletteId>,
    from: [Rgba; COLOR_COUNT],
    current: [Rgba; COLOR_COUNT],
    started: Instant,
}
thread_local! {
    // GPUI renders application chrome and all overlays on its UI thread.
    // Thread-local paint values keep helpers context-free without unsafe globals.
    static PAINT: RefCell<PaintState> = RefCell::new(PaintState {
        target: None, from: palette_colors(PaletteId::Paper),
        current: palette_colors(PaletteId::Paper), started: Instant::now(),
    });
}

pub fn color(token: ColorToken) -> Rgba {
    PAINT.with(|paint| paint.borrow().current[token as usize])
}

pub fn system_dark(cx: &App) -> bool {
    ThemeMode::from(cx.window_appearance()).is_dark()
}

/// Resolve system changes on each notified frame, and retarget palette changes
/// from their current paint values. Persisted settings are the sole authority.
pub fn apply_preference(preference: &ThemePreferences, window: &mut Window, cx: &mut App) {
    let id = preference.resolve(system_dark(cx));
    let target = palette_colors(id);
    let (changed, running) = PAINT.with(|paint| {
        let mut paint = paint.borrow_mut();
        let now = Instant::now();
        if paint.target.is_none() {
            paint.target = Some(id);
            paint.current = target;
            paint.from = target;
            return (true, false);
        }
        if paint.target != Some(id) {
            paint.from = paint.current;
            paint.target = Some(id);
            paint.started = now;
        }
        let t = if cx.reduce_motion() {
            1.
        } else {
            (now.duration_since(paint.started).as_secs_f32()
                / crate::motion::SLOW_EFFECTS.duration().as_secs_f32())
            .min(1.)
        };
        let eased = crate::motion::SLOW_EFFECTS.ease()(t);
        let next = std::array::from_fn(|i| blend(paint.from[i], target[i], eased));
        let changed = next != paint.current;
        paint.current = next;
        (changed, t < 1.)
    });
    if changed {
        sync_component_theme(id.is_dark(), cx);
    }
    if running {
        window.request_animation_frame();
    }
}

fn sync_component_theme(dark: bool, cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.mode = if dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    theme.radius = RADIUS_SMALL;
    theme.radius_lg = RADIUS_CARD;
    let colors = &mut theme.colors;
    macro_rules! set {
        ($value:expr => $($field:ident),+ $(,)?) => {{ let value = $value; $(colors.$field = value.into();)+ }};
    }
    set!(color(CANVAS) => background, title_bar, tab_bar, status_bar, tiles);
    set!(color(SURFACE) => button, popover, list, table, accordion, group_box, tab_active, switch_thumb, slider_thumb);
    set!(color(INSET) => sidebar, table_head, table_foot, description_list_label, list_head, list_even, table_even, tab, secondary, button_secondary);
    set!(color(INK) => foreground, button_foreground, popover_foreground, secondary_foreground, button_secondary_foreground, sidebar_foreground, group_box_foreground, table_head_foreground, table_foot_foreground, tab_active_foreground);
    set!(color(GRAY) => muted_foreground, description_list_label_foreground, tab_foreground);
    set!(color(FAINT) => scrollbar_thumb_hover);
    set!(color(HAIRLINE) => border, title_bar_border, sidebar_border, status_bar_border, table_row_border, window_border, muted, skeleton, tab_bar_segmented, button_active, secondary_active, button_secondary_active);
    set!(color(CONTROL) => input, scrollbar_thumb);
    set!(color(SURFACE_HIGH) => switch);
    set!(color(ACCENT) => accent_foreground, primary, button_primary, caret, ring, link, progress_bar, slider_bar, drag_border, sidebar_primary, list_active_border, table_active_border, sidebar_accent_foreground, info, info_hover, info_active, button_info_foreground);
    set!(color(ON_PRIMARY) => primary_foreground, button_primary_foreground, sidebar_primary_foreground, danger_foreground, success_foreground, warning_foreground, info_foreground);
    set!(blend(color(ACCENT), color(INK), 0.10) => primary_hover, button_primary_hover, link_hover);
    set!(blend(color(ACCENT), color(INK), 0.20) => primary_active, button_primary_active, link_active);
    set!(color(SECONDARY_CONTAINER) => accent, drop_target, list_active, sidebar_accent, table_active, button_info, button_info_hover, button_info_active);
    set!(blend(color(SURFACE), color(INK), 0.05) => button_hover, secondary_hover, button_secondary_hover, list_hover, table_hover);
    set!(color(SELECTION) => selection);
    set!(color(DANGER) => danger, button_danger_foreground, danger_hover, danger_active);
    set!(color(DANGER_BG) => button_danger, button_danger_hover, button_danger_active);
    set!(color(SUCCESS) => success, button_success_foreground, success_hover, success_active);
    set!(color(SUCCESS_BG) => button_success, button_success_hover, button_success_active);
    set!(color(WARNING) => warning, button_warning_foreground, warning_hover, warning_active);
    set!(color(WARNING_BG) => button_warning, button_warning_hover, button_warning_active);
    colors.overlay = gpui::hsla(0., 0., 0., if dark { 0.58 } else { 0.28 });
    colors.scrollbar = gpui::hsla(0., 0., 0., 0.);
    theme.tokens = theme.colors.into();
    Theme::sync_base(cx);
    cx.set_global(menu_style(dark));
}

/// Every dropdown, context menu and submenu: one elevated container, 40px rows
/// with a single 8% state layer, tonal checked rows and a short entrance.
fn menu_style(dark: bool) -> gpui_component::menu::MenuStyle {
    let ink = color(INK);
    let layer = |alpha: f32| Rgba { a: alpha, ..ink };
    gpui_component::menu::MenuStyle {
        // M3 menus sit on surface-container; dark elevation reads as a lighter tone.
        surface: if dark {
            blend(color(SURFACE), ink, 0.06)
        } else {
            color(SURFACE)
        }
        .into(),
        radius: MENU_RADIUS,
        padding: px(6.),
        // M3 menu items are 48px tall.
        item_height: rems(48. / 14.).into(),
        item_radius: px(10.),
        item_padding: rems(12. / 14.).into(),
        gap: rems(12. / 14.).into(),
        text_size: TEXT_BODY.into(),
        icon_size: rems(18. / 14.).into(),
        icon_color: color(GRAY).into(),
        highlight: layer(0.08).into(),
        checked: color(SECONDARY_CONTAINER).into(),
        checked_highlight: blend(color(SECONDARY_CONTAINER), ink, 0.08).into(),
        checked_foreground: color(ON_SECONDARY_CONTAINER).into(),
        danger: color(DANGER).into(),
        disabled: layer(0.38).into(),
        separator: color(HAIRLINE).into(),
        min_width: px(220.),
        enter: std::time::Duration::from_millis(crate::motion::ENTER_MS),
    }
}

/* ---------- Geometry (px; does not scale with text) ---------- */
pub const RADIUS_PILL: Pixels = px(999.);
pub const RADIUS_CARD: Pixels = px(16.);
/// Settings reference surfaces share a 24px corner radius.
pub const SETTINGS_CARD_RADIUS: Pixels = px(24.);
/// Connected tiles: a group keeps the 24px corners on its outside and 6px
/// corners where tiles meet, with 2px of ground between them.
pub const TILE_RADIUS: Pixels = SETTINGS_CARD_RADIUS;
pub const TILE_INNER_RADIUS: Pixels = px(6.);
pub const TILE_GAP: Pixels = px(2.);
/// Larger M3 button heights: the hero's main action and prominent actions.
pub const BUTTON_LARGE: Rems = rems(56. / 14.);
pub const BUTTON_MEDIUM: Rems = rems(48. / 14.);
pub const RADIUS_HERO: Pixels = px(28.);
pub const RADIUS_SMALL: Pixels = px(8.);
/// Menus and the select fields that open them.
pub const MENU_RADIUS: Pixels = px(16.);
pub const FIELD_RADIUS: Pixels = px(12.);
/// A select at the trailing end of a settings row keeps a readable width
/// instead of spanning the row.
pub const INLINE_SELECT_WIDTH: Rems = rems(280. / 14.);
/// Ordinary controls share this height, including padding, at every text scale.
pub const CONTROL_HEIGHT: Rems = rems(40. / 14.);
/// Horizontal inset of an item inside a settings card.
pub const SETTING_ITEM_INSET: Rems = rems(20. / 14.);
/// Leading icon slot of an icon + label cluster, and the gap before its text.
pub const ICON_SLOT: Rems = rems(20. / 14.);
pub const ICON_GAP: Rems = rems(8. / 14.);
/// Supporting lines of an icon-labelled item start on its label text.
pub const LABEL_INSET: Rems = rems(ICON_SLOT.0 + ICON_GAP.0);

/// The app bar holds one control row with 8px above and below.
pub fn title_bar_height(window: &Window) -> Pixels {
    CONTROL_HEIGHT.to_pixels(window.rem_size()) + px(16.)
}

/// Task dialogs sit below the shared title bar with the page gutter.
pub fn task_dialog_top(window: &Window) -> Pixels {
    title_bar_height(window) + px(24.)
}

/// Window widths, in 100%-scale pixels, where page arrangements change.
/// Below `COMPACT_WINDOW` pages use narrow gutters, an icon-only navigation
/// rail and stacked controls; from `WIDE_WINDOW` they use their widest form.
pub const COMPACT_WINDOW: f32 = 960.;
pub const WIDE_WINDOW: f32 = 1200.;

/// The window width in 100%-scale pixels, so breakpoints follow the text scale.
pub fn design_width(window: &Window) -> f32 {
    f32::from(window.bounds().size.width) * 14. / f32::from(window.rem_size())
}

pub fn is_compact(window: &Window) -> bool {
    design_width(window) < COMPACT_WINDOW
}

/// Settings navigation width (100%-scale pixels) at wide, medium and compact
/// windows; the compact rail widens a little once text is enlarged.
pub const SETTINGS_NAV_WIDE: f32 = 260.;
pub const SETTINGS_NAV_MEDIUM: f32 = 220.;
pub const SETTINGS_NAV_RAIL: f32 = 64.;
pub const SETTINGS_NAV_RAIL_ENLARGED: f32 = 72.;
/// Space wide settings layouts keep after their content (100%-scale pixels).
pub const SETTINGS_WIDE_TRAILING: f32 = 56.;

/// Space above the workbench hero (100%-scale pixels). A quiet workbench, with
/// nothing in progress, lets the hero sit lower.
pub fn hero_top(quiet: bool, compact: bool) -> f32 {
    match (quiet, compact) {
        (true, true) => 96.,
        (true, false) => 160.,
        (false, true) => 40.,
        (false, false) => 56.,
    }
}

/// The image viewer keeps this margin to each window edge up to its maximum
/// width, and its content sits inside `IMAGE_DIALOG_PADDING` on each side.
pub const IMAGE_DIALOG_MARGIN: Pixels = px(24.);
pub const IMAGE_DIALOG_MAX_WIDTH: Pixels = px(1180.);
pub const IMAGE_DIALOG_MIN_WIDTH: Pixels = px(280.);
pub const IMAGE_DIALOG_PADDING: Pixels = px(17.);

/// Controls drawn over a screenshot sit on a fixed dark scrim with white text:
/// the image beneath is not themed, so palette roles cannot promise contrast.
pub fn image_scrim() -> Rgba {
    gpui::rgba(0x101416c7)
}

pub fn on_image_scrim() -> Rgba {
    rgb(0xffffff)
}

/* ---------- Column widths (rems; structure that scales with text) ---------- */
pub const COLUMN: Rems = rems(65.714);
pub const TOC_PANEL: Rems = rems(16.);

/* ---------- Type scale (rems; 14px is 1rem) ---------- */
pub const TEXT_AUX: Rems = rems(0.857);
pub const TEXT_BODY: Rems = rems(1.);
pub const TEXT_TITLE: Rems = rems(1.286);
pub const TEXT_READER: Rems = rems(1.143);
/// Main page headline.
pub const TEXT_DISPLAY: Rems = rems(2.);
/// Page titles (M3 headline small at 30px).
pub const TEXT_HEADLINE: Rems = rems(30. / 14.);
/// Settings content reads one step larger than navigation and the workbench.
pub const TEXT_SETTINGS_BODY: Rems = rems(16. / 14.);
pub const TEXT_SETTINGS_AUX: Rems = TEXT_BODY;

/// Gives a shared control the settings reading size. Controls size their own
/// labels, so the role cannot be inherited from the enclosing settings card.
pub trait SettingsText {
    fn settings_text(self) -> Self;
}
impl SettingsText for gpui_component::button::Button {
    fn settings_text(self) -> Self {
        self.label_text_size(TEXT_SETTINGS_BODY)
    }
}
impl SettingsText for gpui_component::input::Input {
    fn settings_text(self) -> Self {
        gpui::Styled::text_size(self, TEXT_SETTINGS_BODY)
    }
}
impl SettingsText for SingleChoiceGroup {
    fn settings_text(self) -> Self {
        self.label_text_size(TEXT_SETTINGS_BODY)
    }
}

/// Page title shared by library, tasks, settings and a non-hero workbench.
/// As on the M3 canvas, the title is text alone in the regular-weight
/// headline role; the app bar already marks the destination with its icon.
pub fn page_heading(
    id: impl Into<gpui::ElementId>,
    title: impl Into<gpui::SharedString>,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div().flex().items_center().min_w_0().child(
        accessible_text(id, title.into())
            .role(Role::Heading)
            .text_size(TEXT_HEADLINE)
            .line_height(rems(36. / 14.))
            .font_weight(FontWeight::NORMAL),
    )
}

/// One tile of a connected group: the surface with 24px corners on the
/// group's outside and 6px where it meets a neighbour. `outer` lists which
/// corners are outside, as top-left, top-right, bottom-right, bottom-left.
pub fn tile(outer: [bool; 4]) -> gpui::Div {
    use gpui::{Styled, div};
    let radius = |outside: bool| {
        if outside {
            TILE_RADIUS
        } else {
            TILE_INNER_RADIUS
        }
    };
    div()
        .w_full()
        .min_w_0()
        .bg(color(SURFACE))
        .rounded_tl(radius(outer[0]))
        .rounded_tr(radius(outer[1]))
        .rounded_br(radius(outer[2]))
        .rounded_bl(radius(outer[3]))
}

/// The tile at `index` of a vertical connected list of `count` tiles.
pub fn list_tile(index: usize, count: usize) -> gpui::Div {
    let first = index == 0;
    let last = index + 1 >= count;
    tile([first, first, last, last])
}

/// A distinct color family for a category icon: a tonal container and its
/// content color, blended from the current surface so every palette keeps
/// its own ground and contrast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Orange,
    Blue,
    Teal,
    Amber,
    Green,
    Pink,
    Purple,
}

impl Tint {
    fn hue(self) -> Rgba {
        rgb(match self {
            Tint::Orange => 0xd9692b,
            Tint::Blue => 0x2f6fe0,
            Tint::Teal => 0x1b8ea6,
            Tint::Amber => 0xc79a12,
            Tint::Green => 0x2f9a52,
            Tint::Pink => 0xc7428a,
            Tint::Purple => 0x7a5cc7,
        })
    }

    pub fn container(self) -> Rgba {
        let dark = is_dark_paint();
        blend(color(SURFACE), self.hue(), if dark { 0.30 } else { 0.24 })
    }

    pub fn content(self) -> Rgba {
        let dark = is_dark_paint();
        blend(self.hue(), color(INK), if dark { 0.35 } else { 0.45 })
    }
}

/// Whether the palette currently painted is a dark one: its text is lighter
/// than its surface.
pub fn is_dark_paint() -> bool {
    let luminance = |c: Rgba| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    luminance(color(INK)) > luminance(color(SURFACE))
}

/// A category or resource icon in a round tonal container.
pub fn tinted_icon(icon: gpui_component::Icon, tint: Tint, size: Pixels) -> gpui::Div {
    use gpui::{ParentElement, Styled, div};
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(size)
        .rounded_full()
        .bg(tint.container())
        .text_color(tint.content())
        .child(icon.size(size * 0.5))
}

/* ---------- Neutral shadows ---------- */
fn shadow_color(alpha: f32) -> gpui::Hsla {
    gpui::hsla(0., 0., 0., alpha)
}
/// Hero input card: near 0/1/2 @5% + far 0/16/40 @8%.
pub fn shadow_hero() -> Vec<gpui::BoxShadow> {
    vec![
        gpui::BoxShadow::new(px(0.), px(1.), shadow_color(0.05)).blur_radius(px(2.)),
        gpui::BoxShadow::new(px(0.), px(16.), shadow_color(0.08)).blur_radius(px(40.)),
    ]
}
/// M3 level-1 card elevation: 0/1/2 @6%, kept faint so tone carries the layer.
pub fn shadow_card() -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow::new(px(0.), px(1.), shadow_color(0.06)).blur_radius(px(2.))]
}
/// Overlay panels and dialogs: 0/12/32 @16%.
pub fn shadow_popover() -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow::new(px(0.), px(12.), shadow_color(0.16)).blur_radius(px(32.))]
}

/// Plain Div text is not exposed by GPUI's native accessibility bridge. Keep labels on
/// leaf elements so an ancestor does not replace the accessibility of its controls.
pub fn accessible_text(
    id: impl Into<gpui::ElementId>,
    value: impl Into<gpui::SharedString>,
) -> gpui::Stateful<gpui::Div> {
    use gpui::*;
    let value = value.into();
    div()
        .id(id)
        .role(Role::Label)
        .aria_label(value.clone())
        .child(value)
}

/// Failure feedback uses one accessible alert form everywhere. Callers
/// chain their own size and wrapping rules.
pub fn error_text(
    id: impl Into<gpui::ElementId>,
    message: impl Into<gpui::SharedString>,
) -> gpui::Stateful<gpui::Div> {
    use gpui::*;
    accessible_text(id, message)
        .role(Role::Alert)
        .text_color(color(DANGER))
}

/// Primary interface labels have one icon column and one text column. The
/// slot also accepts a live spinner, keeping state changes on the same axis.
pub fn semantic_label(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    icon: impl gpui::IntoElement,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .flex()
        .items_center()
        .min_w_0()
        .gap(ICON_GAP)
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(ICON_SLOT)
                .flex_shrink_0()
                .child(icon),
        )
        .child(
            accessible_text(id, label)
                .min_w_0()
                .whitespace_normal()
                .text_size(TEXT_BODY)
                .font_weight(FontWeight::MEDIUM),
        )
}

/// Short supporting text inside an existing setting or resource boundary.
/// The enclosing item supplies the surface; a second frame would compete with it.
pub fn supporting_info(
    id: impl Into<gpui::ElementId>,
    text: impl Into<gpui::SharedString>,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap(ICON_GAP)
        // The info icon sits in the same slot as a label's icon, so this text
        // starts on the label text ruler of the item it explains.
        .child(
            div()
                .flex()
                .justify_center()
                .w(ICON_SLOT)
                .h(rems(18. / 14.))
                .items_center()
                .flex_shrink_0()
                .child(
                    crate::icons::info()
                        .size(rems(16. / 14.))
                        .text_color(color(GRAY)),
                ),
        )
        .child(
            accessible_text(id, text)
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_size(TEXT_AUX)
                .line_height(rems(18. / 14.))
                .text_color(color(GRAY)),
        )
}

/// A supporting fact of an icon-labelled item (an address, a path, a status
/// sentence). It starts on the item's label text ruler, not under its icon.
pub fn item_detail(
    id: impl Into<gpui::ElementId>,
    text: impl Into<gpui::SharedString>,
    tone: ColorToken,
) -> gpui::Stateful<gpui::Div> {
    use gpui::prelude::*;
    accessible_text(id, text)
        .w_full()
        .min_w_0()
        .pl(LABEL_INSET)
        .whitespace_normal()
        .text_size(TEXT_AUX)
        .line_height(rems(18. / 14.))
        .text_color(color(tone))
}

/// Standalone guidance has its own boundary. Inside a setting, use supporting_info.
pub fn info_callout(
    id: impl Into<gpui::ElementId>,
    text: impl Into<gpui::SharedString>,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap(rems(10. / 14.))
        .px(rems(16. / 14.))
        .py(rems(12. / 14.))
        .rounded(RADIUS_CARD)
        .bg(color(SURFACE_HIGH))
        .child(
            crate::icons::info()
                .size(rems(18. / 14.))
                .mt(rems(1. / 14.))
                .flex_shrink_0()
                .text_color(color(GRAY)),
        )
        .child(
            accessible_text(id, text)
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_size(TEXT_AUX)
                .line_height(rems(18. / 14.))
                .text_color(color(GRAY)),
        )
}

/// The type scale of a labelled row: settings content reads one step larger
/// than the workbench's processing panel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowScale {
    Settings,
    Panel,
}

impl RowScale {
    pub fn label(self) -> Rems {
        TEXT_SETTINGS_BODY
    }

    pub fn hint(self) -> Rems {
        match self {
            RowScale::Settings => TEXT_SETTINGS_AUX,
            RowScale::Panel => TEXT_AUX,
        }
    }

    /// A switch row's height; it scales with the text so rows line up at
    /// every interface size.
    fn switch_height(self) -> Rems {
        match self {
            RowScale::Settings => rems(56. / 14.),
            RowScale::Panel => rems(44. / 14.),
        }
    }
}

/// Supporting text under a row's label.
pub fn row_hint(
    id: impl Into<gpui::ElementId>,
    hint: impl Into<gpui::SharedString>,
    scale: RowScale,
) -> gpui::Stateful<gpui::Div> {
    use gpui::Styled;
    accessible_text(id, hint)
        .text_size(scale.hint())
        .text_color(color(GRAY))
}

/// Label and optional hint on the left, a switch on the trailing edge.
pub fn switch_row(
    label: &'static str,
    hint: &'static str,
    switch: impl gpui::IntoElement,
    scale: RowScale,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .flex()
        .w_full()
        .min_w_0()
        .min_h(scale.switch_height())
        .items_center()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    accessible_text(SharedString::from(format!("switch-label-{label}")), label)
                        .text_size(scale.label()),
                )
                .when(!hint.is_empty(), |view| {
                    view.child(row_hint(
                        SharedString::from(format!("switch-hint-{label}")),
                        hint,
                        scale,
                    ))
                }),
        )
        .child(div().flex_shrink_0().child(switch))
}

/// A 16:9 video cover; the width scales with the text size. Without a cover
/// the tonal slot stays, so rows keep their rhythm while covers load.
pub fn cover_slot(width: Rems, cover: Option<std::path::PathBuf>) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .w(width)
        .aspect_ratio(16. / 9.)
        .flex_shrink_0()
        .rounded(RADIUS_SMALL)
        .overflow_hidden()
        .bg(color(SECONDARY_CONTAINER))
        .flex()
        .items_center()
        .justify_center()
        .when_some(cover, |slot, cover| {
            slot.child(
                img(cover)
                    .size_full()
                    .rounded(RADIUS_SMALL)
                    .object_fit(ObjectFit::Cover),
            )
        })
}

/// Related facts share a bounded label column and a stable value start.
pub fn detail_row(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    icon: impl gpui::IntoElement,
    value: impl gpui::IntoElement,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .flex()
        .w_full()
        .min_w_0()
        .min_h(rems(32. / 14.))
        .items_center()
        .flex_wrap()
        // A value that wraps under its label stays closer to it than to
        // the next row's label.
        .gap_x(rems(12. / 14.))
        .gap_y(rems(2. / 14.))
        .child(
            semantic_label(id, label, icon)
                .w(rems(160. / 14.))
                .max_w_full()
                .flex_shrink_0(),
        )
        .child(
            div()
                .flex_1()
                .flex_basis(rems(200. / 14.))
                .min_w_0()
                .max_w_full()
                .font_weight(FontWeight::NORMAL)
                .child(value),
        )
}

pub fn init(cx: &mut App) {
    choice_group::init(cx);
    Theme::change(ThemeMode::Light, None, cx);
    Theme::global_mut(cx).font_size = px(14.);
    sync_component_theme(false, cx);
}

/// One baseline for ordinary actions; content buttons explicitly opt into auto height.
pub fn control(id: impl Into<gpui::ElementId>) -> gpui_component::button::Button {
    use gpui::Styled;
    gpui_component::button::Button::new(id)
        .h(CONTROL_HEIGHT)
        .min_h(CONTROL_HEIGHT)
        .min_w(CONTROL_HEIGHT)
        .rounded(RADIUS_PILL)
        .py_0()
        .flex_shrink_0()
        .font_weight(gpui::FontWeight::MEDIUM)
}

/// A framed single-line field. Multiline editors and embedded find text use
/// their own content layout, without inventing another ordinary field size.
/// The outlined field shared with selects: transparent, a control-strength
/// outline and 12px corners; focus draws the accent ring.
pub fn text_input(
    state: &gpui::Entity<gpui_component::input::InputState>,
) -> gpui_component::input::Input {
    use gpui::Styled;
    gpui_component::input::Input::new(state)
        .h(rems(48. / 14.))
        .min_h(rems(48. / 14.))
        .rounded(FIELD_RADIUS)
        .bg(gpui::transparent_black())
        .border_color(color(CONTROL))
        .px(rems(16. / 14.))
        .text_size(TEXT_BODY)
}

/// A form field whose label floats on its outline, as on the canvas; the
/// label turns accent while the field has focus.
pub fn labeled_input(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    state: &gpui::Entity<gpui_component::input::InputState>,
    input: gpui_component::input::Input,
    window: &gpui::Window,
    cx: &App,
) -> gpui::Div {
    use gpui::{Focusable, Styled};
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    outlined_field_label(
        id,
        label,
        TEXT_AUX,
        if focused { color(ACCENT) } else { color(GRAY) },
        input.h(rems(56. / 14.)).min_h(rems(56. / 14.)),
    )
}

/* ---------- Shared controls ---------- */

/// A section title led by its category icon, as on diagnostic groups.
pub fn section_heading(
    id: impl Into<gpui::ElementId>,
    icon: gpui_component::Icon,
    title: impl Into<gpui::SharedString>,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .flex()
        .min_w_0()
        .items_center()
        .gap(ICON_GAP)
        .text_color(color(GRAY))
        .child(icon.size(rems(18. / 14.)).flex_shrink_0())
        .child(section_title(id, title))
}

/// M3 list section title: a quiet medium-weight label above the list it
/// names, on the list's text inset. Shared by settings groups, task groups,
/// diagnostic groups, the workbench's processing options and its recent notes.
pub fn section_title(
    id: impl Into<gpui::ElementId>,
    title: impl Into<gpui::SharedString>,
) -> gpui::Stateful<gpui::Div> {
    use gpui::prelude::*;
    accessible_text(id, title)
        .role(gpui::Role::Heading)
        .text_size(TEXT_BODY)
        .line_height(rems(20. / 14.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color(GRAY))
}

/// M3 search bar: a filled, borderless pill with a leading search icon. The
/// field's own frame is off, so the bar draws the focus outline itself.
pub fn search_field(
    state: &gpui::Entity<gpui_component::input::InputState>,
    label: impl Into<gpui::SharedString>,
    height: Rems,
    window: &Window,
    cx: &App,
) -> gpui::Div {
    use gpui::{Focusable, prelude::*};
    let focused = state.read(cx).focus_handle(cx).is_focused(window);
    gpui::div()
        .flex()
        .items_center()
        .w_full()
        .min_w_0()
        .h(height)
        .px(rems(16. / 14.))
        .gap(rems(10. / 14.))
        .rounded(RADIUS_PILL)
        .bg(color(SURFACE_HIGH))
        .border_2()
        .border_color(if focused {
            color(ACCENT)
        } else {
            gpui::Rgba {
                a: 0.,
                ..color(SURFACE_HIGH)
            }
        })
        .child(
            crate::icons::search()
                .size(rems(18. / 14.))
                .flex_shrink_0()
                .text_color(color(GRAY)),
        )
        .child(
            gpui::div().flex_1().min_w_0().child(
                gpui_component::input::Input::new(state)
                    .appearance(false)
                    .aria_label(label.into())
                    .w_full()
                    .px_0()
                    .text_size(TEXT_BODY)
                    .cleanable(true),
            ),
        )
}

/// Standard card container: surface fill, card border and corner radius.
/// Callers chain their own gap and contents on top of the shared frame.
pub fn card_surface() -> gpui::Div {
    use gpui::Styled;
    gpui_component::v_flex()
        .w_full()
        .p_4()
        .bg(color(SURFACE))
        .border_1()
        .border_color(color(CARD_LINE))
        .rounded(RADIUS_CARD)
        .shadow(shadow_card())
}

/// A selectable preview surface. Its opaque contents cannot conceal pointer
/// feedback: hover changes the outline/shadow, and press dims the whole card.
pub fn selection_card(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    amount: f32,
) -> gpui_base::Button {
    use gpui::{prelude::*, *};
    gpui_base::Button::new(id)
        .selected(selected)
        .aria_toggled(if selected {
            gpui::accesskit::Toggled::True
        } else {
            gpui::accesskit::Toggled::False
        })
        .flex()
        .flex_col()
        .h_auto()
        .min_h(px(0.))
        .min_w_0()
        .rounded(RADIUS_CARD)
        // M3 selection is tonal: the chosen card fills with the secondary
        // container; the 2px outline is reserved for keyboard focus.
        .border_2()
        .border_color(blend(color(CARD_LINE), color(SECONDARY_CONTAINER), amount))
        .bg(blend(color(SURFACE), color(SECONDARY_CONTAINER), amount))
        .cursor_pointer()
        .hover(|style| style.shadow_sm())
        .active(|style| style.opacity(0.9).shadow_none())
        .focus(|style| style.border_color(color(ACCENT)))
}

/// A choice that needs a short explanation stays visible as one interactive
/// object. The title, description and indicator share the same content bounds.
pub fn described_choice(
    id: impl Into<gpui::ElementId>,
    title: impl Into<gpui::SharedString>,
    description: impl Into<gpui::SharedString>,
    icon: gpui_component::Icon,
    selected: bool,
    window: &mut Window,
    cx: &mut App,
) -> gpui_base::Button {
    use gpui::{prelude::*, *};
    let id = id.into();
    let amount = crate::motion::selection_value(
        SharedString::from(format!("choice-card-{id:?}")),
        if selected { 1. } else { 0. },
        window,
        cx,
    );
    let title = title.into();
    let description = description.into();
    selection_card(id, selected, amount)
        .accessibility_label(title.clone())
        .w_full()
        .h_full()
        .justify_start()
        .items_stretch()
        .p(rems(12. / 14.))
        .gap(rems(8. / 14.))
        .text_color(color(INK))
        .child(
            div()
                .flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap(rems(8. / 14.))
                .child(
                    icon.size(rems(20. / 14.))
                        .flex_shrink_0()
                        .text_color(color(if selected { ACCENT } else { GRAY })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .size(rems(18. / 14.))
                        .rounded_full()
                        .border_1()
                        .border_color(color(if selected { ACCENT } else { CONTROL }))
                        .bg(if selected {
                            color(ACCENT)
                        } else {
                            color(SURFACE)
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |v| {
                            v.child(
                                crate::icons::check()
                                    .size(rems(1.))
                                    .text_color(color(ON_PRIMARY)),
                            )
                        }),
                ),
        )
        .when(!description.is_empty(), |card| {
            card.child(
                div()
                    .w_full()
                    .min_w_0()
                    .pl(LABEL_INSET)
                    .whitespace_normal()
                    .text_size(TEXT_AUX)
                    .line_height(rems(18. / 14.))
                    .text_color(color(GRAY))
                    .child(description),
            )
        })
}

/// Compact forms use a leading label above the field. Padding belongs to the
/// enclosing scroll viewport so focus effects never meet its clipping edge.
pub fn stacked_field(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    icon: gpui_component::Icon,
    field: impl gpui::IntoElement,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .gap(rems(8. / 14.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(rems(8. / 14.))
                .child(
                    icon.size(rems(18. / 14.))
                        .flex_shrink_0()
                        .text_color(color(GRAY)),
                )
                .child(
                    accessible_text(id, label)
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::MEDIUM),
                ),
        )
        .child(field)
}

/// The single forward action of a page or region, in the active theme's accent.
pub fn primary_pill(id: impl Into<gpui::ElementId>) -> gpui_component::button::Button {
    use gpui::Styled;
    use gpui_component::button::ButtonVariants;
    control(id).primary().px(rems(16. / 14.)).gap(ICON_GAP)
}

/// Paints a button with an exact fill. gpui-component dilutes a custom fill
/// to 20%, so the token is also set as the background. With state layers,
/// hover and press add 8% and 10% of the content color, as in M3.
fn custom_fill(
    button: gpui_component::button::Button,
    bg: gpui::Rgba,
    fg: gpui::Rgba,
    state_layers: bool,
    cx: &App,
) -> gpui_component::button::Button {
    use gpui::Styled;
    use gpui_component::button::{ButtonCustomVariant, ButtonVariants};
    let layer = |amount| {
        if state_layers {
            blend(bg, gpui::Rgba { a: 1., ..fg }, amount)
        } else {
            bg
        }
    };
    button
        .custom(
            ButtonCustomVariant::new(cx)
                .color(bg.into())
                .foreground(fg.into())
                .hover(layer(0.08).into())
                .active(layer(0.10).into())
                .shadow(false),
        )
        .bg(bg)
}

/// Top-level destination in the app bar. The current destination sits on the
/// tonal secondary container; others are quiet.
pub fn nav_action(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    cx: &App,
) -> gpui_component::button::Button {
    use gpui::Styled;
    let (bg, fg) = if selected {
        (color(SECONDARY_CONTAINER), color(ON_SECONDARY_CONTAINER))
    } else {
        (
            gpui::Rgba {
                a: 0.,
                ..color(CANVAS)
            },
            color(INK),
        )
    };
    custom_fill(control(id), bg, fg, true, cx)
        .pl(rems(12. / 14.))
        .pr(rems(16. / 14.))
        .gap(ICON_GAP)
        .label_text_size(rems(15. / 14.))
}

/// M3 filled tonal button: a secondary action that still carries weight,
/// e.g. opening a finished note from a list row.
pub fn tonal_pill(id: impl Into<gpui::ElementId>, cx: &App) -> gpui_component::button::Button {
    use gpui::Styled;
    custom_fill(
        control(id),
        color(SECONDARY_CONTAINER),
        color(ON_SECONDARY_CONTAINER),
        true,
        cx,
    )
    .px(rems(16. / 14.))
    .gap(ICON_GAP)
}

/// M3 disabled filled button: on-surface at 12% with 38% content, so an
/// unavailable action never reads as a selected tonal surface.
pub fn disabled_when(
    button: gpui_component::button::Button,
    disabled: bool,
    cx: &App,
) -> gpui_component::button::Button {
    if !disabled {
        return button;
    }
    let ink = color(INK);
    custom_fill(
        button,
        gpui::Rgba { a: 0.12, ..ink },
        gpui::Rgba { a: 0.38, ..ink },
        false,
        cx,
    )
}

/// M3 filter chip: 32px, 8px corners. Selected chips sit on the secondary
/// container with a leading check; others are outlined.
pub fn filter_chip(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    cx: &App,
) -> gpui_component::button::Button {
    use gpui::Styled;
    let size = rems(32. / 14.);
    let (bg, fg) = if selected {
        (color(SECONDARY_CONTAINER), color(ON_SECONDARY_CONTAINER))
    } else {
        (
            gpui::Rgba {
                a: 0.,
                ..color(CANVAS)
            },
            color(INK),
        )
    };
    let chip = custom_fill(gpui_component::button::Button::new(id), bg, fg, true, cx)
        .h(size)
        .min_h(size)
        .px(rems(12. / 14.))
        .gap(rems(6. / 14.))
        .rounded(RADIUS_SMALL)
        .flex_shrink_0()
        .font_weight(gpui::FontWeight::MEDIUM)
        .border_1()
        .border_color(if selected {
            color(SECONDARY_CONTAINER)
        } else {
            color(CONTROL)
        });
    if selected {
        chip.icon(crate::icons::check())
    } else {
        chip
    }
}

/// M3 outlined button: accent label inside an outline-colored border, with
/// accent state layers.
pub fn outline_pill(id: impl Into<gpui::ElementId>, cx: &App) -> gpui_component::button::Button {
    use gpui::Styled;
    custom_fill(
        control(id),
        gpui::Rgba {
            a: 0.,
            ..color(SURFACE)
        },
        color(ACCENT),
        true,
        cx,
    )
    .border_1()
    .border_color(color(CONTROL))
    .px(rems(16. / 14.))
    .gap(ICON_GAP)
}

/// M3 text button: an accent label with accent state layers and no container.
pub fn text_button(id: impl Into<gpui::ElementId>, cx: &App) -> gpui_component::button::Button {
    use gpui::Styled;
    custom_fill(
        control(id),
        gpui::Rgba {
            a: 0.,
            ..color(SURFACE)
        },
        color(ACCENT),
        true,
        cx,
    )
    .px(rems(12. / 14.))
    .gap(ICON_GAP)
}

/// M3 icon button: a round target with a centered icon. Tonal buttons (back,
/// reset) sit on surface-container-highest; standard ones are transparent.
pub fn icon_button(
    id: impl Into<gpui::ElementId>,
    size: Rems,
    tonal: bool,
    cx: &App,
) -> gpui_component::button::Button {
    use gpui::Styled;
    let (bg, fg) = if tonal {
        (color(SURFACE_HIGH), color(INK))
    } else {
        (
            gpui::Rgba {
                a: 0.,
                ..color(SURFACE)
            },
            color(GRAY),
        )
    };
    custom_fill(control(id), bg, fg, true, cx)
        .h(size)
        .min_h(size)
        .w(size)
        .min_w(size)
        .px_0()
}

/// gpui-component's medium button inline padding (`px_2p5`). A quiet action
/// placed on a content edge moves out by this much so its ink, not its hover
/// shape, meets the edge.
pub const QUIET_INSET: gpui::Rems = gpui::rems(0.625);

/// Tertiary inline action: quiet gray text.
///
/// Do not add a hover text color here: gpui-component's Button applies its own
/// variant hover during render, and a user-set hover style trips the base
/// button's `hover style already set` debug assertion (debug builds panic).
pub fn quiet(id: impl Into<gpui::ElementId>) -> gpui_component::button::Button {
    use gpui::Styled;
    use gpui_component::button::ButtonVariants;
    control(id).ghost().text_color(color(GRAY))
}

/// Disclosure under an icon-labelled item: the chevron takes the item's icon
/// slot and the label starts on the item's label ruler, so the button's own
/// inline padding hangs outside the content edge.
pub fn item_disclosure(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    open: bool,
) -> gpui_component::button::Button {
    use gpui::{prelude::*, *};
    let chevron = if open {
        crate::icons::chevron_up()
    } else {
        crate::icons::chevron_down()
    };
    quiet(id).self_start().ml(-QUIET_INSET).child(
        div()
            .flex()
            .items_center()
            .gap(ICON_GAP)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(ICON_SLOT)
                    .flex_shrink_0()
                    .child(chevron.size(rems(16. / 14.))),
            )
            .child(label.into()),
    )
}

/// An icon action inside a field shares that field's boundary. Its compact
/// target leaves room for the input's text and focus ring at every text scale.
pub fn input_action(id: impl Into<gpui::ElementId>) -> gpui_component::button::Button {
    use gpui::Styled;
    let size = rems(28. / 14.);
    quiet(id).h(size).min_h(size).w(size).min_w(size).px_0()
}

pub enum NoticeKind {
    Info,
    Danger,
}

/// A page-level notice: tinted surface, status icon and wrapping text.
/// Callers append the notice's actions, which wrap below the text when narrow.
pub fn notice_banner(
    id: impl Into<gpui::ElementId>,
    kind: NoticeKind,
    text: impl Into<gpui::SharedString>,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    let (bg, tone, icon, role) = match kind {
        NoticeKind::Info => (
            SECONDARY_CONTAINER,
            ACCENT,
            crate::icons::info(),
            Role::Status,
        ),
        NoticeKind::Danger => (DANGER_BG, DANGER, crate::icons::error(), Role::Alert),
    };
    div()
        .flex()
        .flex_wrap()
        .w_full()
        .min_w_0()
        .items_center()
        .gap(rems(12. / 14.))
        .p(rems(12. / 14.))
        .rounded(RADIUS_SMALL)
        .bg(color(bg))
        .child(icon.size(ICON_SLOT).flex_shrink_0().text_color(color(tone)))
        .child(
            accessible_text(id, text)
                .role(role)
                .flex_1()
                .flex_basis(rems(200. / 14.))
                .min_w_0()
                .whitespace_normal(),
        )
}

/// Status badge kinds; text always pairs with its tinted background.
pub enum BadgeKind {
    Success,
    Warning,
    Danger,
    Progress,
    Neutral,
    /// An identity rather than a status, e.g. 默认.
    Tonal,
}

/// Icon, padding and label belong to one container; callers append the label.
pub fn badge(kind: BadgeKind) -> gpui::Div {
    use gpui::{Styled, div, prelude::*};
    use gpui_component::Sizable;
    let (text, bg, icon) = match kind {
        BadgeKind::Success => (SUCCESS, SUCCESS_BG, Some(crate::icons::check_circle())),
        BadgeKind::Warning => (WARNING, WARNING_BG, Some(crate::icons::warning())),
        BadgeKind::Danger => (DANGER, DANGER_BG, Some(crate::icons::error())),
        BadgeKind::Progress => (ACCENT, SECONDARY_CONTAINER, Some(crate::icons::schedule())),
        // Neutral is a plain text chip without a status icon.
        BadgeKind::Neutral => (GRAY, INSET, None),
        BadgeKind::Tonal => (ON_SECONDARY_CONTAINER, SECONDARY_CONTAINER, None),
    };
    let mut view = div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap(px(4.))
        .min_h(px(24.))
        .px(px(10.))
        .py(px(3.))
        .rounded(RADIUS_SMALL)
        .bg(color(bg))
        .text_color(color(text))
        .text_size(TEXT_AUX)
        .font_weight(gpui::FontWeight::MEDIUM)
        .whitespace_nowrap();
    if let Some(icon) = icon {
        view = view.child(icon.small());
    }
    view
}

/// A floating label for an outlined control. The control owns its border;
/// supporting status stays in normal flow below it and can grow naturally.
pub fn outlined_field_label(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    label_size: gpui::Rems,
    tone: Rgba,
    content: impl gpui::IntoElement,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .relative()
        .w_full()
        .min_w_0()
        .mt(rems(8. / 14.))
        .child(content)
        .child(
            accessible_text(id, label)
                .absolute()
                .top(rems(-8. / 14.))
                .left(rems(12. / 14.))
                .px(rems(4. / 14.))
                .bg(color(SURFACE))
                .text_color(tone)
                .text_size(label_size)
                .line_height(rems(16. / 14.))
                .font_weight(FontWeight::NORMAL),
        )
}

/// Every preference switch is the M3 switch with icons: a 52×32 track that
/// scales with UI text, a check on the selected thumb and a cross on the
/// unselected one.
pub fn preference_switch(switch: gpui_component::switch::Switch) -> gpui_component::switch::Switch {
    switch
        .color(color(ACCENT))
        .reference_geometry()
        .thumb_icons(crate::icons::check(), crate::icons::close())
        .track_height(rems(32. / 14.))
}

/// Switches in the processing panel's preference rows.
pub fn processing_switch(switch: gpui_component::switch::Switch) -> gpui_component::switch::Switch {
    preference_switch(switch)
}

/// GPUI Component's Root resets rem size from Theme on every render. Update that
/// authority as well as the current window so parent and modal renders agree.
pub fn apply_scale(scale: f32, window: &mut Window, cx: &mut App) {
    let scale = if crate::preferences::FONT_SCALES.contains(&scale) {
        scale
    } else {
        1.0
    };
    let font_size = px(14.0 * scale);
    if Theme::global(cx).font_size != font_size {
        let theme = Theme::global_mut(cx);
        theme.font_size = font_size;
        theme.mono_font_size = px(13.0 * scale);
        Theme::sync_base(cx);
    }
    window.set_rem_size(font_size);
}
