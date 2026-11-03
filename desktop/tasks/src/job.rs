//! The one background job the app runs at a time, when it asks for a repaint, and when
//! the app's event loop must wake.
use crate::backend::{Done, Event, Job};
use desktop_store::{activity::JobEvents, page::Kind};
use std::time::{Duration, Instant};

/// `Option<Instant>` orders `None` before every `Some`; polling needs the
/// earliest concrete deadline and must ignore absent ones.
pub fn earliest_deadline(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    a.into_iter().chain(b).min()
}

/// The shortest the UI's event loop sleeps. A zero-length GPUI timer is ready at
/// once, so a loop that slept toward a deadline already past would run again without
/// yielding: no frame drawn, no other task (a blocking check's result) let in.
pub const MIN_POLL_SLEEP: Duration = Duration::from_millis(15);
/// How long the loop sleeps with nothing due; an explicit wake ends it sooner.
pub const IDLE_POLL_SLEEP: Duration = Duration::from_secs(3600);

/// How long the event loop sleeps at `now` toward `deadline`.
pub fn poll_timeout(deadline: Option<Instant>, now: Instant) -> Duration {
    deadline
        .map_or(IDLE_POLL_SLEEP, |deadline| {
            deadline.saturating_duration_since(now)
        })
        .max(MIN_POLL_SLEEP)
}

/// Task log/progress events coalesce into at most one repaint per interval;
/// stage, error and completion transitions still repaint immediately.
pub(crate) const EVENT_REPAINT_INTERVAL: Duration = Duration::from_millis(250);

/// How long a finished task's result stays announced.
pub const RESULT_SHOWN: Duration = Duration::from_secs(8);
/// How long the form waits after an edit before its text is saved.
pub const DRAFT_SAVE_DELAY: Duration = Duration::from_millis(350);
/// How often a running job's clock ticks.
const CLOCK_TICK: Duration = Duration::from_secs(1);
/// The most events one poll takes from a job.
const EVENTS_PER_POLL: usize = 512;

/// The running work: the job, whichever kind it is, and the deadlines of the event loop
/// that reads it.
pub struct Work {
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
    /// The task whose end was just announced, and when.
    pub result_shown: Option<(String, Instant)>,
    /// When the form's text is saved, after its last edit.
    draft_deadline: Option<Instant>,
    /// Wakes the event loop: a job's events, a check that ended, a new deadline.
    wake: smol::channel::Sender<()>,
    woken: smol::channel::Receiver<()>,
}

impl Work {
    pub fn new() -> Self {
        let (wake, woken) = smol::channel::bounded(1);
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
            result_shown: None,
            draft_deadline: None,
            wake,
            woken,
        }
    }

    /// What a job or a record sends to wake the event loop.
    pub fn waker(&self) -> smol::channel::Sender<()> {
        self.wake.clone()
    }

    /// What the event loop waits on besides its deadline.
    pub fn woken(&self) -> smol::channel::Receiver<()> {
        self.woken.clone()
    }

    /// Makes the event loop run now and compute its next deadline again.
    pub fn wake(&self) {
        let _ = self.wake.try_send(());
    }

    /// Saves the form's text once it has rested for `DRAFT_SAVE_DELAY` after `now`.
    pub fn save_draft_later(&mut self, now: Instant) {
        self.draft_deadline = Some(now + DRAFT_SAVE_DELAY);
        self.wake();
    }

    /// The form's text was saved: nothing is due.
    pub fn draft_saved(&mut self) {
        self.draft_deadline = None;
    }

    /// Whether the form's text is due to be saved at `now`.
    pub fn draft_due(&self, now: Instant) -> bool {
        self.draft_deadline.is_some_and(|deadline| now >= deadline)
    }

    /// Announces task `id`'s end for `RESULT_SHOWN` from `now`.
    pub fn show_result(&mut self, id: &str, now: Instant) {
        self.result_shown = Some((id.to_owned(), now));
        self.wake();
    }

    /// Ends an announcement shown for `RESULT_SHOWN` by `now`; true if one ended.
    pub fn expire_result(&mut self, now: Instant) -> bool {
        let expired = self
            .result_shown
            .as_ref()
            .is_some_and(|(_, shown)| now.duration_since(*shown) >= RESULT_SHOWN);
        if expired {
            self.result_shown = None;
        }
        expired
    }

    /// The next instant this work needs the event loop at, seen at `now`: the draft save,
    /// the quit deadline, the end of an announcement and a running job's clock.
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        let mut deadline = earliest_deadline(self.draft_deadline, self.quit_deadline);
        if let Some((_, shown)) = &self.result_shown {
            deadline = earliest_deadline(deadline, Some(*shown + RESULT_SHOWN));
        }
        if self.job.is_some() {
            deadline = earliest_deadline(deadline, Some(now + CLOCK_TICK));
        }
        deadline
    }

    /// Whether task `id` holds the running job (its record may lag the job's events).
    pub fn is_running(&self, id: &str) -> bool {
        self.active_task.as_deref() == Some(id) && self.job.is_some()
    }

    /// What the job said since the last poll, at most `EVENTS_PER_POLL` of it.
    pub fn take_events(&self) -> Vec<Event> {
        self.job
            .as_ref()
            .map(|job| job.events.try_iter().take(EVENTS_PER_POLL).collect())
            .unwrap_or_default()
    }

    /// The job ended; the task it ran, if it ran one.
    pub fn exited(&mut self) -> Option<String> {
        self.job = None;
        self.cancelling = false;
        self.active_task.take()
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
    pub fn install(&mut self, id: &str, job: Job, status: String, now: Instant) {
        self.end_start(id);
        self.job = Some(job);
        self.active_task = Some(id.to_owned());
        self.kind = Kind::Convert;
        self.cancelling = false;
        self.job_started = Some(now);
        self.task_status = status;
    }

    /// A model preparation runs.
    pub fn install_models(&mut self, job: Job) {
        self.job = Some(job);
        self.active_task = None;
        self.kind = Kind::Models;
        self.cancelling = false;
        self.job_events = Default::default();
        self.task_status = c2m_i18n::tr!("正在准备识别模型").into();
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

impl Default for Work {
    fn default() -> Self {
        Self::new()
    }
}

/// What a finished model job says.
pub fn job_exit_status(success: bool, cancelled: bool) -> &'static str {
    if success {
        c2m_i18n::tr!("已完成")
    } else if cancelled {
        c2m_i18n::tr!("已停止")
    } else {
        c2m_i18n::tr!("未完成")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DRAFT_SAVE_DELAY, EVENT_REPAINT_INTERVAL, IDLE_POLL_SLEEP, MIN_POLL_SLEEP, RESULT_SHOWN,
        Work, earliest_deadline, poll_timeout,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn log_lines_coalesce_and_transitions_repaint_now() {
        let mut work = Work::new();
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
        let mut work = Work::new();
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

    /// A deadline already past still sleeps on a real timer, so the loop yields.
    #[test]
    fn a_past_deadline_still_yields_to_the_executor() {
        let now = Instant::now();
        let past = now - Duration::from_secs(30);
        assert_eq!(poll_timeout(Some(past), now), MIN_POLL_SLEEP);
        assert_eq!(poll_timeout(Some(now), now), MIN_POLL_SLEEP);
        assert_eq!(
            poll_timeout(Some(now + Duration::from_secs(1)), now),
            Duration::from_secs(1)
        );
        assert_eq!(poll_timeout(None, now), IDLE_POLL_SLEEP);
    }

    /// Each deadline is due once, and a wake is waiting for the loop when one is set.
    #[test]
    fn deadlines_come_from_the_draft_the_announcement_and_quitting() {
        let mut work = Work::new();
        let now = Instant::now();
        assert_eq!(work.deadline(now), None, "idle work sleeps until woken");
        work.save_draft_later(now);
        assert!(
            work.woken().try_recv().is_ok(),
            "a new deadline wakes the loop"
        );
        assert_eq!(work.deadline(now), Some(now + DRAFT_SAVE_DELAY));
        assert!(!work.draft_due(now));
        assert!(work.draft_due(now + DRAFT_SAVE_DELAY));
        work.draft_saved();
        assert_eq!(work.deadline(now), None);

        work.show_result("a", now);
        assert_eq!(work.deadline(now), Some(now + RESULT_SHOWN));
        assert!(!work.expire_result(now + RESULT_SHOWN - Duration::from_millis(1)));
        assert!(work.expire_result(now + RESULT_SHOWN));
        assert_eq!(work.result_shown, None);
        assert!(!work.expire_result(now + RESULT_SHOWN), "it ends once");

        work.quit_deadline = Some(now + Duration::from_secs(10));
        work.save_draft_later(now);
        assert_eq!(
            work.deadline(now),
            Some(now + DRAFT_SAVE_DELAY),
            "the earliest wins"
        );
    }

    /// A running job ticks the clock, holds the start, and its exit names its task once.
    #[cfg(unix)]
    #[test]
    fn a_running_job_ticks_and_its_exit_names_its_task() {
        let mut work = Work::new();
        let now = Instant::now();
        let job = crate::backend::Job::spawn("/bin/sh".into(), vec!["-c".into(), "true".into()])
            .expect("a shell");
        assert!(work.claim_start("t"));
        work.install("t", job, "running".into(), now);
        assert!(work.is_running("t"));
        assert!(!work.is_running("u"));
        assert!(!work.claim_start("u"), "one job at a time");
        assert_eq!(work.deadline(now), Some(now + super::CLOCK_TICK));
        assert_eq!(work.exited().as_deref(), Some("t"));
        assert!(!work.is_running("t"));
        assert_eq!(work.exited(), None);
        assert_eq!(work.deadline(now), None);
    }
}
