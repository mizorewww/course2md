//! Semantic design tokens and animated appearance for all desktop pages.
use crate::palettes::ThemePreferences;
use gpui::{App, Pixels, Rems, Rgba, Window, px, rems, rgb};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit_material::color::{ColorRole, Scheme};
use gpui_kit_material::tokens::typography::TypeRole;
#[path = "choice_group.rs"]
mod choice_group;
pub use choice_group::SingleChoiceGroup;
#[path = "select_field.rs"]
mod select_field;
pub use select_field::{MenuBuilder, SelectField};

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
    CARD_LINE => CARD_LINE,
    CONTROL => OUTLINE,
    ACCENT => PRIMARY,
    ON_PRIMARY => ON_PRIMARY,
    SECONDARY_CONTAINER => SECONDARY_CONTAINER,
    ON_SECONDARY_CONTAINER => ON_SECONDARY_CONTAINER,
    SUCCESS => SUCCESS,
    SUCCESS_BG => SUCCESS_BG,
    WARNING => WARNING,
    WARNING_BG => WARNING_BG,
    DANGER => ERROR,
    DANGER_BG => ERROR_CONTAINER,
    FIND_HIGHLIGHT => FIND_HIGHLIGHT,
    FIND_CURRENT => FIND_CURRENT,
    FIND_CURRENT_LINE => FIND_CURRENT_LINE,
    SELECTION => SELECTION,
    READING_SELECTION => READING_SELECTION,
);

pub use gpui_kit_material::theme::{blend, system_dark};

pub fn color(token: ColorToken) -> Rgba {
    gpui_kit_material::theme::color(token.role())
}

/// Paints `preference`, resolved against the system, through the kit, which retargets
/// palette changes from their current paint. Persisted settings are the sole authority.
pub fn apply_preference(preference: &ThemePreferences, window: &mut Window, cx: &mut App) {
    let scheme = Scheme::from_palette(preference.resolve(system_dark(cx)));
    if gpui_kit_material::theme::apply_scheme(scheme, window, cx) {
        sync_component_theme(scheme.is_dark(), cx);
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
    set!(color(CANVAS) => background, title_bar, tab_bar, status_bar);
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

/// The window width, in 100%-scale pixels, where page arrangements change:
/// below it pages use narrow gutters, compact navigation and stacked controls.
pub const COMPACT_WINDOW: f32 = 960.;

/// The window width in 100%-scale pixels, so breakpoints follow the text scale.
pub fn design_width(window: &Window) -> f32 {
    f32::from(window.bounds().size.width) * 14. / f32::from(window.rem_size())
}

pub fn is_compact(window: &Window) -> bool {
    design_width(window) < COMPACT_WINDOW
}

/// Space above the workbench hero (100%-scale pixels). A quiet workbench, with
/// nothing in progress, lets the hero sit lower.
pub fn hero_top(quiet: bool, compact: bool) -> f32 {
    match (quiet, compact) {
        (true, true) => 96.,
        (true, false) => 160.,
        (false, true) => 24.,
        (false, false) => 48.,
    }
}

/// The image viewer keeps this margin to each window edge up to its maximum
/// width, and its content sits inside `IMAGE_DIALOG_PADDING` on each side.
pub const IMAGE_DIALOG_MARGIN: Pixels = px(24.);
pub const IMAGE_DIALOG_MAX_WIDTH: Pixels = px(1180.);
pub const IMAGE_DIALOG_MIN_WIDTH: Pixels = px(280.);
pub const IMAGE_DIALOG_PADDING: Pixels = px(16.);

/// Controls drawn over a screenshot sit on a fixed dark scrim with white text:
/// the image beneath is not themed, so palette roles cannot promise contrast.
pub fn image_scrim() -> Rgba {
    gpui::rgba(0x101416c7)
}

pub fn on_image_scrim() -> Rgba {
    rgb(0xffffff)
}

/// Window caption buttons (see `window_controls`). Minimize and maximize take
/// the M3 hover and press state layers of the title bar's ink, translucent so
/// they also read over a dialog's scrim. Close turns the platform's red with
/// white glyphs, as Windows draws it, whatever the palette.
pub fn caption_hover() -> Rgba {
    Rgba {
        a: 0.08,
        ..color(INK)
    }
}

pub fn caption_pressed() -> Rgba {
    Rgba {
        a: 0.10,
        ..color(INK)
    }
}

pub fn caption_close_hover() -> Rgba {
    rgb(0xc42b1c)
}

pub fn caption_close_pressed() -> Rgba {
    Rgba {
        a: 0.9,
        ..rgb(0xc42b1c)
    }
}

pub fn on_caption_close() -> Rgba {
    rgb(0xffffff)
}

/* ---------- Column widths (rems; structure that scales with text) ---------- */
pub const COLUMN: Rems = rems(65.714);

/* ---------- Type scale (rems; 14px is 1rem) ---------- */
pub const TEXT_AUX: Rems = rems(0.857);
pub const TEXT_BODY: Rems = rems(1.);
pub const TEXT_TITLE: Rems = rems(1.286);
pub const TEXT_READER: Rems = rems(1.143);
/// Page titles (M3 headline small at 30px).
pub const TEXT_HEADLINE: Rems = rems(30. / 14.);
/// Settings content reads one step larger than navigation and the workbench.
pub const TEXT_SETTINGS_BODY: Rems = rems(16. / 14.);
pub const TEXT_SETTINGS_AUX: Rems = TEXT_BODY;

/// Kit's `Button` fixes its label to one of its sizes' text roles and offers no
/// override, so a label asking for body size or above takes the largest role
/// (1rem). Exact sizes return with `MaterialButton` in the Material phase.
pub trait ButtonLabelSize {
    fn label_text_size(self, size: Rems) -> Self;
}
impl ButtonLabelSize for gpui_kit::component::button::Button {
    fn label_text_size(self, size: Rems) -> Self {
        use gpui_kit::component::Sizable as _;
        if size.0 >= TEXT_BODY.0 {
            self.with_size(gpui_kit::component::Size::Large)
        } else {
            self
        }
    }
}

/// A destructive menu row. Kit's menu items have no danger role, so the row
/// carries its own color.
pub fn danger_menu_item(
    label: impl Into<gpui::SharedString>,
    icon: gpui_kit::component::Icon,
) -> gpui_kit::component::menu::PopupMenuItem {
    use gpui::{ParentElement as _, Styled as _};
    let label = label.into();
    gpui_kit::component::menu::PopupMenuItem::element(move |_, _| {
        gpui_kit::component::h_flex()
            .gap_x_1()
            .text_color(color(DANGER))
            .child(icon.clone())
            .child(label.clone())
    })
}

/// Gives a shared control the settings reading size. Controls size their own
/// labels, so the role cannot be inherited from the enclosing settings card.
pub trait SettingsText {
    fn settings_text(self) -> Self;
}
impl SettingsText for gpui_kit::component::button::Button {
    fn settings_text(self) -> Self {
        self.label_text_size(TEXT_SETTINGS_BODY)
    }
}
impl SettingsText for gpui_kit::component::input::Input {
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
/// A title longer than its column (a narrow window, a large text scale)
/// wraps rather than running past the edge.
pub fn page_heading(
    id: impl Into<gpui::ElementId>,
    title: impl Into<gpui::SharedString>,
) -> gpui::Div {
    use gpui::{prelude::*, *};
    div().flex().items_center().min_w_0().child(
        accessible_text(id, title.into())
            .min_w_0()
            .whitespace_normal()
            .role(Role::Heading)
            .text_size(TEXT_HEADLINE)
            .line_height(rems(36. / 14.))
            .font_weight(FontWeight::NORMAL),
    )
}

/// The corners of a connected tile: 24px on its group's outside and 6px
/// where it meets a neighbour. `outer` lists which corners are outside, as
/// top-left, top-right, bottom-right, bottom-left.
pub fn tile_corners<E: gpui::Styled>(element: E, outer: [bool; 4]) -> E {
    let radius = |outside: bool| {
        if outside {
            TILE_RADIUS
        } else {
            TILE_INNER_RADIUS
        }
    };
    element
        .rounded_tl(radius(outer[0]))
        .rounded_tr(radius(outer[1]))
        .rounded_br(radius(outer[2]))
        .rounded_bl(radius(outer[3]))
}

/// The corners of the tile at `index` of a vertical connected list of `count`.
pub fn list_corners(index: usize, count: usize) -> [bool; 4] {
    let first = index == 0;
    let last = index + 1 >= count;
    [first, first, last, last]
}

/// One tile of a connected group: the surface with `tile_corners`.
pub fn tile(outer: [bool; 4]) -> gpui::Div {
    use gpui::{Styled, div};
    tile_corners(div().w_full().min_w_0().bg(color(SURFACE)), outer)
}

/// The tile at `index` of a vertical connected list of `count` tiles.
pub fn list_tile(index: usize, count: usize) -> gpui::Div {
    tile(list_corners(index, count))
}

/// The main label of a settings tile: 17px medium, as on the canvas.
pub const TEXT_TILE_LABEL: Rems = rems(17. / 14.);

/// The text scale of a tile's content. Settings tiles read one step larger
/// than the workbench's processing options; both share one composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileScale {
    /// Settings: 17px labels over 14px supporting text.
    Regular,
    /// The workbench's options: 16px labels over 13px supporting text.
    Compact,
}

impl TileScale {
    pub fn label_size(self) -> Rems {
        match self {
            TileScale::Regular => TEXT_TILE_LABEL,
            TileScale::Compact => rems(16. / 14.),
        }
    }

    pub fn support_size(self) -> Rems {
        match self {
            TileScale::Regular => TEXT_SETTINGS_AUX,
            TileScale::Compact => rems(13. / 14.),
        }
    }

    /// The label's line box. Tiles that act are Buttons, whose text box is
    /// exactly the font size; an explicit line height keeps wrapped and
    /// clamped lines (and their descenders) inside it at every text scale.
    pub fn label_line_height(self) -> Rems {
        match self {
            TileScale::Regular => rems(24. / 14.),
            TileScale::Compact => rems(22. / 14.),
        }
    }

    pub fn support_line_height(self) -> Rems {
        match self {
            TileScale::Regular => rems(20. / 14.),
            TileScale::Compact => rems(18. / 14.),
        }
    }

    /// A tile's main label, medium weight.
    pub fn label(
        self,
        id: impl Into<gpui::ElementId>,
        value: impl Into<gpui::SharedString>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui::prelude::*;
        accessible_text(id, value)
            .text_size(self.label_size())
            .line_height(self.label_line_height())
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(color(INK))
    }

    /// Supporting text under a tile's label, regular gray.
    pub fn support(
        self,
        id: impl Into<gpui::ElementId>,
        value: impl Into<gpui::SharedString>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui::prelude::*;
        accessible_text(id, value)
            .min_w_0()
            .whitespace_normal()
            .text_size(self.support_size())
            .line_height(self.support_line_height())
            .font_weight(gpui::FontWeight::NORMAL)
            .text_color(color(GRAY))
    }

    /// One content tile of a connected group at this scale: the surface,
    /// its corners, and the reading size settings content inherits.
    pub fn surface(self, outer: [bool; 4]) -> gpui::Div {
        use gpui::prelude::*;
        tile(outer)
            .flex()
            .flex_col()
            .flex_shrink_0()
            .when(self == TileScale::Regular, |tile| {
                tile.text_size(TEXT_SETTINGS_BODY)
            })
    }

    /// A whole tile that acts: the surface with M3 state layers, the tile's
    /// corners and this scale's padding. Callers add one `row` holding the
    /// leading slot, label column and trailing items.
    pub fn button(
        self,
        id: impl Into<gpui::ElementId>,
        outer: [bool; 4],
        cx: &App,
    ) -> gpui_kit::component::button::Button {
        use gpui::Styled;
        let button = custom_fill(
            gpui_kit::component::button::Button::new(id),
            color(SURFACE),
            color(INK),
            true,
            cx,
        )
        .w_full()
        .h_auto()
        .justify_start()
        .text_left()
        .whitespace_normal()
        .font_weight(gpui::FontWeight::NORMAL);
        let button = match self {
            TileScale::Regular => button
                .min_h(rems(64. / 14.))
                .px(SETTING_ITEM_INSET)
                .py(rems(16. / 14.)),
            TileScale::Compact => button
                .min_h(rems(68. / 14.))
                .pl(SETTING_ITEM_INSET)
                .pr(rems(16. / 14.))
                .py(rems(12. / 14.)),
        };
        tile_corners(button, outer)
    }

    /// The gap between a tile's leading slot, text column and trailing items.
    pub fn gap(self) -> Rems {
        match self {
            TileScale::Regular => rems(16. / 14.),
            TileScale::Compact => rems(12. / 14.),
        }
    }

    /// The content of a `button` tile: its leading slot, text column and
    /// trailing items in one row at this scale's gap. The component's own
    /// content row has a fixed 8px gap and does not wrap, so a tile's
    /// children go in here to keep text on the rows' ruler and let long
    /// labels wrap.
    pub fn row(self) -> gpui::Div {
        use gpui::prelude::*;
        gpui_kit::component::h_flex()
            .flex_1()
            .w_full()
            .min_w_0()
            .items_center()
            .whitespace_normal()
            .gap(self.gap())
    }

    /// A tile's text column: its label and, when present, the value or
    /// explanation under it.
    pub fn text(
        self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<gpui::SharedString>,
        support: Option<gpui::SharedString>,
    ) -> gpui::Div {
        use gpui::prelude::*;
        let id = id.into();
        gpui_kit::component::v_flex()
            .flex_1()
            .min_w_0()
            .gap(rems(2. / 14.))
            .child(self.label(id.clone(), label))
            .when_some(support, |column, support| {
                column.child(
                    self.support(gpui::SharedString::from(format!("{id:?}-support")), support),
                )
            })
    }
}

/// Between a section title and its group's first tile.
pub const SECTION_TITLE_GAP: Pixels = px(10.);

pub use tile_group::TileGroup;
mod tile_group {
    use super::{SECTION_TITLE_GAP, SETTING_ITEM_INSET, TILE_GAP, TileScale, list_corners};
    use gpui::{AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled, Window, div};
    use gpui_kit::component::v_flex;

    /// Builds a tile's content once the group knows which corners are outside.
    type TileBuilder = Box<dyn FnOnce([bool; 4]) -> AnyElement>;

    enum TileMain {
        /// Content on the plain surface; the group draws the tile.
        Content(AnyElement),
        /// A tile that draws itself (a button), given its corners.
        Built(TileBuilder),
        /// Two tiles side by side in one row of the group.
        Pair(TileBuilder, TileBuilder),
    }

    struct GroupTile {
        main: TileMain,
        details: Vec<AnyElement>,
    }

    /// One section as on the M3 canvas: a quiet title above a connected group
    /// of tiles, each item on its own surface with 24px outer and 6px inner
    /// corners and 2px between tiles. Settings pages and the workbench's
    /// processing options share it at their own `TileScale`.
    #[derive(gpui::IntoElement)]
    pub struct TileGroup {
        id: &'static str,
        title: &'static str,
        scale: TileScale,
        tiles: Vec<GroupTile>,
    }

    impl TileGroup {
        pub fn new(id: &'static str, title: &'static str) -> Self {
            Self {
                id,
                title,
                scale: TileScale::Regular,
                tiles: Vec::new(),
            }
        }

        /// The text scale of the group's content tiles.
        pub fn scale(mut self, scale: TileScale) -> Self {
            self.scale = scale;
            self
        }

        /// A tile that draws its own surface (a button row), given its corners
        /// as top-left, top-right, bottom-right, bottom-left (true = outside).
        pub fn tile(mut self, build: impl FnOnce([bool; 4]) -> AnyElement + 'static) -> Self {
            self.tiles.push(GroupTile {
                main: TileMain::Built(Box::new(build)),
                details: Vec::new(),
            });
            self
        }

        /// Two tiles sharing one row of the group.
        pub fn pair(
            mut self,
            leading: impl FnOnce([bool; 4]) -> AnyElement + 'static,
            trailing: impl FnOnce([bool; 4]) -> AnyElement + 'static,
        ) -> Self {
            self.tiles.push(GroupTile {
                main: TileMain::Pair(Box::new(leading), Box::new(trailing)),
                details: Vec::new(),
            });
            self
        }

        /// Content that belongs to the previous item (a disclosure, a summary
        /// line): it stays inside that item's tile.
        pub fn detail(mut self, element: impl IntoElement) -> Self {
            match self.tiles.last_mut() {
                Some(tile) => tile.details.push(element.into_any_element()),
                None => self.tiles.push(GroupTile {
                    main: TileMain::Content(element.into_any_element()),
                    details: Vec::new(),
                }),
            }
            self
        }
    }

    impl ParentElement for TileGroup {
        fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
            self.tiles
                .extend(elements.into_iter().map(|element| GroupTile {
                    main: TileMain::Content(element),
                    details: Vec::new(),
                }));
        }
    }

    impl RenderOnce for TileGroup {
        fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
            let count = self.tiles.len();
            let scale = self.scale;
            let mut group = v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap(TILE_GAP)
                .child(
                    super::section_title(self.id, self.title)
                        .px(SETTING_ITEM_INSET)
                        .mb(SECTION_TITLE_GAP - TILE_GAP),
                );
            for (index, tile) in self.tiles.into_iter().enumerate() {
                let first = index == 0;
                let last = index + 1 == count;
                let outer = list_corners(index, count);
                let element = match tile.main {
                    TileMain::Content(content) => scale
                        .surface(outer)
                        .child(content)
                        .children(tile.details)
                        .into_any_element(),
                    TileMain::Built(build) if tile.details.is_empty() => build(outer),
                    // Details continue below the built tile inside the same
                    // surface, so its own bottom corners stay inner.
                    TileMain::Built(build) => scale
                        .surface(outer)
                        .child(build([outer[0], outer[1], false, false]))
                        .children(tile.details)
                        .into_any_element(),
                    TileMain::Pair(leading, trailing) => v_flex()
                        .w_full()
                        .min_w_0()
                        .child(
                            div()
                                .grid()
                                .grid_cols(2)
                                .w_full()
                                .min_w_0()
                                .gap(TILE_GAP)
                                .child(leading([first, false, false, last]))
                                .child(trailing([false, first, last, false])),
                        )
                        .children(tile.details)
                        .into_any_element(),
                };
                group = group.child(element);
            }
            group
        }
    }
}

/// A settings tile's main label (`TileScale::Regular`).
pub fn tile_label(
    id: impl Into<gpui::ElementId>,
    value: impl Into<gpui::SharedString>,
) -> gpui::Stateful<gpui::Div> {
    TileScale::Regular.label(id, value)
}

/// Supporting text under a settings tile's label (`TileScale::Regular`).
pub fn tile_support(
    id: impl Into<gpui::ElementId>,
    value: impl Into<gpui::SharedString>,
) -> gpui::Stateful<gpui::Div> {
    TileScale::Regular.support(id, value)
}

/// A settings tile's text column (`TileScale::Regular`).
pub fn tile_text(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    support: Option<gpui::SharedString>,
) -> gpui::Div {
    TileScale::Regular.text(id, label, support)
}

/// The content row of a settings tile button (`TileScale::Regular`).
pub fn tile_row() -> gpui::Div {
    TileScale::Regular.row()
}

/// A whole settings tile that acts (`TileScale::Regular`); its children go
/// in one `tile_row`.
pub fn tile_button(
    id: impl Into<gpui::ElementId>,
    outer: [bool; 4],
    cx: &App,
) -> gpui_kit::component::button::Button {
    TileScale::Regular.button(id, outer, cx)
}

/// A settings tile that expands content in place below its row.
pub fn tile_disclosure(
    id: impl Into<gpui::ElementId>,
    outer: [bool; 4],
    open: bool,
    cx: &App,
) -> gpui_kit::component::button::Button {
    use gpui_kit::component::Selectable;
    tile_button(id, outer, cx).selected(open)
}

/// An explanation inside a tile: the information icon and supporting text.
pub fn tile_info(id: impl Into<gpui::ElementId>, text: impl Into<gpui::SharedString>) -> gpui::Div {
    use gpui::prelude::*;
    gpui_kit::component::h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap(rems(10. / 14.))
        .child(
            gpui::div()
                .flex()
                .flex_shrink_0()
                .h(rems(20. / 14.))
                .items_center()
                .child(
                    crate::icons::info()
                        .size(rems(18. / 14.))
                        .text_color(color(GRAY)),
                ),
        )
        .child(tile_support(id, text).flex_1())
}

/// The trailing chevron of a tile that opens something.
pub fn tile_chevron() -> gpui_kit::component::Icon {
    use gpui::Styled;
    crate::icons::chevron_right()
        .size(rems(20. / 14.))
        .flex_shrink_0()
        .text_color(color(GRAY))
}

/// The chevron of an in-place disclosure; it turns over when open.
pub fn disclosure_chevron(open: bool) -> gpui_kit::component::Icon {
    use gpui::Styled;
    if open {
        crate::icons::chevron_up()
    } else {
        crate::icons::chevron_down()
    }
    .size(rems(20. / 14.))
    .flex_shrink_0()
    .text_color(color(GRAY))
}

/// The M3 radio of a choice: an outlined ring, filled with a dot when
/// selected; 38% ink when the choice is unavailable.
pub fn choice_radio(selected: bool, enabled: bool) -> gpui::Div {
    use gpui::prelude::*;
    let tone = if !enabled {
        Rgba {
            a: 0.38,
            ..color(INK)
        }
    } else if selected {
        color(ACCENT)
    } else {
        color(CONTROL)
    };
    gpui::div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(rems(20. / 14.))
        .rounded_full()
        .border_2()
        .border_color(tone)
        .when(selected, |ring| {
            ring.child(gpui::div().size(rems(10. / 14.)).rounded_full().bg(tone))
        })
}

pub use choice_tile::ChoiceTile;
mod choice_tile {
    use super::*;
    use gpui::{
        AnyElement, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
        SharedString, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder,
    };
    use gpui_kit::component::v_flex;

    /// One choice in a connected group of tiles, as on the canvas: tonal
    /// selection on the secondary container, a leading icon, the label with
    /// optional supporting text, a trailing status chip and a radio. An
    /// unavailable choice keeps its status chip at full strength.
    pub struct ChoiceTile {
        id: ElementId,
        outer: [bool; 4],
        icon: Option<AnyElement>,
        tinted: Option<(gpui_kit::component::Icon, Tint, Pixels)>,
        title: SharedString,
        description: Option<SharedString>,
        status: Option<AnyElement>,
        selected: bool,
        enabled: bool,
    }

    impl ChoiceTile {
        /// The choice at `index` of a vertical group of `count`.
        pub fn new(
            id: impl Into<ElementId>,
            index: usize,
            count: usize,
            title: impl Into<SharedString>,
            selected: bool,
        ) -> Self {
            Self {
                id: id.into(),
                outer: list_corners(index, count),
                icon: None,
                tinted: None,
                title: title.into(),
                description: None,
                status: None,
                selected,
                enabled: true,
            }
        }

        /// Which corners are outside the group, when the tile is not simply
        /// one of a vertical list (e.g. choices hanging below another tile).
        pub fn corners(mut self, outer: [bool; 4]) -> Self {
            self.outer = outer;
            self
        }

        pub fn icon(mut self, icon: impl IntoElement) -> Self {
            self.icon = Some(icon.into_any_element());
            self
        }

        /// A category icon in its tinted circle. While the tile is selected
        /// the circle turns to the surface, so it stays distinct from the
        /// secondary container behind it.
        pub fn tinted_icon(
            mut self,
            icon: gpui_kit::component::Icon,
            tint: Tint,
            size: Pixels,
        ) -> Self {
            self.tinted = Some((icon, tint, size));
            self
        }

        pub fn description(mut self, description: impl Into<SharedString>) -> Self {
            let description = description.into();
            self.description = (!description.is_empty()).then_some(description);
            self
        }

        pub fn status(mut self, status: Option<impl IntoElement>) -> Self {
            self.status = status.map(IntoElement::into_any_element);
            self
        }

        pub fn enabled(mut self, enabled: bool) -> Self {
            self.enabled = enabled;
            self
        }

        pub fn render(self, window: &mut Window, cx: &mut App) -> gpui_base::Button {
            let id = self.id;
            let selected = self.selected;
            let enabled = self.enabled;
            let amount = crate::motion::selection_value(
                SharedString::from(format!("choice-tile-{id:?}")),
                if selected { 1. } else { 0. },
                window,
                cx,
            );
            let bg = blend(color(SURFACE), color(SECONDARY_CONTAINER), amount);
            let ink = if selected {
                color(ON_SECONDARY_CONTAINER)
            } else {
                color(INK)
            };
            let fg = if enabled {
                ink
            } else {
                Rgba { a: 0.38, ..ink }
            };
            let icon = self.icon.or_else(|| {
                self.tinted.map(|(icon, tint, size)| {
                    tinted_icon(icon, tint, size)
                        .bg(blend(tint.container(), color(SURFACE), amount))
                        .into_any_element()
                })
            });
            let title = self.title;
            tile_corners(
                gpui_base::Button::new(id.clone())
                    .selected(selected)
                    .disabled(!enabled)
                    .aria_toggled(if selected {
                        gpui::accesskit::Toggled::True
                    } else {
                        gpui::accesskit::Toggled::False
                    })
                    .accessibility_label(title.clone())
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    .min_h(rems(64. / 14.))
                    .justify_start()
                    .gap(rems(16. / 14.))
                    .pl(SETTING_ITEM_INSET)
                    .pr(rems(20. / 14.))
                    .py(rems(12. / 14.)),
                self.outer,
            )
            .bg(bg)
            // The outline is reserved for keyboard focus, so focus never
            // moves the content.
            .border_2()
            .border_color(Rgba { a: 0., ..bg })
            .text_color(fg)
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(blend(bg, ink, 0.08)))
                    .active(move |style| style.bg(blend(bg, ink, 0.10)))
            })
            .focus(|style| style.border_color(color(ACCENT)))
            .children(icon)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(rems(2. / 14.))
                    .child(
                        div()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(TEXT_SETTINGS_BODY)
                            .line_height(rems(22. / 14.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .when_some(self.description, |column, description| {
                        column.child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .text_size(TEXT_SETTINGS_AUX)
                                .line_height(rems(20. / 14.))
                                .text_color(if selected {
                                    Rgba { a: 0.8, ..fg }
                                } else {
                                    color(GRAY)
                                })
                                .child(description),
                        )
                    }),
            )
            .children(self.status)
            .child(choice_radio(selected, enabled))
        }
    }
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

/// A category or resource icon in a round tonal container. Give the size in
/// rems where the circle shares a ruler with text, so both follow the text
/// scale.
pub fn tinted_icon(
    icon: gpui_kit::component::Icon,
    tint: Tint,
    size: impl Into<gpui::AbsoluteLength>,
) -> gpui::Div {
    use gpui::{AbsoluteLength, ParentElement, Styled, div};
    let size = size.into();
    let glyph = match size {
        AbsoluteLength::Pixels(size) => AbsoluteLength::Pixels(size * 0.5),
        AbsoluteLength::Rems(size) => AbsoluteLength::Rems(rems(size.0 * 0.5)),
    };
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(size)
        .rounded_full()
        .bg(tint.container())
        .text_color(tint.content())
        .child(icon.size(glyph))
}

/* ---------- Neutral shadows ---------- */
fn shadow_color(alpha: f32) -> gpui::Hsla {
    gpui::hsla(0., 0., 0., alpha)
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
    // Word joiners only steer line wrapping; a screen reader gets the
    // plain words so its pacing matches the text's meaning.
    let label = if value.contains('\u{2060}') {
        SharedString::from(value.replace('\u{2060}', ""))
    } else {
        value.clone()
    };
    div()
        .id(id)
        .role(Role::Label)
        .aria_label(label)
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

/// A 16:9 video cover with `radius` corners. Without a cover the tonal
/// frame stays, so rows keep their rhythm while covers load; callers place
/// their placeholder icon in it. Callers give it its width.
pub fn cover_frame(radius: Pixels, cover: Option<std::path::PathBuf>) -> gpui::Div {
    use gpui::{prelude::*, *};
    div()
        .aspect_ratio(16. / 9.)
        .flex_shrink_0()
        .rounded(radius)
        .overflow_hidden()
        .bg(color(SECONDARY_CONTAINER))
        .flex()
        .items_center()
        .justify_center()
        .when_some(cover, |slot, cover| {
            slot.child(
                img(cover)
                    .size_full()
                    .rounded(radius)
                    .object_fit(ObjectFit::Cover),
            )
        })
}

/// A small list cover: 8px corners, its width scaling with the text size.
pub fn cover_slot(width: Rems, cover: Option<std::path::PathBuf>) -> gpui::Div {
    use gpui::Styled;
    cover_frame(RADIUS_SMALL, cover).w(width)
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
    let theme = Theme::global_mut(cx);
    theme.font_size = px(14.);
    // The kit's type scale; CJK text falls back to the system's fonts.
    theme.font_family = TypeRole::BodyMedium.scale().font_family.into();
    sync_component_theme(false, cx);
}

/// One baseline for ordinary actions; content buttons explicitly opt into auto height.
pub fn control(id: impl Into<gpui::ElementId>) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    gpui_kit::component::button::Button::new(id)
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
    state: &gpui::Entity<gpui_kit::component::input::InputState>,
) -> gpui_kit::component::input::Input {
    use gpui::Styled;
    gpui_kit::component::input::Input::new(state)
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
    state: &gpui::Entity<gpui_kit::component::input::InputState>,
    input: gpui_kit::component::input::Input,
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
    icon: gpui_kit::component::Icon,
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
    state: &gpui::Entity<gpui_kit::component::input::InputState>,
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
                gpui_kit::component::input::Input::new(state)
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
    gpui_kit::component::v_flex()
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
        // Unselected, the M3 outlined card's outline-variant; CARD_LINE
        // fades into the surface of dark palettes.
        .border_color(blend(color(HAIRLINE), color(SECONDARY_CONTAINER), amount))
        .bg(blend(color(SURFACE), color(SECONDARY_CONTAINER), amount))
        .cursor_pointer()
        .hover(|style| style.shadow_sm())
        .active(|style| style.opacity(0.9).shadow_none())
        .focus(|style| style.border_color(color(ACCENT)))
}

/// The single forward action of a page or region, in the active theme's accent.
pub fn primary_pill(id: impl Into<gpui::ElementId>) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    use gpui_kit::component::button::ButtonVariants;
    control(id).primary().px(rems(16. / 14.)).gap(ICON_GAP)
}

/// Paints a button with an exact fill. gpui-component dilutes a custom fill
/// to 20%, so the token is also set as the background. With state layers,
/// hover and press add 8% and 10% of the content color, as in M3.
fn custom_fill(
    button: gpui_kit::component::button::Button,
    bg: gpui::Rgba,
    fg: gpui::Rgba,
    state_layers: bool,
    cx: &App,
) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants};
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
) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    use gpui_kit::component::Sizable;
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
        .pl(rems(NAV_PAD_START / 14.))
        .pr(rems(NAV_PAD_END / 14.))
        .label_text_size(rems(NAV_LABEL_SIZE / 14.))
        .with_size(component_icon_size(NAV_ICON, cx))
}

// App bar destination geometry at the 14px base, shared by the buttons and by
// the measurement that decides whether their labels fit.
const NAV_PAD_START: f32 = 12.;
const NAV_PAD_END: f32 = 16.;
const NAV_ICON: f32 = 20.;
/// The component's icon–label gap (`gap_2`, half a rem) rounded up.
const NAV_ICON_GAP: f32 = 8.;
const NAV_LABEL_SIZE: f32 = 15.;

/// An app bar destination with its icon and `shown` text: its label, or a
/// shorter form (a task count) or nothing when the bar cannot hold labels.
/// The full label always names the button, and shows as its tooltip while it
/// is not drawn in full; a collapsed destination (its icon, or the icon and
/// a count) keeps even padding.
pub fn nav_destination(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    icon: gpui_kit::component::Icon,
    label: impl Into<gpui::SharedString>,
    shown: Option<gpui::SharedString>,
    cx: &App,
) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    use gpui::prelude::FluentBuilder as _;
    let label = label.into();
    let collapsed = shown.as_ref() != Some(&label);
    nav_action(id, selected, cx)
        .icon(icon)
        .when(collapsed, |button| button.pr(rems(NAV_PAD_START / 14.)))
        .when_some(shown, |button, shown| button.label(shown))
        .accessibility_label(label.clone())
        .when(collapsed, |button| button.tooltip(label))
}

/// The width, in window pixels, of a `nav_destination` drawing `shown` at the
/// current text scale; `collapsed` when `shown` is not the full label, which
/// gives the destination even padding.
pub fn nav_destination_width(shown: Option<&str>, collapsed: bool, window: &Window) -> f32 {
    let scaled = |value: f32| value * f32::from(window.rem_size()) / 14.;
    let end = if collapsed {
        NAV_PAD_START
    } else {
        NAV_PAD_END
    };
    match shown {
        None => scaled(NAV_PAD_START + NAV_ICON + end),
        Some(text) => {
            scaled(NAV_PAD_START + NAV_ICON + NAV_ICON_GAP + end)
                + shaped_width(
                    text,
                    scaled(NAV_LABEL_SIZE),
                    gpui::FontWeight::MEDIUM,
                    window,
                )
        }
    }
}

/// The shaped width of one line of `text` in the window's font.
pub fn shaped_width(text: &str, size: f32, weight: gpui::FontWeight, window: &Window) -> f32 {
    let mut style = window.text_style();
    style.font_weight = weight;
    let text = gpui::SharedString::from(text.to_owned());
    f32::from(
        window
            .text_system()
            .shape_line(text.clone(), px(size), &[style.to_run(text.len())], None)
            .width,
    )
}

/// The component `Size` that makes a gpui-component Button draw its icon at
/// `design` px of the 14px base, following the text scale. The Button sizes
/// its icon to 75% of a custom size and overrides the icon's own size; every
/// other dimension a custom size would set is replaced by our explicit
/// height, padding and label size.
fn component_icon_size(design: f32, cx: &App) -> gpui_kit::component::Size {
    let font = Theme::global(cx).font_size;
    gpui_kit::component::Size::Size(font * (design / 14. / 0.75))
}

/// M3 filled tonal button: a secondary action that still carries weight,
/// e.g. opening a finished note from a list row.
pub fn tonal_pill(id: impl Into<gpui::ElementId>, cx: &App) -> gpui_kit::component::button::Button {
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
    button: gpui_kit::component::button::Button,
    disabled: bool,
    cx: &App,
) -> gpui_kit::component::button::Button {
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
) -> gpui_kit::component::button::Button {
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
    let chip = custom_fill(
        gpui_kit::component::button::Button::new(id),
        bg,
        fg,
        true,
        cx,
    )
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
pub fn outline_pill(
    id: impl Into<gpui::ElementId>,
    cx: &App,
) -> gpui_kit::component::button::Button {
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
pub fn text_button(
    id: impl Into<gpui::ElementId>,
    cx: &App,
) -> gpui_kit::component::button::Button {
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
) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    use gpui_kit::component::Sizable;
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
    // gpui-component sizes a button's icon from the button's size and
    // ignores the icon's own; an icon-only button sizes it at three quarters
    // of an explicit size. Give it the glyph the canvas uses (22px in a 48px
    // button, 20px in 40px), in the current text scale.
    let rem = gpui_kit::component::Theme::global(cx).font_size;
    let glyph = rem * (size.0 / 4. + 10. / 14.);
    custom_fill(control(id), bg, fg, true, cx)
        .with_size(gpui_kit::component::Size::Size(glyph / 0.75))
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
pub fn quiet(id: impl Into<gpui::ElementId>) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    use gpui_kit::component::button::ButtonVariants;
    control(id).ghost().text_color(color(GRAY))
}

/// An icon action inside a field shares that field's boundary. Its compact
/// target leaves room for the input's text and focus ring at every text scale.
pub fn input_action(id: impl Into<gpui::ElementId>) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    let size = rems(28. / 14.);
    quiet(id).h(size).min_h(size).w(size).min_w(size).px_0()
}

/* ---------- Dialogs ---------- */

/// A basic dialog with a short form or a single status (rename, new folder,
/// sign-in). In rems so it grows with the text it holds.
pub const DIALOG_WIDTH: Rems = rems(448. / 14.);
/// A dialog that reports a multi-step operation (storage moves).
pub const DIALOG_WIDE_WIDTH: Rems = rems(560. / 14.);
/// Dialogs keep this margin to the window's sides.
pub const DIALOG_MARGIN: Pixels = px(24.);

/// A dialog width token at the current text scale, narrowed to fit the
/// window with its side margins.
pub fn dialog_width(width: Rems, window: &Window) -> Pixels {
    width
        .to_pixels(window.rem_size())
        .min((window.viewport_size().width - DIALOG_MARGIN * 2.).max(px(0.)))
}

/// The M3 dialog headline: 24px regular on-surface text.
pub fn dialog_title(
    id: impl Into<gpui::ElementId>,
    title: impl Into<gpui::SharedString>,
) -> gpui::Stateful<gpui::Div> {
    use gpui::prelude::*;
    accessible_text(id, title)
        .role(gpui::Role::Heading)
        .text_size(rems(24. / 14.))
        .line_height(rems(32. / 14.))
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(color(INK))
}

/// A dialog's actions at its trailing edge, wrapping when narrow: a text
/// button to leave things as they are, then an optional outlined secondary
/// action, then the filled (or error) confirming action.
pub fn dialog_actions() -> gpui::Div {
    use gpui::prelude::*;
    gpui_kit::component::h_flex()
        .w_full()
        .pt(px(8.))
        .gap_2()
        .flex_wrap()
        .justify_end()
}

/// M3 error button: the destructive confirming action of a dialog.
pub fn danger_pill(
    id: impl Into<gpui::ElementId>,
    cx: &App,
) -> gpui_kit::component::button::Button {
    use gpui::Styled;
    custom_fill(control(id), color(DANGER), color(ON_PRIMARY), true, cx)
        .px(rems(16. / 14.))
        .gap(ICON_GAP)
}

/// Every dialog shares one M3 chrome: a 28px-radius surface without an
/// outline, raised by the popover shadow, with 24px padding and the 24px
/// regular headline. Its body scrolls between the headline and the pinned
/// actions (the component's footer).
pub trait MaterialDialog: gpui::Styled + Sized {
    fn headline(self, title: gpui::Stateful<gpui::Div>) -> Self;

    /// The surface alone, for a dialog that lays out its own content (the
    /// image viewer).
    fn material_frame(self) -> Self {
        self.rounded(RADIUS_HERO)
            .bg(color(SURFACE))
            .border_0()
            .shadow(shadow_popover())
    }

    /// The surface, its padding and the headline.
    fn material(
        self,
        id: impl Into<gpui::ElementId>,
        title: impl Into<gpui::SharedString>,
    ) -> Self {
        self.headline(dialog_title(id, title))
            .material_frame()
            .p(px(24.))
    }
}

impl MaterialDialog for gpui_kit::component::dialog::Dialog {
    fn headline(self, title: gpui::Stateful<gpui::Div>) -> Self {
        self.title(title)
    }
}

impl MaterialDialog for gpui_kit::component::dialog::AlertDialog {
    fn headline(self, title: gpui::Stateful<gpui::Div>) -> Self {
        self.title(title)
    }
}

/// A confirmation as an M3 basic dialog: headline, body text and the
/// actions. The buttons dispatch the dialog's own Cancel and Confirm, so
/// Escape, Enter and `on_ok` keep working. A destructive confirmation
/// (`danger`) paints its confirming action in the error color.
///
/// Like every main action, a dialog's confirming action carries an icon with
/// its verb: the trash for a destructive one, the check otherwise. Dialogs
/// that build their own actions follow the same rule.
pub fn confirm_dialog(
    dialog: gpui_kit::component::dialog::AlertDialog,
    title: impl Into<gpui::SharedString>,
    cancel: impl Into<gpui::SharedString>,
    confirm: impl Into<gpui::SharedString>,
    danger: bool,
    cx: &App,
) -> gpui_kit::component::dialog::AlertDialog {
    use gpui::{ParentElement, Styled};
    use gpui_kit::component::dialog::{Cancel, Confirm};
    let confirm_button = if danger {
        danger_pill("material-alert-confirm", cx).icon(crate::icons::delete())
    } else {
        primary_pill("material-alert-confirm").icon(crate::icons::check())
    };
    dialog
        .material("material-alert-title", title)
        .gap(px(16.))
        .text_size(TEXT_SETTINGS_BODY)
        .footer(
            dialog_actions()
                .child(
                    text_button("material-alert-cancel", cx)
                        .settings_text()
                        .label(cancel.into())
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx)),
                )
                .child(
                    confirm_button
                        .settings_text()
                        .label(confirm.into())
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(Confirm { secondary: false }), cx)
                        }),
                ),
        )
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
    use gpui_kit::component::Sizable;
    let (text, bg, icon) = match kind {
        BadgeKind::Success => (SUCCESS, SUCCESS_BG, Some(crate::icons::check_circle())),
        BadgeKind::Warning => (WARNING, WARNING_BG, Some(crate::icons::warning())),
        BadgeKind::Danger => (DANGER, DANGER_BG, Some(crate::icons::error())),
        // A clock would read as waiting; work in progress needs no icon.
        BadgeKind::Progress => (ACCENT, SECONDARY_CONTAINER, None),
        // Neutral is a plain text chip without a status icon, on the
        // highest surface container: a step above the tile in light and dark
        // palettes alike (INSET sinks below the surface in dark ones).
        BadgeKind::Neutral => (GRAY, SURFACE_HIGH, None),
        BadgeKind::Tonal => (ON_SECONDARY_CONTAINER, SECONDARY_CONTAINER, None),
    };
    let mut view = div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap(rems(4. / 14.))
        .min_h(rems(24. / 14.))
        .px(rems(10. / 14.))
        .py(rems(3. / 14.))
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

/// A fact beside a label in the badge's size, outlined rather than filled:
/// e.g. the local engine on its tile.
pub fn outlined_badge(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
) -> gpui::Stateful<gpui::Div> {
    use gpui::prelude::*;
    accessible_text(id, label)
        .flex_shrink_0()
        .px(rems(10. / 14.))
        .py(rems(4. / 14.))
        .rounded(RADIUS_SMALL)
        .border_1()
        .border_color(color(HAIRLINE))
        .text_size(TEXT_AUX)
        .line_height(rems(16. / 14.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(color(GRAY))
        .whitespace_nowrap()
}

/// M3 assist chip geometry: 32px tall with 8px corners, 8px before a leading
/// icon and 12px otherwise. Callers give it their tone and label size.
pub fn chip(leading_icon: bool) -> gpui::Div {
    use gpui::prelude::*;
    gpui_kit::component::h_flex()
        .flex_shrink_0()
        .h(rems(32. / 14.))
        .pl(rems(if leading_icon { 8. } else { 12. } / 14.))
        .pr(rems(12. / 14.))
        .gap(px(6.))
        .items_center()
        .rounded(RADIUS_SMALL)
        .whitespace_nowrap()
}

/// What follows the pointer while a note, folder or queued task is dragged:
/// its icon and name on a raised surface.
pub fn drag_chip(icon: gpui_kit::component::Icon, label: gpui::SharedString) -> gpui::Div {
    use gpui::prelude::*;
    gpui_kit::component::h_flex()
        .max_w(rems(24.))
        .gap_2()
        .px_3()
        .py_2()
        .rounded(RADIUS_SMALL)
        .bg(color(SURFACE))
        .shadow(shadow_popover())
        .text_size(TEXT_BODY)
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(icon.size(ICON_SLOT).text_color(color(GRAY)))
        .child(gpui::div().min_w_0().truncate().child(label))
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

/// Every preference switch. Becomes `MaterialSwitch` in the Material phase; Kit's
/// switch has no 52x32 track or thumb icons.
pub fn preference_switch(
    switch: gpui_kit::component::switch::Switch,
) -> gpui_kit::component::switch::Switch {
    switch.color(color(ACCENT))
}

/// Switches in the processing panel's preference rows.
pub fn processing_switch(
    switch: gpui_kit::component::switch::Switch,
) -> gpui_kit::component::switch::Switch {
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
