//! 机器可读进度协议（--json 模式）：stdout 每行一个 NDJSON 事件，供 GUI/脚本消费。
//!
//! 协议（GUI 已按此实现，勿改字段名）：
//! - `{"type":"log","level":..,"message":..}`        — tracing 日志转发
//! - `{"type":"stage","stage":..,"status":"start"|"done"}`
//! - `{"type":"progress","stage":..,"current":n,"total":n,"message":?}`
//! - `{"type":"done", ...}` / `{"type":"error","message":..}` — 由 pipeline/main 直接 emit
//!
//! human 模式下 [`emit`]/[`stage`] 全是 no-op，[`Bar`] 退化为普通可见进度条，
//! 保证现有终端输出零变化。

use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static JSON_MODE: AtomicBool = AtomicBool::new(false);

/// 进入 NDJSON 事件流模式（进程级，main 在解析完 CLI 后调用一次）。
pub fn set_json_mode() {
    JSON_MODE.store(true, Ordering::Relaxed);
}

pub fn is_json() -> bool {
    JSON_MODE.load(Ordering::Relaxed)
}

/// 输出一个事件：json 模式写 stdout 一行并立即 flush；human 模式 no-op。
pub fn emit(ev: serde_json::Value) {
    if !is_json() {
        return;
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = emit_to(&mut lock, &ev);
}

fn emit_to(mut w: impl std::io::Write, ev: &serde_json::Value) -> std::io::Result<()> {
    writeln!(w, "{ev}")?;
    w.flush()
}

/// 阶段边界事件（json 模式才发）。
pub fn stage(name: &str, status: &str) {
    emit(stage_event(name, status));
}

fn stage_event(name: &str, status: &str) -> serde_json::Value {
    serde_json::json!({"type": "stage", "stage": name, "status": status})
}

fn progress_event(stage: &str, current: u64, total: u64, message: &str) -> serde_json::Value {
    let mut ev = serde_json::json!({
        "type": "progress",
        "stage": stage,
        "current": current,
        "total": total,
    });
    if !message.is_empty() {
        ev["message"] = serde_json::Value::String(message.to_string());
    }
    ev
}

fn log_event(level: &str, message: &str) -> serde_json::Value {
    serde_json::json!({"type": "log", "level": level, "message": message})
}

/// 共享进度条样式：模板均为静态字符串，解析失败是编程错误。
fn progress_style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template)
        .expect("静态进度条模板")
        .progress_chars("##-")
}

/// 进度条包装：human 模式 = 可见 indicatif 进度条（样式与原来一致）；
/// json 模式 = 隐藏 bar，inc/set_position/set_message 时发 progress 事件。
pub struct Bar {
    stage: String,
    bar: ProgressBar,
    current: AtomicU64,
    total: u64,
    message: Mutex<String>,
    last_emit: Mutex<std::time::Instant>,
}

impl Bar {
    pub fn new(stage: impl Into<String>, len: u64) -> Self {
        let bar = if is_json() {
            ProgressBar::hidden()
        } else {
            ProgressBar::new(len)
        };
        Self {
            stage: stage.into(),
            bar,
            current: AtomicU64::new(0),
            total: len,
            message: Mutex::new(String::new()),
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

    pub fn inc(&self, n: u64) {
        self.bar.inc(n);
        if is_json() {
            let cur = self.current.fetch_add(n, Ordering::Relaxed) + n;
            self.emit_progress(cur);
        }
    }

    pub fn set_position(&self, pos: u64) {
        self.bar.set_position(pos);
        if is_json() {
            self.current.store(pos, Ordering::Relaxed);
            self.emit_progress(pos);
        }
    }

    pub fn set_message(&self, msg: impl Into<String>) {
        let msg = msg.into();
        if is_json() {
            *self.message.lock().unwrap() = msg;
            let cur = self.current.load(Ordering::Relaxed);
            self.emit_progress(cur);
        } else {
            self.bar.set_message(msg);
        }
    }

    pub fn finish(&self) {
        if is_json() {
            self.emit_progress(self.current.load(Ordering::Relaxed));
        }
        self.bar.finish_and_clear();
    }

    fn emit_progress(&self, current: u64) {
        let mut last = self.last_emit.lock().unwrap();
        if current != 0
            && current != self.total
            && last.elapsed() < std::time::Duration::from_millis(100)
        {
            return;
        }
        *last = std::time::Instant::now();
        let msg = self.message.lock().map(|m| m.clone()).unwrap_or_default();
        emit(progress_event(&self.stage, current, self.total, &msg));
    }
}

/// tracing Layer：把日志事件格式化为 `{"type":"log"}` NDJSON 行写 stdout。
/// 仅在 json 模式注册（main.rs），level 映射 error/warn/info/debug（trace 归 debug）。
pub struct JsonLogLayer;

pub fn json_log_layer() -> JsonLogLayer {
    JsonLogLayer
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for JsonLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => "error",
            tracing::Level::WARN => "warn",
            tracing::Level::INFO => "info",
            _ => "debug",
        };
        emit(log_event(level, &visitor.into_message()));
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
    fn stage_event_shape() {
        assert_eq!(
            stage_event("scenes", "start"),
            serde_json::json!({"type": "stage", "stage": "scenes", "status": "start"})
        );
    }

    #[test]
    fn progress_event_message_optional() {
        assert_eq!(
            progress_event("transcribe", 3, 10, ""),
            serde_json::json!({"type": "progress", "stage": "transcribe", "current": 3, "total": 10})
        );
        let ev = progress_event("llm", 1, 2, "第 3 节");
        assert_eq!(ev["message"], "第 3 节");
    }

    #[test]
    fn log_event_shape() {
        assert_eq!(
            log_event("warn", "boom"),
            serde_json::json!({"type": "log", "level": "warn", "message": "boom"})
        );
    }

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
        let ev = serde_json::json!({"type": "log", "level": "info", "message": "hi"});
        emit_to(&mut buf, &ev).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.ends_with('\n'));
        assert_eq!(s.trim().parse::<serde_json::Value>().unwrap(), ev);
    }
}
