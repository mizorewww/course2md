//! Durable external-request receipts. A lost response is never permission to resend.

use crate::{
    checkpoint::atomic_write,
    execution,
    progress::{BlockReason, Event},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

/// Durable receipts describe a service snapshot without a stored configuration.
pub const SNAPSHOT_PREFIX: &str = "snapshot:";

pub fn snapshot_id(digest: &str) -> String {
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
    pub stopped_services: Vec<String>,
    /// IDs identify an exact previous attempt, so persistent authorization is consumed once.
    pub resend: Vec<String>,
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
    /// Explicit component reprocessing permits this exact known-failed attempt once.
    /// Sending/uncertain attempts still require Control::resend authorization.
    #[serde(default)]
    pub retry_authorized: Option<String>,
}

#[derive(Debug)]
pub struct Failure {
    pub status: Option<u16>,
    pub retryable: bool,
    pub message: String,
    pub unsupported_response_format: bool,
    pub kind: crate::artifact::FailureKind,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}
impl Failure {
    /// A definite failure that no service status explains.
    pub fn local(error: impl std::fmt::Display) -> Self {
        Self {
            status: None,
            retryable: false,
            message: error.to_string(),
            unsupported_response_format: false,
            kind: crate::artifact::FailureKind::Local,
        }
    }
    fn with_status(status: Option<u16>, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            status,
            retryable,
            message: message.into(),
            kind: kind_for_status(status),
            ..Self::local("")
        }
    }
    /// The request may have been processed; sending it again needs consent.
    pub fn uncertain(&self) -> bool {
        self.kind == crate::artifact::FailureKind::Uncertain
    }
}

/// A service response, and whether it was received now or replayed from a
/// completed receipt (callers that report live usage skip replays).
#[derive(Debug)]
pub struct Reply {
    pub value: Value,
    pub replayed: bool,
}

struct Rejection {
    retryable: bool,
    unsupported_response_format: bool,
    message: String,
    kind: crate::artifact::FailureKind,
}

fn kind_for_status(status: Option<u16>) -> crate::artifact::FailureKind {
    match status {
        Some(401 | 403) => crate::artifact::FailureKind::Authentication,
        Some(404) => crate::artifact::FailureKind::Model,
        Some(400 | 422) => crate::artifact::FailureKind::Parameters,
        Some(429) => crate::artifact::FailureKind::RateLimit,
        Some(500..=599) => crate::artifact::FailureKind::Service,
        None => crate::artifact::FailureKind::Local,
        _ => crate::artifact::FailureKind::Other,
    }
}

fn classify_rejection(status: u16, value: Option<&Value>) -> Rejection {
    Rejection {
        retryable: status == 429,
        unsupported_response_format: rejects_response_format(status, value),
        message: rejection_message(status),
        kind: kind_for_status(Some(status)),
    }
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

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}
pub struct NetworkFailure {
    pub message: String,
    pub definitely_unsent: bool,
}

/// Where a ledger keeps its receipts. A task keeps them on disk so a later run
/// sees them; a plain CLI request keeps them in memory for that one call, so a
/// lost response is classified the same way without surviving the process.
enum Receipts {
    Disk(PathBuf),
    Memory(Mutex<HashMap<String, Receipt>>),
}

struct Ledger {
    receipts: Receipts,
    control_path: Option<PathBuf>,
    /// The control file as last read, with the metadata it had: polling at every
    /// safe boundary must not parse the same bytes again.
    control: Mutex<Option<(std::fs::Metadata, Control)>>,
    service_versions: BTreeMap<String, String>,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    emitted: Mutex<HashSet<String>>,
    /// Sending/Uncertain receipts by stable id: read from disk once, then kept in step by `save`.
    unresolved: Mutex<Option<HashMap<String, Receipt>>>,
}
static ACTIVE: OnceLock<Mutex<Option<Arc<Ledger>>>> = OnceLock::new();

/// How often safe boundaries re-read the task control file.
pub const CONTROL_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Whether an unconfirmed request may be sent again without asking: only when
/// the service has no per-request billing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resend {
    AskUser,
    Free,
}

/// The process that started this task. If it goes away (the app was killed
/// rather than quit), the task stops at its next safe boundary as on quit, so
/// no orphaned worker keeps changing a task the next app instance shows.
#[cfg(unix)]
static PARENT: OnceLock<u32> = OnceLock::new();

pub fn watch_parent() {
    #[cfg(unix)]
    let _ = PARENT.set(std::os::unix::process::parent_id());
}

fn parent_gone() -> bool {
    #[cfg(unix)]
    {
        PARENT
            .get()
            .is_some_and(|parent| std::os::unix::process::parent_id() != *parent)
    }
    #[cfg(not(unix))]
    {
        false
    }
}
fn active() -> Option<Arc<Ledger>> {
    ACTIVE.get_or_init(|| Mutex::new(None)).lock().ok()?.clone()
}
pub struct Guard(Arc<Ledger>);
impl Drop for Guard {
    fn drop(&mut self) {
        // 锁中毒也要清掉当前 guard：否则 install 会一直报「同一进程不能同时执行两个任务」
        let mut current = ACTIVE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if current
            .as_ref()
            .is_some_and(|value| Arc::ptr_eq(value, &self.0))
        {
            *current = None;
        }
    }
}

pub fn install(
    work_dir: &Path,
    control_path: Option<&Path>,
    service_versions: &BTreeMap<String, String>,
) -> Result<Guard> {
    let ledger = Arc::new(Ledger::new(work_dir, control_path, service_versions)?);
    let mut current = ACTIVE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| {
            anyhow::anyhow!(crate::tr!(
                "任务状态异常，请重启应用 / Task state is unavailable; restart the app"
            ))
        })?;
    anyhow::ensure!(
        current.is_none(),
        crate::tr!("同一进程不能同时执行两个任务 / Another task is already active")
    );
    *current = Some(ledger.clone());
    Ok(Guard(ledger))
}

/// Safe boundaries for local work. Unknown cloud receipts do not prevent saving local work.
pub fn check_control() -> Result<()> {
    if let Some(ledger) = active() {
        ledger.check_intent(None).map_err(anyhow::Error::new)?;
    }
    Ok(())
}

/// 轮询任务控制文件，意图不再是 run 时返回 Err。配合 `tokio::select!` 打断
/// 长时间运行的分支：子进程句柄的 kill_on_drop 随分支 drop 生效；spawn_blocking
/// 里的工作不会因 drop 停止，要靠它自己在安全边界调用 check_control。
/// 无活动任务账本时 check_control 恒 Ok，此 future 永不完成，不影响 CLI 独立调用。
pub async fn watch_control() -> anyhow::Error {
    loop {
        tokio::time::sleep(CONTROL_POLL).await;
        if let Err(e) = check_control() {
            return e;
        }
    }
}

/// Exposes unresolved attempts for task state reconciliation even if the process crashed.
pub fn receipts(work_dir: &Path) -> Result<Vec<Receipt>> {
    read_receipts(&work_dir.join("requests"))
}
fn is_unresolved(receipt: &Receipt) -> bool {
    matches!(receipt.state, State::Sending | State::Uncertain)
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
        Ok(Self::with_receipts(
            Receipts::Disk(dir),
            control_path,
            versions,
        ))
    }
    /// A ledger for one request outside a task: same classification, no files.
    fn ephemeral() -> Self {
        Self::with_receipts(
            Receipts::Memory(Mutex::new(HashMap::new())),
            None,
            &BTreeMap::new(),
        )
    }
    fn with_receipts(
        receipts: Receipts,
        control_path: Option<&Path>,
        versions: &BTreeMap<String, String>,
    ) -> Self {
        Self {
            receipts,
            control_path: control_path.map(Path::to_path_buf),
            control: Mutex::new(None),
            service_versions: versions.clone(),
            locks: Mutex::new(HashMap::new()),
            emitted: Mutex::new(HashSet::new()),
            unresolved: Mutex::new(None),
        }
    }
    fn control(&self) -> Result<Control> {
        let Some(path) = &self.control_path else {
            return Ok(Control::default());
        };
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Control::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut cached = crate::runtime::lock_recover(&self.control);
        if let Some((seen, control)) = cached.as_ref()
            && seen.len() == metadata.len()
            && seen.modified().ok() == metadata.modified().ok()
        {
            return Ok(control.clone());
        }
        let control: Control = serde_json::from_slice(&std::fs::read(path)?).context(
            crate::tr!("任务状态无法识别，已暂停 / Task state is unreadable; paused"),
        )?;
        *cached = Some((metadata, control.clone()));
        Ok(control)
    }
    fn load(&self, stable_id: &str) -> std::result::Result<Option<Receipt>, Failure> {
        match &self.receipts {
            Receipts::Disk(dir) => {
                let path = dir.join(format!("{stable_id}.json"));
                if !path.exists() {
                    return Ok(None);
                }
                serde_json::from_slice(&std::fs::read(&path).map_err(Failure::local)?)
                    .map(Some)
                    .map_err(Failure::local)
            }
            Receipts::Memory(map) => {
                Ok(map.lock().map_err(Failure::local)?.get(stable_id).cloned())
            }
        }
    }
    fn block(&self, reason: BlockReason, request: Option<&Receipt>, message: &str) -> Failure {
        let request_id = request.map(|r| r.request_id.clone());
        let message = request.filter(|r| !r.description.is_empty()).map_or_else(
            || message.to_string(),
            |r| crate::tr!("{}：{}", r.description, message),
        );
        let kind = if reason == BlockReason::Uncertain {
            crate::artifact::FailureKind::Uncertain
        } else {
            crate::artifact::FailureKind::Other
        };
        let signature = format!("{reason:?}:{request_id:?}:{message}");
        if self
            .emitted
            .lock()
            .map(|mut seen| seen.insert(signature))
            .unwrap_or(true)
        {
            crate::progress::emit(&Event::Blocked {
                reason,
                request_id: request_id.clone(),
                purpose: request.map(|r| r.purpose.clone()),
                description: request.map(|r| r.description.clone()),
                kind,
                message: message.clone(),
            });
        }
        Failure {
            kind,
            ..Failure::local(message)
        }
    }
    fn check_intent(&self, service_version: Option<&str>) -> std::result::Result<Control, Failure> {
        let control = self.control().map_err(Failure::local)?;
        let intent = if parent_gone() {
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
            return Err(self.block(reason, None, message));
        }
        if service_version
            .is_some_and(|version| control.stopped_services.iter().any(|v| v == version))
        {
            return Err(self.block(
                BlockReason::ServiceStopped,
                None,
                crate::tr!("此服务已删除，没有发送新请求 / Service deleted; no new request sent"),
            ));
        }
        Ok(control)
    }
    fn save(&self, receipt: &Receipt) -> std::result::Result<(), Failure> {
        match &self.receipts {
            Receipts::Disk(dir) => atomic_write(
                &dir.join(format!("{}.json", receipt.stable_id)),
                &serde_json::to_vec_pretty(receipt).map_err(Failure::local)?,
            )
            .map_err(Failure::local)?,
            Receipts::Memory(map) => {
                map.lock()
                    .map_err(Failure::local)?
                    .insert(receipt.stable_id.clone(), receipt.clone());
            }
        }
        if let Some(index) = self.unresolved.lock().map_err(Failure::local)?.as_mut() {
            if is_unresolved(receipt) {
                index.insert(receipt.stable_id.clone(), receipt.clone());
            } else {
                index.remove(&receipt.stable_id);
            }
        }
        Ok(())
    }
    fn unresolved_receipts(&self) -> std::result::Result<Vec<Receipt>, Failure> {
        let mut index = self.unresolved.lock().map_err(Failure::local)?;
        if index.is_none() {
            let all = match &self.receipts {
                Receipts::Disk(dir) => read_receipts(dir).map_err(Failure::local)?,
                Receipts::Memory(map) => map
                    .lock()
                    .map_err(Failure::local)?
                    .values()
                    .cloned()
                    .collect(),
            };
            let loaded = all
                .into_iter()
                .filter(is_unresolved)
                .map(|receipt| (receipt.stable_id.clone(), receipt))
                .collect();
            *index = Some(loaded);
        }
        Ok(index
            .as_ref()
            .map_or_else(Vec::new, |index| index.values().cloned().collect()))
    }
    #[cfg(test)]
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
            service,
            purpose,
            purpose,
            endpoint,
            identity,
            Resend::AskUser,
            send,
            validate,
        )
    }
    // Keep the transport, validation and persisted request identity explicit at the call site.
    #[allow(clippy::too_many_arguments)]
    fn send_described(
        &self,
        service: &str,
        purpose: &str,
        description: &str,
        endpoint: &str,
        identity: &Value,
        resend: Resend,
        send: impl FnOnce() -> std::result::Result<HttpResponse, NetworkFailure>,
        validate: impl FnOnce(&Value) -> Result<()>,
    ) -> std::result::Result<Reply, Failure> {
        let service_version = self
            .service_versions
            .get(service)
            .cloned()
            .unwrap_or_else(|| snapshot_id(&execution::digest(endpoint.as_bytes())));
        let stable_id = execution::digest(&serde_json::to_vec(&serde_json::json!({"service_version":service_version,"purpose":purpose,"endpoint":endpoint,"payload":identity})).map_err(Failure::local)?);
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
        let control = self.check_intent(Some(&service_version))?;
        let authorized = old.as_ref().is_some_and(|receipt| {
            (resend == Resend::Free && matches!(receipt.state, State::Sending | State::Uncertain))
                || control.resend.contains(&receipt.request_id)
                || (matches!(receipt.state, State::Failed | State::Rejected)
                    && receipt.retry_authorized.as_ref() == Some(&receipt.request_id))
        });
        if let Some(old) = &old {
            if matches!(old.state, State::Sending | State::Uncertain) && !authorized {
                let mut uncertain = old.clone();
                uncertain.state = State::Uncertain;
                self.save(&uncertain)?;
                return Err(self.block(BlockReason::Uncertain, Some(old), crate::tr!("未收到服务的确定结果。服务可能已处理这部分，再次提交可能产生额外费用 / Service result is uncertain; resending may incur additional charges")));
            }
            let settled = match old.state {
                State::Failed => Some(crate::tr!("此请求未完成 / Request failed")),
                State::Rejected if old.http_status != Some(429) => Some(crate::tr!(
                    "服务拒绝了此请求 / Service rejected this request"
                )),
                _ => None,
            };
            if let Some(fallback) = settled
                && !authorized
            {
                return Err(Failure {
                    unsupported_response_format: old.unsupported_response_format,
                    ..Failure::with_status(
                        old.http_status,
                        false,
                        old.message.clone().unwrap_or_else(|| fallback.into()),
                    )
                });
            }
        }
        // An unknown prerequisite cannot be bypassed by splitting/relaxing the payload
        // or by beginning downstream summary requests. Unrelated local work remains possible.
        // A free service resends unresolved requests when it reaches them, so
        // overlapping or downstream requests cost nothing extra either.
        for unresolved in self
            .unresolved_receipts()?
            .into_iter()
            .filter(|_| resend == Resend::AskUser)
        {
            if unresolved.stable_id != stable_id {
                // Live in-flight requests may run concurrently; only stale Sending from another
                // process is unknown. A lock currently held in this context proves it is live.
                let live = self
                    .locks
                    .lock()
                    .map_err(Failure::local)?
                    .get(&unresolved.stable_id)
                    .cloned()
                    .is_some_and(|lock| lock.try_lock().is_err());
                if unresolved.state == State::Uncertain || !live {
                    return Err(self.block(BlockReason::Uncertain, Some(&unresolved), crate::tr!("前一步请求结果尚不确定；已保留进度，没有继续发送 / A previous request is unresolved; progress retained")));
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
            retry_authorized: None,
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
                receipt.state = State::Uncertain;
                receipt.message = Some(error.message.clone());
                self.save(&receipt)?;
                // Without per-request billing (a ChatGPT plan, a local server) a
                // stalled or unfinished response costs nothing to send again, so
                // the caller's bounded retry loop resends it instead of stopping
                // every later request behind an uncertain result.
                if resend == Resend::Free {
                    tracing::warn!(
                        "AI 响应中断，将重新发送 / AI response was interrupted; sending again"
                    );
                    return Err(Failure::with_status(None, true, error.message));
                }
                return Err(self.block(BlockReason::Uncertain, Some(&receipt), crate::tr!("未收到服务的确定结果。服务可能已处理这部分，再次提交可能产生额外费用 / Service result is uncertain; resending may incur additional charges")));
            }
        };
        receipt.http_status = Some(response.status);
        let value = serde_json::from_slice::<Value>(&response.body);
        if (400..500).contains(&response.status) {
            let rejection = classify_rejection(response.status, value.as_ref().ok());
            receipt.state = State::Rejected;
            receipt.unsupported_response_format = rejection.unsupported_response_format;
            receipt.message = Some(rejection.message.clone());
            // Error bodies may echo credentials. Keep status, not the untrusted body.
            self.save(&receipt)?;
            // Each caller bounds its retry loop; a later task continuation may retry
            // a confirmed rejection without requiring uncertain-request authorization.
            return Err(Failure {
                unsupported_response_format: rejection.unsupported_response_format,
                kind: rejection.kind,
                ..Failure::with_status(
                    Some(response.status),
                    rejection.retryable,
                    rejection.message,
                )
            });
        }
        if !(200..300).contains(&response.status) {
            receipt.state = State::Uncertain;
            self.save(&receipt)?;
            return Err(self.block(BlockReason::Uncertain, Some(&receipt), crate::tr!("服务返回错误，无法确认是否已处理。再次提交可能产生额外费用 / Service processing is uncertain")));
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
                return Err(Failure::local(receipt.message.unwrap()));
            }
        };
        if let Err(error) = validate(&value) {
            receipt.state = State::Failed;
            receipt.message = Some(format!("{error:#}"));
            self.save(&receipt)?;
            return Err(Failure::local(receipt.message.unwrap()));
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

#[allow(clippy::too_many_arguments)]
pub fn json_request_described(
    service: &str,
    purpose: &str,
    description: &str,
    endpoint: &str,
    identity: &Value,
    resend: Resend,
    send: impl FnOnce() -> std::result::Result<HttpResponse, NetworkFailure>,
    validate: impl FnOnce(&Value) -> Result<()>,
) -> std::result::Result<Reply, Failure> {
    // Outside a task there is no durable context, so one in-memory ledger per
    // request classifies the response by the same rules without keeping it.
    let ledger = active().unwrap_or_else(|| Arc::new(Ledger::ephemeral()));
    ledger.send_described(
        service,
        purpose,
        description,
        endpoint,
        identity,
        resend,
        send,
        validate,
    )
}

/// Drain the HTTP body before recording completion; a body read error is still uncertain.
/// Cloud callers must disable redirects: a failed connection after a redirect would not
/// prove that the initial request was never handled.
pub fn receive(
    result: std::result::Result<ureq::Response, ureq::Error>,
) -> std::result::Result<HttpResponse, NetworkFailure> {
    use std::io::Read;
    let response = match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(error)) => {
            let definitely_unsent = matches!(
                error.kind(),
                ureq::ErrorKind::Dns | ureq::ErrorKind::ConnectionFailed
            );
            return Err(NetworkFailure {
                message: if definitely_unsent {
                    crate::tr!("无法连接服务，尚未提交内容 / Could not connect; request content was not submitted")
                } else {
                    crate::tr!("未完整收到服务响应 / Did not receive a complete service response")
                }
                .into(),
                definitely_unsent,
            });
        }
    };
    let status = response.status();
    let mut body = Vec::new();
    const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| NetworkFailure {
            message: crate::tr!("服务响应接收中断 / Service response was interrupted").into(),
            definitely_unsent: false,
        })?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(NetworkFailure {
            message: crate::tr!(
                "服务响应超过接收上限 / Service response exceeds the receiving limit"
            )
            .into(),
            definitely_unsent: false,
        });
    }
    Ok(HttpResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        assert!(!rejects_response_format(
            500,
            Some(&serde_json::json!({"error":{"message":"response_format is unsupported"}}))
        ));
    }

    #[test]
    fn explicit_failed_component_authorization_is_consumed_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path(), None, &Default::default()).unwrap();
        let payload = serde_json::json!({"text":"lesson"});
        let calls = AtomicUsize::new(0);
        let reject = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse {
                status: 400,
                body: br#"{"error":{"message":"invalid model"}}"#.to_vec(),
            })
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
        let mut receipt = receipts(dir.path()).unwrap().remove(0);
        receipt.retry_authorized = Some(receipt.request_id.clone());
        ledger.save(&receipt).unwrap();
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
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let mut receipt = receipts(dir.path()).unwrap().remove(0);
        assert!(receipt.retry_authorized.is_none());
        assert!(
            ledger
                .send(
                    "llm",
                    "summary",
                    "http://example.test",
                    &payload,
                    || panic!("same task may not repeat a rejected attempt"),
                    |_| Ok(())
                )
                .is_err()
        );
        // Even a stale/forged known-failure authorization never authorizes an unknown result.
        receipt.state = State::Uncertain;
        receipt.retry_authorized = Some(receipt.request_id.clone());
        ledger.save(&receipt).unwrap();
        assert!(
            ledger
                .send(
                    "llm",
                    "summary",
                    "http://example.test",
                    &payload,
                    || panic!("unknown requires Control::resend"),
                    |_| Ok(())
                )
                .unwrap_err()
                .uncertain()
        );
    }
    fn success() -> std::result::Result<HttpResponse, NetworkFailure> {
        Ok(HttpResponse {
            status: 200,
            body: br#"{"text":"saved"}"#.to_vec(),
        })
    }
    #[test]
    fn confirmed_connection_failure_can_resume_without_resend_authorization() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
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
                || receive(client.post(&endpoint).send_json(&payload)),
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(failure.retryable);
        assert!(!failure.uncertain());
        assert_eq!(receipts(dir.path()).unwrap()[0].state, State::NotSent);
        ledger
            .send("llm", "summary", &endpoint, &payload, success, |_| Ok(()))
            .unwrap();
        let receipt = &receipts(dir.path()).unwrap()[0];
        assert_eq!(receipt.state, State::Completed);
        assert_eq!(receipt.attempt, 2);
    }
    #[test]
    fn free_services_resend_unconfirmed_requests_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("control.json");
        let ledger = Ledger::new(dir.path(), Some(&control), &Default::default()).unwrap();
        let send = |payload: &Value, resend, ok: bool| {
            ledger.send_described(
                "llm",
                "proofreading",
                "segment",
                "https://example.test",
                payload,
                resend,
                || {
                    if ok {
                        Ok(HttpResponse {
                            status: 200,
                            body: br#"{"ok":true}"#.to_vec(),
                        })
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
        // A free service's lost response is a plain retryable failure for the
        // caller's bounded loop; the receipt still records it as unresolved.
        let lost = send(&first, Resend::Free, false).unwrap_err();
        assert!(lost.retryable && !lost.uncertain());
        assert!(receipts(dir.path()).unwrap().iter().any(is_unresolved));
        // A billed service still stops at the unresolved request.
        assert!(send(&second, Resend::AskUser, true).is_err());
        // A free one carries on, and resends the unconfirmed request itself.
        assert!(send(&second, Resend::Free, true).is_ok());
        assert!(send(&first, Resend::Free, true).is_ok());
        assert!(
            receipts(dir.path())
                .unwrap()
                .iter()
                .all(|r| !is_unresolved(r))
        );
    }

    #[test]
    fn uncertain_attempt_requires_exact_single_use_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("control.json");
        let ledger = Ledger::new(dir.path(), Some(&control), &Default::default()).unwrap();
        let payload = serde_json::json!({"model":"test","audio":"hash"});
        let calls = AtomicUsize::new(0);
        let fail = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(NetworkFailure {
                message: "lost".into(),
                definitely_unsent: false,
            })
        };
        assert!(
            ledger
                .send(
                    "asr",
                    "transcription",
                    "https://example.test",
                    &payload,
                    fail,
                    |_| Ok(())
                )
                .unwrap_err()
                .uncertain()
        );
        let first = receipts(dir.path()).unwrap()[0].request_id.clone();
        assert!(
            ledger
                .send(
                    "asr",
                    "transcription",
                    "https://example.test",
                    &payload,
                    fail,
                    |_| Ok(())
                )
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        atomic_write(
            &control,
            &serde_json::to_vec(&Control {
                resend: vec![first],
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            ledger
                .send(
                    "asr",
                    "transcription",
                    "https://example.test",
                    &payload,
                    fail,
                    |_| Ok(())
                )
                .is_err()
        );
        assert!(
            ledger
                .send(
                    "asr",
                    "transcription",
                    "https://example.test",
                    &payload,
                    fail,
                    |_| Ok(())
                )
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let second = receipts(dir.path()).unwrap()[0].request_id.clone();
        atomic_write(
            &control,
            &serde_json::to_vec(&Control {
                resend: vec![second],
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        let result = ledger
            .send(
                "asr",
                "transcription",
                "https://example.test",
                &payload,
                success,
                |_| Ok(()),
            )
            .unwrap();
        assert!(!result.replayed);
        let cached = ledger
            .send(
                "asr",
                "transcription",
                "https://example.test",
                &payload,
                || panic!("completed request must not send again"),
                |_| Ok(()),
            )
            .unwrap();
        // The replay is the saved result, marked as not received now.
        assert!(cached.replayed);
        assert_eq!(cached.value, result.value);
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
                Ok(HttpResponse {
                    status: 200,
                    body: br#"{"error":"invalid"}"#.to_vec(),
                })
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
    fn paused_or_revoked_service_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.json");
        let versions = BTreeMap::from([("llm".into(), "service-v1".into())]);
        let ledger = Ledger::new(dir.path(), Some(&path), &versions).unwrap();
        for control in [
            Control {
                intent: Intent::Pause,
                ..Default::default()
            },
            Control {
                stopped_services: vec!["service-v1".into()],
                ..Default::default()
            },
        ] {
            atomic_write(&path, &serde_json::to_vec(&control).unwrap()).unwrap();
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
}

#[cfg(test)]
mod ephemeral_tests {
    use super::*;

    fn request(status: u16, body: &[u8], resend: Resend) -> std::result::Result<Reply, Failure> {
        let body = body.to_vec();
        json_request_described(
            "llm",
            "summary",
            "summary",
            "https://example.test",
            &serde_json::json!({"n":1}),
            resend,
            move || Ok(HttpResponse { status, body }),
            |_| Ok(()),
        )
    }

    #[test]
    fn requests_outside_a_task_are_classified_by_the_same_rules() {
        assert!(active().is_none());
        assert_eq!(
            request(200, br#"{"ok":true}"#, Resend::AskUser)
                .unwrap()
                .value["ok"],
            true
        );
        let rejected = request(401, b"{}", Resend::AskUser).unwrap_err();
        assert_eq!(rejected.kind, crate::artifact::FailureKind::Authentication);
        assert!(!rejected.retryable && !rejected.uncertain());
        assert!(request(429, b"{}", Resend::AskUser).unwrap_err().retryable);
        let unknown = request(503, b"", Resend::AskUser).unwrap_err();
        assert!(unknown.uncertain() && !unknown.retryable);
        // Nothing persists between calls: the same payload sends again.
        assert!(request(200, br#"{"ok":true}"#, Resend::AskUser).is_ok());
        let lost = json_request_described(
            "llm",
            "summary",
            "summary",
            "https://example.test",
            &serde_json::json!({"n":2}),
            Resend::Free,
            || {
                Err(NetworkFailure {
                    message: "stalled".into(),
                    definitely_unsent: false,
                })
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(lost.retryable && !lost.uncertain());
    }
}
