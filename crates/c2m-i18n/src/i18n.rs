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

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) const LANGUAGES: [Lang; 4] = [Lang::En, Lang::ZhHans, Lang::ZhHant, Lang::Ja];

/// In the order of [`LANGUAGES`].
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
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    LANGUAGES[CURRENT.load(Ordering::Relaxed) as usize]
}

// The person's own language, apart from the message language: machine output
// (`--json`, `run-task`) pins messages to the protocol language, while choices
// made for the person, such as which subtitle track to read, still follow them.
// Unset, it is the message language.
static PERSON: AtomicU8 = AtomicU8::new(u8::MAX);

/// Records the person's language before messages are pinned to the protocol's.
pub fn set_person_lang(lang: Lang) {
    PERSON.store(lang as u8, Ordering::Relaxed);
}

/// The language content choices follow: the person's, even while messages stay
/// in the protocol language.
pub fn person_lang() -> Lang {
    LANGUAGES
        .get(PERSON.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or_else(lang)
}

/// Selects the language from `COURSE2MD_LANG` or the operating system locale.
pub fn init_from_env() -> Lang {
    let lang = Lang::detect();
    set_lang(lang);
    lang
}

const CATALOG: &str = include_str!("../../../i18n/catalog.tsv");

/// A row's texts, in the order of [`LANGUAGES`].
type Entry = [&'static str; 4];

fn catalog() -> &'static HashMap<&'static str, Entry> {
    static CATALOG_INDEX: OnceLock<HashMap<&'static str, Entry>> = OnceLock::new();
    CATALOG_INDEX.get_or_init(|| parse_catalog(CATALOG))
}

fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn parse_catalog(source: &'static str) -> HashMap<&'static str, Entry> {
    // The first line is the column header; every other line has five columns.
    // There are no comment lines: a key may begin with `#`. Escaped tabs are `\t`
    // in the file, so a literal tab is always a separator.
    let mut entries = HashMap::new();
    for line in source.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if let [key, zh, en, zh_hant, ja] = fields[..] {
            let text = |field| leak(unescape(field));
            entries.insert(text(key), [text(en), text(zh), text(zh_hant), text(ja)]);
        }
    }
    entries
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
        return entry[lang as usize];
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

/// Rows by their Simplified Chinese text, for text a machine consumer already
/// received in that language (the worker's `run-task` output).
fn by_simplified() -> &'static HashMap<&'static str, Entry> {
    static INDEX: OnceLock<HashMap<&'static str, Entry>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = HashMap::new();
        for entry in catalog().values() {
            index.entry(entry[Lang::ZhHans as usize]).or_insert(*entry);
        }
        index
    })
}

/// Rows whose Simplified Chinese text has `{}` values, split at them, longest literal
/// text first: "校对 00:00–00:17 的文字" is a row with its values filled in.
fn simplified_templates() -> &'static [(Vec<&'static str>, Entry)] {
    static INDEX: OnceLock<Vec<(Vec<&'static str>, Entry)>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut templates: Vec<(Vec<&'static str>, Entry)> = by_simplified()
            .iter()
            .filter(|(zh, _)| zh.contains("{}") && !zh.contains("{{"))
            .map(|(zh, entry)| (zh.split("{}").collect::<Vec<_>>(), *entry))
            // A template of little more than its values would match any text.
            .filter(|(pieces, _)| {
                let literal = pieces.concat();
                literal.chars().count() >= 2 && contains_cjk(&literal)
            })
            .collect();
        templates.sort_by(|(a, _), (b, _)| {
            let len = |pieces: &[&str]| pieces.iter().map(|piece| piece.len()).sum::<usize>();
            len(b).cmp(&len(a)).then_with(|| a.cmp(b))
        });
        templates
    })
}

/// The values in `text` when it is `pieces` joined around them.
fn match_template<'a>(pieces: &[&str], text: &'a str) -> Option<Vec<&'a str>> {
    let (first, rest) = pieces.split_first()?;
    let mut remaining = text.strip_prefix(first)?;
    let mut values = Vec::with_capacity(rest.len());
    for (index, piece) in rest.iter().enumerate() {
        if index + 1 == rest.len() {
            values.push(remaining.strip_suffix(piece)?);
            remaining = "";
        } else {
            if piece.is_empty() {
                return None;
            }
            let at = remaining.find(piece)?;
            values.push(&remaining[..at]);
            remaining = &remaining[at + piece.len()..];
        }
    }
    remaining.is_empty().then_some(values)
}

/// One part of an engine message in `lang`, if the catalog (a row, or a row with its
/// values filled in) or the part's own bilingual halves know it.
fn engine_part(part: &str, lang: Lang, depth: u8) -> Option<Cow<'static, str>> {
    if let Some(entry) = catalog().get(part) {
        return Some(Cow::Borrowed(entry[lang as usize]));
    }
    if let Some(entry) = by_simplified().get(part) {
        return Some(Cow::Borrowed(entry[lang as usize]));
    }
    if depth < 2 {
        for (pieces, entry) in simplified_templates() {
            if let Some(values) = match_template(pieces, part) {
                // Each value is itself an engine text ("错误：{}" wraps another).
                let values: Vec<String> = values
                    .into_iter()
                    .map(|value| engine_message_in(value, lang, depth + 1))
                    .collect();
                let mut out = String::new();
                let mut values = values.iter();
                let mut literal = entry[lang as usize].split("{}");
                out.push_str(literal.next().unwrap_or_default());
                for piece in literal {
                    out.push_str(values.next().map_or("", String::as_str));
                    out.push_str(piece);
                }
                return Some(Cow::Owned(out));
            }
        }
    }
    bilingual_halves(part).map(|(zh, en)| {
        Cow::Owned(
            match lang {
                Lang::ZhHans | Lang::ZhHant => zh,
                Lang::En | Lang::Ja => en,
            }
            .to_owned(),
        )
    })
}

/// Shows a message the engine wrote for machine consumers (Simplified Chinese, or
/// the older "中文 / English" form) in `lang`. An error chain joins its parts with
/// ": "; each known part is translated, the longest first, and the rest (a tool's
/// or the system's own words) is kept as written.
pub fn engine_message_for(text: &str, lang: Lang) -> String {
    engine_message_in(text, lang, 0)
}

fn engine_message_in(text: &str, lang: Lang, depth: u8) -> String {
    let bounds: Vec<usize> = text
        .match_indices(": ")
        .map(|(index, _)| index)
        .chain([text.len()])
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    while start < text.len() {
        let found = bounds
            .iter()
            .rev()
            .filter(|end| **end > start)
            .find_map(|end| engine_part(&text[start..*end], lang, depth).map(|part| (*end, part)));
        let end = match found {
            Some((end, part)) => {
                out.push_str(&part);
                end
            }
            None => {
                let end = bounds
                    .iter()
                    .copied()
                    .find(|end| *end > start)
                    .unwrap_or(text.len());
                out.push_str(&text[start..end]);
                end
            }
        };
        if end < text.len() {
            out.push_str(": ");
        }
        start = end + 2;
    }
    out
}

/// [`engine_message_for`] in the current interface language.
pub fn engine_message(text: &str) -> String {
    engine_message_for(text, lang())
}

/// Looks up a source string without consulting the global language.
/// Used by tests and by callers that resolve a language per document.
pub fn t_for(source: &str, lang: Lang) -> &str {
    t_in(source, lang)
}

/// Every catalog key as `tr!` sees it, for tooling that audits coverage.
pub fn catalog_keys() -> impl Iterator<Item = &'static str> {
    catalog().keys().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker writes Simplified Chinese; the desktop shows it in its own language.
    #[test]
    fn engine_text_with_values_reads_in_the_interface_language() {
        assert_eq!(
            engine_message_for("此笔记版本需要其他版本的软件", Lang::En),
            "Unsupported manifest schema"
        );
        assert_eq!(
            engine_message_for("校对 00:00–00:17 的文字", Lang::En),
            "Proofread transcript 00:00–00:17"
        );
        assert_eq!(
            engine_message_for("校对 00:00–00:17 的文字", Lang::ZhHant),
            "校對 00:00–00:17 的文字"
        );
        assert_eq!(
            engine_message_for(
                "错误：Apple 模型下载失败 / Apple model download failed (qwen3-1.7b): denied",
                Lang::En
            ),
            "Error: Apple model download failed (qwen3-1.7b): denied"
        );
        // A bilingual value no row names gives the half the reader reads.
        let raw = "错误：Apple 模型下载失败 / Apple model download failed (x): denied";
        assert_eq!(
            engine_message_for(raw, Lang::ZhHant),
            "錯誤：Apple 模型下载失败"
        );
        // Text the catalog does not know stays as it is.
        assert_eq!(
            engine_message_for("EOF while parsing", Lang::En),
            "EOF while parsing"
        );
        assert_eq!(engine_message_for("", Lang::En), "");
    }

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
    fn an_engine_message_reads_in_the_interface_language() {
        let message = "启动 yt-dlp 失败: No such file or directory (os error 2)";
        assert_eq!(
            engine_message_for(message, Lang::En),
            "Failed to start yt-dlp: No such file or directory (os error 2)"
        );
        assert_eq!(
            engine_message_for(message, Lang::Ja),
            "yt-dlp の起動に失敗しました: No such file or directory (os error 2)"
        );
        assert_eq!(engine_message_for(message, Lang::ZhHans), message);
        assert_eq!(
            engine_message_for(message, Lang::ZhHant),
            "啟動 yt-dlp 失敗: No such file or directory (os error 2)"
        );
        // An older bilingual part keeps its English half whole, ": " and all.
        let older = "提示：稍后重试。 / Hint: retry later.: 启动 yt-dlp 失败: gone";
        assert_eq!(
            engine_message_for(older, Lang::En),
            "Hint: retry later.: Failed to start yt-dlp: gone"
        );
        assert_eq!(
            engine_message_for(older, Lang::ZhHans),
            "提示：稍后重试。: 启动 yt-dlp 失败: gone"
        );
        // Unknown words stay as written.
        assert_eq!(
            engine_message_for("yt-dlp: [x] gone", Lang::Ja),
            "yt-dlp: [x] gone"
        );
        assert_eq!(engine_message_for("", Lang::En), "");
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
        assert_eq!(entries["#一"][Lang::En as usize], "#one");
    }

    #[test]
    fn catalog_entries_are_complete_and_consistent() {
        for line in CATALOG.lines().skip(1) {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
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
            assert!(
                catalog().contains_key(source),
                "missing catalog entry: {source}"
            );
        }
    }

    #[test]
    fn escaping_round_trips() {
        let value = "第一行\n第二行\tTab \\ \"引号\"";
        assert_eq!(unescape(&escape(value)), value);
    }
}
