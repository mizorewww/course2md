//! The task page: queue rows, running and attention cards, and their menus.
use super::*;

use crate::{
    theme::*,
    workspace::{Intent, TaskRecord, TaskState},
};
use course2md::config::TranscriptSource;
use course2md::execution::Component;
use course2md::progress::{Stage, StageStatus as EngineStageStatus};
use gpui_component::{
    button::*,
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
};
use std::rc::Rc;

/// Request purpose of a remote speech recognition call.
const TRANSCRIPTION_PURPOSE: &str = "transcription";

/// One row of the virtualized task queue. Task rows carry their index into
/// the task snapshot captured for the frame.
enum QueueItem {
    Unavailable,
    Empty,
    GroupLabel {
        group: TaskGroup,
        count: usize,
    },
    /// The task being processed, with its stages, settings and controls.
    Running(usize),
    /// One row of a group's shared surface; queued rows carry their run order.
    Row {
        task: usize,
        group: TaskGroup,
        /// Place among the waiting tasks, for reordering.
        number: Option<usize>,
        /// Run order, shown for queued tasks.
        runs: Option<usize>,
        first: bool,
        last: bool,
    },
    /// Folds a long queue after its first rows.
    QueueFold {
        hidden: usize,
        open: Entity<bool>,
    },
    HistoryToggle {
        count: usize,
        open: Entity<bool>,
    },
}

impl QueueItem {
    /// Identity for the list splice: stable across content updates that keep
    /// the row in place, so measured heights and scroll anchors survive.
    fn key(&self, tasks: &[TaskRecord]) -> String {
        match self {
            QueueItem::Unavailable => "unavailable".to_owned(),
            QueueItem::Empty => "empty".to_owned(),
            QueueItem::GroupLabel { group, .. } => format!("group-{group:?}"),
            QueueItem::Running(index) => format!("running-{}", tasks[*index].id),
            QueueItem::Row { task, .. } => format!("task-{}", tasks[*task].id),
            QueueItem::QueueFold { .. } => "queue-fold".to_owned(),
            QueueItem::HistoryToggle { .. } => "history-toggle".to_owned(),
        }
    }
}

/// Where a row sits on its group's surface.
#[derive(Clone, Copy)]
struct RowPlace {
    group: TaskGroup,
    number: Option<usize>,
    runs: Option<usize>,
    first: bool,
    last: bool,
}

/// What one virtualized queue item is drawn from: the frame's task snapshot,
/// the item's position in the list and its focus container.
struct QueueFrame<'a> {
    tasks: &'a [TaskRecord],
    index: usize,
    last: bool,
    focus: Option<&'a FocusHandle>,
}

/// Queued rows shown before the rest of the queue folds.
const QUEUE_PREVIEW: usize = 5;

/// A queued task being dragged to a new place in the queue.
#[derive(Clone)]
struct QueueDrag {
    id: String,
    title: SharedString,
}

impl Render for QueueDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .max_w(rems(24.))
            .gap_2()
            .px_3()
            .py_2()
            .rounded(RADIUS_SMALL)
            .bg(color(SURFACE))
            .shadow(shadow_popover())
            .text_size(TEXT_BODY)
            .font_weight(FontWeight::SEMIBOLD)
            .child(
                icons::grip_vertical()
                    .size(ICON_SLOT)
                    .text_color(color(GRAY)),
            )
            .child(div().min_w_0().truncate().child(self.title.clone()))
    }
}

/// Page order: the work in progress and what follows it, then what waits on
/// the person, then the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TaskGroup {
    Running,
    Queued,
    Attention,
    Finished,
    History,
}

impl TaskGroup {
    const ORDER: [Self; 5] = [
        Self::Running,
        Self::Queued,
        Self::Attention,
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

    fn ink(self) -> Rgba {
        color(match self {
            Self::Running => ACCENT,
            Self::Complete => SUCCESS,
            Self::Failed => DANGER,
            Self::Uncertain => WARNING,
            _ => GRAY,
        })
    }

    fn icon(self, id: SharedString, indeterminate: bool, cx: &App) -> AnyElement {
        if matches!(self, Self::Running | Self::Pausing | Self::Stopping) && indeterminate {
            return crate::motion::icon_spinner(id, rems(20. / 14.), cx);
        }
        (match self {
            Self::Running => icons::refresh(),
            Self::Complete => icons::check_circle(),
            Self::Paused | Self::Pausing => icons::pause(),
            Self::Stopping => icons::close(),
            Self::Failed | Self::Uncertain => icons::warning(),
            Self::Cancelled => icons::close(),
            _ => icons::schedule(),
        })
        .size(rems(20. / 14.))
        .text_color(self.ink())
        .into_any_element()
    }
}

struct TaskStage {
    id: SharedString,
    title: String,
    status: StageStatus,
    detail: String,
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
    // AI requests have no measurable fraction; their clock and returned
    // tokens show them moving, and stay as the record once they finish.
    if name.is_ai() {
        return (
            live.map(|live| live.live_counter()).unwrap_or_default(),
            None,
        );
    }
    let detail = if !done && let Some(live) = live {
        live.detail(name, true)
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
    // Unmeasured running work shows its clock instead of a bar.
    let detail = match live.filter(|_| !done && fraction.is_none()) {
        Some(live) if detail.is_empty() => live.live_counter(),
        Some(live) => format!("{detail} · {}", live.live_counter()),
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
    task_failure_summary(task.state, task.error.as_deref())
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
    let outcomes = task.outcomes.as_ref()?;
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

#[derive(IntoElement)]
struct TaskStages {
    rows: Vec<TaskStage>,
}

impl RenderOnce for TaskStages {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut view = v_flex().w_full().min_w_0().gap_3();
        for stage in self.rows {
            let status_id = SharedString::from(format!("{}-status", stage.id));
            // A running or finished step reads its measurement; any other
            // state names itself, with what it measured so far.
            let (status, ink) = match stage.status {
                StageStatus::Running => (stage.detail, color(GRAY)),
                StageStatus::Complete if !stage.detail.is_empty() => (stage.detail, color(GRAY)),
                status if stage.detail.is_empty() => (status.label().to_owned(), status.ink()),
                status => (
                    format!("{} · {}", status.label(), stage.detail),
                    status.ink(),
                ),
            };
            let mut row = v_flex().w_full().min_w_0().gap_2().child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_4()
                    .child(
                        semantic_label(
                            stage.id.clone(),
                            stage.title,
                            stage.status.icon(
                                SharedString::from(format!("{}-icon", stage.id)),
                                stage.fraction.is_none(),
                                cx,
                            ),
                        )
                        // The name keeps one line; the measurement wraps instead.
                        .whitespace_nowrap()
                        .flex_shrink_0(),
                    )
                    .child(
                        accessible_text(status_id, status)
                            .flex_1()
                            .min_w_0()
                            .text_right()
                            .whitespace_normal()
                            .text_size(TEXT_BODY)
                            .text_color(ink),
                    ),
            );
            if let Some(fraction) = stage.fraction {
                row = row.child(div().pl(LABEL_INSET).child(crate::motion::progress(
                    SharedString::from(format!("{}-progress", stage.id)),
                    fraction,
                    window,
                    cx,
                )));
            }
            view = view.child(row);
        }
        view
    }
}

/// Disclosure rows on a task card: the chevron sits on the card's content edge
/// (cancelling the quiet button's inline padding) and the compact height keeps
/// stacked disclosures at the card's ordinary row rhythm.
fn disclosure_toggle(id: SharedString) -> Button {
    quiet(id)
        .self_start()
        .h(DISCLOSURE_ROW)
        .min_h(DISCLOSURE_ROW)
        .ml(-crate::theme::QUIET_INSET)
}

/// Same minimum height as the card's `detail_row`.
const DISCLOSURE_ROW: Rems = rems(32. / 14.);

/// Where recovery actions appear: the task list card or the workbench box.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryStyle {
    Card,
    Workbench,
}

#[derive(Clone, Copy)]
enum Emphasis {
    Primary,
    Secondary,
    Quiet,
}

impl RecoveryStyle {
    fn button(self, name: &str, task_id: &str, emphasis: Emphasis) -> Button {
        let prefix = if self == Self::Workbench { "box-" } else { "" };
        let id = SharedString::from(format!("{prefix}{name}-{task_id}"));
        match (self, emphasis) {
            (_, Emphasis::Secondary) => outline_pill(id),
            (Self::Workbench, Emphasis::Primary) => primary_pill(id),
            (Self::Workbench, Emphasis::Quiet) => quiet(id),
            (Self::Card, _) => control(id),
        }
    }
}

/// A collapsible section of a task card. Open state lives on Desktop: a
/// virtualized task row unmounts when it leaves the viewport, and keyed
/// window state would be forgotten.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TaskPanel {
    /// Completed processing steps.
    Stages,
    /// Raw worker log and error text.
    RawLog,
}

impl TaskPanel {
    const ALL: [Self; 2] = [Self::Stages, Self::RawLog];

    fn slug(self) -> &'static str {
        match self {
            Self::Stages => "stage-details",
            Self::RawLog => "raw-log",
        }
    }

    fn label(self, open: bool) -> &'static str {
        match (self, open) {
            (Self::Stages, true) => course2md::tr!("收起已完成的处理"),
            (Self::Stages, false) => course2md::tr!("查看已完成的处理"),
            (Self::RawLog, true) => course2md::tr!("收起技术详情"),
            (Self::RawLog, false) => course2md::tr!("技术详情"),
        }
    }

    /// Key of the panel's one-time entrance animation.
    fn entrance_key(self, task_id: &str) -> String {
        format!("task-{}-{task_id}", self.slug())
    }
}

#[derive(IntoElement)]
struct TaskDisclosure {
    panel: TaskPanel,
    task_id: String,
    /// Task facts shown above the content on recovery cards.
    lead: Option<Div>,
    content: Div,
    open: bool,
    animate: bool,
    desktop: WeakEntity<Desktop>,
}

impl RenderOnce for TaskDisclosure {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self {
            panel,
            task_id,
            lead,
            content,
            open,
            animate,
            desktop,
        } = self;
        let content = match lead {
            Some(lead) => v_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .child(lead)
                .child(content),
            None => content,
        };
        let id = |part: &str| SharedString::from(format!("task-{}-{part}-{task_id}", panel.slug()));
        let toggle_id = id("toggle");
        let content_id = id("content");
        v_flex()
            .w_full()
            .min_w_0()
            .gap(if panel == TaskPanel::Stages {
                rems(12. / 14.)
            } else {
                rems(8. / 14.)
            })
            .child(
                disclosure_toggle(toggle_id)
                    .icon(if open {
                        icons::chevron_up()
                    } else {
                        icons::chevron_down()
                    })
                    .label(panel.label(open))
                    .on_click(move |_, _, cx| {
                        let _ = desktop.update(cx, |this, cx| {
                            this.toggle_task_panel(panel, &task_id);
                            cx.notify();
                        });
                    }),
            )
            .child(if animate {
                crate::motion::disclosure(content_id, open, content)
            } else if open {
                content.into_any_element()
            } else {
                div().hidden().into_any_element()
            })
    }
}

impl Desktop {
    pub fn queue_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rem = f32::from(window.rem_size());
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
        let history_open = window.use_keyed_state("task-history-open", cx, |_, _| false);
        let queue_open = window.use_keyed_state("task-queue-open", cx, |_, _| false);
        for group in TaskGroup::ORDER {
            let members: Vec<usize> = tasks
                .iter()
                .enumerate()
                .filter(|(_, task)| task_group(task) == group)
                .map(|(index, _)| index)
                .collect();
            if members.is_empty() {
                continue;
            }
            if group == TaskGroup::History {
                items.push(QueueItem::HistoryToggle {
                    count: members.len(),
                    open: history_open.clone(),
                });
                if !*history_open.read(cx) {
                    continue;
                }
            } else {
                items.push(QueueItem::GroupLabel {
                    group,
                    count: members.len(),
                });
            }
            if group == TaskGroup::Running {
                items.extend(members.into_iter().map(QueueItem::Running));
                continue;
            }
            // Folding one row would save nothing.
            let folds = group == TaskGroup::Queued && members.len() > QUEUE_PREVIEW + 1;
            let shown = if folds && !*queue_open.read(cx) {
                QUEUE_PREVIEW
            } else {
                members.len()
            };
            // Queued tasks are numbered in the order they will run; paused
            // ones keep their place without a number.
            let mut next = 0;
            for (position, &task) in members.iter().take(shown).enumerate() {
                let runs = tasks[task].state == TaskState::Queued;
                next += usize::from(runs);
                items.push(QueueItem::Row {
                    task,
                    group,
                    number: (group == TaskGroup::Queued).then_some(position + 1),
                    runs: runs.then_some(next),
                    first: position == 0,
                    last: position + 1 == shown && !folds,
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
        if self.queue_rem != rem {
            self.queue_list.remeasure();
            self.queue_rem = rem;
        }
        // Group headings carry the counts; the page heading stays a heading.
        let header = (!tasks.is_empty()).then(|| self.queue_page_header(&tasks, cx));
        let focus: Rc<Vec<FocusHandle>> = Rc::new(self.queue_focus.clone());
        let state = self.queue_list.clone();
        let items = Rc::new(items);
        let desktop = cx.weak_entity();
        let queue = list(state, move |index, _, cx| {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = index + 1 == items.len();
            desktop
                .update(cx, |this, cx| {
                    let frame = QueueFrame {
                        tasks: &tasks,
                        index,
                        last,
                        focus: focus.get(index),
                    };
                    this.queue_item(item, frame, cx)
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
        .w_full()
        .h_full()
        .flex_1()
        .min_h_0()
        .pb_6();
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .pt(px(24.))
            .gap_4()
            .when_some(header, |view, header| view.child(header))
            .child(queue)
            .into_any_element()
    }

    /// The page title with a summary of the work, and the action that stops
    /// or resumes all of it.
    fn queue_page_header(&self, tasks: &[TaskRecord], cx: &mut Context<Self>) -> Div {
        let count = |group: TaskGroup| {
            tasks
                .iter()
                .filter(|task| task_group(task) == group)
                .count()
        };
        let running = count(TaskGroup::Running);
        let attention = count(TaskGroup::Attention);
        let queued = tasks
            .iter()
            .filter(|task| task.state == TaskState::Queued)
            .count();
        let resumable = tasks.iter().filter(|task| resumable(task)).count();
        let summary = [
            (running, course2md::tr!("{} 个正在处理", running)),
            (queued, course2md::tr!("{} 个排队中", queued)),
            (resumable, course2md::tr!("{} 个已暂停", resumable)),
            (attention, course2md::tr!("{} 个需要处理", attention)),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join(" · ");
        // Pausing applies to work that is still meant to run; a task already
        // stopping has nothing left to pause.
        let pausable = tasks
            .iter()
            .filter(|task| {
                matches!(task.state, TaskState::Running | TaskState::Queued)
                    && task.intent == Intent::Run
                    && task.handled_by.is_none()
            })
            .count();
        let pause = (pausable > 0).then(|| {
            outline_pill("tasks-pause-all")
                .icon(icons::pause())
                .label(course2md::tr!("全部暂停"))
                .on_click(cx.listener(|this, _, _, cx| this.pause_all_tasks(cx)))
        });
        let resume = (resumable > 0).then(|| {
            outline_pill("tasks-resume-all")
                .icon(icons::play_arrow())
                .label(course2md::tr!("全部继续"))
                .on_click(cx.listener(|this, _, _, cx| this.resume_all_tasks(cx)))
        });
        h_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .items_center()
            .flex_wrap()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(theme::page_heading(
                        "tasks-page-title",
                        course2md::tr!("任务"),
                    ))
                    .when(!summary.is_empty(), |heading| {
                        heading.child(
                            accessible_text("tasks-page-summary", summary)
                                .text_size(TEXT_BODY)
                                .text_color(color(GRAY)),
                        )
                    }),
            )
            .child(h_flex().gap_2().children(pause).children(resume))
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
        if let Some(workspace) = &mut self.workspace
            && let Err(error) = workspace.transaction(|state| {
                queued
                    .iter()
                    .try_for_each(|id| state.set_intent(id, Intent::Pause))
            })
        {
            self.workspace_error =
                Some(course2md::tr!("任务状态尚未保存：{}", format!("{error:#}")));
        }
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
        if let Some(workspace) = &mut self.workspace
            && let Err(error) = workspace.transaction(|state| state.move_waiting_task(id, position))
        {
            self.workspace_error =
                Some(course2md::tr!("任务顺序尚未保存：{}", format!("{error:#}")));
        }
        cx.notify();
    }

    /// One item of the virtualized queue. Rows of a group share one surface;
    /// the wrapper's padding only separates surfaces and headings.
    fn queue_item(
        &mut self,
        item: &QueueItem,
        frame: QueueFrame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let QueueFrame {
            tasks,
            index,
            last,
            focus,
        } = frame;
        let mut gap = !last;
        let content = match item {
            QueueItem::Unavailable => v_flex()
                .gap_3()
                .child(icons::warning().size_8().text_color(color(WARNING)))
                .child(course2md::tr!("任务记录暂时无法读取"))
                .child(
                    outline_pill("task-records-open-library")
                        .icon(icons::book_open())
                        .label(course2md::tr!("前往笔记库"))
                        .self_start()
                        .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::Library, cx))),
                )
                .into_any_element(),
            QueueItem::Empty => v_flex()
                .gap_3()
                .py_12()
                .items_center()
                .child(icons::task().size_8().text_color(color(GRAY)))
                .child(
                    accessible_text("task-empty-title", course2md::tr!("还没有生成任务"))
                        .role(Role::Heading)
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(
                    primary_pill("task-new")
                        .icon(icons::plus())
                        .label(course2md::tr!("导入视频"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.prepare_workbench_input(window, cx) {
                                this.navigate(Page::New, cx);
                            }
                        })),
                )
                .into_any_element(),
            QueueItem::GroupLabel { group, count } => {
                let title = if *group == TaskGroup::Running {
                    group.label().to_owned()
                } else {
                    format!("{} · {count}", group.label())
                };
                h_flex()
                    .w_full()
                    .min_w_0()
                    .pt_4()
                    .px_4()
                    .gap_4()
                    .items_end()
                    .justify_between()
                    .child(theme::section_title(
                        SharedString::from(format!("task-group-{group:?}")),
                        title,
                    ))
                    .when(*group == TaskGroup::Queued && *count > 1, |row| {
                        row.child(
                            accessible_text(
                                "task-queue-order-hint",
                                course2md::tr!("拖动可以调整顺序"),
                            )
                            .text_size(TEXT_AUX)
                            .text_color(color(GRAY)),
                        )
                    })
                    .into_any_element()
            }
            QueueItem::Running(task_index) => {
                let task = &tasks[*task_index];
                let menu = self.task_menu(task, cx);
                div()
                    .id(SharedString::from(format!("task-menu-{}", task.id)))
                    .w_full()
                    .child(self.running_task_card(task, cx))
                    .context_menu(move |popup, window, cx| menu(popup, window, cx))
                    .into_any_element()
            }
            QueueItem::Row {
                task,
                group,
                number,
                runs,
                first,
                last: last_row,
            } => {
                gap = *last_row && !last;
                let place = RowPlace {
                    group: *group,
                    number: *number,
                    runs: *runs,
                    first: *first,
                    last: *last_row,
                };
                self.task_row(&tasks[*task], place, cx)
            }
            QueueItem::QueueFold { hidden, open } => {
                let toggle = open.clone();
                let opened = *open.read(cx);
                h_flex()
                    .w_full()
                    .justify_center()
                    .py_2()
                    .bg(color(SURFACE))
                    .rounded_b(RADIUS_CARD)
                    .child(
                        quiet("toggle-task-queue")
                            .text_color(color(ACCENT))
                            .label(if opened {
                                course2md::tr!("收起").to_owned()
                            } else {
                                course2md::tr!("显示另外 {} 个", hidden)
                            })
                            .child(
                                if opened {
                                    icons::chevron_up()
                                } else {
                                    icons::chevron_down()
                                }
                                .size_4(),
                            )
                            .on_click(move |_, _, cx| {
                                toggle.update(cx, |open, cx| {
                                    *open = !*open;
                                    cx.notify();
                                });
                            }),
                    )
                    .into_any_element()
            }
            QueueItem::HistoryToggle { count, open } => {
                let toggle = open.clone();
                quiet("toggle-task-history")
                    .self_start()
                    .mt_4()
                    .icon(if *open.read(cx) {
                        icons::chevron_up()
                    } else {
                        icons::chevron_down()
                    })
                    .label(course2md::tr!("此前处理记录 · {}", count))
                    .on_click(move |_, _, cx| {
                        toggle.update(cx, |open, cx| {
                            *open = !*open;
                            cx.notify();
                        });
                    })
                    .into_any_element()
            }
        };
        let gap_after = match item {
            QueueItem::GroupLabel { .. } => rems(8. / 14.),
            _ => rems(12. / 14.),
        };
        let mut wrapper = v_flex()
            .w_full()
            .min_w_0()
            .when(gap, |view| view.pb(gap_after))
            .child(content)
            .id(("queue-item", index))
            .tab_stop(false);
        if let Some(focus) = focus {
            wrapper = wrapper.track_focus(focus);
        }
        wrapper.into_any_element()
    }

    /// Every command for a task, on right-click and its ⋯ button: its note,
    /// its source, the controls that match its state, and cancelling last.
    pub(crate) fn task_menu(&self, task: &TaskRecord, cx: &mut Context<Self>) -> MenuBuilder {
        let entity = cx.weak_entity();
        let task = task.clone();
        Rc::new(move |menu, _, cx| {
            let Some(desktop) = entity.upgrade() else {
                return menu;
            };
            // Read when the menu opens, not for every row on every frame.
            let desktop = desktop.read(cx);
            let id = task.id.clone();
            let state = task.state;
            let active = desktop.is_running_task(&id);
            let open = desktop.expanded_task.as_ref() == Some(&id);
            let queue: Vec<String> = desktop
                .workspace
                .as_ref()
                .map(|workspace| {
                    workspace
                        .state
                        .waiting_tasks()
                        .map(|task| task.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            // The note opens from its path, as the row's button does, even
            // before the library has listed it.
            let course = task.artifact.as_ref().map(|dir| {
                desktop
                    .courses
                    .iter()
                    .find(|course| &course.dir == dir)
                    .cloned()
                    .unwrap_or_else(|| {
                        Rc::new(Course::from_completed(&Done {
                            out_dir: dir.clone(),
                            title: task.plan.title.clone(),
                            ..Default::default()
                        }))
                    })
            });
            let has_exports = desktop
                .task_library(&task)
                .is_some_and(|location| !workspace::task_export_files(&task, location).is_empty());
            let has_log = task_log_text(&task).is_some();
            let uncertain = task.has_uncertain();
            let reassociate = desktop
                .task_library(&task)
                .filter(|library| {
                    desktop
                        .cached_location_check(library)
                        .is_some_and(|check| check.needs_reassociation)
                })
                .map(|library| library.id.clone())
                .filter(|_| !desktop.storage_ui.busy);
            let failures = if task.artifact.is_some() {
                task_component_failures(&task)
            } else {
                Vec::new()
            };
            let follow = |action: fn(&mut Desktop, String, &mut Window, &mut Context<Desktop>)| {
                let entity = entity.clone();
                let id = id.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = entity.update(cx, |this, cx| action(this, id.clone(), window, cx));
                }
            };
            let intent = |intent: Intent| {
                let entity = entity.clone();
                let id = id.clone();
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    let _ =
                        entity.update(cx, |this, cx| this.set_task_intent(id.clone(), intent, cx));
                }
            };
            let mut menu = menu;
            if let Some(course) = course {
                let entity = entity.clone();
                menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("阅读笔记"))
                        .icon(icons::book_open())
                        .on_click(move |_, _, cx| {
                            let _ =
                                entity.update(cx, |this, cx| this.open_course(course.clone(), cx));
                        }),
                );
            }
            if let Some(path) = task.artifact.clone() {
                menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("打开笔记保存位置"))
                        .icon(icons::folder_open())
                        .on_click(move |_, _, cx| cx.open_with_system(&path)),
                );
            }
            if has_exports {
                menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("打开导出位置"))
                        .icon(icons::folder_open())
                        .on_click(follow(|this, id, _, cx| {
                            this.open_task_export_location(id, cx)
                        })),
                );
            }
            if task.plan.source.online {
                let url = task.plan.source.input.clone();
                let copied = url.clone();
                menu = menu
                    .item(
                        PopupMenuItem::new(course2md::tr!("打开原视频"))
                            .icon(icons::external_link())
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .item(
                        PopupMenuItem::new(course2md::tr!("复制视频链接"))
                            .icon(icons::content_copy())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                            }),
                    );
            }
            menu = menu.separator();
            if !matches!(state, TaskState::Running | TaskState::Pausing) {
                menu = menu.item(
                    PopupMenuItem::new(if open {
                        course2md::tr!("收起详情")
                    } else {
                        course2md::tr!("展开详情")
                    })
                    .icon(if open {
                        icons::chevron_up()
                    } else {
                        icons::chevron_down()
                    })
                    .on_click(follow(|this, id, _, cx| this.toggle_task_row(&id, cx))),
                );
            }
            if has_log {
                menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("查看日志"))
                        .icon(icons::file_text())
                        .on_click(follow(|this, id, _, cx| this.show_task_log(&id, cx))),
                );
            }
            if let Some(followup) = task.handled_by.clone() {
                let entity = entity.clone();
                menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("查看后续处理任务"))
                        .icon(icons::arrow_forward())
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| this.select_task(&followup, cx));
                        }),
                );
                return menu;
            }
            menu = menu.separator();
            if let Some(position) = queue.iter().position(|queued| queued == &id) {
                let last = queue.len() - 1;
                let to = |position: usize| {
                    let entity = entity.clone();
                    let id = id.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        let _ =
                            entity.update(cx, |this, cx| this.move_waiting_task(&id, position, cx));
                    }
                };
                menu = menu
                    .item(
                        PopupMenuItem::new(course2md::tr!("移到最前"))
                            .icon(icons::arrow_up())
                            .disabled(position == 0)
                            .on_click(to(0)),
                    )
                    .item(
                        PopupMenuItem::new(course2md::tr!("上移"))
                            .icon(icons::arrow_up())
                            .disabled(position == 0)
                            .on_click(to(position.saturating_sub(1))),
                    )
                    .item(
                        PopupMenuItem::new(course2md::tr!("下移"))
                            .icon(icons::arrow_down())
                            .disabled(position == last)
                            .on_click(to(position + 1)),
                    )
                    .item(
                        PopupMenuItem::new(course2md::tr!("移到最后"))
                            .icon(icons::arrow_down())
                            .disabled(position == last)
                            .on_click(to(last)),
                    )
                    .separator();
            }
            match state {
                TaskState::Running | TaskState::Pausing => {
                    if task.intent != Intent::Cancel {
                        menu = menu.item(
                            PopupMenuItem::new(if state == TaskState::Pausing {
                                course2md::tr!("正在暂停…")
                            } else {
                                course2md::tr!("暂停")
                            })
                            .icon(icons::pause())
                            .disabled(state == TaskState::Pausing)
                            .on_click(intent(Intent::Pause)),
                        );
                    }
                }
                TaskState::Queued if !active => {
                    menu = menu.item(
                        PopupMenuItem::new(course2md::tr!("暂停"))
                            .icon(icons::pause())
                            .on_click(intent(Intent::Pause)),
                    );
                }
                TaskState::Paused | TaskState::NeedsAttention if !uncertain => {
                    menu = menu.item(
                        PopupMenuItem::new(course2md::tr!("继续生成"))
                            .icon(icons::play_arrow())
                            .on_click(intent(Intent::Run)),
                    );
                }
                _ => {}
            }
            if let Some(library) = reassociate {
                let entity = entity.clone();
                menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("重新关联此保存位置"))
                        .icon(icons::storage())
                        .on_click(move |_, window, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.begin_library_reassociation(library.clone(), window, cx)
                            });
                        }),
                );
            }
            if uncertain && !active {
                menu = menu.item(
                    PopupMenuItem::new(course2md::tr!("重新发送以上内容"))
                        .icon(icons::refresh())
                        .on_click(follow(|this, id, _, cx| this.resend_uncertain(id, cx))),
                );
            }
            if task.artifact.is_some() && !uncertain {
                let requires_repair = task_requires_service_repair(&task);
                if requires_repair {
                    menu = menu.item(
                        PopupMenuItem::new(course2md::tr!("修复 AI 服务并补做"))
                            .icon(icons::settings())
                            .on_click(follow(|this, id, window, cx| {
                                this.repair_task_service(id, window, cx)
                            })),
                    );
                }
                for (component, _, _) in failures {
                    if requires_repair && ai_component(&component).is_some() {
                        continue;
                    }
                    let entity = entity.clone();
                    let id = id.clone();
                    let label = retry_component_label(&component);
                    menu = menu.item(PopupMenuItem::new(label).icon(icons::refresh()).on_click(
                        move |_, _, cx| {
                            let parsed =
                                Component::parse(&component).unwrap_or(Component::Proofreading);
                            let _ = entity.update(cx, |this, cx| {
                                this.reprocess_task(id.clone(), vec![parsed], Vec::new(), cx)
                            });
                        },
                    ));
                }
            }
            if !matches!(
                state,
                TaskState::Running | TaskState::Pausing | TaskState::Queued
            ) {
                menu = menu.item(
                    PopupMenuItem::new(adjust_label(&task))
                        .icon(icons::tune())
                        .on_click(follow(|this, id, window, cx| {
                            this.adjust_task(id, window, cx)
                        })),
                );
            }
            if !state.finished() {
                let cancelling = state == TaskState::Pausing && task.intent == Intent::Cancel;
                menu = menu.separator().item(
                    PopupMenuItem::new(if cancelling {
                        course2md::tr!("正在取消…")
                    } else {
                        course2md::tr!("取消任务")
                    })
                    .icon(icons::close())
                    .danger(true)
                    .disabled(cancelling)
                    .on_click(intent(Intent::Cancel)),
                );
            }
            menu
        })
    }

    /// Expand a row's details, or fold the expanded one.
    pub(crate) fn toggle_task_row(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.expanded_task.as_deref() == Some(id) {
            self.clear_task_card_motion(id);
            self.expanded_task = None;
            cx.notify();
            return;
        }
        self.select_task(id, cx);
        self.expanded_task = Some(id.to_owned());
    }

    /// Open a task's details with its log shown.
    fn show_task_log(&mut self, id: &str, cx: &mut Context<Self>) {
        self.task_panels_open
            .insert((TaskPanel::RawLog, id.to_owned()));
        let running = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(id))
            .is_some_and(|task| task_group(task) == TaskGroup::Running);
        if running {
            cx.notify();
        } else {
            self.select_task(id, cx);
        }
    }

    /// Pause and cancel for unfinished work, on bases the caller has placed
    /// and styled. A queued task has produced nothing to keep, so it offers
    /// only cancel.
    fn task_stop_buttons(
        &self,
        task: &TaskRecord,
        pause: Button,
        cancel: Button,
        cx: &mut Context<Self>,
    ) -> (Option<Button>, Button) {
        let id = task.id.clone();
        let cancelling = task.state == TaskState::Pausing && task.intent == Intent::Cancel;
        let pausing = task.state == TaskState::Pausing && !cancelling;
        let queued = task.state == TaskState::Queued && !self.is_running_task(&task.id);
        let pause = (!cancelling && !queued).then(|| {
            pause
                .icon(icons::pause())
                .label(if pausing {
                    course2md::tr!("正在暂停…")
                } else {
                    course2md::tr!("暂停")
                })
                .loading(pausing)
                .disabled(pausing)
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| this.set_task_intent(id.clone(), Intent::Pause, cx)
                }))
        });
        let cancel = cancel
            .icon(icons::close())
            .label(if cancelling {
                course2md::tr!("正在取消…")
            } else {
                course2md::tr!("取消任务")
            })
            .loading(cancelling)
            .disabled(cancelling)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_task_intent(id.clone(), Intent::Cancel, cx)
            }));
        (pause, cancel)
    }

    /// A row of a group's surface: identity, status and the row's commands.
    /// Clicking it folds its details out below, inside the same surface.
    fn task_row(
        &mut self,
        task: &TaskRecord,
        place: RowPlace,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let RowPlace {
            group,
            number,
            runs,
            first,
            last,
        } = place;
        let id = task.id.clone();
        let expanded = self.expanded_task.as_ref() == Some(&id);
        let reason = (group == TaskGroup::Attention && !expanded)
            .then(|| attention_reason(task))
            .flatten();
        let added = course2md::tr!(
            "{} 添加",
            crate::reader_navigation::timestamp_local(task.created * 1000)
        );
        let identity = h_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .items_center()
            .child(task_row_cover(
                self.task_cover(task),
                group == TaskGroup::Queued,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        accessible_text(
                            SharedString::from(format!("task-title-{id}")),
                            task.plan.title.clone(),
                        )
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal()
                        .text_ellipsis()
                        .line_clamp(2),
                    )
                    .child(
                        accessible_text(SharedString::from(format!("task-created-{id}")), added)
                            .text_size(TEXT_AUX)
                            .whitespace_normal()
                            .text_color(color(GRAY)),
                    )
                    .children(reason.map(|reason| {
                        accessible_text(SharedString::from(format!("task-reason-{id}")), reason)
                            .text_size(TEXT_AUX)
                            .whitespace_normal()
                            .text_color(color(DANGER))
                    })),
            );
        // The row carries the pointer feedback, so the whole row reads as one target.
        let toggle = control(SharedString::from(format!("select-{id}")))
            .text()
            // Below this the commands wrap under the identity instead of squeezing it.
            .min_w(rems(16.))
            .accessibility_label(course2md::tr!(
                "{}，{}，{}",
                task.plan.title,
                task_status_label(task),
                if expanded {
                    course2md::tr!("收起")
                } else {
                    course2md::tr!("查看详情")
                }
            ))
            .flex_1()
            .h_auto()
            .min_h(px(0.))
            .p_0()
            .justify_start()
            .rounded(RADIUS_SMALL)
            .child(identity)
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.toggle_task_row(&id, cx)
            }));
        let mut trailing = h_flex()
            .flex_shrink_0()
            .gap_2()
            .items_center()
            // A queued row's number already says it waits.
            .when(task.state != TaskState::Queued, |trailing| {
                trailing.child(badge(task_badge_kind(task.state)).child(task_status_label(task)))
            });
        if let Some(path) = task
            .artifact
            .clone()
            .filter(|_| matches!(group, TaskGroup::Finished | TaskGroup::Attention))
        {
            let title = task.plan.title.clone();
            trailing = trailing.child(
                quiet(SharedString::from(format!("task-read-direct-{id}")))
                    .text_color(color(ACCENT))
                    .icon(icons::book_open())
                    .label(course2md::tr!("阅读笔记"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_course(
                            Course::from_completed(&Done {
                                out_dir: path.clone(),
                                title: title.clone(),
                                ..Default::default()
                            }),
                            cx,
                        );
                    })),
            );
        }
        let menu = self.task_menu(task, cx);
        let dropdown = menu.clone();
        trailing = trailing.child(
            quiet(SharedString::from(format!("task-more-{id}")))
                .icon(icons::ellipsis())
                .accessibility_label(course2md::tr!("更多操作"))
                .tooltip(course2md::tr!("更多操作"))
                .dropdown_menu(move |popup, window, cx| dropdown(popup, window, cx)),
        );
        // The divider spans the row's identity and commands, from the cover on.
        // At narrow widths or large text the commands move under the identity.
        let body = h_flex()
            .flex_1()
            .min_w_0()
            .gap_x_4()
            .gap_y_2()
            .py_3()
            .items_center()
            .flex_wrap()
            .when(!first, |body| {
                body.border_t_1().border_color(color(HAIRLINE))
            })
            .child(toggle)
            .child(trailing);
        let mut row = h_flex()
            .id(SharedString::from(format!("task-row-{id}")))
            .w_full()
            .min_w_0()
            .pl_4()
            .pr_2()
            .gap_3()
            .items_center()
            .when(first, |row| row.rounded_t(RADIUS_CARD))
            .when(last && !expanded, |row| row.rounded_b(RADIUS_CARD))
            .hover(|style| style.bg(blend(color(SURFACE), color(INK), 0.05)));
        if let Some(number) = number {
            row = row.child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .items_center()
                    .cursor_grab()
                    .child(
                        icons::grip_vertical()
                            .size(ICON_SLOT)
                            .text_color(color(GRAY)),
                    )
                    .child(
                        div()
                            .min_w(rems(20. / 14.))
                            .text_size(TEXT_BODY)
                            .text_color(color(GRAY))
                            .children(runs.map(|runs| runs.to_string())),
                    ),
            );
            let drag = QueueDrag {
                id: id.clone(),
                title: task.plan.title.clone().into(),
            };
            row = row
                .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                .drag_over::<QueueDrag>(|style, _, _, _| style.bg(color(SECONDARY_CONTAINER)))
                .on_drop(cx.listener(move |this, drag: &QueueDrag, _, cx| {
                    this.move_waiting_task(&drag.id, number - 1, cx)
                }));
        }
        let row = row.child(body);
        let mut surface = v_flex()
            .w_full()
            .min_w_0()
            .bg(color(SURFACE))
            .when(first, |view| view.rounded_t(RADIUS_CARD))
            .when(last, |view| view.rounded_b(RADIUS_CARD))
            .child(row);
        if expanded {
            // Padding outside the detail keeps its divider on the rows' inset.
            let detail = div().pl_4().pr_2().pb_4().child(self.task_detail(task, cx));
            let detail_id = format!("task-detail-{id}");
            surface = surface.child(if self.enter_once(detail_id.clone()) {
                crate::motion::enter(SharedString::from(detail_id), detail)
            } else {
                detail.into_any_element()
            });
        }
        surface
            .id(SharedString::from(format!("task-menu-{id}")))
            .context_menu(move |popup, window, cx| menu(popup, window, cx))
            .into_any_element()
    }

    /// The task being processed: its identity, the step it is on, every step
    /// so far, the settings it was submitted with, and its controls.
    fn running_task_card(
        &mut self,
        task: &TaskRecord,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = task.id.clone();
        let active = self.is_running_task(&id);
        let library: SharedString = self
            .task_library(task)
            .map(|location| location.name.clone().into())
            .unwrap_or_else(|| course2md::tr!("保存位置暂时不可用").into());
        let header = h_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .items_center()
            .child(task_row_cover(self.task_cover(task), false))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        accessible_text(
                            SharedString::from(format!("task-title-{id}")),
                            task.plan.title.clone(),
                        )
                        .text_size(TEXT_TITLE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal()
                        .text_ellipsis()
                        .line_clamp(2),
                    )
                    .child(
                        accessible_text(
                            SharedString::from(format!("task-created-{id}")),
                            format!(
                                "{} · {}",
                                course2md::tr!(
                                    "{} 添加",
                                    crate::reader_navigation::timestamp_local(task.created * 1000)
                                ),
                                course2md::tr!("保存到 {}", library)
                            ),
                        )
                        .text_size(TEXT_AUX)
                        .whitespace_normal()
                        .text_color(color(GRAY)),
                    ),
            )
            .child(badge(task_badge_kind(task.state)).child(task_status_label(task)));
        let stages = self.task_stage_rows(task);
        // The step in progress and the job's clock, on one line that never
        // wraps under a neighbour.
        // Later steps depend on what the video turns out to need, so the
        // position counts the steps reached so far, without a total.
        let current = stages
            .iter()
            .position(|stage| {
                matches!(
                    stage.status,
                    StageStatus::Running | StageStatus::Pausing | StageStatus::Stopping
                )
            })
            .map(|index| course2md::tr!("第 {} 步 · {}", index + 1, stages[index].title));
        let waiting = worker_wait_state(
            active,
            self.pending_done.is_some(),
            task.intent,
            self.progress.iter().map(|(name, item)| (name, item.done)),
        );
        let step = current.or_else(|| waiting.map(|waiting| waiting.label().to_owned()));
        let clock = self
            .job_started
            .filter(|_| active && self.job.is_some())
            .map(|started| activity::elapsed_label(started.elapsed()));
        let status = (step.is_some() || clock.is_some()).then(|| {
            h_flex()
                .w_full()
                .min_w_0()
                .gap_4()
                .items_center()
                .children(step.map(|step| {
                    accessible_text(SharedString::from(format!("task-step-{id}")), step)
                        .flex_1()
                        .min_w_0()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                }))
                .children(clock.map(|clock| {
                    accessible_text(SharedString::from(format!("task-clock-{id}")), clock)
                        .flex_shrink_0()
                        .whitespace_nowrap()
                        .text_size(TEXT_AUX)
                        .text_color(color(GRAY))
                }))
        });
        let steps = v_flex()
            .flex_1()
            .flex_basis(rems(24.))
            .min_w(rems(18.))
            .gap_3()
            .when(!stages.is_empty(), |view| {
                view.child(TaskStages { rows: stages })
            })
            .when_some(waiting, |view, waiting| {
                view.child(
                    semantic_label(
                        SharedString::from(format!("task-waiting-{id}")),
                        waiting.label(),
                        crate::motion::spinner(
                            SharedString::from(format!("task-waiting-spinner-{id}")),
                            cx,
                        ),
                    )
                    .text_color(color(GRAY)),
                )
            });
        let body = h_flex()
            .w_full()
            .min_w_0()
            .gap_6()
            .items_start()
            .flex_wrap()
            .child(steps)
            .child(self.task_settings_panel(task, &library));
        let log_open = self
            .task_panels_open
            .contains(&(TaskPanel::RawLog, id.clone()));
        let log = task_log_text(task);
        let mut footer = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .pt_3()
            .items_center()
            .flex_wrap()
            .border_t_1()
            .border_color(color(HAIRLINE));
        let (pause, cancel) = self.task_stop_buttons(
            task,
            tonal_pill(SharedString::from(format!("pause-{id}")), cx),
            quiet(SharedString::from(format!("cancel-{id}"))).text_color(color(DANGER)),
            cx,
        );
        footer = footer.children(pause);
        if log.is_some() {
            footer = footer.child(
                quiet(SharedString::from(format!("task-log-toggle-{id}")))
                    .text_color(color(ACCENT))
                    .icon(icons::file_text())
                    .label(if log_open {
                        course2md::tr!("收起日志")
                    } else {
                        course2md::tr!("查看日志")
                    })
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.toggle_task_panel(TaskPanel::RawLog, &id);
                            cx.notify();
                        }
                    })),
            );
        }
        footer = footer.child(div().flex_1()).child(cancel);
        v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .p_5()
            .rounded(RADIUS_CARD)
            .bg(color(SURFACE))
            .child(header)
            .children(status)
            .children(self.task_feedback_view(task))
            .children(self.uncertain_block(task, cx))
            .child(body)
            .child(footer)
            .when_some(log.filter(|_| log_open), |card, log| {
                card.child(task_log_box(&id, log))
            })
            .into_any_element()
    }

    /// The running card's side panel: where the task saves, where its text
    /// comes from, and which service receives its content.
    fn task_settings_panel(&self, task: &TaskRecord, library: &SharedString) -> Div {
        v_flex()
            .flex_1()
            .flex_basis(rems(16.))
            .min_w(rems(14.))
            .max_w(rems(22.))
            .gap_3()
            .p_4()
            .rounded(RADIUS_CARD)
            .border_1()
            .border_color(color(HAIRLINE))
            .child(
                div()
                    .text_size(TEXT_BODY)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(course2md::tr!("这次的设置")),
            )
            .child(self.task_facts(task, library.clone()))
    }

    /// The expanded part of a row: what happened, the recovery that applies,
    /// and the record of finished steps and the raw log.
    fn task_detail(&mut self, task: &TaskRecord, cx: &mut Context<Self>) -> Div {
        let id = task.id.clone();
        let failures = if task.artifact.is_some() {
            task_component_failures(task)
        } else {
            Vec::new()
        };
        let mut details = v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .pt_3()
            .border_t_1()
            .border_color(color(HAIRLINE));
        let destination = self
            .task_library(task)
            .map(|lib| lib.name.clone())
            .unwrap_or_else(|| course2md::tr!("保存位置暂时不可用").into());
        let facts = self.task_facts(task, destination);
        let (progress, mut history) = self.task_progress_sections(task, cx);
        // A card that needs the person keeps only what they can act on; destination and text source move into 「查看已完成的处理」.
        let recovery = matches!(
            task.state,
            TaskState::NeedsAttention | TaskState::Partial | TaskState::Uncertain
        );
        if recovery {
            if let Some(history) = &mut history {
                history.lead = Some(facts);
            }
        } else {
            details = details.child(facts);
        }
        details = details.when_some(
            progress.filter(|_| task.state != TaskState::Partial),
            |view, progress| view.child(progress),
        );
        let uncertain: Vec<_> = task.blocked.iter().filter(|b| b.is_uncertain()).collect();
        if let Some(message) = self
            .task_feedback_view(task)
            .filter(|_| task.state != TaskState::Partial)
        {
            details = details.child(message);
        }
        if let Some(block) = self.uncertain_block(task, cx) {
            details = details.child(block);
        }
        let style = RecoveryStyle::Card;
        let mut actions = h_flex()
            .gap_2()
            .flex_wrap()
            .children(self.reassociate_action(
                self.task_library(task),
                style.button("reassociate", &id, Emphasis::Secondary),
                cx,
            ));
        if let Some(followup) = &task.handled_by {
            let next = followup.clone();
            actions = actions.child(
                control(SharedString::from(format!("followup-{id}")))
                    .icon(icons::arrow_forward())
                    .label(course2md::tr!("查看后续处理任务"))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_task(&next, cx))),
            );
        }
        if task.state == TaskState::Queued {
            let id = &task.id;
            let (pause, cancel) = self.task_stop_buttons(
                task,
                control(SharedString::from(format!("pause-{id}"))),
                control(SharedString::from(format!("cancel-{id}"))),
                cx,
            );
            actions = actions.children(pause).child(cancel);
        }
        if task.handled_by.is_none()
            && matches!(task.state, TaskState::Paused | TaskState::NeedsAttention)
            && uncertain.is_empty()
        {
            actions = actions.child(self.resume_action(task, style, cx));
        }
        if task.artifact.is_some() {
            let requires_repair = task_requires_service_repair(task);
            // Outside a partial result the feedback line already carries the reason.
            if let Some(reason) =
                task_service_repair_reason(task).filter(|_| task.state == TaskState::Partial)
            {
                details = details.child(
                    accessible_text(
                        SharedString::from(format!("service-repair-reason-{id}")),
                        reason,
                    )
                    .text_sm(),
                );
            }
            // Repair is offered only when the service rejected its
            // configuration; any other failure is retried as it is.
            if requires_repair && uncertain.is_empty() && task.handled_by.is_none() {
                actions = actions.child(self.repair_ai_action(task, style, cx));
            }
            for (component, label, outcome) in &failures {
                let unknown_component = uncertain.iter().any(|request| {
                    request
                        .purpose
                        .as_deref()
                        .and_then(ai_component)
                        .is_some_and(|part| part.as_str() == component)
                });
                if unknown_component {
                    continue;
                }
                let reason = activity::outcome_failure_message(label, outcome);
                let ai_component = ai_component(component).is_some();
                if !(requires_repair && ai_component) {
                    details = details.child(
                        accessible_text(
                            SharedString::from(format!("outcome-{id}-{component}")),
                            reason,
                        )
                        .text_sm(),
                    );
                }
                if uncertain.is_empty()
                    && task.handled_by.is_none()
                    && !(requires_repair && ai_component)
                {
                    actions =
                        actions.child(self.retry_component_action(task, component, style, cx));
                }
            }
        }
        if task.handled_by.is_none()
            && !matches!(
                task.state,
                TaskState::Running | TaskState::Pausing | TaskState::Queued
            )
        {
            actions = actions.child(self.adjust_action(task, style, cx));
        }
        details = details.child(actions);
        // The two expanded rows are one group: they keep detail_row's height and gap_1, not the section spacing.
        let logs = self.task_log_view(task, cx);
        if history.is_some() || logs.is_some() {
            details = details.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .when_some(history, |view, history| view.child(history))
                    .when_some(logs, |view, logs| view.child(logs)),
            );
        }
        details
    }

    /// Re-associate a library location whose identity marker needs confirming.
    pub(crate) fn reassociate_action(
        &self,
        library: Option<&workspace::LibraryLocation>,
        button: Button,
        cx: &mut Context<Self>,
    ) -> Option<Button> {
        let library = library.filter(|library| {
            self.cached_location_check(library)
                .is_some_and(|check| check.needs_reassociation)
        })?;
        let library_id = library.id.clone();
        Some(
            button
                .icon(icons::storage())
                .label(course2md::tr!("重新关联此保存位置"))
                .disabled(self.storage_ui.busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.begin_library_reassociation(library_id.clone(), window, cx)
                })),
        )
    }

    fn resume_action(
        &self,
        task: &TaskRecord,
        style: RecoveryStyle,
        cx: &mut Context<Self>,
    ) -> Button {
        let id = task.id.clone();
        style
            .button("resume", &task.id, Emphasis::Primary)
            .icon(icons::play_arrow())
            .label(course2md::tr!("继续生成"))
            .on_click(
                cx.listener(move |this, _, _, cx| {
                    this.set_task_intent(id.clone(), Intent::Run, cx)
                }),
            )
    }

    fn repair_ai_action(
        &self,
        task: &TaskRecord,
        style: RecoveryStyle,
        cx: &mut Context<Self>,
    ) -> Button {
        let id = task.id.clone();
        style
            .button("repair-ai", &task.id, Emphasis::Secondary)
            .icon(icons::settings())
            .label(course2md::tr!("修复 AI 服务并补做"))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.repair_task_service(id.clone(), window, cx);
            }))
    }

    fn retry_component_action(
        &self,
        task: &TaskRecord,
        component: &str,
        style: RecoveryStyle,
        cx: &mut Context<Self>,
    ) -> Button {
        let id = task.id.clone();
        let component = component.to_owned();
        style
            .button(&format!("retry-{component}"), &task.id, Emphasis::Quiet)
            .icon(icons::refresh())
            .label(retry_component_label(&component))
            .on_click(cx.listener(move |this, _, _, cx| {
                let parsed = Component::parse(&component).unwrap_or(Component::Proofreading);
                this.reprocess_task(id.clone(), vec![parsed], Vec::new(), cx)
            }))
    }

    fn adjust_action(
        &self,
        task: &TaskRecord,
        style: RecoveryStyle,
        cx: &mut Context<Self>,
    ) -> Button {
        let id = task.id.clone();
        style
            .button("adjust", &task.id, Emphasis::Secondary)
            .icon(icons::tune())
            .label(adjust_label(task))
            .on_click(
                cx.listener(move |this, _, window, cx| this.adjust_task(id.clone(), window, cx)),
            )
    }

    /// Collapsing a card unmounts its detail; expanding it later starts fresh.
    pub(crate) fn clear_task_card_motion(&mut self, id: &str) {
        self.entered.remove(&format!("task-detail-{id}"));
        for panel in TaskPanel::ALL {
            self.entered.remove(&panel.entrance_key(id));
            self.task_panels_open.remove(&(panel, id.to_owned()));
        }
    }

    fn toggle_task_panel(&mut self, panel: TaskPanel, id: &str) {
        let key = (panel, id.to_owned());
        if !self.task_panels_open.remove(&key) {
            self.task_panels_open.insert(key);
        } else {
            // Closing unmounts the detail; reopening re-enters.
            self.entered.remove(&panel.entrance_key(id));
        }
    }

    /// An opened panel enters once; scrolling the row out of the virtualized
    /// list and back must not replay the entrance.
    fn task_disclosure(
        &mut self,
        panel: TaskPanel,
        task_id: &str,
        content: Div,
        cx: &mut Context<Self>,
    ) -> TaskDisclosure {
        let open = self.task_panels_open.contains(&(panel, task_id.to_owned()));
        let animate = open && self.enter_once(panel.entrance_key(task_id));
        TaskDisclosure {
            panel,
            task_id: task_id.to_owned(),
            lead: None,
            content,
            open,
            animate,
            desktop: cx.weak_entity(),
        }
    }

    /// Live and retained stages use the same labels, status column and icon slot.
    fn task_stage_rows(&self, task: &TaskRecord) -> Vec<TaskStage> {
        let active = self.is_running_task(&task.id);
        // `Stage` orders by pipeline position, so the set is already sorted.
        let mut names: std::collections::BTreeSet<_> = task.stages.keys().cloned().collect();
        if active {
            names.extend(self.progress.keys().cloned());
        }
        names
            .into_iter()
            .map(|name| {
                let stored = task.stages.get(&name);
                let live = active.then(|| self.progress.get(&name)).flatten();
                let done = live.map(|stage| stage.done).unwrap_or_else(|| {
                    stored.is_some_and(|stage| stage.status == EngineStageStatus::Done)
                });
                let outcomes = if active {
                    self.pending_done
                        .as_ref()
                        .and_then(|done| done.outcomes.as_ref())
                } else {
                    task.outcomes.as_ref()
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
                let current = live
                    .map(|stage| stage.current)
                    .or_else(|| stored.map(|stage| stage.current))
                    .unwrap_or(0);
                let total = live
                    .map(|stage| stage.total)
                    .or_else(|| stored.map(|stage| stage.total))
                    .unwrap_or(0);
                let (detail, fraction) = task_stage_progress(&name, done, live, current, total);
                TaskStage {
                    id: SharedString::from(format!("task-stage-{}-{name}", task.id)),
                    title: activity::title(&name),
                    status,
                    detail,
                    fraction,
                }
            })
            .collect()
    }

    /// Compact workbench status uses the same measured progress as task details.
    /// A waiting line with the running job's clock, so a pause between
    /// stages never looks stalled.
    pub(crate) fn with_job_clock(&self, label: &str) -> String {
        match self.job_started.filter(|_| self.job.is_some()) {
            Some(started) => format!("{label} · {}", activity::elapsed_label(started.elapsed())),
            None => label.to_owned(),
        }
    }

    pub(crate) fn compact_task_progress(&self, task: &TaskRecord) -> Option<(String, Option<f32>)> {
        self.task_stage_rows(task)
            .into_iter()
            .find(|stage| {
                matches!(
                    stage.status,
                    StageStatus::Running | StageStatus::Pausing | StageStatus::Stopping
                )
            })
            .map(|stage| {
                let label = if stage.detail.is_empty() {
                    stage.title
                } else {
                    format!("{} · {}", stage.title, stage.detail)
                };
                (label, stage.fraction)
            })
    }

    fn task_progress_sections(
        &mut self,
        task: &TaskRecord,
        cx: &mut Context<Self>,
    ) -> (Option<Div>, Option<TaskDisclosure>) {
        let active = self.is_running_task(&task.id);
        // While the task runs every stage stays in order, finished ones above
        // the current one; afterwards the finished ones fold into history.
        let (completed, current): (Vec<_>, Vec<_>) = self
            .task_stage_rows(task)
            .into_iter()
            .partition(|stage| !active && stage.status == StageStatus::Complete);
        let waiting = worker_wait_state(
            active,
            self.pending_done.is_some(),
            task.intent,
            self.progress.iter().map(|(name, item)| (name, item.done)),
        );
        let progress = if current.is_empty() && waiting.is_none() {
            None
        } else {
            let mut view = v_flex().w_full().min_w_0().gap_3();
            if !current.is_empty() {
                view = view.child(TaskStages { rows: current });
            }
            if let Some(waiting) = waiting {
                view = view.child(
                    semantic_label(
                        SharedString::from(format!("task-waiting-{}", task.id)),
                        self.with_job_clock(waiting.label()),
                        crate::motion::spinner(
                            SharedString::from(format!("task-waiting-spinner-{}", task.id)),
                            cx,
                        ),
                    )
                    .text_color(color(GRAY)),
                );
            }
            Some(view)
        };
        let history = (!completed.is_empty()).then(|| {
            let content = v_flex()
                .w_full()
                .min_w_0()
                .child(TaskStages { rows: completed });
            self.task_disclosure(TaskPanel::Stages, &task.id, content, cx)
        });
        (progress, history)
    }

    fn task_log_view(
        &mut self,
        task: &TaskRecord,
        cx: &mut Context<Self>,
    ) -> Option<TaskDisclosure> {
        task_log_text(task).map(|text| {
            let content = v_flex().child(task_log_box(&task.id, text));
            self.task_disclosure(TaskPanel::RawLog, &task.id, content, cx)
        })
    }

    fn task_feedback_view(&self, task: &TaskRecord) -> Option<AnyElement> {
        if task.has_uncertain() {
            return None;
        }
        let id = SharedString::from(format!("task-feedback-{}", task.id));
        // A concrete service rejection replaces the generic failure line: one
        // reason per task, beside its repair action.
        task_service_repair_reason(task)
            .map(str::to_owned)
            .or_else(|| task_failure_summary(task.state, task.error.as_deref()))
            .map(|message| {
                accessible_text(id, message)
                    .role(Role::Alert)
                    .text_size(TEXT_BODY)
                    .whitespace_normal()
                    .text_color(color(DANGER))
                    .into_any_element()
            })
    }

    /// Uncertain-outcome block with the explicit resend action; shared by the
    /// queue page and the workbench attention card.
    fn uncertain_block(&self, task: &TaskRecord, cx: &mut Context<Self>) -> Option<Div> {
        let id = task.id.clone();
        let uncertain: Vec<_> = task.blocked.iter().filter(|b| b.is_uncertain()).collect();
        if uncertain.is_empty() || task.handled_by.is_some() {
            return None;
        }
        let active = self.is_running_task(&id);
        Some(
            v_flex()
                .gap_2()
                .p_3()
                .rounded(RADIUS_CARD)
                .bg(color(WARNING_BG))
                .children(uncertain.iter().enumerate().map(|(index, blocked)| {
                    accessible_text(
                        SharedString::from(format!("unknown-scope-{id}-{index}")),
                        request_scope(blocked),
                    )
                    .text_sm()
                }))
                .child(accessible_text(
                    SharedString::from(format!("unknown-effect-{id}")),
                    course2md::tr!("服务可能已处理这些内容，重新发送可能产生额外费用"),
                ))
                .when(active, |view| {
                    view.child(accessible_text(
                        SharedString::from(format!("unknown-saving-{id}")),
                        course2md::tr!("正在保存当前结果…"),
                    ))
                })
                .child(
                    primary_pill(SharedString::from(format!("resend-{id}")))
                        .self_start()
                        .icon(icons::refresh())
                        .label(course2md::tr!("重新发送以上内容"))
                        .disabled(active)
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| this.resend_uncertain(id.clone(), cx)
                        })),
                ),
        )
    }

    fn task_cover(&self, task: &TaskRecord) -> Option<PathBuf> {
        task.plan
            .source
            .cover
            .clone()
            .or_else(|| self.cover_by_source.get(&task.plan.source_id).cloned())
    }

    fn task_heading(&self, task: &TaskRecord) -> Div {
        h_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .items_start()
            .when_some(self.task_cover(task), |row, cover| {
                row.child(cover_slot(rems(8.), Some(cover)))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        accessible_text(
                            SharedString::from(format!("workbench-task-title-{}", task.id)),
                            task.plan.title.clone(),
                        )
                        .text_size(TEXT_TITLE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal(),
                    )
                    .child(
                        h_flex().gap_3().flex_wrap().items_center().child(
                            badge(task_badge_kind(task.state)).child(task_status_label(task)),
                        ),
                    ),
            )
    }

    /// Where a task saves and how it processes: one set of facts shared by the
    /// running card and the expanded record.
    fn task_facts(&self, task: &TaskRecord, destination: impl Into<SharedString>) -> Div {
        let source = task_text_source(task);
        let fact_value = |value: SharedString| {
            div()
                .text_size(TEXT_BODY)
                .whitespace_normal()
                .child(value)
        };
        let mut facts = v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .child(detail_row(
                SharedString::from(format!("task-destination-{}", task.id)),
                course2md::tr!("保存到"),
                icons::folder_open()
                    .size(rems(20. / 14.))
                    .text_color(color(GRAY)),
                fact_value(destination.into()),
            ))
            .child(detail_row(
            SharedString::from(format!("task-text-source-{}", task.id)),
            course2md::tr!("文字来源"),
            icons::subtitles()
                .size(rems(20. / 14.))
                .text_color(color(GRAY)),
            fact_value(source.into()),
        ));
        if task.plan.config.llm.uses_service()
            && let Some(version) = task
                .plan
                .ai_service
                .as_ref()
                .and_then(|id| self.preferences.version(id))
        {
            facts = facts.child(detail_row(
                SharedString::from(format!("task-ai-destination-{}", task.id)),
                course2md::tr!("AI 处理"),
                icons::auto_fix()
                    .size(rems(20. / 14.))
                    .text_color(color(GRAY)),
                fact_value(
                    course2md::tr!(
                        "{}发送到「{}」",
                        if task.plan.options.vision {
                            course2md::tr!("文字与截图")
                        } else {
                            course2md::tr!("文字")
                        },
                        version.config.name,
                    )
                    .into(),
                ),
            ));
        }
        facts
    }

    pub(super) fn box_task_opening_result(&self, task: &TaskRecord, cx: &App) -> Div {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(self.task_heading(task))
            .child(crate::motion::state_enter(
                SharedString::from(format!("task-opening-result-{}", task.id)),
                semantic_label(
                    SharedString::from(format!("task-opening-result-label-{}", task.id)),
                    course2md::tr!("正在打开…"),
                    crate::motion::spinner(
                        SharedString::from(format!("task-opening-result-spinner-{}", task.id)),
                        cx,
                    ),
                )
                .text_color(color(SUCCESS)),
            ))
    }

    /// Workbench box card for a task that needs the user: paused, failed or
    /// uncertain outcomes, each with its real recovery action.
    pub fn box_task_attention(&mut self, task: &TaskRecord, cx: &mut Context<Self>) -> Div {
        let id = task.id.clone();
        let uncertain = task.has_uncertain() && task.handled_by.is_none();
        // Partly complete: the note is published; each unfinished extra is named and can be retried alone.
        let partial_failures: Vec<(String, String, String)> = if task.state == TaskState::Partial {
            task.artifact
                .as_ref()
                .map(|_| {
                    task_component_failures(task)
                        .into_iter()
                        .map(|(component, label, outcome)| {
                            let reason = activity::outcome_failure_message(&label, &outcome);
                            (component, label, reason)
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let requires_repair = task_requires_service_repair(task);
        let style = RecoveryStyle::Workbench;
        let mut card = v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(self.task_heading(task));
        // A partly published note shows its unfinished parts instead of the
        // task's feedback, progress, history and log.
        let partial = !partial_failures.is_empty();
        if !partial {
            card = card.children(self.task_feedback_view(task));
        }
        // Without partial failures the feedback line above already names the reason.
        let missing = partial_failures
            .iter()
            .filter(|(component, _, _)| ai_component(component).is_some())
            .map(|(_, label, _)| label.as_str())
            .collect::<Vec<_>>()
            .join(course2md::tr!("、"));
        if let Some(reason) = task_service_repair_reason(task).filter(|_| !missing.is_empty()) {
            card = card.child(
                accessible_text(
                    SharedString::from(format!("box-service-repair-reason-{id}")),
                    course2md::tr!("{}尚未完成。{}", missing, reason),
                )
                .text_sm(),
            );
        }
        for (component, _, reason) in partial_failures
            .iter()
            .filter(|(component, _, _)| !requires_repair || !ai_component(component).is_some())
        {
            card = card.child(
                accessible_text(
                    SharedString::from(format!("box-partial-{id}-{component}")),
                    reason.clone(),
                )
                .text_sm(),
            );
        }
        if let Some(block) = self.uncertain_block(task, cx) {
            card = card.child(block);
        }
        let (progress, history) = if partial {
            (None, None)
        } else {
            self.task_progress_sections(task, cx)
        };
        card = card.children(progress);
        let mut actions = h_flex().gap_2().flex_wrap();
        if partial {
            if let Some(path) = &task.artifact {
                let path = path.clone();
                let title = task.plan.title.clone();
                actions = actions.child(
                    primary_pill(SharedString::from(format!("box-read-{id}")))
                        .icon(icons::book_open())
                        .label(course2md::tr!("阅读笔记"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_course(
                                Course::from_completed(&Done {
                                    out_dir: path.clone(),
                                    title: title.clone(),
                                    ..Default::default()
                                }),
                                cx,
                            );
                        })),
                );
            }
            if requires_repair {
                actions = actions.child(self.repair_ai_action(task, style, cx));
            }
            for (component, _, _) in &partial_failures {
                if requires_repair && ai_component(component).is_some() {
                    continue;
                }
                actions = actions.child(self.retry_component_action(task, component, style, cx));
            }
            let details_id = id.clone();
            actions = actions.child(
                quiet(SharedString::from(format!("box-task-details-{id}")))
                    .icon(icons::task())
                    .label(course2md::tr!("查看任务详情"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_task(&details_id, cx);
                        this.navigate(Page::Task, cx);
                    })),
            );
        } else if !uncertain {
            actions = actions.child(self.resume_action(task, style, cx).self_start());
        }
        if !partial {
            actions = actions
                .children(self.reassociate_action(
                    self.task_library(task),
                    style.button("reassociate", &id, Emphasis::Secondary),
                    cx,
                ))
                .child(self.adjust_action(task, style, cx))
                .child(
                    quiet(SharedString::from(format!("box-cancel-{id}")))
                        .icon(icons::close())
                        .label(course2md::tr!("取消任务"))
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                this.set_task_intent(id.clone(), Intent::Cancel, cx)
                            }
                        })),
                );
        }
        card = card.child(actions).children(history);
        if !partial {
            card = card.children(self.task_log_view(task, cx));
        }
        card
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
        .logs
        .iter()
        .chain(task.error.iter())
        .chain(task.blocked.iter().map(|request| &request.message))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

fn task_log_box(id: &str, text: String) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("task-raw-log-scroll-{id}")))
        .w_full()
        .min_w_0()
        .max_h(rems(16.))
        .overflow_y_scroll()
        .p_4()
        .rounded(RADIUS_CARD)
        .bg(color(INSET))
        .child(
            accessible_text(SharedString::from(format!("task-raw-log-text-{id}")), text)
                .text_size(TEXT_AUX)
                .text_color(color(GRAY))
                .whitespace_normal(),
        )
}

/// A 16:9 cover slot; queued and coverless tasks show what they wait for.
fn task_row_cover(cover: Option<PathBuf>, queued: bool) -> Div {
    let placeholder = cover.is_none();
    cover_slot(rems(96. / 14.), cover)
        .max_w(relative(0.3))
        .when(placeholder, |slot| {
            slot.child(
                if queued {
                    icons::schedule()
                } else {
                    icons::movie()
                }
                .size(rems(20. / 14.))
                .text_color(color(ON_SECONDARY_CONTAINER)),
            )
        })
}

/// The reason a task needs the person, when it differs from its badge.
fn attention_reason(task: &TaskRecord) -> Option<String> {
    let reason = task_attention_summary(task);
    (reason != task_status_label(task)).then_some(reason)
}

fn task_text_source(task: &TaskRecord) -> &'static str {
    if task.plan.options.source_mode != TranscriptSource::Asr
        && task.plan.source.selected_subtitle.is_some()
    {
        course2md::tr!("视频字幕")
    } else if task.plan.options.uses_cloud_provider() {
        course2md::tr!("语音识别服务")
    } else {
        course2md::tr!("本机识别")
    }
}

fn task_badge_kind(state: TaskState) -> BadgeKind {
    match state {
        TaskState::Complete => BadgeKind::Success,
        TaskState::Running | TaskState::Pausing => BadgeKind::Progress,
        TaskState::NeedsAttention => BadgeKind::Danger,
        TaskState::Uncertain | TaskState::Partial => BadgeKind::Warning,
        _ => BadgeKind::Neutral,
    }
}

fn task_status_label(task: &TaskRecord) -> String {
    if task.handled_by.is_some() {
        return course2md::tr!("已有后续处理任务").into();
    }
    if task.has_uncertain() {
        let purpose: std::collections::BTreeSet<_> = task
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
) -> Vec<(String, String, course2md::artifact::Outcome)> {
    let Some(outcomes) = &task.outcomes else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for (component, label, outcome) in [
        (
            Component::Screenshots,
            course2md::tr!("截图"),
            &outcomes.screenshots,
        ),
        (
            Component::Proofreading,
            course2md::tr!("校对"),
            &outcomes.proofreading,
        ),
        (
            Component::Summary,
            course2md::tr!("摘要"),
            &outcomes.summary,
        ),
    ] {
        if matches!(
            outcome.status,
            course2md::artifact::Status::Failed | course2md::artifact::Status::Partial
        ) {
            results.push((component.as_str().into(), label.into(), outcome.clone()));
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
                course2md::tr!("导出").into(),
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
            let row = |task: usize| QueueItem::Row {
                task,
                group: TaskGroup::Queued,
                number: Some(task + 1),
                runs: Some(task + 1),
                first: task == 0,
                last: false,
            };
            let items = [
                QueueItem::GroupLabel {
                    group: TaskGroup::Queued,
                    count: 1,
                },
                QueueItem::Running(0),
                row(0),
                QueueItem::HistoryToggle { count: 1, open },
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
                options: Default::default(),
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
            stages: Default::default(),
            error: None,
            artifact: None,
            outcomes: None,
            unread: false,
            logs: Vec::new(),
            blocked: Vec::new(),
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
            assert!(detail.contains("0s"));
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
                options: Default::default(),
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
            stages: Default::default(),
            error: None,
            artifact: Some(path.clone()),
            outcomes: Some(outcomes.clone()),
            unread: false,
            logs: Vec::new(),
            blocked: Vec::new(),
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
            task.outcomes = Some(outcomes.clone());
            assert_eq!(
                super::task_requires_service_repair(&task),
                requires_repair,
                "{message}"
            );
        }
        outcomes.proofreading.message = Some("服务拒绝凭据".into());
        outcomes.proofreading.kind = Some(course2md::artifact::FailureKind::Authentication);
        task.outcomes = Some(outcomes.clone());
        task.blocked.push(crate::workspace::BlockedRequest {
            reason: course2md::progress::BlockReason::Uncertain,
            request_id: Some("pending".into()),
            purpose: Some("summary".into()),
            description: "摘要".into(),
            kind: Some(course2md::artifact::FailureKind::Uncertain),
            message: "结果尚未确认".into(),
        });
        assert!(!super::task_requires_service_repair(&task));
        task.blocked.clear();
        task.handled_by = Some("recovery".into());
        assert!(!super::task_requires_service_repair(&task));
        task.handled_by = None;

        outcomes.proofreading = Outcome::succeeded();
        outcomes.exports.insert("html".into(), Outcome::succeeded());
        task.outcomes = Some(outcomes);
        assert!(super::task_component_failures(&task).is_empty());
        task.error = Some("服务拒绝凭据".into());
        assert!(!super::task_requires_service_repair(&task));
        task.outcomes = None;
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
