//! The current input form and task execution. Navigation never supplies retry inputs.
use super::*;

/// A stable validation identity for the missing-title case. Rendering compares
/// the typed error instead of the displayed sentence, which changes with the
/// interface language.
#[derive(Debug)]
pub(crate) struct TitleRequired;

impl std::fmt::Display for TitleRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(course2md::tr!("请填写笔记名称"))
    }
}

impl std::error::Error for TitleRequired {}
use crate::import_ui::cover_slot;
use crate::{
    notes::default_output,
    preferences::ServiceRefs,
    theme::*,
    workspace::{Intent, TaskPlan, TaskRecord, TaskState},
};
use anyhow::{Context as _, Result, ensure};
use course2md::config::TranscriptSource;
use course2md::execution::Component;
use course2md::progress::{BlockReason, Stage, StageStatus as EngineStageStatus};
use gpui_component::{
    button::*,
    menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
};
use std::collections::HashMap;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlanValidation {
    Preview,
    Submission,
}

/// Page order: the work in progress and what follows it, then what waits on
/// the person, then the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TaskGroup {
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

fn task_group(task: &TaskRecord) -> TaskGroup {
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

pub(super) fn actionable_task_count(tasks: &[TaskRecord]) -> usize {
    tasks
        .iter()
        .filter(|task| {
            matches!(
                task_group(task),
                TaskGroup::Running | TaskGroup::Attention | TaskGroup::Queued
            )
        })
        .count()
}

/// Published task state and its visible card change together. A full library
/// scan can include slow locations, so its old manifest cannot be the only
/// source of the card while the newly published version is already readable.
fn update_published_course(
    courses: &mut Vec<Course>,
    done: &Done,
    manifest: &course2md::artifact::Manifest,
    thumbnail: Option<PathBuf>,
) {
    let mut course = Course {
        dir: done.out_dir.clone(),
        title: manifest.title.clone(),
        modified: std::time::UNIX_EPOCH + std::time::Duration::from_millis(manifest.created_at_ms),
        slides: manifest.frames.len(),
        thumbnail,
        manifest: Some(manifest.clone()),
        warning: None,
    };
    let storage = course.storage_dir();
    if let Some(previous) = courses
        .iter()
        .find(|previous| previous.storage_dir() == storage)
    {
        // A user-facing library alias belongs to the course across versions.
        if previous
            .manifest
            .as_ref()
            .is_some_and(|manifest| previous.title != manifest.title)
        {
            course.title = previous.title.clone();
        }
    }
    courses.retain(|previous| previous.storage_dir() != storage);
    courses.push(course);
    courses.sort_by_key(|course| std::cmp::Reverse(course.modified));
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

/// 组件 key → 补做按钮文案的唯一来源（任务卡/笔记库/阅读页共用，防止多处漂移）。
pub(crate) fn retry_component_label(component: &str) -> &'static str {
    match Component::parse(component) {
        Some(Component::Screenshots) => course2md::tr!("重新生成截图"),
        Some(Component::Proofreading) => course2md::tr!("重试 AI 校对"),
        Some(Component::Summary) => course2md::tr!("重新生成摘要"),
        Some(Component::Exports) => course2md::tr!("重新导出"),
        None => course2md::tr!("补全未完成部分"),
    }
}

/// 补做按钮文案：单组件按组件命名，多组件统一「补全未完成部分」。
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

fn validate_plan_storage(
    validation: PlanValidation,
    library: &workspace::LibraryLocation,
    folder: Option<u64>,
    cached: Option<&crate::storage_ui::LocationCheck>,
    index: Option<&organize::Library>,
) -> Result<()> {
    let folder_exists = match validation {
        PlanValidation::Preview => {
            let check = cached.context(course2md::tr!("正在检查保存位置，请稍候"))?;
            ensure!(
                check.available,
                course2md::tr!("保存位置不可访问，请连接磁盘或选择其他位置")
            );
            ensure!(
                !check.needs_reassociation,
                course2md::tr!("保存位置需要重新关联")
            );
            if let Some(problem) = &check.problem {
                anyhow::bail!("{problem}");
            }
            match folder {
                Some(folder) => index
                    .context(course2md::tr!("正在读取文件夹信息，请稍候"))?
                    .folders
                    .contains_key(&folder),
                None => true,
            }
        }
        PlanValidation::Submission => {
            ensure!(
                library.root.is_dir(),
                course2md::tr!("保存位置不可访问，请连接磁盘或选择其他位置")
            );
            workspace::check_library(library)?;
            match folder {
                Some(folder) => organize::Library::load(&library.root)?
                    .folders
                    .contains_key(&folder),
                None => true,
            }
        }
    };
    ensure!(
        folder_exists,
        course2md::tr!("文件夹已删除。请选择其他文件夹，或保存到未分类。")
    );
    Ok(())
}

fn update_draft_source_title(
    draft: &mut workspace::Draft,
    input: String,
    title: String,
    source: Option<source::Source>,
) {
    // A source edit can clear Draft::title while the input still contains the
    // previous automatic title. Capture its provenance before changing sources;
    // repeated autosaves while the new metadata is pending must retain it too.
    let unchanged_automatic_title = !draft.custom_title && draft.title == title;
    draft.change_source(input);
    if let Some(source) = source {
        draft.source = Some(source);
    }
    if unchanged_automatic_title {
        draft.title = title;
    } else if draft.title != title {
        draft.custom_title = !title.is_empty()
            && draft
                .source
                .as_ref()
                .is_none_or(|source| source.title != title);
        draft.title = title;
    }
}

/// Apply the visible form without treating a save attempt as a user edit.
/// Navigation often calls this with identical values; keep timestamps and disk
/// records unchanged unless an actual input or option changed.
fn update_input_form(
    draft: &mut workspace::Draft,
    input: String,
    title: String,
    source: Option<source::Source>,
    options: ConversionOptions,
    folder: Option<u64>,
    scroll: f32,
) -> bool {
    let previous = draft.clone();
    update_draft_source_title(draft, input, title, source);
    use workspace::Override;
    for (changed, field) in [
        (
            draft.options.provider != options.provider,
            Override::Provider,
        ),
        (
            draft.options.source_mode != options.source_mode,
            Override::TextSource,
        ),
        (draft.options.llm != options.llm, Override::Proofread),
        (
            draft.options.summarize != options.summarize,
            Override::Summary,
        ),
        (draft.options.vision != options.vision, Override::Vision),
        (
            draft.options.translate != options.translate,
            Override::Translate,
        ),
        (
            draft.options.keep_video != options.keep_video,
            Override::KeepVideo,
        ),
        (draft.options.formats != options.formats, Override::Formats),
    ] {
        if changed {
            draft.overrides.insert(field);
        }
    }
    if !options.uses_cloud_provider() {
        draft.local_provider = options.provider;
    } else if !draft.options.uses_cloud_provider() {
        draft.local_provider = draft.options.provider;
    }
    draft.options = options;
    draft.folder = folder;
    draft.scroll = scroll;
    draft.updated = previous.updated;
    if *draft == previous {
        return false;
    }
    draft.updated = workspace::now();
    true
}

impl Desktop {
    pub fn retry_workspace_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = &self.workspace {
            match workspace.save() {
                Ok(()) => self.workspace_error = None,
                Err(error) => {
                    self.workspace_error =
                        Some(course2md::tr!("记录仍未保存：{}", format!("{error:#}")))
                }
            }
        } else {
            let root = self
                .config
                .defaults
                .out
                .clone()
                .unwrap_or_else(default_output);
            match workspace::Workspace::open(
                root,
                ConversionOptions::from_config(&self.preferences.defaults_config()),
            ) {
                Ok(workspace) => {
                    self.message = workspace.recovery.clone();
                    self.workspace = Some(workspace);
                    self.workspace_error = None;
                    self.restore_draft(window, cx);
                    self.refresh_library(cx);
                }
                Err(error) => {
                    self.workspace_error = Some(course2md::tr!(
                        "记录仍无法读取，原件已保留：{}",
                        format!("{error:#}")
                    ))
                }
            }
        }
        if self.preference_defaults_pending {
            self.refresh_preference_defaults(cx);
        }
        if self.workspace_error.is_none() {
            self.advance_conversion_when_ready(cx);
        }
        cx.notify();
    }

    /// Called only by the explicit global recovery action; rebuilding never starts the queue.
    pub fn rebuild_workspace_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() || self.storage_ui.busy {
            self.workspace_error = Some(
                course2md::tr!("当前还有处理或文件移动在进行，请等候停止后再重建记录。").into(),
            );
            cx.notify();
            return;
        }
        let root = self
            .config
            .defaults
            .out
            .clone()
            .unwrap_or_else(default_output);
        let options = ConversionOptions::from_config(&self.preferences.defaults_config());
        let result = if let Some(workspace) = &self.workspace {
            workspace::Workspace::rebuild_at(workspace.storage_path().to_owned(), root, options)
        } else {
            workspace::Workspace::rebuild(root, options)
        };
        match result {
            Ok(workspace) => {
                self.message = workspace.recovery.clone();
                self.workspace = Some(workspace);
                self.workspace_error = None;
                self.restore_draft(window, cx);
                self.refresh_library(cx);
            }
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "记录尚未重建，原件仍保留：{}",
                    format!("{error:#}")
                ))
            }
        }
        cx.notify();
    }

    pub fn recommended_local_provider(&self) -> course2md::config::AsrProvider {
        use course2md::config::AsrProvider;
        match &self.environment {
            Some(environment) if environment.supports(AsrProvider::Coreml) => AsrProvider::Coreml,
            Some(environment) if environment.supports(AsrProvider::Gpu) => AsrProvider::Gpu,
            Some(environment) if environment.supports(AsrProvider::Npu) && !environment.llama => {
                AsrProvider::Npu
            }
            Some(_) => AsrProvider::Cpu,
            // Detection runs in the background. A temporary display hint must
            // not scan Linux devices or PATH while the interface is rendering.
            None if course2md::config::apple_native_available() => AsrProvider::Coreml,
            None => AsrProvider::Cpu,
        }
    }

    /// 该任务是否正持有运行中的后台作业（记录状态可能尚未追上事件流）。
    pub(crate) fn is_running_task(&self, id: &str) -> bool {
        self.active_task.as_deref() == Some(id) && self.job.is_some()
    }

    /// 任务计划对应的笔记库保存位置；记录可能引用了已移除的库。
    pub(crate) fn task_library(&self, task: &TaskRecord) -> Option<&workspace::LibraryLocation> {
        self.workspace
            .as_ref()
            .and_then(|w| w.state.library(&task.plan.library_id))
    }

    pub fn save_current_draft(&mut self, cx: &mut Context<Self>) -> bool {
        if self.draft_loading {
            return true;
        }
        let input = self.value(Field::Source, cx);
        // A source edit must establish the next form's defaults before autosave
        // can detach the submitted task. Failed preparation cannot fall through
        // to saving the previous video's title and overrides against a new URL.
        if self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.draft())
            .is_some_and(|draft| draft.submitted_task.is_some() && draft.input != input)
        {
            return false;
        }
        let title = self.value(Field::Title, cx);
        let source = self.source_preview.clone();
        let options = self.task_options.clone();
        let folder = self.target_folder;
        let scroll = f32::from(self.scrolls[Page::New as usize].offset().y);
        let Some(workspace) = &mut self.workspace else {
            return false;
        };
        let Some(mut next) = workspace.state.draft().cloned() else {
            self.workspace_error = Some(course2md::tr!("当前输入记录暂时不可用").into());
            return false;
        };
        self.draft_deadline = None;
        if !update_input_form(&mut next, input, title, source, options, folder, scroll) {
            return true;
        }
        let result = workspace.transaction(|state| {
            *state
                .draft_mut()
                .context(course2md::tr!("没有可保存的输入"))? = next;
            Ok(())
        });
        if let Err(error) = result {
            self.workspace_error =
                Some(course2md::tr!("当前输入尚未保存：{}", format!("{error:#}")));
            cx.notify();
            return false;
        }
        self.workspace_error = None;
        true
    }

    pub fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.draft())
            .cloned()
        else {
            return;
        };
        self.draft_loading = true;
        self.online = draft.online;
        self.last_source_input = draft.input.clone();
        self.source_preview = draft.source.clone();
        self.task_options = draft.options.clone();
        self.target_folder = draft.folder;
        self.preview_error = None;
        self.source_validation = None;
        self.title_required = false;
        self.source_candidates.clear();
        self.source_collection_title = None;
        self.source_collection_name = None;
        self.selected_candidates.clear();
        self.subtitle_error = draft.source.as_ref().and_then(|source| {
            source
                .subtitle_read_error
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| {
                    source.subtitle_request.as_ref().map(|_| {
                        course2md::tr!("所选字幕还未读取完成，请继续读取或选择其他文字来源。")
                            .into()
                    })
                })
        });
        self.subtitle_loading = false;
        self.inputs[&Field::Source].update(cx, |s, cx| s.set_value(draft.input, window, cx));
        self.inputs[&Field::Title].update(cx, |s, cx| s.set_value(draft.title, window, cx));
        self.scrolls[Page::New as usize].set_offset(point(px(0.), px(draft.scroll)));
        self.draft_loading = false;
        self.draft_deadline = None;
        cx.notify();
    }

    pub fn switch_source_kind(
        &mut self,
        online: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.online == online {
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
        if let Some(workspace) = &mut self.workspace {
            match workspace.transaction(|state| {
                state.switch_source_kind(online, defaults);
                Ok(())
            }) {
                Ok(()) => {
                    self.following_conversion = None;
                    self.invalidate_source();
                    self.completed_source = None;
                    self.message = None;
                    self.restore_draft(window, cx);
                }
                Err(error) => {
                    self.workspace_error =
                        Some(course2md::tr!("无法切换来源：{}", format!("{error:#}")))
                }
            }
        }
    }

    /// 计划构造函数共享的输入序言：工作区与草稿快照的同一组前置条件。
    fn plan_draft(&self) -> Result<(&workspace::Workspace, &workspace::Draft)> {
        let workspace = self
            .workspace
            .as_ref()
            .context(course2md::tr!("输入与任务记录尚未恢复"))?;
        let draft = workspace
            .state
            .draft()
            .context(course2md::tr!("当前输入尚未准备好"))?;
        Ok((workspace, draft))
    }

    /// 计划构造共享的组件检查：引擎与 FFmpeg 必备；`online` 来源另需 yt-dlp
    /// （批量计划恒为在线来源，单条计划按已确认来源的 online 标记）。
    fn ensure_plan_environment(&self, online: bool) -> Result<&crate::backend::Environment> {
        let environment = self
            .environment
            .as_ref()
            .context(course2md::tr!("正在检查所需组件…"))?;
        ensure!(environment.engine, course2md::tr!("生成引擎无法启动"));
        ensure!(
            environment.ffmpeg && environment.ffprobe,
            course2md::tr!("缺少视频处理组件，请在设置中查看")
        );
        ensure!(
            !online || environment.ytdlp,
            course2md::tr!("缺少在线视频下载组件，请在设置中查看")
        );
        Ok(environment)
    }

    /// 保存位置仍在笔记库中且当前可写；两个计划构造函数同一组校验与错误文案。
    fn ensure_plan_storage(
        &self,
        workspace: &workspace::Workspace,
        draft: &workspace::Draft,
        validation: PlanValidation,
    ) -> Result<()> {
        let library = workspace
            .state
            .library(&draft.library_id)
            .context(course2md::tr!("保存位置已不在笔记库中，请选择保存位置"))?;
        validate_plan_storage(
            validation,
            library,
            draft.folder,
            self.cached_location_check(library),
            self.library_indexes.get(&library.root),
        )?;
        Ok(())
    }

    /// 草稿选项覆写基础配置并规整模型目录；两个计划构造函数同一构建序列。
    fn plan_config(&self, draft: &workspace::Draft) -> course2md::settings::ConfigFile {
        let mut config = draft
            .base_config
            .clone()
            .unwrap_or_else(|| self.preferences.defaults_config());
        draft.options.apply_to(&mut config);
        config.defaults.model_dir = Some(course2md::config::model_dir_from(
            config.defaults.model_dir.as_deref(),
        ));
        config
    }

    /// 草稿上的服务引用，缺省回落到默认服务。
    fn plan_refs(&self, draft: &workspace::Draft) -> ServiceRefs {
        let defaults = self.preferences.default_refs();
        ServiceRefs {
            asr: draft.asr_service.clone().or(defaults.asr),
            llm: draft.ai_service.clone().or(defaults.llm),
        }
    }

    fn build_plan(&self, validation: PlanValidation) -> Result<TaskPlan> {
        self.ordinary_preferences_ready_for_submit()?;
        ensure!(
            !self.preference_defaults_pending,
            course2md::tr!("默认设置已保存，当前视频的选项尚未同步。请重试。")
        );
        let (workspace, draft) = self.plan_draft()?;
        ensure!(
            !draft.input.is_empty(),
            if draft.online {
                course2md::tr!("先粘贴视频链接")
            } else {
                course2md::tr!("先选择一个视频")
            }
        );
        ensure!(
            !self.subtitle_loading && self.preview_cancel.is_none(),
            course2md::tr!("正在确认来源，请等待读取完成")
        );
        ensure!(
            self.source_preview.is_some(),
            course2md::tr!("还没读取到视频信息，请重新预览")
        );
        ensure!(
            self.subtitle_error.is_none(),
            course2md::tr!("字幕还没读取完成，请重新读取，或改用语音识别")
        );
        let source = draft
            .source
            .clone()
            .context(course2md::tr!("请先读取并确认视频"))?;
        let environment = self.ensure_plan_environment(source.online)?;
        ensure!(
            draft.options.source_mode == TranscriptSource::Asr
                || (source.subtitle_request.is_none() && source.subtitle_read_error.is_none()),
            course2md::tr!("所选字幕还未确认，请继续读取或明确选择其他文字来源。")
        );
        ensure!(
            !source.identity.is_empty(),
            course2md::tr!("无法识别这个视频，请重新预览")
        );
        if draft.title.trim().is_empty() {
            return Err(TitleRequired.into());
        }
        self.ensure_plan_storage(workspace, draft, validation)?;
        let mut config = self.plan_config(draft);
        let using_subtitle = draft.options.source_mode != TranscriptSource::Asr
            && (source.selected_subtitle.is_some() || draft.subtitle.is_some());
        if using_subtitle {
            if let Some(subtitle) = &source.selected_subtitle {
                ensure!(
                    subtitle.source_identity == source.identity,
                    course2md::tr!("所选字幕与当前视频不匹配，请重新读取字幕")
                );
            }
            config.defaults.transcript_source = Some(course2md::config::TranscriptSource::Subtitle);
        } else {
            use course2md::subtitle::SubtitleEvidence;
            ensure!(
                draft.options.source_mode == TranscriptSource::Asr
                    || (draft.options.source_mode == TranscriptSource::Auto
                        && matches!(
                            source.subtitles,
                            SubtitleEvidence::NoneFound
                                | SubtitleEvidence::Unsupported { .. }
                                | SubtitleEvidence::Failed { .. }
                        )),
                course2md::tr!("字幕还未确认。请读取字幕，或明确选择语音识别。")
            );
            config.defaults.transcript_source = Some(course2md::config::TranscriptSource::Asr);
            let provider = config
                .defaults
                .provider
                .unwrap_or_else(|| self.recommended_local_provider());
            ensure_asr_capability(environment, provider)?;
            config.defaults.provider = Some(provider);
            fill_default_asr_model(&mut config, provider);
        }
        let refs = self.plan_refs(draft);
        let config = match validation {
            PlanValidation::Preview => self.preferences.config_for_preview(&config, &refs)?,
            PlanValidation::Submission => self.preferences.config_for_refs(&config, &refs)?,
        };
        let mut validation_config = config.clone();
        validation_config
            .defaults
            .provider
            .get_or_insert_with(|| self.recommended_local_provider());
        validate_plan_config(&source.input, &validation_config)?;
        if validation == PlanValidation::Submission && !source.online {
            ensure!(
                std::path::Path::new(&source.input).is_file(),
                course2md::tr!("原视频已移动或无法读取，请重新选择视频")
            );
        }
        Ok(TaskPlan {
            operation: Default::default(),
            source_id: source.identity.clone(),
            source,
            title: draft.title.clone(),
            library_id: draft.library_id.clone(),
            folder: draft.folder,
            options: draft.options.clone(),
            subtitle: draft.subtitle.clone(),
            config,
            asr_service: refs.asr,
            ai_service: refs.llm,
        })
    }

    /// 候选多选的批量计划。环境、保存位置与配置解析同 build_plan；字幕确认留给
    /// 任务运行时（pipeline 按 transcript_source 自行探测发现），因此不要求桌面
    /// 端预先读取每个视频。标题用候选行里用户看到的分集标题。
    fn build_batch_plan(
        &self,
        candidate: &source::SourceCandidate,
        title: String,
    ) -> Result<TaskPlan> {
        self.ordinary_preferences_ready_for_submit()?;
        ensure!(
            !self.preference_defaults_pending,
            course2md::tr!("默认设置已保存，当前选项尚未同步。请重试。")
        );
        let (workspace, draft) = self.plan_draft()?;
        let identity = course2md::fetch::candidate_identity(candidate)
            .context(course2md::tr!("还无法确认这个视频的身份，请单独打开后转换"))?;
        let environment = self.ensure_plan_environment(true)?;
        self.ensure_plan_storage(workspace, draft, PlanValidation::Submission)?;
        let mut config = self.plan_config(draft);
        // 强制字幕（source_mode 1）不会走到语音识别；其余模式运行时可能退回 ASR，
        // 与 build_plan 一样先确认本机识别能力并补默认模型。
        if draft.options.source_mode != TranscriptSource::Subtitle {
            let provider = config
                .defaults
                .provider
                .unwrap_or_else(|| self.recommended_local_provider());
            let capability = ensure_asr_capability(environment, provider);
            if draft.options.source_mode == TranscriptSource::Auto {
                capability.context(course2md::tr!(
                    "部分视频可能没有字幕，需要语音识别能力作为后备"
                ))?;
            } else {
                capability?;
            }
            config.defaults.provider = Some(provider);
            fill_default_asr_model(&mut config, provider);
        }
        let refs = self.plan_refs(draft);
        let config = self.preferences.config_for_refs(&config, &refs)?;
        let mut validation_config = config.clone();
        validation_config
            .defaults
            .provider
            .get_or_insert_with(|| self.recommended_local_provider());
        validate_plan_config(&candidate.input, &validation_config)?;
        Ok(TaskPlan {
            operation: Default::default(),
            source_id: identity.clone(),
            source: source::Source {
                input: candidate.input.clone(),
                title: title.clone(),
                duration: candidate
                    .duration
                    .filter(|value| value.is_finite() && *value > 0.)
                    .unwrap_or_default(),
                cover: self.source_candidate_covers.get(&candidate.input).cloned(),
                identity,
                online: true,
                ..source::Source::default()
            },
            title,
            library_id: draft.library_id.clone(),
            folder: draft.folder,
            options: draft.options.clone(),
            subtitle: None,
            config,
            asr_service: refs.asr,
            ai_service: refs.llm,
        })
    }

    /// 把多选候选一次性加入队列：逐个复用既有去重（进行中的相同任务不重复创建），
    /// 一次事务落盘，随后选中第一个任务并启动队列。
    pub fn enqueue_selected_candidates(&mut self, cx: &mut Context<Self>) {
        let selected: Vec<(usize, source::SourceCandidate)> = self
            .source_candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| self.selected_candidates.contains(&candidate.input))
            .map(|(index, candidate)| (index, candidate.clone()))
            .collect();
        if selected.is_empty() {
            return;
        }
        self.message = None;
        let collection = self.source_collection_name.clone();
        let mut plans = Vec::new();
        let mut skipped = 0usize;
        for (index, candidate) in selected {
            if course2md::fetch::candidate_identity(&candidate).is_none() {
                skipped += 1;
                continue;
            }
            let title = batch_note_title(&candidate, collection.as_deref(), index);
            match self.build_batch_plan(&candidate, title) {
                Ok(plan) => plans.push(plan),
                Err(error) => {
                    self.message = Some(course2md::tr!(
                        "选中的视频尚未加入队列：{}",
                        format!("{error:#}")
                    ));
                    cx.notify();
                    return;
                }
            }
        }
        if plans.is_empty() {
            self.message = Some(if skipped > 0 {
                course2md::tr!("选中的视频无法识别，请单独打开后转换").into()
            } else {
                course2md::tr!("选中的视频尚未加入队列，请重试").into()
            });
            cx.notify();
            return;
        }
        let total = plans.len();
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let result = workspace.transaction(|state| {
            let mut first_id = None;
            let mut duplicates = 0usize;
            for plan in plans {
                let (id, created) = state.enqueue(plan, None)?;
                first_id.get_or_insert_with(|| id.clone());
                if !created {
                    duplicates += 1;
                }
            }
            Ok((first_id, duplicates))
        });
        match result {
            Ok((first_id, duplicates)) => {
                self.selected_candidates.clear();
                let created = total - duplicates;
                let mut parts = Vec::new();
                if created > 0 {
                    parts.push(course2md::tr!("已把 {} 个视频加入队列", created));
                }
                if duplicates > 0 {
                    parts.push(course2md::tr!(
                        "{} 个与进行中的任务相同，未重复加入",
                        duplicates
                    ));
                }
                if skipped > 0 {
                    parts.push(course2md::tr!("{} 个无法识别，已跳过", skipped));
                }
                self.message = Some(parts.join(course2md::tr!("；")));
                if let Some(id) = first_id {
                    self.select_task(&id, cx);
                }
                self.start_next_task(cx);
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("任务尚未加入队列：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

    /// The plan the form would submit, checked against loaded snapshots.
    /// Submission rechecks live storage, local source and dispatch markers
    /// before a task can enter the queue.
    pub fn preview_plan(&self) -> Result<TaskPlan> {
        self.build_plan(PlanValidation::Preview)
    }

    /// An unfinished task already doing the same work as `plan`.
    pub fn matching_task(&self, plan: &TaskPlan) -> Option<&TaskRecord> {
        self.workspace.as_ref()?.state.tasks.iter().find(|task| {
            !task.state.finished() && task.handled_by.is_none() && task.plan.same_work(plan)
        })
    }

    pub fn enqueue_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        self.message = None;
        let plan = match self.build_plan(PlanValidation::Submission) {
            Ok(plan) => plan,
            Err(error) => {
                self.title_required = error.downcast_ref::<TitleRequired>().is_some();
                self.source_validation = Some(format!("{error:#}"));
                self.validation_attempt = self.validation_attempt.wrapping_add(1);
                let subtitle_attention = self.subtitle_attention_required();
                if self.source_preview.is_some() && !subtitle_attention {
                    // Name, destination and processing controls must exist in
                    // the next frame before a validation error focuses them.
                    self.generation_options_open = true;
                }
                let field = if self.source_preview.is_some()
                    && !subtitle_attention
                    && self.value(Field::Title, cx).trim().is_empty()
                {
                    Some(Field::Title)
                } else if self.source_preview.is_none() && self.online {
                    Some(Field::Source)
                } else {
                    None
                };
                if let Some(field) = field {
                    if field == Field::Title {
                        self.pending_conversion = None;
                        self.following_conversion = None;
                    }
                    let input = self.inputs[&field].clone();
                    window.on_next_frame(move |window, cx| {
                        input.update(cx, |input, cx| input.focus(window, cx));
                    });
                }
                cx.notify();
                return;
            }
        };
        // Starting again acknowledges only the terminal feedback belonging to
        // the current foreground input, before the new task replaces its link.
        let acknowledged_input = (self.page == Page::New)
            .then(|| self.current_input_task(cx))
            .flatten()
            .filter(|task| task.state.finished())
            .map(|task| task.id.clone());
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let parent = workspace
            .state
            .draft()
            .and_then(|draft| draft.retry_of.clone());
        let result = workspace.transaction(|state| {
            let (id, created) = state.enqueue(plan, parent)?;
            if let Some(previous) = acknowledged_input
                .as_deref()
                .and_then(|id| state.task_mut(id))
            {
                previous.unread = false;
            }
            if created && let Some(draft) = state.draft_mut() {
                draft.submitted_task = Some(id.clone());
            }
            Ok((id, created))
        });
        match result {
            Ok((id, created)) => {
                self.pending_conversion = None;
                self.source_validation = None;
                self.title_required = false;
                self.message =
                    (!created).then(|| course2md::tr!("已有相同任务，未重复加入").into());
                if created {
                    self.following_conversion = self
                        .following_conversion
                        .take()
                        .filter(|_| self.page == Page::New)
                        .and_then(|follow| follow.submitted(self.preview_generation, id.clone()));
                    self.generation_options_open = false;
                    self.source_editor_open = false;
                    self.scrolls[Page::New as usize].set_offset(point(px(0.), px(0.)));
                }
                self.select_task(&id, cx);
                // Preparation may finish after navigation; keep the user's current page.
                self.start_next_task(cx);
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("任务尚未加入队列：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

    fn request_for(
        &self,
        task: &TaskRecord,
        secrets: &HashMap<String, String>,
    ) -> Result<course2md::execution::Request> {
        let location = self
            .task_library(task)
            .context(course2md::tr!("任务保存位置暂时不可用"))?;
        workspace::check_library(location)?;
        ensure!(
            task.work_dir == location.root.join(".course2md/work").join(&task.id),
            course2md::tr!("任务位置与笔记库记录不匹配，请先恢复存储位置")
        );
        let refs = ServiceRefs {
            asr: task.plan.asr_service.clone(),
            llm: task.plan.ai_service.clone(),
        };
        let config = self
            .preferences
            .resolve_for_execution_with_secrets(&task.plan.config, &refs, secrets)?
            .into_config();
        let course_id = workspace::course_dir_name(&task.plan.source_id);
        let service_versions = [("asr", refs.asr), ("llm", refs.llm)]
            .into_iter()
            .filter_map(|(purpose, reference)| reference.map(|r| (purpose.to_owned(), r)))
            .collect();
        let source = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.reader_sources.get(&task.plan.source_id))
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
            .filter(|_| {
                !task.plan.source.online && !std::path::Path::new(&task.plan.source.input).is_file()
            })
            .unwrap_or_else(|| task.plan.source.input.clone());
        Ok(course2md::execution::Request {
            schema: 1,
            operation: task.plan.operation.clone(),
            task_id: task.id.clone(),
            version_id: task.id.clone(),
            course_dir: location.root.join(&course_id),
            course_id,
            source,
            source_id: task.plan.source_id.clone(),
            title: task.plan.title.clone(),
            author: task.plan.source.author.clone(),
            duration: task.plan.source.duration,
            subtitle: if task.plan.options.source_mode != TranscriptSource::Asr {
                task.plan.subtitle.clone()
            } else {
                None
            },
            subtitle_events: None,
            config,
            work_dir: task.work_dir.clone(),
            allow_unauthenticated_asr: task
                .plan
                .asr_service
                .as_deref()
                .and_then(|id| self.preferences.version(id))
                .is_some_and(|version| {
                    version.config.authentication == preferences::Authentication::None
                }),
            control_path: Some(task.work_dir.join("control.json")),
            service_versions,
        })
    }

    fn write_task_control(&self, task: &TaskRecord, resend: Vec<String>) -> Result<()> {
        let library = self
            .task_library(task)
            .context(course2md::tr!("任务保存位置尚未恢复"))?;
        workspace::check_library(library)?;
        ensure!(
            task.work_dir == library.root.join(".course2md/work").join(&task.id),
            course2md::tr!("任务工作目录与保存位置不匹配")
        );
        let mut authorizations = task.resend.clone();
        for id in resend {
            if !authorizations.contains(&id) {
                authorizations.push(id);
            }
        }
        let stopped: Vec<_> = [&task.plan.asr_service, &task.plan.ai_service]
            .into_iter()
            .flatten()
            .filter(|id| {
                self.preferences
                    .version(id)
                    .is_none_or(|version| self.preferences.is_service_retired(&version.service_id))
            })
            .cloned()
            .collect();
        std::fs::create_dir_all(&task.work_dir)?;
        course2md::checkpoint::atomic_write(
            &task.work_dir.join("control.json"),
            &serde_json::to_vec(&serde_json::json!({
                "intent": task.intent, "stopped_services": stopped, "resend": authorizations,
            }))?,
        )
    }

    pub fn refresh_dispatch_controls(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self
            .active_task
            .as_deref()
            .and_then(|id| self.workspace.as_ref()?.state.task(id))
            .cloned()
            && let Err(error) = self.write_task_control(&task, Vec::new())
        {
            self.workspace_error = Some(course2md::tr!(
                "停止新请求的指令尚未保存：{}",
                format!("{error:#}")
            ));
            if let Some(job) = &self.job {
                job.cancel();
            }
            cx.notify();
        }
    }

    pub fn start_next_task(&mut self, cx: &mut Context<Self>) {
        if self.job.is_some() || self.closing || self.storage_ui.busy || self.start_pending {
            return;
        }
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.next_task())
            .cloned()
        else {
            return;
        };
        let refs = ServiceRefs {
            asr: task.plan.asr_service.clone(),
            llm: task.plan.ai_service.clone(),
        };
        let needed = match self
            .preferences
            .credential_references_for(&task.plan.config, &refs)
        {
            Ok(needed) => needed,
            Err(error) => {
                self.fail_task_before_start(&task.id, format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        if needed.is_empty() {
            self.finish_task_start(&task.id, HashMap::new(), cx);
            return;
        }
        // Keychain resolution may surface a system authorization dialog whose queue can even
        // be held by another application; resolving on the UI thread would freeze the whole
        // interface, so the secrets are gathered on a background executor first.
        self.start_pending = true;
        let vault = self.preferences.vault();
        let task_id = task.id.clone();
        let secrets_rx = crate::spawn_blocking_io(move || -> Result<HashMap<String, String>> {
            let mut secrets = HashMap::new();
            for reference in needed {
                let secret = vault.resolve(&reference).map(|s| s.expose().to_owned())?;
                secrets.insert(reference, secret);
            }
            Ok(secrets)
        });
        cx.spawn(async move |this, cx| {
            let secrets = secrets_rx
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(course2md::tr!("凭据读取意外中断"))));
            let _ = this.update(cx, |this, cx| {
                this.start_pending = false;
                match secrets {
                    Ok(secrets) => this.finish_task_start(&task_id, secrets, cx),
                    Err(error) => {
                        this.fail_task_before_start(&task_id, format!("{error:#}"));
                        cx.notify();
                    }
                }
                this.start_next_task(cx);
            });
        })
        .detach();
    }

    fn finish_task_start(
        &mut self,
        task_id: &str,
        secrets: HashMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some() || self.closing {
            return;
        }
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.task(task_id))
            .cloned()
        else {
            return;
        };
        // The queue may have changed while credentials were being resolved off-thread.
        if task.state != TaskState::Queued || task.intent != Intent::Run {
            return;
        }
        let request = match self.request_for(&task, &secrets) {
            Ok(request) => request,
            Err(error) => {
                self.fail_task_before_start(&task.id, format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let read_task = task.clone();
        let read = crate::spawn_blocking_io(move || {
            let mut request = request;
            if read_task.plan.options.source_mode != TranscriptSource::Asr
                && read_task.plan.subtitle.is_none()
            {
                request.subtitle_events = workspace::task_subtitle_events(&read_task)?;
            }
            request.validate()?;
            Ok::<_, anyhow::Error>(request)
        });
        let desktop = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let request = match read.recv().await {
                Ok(Ok(request)) => request,
                Ok(Err(error)) => {
                    let _ = desktop.update(cx, |this, cx| {
                        this.fail_task_before_start(&task.id, format!("{error:#}"));
                        cx.notify();
                    });
                    return;
                }
                Err(error) => {
                    let _ = desktop.update(cx, |this, cx| {
                        this.fail_task_before_start(&task.id, error.to_string());
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = desktop.update(cx, |this, cx| {
                this.start_task_request(task, request, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn start_task_request(
        &mut self,
        task: TaskRecord,
        request: course2md::execution::Request,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some()
            || self.closing
            || task.state != TaskState::Queued
            || task.intent != Intent::Run
        {
            return;
        }
        if let Err(error) = self.write_task_control(&task, Vec::new()) {
            self.fail_task_before_start(&task.id, format!("{error:#}"));
            cx.notify();
            return;
        }
        let running = self
            .workspace
            .as_mut()
            .context(course2md::tr!("任务记录不可用"))
            .and_then(|workspace| {
                workspace.transaction(|state| {
                    let record = state
                        .task_mut(&task.id)
                        .context(course2md::tr!("任务不存在"))?;
                    record.state = TaskState::Running;
                    record.error = None;
                    record.updated = workspace::now();
                    Ok(())
                })
            });
        if let Err(error) = running {
            self.fail_task_before_start(&task.id, format!("{error:#}"));
            cx.notify();
            return;
        }
        // Starting the child and writing the full request to stdin can block on a pipe;
        // keep it off the UI thread.
        let wake = self.event_wake_tx.clone();
        let started = crate::spawn_blocking_io(move || Job::start_task(&request, wake));
        let desktop = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let job = started.recv().await;
            let _ = desktop.update(cx, |this, cx| match job {
                Ok(Ok(job)) => {
                    this.job = Some(job);
                    this.active_task = Some(task.id.clone());
                    this.kind = Kind::Convert;
                    this.cancelling = false;
                    this.progress.clear();
                    this.token_totals = (0, 0);
                    this.job_started = Some(std::time::Instant::now());
                    this.logs.clear();
                    this.pending_done = None;
                    this.task_error = None;
                    this.task_status = course2md::tr!("正在生成《{}》", task.plan.title);
                    cx.notify();
                }
                Ok(Err(error)) => {
                    this.fail_task_before_start(&task.id, format!("{error:#}"));
                    cx.notify();
                }
                Err(_) => {
                    this.fail_task_before_start(&task.id, crate::interrupted().to_owned());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn fail_task_before_start(&mut self, task_id: &str, message: String) {
        if let Some(workspace) = &mut self.workspace {
            let compensated = workspace.transaction(|state| {
                let record = state
                    .task_mut(task_id)
                    .context(course2md::tr!("任务不存在"))?;
                record.state = TaskState::NeedsAttention;
                record.error = Some(message.clone());
                record.unread = true;
                Ok(())
            });
            if let Err(error) = compensated {
                // 补偿持久化失败也不能让任务停在 Running 被调度器无视：
                // 内存中同样标记 NeedsAttention；重启时 recover() 与磁盘对齐
                if let Some(record) = workspace.state.task_mut(task_id) {
                    record.state = TaskState::NeedsAttention;
                    record.error = Some(message);
                    record.unread = true;
                }
                self.workspace_error =
                    Some(course2md::tr!("任务状态尚未保存：{}", format!("{error:#}")));
            }
        }
    }

    pub fn set_task_intent(&mut self, id: String, intent: Intent, cx: &mut Context<Self>) {
        let active = self.is_running_task(&id);
        if active && intent == Intent::Run {
            self.message = Some(course2md::tr!("当前任务还在处理，请等候当前操作结束。").into());
            cx.notify();
            return;
        }
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        match workspace.transaction(|state| {
            state.set_intent(&id, intent)?;
            if active && intent != Intent::Run {
                state
                    .task_mut(&id)
                    .context(course2md::tr!("任务记录不存在"))?
                    .state = TaskState::Pausing;
            }
            Ok(())
        }) {
            Ok(()) => {
                if intent == Intent::Run {
                    self.follow_resumed_input_task(&id, cx);
                } else if intent == Intent::Cancel {
                    self.following_conversion = None;
                }
                if let Some(task) = self
                    .workspace
                    .as_ref()
                    .and_then(|w| w.state.task(&id))
                    .cloned()
                    && let Err(error) = self.write_task_control(&task, Vec::new())
                {
                    self.workspace_error =
                        Some(course2md::tr!("任务指令尚未保存：{}", format!("{error:#}")));
                    if self.active_task.as_ref() == Some(&id)
                        && let Some(job) = &self.job
                    {
                        job.cancel();
                    }
                }
                self.start_next_task(cx);
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("任务状态尚未保存：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

    pub fn select_task(&mut self, id: &str, cx: &mut Context<Self>) {
        let previous = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.selected_task.clone());
        if previous.as_deref() != Some(id)
            && let Some(previous) = previous
        {
            self.clear_task_card_motion(&previous);
        }
        if let Some(workspace) = &mut self.workspace
            && let Err(error) = workspace.transaction(|state| {
                state.selected_task = Some(id.to_owned());
                if let Some(task) = state.task_mut(id) {
                    task.unread = false;
                }
                Ok(())
            })
        {
            self.workspace_error =
                Some(course2md::tr!("任务选择尚未保存：{}", format!("{error:#}")));
        }
        // Going to a task opens its details when there is something to read
        // or decide; new and running work stays a compact row.
        let waiting = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(id))
            .is_some_and(|task| matches!(task_group(task), TaskGroup::Running | TaskGroup::Queued));
        self.expanded_task = (!waiting).then(|| id.to_owned());
        cx.notify();
    }

    fn follow_resumed_input_task(&mut self, id: &str, cx: &App) {
        if self.page == Page::New
            && self
                .current_input_task(cx)
                .is_some_and(|task| task.id == id)
        {
            self.following_conversion = Some(crate::import_ui::ConversionFollow::Task {
                id: id.to_owned(),
                source_revision: self.preview_generation,
            });
        } else if self.page == Page::Result && !self.reading {
            self.following_conversion = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.task(id))
                .zip(self.preview.as_ref())
                .and_then(|(task, preview)| {
                    crate::import_ui::ConversionFollow::reader_reprocess(
                        task,
                        &preview.course.dir,
                        self.read_generation,
                    )
                });
        }
    }

    pub fn resend_uncertain(&mut self, id: String, cx: &mut Context<Self>) {
        if self.is_running_task(&id) {
            self.message = Some(
                course2md::tr!("正在保存当前结果，请等候当前任务停止后再选择重新发送。").into(),
            );
            cx.notify();
            return;
        }
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.task(&id))
            .cloned()
        else {
            return;
        };
        if let Some(followup) = &task.handled_by {
            self.select_task(followup, cx);
            return;
        }
        let requests: Vec<_> = task
            .blocked
            .iter()
            .filter(|b| b.is_uncertain())
            .filter_map(|b| b.request_id.clone())
            .collect();
        if requests.is_empty() {
            return;
        }
        if task.artifact.is_some() {
            let components = task
                .blocked
                .iter()
                .filter(|b| b.is_uncertain())
                .filter_map(|b| b.purpose.as_deref())
                .map(|purpose| ai_component(purpose).unwrap_or(Component::Proofreading))
                .collect();
            self.reprocess_task(id, components, requests, cx);
            return;
        }
        if let Some(workspace) = &mut self.workspace {
            let result = workspace.transaction(|state| state.authorize_uncertain(&id, &requests));
            if let Err(error) = result {
                self.workspace_error = Some(course2md::tr!(
                    "重新发送的选择尚未保存：{}",
                    format!("{error:#}")
                ));
                return;
            }
        }
        self.follow_resumed_input_task(&id, cx);
        self.start_next_task(cx);
        cx.notify();
    }

    pub fn reprocess_task(
        &mut self,
        id: String,
        mut components: Vec<Component>,
        resend: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.is_running_task(&id) {
            self.message =
                Some(course2md::tr!("正在保存当前结果，请等候当前任务停止后再补做。").into());
            cx.notify();
            return;
        }
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        components.sort();
        components.dedup();
        let result = workspace.transaction(|state| state.reprocess(&id, components, resend));
        match result {
            Ok(id) => {
                self.select_task(&id, cx);
                self.follow_resumed_input_task(&id, cx);
                self.start_next_task(cx);
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("补做任务尚未建立：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

    pub(super) fn repair_task_service(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
        {
            if let Some(next) = task.handled_by.clone() {
                self.select_task(&next, cx);
                self.navigate(Page::Task, cx);
                return;
            }
            if task.has_uncertain() {
                self.select_task(&id, cx);
                self.navigate(Page::Task, cx);
                return;
            }
        }
        let components = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
            .and_then(|task| task.artifact.as_ref().map(|_| task))
            .map(|task| {
                task_component_failures(task)
                    .into_iter()
                    .filter_map(|(component, _, _)| Component::parse(&component))
                    .filter(|component| ai_component(component.as_str()).is_some())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if components.is_empty() {
            self.message = Some(course2md::tr!("这份笔记没有需要修复的校对或摘要").into());
            cx.notify();
            return;
        }
        self.open_reprocess_service_editor(id, components, window, cx);
    }

    pub(crate) fn reprocess_task_with_service(
        &mut self,
        id: String,
        components: Vec<Component>,
        ai_service: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = (|| -> Result<String> {
            ensure!(
                !self.is_running_task(&id),
                course2md::tr!("正在保存当前结果，请等待任务停止后再补做")
            );
            let task = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.task(&id))
                .context(course2md::tr!("原任务记录暂时不可用"))?;
            let mut base = task.plan.config.clone();
            base.defaults.transcript_source = Some(course2md::config::TranscriptSource::Subtitle);
            base.llm.enabled = components.contains(&Component::Proofreading);
            base.llm.summarize = components.contains(&Component::Summary);
            // Translation follows the proofread paragraphs it is redone with.
            if !base.llm.enabled {
                base.llm.translate = None;
            }
            let config = self.preferences.config_for_refs(
                &base,
                &ServiceRefs {
                    asr: None,
                    llm: Some(ai_service.clone()),
                },
            )?;
            self.workspace
                .as_mut()
                .context(course2md::tr!("任务记录暂时不可用"))?
                .transaction(|state| {
                    state.reprocess_with_service(
                        &id,
                        components,
                        Vec::new(),
                        Some((ai_service, config)),
                    )
                })
        })();
        match result {
            Ok(next) => {
                self.workspace_error = None;
                self.select_task(&next, cx);
                self.follow_resumed_input_task(&next, cx);
                self.start_next_task(cx);
                cx.notify();
                true
            }
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "服务已保存，补做任务尚未建立：{}",
                    format!("{error:#}")
                ));
                cx.notify();
                false
            }
        }
    }

    pub fn adjust_task(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running_task(&id) {
            self.message =
                Some(course2md::tr!("当前任务还在处理，请先暂停并等候当前步骤结束。").into());
            cx.notify();
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
            .cloned()
        else {
            return;
        };
        let events =
            crate::spawn_blocking_io(move || workspace::task_subtitle_events(&task).ok().flatten());
        cx.spawn_in(window, async move |this, cx| {
            let events = events.recv().await.ok().flatten();
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(workspace) = &mut this.workspace else {
                    return;
                };
                match workspace.transaction(|state| state.adjust_task(&id, events)) {
                    Ok(()) => {
                        this.following_conversion = None;
                        this.invalidate_source();
                        this.restore_draft(window, cx);
                        this.page = Page::New;
                    }
                    Err(error) => {
                        this.workspace_error = Some(course2md::tr!(
                            "尚未打开任务调整选项：{}",
                            format!("{error:#}")
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn record_task_event(&mut self, event: &EngineEvent) {
        let Some(id) = &self.active_task else {
            return;
        };
        let Some(task) = self.workspace.as_mut().and_then(|w| w.state.task_mut(id)) else {
            return;
        };
        // High-frequency log/progress events keep the persisted timestamp;
        // only state the user would act on moves it (and rewrites the mirror).
        match event {
            EngineEvent::Log { message, .. } => {
                task.logs.push(message.clone());
                if task.logs.len() > 200 {
                    task.logs.drain(..task.logs.len() - 200);
                }
            }
            EngineEvent::Stage { stage, status } => {
                task.updated = workspace::now();
                if matches!(stage, Stage::ScenesScan | Stage::ScenesExtract) {
                    // Old workers combined scanning and extraction under this
                    // key. Do not retain its completed counter on continuation.
                    task.stages.remove(&Stage::Scenes);
                    if *stage == Stage::ScenesScan && *status == EngineStageStatus::Start {
                        task.stages.remove(&Stage::ScenesExtract);
                    }
                }
                let value = task.stages.entry(stage.clone()).or_default();
                match status {
                    EngineStageStatus::Start => value.begin(),
                    EngineStageStatus::Done => value.status = EngineStageStatus::Done,
                }
            }
            EngineEvent::Progress {
                stage,
                current,
                total,
                message,
                phase,
            } => {
                let value = task.stages.entry(stage.clone()).or_default();
                value.current = *current;
                value.total = *total;
                value.detail = message.clone().or_else(|| {
                    phase.map(|phase| crate::activity::model_transfer_phase(stage, Some(phase)))
                });
            }
            EngineEvent::Error { message } => {
                task.updated = workspace::now();
                task.error = Some(message.clone());
            }
            EngineEvent::Blocked {
                reason,
                request_id,
                purpose,
                description,
                kind,
                message,
            } => {
                task.updated = workspace::now();
                if *reason == BlockReason::Uncertain {
                    task.state = TaskState::Uncertain;
                }
                let blocked = workspace::BlockedRequest {
                    reason: *reason,
                    request_id: request_id.clone(),
                    purpose: purpose.clone(),
                    description: description.clone().unwrap_or_default(),
                    kind: Some(*kind),
                    message: message.clone(),
                };
                if !task.blocked.contains(&blocked) {
                    task.blocked.push(blocked);
                }
                task.error = Some(message.clone());
            }
            _ => {}
        }
    }

    pub fn finish_task(
        &mut self,
        id: &str,
        success: bool,
        cancelled: bool,
        cx: &mut Context<Self>,
    ) {
        let done = self.pending_done.take();
        let manifest = done.as_ref().and_then(|d| {
            course2md::artifact::read_manifest(&d.out_dir.join("manifest.json")).ok()
        });
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let visible =
            self.page == Page::Task && workspace.state.selected_task.as_deref() == Some(id);
        let result = workspace.transaction(|state| {
            let task = state
                .task_mut(id)
                .context(course2md::tr!("任务记录不存在"))?;
            task.updated = workspace::now();
            task.unread = !visible;
            task.outcomes = done
                .as_ref()
                .and_then(|done| done.outcomes.clone())
                .or_else(|| manifest.as_ref().map(|manifest| manifest.outcomes.clone()));
            workspace::reconcile_receipts(task)?;
            if let (Some(done), Some(manifest)) = (&done, &manifest) {
                task.artifact = Some(done.out_dir.clone());
                let partial = done.partial.unwrap_or(manifest.partial);
                task.state = if task.has_uncertain() {
                    TaskState::Uncertain
                } else if partial {
                    TaskState::Partial
                } else {
                    TaskState::Complete
                };
                if !partial && task.state != TaskState::Uncertain {
                    task.error = None;
                }
            } else {
                task.state = match task.intent {
                    Intent::Cancel => TaskState::Cancelled,
                    Intent::Pause | Intent::Quit => TaskState::Paused,
                    Intent::Run if task.has_uncertain() => TaskState::Uncertain,
                    Intent::Run => TaskState::NeedsAttention,
                };
                if task.error.is_none() && !cancelled && task.intent == Intent::Run {
                    task.error = Some(
                        if success {
                            course2md::tr!("未生成可读笔记")
                        } else {
                            course2md::tr!("生成中断")
                        }
                        .into(),
                    );
                }
            }
            let record = task.clone();
            if record.state == TaskState::Complete && state.selected_task.as_deref() == Some(id) {
                state.selected_task = None;
            }
            Ok(record)
        });
        match result {
            Ok(task) => {
                if self.reader_viewer_open() {
                    crate::import_ui::ConversionFollow::interrupt_for_reader_viewer(
                        &mut self.following_conversion,
                    );
                }
                let follow_input_completion = self
                    .following_conversion
                    .as_ref()
                    .zip(self.workspace.as_ref())
                    .and_then(|(follow, workspace)| {
                        follow.completed_task(
                            self.page == Page::New,
                            self.preview_generation,
                            workspace.state.draft()?,
                            &workspace.state.tasks,
                            &self.value(Field::Source, cx),
                        )
                    })
                    .is_some_and(|task| task.id == id);
                let follow_reader_completion = !self.reading
                    && self.following_conversion.as_ref().is_some_and(|follow| {
                        follow.completed_reader_task(
                            &task,
                            self.page,
                            self.read_generation,
                            self.preview
                                .as_ref()
                                .map(|preview| preview.course.dir.as_path()),
                        )
                    });
                if let Some(done) = done {
                    self.completed = Some(done.clone());
                    let root = self.task_library(&task).map(|l| l.root.clone());
                    let open = !task.exports_only()
                        && (follow_input_completion || follow_reader_completion);
                    let source = task.plan.source.clone();
                    let folder = task.plan.folder;
                    let out_dir = done.out_dir.clone();
                    let storage_dir = Course::from_completed(&done).storage_dir();
                    let frames = manifest
                        .as_ref()
                        .map(|manifest| manifest.frames.clone())
                        .unwrap_or_default();
                    // Cover copy, folder assignment and thumbnail lookup touch the
                    // library disk; the record above is already committed.
                    let published = crate::spawn_blocking_io(move || {
                        let mut issues = Vec::new();
                        if let Some(root) = &root {
                            if let Err(error) = source::save_cover(&source, &out_dir) {
                                issues
                                    .push(course2md::tr!("封面暂未保存：{}", format!("{error:#}")));
                            }
                            if let Err(error) = organize::Library::edit(root, |library| {
                                library.assign(root, &storage_dir, folder)
                            }) {
                                issues.push(course2md::tr!(
                                    "文件夹归属暂未更新：{}",
                                    format!("{error:#}")
                                ));
                            }
                        }
                        let cover = out_dir.join("cover.jpg");
                        let thumbnail = cover.is_file().then_some(cover).or_else(|| {
                            frames.iter().find_map(|frame| {
                                course2md::artifact::safe_asset_path(&out_dir, &frame.image).ok()
                            })
                        });
                        (issues, thumbnail)
                    });
                    cx.spawn(async move |this, cx| {
                        let Ok((issues, thumbnail)) = published.recv().await else {
                            return;
                        };
                        let _ = this.update(cx, |this, cx| {
                            if !issues.is_empty() {
                                this.message = Some(course2md::tr!(
                                    "笔记已保存，{}",
                                    issues.join(course2md::tr!("；"))
                                ));
                            }
                            if let Some(manifest) = &manifest {
                                update_published_course(
                                    &mut this.courses,
                                    &done,
                                    manifest,
                                    thumbnail,
                                );
                            }
                            this.refresh_library(cx);
                            if open {
                                this.open_completed_conversion(Course::from_completed(&done), cx);
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
                self.task_status = task.status_label().into();
                self.transient_task_result = Some((id.to_owned(), std::time::Instant::now()));
                self.schedule_wake(std::time::Duration::from_secs(8));
            }
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "完成状态尚未保存：{}。已发布的笔记仍保留。",
                    format!("{error:#}")
                ))
            }
        }
        cx.notify();
    }

    pub(crate) fn open_task_export_location(&mut self, id: String, cx: &mut Context<Self>) {
        let destination = (|| {
            let state = &self
                .workspace
                .as_ref()
                .context(course2md::tr!("任务记录暂时不可用"))?
                .state;
            let task = state.task(&id).context(course2md::tr!("任务记录不存在"))?;
            let location = state
                .library(&task.plan.library_id)
                .context(course2md::tr!("任务保存位置暂时不可用"))?;
            workspace::available_task_export(task, location)
        })();
        match destination {
            Ok(path) => cx.reveal_path(&path),
            Err(error) => {
                self.message = Some(course2md::tr!("无法打开导出位置：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

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
        let queue = list(state, move |index, window, cx| {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = index + 1 == items.len();
            desktop
                .update(cx, |this, cx| {
                    this.queue_item(item, index, &tasks, last, focus.get(index), window, cx)
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
    #[allow(clippy::too_many_arguments)]
    fn queue_item(
        &mut self,
        item: &QueueItem,
        index: usize,
        tasks: &[TaskRecord],
        last: bool,
        focus: Option<&FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                    .child(self.running_task_card(task, window, cx))
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
                        Course::from_completed(&Done {
                            out_dir: dir.clone(),
                            title: task.plan.title.clone(),
                            ..Default::default()
                        })
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

    /// 任务卡共享的暂停/取消动作组。队列卡与工作台卡仅按钮构造器、暂停文案与
    /// 元素 id 前缀不同；是否展示由调用点按各自的状态语义决定。
    #[allow(clippy::too_many_arguments)]
    fn task_stop_actions(
        &self,
        mut actions: Div,
        task: &TaskRecord,
        visible: bool,
        id_prefix: &'static str,
        primary: fn(SharedString) -> Button,
        secondary: fn(SharedString) -> Button,
        cx: &mut Context<Self>,
    ) -> Div {
        if visible {
            let id = task.id.clone();
            let cancelling = task.state == TaskState::Pausing && task.intent == Intent::Cancel;
            // 排队中的任务还没有生成任何内容，暂停没有可保留的进度；只提供取消
            let queued = task.state == TaskState::Queued && !self.is_running_task(&task.id);
            actions = actions
                .when(!cancelling && !queued, |actions| {
                    actions.child(
                        primary(SharedString::from(format!("{id_prefix}pause-{id}")))
                            .label(if task.state == TaskState::Pausing {
                                course2md::tr!("正在暂停…")
                            } else {
                                course2md::tr!("暂停")
                            })
                            .icon(icons::pause())
                            .loading(task.state == TaskState::Pausing)
                            .disabled(task.state == TaskState::Pausing)
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |this, _, _, cx| {
                                    this.set_task_intent(id.clone(), Intent::Pause, cx)
                                }
                            })),
                    )
                })
                .child(
                    // 排队时取消是唯一的操作，沿用主操作的按钮形式
                    (if queued { primary } else { secondary })(SharedString::from(format!(
                        "{id_prefix}cancel-{id}"
                    )))
                    .icon(icons::close())
                    .label(if cancelling {
                        course2md::tr!("正在取消…")
                    } else {
                        course2md::tr!("取消任务")
                    })
                    .loading(cancelling)
                    .disabled(cancelling)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.set_task_intent(id.clone(), Intent::Cancel, cx)
                    })),
                );
        }
        actions
    }

    /// One task card in the virtualized queue: the heading row, live progress
    /// or attention summary, and the expanded detail while selected.
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = task.id.clone();
        let active = self.is_running_task(&id);
        let cancelling = task.state == TaskState::Pausing && task.intent == Intent::Cancel;
        let pausing = task.state == TaskState::Pausing && !cancelling;
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
        if !cancelling {
            footer = footer.child(
                tonal_pill(SharedString::from(format!("pause-{id}")), cx)
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
                    })),
            );
        }
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
        footer = footer.child(div().flex_1()).child(
            quiet(SharedString::from(format!("cancel-{id}")))
                .text_color(color(DANGER))
                .icon(icons::close())
                .label(if cancelling {
                    course2md::tr!("正在取消…")
                } else {
                    course2md::tr!("取消任务")
                })
                .loading(cancelling)
                .disabled(cancelling)
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| this.set_task_intent(id.clone(), Intent::Cancel, cx)
                })),
        );
        let _ = window;
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

    /// What the task was submitted with: where it saves, where its text comes
    /// from, and which service receives its content.
    fn task_settings_panel(&self, task: &TaskRecord, library: &SharedString) -> Div {
        let id = &task.id;
        let source = task_text_source(task);
        let service = task
            .plan
            .ai_service
            .as_ref()
            .filter(|_| task.plan.config.llm.uses_service())
            .and_then(|service| self.preferences.version(service));
        let fact = |key: &str, label: &'static str, value: SharedString| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_size(TEXT_AUX)
                        .text_color(color(GRAY))
                        .child(label),
                )
                .child(
                    accessible_text(SharedString::from(format!("{key}-{id}")), value)
                        .text_size(TEXT_BODY)
                        .whitespace_normal(),
                )
        };
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
            .child(fact(
                "task-destination",
                course2md::tr!("保存到"),
                library.clone(),
            ))
            .child(fact(
                "task-text-source",
                course2md::tr!("文字来源"),
                source.into(),
            ))
            .children(service.map(|version| {
                fact(
                    "task-ai-destination",
                    course2md::tr!("AI 服务"),
                    version.config.name.clone().into(),
                )
                .child(
                    div()
                        .text_size(TEXT_AUX)
                        .whitespace_normal()
                        .text_color(color(GRAY))
                        .child(if task.plan.options.vision {
                            course2md::tr!("文字和截图会发送到这个服务")
                        } else {
                            course2md::tr!("文字会发送到这个服务")
                        }),
                )
            }))
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
        let facts = v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .child(detail_row(
                SharedString::from(format!("task-destination-{id}")),
                course2md::tr!("保存到"),
                icons::folder_open()
                    .size(rems(20. / 14.))
                    .text_color(color(GRAY)),
                div()
                    .text_size(TEXT_BODY)
                    .whitespace_normal()
                    .child(destination),
            ))
            .child(self.task_processing_facts(task));
        let (progress, mut history) = self.task_progress_sections(task, cx);
        // 需要处理的卡片只留可操作的信息；保存位置与文字来源收进「查看已完成的处理」
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
        actions = self.task_stop_actions(
            actions,
            task,
            task.state == TaskState::Queued,
            "",
            control,
            control,
            cx,
        );
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
                let reason = activity::component_failure_message(label, outcome.message.as_deref());
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
        // 两个展开行是同一组，沿用 detail_row 的行高与行距（gap_1），而不是区块间距
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
    fn clear_task_card_motion(&mut self, id: &str) {
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

    fn task_processing_facts(&self, task: &TaskRecord) -> Div {
        let source = task_text_source(task);
        let mut facts = v_flex().w_full().min_w_0().gap_1().child(detail_row(
            SharedString::from(format!("task-text-source-{}", task.id)),
            course2md::tr!("文字来源"),
            icons::subtitles()
                .size(rems(20. / 14.))
                .text_color(color(GRAY)),
            div().text_size(TEXT_BODY).whitespace_normal().child(source),
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
                div()
                    .text_size(TEXT_BODY)
                    .whitespace_normal()
                    .child(course2md::tr!(
                        "{}发送到「{}」",
                        if task.plan.options.vision {
                            course2md::tr!("文字与截图")
                        } else {
                            course2md::tr!("文字")
                        },
                        version.config.name,
                    )),
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
        // 部分完成：正文已发布，逐项标明未完成的附加处理并可只补做该项。
        let partial_failures: Vec<(String, String, String)> = if task.state == TaskState::Partial {
            task.artifact
                .as_ref()
                .map(|_| {
                    task_component_failures(task)
                        .into_iter()
                        .map(|(component, label, outcome)| {
                            let reason = activity::component_failure_message(
                                &label,
                                outcome.message.as_deref(),
                            );
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
        if let Some(message) = self
            .task_feedback_view(task)
            .filter(|_| partial_failures.is_empty())
        {
            card = card.child(message);
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
        let (progress, history) = self.task_progress_sections(task, cx);
        card = card.when_some(
            progress.filter(|_| partial_failures.is_empty()),
            |view, progress| view.child(progress),
        );
        let mut actions = h_flex().gap_2().flex_wrap();
        if !partial_failures.is_empty() {
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
        if partial_failures.is_empty() {
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
        card = card.child(actions).when_some(
            history.filter(|_| partial_failures.is_empty()),
            |view, history| view.child(history),
        );
        if let Some(logs) = self
            .task_log_view(task, cx)
            .filter(|_| partial_failures.is_empty())
        {
            card = card.child(logs);
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
    let slot = div()
        .w(rems(96. / 14.))
        .max_w(relative(0.3))
        .flex_shrink_0()
        .aspect_ratio(16. / 9.)
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_SMALL)
        .overflow_hidden()
        .bg(color(SECONDARY_CONTAINER));
    match cover {
        Some(path) => slot.child(
            img(path)
                .size_full()
                .rounded(RADIUS_SMALL)
                .object_fit(ObjectFit::Cover),
        ),
        None => slot.child(
            if queued {
                icons::schedule()
            } else {
                icons::movie()
            }
            .size(rems(20. / 14.))
            .text_color(color(ON_SECONDARY_CONTAINER)),
        ),
    }
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

impl ConversionOptions {
    pub fn apply_to(&self, config: &mut course2md::settings::ConfigFile) {
        use course2md::config::OutputFormat;
        config.defaults.provider = self.provider;
        config.defaults.transcript_source = Some(self.source_mode);
        config.defaults.keep_video = Some(self.keep_video);
        config.defaults.resume = Some(true);
        config.defaults.formats = Some(
            [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json]
                .into_iter()
                .zip(self.formats)
                .filter_map(|(f, selected)| selected.then_some(f))
                .collect(),
        );
        config.llm.enabled = self.llm;
        config.llm.summarize = self.summarize;
        config.llm.vision = self.llm && self.vision;
        config.llm.translate = self.translate.clone();
        config.llm.disable_hint = true;
    }
}

/// 批量任务的笔记名称：候选行里展示的分集标题；裸链接候选回落到「合集 视频 N」
/// （合集名来自探测，占位提示文案永远不会拼进笔记名）。
fn batch_note_title(
    candidate: &source::SourceCandidate,
    collection: Option<&str>,
    index: usize,
) -> String {
    let title = candidate.title.trim();
    if !title.is_empty() && title != candidate.input.trim() {
        return title.to_owned();
    }
    match collection.map(str::trim).filter(|title| !title.is_empty()) {
        Some(collection) => course2md::tr!("{} 视频 {}", collection, index + 1),
        None => course2md::tr!("视频 {}", index + 1),
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

/// 本机识别能力检查：build_plan/build_batch_plan 的错误文案逐字一致，合一处维护。
fn ensure_asr_capability(
    environment: &crate::backend::Environment,
    provider: course2md::config::AsrProvider,
) -> Result<()> {
    use course2md::config::AsrProvider;
    match provider {
        AsrProvider::Coreml => ensure!(
            environment.apple,
            course2md::tr!("Apple 识别组件不可用，请选择其他识别方式")
        ),
        AsrProvider::Gpu => ensure!(
            environment.llama && environment.gpu.is_some(),
            course2md::tr!("没有检测到可用的 GPU 识别引擎。请选择 CPU 或其他本机识别方式。")
        ),
        AsrProvider::Cpu => ensure!(
            environment.llama,
            course2md::tr!("CPU 识别引擎尚未安装。请在设置的「应用」中查看安装方法。")
        ),
        AsrProvider::Npu => ensure!(
            environment.npu(),
            course2md::tr!("没有检测到可用的 Intel NPU 识别环境。请选择其他本机识别方式。")
        ),
        AsrProvider::Api => (),
    }
    Ok(())
}

/// 未显式选择模型时按识别方式补默认模型（NPU 走专属解析）。import_ui 的
/// import_model_request 与两个计划构造函数共用同一回填规则。
pub(crate) fn fill_default_asr_model(
    config: &mut course2md::settings::ConfigFile,
    provider: course2md::config::AsrProvider,
) {
    use course2md::config::AsrProvider;
    if provider != AsrProvider::Api
        && config
            .defaults
            .asr_model
            .as_deref()
            .is_none_or(|model| model.trim().is_empty())
    {
        config.defaults.asr_model = Some(if provider == AsrProvider::Npu {
            course2md::npu::resolve_npu_model(None)
        } else {
            course2md::config::DEFAULT_ASR_MODEL.into()
        });
    }
}

fn validate_plan_config(source: &str, config: &course2md::settings::ConfigFile) -> Result<()> {
    let resolved = course2md::options::resolve(source.to_owned(), &Default::default(), config)?;
    resolved.validate()?;
    if resolved.transcript_source != course2md::config::TranscriptSource::Subtitle {
        // Credentials belong to the service vault, not this static model/provider check.
        resolved.validate_asr_with_auth(false, false)?;
    }
    Ok(())
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
        PlanValidation, QueueItem, StageStatus, TaskGroup, WorkerWait, ai_stage_outcome,
        batch_note_title, task_failure_summary, task_stage_progress, update_draft_source_title,
        update_input_form, validate_plan_config, validate_plan_storage, worker_wait_state,
    };
    use course2md::progress::Stage;

    #[test]
    fn batch_titles_come_from_candidate_rows() {
        let candidate = crate::source::SourceCandidate {
            input: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=2".into(),
            title: "视频与图书介绍".into(),
            identity: None,
            duration: None,
            thumbnail: None,
        };
        assert_eq!(
            batch_note_title(&candidate, Some("合集"), 1),
            "视频与图书介绍"
        );
        // 裸链接候选：回落到「合集 视频 N」，与列表行的展示规则一致
        let bare = crate::source::SourceCandidate {
            title: candidate.input.clone(),
            ..candidate.clone()
        };
        assert_eq!(
            batch_note_title(&bare, Some("《高等数学》"), 1),
            "《高等数学》 视频 2"
        );
        assert_eq!(batch_note_title(&bare, None, 0), "视频 1");
        let blank = crate::source::SourceCandidate {
            title: "  ".into(),
            ..candidate.clone()
        };
        assert_eq!(
            batch_note_title(&blank, Some("《高等数学》"), 4),
            "《高等数学》 视频 5"
        );
    }

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

    #[test]
    fn a_published_repair_replaces_the_stale_card_before_a_library_scan() {
        use crate::{backend::Done as Completed, notes::Course};
        use course2md::artifact::{Manifest, Outcome, Outcomes};
        use std::{
            path::PathBuf,
            time::{Duration, UNIX_EPOCH},
        };

        let outcomes = Outcomes {
            transcript: Outcome::succeeded(),
            proofreading: Outcome::failed("服务拒绝凭据"),
            ..Default::default()
        };
        let old_manifest = Manifest {
            schema: 1,
            task_id: "old".into(),
            course_id: "course".into(),
            source_id: "same-source".into(),
            version_id: "old".into(),
            title: "原始课程名".into(),
            created_at_ms: 1_000,
            revision: 1,
            document: "document.json".into(),
            markdown: "course.md".into(),
            frames: Vec::new(),
            assets: Vec::new(),
            outputs: Vec::new(),
            outcomes,
            partial: true,
        };
        let old = Course {
            dir: "/library/course/versions/old".into(),
            title: "用户命名".into(),
            modified: UNIX_EPOCH + Duration::from_secs(1),
            slides: 0,
            thumbnail: Some("/old-cover.jpg".into()),
            manifest: Some(old_manifest.clone()),
            warning: None,
        };
        let mut other_library = old.clone();
        other_library.dir = "/other-library/course/versions/old".into();
        let mut courses = vec![old.clone(), other_library.clone()];
        let mut repaired = old_manifest;
        repaired.task_id = "repair".into();
        repaired.version_id = "repair".into();
        repaired.revision = 2;
        repaired.created_at_ms = 2_000;
        repaired.partial = false;
        repaired.outcomes.proofreading = Outcome::succeeded();
        let done = Completed {
            out_dir: "/library/course/versions/repair".into(),
            title: repaired.title.clone(),
            ..Default::default()
        };
        super::update_published_course(
            &mut courses,
            &done,
            &repaired,
            Some(PathBuf::from("/new-cover.jpg")),
        );
        assert_eq!(courses.len(), 2);
        let current = &courses[0];
        assert_eq!(current.dir, done.out_dir);
        assert_eq!(current.title, "用户命名");
        assert_eq!(
            current.thumbnail.as_deref(),
            Some(std::path::Path::new("/new-cover.jpg"))
        );
        assert!(!current.incomplete());
        assert_eq!(current.manifest.as_ref().unwrap().version_id, "repair");
        assert_eq!(courses[1].dir, other_library.dir);
        assert!(courses[1].incomplete());
        assert!(old.manifest.as_ref().unwrap().partial);
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
    fn unchanged_form_values_do_not_request_a_save_or_update_the_timestamp() {
        let mut input = crate::workspace::Draft::new(true, "library".into(), Default::default());
        let source = source("https://example.test/a", "课程 A");
        update_draft_source_title(
            &mut input,
            source.input.clone(),
            source.title.clone(),
            Some(source),
        );
        input.updated = 123;
        input.folder = Some(7);
        input.scroll = -42.;
        let expected = input.clone();
        for _ in 0..8 {
            assert!(!update_input_form(
                &mut input,
                expected.input.clone(),
                expected.title.clone(),
                expected.source.clone(),
                expected.options.clone(),
                expected.folder,
                expected.scroll,
            ));
            assert_eq!(input, expected);
        }
        let mut options = expected.options.clone();
        options.formats[0] = !options.formats[0];
        assert!(update_input_form(
            &mut input,
            expected.input,
            expected.title,
            expected.source,
            options.clone(),
            expected.folder,
            expected.scroll,
        ));
        assert_eq!(input.options, options);
        assert!(
            input
                .overrides
                .contains(&crate::workspace::Override::Formats)
        );
        assert_ne!(input.updated, 123);

        let input_text = input.input.clone();
        let title = input.title.clone();
        let source_value = input.source.clone();
        let folder = input.folder;
        let scroll = input.scroll;
        let mut translated = input.options.clone();
        translated.translate = Some("en".into());
        assert!(update_input_form(
            &mut input,
            input_text,
            title,
            source_value,
            translated,
            folder,
            scroll,
        ));
        assert_eq!(input.options.translate.as_deref(), Some("en"));
        assert!(
            input
                .overrides
                .contains(&crate::workspace::Override::Translate)
        );
    }

    #[test]
    fn temporary_speech_service_keeps_the_explicit_local_engine_for_the_same_input() {
        let mut draft = crate::workspace::Draft::new(true, "library".into(), Default::default());
        draft.change_source("video".into());
        let mut options = draft.options.clone();
        options.provider = Some(course2md::config::AsrProvider::Cpu);
        update_input_form(
            &mut draft,
            "video".into(),
            "".into(),
            None,
            options.clone(),
            None,
            0.,
        );
        assert_eq!(
            draft.local_provider,
            Some(course2md::config::AsrProvider::Cpu)
        );
        options.provider = Some(course2md::config::AsrProvider::Api);
        update_input_form(
            &mut draft,
            "video".into(),
            "".into(),
            None,
            options,
            None,
            0.,
        );
        assert_eq!(
            draft.options.provider,
            Some(course2md::config::AsrProvider::Api)
        );
        assert_eq!(
            draft.local_provider,
            Some(course2md::config::AsrProvider::Cpu)
        );
        let next = crate::workspace::Draft::new(true, "library".into(), Default::default());
        assert_eq!(next.local_provider, None);
    }

    #[test]
    fn plan_preview_uses_loaded_storage_but_submission_rechecks_identity_and_folders() {
        let directory = tempfile::tempdir().unwrap();
        let location = crate::workspace::LibraryLocation {
            id: "library".into(),
            name: "笔记库".into(),
            root: directory.path().to_owned(),
            previous_roots: Vec::new(),
        };
        let cached = crate::storage_ui::LocationCheck {
            available: true,
            needs_reassociation: false,
            problem: None,
        };
        let mut index = crate::organize::Library::default();
        index.folders.insert(7, "复习".into());
        std::fs::write(
            location.root.join(".course2md-library-id"),
            "different-library",
        )
        .unwrap();
        validate_plan_storage(
            PlanValidation::Preview,
            &location,
            Some(7),
            Some(&cached),
            Some(&index),
        )
        .unwrap();
        assert!(
            validate_plan_storage(
                PlanValidation::Submission,
                &location,
                Some(7),
                Some(&cached),
                Some(&index)
            )
            .is_err()
        );
        std::fs::write(location.root.join(".course2md-library-id"), &location.id).unwrap();
        validate_plan_storage(
            PlanValidation::Submission,
            &location,
            None,
            Some(&cached),
            Some(&index),
        )
        .unwrap();
        assert!(
            validate_plan_storage(
                PlanValidation::Submission,
                &location,
                Some(7),
                Some(&cached),
                Some(&index)
            )
            .is_err(),
            "a removed folder cannot be authorized by an old preview"
        );
        assert!(
            validate_plan_storage(PlanValidation::Preview, &location, None, None, None)
                .unwrap_err()
                .to_string()
                .contains("正在检查")
        );
    }

    fn source(input: &str, title: &str) -> crate::source::Source {
        crate::source::Source {
            input: input.into(),
            title: title.into(),
            identity: format!("online:{input}"),
            online: true,
            ..Default::default()
        }
    }

    #[test]
    fn changing_source_keeps_automatic_title_provenance_and_frozen_task() {
        use crate::workspace::{TaskPlan, Workspace};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("workspace.json");
        let root = dir.path().join("library");
        let mut workspace =
            Workspace::open_at_inline(file.clone(), root.clone(), Default::default()).unwrap();
        let a = source("https://example.test/video-a", "课程 A");
        let b = source("https://example.test/video-b", "课程 B");
        let id = workspace
            .transaction(|state| {
                update_draft_source_title(
                    state.draft_mut().unwrap(),
                    a.input.clone(),
                    a.title.clone(),
                    Some(a.clone()),
                );
                state.enqueue(
                    TaskPlan {
                        operation: Default::default(),
                        source: a.clone(),
                        source_id: a.identity.clone(),
                        title: a.title.clone(),
                        library_id: state.default_library.clone(),
                        folder: None,
                        options: Default::default(),
                        subtitle: None,
                        config: Default::default(),
                        asr_service: None,
                        ai_service: None,
                    },
                    None,
                )
            })
            .unwrap()
            .0;
        // The visible title input still contains A until B's metadata arrives.
        // More than one autosave, and a restart in between, must not make it custom.
        for _ in 0..3 {
            workspace
                .transaction(|state| {
                    update_draft_source_title(
                        state.draft_mut().unwrap(),
                        b.input.clone(),
                        a.title.clone(),
                        None,
                    );
                    Ok(())
                })
                .unwrap();
        }
        workspace =
            Workspace::open_at_inline(file.clone(), root.clone(), Default::default()).unwrap();
        assert!(!workspace.state.draft().unwrap().custom_title);
        assert!(workspace.state.draft().unwrap().source.is_none());
        workspace
            .transaction(|state| {
                update_draft_source_title(
                    state.draft_mut().unwrap(),
                    b.input.clone(),
                    b.title.clone(),
                    Some(b.clone()),
                );
                Ok(())
            })
            .unwrap();
        let restored = Workspace::open_at_inline(file, root, Default::default()).unwrap();
        let draft = restored.state.draft().unwrap();
        assert_eq!(draft.title, "课程 B");
        assert!(!draft.custom_title);
        assert_eq!(draft.source.as_ref().unwrap(), &b);
        let task = restored.state.task(&id).unwrap();
        assert_eq!(task.plan.source, a);
        assert_eq!(task.plan.title, "课程 A");
    }

    #[test]
    fn manual_title_survives_a_source_change_and_late_metadata() {
        let mut draft = crate::workspace::Draft::new(true, "library".into(), Default::default());
        let a = source("https://example.test/a", "课程 A");
        let b = source("https://example.test/b", "课程 B");
        update_draft_source_title(&mut draft, a.input.clone(), a.title.clone(), Some(a));
        // Source and title may both change before the autosave deadline.
        update_draft_source_title(&mut draft, b.input.clone(), "我的复习笔记".into(), None);
        assert!(draft.custom_title);
        let revision = draft.revision;
        assert!(crate::workspace::accept_source(&mut draft, revision, b));
        assert_eq!(draft.title, "我的复习笔记");
        assert!(draft.custom_title);
    }

    #[test]
    fn submit_validates_the_selected_model_only_when_speech_recognition_is_needed() {
        use course2md::config::{AsrProvider, TranscriptSource};
        let mut config = course2md::settings::ConfigFile::default();
        config.defaults.provider = Some(AsrProvider::Cpu);
        config.defaults.asr_model = Some("whisper".into());
        config.defaults.transcript_source = Some(TranscriptSource::Asr);
        assert!(validate_plan_config("video.mp4", &config).is_err());
        config.defaults.transcript_source = Some(TranscriptSource::Subtitle);
        validate_plan_config("video.mp4", &config).unwrap();
        config.defaults.provider = Some(AsrProvider::Coreml);
        config.defaults.asr_model = Some("qwen3-0.6b".into());
        config.defaults.transcript_source = Some(TranscriptSource::Asr);
        validate_plan_config("video.mp4", &config).unwrap();
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
            super::actionable_task_count(&[task, completed, history, running]),
            2
        );
    }
}
