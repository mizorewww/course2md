//! Progress samples, independent of rendering. ETA excludes cached work and resets on retries.
use std::time::{Duration, Instant};

pub struct Activity {
    pub current: u64,
    pub total: u64,
    pub message: String,
    pub done: bool,
    pub workers: usize,
    /// LLM 阶段累计 token 用量（上传=prompt，下载=completion），实时跳动
    pub tokens_prompt: u64,
    pub tokens_completion: u64,
    sampled: bool,
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
            done: false,
            workers: 1,
            tokens_prompt: 0,
            tokens_completion: 0,
            sampled: false,
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
    pub fn note_tokens(&mut self, prompt: u64, completion: u64) {
        self.tokens_prompt = prompt;
        self.tokens_completion = completion;
    }
    /// AI 阶段的实时反馈：服务用量一到就替换「等待服务返回结果」的静态文案。
    pub fn tokens_detail(&self) -> Option<String> {
        (self.tokens_prompt > 0 || self.tokens_completion > 0).then(|| {
            format!(
                "上传 {} tokens · 下载 {} tokens",
                count(self.tokens_prompt),
                count(self.tokens_completion)
            )
        })
    }
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.current as f32 / self.total as f32).clamp(0., 1.))
    }
    pub fn has_samples(&self) -> bool {
        self.sampled
    }
    fn rate(&self) -> Option<f64> {
        let elapsed = self.started.elapsed().as_secs_f64();
        let processed = self.current.saturating_sub(self.baseline);
        (elapsed >= 1. && processed > 0 && self.updated.elapsed() < Duration::from_secs(30))
            .then(|| processed as f64 / elapsed)
    }
    pub fn transfer_metrics(&self, stage: &str, running: bool) -> TransferMetrics {
        if self.done {
            return TransferMetrics {
                note: Some("已完成".into()),
                ..TransferMetrics::default()
            };
        }
        if !running {
            return TransferMetrics {
                note: Some("已停止".into()),
                ..TransferMetrics::default()
            };
        }
        let quantity = quantity(stage, self.current, self.total);
        // Apple reports weighted preparation stages, not byte throughput. Loading,
        // downloads and compilation do not advance those stages at a constant rate.
        if stage == "model/apple" {
            return TransferMetrics {
                quantity,
                note: Some(format!(
                    "已用 {}",
                    duration(self.started.elapsed().as_secs_f64())
                )),
                ..TransferMetrics::default()
            };
        }
        let byte_download = is_byte_download(stage);
        let stalled = self.updated.elapsed() >= Duration::from_secs(30) && self.current > 0;
        if stalled {
            return TransferMetrics {
                quantity,
                speed: byte_download.then(|| "—".into()),
                note: Some(
                    if stage.starts_with("scenes/") {
                        "仍在处理"
                    } else {
                        "等待响应"
                    }
                    .into(),
                ),
                ..TransferMetrics::default()
            };
        }
        let rate = self.rate();
        let speed = byte_download.then(|| match rate {
            Some(rate) => format!("{}/s", throughput(rate)),
            None => "正在测量".into(),
        });
        let eta = if self.total > self.current {
            match rate {
                Some(rate) => Some(TransferEta::Remaining(duration(
                    (self.total - self.current) as f64 / rate,
                ))),
                None if byte_download => Some(TransferEta::Remaining("计算中".into())),
                None => None,
            }
        } else if self.total > 0 {
            Some(TransferEta::Note("收尾中…".into()))
        } else if !byte_download {
            Some(TransferEta::Note(format!(
                "已用 {}",
                duration(self.started.elapsed().as_secs_f64())
            )))
        } else {
            None
        };
        TransferMetrics {
            quantity,
            speed,
            eta,
            note: None,
        }
    }
    pub fn detail(&self, stage: &str, running: bool) -> String {
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
}

impl TransferMetrics {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.quantity.is_empty() {
            parts.push(self.quantity.clone());
        }
        if let Some(speed) = &self.speed {
            parts.push(format!("速度 {speed}"));
        }
        if let Some(eta) = &self.eta {
            parts.push(match eta {
                TransferEta::Remaining(value) => format!("预计剩余 {value}"),
                TransferEta::Note(value) => value.clone(),
            });
        }
        if let Some(note) = &self.note {
            parts.push(note.clone());
        }
        parts.join(" · ")
    }
}

fn is_byte_download(stage: &str) -> bool {
    stage == "download" || (stage.starts_with("model/") && stage != "model/apple")
}

pub fn quantity(stage: &str, current: u64, total: u64) -> String {
    if stage == "scenes/scan" {
        format!("已检查 {current} 张画面")
    } else if stage == "scenes/extract" && total > 0 {
        format!("已保存 {current} / {total} 张截图")
    } else if is_byte_download(stage) {
        if total > 0 {
            format!("{} / {}", bytes(current), bytes(total))
        } else {
            bytes(current)
        }
    } else if total > 0 {
        if stage == "transcribe" {
            format!("{current} / {total} 段")
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

/// Older saved outcomes combined a stage label, retention message and English
/// diagnostics. Keep these records readable without rewriting their evidence.
pub fn component_failure_message(label: &str, message: Option<&str>) -> String {
    let chinese = message
        .unwrap_or_default()
        .split(" / ")
        .next()
        .unwrap_or_default()
        .trim();
    if chinese.is_empty()
        || matches!(
            chinese,
            "摘要尚未完成，已保留正文"
                | "校对未全部完成，原文已保留"
                | "部分校对未完成，原始文字已保留"
        )
    {
        return format!("{label}尚未完成，正文已保存。可以仅补{label}。");
    }
    let prefix = format!("{label}尚未完成");
    if chinese.starts_with(&prefix) {
        chinese.into()
    } else {
        format!("{label}尚未完成：{chinese}")
    }
}

pub fn stage_order(stage: &str) -> usize {
    match stage {
        "fetch" => 0,
        "subtitle" => 1,
        "download" => 2,
        "scenes" | "scenes/scan" => 3,
        "scenes/extract" => 4,
        "audio" => 5,
        s if s.starts_with("model/") => 6,
        "model-load" => 7,
        "transcribe" => 8,
        "llm" => 9,
        "summary" | "summarize" => 10,
        "render" => 11,
        "export" | "exports" => 12,
        _ => 13,
    }
}

pub fn title(stage: &str) -> String {
    if let Some(file) = stage.strip_prefix("model/") {
        return if file == "apple" {
            "准备 Apple 识别模型".into()
        } else if file.starts_with("mmproj") {
            "下载音频编码器".into()
        } else {
            "下载语音模型".into()
        };
    }
    match stage {
        "model-load" => "加载识别模型",
        "fetch" => "读取课程",
        "download" => "下载视频",
        "scenes" => "提取画面",
        "scenes/scan" => "扫描画面",
        "scenes/extract" => "生成截图",
        "audio" => "提取音频",
        "transcribe" => "语音转写",
        "subtitle" => "读取字幕",
        "llm" => "AI 校对",
        "summary" | "summarize" => "生成摘要",
        "export" | "exports" => "导出文件",
        "render" => "生成笔记",
        _ => stage,
    }
    .into()
}
/// 模型下载/准备类阶段：诊断面板与引导页共用同一过滤器（此前三处复制）。
pub(crate) fn is_model_transfer_stage(stage: &str) -> bool {
    stage.starts_with("model") || stage.contains("download")
}

/// 准备阶段文案：诊断面板与引导页同一来源。
/// 阶段名来自结构化的 stage；消息关键词只用于细分文件类型。
/// 注意：关键词嗅探依赖 worker 输出文案——上游改文案时退化为 stage 标题（可接受的降级，
/// 彻底修法是引擎发结构化阶段事件，记录在 docs/ENGINEERING-AUDIT.md 后续建议）。
const PREPARATION_PHASE_HINTS: &[(&str, &str)] = &[
    ("silero", "下载语音检测文件"),
    ("compil", "编译识别模型"),
    ("whisper", "下载 Whisper 模型文件"),
];

pub(crate) fn model_transfer_phase(stage: &str, message: &str) -> String {
    let lower = message.trim().to_ascii_lowercase();
    for (needle, phase) in PREPARATION_PHASE_HINTS {
        if lower.contains(needle) {
            return (*phase).into();
        }
    }
    if lower
        .split(|character: char| !character.is_ascii_alphabetic())
        .any(|word| matches!(word, "loading" | "load"))
        || message.contains("加载")
    {
        return "加载识别模型".into();
    }
    if lower.contains("qwen") && lower.contains('/') {
        return "下载 Qwen3 模型文件".into();
    }
    title(stage)
}

pub(crate) fn bytes(value: u64) -> String {    if value >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", value as f64 / (1024. * 1024. * 1024.))
    } else if value >= 1024 * 1024 {
        format!("{:.1} MB", value as f64 / (1024. * 1024.))
    } else if value >= 1024 {
        format!("{:.0} KB", value as f64 / 1024.)
    } else {
        format!("{value} 字节")
    }
}

/// 千分位计数：跳动的 token 计数保持数位分组，宽度变化可预期。
fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(character);
    }
    out
}

fn throughput(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1024. * 1024. * 1024. {
        format!("{:.2} GB", bytes_per_sec / (1024. * 1024. * 1024.))
    } else if bytes_per_sec >= 1024. * 1024. {
        format!("{:.1} MB", bytes_per_sec / (1024. * 1024.))
    } else if bytes_per_sec >= 1024. {
        format!("{:.0} KB", bytes_per_sec / 1024.)
    } else {
        format!("{:.0} 字节", bytes_per_sec.max(0.))
    }
}
fn duration(seconds: f64) -> String {
    let seconds = seconds.ceil() as u64;
    if seconds >= 3600 {
        format!("{} 小时 {} 分", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{} 分 {} 秒", seconds / 60, seconds % 60)
    } else {
        format!("{seconds} 秒")
    }
}

#[cfg(test)]
mod tests {
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
        let detail = activity.detail("model/apple", true);
        assert!(
            detail.contains("17%") && detail.contains("已用 3 分"),
            "{detail}"
        );
        assert!(
            !detail.contains("约剩") && !detail.contains("/s") && !detail.contains("MB"),
            "{detail}"
        );

        activity.updated = Instant::now() - Duration::from_secs(60);
        let waiting = activity.detail("model/apple", true);
        assert!(
            waiting.contains("已用") && !waiting.contains("等待响应"),
            "{waiting}"
        );
        assert_eq!(activity.detail("model/apple", false), "已停止");
        activity.done = true;
        assert_eq!(activity.detail("model/apple", true), "已完成");
    }

    #[test]
    fn byte_downloads_keep_measured_speed_and_remaining_time() {
        let mut activity = Activity::new();
        activity.update(0, 8 * 1024 * 1024, None);
        activity.started = Instant::now() - Duration::from_secs(10);
        activity.update(4 * 1024 * 1024, 8 * 1024 * 1024, None);
        let metrics = activity.transfer_metrics("model/model.gguf", true);
        assert_eq!(metrics.quantity, "4.0 MB / 8.0 MB");
        assert_eq!(metrics.speed.as_deref(), Some("410 KB/s"));
        assert!(
            matches!(&metrics.eta, Some(TransferEta::Remaining(value)) if value.ends_with("秒")),
            "{metrics:?}"
        );
        let detail = activity.detail("model/model.gguf", true);
        assert!(detail.contains("4.0 MB / 8.0 MB"), "{detail}");
        assert!(
            detail.contains("速度 410 KB/s") && detail.contains("预计剩余"),
            "{detail}"
        );
    }

    #[test]
    fn model_download_metrics_stay_labeled_while_speed_is_measured() {
        let mut activity = Activity::new();
        activity.update(1_048_576, 8 * 1024 * 1024, None);
        let metrics = activity.transfer_metrics("model/model.gguf", true);
        assert_eq!(metrics.speed.as_deref(), Some("正在测量"));
        assert_eq!(metrics.eta, Some(TransferEta::Remaining("计算中".into())));
        let detail = metrics.summary();
        assert!(
            detail.contains("速度 正在测量") && detail.contains("计算中"),
            "{detail}"
        );
    }
    #[test]
    fn completed_scan_cannot_supply_the_new_extraction_counter_or_eta() {
        let mut scan = Activity::new();
        scan.update(18, 0, Some("已找到 1 张候选截图".into()));
        assert!(scan.fraction().is_none());
        assert!(
            scan.detail("scenes/scan", true)
                .contains("已检查 18 张画面")
        );
        scan.done = true;
        let mut extract = Activity::new();
        extract.update(0, 1, None);
        extract.started = Instant::now() - Duration::from_secs(25);
        let detail = extract.detail("scenes/extract", true);
        assert!(detail.contains("已保存 0 / 1 张截图"), "{detail}");
        assert!(
            !detail.contains("100%") && !detail.contains("收尾") && !detail.contains("预计剩余")
        );
        assert_eq!(extract.fraction(), Some(0.));
        extract.update(1, 1, None);
        extract.done = true;
        assert_eq!(extract.detail("scenes/extract", true), "已完成");
        let mut persisted = crate::workspace::Stage {
            status: "done".into(),
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
            component_failure_message(
                "摘要",
                Some("摘要尚未完成，已保留正文 / Summary incomplete; body retained")
            ),
            "摘要尚未完成，正文已保存。可以仅补摘要。"
        );
        assert_eq!(
            component_failure_message("摘要", Some("服务拒绝了请求（HTTP 400）。")),
            "摘要尚未完成：服务拒绝了请求（HTTP 400）。"
        );
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
        assert!(item.detail("model/test", true).contains("等待响应"));
    }
    #[test]
    fn unknown_total_never_invents_a_percentage_or_eta() {
        let mut item = Activity::new();
        item.update(8192, 0, None);
        assert!(item.fraction().is_none());
        assert!(!item.detail("model/test", true).contains("预计剩余"));
        assert_eq!(item.detail("model/test", false), "已停止");
    }

    #[test]
    fn video_download_reports_bytes_speed_and_remaining_time() {
        let mut item = Activity::new();
        item.update(0, 16 * 1024 * 1024, None);
        item.started = Instant::now() - Duration::from_secs(10);
        item.update(8 * 1024 * 1024, 16 * 1024 * 1024, None);
        let detail = item.detail("download", true);
        assert!(detail.contains("8.0 MB / 16.0 MB"), "{detail}");
        assert!(detail.contains("速度") && detail.contains("预计剩余"), "{detail}");
        assert!(item.fraction().is_some_and(|f| (f - 0.5).abs() < 0.01));
        // 总字节未知：只有实时字节与速度，不编造 ETA
        let mut unknown = Activity::new();
        unknown.update(3 * 1024 * 1024, 0, None);
        let detail = unknown.detail("download", true);
        assert!(detail.contains("3.0 MB") && !detail.contains("预计剩余"), "{detail}");
    }

    #[test]
    fn token_usage_replaces_the_waiting_placeholder() {
        let mut item = Activity::new();
        assert!(item.tokens_detail().is_none());
        item.note_tokens(12345, 6789);
        assert_eq!(
            item.tokens_detail().as_deref(),
            Some("上传 12,345 tokens · 下载 6,789 tokens")
        );
    }
}
