//! The compositions the desktop's pages share on `gpui_kit_material`: rows and fields,
//! embedded symbols and fonts, note text and formulas. No page state lives here, and
//! nothing here knows the app's task model.

// GPUI's macro output and this crate name its types through `gpui`; the Kit selects
// the engine behind that name, as gpui-kit-material does.
extern crate gpui_kit as gpui;

pub mod math;
pub mod note_text;
pub mod symbols;
pub mod ui;
