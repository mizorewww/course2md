//! 输出渲染：course.md / course.html / structured.json。

use crate::fetch::VideoMeta;
use crate::timeline::Section;
use anyhow::Result;
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
pub fn ts_url(meta: &VideoMeta, sec: f64) -> String {
    let source = source_url(meta);
    let Ok(mut url) = url::Url::parse(&source) else {
        return source;
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

pub fn render_markdown(meta: &VideoMeta, sections: &[Section]) -> String {
    let title = md_inline(&meta.title);
    let uploader = md_inline(if meta.uploader.is_empty() {
        "未知"
    } else {
        &meta.uploader
    });
    let mut md = String::new();
    // 写 String 不会失败，unwrap 安全
    write!(md, "# {title}\n\n").unwrap();
    write!(
        md,
        "- 作者：{uploader}\n- 时长：{}\n- 来源：[{}]({})\n- 由 course2md 生成（{} 张截图 / {} 段语音）\n\n",
        fmt_ts(meta.duration),
        meta.webpage_url,
        source_url(meta),
        sections.len(),
        sections.iter().map(|s| s.speech.len()).sum::<usize>(),
    )
    .unwrap();
    md.push_str("---\n\n");
    for s in sections {
        write!(
            md,
            "## [{}]({})\n\n![{}]({})\n\n",
            fmt_ts(s.t),
            ts_url(meta, s.t),
            fmt_ts(s.t),
            s.image
        )
        .unwrap();
        if s.speech.is_empty() {
            md.push_str("_(本段无语音)_\n\n");
        } else {
            for ev in &s.speech {
                write!(md, "{}\n\n", ev.text).unwrap();
            }
        }
    }
    md
}

pub fn render_html(meta: &VideoMeta, sections: &[Section]) -> String {
    let mut body = String::new();
    writeln!(
        body,
        "<header><h1>{}</h1><p>作者 {} · 时长 {} · <a href=\"{}\">源视频</a> · {} 张截图 / {} 段语音</p></header>",
        esc(&meta.title),
        esc(if meta.uploader.is_empty() { "未知" } else { &meta.uploader }),
        fmt_ts(meta.duration),
        esc(&source_url(meta)),
        sections.len(),
        sections.iter().map(|s| s.speech.len()).sum::<usize>(),
    )
    .unwrap();
    for s in sections {
        write!(
            body,
            "<section id=\"t{ts}\"><h2><a href=\"{url}\" target=\"_blank\">[{t}]</a></h2>\n<a href=\"{url}\" target=\"_blank\"><img loading=\"lazy\" src=\"{img}\" alt=\"{t}\"></a>\n",
            ts = s.t.floor() as u64,
            url = esc(&ts_url(meta, s.t)),
            t = esc(&fmt_ts(s.t)),
            img = esc(&s.image),
        )
        .unwrap();
        if s.speech.is_empty() {
            body.push_str("<p class=\"mute\">（本段无语音）</p>\n");
        } else {
            for ev in &s.speech {
                writeln!(body, "<p>{}</p>", esc(&ev.text)).unwrap();
            }
        }
        body.push_str("</section>\n");
    }
    format!(
        "<!DOCTYPE html>\n<html lang=\"zh\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{title}</title>\n<style>\n:root {{ color-scheme: light dark; }}\nbody {{ max-width: 920px; margin: 0 auto; padding: 1rem; font-family: system-ui, -apple-system, sans-serif; line-height: 1.7; }}\nheader h1 {{ font-size: 1.5rem; }}\nheader p {{ color: #888; }}\nsection {{ margin: 2rem 0; }}\nsection h2 {{ font-size: 1rem; font-weight: 600; }}\nsection h2 a {{ color: #0969da; text-decoration: none; }}\nimg {{ max-width: 100%; border: 1px solid #8884; border-radius: 6px; }}\np {{ margin: .4rem 0; }}\np.mute {{ color: #888; }}\n</style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        title = esc(&meta.title),
    )
}

pub fn render_json(meta: &VideoMeta, sections: &[Section]) -> Result<String> {
    Ok(serde_json::to_string_pretty(&crate::timeline::CourseDoc {
        schema_version: 1,
        generator: crate::timeline::Generator {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        meta,
        sections,
    })?)
}

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 按 formats 集合写文件；summary 非空时插入 md/html（元信息之后）。
pub async fn write_outputs(
    out_dir: &Path,
    meta: &VideoMeta,
    sections: &[Section],
    formats: &[crate::config::OutputFormat],
    summary: Option<&crate::summarize::Summary>,
) -> Result<()> {
    for f in formats {
        match f {
            crate::config::OutputFormat::Md => {
                let mut md = render_markdown(meta, sections);
                if let Some(sm) = summary {
                    md = crate::summarize::insert_into_md(&md, sm);
                }
                tokio::fs::write(out_dir.join("course.md"), md).await?;
            }
            crate::config::OutputFormat::Html => {
                let mut html = render_html(meta, sections);
                if let Some(sm) = summary {
                    html = crate::summarize::insert_into_html(&html, sm);
                }
                tokio::fs::write(out_dir.join("course.html"), html).await?;
            }
            crate::config::OutputFormat::Json => {
                tokio::fs::write(
                    out_dir.join("structured.json"),
                    render_json(meta, sections)?,
                )
                .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            ts_url(&meta, 4.0),
            "https://www.youtube.com/watch?v=abc&t=4"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a lesson.mp4");
        std::fs::write(&path, b"video").unwrap();
        meta.webpage_url = path.display().to_string();
        meta.extractor = "local".into();
        let link = ts_url(&meta, 4.0);
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
            ts_url(&m, 61.9),
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
