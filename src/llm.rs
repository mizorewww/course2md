//! LLM 字幕润色（可选，默认关闭）。
//!
//! 配置文件：`~/.config/course2md/config.toml`（XDG；Windows 为 `%APPDATA%\course2md\config.toml`）。
//! 支持 OpenAI 兼容 /chat/completions 端点；所有配置项均可被命令行覆盖。
//! 关闭时每次任务结束打印开启提示（可用配置项或 `--no-llm-hint` 关闭）。
//!
//! 视觉润色（`vision = true`）：按 Section 分批，每个请求附该节幻灯片截图，
//! 供模型校正技术词汇拼写（issue #5）；仅当端点返回参数类 4xx（疑似不支持
//! 图片输入）时该批才降级纯文本，其余错误原样报告（issue #11）。

use crate::timeline::{Section, TranscriptEvent};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_PROMPT: &str = "你是视频逐字稿校对器。输入的每一项是一段已按自然停顿组织的连续讲解。\
修正明显的语音识别错误（错别字、同音字、专有名词拼写），删除不影响原意的冗余口头填充，\
并修复不自然的断句和标点，使文字自然、书面化。不得概括、扩写、翻译、增删实质内容或改变原意；\
保持原语言。若某条内容仅由语气词、口头禅或无实义片段构成（如单独的\"啊\"、\"对吧\"），\
该条的 text 返回空字符串 \"\"（系统会删除该条）；有实质内容的条目不得删除。\
输出与输入逐条对应的 JSON 对象 {\"segments\":[{\"id\":序号,\"text\":\"校对后的文本\"}]}，不要输出任何其他内容。";

/// 每次请求合并的语音段数。
const BATCH: usize = 20;

/// LLM 服务方言/登录方式。typed enum 取代散落的字符串比较。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmProvider {
    /// OpenAI 兼容 /chat/completions 端点
    #[default]
    OpenAiCompatible,
    /// 本地 Ollama 服务（OpenAI 兼容方言，无需密钥）
    Ollama,
    /// OpenAI Codex 订阅登录（ChatGPT 后端 Responses API）
    Codex,
}

impl LlmProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai-compatible",
            Self::Ollama => "ollama",
            Self::Codex => "codex",
        }
    }
}

impl std::fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LlmSettings {
    pub enabled: bool,
    /// 服务方言/登录方式（默认 OpenAI 兼容）
    pub provider: LlmProvider,
    /// OpenAI 兼容 base URL，如 https://api.deepseek.com/v1
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 自定义校对指令（输出格式约束由系统自动追加，prompt 无法覆盖）
    pub prompt: Option<String>,
    /// 关闭「可开启 LLM」的结束提示
    pub disable_hint: bool,
    /// 视觉润色：每个请求附对应幻灯片截图，辅助纠正技术词汇（模型须支持图片输入）
    pub vision: bool,
    /// 独立生成视频总结并写入笔记（不依赖校对 enabled）
    pub summarize: bool,
    /// 润色并发数（chunk 间相互独立；自建网关/代理可调高）
    pub concurrency: usize,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: LlmProvider::default(),
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            prompt: None,
            disable_hint: false,
            vision: false,
            summarize: false,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

/// base_url -> 完整 chat/completions URL。
pub fn endpoint(base_url: &str) -> String {
    let b = base_url.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else {
        format!("{b}/chat/completions")
    }
}

/// 校验配置可直接使用。
pub fn validate(s: &LlmSettings) -> Result<()> {
    if s.provider != LlmProvider::Codex && s.base_url.trim().is_empty() {
        bail!("未配置 LLM 服务地址。 / LLM base URL is missing. Run: course2md llm setup");
    }
    if s.model.trim().is_empty() {
        bail!("未配置 LLM 模型。 / LLM model is missing. Run: course2md llm setup");
    }
    if !s.base_url.trim().is_empty() {
        crate::config::ensure_http_url(&s.base_url)?;
    }
    Ok(())
}

/// LLM 润色默认并发数（chunk 间相互独立；可经 [llm] concurrency 调整）。
const DEFAULT_CONCURRENCY: usize = 8;
/// 润色并发上限：再高对端点限流没有好处，只放大 429 风险
const MAX_CONCURRENCY: usize = 16;
/// LLM 请求最大尝试次数（1 次原始 + 重试）。
const MAX_ATTEMPTS: usize = 3;

/// 润色/总结共享的 HTTP agent：整个任务复用同一 TCP+TLS 连接池，
/// 不再每请求新建（对照 asr.rs 的共享 client 模式）。
/// Agent clone 共享底层连接池，可安全传入 spawn_blocking 任务。
pub(crate) fn chat_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(300))
        .redirects(0)
        .build()
}

/// chat/completions 公共参数：温度固定 0（校对/总结都要求确定性输出）。
pub(crate) const CHAT_TEMPERATURE: f64 = 0.0;
/// 单次请求输出 token 上限（润色与总结共用）。
pub(crate) const CHAT_MAX_TOKENS: u32 = 16384;

/// 构造标准 chat/completions 请求体（temperature=0、json_object 结构化输出）。
/// 润色与总结共用，避免两处各自拼 body 参数漂移；
/// `user` 传 &str 为纯文本消息，传 `serde_json::Value::Array` 为多模态内容块。
pub(crate) fn chat_body(
    model: &str,
    system: &str,
    user: impl Into<serde_json::Value>,
    max_tokens: u32,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "temperature": CHAT_TEMPERATURE,
        "max_tokens": max_tokens,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user.into()},
        ]
    })
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PolishReport {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
    /// 首个失败原因（截断），用于结果消息可见性；无失败或纯跳过时为 None
    #[serde(default)]
    pub note: Option<String>,
}

/// 对已合并的 Section 做润色（在 merge 之后调用）。
/// - 失败批次保留原文（润色失败不阻断转换）
/// - vision=true 时附对应截图（每节只读盘 + base64 一次）；服务失败不自动改为另一种请求。
/// - 模型对纯语气词条目返回空 text → 该条被删除（issue #5）
/// - 队列单元为 chunk（同一 Section 内的 chunk 也相互独立，单节长视频不再退化为
///   纯串行）；worker 池抢占式取活，无波次队头阻塞；已确认响应持久复用；
///   未确认响应不自动重发。
pub fn polish_sections_report(
    sections: &mut [Section],
    frames_root: &Path,
    s: &LlmSettings,
) -> Result<PolishReport> {
    let attempted = sections.iter().map(|section| section.speech.len()).sum();
    // 配置缺失一次性拦截：否则每个分块都会各发满重试后失败，白白放大请求量
    if let Err(e) = validate(s) {
        tracing::warn!(
            "{e:#}；保留原字幕，跳过润色 / Keeping original transcript; skipping LLM polish"
        );
        return Ok(PolishReport {
            attempted,
            succeeded: 0,
            failed: attempted,
            note: Some(brief(&format!("{e:#}"))),
        });
    }
    if attempted == 0 {
        return Ok(PolishReport::default());
    }
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let workers = s.concurrency.clamp(1, MAX_CONCURRENCY);
    // 整个任务共享一个 agent（连接池复用 TCP+TLS），不再每请求新建
    let agent = chat_agent();
    let succeeded = std::sync::atomic::AtomicUsize::new(0);
    let aborted = std::sync::Mutex::new(None::<anyhow::Error>);
    let first_error = std::sync::Mutex::new(None::<String>);

    if !s.vision {
        // 纯文本润色：跨 Section 展平，每 BATCH (20) 条语音合并为一个请求，
        // 彻底杜绝因切片过多导致请求被放大十几倍的严重缺陷。
        let mut flat: Vec<&mut TranscriptEvent> = sections
            .iter_mut()
            .flat_map(|sec| sec.speech.iter_mut())
            .collect();
        let total = flat.chunks(BATCH).len();
        let pb = crate::progress::Bar::new("llm", total as u64)
            .with_template("{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}");
        let queue = std::sync::Mutex::new(flat.chunks_mut(BATCH));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        if let Err(e) = crate::dispatch::check_control() {
                            let mut guard = aborted.lock().unwrap_or_else(|p| p.into_inner());
                            if guard.is_none() {
                                *guard = Some(e);
                            }
                            break;
                        }
                        let next = queue.lock().map(|mut it| it.next());
                        match next {
                            Ok(Some(chunk)) => {
                                pb.inc(1);
                                let count = polish_chunk_refs(
                                    &agent,
                                    s,
                                    chunk,
                                    None,
                                    &warned,
                                    &first_error,
                                );
                                succeeded.fetch_add(count, std::sync::atomic::Ordering::Relaxed);
                            }
                            Ok(None) | Err(_) => break,
                        }
                    }
                });
            }
        });
        if let Some(e) = aborted.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Err(e);
        }
        for sec in sections.iter_mut() {
            sec.speech.retain(|e| !e.text.trim().is_empty());
        }
        pb.finish();
        let succeeded = succeeded.load(std::sync::atomic::Ordering::Relaxed);
        return Ok(PolishReport {
            attempted,
            succeeded,
            failed: attempted.saturating_sub(succeeded),
            note: first_error.lock().unwrap_or_else(|p| p.into_inner()).take(),
        });
    }

    let total: usize = sections
        .iter()
        .map(|sec| sec.speech.chunks(BATCH).len())
        .sum();
    let pb = crate::progress::Bar::new("llm", total as u64)
        .with_template("{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}");
    // vision：同一 Section 的多 chunk 共用一张截图，只读盘 + base64 一次
    //（数 MB 大，逐 chunk 重复编码太贵）；所需截图不可读的 Section 整体保留原文。
    // 空 Section 不读图也不告警（其 chunks 迭代本就不会产生任务）。
    // 逐节检查任务控制文件：大课程读图期间暂停/取消也能及时生效
    let mut images: Vec<Option<Option<String>>> = Vec::with_capacity(sections.len());
    for sec in sections.iter() {
        crate::dispatch::check_control()?;
        images.push(
            if sec.speech.is_empty() {
                Some(None)
            } else {
                section_image_b64(s, frames_root, sec, &warned)
            },
        );
    }
    // worker 池：共享迭代器抢占式取 chunk，谁先完成谁取下一个
    let queue = std::sync::Mutex::new(
        sections
            .iter_mut()
            .enumerate()
            .flat_map(|(si, sec)| sec.speech.chunks_mut(BATCH).map(move |chunk| (si, chunk))),
    );
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    if let Err(e) = crate::dispatch::check_control() {
                        let mut guard = aborted.lock().unwrap_or_else(|p| p.into_inner());
                        if guard.is_none() {
                            *guard = Some(e);
                        }
                        break;
                    }
                    let next = queue.lock().map(|mut it| it.next());
                    match next {
                        Ok(Some((si, chunk))) => {
                            // 从队列取走即计入进度：截图不可用的节不再让进度条停在不满格
                            pb.inc(1);
                            let Some(image_b64) = images[si].as_ref() else {
                                continue; // 截图不可用：整节保留原文（同原实现的提前返回）
                            };
                            let count = polish_chunk(
                                &agent,
                                s,
                                chunk,
                                image_b64.as_deref(),
                                &warned,
                                &first_error,
                            );
                            succeeded.fetch_add(count, std::sync::atomic::Ordering::Relaxed);
                        }
                        Ok(None) => break,
                        Err(_) => break, // 中毒锁：其余 worker 会同样退出
                    }
                }
            });
        }
    });
    if let Some(e) = aborted.lock().unwrap_or_else(|p| p.into_inner()).take() {
        return Err(e);
    }
    // 删除「成功润色为空串」的纯语气词条目（全部 chunk 完成后统一过滤；
    // 截图不可用而跳过的 Section 未经修改，retain 对其是 no-op）
    for sec in sections.iter_mut() {
        sec.speech.retain(|e| !e.text.trim().is_empty());
    }
    pb.finish();
    let succeeded = succeeded.load(std::sync::atomic::Ordering::Relaxed);
    Ok(PolishReport {
        attempted,
        succeeded,
        failed: attempted.saturating_sub(succeeded),
        note: first_error.lock().unwrap_or_else(|p| p.into_inner()).take(),
    })
}

/// 读取本节的校对截图（vision=true 时）：外层 Some = 该节参与润色（内层为截图
/// base64，vision=false 时为 None）；外层 None = 截图不可用，整节跳过保留原文。
fn section_image_b64(
    s: &LlmSettings,
    frames_root: &Path,
    sec: &Section,
    warned: &std::sync::atomic::AtomicBool,
) -> Option<Option<String>> {
    if !s.vision {
        return Some(None);
    }
    let p = frames_root.join(&sec.image);
    if !p.is_file() {
        warn_once(
            warned,
            "校对所需截图暂不可用，原文已保留 / Required image is unavailable; original text retained",
        );
        return None;
    }
    match std::fs::read(&p) {
        Ok(bytes) => {
            use base64::Engine as _;
            Some(Some(
                base64::engine::general_purpose::STANDARD.encode(bytes),
            ))
        }
        Err(e) => {
            warn_once(
                warned,
                &format!(
                    "无法读取校对所需截图，已保留原文 / Required image could not be read; original text retained: {}: {e:#}",
                    p.display()
                ),
            );
            None
        }
    }
}

/// 校对一个已确认的分块；失败保留原文，不拆分或降低本次请求的能力。
/// `image_b64` 为该节幻灯片截图的 base64（由 polish_sections_report 统一读取一次）。
fn polish_chunk(
    agent: &ureq::Agent,
    s: &LlmSettings,
    chunk: &mut [TranscriptEvent],
    image_b64: Option<&str>,
    warned: &std::sync::atomic::AtomicBool,
    first_error: &std::sync::Mutex<Option<String>>,
) -> usize {
    let mut refs: Vec<&mut TranscriptEvent> = chunk.iter_mut().collect();
    polish_chunk_refs(agent, s, &mut refs, image_b64, warned, first_error)
}

/// 引用版校对：纯文本路径跨 Section 展平后只能拿到 &mut 引用集合，
/// 与切片版 polish_chunk 共用同一请求/应用/失败记录逻辑。
fn polish_chunk_refs(
    agent: &ureq::Agent,
    s: &LlmSettings,
    chunk: &mut [&mut TranscriptEvent],
    image_b64: Option<&str>,
    warned: &std::sync::atomic::AtomicBool,
    first_error: &std::sync::Mutex<Option<String>>,
) -> usize {
    if chunk.is_empty() {
        return 0;
    }
    let items: Vec<(usize, &str)> = chunk
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.text.as_str()))
        .collect();
    let description = format!(
        "校对 {0}–{1} 的文字 / Proofread transcript {0}–{1}",
        crate::render::fmt_ts(chunk.first().unwrap().start),
        crate::render::fmt_ts(chunk.last().unwrap().end)
    );
    match chat(agent, s, &items, image_b64, &description) {
        Ok(polished) => {
            let mismatched = apply_polish_refs(chunk, &polished);
            if mismatched {
                warn_once(
                    warned,
                    "润色结果与原文段落不匹配，保留原文 / Polished segments do not match the input; keeping original text",
                );
                record_first_error(
                    first_error,
                    "润色结果与原文段落不匹配 / Polished segments do not match the input".into(),
                );
                0
            } else {
                chunk.len()
            }
        }
        Err(error) => {
            warn_once(
                warned,
                &format!(
                    "校对未完成，已保留原文 / Proofreading incomplete; original text retained: {error:#}"
                ),
            );
            record_first_error(first_error, format!("{error:#}"));
            0
        }
    }
}

/// 记录首个失败原因（后续失败不覆盖），截断避免超长错误刷屏。
fn record_first_error(slot: &std::sync::Mutex<Option<String>>, msg: String) {
    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        *guard = Some(brief(&msg));
    }
}

fn brief(msg: &str) -> String {
    const MAX: usize = 160;
    let mut s: String = msg.chars().take(MAX).collect();
    if msg.chars().count() > MAX {
        s.push('…');
    }
    s
}

/// 润色结果的 id 集恰好覆盖 0..expected（无缺失/重复/越界）的判定。
/// request_chat_once 的响应校验与 apply_polish 的应用前校验共用同一逻辑。
fn segment_ids_match(polished: &[(usize, String)], expected: usize) -> bool {
    let ids: std::collections::HashSet<usize> = polished.iter().map(|(id, _)| *id).collect();
    polished.len() == expected && ids.len() == expected && (0..expected).all(|id| ids.contains(&id))
}

/// 把 (id, 新文本) 应用到一批事件上；空字符串 = 删除该条（由调用方 retain）。
/// 返回 true = 返回集与输入不匹配（重排/缺项/重复），该批保留原文。
#[cfg(test)]
fn apply_polish(chunk: &mut [TranscriptEvent], polished: &[(usize, String)]) -> bool {
    let mut refs: Vec<&mut TranscriptEvent> = chunk.iter_mut().collect();
    apply_polish_refs(&mut refs, polished)
}

fn apply_polish_refs(chunk: &mut [&mut TranscriptEvent], polished: &[(usize, String)]) -> bool {
    if !segment_ids_match(polished, chunk.len()) {
        return true;
    }
    let mut by_id: Vec<Option<&str>> = vec![None; chunk.len()];
    for (id, text) in polished {
        by_id[*id] = Some(text.as_str());
    }
    for (ev, new) in chunk.iter_mut().zip(by_id) {
        let new = new.unwrap_or("");
        if new != ev.text {
            ev.raw.get_or_insert_with(|| ev.text.clone());
            ev.text = new.to_string();
        }
    }
    false
}

fn warn_once(warned: &std::sync::atomic::AtomicBool, msg: &str) {
    use std::sync::atomic::Ordering;
    if !warned.swap(true, Ordering::Relaxed) {
        tracing::warn!("{msg}（同类提示仅显示一次 / shown once per issue）");
    } else {
        tracing::debug!("{msg}");
    }
}

/// 发一批（id, 文本）给 LLM，返回润色后的 (id, 文本) 列表。
/// `image_b64` 提供时在用户消息中附上该幻灯片截图（OpenAI 兼容 image_url 协议）。
/// 失败由持久请求账本记录；此层保留完整原文。
fn chat(
    agent: &ureq::Agent,
    s: &LlmSettings,
    items: &[(usize, &str)],
    image_b64: Option<&str>,
    description: &str,
) -> Result<Vec<(usize, String)>> {
    let body = build_chat_body(s, items, image_b64)?;
    let content = send_chat_described(agent, s, &body, "proofreading", description)
        .map_err(|failure| failure.err)?;
    parse_segments(&content).context("校对响应结构无效 / Invalid proofreading response structure")
}

/// 构造 /chat/completions 请求体（独立出来便于单测覆盖视觉路径）。
fn build_chat_body(
    s: &LlmSettings,
    items: &[(usize, &str)],
    image_b64: Option<&str>,
) -> Result<serde_json::Value> {
    let payload: Vec<serde_json::Value> = items
        .iter()
        .map(|(i, t)| serde_json::json!({"id": i, "text": t}))
        .collect();
    let vision_note = if image_b64.is_some() {
        " 消息附带该段对应的课件截图，仅用于校正术语拼写与专有名词，不要描述或评论图片本身。"
    } else {
        ""
    };
    let system = format!(
        "{} 输出为 JSON 对象 {{\"segments\":[{{\"id\":序号,\"text\":润色后的文本}}]}}，id 必须与输入一一对应；纯语气词条目的 text 为空字符串。{vision_note}",
        effective_prompt(s)
    );
    let mut content = vec![serde_json::json!({
        "type": "text",
        "text": serde_json::to_string(&payload)?,
    })];
    if let Some(b64) = image_b64 {
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": {"url": format!("data:image/jpeg;base64,{b64}")},
        }));
    }
    Ok(crate::provider::chat_body(
        s,
        &system,
        serde_json::Value::Array(content),
        CHAT_MAX_TOKENS,
    ))
}

/// 从模型输出提取润色结果。约定契约为顶层对象 {"segments":[{"id":n,"text":"..."}]}
/// （与 response_format=json_object 一致，issue #11）；兼容模型无视指令仍返回
/// 顶层数组的情况。两者都容忍代码围栏、前后杂文与尾逗号。
pub fn parse_segments(content: &str) -> Option<Vec<(usize, String)>> {
    // 1) 契约路径：{"segments":[...]}
    if let Some(obj) = extract_json_object(content)
        && let Some(arr) = obj.get("segments").and_then(|v| v.as_array())
        && let Some(items) = parse_items(arr)
    {
        return Some(items);
    }
    // 2) 兼容路径：顶层数组（含个别坏项的宽容扫描）
    parse_id_text_pairs(content)
}

/// 截取首个 { 到末个 } 解析为 JSON 对象；容忍尾逗号。
fn extract_json_object(content: &str) -> Option<serde_json::Value> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end <= start {
        return None;
    }
    let slice = &content[start..=end];
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(slice) {
        return Some(v);
    }
    let cleaned = clean_trailing_commas(slice);
    if cleaned != slice
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned)
    {
        return Some(v);
    }
    None
}

/// 从模型输出提取 [{"id":n,"text":"..."}]（容忍代码围栏、前后杂文、尾逗号与个别坏项）。
pub fn parse_id_text_pairs(content: &str) -> Option<Vec<(usize, String)>> {
    let start = content.find('[')?;
    let end = content.rfind(']')?;
    if end <= start {
        return None;
    }
    let slice = &content[start..=end];
    // 1) 严格解析；个别项 id/text 类型不对时 parse_items 返回 None，
    //    必须继续降级而不是整批丢弃（落入第 2/3 级）
    if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(slice)
        && let Some(items) = parse_items(&v)
    {
        return Some(items);
    }
    // 2) 清除尾逗号后重试
    let cleaned = clean_trailing_commas(slice);
    if cleaned != slice
        && let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(&cleaned)
        && let Some(items) = parse_items(&v)
    {
        return Some(items);
    }
    // 3) 宽容扫描：跳过坏项，收集合法 {"id":..,"text":".."}
    lenient_scan(slice)
}

fn parse_items(v: &[serde_json::Value]) -> Option<Vec<(usize, String)>> {
    let mut out = vec![];
    for item in v {
        let id = item.get("id")?.as_u64()? as usize;
        let text = item.get("text")?.as_str()?.to_string();
        out.push((id, text));
    }
    if out.is_empty() { None } else { Some(out) }
}

pub(crate) fn clean_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
            out.push(ch);
        } else if ch == ',' && matches!(chars.clone().find(|c| !c.is_whitespace()), Some('}' | ']'))
        {
            // Repair syntax only; commas inside transcript strings are data.
        } else {
            out.push(ch);
        }
    }
    out
}

/// 逐个扫描顶层 {...} 对象，坏项跳过；能取到至少一项即返回。
/// 按对象顺序遍历（此前实现从 "id" 向后找 {，方向反了，会漏掉首对象）。
fn lenient_scan(s: &str) -> Option<Vec<(usize, String)>> {
    let bytes = s.as_bytes();
    let mut out: Vec<(usize, String)> = vec![];
    let mut i = 0usize;
    let mut guard = 0usize;
    while i < s.len() {
        let Some(rel) = s[i..].find('{') else { break };
        let obj_start = i + rel;
        // 找配对的 }（跳过字符串字面量内的花括号）
        let mut depth = 0usize;
        let mut in_str = false;
        let mut esc = false;
        let mut end = None;
        for (k, &b) in bytes[obj_start..].iter().enumerate() {
            if in_str {
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    in_str = false;
                }
                continue;
            }
            match b {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(obj_start + k + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        guard += 1;
        if guard > 10_000 {
            break;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s[obj_start..end])
            && let (Some(id), Some(text)) = (
                v.get("id").and_then(|x| x.as_u64()).map(|x| x as usize),
                v.get("text")
                    .and_then(|x| x.as_str())
                    .map(|t| t.to_string()),
            )
        {
            out.push((id, text));
        }
        i = end;
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 发原始 chat/completions 请求并返回 message.content（润色与总结共用）。
///
/// 兼容性降级：部分 OpenAI 兼容端点不支持 `response_format: json_object`
/// 仅当服务明确拒绝这个字段（400/422）时去掉该字段重试一次。
pub(crate) fn send_chat_described(
    agent: &ureq::Agent,
    s: &LlmSettings,
    body: &serde_json::Value,
    purpose: &str,
    description: &str,
) -> std::result::Result<String, ChatFailure> {
    let resp = match request_chat(agent, s, body, purpose, description) {
        Ok(r) => r,
        Err(first) => {
            let degradable = first
                .err
                .downcast_ref::<crate::dispatch::Failure>()
                .is_some_and(|failure| failure.unsupported_response_format);
            if !(degradable && crate::provider::has_structured_format(s, body)) {
                return Err(first);
            }
            let mut relaxed = body.clone();
            crate::provider::strip_structured_format(s, &mut relaxed);
            // 降级请求只试一次：原请求已按 MAX_ATTEMPTS 重试过，这里只验证
            // response_format 兼容性，再走完整重试循环会成倍放大等待时间。
            let response = request_chat_once(agent, s, &relaxed, purpose, description)?;
            tracing::debug!("端点不支持 response_format，降级重试成功");
            response
        }
    };
    // token 用量实时反馈：GUI 阶段行据此跳动（上传=prompt，下载=completion）。
    // Codex SSE 已在 provider::sse_to_chat_json 归一成 chat/completions 同一形状。
    let usage = &resp["usage"];
    crate::progress::note_tokens(
        if purpose == "summary" {
            "summary"
        } else {
            "llm"
        },
        usage["prompt_tokens"].as_u64().unwrap_or(0),
        usage["completion_tokens"].as_u64().unwrap_or(0),
    );
    let no_status = |err: anyhow::Error| ChatFailure {
        retryable: false,
        err,
    };
    // A gateway may return HTTP 200 with an error object. Missing content is a
    // protocol failure, never an empty success or permission to send a different payload.
    resp["choices"][0]["message"]["content"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(|c| c.to_string())
        .ok_or_else(|| {
            no_status(anyhow::anyhow!(
                "LLM 响应缺少正文 / LLM response is missing message.content"
            ))
        })
}

/// LLM failure with a bounded-retry decision from the durable request ledger.
pub(crate) struct ChatFailure {
    retryable: bool,
    pub(crate) err: anyhow::Error,
}

/// 进程级抖动序列：与纳秒异或打散，避免并发请求同步重试（不引入 rand）。
static JITTER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 第 attempt 次失败后的退避时长：1s、2s 指数增长 + 亚秒级抖动。
fn backoff_duration(attempt: usize) -> Duration {
    let base = 1_u64 << (attempt.saturating_sub(1).min(6)); // 1, 2, 4, ...
    let seq = JITTER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let jitter_ns = (nanos ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)) % 500_000_000; // 0~500ms 抖动
    Duration::from_nanos(base * 1_000_000_000 + jitter_ns)
}

/// One attempt; validation errors omit provider response bodies which may echo credentials.
fn request_chat_once(
    agent: &ureq::Agent,
    s: &LlmSettings,
    body: &serde_json::Value,
    purpose: &str,
    description: &str,
) -> std::result::Result<serde_json::Value, ChatFailure> {
    let url = crate::provider::endpoint(s);
    let headers = crate::provider::auth_headers(s).map_err(|err| ChatFailure {
        retryable: false,
        err,
    })?;
    let sse = crate::provider::is_sse(s);
    crate::dispatch::json_request_described("llm", purpose, description, &url, body, || {
        let mut request = agent.post(&url).set("Content-Type", "application/json");
        for (name, value) in &headers {
            request = request.set(name, value);
        }
        let response = crate::dispatch::receive(request.send_json(body))?;
        // Codex Responses API 只提供 SSE 流：聚合为 chat/completions 形状，
        // 下游校验与解析不变；非 2xx 原样上交（重试/降级逻辑要看状态码）
        if sse && (200..300).contains(&response.status) {
            let canonical = crate::provider::sse_to_chat_json(&response.body).map_err(|e| {
                crate::dispatch::NetworkFailure {
                    message: format!("{e:#}"),
                    definitely_unsent: false,
                }
            })?;
            return Ok(crate::dispatch::HttpResponse {
                status: response.status,
                body: serde_json::to_vec(&canonical).unwrap_or_default(),
            });
        }
        Ok(response)
    }, |value| {
        anyhow::ensure!(value.get("error").is_none_or(serde_json::Value::is_null), "AI 服务返回错误内容 / AI service returned an error");
        let content = value["choices"][0]["message"]["content"].as_str().filter(|s| !s.trim().is_empty()).context("AI 服务响应缺少正文 / AI response is missing message.content")?;
        if purpose == "summary" { anyhow::ensure!(crate::summarize::parse_summary(content).is_some(), "服务返回的摘要结构无效 / Invalid summary structure"); }
        if purpose == "proofreading" {
            let parsed = parse_segments(content).context("服务返回的校对结构无效 / Invalid proofreading structure")?;
            let input = crate::provider::user_input_text(body).context("校对输入结构无效 / Invalid proofreading input structure")?;
            let expected = serde_json::from_str::<Vec<serde_json::Value>>(input)?.len();
            anyhow::ensure!(segment_ids_match(&parsed, expected), "校对结果与原文段落不对应，已保留原文 / Proofread segments do not match the input");
        }
        Ok(())
    }).map_err(|failure| ChatFailure { retryable: failure.retryable, err: anyhow::Error::new(failure) })
}

/// 发请求：可重试错误按指数退避重试，总共最多 [`MAX_ATTEMPTS`] 次尝试。
fn request_chat(
    agent: &ureq::Agent,
    s: &LlmSettings,
    body: &serde_json::Value,
    purpose: &str,
    description: &str,
) -> std::result::Result<serde_json::Value, ChatFailure> {
    let mut last_err: Option<ChatFailure> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        if attempt > 1 {
            let wait = backoff_duration(attempt - 1);
            tracing::warn!(
                attempt,
                of = MAX_ATTEMPTS,
                ?wait,
                "LLM 请求失败，稍后重试 / LLM request failed; retrying shortly"
            );
            interruptible_backoff(wait)?;
        }
        match request_chat_once(agent, s, body, purpose, description) {
            Ok(resp) => return Ok(resp),
            Err(f) if f.retryable => last_err = Some(f),
            Err(f) => return Err(f),
        }
    }
    Err(last_err.unwrap_or_else(|| ChatFailure {
        retryable: false,
        err: anyhow::anyhow!("LLM 请求失败 / LLM request failed"),
    }))
}

/// 退避期间保持可取消：分段小睡并轮询任务控制文件。
fn interruptible_backoff(wait: Duration) -> std::result::Result<(), ChatFailure> {
    let mut slept = Duration::ZERO;
    while slept < wait {
        crate::dispatch::check_control().map_err(|err| ChatFailure {
            retryable: false,
            err,
        })?;
        let step = (wait - slept).min(Duration::from_millis(200));
        std::thread::sleep(step);
        slept += step;
    }
    Ok(())
}

/// 用户自定义校对指令；空白视为未设置，回落到内置提示词。
/// 注意：输出格式约束（{"segments":[...]} 契约 / id 对应）由系统在构造
/// 请求体时自动追加，自定义 prompt 无法覆盖（见 build_chat_body）。
fn effective_prompt(s: &LlmSettings) -> &str {
    s.prompt
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(DEFAULT_PROMPT)
}

/// 1×1 PNG 测试图：vision=true 时连接测试附带，
/// 避免「文本请求通了但实际图片请求不可用」的假阳性（issue #11）。
const TEST_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

/// 用最小请求验证端点与凭据可用；vision=true 时附带测试图片，
/// 同时验证图片输入路径（失败会明确报出，不会显示为已验证可用）。
pub fn test_connection(s: &LlmSettings) -> Result<()> {
    validate(s)?;
    let user: serde_json::Value = if s.vision {
        serde_json::json!([
            {"type": "text", "text": "只回复两个字符：ok"},
            {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{TEST_PNG_B64}")}},
        ])
    } else {
        serde_json::Value::String("只回复两个字符：ok".into())
    };
    let body = crate::provider::test_body(s, user);
    let fail_hint = if s.vision {
        "连接失败。请确认服务地址、密钥及模型，并检查图片输入支持。 / Connection failed. Check the URL, API key, model, and image input support."
    } else {
        "连接失败。请检查服务地址、密钥和模型。 / Connection failed. Check the URL, API key, and model."
    };
    // 与生产路径同一纪律：禁止跟随重定向；空密钥不发送 Bearer 头（本地无鉴权服务）
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .redirects(0)
        .build();
    let mut request = agent.post(&crate::provider::endpoint(s));
    for (name, value) in crate::provider::auth_headers(s)? {
        request = request.set(&name, &value);
    }
    let resp = request.send_json(body).context(fail_hint)?;
    let v: serde_json::Value = if crate::provider::is_sse(s) {
        let mut bytes = Vec::new();
        use std::io::Read as _;
        resp.into_reader()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .context("服务响应接收中断 / Service response was interrupted")?;
        crate::provider::sse_to_chat_json(&bytes)?
    } else {
        resp.into_json().context("无法解析响应 / Cannot parse response")?
    };
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    anyhow::ensure!(
        !text.is_empty(),
        "服务返回空响应；请确认模型可用 / The service returned an empty response; make sure the model works"
    );
    println!("服务响应 / Service response: {text}");
    if s.vision {
        println!("已测试图片输入。 / Image input tested.");
    }
    Ok(())
}

/// `llm setup`：交互式补齐缺失项并写盘。
/// 使用 dialoguer（console 行编辑）：支持左右箭头/Home/End 移动、
/// 退格/删除等标准编辑键——裸 read_line 无法处理方向键转义序列（issue #3）。
pub fn setup_interactive(
    mut cfg: crate::settings::ConfigFile,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    disable_hint: bool,
) -> Result<crate::settings::ConfigFile> {
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let cur_or = |v: String, cur: &str| {
        if v.trim().is_empty() {
            cur.to_string()
        } else {
            v.trim().to_string()
        }
    };
    if let Some(v) = base_url {
        cfg.llm.base_url = v.trim().to_string();
    } else if interactive {
        let v: String = dialoguer::Input::new()
            .with_prompt(
                "服务地址 / Base URL (OpenAI-compatible, e.g. https://api.deepseek.com/v1)",
            )
            .with_initial_text(&cfg.llm.base_url)
            .allow_empty(true)
            .interact_text()?;
        cfg.llm.base_url = cur_or(v, &cfg.llm.base_url);
    }
    if let Some(v) = api_key {
        cfg.llm.api_key = v;
    } else if interactive {
        let keep_hint = if cfg.llm.api_key.is_empty() {
            "（输入隐藏；无密钥的本地服务可留空） / API key (hidden; leave blank for a local service without authentication)"
        } else {
            "（输入隐藏；回车保留已保存的密钥） / API key (hidden; Enter keeps the saved key)"
        };
        let v = dialoguer::Password::new()
            .with_prompt(format!("API Key{keep_hint}"))
            .allow_empty_password(true)
            .interact()?;
        cfg.llm.api_key = cur_or(v, &cfg.llm.api_key);
    }
    if let Some(v) = model {
        cfg.llm.model = v.trim().to_string();
    } else if interactive {
        let v: String = dialoguer::Input::new()
            .with_prompt("模型名 / Model name (e.g. deepseek-chat)")
            .with_initial_text(&cfg.llm.model)
            .allow_empty(true)
            .interact_text()?;
        cfg.llm.model = cur_or(v, &cfg.llm.model);
    }
    anyhow::ensure!(
        !cfg.llm.base_url.trim().is_empty() && !cfg.llm.model.trim().is_empty(),
        "服务地址和模型名不能为空。交互设置请在终端运行 course2md llm setup；脚本请提供 --base-url <URL> --model <MODEL>（服务需要鉴权时加 --api-key）。 / Base URL and model are required. Run course2md llm setup in a terminal, or pass --base-url <URL> --model <MODEL> in scripts (add --api-key if authentication is required)."
    );
    // 容错：没写 scheme 时补 https://
    if !cfg.llm.base_url.is_empty() && !cfg.llm.base_url.contains("://") {
        cfg.llm.base_url = format!("https://{}", cfg.llm.base_url.trim());
    }
    // 视觉能力仅交互式终端询问（脚本化调用全部传参时不阻塞）；
    // 检查 stdin 而非 stderr，与 dialoguer 读取的流一致
    if interactive {
        cfg.llm.vision = dialoguer::Select::new()
            .with_prompt("润色时附上幻灯片截图？需支持图片的模型。 / Attach slide images when polishing? Requires an image-capable model.")
            .items([
                "仅发送文字 / Send text only",
                "发送文字和截图 / Send text and slide images",
            ])
            .default(if cfg.llm.vision { 1 } else { 0 })
            .interact_opt()?
            .ok_or_else(|| anyhow::anyhow!("已取消设置，未保存配置。 / Setup cancelled; no configuration saved."))? == 1;
    }
    // setup 面向 OpenAI 兼容端点；codex/ollama 登录态由此显式退出
    cfg.llm.provider = LlmProvider::OpenAiCompatible;
    validate(&cfg.llm)?;
    cfg.llm.disable_hint |= disable_hint;
    cfg.llm.enabled = true;
    Ok(cfg)
}

pub fn print_status(cfg: &crate::settings::ConfigFile) {
    let s = &cfg.llm;
    let state = |enabled| {
        if enabled {
            "开启 / enabled"
        } else {
            "关闭 / disabled"
        }
    };
    println!(
        "配置文件 / Configuration: {}",
        crate::settings::config_path().display()
    );
    println!("  LLM 润色 / Transcript polish: {}", state(s.enabled));
    println!("  服务类型 / Provider: {}", s.provider);
    println!(
        "  服务地址 / Base URL: {}",
        if s.base_url.is_empty() {
            "—"
        } else {
            &s.base_url
        }
    );
    println!(
        "  API key: {}",
        if s.api_key.is_empty() {
            "未设置 / not set"
        } else {
            "已设置（隐藏） / set (hidden)"
        }
    );
    println!(
        "  模型 / Model: {}",
        if s.model.is_empty() { "—" } else { &s.model }
    );
    println!(
        "  截图辅助润色 / Slide-assisted polish: {}",
        state(s.vision)
    );
    println!("  自动总结 / Automatic summary: {}", state(s.summarize));
    println!("  并发请求 / Concurrent requests: {}", s.concurrency);
    println!("  使用提示 / Usage hint: {}", state(!s.disable_hint));
    if !s.enabled {
        println!("开启润色 / Enable transcript polish: course2md llm setup");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_unusable_service_urls() {
        for base_url in ["", "not a URL", "file:///tmp/service", "https://"] {
            let settings = LlmSettings {
                base_url: base_url.into(),
                model: "test-model".into(),
                ..Default::default()
            };
            assert!(validate(&settings).is_err(), "accepted {base_url}");
        }
        let settings = LlmSettings {
            base_url: "http://localhost:8080/v1".into(),
            model: "test-model".into(),
            ..Default::default()
        };
        assert!(validate(&settings).is_ok());
    }

    #[test]
    fn trailing_comma_repair_preserves_transcript_literals() {
        let input = r#"{"segments":[{"id":0,"text":"literal ,} and ,] and \"quoted\"", }, ] }"#;
        let parsed = parse_segments(input).unwrap();
        assert_eq!(parsed[0].1, "literal ,} and ,] and \"quoted\"");
    }

    #[test]
    fn endpoint_join() {
        assert_eq!(
            endpoint("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.x.com/v1/"),
            "https://api.x.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.x.com/v1/chat/completions"),
            "https://api.x.com/v1/chat/completions"
        );
    }

    fn test_settings() -> LlmSettings {
        LlmSettings {
            enabled: true,
            provider: LlmProvider::OpenAiCompatible,
            base_url: "https://api.x.com/v1".into(),
            api_key: "k".into(),
            model: "m".into(),
            prompt: None,
            disable_hint: false,
            vision: false,
            summarize: false,
            concurrency: 8,
        }
    }

    #[test]
    fn apply_polish_empty_text_deletes_entry() {
        let mut chunk = vec![
            TranscriptEvent {
                start: 0.0,
                end: 1.0,
                text: "今天讲编译原理".into(),
                raw: None,
            },
            TranscriptEvent {
                start: 1.0,
                end: 2.0,
                text: "啊".into(),
                raw: None,
            },
        ];
        let bad = apply_polish(
            &mut chunk,
            &[(0, "今天讲编译原理".into()), (1, String::new())],
        );
        assert!(!bad);
        assert_eq!(chunk[1].text, "", "纯语气词被置空");
        assert_eq!(chunk[1].raw.as_deref(), Some("啊"), "原文进 raw 溯源");
        // 调用方语义：置空的条目随后被 retain 删除
        chunk.retain(|e| !e.text.trim().is_empty());
        assert_eq!(chunk.len(), 1);
    }

    #[test]
    fn apply_polish_rejects_mismatched_ids() {
        let mut chunk = vec![TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: "a".into(),
            raw: None,
        }];
        assert!(apply_polish(
            &mut chunk,
            &[(0, "x".into()), (1, "y".into())]
        ));
        assert!(apply_polish(&mut chunk, &[]));
        assert_eq!(chunk[0].text, "a", "不匹配时保留原文");
    }

    #[test]
    fn apply_polish_refs_updates_text_and_sets_raw() {
        let mut ev1 = TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: "原始文本一".into(),
            raw: None,
        };
        let mut ev2 = TranscriptEvent {
            start: 1.0,
            end: 2.0,
            text: "语气词".into(),
            raw: None,
        };
        let mut refs = vec![&mut ev1, &mut ev2];
        let mismatched = apply_polish_refs(
            &mut refs,
            &[(0, "校对文本一".into()), (1, "".into())],
        );
        assert!(!mismatched);
        assert_eq!(ev1.text, "校对文本一");
        assert_eq!(ev1.raw.as_deref(), Some("原始文本一"));
        assert_eq!(ev2.text, "");
        assert_eq!(ev2.raw.as_deref(), Some("语气词"));
    }

    #[test]
    fn parse_pairs_tolerates_trailing_commas_and_bad_items() {
        // 尾逗号（推理模型常见输出）
        let got = parse_id_text_pairs("[{\"id\":0,\"text\":\"a\",},{\"id\":1,\"text\":\"b\",},]")
            .unwrap();
        assert_eq!(got, vec![(0, "a".into()), (1, "b".into())]);
        // 个别坏项：跳过而不丢弃整批
        let got = parse_id_text_pairs(
            "[{\"id\":0,\"text\":\"a\"},{\"id\":\"oops\"},{\"id\":2,\"text\":\"c\"}]",
        )
        .unwrap();
        assert_eq!(
            got,
            vec![(0, "a".into()), (2, "c".into())],
            "坏项应被跳过（随后的拆半重试会覆盖 id=1）"
        );
    }

    #[test]
    fn retry_backoff_progression() {
        // 指数退避：1s、2s、4s…（含 0~500ms 抖动）
        let b1 = backoff_duration(1).as_secs_f64();
        let b2 = backoff_duration(2).as_secs_f64();
        let b3 = backoff_duration(3).as_secs_f64();
        assert!((1.0..1.5).contains(&b1));
        assert!((2.0..2.5).contains(&b2));
        assert!((4.0..4.5).contains(&b3));
    }

    #[test]
    fn chat_body_text_vs_vision() {
        let s = test_settings();
        let items = [(0usize, "hello")];
        let text_only = build_chat_body(&s, &items, None).unwrap();
        let user: &Vec<serde_json::Value> = text_only["messages"][1]["content"].as_array().unwrap();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0]["type"], "text");

        let vision = build_chat_body(&s, &items, Some("aGVsbG8=")).unwrap();
        let user: &Vec<serde_json::Value> = vision["messages"][1]["content"].as_array().unwrap();
        assert_eq!(user.len(), 2, "带图时附 image_url 内容块");
        assert_eq!(user[1]["type"], "image_url");
        assert_eq!(
            user[1]["image_url"]["url"].as_str().unwrap(),
            "data:image/jpeg;base64,aGVsbG8="
        );
        let sys = vision["messages"][0]["content"].as_str().unwrap();
        assert!(sys.contains("课件截图"), "带图时系统提示说明截图用途");
        assert!(
            sys.contains("\"segments\""),
            "系统提示与 response_format=json_object 同为 segments 对象契约"
        );
    }

    #[test]
    fn parse_segments_object_contract() {
        // 契约路径：顶层对象 {"segments":[...]}
        let got =
            parse_segments("{\"segments\":[{\"id\":0,\"text\":\"a\"},{\"id\":1,\"text\":\"b\"}]}")
                .unwrap();
        assert_eq!(got, vec![(0, "a".into()), (1, "b".into())]);
        // 容忍围栏 + 尾逗号
        let got =
            parse_segments("```json\n{\"segments\":[{\"id\":0,\"text\":\"a\"},]}\n```").unwrap();
        assert_eq!(got, vec![(0, "a".into())]);
        // 兼容路径：模型无视指令仍返回顶层数组
        let got = parse_segments("[{\"id\":0,\"text\":\"a\"}]").unwrap();
        assert_eq!(got, vec![(0, "a".into())]);
        assert!(parse_segments("{\"segments\":[]}").is_none());
        assert!(parse_segments("没有 JSON").is_none());
    }

    #[test]
    fn parse_id_pairs_tolerates_fences() {
        let got = parse_id_text_pairs(
            "```json\n[{\"id\":0,\"text\":\"a\"},{\"id\":1,\"text\":\"b\"}]\n```",
        )
        .unwrap();
        assert_eq!(got, vec![(0, "a".into()), (1, "b".into())]);
        assert!(parse_id_text_pairs("没有数组").is_none());
        assert!(parse_id_text_pairs("[]").is_none());
    }
}
