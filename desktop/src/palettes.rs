//! The kit's palettes and the serializable, independent light/dark preference,
//! under the names the app's settings already use.
pub use gpui_kit_material::color::palette::ALL;
pub use gpui_kit_material::color::{Appearance, PaletteId, ThemePreferences};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_preferences_keep_their_form() {
        let saved = r#"{"mode":"dark","light":"catppuccin_latte","dark":"tokyo_night"}"#;
        let parsed: ThemePreferences = serde_json::from_str(saved).unwrap();
        assert_eq!(parsed.mode, Appearance::Dark);
        assert_eq!(parsed.resolve(false), PaletteId::TokyoNight);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), saved);
        assert!(serde_json::from_str::<PaletteId>("\"unknown_theme\"").is_err());
        assert_eq!(ALL.len(), 10);
    }
}
