//! 时间线：事件类型、frame/speech 合并、段落组织与 timeline.jsonl 读写。

use crate::fetch::VideoMeta;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrameEvent {
    /// 帧对应的视频时间（秒）
    pub t: f64,
    /// 相对输出目录的图片路径，如 "frames/slide_0001.jpg"
    pub image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptEvent {
    pub start: f64,
    pub end: f64,
    /// 展示文本（LLM 润色后的）；未润色时与 raw 相同
    pub text: String,
    /// ASR 原始文本（provenance；润色后保留）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// 一张截图 + 展示期间的语音。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub t: f64,
    /// 本截图覆盖的结束时间（下一张截图起点；最后一段为媒体时长）
    #[serde(default)]
    pub end: f64,
    pub image: String,
    pub speech: Vec<TranscriptEvent>,
}

const PARAGRAPH_GAP_SECS: f64 = 3.5;
const MAX_PARAGRAPH_CHARS: usize = 420;

/// timeline.jsonl 的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TimelineEvent {
    Frame(FrameEvent),
    Speech(TranscriptEvent),
}

/// 合并算法：每条语音按中点归属「时间 ≤ 中点的最后一张截图」；首张之前的归首张。
///
/// 语音文本不会在截图边界按字符比例拆分。字符位置和语音时间并不一一对应，
/// 这样的拆分会把完整句子截断，降低图文笔记的可读性。跨越边界的语音段保持完整，
/// 并按其中点归属到最贴近的截图。
pub fn merge(
    frames: Vec<FrameEvent>,
    speech: Vec<TranscriptEvent>,
    media_end: f64,
) -> Vec<Section> {
    if frames.is_empty() {
        return vec![];
    }
    let mut sections: Vec<Section> = frames
        .into_iter()
        .map(|f| Section {
            t: f.t,
            end: 0.0,
            image: f.image,
            speech: vec![],
        })
        .collect();
    // 每段的结束时间 = 下一段起点；末段用媒体时长（未知时退化为自身起点）
    for i in 0..sections.len() {
        let next = sections.get(i + 1).map(|s| s.t).unwrap_or(f64::INFINITY);
        sections[i].end = next.min(media_end.max(sections[i].t));
    }
    for ev in speech {
        let mid = (ev.start + ev.end) / 2.0;
        let idx = match sections[..].binary_search_by(|s| s.t.partial_cmp(&mid).unwrap()) {
            Ok(i) => i,
            Err(0) => 0, // 首张截图之前
            Err(i) => i - 1,
        };
        sections[idx].speech.push(ev);
    }
    sections
}

/// 将同一截图下连续的 ASR 片段组织为可阅读的段落。
///
/// 此函数只作用于供渲染和可选 LLM 校对使用的 Section；调用方应在此之前将
/// 细粒度 ASR 事件写入 timeline.jsonl，以保留原始时间线和可追溯性。
/// 独立的无语义填充词不会单独生成段落，但嵌在有效语句中的文本不会被删除。
pub fn coalesce_sections(sections: &mut [Section]) {
    for section in sections {
        let mut paragraphs = Vec::new();
        let mut current: Option<TranscriptEvent> = None;

        for event in std::mem::take(&mut section.speech) {
            let text = event.text.trim();
            if text.is_empty() || is_standalone_filler(text) {
                continue;
            }

            let should_break = current.as_ref().is_some_and(|paragraph| {
                event.start - paragraph.end > PARAGRAPH_GAP_SECS
                    || paragraph.text.chars().count() + text.chars().count() > MAX_PARAGRAPH_CHARS
            });
            if should_break {
                paragraphs.push(current.take().expect("paragraph exists when breaking"));
            }

            match current.as_mut() {
                Some(paragraph) => {
                    append_text(&mut paragraph.text, text);
                    paragraph.end = event.end;
                }
                None => {
                    current = Some(TranscriptEvent {
                        start: event.start,
                        end: event.end,
                        text: text.to_string(),
                        raw: None,
                    });
                }
            }
        }

        if let Some(paragraph) = current {
            paragraphs.push(paragraph);
        }
        section.speech = paragraphs;
    }
}

fn append_text(paragraph: &mut String, next: &str) {
    let previous_is_word = paragraph
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    let next_is_word = next
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    if previous_is_word && next_is_word {
        paragraph.push(' ');
    }
    paragraph.push_str(next);
}

fn is_standalone_filler(text: &str) -> bool {
    let normalized = text.trim_matches(|c: char| {
        c.is_whitespace() || "，。！？、,.!?：:；;“”‘’'\"（）()【】[]".contains(c)
    });
    matches!(normalized, "嗯" | "呃" | "额" | "啊" | "哦" | "唔")
}

pub fn write_jsonl(path: &Path, frames: &[FrameEvent], speech: &[TranscriptEvent]) -> Result<()> {
    use std::io::Write;
    let mut events: Vec<TimelineEvent> = vec![];
    events.extend(frames.iter().cloned().map(TimelineEvent::Frame));
    events.extend(speech.iter().cloned().map(TimelineEvent::Speech));
    events.sort_by(|a, b| {
        let (ta, tb) = (time_of(a), time_of(b));
        ta.partial_cmp(&tb).unwrap()
    });
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for ev in &events {
        serde_json::to_writer(&mut f, ev)?;
        f.write_all(b"\n")?;
    }
    Ok(())
}

fn time_of(e: &TimelineEvent) -> f64 {
    match e {
        TimelineEvent::Frame(f) => f.t,
        TimelineEvent::Speech(s) => s.start,
    }
}

/// 全量结构化输出（structured.json 的主体）。
/// schema_version 标记字段语义版本；后续任何破坏性变更必须递增此值。
#[derive(Debug, Serialize)]
pub struct CourseDoc<'a> {
    pub schema_version: u32,
    pub generator: Generator,
    pub meta: &'a VideoMeta,
    pub sections: &'a [Section],
}

#[derive(Debug, Serialize)]
pub struct Generator {
    pub name: &'static str,
    pub version: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: f64) -> FrameEvent {
        FrameEvent {
            t,
            image: format!("f{t}.jpg"),
        }
    }

    fn sp(start: f64, end: f64) -> TranscriptEvent {
        TranscriptEvent {
            start,
            end,
            text: format!("{start}-{end}"),
            raw: None,
        }
    }

    #[test]
    fn merge_assigns_complete_speech_by_midpoint() {
        let frames = vec![frame(0.0), frame(60.0), frame(120.0)];
        let speech = vec![sp(10.0, 20.0), sp(50.0, 70.0), sp(5.0, 8.0)];
        let s = merge(frames, speech, 120.0);
        assert_eq!(s[0].speech.len(), 2);
        assert_eq!(s[1].speech.len(), 1);
        assert_eq!(s[1].speech[0].text, "50-70");
        assert_eq!(s[1].speech[0].start, 50.0);
        assert_eq!(s[1].speech[0].end, 70.0);
        assert!(merge(vec![], vec![sp(1.0, 2.0)], 10.0).is_empty());
    }

    #[test]
    fn section_end_is_next_frame_start_or_media_end() {
        let frames = vec![frame(0.0), frame(60.0)];
        let s = merge(frames, vec![], 95.0);
        assert_eq!(s[0].end, 60.0);
        assert_eq!(s[1].end, 95.0);
    }

    #[test]
    fn coalesce_sections_groups_speech_without_losing_sentence_boundaries() {
        let mut sections = vec![Section {
            t: 0.0,
            image: "f0.jpg".into(),
            speech: vec![
                TranscriptEvent {
                    start: 0.0,
                    end: 0.4,
                    text: "嗯。".into(),
                    raw: None,
                },
                TranscriptEvent {
                    start: 0.8,
                    end: 1.5,
                    text: "Flash 和硬盘呢，".into(),
                    raw: None,
                },
                TranscriptEvent {
                    start: 1.7,
                    end: 3.0,
                    text: "它可以长期地保存内容。".into(),
                    raw: None,
                },
                TranscriptEvent {
                    start: 7.0,
                    end: 8.0,
                    text: "接下来讲层次结构。".into(),
                    raw: None,
                },
            ],
        }];

        coalesce_sections(&mut sections);
        assert_eq!(sections[0].speech.len(), 2);
        assert_eq!(
            sections[0].speech[0].text,
            "Flash 和硬盘呢，它可以长期地保存内容。"
        );
        assert_eq!(sections[0].speech[0].start, 0.8);
        assert_eq!(sections[0].speech[0].end, 3.0);
    }

    #[test]
    fn coalesce_sections_keeps_spaces_between_english_words() {
        let mut sections = vec![Section {
            t: 0.0,
            image: "f0.jpg".into(),
            speech: vec![
                TranscriptEvent {
                    start: 0.0,
                    end: 1.0,
                    text: "random access".into(),
                    raw: None,
                },
                TranscriptEvent {
                    start: 1.1,
                    end: 2.0,
                    text: "memory".into(),
                    raw: None,
                },
            ],
        }];

        coalesce_sections(&mut sections);
        assert_eq!(sections[0].speech[0].text, "random access memory");
    }
}
