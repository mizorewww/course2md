//! Translation targets and a light guess of a transcript's language.
//!
//! Built-in targets have localized menu labels, while CLI/config callers may
//! also pass any custom language code or name. Simplified and Traditional
//! Chinese are treated as distinct targets so users can convert between them.

/// Target languages offered in desktop menus for bilingual notes: stable code
/// and the name the AI service is asked to write in.
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

/// The built-in target matching `code` (case and `_`/`-` insensitive, or by name).
pub fn target(code: &str) -> Option<&'static str> {
    let wanted = code.trim().replace('_', "-").to_ascii_lowercase();
    TARGETS.iter().find_map(|(canonical, english)| {
        (canonical.to_ascii_lowercase() == wanted
            || english.to_ascii_lowercase() == wanted
            || native_name(canonical).to_lowercase() == code.trim().to_lowercase())
        .then_some(*canonical)
    })
}

/// Normalizes a target language: returns the canonical built-in code when matched,
/// or keeps a valid custom language code/name (such as `pt`, `ru`, `Italian`).
pub fn normalize_target(code: &str) -> Option<String> {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 64 || trimmed.chars().any(char::is_control) {
        return None;
    }
    Some(target(trimmed).unwrap_or(trimmed).to_owned())
}

/// The name used in the translation instruction: a built-in English name when
/// known, or the caller's custom target name/code verbatim.
pub fn english_name(code: &str) -> &str {
    if let Some(canonical) = target(code)
        && let Some((_, name)) = TARGETS.iter().find(|(t, _)| *t == canonical)
    {
        return name;
    }
    code.trim()
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

fn family(code: &str) -> &str {
    code.split(['-', '_']).next().unwrap_or(code)
}

/// Whether text that `detect` judged to be `detected` is already in `target`.
/// Simplified (`zh-Hans`) and Traditional (`zh-Hant`) Chinese remain distinct so
/// converting between them is never skipped.
pub fn already_in(detected: Option<&str>, target_lang: &str) -> bool {
    let Some(detected) = detected else {
        return false;
    };
    let canonical = target(target_lang).unwrap_or_else(|| target_lang.trim());
    if matches!(canonical, "zh-Hans" | "zh-Hant") {
        return detected.eq_ignore_ascii_case(canonical);
    }
    family(detected).eq_ignore_ascii_case(family(canonical))
}

/// Minimum letters before a guess is trusted.
const MIN_LETTERS: usize = 40;

const HANS_ONLY: &str = "这说们会学对时个国发经动体点开关实长见进种题应现机无问变线阵导语译页记识课讲节图视讯转择标览设网连误试载录锁难响显验矩阵特征";
const HANT_ONLY: &str = "這說們會學對時個國發經動體點開關實長見進種題應現機無問變線陣導語譯頁記識課講節圖視訊轉擇標覽設網連誤試載錄鎖難響顯驗矩陣特徵";

fn detect_chinese_script(text: &str) -> &'static str {
    let mut hans = 0usize;
    let mut hant = 0usize;
    for ch in text.chars() {
        if HANS_ONLY.contains(ch) {
            hans += 1;
        } else if HANT_ONLY.contains(ch) {
            hant += 1;
        }
    }
    if hant > hans {
        "zh-Hant"
    } else if hans > hant {
        "zh-Hans"
    } else {
        "zh"
    }
}

/// Language code of `text` (`zh-Hans`, `zh-Hant`, `zh`, `ja`, `ko`, `en`, `fr`,
/// `de`, `es`), or `None` when the sample is too small or no language dominates.
pub fn detect(text: &str) -> Option<&'static str> {
    use whatlang::{Lang, Script};
    if text.chars().filter(|c| c.is_alphabetic()).count() < MIN_LETTERS {
        return None;
    }
    // Kanji-heavy Japanese is told from Chinese with low confidence, but correctly.
    let info = whatlang::detect(text)
        .filter(|info| info.is_reliable() || info.script() == Script::Mandarin)?;
    Some(match info.lang() {
        Lang::Cmn => detect_chinese_script(text),
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
        let simplified = "今天我们来讲一下线性代数里面最重要的概念，也就是矩阵的特征值和特征向量。";
        assert_eq!(detect(&simplified.repeat(2)), Some("zh-Hans"));
        let traditional =
            "今天我們來講一下線性代數裡面最重要的概念，也就是矩陣的特徵值和特徵向量。";
        assert_eq!(detect(&traditional.repeat(2)), Some("zh-Hant"));
        let japanese = "今日は線形代数の中で最も重要な概念、つまり行列の固有値と固有ベクトルについて説明します。";
        assert_eq!(detect(&japanese.repeat(2)), Some("ja"));
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
    fn chinese_variants_remain_distinct_for_conversion() {
        assert!(!already_in(Some("zh-Hans"), "zh-Hant"));
        assert!(!already_in(Some("zh-Hant"), "zh-Hans"));
        assert!(already_in(Some("zh-Hans"), "zh-Hans"));
        assert!(already_in(Some("zh-Hant"), "zh-Hant"));
        assert!(already_in(Some("en"), "en"));
        assert!(!already_in(Some("en"), "zh-Hans"));
        assert!(!already_in(None, "en"));
    }

    #[test]
    fn targets_parse_built_in_and_custom_languages() {
        assert_eq!(target("zh_hans"), Some("zh-Hans"));
        assert_eq!(target(" EN "), Some("en"));
        assert_eq!(target("Italian"), None);
        assert_eq!(normalize_target("zh_hant").as_deref(), Some("zh-Hant"));
        assert_eq!(normalize_target(" Italian ").as_deref(), Some("Italian"));
        assert_eq!(normalize_target("   "), None);
        assert_eq!(english_name("ja"), "Japanese");
        assert_eq!(english_name("Italian"), "Italian");
    }
}
