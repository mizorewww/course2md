//! LLM 视频总结：基于带时间戳字幕生成 TL;DR / 核心要点 / 内容大纲。
//!
//! 支持超长视频：字幕超过阈值时自动 map-reduce（分段总结 → 合并）。
//! 幻觉防护：仅以字幕为输入、temperature=0、json_object 结构化输出、要点带时间戳可溯源。

use crate::fetch::VideoMeta;
use crate::llm::{self, LlmSettings};
use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// 直接单次总结的最大字幕字符数。
/// 依据：中文场景 1 字符 ≈ 1-2 token（Qwen/GPT 系分词），25_000 字符 ≈ 2.5-5 万
/// token，加上 system/user prompt 与结构化输出，128K 上下文内仍留足余量；
/// 英文等低 token 密度语言则更宽松。超过即走 map-reduce。
const DIRECT_CHAR_LIMIT: usize = 25_000;
/// map-reduce 每个分块的字符上限。
const CHUNK_CHAR_LIMIT: usize = 25_000;
/// map-reduce 分段总结的并发上限：LLM 端点普遍限流，取保守的 4 路。
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

/// 总结区块哨兵注释（md/html 通用；HTML 注释在两种渲染产物中都原样保留）。
/// 幂等判断 / strip / 原地替换均以哨兵为准，不再扫描正文结构字面量。
const SUMMARY_BEGIN: &str = "<!-- course2md:summary -->";
const SUMMARY_END: &str = "<!-- /course2md:summary -->";

const SYSTEM_PROMPT: &str = "你是视频内容总结助手。根据提供的带时间戳字幕为视频生成结构化总结。\
严格要求：1) 只依据字幕内容，严禁编造字幕中不存在的事实、数字、人名或观点；\
2) 对不确定的信息宁可省略也不要猜测；3) 使用视频原语言输出；\
4) 只输出一个合法 JSON 对象，不要代码围栏、不要任何多余文字。";

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
    if let Some(s) = v.and_then(|x| x.as_str()) {
        let s = s.trim().trim_start_matches('[').trim_end_matches(']');
        // 容忍 "120s" 纯秒格式（map-reduce 合并输入按 [{:.0}s] 标注时间）
        let s = s.strip_suffix('s').unwrap_or(s);
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 1
            && let Ok(sec) = parts[0].parse::<f64>()
        {
            return sec;
        }
        if parts.len() == 2
            && let (Ok(m), Ok(sec)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>())
        {
            return m * 60.0 + sec;
        } else if parts.len() == 3
            && let (Ok(h), Ok(m), Ok(sec)) = (
                parts[0].parse::<f64>(),
                parts[1].parse::<f64>(),
                parts[2].parse::<f64>(),
            )
        {
            return h * 3600.0 + m * 60.0 + sec;
        }
    }
    0.0
}

fn parse_summary(content: &str) -> Option<Summary> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end <= start {
        return None;
    }
    let slice = &content[start..=end];
    let parsed = serde_json::from_str::<serde_json::Value>(slice)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(&llm::clean_trailing_commas(slice)))
        .ok()?;
    let tldr = parsed
        .get("tldr")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let mut key_points = vec![];
    if let Some(arr) = parsed.get("key_points").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    key_points.push(s.to_string());
                }
            }
        }
    }
    let mut outline = vec![];
    if let Some(arr) = parsed.get("outline").and_then(|v| v.as_array()) {
        for v in arr {
            let title = v
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let detail = v
                .get("detail")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let t = parse_time(v.get("t"));
            if !title.is_empty() || !detail.is_empty() {
                outline.push(OutlineItem { t, title, detail });
            }
        }
    }
    if tldr.is_empty() && key_points.is_empty() && outline.is_empty() {
        return None;
    }
    Some(Summary {
        tldr,
        key_points,
        outline,
    })
}

fn chat_once(s: &LlmSettings, sys: &str, user: &str) -> Result<String> {
    let body = llm::chat_body(&s.model, sys, user, llm::CHAT_MAX_TOKENS);
    llm::send_chat(s, &body)
        .map_err(|f| f.err)
        .context("LLM 总结请求失败")
}

/// 单次总结；解析失败带修复指令重试一次。
fn summarize_text(s: &LlmSettings, transcript: &str) -> Result<Summary> {
    let content = chat_once(s, SYSTEM_PROMPT, &user_prompt(transcript))?;
    if let Some(sm) = parse_summary(&content) {
        return Ok(sm);
    }
    let repair = chat_once(
        s,
        "你是严格的 JSON 输出器。输出必须且只能是一个合法 JSON 对象，包含 tldr、key_points、outline 字段；不要代码围栏、不要注释、不要多余文字。",
        &user_prompt(transcript),
    )?;
    // 报错须打印刚失败的 repair 内容，而不是第一次的 content
    parse_summary(&repair).with_context(|| {
        format!(
            "LLM 总结无法解析服务响应 / Could not parse the service response: {:.200}",
            repair
        )
    })
}

fn split_chunks(events: &[TranscriptEvent], char_limit: usize) -> Vec<Vec<TranscriptEvent>> {
    let mut chunks: Vec<Vec<TranscriptEvent>> = vec![];
    let mut cur: Vec<TranscriptEvent> = vec![];
    let mut cur_chars = 0usize;
    for e in events {
        let c = e.text.chars().count() + 16;
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

/// 视频元信息（标题/UP主）作为背景注入 prompt，提升总结相关性；
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

/// 主入口：对全部字幕生成总结；超长自动 map-reduce。
pub async fn summarize(
    s: &LlmSettings,
    events: &[TranscriptEvent],
    meta: &VideoMeta,
) -> Result<Summary> {
    let total_chars: usize = events.iter().map(|e| e.text.chars().count() + 16).sum();
    // 空转写直接报错：发给模型只会得到编造内容或报错，浪费请求
    if events.is_empty() || total_chars == 0 {
        bail!("转写为空，无法总结");
    }
    llm::validate(s)?;
    let ctx = meta_context(meta);
    let transcript = format!("{ctx}{}", build_transcript(events));
    if total_chars <= DIRECT_CHAR_LIMIT {
        let t = transcript;
        let s2 = s.clone();
        return tokio::task::spawn_blocking(move || summarize_text(&s2, &t))
            .await
            .context("总结线程 join 失败")?;
    }
    // ---- map-reduce：分段按 SUMMARIZE_CONCURRENCY 分批并发 ----
    let chunks = split_chunks(events, CHUNK_CHAR_LIMIT);
    tracing::info!(
        chunks = chunks.len(),
        chars = total_chars,
        "summary map-reduce"
    );
    let mut partials: Vec<Summary> = Vec::new();
    for batch in chunks.chunks(SUMMARIZE_CONCURRENCY) {
        let mut handles = Vec::with_capacity(batch.len());
        for chunk in batch {
            let t = format!("{ctx}{}", build_transcript(chunk));
            let s2 = s.clone();
            handles.push(tokio::task::spawn_blocking(move || summarize_text(&s2, &t)));
        }
        let base = partials.len();
        for (off, h) in handles.into_iter().enumerate() {
            let idx = base + off;
            let sm = h.await.context("总结线程 join 失败")?.unwrap_or_else(|e| {
                tracing::warn!("部分内容总结失败 / Could not summarize section {idx}: {e:#}");
                Summary {
                    tldr: String::new(),
                    key_points: vec![],
                    outline: vec![],
                }
            });
            partials.push(sm);
        }
    }
    // 合并分段总结
    let mut combiner_input = ctx.clone();
    for (idx, sm) in partials.iter().enumerate() {
        combiner_input.push_str(&format!("== 第 {} 段总结 ==\n", idx + 1));
        if !sm.tldr.is_empty() {
            combiner_input.push_str(&format!("概述：{}\n", sm.tldr));
        }
        for p in &sm.key_points {
            combiner_input.push_str(&format!("- {p}\n"));
        }
        for o in &sm.outline {
            combiner_input.push_str(&format!("- [{:.0}s] {}：{}\n", o.t, o.title, o.detail));
        }
        combiner_input.push('\n');
    }
    let input = combiner_input;
    let s2 = s.clone();
    let combined = tokio::task::spawn_blocking(move || {
        chat_once(
            &s2,
            SYSTEM_PROMPT,
            &format!(
                "以下是各分段的总结（时间已按原视频绝对秒数标注）：\n\n{input}\n\n\
请合并为整个视频的最终总结，输出 JSON：{{\"tldr\": \"不超过150字的一句话概述\", \
\"key_points\": [整个视频的3-8条要点], \
\"outline\": [{{\"t\":秒,\"title\":\"章节标题\",\"detail\":\"简述\"}}]}}"
            ),
        )
    })
    .await
    .context("合并线程 join 失败")?;
    if let Ok(combined) = combined
        && let Some(sm) = parse_summary(&combined)
    {
        return Ok(sm);
    }
    // 合并失败：拼接分块总结兜底
    let mut tldr = String::new();
    let mut kp: Vec<String> = vec![];
    let mut ol: Vec<OutlineItem> = vec![];
    for sm in &partials {
        if tldr.is_empty() && !sm.tldr.is_empty() {
            tldr = sm.tldr.clone();
        }
        kp.extend(sm.key_points.iter().cloned());
        ol.extend(sm.outline.iter().cloned());
    }
    if kp.is_empty() && ol.is_empty() {
        bail!("视频总结失败：所有分段均未返回有效内容");
    }
    Ok(Summary {
        tldr,
        key_points: kp,
        outline: ol,
    })
}

/// 生成插入 course.md 的总结区块（markdown，哨兵注释包裹）。
/// 有意信任 LLM 输出（markdown 语义直通），不做转义。
pub fn render_md_block(sm: &Summary) -> String {
    let mut out = format!("\n{SUMMARY_BEGIN}\n\n## 📝 视频总结\n\n");
    out.push_str(&format!("> {}\n", sm.tldr));
    if !sm.key_points.is_empty() {
        out.push_str("\n### 核心要点\n\n");
        for p in &sm.key_points {
            out.push_str(&format!("- {p}\n"));
        }
    }
    if !sm.outline.is_empty() {
        out.push_str("\n### 内容大纲\n\n");
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

/// 生成插入 course.html 的总结区块（HTML，哨兵注释包裹）。
pub fn render_html_block(sm: &Summary) -> String {
    let mut out = format!("{SUMMARY_BEGIN}<section class=\"summary\"><h2>📝 视频总结</h2>");
    out.push_str(&format!(
        "<p class=\"mute\">{}</p>",
        crate::render::esc(&sm.tldr)
    ));
    if !sm.key_points.is_empty() {
        out.push_str("<h3>核心要点</h3><ul>");
        for p in &sm.key_points {
            out.push_str(&format!("<li>{}</li>", crate::render::esc(p)));
        }
        out.push_str("</ul>");
    }
    if !sm.outline.is_empty() {
        out.push_str("<h3>内容大纲</h3><ul>");
        for o in &sm.outline {
            out.push_str(&format!(
                "<li><b>{}</b> {}：{}</li>",
                crate::render::esc(&crate::render::fmt_ts(o.t)),
                crate::render::esc(&o.title),
                crate::render::esc(&o.detail)
            ));
        }
        out.push_str("</ul>");
    }
    out.push_str(&format!("</section>{SUMMARY_END}"));
    out
}

/// 把总结区块插入已渲染的 markdown：已有总结块时按哨兵原地替换（幂等）；
/// 首次插入定位到元信息之后、首个字幕小节（## [mm:ss]）之前。
pub fn insert_into_md(md: &str, sm: &Summary) -> String {
    let block = render_md_block(sm);
    if contains_summary(md) {
        return replace_sentinel_block(md, &block);
    }
    if let Some(pos) = md.find("\n## [") {
        let mut out = md.to_string();
        out.insert_str(pos, &block);
        return out;
    }
    let mut out = md.to_string();
    out.push_str(&block);
    out
}

/// 把总结区块插入已渲染的 HTML：已有总结块时按哨兵原地替换（幂等）；
/// 首次插入定位到 </header> 之后（兜底 </body> 前 / 文末）。
pub fn insert_into_html(html: &str, sm: &Summary) -> String {
    let block = render_html_block(sm);
    if contains_html_summary(html) {
        return replace_sentinel_block(html, &block);
    }
    if let Some(pos) = html.find("</header>") {
        let insert_at = pos + "</header>".len();
        let mut out = html.to_string();
        out.insert_str(insert_at, &block);
        return out;
    }
    if let Some(pos) = html.rfind("</body>") {
        let mut out = html.to_string();
        out.insert_str(pos, &block);
        return out;
    }
    let mut out = html.to_string();
    out.push_str(&block);
    out
}

/// 原地替换哨兵之间的总结区块；缺闭合哨兵时保守不改（告警）。
/// 调用方需先经 contains_summary / contains_html_summary 确认起始哨兵存在。
fn replace_sentinel_block(doc: &str, block: &str) -> String {
    let Some(start) = doc.find(SUMMARY_BEGIN) else {
        return doc.to_string();
    };
    let Some(end_rel) = doc[start..].find(SUMMARY_END) else {
        tracing::warn!(
            "已有总结的格式不完整，保留原文 / Existing summary markup is incomplete; original text kept"
        );
        return doc.to_string();
    };
    let end = start + end_rel + SUMMARY_END.len();
    format!("{}{block}{}", &doc[..start], &doc[end..])
}

/// 生成独立总结文件（markdown），用于 -o 导出。
pub fn render_standalone_md(title: &str, sm: &Summary) -> String {
    let mut out = format!("# {title}\n\n");
    out.push_str(&render_md_block(sm));
    out
}

/// 把文件名中的非法字符替换为下划线（Windows 保留字符 + 全角引号等）。
pub fn sanitize_filename(name: &str) -> String {
    let mut s = String::new();
    for ch in name.chars() {
        match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\u{201c}' | '\u{201d}'
            | '\u{ff1f}' | '\u{ff1a}' => s.push('_'),
            c if c.is_control() => s.push('_'),
            c => s.push(c),
        }
    }
    let s = s.trim().trim_end_matches('.').to_string();
    if s.is_empty() {
        "summary".to_string()
    } else {
        s
    }
}

/// 判断已渲染 markdown 是否已包含总结区块（幂等跳过；以哨兵注释为准，
/// 视频标题本身含「视频总结」字样时不会误判）。
pub fn contains_summary(md: &str) -> bool {
    md.contains(SUMMARY_BEGIN)
}

/// 判断已渲染 HTML 是否已包含总结区块（幂等跳过；以哨兵注释为准）。
pub fn contains_html_summary(html: &str) -> bool {
    html.contains(SUMMARY_BEGIN)
}

/// 删除哨兵之间的总结区块；只有起始哨兵没有闭合哨兵时保守不删（告警）。
fn strip_sentinel_block(doc: &str) -> String {
    let Some(start) = doc.find(SUMMARY_BEGIN) else {
        return doc.to_string();
    };
    let Some(end_rel) = doc[start..].find(SUMMARY_END) else {
        tracing::warn!(
            "已有总结的格式不完整，保留原文 / Existing summary markup is incomplete; original text kept"
        );
        return doc.to_string();
    };
    let end = start + end_rel + SUMMARY_END.len();
    format!("{}{}", &doc[..start], &doc[end..])
}

/// 从 markdown 中移除已有总结区块（--force 重写时使用）。
pub fn strip_md_summary(md: &str) -> String {
    strip_sentinel_block(md)
}

/// 从 HTML 中移除已有总结区块（--force 重写时使用）。
pub fn strip_html_summary(html: &str) -> String {
    strip_sentinel_block(html)
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
    fn md_insert_and_strip_roundtrip() {
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
        assert!(contains_summary(&with));
        assert!(
            with.find("视频总结").unwrap() < with.find("## [00:00]").unwrap(),
            "总结在正文前"
        );
        let stripped = strip_md_summary(&with);
        assert!(!contains_summary(&stripped));
        assert!(stripped.contains("## [00:00]"), "正文保留");
        // 标题含「视频总结」不误判
        assert!(!contains_summary("# 视频总结速览课\n\n正文"));
    }

    #[test]
    fn html_insert_and_strip_roundtrip() {
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
        assert!(contains_html_summary(&with));
        let stripped = strip_html_summary(&with);
        assert!(!contains_html_summary(&stripped));
        assert!(stripped.contains("<section>"));
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
    fn unclosed_sentinel_is_not_stripped() {
        let md = "# 标题\n\n<!-- course2md:summary -->\n\n## 残缺块\n\n正文\n";
        assert_eq!(strip_md_summary(md), md, "缺闭合哨兵时保守不删");
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
        let err = summarize(&s, &[], &meta).await.unwrap_err();
        assert!(err.to_string().contains("转写为空"));
    }
}
