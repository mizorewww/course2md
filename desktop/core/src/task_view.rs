//! What a task is doing and what can be done about it, as words and decisions. The task
//! page draws this.
use crate::{
    activity,
    workspace::{self, Intent, TaskRecord, TaskState},
};
use course2md::config::TranscriptSource;
use course2md::execution::Component;
use course2md::progress::Stage;

/// Request purpose of a remote speech recognition call.
pub const TRANSCRIPTION_PURPOSE: &str = "transcription";

/// A settled task that waits for the person.
pub fn needs_person(task: &TaskRecord) -> bool {
    matches!(
        task.state,
        TaskState::NeedsAttention | TaskState::Uncertain | TaskState::Paused | TaskState::Partial
    )
}

/// Page order: the work in progress, what waits on the person, what follows,
/// then the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskGroup {
    Running,
    Attention,
    Queued,
    Finished,
    History,
}

impl TaskGroup {
    pub const ORDER: [Self; 5] = [
        Self::Running,
        Self::Attention,
        Self::Queued,
        Self::Finished,
        Self::History,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => course2md::tr!("正在处理"),
            Self::Attention => course2md::tr!("需要处理"),
            Self::Queued => course2md::tr!("排队中"),
            Self::Finished => course2md::tr!("已结束"),
            Self::History => course2md::tr!("此前处理记录"),
        }
    }
}

pub fn task_group(task: &TaskRecord) -> TaskGroup {
    if task.handled_by.is_some() {
        TaskGroup::History
    } else {
        match task.state {
            // A task the person paused keeps its place in the queue.
            TaskState::Paused if !task.has_uncertain() => TaskGroup::Queued,
            TaskState::Paused
            | TaskState::NeedsAttention
            | TaskState::Uncertain
            | TaskState::Partial => TaskGroup::Attention,
            TaskState::Running | TaskState::Pausing => TaskGroup::Running,
            TaskState::Queued => TaskGroup::Queued,
            TaskState::Complete | TaskState::Cancelled => TaskGroup::Finished,
        }
    }
}

/// What the app bar shows for the task page, in one pass over the tasks: how
/// many still need doing, and whether one needs the person.
pub fn task_navigation_summary(tasks: &[TaskRecord]) -> (usize, bool) {
    tasks.iter().fold((0, false), |(count, attention), task| {
        let actionable = matches!(
            task_group(task),
            TaskGroup::Running | TaskGroup::Attention | TaskGroup::Queued
        );
        let needs_person = task.handled_by.is_none()
            && matches!(
                task.state,
                TaskState::NeedsAttention | TaskState::Uncertain | TaskState::Partial
            );
        (count + usize::from(actionable), attention || needs_person)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerWait {
    Starting,
    BetweenStages,
    Finishing,
}

impl WorkerWait {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => course2md::tr!("正在启动任务…"),
            Self::BetweenStages => course2md::tr!("正在继续处理…"),
            Self::Finishing => course2md::tr!("正在完成笔记…"),
        }
    }
}

/// A closed stage is not a completed task: later stages may not have started.
/// Only the final render event or worker result identifies the finishing phase;
/// the task itself is still completed by the existing Exit/manifest path.
/// Pause and cancel are already shown by the status badge and the busy stop
/// button, so a stopping worker reports no separate waiting line.
pub fn worker_wait_state<'a>(
    active: bool,
    result_received: bool,
    intent: Intent,
    stages: impl IntoIterator<Item = (&'a Stage, bool)>,
) -> Option<WorkerWait> {
    if !active || intent != Intent::Run {
        return None;
    }
    let mut has_stage = false;
    let mut render_done = false;
    for (stage, done) in stages {
        has_stage = true;
        if !done {
            return None;
        }
        render_done |= *stage == Stage::Render;
    }
    Some(if render_done || result_received {
        WorkerWait::Finishing
    } else if has_stage {
        WorkerWait::BetweenStages
    } else {
        WorkerWait::Starting
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageStatus {
    Running,
    Complete,
    Pausing,
    Stopping,
    Paused,
    Waiting,
    Failed,
    Uncertain,
    Cancelled,
}

/// AI counters describe dispatched batches, including requests still in flight.
/// Only the worker's stage result can move these stages into completed history.
/// A returned response is the live signal instead, without inventing a fraction.
pub fn task_stage_progress(
    name: &Stage,
    done: bool,
    live: Option<&activity::Activity>,
    current: u64,
    total: u64,
) -> (String, Option<f32>) {
    stage_progress(name, done, live, current, total, false)
}

/// A stage's measurement: in full for the task details, or `compact` for a
/// one-line card status, which leaves out supplementary context.
pub fn stage_progress(
    name: &Stage,
    done: bool,
    live: Option<&activity::Activity>,
    current: u64,
    total: u64,
    compact: bool,
) -> (String, Option<f32>) {
    // AI requests have no measurable fraction; their clock and returned
    // tokens show them moving, and stay as the record once they finish.
    if name.is_ai() {
        return (
            live.map(|live| live.live_counter()).unwrap_or_default(),
            None,
        );
    }
    let detail = if !done && let Some(live) = live {
        if compact {
            live.compact_detail(name, true)
        } else {
            live.detail(name, true)
        }
    } else if !done
        || matches!(
            name,
            Stage::ScenesScan | Stage::ScenesExtract | Stage::Transcribe
        )
        || name.is_model_transfer()
    {
        activity::quantity(name, current, total)
    } else {
        String::new()
    };
    // A finished measured stage keeps a full bar, so the stages read as a
    // growing stack rather than one bar replacing another.
    let fraction = if done {
        (total > 0).then_some(1.)
    } else {
        live.and_then(|stage| stage.fraction())
    };
    // Unmeasured running work shows its clock instead of a bar. Without a
    // total, the stage's own metrics end in an elapsed note; the live counter
    // carries that clock once, with any returned tokens.
    let detail = match live.filter(|_| !done && fraction.is_none()) {
        Some(live) => {
            let mut metrics = live.transfer_metrics(name, true);
            if matches!(metrics.eta, Some(activity::TransferEta::Note(_))) {
                metrics.eta = None;
            }
            let rest = if compact {
                metrics.compact_summary()
            } else {
                metrics.summary()
            };
            if rest.is_empty() {
                live.live_counter()
            } else {
                format!("{rest} · {}", live.live_counter())
            }
        }
        None => detail,
    };
    (detail, fraction)
}

/// Proofreading and summary are the AI components; remote request purposes
/// use the same names.
pub fn ai_component(name: &str) -> Option<Component> {
    Component::parse(name)
        .filter(|part| matches!(part, Component::Proofreading | Component::Summary))
}

/// A proofreading stage can close after failed requests. Published component
/// outcomes distinguish an ended attempt from a successfully completed step.
pub fn ai_stage_outcome(
    name: &Stage,
    outcomes: Option<&course2md::artifact::Outcomes>,
) -> Option<StageStatus> {
    let outcome = match name {
        Stage::Llm => &outcomes?.proofreading,
        Stage::Translate => &outcomes?.translation,
        Stage::Summary => &outcomes?.summary,
        _ => return None,
    };
    match outcome.status {
        course2md::artifact::Status::Succeeded => Some(StageStatus::Complete),
        course2md::artifact::Status::Failed | course2md::artifact::Status::Partial => {
            Some(StageStatus::Failed)
        }
        _ => None,
    }
}

/// User-initiated stops are normal states shown by the status badge, and a
/// partial result names each unfinished component beside its retry action.
/// Keep worker diagnostics in raw logs, and retain only a short, actionable
/// localized failure summary in the task.
pub fn task_failure_summary(state: TaskState, error: Option<&str>) -> Option<String> {
    if matches!(
        state,
        TaskState::Paused | TaskState::Cancelled | TaskState::Partial
    ) {
        return None;
    }
    error.map(|error| {
        let summary = error
            .lines()
            .map(str::trim)
            .find(|line| {
                !line.is_empty()
                    && !line.starts_with("ERROR:")
                    && !line.starts_with("WARNING:")
                    && !line.starts_with("Traceback")
                    && !line.starts_with("panic")
            })
            .unwrap_or_default();
        if summary.chars().count() <= 140 {
            summary.into()
        } else {
            course2md::tr!("转换未完成").into()
        }
    })
}

pub fn task_attention_summary(task: &TaskRecord) -> String {
    if task.has_uncertain() {
        return task_status_label(task);
    }
    if let Some(reason) = task_service_repair_reason(task) {
        return reason.into();
    }
    task_failure_summary(task.state, task.events.error.as_deref())
        .unwrap_or_else(|| task_status_label(task))
}

/// Only concrete configuration rejections change recovery from Retry to Repair.
/// Rate limiting, timeouts and unknown outcomes keep their own recovery.
pub fn service_repair_reason(kind: course2md::artifact::FailureKind) -> Option<&'static str> {
    match kind {
        course2md::artifact::FailureKind::Authentication => {
            Some(course2md::tr!("AI 服务拒绝了 API Key，请检查密钥或权限"))
        }
        course2md::artifact::FailureKind::Model => {
            Some(course2md::tr!("AI 服务地址或模型名称有误"))
        }
        course2md::artifact::FailureKind::Parameters => {
            Some(course2md::tr!("AI 服务不支持当前的模型设置"))
        }
        _ => None,
    }
}

pub fn task_service_repair_reason(task: &TaskRecord) -> Option<&'static str> {
    if task.handled_by.is_some() || task.has_uncertain() {
        return None;
    }
    let outcomes = task.events.outcomes.as_ref()?;
    let components = [
        &outcomes.proofreading,
        &outcomes.translation,
        &outcomes.summary,
    ]
    .into_iter()
    .filter(|outcome| {
        matches!(
            outcome.status,
            course2md::artifact::Status::Failed | course2md::artifact::Status::Partial
        )
    });
    let blocked = task
        .events
        .blocked
        .iter()
        .filter_map(|request| ai_component(request.purpose.as_deref()?).and(request.kind));
    components
        .filter_map(|outcome| outcome.kind)
        .chain(blocked)
        .find_map(service_repair_reason)
}

pub fn task_requires_service_repair(task: &TaskRecord) -> bool {
    task_service_repair_reason(task).is_some()
}

/// Component key → retry button label; the one source shared by task cards, the library and the reader.
pub fn retry_component_label(component: &str) -> &'static str {
    match Component::parse(component) {
        Some(Component::Screenshots) => course2md::tr!("重新生成截图"),
        Some(Component::Proofreading) => course2md::tr!("重试 AI 校对"),
        Some(Component::Summary) => course2md::tr!("重新生成摘要"),
        Some(Component::Exports) => course2md::tr!("重新导出"),
        None => course2md::tr!("补全未完成部分"),
    }
}

/// Retry button label: one component is named; several read as 「补全未完成部分」.
pub fn retry_components_label(components: &[String]) -> &'static str {
    if components.len() == 1 {
        retry_component_label(&components[0])
    } else {
        course2md::tr!("补全未完成部分")
    }
}

/// Which controls a task's commands are drawn into.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskSurface {
    /// Every command, on right-click and behind ⋯.
    Menu,
    /// The buttons of a card on the task page.
    Card,
    /// The buttons of the card standing in for the video on the workbench.
    Home,
}

/// One thing a person can do to a task. [`task_actions`] says which apply;
/// every button and menu item is drawn from it.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskAction {
    Read,
    OpenNoteFolder,
    OpenExports,
    OpenSource,
    CopyLink,
    Details,
    Log,
    Followup(String),
    /// A move within the queue; `to` is `None` where it would change nothing.
    Move {
        label: &'static str,
        up: bool,
        to: Option<usize>,
    },
    Pause,
    Resume,
    Repair,
    Retry(String),
    Reassociate(String),
    Resend,
    Adjust,
    Cancel,
    ViewTask,
    NextVideo,
}

/// What a person came to the task for: where a card puts its one filled button.
pub fn lead_action<'a>(
    actions: &'a [TaskAction],
    task: &TaskRecord,
    surface: TaskSurface,
) -> Option<&'a TaskAction> {
    let recovery = || {
        actions.iter().find(|action| {
            matches!(
                action,
                TaskAction::Repair | TaskAction::Resume | TaskAction::Retry(_)
            )
        })
    };
    let home = surface == TaskSurface::Home;
    let read = (home && task.state == TaskState::Partial)
        .then(|| actions.iter().find(|action| **action == TaskAction::Read))
        .flatten();
    // A running task has nothing to decide, so its next step is the next video.
    let working = runs_in_card(task);
    read.or_else(recovery).or_else(|| {
        actions
            .iter()
            .find(|action| home && working && **action == TaskAction::NextVideo)
    })
}

/// A task still working carries the way to the next video on its card; otherwise it
/// sits right under the options panel's header.
pub fn runs_in_card(task: &TaskRecord) -> bool {
    !task.state.finished() && task.state != TaskState::Uncertain && !task.has_uncertain()
}

/// Why a task needs the person: each unfinished part of a partial note, or its one reason.
pub fn task_reasons(task: &TaskRecord) -> Vec<String> {
    let failures = task_note_failures(task);
    if task.state != TaskState::Partial || failures.is_empty() {
        return task_service_repair_reason(task)
            .map(str::to_owned)
            .or_else(|| {
                task_failure_summary(task.state, task.events.error.as_deref())
                    .filter(|_| !task.has_uncertain())
            })
            .into_iter()
            .collect();
    }
    let repair = task_requires_service_repair(task);
    let unconfirmed = |component: &str| {
        task.events.blocked.iter().any(|request| {
            request.is_uncertain()
                && request
                    .purpose
                    .as_deref()
                    .and_then(ai_component)
                    .is_some_and(|part| part.as_str() == component)
        })
    };
    task_service_repair_reason(task)
        .map(str::to_owned)
        .into_iter()
        .chain(
            failures
                .iter()
                .filter(|(component, ..)| !repair || ai_component(component).is_none())
                .filter(|(component, ..)| !unconfirmed(component))
                .map(|(_, part, outcome)| activity::outcome_failure_message(*part, outcome)),
        )
        .collect()
}

/// The unfinished parts of a saved note; a task with no note has none.
pub fn task_note_failures(
    task: &TaskRecord,
) -> Vec<(String, activity::NotePart, course2md::artifact::Outcome)> {
    if task.artifact.is_some() {
        task_component_failures(task)
    } else {
        Vec::new()
    }
}

/// A paused task that can continue as it is.
pub fn resumable(task: &TaskRecord) -> bool {
    task.state == TaskState::Paused && task.handled_by.is_none() && !task.has_uncertain()
}

pub fn adjust_label(task: &TaskRecord) -> &'static str {
    if task.artifact.is_some() {
        course2md::tr!("调整并生成新版")
    } else if task.state == TaskState::Paused {
        course2md::tr!("调整选项")
    } else {
        course2md::tr!("调整后重试")
    }
}

/// The worker's log, failure and blocked-request text, when there is any.
pub fn task_log_text(task: &TaskRecord) -> Option<String> {
    let text = task
        .events
        .logs
        .iter()
        .chain(task.events.error.iter())
        .chain(task.events.blocked.iter().map(|request| &request.message))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

pub fn task_text_source(task: &TaskRecord) -> &'static str {
    if task.plan.options().source_mode != TranscriptSource::Asr
        && task.plan.source.selected_subtitle.is_some()
    {
        course2md::tr!("视频字幕")
    } else if task.plan.options().uses_cloud_provider() {
        course2md::tr!("语音识别服务")
    } else {
        course2md::tr!("本机识别")
    }
}

pub fn task_status_label(task: &TaskRecord) -> String {
    if task.handled_by.is_some() {
        return course2md::tr!("已有后续处理任务").into();
    }
    if task.has_uncertain() {
        let purpose: std::collections::BTreeSet<_> = task
            .events
            .blocked
            .iter()
            .filter(|request| request.is_uncertain())
            .filter_map(|request| request.purpose.as_deref())
            .collect();
        // The 阅读笔记 action shows whether the note is readable.
        let all = |component: Component| {
            !purpose.is_empty()
                && purpose
                    .iter()
                    .all(|purpose| ai_component(purpose) == Some(component))
        };
        return if all(Component::Proofreading) {
            course2md::tr!("AI 校对结果未确认")
        } else if all(Component::Summary) {
            course2md::tr!("摘要结果未确认")
        } else if purpose.contains(TRANSCRIPTION_PURPOSE) {
            course2md::tr!("语音识别结果未确认")
        } else {
            course2md::tr!("部分处理结果未确认")
        }
        .into();
    }
    if task.exports_only() && task.state == TaskState::Complete {
        course2md::tr!("文件已导出").into()
    } else {
        task.status_label().into()
    }
}

pub fn request_scope(request: &workspace::BlockedRequest) -> String {
    if !request.description.trim().is_empty() {
        request.description.clone()
    } else {
        let purpose = request.purpose.as_deref().unwrap_or_default();
        let name = match ai_component(purpose) {
            Some(Component::Proofreading) => course2md::tr!("AI 校对"),
            Some(Component::Summary) => course2md::tr!("生成摘要"),
            _ if purpose == TRANSCRIPTION_PURPOSE => course2md::tr!("语音识别"),
            _ => course2md::tr!("外部处理"),
        };
        course2md::tr!("{}（具体内容未记录）", name)
    }
}

/// Unfinished parts of a published note: (component, label, outcome).
pub fn task_component_failures(
    task: &TaskRecord,
) -> Vec<(String, activity::NotePart, course2md::artifact::Outcome)> {
    let Some(outcomes) = &task.events.outcomes else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for (component, part, outcome) in [
        (
            Component::Screenshots,
            activity::NotePart::Screenshots,
            &outcomes.screenshots,
        ),
        (
            Component::Proofreading,
            activity::NotePart::Proofreading,
            &outcomes.proofreading,
        ),
        (
            Component::Summary,
            activity::NotePart::Summary,
            &outcomes.summary,
        ),
    ] {
        if matches!(
            outcome.status,
            course2md::artifact::Status::Failed | course2md::artifact::Status::Partial
        ) {
            results.push((component.as_str().into(), part, outcome.clone()));
        }
    }
    {
        let failures: Vec<course2md::artifact::Outcome> = outcomes
            .exports
            .values()
            .filter(|outcome| {
                matches!(
                    outcome.status,
                    course2md::artifact::Status::Failed | course2md::artifact::Status::Partial
                )
            })
            .cloned()
            .collect();
        if !failures.is_empty() {
            results.push((
                Component::Exports.as_str().into(),
                activity::NotePart::Exports,
                course2md::artifact::Outcome::failed(
                    failures
                        .iter()
                        .filter_map(|o| o.message.clone())
                        .collect::<Vec<_>>()
                        .join(course2md::tr!("；")),
                ),
            ));
        }
    }
    results
}

impl TaskAction {
    /// Its menu section.
    pub fn group(&self) -> u8 {
        match self {
            Self::Read
            | Self::OpenNoteFolder
            | Self::OpenExports
            | Self::OpenSource
            | Self::CopyLink => 0,
            Self::Details | Self::Log | Self::Followup(_) | Self::ViewTask | Self::NextVideo => 1,
            Self::Move { .. } => 2,
            Self::Cancel => 4,
            _ => 3,
        }
    }
}

impl StageStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => course2md::tr!("处理中"),
            Self::Complete => course2md::tr!("已完成"),
            Self::Pausing => course2md::tr!("暂停中"),
            Self::Stopping => course2md::tr!("停止中"),
            Self::Paused => course2md::tr!("已暂停"),
            Self::Waiting => course2md::tr!("等待继续"),
            Self::Failed => course2md::tr!("未完成"),
            Self::Uncertain => course2md::tr!("待确认"),
            Self::Cancelled => course2md::tr!("已取消"),
        }
    }

    pub fn running(self) -> bool {
        matches!(self, Self::Running | Self::Pausing | Self::Stopping)
    }
}

/// One step of a task, as a row or a chip says it.
pub struct TaskStage {
    pub id: String,
    pub title: String,
    pub status: StageStatus,
    pub detail: String,
    /// The measurement as one line for a compact card.
    pub compact_detail: String,
    pub fraction: Option<f32>,
}

/// What a task's actions depend on beyond its own record.
pub struct TaskFacts<'a> {
    /// It holds the running job.
    pub active: bool,
    /// Its details are open.
    pub expanded: bool,
    pub library: Option<&'a workspace::LibraryLocation>,
    pub needs_reassociation: bool,
    /// The waiting tasks, in order.
    pub queue: Vec<&'a str>,
}

/// What can be done to `task` right now, in the order `surface` draws it.
pub fn task_actions(
    task: &TaskRecord,
    surface: TaskSurface,
    facts: &TaskFacts<'_>,
) -> Vec<TaskAction> {
    use TaskAction::*;
    let active = facts.active;
    let uncertain = task.has_uncertain();
    let running = matches!(task.state, TaskState::Running | TaskState::Pausing);
    let queued = task.state == TaskState::Queued;
    let has_note = task.artifact.is_some();
    let has_log = task_log_text(task).is_some();
    let mut recovery = Vec::new();
    if let Some(next) = &task.handled_by {
        recovery.push(Followup(next.clone()));
    } else {
        let repair = task_requires_service_repair(task);
        if repair && has_note {
            recovery.push(Repair);
        }
        if matches!(task.state, TaskState::Paused | TaskState::NeedsAttention) && !uncertain {
            recovery.push(Resume);
        }
        if !uncertain {
            recovery.extend(
                task_note_failures(task)
                    .into_iter()
                    .filter(|(component, ..)| !repair || ai_component(component).is_none())
                    .map(|(component, ..)| Retry(component)),
            );
        }
        if let Some(library) = facts.library.filter(|_| facts.needs_reassociation) {
            recovery.push(Reassociate(library.id.clone()));
        }
        if !running && !queued {
            recovery.push(Adjust);
        }
    }
    let mut actions = Vec::new();
    match surface {
        TaskSurface::Menu => {
            if has_note {
                actions.extend([Read, OpenNoteFolder]);
            }
            if facts
                .library
                .is_some_and(|location| !workspace::task_export_files(task, location).is_empty())
            {
                actions.push(OpenExports);
            }
            if task.plan.source.online {
                actions.extend([OpenSource, CopyLink]);
            }
            if !running {
                actions.push(Details);
            }
            if has_log {
                actions.push(Log);
            }
            if task.handled_by.is_some() {
                actions.extend(recovery);
                return actions;
            }
            actions.extend(task_moves(task, &facts.queue));
            if (running && task.intent != Intent::Cancel) || (queued && !active) {
                actions.push(Pause);
            }
            actions.extend(recovery);
            if uncertain && !active {
                actions.push(Resend);
            }
            if !task.state.finished() {
                actions.push(Cancel);
            }
        }
        TaskSurface::Card if running || (queued && active) => {
            if has_log {
                actions.push(Log);
            }
            actions.push(Cancel);
            if task.intent != Intent::Cancel {
                actions.push(Pause);
            }
        }
        // A queued task has produced nothing to keep, so it offers only cancel.
        TaskSurface::Card if queued => actions.push(Cancel),
        TaskSurface::Card => {
            actions.extend(recovery);
            if has_note {
                actions.push(Read);
            }
            actions.push(Details);
            if has_log && facts.expanded {
                actions.push(Log);
            }
        }
        TaskSurface::Home => {
            if running || queued {
                actions.push(Cancel);
            } else {
                if has_note {
                    actions.push(Read);
                }
                // The rest stays behind ⋯: this card is one step of the workbench.
                if recovery.len() > 1 {
                    recovery.retain(|action| *action != Adjust);
                }
                actions.extend(recovery);
            }
            actions.push(ViewTask);
            if runs_in_card(task) {
                actions.push(NextVideo);
            }
        }
    }
    actions
}

/// Moves within the queue, for those who don't drag.
fn task_moves(task: &TaskRecord, queue: &[&str]) -> Vec<TaskAction> {
    let Some(position) = queue.iter().position(|queued| *queued == task.id) else {
        return Vec::new();
    };
    let last = queue.len() - 1;
    let step = |label, up, to, can: bool| TaskAction::Move {
        label,
        up,
        to: can.then_some(to),
    };
    vec![
        step(course2md::tr!("移到最前"), true, 0, position > 0),
        step(
            course2md::tr!("上移"),
            true,
            position.saturating_sub(1),
            position > 0,
        ),
        step(course2md::tr!("下移"), false, position + 1, position < last),
        step(course2md::tr!("移到最后"), false, last, position < last),
    ]
}

/// Live and retained stages use the same labels, status column and icon slot.
pub fn task_stage_rows(task: &TaskRecord, active: bool) -> Vec<TaskStage> {
    // `Stage` orders by pipeline position, so the map is already sorted.
    task.events
        .stages
        .iter()
        .map(|(name, stored)| {
            let name = name.clone();
            let live = active
                .then_some(stored)
                .filter(|stage| !stage.elapsed().is_zero());
            let done = stored.done;
            let outcomes = if active {
                task.events
                    .done
                    .as_ref()
                    .and_then(|done| done.outcomes.as_ref())
            } else {
                task.events.outcomes.as_ref()
            };
            let status = if let Some(outcome) = ai_stage_outcome(&name, outcomes) {
                outcome
            } else if done {
                StageStatus::Complete
            } else if active {
                match task.intent {
                    Intent::Pause | Intent::Quit => StageStatus::Pausing,
                    Intent::Cancel => StageStatus::Stopping,
                    Intent::Run => StageStatus::Running,
                }
            } else {
                match task.state {
                    TaskState::Paused | TaskState::Pausing => StageStatus::Paused,
                    TaskState::Uncertain => StageStatus::Uncertain,
                    TaskState::Cancelled => StageStatus::Cancelled,
                    TaskState::Queued => StageStatus::Waiting,
                    _ => StageStatus::Failed,
                }
            };
            let (current, total) = (stored.current, stored.total);
            let (detail, fraction) = task_stage_progress(&name, done, live, current, total);
            let (compact_detail, _) = stage_progress(&name, done, live, current, total, true);
            TaskStage {
                id: format!("task-stage-{}-{name}", task.id),
                title: activity::title(&name),
                status,
                detail,
                compact_detail,
                fraction,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        StageStatus, WorkerWait, ai_stage_outcome, task_failure_summary, task_stage_progress,
        worker_wait_state,
    };
    use crate::plan::tests::source;
    use course2md::progress::Stage;

    fn task_record(id: &str) -> crate::workspace::TaskRecord {
        crate::workspace::TaskRecord {
            id: id.into(),
            plan: crate::workspace::TaskPlan {
                operation: Default::default(),
                source: source("https://example.test/video", "课程"),
                source_id: "online:video".into(),
                title: "课程".into(),
                library_id: "library".into(),
                folder: None,
                subtitle: None,
                config: Default::default(),
                asr_service: None,
                ai_service: None,
            },
            state: crate::workspace::TaskState::Paused,
            intent: crate::workspace::Intent::Run,
            created: 0,
            updated: 0,
            parent: None,
            handled_by: None,
            work_dir: "work".into(),
            events: crate::activity::JobEvents {
                error: None,
                ..Default::default()
            },
            artifact: None,
            unread: false,
            resend: Vec::new(),
        }
    }

    use crate::workspace::{Intent, TaskState};

    #[test]
    fn a_waiting_task_offers_the_moves_its_place_allows() {
        use super::{TaskAction, task_moves};
        let task = task_record("b");
        let moves = |queue: &[&str]| -> Vec<Option<usize>> {
            task_moves(&task, queue)
                .into_iter()
                .map(|action| match action {
                    TaskAction::Move { to, .. } => to,
                    _ => panic!("only moves"),
                })
                .collect()
        };
        assert_eq!(
            moves(&["a", "b", "c"]),
            [Some(0), Some(0), Some(2), Some(2)]
        );
        assert_eq!(moves(&["b", "c"]), [None, None, Some(1), Some(1)]);
        assert!(moves(&["a"]).is_empty());
    }

    #[test]
    fn one_action_is_filled_and_an_unconfirmed_request_leaves_the_card_without_one() {
        use super::{TaskAction as Action, TaskSurface, lead_action};
        let mut task = task_record("task");
        task.state = TaskState::Partial;
        let home = [Action::Read, Action::Repair, Action::NextVideo];
        assert_eq!(
            lead_action(&home, &task, TaskSurface::Home),
            Some(&Action::Read)
        );
        assert_eq!(
            lead_action(&home, &task, TaskSurface::Card),
            Some(&Action::Repair)
        );
        task.state = TaskState::Running;
        let running = [Action::Cancel, Action::ViewTask, Action::NextVideo];
        assert_eq!(
            lead_action(&running, &task, TaskSurface::Home),
            Some(&Action::NextVideo)
        );
        // The resend button in the card's own block is that decision's one main action.
        task.state = TaskState::Uncertain;
        let uncertain = [Action::Adjust, Action::ViewTask, Action::NextVideo];
        assert_eq!(lead_action(&uncertain, &task, TaskSurface::Home), None);
    }

    #[test]
    fn a_closed_intermediate_stage_does_not_claim_the_task_is_finishing() {
        assert_eq!(
            worker_wait_state(true, false, Intent::Run, []),
            Some(WorkerWait::Starting)
        );
        assert_eq!(
            worker_wait_state(
                true,
                false,
                Intent::Run,
                [(&Stage::parse("subtitle"), true)]
            ),
            Some(WorkerWait::BetweenStages),
        );
        assert_eq!(
            worker_wait_state(
                true,
                false,
                Intent::Run,
                [
                    (&Stage::parse("subtitle"), true),
                    (&Stage::parse("download"), false)
                ]
            ),
            None,
        );
        assert_eq!(
            worker_wait_state(
                true,
                false,
                Intent::Run,
                [
                    (&Stage::parse("subtitle"), true),
                    (&Stage::parse("download"), true)
                ]
            ),
            Some(WorkerWait::BetweenStages),
        );
    }

    #[test]
    fn finishing_uses_worker_evidence_and_stops_with_the_process() {
        let stages = [
            (&Stage::parse("subtitle"), true),
            (&Stage::parse("render"), true),
        ];
        assert_eq!(
            worker_wait_state(true, false, Intent::Run, stages),
            Some(WorkerWait::Finishing),
        );
        // Reusing a complete artifact can emit Done without any new stages.
        assert_eq!(
            worker_wait_state(true, true, Intent::Run, []),
            Some(WorkerWait::Finishing),
        );
        // A retry/remaining active stage keeps its real progress visible.
        assert_eq!(
            worker_wait_state(
                true,
                false,
                Intent::Run,
                [
                    (&Stage::parse("render"), true),
                    (&Stage::parse("summary"), false)
                ]
            ),
            None,
        );
        assert_eq!(worker_wait_state(false, true, Intent::Run, stages), None);
        assert_eq!(worker_wait_state(false, false, Intent::Run, []), None);
    }

    #[test]
    fn ai_dispatch_counts_do_not_claim_received_results() {
        for name in [&Stage::Llm, &Stage::Summary] {
            let mut activity = crate::activity::Activity::new();
            for (current, total) in [(0, 3), (1, 3), (3, 3), (1, 1)] {
                activity.update(current, total, None);
                let (detail, fraction) =
                    task_stage_progress(name, activity.done, Some(&activity), current, total);
                assert!(fraction.is_none());
                assert!(!detail.contains('%'));
                assert!(!detail.contains("收尾"));
                assert!(!activity.done);
            }
            activity.finish();
            // The finished stage keeps its clock as the record, still without a fraction.
            let (detail, fraction) = task_stage_progress(name, true, Some(&activity), 1, 1);
            assert_eq!(detail, "用时 0 秒");
            assert!(fraction.is_none());
        }
        // Quantities that measure completed work keep their determinate progress.
        let mut screenshots = crate::activity::Activity::new();
        screenshots.update(2, 6, None);
        let (detail, fraction) = task_stage_progress(
            &Stage::parse("scenes/extract"),
            false,
            Some(&screenshots),
            2,
            6,
        );
        assert!(detail.contains("2 / 6"));
        assert_eq!(fraction, Some(2. / 6.));
    }

    #[test]
    fn pause_and_cancel_intents_do_not_report_continued_generation() {
        for intent in [Intent::Pause, Intent::Quit, Intent::Cancel] {
            assert_eq!(worker_wait_state(true, false, intent, []), None);
            assert_eq!(
                worker_wait_state(true, false, intent, [(&Stage::parse("llm"), true)]),
                None
            );
            assert_eq!(
                worker_wait_state(true, true, intent, [(&Stage::parse("render"), true)]),
                None
            );
            assert_eq!(worker_wait_state(false, false, intent, []), None);
        }
    }

    #[test]
    fn completed_ai_attempts_use_component_results_to_classify_success() {
        use course2md::artifact::{Outcome, Outcomes, Status};
        let mut outcomes = Outcomes {
            proofreading: Outcome::failed("服务拒绝凭据"),
            summary: Outcome::succeeded(),
            ..Default::default()
        };
        let value = outcomes.clone();
        assert_eq!(
            ai_stage_outcome(&Stage::parse("llm"), Some(&value)),
            Some(StageStatus::Failed)
        );
        assert_eq!(
            ai_stage_outcome(&Stage::parse("summary"), Some(&value)),
            Some(StageStatus::Complete)
        );
        outcomes.proofreading.status = Status::Partial;
        let value = outcomes.clone();
        assert_eq!(
            ai_stage_outcome(&Stage::parse("llm"), Some(&value)),
            Some(StageStatus::Failed)
        );
        outcomes.proofreading = Outcome::succeeded();
        let value = outcomes.clone();
        assert_eq!(
            ai_stage_outcome(&Stage::parse("llm"), Some(&value)),
            Some(StageStatus::Complete)
        );
        assert_eq!(ai_stage_outcome(&Stage::parse("llm"), None), None);
        assert_eq!(
            ai_stage_outcome(&Stage::parse("scenes/extract"), Some(&value)),
            None
        );
    }

    #[test]
    fn user_stops_have_no_feedback_and_worker_failures_have_a_localized_summary() {
        let paused = "生成已暂停，进度已保留 / Generation paused; progress retained";
        assert_eq!(task_failure_summary(TaskState::Paused, Some(paused)), None);
        assert_eq!(
            task_failure_summary(TaskState::Cancelled, Some("任务已取消 / Task cancelled")),
            None
        );
        assert_eq!(
            task_failure_summary(
                TaskState::NeedsAttention,
                Some("服务拒绝凭据，请检查 API Key\nraw details")
            ),
            Some("服务拒绝凭据，请检查 API Key".into()),
        );
        let Some(message) = task_failure_summary(
            TaskState::NeedsAttention,
            Some("ERROR: request failed\nTraceback: provider transport details"),
        ) else {
            panic!("a failed task needs a user-facing summary");
        };
        assert!(!message.contains("ERROR"));
        assert!(!message.contains("Traceback"));
    }

    #[test]
    fn partial_task_recovery_excludes_successful_and_unrequested_components() {
        use crate::workspace::{Intent, TaskPlan, TaskRecord, TaskState};
        use course2md::artifact::{Outcome, Outcomes, Status};

        let mut outcomes = Outcomes {
            transcript: Outcome::succeeded(),
            screenshots: Outcome::succeeded(),
            ..Default::default()
        };
        outcomes.proofreading = Outcome {
            status: Status::Partial,
            message: Some("服务拒绝凭据".into()),
            completed: Some(1),
            total: Some(2),
            kind: Some(course2md::artifact::FailureKind::Authentication),
        };
        outcomes.exports.insert("md".into(), Outcome::succeeded());
        outcomes
            .exports
            .insert("json".into(), Outcome::not_requested());
        outcomes
            .exports
            .insert("html".into(), Outcome::failed("HTML 写入失败"));
        let path = std::path::PathBuf::from("published-note");
        let mut task = TaskRecord {
            id: "partial-task".into(),
            plan: TaskPlan {
                operation: Default::default(),
                source: source("https://example.test/video", "课程"),
                source_id: "online:video".into(),
                title: "课程".into(),
                library_id: "library".into(),
                folder: None,
                subtitle: None,
                config: Default::default(),
                asr_service: None,
                ai_service: None,
            },
            state: TaskState::Partial,
            intent: Intent::Run,
            created: 0,
            updated: 0,
            parent: None,
            handled_by: None,
            work_dir: "work".into(),
            events: crate::activity::JobEvents {
                outcomes: Some(outcomes.clone()),
                ..Default::default()
            },
            artifact: Some(path.clone()),
            unread: false,
            resend: Vec::new(),
        };
        // The first recovery action in Recent Notes and every action in the
        // task cards must target actual incomplete work, never the first success.
        let failures = super::task_component_failures(&task);
        assert_eq!(
            failures
                .iter()
                .map(|(key, _, _)| key.as_str())
                .collect::<Vec<_>>(),
            vec!["proofreading", "exports"]
        );
        assert_eq!(failures[0].2.status, Status::Partial);
        assert_eq!(failures[0].2.completed, Some(1));
        assert_eq!(failures[1].2.message.as_deref(), Some("HTML 写入失败"));

        for (message, kind, requires_repair) in [
            (
                "服务未接受此任务保存的凭据",
                course2md::artifact::FailureKind::Authentication,
                true,
            ),
            (
                "服务未找到此任务指定的接口或模型",
                course2md::artifact::FailureKind::Model,
                true,
            ),
            (
                "服务拒绝了请求参数（HTTP 422）",
                course2md::artifact::FailureKind::Parameters,
                true,
            ),
            (
                "服务请求次数达到限制，请稍后重试。",
                course2md::artifact::FailureKind::RateLimit,
                false,
            ),
            (
                "服务未完成此请求（HTTP 500）。",
                course2md::artifact::FailureKind::Service,
                false,
            ),
            (
                "请求超时，请稍后重试。",
                course2md::artifact::FailureKind::Other,
                false,
            ),
        ] {
            outcomes.proofreading.message = Some(message.into());
            outcomes.proofreading.kind = Some(kind);
            task.events.outcomes = Some(outcomes.clone());
            assert_eq!(
                super::task_requires_service_repair(&task),
                requires_repair,
                "{message}"
            );
        }
        outcomes.proofreading.message = Some("服务拒绝凭据".into());
        outcomes.proofreading.kind = Some(course2md::artifact::FailureKind::Authentication);
        task.events.outcomes = Some(outcomes.clone());
        task.events.blocked.push(crate::workspace::BlockedRequest {
            reason: course2md::progress::BlockReason::Uncertain,
            request_id: Some("pending".into()),
            purpose: Some("summary".into()),
            description: "摘要".into(),
            kind: Some(course2md::artifact::FailureKind::Uncertain),
            message: "结果尚未确认".into(),
        });
        assert!(!super::task_requires_service_repair(&task));
        task.events.blocked.clear();
        task.handled_by = Some("recovery".into());
        assert!(!super::task_requires_service_repair(&task));
        task.handled_by = None;

        outcomes.proofreading = Outcome::succeeded();
        outcomes.exports.insert("html".into(), Outcome::succeeded());
        task.events.outcomes = Some(outcomes);
        assert!(super::task_component_failures(&task).is_empty());
        task.events.error = Some("服务拒绝凭据".into());
        assert!(!super::task_requires_service_repair(&task));
        task.events.outcomes = None;
        assert!(super::task_component_failures(&task).is_empty());

        let mut completed = task.clone();
        completed.id = "recovered".into();
        completed.state = TaskState::Complete;
        let mut history = task.clone();
        history.id = "original".into();
        history.state = TaskState::Paused;
        history.handled_by = Some(completed.id.clone());
        let mut running = task.clone();
        running.id = "running".into();
        running.state = TaskState::Running;
        assert_eq!(super::task_group(&task), super::TaskGroup::Attention);
        assert_eq!(super::task_group(&history), super::TaskGroup::History);
        assert_eq!(
            super::task_navigation_summary(&[task, completed, history, running]).0,
            2
        );
    }
}
