//! Interface languages and the shared message catalog.
//!
//! The catalog is keyed by the Simplified Chinese source text, so existing call
//! sites stay readable and a missing translation degrades to the source instead
//! of hiding the message. Bilingual engine strings are catalogued as a whole:
//! the `zh` and `en` columns split them into their two halves, while
//! Traditional Chinese and Japanese get their own translations.
//!
//! Machine-readable output (`--json`, `run-task`) is not localized: the desktop
//! pins the protocol language so field meanings and messages stay stable.
//! Human output (help, progress, errors) follows the selected language.
//!
//! Call sites use the [`tr!`] macro: a bare literal becomes a translated
//! `&str`, and extra arguments fill `{}` placeholders in the translated text.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

pub const LANGUAGES: [Lang; 4] = [Lang::En, Lang::ZhHans, Lang::ZhHant, Lang::Ja];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    ZhHans,
    ZhHant,
    Ja,
}

impl Lang {
    /// Stable BCP-47 code; also the value stored in desktop preferences.
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::Ja => "ja",
        }
    }

    /// The language's own name, used by the interface-language selector.
    pub fn native_name(self) -> &'static str {
        crate::language::native_name(self.code())
    }

    pub fn from_code(value: &str) -> Option<Self> {
        let value = value.trim().replace('_', "-").to_ascii_lowercase();
        if value.is_empty() || value == "auto" {
            return None;
        }
        let value = value.split(['.', '@']).next().unwrap_or_default();
        if value.starts_with("zh") || value.starts_with("cmn") || value.starts_with("yue") {
            // An explicit script decides; the region is only a default for tags without one.
            let mut subtags = value.split('-').skip(1);
            let traditional = subtags.clone().any(|tag| tag == "hant")
                || (!subtags.clone().any(|tag| tag == "hans")
                    && subtags.any(|tag| matches!(tag, "tw" | "hk" | "mo")));
            return Some(if traditional {
                Self::ZhHant
            } else {
                Self::ZhHans
            });
        }
        if value.starts_with("ja") {
            return Some(Self::Ja);
        }
        if value.starts_with("en") {
            return Some(Self::En);
        }
        None
    }

    /// `COURSE2MD_LANG`, then the POSIX locale variables, then the platform locale.
    pub fn detect() -> Self {
        Self::detect_from(
            |name| std::env::var(name).ok(),
            sys_locale::get_locale().as_deref(),
        )
    }

    fn detect_from(env: impl Fn(&str) -> Option<String>, system: Option<&str>) -> Self {
        for name in ["COURSE2MD_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            let Some(value) = env(name) else { continue };
            if let Some(lang) = Self::from_code(&value) {
                return lang;
            }
            // The C locale asks for untranslated output: it is a choice, not an
            // absence of one, so it does not fall through to a lower-priority source.
            if name != "COURSE2MD_LANG" && is_c_locale(&value) {
                return Self::En;
            }
        }
        system.and_then(Self::from_code).unwrap_or(Self::En)
    }

    fn index(self) -> u8 {
        match self {
            Self::En => 0,
            Self::ZhHans => 1,
            Self::ZhHant => 2,
            Self::Ja => 3,
        }
    }

    fn from_index(index: u8) -> Self {
        match index {
            0 => Self::En,
            2 => Self::ZhHant,
            3 => Self::Ja,
            _ => Self::ZhHans,
        }
    }
}

fn is_c_locale(value: &str) -> bool {
    let name = value.trim().split(['.', '@']).next().unwrap_or_default();
    name.eq_ignore_ascii_case("C") || name.eq_ignore_ascii_case("POSIX")
}

// Library callers (tests, embedded use) keep the source language until an
// application boundary selects the user's language.
static CURRENT: AtomicU8 = AtomicU8::new(1);

/// Selects the language for subsequent [`t`] lookups.
pub fn set_lang(lang: Lang) {
    CURRENT.store(lang.index(), Ordering::Relaxed);
}

pub fn lang() -> Lang {
    Lang::from_index(CURRENT.load(Ordering::Relaxed))
}

/// Selects the language from `COURSE2MD_LANG` or the operating system locale.
pub fn init_from_env() -> Lang {
    let lang = Lang::detect();
    set_lang(lang);
    lang
}

const CATALOG: &str = include_str!("../i18n/catalog.tsv");

struct Entry {
    zh: &'static str,
    en: &'static str,
    zh_hant: &'static str,
    ja: &'static str,
}

impl Entry {
    fn text(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.en,
            Lang::ZhHans => self.zh,
            Lang::ZhHant => self.zh_hant,
            Lang::Ja => self.ja,
        }
    }
}

fn catalog() -> &'static HashMap<&'static str, Entry> {
    static CATALOG_INDEX: OnceLock<HashMap<&'static str, Entry>> = OnceLock::new();
    CATALOG_INDEX.get_or_init(|| parse_catalog(CATALOG))
}

fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn parse_catalog(source: &'static str) -> HashMap<&'static str, Entry> {
    let mut entries = HashMap::new();
    // The first line is the column header; every other line has five columns.
    // There are no comment lines: a key may begin with `#`.
    for line in source.lines().skip(1) {
        if line.is_empty() {
            continue;
        }
        let fields = split_fields(line);
        if fields.len() != 5 {
            continue;
        }
        let key = leak(unescape(fields[0]));
        entries.insert(
            key,
            Entry {
                zh: leak(unescape(fields[1])),
                en: leak(unescape(fields[2])),
                zh_hant: leak(unescape(fields[3])),
                ja: leak(unescape(fields[4])),
            },
        );
    }
    entries
}

fn split_fields(line: &str) -> Vec<&str> {
    // Escaped tabs are `\t` in the file, so a literal tab is always a separator.
    line.split('\t').collect()
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Ok(code) = u32::from_str_radix(&hex, 16)
                    && let Some(c) = char::from_u32(code)
                {
                    out.push(c);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Left and right halves of the historical bilingual engine messages.
fn bilingual_halves(source: &str) -> Option<(&str, &str)> {
    for index in source.match_indices(" / ").map(|(index, _)| index) {
        let (left, right) = (&source[..index], &source[index + 3..]);
        if contains_cjk(left) && !contains_cjk(right) {
            return Some((left.trim(), right.trim()));
        }
    }
    None
}

fn contains_cjk(value: &str) -> bool {
    value
        .chars()
        .any(|c| matches!(c as u32, 0x3400..=0x9fff | 0xf900..=0xfaff))
}

fn t_in(source: &str, lang: Lang) -> &str {
    if let Some(entry) = catalog().get(source) {
        return entry.text(lang);
    }
    match lang {
        Lang::ZhHans | Lang::ZhHant => bilingual_halves(source).map_or(source, |(zh, _)| zh),
        Lang::En | Lang::Ja => bilingual_halves(source).map_or(source, |(_, en)| en),
    }
}

/// Translates a source message into the current interface language.
///
/// Missing entries keep the source text, so an incomplete catalog never
/// makes a message unreadable.
pub fn t(source: &str) -> &str {
    t_in(source, lang())
}

/// Translates a message and fills its `{}` placeholders.
///
/// ```ignore
/// tr!("共 {} 项", count)
/// ```
#[macro_export]
macro_rules! tr {
    ($text:expr $(,)?) => {
        $crate::i18n::t($text)
    };
    ($text:expr, $($arg:expr),+ $(,)?) => {{
        // Build the slice inside the call so temporaries live until formatting ends.
        $crate::i18n::template($text, &[$(&$arg as &dyn ::std::fmt::Display),+])
    }};
}

/// Localized `println!`; placeholders use the same subset as [`tr!`].
#[macro_export]
macro_rules! println_tr {
    ($($arg:tt)*) => {
        println!("{}", $crate::tr!($($arg)*))
    };
}

/// Localized `eprintln!`.
#[macro_export]
macro_rules! eprintln_tr {
    ($($arg:tt)*) => {
        eprintln!("{}", $crate::tr!($($arg)*))
    };
}

/// Localized `print!`.
#[macro_export]
macro_rules! print_tr {
    ($($arg:tt)*) => {
        print!("{}", $crate::tr!($($arg)*))
    };
}

/// Localized `eprint!`.
#[macro_export]
macro_rules! eprint_tr {
    ($($arg:tt)*) => {
        eprint!("{}", $crate::tr!($($arg)*))
    };
}

/// Fills `{}` placeholders in an already translated template.
///
/// `{{` and `}}` are literal braces, and so is a brace outside `{}`. A missing
/// translation keeps the source template; placeholders are consumed left to right.
pub fn template(source: &str, args: &[&dyn Display]) -> String {
    let template = t(source);
    let mut out = String::with_capacity(template.len());
    let mut args = args.iter();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            // Only the plain `{}` form is a placeholder; any other brace is text
            // and consumes no argument.
            '{' if chars.peek() == Some(&'}') => {
                chars.next();
                if let Some(arg) = args.next() {
                    out.push_str(&arg.to_string());
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Looks up a source string without consulting the global language.
/// Used by tests and by callers that resolve a language per document.
pub fn t_for(source: &str, lang: Lang) -> &str {
    t_in(source, lang)
}

/// Catalog coverage for a source string (diagnostics and tests).
pub fn has_translation(source: &str) -> bool {
    catalog().contains_key(source)
}

/// Every catalog key as `tr!` sees it, for tooling that audits coverage.
pub fn catalog_keys() -> impl Iterator<Item = &'static str> {
    catalog().keys().copied()
}

/// Rendered translation table used by tooling that audits coverage.
pub fn catalog_len() -> usize {
    catalog().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes_round_trip() {
        for lang in LANGUAGES {
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(Lang::from_code("zh_CN.UTF-8"), Some(Lang::ZhHans));
        assert_eq!(Lang::from_code("zh_TW.UTF-8"), Some(Lang::ZhHant));
        assert_eq!(Lang::from_code("zh-HK"), Some(Lang::ZhHant));
        assert_eq!(Lang::from_code("ja_JP"), Some(Lang::Ja));
        assert_eq!(Lang::from_code("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::from_code("C"), None);
        assert_eq!(Lang::from_code("auto"), None);
    }

    #[test]
    fn an_explicit_script_outranks_the_region() {
        for code in ["zh-Hans-HK", "zh-Hans-TW", "zh_Hans_MO", "cmn-Hans-TW"] {
            assert_eq!(Lang::from_code(code), Some(Lang::ZhHans), "{code}");
        }
        for code in [
            "zh-Hant-CN",
            "zh-Hant",
            "zh-HK",
            "zh-MO",
            "zh_TW.Big5",
            "yue-Hant",
        ] {
            assert_eq!(Lang::from_code(code), Some(Lang::ZhHant), "{code}");
        }
        assert_eq!(Lang::from_code("zh-CN"), Some(Lang::ZhHans));
    }

    fn detect(vars: &[(&str, &str)], system: Option<&str>) -> Lang {
        Lang::detect_from(
            |name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            },
            system,
        )
    }

    #[test]
    fn the_c_locale_selects_untranslated_output() {
        for value in ["C", "POSIX", "C.UTF-8", "c"] {
            let vars = [("LC_ALL", value), ("LANG", "zh_CN.UTF-8")];
            assert_eq!(detect(&vars, Some("ja-JP")), Lang::En, "{value}");
        }
        assert_eq!(detect(&[("LANG", "C")], Some("zh-CN")), Lang::En);
        // An explicit language outranks it, and an empty variable is unset.
        let vars = [("COURSE2MD_LANG", "ja"), ("LC_ALL", "C")];
        assert_eq!(detect(&vars, None), Lang::Ja);
        let vars = [("LC_ALL", ""), ("LANG", "zh_TW.UTF-8")];
        assert_eq!(detect(&vars, None), Lang::ZhHant);
        assert_eq!(detect(&[], Some("zh-Hans-HK")), Lang::ZhHans);
        // A language without a catalog keeps looking for one that has it.
        let vars = [("LC_ALL", "fr_FR.UTF-8"), ("LANG", "ja_JP.UTF-8")];
        assert_eq!(detect(&vars, None), Lang::Ja);
    }

    #[test]
    fn bilingual_messages_split_into_halves() {
        let source = "读取失败 / Failed to read";
        assert_eq!(t_for(source, Lang::ZhHans), "读取失败");
        assert_eq!(t_for(source, Lang::En), "Failed to read");
        // A plain slash-separated list is not a bilingual message.
        let list = "YouTube / Bilibili";
        assert_eq!(t_for(list, Lang::ZhHans), list);
        assert_eq!(t_for(list, Lang::En), list);
    }

    #[test]
    fn template_fills_placeholders() {
        // Uses the source language so the assertion is independent of the environment.
        set_lang(Lang::ZhHans);
        assert_eq!(
            template("共 {} 项，{} 项完成", &[&3, &2]),
            "共 3 项，2 项完成"
        );
        assert_eq!(template("花括号 {{}} 保留", &[]), "花括号 {} 保留");
    }

    #[test]
    fn only_the_plain_form_is_a_placeholder() {
        set_lang(Lang::ZhHans);
        assert_eq!(template("{name} 和 {}", &[&1]), "{name} 和 1");
        assert_eq!(template("{0} 末尾 {", &[&1]), "{0} 末尾 {");
    }

    #[test]
    fn rows_starting_with_a_hash_are_rows() {
        let source = "zh\tzh_hans\ten\tzh_hant\tja\n#一\t#一\t#one\t#一\t#いち\n";
        let entries = parse_catalog(source);
        assert_eq!(entries["#一"].en, "#one");
    }

    #[test]
    fn catalog_entries_are_complete_and_consistent() {
        for line in CATALOG.lines().skip(1) {
            if line.is_empty() {
                continue;
            }
            let fields = split_fields(line);
            assert_eq!(fields.len(), 5, "malformed catalog row: {line}");
            let placeholders: Vec<usize> = (1..5)
                .map(|column| fields[column].matches("{}").count())
                .collect();
            assert!(
                placeholders.iter().all(|count| *count == placeholders[0]),
                "placeholder count differs across languages: {line}"
            );
            assert!(
                fields.iter().skip(1).all(|field| !field.is_empty()),
                "empty translation: {line}"
            );
        }
    }

    #[test]
    fn known_messages_have_all_languages() {
        // A small sentinel set keeps the catalog from silently losing entries.
        for source in ["笔记库", "设置", "取消"] {
            assert!(has_translation(source), "missing catalog entry: {source}");
        }
    }

    #[test]
    fn escaping_round_trips() {
        let value = "第一行\n第二行\tTab \\ \"引号\"";
        assert_eq!(unescape(&escape(value)), value);
    }
}
