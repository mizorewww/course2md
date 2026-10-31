//! The task page: one card for every task, and one table of what can be done to it.
use crate::{
    Desktop, Done, Page, activity,
    import_ui::{self, video_cover, video_title},
    notes::Course,
    symbols, ui,
    workspace::{self, Intent, TaskRecord, TaskState},
};
use course2md::config::TranscriptSource;
use course2md::execution::Component;
use course2md::progress::Stage;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit_material::{
    color::ColorRole,
    icons::{Icon, SymbolIcon},
    motion,
    prelude::*,
    tokens::shape::OuterCorners,
};
use std::{path::PathBuf, rc::Rc};

/// Request purpose of a remote speech recognition call.
const TRANSCRIPTION_PURPOSE: &str = "transcription";

/// One row of the virtualized task queue. Task rows carry their index into
/// the task snapshot captured for the frame.
enum QueueItem {
    /// The page heading with its counts; it scrolls with the sections and
    /// shares their column.
    Header,
    Unavailable,
    Empty,
    GroupLabel {
        group: TaskGroup,
        count: usize,
        /// Members waiting to run, as opposed to paused.
        queued: usize,
    },
    Task {
        task: usize,
        place: CardPlace,
        /// The next item shares this one's surface.
        joined: bool,
    },
    /// Folds a long queue after its first rows.
    QueueFold {
        hidden: usize,
        open: Entity<bool>,
    },
    /// Folds finished work and earlier records behind one tile.
    CompletedToggle {
        count: usize,
        open: bool,
        state: Entity<bool>,
    },
}

impl QueueItem {
    /// Identity for the list splice: stable across content updates that keep
    /// the row in place, so measured heights and scroll anchors survive.
    fn key(&self, tasks: &[TaskRecord]) -> String {
        match self {
            QueueItem::Header => "header".to_owned(),
            QueueItem::Unavailable => "unavailable".to_owned(),
            QueueItem::Empty => "empty".to_owned(),
            QueueItem::GroupLabel { group, .. } => format!("group-{group:?}"),
            QueueItem::Task { task, .. } => format!("task-{}", tasks[*task].id),
            QueueItem::QueueFold { .. } => "queue-fold".to_owned(),
            QueueItem::CompletedToggle { .. } => "completed-toggle".to_owned(),
        }
    }
}

/// Where a task's card stands, which decides what it shows without being asked.
#[derive(Clone, Copy)]
pub(crate) enum CardPlace {
    /// A row on its group's surface that folds its details out; `number` is its
    /// place among the waiting tasks.
    Row {
        outer: OuterCorners,
        number: Option<usize>,
    },
    /// A task the person should watch or decide on: running or needing attention.
    Open { outer: OuterCorners },
    /// In the video card's place on the workbench, joined to its options.
    Home,
}

/// Queued rows shown before the rest of the queue folds.
const QUEUE_PREVIEW: usize = 5;
/// The task page's content column, inside the shell's gutters.
const TASK_COLUMN: Rems = rem(832.);
/// Between the page heading and each section.
const TASK_SECTION_GAP: Rems = rem(28.);

/// A queued task being dragged to a new place in the queue.
#[derive(Clone)]
struct QueueDrag {
    id: String,
    title: SharedString,
}

impl Render for QueueDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        AssistChip::new("queue-drag", self.title.clone())
            .elevated(true)
            .icon(symbols::drag_indicator())
    }
}
/// Page order: the work in progress, what waits on the person, what follows,
/// then the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TaskGroup {
    Running,
    Attention,
    Queued,
    Finished,
    History,
}

impl TaskGroup {
    const ORDER: [Self; 5] = [
        Self::Running,
        Self::Attention,
        Self::Queued,
        Self::Finished,
        Self::History,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Running => course2md::tr!("正在处理"),
            Self::Attention => course2md::tr!("需要处理"),
            Self::Queued => course2md::tr!("排队中"),
            Self::Finished => course2md::tr!("已结束"),
            Self::History => course2md::tr!("此前处理记录"),
        }
    }
}

pub(crate) fn task_group(task: &TaskRecord) -> TaskGroup {
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
pub(super) fn task_navigation_summary(tasks: &[TaskRecord]) -> (usize, bool) {
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
enum WorkerWait {
    Starting,
    BetweenStages,
    Finishing,
}

impl WorkerWait {
    fn label(self) -> &'static str {
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
fn worker_wait_state<'a>(
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
enum StageStatus {
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

impl StageStatus {
    fn label(self) -> &'static str {
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

    fn running(self) -> bool {
        matches!(self, Self::Running | Self::Pausing | Self::Stopping)
    }

    fn role(self) -> ColorRole {
        match self {
            Self::Running => ColorRole::PRIMARY,
            Self::Complete => ColorRole::SUCCESS,
            Self::Failed => ColorRole::ERROR,
            Self::Uncertain => ColorRole::WARNING,
            _ => ColorRole::ON_SURFACE_VARIANT,
        }
    }

    fn chip(self) -> StatusKind {
        match self {
            Self::Complete => StatusKind::Success,
            Self::Failed => StatusKind::Danger,
            Self::Uncertain => StatusKind::Warning,
            _ if self.running() => StatusKind::Progress,
            _ => StatusKind::Neutral,
        }
    }

    fn icon(self) -> SymbolIcon {
        match self {
            Self::Running => symbols::refresh(),
            Self::Complete => symbols::check_circle(),
            Self::Paused | Self::Pausing => symbols::pause(),
            Self::Stopping | Self::Cancelled => symbols::close(),
            Self::Failed | Self::Uncertain => symbols::warning(),
            Self::Waiting => symbols::schedule(),
        }
    }
}

struct TaskStage {
    id: SharedString,
    title: String,
    status: StageStatus,
    detail: String,
    /// The measurement as one line for a compact card.
    compact_detail: String,
    fraction: Option<f32>,
}

/// AI counters describe dispatched batches, including requests still in flight.
/// Only the worker's stage result can move these stages into completed history.
/// A returned response is the live signal instead, without inventing a fraction.
fn task_stage_progress(
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
fn stage_progress(
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
pub(crate) fn ai_component(name: &str) -> Option<Component> {
    Component::parse(name)
        .filter(|part| matches!(part, Component::Proofreading | Component::Summary))
}

/// A proofreading stage can close after failed requests. Published component
/// outcomes distinguish an ended attempt from a successfully completed step.
fn ai_stage_outcome(
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
fn task_failure_summary(state: TaskState, error: Option<&str>) -> Option<String> {
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

pub(crate) fn task_attention_summary(task: &TaskRecord) -> String {
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
fn service_repair_reason(kind: course2md::artifact::FailureKind) -> Option<&'static str> {
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

fn task_service_repair_reason(task: &TaskRecord) -> Option<&'static str> {
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

pub(crate) fn task_requires_service_repair(task: &TaskRecord) -> bool {
    task_service_repair_reason(task).is_some()
}

/// Component key → retry button label; the one source shared by task cards, the library and the reader.
pub(crate) fn retry_component_label(component: &str) -> &'static str {
    match Component::parse(component) {
        Some(Component::Screenshots) => course2md::tr!("重新生成截图"),
        Some(Component::Proofreading) => course2md::tr!("重试 AI 校对"),
        Some(Component::Summary) => course2md::tr!("重新生成摘要"),
        Some(Component::Exports) => course2md::tr!("重新导出"),
        None => course2md::tr!("补全未完成部分"),
    }
}

/// Retry button label: one component is named; several read as 「补全未完成部分」.
pub(crate) fn retry_components_label(components: &[String]) -> &'static str {
    if components.len() == 1 {
        retry_component_label(&components[0])
    } else {
        course2md::tr!("补全未完成部分")
    }
}

/// Which controls a task's commands are drawn into.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskSurface {
    /// Every command, on right-click and behind ⋯.
    Menu,
    /// The buttons of a card on the task page.
    Card,
    /// The buttons of the card standing in for the video on the workbench.
    Home,
}

/// One thing a person can do to a task. [`Desktop::task_actions`] says which apply;
/// every button and menu item is drawn from it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TaskAction {
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

impl TaskAction {
    /// Its menu section.
    fn group(&self) -> u8 {
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

    /// Its label and icon, and whether it is unavailable right now.
    fn present(&self, task: &TaskRecord, desktop: &Desktop) -> (&'static str, SymbolIcon, bool) {
        let cancelling = task.state == TaskState::Pausing && task.intent == Intent::Cancel;
        let pausing = task.state == TaskState::Pausing && !cancelling;
        let details = desktop.expanded_task.as_ref() == Some(&task.id);
        let log = desktop.task_logs_open.contains(&task.id);
        use course2md::tr;
        match self {
            Self::Read => (tr!("阅读笔记"), symbols::menu_book(), false),
            Self::OpenNoteFolder => (tr!("打开笔记保存位置"), symbols::folder_open(), false),
            Self::OpenExports => (tr!("打开导出位置"), symbols::folder_open(), false),
            Self::OpenSource => (tr!("打开原视频"), symbols::open_in_new(), false),
            Self::CopyLink => (tr!("复制视频链接"), symbols::content_copy(), false),
            Self::Details if details => (tr!("收起详情"), symbols::keyboard_arrow_up(), false),
            Self::Details => (tr!("查看详情"), symbols::keyboard_arrow_down(), false),
            Self::Log if log => (tr!("收起日志"), symbols::description(), false),
            Self::Log => (tr!("查看日志"), symbols::description(), false),
            Self::Followup(_) => (tr!("查看后续处理任务"), symbols::arrow_forward(), false),
            Self::Move { label, up, to } => (
                label,
                if *up {
                    symbols::arrow_upward()
                } else {
                    symbols::arrow_downward()
                },
                to.is_none(),
            ),
            Self::Pause if pausing => (tr!("正在暂停…"), symbols::pause(), true),
            Self::Pause => (tr!("暂停"), symbols::pause(), false),
            Self::Resume => (tr!("继续生成"), symbols::play_arrow(), false),
            Self::Repair => (tr!("修复 AI 服务并补做"), symbols::settings(), false),
            Self::Retry(component) => (retry_component_label(component), symbols::refresh(), false),
            Self::Reassociate(_) => (
                tr!("重新关联此保存位置"),
                symbols::storage(),
                desktop.storage_ui.busy,
            ),
            Self::Resend => (tr!("重新发送以上内容"), symbols::refresh(), false),
            Self::Adjust => (adjust_label(task), symbols::tune(), false),
            Self::Cancel if cancelling => (tr!("正在取消…"), symbols::close(), true),
            Self::Cancel => (tr!("取消任务"), symbols::close(), false),
            Self::ViewTask => (tr!("查看任务"), symbols::task(), false),
            Self::NextVideo => (tr!("设置下一个视频"), symbols::add(), false),
        }
    }

    fn run(&self, id: &str, this: &mut Desktop, window: &mut Window, cx: &mut Context<Desktop>) {
        let Some(task) = this
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(id))
            .cloned()
        else {
            return;
        };
        let id = id.to_owned();
        match self {
            Self::Read => this.open_task_note(&task, cx),
            Self::OpenNoteFolder => {
                if let Some(path) = &task.artifact {
                    cx.open_with_system(path);
                }
            }
            Self::OpenExports => this.open_task_export_location(id, cx),
            Self::OpenSource => cx.open_url(&task.plan.source.input),
            Self::CopyLink => {
                cx.write_to_clipboard(ClipboardItem::new_string(task.plan.source.input.clone()))
            }
            Self::Details => this.toggle_task_row(&id, cx),
            Self::Log => this.toggle_task_log(&task, cx),
            Self::Followup(next) => this.select_task(next, cx),
            Self::Move { to, .. } => {
                if let Some(to) = to {
                    this.move_waiting_task(&id, *to, cx);
                }
            }
            Self::Pause => this.set_task_intent(id, Intent::Pause, cx),
            Self::Resume => this.set_task_intent(id, Intent::Run, cx),
            Self::Cancel => this.set_task_intent(id, Intent::Cancel, cx),
            Self::Repair => this.repair_task_service(id, window, cx),
            Self::Retry(component) => {
                let part = Component::parse(component).unwrap_or(Component::Proofreading);
                this.reprocess_task(id, vec![part], Vec::new(), cx)
            }
            Self::Reassociate(library) => {
                this.begin_library_reassociation(library.clone(), window, cx)
            }
            // A menu has no room for the consequence, so it is asked here. The card's
            // own button stands beside it and sends directly.
            Self::Resend => {
                let mut question = ui::Confirm::new(
                    course2md::tr!("重新发送以上内容"),
                    course2md::tr!("暂不发送"),
                    course2md::tr!("重新发送以上内容"),
                );
                for request in task.events.blocked.iter().filter(|b| b.is_uncertain()) {
                    question = question.text(request_scope(request));
                }
                question
                    .text(course2md::tr!(
                        "服务可能已处理这些内容，重新发送可能产生额外费用"
                    ))
                    .ask(
                        cx.listener(move |this, _, _, cx| this.resend_uncertain(id.clone(), cx)),
                        window,
                        cx,
                    );
            }
            Self::Adjust => this.adjust_task(id, window, cx),
            Self::ViewTask => {
                this.select_task(&id, cx);
                this.navigate(Page::Task, cx);
            }
            Self::NextVideo => this.set_up_next_video(window, cx),
        }
    }

    fn click(
        self,
        desktop: WeakEntity<Desktop>,
        id: String,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        move |_, window, cx| {
            let _ = desktop.update(cx, |this, cx| self.run(&id, this, window, cx));
        }
    }
}

/// What a person came to the task for: where a card puts its one filled button.
fn lead_action<'a>(
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
    let working =
        !task.state.finished() && task.state != TaskState::Uncertain && !task.has_uncertain();
    read.or_else(recovery).or_else(|| {
        actions
            .iter()
            .find(|action| home && working && **action == TaskAction::NextVideo)
    })
}

/// Why a task needs the person: each unfinished part of a partial note, or its one reason.
fn task_reasons(task: &TaskRecord) -> Vec<String> {
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
fn task_note_failures(
    task: &TaskRecord,
) -> Vec<(String, activity::NotePart, course2md::artifact::Outcome)> {
    if task.artifact.is_some() {
        task_component_failures(task)
    } else {
        Vec::new()
    }
}

/// An icon, a label and its value: one fact about how a task was planned.
pub(crate) fn fact_row(
    id: impl Into<SharedString>,
    icon: SymbolIcon,
    label: &'static str,
    value: String,
) -> Div {
    let scale = TileScale::Compact;
    scale
        .row()
        .child(Icon::from(icon).color(color(ColorRole::ON_SURFACE_VARIANT)))
        .child(scale.text(ElementId::Name(id.into()), label, Some(value.into())))
}

fn stage_row(stage: TaskStage, window: &mut Window, cx: &mut App) -> Div {
    let icon_id = SharedString::from(format!("{}-icon", stage.id));
    let status_id = SharedString::from(format!("{}-status", stage.id));
    let progress_id = SharedString::from(format!("{}-progress", stage.id));
    // A running or finished step reads its measurement; any other state names
    // itself, with what it measured so far.
    let (status, ink) = match stage.status {
        StageStatus::Running => (stage.detail, ColorRole::ON_SURFACE_VARIANT),
        StageStatus::Complete if !stage.detail.is_empty() => {
            (stage.detail, ColorRole::ON_SURFACE_VARIANT)
        }
        status if stage.detail.is_empty() => (status.label().to_owned(), status.role()),
        status => (
            format!("{} · {}", status.label(), stage.detail),
            status.role(),
        ),
    };
    let icon: AnyElement = if stage.status.running() && stage.fraction.is_none() {
        icon_spinner(icon_id, stage.title.clone(), ICON_SLOT, cx)
    } else {
        Icon::from(stage.status.icon())
            .size(ICON_SLOT)
            .color(color(stage.status.role()))
            .into_any_element()
    };
    v_flex()
        .w_full()
        .min_w_0()
        .gap(rem(8.))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap(rem(16.))
                .child(
                    semantic_label(stage.id.clone(), stage.title, icon)
                        // The name keeps one line; the measurement wraps instead.
                        .whitespace_nowrap()
                        .flex_shrink_0(),
                )
                .child(
                    ui::status_line(status_id, &status)
                        .flex_1()
                        .justify_end()
                        .text_size(TEXT_BODY)
                        .text_color(color(ink)),
                ),
        )
        .when_some(stage.fraction, |row, fraction| {
            row.child(
                div()
                    .pl(LABEL_INSET)
                    .child(progress(progress_id, fraction, window, cx)),
            )
        })
}

fn log_box(id: &str, text: String) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("task-log-{id}")))
        .w_full()
        .min_w_0()
        .max_h(rem(224.))
        .overflow_y_scroll()
        .p(rem(16.))
        .rounded(RADIUS_CARD)
        .bg(color(ColorRole::INSET))
        .child(
            accessible_text(SharedString::from(format!("task-log-text-{id}")), text)
                .text_size(TEXT_AUX)
                .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                .whitespace_normal(),
        )
}

impl Desktop {
    pub fn queue_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rem_size = f32::from(window.rem_size());
        let mut items = Vec::new();
        let tasks: Rc<Vec<TaskRecord>> = match &self.workspace {
            None => {
                items.push(QueueItem::Unavailable);
                self.queue_cache = None;
                Rc::new(Vec::new())
            }
            Some(workspace) if workspace.state.tasks.is_empty() => {
                items.push(QueueItem::Empty);
                self.queue_cache = None;
                Rc::new(Vec::new())
            }
            Some(workspace) => {
                if let Some((revision, tasks)) = &self.queue_cache
                    && *revision == workspace.state.revision
                {
                    Rc::clone(tasks)
                } else {
                    // Running and queued work keeps its run order; the rest
                    // shows the newest first.
                    let mut tasks: Vec<_> =
                        workspace.state.tasks.iter().cloned().enumerate().collect();
                    tasks.sort_by_key(|(index, task)| {
                        let group = task_group(task);
                        let order = match group {
                            TaskGroup::Running | TaskGroup::Queued => *index as i64,
                            _ => -(task.created as i64),
                        };
                        (group, order)
                    });
                    let tasks: Vec<_> = tasks.into_iter().map(|(_, task)| task).collect();
                    let tasks = Rc::new(tasks);
                    self.queue_cache = Some((workspace.state.revision, Rc::clone(&tasks)));
                    tasks
                }
            }
        };
        if self.workspace.is_some() {
            items.insert(0, QueueItem::Header);
        }
        let completed_state = window.use_keyed_state("task-history-open", cx, |_, _| false);
        let queue_open = window.use_keyed_state("task-queue-open", cx, |_, _| false);
        let done = |task: &TaskRecord| {
            matches!(task_group(task), TaskGroup::Finished | TaskGroup::History)
        };
        // Finished work and earlier records fold behind one tile; a task the
        // person went to (from the library or a notice) stays visible.
        let expanded_done = self
            .expanded_task
            .as_ref()
            .is_some_and(|id| tasks.iter().any(|task| &task.id == id && done(task)));
        let completed_open = *completed_state.read(cx) || expanded_done;
        for group in TaskGroup::ORDER {
            let members: Vec<usize> = tasks
                .iter()
                .enumerate()
                .filter(|(_, task)| task_group(task) == group)
                .map(|(index, _)| index)
                .collect();
            let count = tasks.iter().filter(|task| done(task)).count();
            if group == TaskGroup::Finished && count > 0 {
                items.push(QueueItem::CompletedToggle {
                    count,
                    open: completed_open,
                    state: completed_state.clone(),
                });
            }
            let folded = matches!(group, TaskGroup::Finished | TaskGroup::History);
            if members.is_empty() || (folded && !completed_open) {
                continue;
            }
            // The fold tile just above already names finished work.
            if group != TaskGroup::Finished {
                items.push(QueueItem::GroupLabel {
                    group,
                    count: members.len(),
                    queued: members
                        .iter()
                        .filter(|&&index| tasks[index].state == TaskState::Queued)
                        .count(),
                });
            }
            // Folding one row would save nothing.
            let folds = group == TaskGroup::Queued && members.len() > QUEUE_PREVIEW + 1;
            let shown = if folds && !*queue_open.read(cx) {
                QUEUE_PREVIEW
            } else {
                members.len()
            };
            // The tile order is the run order; paused tasks keep their place.
            let tiles = shown + usize::from(folds);
            for (position, &task) in members.iter().take(shown).enumerate() {
                let outer = list_corners(position, tiles);
                let place = match group {
                    TaskGroup::Running => CardPlace::Open { outer: [true; 4] },
                    TaskGroup::Attention => CardPlace::Open { outer },
                    _ => CardPlace::Row {
                        outer,
                        number: (group == TaskGroup::Queued).then_some(position + 1),
                    },
                };
                items.push(QueueItem::Task {
                    task,
                    place,
                    joined: group != TaskGroup::Running && position + 1 < tiles,
                });
            }
            if folds {
                items.push(QueueItem::QueueFold {
                    hidden: members.len() - shown,
                    open: queue_open.clone(),
                });
            }
        }
        let keys = items.iter().map(|item| item.key(&tasks)).collect();
        Self::reconcile_list_items(
            &self.queue_list,
            &mut self.queue_keys,
            &mut self.queue_focus,
            keys,
            cx,
        );
        if self.queue_rem != rem_size {
            self.queue_list.remeasure();
            self.queue_rem = rem_size;
        }
        let focus = Rc::new(self.queue_focus.clone());
        let state = self.queue_list.clone();
        let items = Rc::new(items);
        let desktop = cx.weak_entity();
        let queue = list(state, move |index, window, cx| {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = index + 1 == items.len();
            desktop
                .update(cx, |this, cx| {
                    let gap = match item {
                        QueueItem::GroupLabel { .. } => rems(SECTION_TITLE_GAP.as_f32() / 14.),
                        QueueItem::Task { joined: true, .. } => rems(TILE_GAP.as_f32() / 14.),
                        _ => TASK_SECTION_GAP,
                    };
                    let mut wrapper = div()
                        .w_full()
                        .min_w_0()
                        .max_w(TASK_COLUMN)
                        .mx_auto()
                        .flex()
                        .flex_col()
                        .when(!last, |view| view.pb(gap))
                        .child(this.queue_item(item, &tasks, window, cx))
                        .id(("queue-item", index))
                        .tab_stop(false);
                    if let Some(focus) = focus.get(index) {
                        wrapper = wrapper.track_focus(focus);
                    }
                    wrapper.into_any_element()
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
        .w_full()
        .h_full()
        .flex_1()
        .min_h_0()
        .pb(rem(72.));
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(queue)
            .into_any_element()
    }

    /// The page title with a chip for each kind of work there is, and the
    /// action that stops or resumes all of it.
    fn queue_page_header(&self, tasks: &[TaskRecord], cx: &mut Context<Self>) -> Div {
        let count =
            |wanted: &dyn Fn(&TaskRecord) -> bool| tasks.iter().filter(|task| wanted(task)).count();
        let running = count(&|task| task_group(task) == TaskGroup::Running);
        let attention = count(&|task| task_group(task) == TaskGroup::Attention);
        let queued = count(&|task| task.state == TaskState::Queued);
        let resumable = count(&resumable);
        // Pausing applies to work that is still meant to run; a task already
        // stopping has nothing left to pause.
        let pausable = count(&|task| {
            matches!(task.state, TaskState::Running | TaskState::Queued)
                && task.intent == Intent::Run
                && task.handled_by.is_none()
        });
        let chips = [
            (
                running,
                StatusKind::Progress,
                course2md::tr!("{} 个正在处理", running),
            ),
            (
                attention,
                StatusKind::Danger,
                course2md::tr!("{} 个需要处理", attention),
            ),
            (
                queued,
                StatusKind::Neutral,
                course2md::tr!("{} 个排队中", queued),
            ),
            (
                resumable,
                StatusKind::Neutral,
                course2md::tr!("{} 个已暂停", resumable),
            ),
        ]
        .into_iter()
        .filter(|(count, ..)| *count > 0)
        .map(|(_, kind, text)| status_chip(kind).child(text));
        h_flex()
            .w_full()
            .min_w_0()
            .pt(rem(16.))
            .gap(rem(16.))
            .items_center()
            .flex_wrap()
            .child(
                v_flex()
                    .flex_1()
                    .flex_basis(rem(320.))
                    .min_w_0()
                    .gap(rem(8.))
                    .child(page_heading("tasks-page-title", course2md::tr!("任务")))
                    .child(h_flex().gap(rem(8.)).flex_wrap().children(chips)),
            )
            .child(
                h_flex()
                    .gap(rem(8.))
                    .when(pausable > 0, |row| {
                        row.child(
                            material_button("tasks-pause-all", ButtonVariant::Outlined)
                                .icon(symbols::pause())
                                .child(course2md::tr!("全部暂停"))
                                .on_click(cx.listener(|this, _, _, cx| this.pause_all_tasks(cx))),
                        )
                    })
                    .when(resumable > 0, |row| {
                        row.child(
                            material_button("tasks-resume-all", ButtonVariant::Outlined)
                                .icon(symbols::play_arrow())
                                .child(course2md::tr!("全部继续"))
                                .on_click(cx.listener(|this, _, _, cx| this.resume_all_tasks(cx))),
                        )
                    }),
            )
    }

    /// Stop the running task first, so its stop starts nothing new, then hold
    /// the queue in one step, so no queued task starts between two pauses.
    pub(crate) fn pause_all_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let pausable = |task: &&TaskRecord| task.handled_by.is_none() && task.intent == Intent::Run;
        let running: Vec<String> = workspace
            .state
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Running)
            .filter(pausable)
            .map(|task| task.id.clone())
            .collect();
        let queued: Vec<String> = workspace
            .state
            .queued_tasks()
            .filter(pausable)
            .map(|task| task.id.clone())
            .collect();
        for id in running {
            self.set_task_intent(id, Intent::Pause, cx);
        }
        // Queued work has no worker to tell; its record is the whole state.
        self.transact(course2md::tr!("任务状态尚未保存：{}"), |state| {
            queued
                .iter()
                .try_for_each(|id| state.set_intent(id, Intent::Pause))
        });
        cx.notify();
    }

    pub(crate) fn resume_all_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let ids: Vec<String> = workspace
            .state
            .tasks
            .iter()
            .filter(|task| resumable(task))
            .map(|task| task.id.clone())
            .collect();
        for id in ids {
            self.set_task_intent(id, Intent::Run, cx);
        }
    }

    pub(crate) fn move_waiting_task(&mut self, id: &str, position: usize, cx: &mut Context<Self>) {
        self.transact(course2md::tr!("任务顺序尚未保存：{}"), |state| {
            state.move_waiting_task(id, position)
        });
        cx.notify();
    }

    fn queue_item(
        &mut self,
        item: &QueueItem,
        tasks: &[TaskRecord],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match item {
            // The page heading summarizes the counts; group headings stay names.
            QueueItem::Header => self.queue_page_header(tasks, cx).into_any_element(),
            QueueItem::Unavailable => notice_banner(
                "task-records-unavailable",
                NoticeKind::Danger,
                course2md::tr!("任务记录暂时无法读取"),
            )
            .child(
                material_button("task-records-open-library", ButtonVariant::Outlined)
                    .icon(symbols::menu_book())
                    .child(course2md::tr!("前往笔记库"))
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::Library, cx))),
            )
            .into_any_element(),
            QueueItem::Empty => tile([true; 4])
                .flex()
                .flex_col()
                .gap(rem(16.))
                .px(rem(20.))
                .py(rem(40.))
                .items_center()
                .child(tinted_icon(symbols::checklist(), Tint::Teal, rem(64.)))
                .child(
                    accessible_text("task-empty-title", course2md::tr!("还没有生成任务"))
                        .role(Role::Heading)
                        .text_size(TEXT_SECTION),
                )
                .child(
                    material_button("task-new", ButtonVariant::Filled)
                        .icon(symbols::add())
                        .child(course2md::tr!("导入视频"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.prepare_workbench_input(window, cx) {
                                this.navigate(Page::New, cx);
                            }
                        })),
                )
                .into_any_element(),
            QueueItem::GroupLabel {
                group,
                count,
                queued,
            } => {
                // Paused work keeps its place in the queue; when nothing in it
                // is waiting to run, the section says so.
                let label = if *group == TaskGroup::Queued && *queued == 0 {
                    course2md::tr!("已暂停")
                } else {
                    group.label()
                };
                h_flex()
                    .w_full()
                    .min_w_0()
                    .px(ITEM_INSET)
                    .gap_x(rem(12.))
                    .items_baseline()
                    .flex_wrap()
                    .child(section_title(
                        SharedString::from(format!("task-group-{group:?}")),
                        label,
                    ))
                    .when(
                        *group == TaskGroup::Queued && *count > 1 && *queued > 0,
                        |row| {
                            row.child(
                                accessible_text(
                                    "task-queue-order-hint",
                                    course2md::tr!("拖动可以调整顺序"),
                                )
                                .text_size(TEXT_AUX)
                                .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
                            )
                        },
                    )
                    .into_any_element()
            }
            QueueItem::Task { task, place, .. } => {
                self.task_card(&tasks[*task], *place, window, cx)
            }
            QueueItem::QueueFold { hidden, open } => {
                let toggle = open.clone();
                let opened = *open.read(cx);
                ListItem::new(
                    "toggle-task-queue",
                    if opened {
                        course2md::tr!("收起").to_owned()
                    } else {
                        course2md::tr!("显示另外 {} 个", hidden)
                    },
                )
                .trailing(ListTrailing::Icon(disclosure_chevron(opened)))
                .segment([false, false, true, true])
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |open, cx| {
                        *open = !*open;
                        cx.notify();
                    });
                })
                .into_any_element()
            }
            QueueItem::CompletedToggle { count, open, state } => {
                let toggle = state.clone();
                let open = *open;
                // The tile names what it folds; the count is the record's size.
                ListItem::new(
                    "toggle-task-history",
                    if open {
                        course2md::tr!("收起已完成的处理")
                    } else {
                        course2md::tr!("查看已完成的处理")
                    },
                )
                .leading(ListLeading::Icon(symbols::check_circle().into()))
                .trailing_text(count.to_string())
                .trailing(ListTrailing::Icon(disclosure_chevron(open)))
                .segment([true; 4])
                // Folding also closes a finished task that was opened
                // from elsewhere, or the fold could not close.
                .on_click(cx.listener(move |this, _, _, cx| {
                    if open
                        && let Some(id) = this.expanded_task.clone()
                        && this
                            .workspace
                            .as_ref()
                            .and_then(|workspace| workspace.state.task(&id))
                            .is_some_and(|task| {
                                matches!(task_group(task), TaskGroup::Finished | TaskGroup::History)
                            })
                    {
                        this.close_task_details(&id);
                        this.expanded_task = None;
                    }
                    toggle.update(cx, |state, cx| {
                        *state = !open;
                        cx.notify();
                    });
                }))
                .into_any_element()
            }
        }
    }

    /// What can be done to `task` right now, in the order `surface` draws it.
    pub(crate) fn task_actions(&self, task: &TaskRecord, surface: TaskSurface) -> Vec<TaskAction> {
        use TaskAction::*;
        let active = self.is_running_task(&task.id);
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
            if let Some(library) = self.task_library(task).filter(|library| {
                self.cached_location_check(library)
                    .is_some_and(|check| check.needs_reassociation)
            }) {
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
                if self.task_library(task).is_some_and(|location| {
                    !workspace::task_export_files(task, location).is_empty()
                }) {
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
                actions.extend(self.task_moves(task));
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
                if has_log && self.expanded_task.as_ref() == Some(&task.id) {
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
                actions.extend([ViewTask, NextVideo]);
            }
        }
        actions
    }

    /// Moves within the queue, for those who don't drag.
    fn task_moves(&self, task: &TaskRecord) -> Vec<TaskAction> {
        let queue: Vec<&str> = self
            .workspace
            .as_ref()
            .map(|workspace| {
                workspace
                    .state
                    .waiting_tasks()
                    .map(|task| task.id.as_str())
                    .collect()
            })
            .unwrap_or_default();
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

    /// Every command for a task, on right-click: Kit's context menu holds them.
    pub(crate) fn task_menu(
        &self,
        task: &TaskRecord,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let entity = cx.weak_entity();
        let id = task.id.clone();
        let items: Vec<_> = self
            .task_actions(task, TaskSurface::Menu)
            .into_iter()
            .map(|action| (action.present(task, self), action))
            .collect();
        move |mut menu, _, _| {
            let mut group = None;
            for ((label, icon, disabled), action) in &items {
                if group.is_some_and(|group| group != action.group()) {
                    menu = menu.separator();
                }
                group = Some(action.group());
                menu = menu.item(
                    PopupMenuItem::new(*label)
                        .icon(icon.clone().component())
                        .disabled(*disabled)
                        .on_click(action.clone().click(entity.clone(), id.clone())),
                );
            }
            menu
        }
    }

    /// The same commands behind the card's ⋯ button.
    fn task_menu_button(
        &self,
        task: &TaskRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = &task.id;
        let label = course2md::tr!("更多操作");
        let actions = self.task_actions(task, TaskSurface::Menu);
        let groups: Vec<u8> = actions.iter().map(TaskAction::group).collect();
        let items = actions.into_iter().enumerate().map(|(index, action)| {
            let (text, icon, disabled) = action.present(task, self);
            let danger = action == TaskAction::Cancel;
            MaterialMenuItem::new(text)
                .leading(Icon::from(icon).when(danger, |icon| icon.color(color(ColorRole::ERROR))))
                .disabled(disabled)
                .divider(
                    groups
                        .get(index + 1)
                        .is_some_and(|next| *next != groups[index]),
                )
                .on_click(action.click(cx.weak_entity(), id.clone()))
        });
        let trigger =
            MaterialTooltip::plain(SharedString::from(format!("task-more-tip-{id}")), label)
                .trigger(IconButton::new(
                    SharedString::from(format!("task-more-{id}")),
                    symbols::more_vert(),
                    label,
                ));
        ui::menu(
            SharedString::from(format!("task-more-menu-{id}")),
            trigger,
            MaterialMenu::new(SharedString::from(format!("task-more-items-{id}"))).items(items),
            window,
            cx,
        )
        .anchor(Anchor::TopRight)
        .into_any_element()
    }

    /// Expand a row's details, or fold the expanded one.
    pub(crate) fn toggle_task_row(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.expanded_task.as_deref() == Some(id) {
            self.close_task_details(id);
            self.expanded_task = None;
            cx.notify();
            return;
        }
        self.select_task(id, cx);
        self.expanded_task = Some(id.to_owned());
    }

    /// Show or hide a task's log; a stopped task opens its details to show it.
    fn toggle_task_log(&mut self, task: &TaskRecord, cx: &mut Context<Self>) {
        if !self.task_logs_open.remove(&task.id) {
            self.task_logs_open.insert(task.id.clone());
            if task_group(task) != TaskGroup::Running {
                self.select_task(&task.id, cx);
            }
        }
        cx.notify();
    }

    /// Folding a card unmounts its details; opening it later starts fresh.
    pub(crate) fn close_task_details(&mut self, id: &str) {
        self.entered.remove(&format!("task-detail-{id}"));
        self.task_logs_open.remove(id);
    }

    fn open_task_note(&mut self, task: &TaskRecord, cx: &mut Context<Self>) {
        let Some(path) = &task.artifact else {
            return;
        };
        // The note opens from its path even before the library has listed it.
        let course = self
            .courses
            .iter()
            .find(|course| &course.dir == path)
            .cloned()
            .unwrap_or_else(|| {
                Rc::new(Course::from_completed(&Done {
                    out_dir: path.clone(),
                    title: task.plan.title.clone(),
                    ..Default::default()
                }))
            });
        self.open_course(course, cx);
    }

    /// Where a task saves, as its supporting line.
    fn task_destination(&self, task: &TaskRecord) -> String {
        match self.task_library(task) {
            Some(location) => course2md::tr!("保存到 {}", location.name),
            None => course2md::tr!("保存位置暂时不可用").to_owned(),
        }
    }

    /// Which service receives the text, when one does.
    fn task_ai_destination(&self, task: &TaskRecord) -> Option<String> {
        let version = self.preferences.version(
            task.plan
                .ai_service
                .as_ref()
                .filter(|_| task.plan.config.llm.uses_service())?,
        )?;
        Some(course2md::tr!(
            "{}发送到「{}」",
            if task.plan.options().vision {
                course2md::tr!("文字与截图")
            } else {
                course2md::tr!("文字")
            },
            version.config.name,
        ))
    }

    /// What a task was planned with, read from its saved plan: a submitted task's
    /// options on the workbench, and the record in its details.
    pub(crate) fn task_plan_rows(&self, task: &TaskRecord) -> Vec<Div> {
        let options = task.plan.options();
        let mut ai: Vec<String> = [
            import_ui::ConversionAiOption::Proofread,
            import_ui::ConversionAiOption::Summary,
            import_ui::ConversionAiOption::Vision,
        ]
        .into_iter()
        .filter(|option| import_ui::conversion_ai_option_enabled(&options, *option))
        .map(|option| import_ui::conversion_ai_option_label(option).to_owned())
        .collect();
        if options.translate.is_some() {
            ai.push(course2md::tr!("双语翻译").to_owned());
        }
        let ai = if ai.is_empty() {
            course2md::tr!("不使用 AI").to_owned()
        } else {
            ai.join(course2md::tr!("、"))
        };
        let ai = match self.task_ai_destination(task) {
            Some(destination) => format!("{ai} · {destination}"),
            None => ai,
        };
        let library = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.library(&task.plan.library_id));
        let folder = task
            .plan
            .folder
            .and_then(|id| {
                self.library_indexes
                    .get(&library?.root)?
                    .folders
                    .get(&id)
                    .cloned()
            })
            .unwrap_or_else(|| course2md::tr!("未分类").to_owned());
        let destination = match library {
            Some(library) => format!("{} · {folder}", library.name),
            None => folder,
        };
        let id = &task.id;
        vec![
            fact_row(
                format!("task-destination-{id}"),
                symbols::folder_open(),
                course2md::tr!("保存到"),
                destination,
            ),
            fact_row(
                format!("task-text-source-{id}"),
                symbols::subtitles(),
                course2md::tr!("文字来源"),
                task_text_source(task).to_owned(),
            ),
            fact_row(
                format!("task-ai-{id}"),
                symbols::auto_fix_high(),
                course2md::tr!("AI 处理"),
                ai,
            ),
        ]
    }

    fn task_cover(&self, task: &TaskRecord) -> Option<PathBuf> {
        task.plan
            .source
            .cover
            .clone()
            .or_else(|| self.cover_by_source.get(&task.plan.source_id).cloned())
    }

    /// One task, wherever it stands.
    pub(crate) fn task_card(
        &mut self,
        task: &TaskRecord,
        place: CardPlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = &task.id;
        let expanded = self.expanded_task.as_ref() == Some(id);
        let more = self.task_menu_button(task, window, cx);
        let chip = (task.state != TaskState::Queued || !matches!(place, CardPlace::Row { .. }))
            .then(|| status_chip(task_status_kind(task.state)).child(task_status_label(task)));
        let context_menu = self.task_menu(task, cx);
        let (outer, head, body) = match place {
            CardPlace::Row { outer, number } => {
                let head = self.task_row_head(task, number, chip, more, cx);
                (outer, head.into_any_element(), expanded)
            }
            CardPlace::Open { outer } => {
                let head = self.task_card_head(task, place, chip, more, window);
                (outer, head.into_any_element(), true)
            }
            CardPlace::Home => {
                let head = self.task_card_head(task, place, chip, more, window);
                ([true, true, false, false], head.into_any_element(), true)
            }
        };
        let body = body.then(|| self.task_body(task, place, window, cx));
        tile(outer)
            .id(SharedString::from(format!("task-menu-{id}")))
            .flex()
            .flex_col()
            .overflow_hidden()
            .map(|card| match place {
                CardPlace::Row { .. } => card
                    .child(head)
                    .children(body.map(|body| div().px(rem(16.)).pb(rem(16.)).child(body))),
                _ => card.p(rem(16.)).gap(rem(16.)).child(head).children(body),
            })
            .context_menu(move |popup, window, cx| context_menu(popup, window, cx))
            .into_any_element()
    }

    /// A row of its group: the grip for queued work, the row that folds details out,
    /// and what it says and offers at its trailing edge.
    fn task_row_head(
        &self,
        task: &TaskRecord,
        number: Option<usize>,
        chip: Option<Div>,
        more: AnyElement,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = task.id.clone();
        let cover = self.task_cover(task);
        let waiting = number.map(|_| task.state);
        let thumbnail = match cover {
            Some(cover) => img(cover)
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => h_flex()
                .size_full()
                .justify_center()
                .child(
                    Icon::from(match waiting {
                        Some(TaskState::Paused) => symbols::pause(),
                        Some(_) => symbols::schedule(),
                        None => symbols::movie(),
                    })
                    .color(color(ColorRole::ON_SURFACE_VARIANT)),
                )
                .into_any_element(),
        };
        let row = ListItem::new(
            SharedString::from(format!("select-{id}")),
            task.plan.title.clone(),
        )
        .supporting(self.task_destination(task))
        .leading(ListLeading::Video(Some(thumbnail)))
        .on_click(cx.listener({
            let id = id.clone();
            move |this, _, _, cx| this.toggle_task_row(&id, cx)
        }));
        let head =
            h_flex()
                .id(SharedString::from(format!("task-row-{id}")))
                .w_full()
                .min_w_0()
                .items_center()
                .flex_wrap()
                .when(number.is_some(), |row| {
                    row.child(h_flex().flex_shrink_0().pl(rem(12.)).cursor_grab().child(
                        Icon::from(symbols::drag_indicator()).color(color(ColorRole::OUTLINE)),
                    ))
                })
                .child(div().flex_1().flex_basis(rem(300.)).min_w_0().child(row))
                // At narrow widths or large text the commands move under the identity.
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .ml_auto()
                        .px(rem(8.))
                        .gap(rem(8.))
                        .items_center()
                        .children(chip)
                        .when(
                            task.artifact.is_some() && task_group(task) != TaskGroup::Queued,
                            |row| {
                                row.child(
                                    material_button(
                                        SharedString::from(format!("task-read-{id}")),
                                        ButtonVariant::Text,
                                    )
                                    .child(course2md::tr!("阅读笔记"))
                                    .on_click(TaskAction::Read.click(cx.weak_entity(), id.clone())),
                                )
                            },
                        )
                        .child(more),
                );
        match number {
            Some(number) => {
                let drag = QueueDrag {
                    id,
                    title: task.plan.title.clone().into(),
                };
                head.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                    .drag_over::<QueueDrag>(|style, _, _, _| {
                        style.bg(color(ColorRole::SECONDARY_CONTAINER))
                    })
                    .on_drop(cx.listener(move |this, drag: &QueueDrag, _, cx| {
                        this.move_waiting_task(&drag.id, number - 1, cx)
                    }))
            }
            None => head,
        }
    }

    /// A card's identity: its cover, title and what is known so far.
    fn task_card_head(
        &self,
        task: &TaskRecord,
        place: CardPlace,
        chip: Option<Div>,
        more: AnyElement,
        window: &Window,
    ) -> Div {
        let id = &task.id;
        let home = matches!(place, CardPlace::Home);
        let title = if task.plan.title.trim().is_empty() {
            task.plan.source.title.clone()
        } else {
            task.plan.title.clone()
        };
        // Where it saves, where its text comes from and which service
        // receives it, on one line; on the workbench, whose video it is.
        let support = if home {
            import_ui::source_metadata(&task.plan.source)
        } else {
            [
                self.task_destination(task),
                task_text_source(task).to_owned(),
            ]
            .into_iter()
            .chain(self.task_ai_destination(task))
            .collect::<Vec<_>>()
            .join(" · ")
        };
        let cover_width = if !home {
            rem(160.)
        } else {
            import_ui::cover_width(window)
        };
        let step = self.task_step_line(task);
        h_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .flex_wrap()
            .gap(rem(16.))
            .child(video_cover(self.task_cover(task), cover_width))
            .child(
                v_flex()
                    .flex_1()
                    .flex_basis(rem(260.))
                    .min_w_0()
                    .gap(rem(4.))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_start()
                            .gap(rem(8.))
                            .child(video_title(format!("task-title-{id}"), title).flex_1())
                            .child(more),
                    )
                    .when_some(step, |column, step| {
                        column.child(
                            accessible_text(SharedString::from(format!("task-step-{id}")), step)
                                .role(Role::Status)
                                .text_size(TEXT_BODY)
                                .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                                .whitespace_normal(),
                        )
                    })
                    .when(!support.is_empty(), |column| {
                        column.child(
                            accessible_text(
                                SharedString::from(format!("task-support-{id}")),
                                support,
                            )
                            .text_size(TEXT_AUX)
                            .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                            .whitespace_normal(),
                        )
                    })
                    .child(h_flex().pt(rem(4.)).children(chip)),
            )
    }

    /// The step a running task is on. Later steps depend on what the video turns out
    /// to need, so the position counts the steps reached so far, without a total.
    fn task_step_line(&self, task: &TaskRecord) -> Option<String> {
        let stages = self.task_stage_rows(task);
        stages
            .iter()
            .position(|stage| stage.status.running())
            .map(|index| course2md::tr!("第 {} 步 · {}", index + 1, stages[index].title))
            .or_else(|| self.task_wait(task).map(|wait| wait.label().to_owned()))
    }

    fn task_wait(&self, task: &TaskRecord) -> Option<WorkerWait> {
        worker_wait_state(
            self.is_running_task(&task.id),
            task.events.done.is_some(),
            task.intent,
            task.events
                .stages
                .iter()
                .map(|(name, item)| (name, item.done)),
        )
    }

    /// One indicator for the step in progress: its measured fraction when the
    /// worker reports one, motion otherwise. The line under it carries the step's
    /// own measurement and clock, never an estimate.
    fn task_progress(&self, task: &TaskRecord, stages: &[TaskStage]) -> Option<Div> {
        let id = &task.id;
        let active = self.is_running_task(id);
        let current = stages.iter().find(|stage| stage.status.running());
        if !active || (current.is_none() && self.task_wait(task).is_none()) {
            return None;
        }
        let detail = current
            .map(|stage| stage.detail.clone())
            .filter(|detail| !detail.is_empty())
            .or_else(|| {
                self.job_started
                    .filter(|_| self.job.is_some())
                    .map(|started| activity::elapsed_label(started.elapsed()))
            });
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .child(
                    LinearProgress::new(
                        SharedString::from(format!("task-progress-{id}")),
                        current.map_or("", |stage| stage.title.as_str()).to_owned(),
                    )
                    .value(current.and_then(|stage| stage.fraction))
                    .muted(task.state == TaskState::Pausing || task.intent != Intent::Run),
                )
                .when_some(detail, |view, detail| {
                    view.child(
                        ui::status_line(SharedString::from(format!("task-measure-{id}")), &detail)
                            .text_size(TEXT_AUX)
                            .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
                    )
                }),
        )
    }

    /// The uncertain-outcome decision: exactly what may be billed again, beside the
    /// one button that sends it. Nothing here runs without that press.
    fn uncertain_block(&self, task: &TaskRecord, cx: &mut Context<Self>) -> Option<Div> {
        let id = &task.id;
        let uncertain: Vec<_> = task
            .events
            .blocked
            .iter()
            .filter(|blocked| blocked.is_uncertain())
            .collect();
        if uncertain.is_empty() || task.handled_by.is_some() {
            return None;
        }
        let active = self.is_running_task(id);
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .p(rem(12.))
                .rounded(RADIUS_CARD)
                .bg(color(ColorRole::WARNING_BG))
                .text_size(TEXT_BODY)
                .children(uncertain.iter().enumerate().map(|(index, blocked)| {
                    accessible_text(
                        SharedString::from(format!("unknown-scope-{id}-{index}")),
                        request_scope(blocked),
                    )
                    .whitespace_normal()
                }))
                .child(
                    accessible_text(
                        SharedString::from(format!("unknown-effect-{id}")),
                        course2md::tr!("服务可能已处理这些内容，重新发送可能产生额外费用"),
                    )
                    .font_weight(FontWeight::MEDIUM)
                    .whitespace_normal(),
                )
                .when(active, |view| {
                    view.child(accessible_text(
                        SharedString::from(format!("unknown-saving-{id}")),
                        course2md::tr!("正在保存当前结果…"),
                    ))
                })
                .child(
                    material_button(
                        SharedString::from(format!("resend-{id}")),
                        ButtonVariant::Filled,
                    )
                    .self_start()
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重新发送以上内容"))
                    .disabled(active)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.resend_uncertain(id.clone(), cx)
                    })),
                ),
        )
    }

    /// What a card shows under its identity: why it stopped, what is decided next,
    /// how far it got, and the actions; its record once its details are open.
    fn task_body(
        &mut self,
        task: &TaskRecord,
        place: CardPlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = task.id.clone();
        let row = matches!(place, CardPlace::Row { .. });
        let surface = if matches!(place, CardPlace::Home) {
            TaskSurface::Home
        } else {
            TaskSurface::Card
        };
        let mut actions = self.task_actions(task, surface);
        // A row carries reading at its edge and folds itself.
        actions.retain(|action| !row || !matches!(action, TaskAction::Read | TaskAction::Details));
        let lead = lead_action(&actions, task, surface).cloned();
        let failures = task_note_failures(task);
        let consequence = (!failures.is_empty()).then(|| {
            course2md::tr!(
                "{}尚未完成，笔记的其余部分已保存",
                failures
                    .iter()
                    .map(|(_, part, _)| part.label())
                    .collect::<Vec<_>>()
                    .join(course2md::tr!("、"))
            )
        });
        let stages = self.task_stage_rows(task);
        let running = stages.iter().any(|stage| stage.status.running());
        let progress = self.task_progress(task, &stages);
        // A single reached step is already the step line above.
        let chips = (running && stages.len() > 1).then(|| {
            h_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .flex_wrap()
                .children(
                    stages
                        .iter()
                        .map(|stage| status_chip(stage.status.chip()).child(stage.title.clone())),
                )
        });
        let buttons = actions.iter().map(|action| {
            let (label, icon, disabled) = action.present(task, self);
            let variant = if lead.as_ref() == Some(action) {
                ButtonVariant::Filled
            } else {
                match action {
                    TaskAction::Pause => ButtonVariant::Tonal,
                    TaskAction::Read
                    | TaskAction::Details
                    | TaskAction::Log
                    | TaskAction::Cancel
                    | TaskAction::ViewTask => ButtonVariant::Text,
                    _ => ButtonVariant::Outlined,
                }
            };
            material_button(SharedString::from(format!("task-{label}-{id}")), variant)
                .icon(icon)
                .child(label)
                .disabled(disabled)
                .when(
                    disabled && matches!(action, TaskAction::Pause | TaskAction::Cancel),
                    |b| b.loading(true, label),
                )
                .on_click(action.clone().click(cx.weak_entity(), id.clone()))
        });
        let buttons = h_flex()
            .w_full()
            .min_w_0()
            .gap(rem(8.))
            .flex_wrap()
            .justify_end()
            .children(buttons.collect::<Vec<_>>());
        let details = (self.expanded_task.as_ref() == Some(&id)).then(|| {
            let content = v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(12.))
                .pt(rem(12.))
                .border_t_1()
                .border_color(color(ColorRole::OUTLINE_VARIANT))
                .children(self.task_plan_rows(task))
                .children(
                    self.task_stage_rows(task)
                        .into_iter()
                        .map(|stage| stage_row(stage, window, cx)),
                );
            let key = format!("task-detail-{id}");
            if self.enter_once(key.clone()) {
                motion::enter(SharedString::from(key), content)
            } else {
                content.into_any_element()
            }
        });
        let log = task_log_text(task)
            .filter(|_| self.task_logs_open.contains(&id))
            .map(|text| log_box(&id, text));
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(12.))
            .children(
                task_reasons(task)
                    .into_iter()
                    .enumerate()
                    .map(|(index, reason)| {
                        error_text(
                            SharedString::from(format!("task-reason-{id}-{index}")),
                            reason,
                        )
                        .text_size(TEXT_BODY)
                        .whitespace_normal()
                    }),
            )
            .children(consequence.map(|consequence| {
                accessible_text(
                    SharedString::from(format!("task-consequence-{id}")),
                    consequence,
                )
                .text_size(TEXT_AUX)
                .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                .whitespace_normal()
            }))
            .children(self.uncertain_block(task, cx))
            .children(progress)
            .children(chips)
            .child(buttons)
            .children(details)
            .children(log)
    }

    /// Live and retained stages use the same labels, status column and icon slot.
    fn task_stage_rows(&self, task: &TaskRecord) -> Vec<TaskStage> {
        let active = self.is_running_task(&task.id);
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
                    id: SharedString::from(format!("task-stage-{}-{name}", task.id)),
                    title: activity::title(&name),
                    status,
                    detail,
                    compact_detail,
                    fraction,
                }
            })
            .collect()
    }

    /// A waiting line with the running job's clock, so a pause between
    /// stages never looks stalled.
    pub(crate) fn with_job_clock(&self, label: &str) -> String {
        match self.job_started.filter(|_| self.job.is_some()) {
            Some(started) => format!("{label} · {}", activity::elapsed_label(started.elapsed())),
            None => label.to_owned(),
        }
    }

    /// Compact status uses the same measured progress as task details.
    pub(crate) fn compact_task_progress(&self, task: &TaskRecord) -> Option<(String, Option<f32>)> {
        self.task_stage_rows(task)
            .into_iter()
            .find(|stage| stage.status.running())
            .map(|stage| {
                let label = if stage.compact_detail.is_empty() {
                    stage.title
                } else {
                    format!("{} · {}", stage.title, stage.compact_detail)
                };
                (label, stage.fraction)
            })
    }
}

/// A paused task that can continue as it is.
fn resumable(task: &TaskRecord) -> bool {
    task.state == TaskState::Paused && task.handled_by.is_none() && !task.has_uncertain()
}

fn adjust_label(task: &TaskRecord) -> &'static str {
    if task.artifact.is_some() {
        course2md::tr!("调整并生成新版")
    } else if task.state == TaskState::Paused {
        course2md::tr!("调整选项")
    } else {
        course2md::tr!("调整后重试")
    }
}

/// The worker's log, failure and blocked-request text, when there is any.
fn task_log_text(task: &TaskRecord) -> Option<String> {
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

pub(crate) fn task_text_source(task: &TaskRecord) -> &'static str {
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

fn task_status_kind(state: TaskState) -> StatusKind {
    match state {
        TaskState::Complete => StatusKind::Success,
        TaskState::Running | TaskState::Pausing => StatusKind::Progress,
        TaskState::NeedsAttention => StatusKind::Danger,
        TaskState::Uncertain | TaskState::Partial => StatusKind::Warning,
        _ => StatusKind::Neutral,
    }
}

fn task_status_label(task: &TaskRecord) -> String {
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

fn request_scope(request: &workspace::BlockedRequest) -> String {
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
pub(crate) fn task_component_failures(
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

#[cfg(test)]
mod tests {
    use super::{
        QueueItem, StageStatus, TaskGroup, WorkerWait, ai_stage_outcome, task_failure_summary,
        task_stage_progress, worker_wait_state,
    };
    use crate::task_runner::tests::source;
    use course2md::progress::Stage;

    #[gpui::test]
    fn queue_item_keys_track_identity_not_content(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| {
            let tasks = vec![task_record("task-a"), task_record("task-b")];
            let open = cx.new(|_| false);
            let row = |task: usize| QueueItem::Task {
                task,
                place: super::CardPlace::Row {
                    outer: [false; 4],
                    number: Some(task + 1),
                },
                joined: false,
            };
            let items = [
                QueueItem::GroupLabel {
                    group: TaskGroup::Queued,
                    count: 1,
                    queued: 1,
                },
                row(0),
                QueueItem::CompletedToggle {
                    count: 1,
                    open: false,
                    state: open,
                },
                row(1),
            ];
            let keys: Vec<String> = items.iter().map(|item| item.key(&tasks)).collect();
            let unique: std::collections::HashSet<_> = keys.iter().collect();
            assert_eq!(unique.len(), keys.len(), "queue rows need distinct keys");
            // Content updates with the same identities leave keys untouched.
            let tasks = vec![task_record("task-a"), task_record("task-b")];
            let again: Vec<String> = items.iter().map(|item| item.key(&tasks)).collect();
            assert_eq!(keys, again);
            // Task rows key on the task id, not the position in the queue.
            let mut moved = tasks;
            moved.swap(0, 1);
            assert_eq!(row(0).key(&moved), "task-task-b");
        });
    }

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
