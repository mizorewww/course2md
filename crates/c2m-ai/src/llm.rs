//! LLM 字幕校对（可选，默认关闭）。
//!
//! 配置文件：`~/.config/course2md/config.toml`（XDG；Windows 为 `%APPDATA%\course2md\config.toml`）。
//! 支持 OpenAI 兼容 /chat/completions 端点；所有配置项均可被命令行覆盖。
//! 关闭时每次任务结束打印开启提示（可用配置项或 `--no-llm-hint` 关闭）。
//!
//! 视觉校对（`vision = true`）：按 Section 分批，每个请求附该节幻灯片截图，
//! 供模型校正技术词汇拼写；请求失败时该批保留原文，不改发纯文本请求。

pub use crate::llm_settings::{LlmProvider, LlmSettings};
use crate::progress::Stage;
use crate::timeline::{Section, TranscriptEvent};
use crate::{println_tr, tr};
use anyhow::{Context, Result, bail};
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
pub(crate) const MATH_RULE: &str = "数学公式、符号与表达式一律写成 LaTeX：行内公式用 $...$ 包围，\
独立成行的公式用 $$...$$ 包围（如 $f(x)=x^2$、$$\\int_a^b f(x)\\,dx$$）；\
口述的公式（如“x 的平方”“从 a 到 b 积分”）同样改写为 LaTeX；没有公式时不要添加 $。\
JSON 字符串里的反斜杠必须写成 \\\\（如 \\\\frac）。";

/// The proofreading output rule and the vision note, appended to the prompt.
///
/// Everything sent to a service stays one fixed text, like [`DEFAULT_PROMPT`]: the
/// request is the ledger's identity, so a prompt that followed the interface
/// language would make a resumed task pay for its finished requests again.
struct ProofreadingRule {
    rule: &'static str,
    vision_note: &'static str,
}

const PROOFREADING_RULE: ProofreadingRule = ProofreadingRule {
    rule: " 输出为 JSON 对象 {\"segments\":[{\"id\":序号,\"text\":校对后的文本}]}，id 必须与输入一一对应；纯语气词条目的 text 为空字符串。",
    vision_note: " 消息附带该段对应的课件截图，仅用于校正术语拼写与专有名词，不要描述或评论图片本身。",
};

/// The rule as 2.0.1 and earlier sent it from the CLI in English, Traditional Chinese
/// and Japanese, when it still followed the interface language. Receipts those
/// versions saved stay valid ([`former_proofreading_bodies`]).
const FORMER_PROOFREADING_RULES: [ProofreadingRule; 3] = [
    ProofreadingRule {
        rule: " Output a JSON object {\"segments\":[{\"id\":index,\"text\":proofread text}]}; each id must match the input one-to-one; for entries that contain only filler words, text is an empty string. ",
        vision_note: " The message is accompanied by a screenshot of the courseware corresponding to the paragraph. It is only used to correct the spelling of terms and proper nouns. Do not describe or comment on the picture itself.",
    },
    ProofreadingRule {
        rule: " 輸出為 JSON 物件 {\"segments\":[{\"id\":序號,\"text\":校對後的文字}]}，id 必須與輸入一一對應；純語氣詞條目的 text 為空字串。",
        vision_note: " 訊息附帶該段對應的課件截圖，僅用於校正術語拼寫與專有名詞，不要描述或評論圖片本身。",
    },
    ProofreadingRule {
        rule: " JSON オブジェクト {\"segments\":[{\"id\":番号,\"text\":校正後のテキスト}]} として出力してください。id は入力と一対一で対応させてください。フィラー（つなぎ言葉）だけの項目は text を空文字列にしてください。",
        vision_note: " メッセージには、その段落に対応するコースウェアのスクリーンショットが添付されています。用語や固有名詞のスペルを修正するためにのみ使用されます。画像自体について説明したりコメントしたりしないでください。",
    },
];

impl ProofreadingRule {
    fn system(&self, prompt: &str, vision: bool) -> String {
        let note = if vision { self.vision_note } else { "" };
        format!("{prompt}{}{note} {MATH_RULE}", self.rule)
    }
}

/// The connection test's question; see [`PROOFREADING_RULE`].
const TEST_QUESTION: &str = "只回复两个字符：ok";

/// 每次请求合并的语音段数。
const BATCH: usize = 20;

/// The most transcript characters one request carries. Proofreading and translation
/// answer with about as much text as they are given, so this bounds the answer too:
/// 1,500 CJK characters are roughly 1,500–2,500 tokens out, a few seconds to half a
/// minute of generation. Most 20-paragraph batches stay within it, so they are the
/// same requests (and receipts) as before; only the long ones are split.
const BATCH_CHARS: usize = 1_500;

/// The output budget for proofreading or translating `items`: twice their length
/// (a token per CJK character at worst, with room for rewording), the JSON around
/// each item, and a margin; never less than [`MIN_BATCH_TOKENS`] so a model that
/// thinks first still has room, never more than [`CHAT_MAX_TOKENS`]. A model that
/// runs on (thinking or repeating itself) stops here instead of at the global cap.
fn batch_max_tokens(items: &[&str]) -> u32 {
    let chars: usize = items.iter().map(|item| item.chars().count()).sum();
    let budget = 2 * chars + 32 * items.len() + 1024;
    u32::try_from(budget)
        .unwrap_or(u32::MAX)
        .clamp(MIN_BATCH_TOKENS, CHAT_MAX_TOKENS)
}

const MIN_BATCH_TOKENS: u32 = 4096;

/// 请求用途：请求账本分类与 token 用量归属。
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub(crate) enum Purpose {
    Proofreading,
    Summary,
    Translation,
}

impl Purpose {
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
        bail!(tr!(
            "未设置 AI 服务地址 / AI service URL is not set. Run: course2md llm setup"
        ));
    }
    if s.model.trim().is_empty() {
        bail!(tr!(
            "未设置 AI 模型 / AI model is not set. Run: course2md llm setup"
        ));
    }
    if !s.base_url.trim().is_empty() {
        crate::config::ensure_http_url(&s.base_url)?;
    }
    Ok(())
}

/// 校对并发上限：再高对端点限流没有好处，只放大 429 风险
const MAX_CONCURRENCY: usize = 16;
/// LLM 请求最大尝试次数（1 次原始 + 重试），未开启自动重试时。
const MAX_ATTEMPTS: u32 = 3;
/// The most sends `retry_attempts` may ask for.
const MAX_RETRY_ATTEMPTS: u32 = 8;

/// How a failed AI request is retried. With automatic retries on: the configured
/// number of sends, waiting 1 s, 2 s, 4 s… up to 30 s, plus jitter. Off: the
/// requests that provably cost nothing more (never sent, rate limited) as before.
fn retry_policy(s: &LlmSettings) -> crate::pool::RetryPolicy {
    if s.auto_retry {
        crate::pool::RetryPolicy {
            max_attempts: s.retry_attempts.clamp(1, MAX_RETRY_ATTEMPTS),
            base: Duration::from_secs(1),
            cap: Duration::from_secs(30),
        }
    } else {
        crate::pool::RetryPolicy {
            max_attempts: MAX_ATTEMPTS,
            ..crate::pool::RetryPolicy::DEFAULT
        }
    }
}

/// Whether a request whose result is unknown may be sent again without asking.
fn resend_rule(s: &LlmSettings) -> crate::dispatch::Resend {
    if s.auto_retry {
        crate::dispatch::Resend::Automatic
    } else if crate::provider::free_to_resend(s) {
        crate::dispatch::Resend::Free
    } else {
        crate::dispatch::Resend::AskUser
    }
}

/// A response that stops arriving for `stall` is abandoned then, instead of holding
/// the task for the whole request budget. The two shapes stall differently:
///
/// - A plain response has nothing to send until the model finishes, which can take
///   minutes, so only its body is limited: once headers arrive the body follows.
/// - A stream can legitimately outlast `stall`, but sends something all along, so
///   the limit applies to each read and not to the stream.
fn stall_limited_agent(streaming: bool, stall: Duration) -> ureq::Agent {
    let builder = crate::runtime::agent_builder().http_status_as_error(false);
    let builder = if streaming {
        builder
            .timeout_global(Some(STREAM_BUDGET))
            .timeout_per_read(Some(stall))
    } else {
        builder
            .timeout_global(Some(REQUEST_BUDGET))
            .timeout_recv_body(Some(stall))
    };
    builder.build().new_agent()
}

const REQUEST_BUDGET: Duration = Duration::from_secs(300);
/// Only a safety net: a stream that keeps sending is alive. Output limits end
/// every AI request long before this.
const STREAM_BUDGET: Duration = Duration::from_secs(30 * 60);
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
    /// Batches sent this run (a batch the service answered only in part counts once).
    #[serde(default)]
    pub batches: usize,
    /// Batches that kept some of their text unfinished after every retry. Running
    /// the same step again with the same task control sends only these: finished
    /// batches come back from their receipts.
    #[serde(default)]
    pub failed_batches: usize,
}

/// A transcript batch: at most [`BATCH`] events and [`BATCH_CHARS`] characters, and
/// the slide image sent with them.
type Chunk<'a> = (Option<std::path::PathBuf>, Vec<&'a mut TranscriptEvent>);

/// Cuts `events` into batches in order, between events and never inside one: a
/// batch closes at [`BATCH`] events or before the event that would take it past
/// [`BATCH_CHARS`]. An event longer than that on its own is a batch by itself.
fn into_chunks<'a>(
    image: Option<std::path::PathBuf>,
    events: impl Iterator<Item = &'a mut TranscriptEvent>,
    chunks: &mut Vec<Chunk<'a>>,
) {
    let mut batch = Vec::new();
    let mut chars = 0;
    for event in events {
        let length = event.text.chars().count();
        if !batch.is_empty() && (batch.len() == BATCH || chars + length > BATCH_CHARS) {
            chunks.push((image.clone(), std::mem::take(&mut batch)));
            chars = 0;
        }
        chars += length;
        batch.push(event);
    }
    if !batch.is_empty() {
        chunks.push((image, batch));
    }
}

/// One AI step's batch requests and what each answer does to its events.
struct Batches<'s, B, A> {
    chat: Chat<'s>,
    purpose: Purpose,
    body_for: B,
    apply: A,
    warned: std::sync::atomic::AtomicBool,
}

impl<B, A> Batches<'_, B, A>
where
    B: Fn(&[&str], Option<&str>) -> Result<serde_json::Value> + Sync,
    A: Fn(&mut [&mut TranscriptEvent], &[(usize, String)]) + Sync,
{
    /// Sends `events` as one request and applies the answer. An answer cut off at
    /// its output limit keeps the items it finished, and only the rest is asked
    /// again: as one smaller request, or split in two when nothing was finished
    /// (the same request would only stop at the same place). Returns the events
    /// finished, and why the others were not.
    fn send(
        &self,
        image: Option<&str>,
        events: &mut [&mut TranscriptEvent],
    ) -> (usize, Option<anyhow::Error>) {
        let (Some(first), Some(last)) = (events.first(), events.last()) else {
            return (0, None);
        };
        let (start, end) = (
            crate::timeline::fmt_ts(first.start),
            crate::timeline::fmt_ts(last.end),
        );
        let description = if self.purpose == Purpose::Proofreading {
            tr!(
                "校对 {}–{} 的文字 / Proofread transcript {}–{}",
                start,
                end,
                start,
                end
            )
        } else {
            tr!(
                "翻译 {}–{} 的文字 / Translate transcript {}–{}",
                start,
                end,
                start,
                end
            )
        };
        let items: Vec<&str> = events.iter().map(|e| e.text.as_str()).collect();
        let answer = (self.body_for)(&items, image).and_then(|body| {
            self.chat
                .segments(&body, &description, items.len(), batch_max_tokens(&items))
        });
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) => return (0, Some(error)),
        };
        (self.apply)(events, &answer.segments);
        if !answer.truncated {
            return (events.len(), None);
        }
        let finished: std::collections::HashSet<usize> =
            answer.segments.iter().map(|(id, _)| *id).collect();
        let total = events.len();
        let mut rest: Vec<&mut TranscriptEvent> = events
            .iter_mut()
            .enumerate()
            .filter(|(index, _)| !finished.contains(index))
            .map(|(_, event)| &mut **event)
            .collect();
        tracing::info!(
            finished = finished.len(),
            rest = rest.len(),
            "AI 回答达到输出上限，补发未完成的部分 / The answer reached its output limit; asking again for the rest"
        );
        let done = total - rest.len();
        if rest.is_empty() {
            return (done, None);
        }
        if done > 0 {
            let (more, error) = self.send(image, &mut rest);
            return (done + more, error);
        }
        if rest.len() == 1 {
            return (
                0,
                Some(anyhow::anyhow!(tr!(
                    "这一段的结果超出了输出上限 / The answer for this paragraph exceeds the output limit"
                ))),
            );
        }
        let middle = rest.len() / 2;
        let (left, right) = rest.split_at_mut(middle);
        let (left_done, left_error) = self.send(image, left);
        let (right_done, right_error) = self.send(image, right);
        (left_done + right_done, left_error.or(right_error))
    }
}

/// One AI step over transcript batches, on the shared worker pool. A batch counts
/// toward progress once taken. A failed batch keeps its text and records the first
/// cause, and never stops the note; Err = the task was stopped.
///
/// Every request goes through the task's ledger, so running a step again with the
/// same control is a retry of what failed: finished batches are read back from
/// their receipts and only the unfinished ones are sent.
fn run_batches(
    s: &LlmSettings,
    purpose: Purpose,
    chunks: Vec<Chunk<'_>>,
    attempted: usize,
    control: &crate::dispatch::TaskControl,
    body_for: impl Fn(&[&str], Option<&str>) -> Result<serde_json::Value> + Sync,
    apply: impl Fn(&mut [&mut TranscriptEvent], &[(usize, String)]) + Sync,
) -> Result<PolishReport> {
    let label = if purpose == Purpose::Proofreading {
        "llm"
    } else {
        "translate"
    };
    let pb = crate::progress::Bar::new(purpose.usage_stage(), chunks.len() as u64).with_template(
        &format!("{{spinner:.green}} {label} {{pos}}/{{len}} [{{bar:32.cyan/blue}}] {{msg}}"),
    );
    let batches = Batches {
        chat: Chat::new(s, purpose, control),
        purpose,
        body_for,
        apply,
        warned: std::sync::atomic::AtomicBool::new(false),
    };
    let first_error = std::sync::Mutex::new(None::<String>);
    let total = chunks.len();
    let counts = crate::pool::map_pool(
        chunks,
        s.concurrency.clamp(1, MAX_CONCURRENCY),
        control,
        |(image_path, mut chunk): Chunk<'_>| {
            pb.inc(1);
            let image = image_path
                .as_deref()
                .and_then(|path| section_image_b64(path, &batches.warned));
            let (done, error) = batches.send(image.as_deref(), &mut chunk);
            if let Some(error) = error {
                let cause = format!("{error:#}");
                warn_once(
                    &batches.warned,
                    &if purpose == Purpose::Proofreading {
                        tr!("校对未完成，已保留原文：{}", cause)
                    } else {
                        tr!("翻译未完成：{}", cause)
                    },
                );
                let mut first = crate::runtime::lock_recover(&first_error);
                first.get_or_insert_with(|| brief(&cause));
            }
            Ok((done, done < chunk.len()))
        },
    )?;
    pb.finish();
    let succeeded = counts.iter().map(|(done, _)| done).sum();
    Ok(PolishReport {
        attempted,
        succeeded,
        failed: attempted.saturating_sub(succeeded),
        note: crate::runtime::lock_recover(&first_error).take(),
        batches: total,
        failed_batches: counts.iter().filter(|(_, failed)| *failed).count(),
    })
}

/// 对已合并的 Section 做校对（在 merge 之后调用；配置已由调用方校验）。
/// - 失败批次保留原文（校对失败不阻断转换），不拆分、不改发另一种请求
/// - vision=true 时附对应截图（每批只读盘 + base64 一次）
/// - 模型对纯语气词条目返回空 text → 该条被删除
/// - 队列单元为 chunk：纯文本跨 Section 展平后每 BATCH 条一批；vision 按节分批
///   （同一请求只能附一张截图）。worker 池抢占式取活，已确认响应由请求账本复用。
pub fn polish_sections_report(
    sections: &mut [Section],
    frames_root: &Path,
    s: &LlmSettings,
    control: &crate::dispatch::TaskControl,
) -> Result<PolishReport> {
    let attempted = sections.iter().map(|section| section.speech.len()).sum();
    if attempted == 0 {
        return Ok(PolishReport::default());
    }
    let mut chunks = Vec::new();
    if s.vision {
        for section in sections
            .iter_mut()
            .filter(|section| !section.speech.is_empty())
        {
            let image = Some(frames_root.join(&section.image));
            into_chunks(image, section.speech.iter_mut(), &mut chunks);
        }
    } else {
        let events = sections
            .iter_mut()
            .flat_map(|section| section.speech.iter_mut());
        into_chunks(None, events, &mut chunks);
    }
    let report = run_batches(
        s,
        Purpose::Proofreading,
        chunks,
        attempted,
        control,
        |items, image| build_chat_body(s, items, image),
        apply_polish,
    )?;
    for section in sections.iter_mut() {
        // A formula spoken across a subtitle boundary comes back in two pieces.
        join_split_formulas(&mut section.speech);
        // 删除「成功校对为空串」的纯语气词条目（全部 chunk 完成后统一过滤）
        section.speech.retain(|e| !e.text.trim().is_empty());
    }
    Ok(report)
}

/// Translates every paragraph into `target`, kept beside the original.
/// Failed batches stay untranslated; the note is published either way.
/// A batch whose paragraphs already carry a translation (a saved note being
/// completed) is kept as it is and not sent: its receipt may be gone, and a
/// finished paid batch must not go out again. Batches keep the boundaries they
/// always had, so the remaining ones are the same requests as before.
pub fn translate_sections_report(
    sections: &mut [Section],
    s: &LlmSettings,
    target: &str,
    control: &crate::dispatch::TaskControl,
) -> Result<PolishReport> {
    let is_text = |event: &&mut TranscriptEvent| !event.text.trim().is_empty();
    let attempted = sections
        .iter_mut()
        .flat_map(|section| section.speech.iter_mut())
        .filter(is_text)
        .count();
    if attempted == 0 {
        return Ok(PolishReport::default());
    }
    let mut chunks = Vec::new();
    let events = sections
        .iter_mut()
        .flat_map(|section| section.speech.iter_mut())
        .filter(is_text);
    into_chunks(None, events, &mut chunks);
    let (done, chunks): (Vec<_>, Vec<_>) = chunks
        .into_iter()
        .partition(|(_, chunk)| chunk.iter().any(|event| event.translation.is_some()));
    let kept: usize = done.iter().map(|(_, chunk)| chunk.len()).sum();
    let mut report = run_batches(
        s,
        Purpose::Translation,
        chunks,
        attempted - kept,
        control,
        |items, _| translation_body(s, target, items),
        |chunk, translated| {
            for (id, text) in translated {
                if let Some(event) = chunk.get_mut(*id) {
                    let text = text.trim();
                    event.translation = (!text.is_empty()).then(|| text.to_owned());
                }
            }
        },
    )?;
    report.attempted += kept;
    report.succeeded += kept;
    Ok(report)
}

/// The batch as the JSON the service gets.
fn batch_payload(items: &[&str]) -> Result<String> {
    let payload: Vec<serde_json::Value> = items
        .iter()
        .enumerate()
        .map(|(i, t)| serde_json::json!({"id": i, "text": t}))
        .collect();
    Ok(serde_json::to_string(&payload)?)
}

fn translation_body(s: &LlmSettings, target: &str, items: &[&str]) -> Result<serde_json::Value> {
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
        serde_json::Value::String(batch_payload(items)?),
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

/// 构造 /chat/completions 请求体（独立出来便于单测覆盖视觉路径）。
fn build_chat_body(
    s: &LlmSettings,
    items: &[&str],
    image_b64: Option<&str>,
) -> Result<serde_json::Value> {
    let system = PROOFREADING_RULE.system(effective_prompt(s), image_b64.is_some());
    let mut content = vec![serde_json::json!({
        "type": "text",
        "text": batch_payload(items)?,
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

/// The same proofreading request as earlier CLI versions sent it in another interface
/// language: `body` with its system text in that language's wording.
fn former_proofreading_bodies(s: &LlmSettings, body: &serde_json::Value) -> Vec<serde_json::Value> {
    let prompt = effective_prompt(s);
    let Some(vision) = [false, true]
        .into_iter()
        .find(|&vision| contains_string(body, &PROOFREADING_RULE.system(prompt, vision)))
    else {
        return Vec::new();
    };
    let current = PROOFREADING_RULE.system(prompt, vision);
    FORMER_PROOFREADING_RULES
        .iter()
        .map(|rule| replace_string(body, &current, &rule.system(prompt, vision)))
        .collect()
}

fn contains_string(value: &serde_json::Value, text: &str) -> bool {
    match value {
        serde_json::Value::String(value) => value == text,
        serde_json::Value::Array(items) => items.iter().any(|item| contains_string(item, text)),
        serde_json::Value::Object(map) => map.values().any(|item| contains_string(item, text)),
        _ => false,
    }
}

fn replace_string(value: &serde_json::Value, from: &str, to: &str) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) if value == from => to.into(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| replace_string(item, from, to))
            .collect(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(key, item)| (key.clone(), replace_string(item, from, to)))
            .collect(),
        other => other.clone(),
    }
}

/// 从模型输出提取校对结果。约定契约为顶层对象 {"segments":[{"id":n,"text":"..."}]}
/// （与 response_format=json_object 一致）；兼容模型无视指令仍返回
/// 顶层数组的情况。两者都容忍代码围栏、前后杂文与尾逗号。
pub fn parse_segments(content: &str) -> Option<Vec<(usize, String)>> {
    extract_json_object(content)
        .and_then(|object| parse_items(object.get("segments")?.as_array()?))
        .or_else(|| parse_id_text_pairs(content))
}

/// 首个 `open` 到末个 `close` 之间（含两端）的文本。
fn between(content: &str, open: char, close: char) -> Option<&str> {
    let (start, end) = (content.find(open)?, content.rfind(close)?);
    (end > start).then(|| &content[start..=end])
}

fn tolerant_json(slice: &str) -> Option<serde_json::Value> {
    serde_json::from_str(slice)
        .or_else(|_| serde_json::from_str(&clean_trailing_commas(slice)))
        .ok()
}

/// 截取首个 { 到末个 } 解析为 JSON 对象；容忍尾逗号。
pub fn extract_json_object(content: &str) -> Option<serde_json::Value> {
    tolerant_json(between(content, '{', '}')?)
}

/// 从模型输出提取 [{"id":n,"text":"..."}]（容忍代码围栏、前后杂文、尾逗号与个别坏项）。
fn parse_id_text_pairs(content: &str) -> Option<Vec<(usize, String)>> {
    let slice = between(content, '[', ']')?;
    tolerant_json(slice)
        .and_then(|items| parse_items(items.as_array()?))
        .or_else(|| lenient_scan(slice))
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

fn segment(item: &serde_json::Value) -> Option<(usize, String)> {
    let id = item.get("id")?.as_u64()? as usize;
    Some((id, restore_latex(item.get("text")?.as_str()?)))
}

fn parse_items(items: &[serde_json::Value]) -> Option<Vec<(usize, String)>> {
    items
        .iter()
        .map(segment)
        .collect::<Option<Vec<_>>>()
        .filter(|items| !items.is_empty())
}

fn clean_trailing_commas(s: &str) -> String {
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

/// 逐个扫描 {...} 对象，坏项跳过；能取到至少一项即返回。
fn lenient_scan(s: &str) -> Option<Vec<(usize, String)>> {
    let mut out = vec![];
    let mut at = 0;
    while let Some(start) = s[at..].find('{').map(|offset| at + offset) {
        let mut values =
            serde_json::Deserializer::from_str(&s[start..]).into_iter::<serde_json::Value>();
        at = match values.next() {
            Some(Ok(value)) => {
                out.extend(segment(&value));
                start + values.byte_offset()
            }
            _ => start + 1,
        };
    }
    (!out.is_empty()).then_some(out)
}

/// One AI step's requests: the service, its connection pool and the ledger.
pub(crate) struct Chat<'a> {
    agent: ureq::Agent,
    settings: &'a LlmSettings,
    purpose: Purpose,
    control: &'a crate::dispatch::TaskControl,
    policy: crate::pool::RetryPolicy,
    resend: crate::dispatch::Resend,
}

/// A service's answer: its text, and whether it stopped at its output limit.
pub(crate) struct Answer {
    pub content: String,
    pub truncated: bool,
}

/// A batch's answer as (id, text). A truncated answer holds the items it finished,
/// which may be none; an answer that was not cut off holds every item.
pub(crate) struct Segments {
    pub segments: Vec<(usize, String)>,
    pub truncated: bool,
}

/// The finished items of an answer cut off at its output limit: every complete
/// `{"id":…,"text":…}` object for an input id, the first one for each id.
fn salvage_segments(content: &str, expected: usize) -> Vec<(usize, String)> {
    let found = parse_segments(content)
        .or_else(|| lenient_scan(content))
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter(|(id, _)| *id < expected && seen.insert(*id))
        .collect()
}

impl<'a> Chat<'a> {
    pub(crate) fn new(
        settings: &'a LlmSettings,
        purpose: Purpose,
        control: &'a crate::dispatch::TaskControl,
    ) -> Self {
        Self {
            agent: stall_limited_agent(true, STALL_TIMEOUT),
            settings,
            purpose,
            control,
            policy: retry_policy(settings),
            resend: resend_rule(settings),
        }
    }

    /// 发 chat/completions 请求并返回 message.content。`validate` 校验正文结构，在请求
    /// 账本里执行：无效响应记为失败，不会被当成已完成复用。它也收到回答是否在输出
    /// 上限处截断：截断的回答可以只有部分内容。
    ///
    /// 兼容性降级：部分 OpenAI 兼容端点不支持 `response_format: json_object`，
    /// 仅当服务明确拒绝这个字段（400/422）时去掉该字段重试一次。A service that
    /// refuses the request for turning thinking off is asked once more without it.
    pub(crate) fn send(
        &self,
        body: &serde_json::Value,
        description: &str,
        delivery: crate::provider::Delivery,
        validate: &dyn Fn(&str, bool) -> Result<()>,
    ) -> Result<Answer> {
        let s = self.settings;
        let attempt = |identity: &serde_json::Value, sent: &serde_json::Value| {
            crate::pool::retry_with(self.control, &self.policy, || {
                self.once(identity, sent, description, validate)
            })
        };
        let rejected_parameters = |error: &anyhow::Error| {
            error
                .downcast_ref::<crate::dispatch::Failure>()
                .is_some_and(|failure| matches!(failure.status, Some(400 | 422)))
        };
        let sent = crate::provider::sent_body(s, body, delivery);
        let reply = match attempt(body, &sent) {
            Ok(reply) => reply,
            Err(first)
                if crate::provider::controls_reasoning(s, delivery)
                    && rejected_parameters(&first) =>
            {
                // The service does not know how to turn thinking off. The request
                // without it is another request, under its own receipt.
                let plain = crate::provider::Delivery {
                    no_reasoning: false,
                    ..delivery
                };
                let identity = serde_json::json!({"request": body, "reasoning": "default"});
                attempt(&identity, &crate::provider::sent_body(s, body, plain))?
            }
            Err(first) => {
                let degradable = first
                    .downcast_ref::<crate::dispatch::Failure>()
                    .is_some_and(|failure| failure.unsupported_response_format);
                if !(degradable && crate::provider::has_structured_format(s, body)) {
                    return Err(first);
                }
                let mut relaxed = body.clone();
                crate::provider::strip_structured_format(s, &mut relaxed);
                let sent = crate::provider::sent_body(s, &relaxed, delivery);
                // 原请求已按重试策略重试过，这里只验证兼容性：只试一次
                let reply = self.once(&relaxed, &sent, description, validate)?;
                tracing::debug!("端点不支持 response_format，降级重试成功");
                reply
            }
        };
        let resp = reply.value;
        // token 用量实时反馈：GUI 阶段行据此跳动（上传=prompt，下载=completion）。
        // 各方言的流已在 provider 归一成 chat/completions 同一形状。
        let usage = &resp["usage"];
        if !reply.replayed {
            crate::progress::note_tokens(
                self.purpose.usage_stage(),
                usage["prompt_tokens"].as_u64().unwrap_or(0),
                usage["completion_tokens"].as_u64().unwrap_or(0),
            );
        }
        // 账本已校验正文存在且结构有效
        Ok(Answer {
            content: resp["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            truncated: crate::provider::truncated(&resp),
        })
    }

    /// One attempt; validation errors omit provider response bodies which may echo
    /// credentials. `sent` goes out as a stream; `identity` is what the ledger knows
    /// the request by.
    fn once(
        &self,
        identity: &serde_json::Value,
        sent: &serde_json::Value,
        description: &str,
        validate: &dyn Fn(&str, bool) -> Result<()>,
    ) -> std::result::Result<crate::dispatch::Reply, crate::dispatch::Failure> {
        let s = self.settings;
        let url = crate::provider::endpoint(s);
        let headers = crate::provider::auth_headers(s).map_err(crate::dispatch::Failure::local)?;
        let former = if self.purpose == Purpose::Proofreading {
            former_proofreading_bodies(s, identity)
        } else {
            Vec::new()
        };
        self.control.request(
            &crate::dispatch::RequestSpec {
                service: "llm",
                purpose: self.purpose.into(),
                description,
                endpoint: &url,
                identity,
                former: &former,
                resend: self.resend,
            },
            || {
                let mut request = self.agent.post(&url);
                for (name, value) in &headers {
                    request = request.header(name, value);
                }
                let response = crate::runtime::receive(crate::runtime::send_json(request, sent))?;
                // 流聚合为 chat/completions 形状，下游校验与解析不变；
                // 非 2xx 原样上交（重试/降级逻辑要看状态码）。An unfinished stream is a
                // lost answer: the service may have produced (and charged) it.
                if (200..300).contains(&response.status) {
                    let canonical = crate::provider::stream_to_chat_json(s, &response.body)
                        .map_err(|e| crate::dispatch::NetworkFailure {
                            message: format!("{e:#}"),
                            definitely_unsent: false,
                        })?;
                    return Ok(crate::dispatch::HttpResponse {
                        body: serde_json::to_vec(&canonical).unwrap_or_default(),
                        ..response
                    });
                }
                Ok(response)
            },
            |value| {
                // A gateway may return HTTP 200 with an error object. Missing content is a
                // protocol failure, never an empty success.
                anyhow::ensure!(
                    value.get("error").is_none_or(serde_json::Value::is_null),
                    tr!("AI 服务返回了错误 / The AI service returned an error")
                );
                let truncated = crate::provider::truncated(value);
                let content = value["choices"][0]["message"]["content"].as_str();
                let content = if truncated {
                    content.unwrap_or_default()
                } else {
                    content.filter(|s| !s.trim().is_empty()).context(tr!(
                        "AI 服务没有返回内容 / The AI service returned no content"
                    ))?
                };
                validate(content, truncated)
            },
        )
    }

    /// A batch's reply as (id, text). A complete reply has one entry per input item;
    /// a reply cut off at its output limit is kept as it is, a definite answer, and
    /// yields the items it finished, so asking again for the rest never resends
    /// what was answered.
    fn segments(
        &self,
        body: &serde_json::Value,
        description: &str,
        expected: usize,
        max_tokens: u32,
    ) -> Result<Segments> {
        let proofreading = self.purpose == Purpose::Proofreading;
        let parsed = |content: &str| {
            parse_segments(content).context(if proofreading {
                tr!("服务返回的校对结构无效 / Invalid proofreading structure")
            } else {
                tr!("服务返回的翻译结构无效 / Invalid translation structure")
            })
        };
        let delivery = crate::provider::Delivery {
            max_tokens: Some(max_tokens),
            no_reasoning: true,
        };
        let answer = self.send(body, description, delivery, &|content, truncated| {
            if truncated {
                return Ok(());
            }
            anyhow::ensure!(
                segment_ids_match(&parsed(content)?, expected),
                if proofreading {
                    tr!(
                        "校对结果与原文段落不对应，已保留原文 / Proofread segments do not match the input"
                    )
                } else {
                    tr!(
                        "翻译结果与原文段落不对应 / Translated segments do not match the input"
                    )
                }
            );
            Ok(())
        })?;
        if answer.truncated {
            return Ok(Segments {
                segments: salvage_segments(&answer.content, expected),
                truncated: true,
            });
        }
        Ok(Segments {
            segments: parsed(&answer.content)?,
            truncated: false,
        })
    }
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

/// Why [`test_connection`]'s request got no usable answer, in the error chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The service answered with this HTTP error status.
    #[error("HTTP {0}")]
    Status(u16),
    /// No answer arrived; the text says why.
    #[error("{0}")]
    Transport(String),
}

impl From<ureq::Error> for RequestError {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::StatusCode(status) => Self::Status(status),
            other => Self::Transport(other.to_string()),
        }
    }
}

/// 用最小请求验证端点与凭据可用；vision=true 时附带测试图片，
/// 同时验证图片输入路径（失败会明确报出，不会显示为已验证可用）。
pub fn test_connection(s: &LlmSettings) -> Result<()> {
    validate(s)?;
    let user: serde_json::Value = if s.vision {
        serde_json::json!([
            {"type": "text", "text": TEST_QUESTION},
            {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{TEST_PNG_B64}")}},
        ])
    } else {
        serde_json::Value::String(TEST_QUESTION.into())
    };
    let body = crate::provider::test_body(s, user);
    let fail_hint = if s.vision {
        tr!(
            "连接失败。请确认服务地址、密钥及模型，并检查图片输入支持。 / Connection failed. Check the URL, API key, model, and image input support."
        )
    } else {
        tr!(
            "连接失败。请检查服务地址、密钥和模型。 / Connection failed. Check the URL, API key, and model."
        )
    };
    // 与生产路径同一纪律：禁止跟随重定向；空密钥不发送 Bearer 头（本地无鉴权服务）
    let agent = crate::runtime::http_agent(Duration::from_secs(60));
    let mut request = agent.post(&crate::provider::endpoint(s));
    for (name, value) in crate::provider::auth_headers(s)? {
        request = request.header(&name, &value);
    }
    // The cause follows the hint after a colon, so the hint's own full stop goes.
    let mut resp = crate::runtime::send_json(request, &body)
        .map_err(RequestError::from)
        .context(fail_hint.trim_end_matches(['.', '。']))?;
    let v: serde_json::Value = if crate::provider::is_sse(s) {
        let bytes = read_sse_body(resp.into_body().into_reader())?;
        crate::provider::sse_to_chat_json(&bytes)?
    } else {
        resp.body_mut()
            .read_json()
            .context(tr!("无法解析响应 / Cannot parse response"))?
    };
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim();
    anyhow::ensure!(
        !text.is_empty(),
        tr!(
            "服务返回空响应；请确认模型可用 / The service returned an empty response; make sure the model works"
        )
    );
    Ok(())
}

/// The whole streamed body, or an error: a test of a truncated stream would judge
/// a response nobody has read to its end.
fn read_sse_body(reader: impl std::io::Read) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    reader
        .take(SSE_BODY_LIMIT + 1)
        .read_to_end(&mut bytes)
        .context(tr!("服务响应接收中断 / Service response was interrupted"))?;
    anyhow::ensure!(
        bytes.len() as u64 <= SSE_BODY_LIMIT,
        tr!("服务响应超过接收上限 / Service response exceeds the receiving limit")
    );
    Ok(bytes)
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
    let interactive = crate::runtime::interactive();
    // 空输入保留当前值
    let ask = |prompt: &str, current: &mut String| -> Result<()> {
        let value: String = dialoguer::Input::new()
            .with_prompt(prompt)
            .with_initial_text(current.as_str())
            .allow_empty(true)
            .interact_text()?;
        if !value.trim().is_empty() {
            *current = value.trim().to_string();
        }
        Ok(())
    };
    if let Some(v) = base_url {
        cfg.llm.base_url = v.trim().to_string();
    } else if interactive {
        ask(
            tr!("服务地址 / Base URL (OpenAI-compatible, e.g. https://api.deepseek.com/v1)"),
            &mut cfg.llm.base_url,
        )?;
    }
    if let Some(v) = api_key {
        cfg.llm.api_key = v;
    } else if interactive {
        let keep_hint = if cfg.llm.api_key.is_empty() {
            tr!(
                "（输入隐藏；无密钥的本地服务可留空） / API key (hidden; leave blank for a local service without authentication)"
            )
        } else {
            tr!("（输入隐藏；回车保留已保存的密钥） / API key (hidden; Enter keeps the saved key)")
        };
        let v = dialoguer::Password::new()
            .with_prompt(format!("API Key{keep_hint}"))
            .allow_empty_password(true)
            .interact()?;
        if !v.trim().is_empty() {
            cfg.llm.api_key = v.trim().to_string();
        }
    }
    if let Some(v) = model {
        cfg.llm.model = v.trim().to_string();
    } else if interactive {
        ask(
            tr!("模型名 / Model name (e.g. deepseek-chat)"),
            &mut cfg.llm.model,
        )?;
    }
    anyhow::ensure!(
        !cfg.llm.base_url.trim().is_empty() && !cfg.llm.model.trim().is_empty(),
        tr!(
            "需要填写服务地址和模型名。在终端运行 course2md llm setup，或传入 --base-url 和 --model / A base URL and model are required. Run course2md llm setup in a terminal, or pass --base-url and --model."
        )
    );
    // 容错：没写 scheme 时补 https://
    if !cfg.llm.base_url.is_empty() && !cfg.llm.base_url.contains("://") {
        cfg.llm.base_url = format!("https://{}", cfg.llm.base_url.trim());
    }
    // 视觉能力仅交互式终端询问（脚本化调用全部传参时不阻塞）
    if interactive {
        cfg.llm.vision = crate::runtime::choose(
            tr!(
                "校对时附上幻灯片截图？需支持图片的模型。 / Attach slide images when polishing? Requires an image-capable model."
            ),
            &[
                tr!("仅发送文字 / Send text only"),
                tr!("发送文字和截图 / Send text and slide images"),
            ],
            usize::from(cfg.llm.vision),
            tr!("已取消设置，未保存配置。 / Setup cancelled; no configuration saved."),
        )? == 1;
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
            tr!("开启 / enabled")
        } else {
            tr!("关闭 / disabled")
        }
    };
    let or_dash = |value: &'_ str| {
        if value.is_empty() {
            "—".to_owned()
        } else {
            value.to_owned()
        }
    };
    let key = if s.api_key.is_empty() {
        tr!("未设置 / not set")
    } else {
        tr!("已设置（隐藏） / set (hidden)")
    };
    println_tr!(
        "配置文件 / Configuration: {}",
        crate::settings::config_path().display()
    );
    println_tr!("  LLM 校对 / Transcript polish: {}", state(s.enabled));
    println_tr!("  服务类型 / Provider: {}", s.provider);
    println_tr!("  服务地址 / Base URL: {}", or_dash(&s.base_url));
    println_tr!("  API 密钥 / API key: {}", key);
    println_tr!("  模型 / Model: {}", or_dash(&s.model));
    println_tr!(
        "  截图辅助校对 / Slide-assisted polish: {}",
        state(s.vision)
    );
    println_tr!("  自动摘要 / Automatic summary: {}", state(s.summarize));
    println_tr!("  并发请求 / Concurrent requests: {}", s.concurrency);
    println_tr!(
        "  失败自动重试 / Automatic retries: {}",
        if s.auto_retry {
            tr!(
                "开启，每个请求最多发送 {} 次 / on, up to {} sends per request",
                s.retry_attempts,
                s.retry_attempts
            )
        } else {
            state(false).to_owned()
        }
    );
    println_tr!("  使用提示 / Usage hint: {}", state(!s.disable_hint));
    if !s.enabled {
        println_tr!("开启校对 / Enable transcript polish: course2md llm setup");
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

    /// Answers one connection by running `script` on it, after reading the request.
    fn stalling_server(script: impl FnOnce(&mut std::net::TcpStream) + Send + 'static) -> String {
        use std::io::Read as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read the whole request: answering before the body is in lets
            // the client's write fail (EINVAL on macOS) instead of the status.
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            while let Ok(n @ 1..) = stream.read(&mut chunk) {
                request.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&request);
                let Some(end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let length = text[..end]
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            script(&mut stream);
        });
        url
    }

    fn hold(seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }

    #[test]
    fn a_body_that_stops_arriving_is_abandoned_at_the_stall_limit() {
        use std::io::Write as _;
        let url = stalling_server(|stream| {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial"
            )
            .unwrap();
            stream.flush().unwrap();
            hold(30.0);
        });
        let agent = stall_limited_agent(false, Duration::from_millis(500));
        let started = std::time::Instant::now();
        let failure = crate::runtime::receive(agent.post(&url).send_json(serde_json::json!({})))
            .err()
            .expect("a stalled body is not a response");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!failure.definitely_unsent, "the request was already sent");
    }

    #[test]
    fn a_slow_model_is_not_a_stalled_response() {
        use std::io::Write as _;
        // Thinks for longer than the stall limit before the first byte.
        let url = stalling_server(|stream| {
            hold(1.5);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
        });
        let agent = stall_limited_agent(false, Duration::from_millis(500));
        let response = crate::runtime::receive(agent.post(&url).send_json(serde_json::json!({})))
            .ok()
            .expect("waiting for the first byte is not a stall");
        assert_eq!(response.body, b"ok");
    }

    #[test]
    fn a_stream_may_outlast_the_limit_but_not_go_quiet() {
        use std::io::Write as _;
        // Longer in total than the limit, with no silence as long as it.
        let url = stalling_server(|stream| {
            write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n").unwrap();
            for event in 0..5 {
                write!(stream, "data: {event}\n\n").unwrap();
                stream.flush().unwrap();
                hold(0.4);
            }
        });
        let agent = stall_limited_agent(true, Duration::from_secs(2));
        let started = std::time::Instant::now();
        let response = crate::runtime::receive(agent.post(&url).send_json(serde_json::json!({})))
            .ok()
            .expect("a stream that keeps sending is alive");
        assert!(started.elapsed() > Duration::from_millis(1500));
        assert!(response.body.ends_with(b"data: 4\n\n"));

        let url = stalling_server(|stream| {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ndata: 0\n\n"
            )
            .unwrap();
            stream.flush().unwrap();
            hold(30.0);
        });
        let agent = stall_limited_agent(true, Duration::from_millis(500));
        let started = std::time::Instant::now();
        let failure = crate::runtime::receive(agent.post(&url).send_json(serde_json::json!({})))
            .err()
            .expect("a silent stream is stalled");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!failure.definitely_unsent);
    }

    #[test]
    fn connection_test_failures_name_their_cause_without_exposing_the_http_client() {
        use std::io::Write as _;
        let url = stalling_server(|stream| {
            write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let settings = LlmSettings {
            base_url: url,
            model: "m".into(),
            ..Default::default()
        };
        let error = test_connection(&settings).unwrap_err();
        assert!(
            error
                .chain()
                .any(|cause| cause.downcast_ref() == Some(&RequestError::Status(401))),
            "{error:#}"
        );
    }

    #[test]
    fn an_oversized_stream_is_an_error_not_a_shorter_stream() {
        use std::io::Read as _;
        let at_limit = std::io::repeat(b'x').take(SSE_BODY_LIMIT);
        assert_eq!(
            read_sse_body(at_limit).unwrap().len() as u64,
            SSE_BODY_LIMIT
        );
        assert!(read_sse_body(std::io::repeat(b'x')).is_err());
    }

    #[test]
    fn debug_never_contains_the_api_key() {
        let settings = LlmSettings {
            api_key: "secret-key".into(),
            ..Default::default()
        };
        let debug = format!("{settings:?}");
        assert!(!debug.contains("secret-key"), "{debug}");
        assert!(debug.contains("<set>"), "{debug}");
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
            auto_retry: true,
            retry_attempts: 4,
        }
    }

    #[test]
    fn translation_keeps_originals_and_adds_each_translation() {
        let content = serde_json::json!({"segments":[{"id":0,"text":"Today: compilers."},{"id":1,"text":"Next, parsing."}]});
        let body = serde_json::json!({"choices":[{"message":{"content":content.to_string()}}]})
            .to_string();
        let (address, server) =
            crate::test_http::serve(vec![crate::test_http::json_response(&body)]);
        let server = std::thread::spawn(move || {
            String::from_utf8_lossy(&server.join().unwrap().remove(0).body).into_owned()
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
        settings.base_url = format!("{address}/v1");
        settings.concurrency = 1;
        let report =
            translate_sections_report(&mut sections, &settings, "en", &Default::default()).unwrap();
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
        let (mut a, mut b) = (event(tr!("今天讲编译原理")), event(tr!("啊")));
        let mut refs = vec![&mut a, &mut b];
        let polished = [(0, tr!("今天讲编译原理").into()), (1, String::new())];
        assert!(segment_ids_match(&polished, refs.len()));
        apply_polish(&mut refs, &polished);
        assert_eq!(a.raw, None, "未改动的条目不记 raw");
        assert_eq!(b.text, "", "纯语气词被置空，随后由调用方删除");
        assert_eq!(b.raw.as_deref(), Some(tr!("啊")), "原文进 raw 溯源");
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
        assert!(parse_segments(tr!("没有 JSON")).is_none());
    }

    #[test]
    fn parse_id_pairs_tolerates_fences() {
        let got = parse_id_text_pairs(
            "```json\n[{\"id\":0,\"text\":\"a\"},{\"id\":1,\"text\":\"b\"}]\n```",
        )
        .unwrap();
        assert_eq!(got, vec![(0, "a".into()), (1, "b".into())]);
        assert!(parse_id_text_pairs(tr!("没有数组")).is_none());
        assert!(parse_id_text_pairs("[]").is_none());
    }

    fn speech(text: &str) -> TranscriptEvent {
        TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: text.into(),
            raw: None,
            translation: None,
        }
    }

    #[test]
    fn batches_break_between_events_by_count_and_length_in_order() {
        let long = "长".repeat(BATCH_CHARS + 10);
        let mut events: Vec<TranscriptEvent> = (0..25)
            .map(|i| speech(&format!("第{i}段")))
            .chain([speech(&long), speech("尾")])
            .collect();
        let mut chunks = Vec::new();
        into_chunks(None, events.iter_mut(), &mut chunks);
        let sizes: Vec<usize> = chunks.iter().map(|(_, chunk)| chunk.len()).collect();
        // 20 short events, the 5 after them, the oversized one alone, then the rest.
        assert_eq!(sizes, [20, 5, 1, 1]);
        let order: Vec<&str> = chunks
            .iter()
            .flat_map(|(_, chunk)| chunk.iter().map(|e| e.text.as_str()))
            .collect();
        let expected: Vec<String> = (0..25)
            .map(|i| format!("第{i}段"))
            .chain([long.clone(), "尾".into()])
            .collect();
        assert_eq!(order, expected, "nothing is lost, split or reordered");

        // Characters close a batch before the event that would cross the limit.
        let half = "半".repeat(BATCH_CHARS / 2);
        let mut events = [speech(&half), speech(&half), speech("x")];
        let mut chunks = Vec::new();
        into_chunks(None, events.iter_mut(), &mut chunks);
        let sizes: Vec<usize> = chunks.iter().map(|(_, chunk)| chunk.len()).collect();
        assert_eq!(sizes, [2, 1]);
        for (_, chunk) in &chunks {
            let chars: usize = chunk.iter().map(|e| e.text.chars().count()).sum();
            assert!(chars <= BATCH_CHARS);
        }
    }

    #[test]
    fn the_output_budget_follows_the_batch() {
        assert_eq!(batch_max_tokens(&["短"]), MIN_BATCH_TOKENS);
        let full = "字".repeat(BATCH_CHARS);
        let budget = batch_max_tokens(&[full.as_str()]);
        assert!(budget > 2 * BATCH_CHARS as u32 && budget < CHAT_MAX_TOKENS / 2);
        let huge = "字".repeat(20_000);
        assert_eq!(batch_max_tokens(&[huge.as_str()]), CHAT_MAX_TOKENS);
    }

    #[test]
    fn a_truncated_answer_keeps_its_finished_items() {
        // Cut off inside the third item: no closing `]}`.
        let content = r#"{"segments":[{"id":0,"text":"甲 $\\frac{a}{b}$"},{"id":1,"text":"乙"},{"id":2,"text":"丙丙丙"#;
        assert!(parse_segments(content).is_none() || parse_segments(content).unwrap().len() < 3);
        assert_eq!(
            salvage_segments(content, 3),
            vec![(0, "甲 $\\frac{a}{b}$".into()), (1, "乙".into())]
        );
        // Out-of-range and repeated ids (a model repeating itself) are dropped.
        let content = r#"{"segments":[{"id":0,"text":"a"},{"id":9,"text":"x"},{"id":0,"text":"again"},{"id":1,"text":"b"#;
        assert_eq!(salvage_segments(content, 2), vec![(0, "a".into())]);
        assert!(salvage_segments("{\"segm", 2).is_empty());
    }

    /// A loopback service: `answer(n, items)` replies to the n-th request (from 0),
    /// given the batch it carries. Returns the base URL and every request body.
    fn scripted_service(
        answer: impl Fn(usize, &[String]) -> String + Send + 'static,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let bodies = seen.clone();
        std::thread::spawn(move || {
            for (index, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { return };
                let mut data = Vec::new();
                let mut chunk = [0u8; 8192];
                let body = loop {
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
                        let length: usize = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .map_or(0, |value| value.trim().parse().unwrap());
                        if data.len() >= end + 4 + length {
                            break data[end + 4..end + 4 + length].to_vec();
                        }
                    }
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break Vec::new(),
                        Ok(n) => data.extend_from_slice(&chunk[..n]),
                    }
                };
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                let items: Vec<String> = body["messages"][1]["content"][0]["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Vec<serde_json::Value>>(text).ok())
                    .unwrap_or_default()
                    .iter()
                    .map(|item| item["text"].as_str().unwrap_or_default().to_owned())
                    .collect();
                bodies.lock().unwrap().push(body);
                let _ = stream.write_all(answer(index, &items).as_bytes());
            }
        });
        (url, seen)
    }

    /// A streamed answer carrying `content`, ending with `finish`.
    fn streamed(content: &str, finish: &str) -> String {
        let events = [
            serde_json::json!({"choices": [{"delta": {"content": content}}]}),
            serde_json::json!({"choices": [{"delta": {}, "finish_reason": finish}]}),
        ];
        let body: String = events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .chain(["data: [DONE]\n\n".to_owned()])
            .collect();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Every item proofread as `<text>!`, ids `from..`.
    fn proofread(items: &[String]) -> String {
        let segments: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(id, text)| serde_json::json!({"id": id, "text": format!("{text}!")}))
            .collect();
        serde_json::json!({ "segments": segments }).to_string()
    }

    fn status(code: u16) -> String {
        format!("HTTP/1.1 {code} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }

    fn polish_settings(url: String, attempts: u32) -> LlmSettings {
        LlmSettings {
            base_url: url,
            concurrency: 1,
            retry_attempts: attempts,
            ..test_settings()
        }
    }

    fn one_section(texts: &[&str]) -> Vec<Section> {
        vec![Section {
            t: 0.0,
            end: 1.0,
            image: String::new(),
            speech: texts.iter().map(|text| speech(text)).collect(),
        }]
    }

    fn texts(sections: &[Section]) -> Vec<String> {
        sections[0].speech.iter().map(|e| e.text.clone()).collect()
    }

    #[test]
    fn running_again_sends_only_the_batches_that_failed() {
        let failing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let fail = failing.clone();
        let (url, bodies) = scripted_service(move |_, items| {
            if fail.load(std::sync::atomic::Ordering::SeqCst) && items.iter().any(|i| i == "坏") {
                status(502)
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        // Three batches of 20; the middle one meets a failing gateway.
        let mut texts_in: Vec<String> = (0..60).map(|i| format!("段{i}")).collect();
        texts_in[25] = "坏".into();
        let refs: Vec<&str> = texts_in.iter().map(String::as_str).collect();
        let settings = polish_settings(url, 1);
        let work = tempfile::tempdir().unwrap();
        let control = crate::dispatch::TaskControl::open(work.path(), Default::default()).unwrap();

        let mut sections = one_section(&refs);
        let report =
            polish_sections_report(&mut sections, Path::new(""), &settings, &control).unwrap();
        assert_eq!((report.batches, report.failed_batches), (3, 1));
        assert_eq!((report.succeeded, report.failed), (40, 20));
        assert_eq!(texts(&sections)[25], "坏", "a failed batch keeps its text");
        assert_eq!(bodies.lock().unwrap().len(), 3);

        // The service recovers; the same step again sends the failed batch alone.
        failing.store(false, std::sync::atomic::Ordering::SeqCst);
        let mut sections = one_section(&refs);
        let report =
            polish_sections_report(&mut sections, Path::new(""), &settings, &control).unwrap();
        assert_eq!((report.succeeded, report.failed_batches), (60, 0));
        let bodies = bodies.lock().unwrap();
        assert_eq!(
            bodies.len(),
            4,
            "finished batches come back from their receipts"
        );
        assert!(bodies[3].to_string().contains("坏"));
        assert!(texts(&sections).iter().all(|text| text.ends_with('!')));
    }

    #[test]
    fn failures_are_retried_automatically_but_rejections_are_not() {
        let (url, bodies) = scripted_service(|index, items| match index {
            0 => status(503),
            _ => streamed(&proofread(items), "stop"),
        });
        let mut sections = one_section(&["一", "二"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 3),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.succeeded, 2);
        assert_eq!(bodies.lock().unwrap().len(), 2);

        let (url, bodies) = scripted_service(|_, _| status(401));
        let mut sections = one_section(&["一"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 3),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(
            bodies.lock().unwrap().len(),
            1,
            "a refused key is not retried"
        );
    }

    #[test]
    fn a_cut_off_answer_keeps_what_it_finished_and_asks_for_the_rest() {
        let (url, bodies) = scripted_service(|index, items| match index {
            // Stops at its output limit inside the third item.
            0 => {
                let finished = &proofread(&items[..2]);
                let cut = format!(
                    "{},{{\"id\":2,\"text\":\"半",
                    finished.trim_end_matches("]}")
                );
                streamed(&cut, "length")
            }
            _ => streamed(&proofread(items), "stop"),
        });
        let mut sections = one_section(&["一", "二", "三", "四"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        assert_eq!((report.succeeded, report.failed), (4, 0));
        assert_eq!(texts(&sections), ["一!", "二!", "三!", "四!"]);
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        let second = bodies[1]["messages"][1]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(second.contains("三") && second.contains("四"));
        assert!(!second.contains("一"), "a finished item is not asked again");
        // Streamed, with an output budget sized to the batch.
        assert_eq!(bodies[0]["stream"], true);
        assert_eq!(bodies[0]["max_tokens"], MIN_BATCH_TOKENS);
    }

    #[test]
    fn a_cut_off_answer_with_nothing_finished_is_split() {
        let (url, bodies) = scripted_service(|_, items| {
            if items.len() > 1 {
                streamed("{\"segments\":[{\"id\":0,\"text\":\"", "length")
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        let mut sections = one_section(&["一", "二", "三"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.succeeded, 3);
        assert_eq!(texts(&sections), ["一!", "二!", "三!"]);
        // [一二三] → [一] + [二三] → [二] + [三]
        let sizes: Vec<usize> = bodies
            .lock()
            .unwrap()
            .iter()
            .map(|body| {
                let text = body["messages"][1]["content"][0]["text"].as_str().unwrap();
                serde_json::from_str::<Vec<serde_json::Value>>(text)
                    .unwrap()
                    .len()
            })
            .collect();
        assert_eq!(sizes, [3, 1, 2, 1, 1]);

        // A single paragraph that cannot fit fails alone, with its text kept.
        let (url, _) = scripted_service(|_, _| streamed("", "length"));
        let mut sections = one_section(&["一"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(texts(&sections), ["一"]);
    }
}
