//! LLM 字幕校对（可选，默认关闭）。
//!
//! 配置文件：`~/.config/course2md/config.toml`（XDG；Windows 为 `%APPDATA%\course2md\config.toml`）。
//! 支持 OpenAI 兼容 /chat/completions 端点；所有配置项均可被命令行覆盖。
//! 关闭时每次任务结束打印开启提示（可用配置项或 `--no-llm-hint` 关闭）。
//!
//! 视觉校对（`vision = true`）：按 Section 分批，每个请求附该节幻灯片截图，
//! 供模型校正技术词汇拼写；请求失败时该批保留原文，不改发纯文本请求。

use crate::progress::Stage;
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
";

/// 公式书写约定：校对与摘要输出共用，系统自动追加，自定义 prompt 无法覆盖。
/// 阅读器与导出按 `$` / `$$` 识别公式并排版。
pub const MATH_RULE: &str = "数学公式、符号与表达式一律写成 LaTeX：行内公式用 $...$ 包围，\
独立成行的公式用 $$...$$ 包围（如 $f(x)=x^2$、$$\\int_a^b f(x)\\,dx$$）；\
口述的公式（如“x 的平方”“从 a 到 b 积分”）同样改写为 LaTeX；没有公式时不要添加 $。\
JSON 字符串里的反斜杠必须写成 \\\\（如 \\\\frac）。";

/// 每次请求合并的语音段数。
const BATCH: usize = 20;

/// LLM 服务方言/登录方式。
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
    /// 视觉校对：每个请求附对应幻灯片截图，辅助纠正技术词汇（模型须支持图片输入）
    pub vision: bool,
    /// 独立生成视频摘要并写入笔记（不依赖校对 enabled）
    pub summarize: bool,
    /// Bilingual notes: the target language code (see [`crate::language::TARGETS`]).
    /// Skipped when the transcript is already in that language.
    pub translate: Option<String>,
    /// 校对并发数（chunk 间相互独立；自建网关/代理可调高）
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
            translate: None,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
}

/// 请求用途：请求账本分类与 token 用量归属。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    Proofreading,
    Summary,
    Translation,
}

impl Purpose {
    fn ledger_name(self) -> &'static str {
        match self {
            Self::Proofreading => "proofreading",
            Self::Summary => "summary",
            Self::Translation => "translation",
        }
    }
    fn usage_stage(self) -> Stage {
        match self {
            Self::Proofreading => Stage::Llm,
            Self::Summary => Stage::Summary,
            Self::Translation => Stage::Translate,
        }
    }
}

/// base_url -> 完整 chat/completions URL。
pub fn endpoint(base_url: &str) -> String {
    crate::config::append_endpoint_suffix(base_url.trim(), "/chat/completions")
}

/// 校验配置可直接使用。
pub fn validate(s: &LlmSettings) -> Result<()> {
    if s.provider != LlmProvider::Codex && s.base_url.trim().is_empty() {
        bail!(crate::tr!(
            "未设置 AI 服务地址 / AI service URL is not set. Run: course2md llm setup"
        ));
    }
    if s.model.trim().is_empty() {
        bail!(crate::tr!(
            "未设置 AI 模型 / AI model is not set. Run: course2md llm setup"
        ));
    }
    if !s.base_url.trim().is_empty() {
        crate::config::ensure_http_url(&s.base_url)?;
    }
    Ok(())
}

/// LLM 校对默认并发数（chunk 间相互独立；可经 [llm] concurrency 调整）。
const DEFAULT_CONCURRENCY: usize = 8;
/// 校对并发上限：再高对端点限流没有好处，只放大 429 风险
const MAX_CONCURRENCY: usize = 16;
/// LLM 请求最大尝试次数（1 次原始 + 重试）。
const MAX_ATTEMPTS: u32 = 3;

/// 校对/摘要共享的 HTTP agent：整个任务复用同一连接池（clone 共享底层池）。
/// A response that sends nothing for [`STALL_TIMEOUT`] has stalled; it is
/// abandoned then instead of holding the task for the whole request budget.
pub(crate) fn chat_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(300))
        .timeout_read(STALL_TIMEOUT)
        .redirects(0)
        .build()
}

const STALL_TIMEOUT: Duration = Duration::from_secs(90);

/// Upper bound on a streamed (SSE) response body read in one connection test.
const SSE_BODY_LIMIT: u64 = 4 * 1024 * 1024;
/// 单次请求输出 token 上限（校对与摘要共用）。
pub(crate) const CHAT_MAX_TOKENS: u32 = 16384;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PolishReport {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
    /// 首个失败原因（截断），用于结果消息可见性；无失败或纯跳过时为 None
    #[serde(default)]
    pub note: Option<String>,
}

/// Runs the AI batches on the shared worker pool. A batch counts toward
/// progress once taken, even if it is skipped; `work` returns how many items
/// it improved (0 for a skipped or failed batch, which keeps the original).
/// Err = the task was stopped.
fn run_pool<T: Send>(
    chunks: Vec<T>,
    workers: usize,
    pb: &crate::progress::Bar,
    work: &(dyn Fn(T) -> usize + Sync),
) -> Result<usize> {
    let counts = crate::runtime::map_pool(chunks, workers, |item| {
        pb.inc(1);
        Ok(work(item))
    })?;
    Ok(counts.into_iter().sum())
}

/// 对已合并的 Section 做校对（在 merge 之后调用；配置已由调用方校验）。
/// - 失败批次保留原文（校对失败不阻断转换），不拆分、不改发另一种请求
/// - vision=true 时附对应截图（每节只读盘 + base64 一次）；截图不可用的节保留原文
/// - 模型对纯语气词条目返回空 text → 该条被删除
/// - 队列单元为 chunk：纯文本跨 Section 展平后每 BATCH 条一批；vision 按节分批
///   （同一请求只能附一张截图）。worker 池抢占式取活，已确认响应由请求账本复用。
pub fn polish_sections_report(
    sections: &mut [Section],
    frames_root: &Path,
    s: &LlmSettings,
) -> Result<PolishReport> {
    let attempted = sections.iter().map(|section| section.speech.len()).sum();
    if attempted == 0 {
        return Ok(PolishReport::default());
    }
    let warned = std::sync::atomic::AtomicBool::new(false);
    // Paths only: the image is read and encoded once per request batch, not all up front.
    let mut image_paths: Vec<Option<std::path::PathBuf>> = Vec::with_capacity(sections.len());
    for sec in sections.iter() {
        crate::dispatch::check_control()?;
        image_paths
            .push((s.vision && !sec.speech.is_empty()).then(|| frames_root.join(&sec.image)));
    }
    let mut chunks: Vec<(Option<std::path::PathBuf>, Vec<&mut TranscriptEvent>)> = Vec::new();
    if s.vision {
        for (sec, image) in sections.iter_mut().zip(&image_paths) {
            let Some(image) = image else { continue };
            let mut refs: Vec<_> = sec.speech.iter_mut().collect();
            while !refs.is_empty() {
                let rest = refs.split_off(refs.len().min(BATCH));
                chunks.push((Some(image.clone()), std::mem::replace(&mut refs, rest)));
            }
        }
    } else {
        let mut refs: Vec<_> = sections
            .iter_mut()
            .flat_map(|sec| sec.speech.iter_mut())
            .collect();
        while !refs.is_empty() {
            let rest = refs.split_off(refs.len().min(BATCH));
            chunks.push((None, std::mem::replace(&mut refs, rest)));
        }
    }
    let pb = crate::progress::Bar::new(Stage::Llm, chunks.len() as u64)
        .with_template("{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}");
    let agent = chat_agent();
    let first_error = std::sync::Mutex::new(None::<String>);
    let succeeded = run_pool(
        chunks,
        s.concurrency.clamp(1, MAX_CONCURRENCY),
        &pb,
        &|(image_path, mut chunk)| {
            let image = image_path
                .as_deref()
                .and_then(|path| section_image_b64(path, &warned));
            polish_chunk(
                &agent,
                s,
                &mut chunk,
                image.as_deref(),
                &warned,
                &first_error,
            )
        },
    )?;
    pb.finish();
    // A formula spoken across a subtitle boundary comes back in two pieces.
    for sec in sections.iter_mut() {
        join_split_formulas(&mut sec.speech);
    }
    // 删除「成功校对为空串」的纯语气词条目（全部 chunk 完成后统一过滤）
    for sec in sections.iter_mut() {
        sec.speech.retain(|e| !e.text.trim().is_empty());
    }
    Ok(PolishReport {
        attempted,
        succeeded,
        failed: attempted.saturating_sub(succeeded),
        note: crate::runtime::lock_recover(&first_error).take(),
    })
}

impl LlmSettings {
    /// Whether any requested step sends text to the AI service.
    pub fn uses_service(&self) -> bool {
        self.enabled || self.summarize || self.translation_target().is_some()
    }

    /// The supported bilingual target, if one is set.
    pub fn translation_target(&self) -> Option<&'static str> {
        self.translate.as_deref().and_then(crate::language::target)
    }
}

/// Translates every paragraph into `target`, kept beside the original.
/// Failed batches stay untranslated; the note is published either way.
pub fn translate_sections_report(
    sections: &mut [Section],
    s: &LlmSettings,
    target: &str,
) -> Result<PolishReport> {
    let mut chunks: Vec<Vec<&mut TranscriptEvent>> = Vec::new();
    let mut refs: Vec<_> = sections
        .iter_mut()
        .flat_map(|section| section.speech.iter_mut())
        .filter(|event| !event.text.trim().is_empty())
        .collect();
    let attempted = refs.len();
    if attempted == 0 {
        return Ok(PolishReport::default());
    }
    while !refs.is_empty() {
        let rest = refs.split_off(refs.len().min(BATCH));
        chunks.push(std::mem::replace(&mut refs, rest));
    }
    let pb = crate::progress::Bar::new(Stage::Translate, chunks.len() as u64)
        .with_template("{spinner:.green} translate {pos}/{len} [{bar:32.cyan/blue}] {msg}");
    let agent = chat_agent();
    let warned = std::sync::atomic::AtomicBool::new(false);
    let first_error = std::sync::Mutex::new(None::<String>);
    let succeeded = run_pool(
        chunks,
        s.concurrency.clamp(1, MAX_CONCURRENCY),
        &pb,
        &|mut chunk| translate_chunk(&agent, s, target, &mut chunk, &warned, &first_error),
    )?;
    pb.finish();
    Ok(PolishReport {
        attempted,
        succeeded,
        failed: attempted.saturating_sub(succeeded),
        note: crate::runtime::lock_recover(&first_error).take(),
    })
}

fn translate_chunk(
    agent: &ureq::Agent,
    s: &LlmSettings,
    target: &str,
    chunk: &mut [&mut TranscriptEvent],
    warned: &std::sync::atomic::AtomicBool,
    first_error: &std::sync::Mutex<Option<String>>,
) -> usize {
    let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else {
        return 0;
    };
    let description = crate::tr!(
        "翻译 {}–{} 的文字 / Translate transcript {}–{}",
        crate::render::fmt_ts(first.start),
        crate::render::fmt_ts(last.end),
        crate::render::fmt_ts(first.start),
        crate::render::fmt_ts(last.end)
    );
    let items: Vec<&str> = chunk.iter().map(|e| e.text.as_str()).collect();
    let result = translation_body(s, target, &items).and_then(|body| {
        let expected = items.len();
        let content = send_chat_described(
            agent,
            s,
            &body,
            Purpose::Translation,
            &description,
            &|content| {
                let parsed = parse_segments(content).context(crate::tr!(
                    "服务返回的翻译结构无效 / Invalid translation structure"
                ))?;
                anyhow::ensure!(
                    segment_ids_match(&parsed, expected),
                    crate::tr!(
                        "翻译结果与原文段落不对应 / Translated segments do not match the input"
                    )
                );
                Ok(())
            },
        )?;
        parse_segments(&content).context(crate::tr!(
            "服务返回的翻译结构无效 / Invalid translation structure"
        ))
    });
    match result {
        Ok(translated) => {
            for (id, text) in translated {
                if let Some(event) = chunk.get_mut(id) {
                    let text = text.trim();
                    event.translation = (!text.is_empty()).then(|| text.to_owned());
                }
            }
            chunk.len()
        }
        Err(error) => {
            warn_once(warned, &crate::tr!("翻译未完成：{}", format!("{error:#}")));
            record_first_error(first_error, format!("{error:#}"));
            0
        }
    }
}

fn translation_body(s: &LlmSettings, target: &str, items: &[&str]) -> Result<serde_json::Value> {
    let payload: Vec<serde_json::Value> = items
        .iter()
        .enumerate()
        .map(|(i, t)| serde_json::json!({"id": i, "text": t}))
        .collect();
    // The instruction stays in English: it names the target for any service.
    let system = format!(
        "You translate lecture transcripts. Translate each item into {}. Keep the meaning, \
         terminology and tone; do not summarize, add or omit content. Keep names, code and \
         formulas as they are. Output a JSON object {{\"segments\":[{{\"id\":number,\"text\":translation}}]}} \
         with exactly one entry per input id. {MATH_RULE}",
        crate::language::english_name(target)
    );
    Ok(crate::provider::chat_body(
        s,
        &system,
        serde_json::Value::String(serde_json::to_string(&payload)?),
        CHAT_MAX_TOKENS,
    ))
}

/// Joins an inline formula that a segment ends unfinished (`$0.$`, `$a+$`)
/// with the formula the next segment opens with, so `加上 $0.$` + `$2\cos 2x$`
/// reads `加上 $0.2\cos 2x$`. The next segment keeps the rest of its text.
fn join_split_formulas(events: &mut [TranscriptEvent]) {
    for index in 1..events.len() {
        let (before, after) = events.split_at_mut(index);
        let (previous, next) = (&mut before[index - 1].text, &mut after[0].text);
        let Some(head) = unfinished_tail_formula(previous) else {
            continue;
        };
        let rest = next.trim_start();
        let Some(body) = rest.strip_prefix('$').filter(|body| !body.starts_with('$')) else {
            continue;
        };
        let Some(close) = body.find('$') else {
            continue;
        };
        let joined = format!("{head}{}$", &body[..close]);
        let remainder = body[close + 1..].to_owned();
        let keep = previous.trim_end().len() - 1;
        previous.truncate(keep - (head.len() + 1));
        previous.push('$');
        previous.push_str(&joined);
        *next = remainder;
    }
}

/// The content of a closing inline formula that cannot end where it does.
fn unfinished_tail_formula(text: &str) -> Option<String> {
    let trimmed = text.trim_end().strip_suffix('$')?;
    let open = trimmed.rfind('$')?;
    let content = &trimmed[open + 1..];
    if content.is_empty() || trimmed[..open].ends_with('$') {
        return None;
    }
    let tail = content.trim_end();
    let unfinished = tail.ends_with(['+', '-', '=', '(', '[', '{', '^', '_', ',', '\\'])
        || (tail.ends_with('.') && tail[..tail.len() - 1].ends_with(|c: char| c.is_ascii_digit()));
    unfinished.then(|| content.to_owned())
}

/// 读取本节校对截图的 base64；None = 截图不可用（已告警）。
fn section_image_b64(path: &Path, warned: &std::sync::atomic::AtomicBool) -> Option<String> {
    use base64::Engine as _;
    match std::fs::read(path) {
        Ok(bytes) => Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        Err(e) => {
            warn_once(
                warned,
                &format!(
                    "无法读取校对所需截图，已保留原文 / Required image could not be read; original text retained: {}: {e:#}",
                    path.display()
                ),
            );
            None
        }
    }
}

/// 校对一个分块；返回成功条数（失败保留原文并记录首个错误）。
fn polish_chunk(
    agent: &ureq::Agent,
    s: &LlmSettings,
    chunk: &mut [&mut TranscriptEvent],
    image_b64: Option<&str>,
    warned: &std::sync::atomic::AtomicBool,
    first_error: &std::sync::Mutex<Option<String>>,
) -> usize {
    let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else {
        return 0;
    };
    let description = crate::tr!(
        "校对 {}–{} 的文字 / Proofread transcript {}–{}",
        crate::render::fmt_ts(first.start),
        crate::render::fmt_ts(last.end),
        crate::render::fmt_ts(first.start),
        crate::render::fmt_ts(last.end)
    );
    let items: Vec<&str> = chunk.iter().map(|e| e.text.as_str()).collect();
    match chat(agent, s, &items, image_b64, &description) {
        Ok(polished) => {
            apply_polish(chunk, &polished);
            chunk.len()
        }
        Err(error) => {
            warn_once(
                warned,
                &crate::tr!("校对未完成，已保留原文：{}", format!("{error:#}")),
            );
            record_first_error(first_error, format!("{error:#}"));
            0
        }
    }
}

/// 记录首个失败原因（后续失败不覆盖），截断避免超长错误刷屏。
fn record_first_error(slot: &std::sync::Mutex<Option<String>>, msg: String) {
    let mut guard = crate::runtime::lock_recover(slot);
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

/// 校对结果的 id 集恰好覆盖 0..expected（无缺失/重复/越界）。
fn segment_ids_match(polished: &[(usize, String)], expected: usize) -> bool {
    let ids: std::collections::HashSet<usize> = polished.iter().map(|(id, _)| *id).collect();
    polished.len() == expected && ids.len() == expected && (0..expected).all(|id| ids.contains(&id))
}

/// 把 (id, 新文本) 应用到一批事件上；空字符串 = 删除该条（由调用方 retain）。
/// 调用前 id 集已由请求账本校验为恰好覆盖本批（[`segment_ids_match`]）。
fn apply_polish(chunk: &mut [&mut TranscriptEvent], polished: &[(usize, String)]) {
    for (id, new) in polished {
        let Some(ev) = chunk.get_mut(*id) else {
            continue;
        };
        if *new != ev.text {
            ev.raw.get_or_insert_with(|| ev.text.clone());
            ev.text = new.clone();
        }
    }
}

fn warn_once(warned: &std::sync::atomic::AtomicBool, msg: &str) {
    use std::sync::atomic::Ordering;
    if !warned.swap(true, Ordering::Relaxed) {
        tracing::warn!("{msg}");
    } else {
        tracing::debug!("{msg}");
    }
}

/// 发一批（id, 文本）给 LLM，返回校对后的 (id, 文本) 列表。
/// `image_b64` 提供时在用户消息中附上该幻灯片截图（OpenAI 兼容 image_url 协议）。
/// 失败由持久请求账本记录；此层保留完整原文。
fn chat(
    agent: &ureq::Agent,
    s: &LlmSettings,
    items: &[&str],
    image_b64: Option<&str>,
    description: &str,
) -> Result<Vec<(usize, String)>> {
    let body = build_chat_body(s, items, image_b64)?;
    let expected = items.len();
    let content = send_chat_described(
        agent,
        s,
        &body,
        Purpose::Proofreading,
        description,
        &|content| {
            let parsed = parse_segments(content).context(crate::tr!(
                "服务返回的校对结构无效 / Invalid proofreading structure"
            ))?;
            anyhow::ensure!(
                segment_ids_match(&parsed, expected),
                crate::tr!(
                    "校对结果与原文段落不对应，已保留原文 / Proofread segments do not match the input"
                )
            );
            Ok(())
        },
    )?;
    parse_segments(&content).context(crate::tr!(
        "服务返回的校对结构无效 / Invalid proofreading structure"
    ))
}

/// 构造 /chat/completions 请求体（独立出来便于单测覆盖视觉路径）。
fn build_chat_body(
    s: &LlmSettings,
    items: &[&str],
    image_b64: Option<&str>,
) -> Result<serde_json::Value> {
    let payload: Vec<serde_json::Value> = items
        .iter()
        .enumerate()
        .map(|(i, t)| serde_json::json!({"id": i, "text": t}))
        .collect();
    let vision_note = if image_b64.is_some() {
        crate::tr!(
            " 消息附带该段对应的课件截图，仅用于校正术语拼写与专有名词，不要描述或评论图片本身。"
        )
    } else {
        ""
    };
    let system = format!(
        "{} {MATH_RULE}",
        crate::tr!(
            "{} 输出为 JSON 对象 {{\"segments\":[{{\"id\":序号,\"text\":校对后的文本}}]}}，id 必须与输入一一对应；纯语气词条目的 text 为空字符串。{}",
            effective_prompt(s),
            vision_note
        )
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

/// 从模型输出提取校对结果。约定契约为顶层对象 {"segments":[{"id":n,"text":"..."}]}
/// （与 response_format=json_object 一致）；兼容模型无视指令仍返回
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
pub(crate) fn extract_json_object(content: &str) -> Option<serde_json::Value> {
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

/// A model that writes `\frac` or `\times` unescaped in JSON gets a form feed
/// or tab back from the parser. Those characters never belong in a transcript,
/// so before a letter they are restored to the LaTeX command they came from;
/// a newline is restored only inside a `$` formula, where it cannot be prose.
pub(crate) fn restore_latex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_math = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let letter_follows = chars.peek().is_some_and(char::is_ascii_alphabetic);
        let escape = match c {
            '\u{8}' => Some('b'),
            '\u{c}' => Some('f'),
            '\t' => Some('t'),
            '\r' => Some('r'),
            '\n' if in_math => Some('n'),
            _ => None,
        };
        match escape {
            Some(letter) if letter_follows => {
                out.push('\\');
                out.push(letter);
            }
            _ => {
                if c == '$' && !out.ends_with('\\') {
                    // `$$` opens or closes one display formula.
                    if chars.peek() == Some(&'$') {
                        out.push(chars.next().unwrap_or('$'));
                    }
                    in_math = !in_math;
                }
                out.push(c);
            }
        }
    }
    out
}

fn parse_items(v: &[serde_json::Value]) -> Option<Vec<(usize, String)>> {
    let mut out = vec![];
    for item in v {
        let id = item.get("id")?.as_u64()? as usize;
        let text = restore_latex(item.get("text")?.as_str()?);
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
fn lenient_scan(s: &str) -> Option<Vec<(usize, String)>> {
    let bytes = s.as_bytes();
    let mut out: Vec<(usize, String)> = vec![];
    let mut i = 0usize;
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
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s[obj_start..end])
            && let (Some(id), Some(text)) = (
                v.get("id").and_then(|x| x.as_u64()).map(|x| x as usize),
                v.get("text").and_then(|x| x.as_str()).map(restore_latex),
            )
        {
            out.push((id, text));
        }
        i = end;
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 发 chat/completions 请求并返回 message.content（校对与摘要共用）。
/// `validate` 校验正文结构，在请求账本里执行：无效响应记为失败，不会被当成已完成复用。
///
/// 兼容性降级：部分 OpenAI 兼容端点不支持 `response_format: json_object`，
/// 仅当服务明确拒绝这个字段（400/422）时去掉该字段重试一次。
pub(crate) fn send_chat_described(
    agent: &ureq::Agent,
    s: &LlmSettings,
    body: &serde_json::Value,
    purpose: Purpose,
    description: &str,
    validate: &dyn Fn(&str) -> Result<()>,
) -> Result<String> {
    let reply = match request_chat(agent, s, body, purpose, description, validate) {
        Ok(r) => r,
        Err(first) => {
            let degradable = first
                .downcast_ref::<crate::dispatch::Failure>()
                .is_some_and(|failure| failure.unsupported_response_format);
            if !(degradable && crate::provider::has_structured_format(s, body)) {
                return Err(first);
            }
            let mut relaxed = body.clone();
            crate::provider::strip_structured_format(s, &mut relaxed);
            // 降级请求只试一次：原请求已按 MAX_ATTEMPTS 重试过，这里只验证兼容性
            let reply = request_chat_once(agent, s, &relaxed, purpose, description, validate)?;
            tracing::debug!("端点不支持 response_format，降级重试成功");
            reply
        }
    };
    let resp = reply.value;
    // token 用量实时反馈：GUI 阶段行据此跳动（上传=prompt，下载=completion）。
    // Codex SSE 已在 provider::sse_to_chat_json 归一成 chat/completions 同一形状。
    let usage = &resp["usage"];
    if !reply.replayed {
        crate::progress::note_tokens(
            purpose.usage_stage(),
            usage["prompt_tokens"].as_u64().unwrap_or(0),
            usage["completion_tokens"].as_u64().unwrap_or(0),
        );
    }
    // 账本已校验正文存在且结构有效
    Ok(resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

/// One attempt; validation errors omit provider response bodies which may echo credentials.
fn request_chat_once(
    agent: &ureq::Agent,
    s: &LlmSettings,
    body: &serde_json::Value,
    purpose: Purpose,
    description: &str,
    validate: &dyn Fn(&str) -> Result<()>,
) -> std::result::Result<crate::dispatch::Reply, crate::dispatch::Failure> {
    let url = crate::provider::endpoint(s);
    let headers = crate::provider::auth_headers(s).map_err(crate::dispatch::Failure::local)?;
    let sse = crate::provider::is_sse(s);
    crate::dispatch::json_request_described(
        "llm",
        purpose.ledger_name(),
        description,
        &url,
        body,
        if crate::provider::free_to_resend(s) {
            crate::dispatch::Resend::Free
        } else {
            crate::dispatch::Resend::AskUser
        },
        || {
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
        },
        |value| {
            // A gateway may return HTTP 200 with an error object. Missing content is a
            // protocol failure, never an empty success.
            anyhow::ensure!(
                value.get("error").is_none_or(serde_json::Value::is_null),
                crate::tr!("AI 服务返回了错误 / The AI service returned an error")
            );
            let content = value["choices"][0]["message"]["content"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .context(crate::tr!(
                    "AI 服务没有返回内容 / The AI service returned no content"
                ))?;
            validate(content)
        },
    )
}

/// 发请求：可重试错误按退避重试（退避期间可取消），总共最多 [`MAX_ATTEMPTS`] 次。
fn request_chat(
    agent: &ureq::Agent,
    s: &LlmSettings,
    body: &serde_json::Value,
    purpose: Purpose,
    description: &str,
    validate: &dyn Fn(&str) -> Result<()>,
) -> Result<crate::dispatch::Reply> {
    crate::runtime::retry(MAX_ATTEMPTS, || {
        request_chat_once(agent, s, body, purpose, description, validate)
    })
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
/// 避免「文本请求通了但实际图片请求不可用」的假阳性。
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
        serde_json::Value::String(crate::tr!("只回复两个字符：ok").into())
    };
    let body = crate::provider::test_body(s, user);
    let fail_hint = if s.vision {
        crate::tr!(
            "连接失败。请确认服务地址、密钥及模型，并检查图片输入支持。 / Connection failed. Check the URL, API key, model, and image input support."
        )
    } else {
        crate::tr!(
            "连接失败。请检查服务地址、密钥和模型。 / Connection failed. Check the URL, API key, and model."
        )
    };
    // 与生产路径同一纪律：禁止跟随重定向；空密钥不发送 Bearer 头（本地无鉴权服务）
    let agent = crate::runtime::http_agent(Duration::from_secs(60));
    let mut request = agent.post(&crate::provider::endpoint(s));
    for (name, value) in crate::provider::auth_headers(s)? {
        request = request.set(&name, &value);
    }
    let resp = request.send_json(body).context(fail_hint)?;
    let v: serde_json::Value = if crate::provider::is_sse(s) {
        let mut bytes = Vec::new();
        use std::io::Read as _;
        resp.into_reader()
            .take(SSE_BODY_LIMIT + 1)
            .read_to_end(&mut bytes)
            .context(crate::tr!(
                "服务响应接收中断 / Service response was interrupted"
            ))?;
        crate::provider::sse_to_chat_json(&bytes)?
    } else {
        resp.into_json()
            .context(crate::tr!("无法解析响应 / Cannot parse response"))?
    };
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim();
    anyhow::ensure!(
        !text.is_empty(),
        crate::tr!(
            "服务返回空响应；请确认模型可用 / The service returned an empty response; make sure the model works"
        )
    );
    Ok(())
}

/// `llm setup`：交互式补齐缺失项并写盘。
/// 使用 dialoguer（console 行编辑）：支持左右箭头/Home/End 移动、
/// 退格/删除等标准编辑键（裸 read_line 无法处理方向键转义序列）。
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
            .with_prompt(crate::tr!(
                "服务地址 / Base URL (OpenAI-compatible, e.g. https://api.deepseek.com/v1)"
            ))
            .with_initial_text(&cfg.llm.base_url)
            .allow_empty(true)
            .interact_text()?;
        cfg.llm.base_url = cur_or(v, &cfg.llm.base_url);
    }
    if let Some(v) = api_key {
        cfg.llm.api_key = v;
    } else if interactive {
        let keep_hint = if cfg.llm.api_key.is_empty() {
            crate::tr!(
                "（输入隐藏；无密钥的本地服务可留空） / API key (hidden; leave blank for a local service without authentication)"
            )
        } else {
            crate::tr!(
                "（输入隐藏；回车保留已保存的密钥） / API key (hidden; Enter keeps the saved key)"
            )
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
            .with_prompt(crate::tr!("模型名 / Model name (e.g. deepseek-chat)"))
            .with_initial_text(&cfg.llm.model)
            .allow_empty(true)
            .interact_text()?;
        cfg.llm.model = cur_or(v, &cfg.llm.model);
    }
    anyhow::ensure!(
        !cfg.llm.base_url.trim().is_empty() && !cfg.llm.model.trim().is_empty(),
        crate::tr!(
            "需要填写服务地址和模型名。在终端运行 course2md llm setup，或传入 --base-url 和 --model / A base URL and model are required. Run course2md llm setup in a terminal, or pass --base-url and --model."
        )
    );
    // 容错：没写 scheme 时补 https://
    if !cfg.llm.base_url.is_empty() && !cfg.llm.base_url.contains("://") {
        cfg.llm.base_url = format!("https://{}", cfg.llm.base_url.trim());
    }
    // 视觉能力仅交互式终端询问（脚本化调用全部传参时不阻塞）
    if interactive {
        cfg.llm.vision = dialoguer::Select::new()
            .with_prompt(crate::tr!("校对时附上幻灯片截图？需支持图片的模型。 / Attach slide images when polishing? Requires an image-capable model."))
            .items([
                crate::tr!("仅发送文字 / Send text only"),
                crate::tr!("发送文字和截图 / Send text and slide images"),
            ])
            .default(if cfg.llm.vision { 1 } else { 0 })
            .interact_opt()?
            .ok_or_else(|| anyhow::anyhow!(crate::tr!("已取消设置，未保存配置。 / Setup cancelled; no configuration saved.")))? == 1;
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
            crate::tr!("开启 / enabled")
        } else {
            crate::tr!("关闭 / disabled")
        }
    };
    println!(
        "配置文件 / Configuration: {}",
        crate::settings::config_path().display()
    );
    println!("  LLM 校对 / Transcript polish: {}", state(s.enabled));
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
            crate::tr!("未设置 / not set")
        } else {
            crate::tr!("已设置（隐藏） / set (hidden)")
        }
    );
    println!(
        "  模型 / Model: {}",
        if s.model.is_empty() { "—" } else { &s.model }
    );
    println!(
        "  截图辅助校对 / Slide-assisted polish: {}",
        state(s.vision)
    );
    println!("  自动摘要 / Automatic summary: {}", state(s.summarize));
    println!("  并发请求 / Concurrent requests: {}", s.concurrency);
    println!("  使用提示 / Usage hint: {}", state(!s.disable_hint));
    if !s.enabled {
        crate::println_tr!("开启校对 / Enable transcript polish: course2md llm setup");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_split_across_segments_are_joined() {
        let event = |text: &str| TranscriptEvent {
            start: 0.,
            end: 1.,
            text: text.into(),
            raw: None,
            translation: None,
        };
        let mut events = vec![
            event("加上 $0.$"),
            event("$2\\cos 2x$，等等。"),
            event("这是 $x$"),
            event("$y$ 不变"),
        ];
        join_split_formulas(&mut events);
        assert_eq!(events[0].text, "加上 $0.2\\cos 2x$");
        assert_eq!(events[1].text, "，等等。");
        // A complete formula followed by another stays two formulas.
        assert_eq!(events[2].text, "这是 $x$");
        assert_eq!(events[3].text, "$y$ 不变");
    }

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
    fn unescaped_latex_commands_survive_json_parsing() {
        // `{"text":"$\frac{a}{b}\times c$"}` written without escaping the backslashes.
        let content = "{\"segments\":[{\"id\":0,\"text\":\"面积 $\\frac{a}{b}\\times c$ 与 $$\\neq 0$$\\n下一句\"}]}";
        let parsed = parse_segments(content).unwrap();
        assert_eq!(
            parsed[0].1,
            "面积 $\\frac{a}{b}\\times c$ 与 $$\\neq 0$$\n下一句"
        );
        // Properly escaped LaTeX is left as written.
        assert_eq!(restore_latex("$\\frac{1}{2}$"), "$\\frac{1}{2}$");
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
            translate: None,
            concurrency: 8,
        }
    }

    #[test]
    fn translation_keeps_originals_and_adds_each_translation() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            // Read the head, then the body announced by Content-Length.
            let body_start = loop {
                let n = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..n]);
                if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&request[..body_start]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap();
            while request.len() < body_start + length {
                let n = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..n]);
            }
            let content = serde_json::json!({"segments":[{"id":0,"text":"Today: compilers."},{"id":1,"text":"Next, parsing."}]});
            let body = serde_json::json!({"choices":[{"message":{"content":content.to_string()}}]})
                .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            String::from_utf8_lossy(&request[body_start..]).into_owned()
        });
        let event = |text: &str| TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: text.into(),
            raw: None,
            translation: None,
        };
        let mut sections = vec![Section {
            t: 0.0,
            end: 1.0,
            image: String::new(),
            speech: vec![event("今天讲编译原理。"), event("接下来是语法分析。")],
        }];
        let mut settings = test_settings();
        settings.base_url = format!("http://{address}/v1");
        settings.concurrency = 1;
        let report = translate_sections_report(&mut sections, &settings, "en").unwrap();
        let request = server.join().unwrap();
        assert!(request.contains("English"), "{request}");
        assert_eq!((report.attempted, report.succeeded), (2, 2));
        let speech = &sections[0].speech;
        assert_eq!(speech[0].text, "今天讲编译原理。");
        assert_eq!(speech[0].translation.as_deref(), Some("Today: compilers."));
        assert_eq!(speech[1].translation.as_deref(), Some("Next, parsing."));
    }

    #[test]
    fn polish_records_raw_text_and_mismatched_ids_are_rejected() {
        let event = |text: &str| TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: text.into(),
            raw: None,
            translation: None,
        };
        let (mut a, mut b) = (event(crate::tr!("今天讲编译原理")), event(crate::tr!("啊")));
        let mut refs = vec![&mut a, &mut b];
        let polished = [(0, crate::tr!("今天讲编译原理").into()), (1, String::new())];
        assert!(segment_ids_match(&polished, refs.len()));
        apply_polish(&mut refs, &polished);
        assert_eq!(a.raw, None, "未改动的条目不记 raw");
        assert_eq!(b.text, "", "纯语气词被置空，随后由调用方删除");
        assert_eq!(b.raw.as_deref(), Some(crate::tr!("啊")), "原文进 raw 溯源");
        assert!(!segment_ids_match(&[(0, "x".into()), (1, "y".into())], 1));
        assert!(!segment_ids_match(&[], 1));
        assert!(!segment_ids_match(&[(0, "x".into()), (0, "y".into())], 2));
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
        assert_eq!(got, vec![(0, "a".into()), (2, "c".into())], "坏项应被跳过");
    }

    #[test]
    fn chat_body_text_vs_vision() {
        let s = test_settings();
        let items = ["hello"];
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
        assert!(parse_segments(crate::tr!("没有 JSON")).is_none());
    }

    #[test]
    fn parse_id_pairs_tolerates_fences() {
        let got = parse_id_text_pairs(
            "```json\n[{\"id\":0,\"text\":\"a\"},{\"id\":1,\"text\":\"b\"}]\n```",
        )
        .unwrap();
        assert_eq!(got, vec![(0, "a".into()), (1, "b".into())]);
        assert!(parse_id_text_pairs(crate::tr!("没有数组")).is_none());
        assert!(parse_id_text_pairs("[]").is_none());
    }
}
