//! Progress samples, independent of rendering. ETA excludes cached work and resets on retries.
use std::time::{Duration, Instant};

pub struct Activity {
    pub current: u64,
    pub total: u64,
    pub message: String,
    pub done: bool,
    pub workers: usize,
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
            started: now,
            baseline: 0,
            updated: now,
        }
    }
    pub fn update(&mut self, current: u64, total: u64, message: Option<String>) {
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
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.current as f32 / self.total as f32).clamp(0., 1.))
    }
    fn rate(&self) -> Option<f64> {
        let elapsed = self.started.elapsed().as_secs_f64();
        let processed = self.current.saturating_sub(self.baseline);
        (elapsed >= 1. && processed > 0 && self.updated.elapsed() < Duration::from_secs(30))
            .then(|| processed as f64 / elapsed)
    }
    pub fn detail(&self, stage: &str, running: bool) -> String {
        if self.done {
            return "已完成".into();
        }
        if !running {
            return "已停止".into();
        }
        let quantity = if stage.starts_with("model/") && stage != "model/apple" {
            if self.total > 0 {
                format!("{} / {}", bytes(self.current), bytes(self.total))
            } else {
                bytes(self.current)
            }
        } else if let Some(fraction) = self.fraction() {
            if stage == "transcribe" {
                format!("{} / {} 段", self.current, self.total)
            } else {
                format!("{:.0}%", fraction * 100.)
            }
        } else {
            String::new()
        };
        let remaining = if self.updated.elapsed() >= Duration::from_secs(30) && self.current > 0 {
            "等待响应".into()
        } else if self.total > self.current {
            self.rate()
                .map(|rate| {
                    format!(
                        "约剩 {}",
                        duration((self.total - self.current) as f64 / rate)
                    )
                })
                .unwrap_or_else(|| "估算中…".into())
        } else if self.total > 0 {
            "收尾中…".into()
        } else {
            format!("已用 {}", duration(self.started.elapsed().as_secs_f64()))
        };
        let speed = if stage.starts_with("model/") && stage != "model/apple" {
            self.rate()
                .map(|rate| format!(" · {}/s", bytes(rate as u64)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        if quantity.is_empty() {
            remaining
        } else {
            format!("{quantity}{speed} · {remaining}")
        }
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
        "audio" => "提取音频",
        "transcribe" => "语音转写",
        "llm" => "整理文字",
        "render" => "生成笔记",
        _ => stage,
    }
    .into()
}
fn bytes(value: u64) -> String {
    if value >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", value as f64 / (1024. * 1024. * 1024.))
    } else {
        format!("{:.1} MB", value as f64 / (1024. * 1024.))
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
        assert!(!item.detail("model/test", true).contains("约剩"));
        assert_eq!(item.detail("model/test", false), "已停止");
    }
}
