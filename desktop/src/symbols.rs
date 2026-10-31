//! The app's assets: the Material Symbols listed in `assets/symbols.txt`, rendered
//! at build time, and the few files that are not symbols.
//!
//! `embedded-symbols` would put every symbol's font in the binary (about 52 MB). Instead
//! `build.rs` renders the listed names and generates one function per name, so
//! adding a symbol takes one line in that file:
//!
//! ```ignore
//! // assets/symbols.txt:  bookmark
//! IconButton::new("save-note", symbols::bookmark(), course2md::tr!("收藏"));
//! MaterialButton::new("add").icon(symbols::add());
//! ListItem::new("x", label).leading(ListLeading::Icon(symbols::folder().into()));
//! ```
//!
//! A function returns a `SymbolIcon`, which converts into the kit's `Icon` wherever a
//! component takes one. A name outside the catalog fails the build; a symbol not in the
//! list does not compile. The filled variant is a separate asset: mark the line ` fill`.
use gpui::{AssetSource, SharedString};
use gpui_kit_material::icons::{Style, SymbolIcon};
use std::borrow::Cow;

include!(concat!(env!("OUT_DIR"), "/symbols.rs"));

const FILES: &[(&str, &[u8])] = &[
    (
        "images/course2md.png",
        include_bytes!("../assets/128x128@2x.png"),
    ),
    (
        "icons/bilibili.svg",
        include_bytes!("../assets/brands/bilibili.svg"),
    ),
];

const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/roboto/Roboto-Regular.ttf"),
    include_bytes!("../assets/fonts/roboto/Roboto-Medium.ttf"),
    include_bytes!("../assets/fonts/roboto/Roboto-SemiBold.ttf"),
];

/// The faces of the kit's type scale. CJK text falls back to the system's fonts.
pub fn register_fonts(cx: &mut gpui::App) {
    cx.text_system()
        .add_fonts(FONTS.map(Cow::Borrowed).to_vec())
        .expect("the bundled fonts are valid");
}

/// The source the application registers, instead of the kit's full `icons::Assets`.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with("material-symbols/") {
            let found = SYMBOLS
                .binary_search_by_key(&path, |(symbol, _)| symbol)
                .ok()
                .map(|index| Cow::Borrowed(SYMBOLS[index].1.as_bytes()));
            #[cfg(debug_assertions)]
            if found.is_none() {
                eprintln!("{path} is not embedded: add it to desktop/assets/symbols.txt");
            }
            return Ok(found);
        }
        if let Some(formula) = crate::math::asset(path) {
            return Ok(Some(formula));
        }
        Ok(FILES
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, prefix: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(SYMBOLS
            .iter()
            .map(|(path, _)| *path)
            .chain(FILES.iter().map(|(path, _)| *path))
            .filter(|path| path.starts_with(prefix))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::resolve_icon_path;
    use gpui_kit_material::icons::IconName;

    #[gpui::test]
    fn every_kit_default_icon_that_translates_is_embedded(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit_material::init(cx);
            let mut translated = 0;
            for path in gpui_kit::assets::Assets.list("icons/").unwrap() {
                let resolved = resolve_icon_path(path.clone(), cx);
                // The kit leaves brand art and crossed-out favorites untranslated.
                if resolved.starts_with("material-symbols/") {
                    translated += 1;
                    assert!(
                        Assets.load(&resolved).unwrap().is_some(),
                        "{path} draws {resolved}, which is not in assets/symbols.txt"
                    );
                }
            }
            assert!(translated > 90, "only {translated} translated icons found");
        });
    }

    #[test]
    fn the_kit_aliases_are_embedded() {
        for name in IconName::ALL {
            assert!(Assets.load(name.path()).unwrap().is_some(), "{name:?}");
        }
    }

    #[test]
    fn a_generated_function_loads_and_an_unknown_symbol_draws_nothing() {
        let path = search().asset_path();
        assert!(Assets.load(&path).unwrap().unwrap().starts_with(b"<svg"));
        assert!(Assets.load(&star().filled(true).asset_path()).unwrap().is_some());
        let absent = "material-symbols/rounded/not_embedded/0/400/0/24.svg";
        assert!(Assets.load(absent).unwrap().is_none());
    }
}
