//! Durable external-request receipts: finished requests replay at zero cost;
//! unfinished ones from an earlier run never deadlock a retry.

use crate::outcome::FailureKind;
pub use crate::outcome::{Failure, HttpResponse, NetworkFailure, Repeats};
use crate::{
    digest::{digest, json_digest},
    fsio::atomic_write,
    progress::{BlockReason, Event},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Durable receipts describe a service snapshot without a stored configuration.
pub const SNAPSHOT_PREFIX: &str = "snapshot:";

fn snapshot_id(digest: &str) -> String {
    format!("{SNAPSHOT_PREFIX}{digest}")
}

/// 任务运行意图：桌面端写入 control.json，引擎在每个安全边界读取。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    #[default]
    Run,
    Pause,
    Cancel,
    Quit,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Control {
    pub intent: Intent,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Sending,
    Completed,
    /// DNS or connection establishment failed before the request body could be sent.
    NotSent,
    Rejected,
    Failed,
    /// Legacy state retained so receipts written by earlier versions deserialize cleanly.
    Uncertain,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Receipt {
    pub schema: u32,
    pub stable_id: String,
    pub request_id: String,
    pub purpose: String,
    #[serde(default)]
    pub description: String,
    pub service_version: String,
    pub attempt: u32,
    pub state: State,
    pub http_status: Option<u16>,
    pub response: Option<Value>,
    pub message: Option<String>,
    /// A reviewed compatibility rejection; never stores the provider's error body.
    #[serde(default)]
    pub unsupported_response_format: bool,
    /// An optional parameter the service named when it refused the request, as
    /// [`refused_parameter`] reads it; never the provider's error body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused_parameter: Option<String>,
    /// The answer failed in a way the same request would only repeat
    /// ([`Repeats`]): it is not sent again within the same run, so the caller
    /// asks differently without paying again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repeats: bool,
    /// The service answered but the answer did not fit the request
    /// ([`Failure::unusable`]): asking for less at once may help.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unusable: bool,
    /// What the service itself said about a refused or failed request, as it
    /// wrote it, with anything that looks like a credential removed. Kept so the
    /// person can see (and send) the actual cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What identifies one service request in the ledger, and how it may be resent.
pub struct RequestSpec<'a> {
    /// The configured service ("llm", "asr"), resolved to its version.
    pub service: &'a str,
    /// Ledger purpose, shared by retries of the same work.
    pub purpose: &'a str,
    /// Shown to the person when the request needs a decision.
    pub description: &'a str,
    pub endpoint: &'a str,
    /// The request content that identifies a repeat of the same request.
    pub identity: &'a Value,
    /// Identities earlier versions gave this same request. A receipt saved under one
    /// of them is this request's, and the request continues under that receipt's id,
    /// so an upgrade never pays for a request an older version already sent.
    pub former: &'a [Value],
    pub resend: Resend,
    /// The credentials the request carries, removed from anything the service
    /// said that is kept ([`Receipt::detail`]).
    pub secrets: &'a [&'a str],
}

/// A service response, and whether it was received now or replayed from a
/// completed receipt (callers that report live usage skip replays).
#[derive(Debug)]
pub struct Reply {
    pub value: Value,
    pub replayed: bool,
}

fn rejection_message(status: u16) -> String {
    match status {
        401 => crate::tr!("服务未接受此任务保存的凭据，请检查对应服务的 API Key。 / The service rejected the saved credentials; check the API key for that service.").into(),
        403 => crate::tr!("此任务使用的凭据没有访问该服务或模型的权限。 / The credentials used by this task cannot access that service or model.").into(),
        404 => crate::tr!("服务未找到此任务指定的接口或模型，请检查服务地址和模型。 / The service does not have the endpoint or model this task names; check the base URL and model.").into(),
        429 => crate::tr!("服务请求次数达到限制，请稍后重试。 / The service rate limit was reached; try again later.").into(),
        400 | 422 => {
            crate::tr!("服务拒绝了请求参数（HTTP {}），请检查此任务使用的模型与服务设置。 / The service rejected the request parameters (HTTP {}); check the model and service settings for this task.", status, status)
        }
        _ => crate::tr!("服务未完成此请求（HTTP {}）。 / The service did not complete this request (HTTP {}).", status, status),
    }
}

/// A status alone is not evidence that changing the payload will help. Classify
/// only an explicit rejection of response_format, without retaining error text.
fn rejects_response_format(status: u16, value: Option<&Value>) -> bool {
    if !matches!(status, 400 | 422) {
        return false;
    }
    let Some(error) = value.and_then(|v| v.get("error")) else {
        return false;
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let param = error
        .get("param")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let named = param == "response_format" || message.contains("response_format");
    named
        && (matches!(
            code,
            "unsupported_parameter" | "unknown_parameter" | "unrecognized_parameter"
        ) || [
            "not supported",
            "unsupported",
            "unknown parameter",
            "unrecognized",
            "not recognized",
            "does not support",
            crate::tr!("不支持"),
        ]
        .iter()
        .any(|term| message.contains(term)))
}

/// Optional request parameters a service may refuse by name, most specific first:
/// when a refusal names several ("reasoning_effort is not supported when stream is
/// true"), the one it is about comes before plain streaming. Each has the words
/// that name it.
const OPTIONAL_PARAMETERS: [(&str, &[&str]); 3] = [
    ("reasoning", &["reasoning", "think", "thinking"]),
    ("stream_options", &["stream_options"]),
    ("stream", &["stream", "streaming"]),
];

/// The optional parameter a 400/422 names anywhere in its body (messages, `param`,
/// `code`, pydantic's `loc`, nested or in arrays), so the request may go again
/// without it. Only a name stands as evidence: an arbitrary rejection never changes
/// the payload. The body itself is not kept.
pub fn refused_parameter(status: u16, value: Option<&Value>) -> Option<&'static str> {
    if !matches!(status, 400 | 422) {
        return None;
    }
    let mut strings = Vec::new();
    collect_strings(value?, &mut strings);
    let text = strings.join(" ").to_ascii_lowercase();
    OPTIONAL_PARAMETERS
        .into_iter()
        .find(|(_, words)| words.iter().any(|word| names(&text, word)))
        .map(|(name, _)| name)
}

fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        Value::Object(map) => map.values().for_each(|item| collect_strings(item, out)),
        _ => {}
    }
}

/// Whether `text` holds `word` as a word of its own (`stream`, not `upstream`;
/// `reasoning` also as the start of `reasoning_effort`).
fn names(text: &str, word: &str) -> bool {
    let part = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !part(before) && (!part(after) || (word == "reasoning" && after == Some('_')))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SeenInRun {
    /// Short-circuited once on an earlier run's `repeats` receipt so the caller
    /// can check whether a larger-budget or split receipt already completed.
    ReplayedRepeats,
    /// Attempted during this `Ledger` instance's lifetime.
    Attempted,
}

struct Ledger {
    /// Where receipts are kept, so a later run sees them. A plain CLI request has none:
    /// nothing outlives the call.
    receipts: Option<PathBuf>,
    control_path: Option<PathBuf>,
    service_versions: BTreeMap<String, String>,
    parent: Option<ParentProcess>,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Stable IDs visited during this `Ledger` instance's lifetime.
    seen_in_run: Mutex<HashMap<String, SeenInRun>>,
    /// The service versions of the receipts on disk when first needed.
    versions: Mutex<Option<std::collections::BTreeSet<String>>>,
}

/// How often safe boundaries re-read the task control file.
pub const CONTROL_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// How a failed request may be retried within the same run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resend {
    AskUser,
    Free,
    /// The person allowed failed requests to be sent again within the same run.
    Automatic,
    /// A free service with automatic retries: as [`Resend::Free`], and an answer
    /// that could not be used is sent again too.
    FreeAutomatic,
}

impl Resend {
    /// Sending again costs nothing: a transport or server failure is retried in-run.
    fn free(self) -> bool {
        matches!(self, Self::Free | Self::FreeAutomatic)
    }

    /// A failed answer may be sent again within the same run.
    fn retries_failures(self) -> bool {
        matches!(self, Self::Automatic | Self::FreeAutomatic)
    }
}

/// The longest explanation of a service's own kept from an error response.
const DETAIL_CHARS: usize = 300;

/// What a refused or failed response says about itself: the message of an
/// OpenAI-style `error` object (or `message`, `detail`, a plain-text body), on
/// one line, shortened, and with anything that looks like a key, token or
/// signature replaced, since an error body may echo the credentials it refused:
/// `secrets` (the request's own), then whatever looks like one.
fn service_detail(value: Option<&Value>, body: &[u8], secrets: &[&str]) -> Option<String> {
    let text = match value {
        Some(value) => {
            let error = value.get("error").unwrap_or(value);
            [
                error.get("message"),
                Some(error).filter(|error| error.is_string()),
                value.get("message"),
                value.get("detail"),
                error.get("code"),
            ]
            .into_iter()
            .flatten()
            .find_map(Value::as_str)
            .map(str::to_owned)?
        }
        // A gateway's own page (HTML) says nothing the status does not.
        None => Some(String::from_utf8_lossy(body).into_owned())
            .filter(|text| !text.trim_start().starts_with('<'))?,
    };
    let mut line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for secret in secrets.iter().filter(|secret| secret.len() >= 4) {
        line = line.replace(secret, "***");
    }
    let redacted = redact_secrets(&line);
    let mut detail: String = redacted.chars().take(DETAIL_CHARS).collect();
    if redacted.chars().count() > DETAIL_CHARS {
        detail.push('…');
    }
    (!detail.is_empty()).then_some(detail)
}

/// `text` with every word that could be a credential replaced by `***`: one
/// starting `sk-` or `AIza`, a JSON web token (`eyJ…` in three dotted parts), or
/// a run of 20 or more letters, digits, `-` and `_` mixing letters and digits.
/// A run whose hyphens join two or more plain words is a name, as model names
/// are (`gpt-4o-mini-2024-07-18`), and is kept.
fn redact_secrets(text: &str) -> String {
    let random = |part: &str| {
        let letters = part.chars().any(|c| c.is_ascii_alphabetic());
        let digits = part.chars().any(|c| c.is_ascii_digit());
        let words = part
            .split('-')
            .filter(|piece| piece.len() >= 2 && piece.chars().all(|c| c.is_ascii_alphabetic()))
            .count();
        part.len() >= 20 && letters && digits && words < 2
    };
    let secret = |word: &str| {
        let parts: Vec<&str> = word.split('.').collect();
        (word.starts_with("sk-") && word.len() > 6)
            || (word.starts_with("AIza") && word.len() >= 30)
            || (parts.len() == 3 && word.starts_with("eyJ") && parts.iter().all(|p| p.len() >= 8))
            || parts.iter().any(|part| random(part))
    };
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        // A sentence's full stop is not part of the word before it.
        let trimmed = word.trim_end_matches('.');
        let stops = &word[trimmed.len()..];
        if secret(trimmed) {
            out.push_str("***");
        } else {
            out.push_str(trimmed);
        }
        out.push_str(stops);
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// `message` followed by what the service said, when it said anything.
fn with_detail(message: String, detail: Option<&str>) -> String {
    match detail {
        Some(detail) => crate::tr!("{}（服务说明：{}）", message, detail),
        None => message,
    }
}

/// Statuses that say "not now" rather than "not this": the same request may follow.
fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429)
}

/// The process that started a task. If it goes away (the app was killed
/// rather than quit), the task stops at its next safe boundary as on quit, so
/// no orphaned worker keeps changing a task the next app instance shows.
#[derive(Clone, Copy, Debug)]
pub struct ParentProcess {
    #[cfg(unix)]
    pid: u32,
}

impl ParentProcess {
    /// The current parent. Record it when the command starts, before the parent
    /// has had a chance to go away.
    pub fn current() -> Self {
        Self {
            #[cfg(unix)]
            pid: std::os::unix::process::parent_id(),
        }
    }

    fn gone(&self) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::process::parent_id() != self.pid
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

/// What a task's control handle answers to, besides its work directory.
#[derive(Clone, Debug, Default)]
pub struct ControlSource {
    /// The control file the desktop app writes; without one the task always runs.
    pub control_path: Option<PathBuf>,
    /// Configured services by name, resolved to the versions this task uses.
    pub service_versions: BTreeMap<String, String>,
    /// Stop as on quit once this process is gone.
    pub parent: Option<ParentProcess>,
}

/// A task's request ledger and control file, passed to every safe boundary.
/// The default handle has no task context: checks always pass, and each
/// request is classified through its own in-memory ledger.
#[derive(Clone, Default)]
pub struct TaskControl(Option<Arc<Ledger>>);

impl TaskControl {
    /// The control of a task whose receipts live in `work_dir/requests`.
    pub fn open(work_dir: &Path, source: ControlSource) -> Result<Self> {
        let mut ledger = Ledger::new(
            work_dir,
            source.control_path.as_deref(),
            &source.service_versions,
        )?;
        ledger.parent = source.parent;
        Ok(Self(Some(Arc::new(ledger))))
    }

    /// Safe boundaries for local work.
    pub fn check(&self) -> Result<()> {
        if let Some(ledger) = &self.0 {
            ledger.check_intent().map_err(anyhow::Error::new)?;
        }
        Ok(())
    }

    /// 轮询任务控制文件，意图不再是 run 时返回 Err。配合 `tokio::select!` 打断
    /// 长时间运行的分支：子进程句柄的 kill_on_drop 随分支 drop 生效；spawn_blocking
    /// 里的工作不会因 drop 停止，要靠它自己在安全边界调用 check。
    /// 没有任务上下文时 check 恒 Ok，此 future 永不完成，不影响 CLI 独立调用。
    pub async fn watch(&self) -> anyhow::Error {
        loop {
            tokio::time::sleep(CONTROL_POLL).await;
            if let Err(e) = self.check() {
                return e;
            }
        }
    }

    /// Sends one service request through this task's ledger.
    pub fn request(
        &self,
        spec: &RequestSpec,
        send: impl FnOnce() -> std::result::Result<HttpResponse, NetworkFailure>,
        validate: impl FnOnce(&Value) -> Result<()>,
    ) -> std::result::Result<Reply, Failure> {
        match &self.0 {
            Some(ledger) => ledger.send_described(spec, send, validate),
            // Outside a task there is no durable context, so one in-memory ledger per
            // request classifies the response by the same rules without keeping it.
            None => Ledger::with_receipts(None, None, &BTreeMap::new())
                .send_described(spec, send, validate),
        }
    }
}

/// Saved request receipts for a work directory.
pub fn receipts(work_dir: &Path) -> Result<Vec<Receipt>> {
    read_receipts(&work_dir.join("requests"))
}
fn read_receipts(dir: &Path) -> Result<Vec<Receipt>> {
    if !dir.is_dir() {
        return Ok(vec![]);
    }
    let mut result = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "json") {
            result.push(serde_json::from_slice(&std::fs::read(path)?).context(crate::tr!("已保存的服务请求记录已损坏；为避免重复扣费，没有重新发送 / A saved service request record is damaged; nothing was resent to avoid duplicate charges"))?);
        }
    }
    Ok(result)
}

impl Ledger {
    fn new(
        work_dir: &Path,
        control_path: Option<&Path>,
        versions: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let dir = work_dir.join("requests");
        std::fs::create_dir_all(&dir)?;
        Ok(Self::with_receipts(Some(dir), control_path, versions))
    }
    fn with_receipts(
        receipts: Option<PathBuf>,
        control_path: Option<&Path>,
        versions: &BTreeMap<String, String>,
    ) -> Self {
        Self {
            receipts,
            control_path: control_path.map(Path::to_path_buf),
            service_versions: versions.clone(),
            parent: None,
            locks: Mutex::new(HashMap::new()),
            seen_in_run: Mutex::new(HashMap::new()),
            versions: Mutex::new(None),
        }
    }
    fn control(&self) -> Result<Control> {
        let Some(path) = &self.control_path else {
            return Ok(Control::default());
        };
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context(crate::tr!(
                "任务状态无法识别，已暂停 / Task state is unreadable; paused"
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Control::default()),
            Err(error) => Err(error.into()),
        }
    }
    fn load(&self, stable_id: &str) -> std::result::Result<Option<Receipt>, Failure> {
        let Some(dir) = &self.receipts else {
            return Ok(None);
        };
        let path = dir.join(format!("{stable_id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        serde_json::from_slice(&std::fs::read(&path).map_err(Failure::local)?)
            .map(Some)
            .map_err(Failure::local)
    }
    fn block(&self, reason: BlockReason, message: &str) -> Failure {
        let message = message.to_string();
        let kind = FailureKind::Other;
        crate::progress::emit(&Event::Blocked {
            reason,
            request_id: None,
            purpose: None,
            description: None,
            kind,
            message: message.clone(),
        });
        Failure {
            kind,
            ..Failure::local(message)
        }
    }
    fn check_intent(&self) -> std::result::Result<Control, Failure> {
        let control = self.control().map_err(Failure::local)?;
        let intent = if self.parent.is_some_and(|parent| parent.gone()) {
            &Intent::Quit
        } else {
            &control.intent
        };
        let reason = match intent {
            Intent::Run => None,
            Intent::Pause => Some((
                BlockReason::Paused,
                crate::tr!("生成已暂停，进度已保留 / Generation paused; progress retained"),
            )),
            Intent::Cancel => Some((
                BlockReason::Cancelled,
                crate::tr!("任务已取消，已有成果已保留 / Task cancelled; saved results retained"),
            )),
            Intent::Quit => Some((
                BlockReason::Quit,
                crate::tr!("已保存进度，等待下次继续 / Progress saved; waiting to continue"),
            )),
        };
        if let Some((reason, message)) = reason {
            return Err(self.block(reason, message));
        }
        Ok(control)
    }
    fn save(&self, receipt: &Receipt) -> std::result::Result<(), Failure> {
        if let Some(dir) = &self.receipts {
            atomic_write(
                &dir.join(format!("{}.json", receipt.stable_id)),
                &serde_json::to_vec_pretty(receipt).map_err(Failure::local)?,
            )
            .map_err(Failure::local)?;
        }
        Ok(())
    }
    /// The other service versions this task's saved requests were sent with.
    fn earlier_versions(&self, current: &str) -> std::result::Result<Vec<String>, Failure> {
        let mut versions = self.versions.lock().map_err(Failure::local)?;
        if versions.is_none() {
            let all = match &self.receipts {
                Some(dir) => read_receipts(dir).map_err(Failure::local)?,
                None => Vec::new(),
            };
            *versions = Some(
                all.into_iter()
                    .map(|receipt| receipt.service_version)
                    .collect(),
            );
        }
        Ok(versions
            .as_ref()
            .into_iter()
            .flatten()
            .filter(|version| version.as_str() != current)
            .cloned()
            .collect())
    }
    fn send_described(
        &self,
        spec: &RequestSpec,
        send: impl FnOnce() -> std::result::Result<HttpResponse, NetworkFailure>,
        validate: impl FnOnce(&Value) -> Result<()>,
    ) -> std::result::Result<Reply, Failure> {
        let RequestSpec {
            service,
            purpose,
            description,
            endpoint,
            identity,
            former,
            resend,
            secrets,
        } = *spec;
        let service_version = self
            .service_versions
            .get(service)
            .cloned()
            .unwrap_or_else(|| snapshot_id(&digest(endpoint.as_bytes())));
        let id_of = |payload: &Value| {
            json_digest(&serde_json::json!({"service_version":service_version,"purpose":purpose,"endpoint":endpoint,"payload":payload})).map_err(Failure::local)
        };
        let mut stable_id = id_of(identity)?;
        if self.load(&stable_id)?.is_none() {
            for payload in former {
                let id = id_of(payload)?;
                if self.load(&id)?.is_some() {
                    stable_id = id;
                    break;
                }
            }
        }
        // A task runs with the settings current when it starts, so a resumed task
        // may name another version of its service than the one its finished requests
        // went to. The same request to the same endpoint was already paid for and
        // answered: its saved result is used instead of sending it again.
        if self.load(&stable_id)?.is_none() {
            for version in self.earlier_versions(&service_version)? {
                let id = json_digest(&serde_json::json!({"service_version":version,"purpose":purpose,"endpoint":endpoint,"payload":identity})).map_err(Failure::local)?;
                if self
                    .load(&id)?
                    .is_some_and(|receipt| receipt.state == State::Completed)
                {
                    stable_id = id;
                    break;
                }
            }
        }
        let lock = self
            .locks
            .lock()
            .map_err(Failure::local)?
            .entry(stable_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _lock = lock.lock().map_err(Failure::local)?;
        let old = self.load(&stable_id)?;
        if let Some(old) = &old
            && old.state == State::Completed
        {
            let value = old.response.clone().ok_or_else(|| {
                Failure::local(crate::tr!(
                    "已完成请求缺少保存结果；没有重新发送 / Saved request result is missing"
                ))
            })?;
            validate(&value).map_err(Failure::local)?;
            return Ok(Reply {
                value,
                replayed: true,
            });
        }
        self.check_intent()?;
        let previous_in_run = {
            let mut seen = self.seen_in_run.lock().map_err(Failure::local)?;
            let prev = seen.get(&stable_id).copied();
            let next = match prev {
                None if old.as_ref().is_some_and(|r| r.repeats) => SeenInRun::ReplayedRepeats,
                _ => SeenInRun::Attempted,
            };
            seen.insert(stable_id.clone(), next);
            prev
        };
        if let Some(old) = &old {
            if old.repeats && previous_in_run != Some(SeenInRun::ReplayedRepeats) {
                return Err(Failure {
                    repeats: true,
                    ..Failure::local(old.message.clone().unwrap_or_default())
                });
            }
            if previous_in_run == Some(SeenInRun::Attempted) {
                let can_retry_in_run = match old.state {
                    State::NotSent => true,
                    State::Rejected => old.http_status.is_some_and(retryable_status),
                    State::Failed | State::Sending | State::Uncertain | State::Completed => {
                        resend.retries_failures()
                            || (resend.free()
                                && !old.unusable
                                && old.http_status.is_none_or(|s| !(200..500).contains(&s)))
                    }
                };
                if !can_retry_in_run {
                    let fallback = match old.state {
                        State::Rejected => {
                            crate::tr!("服务拒绝了此请求 / Service rejected this request")
                        }
                        _ => crate::tr!("此请求未完成 / Request failed"),
                    };
                    return Err(Failure {
                        unsupported_response_format: old.unsupported_response_format,
                        refused_parameter: old.refused_parameter.clone(),
                        unusable: old.unusable,
                        detail: old.detail.clone(),
                        ..Failure::with_status(
                            old.http_status,
                            false,
                            old.message.clone().unwrap_or_else(|| fallback.into()),
                        )
                    });
                }
            }
        }
        let attempt = old.as_ref().map_or(1, |r| r.attempt + 1);
        let mut receipt = Receipt {
            schema: 1,
            stable_id: stable_id.clone(),
            request_id: format!("{stable_id}.{attempt}"),
            purpose: purpose.into(),
            description: description.into(),
            service_version,
            attempt,
            state: State::Sending,
            http_status: None,
            response: None,
            message: None,
            unsupported_response_format: false,
            refused_parameter: None,
            repeats: false,
            unusable: false,
            detail: None,
        };
        // This durable write happens before the only call that can send network bytes.
        self.save(&receipt)?;
        let response = match send() {
            Ok(response) => response,
            Err(error) => {
                if error.definitely_unsent {
                    receipt.state = State::NotSent;
                    receipt.message = Some(error.message.clone());
                    self.save(&receipt)?;
                    return Err(Failure::with_status(None, true, error.message));
                }
                let retryable = resend.retries_failures() || resend.free();
                receipt.state = State::Failed;
                receipt.message = Some(error.message.clone());
                self.save(&receipt)?;
                if retryable {
                    tracing::warn!(
                        "AI 响应中断，将重新发送 / AI response was interrupted; sending again"
                    );
                }
                return Err(Failure {
                    sent: true,
                    ..Failure::with_status(None, retryable, error.message)
                });
            }
        };
        receipt.http_status = Some(response.status);
        let value = serde_json::from_slice::<Value>(&response.body);
        // What an AI or speech service said is kept for a refusal of the request's
        // content or size and for a busy or failing service: the actual cause, which
        // nothing else names. Not for a refused key, where an error body is the
        // likeliest to echo it.
        let detail = (matches!(service, "llm" | "asr")
            && !matches!(response.status, 200..=299 | 401 | 403))
        .then(|| service_detail(value.as_ref().ok(), &response.body, secrets))
        .flatten();
        receipt.detail.clone_from(&detail);
        if (400..500).contains(&response.status) {
            let unsupported = rejects_response_format(response.status, value.as_ref().ok());
            let message = with_detail(rejection_message(response.status), detail.as_deref());
            receipt.state = State::Rejected;
            receipt.unsupported_response_format = unsupported;
            let refused =
                refused_parameter(response.status, value.as_ref().ok()).map(str::to_owned);
            receipt.refused_parameter = refused.clone();
            receipt.message = Some(message.clone());
            // Error bodies may echo credentials. Keep status, not the untrusted body.
            self.save(&receipt)?;
            return Err(Failure {
                unsupported_response_format: unsupported,
                refused_parameter: refused,
                detail,
                retry_after: response.retry_after,
                sent: true,
                ..Failure::with_status(
                    Some(response.status),
                    retryable_status(response.status),
                    message,
                )
            });
        }
        if !(200..300).contains(&response.status) {
            let retryable = resend.retries_failures() || resend.free();
            let message = with_detail(rejection_message(response.status), detail.as_deref());
            receipt.state = State::Failed;
            receipt.message = Some(message.clone());
            self.save(&receipt)?;
            return Err(Failure {
                retry_after: response.retry_after,
                detail,
                sent: true,
                ..Failure::with_status(Some(response.status), retryable, message)
            });
        }
        let value = match value {
            Ok(value) => value,
            Err(_) => {
                receipt.state = State::Failed;
                receipt.message = Some(
                    crate::tr!("服务返回的内容无法读取 / The service response could not be read")
                        .into(),
                );
                self.save(&receipt)?;
                return Err(Failure {
                    retryable: resend.retries_failures(),
                    sent: true,
                    ..Failure::local(receipt.message.unwrap())
                });
            }
        };
        if let Err(error) = validate(&value) {
            // An unusable answer may come out usable the next time; sending it
            // again is what the person allowed with automatic retries. One that
            // would only come back the same is left for the caller to change.
            let repeats = error.downcast_ref::<crate::outcome::Repeats>().is_some();
            receipt.state = State::Failed;
            receipt.message = Some(format!("{error:#}"));
            receipt.repeats = repeats;
            receipt.unusable = !repeats;
            self.save(&receipt)?;
            return Err(Failure {
                retryable: resend.retries_failures() && !repeats,
                repeats,
                unusable: !repeats,
                sent: true,
                ..Failure::local(receipt.message.unwrap())
            });
        }
        receipt.state = State::Completed;
        receipt.response = Some(value.clone());
        self.save(&receipt)?;
        Ok(Reply {
            value,
            replayed: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    impl Ledger {
        fn send(
            &self,
            service: &str,
            purpose: &str,
            endpoint: &str,
            identity: &Value,
            send: impl FnOnce() -> std::result::Result<HttpResponse, NetworkFailure>,
            validate: impl FnOnce(&Value) -> Result<()>,
        ) -> std::result::Result<Reply, Failure> {
            self.send_described(
                &RequestSpec {
                    service,
                    purpose,
                    description: purpose,
                    endpoint,
                    identity,
                    former: &[],
                    resend: Resend::AskUser,
                    secrets: &[],
                },
                send,
                validate,
            )
        }
    }

    #[test]
    fn unfinished_receipts_from_earlier_runs_never_block_a_new_run() {
        let dir = tempfile::tempdir().unwrap();
        let payload = serde_json::json!({"text":"paid"});
        let first = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        // The process died inside `send`: the receipt says Sending and nothing else.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = first.send(
                "llm",
                "summary",
                "https://example.test",
                &payload,
                || panic!("the process died here"),
                |_| Ok(()),
            );
        }));
        assert_eq!(receipts(dir.path()).unwrap()[0].state, State::Sending);
        // A new run sends it cleanly without deadlocking, and later requests proceed too.
        let restarted = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let reply = restarted
            .send(
                "llm",
                "summary",
                "https://example.test",
                &payload,
                success,
                |_| Ok(()),
            )
            .unwrap();
        assert!(!reply.replayed);
        assert_eq!(receipts(dir.path()).unwrap()[0].attempt, 2);
        assert_eq!(receipts(dir.path()).unwrap()[0].state, State::Completed);
        // Completed receipt replays at zero cost in the same run and in future runs.
        let replayed = restarted
            .send(
                "llm",
                "summary",
                "https://example.test",
                &payload,
                || panic!("completed receipt must replay"),
                |_| Ok(()),
            )
            .unwrap();
        assert!(replayed.replayed);
        let third = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let replayed_next = third
            .send(
                "llm",
                "summary",
                "https://example.test",
                &payload,
                || panic!("completed receipt must replay across runs"),
                |_| Ok(()),
            )
            .unwrap();
        assert!(replayed_next.replayed);
    }

    #[test]
    fn response_format_fallback_requires_explicit_provider_evidence() {
        for status in [400, 422] {
            assert!(rejects_response_format(
                status,
                Some(
                    &serde_json::json!({"error":{"param":"response_format","code":"unsupported_parameter"}})
                )
            ));
            assert!(rejects_response_format(
                status,
                Some(
                    &serde_json::json!({"error":{"message":"This model does not support response_format"}})
                )
            ));
            for error in [
                serde_json::json!({"error":{"message":"Invalid request parameters"}}),
                serde_json::json!({"error":{"param":"model","code":"unsupported_parameter"}}),
                serde_json::json!({"error":{"param":"response_format","message":"Request quota exhausted"}}),
            ] {
                assert!(!rejects_response_format(status, Some(&error)));
            }
        }
        let named = |body: Value| refused_parameter(400, Some(&body));
        assert_eq!(
            named(
                serde_json::json!({"error":{"message":"Unrecognized request argument supplied: stream_options"}})
            ),
            Some("stream_options")
        );
        assert_eq!(
            named(serde_json::json!({"error":{"param":"stream","code":"unsupported_value"}})),
            Some("stream")
        );
        assert_eq!(
            named(serde_json::json!({"error":{"message":"invalid reasoning value: \"none\""}})),
            Some("reasoning")
        );
        assert_eq!(
            named(serde_json::json!({"detail":"reasoning_effort is not permitted"})),
            Some("reasoning")
        );
        // Bodies as services send them.
        for (body, expected) in [
            // FastAPI / pydantic (vLLM, LiteLLM, many gateways)
            (
                serde_json::json!({"detail":[{"loc":["body","stream_options"],"msg":"extra fields not permitted","type":"value_error.extra"}]}),
                Some("stream_options"),
            ),
            // Gemini's OpenAI layer answers with an array.
            (
                serde_json::json!([{"error":{"code":400,"message":"Invalid JSON payload received. Unknown name \"stream_options\"","status":"INVALID_ARGUMENT"}}]),
                Some("stream_options"),
            ),
            (
                serde_json::json!({"error":{"message":"Streaming is not supported for this model"}}),
                Some("stream"),
            ),
            (
                serde_json::json!({"error":{"message":"This model does not support thinking"}}),
                Some("reasoning"),
            ),
            // Several names: the specific one is what was refused.
            (
                serde_json::json!({"error":{"message":"reasoning_effort is not supported when stream is true"}}),
                Some("reasoning"),
            ),
            (
                serde_json::json!({"error":{"message":"stream_options is only allowed when stream is true"}}),
                Some("stream_options"),
            ),
            (serde_json::json!({"error":{"message":"rate limit"}}), None),
        ] {
            assert_eq!(named(body.clone()), expected, "{body}");
        }
        // An arbitrary rejection, or one that only contains the word, names nothing.
        assert_eq!(
            named(serde_json::json!({"error":{"message":"upstream error"}})),
            None
        );
        assert_eq!(
            named(serde_json::json!({"error":{"message":"invalid model"}})),
            None
        );
        assert_eq!(
            refused_parameter(500, Some(&serde_json::json!({"error":"stream"}))),
            None
        );
        assert!(!rejects_response_format(
            500,
            Some(&serde_json::json!({"error":{"message":"response_format is unsupported"}}))
        ));
    }

    #[test]
    fn rejected_attempts_short_circuit_in_run_but_retry_across_runs_and_legacy_json_loads() {
        let dir = tempfile::tempdir().unwrap();
        let control_path = dir.path().join("control.json");
        // Legacy control.json fields must be ignored cleanly without error.
        atomic_write(
            &control_path,
            br#"{"intent":"run","stopped_services":["old-v1"],"resend":["abc.1"]}"#,
        )
        .unwrap();
        let ledger = Ledger::new(dir.path(), Some(&control_path), &Default::default()).unwrap();
        let payload = serde_json::json!({"text":"lesson"});
        let calls = AtomicUsize::new(0);
        let reject = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse::new(
                400,
                br#"{"error":{"message":"invalid model"}}"#.to_vec(),
            ))
        };
        assert!(
            ledger
                .send(
                    "llm",
                    "summary",
                    "http://example.test",
                    &payload,
                    reject,
                    |_| Ok(())
                )
                .is_err()
        );
        // Within the same Ledger run, a non-retryable rejection is not sent again.
        assert!(
            ledger
                .send(
                    "llm",
                    "summary",
                    "http://example.test",
                    &payload,
                    || panic!("same run may not repeat a rejected attempt"),
                    |_| Ok(())
                )
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Legacy receipt fields (`retry_authorized`, `state: "uncertain"`) deserialize
        // cleanly and do not lock out a new run.
        let receipt = receipts(dir.path()).unwrap().remove(0);
        let legacy_path = dir
            .path()
            .join("requests")
            .join(format!("{}.json", receipt.stable_id));
        atomic_write(
            &legacy_path,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "stable_id": receipt.stable_id,
                "request_id": receipt.request_id,
                "purpose": "summary",
                "service_version": receipt.service_version,
                "attempt": 1,
                "state": "uncertain",
                "http_status": null,
                "response": null,
                "message": "legacy uncertain",
                "retry_authorized": "legacy.1",
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap();

        let next_run = Ledger::new(dir.path(), Some(&control_path), &Default::default()).unwrap();
        let reply = next_run
            .send(
                "llm",
                "summary",
                "http://example.test",
                &payload,
                success,
                |_| Ok(()),
            )
            .unwrap();
        assert!(!reply.replayed);
        assert_eq!(receipts(dir.path()).unwrap()[0].state, State::Completed);
        assert_eq!(receipts(dir.path()).unwrap()[0].attempt, 2);
    }
    fn success() -> std::result::Result<HttpResponse, NetworkFailure> {
        Ok(HttpResponse::new(200, br#"{"text":"saved"}"#.to_vec()))
    }
    /// Reads one request, then runs `answer` on its connection. Counts requests.
    fn answering_server(
        answer: impl Fn(&mut std::net::TcpStream) + Send + Sync + 'static,
    ) -> (String, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let url = c2m_test_http::serve_stream(move |_, stream| {
            seen.fetch_add(1, Ordering::SeqCst);
            answer(stream);
        });
        (url, count)
    }

    fn body() -> Value {
        serde_json::json!({"text":"paid"})
    }

    #[test]
    fn a_lost_connection_after_sending_is_recorded_failed_and_not_resent_within_ask_user_run() {
        // The server takes the request and goes away before answering.
        let (url, requests) = answering_server(|_| {});
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let client = crate::runtime::http_agent_keeping_errors(std::time::Duration::from_secs(5));
        let send = || crate::runtime::receive(client.post(&url).send_json(body()));
        let failure = ledger
            .send("asr", "transcription", &url, &body(), send, |_| Ok(()))
            .unwrap_err();
        assert!(!failure.retryable && failure.sent, "{}", failure.message);
        assert_eq!(failure.kind, FailureKind::Local);
        assert!(
            !ledger
                .send("asr", "transcription", &url, &body(), send, |_| Ok(()))
                .unwrap_err()
                .retryable
        );
        // Neither the client library nor the same-run ledger sent it a second time.
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(receipts(dir.path()).unwrap()[0].state, State::Failed);
    }

    #[test]
    fn redirects_are_answers_not_followed() {
        use std::io::Write;
        let (target, target_requests) = answering_server(|_| {});
        let location = target.clone();
        let (url, requests) = answering_server(move |stream| {
            let _ = write!(
                stream,
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}/moved\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        });
        for client in [
            crate::runtime::http_agent(std::time::Duration::from_secs(5)),
            crate::runtime::http_agent_keeping_errors(std::time::Duration::from_secs(5)),
        ] {
            let response = crate::runtime::receive(client.post(&url).send_json(body()))
                .ok()
                .unwrap();
            assert_eq!(response.status, 307);
        }
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(target_requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn error_bodies_are_kept_for_classification() {
        use std::io::Write;
        let (url, _) = answering_server(|stream| {
            let body = r#"{"error":{"param":"response_format","code":"unsupported_parameter"}}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        let client = crate::runtime::http_agent_keeping_errors(std::time::Duration::from_secs(5));
        let response = crate::runtime::receive(client.post(&url).send_json(body()))
            .ok()
            .unwrap();
        assert_eq!(response.status, 400);
        let value = serde_json::from_slice(&response.body).unwrap();
        assert!(rejects_response_format(400, Some(&value)));
        // An agent reporting statuses as errors still gives the status.
        let client = crate::runtime::http_agent(std::time::Duration::from_secs(5));
        let response = crate::runtime::receive(client.post(&url).send_json(body()))
            .ok()
            .unwrap();
        assert_eq!(response.status, 400);
    }

    #[test]
    fn confirmed_connection_failure_can_resume_without_resend_authorization() {
        // Nothing can listen on port 0, so the connection always fails before
        // a byte is sent; a freed ephemeral port could be reused meanwhile.
        let endpoint = "http://127.0.0.1:0".to_owned();
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let payload = serde_json::json!({"text":"lesson"});
        let client = crate::runtime::http_agent(std::time::Duration::from_secs(2));
        let failure = ledger
            .send(
                "llm",
                "summary",
                &endpoint,
                &payload,
                || crate::runtime::receive(client.post(&endpoint).send_json(&payload)),
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(failure.retryable);
        assert_eq!(receipts(dir.path()).unwrap()[0].state, State::NotSent);
        ledger
            .send("llm", "summary", &endpoint, &payload, success, |_| Ok(()))
            .unwrap();
        let receipt = &receipts(dir.path()).unwrap()[0];
        assert_eq!(receipt.state, State::Completed);
        assert_eq!(receipt.attempt, 2);
    }
    #[test]
    fn free_services_resend_failed_requests_without_blocking_other_requests() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("control.json");
        let ledger = Ledger::new(dir.path(), Some(&control), &Default::default()).unwrap();
        let send = |payload: &Value, resend, ok: bool| {
            ledger.send_described(
                &RequestSpec {
                    service: "llm",
                    purpose: "proofreading",
                    description: "segment",
                    endpoint: "https://example.test",
                    identity: payload,
                    former: &[],
                    resend,
                    secrets: &[],
                },
                || {
                    if ok {
                        Ok(HttpResponse::new(200, br#"{"ok":true}"#.to_vec()))
                    } else {
                        Err(NetworkFailure {
                            message: "stalled".into(),
                            definitely_unsent: false,
                        })
                    }
                },
                |_| Ok(()),
            )
        };
        let first = serde_json::json!({"text":"one"});
        let second = serde_json::json!({"text":"two"});
        let lost = send(&first, Resend::Free, false).unwrap_err();
        assert!(lost.retryable && lost.sent);
        assert_eq!(receipts(dir.path()).unwrap()[0].state, State::Failed);
        // Other requests are never blocked behind the failed one, even under AskUser.
        assert!(send(&second, Resend::AskUser, true).is_ok());
        assert!(send(&first, Resend::Free, true).is_ok());
        assert!(
            receipts(dir.path())
                .unwrap()
                .iter()
                .all(|r| r.state == State::Completed)
        );
    }

    #[test]
    fn free_service_server_errors_resend_without_blocking_later_requests() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let payload = serde_json::json!({"text":"one"});
        let spec = RequestSpec {
            service: "llm",
            purpose: "proofreading",
            description: "segment",
            endpoint: "https://example.test",
            identity: &payload,
            former: &[],
            resend: Resend::Free,
            secrets: &[],
        };
        let failed = || Ok(HttpResponse::new(502, b"bad gateway".to_vec()));
        let first = ledger
            .send_described(&spec, failed, |_| Ok(()))
            .unwrap_err();
        assert!(first.retryable && first.sent);
        let receipt = &receipts(dir.path()).unwrap()[0];
        assert_eq!(receipt.state, State::Failed);
        assert_eq!(receipt.http_status, Some(502));
        assert!(receipt.response.is_none());
        let resent = ledger.send_described(&spec, success, |_| Ok(())).unwrap();
        assert!(!resent.replayed);
        assert_eq!(receipts(dir.path()).unwrap()[0].attempt, 2);
    }

    #[test]
    fn what_a_service_said_is_kept_without_credentials() {
        let json = |text: &str| serde_json::from_str::<Value>(text).ok();
        let said = |body: &str| {
            service_detail(json(body).as_ref(), body.as_bytes(), &["private-task-key"])
        };
        assert_eq!(
            said(r#"{"error":{"message":"Content Exists Risk","type":"invalid_request_error"}}"#)
                .as_deref(),
            Some("Content Exists Risk")
        );
        assert_eq!(
            said(r#"{"error":"Rate limit reached for model"}"#).as_deref(),
            Some("Rate limit reached for model")
        );
        assert_eq!(
            said(r#"{"code":"DataInspectionFailed","message":"Input data may contain\n inappropriate content."}"#)
                .as_deref(),
            Some("Input data may contain inappropriate content.")
        );
        let echoed = said(
            r#"{"error":{"message":"Incorrect API key provided: sk-abcdef. Key tok_9f8e7d6c5b4a3f2e1d0c9b8a is invalid."}}"#,
        )
        .unwrap();
        assert!(
            !echoed.contains("sk-abcdef") && !echoed.contains("9f8e7d6c5b4a"),
            "{echoed}"
        );
        assert!(
            echoed.starts_with("Incorrect API key provided: ***."),
            "{echoed}"
        );
        // Keys with hyphens and underscores, and tokens, are.
        for key in [
            "AIzaSyA-1b2C3d4E5f6G7h8I9j0K_lmnopqrstu",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            "a1b2c3d4-e5f6-a7b8-c9d0-e1f2a3b4c5d6",
            "sk-proj-Ab_12-Cd",
        ] {
            let said = said(&format!(
                r#"{{"error":{{"message":"Key {key} was rejected."}}}}"#
            ))
            .unwrap();
            assert_eq!(said, "Key *** was rejected.", "{key}");
        }
        // A model name is not a credential.
        assert_eq!(
            said(r#"{"error":{"message":"The model gpt-4o-mini-2024-07-18 is overloaded"}}"#)
                .as_deref(),
            Some("The model gpt-4o-mini-2024-07-18 is overloaded")
        );
        // A gateway's HTML page and an empty body say nothing.
        assert_eq!(said("<html><body>502 Bad Gateway</body></html>"), None);
        assert_eq!(said(""), None);
        assert_eq!(
            said("upstream connect error").as_deref(),
            Some("upstream connect error")
        );
        let long = said(&format!(
            r#"{{"error":{{"message":"{}"}}}}"#,
            "x ".repeat(400)
        ))
        .unwrap();
        assert_eq!(long.chars().count(), DETAIL_CHARS + 1);
    }

    #[test]
    fn a_refusal_keeps_what_the_service_said_in_its_receipt_and_message() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let body = br#"{"error":{"message":"Content Exists Risk"}}"#;
        let failure = ledger
            .send(
                "llm",
                "proofreading",
                "https://example.test",
                &serde_json::json!({"text": "x"}),
                || {
                    Ok(HttpResponse {
                        status: 400,
                        body: body.to_vec(),
                        retry_after: None,
                    })
                },
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(
            failure.message.contains("Content Exists Risk"),
            "{}",
            failure.message
        );
        let receipt = &receipts(dir.path()).unwrap()[0];
        assert_eq!(receipt.detail.as_deref(), Some("Content Exists Risk"));
        assert_eq!(receipt.state, State::Rejected);

        let asr_failure = ledger
            .send(
                "asr",
                "transcription",
                "https://example.test",
                &serde_json::json!({"chunk": 1}),
                || {
                    Ok(HttpResponse {
                        status: 400,
                        body: br#"{"error":{"message":"audio file exceeds 25MB"}}"#.to_vec(),
                        retry_after: None,
                    })
                },
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(asr_failure.sent);
        assert_eq!(
            asr_failure.detail.as_deref(),
            Some("audio file exceeds 25MB")
        );
    }

    #[test]
    fn automatic_retries_resend_failures_but_never_rejections() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let payload = |text: &str| serde_json::json!({ "text": text });
        let send = |payload: &Value,
                    answer: &dyn Fn() -> std::result::Result<HttpResponse, NetworkFailure>,
                    valid: bool| {
            ledger.send_described(
                &RequestSpec {
                    service: "llm",
                    purpose: "proofreading",
                    description: "segment",
                    endpoint: "https://example.test",
                    identity: payload,
                    former: &[],
                    resend: Resend::Automatic,
                    secrets: &[],
                },
                answer,
                |_| {
                    anyhow::ensure!(valid, "invalid structure");
                    Ok(())
                },
            )
        };
        let state = |text: &str| {
            let id = receipts(dir.path())
                .unwrap()
                .into_iter()
                .find(|r| {
                    r.request_id
                        .starts_with(&ledger_id(&ledger, &payload(text)))
                })
                .unwrap();
            (id.state, id.attempt)
        };
        // A lost answer is a failure to send again, never a question.
        let lost = send(
            &payload("lost"),
            &|| {
                Err(NetworkFailure {
                    message: "stalled".into(),
                    definitely_unsent: false,
                })
            },
            true,
        )
        .unwrap_err();
        assert!(lost.retryable && lost.sent);
        assert_eq!(state("lost"), (State::Failed, 1));
        // It holds nothing back, and goes out again itself.
        assert!(send(&payload("other"), &success, true).is_ok());
        assert!(send(&payload("lost"), &success, true).is_ok());
        assert_eq!(state("lost"), (State::Completed, 2));
        // An answer that cannot be used is sent again too.
        let invalid = send(&payload("invalid"), &success, false).unwrap_err();
        assert!(invalid.retryable);
        assert!(send(&payload("invalid"), &success, true).is_ok());
        assert_eq!(state("invalid"), (State::Completed, 2));
        // A server error, with the wait the service asked for.
        let busy = send(
            &payload("busy"),
            &|| {
                Ok(HttpResponse {
                    retry_after: Some(std::time::Duration::from_secs(3)),
                    ..HttpResponse::new(503, Vec::new())
                })
            },
            true,
        )
        .unwrap_err();
        assert!(busy.retryable);
        assert_eq!(busy.retry_after, Some(std::time::Duration::from_secs(3)));
        assert_eq!(state("busy"), (State::Failed, 1));
        // A timeout status is temporary; a bad key is not.
        let timeout = send(
            &payload("408"),
            &|| Ok(HttpResponse::new(408, vec![])),
            true,
        );
        assert!(timeout.unwrap_err().retryable);
        assert!(send(&payload("408"), &success, true).is_ok());
        let denied = send(
            &payload("401"),
            &|| Ok(HttpResponse::new(401, vec![])),
            true,
        );
        assert!(!denied.unwrap_err().retryable);
        let again = send(
            &payload("401"),
            &|| panic!("a rejected request was sent again"),
            true,
        );
        assert!(again.is_err());
        // A request another process left mid-flight is retried cleanly in a new run.
        let stable_id = ledger_id(&ledger, &payload("dead"));
        ledger
            .save(&Receipt {
                schema: 1,
                stable_id: stable_id.clone(),
                request_id: format!("{stable_id}.1"),
                purpose: "proofreading".into(),
                description: "segment".into(),
                service_version: snapshot_id(&digest(b"https://example.test")),
                attempt: 1,
                state: State::Sending,
                http_status: None,
                response: None,
                message: None,
                unsupported_response_format: false,
                refused_parameter: None,
                repeats: false,
                unusable: false,
                detail: None,
            })
            .unwrap();
        assert!(send(&payload("dead"), &success, true).is_ok());
        assert_eq!(state("dead"), (State::Completed, 2));
    }

    #[test]
    fn free_automatic_retries_record_failures_and_repeats_short_circuits_only_within_run() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let spec = |payload: &'static Value, resend| RequestSpec {
            service: "llm",
            purpose: "proofreading",
            description: "segment",
            endpoint: "https://example.test",
            identity: payload,
            former: &[],
            resend,
            secrets: &[],
        };
        static LOST: std::sync::LazyLock<Value> =
            std::sync::LazyLock::new(|| serde_json::json!({"text": "lost"}));
        static FULL: std::sync::LazyLock<Value> =
            std::sync::LazyLock::new(|| serde_json::json!({"text": "full"}));
        let lost = ledger
            .send_described(
                &spec(&LOST, Resend::FreeAutomatic),
                || {
                    Err(NetworkFailure {
                        message: "stalled".into(),
                        definitely_unsent: false,
                    })
                },
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(lost.retryable);
        let states: Vec<_> = receipts(dir.path())
            .unwrap()
            .into_iter()
            .map(|r| r.state)
            .collect();
        assert_eq!(states, [State::Failed]);

        // Within the same run, an answer marked `repeats` is not sent again.
        let repeats = || {
            ledger.send_described(&spec(&FULL, Resend::Automatic), success, |_| {
                Err(anyhow::Error::new(crate::outcome::Repeats(
                    "ran out".into(),
                )))
            })
        };
        let first = repeats().unwrap_err();
        assert!(first.repeats && !first.retryable);
        for resend in [Resend::Automatic, Resend::AskUser, Resend::FreeAutomatic] {
            let again = ledger
                .send_described(
                    &spec(&FULL, resend),
                    || panic!("an answer that repeats was asked for again in the same run"),
                    |_| Ok(()),
                )
                .unwrap_err();
            assert!(again.repeats, "{resend:?}");
        }
        // A subsequent run first lets the caller probe whether an escalated step
        // already completed, then sends cleanly when asked again.
        let next_run = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let probe = next_run
            .send_described(
                &spec(&FULL, Resend::AskUser),
                || panic!("probe visit should check escalated steps before sending"),
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(probe.repeats);
        let retried = next_run
            .send_described(&spec(&FULL, Resend::AskUser), success, |_| Ok(()))
            .unwrap();
        assert!(!retried.replayed);
    }

    fn ledger_id(ledger: &Ledger, payload: &Value) -> String {
        let version = ledger
            .service_versions
            .get("llm")
            .cloned()
            .unwrap_or_else(|| snapshot_id(&digest(b"https://example.test")));
        json_digest(&serde_json::json!({"service_version":version,"purpose":"proofreading","endpoint":"https://example.test","payload":payload})).unwrap()
    }

    #[test]
    fn retry_after_is_read_from_the_response() {
        use std::io::Write;
        let (url, _) = answering_server(|stream| {
            let _ = write!(
                stream,
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 12\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        });
        let client = crate::runtime::http_agent_keeping_errors(std::time::Duration::from_secs(5));
        let response = crate::runtime::receive(client.post(&url).send_json(body()))
            .ok()
            .unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(
            response.retry_after,
            Some(std::time::Duration::from_secs(12))
        );
    }

    /// Lost responses in an earlier run record `State::Failed` and never block
    /// sibling or downstream batches in a new run; completed batches replay at zero cost.
    #[test]
    fn failed_batches_from_earlier_run_retry_independently_while_completed_replay() {
        for count in [2, 3] {
            let dir = tempfile::tempdir().unwrap();
            let lost = || {
                Err(NetworkFailure {
                    message: "lost".into(),
                    definitely_unsent: false,
                })
            };
            let payload = |index: usize| serde_json::json!({"batch": index});
            let first = Ledger::new(dir.path(), None, &Default::default()).unwrap();
            for index in 0..count {
                let err = first
                    .send(
                        "llm",
                        "translation",
                        "http://e.test",
                        &payload(index),
                        lost,
                        |_| Ok(()),
                    )
                    .unwrap_err();
                assert!(!err.retryable && err.sent);
            }
            // A sibling batch in the same run is not blocked behind the failed ones.
            first
                .send(
                    "llm",
                    "translation",
                    "http://e.test",
                    &payload(count),
                    || Ok(HttpResponse::new(200, b"{}".to_vec())),
                    |_| Ok(()),
                )
                .unwrap();

            // Next run: the completed sibling replays at 0 cost, and the failed ones resend once.
            let ok = |calls: &AtomicUsize| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(HttpResponse::new(200, b"{}".to_vec()))
            };
            let next = Ledger::new(dir.path(), None, &Default::default()).unwrap();
            let calls = AtomicUsize::new(0);
            for index in std::iter::once(count).chain((0..count).rev()) {
                let reply = next
                    .send(
                        "llm",
                        "translation",
                        "http://e.test",
                        &payload(index),
                        || ok(&calls),
                        |_| Ok(()),
                    )
                    .unwrap();
                assert_eq!(reply.replayed, index == count);
            }
            assert_eq!(calls.load(Ordering::SeqCst), count);
            for index in 0..=count {
                let replay = next
                    .send(
                        "llm",
                        "translation",
                        "http://e.test",
                        &payload(index),
                        || panic!("a completed request was sent again"),
                        |_| Ok(()),
                    )
                    .unwrap();
                assert!(replay.replayed);
            }
        }
    }
    /// A task resumed with another version of its service (the settings changed
    /// while it was paused) does not pay again for a request it already has the
    /// answer to; a request the new version changes is sent.
    #[test]
    fn a_finished_request_is_not_sent_again_under_another_service_version() {
        let dir = tempfile::tempdir().unwrap();
        let payload = serde_json::json!({"model":"m","text":"lesson"});
        let first = Ledger::new(
            dir.path(),
            None,
            &[("llm".to_owned(), "version-1".to_owned())].into(),
        )
        .unwrap();
        let answered = first
            .send(
                "llm",
                "proofreading",
                "https://example.test",
                &payload,
                success,
                |_| Ok(()),
            )
            .unwrap();
        assert!(!answered.replayed);
        let resumed = Ledger::new(
            dir.path(),
            None,
            &[("llm".to_owned(), "version-2".to_owned())].into(),
        )
        .unwrap();
        let replay = resumed
            .send(
                "llm",
                "proofreading",
                "https://example.test",
                &payload,
                || panic!("an identical finished request must not be paid for again"),
                |_| Ok(()),
            )
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.value, answered.value);
        // Another model is another request.
        let changed = resumed
            .send(
                "llm",
                "proofreading",
                "https://example.test",
                &serde_json::json!({"model":"other","text":"lesson"}),
                success,
                |_| Ok(()),
            )
            .unwrap();
        assert!(!changed.replayed);
    }

    #[test]
    fn durable_sending_and_protocol_failure_are_not_automatic_retries() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let payload = serde_json::json!({"text":"lesson"});
        let result = ledger.send(
            "llm",
            "proofreading",
            "https://example.test",
            &payload,
            || {
                assert_eq!(receipts(dir.path()).unwrap()[0].state, State::Sending);
                Ok(HttpResponse::new(200, br#"{"error":"invalid"}"#.to_vec()))
            },
            |_| anyhow::bail!("协议错误"),
        );
        assert!(result.is_err());
        assert!(
            ledger
                .send(
                    "llm",
                    "proofreading",
                    "https://example.test",
                    &payload,
                    || panic!("protocol errors must not resend"),
                    |_| Ok(())
                )
                .is_err()
        );
    }
    #[test]
    fn paused_or_cancelled_task_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.json");
        let versions = BTreeMap::from([("llm".into(), "service-v1".into())]);
        let ledger = Ledger::new(dir.path(), Some(&path), &versions).unwrap();
        for intent in [Intent::Pause, Intent::Cancel, Intent::Quit] {
            atomic_write(&path, &serde_json::to_vec(&Control { intent }).unwrap()).unwrap();
            assert!(
                ledger
                    .send(
                        "llm",
                        "summary",
                        "https://example.test",
                        &Value::Null,
                        || panic!("blocked request sent"),
                        |_| Ok(())
                    )
                    .is_err()
            );
        }
        assert!(receipts(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn each_task_answers_only_to_its_own_control_file() {
        let (paused, running) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let open = |dir: &Path| {
            TaskControl::open(
                dir,
                ControlSource {
                    control_path: Some(dir.join("control.json")),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let (first, second) = (open(paused.path()), open(running.path()));
        atomic_write(
            &paused.path().join("control.json"),
            &serde_json::to_vec(&Control {
                intent: Intent::Pause,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(first.check().is_err());
        assert!(first.clone().check().is_err());
        second.check().unwrap();
        TaskControl::default().check().unwrap();
    }
}

#[cfg(test)]
mod ephemeral_tests {
    use super::*;

    fn request(status: u16, body: &[u8], resend: Resend) -> std::result::Result<Reply, Failure> {
        let body = body.to_vec();
        TaskControl::default().request(
            &RequestSpec {
                service: "llm",
                purpose: "summary",
                description: "summary",
                endpoint: "https://example.test",
                identity: &serde_json::json!({"n":1}),
                former: &[],
                resend,
                secrets: &[],
            },
            move || Ok(HttpResponse::new(status, body)),
            |_| Ok(()),
        )
    }

    #[test]
    fn requests_outside_a_task_are_classified_by_the_same_rules() {
        assert_eq!(
            request(200, br#"{"ok":true}"#, Resend::AskUser)
                .unwrap()
                .value["ok"],
            true
        );
        let rejected = request(401, b"{}", Resend::AskUser).unwrap_err();
        assert_eq!(rejected.kind, FailureKind::Authentication);
        assert!(!rejected.retryable && rejected.sent);
        assert!(request(429, b"{}", Resend::AskUser).unwrap_err().retryable);
        let server_err = request(503, b"", Resend::AskUser).unwrap_err();
        assert!(!server_err.retryable && server_err.sent);
        assert_eq!(server_err.kind, FailureKind::Service);
        let free = request(503, b"", Resend::Free).unwrap_err();
        assert!(free.retryable && free.sent);
        assert_eq!(free.status, Some(503));
        assert_eq!(free.kind, FailureKind::Service);
        // Nothing persists between calls: the same payload sends again.
        assert!(request(200, br#"{"ok":true}"#, Resend::AskUser).is_ok());
        let lost = TaskControl::default()
            .request(
                &RequestSpec {
                    service: "llm",
                    purpose: "summary",
                    description: "summary",
                    endpoint: "https://example.test",
                    identity: &serde_json::json!({"n":2}),
                    former: &[],
                    resend: Resend::Free,
                    secrets: &[],
                },
                || {
                    Err(NetworkFailure {
                        message: "stalled".into(),
                        definitely_unsent: false,
                    })
                },
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(lost.retryable && lost.sent);
    }
}
