//! A summary as part of a note: its Markdown and HTML blocks and where they go.

use crate::meta::Summary;
use crate::tr;

/// 摘要区块哨兵注释（md/html 通用；HTML 注释在两种渲染产物中都原样保留）。
/// 幂等判断与原地替换均以哨兵为准。
const SUMMARY_BEGIN: &str = "<!-- course2md:summary -->";

const SUMMARY_END: &str = "<!-- /course2md:summary -->";

/// 生成插入 course.md 的摘要区块（markdown，哨兵注释包裹）。
/// 有意信任 LLM 输出（markdown 语义直通），不做转义。
pub fn render_md_block(sm: &Summary) -> String {
    let mut out = format!("\n{SUMMARY_BEGIN}\n\n## 📝 {}\n\n", tr!("视频摘要"));
    out.push_str(&format!("> {}\n", sm.tldr));
    if !sm.key_points.is_empty() {
        out.push_str(&format!("\n### {}\n\n", tr!("核心要点")));
        for p in &sm.key_points {
            out.push_str(&format!("- {p}\n"));
        }
    }
    if !sm.outline.is_empty() {
        out.push_str(&format!("\n### {}\n\n", tr!("内容大纲")));
        for o in &sm.outline {
            out.push_str(&format!(
                "- **{}** {}：{}\n",
                crate::timeline::fmt_ts(o.t),
                o.title,
                o.detail
            ));
        }
    }
    out.push_str(&format!("\n{SUMMARY_END}\n"));
    out
}

/// 生成插入 course.html 的摘要区块（HTML，哨兵注释包裹）。
pub fn render_html_block(sm: &Summary) -> String {
    let mut out = format!(
        "{SUMMARY_BEGIN}<section class=\"summary\"><h2>📝 {}</h2>",
        crate::render::esc(tr!("视频摘要"))
    );
    out.push_str(&format!(
        "<p class=\"mute\">{}</p>",
        crate::render::rich(&sm.tldr)
    ));
    if !sm.key_points.is_empty() {
        out.push_str(&format!(
            "<h3>{}</h3><ul>",
            crate::render::esc(tr!("核心要点"))
        ));
        for p in &sm.key_points {
            out.push_str(&format!("<li>{}</li>", crate::render::rich(p)));
        }
        out.push_str("</ul>");
    }
    if !sm.outline.is_empty() {
        out.push_str(&format!(
            "<h3>{}</h3><ul>",
            crate::render::esc(tr!("内容大纲"))
        ));
        for o in &sm.outline {
            out.push_str(&tr!(
                "<li><b>{}</b> {}：{}</li>",
                crate::render::esc(&crate::timeline::fmt_ts(o.t)),
                crate::render::rich(&o.title),
                crate::render::rich(&o.detail)
            ));
        }
        out.push_str("</ul>");
    }
    out.push_str(&format!("</section>{SUMMARY_END}"));
    out
}

/// 把摘要区块插入文档：已有摘要块时按哨兵原地替换（幂等）；缺闭合哨兵时保守不改（告警）；
/// 否则 `position` 决定首次插入的字节位置，默认文末。
fn insert_block(doc: &str, block: &str, position: Option<usize>) -> String {
    if let Some(start) = doc.find(SUMMARY_BEGIN) {
        let Some(end) = doc[start..].find(SUMMARY_END) else {
            tracing::warn!(
                "已有摘要的格式不完整，保留原文 / Existing summary markup is incomplete; original text kept"
            );
            return doc.to_string();
        };
        let end = start + end + SUMMARY_END.len();
        return format!("{}{block}{}", &doc[..start], &doc[end..]);
    }
    let mut out = doc.to_string();
    out.insert_str(position.unwrap_or(doc.len()), block);
    out
}

/// 首次插入定位到元信息之后、首个字幕小节（## [mm:ss]）之前。
pub fn insert_into_md(md: &str, sm: &Summary) -> String {
    insert_block(md, &render_md_block(sm), md.find("\n## ["))
}

/// 首次插入定位到 </header> 之后（兜底 </body> 前 / 文末）。
pub fn insert_into_html(html: &str, sm: &Summary) -> String {
    let position = html
        .find("</header>")
        .map(|at| at + "</header>".len())
        .or_else(|| html.rfind("</body>"));
    insert_block(html, &render_html_block(sm), position)
}

/// 生成独立摘要文件（markdown），用于 -o 导出。
pub fn render_standalone_md(title: &str, sm: &Summary) -> String {
    let mut out = format!("# {title}\n\n");
    out.push_str(&render_md_block(sm));
    out
}

/// 导出文件名净化（= 共享净化原语，空名回退 "summary"）。
/// 与 config::sanitize_component 同一规则。
pub fn sanitize_filename(name: &str) -> String {
    crate::config::sanitize_filename_with_fallback(name, "summary")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::OutlineItem;

    #[test]
    fn md_insert_places_summary_before_the_body() {
        let md = "# 标题\n\n---\n\n## [00:00](u)\n\n正文\n";
        let sm = Summary {
            tldr: "概述".into(),
            key_points: vec!["要点一".into()],
            outline: vec![OutlineItem {
                t: 12.0,
                title: "章节".into(),
                detail: "内容".into(),
            }],
        };
        let with = insert_into_md(md, &sm);
        assert!(
            with.find(SUMMARY_BEGIN).unwrap() < with.find("## [00:00]").unwrap(),
            "摘要在正文前"
        );
        // 标题含「视频摘要」不当作已有摘要
        let titled = "# 视频摘要速览课\n\n## [00:00](u)\n";
        assert_eq!(
            insert_into_md(titled, &sm).matches(SUMMARY_BEGIN).count(),
            1
        );
    }

    #[test]
    fn html_insert_places_summary_after_the_header() {
        let html =
            "<html><body><header><h1>t</h1></header>\n<section><p>x</p></section>\n</body></html>";
        let sm = Summary {
            tldr: "t".into(),
            key_points: vec![],
            outline: vec![],
        };
        let with = insert_into_html(html, &sm);
        assert!(
            with.find("<section class=\"summary\">").unwrap() < with.find("<section>").unwrap()
        );
    }

    #[test]
    fn insert_is_idempotent_replace() {
        let md = "# 标题\n\n## [00:00](u)\n\n正文\n";
        let sm1 = Summary {
            tldr: "旧概述".into(),
            key_points: vec![],
            outline: vec![],
        };
        let sm2 = Summary {
            tldr: "新概述".into(),
            key_points: vec![],
            outline: vec![],
        };
        let once = insert_into_md(md, &sm1);
        let twice = insert_into_md(&once, &sm2);
        assert_eq!(
            twice.matches(SUMMARY_BEGIN).count(),
            1,
            "重复插入应原地替换而非追加"
        );
        assert!(twice.contains("新概述"));
        assert!(!twice.contains("旧概述"));
        assert!(twice.contains("## [00:00]"), "正文保留");
    }

    #[test]
    fn unclosed_sentinel_is_left_unchanged() {
        let md = "# 标题\n\n<!-- course2md:summary -->\n\n## 残缺块\n\n正文\n";
        let sm = Summary {
            tldr: "新".into(),
            key_points: vec![],
            outline: vec![],
        };
        assert_eq!(insert_into_md(md, &sm), md, "缺闭合哨兵时保守不改");
    }
}
