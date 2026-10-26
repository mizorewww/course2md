//! Progress samples, independent of rendering. ETA excludes cached work and resets on retries.
use course2md::progress::{Phase, Stage, Unit};
use std::time::{Duration, Instant};

/// Silence after which a source that reports continuously has stopped.
const QUIET_AFTER: Duration = Duration::from_secs(30);
/// speech-swift abandons an Apple download attempt after five minutes without
/// a landed range (`HuggingFaceDownloader.downloadStallTimeoutSeconds`).
const BLOCK_STALL_AFTER: Duration = Duration::from_secs(300);

pub struct Activity {
    pub current: u64,
    pub total: u64,
    pub message: String,
    /// Structured phase from the engine (model transfers).
    pub phase: Option<Phase>,
    /// What the counter counts, when the stage alone does not say.
    pub unit: Option<Unit>,
    pub done: bool,
    pub workers: usize,
    /// Tokens the AI service reported for this stage, sent and received.
    prompt_tokens: u64,
    completion_tokens: u64,
    sampled: bool,
    /// When the stage began and ended, for its elapsed time; `started`
    /// restarts with each retry for the rate.
    created: Instant,
    finished: Option<Instant>,
    started: Instant,
    baseline: u64,
    updated: Instant,
}
impl Activity {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            current: 0,
            total: 0,
            message: String::new(),
            phase: None,
            unit: None,
            done: false,
            workers: 1,
            prompt_tokens: 0,
            completion_tokens: 0,
            sampled: false,
            created: now,
            finished: None,
            started: now,
            baseline: 0,
            updated: now,
        }
    }
    pub fn update(&mut self, current: u64, total: u64, message: Option<String>) {
        self.sampled = true;
        let now = Instant::now();
        if current < self.current || total != self.total {
            self.started = now;
            self.baseline = current;
        }
        if current != self.current {
            self.updated = now;
        }
        self.current = current;
        self.total = total;
        if let Some(message) = message {
            self.message = message;
        }
    }
    pub fn finish(&mut self) {
        self.done = true;
        self.finished.get_or_insert_with(Instant::now);
    }
    /// Adds the tokens of responses that arrived since the last report.
    pub fn note_tokens(&mut self, prompt: u64, completion: u64) {
        self.prompt_tokens += prompt;
        self.completion_tokens += completion;
    }
    pub fn elapsed(&self) -> Duration {
        self.finished.unwrap_or_else(Instant::now) - self.created
    }
    /// Unmeasured work shows that it is moving: how long it has run and, for
    /// AI requests, how much the service has sent back.
    pub fn live_counter(&self) -> String {
        let mut parts = vec![elapsed_label(self.elapsed())];
        if self.completion_tokens > 0 {
            parts.push(course2md::tr!(
                "↓ {} tokens",
                compact_count(self.completion_tokens)
            ));
        }
        parts.join(" · ")
    }
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.current as f32 / self.total as f32).clamp(0., 1.))
    }
    pub fn has_samples(&self) -> bool {
        self.sampled
    }
    /// Records the engine's phase. Entering a download starts a new speed
    /// sample at the current count: a transfer that resumes a partial file
    /// (or reconnects) may report the bytes already on disk only once it is
    /// under way, and those bytes did not cross the network in this session.
    pub fn set_phase(&mut self, phase: Option<Phase>) {
        if phase != self.phase {
            let now = Instant::now();
            if phase == Some(Phase::Download) {
                self.started = now;
                self.baseline = self.current;
            }
            self.updated = now;
        }
        self.phase = phase;
    }
    /// Units per second since the sample began, counting only work done in
    /// this sample. Between reports it decays toward zero as time passes.
    fn average_rate(&self) -> Option<f64> {
        let elapsed = self.started.elapsed().as_secs_f64();
        let processed = self.current.saturating_sub(self.baseline);
        (elapsed >= 1. && processed > 0).then(|| processed as f64 / elapsed)
    }
    /// The average rate while reports still arrive at their usual cadence.
    fn rate(&self) -> Option<f64> {
        self.average_rate()
            .filter(|_| self.updated.elapsed() < QUIET_AFTER)
    }
    pub fn transfer_metrics(&self, stage: &Stage, running: bool) -> TransferMetrics {
        if self.done {
            return TransferMetrics {
                note: Some(course2md::tr!("已完成").into()),
                ..TransferMetrics::default()
            };
        }
        if !running {
            return TransferMetrics {
                note: Some(course2md::tr!("已停止").into()),
                ..TransferMetrics::default()
            };
        }
        let byte_download = self.counts_bytes(stage);
        let quantity = quantity_of(stage, byte_download, self.current, self.total);
        // Apple loading and compilation report weighted stages, not throughput,
        // and do not advance at a constant rate. Its downloads report bytes.
        if *stage == Stage::ModelApple && !byte_download {
            return TransferMetrics {
                quantity,
                note: Some(course2md::tr!(
                    "已用 {}",
                    duration_label(self.started.elapsed().as_secs())
                )),
                ..TransferMetrics::default()
            };
        }
        // Model files name their host, and whether the engine is connecting,
        // resuming after a dropped connection or checking the finished file.
        let host = ((matches!(stage, Stage::ModelFile(_))
            || (*stage == Stage::ModelApple && byte_download))
            && !self.message.is_empty())
        .then_some(self.message.as_str());
        let source = host.map(|host| course2md::tr!("来源 {}", host));
        match (self.phase, host) {
            (Some(Phase::Connect), Some(host)) => {
                return TransferMetrics {
                    quantity,
                    note: Some(course2md::tr!("正在连接 {}", host)),
                    ..TransferMetrics::default()
                };
            }
            (Some(Phase::Retry), Some(host)) => {
                return TransferMetrics {
                    quantity,
                    note: Some(course2md::tr!("连接中断，正在重新连接 {}", host)),
                    ..TransferMetrics::default()
                };
            }
            (Some(Phase::Verify), _) => {
                return TransferMetrics {
                    note: Some(course2md::tr!("正在校验文件完整性").into()),
                    ..TransferMetrics::default()
                };
            }
            _ => {}
        }
        // The Apple downloader reports a whole 8 MiB range only once it has
        // landed, sixteen in flight, so on a slow link minutes can pass
        // between reports while data keeps flowing. Its silence is a stall
        // only once its own guard would abandon the attempt.
        let in_blocks = *stage == Stage::ModelApple && byte_download;
        let silent = self.updated.elapsed();
        let stall_after = if in_blocks {
            BLOCK_STALL_AFTER
        } else {
            QUIET_AFTER
        };
        if silent >= stall_after && self.current > 0 {
            return TransferMetrics {
                quantity,
                source,
                speed: byte_download.then(|| "—".into()),
                note: Some(
                    if matches!(stage, Stage::ScenesScan | Stage::ScenesExtract) {
                        course2md::tr!("仍在处理").into()
                    } else if byte_download {
                        course2md::tr!("已 {}没有收到新数据", duration_label(silent.as_secs()))
                    } else {
                        course2md::tr!("等待响应").into()
                    },
                ),
                ..TransferMetrics::default()
            };
        }
        // It also fetches small configuration files first, so its early
        // samples miss most of the bytes already received. Until 5% of the
        // model has landed in this run, the speed is still being measured.
        let settled = !in_blocks || self.current.saturating_sub(self.baseline) * 20 >= self.total;
        // Between its blocks the average keeps falling, which is what a slow
        // link looks like; the estimate follows it.
        let rate = if in_blocks {
            self.average_rate()
        } else {
            self.rate()
        }
        .filter(|_| settled);
        let speed = rate
            .filter(|_| byte_download)
            .map(|rate| format!("{}/s", throughput(rate)));
        let eta = if self.total > self.current {
            match rate {
                Some(rate) => Some(TransferEta::Remaining(duration(
                    (self.total - self.current) as f64 / rate,
                ))),
                None if byte_download => {
                    Some(TransferEta::Remaining(course2md::tr!("计算中").into()))
                }
                None => None,
            }
        } else if self.total > 0 {
            Some(TransferEta::Note(course2md::tr!("收尾中…").into()))
        } else if !byte_download {
            Some(TransferEta::Note(course2md::tr!(
                "已用 {}",
                duration_label(self.started.elapsed().as_secs())
            )))
        } else {
            None
        };
        let note = if in_blocks && silent >= QUIET_AFTER {
            Some(course2md::tr!(
                "数据分块到达，上一块在 {}前",
                duration_label(silent.as_secs())
            ))
        } else {
            (byte_download && speed.is_none()).then(|| course2md::tr!("正在测量速度").into())
        };
        TransferMetrics {
            quantity,
            note,
            speed,
            eta,
            source,
        }
    }
    /// Whether `current` and `total` are bytes moving over the network.
    pub fn counts_bytes(&self, stage: &Stage) -> bool {
        is_byte_download(stage) || self.unit == Some(Unit::Bytes)
    }
    pub fn detail(&self, stage: &Stage, running: bool) -> String {
        self.transfer_metrics(stage, running).summary()
    }
}

/// A labeled 预计剩余 value, or a loose note shown without the label.
/// The producer knows which it is emitting; consumers must not re-infer it
/// from the wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEta {
    Remaining(String),
    Note(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferMetrics {
    pub quantity: String,
    pub speed: Option<String>,
    pub eta: Option<TransferEta>,
    pub note: Option<String>,
    /// Where the bytes come from, for model files.
    pub source: Option<String>,
}

impl TransferMetrics {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.quantity.is_empty() {
            parts.push(self.quantity.clone());
        }
        if let Some(speed) = &self.speed {
            parts.push(course2md::tr!("速度 {}", speed));
        }
        if let Some(eta) = &self.eta {
            parts.push(match eta {
                TransferEta::Remaining(value) => course2md::tr!("预计剩余 {}", value),
                TransferEta::Note(value) => value.clone(),
            });
        }
        if let Some(note) = &self.note {
            parts.push(note.clone());
        }
        if let Some(source) = &self.source {
            parts.push(source.clone());
        }
        parts.join(" · ")
    }
}

/// "用时 45 秒", "用时 1 分 23 秒": how long work has run, in the interface
/// language.
pub fn elapsed_label(elapsed: Duration) -> String {
    course2md::tr!("用时 {}", duration_label(elapsed.as_secs()))
}

/// The one way the desktop words a span of time, for elapsed clocks and
/// remaining-time estimates alike: "16 秒", "1 分 05 秒", "1 小时 02 分"
/// ("16s", "1m 05s", "1h 02m" in English). Within a larger unit the smaller
/// one keeps two digits, so a ticking clock does not change width.
pub fn duration_label(seconds: u64) -> String {
    if seconds >= 3600 {
        course2md::tr!(
            "{} 小时 {} 分",
            seconds / 3600,
            format!("{:02}", seconds % 3600 / 60)
        )
    } else if seconds >= 60 {
        course2md::tr!("{} 分 {} 秒", seconds / 60, format!("{:02}", seconds % 60))
    } else {
        course2md::tr!("{} 秒", seconds)
    }
}

/// "950", "12.3k", "1.2M".
fn compact_count(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{:.1}k", count as f64 / 1e3),
        _ => format!("{:.1}M", count as f64 / 1e6),
    }
}

fn is_byte_download(stage: &Stage) -> bool {
    matches!(stage, Stage::Download | Stage::ModelFile(_))
}

pub fn quantity(stage: &Stage, current: u64, total: u64) -> String {
    quantity_of(stage, is_byte_download(stage), current, total)
}

fn quantity_of(stage: &Stage, byte_download: bool, current: u64, total: u64) -> String {
    if *stage == Stage::ScenesScan {
        course2md::tr!("已检查 {} 张画面", current)
    } else if *stage == Stage::ScenesExtract && total > 0 {
        course2md::tr!("已保存 {} / {} 张截图", current, total)
    } else if byte_download {
        if total > 0 {
            format!("{} / {}", bytes(current), bytes(total))
        } else {
            bytes(current)
        }
    } else if total > 0 {
        if *stage == Stage::Transcribe {
            course2md::tr!("{} / {} 段", current, total)
        } else {
            format!(
                "{:.0}%",
                (current as f64 / total as f64).clamp(0., 1.) * 100.
            )
        }
    } else {
        String::new()
    }
}

/// The one term naming what is missing.
fn missing_term(label: &str) -> String {
    match label {
        "截图" | "摘要" => course2md::tr!("{}未生成", label),
        _ => course2md::tr!("{}未完成", label),
    }
}

/// Why a component of a saved note is missing. A classified failure words its
/// own cause. Local and unclassified failures carry their cause only as text,
/// and records written before the engine classified failures have no kind;
/// those fall back to [`legacy_outcome_message`].
///
/// One term names what is missing; a cause follows only when it says more than
/// the term (the retry button beside it already names the next step).
pub fn outcome_failure_message(label: &str, outcome: &course2md::artifact::Outcome) -> String {
    use course2md::artifact::FailureKind;
    let cause = match outcome.kind {
        Some(FailureKind::Authentication) => course2md::tr!("服务拒绝了 API Key"),
        Some(FailureKind::Model) => course2md::tr!("服务地址或模型名称有误"),
        Some(FailureKind::Parameters) => course2md::tr!("服务不支持当前的模型设置"),
        Some(FailureKind::RateLimit) => course2md::tr!("服务暂时限制了请求次数"),
        Some(FailureKind::Service) => course2md::tr!("服务暂时无法完成请求"),
        Some(FailureKind::Uncertain) => course2md::tr!("请求结果未确认"),
        Some(FailureKind::Local | FailureKind::Other) | None => {
            return legacy_outcome_message(label, outcome.message.as_deref());
        }
    };
    course2md::tr!("{}：{}", missing_term(label), cause)
}

/// Older saved outcomes combined a stage label, retention message and English
/// diagnostics as one bilingual text. This reads them by matching that text;
/// delete it once saved records carry a failure kind and a plain cause.
pub fn legacy_outcome_message(label: &str, message: Option<&str>) -> String {
    let term = missing_term(label);
    let chinese = message
        .unwrap_or_default()
        .split(" / ")
        .next()
        .unwrap_or_default()
        .trim();
    let mut cause = chinese;
    for prefix in [
        course2md::tr!("{}尚未完成", label),
        course2md::tr!("{}未完成", label),
        term.clone(),
    ] {
        if let Some(rest) = cause.strip_prefix(prefix.as_str()) {
            cause = rest;
            break;
        }
    }
    let cause = cause.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '：' | ':' | '，' | ',' | '；' | ';' | '。')
    });
    let restates = cause.is_empty()
        || matches!(
            cause,
            "已保留正文"
                | "正文已保存"
                | "校对未全部完成，原文已保留"
                | "部分校对未完成，原始文字已保留"
                | "无法提取截图"
                | "没有提取到截图"
                | "未提取到截图"
        );
    if restates {
        term
    } else {
        course2md::tr!("{}：{}", term, cause)
    }
}

pub fn title(stage: &Stage) -> String {
    match stage {
        Stage::ModelApple => course2md::tr!("准备 Apple 识别模型"),
        Stage::ModelFile(file) if file.starts_with("mmproj") => {
            course2md::tr!("下载音频编码器")
        }
        Stage::ModelFile(_) => course2md::tr!("下载语音模型"),
        Stage::ModelPrepare => course2md::tr!("检查识别模型"),
        Stage::ModelLoad => course2md::tr!("加载识别模型"),
        Stage::Fetch => course2md::tr!("读取课程"),
        Stage::Download => course2md::tr!("下载视频"),
        Stage::Scenes => course2md::tr!("提取画面"),
        Stage::ScenesScan => course2md::tr!("扫描画面"),
        Stage::ScenesExtract => course2md::tr!("生成截图"),
        Stage::Audio => course2md::tr!("提取音频"),
        Stage::Transcribe => course2md::tr!("语音转写"),
        Stage::Subtitle => course2md::tr!("读取字幕"),
        Stage::Llm => course2md::tr!("AI 校对"),
        Stage::Translate => course2md::tr!("AI 翻译"),
        Stage::Summary => course2md::tr!("生成摘要"),
        Stage::Exports => course2md::tr!("导出文件"),
        Stage::Render => course2md::tr!("生成笔记"),
        Stage::Other(name) => return name.clone(),
    }
    .into()
}

/// 准备阶段文案：结构化 phase 来自引擎，缺失时退化为阶段标题。
pub(crate) fn model_transfer_phase(stage: &Stage, phase: Option<Phase>) -> String {
    // Only Apple preparation names its phase in the title; model files keep
    // their own title and word their phase in the detail.
    match phase {
        Some(Phase::Load) if *stage == Stage::ModelApple => course2md::tr!("加载识别模型").into(),
        Some(Phase::Download) if *stage == Stage::ModelApple => {
            course2md::tr!("下载识别模型").into()
        }
        _ => title(stage),
    }
}

pub(crate) fn bytes(value: u64) -> String {
    if value >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", value as f64 / (1024. * 1024. * 1024.))
    } else if value >= 1024 * 1024 {
        format!("{:.1} MB", value as f64 / (1024. * 1024.))
    } else if value >= 1024 {
        format!("{:.0} KB", value as f64 / 1024.)
    } else {
        course2md::tr!("{} 字节", value)
    }
}

fn throughput(bytes_per_sec: f64) -> String {
    // 复用 bytes 的阈值与格式化；sub-KB 保留原有的 "{:.0}" 舍入语义
    // （bytes 只接 u64，无法表达小数舍入）。
    let value = bytes_per_sec.max(0.);
    if value < 1024. {
        return course2md::tr!("{} 字节", format!("{value:.0}"));
    }
    bytes(value as u64)
}
/// A remaining time rounds up, so an estimate never reaches zero early.
fn duration(seconds: f64) -> String {
    duration_label(seconds.ceil() as u64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn counters_are_compact() {
        use super::{Duration, compact_count, elapsed_label};
        assert_eq!(compact_count(950), "950");
        assert_eq!(compact_count(12_345), "12.3k");
        assert_eq!(compact_count(1_250_000), "1.2M");
        assert_eq!(elapsed_label(Duration::from_secs(16)), "用时 16 秒");
        assert_eq!(elapsed_label(Duration::from_secs(65)), "用时 1 分 05 秒");
        assert_eq!(
            elapsed_label(Duration::from_secs(3720)),
            "用时 1 小时 02 分"
        );
    }

    /// Every language words a span with its own units; no Latin unit leaks
    /// into the Chinese or Japanese interface.
    #[test]
    fn durations_use_the_interface_language_units() {
        use course2md::i18n::{Lang, t_for};
        let words = |lang| {
            [
                "用时 {}",
                "已用 {}",
                "预计剩余 {}",
                "{} 秒",
                "{} 分 {} 秒",
                "{} 小时 {} 分",
            ]
            .map(|key| t_for(key, lang).to_owned())
        };
        assert_eq!(
            words(Lang::En),
            [
                "{} elapsed",
                "Elapsed {}",
                "{} remaining",
                "{}s",
                "{}m {}s",
                "{}h {}m"
            ]
        );
        assert_eq!(
            words(Lang::Ja),
            [
                "経過 {}",
                "経過 {}",
                "推定残り時間 {}",
                "{} 秒",
                "{} 分 {} 秒",
                "{} 時間 {} 分"
            ]
        );
        for lang in [Lang::ZhHans, Lang::ZhHant, Lang::Ja] {
            for text in words(lang) {
                assert!(
                    !text.chars().any(|c| c.is_ascii_alphabetic()),
                    "{lang:?}: {text}"
                );
            }
        }
    }

    use super::*;
    #[test]
    fn stage_boundaries_do_not_invent_download_samples() {
        let mut activity = Activity::new();
        assert!(!activity.has_samples());
        activity.update(0, 0, Some("connecting".into()));
        assert!(activity.has_samples());
        assert!(activity.fraction().is_none());
    }

    #[test]
    fn apple_preparation_reports_elapsed_time_without_byte_speed_or_eta() {
        let mut activity = Activity::new();
        activity.update(1000, 10000, None);
        activity.started = Instant::now() - Duration::from_secs(180);
        activity.update(1700, 10000, None);
        let detail = activity.detail(&Stage::parse("model/apple"), true);
        assert!(
            detail.contains("17%") && detail.contains("已用 3 分"),
            "{detail}"
        );
        assert!(
            !detail.contains("约剩") && !detail.contains("/s") && !detail.contains("MB"),
            "{detail}"
        );

        activity.updated = Instant::now() - Duration::from_secs(60);
        let waiting = activity.detail(&Stage::parse("model/apple"), true);
        assert!(
            waiting.contains("已用") && !waiting.contains("等待响应"),
            "{waiting}"
        );
        assert_eq!(
            activity.detail(&Stage::parse("model/apple"), false),
            "已停止"
        );
        activity.done = true;
        assert_eq!(
            activity.detail(&Stage::parse("model/apple"), true),
            "已完成"
        );
    }

    #[test]
    fn apple_downloads_report_bytes_speed_and_remaining_time() {
        let apple = Stage::parse("model/apple");
        let mut activity = Activity::new();
        // Cached files arrive in the first sample and are not throughput.
        activity.update(
            300 * 1024 * 1024,
            1600 * 1024 * 1024,
            Some("huggingface.co".into()),
        );
        activity.phase = Some(Phase::Download);
        activity.unit = Some(Unit::Bytes);
        let measuring = activity.transfer_metrics(&apple, true);
        assert_eq!(measuring.quantity, "300.0 MB / 1.56 GB");
        assert_eq!(measuring.note.as_deref(), Some("正在测量速度"));
        assert_eq!(measuring.eta, Some(TransferEta::Remaining("计算中".into())));

        // Configuration files and the first ranges are too little to time.
        activity.started = Instant::now() - Duration::from_secs(20);
        activity.update(340 * 1024 * 1024, 1600 * 1024 * 1024, None);
        let early = activity.transfer_metrics(&apple, true);
        assert!(early.speed.is_none(), "{early:?}");
        assert_eq!(early.eta, Some(TransferEta::Remaining("计算中".into())));

        activity.update(500 * 1024 * 1024, 1600 * 1024 * 1024, None);
        let metrics = activity.transfer_metrics(&apple, true);
        assert_eq!(metrics.quantity, "500.0 MB / 1.56 GB");
        assert_eq!(metrics.speed.as_deref(), Some("10.0 MB/s"));
        // 1100 MB left at 10 MB/s; the test's own runtime may add a second.
        assert!(
            matches!(&metrics.eta, Some(TransferEta::Remaining(v)) if v.starts_with("1 分 5")),
            "{metrics:?}"
        );
        assert_eq!(metrics.source.as_deref(), Some("来源 huggingface.co"));
        let summary = metrics.summary();
        assert!(
            summary.starts_with("500.0 MB / 1.56 GB · 速度 10.0 MB/s · 预计剩余 1 分 5")
                && summary.ends_with(" · 来源 huggingface.co"),
            "{summary}"
        );
        assert_eq!(model_transfer_phase(&apple, activity.phase), "下载识别模型");

        // Loading after the download reports a weighted fraction again.
        activity.update(2000, 10000, Some("正在加载本机模型…".into()));
        activity.phase = Some(Phase::Load);
        activity.unit = None;
        let loading = activity.detail(&apple, true);
        assert!(
            loading.contains("20%") && loading.contains("已用"),
            "{loading}"
        );
        assert!(!loading.contains("/s") && !loading.contains("预计剩余"));
    }

    #[test]
    fn byte_downloads_keep_measured_speed_and_remaining_time() {
        let mut activity = Activity::new();
        activity.update(0, 8 * 1024 * 1024, None);
        activity.started = Instant::now() - Duration::from_secs(10);
        activity.update(4 * 1024 * 1024, 8 * 1024 * 1024, None);
        let metrics = activity.transfer_metrics(&Stage::parse("model/model.gguf"), true);
        assert_eq!(metrics.quantity, "4.0 MB / 8.0 MB");
        assert!(
            metrics
                .speed
                .as_deref()
                .is_some_and(|speed| speed.ends_with(" KB/s")),
            "{metrics:?}"
        );
        assert!(
            matches!(&metrics.eta, Some(TransferEta::Remaining(value)) if value.ends_with("秒")),
            "{metrics:?}"
        );
        let detail = activity.detail(&Stage::parse("model/model.gguf"), true);
        assert!(detail.contains("4.0 MB / 8.0 MB"), "{detail}");
        assert!(
            detail.contains("速度 ") && detail.contains("预计剩余"),
            "{detail}"
        );
    }

    #[test]
    fn model_download_metrics_stay_labeled_while_speed_is_measured() {
        let mut activity = Activity::new();
        activity.update(1_048_576, 8 * 1024 * 1024, None);
        let metrics = activity.transfer_metrics(&Stage::parse("model/model.gguf"), true);
        assert!(metrics.speed.is_none() && metrics.note.is_some());
        assert_eq!(metrics.eta, Some(TransferEta::Remaining("计算中".into())));
    }
    #[test]
    fn model_files_show_their_host_and_phase() {
        let mut activity = Activity::new();
        activity.update(0, 8 * 1024 * 1024, Some("modelscope.cn".into()));
        activity.phase = Some(Phase::Connect);
        let metrics = activity.transfer_metrics(&Stage::parse("model/model.gguf"), true);
        assert_eq!(metrics.note.as_deref(), Some("正在连接 modelscope.cn"));
        assert!(metrics.speed.is_none() && metrics.eta.is_none());

        activity.phase = Some(Phase::Download);
        activity.started = Instant::now() - Duration::from_secs(10);
        activity.update(4 * 1024 * 1024, 8 * 1024 * 1024, None);
        let metrics = activity.transfer_metrics(&Stage::parse("model/model.gguf"), true);
        assert_eq!(metrics.source.as_deref(), Some("来源 modelscope.cn"));
        assert!(metrics.speed.is_some(), "{metrics:?}");

        activity.phase = Some(Phase::Retry);
        let detail = activity.detail(&Stage::parse("model/model.gguf"), true);
        assert!(detail.contains("正在重新连接 modelscope.cn"), "{detail}");

        activity.phase = Some(Phase::Verify);
        let metrics = activity.transfer_metrics(&Stage::parse("model/model.gguf"), true);
        assert_eq!(metrics.note.as_deref(), Some("正在校验文件完整性"));
        assert!(metrics.quantity.is_empty());
        assert_eq!(
            model_transfer_phase(&Stage::parse("model/model.gguf"), Some(Phase::Download)),
            "下载语音模型"
        );
        assert_eq!(
            model_transfer_phase(&Stage::parse("model/apple"), Some(Phase::Download)),
            "下载识别模型"
        );
    }

    #[test]
    fn completed_scan_cannot_supply_the_new_extraction_counter_or_eta() {
        let mut scan = Activity::new();
        scan.update(18, 0, Some("已找到 1 张候选截图".into()));
        assert!(scan.fraction().is_none());
        assert!(
            scan.detail(&Stage::parse("scenes/scan"), true)
                .contains("已检查 18 张画面")
        );
        scan.done = true;
        let mut extract = Activity::new();
        extract.update(0, 1, None);
        extract.started = Instant::now() - Duration::from_secs(25);
        let detail = extract.detail(&Stage::parse("scenes/extract"), true);
        assert!(detail.contains("已保存 0 / 1 张截图"), "{detail}");
        assert!(
            !detail.contains("100%") && !detail.contains("收尾") && !detail.contains("预计剩余")
        );
        assert_eq!(extract.fraction(), Some(0.));
        extract.update(1, 1, None);
        extract.done = true;
        assert_eq!(
            extract.detail(&Stage::parse("scenes/extract"), true),
            "已完成"
        );
        let mut persisted = crate::workspace::StageRecord {
            status: course2md::progress::StageStatus::Done,
            current: 18,
            total: 18,
            detail: Some("old sample".into()),
        };
        persisted.begin();
        assert_eq!((persisted.current, persisted.total), (0, 0));
        assert!(persisted.detail.is_none());
    }
    #[test]
    fn saved_component_failures_have_one_label_and_no_english_diagnostics() {
        assert_eq!(
            legacy_outcome_message(
                "摘要",
                Some("摘要尚未完成，已保留正文 / Summary incomplete; body retained")
            ),
            "摘要未生成"
        );
        assert_eq!(
            legacy_outcome_message("摘要", Some("服务拒绝了请求（HTTP 400）。")),
            "摘要未生成：服务拒绝了请求（HTTP 400）"
        );
        // 截图失败只说一次：旧记录里的「无法提取截图」与术语重复，不再作为原因
        assert_eq!(
            legacy_outcome_message(
                "截图",
                Some("无法提取截图 / Could not extract slide 2 (t=4.0s): ffmpeg")
            ),
            "截图未生成"
        );
        assert_eq!(
            legacy_outcome_message(
                "截图",
                Some("第 2 张截图（0:04）提取失败 / Could not extract slide 2")
            ),
            "截图未生成：第 2 张截图（0:04）提取失败"
        );
        assert_eq!(
            legacy_outcome_message("校对", Some("校对未全部完成，原文已保留")),
            "校对未完成"
        );
    }
    #[test]
    fn classified_failures_are_worded_from_their_kind() {
        use course2md::artifact::{FailureKind, Outcome};
        // The text is ignored when the kind already names the cause.
        let rejected = Outcome::failed_kind(
            "摘要尚未完成 / Summary incomplete: HTTP 401",
            FailureKind::Authentication,
        );
        assert_eq!(
            outcome_failure_message("摘要", &rejected),
            "摘要未生成：服务拒绝了 API Key"
        );
        let limited = Outcome::failed_kind("anything", FailureKind::RateLimit);
        assert_eq!(
            outcome_failure_message("校对", &limited),
            "校对未完成：服务暂时限制了请求次数"
        );
        // A local failure's cause exists only as its text.
        let local = Outcome::failed("第 2 张截图（0:04）提取失败 / Could not extract slide 2");
        assert_eq!(
            outcome_failure_message("截图", &local),
            "截图未生成：第 2 张截图（0:04）提取失败"
        );
        // Records from before classification have no kind.
        let legacy = Outcome {
            kind: None,
            ..Outcome::failed("摘要尚未完成，已保留正文 / Summary incomplete; body retained")
        };
        assert_eq!(outcome_failure_message("摘要", &legacy), "摘要未生成");
    }
    #[test]
    fn retry_and_cached_work_do_not_inflate_eta() {
        let mut item = Activity::new();
        item.update(60, 100, None); // Resumed work must not count as throughput.
        assert!(item.rate().is_none());
        item.started = Instant::now() - Duration::from_secs(10);
        item.update(80, 100, None);
        assert!((1.9..2.1).contains(&item.rate().unwrap()));
        item.update(5, 100, None); // A retry restarts the sample window.
        assert!(item.rate().is_none());
        item.started = Instant::now() - Duration::from_secs(60);
        item.updated = Instant::now() - Duration::from_secs(31);
        // A byte stream that reports continuously has stopped; it says for
        // how long instead of keeping a stale speed.
        let stalled = item.transfer_metrics(&Stage::parse("model/test"), true);
        assert_eq!(stalled.speed.as_deref(), Some("—"));
        assert_eq!(stalled.eta, None);
        assert_eq!(stalled.note.as_deref(), Some("已 31 秒没有收到新数据"));
        assert_eq!(stalled.quantity, "5 字节 / 100 字节");
        // Work that is not a byte transfer waits for its response.
        let mut proofreading = Activity::new();
        proofreading.update(1, 4, None);
        proofreading.updated = Instant::now() - Duration::from_secs(31);
        assert!(
            proofreading
                .detail(&Stage::Transcribe, true)
                .contains("等待响应")
        );
    }

    const MIB: u64 = 1024 * 1024;

    /// Speed counts only bytes that crossed the network in this session;
    /// the progress fraction still includes the bytes already on disk.
    #[test]
    fn resumed_downloads_time_only_this_sessions_bytes() {
        let file = Stage::parse("model/model.gguf");
        // The built-in client reports the partial file before it connects.
        let mut native = Activity::new();
        native.update(600 * MIB, 1000 * MIB, None);
        native.set_phase(Some(Phase::Connect));
        native.set_phase(Some(Phase::Download));
        native.started = Instant::now() - Duration::from_secs(10);
        native.update(700 * MIB, 1000 * MIB, None);
        let rate = native.rate().unwrap();
        assert!(
            (9.9 * MIB as f64..10.1 * MIB as f64).contains(&rate),
            "{rate}"
        );
        assert_eq!(native.fraction(), Some(0.7));

        // aria2 starts from zero and reports the resumed bytes only once it
        // runs; entering the download phase starts the sample there.
        let mut aria2 = Activity::new();
        aria2.update(0, 1000 * MIB, None);
        aria2.set_phase(Some(Phase::Connect));
        aria2.update(600 * MIB, 1000 * MIB, None);
        aria2.set_phase(Some(Phase::Download));
        assert!(aria2.rate().is_none());
        aria2.started = Instant::now() - Duration::from_secs(10);
        aria2.update(700 * MIB, 1000 * MIB, None);
        let metrics = aria2.transfer_metrics(&file, true);
        assert_eq!(metrics.speed.as_deref(), Some("10.0 MB/s"), "{metrics:?}");
        // 300 MB left at 10 MB/s, not at the 70 MB/s the resumed bytes imply.
        assert!(
            matches!(&metrics.eta, Some(TransferEta::Remaining(v)) if v == "30 秒" || v == "31 秒"),
            "{metrics:?}"
        );
        assert_eq!(aria2.fraction(), Some(0.7));

        // Repeated phase reports within the download keep the sample.
        aria2.set_phase(Some(Phase::Download));
        assert!(aria2.rate().is_some());
        // Reconnecting and resuming starts a new sample at the resumed count.
        aria2.set_phase(Some(Phase::Retry));
        aria2.set_phase(Some(Phase::Download));
        assert!(aria2.rate().is_none());
        assert_eq!(aria2.fraction(), Some(0.7));
    }

    /// The Apple downloader reports whole ranges once they land; a quiet
    /// spell between them is a slow link, not a stall.
    #[test]
    fn apple_downloads_between_blocks_stay_informative() {
        let apple = Stage::parse("model/apple");
        let mut activity = Activity::new();
        activity.update(200 * MIB, 1600 * MIB, Some("hf-mirror.com".into()));
        activity.set_phase(Some(Phase::Download));
        activity.unit = Some(Unit::Bytes);

        // The first sixteen ranges are still in flight after a minute.
        activity.started = Instant::now() - Duration::from_secs(60);
        activity.updated = Instant::now() - Duration::from_secs(60);
        let first = activity.transfer_metrics(&apple, true);
        assert_eq!(first.quantity, "200.0 MB / 1.56 GB");
        assert_eq!(first.speed, None);
        assert_eq!(first.eta, Some(TransferEta::Remaining("计算中".into())));
        assert_eq!(
            first.note.as_deref(),
            Some("数据分块到达，上一块在 1 分 00 秒前")
        );

        // 160 MB landed in 100 s; 40 s later the next block has not.
        activity.update(360 * MIB, 1600 * MIB, None);
        activity.started = Instant::now() - Duration::from_secs(140);
        activity.updated = Instant::now() - Duration::from_secs(40);
        let slow = activity.transfer_metrics(&apple, true);
        assert_eq!(slow.quantity, "360.0 MB / 1.56 GB");
        // The average falls between blocks instead of disappearing.
        assert_eq!(slow.speed.as_deref(), Some("1.1 MB/s"), "{slow:?}");
        assert!(
            matches!(&slow.eta, Some(TransferEta::Remaining(v)) if v.starts_with("18 分")),
            "{slow:?}"
        );
        assert_eq!(slow.note.as_deref(), Some("数据分块到达，上一块在 40 秒前"));
        assert_eq!(slow.source.as_deref(), Some("来源 hf-mirror.com"));
        assert!(!slow.summary().contains("等待响应"));

        // Five minutes without a block is the downloader's own stall.
        activity.updated = Instant::now() - Duration::from_secs(305);
        let stalled = activity.transfer_metrics(&apple, true);
        assert_eq!(stalled.quantity, "360.0 MB / 1.56 GB");
        assert_eq!(stalled.speed.as_deref(), Some("—"));
        assert_eq!(stalled.eta, None);
        assert_eq!(stalled.note.as_deref(), Some("已 5 分 05 秒没有收到新数据"));
    }
    #[test]
    fn unknown_total_never_invents_a_percentage_or_eta() {
        let mut item = Activity::new();
        item.update(8192, 0, None);
        assert!(item.fraction().is_none());
        assert!(
            !item
                .detail(&Stage::parse("model/test"), true)
                .contains("预计剩余")
        );
        assert_eq!(item.detail(&Stage::parse("model/test"), false), "已停止");
    }

    #[test]
    fn video_download_reports_bytes_speed_and_remaining_time() {
        let mut item = Activity::new();
        item.update(0, 16 * 1024 * 1024, None);
        item.started = Instant::now() - Duration::from_secs(10);
        item.update(8 * 1024 * 1024, 16 * 1024 * 1024, None);
        let detail = item.detail(&Stage::parse("download"), true);
        assert!(detail.contains("8.0 MB / 16.0 MB"), "{detail}");
        assert!(
            detail.contains("速度") && detail.contains("预计剩余"),
            "{detail}"
        );
        assert!(item.fraction().is_some_and(|f| (f - 0.5).abs() < 0.01));
        // 总字节未知：只有实时字节与速度，不编造 ETA
        let mut unknown = Activity::new();
        unknown.update(3 * 1024 * 1024, 0, None);
        let detail = unknown.detail(&Stage::parse("download"), true);
        assert!(
            detail.contains("3.0 MB") && !detail.contains("预计剩余"),
            "{detail}"
        );
    }
}
