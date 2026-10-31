//! LLM 视频摘要：基于带时间戳字幕生成 TL;DR / 核心要点 / 内容大纲。
//!
//! 支持超长视频：字幕超过阈值时自动 map-reduce（分段摘要 → 合并）。
//! 幻觉防护：仅以字幕为输入、temperature=0、json_object 结构化输出、要点带时间戳可溯源。

use crate::fetch::VideoMeta;
use crate::llm::{self, LlmSettings};
use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// 直接单次摘要的最大字幕字符数。
/// 依据：中文场景 1 字符 ≈ 1-2 token（Qwen/GPT 系分词），25_000 字符 ≈ 2.5-5 万
/// token，加上 system/user prompt 与结构化输出，128K 上下文内仍留足余量；
/// 英文等低 token 密度语言则更宽松。超过即走 map-reduce。
const DIRECT_CHAR_LIMIT: usize = 25_000;
/// map-reduce 每个分块的字符上限。
const CHUNK_CHAR_LIMIT: usize = 25_000;
/// 每个字幕事件在 prompt 里的非文本开销（"[mm:ss] " 时间戳前缀 + 换行符）。
const PER_EVENT_OVERHEAD: usize = 16;
/// map-reduce 分段摘要的并发上限：LLM 端点普遍限流，取保守的 4 路。
const SUMMARIZE_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutlineItem {
    /// 章节起始秒数（绝对时间）
    pub t: f64,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub tldr: String,
    pub key_points: Vec<String>,
    pub outline: Vec<OutlineItem>,
}

/// 摘要区块哨兵注释（md/html 通用；HTML 注释在两种渲染产物中都原样保留）。
/// 幂等判断与原地替换均以哨兵为准。
const SUMMARY_BEGIN: &str = "<!-- course2md:summary -->";
const SUMMARY_END: &str = "<!-- /course2md:summary -->";

const SYSTEM_PROMPT: &str = "你是视频内容摘要助手。根据提供的带时间戳字幕为视频生成结构化摘要。\
严格要求：1) 只依据字幕内容，严禁编造字幕中不存在的事实、数字、人名或观点；\
2) 对不确定的信息宁可省略也不要猜测；3) 使用视频原语言输出；\
4) 只输出一个合法 JSON 对象，不要代码围栏、不要任何多余文字。";

/// The summary rules plus the shared formula convention, which a summary of a
/// mathematical lecture needs as much as its transcript does.
fn system_prompt() -> String {
    format!("{SYSTEM_PROMPT} {}", llm::MATH_RULE)
}

fn build_transcript(events: &[TranscriptEvent]) -> String {
    let mut out = String::new();
    for e in events {
        out.push_str(&format!(
            "[{}] {}\n",
            crate::render::fmt_ts(e.start),
            e.text
        ));
    }
    out
}

fn user_prompt(transcript: &str) -> String {
    format!(
        "以下是视频字幕（每行 [mm:ss] 为起始时间）：\n\n{transcript}\n\n\
请输出 JSON 对象：{{\"tldr\": \"不超过120字的一句话概述\", \
\"key_points\": [3-6条要点，每条不超过60字], \
\"outline\": [{{\"t\": 起始秒数(数字), \"title\": \"章节标题\", \"detail\": \"该章节内容简述，不超过100字\"}}]}}。\
outline 按时间顺序覆盖整个视频，3-8 节。"
    )
}

fn parse_time(v: Option<&serde_json::Value>) -> f64 {
    if let Some(n) = v.and_then(|x| x.as_f64()) {
        return n;
    }
    // 字符串形式（"01:30" / "120s" / "[90s]" 等）走共享的宽容解析；
    // 解析失败按 0.0 处理（outline 条目缺时间不至于丢弃整条摘要）
    v.and_then(|x| x.as_str())
        .and_then(crate::timeline::parse_timestamp)
        .unwrap_or(0.0)
}

pub(crate) fn parse_summary(content: &str) -> Option<Summary> {
    let parsed = llm::extract_json_object(content)?;
    let text = |v: &serde_json::Value, key: &str| {
        llm::restore_latex(v.get(key).and_then(|x| x.as_str()).unwrap_or_default())
            .trim()
            .to_string()
    };
    let list = |key: &str| {
        parsed
            .get(key)
            .and_then(|v| v.as_array())
            .map_or(&[][..], Vec::as_slice)
    };
    let tldr = text(&parsed, "tldr");
    let key_points: Vec<String> = list("key_points")
        .iter()
        .filter_map(|v| v.as_str())
        .map(|s| llm::restore_latex(s.trim()))
        .filter(|s| !s.is_empty())
        .collect();
    let outline: Vec<OutlineItem> = list("outline")
        .iter()
        .map(|v| OutlineItem {
            t: parse_time(v.get("t")),
            title: text(v, "title"),
            detail: text(v, "detail"),
        })
        .filter(|item| !item.title.is_empty() || !item.detail.is_empty())
        .collect();
    if tldr.is_empty() && key_points.is_empty() && outline.is_empty() {
        return None;
    }
    Some(Summary {
        tldr,
        key_points,
        outline,
    })
}

/// One requested summary scope. A protocol failure is retained, never hidden by another paid request.
fn request_summary(
    chat: &llm::Chat<'_>,
    s: &LlmSettings,
    user: &str,
    description: &str,
) -> Result<String> {
    let body = crate::provider::chat_body(s, &system_prompt(), user, llm::CHAT_MAX_TOKENS);
    chat.send(&body, description, &|content| {
        anyhow::ensure!(
            parse_summary(content).is_some(),
            crate::tr!("服务返回的摘要结构无效 / Invalid summary structure")
        );
        Ok(())
    })
    .context(crate::tr!("生成摘要请求失败 / Summary request failed"))
}

/// Show the actionable cause once. Provider bodies never reach these errors;
/// their safe status classification is produced by the request ledger.
pub fn failure_message(error: &anyhow::Error) -> String {
    let message = error
        .downcast_ref::<crate::dispatch::Failure>()
        .map(|failure| failure.message.clone())
        .unwrap_or_else(|| error.chain().last().unwrap_or(error.as_ref()).to_string());
    message.trim().to_string()
}

fn split_chunks(events: &[TranscriptEvent], char_limit: usize) -> Vec<Vec<TranscriptEvent>> {
    let mut chunks: Vec<Vec<TranscriptEvent>> = vec![];
    let mut cur: Vec<TranscriptEvent> = vec![];
    let mut cur_chars = 0usize;
    for e in events {
        let c = e.text.chars().count() + PER_EVENT_OVERHEAD;
        if !cur.is_empty() && cur_chars + c > char_limit {
            chunks.push(std::mem::take(&mut cur));
            cur_chars = 0;
        }
        cur.push(e.clone());
        cur_chars += c;
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// 视频元信息（标题/UP主）作为背景注入 prompt，提升摘要相关性；
/// 幻觉防护不变——SYSTEM_PROMPT 仍要求只依据字幕内容。
fn meta_context(meta: &VideoMeta) -> String {
    let mut s = String::new();
    if !meta.title.trim().is_empty() {
        s.push_str(&format!("视频标题：{}\n", meta.title.trim()));
    }
    if !meta.uploader.trim().is_empty() {
        s.push_str(&format!("UP主/作者：{}\n", meta.uploader.trim()));
    }
    if s.is_empty() { s } else { format!("{s}\n") }
}

/// 主入口：对全部字幕生成摘要；超长自动 map-reduce。
pub async fn summarize(
    s: &LlmSettings,
    events: &[TranscriptEvent],
    meta: &VideoMeta,
    control: &crate::dispatch::TaskControl,
) -> Result<Summary> {
    // 空转写直接报错：发给模型只会得到编造内容或报错，浪费请求
    if events.is_empty() {
        bail!(crate::tr!(
            "转写为空，无法摘要 / Transcript is empty; cannot summarize"
        ));
    }
    llm::validate(s)?;
    let (s, events, ctx, control) = (
        s.clone(),
        events.to_vec(),
        meta_context(meta),
        control.clone(),
    );
    tokio::task::spawn_blocking(move || summarize_blocking(&s, &events, &ctx, &control))
        .await
        .context(crate::tr!(
            "生成摘要时意外中断 / Summary generation was interrupted"
        ))?
}

fn summarize_blocking(
    s: &LlmSettings,
    events: &[TranscriptEvent],
    ctx: &str,
    control: &crate::dispatch::TaskControl,
) -> Result<Summary> {
    let chat = llm::Chat::new(s, llm::Purpose::Summary, control);
    let invalid = || crate::tr!("服务返回的摘要结构无效 / Invalid summary response");
    let range = |chunk: &[TranscriptEvent]| {
        let edge = |event: Option<&TranscriptEvent>, at: fn(&TranscriptEvent) -> f64| {
            event
                .map(|e| crate::render::fmt_ts(at(e)))
                .unwrap_or_default()
        };
        (
            edge(chunk.first(), |e| e.start),
            edge(chunk.last(), |e| e.end),
        )
    };
    let summary_of = |chunk: &[TranscriptEvent], description: &str| -> Result<Summary> {
        let user = user_prompt(&format!("{ctx}{}", build_transcript(chunk)));
        let content = request_summary(&chat, s, &user, description)?;
        parse_summary(&content).context(invalid())
    };
    let total_chars: usize = events
        .iter()
        .map(|e| e.text.chars().count() + PER_EVENT_OVERHEAD)
        .sum();
    if total_chars <= DIRECT_CHAR_LIMIT {
        let (first, last) = range(events);
        return summary_of(
            events,
            &crate::tr!(
                "生成整篇摘要（{}–{}） / Generate the full summary ({}–{})",
                first,
                last,
                first,
                last
            ),
        );
    }
    // ---- map-reduce：分段按 SUMMARIZE_CONCURRENCY 并发 ----
    let chunks = split_chunks(events, CHUNK_CHAR_LIMIT);
    tracing::info!(
        chunks = chunks.len(),
        chars = total_chars,
        "summary map-reduce"
    );
    let total = chunks.len();
    let partials = crate::runtime::map_pool(
        chunks.into_iter().enumerate().collect(),
        SUMMARIZE_CONCURRENCY,
        control,
        |(index, chunk)| {
            let (first, last) = range(&chunk);
            summary_of(
                &chunk,
                &crate::tr!(
                    "生成第 {}/{} 部分摘要（{}–{}） / Generate summary part {}/{} ({}–{})",
                    index + 1,
                    total,
                    first,
                    last,
                    index + 1,
                    total,
                    first,
                    last
                ),
            )
        },
    )?;
    // 合并分段摘要
    let mut input = ctx.to_owned();
    for (idx, sm) in partials.iter().enumerate() {
        input.push_str(&format!("== 第 {} 段摘要 ==\n", idx + 1));
        if !sm.tldr.is_empty() {
            input.push_str(&format!("概述：{}\n", sm.tldr));
        }
        for p in &sm.key_points {
            input.push_str(&format!("- {p}\n"));
        }
        for o in &sm.outline {
            input.push_str(&format!("- [{:.0}s] {}：{}\n", o.t, o.title, o.detail));
        }
        input.push('\n');
    }
    let combined = request_summary(
        &chat,
        s,
        &format!(
            "以下是各分段的摘要（时间已按原视频绝对秒数标注）：\n\n{input}\n\n\
请合并为整个视频的最终摘要，输出 JSON：{{\"tldr\": \"不超过150字的一句话概述\", \
\"key_points\": [整个视频的3-8条要点], \
\"outline\": [{{\"t\":秒,\"title\":\"章节标题\",\"detail\":\"简述\"}}]}}"
        ),
        crate::tr!(
            "将全部分段摘要合并为整篇摘要 / Merge all partial summaries into the full summary"
        ),
    )?;
    parse_summary(&combined).context(crate::tr!(
        "摘要合并未完成，已保存各段已确认结果 / Summary merge failed; completed parts retained"
    ))
}

/// 生成插入 course.md 的摘要区块（markdown，哨兵注释包裹）。
/// 有意信任 LLM 输出（markdown 语义直通），不做转义。
pub fn render_md_block(sm: &Summary) -> String {
    let mut out = format!("\n{SUMMARY_BEGIN}\n\n## 📝 {}\n\n", crate::tr!("视频摘要"));
    out.push_str(&format!("> {}\n", sm.tldr));
    if !sm.key_points.is_empty() {
        out.push_str(&format!("\n### {}\n\n", crate::tr!("核心要点")));
        for p in &sm.key_points {
            out.push_str(&format!("- {p}\n"));
        }
    }
    if !sm.outline.is_empty() {
        out.push_str(&format!("\n### {}\n\n", crate::tr!("内容大纲")));
        for o in &sm.outline {
            out.push_str(&format!(
                "- **{}** {}：{}\n",
                crate::render::fmt_ts(o.t),
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
        crate::render::esc(crate::tr!("视频摘要"))
    );
    out.push_str(&format!(
        "<p class=\"mute\">{}</p>",
        crate::render::rich(&sm.tldr)
    ));
    if !sm.key_points.is_empty() {
        out.push_str(&format!(
            "<h3>{}</h3><ul>",
            crate::render::esc(crate::tr!("核心要点"))
        ));
        for p in &sm.key_points {
            out.push_str(&format!("<li>{}</li>", crate::render::rich(p)));
        }
        out.push_str("</ul>");
    }
    if !sm.outline.is_empty() {
        out.push_str(&format!(
            "<h3>{}</h3><ul>",
            crate::render::esc(crate::tr!("内容大纲"))
        ));
        for o in &sm.outline {
            out.push_str(&crate::tr!(
                "<li><b>{}</b> {}：{}</li>",
                crate::render::esc(&crate::render::fmt_ts(o.t)),
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

    #[test]
    fn parse_summary_tolerates_fences_and_trailing_commas() {
        let content = "```json\n{\"tldr\": \"讲编译原理\", \"key_points\": [\"词法\",\"语法\",], \"outline\": [{\"t\": \"00:30\", \"title\": \"开场\", \"detail\": \"介绍\"}]}\n```";
        let sm = parse_summary(content).unwrap();
        assert_eq!(sm.tldr, "讲编译原理");
        assert_eq!(sm.key_points.len(), 2);
        assert_eq!(sm.outline.len(), 1);
        assert!(
            (sm.outline[0].t - 30.0).abs() < 1e-6,
            "mm:ss 字符串时间可解析"
        );
    }

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

    #[test]
    fn parse_time_accepts_seconds_suffix() {
        // map-reduce 合并输入按 [{:.0}s] 标注时间
        let v = serde_json::json!("120s");
        assert!((parse_time(Some(&v)) - 120.0).abs() < 1e-6);
        let v = serde_json::json!("[90s]");
        assert!((parse_time(Some(&v)) - 90.0).abs() < 1e-6);
        let v = serde_json::json!("01:30");
        assert!((parse_time(Some(&v)) - 90.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn empty_transcript_bails() {
        let s = LlmSettings::default();
        let meta = VideoMeta {
            title: "t".into(),
            uploader: String::new(),
            duration: 0.0,
            webpage_url: String::new(),
            extractor: String::new(),
            id: String::new(),
        };
        let err = summarize(&s, &[], &meta, &Default::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("转写为空"));
    }
}
