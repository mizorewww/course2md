//! 平台字幕作为转写源：解析和发现在 `subtitle-parse`，这里是界面文字、任务状态与事件转换。
//!
//! 与 ASR 产物统一到 `TranscriptEvent`，下游 timeline/LLM/渲染完全复用。

use crate::timeline::TranscriptEvent;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use subtitle_parse::{DecodeError, ReadError, best_file, normalize_language};

pub use subtitle_parse::{
    Track as SubtitleTrack, TrackKind as SubtitleKind, TrackOrigin as SubtitleOrigin,
    is_subtitle_file, sort_tracks,
};

/// How many tracks a default choice tries before it gives up: the chosen one and
/// two after it. Each try is a request to the platform, and one that rate-limits
/// (YouTube's 429) should not be met with a burst.
pub const SUBTITLE_ATTEMPTS: usize = 3;

/// The tracks to try, in order, when `chosen` was picked for the person rather than
/// by them: `chosen`, then up to two of the tracks after it in `ordered` (as
/// [`sort_tracks`] left them). An explicit choice is read alone and never falls back.
///
/// A fallback stays in a language the person can use: a track of the video's own
/// language (`original_language`, or `chosen`'s when the source does not say) or
/// of `interface_language`, and a machine translation only into
/// `interface_language`. A track in an unrelated language is never read in their
/// place; one that names no language may be. A track whose text was already
/// tried does not use a try: a second key for the same online text (YouTube's
/// `en-orig` is `en`), or a machine translation into a language already tried
/// (`en-en` after `en-orig`).
pub fn fallback_tracks<'a>(
    ordered: &'a [SubtitleTrack],
    chosen: &'a SubtitleTrack,
    interface_language: &str,
    original_language: Option<&str>,
) -> Vec<&'a SubtitleTrack> {
    fn text_key(track: &SubtitleTrack) -> Option<(&str, bool)> {
        match &track.origin {
            SubtitleOrigin::Online {
                language_key,
                automatic,
                ..
            } => Some((
                language_key.strip_suffix("-orig").unwrap_or(language_key),
                *automatic,
            )),
            _ => None,
        }
    }
    /// The language family (`zh` for `zh-Hans`, `en` for `en-orig`), which is
    /// also the target of a translation key such as `zh-Hans-en`.
    fn family(language: &str) -> String {
        normalize_language(language)
            .split('-')
            .next()
            .unwrap_or_default()
            .to_owned()
    }
    let track_family = |track: &SubtitleTrack| track.language.as_deref().map(family);
    let interface = family(interface_language);
    let original = original_language
        .map(family)
        .filter(|language| !language.is_empty())
        .or_else(|| (!chosen.translated).then(|| track_family(chosen)).flatten());
    let acceptable = |track: &SubtitleTrack| {
        // A track that names no language (a local file) is not known to be
        // unrelated; only a translation always names the one it was made into.
        let Some(language) = track_family(track) else {
            return !track.translated;
        };
        language == interface || (!track.translated && Some(&language) == original.as_ref())
    };
    let after = ordered
        .iter()
        .position(|track| track.id == chosen.id)
        .map_or(&[][..], |index| &ordered[index + 1..]);
    let mut tracks = vec![chosen];
    for track in after {
        if tracks.len() == SUBTITLE_ATTEMPTS {
            break;
        }
        let same_text = text_key(track)
            .is_some_and(|key| tracks.iter().any(|tried| text_key(tried) == Some(key)))
            || (track.translated
                && track_family(track).is_some_and(|language| {
                    tracks
                        .iter()
                        .any(|tried| track_family(tried).as_ref() == Some(&language))
                }));
        if !same_text && acceptable(track) {
            tracks.push(track);
        }
    }
    tracks
}

/// A track's name for people: its name or language and, when known, what kind of
/// track it is ("中文 · 机器翻译").
pub fn track_label(track: &SubtitleTrack) -> String {
    match track_kind_label(track) {
        Some(kind) => format!("{} · {kind}", track_name(track)),
        None => track_name(track),
    }
}

/// A track's name or language, without its kind.
pub fn track_name(track: &SubtitleTrack) -> String {
    if let Some(name) = track.name.as_ref().filter(|value| !value.trim().is_empty()) {
        name.clone()
    } else if let Some(language) = &track.language {
        language_label(language)
    } else if let SubtitleOrigin::File { path } = &track.origin {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    } else if let SubtitleOrigin::Embedded { stream_index, .. } = &track.origin {
        crate::tr!("内嵌字幕（轨道 {}）", stream_index)
    } else {
        crate::tr!("字幕").into()
    }
}

/// What kind of track it is, which tells two tracks of one language apart.
pub fn track_kind_label(track: &SubtitleTrack) -> Option<String> {
    match track.kind {
        // Whatever its name says, a translation the platform makes on request is one.
        _ if track.translated => Some(crate::tr!("机器翻译").into()),
        SubtitleKind::Manual => Some(crate::tr!("人工字幕").into()),
        SubtitleKind::Automatic => Some(crate::tr!("自动字幕").into()),
        SubtitleKind::Unknown => None,
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

pub(crate) fn language_label(language: &str) -> String {
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

/// Decode supported sidecars without lossy replacement of the original words.
pub fn read_subtitle_text(path: &Path) -> Result<String> {
    subtitle_parse::read_text(path).map_err(|error| match error {
        ReadError::Open { path, source } => anyhow::Error::new(source).context(crate::tr!(
            "无法读取字幕文件 {} / Cannot read subtitle file {}",
            path.display(),
            path.display()
        )),
        ReadError::Read(source) => source.into(),
        ReadError::TooLarge => anyhow::anyhow!(crate::tr!(
            "字幕文件超过 32 MB，请选择较小的 SRT 或 VTT 文件 / Subtitle file exceeds 32 MB; choose a smaller SRT or VTT file"
        )),
        ReadError::Decode(DecodeError::IncompleteUtf16) => anyhow::anyhow!(crate::tr!(
            "文件的 UTF-16 编码不完整，请重新导出为 UTF-8 / The file's UTF-16 encoding is incomplete; re-export as UTF-8"
        )),
        ReadError::Decode(source) => anyhow::Error::new(source).context(crate::tr!(
            "文件的字符编码无法读取，请重新导出为 UTF-8 / The file's character encoding is unreadable; re-export as UTF-8"
        )),
    })
}

/// 解析 SRT / VTT 字幕为转写事件；时间区间无效的 cue 记入日志后跳过。
pub fn parse_subtitle(content: &str) -> Vec<TranscriptEvent> {
    let parsed = subtitle_parse::parse(content);
    for skipped in &parsed.skipped {
        tracing::warn!(
            line = skipped.header,
            "字幕时间区间无效，已跳过 / Skipped subtitle with invalid timestamps"
        );
    }
    parsed
        .cues
        .into_iter()
        .map(|cue| TranscriptEvent {
            start: cue.start,
            end: cue.end,
            text: cue.text,
            raw: None,
            translation: None,
        })
        .collect()
}

pub fn to_srt(events: &[TranscriptEvent]) -> String {
    let cues: Vec<_> = events
        .iter()
        .map(|event| subtitle_parse::Cue {
            start: event.start,
            end: event.end,
            text: event.text.clone(),
        })
        .collect();
    subtitle_parse::to_srt(&cues)
}

pub fn file_track(path: PathBuf, language: Option<String>) -> SubtitleTrack {
    SubtitleTrack::file(path, language)
}

/// 在已有目录中挑一个字幕文件（界面语言优先、其次英文）。新调用方会把确定的一条字幕抓取到
/// 独立目录；这里绝不能选中属于其他来源的产物。
pub fn pick_subtitle_file(dir: &Path) -> Option<PathBuf> {
    preferred_file(subtitle_parse::files_in(dir), c2m_i18n::i18n::lang().code())
}

/// The sidecars of a local video, at absolute paths. `subtitle-parse` keeps the form of the
/// video's path, so a bare `lecture.mp4` would give a bare `lecture.srt`; a track stored with
/// a task or read by another process must not depend on this process's working directory.
pub fn sidecar_tracks(video: &Path) -> Vec<SubtitleTrack> {
    match std::env::current_dir() {
        Ok(cwd) => sidecar_tracks_in(video, &cwd),
        Err(_) if video.is_absolute() => subtitle_parse::sidecars(video),
        Err(_) => Vec::new(),
    }
}

fn sidecar_tracks_in(video: &Path, cwd: &Path) -> Vec<SubtitleTrack> {
    subtitle_parse::sidecars(&cwd.join(video))
}

/// 本地视频的同名字幕 sidecar（`lecture.srt`、`lecture.zh.vtt` 等，与桌面端同一发现规则），
/// 界面语言优先、其次英文。A task that has its transcript keeps it, so this choice
/// never changes under a resumed task.
pub fn sidecar_subtitle(video: &Path) -> Option<PathBuf> {
    preferred_file(sidecar_tracks(video), c2m_i18n::i18n::lang().code())
}

fn preferred_file(tracks: Vec<SubtitleTrack>, interface_language: &str) -> Option<PathBuf> {
    best_file(tracks, &[], interface_language, Some("en"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn online(key: &str, automatic: bool, translated: bool) -> SubtitleTrack {
        SubtitleTrack {
            id: format!("v:subtitle:{automatic}:{key}"),
            language: Some(key.into()),
            name: None,
            kind: if automatic {
                SubtitleKind::Automatic
            } else {
                SubtitleKind::Manual
            },
            translated,
            origin: SubtitleOrigin::Online {
                language_key: key.into(),
                automatic,
                inline_text: None,
            },
            source_order: 0,
        }
    }

    fn keys(tracks: Vec<&SubtitleTrack>) -> Vec<String> {
        tracks
            .into_iter()
            .map(|track| match &track.origin {
                SubtitleOrigin::Online {
                    language_key,
                    automatic,
                    ..
                } => format!(
                    "{} {language_key}",
                    if *automatic { "auto" } else { "manual" }
                ),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn a_default_choice_falls_back_three_tracks_deep_without_repeating_text() {
        let tracks = [
            online("en-orig", true, false),
            online("en", true, false),
            online("en", false, false),
            online("en-US", true, false),
            online("fr", true, false),
        ];
        assert_eq!(
            keys(fallback_tracks(&tracks, &tracks[0], "ja", Some("en"))),
            ["auto en-orig", "manual en", "auto en-US"]
        );
        // Only what comes after the choice, and a track not in the list alone.
        assert_eq!(
            keys(fallback_tracks(&tracks, &tracks[4], "fr", Some("en"))),
            ["auto fr"]
        );
        // Files that name no language are not ruled out.
        let files = [
            file_track("a.srt".into(), None),
            file_track("b.srt".into(), None),
        ];
        assert_eq!(fallback_tracks(&files, &files[0], "en", None).len(), 2);
        let alone = online("ja", true, false);
        assert_eq!(
            keys(fallback_tracks(&tracks, &alone, "ja", None)),
            ["auto ja"]
        );
    }

    /// YouTube's list for an English video with only automatic captions, as
    /// [`sort_tracks`] orders it: a 429 on the captions must never end in an
    /// Afrikaans transcript, and `en-en` is the captions' own words again.
    #[test]
    fn a_fallback_never_reads_an_unrelated_language() {
        let list = || {
            [
                ("af-en", true),
                ("ar-en", true),
                ("zh-Hans-en", true),
                ("en-en", true),
                ("ja-en", true),
                ("af", true),
                ("zh-Hans", true),
                ("en-orig", false),
                ("en", false),
                ("ja", true),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (key, translated))| SubtitleTrack {
                source_order: index,
                ..online(key, true, translated)
            })
            .collect::<Vec<_>>()
        };
        let tried = |interface: &str, original: Option<&str>| {
            let mut tracks = list();
            sort_tracks(&mut tracks, &[], interface, original);
            keys(fallback_tracks(&tracks, &tracks[0], interface, original))
        };
        // The captions once; nothing else is English or the video's.
        assert_eq!(tried("en", Some("en")), ["auto en-orig"]);
        assert_eq!(tried("en", None), ["auto en-orig"]);
        // A person reading Chinese may get a translation into Chinese, once.
        assert_eq!(
            tried("zh-Hans", Some("en")),
            ["auto en-orig", "auto zh-Hans"]
        );
        assert_eq!(tried("ja", None), ["auto en-orig", "auto ja-en"]);
        // A language with no track of its own gets no stranger in its place.
        assert_eq!(tried("fr", Some("en")), ["auto en-orig"]);
    }

    /// With English subtitles as well, the subtitles and the captions are both
    /// the video's language; `en-en` (the subtitles, "translated" to English)
    /// is not a third try.
    #[test]
    fn a_translation_into_a_tried_language_is_not_tried() {
        let mut tracks = vec![online("en", false, false)];
        tracks.extend(
            [
                ("en-en", true),
                ("af-en", true),
                ("ja-en", true),
                ("en-orig", false),
                ("en", false),
                ("ja", true),
            ]
            .into_iter()
            .map(|(key, translated)| online(key, true, translated)),
        );
        for (index, track) in tracks.iter_mut().enumerate() {
            track.source_order = index;
        }
        let tried = |interface: &str| {
            let mut tracks = tracks.clone();
            sort_tracks(&mut tracks, &[], interface, Some("en"));
            keys(fallback_tracks(&tracks, &tracks[0], interface, Some("en")))
        };
        assert_eq!(tried("en"), ["manual en", "auto en"]);
        assert_eq!(tried("ja"), ["manual en", "auto en", "auto ja-en"]);
    }

    #[test]
    fn labels_never_invent_a_caption_kind() {
        let mut track = file_track("lesson.zh.srt".into(), Some("zh-Hans".into()));
        assert!(!track_label(&track).contains("人工"));
        assert!(!track_label(&track).contains("自动"));
        track.kind = SubtitleKind::Manual;
        assert!(track_label(&track).ends_with("人工字幕"));
        // A translation YouTube makes on request reads as one, not as captions.
        track.kind = SubtitleKind::Automatic;
        track.translated = true;
        track.name = Some("Chinese (Simplified)".into());
        assert_eq!(track_label(&track), "Chinese (Simplified) · 机器翻译");
    }

    #[test]
    fn events_survive_the_srt_round_trip() {
        let events = parse_subtitle("1\n00:00:01,125 --> 00:00:02,375\nBonjour 世界\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events, parse_subtitle(&to_srt(&events)));
    }

    #[test]
    fn a_blank_line_in_a_cue_does_not_split_it() {
        let event = |start: f64, text: &str| TranscriptEvent {
            start,
            end: start + 1.,
            text: text.into(),
            raw: None,
            translation: None,
        };
        let events = [event(1., "first\n\n  \nsecond"), event(3., "third")];
        let srt = to_srt(&events);
        assert!(srt.contains("first\nsecond\n\n2\n"), "{srt}");
        let parsed = parse_subtitle(&srt);
        assert_eq!(parsed.len(), 2);
        assert_eq!((parsed[0].start, parsed[1].start), (1., 3.));
        assert!(parsed[0].text.contains("first") && parsed[0].text.contains("second"));
        assert_eq!(parsed[1].text, "third");
    }

    #[test]
    fn read_failures_keep_their_words_and_causes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lesson.srt");
        let open = read_subtitle_text(&file).unwrap_err();
        assert!(open.to_string().contains(&*file.to_string_lossy()));
        assert!(open.source().is_some());
        std::fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
        let incomplete = read_subtitle_text(&file).unwrap_err().to_string();
        assert!(incomplete.contains("UTF-16"));
        std::fs::write(&file, [0xff, 0xff, 0xfe]).unwrap();
        let invalid = read_subtitle_text(&file).unwrap_err();
        assert!(invalid.to_string().contains("UTF-8"));
        assert!(invalid.source().is_some());
    }

    #[test]
    fn stored_subtitle_state_keeps_its_wire_format() {
        let evidence = SubtitleEvidence::Found {
            tracks: vec![file_track("lesson.srt".into(), None)],
            warning: None,
        };
        let json = serde_json::to_value(&evidence).unwrap();
        assert_eq!(json["state"], "found");
        assert_eq!(json["tracks"][0]["kind"], "unknown");
        assert_eq!(json["tracks"][0]["origin"]["kind"], "file");
        assert_eq!(
            serde_json::from_value::<SubtitleEvidence>(json).unwrap(),
            evidence
        );
    }

    #[test]
    fn files_prefer_the_interface_language_then_english() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["sub.en.srt", "sub.zh-Hans.srt", "sub.ja.srt"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let video = dir.path().join("sub.mp4");
        for (language, expected) in [
            ("zh-Hans", "sub.zh-Hans.srt"),
            ("zh-Hant", "sub.zh-Hans.srt"),
            ("ja", "sub.ja.srt"),
            ("en", "sub.en.srt"),
            ("ko", "sub.en.srt"),
        ] {
            let picked = preferred_file(subtitle_parse::files_in(dir.path()), language).unwrap();
            assert_eq!(picked.file_name().unwrap(), expected, "{language}");
            let sidecar = preferred_file(sidecar_tracks(&video), language).unwrap();
            assert_eq!(sidecar, picked, "{language}");
        }
        // The process language is Simplified Chinese here, as for --json and run-task.
        assert_eq!(
            pick_subtitle_file(dir.path()).unwrap(),
            dir.path().join("sub.zh-Hans.srt")
        );
    }

    #[test]
    fn a_bare_video_name_finds_its_sidecars_at_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lecture.srt"), b"x").unwrap();
        std::fs::write(other.path().join("lecture.srt"), b"x").unwrap();
        let picked = preferred_file(
            sidecar_tracks_in(Path::new("lecture.mp4"), dir.path()),
            "en",
        )
        .unwrap();
        assert_eq!(picked, dir.path().join("lecture.srt"));
        assert!(picked.is_absolute());
        // An absolute video keeps its own directory, whatever the working directory.
        let video = other.path().join("lecture.mp4");
        assert_eq!(
            preferred_file(sidecar_tracks_in(&video, dir.path()), "en").unwrap(),
            other.path().join("lecture.srt")
        );
    }
}
