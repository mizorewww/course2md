//! Opt-in native measurements. No collection task or file I/O in ordinary builds.
use gpui::{App, ElementId, Window};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

const MOTION_EVENT_CAPACITY: usize = 2048;
const MOTION_KEY_CAPACITY: usize = 128;

struct MotionEvent {
    id: ElementId,
    target: f32,
    value: f32,
    recorded_at: Instant,
}

struct MotionKey {
    id: ElementId,
    target_bits: u32,
    value_bits: u32,
}

struct MotionTrace {
    events: VecDeque<MotionEvent>,
    keys: VecDeque<MotionKey>,
    dropped: usize,
}

impl MotionTrace {
    fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(MOTION_EVENT_CAPACITY),
            keys: VecDeque::with_capacity(MOTION_KEY_CAPACITY),
            dropped: 0,
        }
    }

    fn record(&mut self, id: ElementId, target: f32, value: f32) {
        let target_bits = target.to_bits();
        let value_bits = value.to_bits();
        if let Some(previous) = self.keys.iter_mut().find(|previous| previous.id == id) {
            if previous.target_bits == target_bits && previous.value_bits == value_bits {
                return;
            }
            previous.target_bits = target_bits;
            previous.value_bits = value_bits;
        } else {
            if self.keys.len() == MOTION_KEY_CAPACITY {
                self.keys.pop_front();
            }
            self.keys.push_back(MotionKey {
                id: id.clone(),
                target_bits,
                value_bits,
            });
        }
        if self.events.len() == MOTION_EVENT_CAPACITY {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(MotionEvent {
            id,
            target,
            value,
            recorded_at: Instant::now(),
        });
    }
}

thread_local! {
    // Rendering only touches this bounded memory; it never formats IDs,
    // encodes JSON, writes files or requests frames.
    static MOTION_TRACE: RefCell<Option<MotionTrace>> = const { RefCell::new(None) };
}

pub(crate) fn record_motion(id: ElementId, target: f32, value: f32) {
    MOTION_TRACE.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            trace.record(id, target, value);
        }
    });
}

fn take_motion_events(mut spare: VecDeque<MotionEvent>) -> (VecDeque<MotionEvent>, usize) {
    MOTION_TRACE.with(|trace| {
        let mut trace = trace.borrow_mut();
        let Some(trace) = trace.as_mut() else {
            return (spare, 0);
        };
        // Both buffers were allocated off the UI thread and retain capacity
        // between flushes. A snapshot swaps ownership without copying events.
        std::mem::swap(&mut trace.events, &mut spare);
        (spare, std::mem::take(&mut trace.dropped))
    })
}

/// Record GPUI's measured draw and input-to-present histograms once per second.
/// File creation, encoding and writes run on the background executor. This does
/// not request frames, navigate the app, or change the animation scheduler.
pub fn start(window: &mut Window, cx: &App) {
    let Some(path) = std::env::var_os("COURSE2MD_PERFORMANCE_LOG").map(PathBuf::from) else {
        return;
    };
    let trace_frames = std::env::var_os("COURSE2MD_FRAME_TRACE").is_some();
    let trace_motion =
        std::env::var_os("COURSE2MD_MOTION_TRACE").as_deref() == Some(std::ffi::OsStr::new("1"));
    if trace_frames {
        gpui::profiler::set_trace_enabled(true);
    }
    MOTION_TRACE.with(|trace| *trace.borrow_mut() = trace_motion.then(MotionTrace::new));
    let executor = cx.background_executor().clone();
    window
        .spawn(cx, async move |cx| {
            let mut motion_spare =
                VecDeque::with_capacity(if trace_motion { MOTION_EVENT_CAPACITY } else { 0 });
            let started = Instant::now();
            let mut collector = gpui::profiler::FrameTimingCollector::new();
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                let Ok((frames, inputs, active, mut motion_events, motion_dropped)) =
                    cx.update(move |window, _| {
                        let (motion_events, motion_dropped) = take_motion_events(motion_spare);
                        (
                            window.frame_duration_snapshot(),
                            window.input_latency_snapshot(),
                            window.is_window_active(),
                            motion_events,
                            motion_dropped,
                        )
                    })
                else {
                    break;
                };
                let elapsed_ms = started.elapsed().as_millis();
                let events = collector.collect_unseen();
                let path = path.clone();
                let result = executor
                    .spawn(async move {
                        macro_rules! histogram {
                        ($hist:expr) => {{
                            let h = &$hist;
                            json!({
                                "count": h.len(),
                                "p50_ms": h.value_at_quantile(0.50) as f64 / 1e6,
                                "p95_ms": h.value_at_quantile(0.95) as f64 / 1e6,
                                "p99_ms": h.value_at_quantile(0.99) as f64 / 1e6,
                                "max_ms": h.max() as f64 / 1e6,
                                "over_16_7_ms": h.count_between(16_700_000, u64::MAX),
                                "over_100_ms": h.count_between(100_000_000, u64::MAX),
                            })
                        }};
                    }
                        let sample = json!({
                            "pid": std::process::id(),
                            "unix_ms": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(),
                            "elapsed_ms": elapsed_ms,
                            "window_active_at_sample": active,
                            "motion_trace_enabled": trace_motion,
                            "motion_events": motion_events.iter().map(|event| json!({
                                "id": event.id.to_string(),
                                "target": event.target,
                                "value": event.value,
                                "elapsed_ms": event.recorded_at.duration_since(started).as_secs_f64() * 1000.,
                            })).collect::<Vec<_>>(),
                            "motion_events_dropped": motion_dropped,
                            "frame_events": events.into_iter().map(|event| match event {
                                gpui::profiler::FrameEvent::Complete(frame) => json!({
                                    "kind": "complete",
                                    "age_ms": frame.start.elapsed().as_secs_f64() * 1000.,
                                    "duration_ms": frame.duration().as_secs_f64() * 1000.,
                                    "presentation_wait_ms": frame.presentation_wait.as_secs_f64() * 1000.,
                                }),
                                gpui::profiler::FrameEvent::Draw(frame) => json!({
                                    "kind": "draw",
                                    "age_ms": frame.draw_start.elapsed().as_secs_f64() * 1000.,
                                    "draw_ms": frame.draw_duration().as_secs_f64() * 1000.,
                                    "dirty_to_draw_ms": frame.dirty_to_draw_duration().map(|value| value.as_secs_f64() * 1000.),
                                }),
                                gpui::profiler::FrameEvent::Present(frame) => json!({
                                    "kind": "present",
                                    "age_ms": frame.present_start.elapsed().as_secs_f64() * 1000.,
                                    "present_ms": frame.present_duration().as_secs_f64() * 1000.,
                                    "successful_new_frame": frame.successful_new_frame,
                                }),
                            }).collect::<Vec<_>>(),
                            "draw": histogram!(frames.draw_duration_histogram),
                            "dirty_to_present": histogram!(frames.dirty_to_present_histogram),
                            "animation_interval": histogram!(frames.present_interval_histogram),
                            "input_to_present": histogram!(inputs.latency_histogram),
                            "mid_draw_events_dropped": inputs.mid_draw_events_dropped,
                        });
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)?;
                        serde_json::to_writer(&mut file, &sample)?;
                        file.write_all(b"\n")?;
                        motion_events.clear();
                        Ok::<_, std::io::Error>(motion_events)
                    })
                    .await;
                match result {
                    Ok(spare) => motion_spare = spare,
                    Err(error) => {
                        eprintln!("Performance recording stopped: {error}");
                        break;
                    }
                }
            }
            cx.update(|_, _| MOTION_TRACE.with(|trace| *trace.borrow_mut() = None))
                .ok();
        })
        .detach();
}
