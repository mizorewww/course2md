//! 平台字幕作为转写源：SRT/VTT 解析、sidecar 查找、语言偏好挑选。
//!
//! 与 ASR 产物统一到 `TranscriptEvent`，下游 timeline/LLM/渲染完全复用。

use crate::timeline::TranscriptEvent;
use std::path::{Path, PathBuf};

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
    Some((parse_ts(a)?, parse_ts(end)?))
}

fn parse_ts(s: &str) -> Option<f64> {
    let s = s.trim();
    let (main, frac) = match s.split_once([',', '.']) {
        Some((m, f)) => (m, f),
        None => (s, ""),
    };
    let parts: Vec<u64> = main
        .split(':')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<Vec<_>>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        [s] => (0, 0, *s),
        _ => return None,
    };
    if (parts.len() >= 2 && sec >= 60) || (parts.len() == 3 && m >= 60) {
        return None;
    }
    let ms = if frac.is_empty() {
        0.0
    } else {
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        format!("0.{frac}").parse::<f64>().ok()?
    };
    Some(h as f64 * 3600.0 + m as f64 * 60.0 + sec as f64 + ms)
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

/// yt-dlp `--sub-langs` 的语言偏好参数，fetch.rs 抓取字幕时引用；
/// 与下方 `lang_rank` 的挑选顺序同源（zh 优先、en 其次），改动时需同步检查。
pub(crate) const SUB_LANGS: &str = "zh.*,en.*";

/// 在目录里挑选字幕文件（`sub.<lang>.srt`）：zh* 优先，其次 en*，再任意。
/// 只认 `.srt`：依赖 fetch.rs 抓取时 `--convert-subs srt` 的约定（两边需保持一致）。
pub fn pick_subtitle_file(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "srt"))
        .collect();
    if files.is_empty() {
        return None;
    }
    files.sort_by_key(|p| lang_rank(p));
    files.into_iter().next()
}

/// 语言排名：与 `SUB_LANGS`（fetch.rs `--sub-langs`）同源，zh 优先、en 其次。
fn lang_rank(p: &Path) -> (u8, String) {
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let lang = stem.rsplit('.').next().unwrap_or_default();
    let rank = if lang.starts_with("zh") {
        0
    } else if lang.starts_with("en") {
        1
    } else {
        2
    };
    (rank, stem)
}

/// 本地视频的同名字幕 sidecar：`lecture.mp4` → `lecture.srt` / `lecture.vtt`。
pub fn sidecar_subtitle(video: &Path) -> Option<PathBuf> {
    for ext in ["srt", "vtt"] {
        let p = video.with_extension(ext);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_words_after_a_pause_remain_separate() {
        let events = parse_subtitle(
            "1\n00:00:01,000 --> 00:00:02,000\nhello\n\n2\n00:00:10,000 --> 00:00:11,000\nhello\n",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].end, 2.0);
        assert!(parse_ts("00:00:01.bad").is_none());
        assert!(parse_ts("00:99:01.000").is_none());
        assert!(parse_subtitle("00:00:02,000 --> 00:00:01,000\nbackwards\n").is_empty());
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
        let _ = std::fs::remove_dir_all(&d);
    }
}
