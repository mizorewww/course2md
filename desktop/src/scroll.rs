//! Scrollbars take their policy from Kit's theme, which `Theme::sync_scrollbar_appearance`
//! keeps with the system.
use gpui_kit::component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarMode};

pub(crate) fn vertical_scrollbar(handle: &gpui::ScrollHandle) -> Scrollbar {
    Scrollbar::vertical(handle)
}

/// A horizontal scrollbar, as on a formula wider than its line: `overflowing` content
/// keeps it showing, so a person can see the line goes on.
pub(crate) fn horizontal_scrollbar(handle: &gpui::ScrollHandle, overflowing: bool) -> Scrollbar {
    let scrollbar = Scrollbar::horizontal(handle);
    if overflowing {
        scrollbar.mode(ScrollbarMode::Always)
    } else {
        scrollbar
    }
}

/// For any scrollable handle, including a virtualized list's state.
pub(crate) fn vertical_scrollbar_for<H: ScrollbarHandle + Clone>(handle: &H) -> Scrollbar {
    Scrollbar::vertical(handle)
}
