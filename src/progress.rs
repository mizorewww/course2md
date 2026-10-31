//! 机器可读进度协议（--json 模式）：stdout 每行一个 NDJSON 事件，供 GUI/脚本消费。
//!
//! [`Event`] 是协议的唯一定义：引擎只通过 [`emit`] 发出它，桌面端用同一类型
//! 反序列化。字段名即线上格式（`--json` / `run-task` 的消费者依赖它们），改动
//! 必须同时考虑外部脚本。
//!
//! human 模式下 [`emit`]/[`stage`] 全是 no-op，[`Bar`] 退化为普通可见进度条，
//! 保证现有终端输出零变化。

use crate::tr;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Pipeline stages in pipeline order; the derived `Ord` is the display order.
/// Serialized as the stage name (`"scenes/scan"`, `"model/<file>"`).
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    strum::IntoStaticStr,
    strum::EnumString,
)]
#[serde(into = "String", from = "String")]
#[strum(serialize_all = "lowercase")]
pub enum Stage {
    Fetch,
    Subtitle,
    Download,
    /// Old workers reported scanning and extraction under this one key; it only
    /// survives in saved task records.
    Scenes,
    #[strum(serialize = "scenes/scan")]
    ScenesScan,
    #[strum(serialize = "scenes/extract")]
    ScenesExtract,
    Audio,
    #[strum(serialize = "model/prepare")]
    ModelPrepare,
    /// One model file download, named by its file.
    #[strum(disabled)]
    ModelFile(String),
    #[strum(serialize = "model/load")]
    ModelLoad,
    #[strum(serialize = "model/apple")]
    ModelApple,
    Transcribe,
    Llm,
    Translate,
    Summary,
    Render,
    #[strum(serialize = "exports", serialize = "export")]
    Exports,
    /// A name this build does not know (a newer or older engine).
    #[strum(disabled)]
    Other(String),
}

impl Stage {
    pub fn name(&self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::ModelFile(file) => format!("model/{file}").into(),
            Self::Other(name) => name.clone().into(),
            known => <&'static str>::from(known).into(),
        }
    }
    pub fn parse(name: &str) -> Self {
        name.parse()
            .unwrap_or_else(|_| match name.strip_prefix("model/") {
                Some(file) if !file.is_empty() => Self::ModelFile(file.to_owned()),
                _ => Self::Other(name.to_owned()),
            })
    }
    /// Stages that move model files or video bytes rather than count items.
    pub fn is_model_transfer(&self) -> bool {
        matches!(
            self,
            Self::ModelPrepare | Self::ModelFile(_) | Self::ModelLoad | Self::ModelApple
        )
    }
    /// AI requests have no measurable fraction.
    pub fn is_ai(&self) -> bool {
        matches!(self, Self::Llm | Self::Translate | Self::Summary)
    }
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}
impl From<Stage> for String {
    fn from(stage: Stage) -> Self {
        stage.name().into_owned()
    }
}
impl From<String> for Stage {
    fn from(name: String) -> Self {
        Self::parse(&name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Start,
    Done,
}

/// What a byte transfer is doing, for the GUI to word; `Load` is Apple model
/// preparation that is not downloading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Connect,
    Retry,
    Download,
    Verify,
    Load,
}

/// What `current` and `total` count when the stage alone does not say.
/// Apple model preparation reports bytes while downloading and a weighted
/// fraction (out of 10000) otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Bytes,
}

/// Why a task stopped at a safe boundary instead of sending the next request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    Paused,
    Cancelled,
    Quit,
    ServiceStopped,
    /// A request was sent but its result is unknown; resending needs consent.
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelOperation {
    Prepare,
    Download,
}

/// Why a command failed, for scripts that must not parse the message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The command line or task input was rejected before any work started.
    InvalidArguments,
    /// The configuration file could not be used as written.
    InvalidConfiguration,
    /// A required program (ffmpeg, ffprobe, yt-dlp, llama-server) is missing.
    MissingTool,
    /// A service rejected the credentials.
    Authentication,
    /// A service does not have the endpoint or model asked for.
    Model,
    /// A service rejected the request parameters.
    Parameters,
    RateLimit,
    /// A service failed to complete the request.
    Service,
    /// A request was sent but its result is unknown.
    Uncertain,
    #[default]
    Other,
}

impl ErrorCode {
    /// The code a typed error carries; untyped errors are `Other`.
    pub fn of(error: &anyhow::Error) -> Self {
        use crate::outcome::FailureKind;
        if error.downcast_ref::<crate::error::MissingTool>().is_some() {
            return Self::MissingTool;
        }
        if error
            .downcast_ref::<crate::invalid::InvalidInput>()
            .is_some()
        {
            return Self::InvalidArguments;
        }
        if error
            .downcast_ref::<crate::invalid::InvalidConfiguration>()
            .is_some()
        {
            return Self::InvalidConfiguration;
        }
        match error
            .downcast_ref::<crate::dispatch::Failure>()
            .map(|failure| failure.kind)
        {
            Some(FailureKind::Authentication) => Self::Authentication,
            Some(FailureKind::Model) => Self::Model,
            Some(FailureKind::Parameters) => Self::Parameters,
            Some(FailureKind::RateLimit) => Self::RateLimit,
            Some(FailureKind::Service) => Self::Service,
            Some(FailureKind::Uncertain) => Self::Uncertain,
            _ => Self::Other,
        }
    }
}

/// The published note version a finished task reports.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Done {
    /// Set when the task only produced exports for an existing version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    pub task_id: String,
    pub course_id: String,
    pub version_id: String,
    pub out_dir: PathBuf,
    pub manifest: Option<PathBuf>,
    pub title: String,
    pub slides: usize,
    pub segments: usize,
    pub chars: usize,
    pub outputs: Vec<PathBuf>,
    pub partial: Option<bool>,
    pub outcomes: Option<crate::outcome::Outcomes>,
    pub elapsed_secs: f64,
}

/// One NDJSON line of the `--json` / `run-task` stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Log {
        level: LogLevel,
        message: String,
    },
    Stage {
        stage: Stage,
        status: StageStatus,
    },
    Progress {
        stage: Stage,
        current: u64,
        total: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<Phase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<Unit>,
    },
    /// Running LLM token totals for the stage.
    Tokens {
        stage: Stage,
        prompt: u64,
        completion: u64,
    },
    /// How many requests the stage runs concurrently.
    Workers {
        stage: Stage,
        workers: usize,
    },
    Blocked {
        reason: BlockReason,
        request_id: Option<String>,
        purpose: Option<String>,
        description: Option<String>,
        kind: crate::outcome::FailureKind,
        message: String,
    },
    Error {
        message: String,
        /// Stable for consumers; the message is for people and may change.
        #[serde(default)]
        code: ErrorCode,
    },
    Done(Box<Done>),
    ModelReady {
        operation: ModelOperation,
        dir: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<crate::config::AsrProvider>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<Box<crate::model_state::LocalModelStatus>>,
    },
}

static QUIET_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET_MODE.store(quiet, Ordering::Relaxed);
}
pub fn is_quiet() -> bool {
    QUIET_MODE.load(Ordering::Relaxed)
}

static JSON_MODE: AtomicBool = AtomicBool::new(false);

/// 进入 NDJSON 事件流模式（进程级，main 在解析完 CLI 后调用一次）。
pub fn set_json_mode() {
    JSON_MODE.store(true, Ordering::Relaxed);
}

pub fn is_json() -> bool {
    JSON_MODE.load(Ordering::Relaxed)
}

/// 输出一个事件：json 模式写 stdout 一行并立即 flush；human 模式 no-op。
pub fn emit(event: &Event) {
    if !is_json() {
        return;
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = emit_to(&mut lock, event);
}

fn emit_to(mut w: impl std::io::Write, event: &Event) -> std::io::Result<()> {
    serde_json::to_writer(&mut w, event)?;
    writeln!(w)?;
    w.flush()
}

pub fn start(stage: Stage) {
    self::stage(stage, StageStatus::Start);
}
pub fn done(stage: Stage) {
    self::stage(stage, StageStatus::Done);
}

/// 阶段边界事件（json 模式才发）。新阶段开始时清零 token 累计，
/// 保证 GUI 看到的是当前阶段的实时用量。
pub fn stage(stage: Stage, status: StageStatus) {
    if status == StageStatus::Start {
        reset_tokens();
    }
    if is_json() {
        emit(&Event::Stage { stage, status });
        return;
    }
    if is_quiet() || status != StageStatus::Start {
        return;
    }
    let label = match stage {
        Stage::ModelLoad | Stage::ModelApple => {
            tr!(
                "加载识别模型（首次可能需要下载）/ Loading speech model (first use may download files)"
            )
        }
        Stage::Fetch => tr!("读取视频信息 / Reading video information"),
        Stage::Subtitle => tr!("查找字幕 / Looking for subtitles"),
        Stage::Download => tr!("下载视频 / Downloading video"),
        Stage::ScenesScan => tr!("扫描画面 / Scanning video frames"),
        Stage::ScenesExtract => tr!("生成截图 / Saving screenshots"),
        Stage::Audio => tr!("提取音频 / Extracting audio"),
        Stage::Transcribe => tr!("识别语音 / Transcribing speech"),
        Stage::Llm => tr!("校对文字 / Proofreading transcript"),
        Stage::Translate => tr!("翻译文字 / Translating transcript"),
        Stage::Summary => tr!("生成摘要 / Generating summary"),
        Stage::Render => tr!("生成笔记 / Writing notes"),
        Stage::ModelPrepare => tr!("检查识别模型 / Checking speech model"),
        Stage::ModelFile(_) => tr!("下载识别模型 / Downloading speech model"),
        Stage::Scenes | Stage::Exports | Stage::Other(_) => return,
    };
    eprintln!("{label}…");
}

/// LLM token 累计（当前阶段）：并发 worker 各自完成请求后累加，
/// GUI 展示的是单调递增的实时用量。阶段开始时由 [`stage`] 清零。
static TOKENS: Mutex<(u64, u64)> = Mutex::new((0, 0));

fn reset_tokens() {
    *crate::runtime::lock_recover(&TOKENS) = (0, 0);
}

/// 记录一次 LLM 响应的 token 用量并发出累计值（json 模式才有事件）。
pub fn note_tokens(stage: Stage, prompt: u64, completion: u64) {
    if prompt == 0 && completion == 0 {
        return;
    }
    let mut guard = crate::runtime::lock_recover(&TOKENS);
    guard.0 += prompt;
    guard.1 += completion;
    emit(&Event::Tokens {
        stage,
        prompt: guard.0,
        completion: guard.1,
    });
}

/// 视频下载字节进度：yt-dlp 每次刷一行，这里限流后发给 GUI。
pub fn download_progress(current: u64, total: u64) {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut last = crate::runtime::lock_recover(&LAST);
    if current != total && last.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(200))
    {
        return;
    }
    *last = Some(std::time::Instant::now());
    drop(last);
    emit(&Event::Progress {
        stage: Stage::Download,
        current,
        total,
        message: None,
        phase: None,
        unit: Some(Unit::Bytes),
    });
}

/// The files a multi-repository download will fetch, in transfer order, and
/// how much of each was on disk before it started. Planned by the Apple
/// downloader, which reports only a byte-weighted fraction per repository.
#[derive(Clone, Debug, Deserialize)]
pub struct TransferPlan {
    /// The host the bytes come from.
    pub host: String,
    pub repos: Vec<RepoTransfer>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RepoTransfer {
    pub repo: String,
    /// `[size, bytes already on disk]` per file.
    pub files: Vec<(u64, u64)>,
}

/// Turns per-repository fractions into bytes for the whole model.
///
/// The library counts a file it finds complete, or the staged chunks of one it
/// resumes, as soon as it reaches it. Counted that way, cached bytes would
/// arrive mid-transfer and read as speed. Here they are counted once, up front,
/// so the counter only grows by bytes that crossed the network.
pub struct TransferLedger {
    plan: TransferPlan,
    reported: u64,
}

impl TransferLedger {
    pub fn new(plan: TransferPlan) -> Self {
        let mut ledger = Self { plan, reported: 0 };
        ledger.reported = ledger.present();
        ledger
    }
    pub fn host(&self) -> &str {
        &self.plan.host
    }
    pub fn total(&self) -> u64 {
        self.plan.repos.iter().map(repo_size).sum()
    }
    /// Bytes on disk as last reported.
    pub fn reported(&self) -> u64 {
        self.reported
    }
    /// Bytes on disk before the transfer started.
    pub fn present(&self) -> u64 {
        self.plan.repos.iter().map(repo_present).sum()
    }
    /// Bytes on disk once repository `repo` has reported `fraction`. Never
    /// decreases: a retry re-walks files this run already fetched.
    pub fn advance(&mut self, repo: usize, fraction: f64) -> u64 {
        let Some(files) = self.plan.repos.get(repo).map(|r| &r.files) else {
            return self.reported;
        };
        let size: u64 = files.iter().map(|(size, _)| size).sum();
        let fraction = if fraction.is_finite() {
            fraction.clamp(0., 1.)
        } else {
            0.
        };
        let reached = (fraction * size as f64).round() as u64;
        // Bytes the library has passed that were already on disk.
        let mut start = 0;
        let mut passed_present = 0;
        for &(size, present) in files {
            let into = reached.saturating_sub(start).min(size);
            passed_present += into.min(present.min(size));
            start += size;
        }
        let earlier: u64 = self.plan.repos[..repo]
            .iter()
            .map(|r| repo_size(r) - repo_present(r))
            .sum();
        let value = self.present() + earlier + reached.saturating_sub(passed_present);
        self.reported = self.reported.max(value.min(self.total()));
        self.reported
    }
}

#[cfg_attr(not(apple_native), allow(dead_code))]
fn repo_size(repo: &RepoTransfer) -> u64 {
    repo.files.iter().map(|(size, _)| size).sum()
}

#[cfg_attr(not(apple_native), allow(dead_code))]
fn repo_present(repo: &RepoTransfer) -> u64 {
    repo.files
        .iter()
        .map(|&(size, present)| present.min(size))
        .sum()
}

/// 共享进度条样式：模板均为静态字符串，解析失败是编程错误。
fn progress_style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template)
        .expect(tr!("静态进度条模板"))
        .progress_chars("##-")
}

/// 进度条包装：human 模式 = 可见 indicatif 进度条（样式与原来一致）；
/// json 模式 = 隐藏 bar，inc/set_position/set_message 时发 progress 事件。
pub struct Bar {
    stage: Stage,
    bar: ProgressBar,
    current: AtomicU64,
    total: u64,
    message: Mutex<String>,
    phase: Mutex<Option<Phase>>,
    unit: Option<Unit>,
    last_emit: Mutex<std::time::Instant>,
}

impl Bar {
    pub fn new(stage: Stage, len: u64) -> Self {
        let bar = if is_json() || is_quiet() {
            ProgressBar::hidden()
        } else {
            ProgressBar::new(len)
        };
        Self {
            stage,
            bar,
            current: AtomicU64::new(0),
            total: len,
            message: Mutex::new(String::new()),
            phase: Mutex::new(None),
            unit: None,
            last_emit: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        }
    }

    /// 应用 human 模式的进度条模板（json 模式下 bar 隐藏，跳过）。
    pub fn with_template(self, template: &str) -> Self {
        if !is_json() {
            self.bar.set_style(progress_style(template));
        }
        self
    }

    /// Names what the counter counts in progress events.
    pub fn with_unit(mut self, unit: Unit) -> Self {
        self.unit = Some(unit);
        self
    }

    /// Starts the counter at `pos` without an event, so the first sample
    /// already includes work done before this run and is not read as speed.
    pub fn with_position(self, pos: u64) -> Self {
        self.resume_at(pos);
        self
    }

    /// Moves the counter to `pos` without an event, as work done before this
    /// transfer: a resumed partial file. The terminal bar's estimate restarts
    /// there, and the next event (a phase report) carries it to the desktop,
    /// which starts its speed sample on entering the download phase.
    pub fn resume_at(&self, pos: u64) {
        self.bar.set_position(pos);
        self.bar.reset_eta();
        self.current.store(pos, Ordering::Relaxed);
    }

    pub fn inc(&self, n: u64) {
        self.bar.inc(n);
        if is_json() {
            let cur = self.current.fetch_add(n, Ordering::Relaxed) + n;
            self.emit_progress(cur, false);
        }
    }

    pub fn set_position(&self, pos: u64) {
        self.move_to(pos, false);
    }

    /// Like [`Self::set_position`] but never throttled, for producers that
    /// already report coarsely: a dropped sample before a quiet spell would
    /// leave the count stale until the next one.
    pub fn report_position(&self, pos: u64) {
        self.move_to(pos, true);
    }

    fn move_to(&self, pos: u64, force: bool) {
        self.bar.set_position(pos);
        if is_json() {
            self.current.store(pos, Ordering::Relaxed);
            self.emit_progress(pos, force);
        }
    }

    pub fn set_message(&self, msg: impl Into<String>) {
        let msg = msg.into();
        if is_json() {
            *crate::runtime::lock_recover(&self.message) = msg;
            let cur = self.current.load(Ordering::Relaxed);
            self.emit_progress(cur, false);
        } else {
            self.bar.set_message(msg);
        }
    }

    /// Structured phase and its data (json mode); the GUI words it. Human mode
    /// shows `human` as the bar message instead.
    pub fn set_phase(&self, phase: Phase, data: &str, human: impl Into<String>) {
        if is_json() {
            *crate::runtime::lock_recover(&self.phase) = Some(phase);
            *crate::runtime::lock_recover(&self.message) = data.to_string();
            self.emit_progress(self.current.load(Ordering::Relaxed), true);
        } else {
            self.bar.set_message(human.into());
        }
    }

    pub fn finish(&self) {
        if is_json() {
            self.emit_progress(self.current.load(Ordering::Relaxed), true);
        }
        self.bar.finish_and_clear();
    }

    fn emit_progress(&self, current: u64, force: bool) {
        let mut last = crate::runtime::lock_recover(&self.last_emit);
        if !force
            && current != 0
            && current != self.total
            && last.elapsed() < std::time::Duration::from_millis(100)
        {
            return;
        }
        *last = std::time::Instant::now();
        let message = crate::runtime::lock_recover(&self.message).clone();
        let phase = *crate::runtime::lock_recover(&self.phase);
        emit(&Event::Progress {
            stage: self.stage.clone(),
            current,
            total: self.total,
            message: Some(message).filter(|m| !m.is_empty()),
            phase,
            unit: self.unit,
        });
    }
}

/// tracing Layer：把日志事件格式化为 `{"type":"log"}` NDJSON 行写 stdout。
/// 仅在 json 模式注册（main.rs），level 映射 error/warn/info/debug（trace 归 debug）。
pub struct JsonLogLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for JsonLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => LogLevel::Error,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::INFO => LogLevel::Info,
            _ => LogLevel::Debug,
        };
        emit(&Event::Log {
            level,
            message: visitor.into_message(),
        });
    }
}

/// 提取事件 message；没有 message 字段时把其余 fields 拼成 `k=v` 文本。
#[derive(Default)]
struct LogVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl LogVisitor {
    fn push(&mut self, field: &tracing::field::Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }

    fn into_message(self) -> String {
        self.message.unwrap_or_else(|| self.fields.join(" "))
    }
}

impl tracing::field::Visit for LogVisitor {
    // `%`（Display）与 `?`（Debug）值、数值都经 record_debug：fmt::Arguments 的
    // Debug 输出即格式化文本本身，不带引号。
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.push(field, value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visitor_message_fallback_to_fields() {
        let v = LogVisitor {
            message: Some("hello".into()),
            fields: vec!["k=v".into()],
        };
        assert_eq!(v.into_message(), "hello");

        let mut v = LogVisitor::default();
        v.fields.push("a=1".into());
        v.fields.push("b=2".into());
        assert_eq!(v.into_message(), "a=1 b=2");

        assert_eq!(LogVisitor::default().into_message(), "");
    }

    #[test]
    fn emit_to_writes_single_ndjson_line() {
        let mut buf: Vec<u8> = Vec::new();
        let ev = Event::Log {
            level: LogLevel::Info,
            message: "hi".into(),
        };
        emit_to(&mut buf, &ev).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.ends_with('\n'));
        assert_eq!(
            s.trim().parse::<serde_json::Value>().unwrap(),
            serde_json::json!({"type": "log", "level": "info", "message": "hi"})
        );
    }

    fn ledger(repos: &[&[(u64, u64)]]) -> TransferLedger {
        TransferLedger::new(TransferPlan {
            host: "huggingface.co".into(),
            repos: repos
                .iter()
                .enumerate()
                .map(|(index, files)| RepoTransfer {
                    repo: format!("r{index}"),
                    files: files.to_vec(),
                })
                .collect(),
        })
    }

    #[test]
    fn transfer_ledger_counts_whole_model_bytes_across_repositories() {
        let mut ledger = ledger(&[&[(100, 0), (300, 0)], &[(600, 0)]]);
        assert_eq!((ledger.present(), ledger.total()), (0, 1000));
        assert_eq!(ledger.advance(0, 0.25), 100);
        assert_eq!(ledger.advance(0, 1.0), 400);
        assert_eq!(ledger.advance(1, 0.0), 400);
        assert_eq!(ledger.advance(1, 0.5), 700);
        assert_eq!(ledger.advance(1, 1.0), 1000);
    }

    #[test]
    fn transfer_ledger_counts_cached_and_staged_bytes_before_the_transfer() {
        // r0: a complete file, then a fresh one. r1: a resumed file with 200
        // staged bytes, then a complete one.
        let mut ledger = ledger(&[&[(100, 100), (300, 0)], &[(400, 200), (200, 200)]]);
        assert_eq!(ledger.present(), 500);
        // Reaching the cached file adds nothing.
        assert_eq!(ledger.advance(0, 0.25), 500);
        assert_eq!(ledger.advance(0, 0.5), 600);
        assert_eq!(ledger.advance(0, 1.0), 800);
        // The staged chunks are reported first; they were already counted.
        assert_eq!(ledger.advance(1, 200. / 600.), 800);
        assert_eq!(ledger.advance(1, 300. / 600.), 900);
        assert_eq!(ledger.advance(1, 400. / 600.), 1000);
        // The trailing cached file arrives at once and adds nothing.
        assert_eq!(ledger.advance(1, 1.0), 1000);
        assert_eq!(ledger.total(), 1000);
    }

    #[test]
    fn transfer_ledger_counts_a_resumed_file_once_across_retries() {
        // One 800-byte file with 300 bytes staged by an earlier run.
        let mut ledger = ledger(&[&[(800, 300)]]);
        assert_eq!(ledger.present(), 300);
        // The first attempt reports the staged chunks, then lands 200 bytes.
        assert_eq!(ledger.advance(0, 300. / 800.), 300);
        assert_eq!(ledger.advance(0, 500. / 800.), 500);
        // A retry re-walks the file: the staged and the newly landed chunks
        // come back at once, and are neither counted again nor as new bytes.
        assert_eq!(ledger.advance(0, 0.0), 500);
        assert_eq!(ledger.advance(0, 500. / 800.), 500);
        // Only bytes past what this run already had advance the count.
        assert_eq!(ledger.advance(0, 600. / 800.), 600);
        assert_eq!(ledger.advance(0, 1.0), 800);
        // Bytes that crossed the network this session: 500 of the 800.
        assert_eq!(ledger.reported() - ledger.present(), 500);
    }

    #[test]
    fn transfer_ledger_never_runs_backwards_or_past_the_total() {
        let mut ledger = ledger(&[&[(100, 0), (300, 0)]]);
        assert_eq!(ledger.advance(0, 0.75), 300);
        // A retry restarts the library's count from zero.
        assert_eq!(ledger.advance(0, 0.0), 300);
        assert_eq!(ledger.advance(0, 0.5), 300);
        assert_eq!(ledger.advance(0, 2.0), 400);
        assert_eq!(ledger.advance(0, f64::NAN), 400);
        assert_eq!(ledger.advance(7, 1.0), 400);
        let event = Event::Progress {
            stage: Stage::ModelApple,
            current: 300,
            total: 400,
            message: Some(ledger.host().into()),
            phase: Some(Phase::Download),
            unit: Some(Unit::Bytes),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({"type": "progress", "stage": "model/apple", "current": 300,
                "total": 400, "message": "huggingface.co", "phase": "download", "unit": "bytes"})
        );
        let plan: TransferPlan = serde_json::from_str(
            r#"{"host":"hf-mirror.com","repos":[{"repo":"a/b","files":[[10,10],[5,0]]}]}"#,
        )
        .unwrap();
        assert_eq!(TransferLedger::new(plan).present(), 10);
    }

    #[test]
    fn stage_names_round_trip_and_keep_pipeline_order() {
        for name in [
            "fetch",
            "scenes/scan",
            "model/prepare",
            "model/Qwen3.gguf",
            "model/apple",
            "exports",
            "someday/new",
        ] {
            assert_eq!(Stage::parse(name).name(), name);
        }
        assert_eq!(Stage::parse("export"), Stage::Exports);
        assert!(Stage::Fetch < Stage::ModelFile("a".into()));
        assert!(Stage::ModelFile("a".into()) < Stage::Transcribe);
        assert!(Stage::Render < Stage::Other("x".into()));
        let json = serde_json::to_string(&Event::Stage {
            stage: Stage::ModelFile("m.gguf".into()),
            status: StageStatus::Done,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"type":"stage","stage":"model/m.gguf","status":"done"}"#
        );
        let event: Event = serde_json::from_str(
            r#"{"type":"blocked","reason":"service_stopped","request_id":null,"purpose":null,"description":null,"kind":"other","message":"m"}"#,
        )
        .unwrap();
        assert!(matches!(
            event,
            Event::Blocked {
                reason: BlockReason::ServiceStopped,
                ..
            }
        ));
    }
}
