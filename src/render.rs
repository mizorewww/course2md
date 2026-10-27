//! 笔记渲染：Markdown 与独立 HTML。

use crate::fetch::VideoMeta;
use crate::timeline::Section;
use std::fmt::Write as _;
use std::path::Path;

/// mm:ss 或 h:mm:ss
pub fn fmt_ts(sec: f64) -> String {
    let s = sec.max(0.0).floor() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn source_url(meta: &VideoMeta) -> String {
    let source = meta.webpage_url.trim();
    if meta.extractor == "local" {
        let path = Path::new(source)
            .canonicalize()
            .unwrap_or_else(|_| source.into());
        if let Ok(url) = url::Url::from_file_path(path) {
            return url.into();
        }
    }
    source.to_string()
}

/// Existing timestamps and fragments must not mask the requested seek time.
/// `source` is resolved by the caller (canonicalize runs once per render,
/// not once per section).
fn ts_url_from(source: &str, sec: f64) -> String {
    let Some(mut url) = parse_url(source) else {
        return source.to_string();
    };
    let seconds = (sec.max(0.0).floor() as u64).to_string();
    if url.scheme() == "file" {
        url.set_fragment(Some(&format!("t={seconds}")));
    } else {
        let pairs: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key != "t")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair("t", &seconds);
        url.set_fragment(None);
    }
    url.into()
}

/// md 侧的最小转义策略：标题/作者等内联元信息只 strip 换行（换行会截断 ATX
/// 标题与列表行）与行首 `#`（防伪造标题）。其余 markdown 特殊字符不转义——
/// 最坏是渲染偏差，不破坏文档结构；html 侧则由 esc 全量转义。
fn md_inline(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
        .trim_start_matches('#')
        .to_string()
}

/// Link text: a Windows path's backslashes (`C:\a\[1].mp4`) and brackets
/// would otherwise act as Markdown escapes or end the text early.
fn md_link_text(s: &str) -> String {
    let mut out = String::new();
    for c in md_inline(s).chars() {
        if matches!(c, '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A URL, but not a Windows path: `C:\a.mp4` parses as a URL with scheme `c`.
fn parse_url(value: &str) -> Option<url::Url> {
    url::Url::parse(value)
        .ok()
        .filter(|url| url.scheme().len() > 1)
}

/// Link destination: a URL as is; anything else (a path that is not a file
/// URL here, such as a note made on another system) inside `<...>`, so spaces
/// and parentheses stay part of the destination.
fn md_link_target(target: &str) -> String {
    if parse_url(target).is_some() || !target.contains([' ', '(', ')', '<', '>']) {
        target.to_string()
    } else {
        format!("<{}>", target.replace(['<', '>'], ""))
    }
}

pub fn render_markdown(meta: &VideoMeta, sections: &[Section]) -> String {
    let title = md_inline(&meta.title);
    let uploader = if meta.uploader.is_empty() {
        String::new()
    } else {
        format!("- {}\n", crate::tr!("作者 {}", md_inline(&meta.uploader)))
    };
    // 本地来源的 canonicalize 每次渲染只算一次
    let source = source_url(meta);
    let mut md = String::new();
    // 写 String 不会失败，unwrap 安全
    write!(md, "# {title}\n\n").unwrap();
    md.push_str(&uploader);
    if meta.duration > 0. {
        writeln!(md, "- {}", crate::tr!("时长 {}", fmt_ts(meta.duration))).unwrap();
    }
    if !meta.webpage_url.is_empty() {
        writeln!(
            md,
            "- {} [{}]({})",
            crate::tr!("来源"),
            md_link_text(&meta.webpage_url),
            md_link_target(&source)
        )
        .unwrap();
    }
    writeln!(
        md,
        "- {} · {}\n",
        crate::tr!("由 course2md 生成"),
        crate::tr!(
            "{} 张截图 / {} 段文字",
            sections.iter().filter(|s| !s.image.is_empty()).count(),
            sections.iter().map(|s| s.speech.len()).sum::<usize>()
        )
    )
    .unwrap();
    md.push_str("---\n\n");
    for s in sections {
        if meta.webpage_url.is_empty() {
            write!(md, "## {}\n\n", fmt_ts(s.t)).unwrap();
        } else {
            write!(
                md,
                "## [{}]({})\n\n",
                fmt_ts(s.t),
                md_link_target(&ts_url_from(&source, s.t))
            )
            .unwrap();
        }
        if !s.image.is_empty() {
            write!(
                md,
                "![{}]({})\n\n",
                crate::tr!("视频 {} 的截图", fmt_ts(s.t)),
                s.image
            )
            .unwrap();
        }
        for ev in &s.speech {
            write!(md, "{}\n\n", ev.text).unwrap();
            // Bilingual notes quote the translation under its original.
            if let Some(translation) = &ev.translation {
                write!(md, "> {}\n\n", md_inline(translation)).unwrap();
            }
        }
    }
    md
}

pub fn render_html(meta: &VideoMeta, sections: &[Section]) -> String {
    let mut body = String::new();
    let mut details = Vec::new();
    // 本地来源的 canonicalize 每次渲染只算一次
    let source = source_url(meta);
    if !meta.uploader.is_empty() {
        details.push(crate::tr!("作者 {}", esc(&meta.uploader)));
    }
    if meta.duration > 0. {
        details.push(crate::tr!("时长 {}", fmt_ts(meta.duration)));
    }
    if !meta.webpage_url.is_empty() {
        details.push(format!(
            "<a href=\"{}\">{}</a>",
            esc(&source),
            esc(crate::tr!("源视频"))
        ));
    }
    details.push(crate::tr!(
        "{} 张截图 / {} 段文字",
        sections.iter().filter(|s| !s.image.is_empty()).count(),
        sections.iter().map(|s| s.speech.len()).sum::<usize>()
    ));
    writeln!(
        body,
        "<header><h1>{}</h1><p>{}</p></header>",
        esc(&meta.title),
        details.join(" · ")
    )
    .unwrap();
    for s in sections {
        write!(body, "<section id=\"t{}\"><h2>", s.t.floor() as u64).unwrap();
        if meta.webpage_url.is_empty() {
            write!(body, "{}", esc(&fmt_ts(s.t))).unwrap();
        } else {
            write!(
                body,
                "<a href=\"{}\" target=\"_blank\">{}</a>",
                esc(&ts_url_from(&source, s.t)),
                esc(&fmt_ts(s.t))
            )
            .unwrap();
        }
        body.push_str("</h2>\n");
        if !s.image.is_empty() {
            writeln!(
                body,
                "<img loading=\"lazy\" src=\"{}\" alt=\"视频 {} 的截图\">",
                esc(&s.image),
                esc(&fmt_ts(s.t))
            )
            .unwrap();
        }
        for ev in &s.speech {
            writeln!(body, "<p>{}</p>", rich(&ev.text)).unwrap();
            if let Some(translation) = &ev.translation {
                writeln!(body, "<p class=\"translation\">{}</p>", rich(translation)).unwrap();
            }
        }
        body.push_str("</section>\n");
    }
    format!(
        "<!DOCTYPE html>\n<html lang=\"zh\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{title}</title>\n<style>\n:root {{ color-scheme: light dark; }}\nbody {{ max-width: 920px; margin: 0 auto; padding: 1rem; font-family: system-ui, -apple-system, sans-serif; line-height: 1.7; }}\nheader h1 {{ font-size: 1.5rem; }}\nheader p {{ color: #888; }}\nsection {{ margin: 2rem 0; }}\nsection h2 {{ font-size: 1rem; font-weight: 600; }}\nsection h2 a {{ color: #0969da; text-decoration: none; }}\nimg {{ max-width: 100%; border: 1px solid #8884; border-radius: 6px; }}\np {{ margin: .4rem 0; }}\np.translation {{ color: #777; margin-top: 0; }}\n.math {{ display: inline-block; }}\n.math svg {{ display: block; }}\n.math.display {{ display: block; margin: .6rem auto; max-width: 100%; }}\n</style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        title = esc(&meta.title),
    )
}

/// Note text with its formulas typeset; everything else escaped.
pub(crate) fn rich(s: &str) -> String {
    crate::math::html(s, esc)
}

pub(crate) fn esc(s: &str) -> String {
    // 单趟扫描：四趟 replace 会反复分配与搬移整串
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_source_link_keeps_windows_paths_literal() {
        assert_eq!(
            md_link_text(r"C:\课程\[1] a_b.mp4"),
            r"C:\\课程\\\[1\] a_b.mp4"
        );
        assert_eq!(
            md_link_target("https://example.com/v?id=1"),
            "https://example.com/v?id=1"
        );
        assert_eq!(
            md_link_target("file:///C:/%E8%AF%BE/a.mp4"),
            "file:///C:/%E8%AF%BE/a.mp4"
        );
        assert_eq!(md_link_target("lectures/a (1).mp4"), "<lectures/a (1).mp4>");
        // A Windows path read on another system parses as a URL with scheme "c".
        assert_eq!(md_link_target(r"C:\a b\(1).mp4"), r"<C:\a b\(1).mp4>");
        assert_eq!(ts_url_from(r"C:\a b.mp4", 65.), r"C:\a b.mp4");
    }
    use crate::timeline::TranscriptEvent;

    #[test]
    fn timestamp_replaces_existing_seek_and_encodes_local_paths() {
        let mut meta = VideoMeta {
            title: "test".into(),
            uploader: String::new(),
            duration: 10.0,
            webpage_url: "https://www.youtube.com/watch?v=abc&t=99#old".into(),
            extractor: "youtube".into(),
            id: "abc".into(),
        };
        assert_eq!(
            ts_url_from(&source_url(&meta), 4.0),
            "https://www.youtube.com/watch?v=abc&t=4"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a lesson.mp4");
        std::fs::write(&path, b"video").unwrap();
        meta.webpage_url = path.display().to_string();
        meta.extractor = "local".into();
        let link = ts_url_from(&source_url(&meta), 4.0);
        assert!(link.starts_with("file://"));
        assert!(link.ends_with("a%20lesson.mp4#t=4"));
    }

    #[test]
    fn render_basics() {
        let m = VideoMeta {
            title: "测试<课>".into(),
            uploader: "up".into(),
            duration: 3725.0,
            webpage_url: "https://www.bilibili.com/video/BV1xx".into(),
            extractor: "bilibili".into(),
            id: "BV1xx".into(),
        };
        assert_eq!(fmt_ts(65.4), "01:05");
        assert_eq!(fmt_ts(3725.0), "1:02:05");
        assert_eq!(
            ts_url_from(&source_url(&m), 61.9),
            "https://www.bilibili.com/video/BV1xx?t=61"
        );
        let s = [Section {
            t: 10.0,
            end: 60.0,
            image: "frames/slide_0001.jpg".into(),
            speech: vec![TranscriptEvent {
                start: 10.2,
                end: 12.5,
                text: "你好".into(),
                raw: None,
                translation: None,
            }],
        }];
        let md = render_markdown(&m, &s);
        assert!(md.contains("你好") && md.contains("frames/slide_0001.jpg"));
        let html = render_html(&m, &s);
        assert!(html.contains("&lt;课&gt;"));
        assert!(
            html.contains("<p>你好</p>"),
            "正文直接成段，不再用「」对话包裹"
        );
        assert!(!html.contains("「你好」"));
    }
}
