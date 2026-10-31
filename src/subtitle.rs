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
    is_subtitle_file, sidecars as sidecar_tracks, sort_tracks,
};

/// A track's name for people.
pub fn track_label(track: &SubtitleTrack) -> String {
    let mut parts = Vec::new();
    if let Some(name) = track.name.as_ref().filter(|value| !value.trim().is_empty()) {
        parts.push(name.clone());
    } else if let Some(language) = &track.language {
        parts.push(language_label(language));
    } else if let SubtitleOrigin::File { path } = &track.origin {
        parts.push(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        );
    } else if let SubtitleOrigin::Embedded { stream_index, .. } = &track.origin {
        parts.push(crate::tr!("内嵌字幕（轨道 {}）", stream_index));
    } else {
        parts.push(crate::tr!("字幕").into());
    }
    match track.kind {
        SubtitleKind::Manual => parts.push(crate::tr!("人工字幕").into()),
        SubtitleKind::Automatic => parts.push(crate::tr!("自动字幕").into()),
        SubtitleKind::Unknown => {}
    }
    parts.join(" · ")
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

/// 在已有目录中挑一个字幕文件（中文优先、其次英文）。新调用方会把确定的一条字幕抓取到
/// 独立目录；这里绝不能选中属于其他来源的产物。
pub fn pick_subtitle_file(dir: &Path) -> Option<PathBuf> {
    best_file(subtitle_parse::files_in(dir), &[], "zh", Some("en"))
}

/// 本地视频的同名字幕 sidecar（`lecture.srt`、`lecture.zh.vtt` 等，与桌面端同一发现规则），
/// 中文优先、其次英文。
pub fn sidecar_subtitle(video: &Path) -> Option<PathBuf> {
    best_file(sidecar_tracks(video), &[], "zh", Some("en"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_never_invent_a_caption_kind() {
        let mut track = file_track("lesson.zh.srt".into(), Some("zh-Hans".into()));
        assert!(!track_label(&track).contains("人工"));
        assert!(!track_label(&track).contains("自动"));
        track.kind = SubtitleKind::Manual;
        assert!(track_label(&track).ends_with("人工字幕"));
    }

    #[test]
    fn events_survive_the_srt_round_trip() {
        let events = parse_subtitle("1\n00:00:01,125 --> 00:00:02,375\nBonjour 世界\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events, parse_subtitle(&to_srt(&events)));
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
    fn the_defaults_prefer_chinese_then_english() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["sub.en.srt", "sub.zh-Hans.srt", "sub.ja.srt"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let picked = pick_subtitle_file(dir.path()).unwrap();
        assert_eq!(picked.file_name().unwrap(), "sub.zh-Hans.srt");
        let video = dir.path().join("sub.mp4");
        assert_eq!(sidecar_subtitle(&video).unwrap(), picked);
    }
}
