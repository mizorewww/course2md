//! Held for the design restoration: the shapes, tile geometry, tonal icons and pill field
//! of the old look, until the restoration agent confirms they are ported to the kit.
//! Only `sync_unprojected` has a caller now; see its comment.
use gpui::{App, Pixels, Rems, Rgba, Window, px, rems, rgb};
use gpui_kit::component::Theme;
use gpui_kit_material::color::ColorRole;

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
    theme.radius_lg = RADIUS_CARD;
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
                    crate::symbols::info()
                        .component()
                        .size(rems(18. / 14.))
                        .text_color(color(GRAY)),
                ),
        )
        .child(tile_support(id, text).flex_1())
}

/// The trailing chevron of a tile that opens something.
pub fn tile_chevron() -> gpui_kit::component::Icon {
    use gpui::Styled;
    crate::symbols::chevron_right()
        .component()
        .size(rems(20. / 14.))
        .flex_shrink_0()
        .text_color(color(GRAY))
}

/// The chevron of an in-place disclosure; it turns over when open.
pub fn disclosure_chevron(open: bool) -> gpui_kit::component::Icon {
    use gpui::Styled;
    if open {
        crate::symbols::keyboard_arrow_up().component()
    } else {
        crate::symbols::keyboard_arrow_down().component()
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
            let amount = gpui_kit_material::motion::selection_value(
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
            crate::symbols::search()
                .component()
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
