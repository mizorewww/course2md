//! The one background job the app runs at a time, and when it asks for a repaint.
use crate::{
    activity::JobEvents,
    backend::{Done, Job},
    page::Kind,
};
use std::time::{Duration, Instant};

/// `Option<Instant>` orders `None` before every `Some`; polling needs the
/// earliest concrete deadline and must ignore absent ones.
pub fn earliest_deadline(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

/// Task log/progress events coalesce into at most one repaint per interval;
/// stage, error and completion transitions still repaint immediately.
pub const EVENT_REPAINT_INTERVAL: Duration = Duration::from_millis(250);

/// A job's state, whichever kind it is.
pub struct WorkState {
    pub job: Option<Job>,
    pub kind: Kind,
    pub cancelling: bool,
    pub closing: bool,
    /// The task being started: picked from the queue, its job not yet running. Starting
    /// reads credentials and the library off the UI thread, and the task is still queued
    /// meanwhile, so this, not the queue, keeps a second start out.
    starting: Option<String>,
    pub task_status: String,
    /// What a model job has said; a task's events live on its record.
    pub job_events: JobEvents,
    /// When the current job started, for its elapsed time before any stage.
    pub job_started: Option<Instant>,
    pub active_task: Option<String>,
    pub completed: Option<Done>,
    pub quit_deadline: Option<Instant>,
    pub last_tick: Instant,
    pub event_repaint_pending: bool,
}

impl WorkState {
    pub fn new() -> Self {
        Self {
            job: None,
            kind: Kind::Convert,
            cancelling: false,
            closing: false,
            starting: None,
            task_status: String::new(),
            job_events: Default::default(),
            job_started: None,
            active_task: None,
            completed: None,
            quit_deadline: None,
            last_tick: Instant::now(),
            event_repaint_pending: false,
        }
    }

    /// Claims the start of task `id`: false while a job runs, the app closes, or another
    /// start is under way.
    pub fn claim_start(&mut self, id: &str) -> bool {
        if self.job.is_some() || self.closing || self.starting.is_some() {
            return false;
        }
        self.starting = Some(id.to_owned());
        true
    }

    /// Whether task `id` holds the start claimed for it.
    pub fn is_starting(&self, id: &str) -> bool {
        self.starting.as_deref() == Some(id)
    }

    /// The start of task `id` ended without a job: it failed, or the task changed.
    pub fn end_start(&mut self, id: &str) {
        if self.is_starting(id) {
            self.starting = None;
        }
    }

    /// Task `id`'s job runs; its start is over.
    pub fn install(&mut self, id: &str, job: Job) {
        self.end_start(id);
        self.job = Some(job);
        self.active_task = Some(id.to_owned());
    }

    /// Whether a poll that saw these events should repaint now. Stage, error and completion
    /// transitions (`immediate`) always do; log and progress lines coalesce.
    pub fn repaint_due(&mut self, streaming: bool, immediate: bool) -> bool {
        self.repaint_due_at(streaming, immediate, Instant::now())
    }

    fn repaint_due_at(&mut self, streaming: bool, immediate: bool, now: Instant) -> bool {
        let ticking = streaming || immediate || self.job.is_some() || self.event_repaint_pending;
        // Model downloads repaint at the task rate so their progress keeps up.
        let interval = if streaming || self.event_repaint_pending || self.kind == Kind::Models {
            EVENT_REPAINT_INTERVAL
        } else {
            Duration::from_secs(1)
        };
        if ticking && (immediate || now.duration_since(self.last_tick) >= interval) {
            self.last_tick = now;
            self.event_repaint_pending = false;
            return true;
        }
        if streaming {
            self.event_repaint_pending = true;
        }
        false
    }
}

impl Default for WorkState {
    fn default() -> Self {
        Self::new()
    }
}

/// What a finished model job says.
pub fn job_exit_status(success: bool, cancelled: bool) -> &'static str {
    if success {
        course2md::tr!("已完成")
    } else if cancelled {
        course2md::tr!("已停止")
    } else {
        course2md::tr!("未完成")
    }
}

#[cfg(test)]
mod tests {
    use super::{EVENT_REPAINT_INTERVAL, WorkState, earliest_deadline};
    use std::time::{Duration, Instant};

    #[test]
    fn log_lines_coalesce_and_transitions_repaint_now() {
        let mut work = WorkState::new();
        let start = work.last_tick;
        assert!(
            work.repaint_due_at(false, true, start),
            "a transition is immediate"
        );
        assert!(
            !work.repaint_due_at(true, false, start),
            "a line waits its turn"
        );
        assert!(work.event_repaint_pending);
        let later = start + EVENT_REPAINT_INTERVAL;
        assert!(
            work.repaint_due_at(false, false, later),
            "and repaints once it is due"
        );
        assert!(!work.event_repaint_pending);
        assert!(
            !work.repaint_due_at(false, false, later + Duration::from_secs(9)),
            "nothing running, nothing to repaint"
        );
    }

    /// A poll may come while a task's start reads the library: the task is still queued
    /// then, and must not be started twice.
    #[test]
    fn a_task_being_started_holds_dispatch_until_its_start_ends() {
        let mut work = WorkState::new();
        assert!(work.claim_start("a"));
        assert!(!work.claim_start("a"), "a poll during the start");
        assert!(!work.claim_start("b"));
        work.end_start("b");
        assert!(work.is_starting("a"), "another task cannot release it");
        work.end_start("a");
        assert!(work.claim_start("b"));
        work.closing = true;
        work.end_start("b");
        assert!(!work.claim_start("a"), "nothing starts while closing");
    }

    #[test]
    fn absent_deadlines_do_not_hide_other_deadlines() {
        let now = Instant::now();
        assert_eq!(earliest_deadline(None, None), None);
        assert_eq!(earliest_deadline(Some(now), None), Some(now));
        assert_eq!(earliest_deadline(None, Some(now)), Some(now));
        assert_eq!(
            earliest_deadline(Some(now + Duration::from_secs(2)), Some(now)),
            Some(now)
        );
    }
}
