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

/// The first wait after a failure the service did not call busy.
const RETRY_BASE: Duration = if cfg!(test) {
    Duration::from_millis(5)
} else {
    Duration::from_secs(1)
};
/// The first wait after a service said it is busy (429, 5xx). A rate limit
/// usually clears within a minute, which waits of 1 s, 2 s and 4 s used up in seconds.
const BUSY_BASE: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_secs(4)
};
/// The most sends for a request refused only for now (429, 503): it was not
/// processed, so the wait for a rate limit to clear may take more tries.
const BUSY_ATTEMPTS: u32 = 6;

/// How a failed AI request is retried. With automatic retries on: the configured
/// number of sends, waiting 1 s, 2 s, 4 s… up to 60 s, plus jitter; after the
/// service said it is busy 4 s, 8 s, 16 s…, and up to [`BUSY_ATTEMPTS`] sends for
/// a rate limit, about two minutes in all. Off: the requests that provably cost
/// nothing more (never sent, rate limited) as before.
fn retry_policy(s: &LlmSettings) -> crate::pool::RetryPolicy {
    if s.auto_retry {
        crate::pool::RetryPolicy {
            max_attempts: s.retry_attempts.clamp(1, MAX_RETRY_ATTEMPTS),
            base: RETRY_BASE,
            cap: Duration::from_secs(60),
            follows_service: true,
            busy_base: BUSY_BASE,
            busy_attempts: BUSY_ATTEMPTS,
        }
    } else {
        crate::pool::RetryPolicy {
            max_attempts: MAX_ATTEMPTS,
            follows_service: true,
            ..crate::pool::RetryPolicy::DEFAULT
        }
    }
}

/// Whether a request whose result is unknown may be sent again without asking.
/// A free service (a ChatGPT plan, a local server) always may, whatever another
/// process left behind; automatic retries add the answers that could not be used.
fn resend_rule(s: &LlmSettings) -> crate::dispatch::Resend {
    use crate::dispatch::Resend;
    match (crate::provider::free_to_resend(s), s.auto_retry) {
        (true, true) => Resend::FreeAutomatic,
        (true, false) => Resend::Free,
        (false, true) => Resend::Automatic,
        (false, false) => Resend::AskUser,
    }
}

/// The connection for AI requests. A service may think for minutes before its
/// first byte (a non-streaming answer only comes once it is finished), so the
/// transport waits up to `first_byte` for any read; the shorter silence that
/// ends a stream already flowing is [`receive_streamed`]'s.
fn stream_agent(first_byte: Duration) -> ureq::Agent {
    crate::runtime::agent_builder()
        .http_status_as_error(false)
        .timeout_global(Some(STREAM_BUDGET))
        .timeout_per_read(Some(first_byte))
        .build()
        .new_agent()
}

/// Reads a response to its end, abandoning it once its body has been silent for
/// `idle`: a stream can outlast any total limit, but sends something all along.
/// The reading thread left behind ends at the transport's own read limit.
fn receive_streamed(
    result: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    idle: Duration,
) -> std::result::Result<crate::dispatch::HttpResponse, crate::dispatch::NetworkFailure> {
    use std::io::Read as _;
    let response = match result {
        Ok(response) => response,
        Err(error) => return crate::runtime::receive(Err(error)),
    };
    let status = response.status().as_u16();
    let retry_after = crate::runtime::retry_after(response.headers());
    let mut reader = response.into_body().into_reader();
    let (chunks, received) = std::sync::mpsc::channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let read = reader.read(&mut buffer).map(|n| buffer[..n].to_vec());
            let end = !matches!(&read, Ok(bytes) if !bytes.is_empty());
            if chunks.send(read).is_err() || end {
                return;
            }
        }
    });
    let interrupted = || crate::dispatch::NetworkFailure {
        message: tr!("服务响应接收中断 / Service response was interrupted").into(),
        definitely_unsent: false,
    };
    let mut body = Vec::new();
    loop {
        match received.recv_timeout(idle) {
            Ok(Ok(bytes)) if bytes.is_empty() => break,
            Ok(Ok(bytes)) => body.extend_from_slice(&bytes),
            Ok(Err(_)) | Err(_) => return Err(interrupted()),
        }
        if body.len() > STREAM_BODY_LIMIT {
            return Err(crate::dispatch::NetworkFailure {
                message: tr!("服务响应超过接收上限 / Service response exceeds the receiving limit")
                    .into(),
                definitely_unsent: false,
            });
        }
    }
    Ok(crate::dispatch::HttpResponse {
        status,
        body,
        retry_after,
    })
}

/// Only a safety net: a stream that keeps sending is alive. Output limits end
/// every AI request long before this.
const STREAM_BUDGET: Duration = Duration::from_secs(30 * 60);
/// The longest wait for a service's first byte.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(300);
/// The longest silence inside an answer that has begun.
const STALL_TIMEOUT: Duration = Duration::from_secs(90);
/// The most a streamed answer may hold.
const STREAM_BODY_LIMIT: usize = 32 * 1024 * 1024;

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
    /// What kind of failure [`Self::note`] is, when the service classified it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<c2m_core::outcome::FailureKind>,
    /// Where the first unfinished part is and what the service said about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_failure: Option<c2m_core::outcome::FirstFailure>,
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
    /// Some request of this step got a usable answer: the service and its
    /// settings work, so a batch it refused or answered unusably is about its
    /// own text, and smaller parts of it are worth asking.
    answered: std::sync::atomic::AtomicBool,
    /// The requests this step may still spend asking for parts of batches on
    /// their own ([`Self::isolate`]).
    isolation_budget: std::sync::atomic::AtomicUsize,
}

/// The part of a batch that stayed unfinished, and why.
struct Unfinished {
    /// "00:12:30–00:13:10": where in the video it is.
    range: String,
    error: anyhow::Error,
}

impl Unfinished {
    /// "00:12:30–00:13:10：cause", for the note's record of what failed. A part
    /// the service refused as it is says so with the service's own words, not
    /// as the generic refusal that names the model and settings.
    fn note(&self) -> String {
        let cause = match self.failure().filter(|failure| refused(failure)) {
            Some(failure) => match &failure.detail {
                Some(detail) => tr!("服务拒绝处理这部分内容：{}", detail),
                None => tr!("服务拒绝处理这部分内容").to_owned(),
            },
            None => format!("{:#}", self.error),
        };
        tr!("{}：{}", self.range, cause)
    }

    fn failure(&self) -> Option<&crate::dispatch::Failure> {
        self.error.downcast_ref::<crate::dispatch::Failure>()
    }

    /// What the step's outcome records of it.
    fn first_failure(&self) -> c2m_core::outcome::FirstFailure {
        let failure = self.failure();
        c2m_core::outcome::FirstFailure {
            range: self.range.clone(),
            detail: failure.and_then(|failure| failure.detail.clone()),
            refused: failure.is_some_and(refused),
        }
    }
}

/// A refusal of the request as it is: HTTP 400/422 that names no parameter to
/// leave out, which a service gives for content it will not process or a
/// request too large.
fn refused(failure: &crate::dispatch::Failure) -> bool {
    matches!(failure.status, Some(400 | 422))
        && failure.refused_parameter.is_none()
        && !failure.unsupported_response_format
}

impl<B, A> Batches<'_, B, A>
where
    B: Fn(&[&str], Option<&str>) -> Result<serde_json::Value> + Sync,
    A: Fn(&mut [&mut TranscriptEvent], &[(usize, String)]) + Sync,
{
    /// Sends `events` as one request and applies the answer. An answer cut off at
    /// its output limit, or one that left some paragraphs out, keeps the items it
    /// finished, and only the rest is asked again as one smaller request. One cut
    /// off with nothing finished (the model thought or repeated itself through the
    /// budget) is asked once more with the full budget, then split in two, at most
    /// [`MAX_SPLITS`] times: the same request would only stop at the same place.
    /// A batch the service refused for its content, or kept answering unusably
    /// after every retry, is asked for in parts ([`Self::isolate`]), unless it is
    /// itself such a `part`, which is sent once and left to its caller. Returns
    /// the events finished, and why the others were not.
    fn send(
        &self,
        image: Option<&str>,
        events: &mut [&mut TranscriptEvent],
        splits: u32,
        full_budget: bool,
        part: bool,
    ) -> (usize, Option<Unfinished>) {
        let (Some(first), Some(last)) = (events.first(), events.last()) else {
            return (0, None);
        };
        let (start, end) = (
            crate::timeline::fmt_ts(first.start),
            crate::timeline::fmt_ts(last.end),
        );
        let unfinished = |error| Unfinished {
            range: format!("{start}–{end}"),
            error,
        };
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
        let budget = (!full_budget).then(|| batch_max_tokens(&items));
        let answer = (self.body_for)(&items, image).and_then(|body| {
            self.chat
                .segments(&body, &description, &items, budget, part)
        });
        let answer = match answer {
            Ok(answer) => answer,
            Err(error) if !part => return self.isolate(image, events, splits, unfinished(error)),
            Err(error) => return (0, Some(unfinished(error))),
        };
        if !answer.segments.is_empty() {
            self.answered
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
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
            "AI 回答没有完成全部段落，补发未完成的部分 / The answer did not finish every paragraph; asking again for the rest"
        );
        let done = total - rest.len();
        if rest.is_empty() {
            return (done, None);
        }
        if done > 0 {
            let (more, error) = self.send(image, &mut rest, splits, false, part);
            return (done + more, error);
        }
        // Codex takes no output limit from here: its full budget is the same request.
        let budgeted = self.chat.settings.provider != LlmProvider::Codex;
        match after_empty_answer(rest.len(), splits, full_budget, budgeted) {
            EmptyAnswer::FullBudget => self.send(image, &mut rest, splits, true, part),
            EmptyAnswer::GiveUp => (0, Some(unfinished(anyhow::anyhow!(output_limit_message())))),
            EmptyAnswer::Halve => {
                let middle = rest.len() / 2;
                let (left, right) = rest.split_at_mut(middle);
                // The full budget did not do for the whole; the halves start with it.
                let (left_done, left_error) = self.send(image, left, splits + 1, budgeted, part);
                let (right_done, right_error) = self.send(image, right, splits + 1, budgeted, part);
                (left_done + right_done, left_error.or(right_error))
            }
        }
    }

    /// Asks for the two halves of a batch that ended unfinished, each once, and
    /// again for the half of a half that still failed, so that only the
    /// paragraph a refusal or an unusable answer is about stays unfinished.
    ///
    /// Only once another request of this step was answered, so the service and
    /// its settings work, and within the step's budget of such requests. A
    /// refusal is followed down to one paragraph; an answer unusable after every
    /// retry, which costs a request each time, [`MAX_SPLITS`] times. When both
    /// halves are refused, the refusal is about the request as a whole (its
    /// image, its size) and halving stops. A refused key, model or address, a
    /// rate limit, a lost answer and a stop are not about the batch's text and
    /// are never split.
    fn isolate(
        &self,
        image: Option<&str>,
        events: &mut [&mut TranscriptEvent],
        splits: u32,
        unfinished: Unfinished,
    ) -> (usize, Option<Unfinished>) {
        if events.len() < 2 || !self.isolates(&unfinished.error, splits) || !self.spend(2) {
            return (0, Some(unfinished));
        }
        tracing::info!(
            items = events.len(),
            "AI 服务没有完成这一批，分成两半再试 / The service did not finish this batch; asking for each half: {:#}",
            unfinished.error
        );
        let middle = events.len() / 2;
        let (left, right) = events.split_at_mut(middle);
        let (left_done, left_failed) = self.send(image, left, splits + 1, false, true);
        let (right_done, right_failed) = self.send(image, right, splits + 1, false, true);
        let whole_refused = left_done + right_done == 0
            && [&left_failed, &right_failed].iter().all(|failed| {
                failed
                    .as_ref()
                    .and_then(Unfinished::failure)
                    .is_some_and(refused)
            });
        if whole_refused {
            return (0, left_failed);
        }
        let mut done = left_done + right_done;
        let mut first = None;
        for (half, half_done, failed) in [
            (left, left_done, left_failed),
            (right, right_done, right_failed),
        ] {
            let Some(failed) = failed else { continue };
            let (more, still) = if half_done == 0 {
                self.isolate(image, half, splits + 1, failed)
            } else {
                (0, Some(failed))
            };
            done += more;
            first = first.or(still);
        }
        (done, first)
    }

    fn isolates(&self, error: &anyhow::Error, splits: u32) -> bool {
        if !self.answered.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let Some(failure) = error.downcast_ref::<crate::dispatch::Failure>() else {
            return false;
        };
        (refused(failure) && splits < MAX_ISOLATING_SPLITS)
            || (failure.unusable && splits < MAX_SPLITS)
    }

    /// Takes `requests` from the step's budget for asking for parts, if it has them.
    fn spend(&self, requests: usize) -> bool {
        self.isolation_budget
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |left| left.checked_sub(requests),
            )
            .is_ok()
    }
}

thread_local! {
    /// Whether the work on this thread got an answer from the service, rather
    /// than reading every answer back from its receipt.
    static SENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `work` (one batch, on one worker thread) and says whether it took the
/// time of real requests: an answer came from the service, or it ran for a
/// second or more (a request that failed was still sent and waited for). A
/// batch read back from its receipts takes no time, and is not speed.
pub(crate) fn answered_now<T>(work: impl FnOnce() -> T) -> (T, bool) {
    SENT.with(|sent| sent.set(false));
    let started = std::time::Instant::now();
    let result = work();
    let sent = SENT.with(std::cell::Cell::get) || started.elapsed() >= Duration::from_secs(1);
    (result, sent)
}

/// How many times a batch the service refused is halved: down to one paragraph
/// for a batch of [`BATCH`]. A refused request is not processed, and a half that
/// is accepted stops there, so one refused paragraph costs about two requests
/// per halving.
const MAX_ISOLATING_SPLITS: u32 = 5;

/// What follows an answer cut off with nothing finished.
#[derive(Debug, PartialEq, Eq)]
enum EmptyAnswer {
    /// The same items with the full output budget.
    FullBudget,
    /// The items in two halves.
    Halve,
    /// Nothing: the items stay unfinished.
    GiveUp,
}

/// For `items` events halved `splits` times so far, asked with the full budget
/// or not, on a service that takes an output budget (`budgeted`) or not.
fn after_empty_answer(items: usize, splits: u32, full_budget: bool, budgeted: bool) -> EmptyAnswer {
    if !full_budget && budgeted {
        EmptyAnswer::FullBudget
    } else if items > 1 && splits < MAX_SPLITS {
        EmptyAnswer::Halve
    } else {
        EmptyAnswer::GiveUp
    }
}

/// How many times a batch that finished nothing is halved: at most 4 parts, so a
/// batch no answer fits costs at most 1 + 1 + 2 + 4 = 8 requests.
const MAX_SPLITS: u32 = 2;

/// One AI step over transcript batches, on the shared worker pool. A batch counts
/// toward progress once it is answered or given up on. A failed batch keeps its text and records the first
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
    // Counts the batches answered or given up on, which the desktop times the
    // rest by, and says how many run at once.
    let pb = crate::progress::Bar::new(purpose.usage_stage(), chunks.len() as u64)
        .with_template(&format!(
            "{{spinner:.green}} {label} {{pos}}/{{len}} [{{bar:32.cyan/blue}}] {{msg}}"
        ))
        .with_unit(crate::progress::Unit::Batches);
    let workers = s.concurrency.clamp(1, MAX_CONCURRENCY);
    crate::progress::workers(purpose.usage_stage(), workers.min(chunks.len()));
    pb.set_position(0);
    let batches = Batches {
        chat: Chat::new(s, purpose, control),
        purpose,
        body_for,
        apply,
        warned: std::sync::atomic::AtomicBool::new(false),
        answered: std::sync::atomic::AtomicBool::new(false),
        isolation_budget: std::sync::atomic::AtomicUsize::new(
            (2 * chunks.len()).max(MIN_ISOLATION_BUDGET),
        ),
    };
    let total = chunks.len();
    // Why a batch stayed unfinished, as the step's record keeps it.
    let record = |unfinished: &Unfinished| {
        let cause = unfinished.note();
        warn_once(
            &batches.warned,
            &if purpose == Purpose::Proofreading {
                tr!("校对未完成，已保留原文：{}", cause)
            } else {
                tr!("翻译未完成：{}", cause)
            },
        );
        Cause {
            note: brief(&cause),
            kind: unfinished.failure().map(|failure| failure.kind),
            first: unfinished.first_failure(),
        }
    };
    let mut results = crate::pool::map_pool(
        chunks,
        workers,
        control,
        |(image_path, mut chunk): Chunk<'_>| {
            let image = image_path
                .as_deref()
                .and_then(|path| section_image_b64(path, &batches.warned));
            let ((done, unfinished), sent) =
                answered_now(|| batches.send(image.as_deref(), &mut chunk, 0, false, false));
            if sent {
                pb.advance(1);
            } else {
                pb.advance_replayed(1);
            }
            // A batch that met only a passing failure (a busy service, a lost
            // answer) gets one more try once the others are done.
            let again = s.auto_retry
                && done == 0
                && unfinished
                    .as_ref()
                    .and_then(Unfinished::failure)
                    .is_some_and(|failure| {
                        failure.retryable && !failure.unusable && !refused(failure)
                    });
            Ok(BatchResult {
                done,
                size: chunk.len(),
                cause: unfinished.as_ref().map(record),
                again: again.then_some((image, chunk)),
            })
        },
    )?;
    let second: Vec<_> = results
        .iter_mut()
        .enumerate()
        .filter_map(|(index, result)| Some((index, result.again.take()?)))
        .collect();
    if !second.is_empty() {
        tracing::info!(
            batches = second.len(),
            "部分批次稍后再试一次 / Trying the batches that met a passing failure once more"
        );
        crate::pool::interruptible_sleep(control, SECOND_PASS_PAUSE)?;
        let retried =
            crate::pool::map_pool(second, workers, control, |(index, (image, mut chunk))| {
                let (done, unfinished) = batches.send(image.as_deref(), &mut chunk, 0, false, true);
                Ok((index, done, unfinished.as_ref().map(record)))
            })?;
        for (index, done, cause) in retried {
            results[index].done = done;
            results[index].cause = cause;
        }
    }
    pb.finish();
    let succeeded = results.iter().map(|result| result.done).sum();
    // The first unfinished part in the video, whichever finished first.
    let first = results.iter().find_map(|result| result.cause.clone());
    Ok(PolishReport {
        attempted,
        succeeded,
        failed: attempted.saturating_sub(succeeded),
        kind: first.as_ref().and_then(|cause| cause.kind),
        first_failure: first.as_ref().map(|cause| cause.first.clone()),
        note: first.map(|cause| cause.note),
        batches: total,
        failed_batches: results
            .iter()
            .filter(|result| result.done < result.size)
            .count(),
    })
}

/// What one batch of a step came to.
struct BatchResult<'a> {
    done: usize,
    size: usize,
    cause: Option<Cause>,
    /// The batch, to try once more after the others.
    again: Option<(Option<String>, Vec<&'a mut TranscriptEvent>)>,
}

/// Why a batch stayed unfinished, as the step records it.
#[derive(Clone)]
struct Cause {
    note: String,
    kind: Option<c2m_core::outcome::FailureKind>,
    first: c2m_core::outcome::FirstFailure,
}

/// The least a step may spend asking for parts of batches on their own.
const MIN_ISOLATION_BUDGET: usize = 16;

/// The wait before the batches that met a passing failure are tried once more.
const SECOND_PASS_PAUSE: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(10)
};

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
/// A paragraph that already carries a translation (a saved note being completed)
/// is kept as it is and not sent: its receipt may be gone, and finished paid work
/// must not go out again. Batches keep the boundaries they always had, so a batch
/// with nothing translated is the same request as before.
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
    // A batch translated in full is kept; one translated in part (an answer cut
    // off at its output limit, or batches drawn differently by an older version)
    // sends only its untranslated paragraphs.
    let mut kept = 0;
    let mut pending = Vec::new();
    for (image, chunk) in chunks {
        let (done, rest): (Vec<_>, Vec<_>) = chunk
            .into_iter()
            .partition(|event| event.translation.is_some());
        kept += done.len();
        if !rest.is_empty() {
            pending.push((image, rest));
        }
    }
    let chunks = pending;
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

/// A cause short enough to record, long enough for its time range, the error
/// and what the service said about it.
fn brief(msg: &str) -> String {
    const MAX: usize = 480;
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

/// One `{"id":…,"text":…}` item. Only a string is a text: a `null` one is not
/// an answer for that paragraph (it is asked again), and only an explicit ""
/// removes a filler paragraph.
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

/// What the ledger knows a request by: `body`, what it asks, unless a service made
/// the request change how it is sent. Each change is another request, under its
/// own receipt; the first form keeps the identity earlier versions saved.
fn identity_for(
    body: &serde_json::Value,
    delivery: crate::provider::Delivery,
    reasoning_kept: bool,
    full_budget: bool,
) -> serde_json::Value {
    let mut variant = serde_json::Map::new();
    if reasoning_kept {
        variant.insert("reasoning".into(), "default".into());
    }
    if delivery.stream != crate::provider::Stream::WithUsage {
        variant.insert("stream".into(), delivery.stream.as_str().into());
    }
    if full_budget {
        variant.insert("budget".into(), "full".into());
    }
    if variant.is_empty() {
        return body.clone();
    }
    variant.insert("request".into(), body.clone());
    serde_json::Value::Object(variant)
}

impl<'a> Chat<'a> {
    pub(crate) fn new(
        settings: &'a LlmSettings,
        purpose: Purpose,
        control: &'a crate::dispatch::TaskControl,
    ) -> Self {
        Self {
            agent: stream_agent(FIRST_BYTE_TIMEOUT),
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
    /// A service that refuses part of how the request is sent, and names it (a
    /// 400/422 naming `response_format`, the thinking control, `stream_options` or
    /// `stream`), gets the request once more without that part; the steps add up.
    /// Any other rejection is final: an arbitrary 400 never changes the payload.
    /// `full_budget` marks the larger ask after an answer ran out of room.
    pub(crate) fn send(
        &self,
        body: &serde_json::Value,
        description: &str,
        delivery: crate::provider::Delivery,
        full_budget: bool,
        validate: &dyn Fn(&str, bool) -> Result<()>,
    ) -> Result<Answer> {
        self.send_with(
            &self.policy,
            body,
            description,
            delivery,
            full_budget,
            validate,
        )
    }

    /// [`Self::send`], retried by `policy`.
    fn send_with(
        &self,
        policy: &crate::pool::RetryPolicy,
        body: &serde_json::Value,
        description: &str,
        delivery: crate::provider::Delivery,
        full_budget: bool,
        validate: &dyn Fn(&str, bool) -> Result<()>,
    ) -> Result<Answer> {
        let s = self.settings;
        let mut body = body.clone();
        let mut delivery = delivery;
        let mut reasoning_kept = false;
        let reply = loop {
            let identity = identity_for(&body, delivery, reasoning_kept, full_budget);
            let sent = crate::provider::sent_body(s, &body, delivery);
            let error = match crate::pool::retry_with(self.control, policy, || {
                self.once(&identity, &sent, description, validate)
            }) {
                Ok(reply) => break reply,
                Err(error) => error,
            };
            let failure = error.downcast_ref::<crate::dispatch::Failure>();
            let refused = failure.and_then(|f| f.refused_parameter.as_deref());
            let stream = |to| (delivery.stream != to).then_some(to);
            if failure.is_some_and(|f| f.unsupported_response_format)
                && crate::provider::has_structured_format(s, &body)
            {
                crate::provider::strip_structured_format(s, &mut body);
                tracing::debug!("端点不支持 response_format，降级重试");
            } else if refused == Some("reasoning")
                && crate::provider::controls_reasoning(s, delivery)
            {
                delivery.no_reasoning = false;
                reasoning_kept = true;
                tracing::debug!("服务不接受关闭思考，改用默认设置重试");
            } else if let Some(plainer) = match refused {
                _ if s.provider == LlmProvider::Codex => None,
                Some("stream_options") => stream(crate::provider::Stream::Bare)
                    .filter(|_| delivery.stream == crate::provider::Stream::WithUsage),
                Some("stream") => stream(crate::provider::Stream::Off),
                _ => None,
            } {
                delivery.stream = plainer;
                tracing::debug!(stream = plainer.as_str(), "服务不接受流式参数，降级重试");
            } else {
                return Err(error);
            }
        };
        if !reply.replayed {
            SENT.with(|sent| sent.set(true));
        }
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
    /// credentials. `sent` goes out; `identity` is what the ledger knows it by.
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
        // The key and every credential header, as sent and without its scheme.
        let credential = |name: &str| {
            let name = name.to_ascii_lowercase();
            ["auth", "key", "token", "account"]
                .iter()
                .any(|part| name.contains(part))
        };
        let secrets: Vec<&str> = std::iter::once(s.api_key.as_str())
            .chain(
                headers
                    .iter()
                    .filter(|(name, _)| credential(name))
                    .flat_map(|(_, value)| {
                        [value.as_str(), value.rsplit(' ').next().unwrap_or_default()]
                    }),
            )
            .filter(|secret| !secret.trim().is_empty())
            .collect();
        self.control.request(
            &crate::dispatch::RequestSpec {
                service: "llm",
                purpose: self.purpose.into(),
                description,
                endpoint: &url,
                identity,
                former: &former,
                resend: self.resend,
                secrets: &secrets,
            },
            || {
                let mut request = self.agent.post(&url);
                for (name, value) in &headers {
                    request = request.header(name, value);
                }
                let response =
                    receive_streamed(crate::runtime::send_json(request, sent), STALL_TIMEOUT)?;
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

    /// A batch's reply as (id, text), asking for at most `max_tokens` (None: the
    /// full budget). A complete reply has one entry per input item. A reply cut
    /// off at its output limit with finished items is kept as it is, a definite
    /// answer, and yields those items, so asking again for the rest never resends
    /// what was answered; so is one that left some paragraphs out ([`reconcile`]),
    /// which yields the paragraphs that are certainly their own. One cut off with
    /// nothing finished (thinking or repetition used the budget) is a failure, so
    /// it is never read back as an answer, and comes back as an empty truncated
    /// reply for the caller to ask differently.
    fn segments(
        &self,
        body: &serde_json::Value,
        description: &str,
        items: &[&str],
        max_tokens: Option<u32>,
        single_attempt: bool,
    ) -> Result<Segments> {
        let expected = items.len();
        // A part of a batch asked on its own is sent once: the batch it came from
        // already had its retries. A request refused only for now still waits.
        let policy = crate::pool::RetryPolicy {
            max_attempts: if single_attempt {
                1
            } else {
                self.policy.max_attempts
            },
            ..self.policy
        };
        let proofreading = self.purpose == Purpose::Proofreading;
        let parsed = |content: &str| {
            parse_segments(content).context(if proofreading {
                tr!("服务返回的校对结构无效 / Invalid proofreading structure")
            } else {
                tr!("服务返回的翻译结构无效 / Invalid translation structure")
            })
        };
        let usable = |content: &str| -> Result<Reconciled> {
            reconcile(parsed(content)?, items, self.purpose).context(if proofreading {
                tr!(
                    "校对结果与原文段落不对应，已保留原文 / Proofread segments do not match the input"
                )
            } else {
                tr!("翻译结果与原文段落不对应 / Translated segments do not match the input")
            })
        };
        let delivery = crate::provider::Delivery {
            max_tokens,
            no_reasoning: true,
            ..Default::default()
        };
        let answer = self.send_with(
            &policy,
            body,
            description,
            delivery,
            max_tokens.is_none(),
            &|content, truncated| {
                if truncated {
                    if salvage_segments(content, expected).is_empty() {
                        return Err(anyhow::Error::new(crate::dispatch::Repeats(
                            output_limit_message().into(),
                        )));
                    }
                    return Ok(());
                }
                usable(content).map(drop)
            },
        );
        let answer = match answer {
            Ok(answer) => answer,
            // Now, or recorded by an earlier run: either way nothing was sent again.
            Err(error)
                if error
                    .downcast_ref::<crate::dispatch::Failure>()
                    .is_some_and(|failure| failure.repeats) =>
            {
                return Ok(Segments {
                    segments: Vec::new(),
                    truncated: true,
                });
            }
            Err(error) => return Err(error),
        };
        if answer.truncated {
            return Ok(Segments {
                segments: salvage_segments(&answer.content, expected),
                truncated: true,
            });
        }
        let reconciled = usable(&answer.content)?;
        if !reconciled.complete {
            tracing::info!(
                answered = reconciled.segments.len(),
                expected,
                "AI 回答缺少部分段落 / The answer left some paragraphs out"
            );
        }
        Ok(Segments {
            segments: reconciled.segments,
            truncated: !reconciled.complete,
        })
    }
}

/// An answer lined up with the paragraphs asked.
#[derive(Debug, PartialEq)]
struct Reconciled {
    segments: Vec<(usize, String)>,
    /// Every paragraph has its answer; otherwise the rest is to be asked again.
    complete: bool,
}

/// How an answer that was not cut off lines up with the `items` asked, or None
/// when nothing in it can be trusted.
///
/// One entry per id is complete. An answer numbered from 1 instead of 0, every id
/// once, is the same answer. One that left some ids out (a model that drops a
/// filler paragraph instead of returning it empty, or merges two paragraphs into
/// one) may also have numbered the rest anew, so that an id carries the next
/// paragraph's text: an id is no evidence there. A proofreading keeps only the
/// paragraphs whose text is clearly their own — more like their own original
/// than like either neighbor's, and, beside a missing one, no longer than their
/// own and half of it, where a merge would have put its text — and the rest is
/// asked again, an empty one too: it removes a filler paragraph only in a
/// complete answer. A translation cannot be told apart from its original that way,
/// and keeps nothing: the batch is asked again as it was. An id given twice
/// counts as left out; an id past the batch means the numbering is off.
fn reconcile(answer: Vec<(usize, String)>, items: &[&str], purpose: Purpose) -> Option<Reconciled> {
    let expected = items.len();
    if segment_ids_match(&answer, expected) {
        return Some(Reconciled {
            segments: answer,
            complete: true,
        });
    }
    let from_one: Vec<(usize, String)> = answer
        .iter()
        .filter_map(|(id, text)| Some((id.checked_sub(1)?, text.clone())))
        .collect();
    if from_one.len() == answer.len() && segment_ids_match(&from_one, expected) {
        return Some(Reconciled {
            segments: from_one,
            complete: true,
        });
    }
    if purpose != Purpose::Proofreading || answer.iter().any(|(id, _)| *id >= expected) {
        return None;
    }
    let mut count = vec![0usize; expected];
    for (id, _) in &answer {
        count[*id] += 1;
    }
    let missing = |id: usize| count.get(id).is_some_and(|n| *n != 1);
    let length = |id: usize| items.get(id).map_or(0, |text| text.chars().count());
    let own = |id: usize, text: &str| {
        let neighbors = [id.checked_sub(1), Some(id + 1)]
            .into_iter()
            .flatten()
            .filter(|&neighbor| neighbor < expected);
        let gap: usize = neighbors
            .clone()
            .filter(|&neighbor| missing(neighbor))
            .map(length)
            .sum();
        // An empty text in an answer whose numbering is in doubt may belong to
        // any paragraph: nothing is removed on its word. The paragraph is asked
        // again, alone or with fewer, where its "" can be trusted.
        if text.trim().is_empty() {
            return false;
        }
        let likeness = resemblance(text, items[id]);
        // Room for the punctuation a proofreading adds, not for half the
        // missing paragraph.
        (gap == 0 || text.chars().count() <= length(id) + (gap / 2).max(3))
            && neighbors
                .into_iter()
                .all(|neighbor| likeness > resemblance(text, items[neighbor]))
    };
    let segments: Vec<(usize, String)> = answer
        .into_iter()
        .filter(|(id, text)| !missing(*id) && own(*id, text))
        .collect();
    (!segments.is_empty()).then_some(Reconciled {
        segments,
        complete: false,
    })
}

/// How alike two texts are, from 0 to 1: the share of their pairs of adjacent
/// characters they have in common (single characters for a text of one). A
/// proofread paragraph stays far more like its own original than like another.
fn resemblance(a: &str, b: &str) -> f64 {
    fn pieces(text: &str) -> std::collections::HashMap<String, usize> {
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        let mut out = std::collections::HashMap::new();
        if chars.len() < 2 {
            for c in &chars {
                *out.entry(c.to_string()).or_insert(0) += 1;
            }
        } else {
            for pair in chars.windows(2) {
                *out.entry(pair.iter().collect::<String>()).or_insert(0) += 1;
            }
        }
        out
    }
    let (a, b) = (pieces(a), pieces(b));
    let size = |p: &std::collections::HashMap<String, usize>| p.values().sum::<usize>();
    let total = size(&a) + size(&b);
    if total == 0 {
        return 0.;
    }
    let shared: usize = a
        .iter()
        .map(|(piece, n)| (*n).min(b.get(piece).copied().unwrap_or(0)))
        .sum();
    2. * shared as f64 / total as f64
}
fn output_limit_message() -> &'static str {
    tr!("这一段的结果超出了输出上限 / The answer for this paragraph exceeds the output limit")
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
        let (agent, idle) = (
            stream_agent(Duration::from_secs(5)),
            Duration::from_millis(500),
        );
        let started = std::time::Instant::now();
        let failure = receive_streamed(agent.post(&url).send_json(serde_json::json!({})), idle)
            .err()
            .expect("a stalled body is not a response");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!failure.definitely_unsent, "the request was already sent");
    }

    #[test]
    fn a_slow_model_is_not_a_stalled_response() {
        use std::io::Write as _;
        // Thinks for longer than the stall limit before the first byte, as a
        // service answering without a stream does.
        let url = stalling_server(|stream| {
            hold(1.5);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
        });
        let (agent, idle) = (
            stream_agent(Duration::from_secs(5)),
            Duration::from_millis(500),
        );
        let response = receive_streamed(agent.post(&url).send_json(serde_json::json!({})), idle)
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
        let (agent, idle) = (stream_agent(Duration::from_secs(5)), Duration::from_secs(2));
        let started = std::time::Instant::now();
        let response = receive_streamed(agent.post(&url).send_json(serde_json::json!({})), idle)
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
        let (agent, idle) = (
            stream_agent(Duration::from_secs(5)),
            Duration::from_millis(500),
        );
        let started = std::time::Instant::now();
        let failure = receive_streamed(agent.post(&url).send_json(serde_json::json!({})), idle)
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
    ) -> (String, Bodies) {
        let count = std::sync::atomic::AtomicUsize::new(0);
        scripted_service_with_body(move |_, items| {
            answer(
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                items,
            )
        })
    }

    type Bodies = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

    /// [`scripted_service`] whose `answer` sees the request body instead of its number.
    fn scripted_service_with_body(
        answer: impl Fn(&serde_json::Value, &[String]) -> String + Send + 'static,
    ) -> (String, Bodies) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let bodies = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
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
                let user = &body["messages"][1]["content"];
                let items: Vec<String> = user
                    .as_str()
                    .or_else(|| user[0]["text"].as_str())
                    .and_then(|text| serde_json::from_str::<Vec<serde_json::Value>>(text).ok())
                    .unwrap_or_default()
                    .iter()
                    .map(|item| item["text"].as_str().unwrap_or_default().to_owned())
                    .collect();
                let reply = answer(&body, &items);
                bodies.lock().unwrap().push(body);
                let _ = stream.write_all(reply.as_bytes());
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
        // The failed batch had one more try once the others were done.
        assert_eq!(bodies.lock().unwrap().len(), 4);

        // The service recovers; the same step again sends the failed batch alone.
        failing.store(false, std::sync::atomic::Ordering::SeqCst);
        let mut sections = one_section(&refs);
        let report =
            polish_sections_report(&mut sections, Path::new(""), &settings, &control).unwrap();
        assert_eq!((report.succeeded, report.failed_batches), (60, 0));
        let bodies = bodies.lock().unwrap();
        assert_eq!(
            bodies.len(),
            5,
            "finished batches come back from their receipts"
        );
        assert!(bodies[4].to_string().contains("坏"));
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
        // [一二三] → the full budget → [一] + [二三] → [二] + [三], the halves
        // with the full budget the whole could not do without.
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
        assert_eq!(sizes, [3, 3, 1, 2, 1, 1]);

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

    #[test]
    fn an_answer_with_room_only_for_thinking_gets_the_full_budget_and_is_not_kept() {
        // Spends any budget short of the full one on thinking.
        let full = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = full.clone();
        let (url, _) = scripted_service_with_body(move |body, items| {
            seen.lock().unwrap().push(body["max_tokens"].clone());
            if body["max_tokens"] == CHAT_MAX_TOKENS {
                streamed(&proofread(items), "stop")
            } else {
                streamed("", "length")
            }
        });
        let work = tempfile::tempdir().unwrap();
        let control = crate::dispatch::TaskControl::open(work.path(), Default::default()).unwrap();
        let mut sections = one_section(&["一", "二", "三"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &control,
        )
        .unwrap();
        assert_eq!(report.succeeded, 3);
        assert_eq!(
            *full.lock().unwrap(),
            [
                serde_json::json!(MIN_BATCH_TOKENS),
                serde_json::json!(CHAT_MAX_TOKENS)
            ]
        );
        // The empty answer is a failure on record, never an answer to read back.
        let states: Vec<_> = crate::dispatch::receipts(work.path())
            .unwrap()
            .into_iter()
            .map(|receipt| receipt.state)
            .collect();
        assert!(
            states.contains(&crate::dispatch::State::Failed),
            "{states:?}"
        );
        assert!(
            states.contains(&crate::dispatch::State::Completed),
            "{states:?}"
        );
    }

    #[test]
    fn only_the_persons_retry_sends_an_answer_that_ran_out_again() {
        for authorize in [true, false] {
            let healthy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let answers = healthy.clone();
            let (url, bodies) = scripted_service(move |_, items| {
                if answers.load(std::sync::atomic::Ordering::SeqCst) {
                    streamed(&proofread(items), "stop")
                } else {
                    streamed("", "length")
                }
            });
            let settings = LlmSettings {
                auto_retry: false,
                ..polish_settings(url, 1)
            };
            let work = tempfile::tempdir().unwrap();
            let run = || {
                let control =
                    crate::dispatch::TaskControl::open(work.path(), Default::default()).unwrap();
                let mut sections = one_section(&["一", "二", "三", "四"]);
                polish_sections_report(&mut sections, Path::new(""), &settings, &control)
                    .unwrap()
                    .succeeded
            };
            assert_eq!(run(), 0);
            // The batch, its full budget, then two rounds of halves.
            assert_eq!(bodies.lock().unwrap().len(), 8);
            if authorize {
                // As a retry from the task page, reader or library imports them.
                for entry in std::fs::read_dir(work.path().join("requests")).unwrap() {
                    let path = entry.unwrap().path();
                    let mut receipt: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    if receipt["state"] == "failed" {
                        receipt["retry_authorized"] = receipt["request_id"].clone();
                    }
                    std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
                }
            }
            healthy.store(true, std::sync::atomic::Ordering::SeqCst);
            let succeeded = run();
            let sent = bodies.lock().unwrap().len() - 8;
            if authorize {
                assert_eq!((succeeded, sent), (4, 1), "the person's retry is sent");
            } else {
                assert_eq!((succeeded, sent), (0, 0), "nothing is sent unasked");
            }
        }
    }

    #[test]
    fn a_later_run_goes_straight_to_the_full_budget_answer() {
        for auto_retry in [true, false] {
            let (url, bodies) = scripted_service_with_body(|body, items| {
                if body["max_tokens"] == CHAT_MAX_TOKENS {
                    streamed(&proofread(items), "stop")
                } else {
                    streamed("", "length")
                }
            });
            let settings = LlmSettings {
                auto_retry,
                ..polish_settings(url, 3)
            };
            let work = tempfile::tempdir().unwrap();
            let run = || {
                let control =
                    crate::dispatch::TaskControl::open(work.path(), Default::default()).unwrap();
                let mut sections = one_section(&["一", "二", "三"]);
                let report =
                    polish_sections_report(&mut sections, Path::new(""), &settings, &control)
                        .unwrap();
                (report.succeeded, texts(&sections))
            };
            assert_eq!(run().0, 3, "auto_retry={auto_retry}");
            assert_eq!(bodies.lock().unwrap().len(), 2, "auto_retry={auto_retry}");
            // Resumed or retried: both answers are on record, nothing is sent.
            let (succeeded, texts) = run();
            assert_eq!(succeeded, 3, "auto_retry={auto_retry}");
            assert_eq!(texts, ["一!", "二!", "三!"]);
            assert_eq!(bodies.lock().unwrap().len(), 2, "auto_retry={auto_retry}");
        }
    }

    #[test]
    fn a_batch_that_finished_nothing_asks_for_more_room_then_halves() {
        use EmptyAnswer::*;
        // A budget the service takes: once with the full budget, then halves.
        assert_eq!(after_empty_answer(20, 0, false, true), FullBudget);
        assert_eq!(after_empty_answer(20, 0, true, true), Halve);
        assert_eq!(after_empty_answer(20, MAX_SPLITS, true, true), GiveUp);
        assert_eq!(after_empty_answer(1, 0, true, true), GiveUp);
        // Codex takes no output limit: a "full budget" ask would be the same
        // request again, so it halves at once.
        assert_eq!(after_empty_answer(20, 0, false, false), Halve);
        assert_eq!(after_empty_answer(1, 0, false, false), GiveUp);
    }

    #[test]
    fn a_batch_no_answer_fits_stops_after_a_bounded_number_of_requests() {
        let (url, bodies) = scripted_service(|_, _| streamed("", "length"));
        let texts_in: Vec<String> = (0..20).map(|i| format!("段{i}")).collect();
        let refs: Vec<&str> = texts_in.iter().map(String::as_str).collect();
        let mut sections = one_section(&refs);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.failed, 20);
        // The batch, its full-budget ask, then two rounds of halves.
        assert_eq!(bodies.lock().unwrap().len(), 1 + 1 + 2 + 4);
    }

    #[test]
    fn a_service_that_refuses_streaming_options_is_asked_without_them() {
        let (url, bodies) = scripted_service_with_body(|body, items| {
            if body.get("stream_options").is_some() {
                refusal(
                    400,
                    r#"{"error":{"message":"Unrecognized request argument supplied: stream_options"}}"#,
                )
            } else if body.get("stream").is_some() {
                refusal(422, r#"{"message":"stream is not supported"}"#)
            } else {
                // A plain answer, as from a service that does not stream.
                let reply = serde_json::json!({"choices": [{"message": {"content": proofread(items)}, "finish_reason": "stop"}]}).to_string();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
            }
        });
        let mut sections = one_section(&["一"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.succeeded, 1, "{:?}", report.note);
        assert_eq!(texts(&sections), ["一!"]);
        assert_eq!(bodies.lock().unwrap().len(), 3);

        // A rejection that names nothing changes nothing: it is final.
        let (url, bodies) = scripted_service_with_body(|_, _| status(400));
        let mut sections = one_section(&["一"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 3),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(bodies.lock().unwrap().len(), 1);
    }

    /// A refusal with `body`, the service's explanation.
    fn refusal(code: u16, body: &str) -> String {
        format!(
            "HTTP/1.1 {code} Refused\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn refused_thinking_control_and_response_format_fall_back_in_turn() {
        // Ollama refuses both `reasoning_effort` and `response_format`.
        let (url, bodies) = scripted_service_with_body(|body, items| {
            if body.get("response_format").is_some() {
                let error =
                    r#"{"error":{"param":"response_format","code":"unsupported_parameter"}}"#;
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{error}",
                    error.len()
                )
            } else if body.get("reasoning_effort").is_some() {
                refusal(
                    400,
                    r#"{"error":{"message":"invalid reasoning value: \"none\""}}"#,
                )
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        let settings = LlmSettings {
            provider: LlmProvider::Ollama,
            api_key: String::new(),
            ..polish_settings(url, 1)
        };
        let mut sections = one_section(&["一"]);
        let report =
            polish_sections_report(&mut sections, Path::new(""), &settings, &Default::default())
                .unwrap();
        assert_eq!(report.succeeded, 1, "{:?}", report.note);
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 3);
        assert!(bodies[1].get("response_format").is_none());
        assert_eq!(bodies[1]["reasoning_effort"], "none");
        assert!(bodies[2].get("reasoning_effort").is_none());
        assert_eq!(bodies[2]["stream"], true, "streaming was never the problem");
    }

    #[test]
    fn a_partly_translated_batch_sends_only_its_untranslated_paragraphs() {
        let (url, bodies) = scripted_service(|_, items| streamed(&proofread(items), "stop"));
        let mut sections = one_section(&["一", "二", "三"]);
        sections[0].speech[1].translation = Some("two".into());
        let settings = polish_settings(url, 1);
        let report =
            translate_sections_report(&mut sections, &settings, "en", &Default::default()).unwrap();
        assert_eq!(
            (report.attempted, report.succeeded, report.failed),
            (3, 3, 0)
        );
        let speech = &sections[0].speech;
        let translations: Vec<_> = speech.iter().map(|e| e.translation.as_deref()).collect();
        assert_eq!(translations, [Some("一!"), Some("two"), Some("三!")]);
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        let sent = bodies[0]["messages"][1]["content"].as_str().unwrap();
        assert!(sent.contains("一") && sent.contains("三") && !sent.contains("二"));
    }

    #[test]
    fn a_free_service_resends_what_an_earlier_run_left_unresolved() {
        for (provider, resent) in [
            (LlmProvider::Ollama, true),
            (LlmProvider::OpenAiCompatible, false),
        ] {
            let (url, bodies) = scripted_service(|_, items| streamed(&proofread(items), "stop"));
            // Automatic retries on, as by default.
            let settings = LlmSettings {
                provider,
                ..polish_settings(url, 1)
            };
            assert!(settings.auto_retry);
            let work = tempfile::tempdir().unwrap();
            let control =
                crate::dispatch::TaskControl::open(work.path(), Default::default()).unwrap();
            let mut sections = one_section(&["一"]);
            polish_sections_report(&mut sections, Path::new(""), &settings, &control).unwrap();
            // The app was killed while that request was in flight.
            for entry in std::fs::read_dir(work.path().join("requests")).unwrap() {
                let path = entry.unwrap().path();
                let mut receipt: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                receipt["state"] = "sending".into();
                receipt["response"] = serde_json::Value::Null;
                std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
            }
            let control =
                crate::dispatch::TaskControl::open(work.path(), Default::default()).unwrap();
            let mut sections = one_section(&["一"]);
            let report =
                polish_sections_report(&mut sections, Path::new(""), &settings, &control).unwrap();
            assert_eq!(report.succeeded == 1, resent, "{provider:?}");
            assert_eq!(bodies.lock().unwrap().len(), 1 + usize::from(resent));
        }
    }

    /// Paragraph `i` of a minute-long lecture, `i` seconds in.
    fn timed_section(texts: &[String]) -> Vec<Section> {
        vec![Section {
            t: 0.0,
            end: texts.len() as f64,
            image: String::new(),
            speech: texts
                .iter()
                .enumerate()
                .map(|(i, text)| TranscriptEvent {
                    start: i as f64,
                    end: i as f64 + 1.0,
                    ..speech(text)
                })
                .collect(),
        }]
    }

    /// `items` proofread as `<text>!`, leaving out those `skip` names.
    fn proofread_except(items: &[String], skip: impl Fn(&str) -> bool) -> String {
        let segments: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(_, text)| !skip(text))
            .map(|(id, text)| serde_json::json!({"id": id, "text": format!("{text}!")}))
            .collect();
        serde_json::json!({ "segments": segments }).to_string()
    }

    fn sent_items(body: &serde_json::Value) -> String {
        body["messages"][1]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn an_answer_that_leaves_paragraphs_out_keeps_the_rest_and_asks_for_those() {
        let items = [
            "今天我们讲编译原理的基本概念",
            "啊",
            "词法分析把字符流变成记号流",
            "语法分析根据文法构造语法树",
            "语义分析检查类型和作用域规则",
        ];
        let answer = |pairs: &[(usize, &str)]| -> Vec<(usize, String)> {
            pairs.iter().map(|(id, t)| (*id, (*t).to_owned())).collect()
        };
        let ids = |reconciled: Option<Reconciled>| -> Vec<usize> {
            reconciled
                .map(|r| r.segments.iter().map(|(id, _)| *id).collect())
                .unwrap_or_default()
        };
        // A filler paragraph left out: the rest is clearly its own.
        let dropped = reconcile(
            answer(&[
                (0, "今天我们讲编译原理的基本概念。"),
                (2, "词法分析把字符流变成记号流。"),
                (3, "语法分析根据文法构造语法树。"),
                (4, "语义分析检查类型和作用域规则。"),
            ]),
            &items,
            Purpose::Proofreading,
        )
        .unwrap();
        assert!(!dropped.complete);
        assert_eq!(dropped.segments.len(), 4);
        // Two paragraphs merged into one: the merged one is asked again with the
        // one it swallowed.
        let merged = ids(reconcile(
            answer(&[
                (0, "今天我们讲编译原理的基本概念。"),
                (1, ""),
                (
                    2,
                    "词法分析把字符流变成记号流，语法分析根据文法构造语法树。",
                ),
                (4, "语义分析检查类型和作用域规则。"),
            ]),
            &items,
            Purpose::Proofreading,
        ));
        assert_eq!(merged, [0, 4]);
        // Paragraph 2 left out and the rest numbered anew: each id carries the
        // next paragraph's text. Only what is clearly its own is kept.
        let renumbered = ids(reconcile(
            answer(&[
                (0, "今天我们讲编译原理的基本概念。"),
                (1, ""),
                (2, "语法分析根据文法构造语法树。"),
                (3, "语义分析检查类型和作用域规则。"),
            ]),
            &items,
            Purpose::Proofreading,
        ));
        assert_eq!(renumbered, [0]);
        // A merge that kept its length within the limit is still not its own:
        // it is as much like the paragraph it swallowed.
        let swallowed = ids(reconcile(
            answer(&[
                (0, "今天我们讲编译原理的基本概念。"),
                (1, ""),
                (2, "词法分析把字符流变成记号流"),
                (3, "语法分析构造语法树，语义分析检查类型和作用域"),
            ]),
            &items,
            Purpose::Proofreading,
        ));
        assert_eq!(swallowed, [0, 2]);
        // A translation cannot be matched to its original: nothing is kept, and
        // the batch is asked again as it was.
        let translated = reconcile(
            answer(&[
                (0, "Today: compilers."),
                (1, "Lexing makes tokens."),
                (2, "Parsing builds a tree."),
                (3, "Semantics checks types."),
            ]),
            &items,
            Purpose::Translation,
        );
        assert!(translated.is_none());
        // Numbered from one: the same complete answer.
        let shifted = reconcile(
            answer(&[(1, "a"), (2, "b"), (3, "c"), (4, "d"), (5, "e")]),
            &items,
            Purpose::Proofreading,
        )
        .unwrap();
        assert!(shifted.complete);
        assert_eq!(shifted.segments[0], (0, "a".to_owned()));
        // An id past the batch: nothing can be matched.
        assert!(reconcile(answer(&[(0, "a"), (7, "b")]), &items, Purpose::Proofreading).is_none());
        // An id given twice counts as left out.
        let twice = ids(reconcile(
            answer(&[
                (0, "今天我们讲编译原理的基本概念。"),
                (0, "今天我们讲编译原理。"),
                (1, ""),
                (2, "词法分析把字符流变成记号流。"),
                (3, "语法分析根据文法构造语法树。"),
                (4, "语义分析检查类型和作用域规则。"),
            ]),
            &items,
            Purpose::Proofreading,
        ));
        assert_eq!(twice, [2, 3, 4]);
        // A `null` text is no answer for its paragraph: it is asked again, and
        // only an explicit "" removes a filler paragraph.
        assert_eq!(
            parse_segments(r#"{"segments":[{"id":0,"text":"a"},{"id":1,"text":null}]}"#),
            Some(vec![(0, "a".to_owned())])
        );
    }

    #[test]
    fn a_paragraph_the_model_keeps_leaving_out_is_asked_alone_and_the_rest_finishes() {
        let (url, bodies) = scripted_service(|_, items| {
            if items.len() == 1 && items[0] == "嗯嗯嗯嗯" {
                // Asked alone, the paragraph comes back empty, as the prompt asks.
                streamed(r#"{"segments":[{"id":0,"text":""}]}"#, "stop")
            } else {
                streamed(&proofread_except(items, |text| text == "嗯嗯嗯嗯"), "stop")
            }
        });
        let mut texts_in: Vec<String> = (0..20).map(|i| format!("第{i}段内容")).collect();
        texts_in[7] = "嗯嗯嗯嗯".into();
        let refs: Vec<&str> = texts_in.iter().map(String::as_str).collect();
        let mut sections = one_section(&refs);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        assert_eq!((report.succeeded, report.failed), (20, 0));
        let out = texts(&sections);
        assert_eq!(out.len(), 19, "the filler paragraph is removed");
        assert!(out.iter().all(|text| text.ends_with('!')), "{out:?}");
        let bodies = bodies.lock().unwrap();
        // The batch, then the paragraph it left out alone: its neighbors kept
        // their own length, so they were not merged with it.
        assert_eq!(bodies.len(), 2, "{bodies:?}");
        let second = sent_items(&bodies[1]);
        assert!(
            second.contains("嗯") && !second.contains("第6段"),
            "{second}"
        );
    }

    #[test]
    fn a_paragraph_the_service_refuses_stays_as_it_was_and_only_it() {
        let (url, bodies) = scripted_service(|_, items| {
            if items.iter().any(|text| text.contains("敏感")) {
                let body =
                    r#"{"error":{"message":"Content Exists Risk","type":"invalid_request_error"}}"#;
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        let mut texts_in: Vec<String> = (0..40).map(|i| format!("段{i}")).collect();
        texts_in[33] = "敏感".into();
        let mut sections = timed_section(&texts_in);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 4),
            &Default::default(),
        )
        .unwrap();
        assert_eq!((report.succeeded, report.failed), (39, 1));
        assert_eq!(report.failed_batches, 1);
        let out = texts(&sections);
        assert_eq!(out[33], "敏感", "the refused paragraph keeps its text");
        assert_eq!(out.iter().filter(|text| text.ends_with('!')).count(), 39);
        // Where it is, and what the service said.
        let note = report.note.unwrap();
        assert!(note.contains("00:33–00:34"), "{note}");
        assert!(note.contains("Content Exists Risk"), "{note}");
        // One batch, then halving down to the paragraph: 20 → 10 → 5 → 3 → 2 → 1,
        // each refused part costing one request and each accepted half one.
        let sent = bodies.lock().unwrap().len();
        assert!(sent <= 2 + 2 * 5, "{sent} requests");
    }

    #[test]
    fn refusals_are_not_split_while_nothing_has_been_answered() {
        let (url, bodies) = scripted_service(|_, _| status(400));
        let texts_in: Vec<String> = (0..20).map(|i| format!("段{i}")).collect();
        let mut sections = timed_section(&texts_in);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 4),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.failed, 20);
        assert_eq!(
            bodies.lock().unwrap().len(),
            1,
            "a refused setting is not split"
        );
    }

    #[test]
    fn an_unusable_answer_for_one_paragraph_leaves_only_a_few_unfinished() {
        // Any batch holding the paragraph gets an answer without structure.
        let (url, bodies) = scripted_service(|_, items| {
            if items.iter().any(|text| text == "坏") {
                streamed("抱歉，我无法处理。", "stop")
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        let mut texts_in: Vec<String> = (0..40).map(|i| format!("段{i}")).collect();
        texts_in[25] = "坏".into();
        let mut sections = timed_section(&texts_in);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        // Halved twice: the quarter holding it stays unfinished, not the batch.
        assert_eq!((report.succeeded, report.failed), (35, 5));
        assert!(texts(&sections)[25] == "坏");
        assert_eq!(bodies.lock().unwrap().len(), 2 + 2 + 2);
    }

    #[test]
    fn a_rate_limit_is_waited_out_past_the_configured_tries() {
        let (url, bodies) = scripted_service(|index, items| {
            if index < 3 {
                status(429)
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        let mut sections = one_section(&["一"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 2),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.succeeded, 1);
        assert_eq!(bodies.lock().unwrap().len(), 4);
    }

    /// With every retry allowed, a batch whose answers stay unusable costs its
    /// retries once; its parts are asked once each, not with retries of their own.
    #[test]
    fn asking_for_parts_of_an_unusable_batch_costs_one_request_each() {
        let (url, bodies) = scripted_service(|_, items| {
            if items.iter().any(|text| text == "坏") {
                streamed("抱歉，我无法处理。", "stop")
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        let mut texts_in: Vec<String> = (0..40).map(|i| format!("段{i}")).collect();
        texts_in[25] = "坏".into();
        let mut sections = timed_section(&texts_in);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, MAX_RETRY_ATTEMPTS),
            &Default::default(),
        )
        .unwrap();
        assert_eq!((report.succeeded, report.failed), (35, 5));
        // The first batch; the second with all its retries; two halves, then
        // two quarters, once each.
        let sent = bodies.lock().unwrap().len();
        assert_eq!(
            sent,
            1 + MAX_RETRY_ATTEMPTS as usize + 2 + 2,
            "{sent} requests"
        );
    }

    /// A refusal of both halves is about the request as a whole (its image, its
    /// size), not a paragraph: halving stops there.
    #[test]
    fn a_batch_refused_whole_is_not_split_further() {
        let (url, bodies) = scripted_service(|_, items| {
            if items.iter().any(|text| text.starts_with('图')) {
                status(400)
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        let texts_in: Vec<String> = (0..40)
            .map(|i| {
                if i < 20 {
                    format!("段{i}")
                } else {
                    format!("图{i}")
                }
            })
            .collect();
        let mut sections = timed_section(&texts_in);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 4),
            &Default::default(),
        )
        .unwrap();
        assert_eq!((report.succeeded, report.failed), (20, 20));
        // The good batch, the refused one and its two halves.
        assert_eq!(bodies.lock().unwrap().len(), 4);
    }

    /// An answer that left a paragraph out and numbered the rest anew is not
    /// read by its ids: the paragraphs after the gap are asked again, and no
    /// paragraph ends up with another's text.
    #[test]
    fn a_renumbered_answer_does_not_move_text_between_paragraphs() {
        let paragraphs = [
            "今天我们讲编译原理的基本概念",
            "这一段被模型漏掉了",
            "词法分析把字符流变成记号流",
            "语法分析根据文法构造语法树",
            "语义分析检查类型和作用域规则",
        ];
        let (url, bodies) = scripted_service(|_, items| {
            // Leaves out the second paragraph and numbers the rest from where it was.
            let kept: Vec<_> = items
                .iter()
                .filter(|text| !text.contains("漏掉"))
                .enumerate()
                .map(|(id, text)| serde_json::json!({"id": id, "text": format!("{text}。")}))
                .collect();
            if kept.len() == items.len() {
                streamed(&proofread(items), "stop")
            } else if kept.is_empty() {
                streamed(
                    r#"{"segments":[{"id":0,"text":"这一段被模型漏掉了。"}]}"#,
                    "stop",
                )
            } else {
                streamed(&serde_json::json!({ "segments": kept }).to_string(), "stop")
            }
        });
        let mut sections = one_section(&paragraphs);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.failed, 0);
        let out = texts(&sections);
        for (text, original) in out.iter().zip(paragraphs) {
            assert!(text.starts_with(original), "{out:?}");
        }
        assert!(bodies.lock().unwrap().len() <= 6);
    }

    /// A translation that left a paragraph out is asked again whole: nothing in
    /// it can be matched to its original.
    #[test]
    fn a_translation_that_left_a_paragraph_out_is_asked_again() {
        let (url, bodies) = scripted_service(|index, items| {
            if index == 0 {
                let kept: Vec<_> = items
                    .iter()
                    .skip(1)
                    .enumerate()
                    .map(|(id, text)| serde_json::json!({"id": id, "text": format!("EN {text}")}))
                    .collect();
                streamed(&serde_json::json!({ "segments": kept }).to_string(), "stop")
            } else {
                let all: Vec<_> = items
                    .iter()
                    .enumerate()
                    .map(|(id, text)| serde_json::json!({"id": id, "text": format!("EN {text}")}))
                    .collect();
                streamed(&serde_json::json!({ "segments": all }).to_string(), "stop")
            }
        });
        let mut sections = one_section(&["一", "二", "三"]);
        let report = translate_sections_report(
            &mut sections,
            &polish_settings(url, 2),
            "en",
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.succeeded, 3);
        let translations: Vec<_> = sections[0]
            .speech
            .iter()
            .map(|event| event.translation.clone().unwrap_or_default())
            .collect();
        assert_eq!(translations, ["EN 一", "EN 二", "EN 三"]);
        assert_eq!(bodies.lock().unwrap().len(), 2);
    }

    /// A batch that met only passing failures (a busy service) gets one more try
    /// once the other batches are done.
    #[test]
    fn a_busy_batch_is_tried_once_more_at_the_end() {
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = busy.clone();
        let (url, bodies) = scripted_service(move |_, items| {
            if items.iter().any(|text| text == "忙")
                && count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2
            {
                status(502)
            } else {
                streamed(&proofread(items), "stop")
            }
        });
        let mut texts_in: Vec<String> = (0..40).map(|i| format!("段{i}")).collect();
        texts_in[30] = "忙".into();
        let mut sections = timed_section(&texts_in);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 2),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.failed, 0);
        // Two batches, the second's retry, then its try at the end.
        assert_eq!(bodies.lock().unwrap().len(), 4);
    }

    /// An answer that stayed unusable after its retries would only come back the
    /// same: it gets no try at the end.
    #[test]
    fn an_unusable_batch_is_not_tried_again_at_the_end() {
        let (url, bodies) = scripted_service(|_, _| streamed("抱歉，我无法处理。", "stop"));
        let mut sections = one_section(&["一", "二"]);
        let report = polish_sections_report(
            &mut sections,
            Path::new(""),
            &polish_settings(url, 1),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(report.failed, 2);
        assert_eq!(bodies.lock().unwrap().len(), 1);
    }

    /// A batch read back from its receipt is not timed as a request.
    #[test]
    fn work_read_back_takes_no_request_time() {
        let ((), read_back) = answered_now(|| ());
        assert!(!read_back);
        let ((), sent) = answered_now(|| SENT.with(|sent| sent.set(true)));
        assert!(sent);
    }
}
