//! 平台字幕作为转写源：SRT/VTT 解析、sidecar 查找、语言偏好挑选。
//!
//! 与 ASR 产物统一到 `TranscriptEvent`，下游 timeline/LLM/渲染完全复用。

use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A track is a discovery result, not proof that its text can be read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub id: String,
    pub language: Option<String>,
    pub name: Option<String>,
    pub kind: SubtitleKind,
    pub origin: SubtitleOrigin,
    #[serde(default)]
    pub source_order: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleKind {
    Manual,
    Automatic,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubtitleOrigin {
    Online {
        /// Exact extractor key. This is not a translated display label.
        language_key: String,
        automatic: bool,
        #[serde(default)]
        inline_text: Option<String>,
    },
    Embedded {
        stream_index: u32,
        codec: String,
    },
    File {
        path: PathBuf,
    },
}

impl SubtitleTrack {
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(name) = self.name.as_ref().filter(|value| !value.trim().is_empty()) {
            parts.push(name.clone());
        } else if let Some(language) = &self.language {
            parts.push(language_label(language));
        } else if let SubtitleOrigin::File { path } = &self.origin {
            parts.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
        } else if let SubtitleOrigin::Embedded { stream_index, .. } = &self.origin {
            parts.push(crate::tr!("内嵌字幕（轨道 {}）", stream_index));
        } else {
            parts.push(crate::tr!("字幕").into());
        }
        match self.kind {
            SubtitleKind::Manual => parts.push(crate::tr!("人工字幕").into()),
            SubtitleKind::Automatic => parts.push(crate::tr!("自动字幕").into()),
            SubtitleKind::Unknown => {}
        }
        parts.join(" · ")
    }
}

/// Failed discovery and successful discovery with no readable tracks are distinct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SubtitleEvidence {
    #[default]
    Unchecked,
    Found {
        tracks: Vec<SubtitleTrack>,
        /// Some channels may fail while others return usable candidates.
        #[serde(default)]
        warning: Option<String>,
    },
    NoneFound,
    Failed {
        message: String,
    },
    Unsupported {
        message: String,
    },
}

impl SubtitleEvidence {
    pub fn tracks(&self) -> &[SubtitleTrack] {
        match self {
            Self::Found { tracks, .. } => tracks,
            _ => &[],
        }
    }
}

/// This value is created only after parsing nonempty text. Tasks may copy events,
/// avoiding a later change to the attached file or an expired remote subtitle URL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedSubtitle {
    pub source_identity: String,
    pub track_id: String,
    pub label: String,
    pub path: PathBuf,
    pub events: Vec<TranscriptEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SubtitleReadError {
    Failed { message: String },
    NoReadableText { message: String },
    Unsupported { message: String },
    Cancelled,
}

impl std::fmt::Display for SubtitleReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed { message }
            | Self::NoReadableText { message }
            | Self::Unsupported { message } => f.write_str(message),
            Self::Cancelled => {
                f.write_str(crate::tr!("已取消读取字幕 / Subtitle reading cancelled"))
            }
        }
    }
}

impl std::error::Error for SubtitleReadError {}

pub fn language_label(language: &str) -> String {
    match normalize_language(language).as_str() {
        "zh" => crate::tr!("中文").into(),
        "zh-hans" | "zh-cn" | "zh-sg" => crate::tr!("简体中文").into(),
        "zh-hant" | "zh-tw" | "zh-hk" => crate::tr!("繁体中文").into(),
        "en" => crate::tr!("英语").into(),
        "fr" => crate::tr!("法语").into(),
        "de" => crate::tr!("德语").into(),
        "ja" => crate::tr!("日语").into(),
        "ko" => crate::tr!("韩语").into(),
        "es" => crate::tr!("西班牙语").into(),
        "pt" => crate::tr!("葡萄牙语").into(),
        "ru" => crate::tr!("俄语").into(),
        "ar" => crate::tr!("阿拉伯语").into(),
        "it" => crate::tr!("意大利语").into(),
        _ => language.to_owned(),
    }
}

fn normalize_language(language: &str) -> String {
    let value = language.trim().replace('_', "-").to_ascii_lowercase();
    // ffprobe commonly returns ISO 639-2 language codes.
    match value.as_str() {
        "zho" | "chi" => "zh".into(),
        "eng" => "en".into(),
        "fra" | "fre" => "fr".into(),
        "deu" | "ger" => "de".into(),
        "jpn" => "ja".into(),
        "kor" => "ko".into(),
        "spa" => "es".into(),
        "por" => "pt".into(),
        "rus" => "ru".into(),
        "ara" => "ar".into(),
        "ita" => "it".into(),
        _ => value,
    }
}

/// Preference list → interface language → known original language → discovery
/// order. A missed preference always falls through, never removes other languages.
/// A generic preference such as `zh` covers its language family equally; a
/// script/region preference such as `zh-Hans` prefers that exact variant.
/// Within an identical language, known human captions precede known automatic ones.
pub fn sort_tracks(
    tracks: &mut [SubtitleTrack],
    preferences: &[String],
    interface_language: &str,
    original_language: Option<&str>,
) {
    let mut languages = Vec::new();
    for language in preferences
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(interface_language))
        .chain(original_language)
    {
        let value = normalize_language(language);
        if !value.is_empty() && !languages.contains(&value) {
            languages.push(value);
        }
    }
    // Precompute one rank for each exact language group. This preserves the
    // extractor's order between remaining languages, including non-Latin tracks.
    let mut groups: Vec<Option<String>> = Vec::new();
    let mut discovery: Vec<_> = tracks.iter().collect();
    discovery.sort_by_key(|track| track.source_order);
    for track in discovery {
        let language = track.language.as_deref().map(normalize_language);
        if !groups.contains(&language) {
            groups.push(language);
        }
    }
    let rank = |track: &SubtitleTrack| {
        let language = track.language.as_deref().map(normalize_language);
        let preferred = language
            .as_ref()
            .and_then(|language| {
                languages
                    .iter()
                    .enumerate()
                    .find_map(|(index, preference)| {
                        let same = language == preference;
                        let family = language.split('-').next() == preference.split('-').next();
                        (same || family).then_some((
                            index,
                            if same || !preference.contains('-') {
                                0
                            } else {
                                1
                            },
                        ))
                    })
            })
            .unwrap_or((languages.len(), 0));
        let group = groups
            .iter()
            .position(|group| group == &language)
            .unwrap_or(usize::MAX);
        (preferred, group)
    };
    tracks.sort_by_key(rank);
    // Unknown labels retain their positions. Only reorder slots whose type the
    // source explicitly distinguishes, avoiding invented claims about a track.
    let mut start = 0;
    while start < tracks.len() {
        let language = tracks[start].language.as_deref().map(normalize_language);
        let end = tracks[start..]
            .iter()
            .position(|track| track.language.as_deref().map(normalize_language) != language)
            .map(|offset| start + offset)
            .unwrap_or(tracks.len());
        let positions: Vec<_> = (start..end)
            .filter(|index| tracks[*index].kind != SubtitleKind::Unknown)
            .collect();
        let mut known: Vec<_> = positions
            .iter()
            .map(|index| tracks[*index].clone())
            .collect();
        known.sort_by_key(|track| track.kind == SubtitleKind::Automatic);
        for (position, track) in positions.into_iter().zip(known) {
            tracks[position] = track;
        }
        start = end;
    }
}

/// Decode supported sidecars without lossy replacement of the original words.
pub fn read_subtitle_text(path: &Path) -> Result<String> {
    use std::io::Read;
    const MAX_BYTES: usize = 32 * 1024 * 1024;
    let file = std::fs::File::open(path).with_context(|| {
        crate::tr!(
            "无法读取字幕文件 {} / Cannot read subtitle file {}",
            path.display(),
            path.display()
        )
    })?;
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        crate::tr!(
            "字幕文件超过 32 MB，请选择较小的 SRT 或 VTT 文件 / Subtitle file exceeds 32 MB; choose a smaller SRT or VTT file"
        )
    );
    decode_text_with_bom(&bytes)
}

/// 解码 UTF-8 / UTF-16（按 BOM 判端序）文本：UTF-8 BOM 与 UTF-16 BOM 都会剥掉，
/// 无 BOM 按 UTF-8 处理。拒绝不完整的 UTF-16 与非法编码，不做有损替换。
pub(crate) fn decode_text_with_bom(bytes: &[u8]) -> Result<String> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        ensure!(
            (bytes.len() - 2).is_multiple_of(2),
            crate::tr!(
                "文件的 UTF-16 编码不完整，请重新导出为 UTF-8 / The file's UTF-16 encoding is incomplete; re-export as UTF-8"
            )
        );
        let little = bytes[0] == 0xff;
        let words: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                if little {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        return String::from_utf16(&words).context(crate::tr!("文件的字符编码无法读取，请重新导出为 UTF-8 / The file's character encoding is unreadable; re-export as UTF-8"));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8(bytes.to_vec()).context(crate::tr!("文件的字符编码无法读取，请重新导出为 UTF-8 / The file's character encoding is unreadable; re-export as UTF-8"))
}

pub fn to_srt(events: &[TranscriptEvent]) -> String {
    fn timestamp(seconds: f64) -> String {
        let millis = (seconds.max(0.) * 1000.).round() as u64;
        format!(
            "{:02}:{:02}:{:02},{:03}",
            millis / 3_600_000,
            millis / 60_000 % 60,
            millis / 1000 % 60,
            millis % 1000
        )
    }
    events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            format!(
                "{}\n{} --> {}\n{}\n\n",
                index + 1,
                timestamp(event.start),
                timestamp(event.end),
                event.text
            )
        })
        .collect()
}

/// All exact-name and language-qualified sidecars, retaining both SRT and VTT.
pub fn sidecar_tracks(video: &Path) -> Vec<SubtitleTrack> {
    let Some(parent) = video.parent() else {
        return Vec::new();
    };
    let Some(stem) = video.file_stem().and_then(|value| value.to_str()) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            if !path.is_file() || !is_subtitle_file(path) {
                return false;
            }
            path.file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|candidate| {
                    candidate == stem || candidate.starts_with(&format!("{stem}."))
                })
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let language = path
                .file_stem()
                .and_then(|value| value.to_str())
                .and_then(|candidate| candidate.strip_prefix(&format!("{stem}.")))
                .filter(|candidate| {
                    candidate
                        .bytes()
                        .all(|byte| byte.is_ascii_alphabetic() || byte == b'-' || byte == b'_')
                })
                .map(str::to_owned);
            let mut track = file_track(path, language);
            track.source_order = index;
            track
        })
        .collect()
}

pub fn is_subtitle_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "srt" | "vtt"))
}

pub fn file_track(path: PathBuf, language: Option<String>) -> SubtitleTrack {
    SubtitleTrack {
        id: format!("file:{}", path.display()),
        language,
        name: None,
        kind: SubtitleKind::Unknown,
        origin: SubtitleOrigin::File { path },
        source_order: usize::MAX,
    }
}

/// 解析 SRT / VTT 字幕为转写事件。
/// - 时间戳兼容 `HH:MM:SS,mmm`（SRT）与 `HH:MM:SS.mmm`（VTT）及 `MM:SS.mmm`
/// - 去除 VTT 行内标签（`<c>...</c>` 等）与常见 HTML 实体
/// - 合并同一 cue 的多行文本；滚动字幕（YouTube/B站自动字幕常见）按相邻去重
pub fn parse_subtitle(content: &str) -> Vec<TranscriptEvent> {
    let mut out: Vec<TranscriptEvent> = vec![];
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((start, end)) = parse_cue_header(line) {
            i += 1;
            // 外部数据不信任：非有限时间戳的 cue 直接丢弃，
            // 避免 NaN/inf 污染下游排序与二分查找
            if !start.is_finite() || !end.is_finite() || end <= start {
                tracing::warn!(
                    line,
                    "字幕时间区间无效，已跳过 / Skipped subtitle with invalid timestamps"
                );
                continue;
            }
            let mut text_parts: Vec<String> = vec![];
            while i < lines.len() && !lines[i].trim().is_empty() {
                let cleaned = clean_cue_text(lines[i]);
                if !cleaned.is_empty() {
                    text_parts.push(cleaned);
                }
                i += 1;
            }
            let text = text_parts.join(" ");
            // 滚动字幕去重：相邻 cue 文本相同则只保留首个（并延长时长）
            if !text.is_empty() {
                match out.last_mut() {
                    Some(prev) if prev.text == text && start <= prev.end => {
                        prev.end = end.max(prev.end)
                    }
                    _ => out.push(TranscriptEvent {
                        start,
                        end,
                        text,
                        raw: None,
                        translation: None,
                    }),
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

fn parse_cue_header(line: &str) -> Option<(f64, f64)> {
    let (a, b) = line.split_once("-->")?;
    // VTT cue header 可能带 "line:0" 等设置
    let end = b.split_whitespace().next()?;
    Some((
        crate::timeline::parse_timestamp(a)?,
        crate::timeline::parse_timestamp(end)?,
    ))
}

/// 去掉 `<...>` 标签并还原常见实体。
fn clean_cue_text(line: &str) -> String {
    let mut s = String::with_capacity(line.len());
    let mut in_tag = false;
    for c in line.trim().chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => s.push(c),
            _ => {}
        }
    }
    // 注意顺序：&amp; 必须最后替换，否则 "&amp;lt;" 会先被还原成 "&lt;"
    // 再被二次还原成 "<"，造成双重反转义
    for (from, to) in [
        ("&nbsp;", " "),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&amp;", "&"),
    ] {
        s = s.replace(from, to);
    }
    s.trim().to_string()
}

/// 在已有目录中挑一个字幕文件。新调用方会把确定的一条字幕抓取到独立目录；
/// 这里绝不能选中属于其他来源的产物。
pub fn pick_subtitle_file(dir: &Path) -> Option<PathBuf> {
    let files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_subtitle_file(path))
        .collect();
    if files.is_empty() {
        return None;
    }
    let tracks: Vec<SubtitleTrack> = files
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let language = path
                .file_stem()
                .and_then(|value| value.to_str())
                .and_then(|stem| stem.split_once('.'))
                .map(|(_, language)| language.to_owned());
            let mut track = file_track(path, language);
            track.source_order = index;
            track
        })
        .collect();
    preferred_file(tracks)
}

/// The best file track: Chinese first, then English.
fn preferred_file(mut tracks: Vec<SubtitleTrack>) -> Option<PathBuf> {
    sort_tracks(&mut tracks, &[], "zh", Some("en"));
    tracks.into_iter().find_map(|track| match track.origin {
        SubtitleOrigin::File { path } => Some(path),
        _ => None,
    })
}

/// 本地视频的同名字幕 sidecar（`lecture.srt`、`lecture.zh.vtt` 等，与桌面端同一发现规则），
/// 中文优先、其次英文。
pub fn sidecar_subtitle(video: &Path) -> Option<PathBuf> {
    preferred_file(sidecar_tracks(video))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(language: &str, kind: SubtitleKind) -> SubtitleTrack {
        SubtitleTrack {
            id: format!("{language}:{kind:?}"),
            language: Some(language.into()),
            name: None,
            kind,
            source_order: 0,
            origin: SubtitleOrigin::Online {
                language_key: language.into(),
                automatic: kind == SubtitleKind::Automatic,
                inline_text: None,
            },
        }
    }

    #[test]
    fn missed_preferences_fall_through_without_hiding_languages() {
        let mut tracks = vec![
            track("fr", SubtitleKind::Automatic),
            track("ja", SubtitleKind::Manual),
            track("fr", SubtitleKind::Manual),
            track("de", SubtitleKind::Manual),
        ];
        sort_tracks(&mut tracks, &["zh".into()], "en", Some("ja"));
        assert_eq!(
            tracks
                .iter()
                .map(|track| track.language.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["ja", "fr", "fr", "de"]
        );
        assert_eq!(tracks[1].kind, SubtitleKind::Manual);
        assert_eq!(tracks[2].kind, SubtitleKind::Automatic);
        sort_tracks(&mut tracks, &["xx".into()], "yy", None);
        assert_eq!(tracks.len(), 4);
    }

    #[test]
    fn explicit_language_then_interface_then_original_then_discovery_order() {
        let mut tracks = vec![
            track("ru", SubtitleKind::Manual),
            track("fr", SubtitleKind::Manual),
            track("de", SubtitleKind::Manual),
            track("en", SubtitleKind::Manual),
            track("ja", SubtitleKind::Manual),
        ];
        sort_tracks(&mut tracks, &["ja".into()], "en", Some("de"));
        assert_eq!(
            tracks
                .iter()
                .map(|track| track.language.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["ja", "en", "de", "ru", "fr"]
        );
    }

    #[test]
    fn simplified_interface_prefers_hans_without_inventing_caption_kind() {
        let mut tracks = vec![
            track("zh", SubtitleKind::Automatic),
            track("zh-Hans", SubtitleKind::Unknown),
            track("ja", SubtitleKind::Manual),
        ];
        for (index, track) in tracks.iter_mut().enumerate() {
            track.source_order = index;
        }
        sort_tracks(&mut tracks, &[], "zh-Hans", Some("ja"));
        assert_eq!(tracks[0].language.as_deref(), Some("zh-Hans"));
        assert_eq!(tracks[0].kind, SubtitleKind::Unknown);
        assert!(!tracks[0].label().contains("人工"));
        assert_eq!(tracks[1].kind, SubtitleKind::Automatic);
    }

    #[test]
    fn generic_language_preference_does_not_promote_unspecified_script() {
        let mut tracks = vec![
            track("zh-Hans", SubtitleKind::Unknown),
            track("zh", SubtitleKind::Automatic),
            track("ja", SubtitleKind::Manual),
        ];
        for (index, track) in tracks.iter_mut().enumerate() {
            track.source_order = index;
        }
        sort_tracks(&mut tracks, &["zh".into()], "en", None);
        assert_eq!(tracks[0].language.as_deref(), Some("zh-Hans"));
        assert_eq!(tracks[1].language.as_deref(), Some("zh"));
    }

    #[test]
    fn sidecars_include_every_language_and_both_formats_without_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("Lecture.mp4");
        for name in [
            "Lecture.fr.srt",
            "Lecture.JA.VTT",
            "Lecture.srt",
            "Other.srt",
        ] {
            std::fs::write(
                dir.path().join(name),
                "1\n00:00:00,000 --> 00:00:01,000\nBonjour\n",
            )
            .unwrap();
        }
        let tracks = sidecar_tracks(&video);
        assert_eq!(tracks.len(), 3);
        assert!(
            tracks
                .iter()
                .any(|track| track.language.as_deref() == Some("fr"))
        );
        assert!(
            tracks
                .iter()
                .any(|track| track.language.as_deref() == Some("JA"))
        );
    }

    #[test]
    fn utf16_sidecars_and_standard_cache_preserve_words_and_timing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("arbitrary-name.srt");
        let text = "1\n00:00:01,125 --> 00:00:02,375\nBonjour 世界\n";
        let bytes: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        std::fs::write(&file, bytes).unwrap();
        let events = parse_subtitle(&read_subtitle_text(&file).unwrap());
        assert_eq!(events.len(), 1);
        assert_eq!(events, parse_subtitle(&to_srt(&events)));
        std::fs::write(&file, [0xff, 0xff, 0xfe]).unwrap();
        assert!(read_subtitle_text(&file).is_err());
    }

    #[test]
    fn repeated_words_after_a_pause_remain_separate() {
        let events = parse_subtitle(
            "1\n00:00:01,000 --> 00:00:02,000\nhello\n\n2\n00:00:10,000 --> 00:00:11,000\nhello\n",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].end, 2.0);
        assert!(parse_subtitle("00:00:02,000 --> 00:00:01,000\nbackwards\n").is_empty());
        assert!(crate::timeline::parse_timestamp("00:00:01.bad").is_none());
        assert!(crate::timeline::parse_timestamp("00:99:01.000").is_none());
    }

    #[test]
    fn parses_srt_with_multiline_cues() {
        let srt = "\
1
00:00:01,000 --> 00:00:04,000
大家好，今天我们讲
编译原理

2
00:00:04,500 --> 00:00:07,250
这是第二段
";
        let ev = parse_subtitle(srt);
        assert_eq!(ev.len(), 2);
        assert!((ev[0].start - 1.0).abs() < 1e-3);
        assert!((ev[0].end - 4.0).abs() < 1e-3);
        assert_eq!(ev[0].text, "大家好，今天我们讲 编译原理");
        assert!((ev[1].start - 4.5).abs() < 1e-3);
    }

    #[test]
    fn parses_vtt_and_strips_tags() {
        let vtt = "\
WEBVTT

NOTE 这是注释

00:01:01.000 --> 00:01:03.000 align:start
<c>hello</c> world&nbsp;!

00:03.500 --> 00:05.000
second cue
";
        let ev = parse_subtitle(vtt);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].text, "hello world !");
        assert!((ev[0].start - 61.0).abs() < 1e-3);
        assert_eq!(ev[1].text, "second cue");
    }

    #[test]
    fn dedupes_rolling_captions() {
        // YouTube 自动字幕的滚动重复：相邻 cue 文本相同
        let srt = "\
1
00:00:01,000 --> 00:00:02,000
机器学习是

2
00:00:02,000 --> 00:00:03,000
机器学习是

3
00:00:03,000 --> 00:00:04,000
一门人工智能分支
";
        let ev = parse_subtitle(srt);
        assert_eq!(ev.len(), 2, "相邻重复应合并");
        assert!((ev[0].end - 3.0).abs() < 1e-3, "重复 cue 延长时长");
        assert_eq!(ev[1].text, "一门人工智能分支");
    }

    #[test]
    fn pick_prefers_zh_then_en() {
        let d = std::env::temp_dir().join(format!("c2m-subs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for name in ["sub.en.srt", "sub.zh-Hans.srt", "sub.ja.srt"] {
            std::fs::write(d.join(name), b"1\n00:00:01,000 --> 00:00:02,000\nx\n").unwrap();
        }
        let picked = pick_subtitle_file(&d).unwrap();
        assert_eq!(picked.file_name().unwrap(), "sub.zh-Hans.srt");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sidecar_lookup() {
        let d = std::env::temp_dir().join(format!("c2m-side-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let video = d.join("lecture.mp4");
        std::fs::write(&video, b"v").unwrap();
        assert!(sidecar_subtitle(&video).is_none());
        std::fs::write(
            d.join("lecture.srt"),
            b"1\n00:00:01,000 --> 00:00:02,000\nx\n",
        )
        .unwrap();
        assert_eq!(sidecar_subtitle(&video).unwrap(), d.join("lecture.srt"));
        std::fs::write(
            d.join("lecture.zh.VTT"),
            b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nx\n",
        )
        .unwrap();
        assert_eq!(sidecar_subtitle(&video).unwrap(), d.join("lecture.zh.VTT"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
