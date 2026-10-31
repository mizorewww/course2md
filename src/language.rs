//! Translation targets and a light guess of a transcript's language.
//!
//! The guess only decides whether a bilingual note needs translating at all,
//! so it compares language families: Simplified and Traditional Chinese count
//! as the same language, and a transcript too short or mixed to judge, or in
//! another language, is translated rather than skipped.

/// Target languages offered for bilingual notes: stable code and the name
/// the AI service is asked to write in.
pub const TARGETS: [(&str, &str); 8] = [
    ("zh-Hans", "Simplified Chinese"),
    ("zh-Hant", "Traditional Chinese"),
    ("en", "English"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("fr", "French"),
    ("de", "German"),
    ("es", "Spanish"),
];

/// The supported target matching `code` (case and `_`/`-` insensitive).
pub fn target(code: &str) -> Option<&'static str> {
    let wanted = code.trim().replace('_', "-").to_ascii_lowercase();
    TARGETS
        .iter()
        .map(|(code, _)| *code)
        .find(|code| code.to_ascii_lowercase() == wanted)
}

/// The English name used in the translation instruction.
pub fn english_name(code: &str) -> &'static str {
    TARGETS
        .iter()
        .find(|(target, _)| *target == code)
        .map(|(_, name)| *name)
        .unwrap_or("English")
}

/// The language's own name, for menus.
pub fn native_name(code: &str) -> &'static str {
    match code {
        "zh-Hans" => "简体中文",
        "zh-Hant" => "繁體中文",
        "en" => "English",
        "ja" => "日本語",
        "ko" => "한국어",
        "fr" => "Français",
        "de" => "Deutsch",
        "es" => "Español",
        _ => "English",
    }
}

/// Language family of a target code: `zh-Hans` and `zh-Hant` are both `zh`.
fn family(code: &str) -> &str {
    code.split('-').next().unwrap_or(code)
}

/// Whether text that `detect` judged to be `detected` is already in `target`.
pub fn already_in(detected: Option<&str>, target: &str) -> bool {
    detected.is_some_and(|detected| family(detected) == family(target))
}

/// Minimum letters before a guess is trusted.
const MIN_LETTERS: usize = 40;

/// Language family of `text` (`zh`, `ja`, `ko`, `en`, `fr`, `de`, `es`), or
/// `None` when the sample is too small or no language clearly dominates.
pub fn detect(text: &str) -> Option<&'static str> {
    use whatlang::{Lang, Script};
    if text.chars().filter(|c| c.is_alphabetic()).count() < MIN_LETTERS {
        return None;
    }
    // Kanji-heavy Japanese is told from Chinese with low confidence, but correctly.
    let info = whatlang::detect(text)
        .filter(|info| info.is_reliable() || info.script() == Script::Mandarin)?;
    Some(match info.lang() {
        Lang::Cmn => "zh",
        Lang::Jpn => "ja",
        Lang::Kor => "ko",
        Lang::Eng => "en",
        Lang::Fra => "fr",
        Lang::Deu => "de",
        Lang::Spa => "es",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_and_function_words_identify_the_language() {
        let chinese = "今天我们来讲一下线性代数里面最重要的概念，也就是矩阵的特征值和特征向量。";
        assert_eq!(detect(&chinese.repeat(2)), Some("zh"));
        let japanese = "今日は線形代数の中で最も重要な概念、つまり行列の固有値と固有ベクトルについて説明します。";
        assert_eq!(detect(&japanese.repeat(2)), Some("ja"));
        // Mostly kanji with only a little kana is still Japanese.
        let kanji_heavy = "線形代数学基礎講義第三回固有値問題解法概要説明行列対角化応用事例紹介以上。\
                           今回の講義内容は期末試験範囲です。";
        assert_eq!(detect(&kanji_heavy.repeat(2)), Some("ja"));
        let korean = "오늘은 선형대수에서 가장 중요한 개념인 행렬의 고유값과 고유벡터에 대해 설명하겠습니다.";
        assert_eq!(detect(&korean.repeat(2)), Some("ko"));
        let english = "So today we are going to talk about the most important idea in linear algebra, \
                       and that is what we call the eigenvalues of a matrix. It is the key to this course.";
        assert_eq!(detect(english), Some("en"));
        let french = "Aujourd'hui nous allons parler de la notion la plus importante dans les cours \
                      d'algèbre, et c'est une idée que vous devez comprendre pour la suite des cours.";
        assert_eq!(detect(french), Some("fr"));
        let german = "Heute sprechen wir über das wichtigste Konzept der linearen Algebra, und das ist \
                      die Idee, die wir in der Vorlesung mit den Eigenwerten einer Matrix beschreiben.";
        assert_eq!(detect(german), Some("de"));
        assert_eq!(detect("short"), None);
    }

    #[test]
    fn chinese_variants_are_one_language() {
        assert!(already_in(Some("zh"), "zh-Hant"));
        assert!(already_in(Some("en"), "en"));
        assert!(!already_in(Some("en"), "zh-Hans"));
        assert!(!already_in(None, "en"));
    }

    #[test]
    fn targets_parse_loosely() {
        assert_eq!(target("zh_hans"), Some("zh-Hans"));
        assert_eq!(target(" EN "), Some("en"));
        assert_eq!(target("xx"), None);
        assert_eq!(english_name("ja"), "Japanese");
    }
}
