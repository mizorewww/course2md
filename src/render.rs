//! 输出渲染：course.md / course.html / structured.json。

use crate::fetch::VideoMeta;
use crate::timeline::Section;
use anyhow::Result;
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

/// 指向源视频某时刻的 URL（bilibili/youtube 的 t 参数语义一致）。
pub fn ts_url(meta: &VideoMeta, sec: f64) -> String {
    let base = meta.webpage_url.trim();
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}t={}", sec.floor() as u64)
}

pub fn render_markdown(meta: &VideoMeta, sections: &[Section]) -> String {
    let mut md = String::new();
    md.push_str(&format!("# {}\n\n", meta.title));
    md.push_str(&format!(
        "- 作者：{}\n- 时长：{}\n- 来源：[{}]({})\n- 由 course2md 生成（{} 张截图 / {} 段语音）\n\n",
        if meta.uploader.is_empty() { "未知" } else { &meta.uploader },
        fmt_ts(meta.duration),
        meta.webpage_url,
        meta.webpage_url,
        sections.len(),
        sections.iter().map(|s| s.speech.len()).sum::<usize>(),
    ));
    md.push_str("---\n\n");
    for s in sections {
        md.push_str(&format!(
            "## [{}]({})\n\n![{}]({})\n\n",
            fmt_ts(s.t),
            ts_url(meta, s.t),
            fmt_ts(s.t),
            s.image
        ));
        if s.speech.is_empty() {
            md.push_str("_(本段无语音)_\n\n");
        } else {
            for paragraph in &s.speech {
                md.push_str(&format!("{}\n\n", paragraph.text));
            }
        }
    }
    md
}

pub fn render_html(meta: &VideoMeta, sections: &[Section]) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "<header><h1>{}</h1><p>作者 {} · 时长 {} · <a href=\"{}\">源视频</a> · {} 张截图 / {} 段语音</p></header>\n",
        esc(&meta.title),
        esc(if meta.uploader.is_empty() { "未知" } else { &meta.uploader }),
        fmt_ts(meta.duration),
        esc(&meta.webpage_url),
        sections.len(),
        sections.iter().map(|s| s.speech.len()).sum::<usize>(),
    ));
    for s in sections {
        body.push_str(&format!(
            "<section id=\"t{ts}\"><h2><a href=\"{url}\" target=\"_blank\">[{t}]</a></h2>\n<a href=\"{url}\" target=\"_blank\"><img loading=\"lazy\" src=\"{img}\" alt=\"{t}\"></a>\n",
            ts = s.t.floor() as u64,
            url = esc(&ts_url(meta, s.t)),
            t = esc(&fmt_ts(s.t)),
            img = esc(&s.image),
        ));
        if s.speech.is_empty() {
            body.push_str("<p class=\"mute\">（本段无语音）</p>\n");
        } else {
            for paragraph in &s.speech {
                body.push_str(&format!("<p>{}</p>\n", esc(&paragraph.text)));
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

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 按 formats 集合写文件。
pub async fn write_outputs(
    out_dir: &Path,
    meta: &VideoMeta,
    sections: &[Section],
    formats: &[crate::config::OutputFormat],
) -> Result<()> {
    for f in formats {
        match f {
            crate::config::OutputFormat::Md => {
                tokio::fs::write(out_dir.join("course.md"), render_markdown(meta, sections))
                    .await?;
            }
            crate::config::OutputFormat::Html => {
                tokio::fs::write(out_dir.join("course.html"), render_html(meta, sections)).await?;
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
        assert!(render_html(&m, &[]).contains("&lt;课&gt;"));
    }
    #[test]
    fn html_renders_prose_without_dialogue_quotes() {
        let m = VideoMeta {
            title: "测试".into(),
            uploader: "up".into(),
            duration: 10.0,
            webpage_url: "https://example.com/video".into(),
            extractor: "test".into(),
            id: "1".into(),
        };
        let s = [Section {
            t: 0.0,
            end: 10.0,
            image: "frames/slide_0001.jpg".into(),
            speech: vec![TranscriptEvent {
                start: 0.0,
                end: 1.0,
                text: "正常段落。".into(),
                raw: None,
            }],
        }];
        let html = render_html(&m, &s);
        assert!(html.contains("<p>正常段落。</p>"));
        assert!(!html.contains("「正常段落。"));
    }
}
