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
use std::io::{IsTerminal, Write};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LlmSettings {
    pub enabled: bool,
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
    /// 转换完成后自动生成视频总结并写入 md/html（需 enabled）
    pub summarize: bool,
    /// 润色并发数（Section 间相互独立；自建网关/代理可调高）
    pub concurrency: usize,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            enabled: false,
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
    if s.base_url.trim().is_empty() {
        bail!("未配置 LLM 服务地址。 / LLM base URL is missing. Run: course2md llm setup");
    }
    if s.model.trim().is_empty() {
        bail!("未配置 LLM 模型。 / LLM model is missing. Run: course2md llm setup");
    }
    let endpoint = url::Url::parse(s.base_url.trim()).context(
        "服务地址无效，请使用完整的 HTTP(S) URL。 / Invalid base URL; use a complete HTTP(S) URL.",
    )?;
    anyhow::ensure!(
        matches!(endpoint.scheme(), "http" | "https") && endpoint.host_str().is_some(),
        "服务地址无效，请使用完整的 HTTP(S) URL。 / Invalid base URL; use a complete HTTP(S) URL."
    );
    Ok(())
}

/// LLM 润色默认并发数（Section 间相互独立；可经 [llm] concurrency 调整）。
const DEFAULT_CONCURRENCY: usize = 8;
/// LLM 请求最大尝试次数（1 次原始 + 重试）。
const MAX_ATTEMPTS: usize = 3;

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

/// 对已合并的 Section 做润色（在 merge 之后调用）。
/// - 失败批次保留原文（润色失败不阻断转换）
/// - vision=true 且截图存在时，请求附该节幻灯片；仅参数类 4xx（疑似不支持
///   图片）才降级纯文本重试一次，401/429/5xx 等不误判为图片问题（issue #11）
/// - 模型对纯语气词条目返回空 text → 该条被删除（issue #5）
/// - Section 间真并发（worker 池抢占式取活，无波次队头阻塞）；
///   疑似输出长度问题的失败（网络/5xx/解析/id 不匹配）拆半递归重试 +
///   请求级指数退避；4xx 确定性错误与限流不拆分，避免请求量放大
pub fn polish_sections(sections: &mut [Section], frames_root: &Path, s: &LlmSettings) {
    // 配置缺失一次性拦截：否则每个分块都会各发满重试后失败，白白放大请求量
    if let Err(e) = validate(s) {
        tracing::warn!(
            "{e:#}；保留原字幕，跳过润色 / Keeping original transcript; skipping LLM polish"
        );
        return;
    }
    let total: usize = sections
        .iter()
        .map(|sec| sec.speech.chunks(BATCH).len())
        .sum();
    let pb = crate::progress::Bar::new("llm", total as u64)
        .with_template("{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}");
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let vision_warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let workers = s.concurrency.clamp(1, 16);
    // worker 池：共享迭代器抢占式取 Section，谁先完成谁取下一个
    // （旧波次实现里最慢的 Section 会挡住整队）
    let queue = std::sync::Mutex::new(sections.iter_mut());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().map(|mut it| it.next());
                    match next {
                        Ok(Some(sec)) => {
                            polish_section(s, frames_root, sec, &pb, &warned, &vision_warned);
                        }
                        Ok(None) => break,
                        Err(_) => break, // 中毒锁：其余 worker 会同样退出
                    }
                }
            });
        }
    });
    pb.finish();
}

/// 润色单个 Section（含视觉图片解析与纯语气词条目删除）。
fn polish_section(
    s: &LlmSettings,
    frames_root: &Path,
    sec: &mut Section,
    pb: &crate::progress::Bar,
    warned: &std::sync::atomic::AtomicBool,
    vision_warned: &std::sync::atomic::AtomicBool,
) {
    if sec.speech.is_empty() {
        return;
    }
    // 同一 Section 的多 chunk 共用一张截图：只读盘 + base64 一次
    //（数 MB 大，逐 chunk 重复编码太贵）；读取失败则该节按纯文本润色
    let image_b64 = if s.vision {
        let p = frames_root.join(&sec.image);
        if p.is_file() {
            match std::fs::read(&p) {
                Ok(bytes) => {
                    use base64::Engine as _;
                    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
                }
                Err(e) => {
                    warn_once(
                        warned,
                        &format!(
                            "无法读取截图 / Cannot read slide image {}: {e:#}. 本节仅润色文字 / Using text-only polish for this section.",
                            p.display()
                        ),
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };
    for chunk in sec.speech.chunks_mut(BATCH) {
        pb.inc(1);
        polish_chunk(s, chunk, image_b64.as_deref(), warned, vision_warned);
    }
    // 删除「成功润色为空串」的纯语气词条目
    sec.speech.retain(|e| !e.text.trim().is_empty());
}

/// 一次润色调用的失败：携带 HTTP 状态码（网络/解析错误为 None），
/// 供上层区分「服务端明确拒绝参数」与「鉴权/限流/解析失败」（issue #11）。
struct PolishError {
    status: Option<u16>,
    err: anyhow::Error,
}

impl PolishError {
    /// 仅 400/422 参数校验错误才疑似「不支持图片输入」，允许一次纯文本降级；
    /// 401/403/404（鉴权/权限/模型不存在）、429（限流）、5xx、网络与解析
    /// 失败都不能证明是图片问题，原样报错，不误降级掩盖真实错误。
    fn allows_vision_fallback(&self) -> bool {
        matches!(self.status, Some(400) | Some(422))
    }

    /// 只有疑似输出长度或单批复杂度导致的失败（网络错误、5xx、解析失败）
    /// 才值得拆半重试；4xx 确定性错误与限流拆半只会放大请求量。
    fn allows_split(&self) -> bool {
        match self.status {
            None => true,
            Some(c) => c >= 500,
        }
    }
}

impl std::fmt::Display for PolishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.err)
    }
}

/// 递归润色一个分块；失败（含 id 集不匹配）时按错误类型决定是否拆半重试。
/// `image_b64` 为该节幻灯片截图的 base64（由 polish_section 统一读取一次）。
fn polish_chunk(
    s: &LlmSettings,
    chunk: &mut [TranscriptEvent],
    image_b64: Option<&str>,
    warned: &std::sync::atomic::AtomicBool,
    vision_warned: &std::sync::atomic::AtomicBool,
) {
    if chunk.is_empty() {
        return;
    }
    let items: Vec<(usize, &str)> = chunk
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.text.as_str()))
        .collect();
    let r = match (chat(s, &items, image_b64), image_b64) {
        (Ok(v), _) => Ok(v),
        (Err(e), Some(_)) if e.allows_vision_fallback() => {
            warn_once(
                vision_warned,
                &format!(
                    "服务拒绝图片输入，改用文字重试 / Image input rejected; retrying with text only: {e}"
                ),
            );
            chat(s, &items, None)
        }
        (Err(e), _) => Err(e),
    };
    match r {
        Ok(polished) => {
            let mismatched = apply_polish(chunk, &polished);
            if mismatched {
                if chunk.len() > 1 {
                    split_and_retry(s, chunk, image_b64, warned, vision_warned);
                } else {
                    warn_once(
                        warned,
                        "润色结果与原文段落不匹配，保留原文 / Polished segments do not match the input; keeping original text",
                    );
                }
            }
        }
        Err(e) => {
            if chunk.len() > 1 && e.allows_split() {
                split_and_retry(s, chunk, image_b64, warned, vision_warned);
            } else {
                warn_once(
                    warned,
                    &format!("润色失败，保留原文 / Polish failed; keeping original text: {e}"),
                );
            }
        }
    }
}

/// 把批次一分为二递归重试（更小批次更易成功，如推理模型 token 耗尽）。
fn split_and_retry(
    s: &LlmSettings,
    chunk: &mut [TranscriptEvent],
    image_b64: Option<&str>,
    warned: &std::sync::atomic::AtomicBool,
    vision_warned: &std::sync::atomic::AtomicBool,
) {
    let mid = chunk.len() / 2;
    polish_chunk(s, &mut chunk[..mid], image_b64, warned, vision_warned);
    polish_chunk(s, &mut chunk[mid..], image_b64, warned, vision_warned);
}

/// 把 (id, 新文本) 应用到一批事件上；空字符串 = 删除该条（由调用方 retain）。
/// 返回 true = 返回集与输入不匹配（重排/缺项/重复），该批保留原文。
fn apply_polish(chunk: &mut [TranscriptEvent], polished: &[(usize, String)]) -> bool {
    let mut by_id: Vec<Option<&str>> = vec![None; chunk.len()];
    for (id, text) in polished {
        if *id >= chunk.len() || by_id[*id].is_some() {
            return true;
        }
        by_id[*id] = Some(text.as_str());
    }
    if by_id.iter().any(|v| v.is_none()) {
        return true;
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
/// 错误携带 HTTP 状态码（PolishError），供上层决定降级/拆分策略。
fn chat(
    s: &LlmSettings,
    items: &[(usize, &str)],
    image_b64: Option<&str>,
) -> std::result::Result<Vec<(usize, String)>, PolishError> {
    let no_status = |err: anyhow::Error| PolishError { status: None, err };
    let body = build_chat_body(s, items, image_b64).map_err(no_status)?;
    let content = send_chat(s, &body).map_err(|f| PolishError {
        status: f.status,
        err: f.err,
    })?;
    parse_segments(&content).ok_or_else(|| {
        no_status(anyhow::anyhow!(
            "润色响应格式无效 / Invalid polish response (expected segments JSON): {:.200}",
            content
        ))
    })
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
    Ok(chat_body(
        &s.model,
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
///（直接 400）。仅当错误是参数类 4xx 时去掉该字段重试一次。
pub(crate) fn send_chat(
    s: &LlmSettings,
    body: &serde_json::Value,
) -> std::result::Result<String, ChatFailure> {
    let resp = match request_chat(s, body) {
        Ok(r) => r,
        Err(first) => {
            // 只有 400（或其他非鉴权/限流的 4xx）才有理由怀疑是 response_format
            // 不兼容；401/403/404（鉴权/权限/路径）与 429（限流）、超时、5xx
            // 与该字段无关，原样报错（issue #11：404 也从可降级集中剔除）。
            let degradable = match first.status {
                Some(400) => true,
                Some(c) => (400..500).contains(&c) && !matches!(c, 401 | 403 | 404 | 429),
                None => false,
            };
            if !(degradable && body.get("response_format").is_some()) {
                return Err(first);
            }
            let mut relaxed = body.clone();
            if let Some(obj) = relaxed.as_object_mut() {
                obj.remove("response_format");
            }
            // 降级请求只试一次：原请求已按 MAX_ATTEMPTS 重试过，这里只验证
            // response_format 兼容性，再走完整重试循环会成倍放大等待时间。
            match request_chat_once(s, &relaxed) {
                Ok(r) => {
                    tracing::debug!("端点不支持 response_format，降级重试成功");
                    r
                }
                Err(_) => return Err(first),
            }
        }
    };
    let no_status = |err: anyhow::Error| ChatFailure {
        status: None,
        retryable: false,
        err,
    };
    let v: serde_json::Value = resp.into_json().map_err(|e| {
        no_status(anyhow::Error::new(e).context("无法解析 LLM 响应 / Cannot parse LLM response"))
    })?;
    // 代理/网关可能返回 200 但 body 是错误结构；静默取空串会劣化成
    // 「解析失败 → 拆半重试」的请求风暴，这里直接报出响应头部便于定位
    v["choices"][0]["message"]["content"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(|c| c.to_string())
        .ok_or_else(|| {
            no_status(anyhow::anyhow!(
                "LLM 响应缺少正文 / LLM response is missing message.content: {:.200}",
                v
            ))
        })
}

/// LLM 请求失败：携带 HTTP 状态码（网络错误为 None）与是否可重试，
/// 供 send_chat/polish 判断 response_format 降级、视觉降级与批次拆分是否有意义。
pub(crate) struct ChatFailure {
    pub(crate) status: Option<u16>,
    retryable: bool,
    pub(crate) err: anyhow::Error,
}

/// 网络层错误 / 429 / 5xx 可重试；其余 4xx（鉴权、参数）重试无意义。
fn is_retryable(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Status(code, _) => *code == 429 || *code >= 500,
        ureq::Error::Transport(_) => true,
    }
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

/// 单次请求（不重试）；状态错误带上服务端返回体（鉴权/限流/参数问题一目了然）。
fn request_chat_once(
    s: &LlmSettings,
    body: &serde_json::Value,
) -> std::result::Result<ureq::Response, ChatFailure> {
    match ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(300))
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", s.api_key))
        // &Value 实现了 Serialize：传引用，避免克隆含 base64 截图的数 MB 请求体
        .send_json(body)
    {
        Ok(resp) => Ok(resp),
        Err(e) => {
            let retryable = is_retryable(&e);
            let (status, err) = match e {
                ureq::Error::Status(code, resp) => {
                    let tail = resp.into_string().unwrap_or_default();
                    (
                        Some(code),
                        anyhow::anyhow!(
                            "LLM 服务返回错误 / LLM service returned HTTP {code}: {}",
                            tail.chars().take(300).collect::<String>()
                        ),
                    )
                }
                other => (
                    None,
                    anyhow::anyhow!("LLM 请求失败 / LLM request failed: {other}"),
                ),
            };
            Err(ChatFailure {
                status,
                retryable,
                err,
            })
        }
    }
}

/// 发请求：可重试错误按指数退避重试，总共最多 [`MAX_ATTEMPTS`] 次尝试。
fn request_chat(
    s: &LlmSettings,
    body: &serde_json::Value,
) -> std::result::Result<ureq::Response, ChatFailure> {
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
            std::thread::sleep(wait);
        }
        match request_chat_once(s, body) {
            Ok(resp) => return Ok(resp),
            Err(f) if f.retryable => last_err = Some(f),
            Err(f) => return Err(f),
        }
    }
    Err(last_err.unwrap_or_else(|| ChatFailure {
        status: None,
        retryable: false,
        err: anyhow::anyhow!("LLM 请求失败 / LLM request failed"),
    }))
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
    let body = serde_json::json!({
        "model": s.model,
        "max_tokens": 8,
        "messages": [{"role": "user", "content": user}],
    });
    let fail_hint = if s.vision {
        "连接失败。请确认服务地址、密钥及模型，并检查图片输入支持。 / Connection failed. Check the URL, API key, model, and image input support."
    } else {
        "连接失败。请检查服务地址、密钥和模型。 / Connection failed. Check the URL, API key, and model."
    };
    let resp = ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(60))
        .set("Authorization", &format!("Bearer {}", s.api_key))
        .send_json(body)
        .context(fail_hint)?;
    let v: serde_json::Value = resp
        .into_json()
        .context("无法解析响应 / Cannot parse response")?;
    let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
    println!("服务响应 / Service response: {}", text.trim());
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

pub fn write_hint_note(_path: &std::path::Path) {
    let msg = "\n可选：配置 LLM 润色字幕 / Optional: set up transcript polishing with course2md llm setup.\n隐藏此提示 / Hide this hint: --no-llm-hint\n";
    let _ = std::io::stderr().write_all(msg.as_bytes());
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
    fn retry_classification_and_backoff() {
        use ureq::Error;
        // Transport 无公共构造器：经由真实失败请求构造（127.0.0.1:1 必连接拒绝）
        let transport = ureq::get("http://127.0.0.1:1/health")
            .timeout(Duration::from_millis(500))
            .call()
            .unwrap_err();
        assert!(
            matches!(transport, Error::Transport(_)),
            "closed port should be transport error"
        );
        assert!(is_retryable(&transport), "网络/TLS 错误可重试");
        let mk_status = |code: u16| {
            let resp = ureq::Response::new(code, "x", "").unwrap();
            Error::Status(code, resp)
        };
        assert!(is_retryable(&mk_status(429)), "限流可重试");
        assert!(is_retryable(&mk_status(500)));
        assert!(is_retryable(&mk_status(503)));
        assert!(!is_retryable(&mk_status(400)), "参数错误重试无意义");
        assert!(!is_retryable(&mk_status(401)), "鉴权错误重试无意义");
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
    fn polish_error_fallback_and_split_policy() {
        let mk = |status: Option<u16>| PolishError {
            status,
            err: anyhow::anyhow!("x"),
        };
        // 仅参数类 4xx 疑似「不支持图片」才允许纯文本降级
        assert!(mk(Some(400)).allows_vision_fallback());
        assert!(mk(Some(422)).allows_vision_fallback());
        for c in [401, 403, 404, 429, 500, 503] {
            assert!(!mk(Some(c)).allows_vision_fallback(), "{c} 不得降级");
        }
        assert!(!mk(None).allows_vision_fallback(), "解析/网络失败不得降级");
        // 拆半只用于疑似输出长度问题（网络/5xx/解析）；4xx 与限流不放大请求量
        assert!(mk(None).allows_split());
        assert!(mk(Some(500)).allows_split());
        for c in [400, 401, 403, 404, 422, 429] {
            assert!(!mk(Some(c)).allows_split(), "{c} 不得拆半放大请求量");
        }
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
