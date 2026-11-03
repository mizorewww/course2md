//! Desktop binding for the shared catalog and the persisted interface language.
//!
//! The core crate owns language resolution and the catalog. This module applies
//! the preference to both the catalog and gpui-component's built-in strings.

use crate::preferences::InterfaceLanguage;
use course2md::i18n::{self, Lang};

/// Applies a stored preference. Call before the first frame and after a change.
pub(crate) fn apply(language: InterfaceLanguage) {
    let lang = language.resolve();
    i18n::set_lang(lang);
    gpui_kit::component::set_locale(component_locale(lang));
}

/// Short calendar date in the interface language: "Sep 27" or "9月27日".
pub(crate) fn month_day(month: u32, day: u32) -> String {
    month_day_for(i18n::lang(), month, day)
}

fn month_day_for(lang: Lang, month: u32, day: u32) -> String {
    match lang {
        Lang::En => {
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let name = MONTHS
                .get(month.saturating_sub(1) as usize)
                .copied()
                .unwrap_or("Jan");
            format!("{name} {day}")
        }
        _ => format!("{month}月{day}日"),
    }
}

/// gpui-component ships English, Simplified and Traditional Chinese; Japanese
/// falls back to English for its built-in controls, while our catalog covers
/// the app.
fn component_locale(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        Lang::ZhHans => "zh-CN",
        Lang::ZhHant => "zh-TW",
        Lang::Ja => "ja",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::{ApplicationPreferences, InterfaceLanguage};

    #[test]
    fn interface_language_saved_names() {
        assert_eq!(
            serde_json::to_string(&InterfaceLanguage::ZhHant).unwrap(),
            "\"zh-Hant\""
        );
        assert_eq!(
            serde_json::to_string(&InterfaceLanguage::System).unwrap(),
            "\"system\""
        );
    }

    #[test]
    fn missing_language_uses_the_system_default() {
        let preferences: ApplicationPreferences =
            serde_json::from_str(r#"{"font_scale":1.0}"#).unwrap();
        assert_eq!(preferences.language, InterfaceLanguage::System);
    }

    #[test]
    fn short_dates_follow_the_interface_language() {
        assert_eq!(month_day_for(Lang::En, 9, 27), "Sep 27");
        assert_eq!(month_day_for(Lang::ZhHans, 9, 27), "9月27日");
        assert_eq!(month_day_for(Lang::ZhHant, 9, 27), "9月27日");
        assert_eq!(month_day_for(Lang::Ja, 9, 27), "9月27日");
    }
}
