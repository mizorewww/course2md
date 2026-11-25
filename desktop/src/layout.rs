use gpui::{Pixels, Rems, prelude::*, px};
use gpui_kit_material::prelude::{
    ButtonSize, DividerOrientation, ListDensity, MaterialDivider, rem,
};

pub(crate) const SHELL_GUTTER: f32 = 24.;
pub(crate) const APP_BAR_MIN_HEIGHT: Pixels = px(56.);
pub(crate) const APP_BAR_VERTICAL_SPACE: Pixels = px(16.);
pub(crate) const CONTROL_SIZE: ButtonSize = ButtonSize::Small;
pub(crate) const CONTROL_HEIGHT: Rems = rem(40.);
pub(crate) const CONTROL_SPACING: Rems = rem(12.);
pub(crate) const RELATED_SPACING: Rems = rem(8.);
pub(crate) const CONTROL_CLUSTER_SPACING: Rems = rem(4.);

pub(crate) const LIBRARY_WIDTH: f32 = 1280.;
pub(crate) const LIBRARY_SECTION_SPACING: Rems = rem(16.);
pub(crate) const SIDEBAR_WIDTH: f32 = 256.;
pub(crate) const SIDEBAR_GAP: f32 = 32.;
pub(crate) const SIDEBAR_MIN_CONTENT: f32 = 832.;
pub(crate) const SIDEBAR_DENSITY: ListDensity = ListDensity::Compact;
pub(crate) const NOTE_GRID_GAP: f32 = 12.;
pub(crate) const NOTE_CARD_INSET: Rems = rem(10.);
pub(crate) const NOTE_CARD_TEXT_INSET: Rems = rem(4.);
pub(crate) const NOTE_CARD_ACTION_OFFSET: Rems = rem(8.);

pub(crate) fn library_app_bar_divider() -> MaterialDivider {
    MaterialDivider::new("library-app-bar-divider").flex_shrink_0()
}

pub(crate) fn library_sidebar_divider() -> MaterialDivider {
    MaterialDivider::new("library-sidebar-divider")
        .orientation(DividerOrientation::Vertical)
        .absolute()
        .left(rem(SIDEBAR_WIDTH + SIDEBAR_GAP / 2.))
        .top_0()
        .bottom(LIBRARY_SECTION_SPACING)
        .h_auto()
}
