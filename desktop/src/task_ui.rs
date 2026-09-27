//! The current input form and task execution. Navigation never supplies retry inputs.
use super::*;
use crate::{
    notes::default_output,
    preferences::ServiceRefs,
    theme::*,
    workspace::{Intent, TaskPlan, TaskRecord, TaskState},
};
use anyhow::{Context as _, Result, ensure};
use gpui_component::button::*;
use std::collections::HashMap;
use std::rc::Rc;

/// One row of the virtualized task queue. Task rows carry their index into
/// the sorted task snapshot captured for the frame.
enum QueueItem {
    Unavailable,
    Empty,
    GroupLabel { group: TaskGroup, count: usize },
    HistoryToggle { count: usize, open: Entity<bool> },
    Task(usize),
}

impl QueueItem {
    /// Identity for the list splice: stable across content updates that keep
    /// the row in place, so measured heights and scroll anchors survive.
    fn key(&self, tasks: &[TaskRecord]) -> String {
        match self {
            QueueItem::Unavailable => "unavailable".to_owned(),
            QueueItem::Empty => "empty".to_owned(),
            QueueItem::GroupLabel { group, .. } => format!("group-{group:?}"),
            QueueItem::HistoryToggle { .. } => "history-toggle".to_owned(),
            QueueItem::Task(index) => format!("task-{}", tasks[*index].id),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlanValidation {
    Preview,
    Submission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TaskGroup {
    Attention,
    Processing,
    Finished,
    History,
}

impl TaskGroup {
    fn label(self) -> &'static str {
        match self {
            Self::Attention => "需要处理",
            Self::Processing => "进行中",
            Self::Finished => "已结束",
            Self::History => "此前处理记录",
        }
    }
}

fn task_group(task: &TaskRecord) -> TaskGroup {
    if task.handled_by.is_some() {
        TaskGroup::History
    } else {
        match task.state {
            TaskState::Paused
            | TaskState::NeedsAttention
            | TaskState::Uncertain
            | TaskState::Partial => TaskGroup::Attention,
            TaskState::Queued | TaskState::Running | TaskState::Pausing => TaskGroup::Processing,
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
                TaskGroup::Attention | TaskGroup::Processing
            )
        })
        .count()
}

/// Published task state and its visible card change together. A full library
/// scan can include slow locations, so its old manifest cannot be the only
/// source of the card while the newly published version is already readable.
fn update_published_course(
    courses: &mut Vec<Course>,
    done: &Completed,
    manifest: &course2md::artifact::Manifest,
    thumbnail: Option<PathBuf>,
) {
    let mut course = Course {
        dir: done.out_dir.clone(),
        title: manifest.title.clone(),
        modified: std::time::UNIX_EPOCH + std::time::Duration::from_millis(manifest.created_at_ms),
        slides: manifest.frames.len(),
        segments: done.segments,
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
    Pausing,
    Cancelling,
}

impl WorkerWait {
    fn label(self) -> &'static str {
        match self {
            Self::Starting => "正在启动任务…",
            Self::BetweenStages => "正在继续处理…",
            Self::Finishing => "正在完成笔记…",
            Self::Pausing => "正在保存进度，随后暂停…",
            Self::Cancelling => "正在保存进度并取消任务…",
        }
    }
}

/// A closed stage is not a completed task: later stages may not have started.
/// Only the final render event or worker result identifies the finishing phase;
/// the task itself is still completed by the existing Exit/manifest path.
fn worker_wait_state<'a>(
    active: bool,
    result_received: bool,
    intent: Intent,
    stages: impl IntoIterator<Item = (&'a str, bool)>,
) -> Option<WorkerWait> {
    if !active {
        return None;
    }
    let mut has_stage = false;
    let mut render_done = false;
    for (stage, done) in stages {
        has_stage = true;
        if !done {
            return None;
        }
        render_done |= stage == "render";
    }
    Some(if matches!(intent, Intent::Pause | Intent::Quit) {
        WorkerWait::Pausing
    } else if intent == Intent::Cancel {
        WorkerWait::Cancelling
    } else if render_done || result_received {
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
            Self::Running => "处理中",
            Self::Complete => "已完成",
            Self::Pausing => "暂停中",
            Self::Stopping => "停止中",
            Self::Paused => "已暂停",
            Self::Waiting => "等待继续",
            Self::Failed => "未完成",
            Self::Uncertain => "待确认",
            Self::Cancelled => "已取消",
        }
    }

    fn ink(self) -> Rgba {
        color(match self {
            Self::Running => ACCENT_STRONG,
            Self::Complete => SUCCESS,
            Self::Failed => DANGER,
            Self::Uncertain => WARNING,
            _ => GRAY,
        })
    }

    fn icon(self, id: SharedString, indeterminate: bool, cx: &App) -> AnyElement {
        if matches!(self, Self::Running | Self::Pausing | Self::Stopping) && indeterminate {
            return crate::motion::spinner(id, cx);
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
/// Token usage is the live signal instead: it arrives per completed response and
/// keeps the stage visibly moving without inventing a fraction.
fn task_stage_progress(
    name: &str,
    done: bool,
    live: Option<&activity::Activity>,
    current: u64,
    total: u64,
) -> (String, Option<f32>) {
    if matches!(name, "llm" | "summary" | "summarize") {
        return (
            if !done && let Some(live) = live {
                live.tokens_detail()
                    .unwrap_or_else(|| "等待服务返回结果".into())
            } else {
                String::new()
            },
            None,
        );
    }
    let detail = if !done && let Some(live) = live {
        live.detail(name, true)
    } else if !done
        || name.starts_with("scenes/")
        || name == "transcribe"
        || name.starts_with("model/")
    {
        activity::quantity(name, current, total)
    } else {
        String::new()
    };
    let fraction = (!done)
        .then(|| live.and_then(|stage| stage.fraction()))
        .flatten();
    (detail, fraction)
}

/// A proofreading stage can close after failed requests. Published component
/// outcomes distinguish an ended attempt from a successfully completed step.
fn ai_stage_outcome(name: &str, outcomes: Option<&serde_json::Value>) -> Option<StageStatus> {
    let component = match name {
        "llm" => "proofreading",
        "summary" | "summarize" => "summary",
        _ => return None,
    };
    match outcomes?.get(component)?.get("status")?.as_str()? {
        "succeeded" => Some(StageStatus::Complete),
        "failed" | "partial" => Some(StageStatus::Failed),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TaskFeedback {
    Information(&'static str),
    Failure(String),
}

/// User-initiated stops are normal states. Keep worker diagnostics in raw logs,
/// and retain only a short, actionable localized failure summary in the task.
fn task_feedback(state: TaskState, error: Option<&str>) -> Option<TaskFeedback> {
    match state {
        TaskState::Paused => Some(TaskFeedback::Information("转换已暂停，进度已保留")),
        TaskState::Cancelled => Some(TaskFeedback::Information("任务已取消，已保存的内容仍保留")),
        _ => error.map(|error| {
            let summary = error
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .split(" / ")
                .next()
                .unwrap_or_default()
                .trim();
            let localized = summary
                .chars()
                .any(|c| ('\u{3400}'..='\u{9fff}').contains(&c));
            TaskFeedback::Failure(if localized && summary.chars().count() <= 140 {
                summary.into()
            } else {
                "转换未完成，已保存的进度仍保留。可以继续任务，或查看技术详情。".into()
            })
        }),
    }
}

pub(crate) fn task_attention_summary(task: &TaskRecord) -> String {
    if task
        .blocked
        .iter()
        .any(|request| request.reason == "uncertain")
    {
        return task_status_label(task);
    }
    if let Some(reason) = task_service_repair_reason(task) {
        return reason.into();
    }
    match task_feedback(task.state, task.error.as_deref()) {
        Some(TaskFeedback::Information(message)) => message.into(),
        Some(TaskFeedback::Failure(message)) => message,
        None => task_status_label(task),
    }
}

/// Only concrete configuration rejections change recovery from Retry to Repair.
/// Rate limiting, timeouts and unknown outcomes must keep their own recovery.
fn service_configuration_issue(message: &str) -> Option<&'static str> {
    let lower = message.to_ascii_lowercase();
    if [
        "api key",
        "api_key",
        "unauthorized",
        "forbidden",
        "http 401",
        "http 403",
    ]
    .iter()
    .any(|hint| lower.contains(hint))
        || ["凭据", "鉴权失败", "认证失败", "认证被拒绝"]
            .iter()
            .any(|hint| message.contains(hint))
    {
        Some("AI 服务未接受此任务的凭据或权限，请修复服务后补做。")
    } else if ["http 404", "model not found", "unknown model"]
        .iter()
        .any(|hint| lower.contains(hint))
        || message.contains("服务未找到此任务指定的接口或模型")
    {
        Some("AI 服务未找到此任务使用的接口或模型，请修复服务后补做。")
    } else if ["http 400", "http 422"]
        .iter()
        .any(|hint| lower.contains(hint))
        || message.contains("服务拒绝了请求参数")
    {
        Some("AI 服务拒绝了此任务的请求设置，请修复服务后补做。")
    } else {
        None
    }
}

fn task_service_repair_reason(task: &TaskRecord) -> Option<&'static str> {
    if task.handled_by.is_some()
        || task
            .blocked
            .iter()
            .any(|request| request.reason == "uncertain")
    {
        return None;
    }
    let outcomes = &task.outcomes;
    let failures: Vec<_> = ["proofreading", "summary"]
        .into_iter()
        .filter_map(|component| outcomes.get(component))
        .filter(|outcome| {
            matches!(
                outcome.get("status").and_then(|status| status.as_str()),
                Some("failed" | "partial")
            )
        })
        .collect();
    if failures.is_empty() {
        return None;
    }
    failures
        .iter()
        .filter_map(|outcome| outcome.get("message").and_then(|message| message.as_str()))
        .chain(task.blocked.iter().filter_map(|request| {
            let purpose = request.purpose.as_deref().unwrap_or_default();
            (purpose.contains("proof") || purpose.contains("summary"))
                .then_some(request.message.as_str())
        }))
        .chain(task.error.as_deref())
        .find_map(service_configuration_issue)
}

pub(crate) fn task_requires_service_repair(task: &TaskRecord) -> bool {
    task_service_repair_reason(task).is_some()
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
            let mut row = v_flex().w_full().min_w_0().gap_2().child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .flex_wrap()
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
                        // 与卡片内 detail_row 同一 160px 标签列：值/状态列共用一条
                        // 起始尺（review#2；theme.rs settings detail row 的宽度）
                        .w(rems(160. / 14.))
                        .max_w_full()
                        .flex_shrink_0(),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(rems(12.))
                            .max_w_full()
                            .gap_3()
                            .items_start()
                            .child(
                                accessible_text(status_id, stage.status.label())
                                    .w(rems(4.5))
                                    .flex_shrink_0()
                                    .text_size(TEXT_BODY)
                                    .text_color(stage.status.ink()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_size(TEXT_AUX)
                                    .text_color(color(GRAY))
                                    .child(stage.detail),
                            ),
                    ),
            );
            if let Some(fraction) = stage.fraction {
                row = row.child(div().pl(rems(2.)).child(crate::motion::progress(
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

#[derive(IntoElement)]
struct TaskProcessingDetails {
    task_id: String,
    rows: Vec<TaskStage>,
    open: bool,
    animate: bool,
    desktop: WeakEntity<Desktop>,
}

impl RenderOnce for TaskProcessingDetails {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Open state lives on Desktop: a virtualized task row unmounts when it
        // leaves the viewport, and keyed window state would be forgotten.
        let open = self.open;
        let task_id = self.task_id.clone();
        let desktop = self.desktop.clone();
        let details = v_flex().child(TaskStages { rows: self.rows });
        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                quiet(SharedString::from(format!(
                    "task-stage-details-toggle-{}",
                    self.task_id
                )))
                .self_start()
                .icon(if open {
                    icons::chevron_up()
                } else {
                    icons::chevron_down()
                })
                .label(if open {
                    "收起处理详情"
                } else {
                    "查看已完成的处理"
                })
                .on_click(move |_, _, cx| {
                    let _ = desktop.update(cx, |this, cx| {
                        let key = format!("task-stage-details-state-{task_id}");
                        if !this.task_panels_open.remove(&key) {
                            this.task_panels_open.insert(key);
                        } else {
                            // Closing unmounts the detail; reopening re-enters.
                            this.entered
                                .remove(&format!("task-stage-details-{task_id}"));
                        }
                        cx.notify();
                    });
                }),
            )
            .child(if self.animate {
                disclosure(
                    SharedString::from(format!("task-stage-details-{}", self.task_id)),
                    open,
                    details,
                    window,
                    cx,
                )
            } else if open {
                details.into_any_element()
            } else {
                div().hidden().into_any_element()
            })
    }
}

#[derive(IntoElement)]
struct TaskRawLogs {
    task_id: String,
    text: String,
    open: bool,
    animate: bool,
    desktop: WeakEntity<Desktop>,
}

impl RenderOnce for TaskRawLogs {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let open = self.open;
        let task_id = self.task_id.clone();
        let desktop = self.desktop.clone();
        let log = v_flex().child(
            div()
                .id(SharedString::from(format!(
                    "task-raw-log-content-{}",
                    self.task_id
                )))
                .w_full()
                .min_w_0()
                .max_h(rems(16.))
                .overflow_y_scroll()
                .p_4()
                .rounded(RADIUS_CARD)
                .bg(color(INSET))
                .child(
                    accessible_text(
                        SharedString::from(format!("task-raw-log-text-{}", self.task_id)),
                        self.text,
                    )
                    .text_size(TEXT_AUX)
                    .text_color(color(GRAY))
                    .whitespace_normal(),
                ),
        );
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                quiet(SharedString::from(format!(
                    "task-raw-log-toggle-{}",
                    self.task_id
                )))
                .self_start()
                .icon(if open {
                    icons::chevron_up()
                } else {
                    // 与同级「查看已完成的处理」一致：可展开项用 chevron 表达（review4 可选）
                    icons::chevron_down()
                })
                .label(if open {
                    "收起技术详情"
                } else {
                    "技术详情"
                })
                .on_click(move |_, _, cx| {
                    let _ = desktop.update(cx, |this, cx| {
                        let key = format!("task-raw-log-state-{task_id}");
                        if !this.task_panels_open.remove(&key) {
                            this.task_panels_open.insert(key);
                        } else {
                            this.entered
                                .remove(&format!("task-raw-log-disclosure-{task_id}"));
                        }
                        cx.notify();
                    });
                }),
            )
            .child(if self.animate {
                disclosure(
                    SharedString::from(format!("task-raw-log-disclosure-{}", self.task_id)),
                    open,
                    log,
                    window,
                    cx,
                )
            } else if open {
                log.into_any_element()
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
            let check = cached.context("正在检查保存位置，请稍候")?;
            ensure!(
                check.available,
                "保存位置暂时不可访问。请连接对应磁盘，或在设置的存储中选择其他位置。当前输入和已有任务仍保留。"
            );
            ensure!(
                !check.needs_reassociation,
                "这个保存位置尚未重新关联，请在设置的存储中选择“重新关联此保存位置”。原任务和文件仍保留。"
            );
            if let Some(problem) = &check.problem {
                anyhow::bail!("{problem}");
            }
            match folder {
                Some(folder) => index
                    .context("正在读取文件夹信息，请稍候")?
                    .folders
                    .contains_key(&folder),
                None => true,
            }
        }
        PlanValidation::Submission => {
            ensure!(
                library.root.is_dir(),
                "保存位置暂时不可访问。请连接对应磁盘，或在设置的存储中选择其他位置。当前输入和已有任务仍保留。"
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
        "文件夹已删除。请选择其他文件夹，或保存到未分类。"
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
        draft.local_provider = Some(options.provider);
    } else if !draft.options.uses_cloud_provider() {
        draft.local_provider = Some(draft.options.provider);
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
                Err(error) => self.workspace_error = Some(format!("记录仍未保存：{error:#}")),
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
                    self.workspace_error = Some(format!("记录仍无法读取，原件已保留：{error:#}"))
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
            self.workspace_error =
                Some("当前还有处理或文件移动在进行，请等候停止后再重建记录。".into());
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
                self.workspace_error = Some(format!("记录尚未重建，原件仍保留：{error:#}"))
            }
        }
        cx.notify();
    }

    pub fn recommended_local_provider(&self) -> course2md::config::AsrProvider {
        use course2md::config::AsrProvider;
        match &self.environment {
            Some(environment) if environment.apple => AsrProvider::Coreml,
            Some(environment) if environment.gpu.is_some() && environment.llama => AsrProvider::Gpu,
            Some(environment) if environment.npu && !environment.llama => AsrProvider::Npu,
            Some(_) => AsrProvider::Cpu,
            // Detection runs in the background. A temporary display hint must
            // not scan Linux devices or PATH while the interface is rendering.
            None if course2md::config::apple_native_available() => AsrProvider::Coreml,
            None => AsrProvider::Cpu,
        }
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
            self.workspace_error = Some("当前输入记录暂时不可用，窗口中的内容仍保留。".into());
            return false;
        };
        self.draft_deadline = None;
        if !update_input_form(&mut next, input, title, source, options, folder, scroll) {
            return true;
        }
        let result = workspace.transaction(|state| {
            *state.draft_mut().context("没有可保存的输入")? = next;
            Ok(())
        });
        if let Err(error) = result {
            self.workspace_error =
                Some(format!("当前输入尚未保存：{error:#}。内容仍保留在窗口中。"));
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
        self.source_candidates.clear();
        self.source_collection_title = None;
        self.subtitle_error = draft.source.as_ref().and_then(|source| {
            source
                .subtitle_read_error
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| {
                    source
                        .subtitle_request
                        .as_ref()
                        .map(|_| "所选字幕还未读取完成，请继续读取或选择其他文字来源。".into())
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
                Err(error) => self.workspace_error = Some(format!("无法切换来源：{error:#}")),
            }
        }
    }

    fn build_plan(&self, validation: PlanValidation) -> Result<TaskPlan> {
        self.ordinary_preferences_ready_for_submit()?;
        ensure!(
            !self.preference_defaults_pending,
            "默认设置已保存，当前视频的选项尚未同步。请重试保存输入记录。"
        );
        let workspace = self.workspace.as_ref().context("输入与任务记录尚未恢复")?;
        let draft = workspace.state.draft().context("当前输入尚未准备好")?;
        ensure!(
            !draft.input.is_empty(),
            if draft.online {
                "先粘贴视频链接"
            } else {
                "先选择一个视频"
            }
        );
        ensure!(
            !self.subtitle_loading && self.preview_cancel.is_none(),
            "正在确认来源，请等待读取完成"
        );
        ensure!(
            self.source_preview.is_some(),
            "本次来源尚未确认，请重新读取视频"
        );
        ensure!(
            self.subtitle_error.is_none(),
            "当前字幕尚未确认，请重新读取字幕，或明确选择其他文字来源"
        );
        let source = draft.source.clone().context("请先读取并确认视频")?;
        let environment = self
            .environment
            .as_ref()
            .context("正在检查生成笔记需要的组件，请稍候")?;
        ensure!(
            environment.engine,
            "生成引擎无法启动。请在设置的应用与诊断中检查应用组件。"
        );
        ensure!(
            environment.ffmpeg && environment.ffprobe,
            "视频读取组件不可用。请在设置的应用与诊断中查看 FFmpeg 的安装方法。"
        );
        ensure!(
            !source.online || environment.ytdlp,
            "在线视频读取组件不可用。请在设置的应用与诊断中查看 yt-dlp 的安装方法。"
        );
        ensure!(
            draft.options.source_mode == 2
                || (source.subtitle_request.is_none() && source.subtitle_read_error.is_none()),
            "所选字幕还未确认，请继续读取或明确选择其他文字来源。"
        );
        ensure!(
            !source.identity.is_empty(),
            "视频身份尚未确认，请重新读取视频"
        );
        ensure!(!draft.title.trim().is_empty(), "请填写笔记名称");
        let library = workspace
            .state
            .library(&draft.library_id)
            .context("保存位置已不在课程库中，请选择保存位置")?;
        validate_plan_storage(
            validation,
            library,
            draft.folder,
            self.cached_location_check(library),
            self.library_indexes.get(&library.root),
        )?;
        let mut config = draft
            .base_config
            .clone()
            .unwrap_or_else(|| self.preferences.defaults_config());
        draft.options.apply_to(&mut config);
        config.defaults.model_dir = Some(course2md::config::model_dir_from(
            config.defaults.model_dir.as_deref(),
        ));
        let using_subtitle = draft.options.source_mode != 2
            && (source.selected_subtitle.is_some() || draft.subtitle.is_some());
        if using_subtitle {
            if let Some(subtitle) = &source.selected_subtitle {
                ensure!(
                    subtitle.source_identity == source.identity,
                    "所选字幕与当前视频不匹配。请重新读取字幕，已填写的内容会保留。"
                );
            }
            config.defaults.transcript_source = Some(course2md::config::TranscriptSource::Subtitle);
        } else {
            use course2md::subtitle::SubtitleEvidence;
            ensure!(
                draft.options.source_mode == 2
                    || (draft.options.source_mode == 0
                        && matches!(
                            source.subtitles,
                            SubtitleEvidence::NoneFound
                                | SubtitleEvidence::Unsupported { .. }
                                | SubtitleEvidence::Failed { .. }
                        )),
                "字幕还未确认。请读取字幕，或明确选择识别视频声音。"
            );
            config.defaults.transcript_source = Some(course2md::config::TranscriptSource::Asr);
            let environment = self
                .environment
                .as_ref()
                .context("正在检查本机识别能力，请稍候")?;
            let provider = config
                .defaults
                .provider
                .unwrap_or_else(|| self.recommended_local_provider());
            use course2md::config::AsrProvider;
            match provider {
                AsrProvider::Coreml => ensure!(
                    environment.apple,
                    "Apple 原生识别组件不可用。请选择其他本机识别方式，或在设置中检查应用组件。"
                ),
                AsrProvider::Gpu => ensure!(
                    environment.llama && environment.gpu.is_some(),
                    "没有检测到可用的 GPU 识别引擎。请选择 CPU 或其他本机识别方式。"
                ),
                AsrProvider::Cpu => ensure!(
                    environment.llama,
                    "CPU 识别引擎尚未安装。请在设置的应用与诊断中查看安装方法。"
                ),
                AsrProvider::Npu => ensure!(
                    environment.npu,
                    "没有检测到可用的 Intel NPU 识别环境。请选择其他本机识别方式。"
                ),
                AsrProvider::Api => (),
            }
            config.defaults.provider = Some(provider);
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
        let defaults = self.preferences.default_refs();
        let refs = ServiceRefs {
            asr: draft.asr_service.clone().or(defaults.asr),
            llm: draft.ai_service.clone().or(defaults.llm),
        };
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
                "原视频已移动或无法读取，请重新选择视频"
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

    /// The form uses loaded snapshots. Submission rechecks live storage, local
    /// source and dispatch markers before a task can enter the queue.
    pub fn submission_issue(&self) -> Option<String> {
        self.build_plan(PlanValidation::Preview)
            .err()
            .map(|error| format!("{error:#}"))
    }

    pub fn matching_current_task(&self) -> Option<&TaskRecord> {
        let plan = self.build_plan(PlanValidation::Preview).ok()?;
        self.workspace.as_ref()?.state.tasks.iter().find(|task| {
            !task.state.finished() && task.handled_by.is_none() && task.plan.same_work(&plan)
        })
    }

    pub(super) fn clear_queued_message(&mut self) {
        if self
            .message
            .as_deref()
            .is_some_and(|message| message.starts_with("已加入任务"))
        {
            self.message = None;
        }
    }

    pub fn enqueue_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        self.message = None;
        let plan = match self.build_plan(PlanValidation::Submission) {
            Ok(plan) => plan,
            Err(error) => {
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
                self.message = (!created).then(|| {
                    "已有相同处理任务。该任务采用原来的名称和保存位置；当前输入与选项仍保留。"
                        .into()
                });
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
                    Some(format!("任务尚未加入队列：{error:#}。来源和选项已保留。"))
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
            .workspace
            .as_ref()
            .and_then(|w| w.state.library(&task.plan.library_id))
            .context("任务保存位置暂时不可用")?;
        workspace::check_library(location)?;
        ensure!(
            task.work_dir == location.root.join(".course2md/work").join(&task.id),
            "任务位置与课程库记录不匹配，请先恢复存储位置"
        );
        let refs = ServiceRefs {
            asr: task.plan.asr_service.clone(),
            llm: task.plan.ai_service.clone(),
        };
        let config = self
            .preferences
            .resolve_for_execution_with_secrets(&task.plan.config, &refs, secrets)?
            .into_config();
        let course_id = format!(
            "course-{}",
            &course2md::execution::digest(task.plan.source_id.as_bytes())[..32]
        );
        let subtitle_events = if task.plan.options.source_mode != 2 && task.plan.subtitle.is_none()
        {
            task.plan
                .source
                .selected_subtitle
                .as_ref()
                .map(|s| s.events.clone())
        } else {
            None
        };
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
            subtitle: if task.plan.options.source_mode != 2 {
                task.plan.subtitle.clone()
            } else {
                None
            },
            subtitle_events,
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
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.library(&task.plan.library_id))
            .context("任务保存位置尚未恢复")?;
        workspace::check_library(library)?;
        ensure!(
            task.work_dir == library.root.join(".course2md/work").join(&task.id),
            "任务工作目录与保存位置不匹配"
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
            self.workspace_error = Some(format!("停止新请求的指令尚未保存：{error:#}"));
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
                .unwrap_or_else(|_| Err(anyhow::anyhow!("凭据读取线程意外结束")));
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
        let request = self.request_for(&task, &secrets).and_then(|request| {
            request.validate()?;
            self.write_task_control(&task, Vec::new())?;
            Ok(request)
        });
        let result = request.and_then(|request| {
            // The durable running state must exist before the subprocess can do work.
            self.workspace
                .as_mut()
                .context("任务记录不可用")?
                .transaction(|state| {
                    let record = state.task_mut(&task.id).context("任务不存在")?;
                    record.state = TaskState::Running;
                    record.error = None;
                    record.updated = workspace::now();
                    Ok(())
                })?;
            Job::start_task(&request)
        });
        match result {
            Ok(job) => {
                self.clear_queued_message();
                self.job = Some(job);
                self.active_task = Some(task.id.clone());
                self.kind = Kind::Convert;
                self.cancelling = false;
                self.progress.clear();
                self.logs.clear();
                self.pending_done = None;
                self.task_error = None;
                self.task_status = format!("正在生成《{}》", task.plan.title);
            }
            Err(error) => {
                self.fail_task_before_start(&task.id, format!("{error:#}"));
            }
        }
        cx.notify();
    }

    fn fail_task_before_start(&mut self, task_id: &str, message: String) {
        if let Some(workspace) = &mut self.workspace {
            let compensated = workspace.transaction(|state| {
                let record = state.task_mut(task_id).context("任务不存在")?;
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
                self.workspace_error = Some(format!("任务状态尚未保存：{error:#}"));
            }
        }
    }

    pub fn set_task_intent(&mut self, id: String, intent: Intent, cx: &mut Context<Self>) {
        let active = self.active_task.as_deref() == Some(&id) && self.job.is_some();
        if active && intent == Intent::Run {
            self.message = Some("当前任务还在处理，请等候当前操作结束。".into());
            cx.notify();
            return;
        }
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        match workspace.transaction(|state| {
            state.set_intent(&id, intent)?;
            if active && intent != Intent::Run {
                state.task_mut(&id).context("任务记录不存在")?.state = TaskState::Pausing;
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
                    self.workspace_error = Some(format!("任务指令尚未保存：{error:#}"));
                    if self.active_task.as_ref() == Some(&id)
                        && let Some(job) = &self.job
                    {
                        job.cancel();
                    }
                }
                self.start_next_task(cx);
            }
            Err(error) => self.workspace_error = Some(format!("任务状态尚未保存：{error:#}")),
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
        if let Some(workspace) = &mut self.workspace {
            if let Err(error) = workspace.transaction(|state| {
                state.selected_task = Some(id.to_owned());
                if let Some(task) = state.task_mut(id) {
                    task.unread = false;
                }
                Ok(())
            }) {
                self.workspace_error = Some(format!("任务选择尚未保存：{error:#}"));
            }
        }
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
        if self.active_task.as_deref() == Some(&id) && self.job.is_some() {
            self.message = Some("正在保存当前结果，请等候当前任务停止后再选择重新发送。".into());
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
            .filter(|b| b.reason == "uncertain")
            .filter_map(|b| b.request_id.clone())
            .collect();
        if requests.is_empty() {
            return;
        }
        if task.artifact.is_some() {
            let components = task
                .blocked
                .iter()
                .filter(|b| b.reason == "uncertain")
                .filter_map(|b| b.purpose.as_deref())
                .map(|purpose| {
                    if purpose.contains("summary") {
                        "summary".to_owned()
                    } else {
                        "proofreading".to_owned()
                    }
                })
                .collect();
            self.reprocess_task(id, components, requests, cx);
            return;
        }
        if let Some(workspace) = &mut self.workspace {
            let result = workspace.transaction(|state| state.authorize_uncertain(&id, &requests));
            if let Err(error) = result {
                self.workspace_error = Some(format!("重新发送的选择尚未保存：{error:#}"));
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
        mut components: Vec<String>,
        resend: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.active_task.as_deref() == Some(&id) && self.job.is_some() {
            self.message = Some("正在保存当前结果，请等候当前任务停止后再补做。".into());
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
            Err(error) => self.workspace_error = Some(format!("补做任务尚未建立：{error:#}")),
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
            if task
                .blocked
                .iter()
                .any(|request| request.reason == "uncertain")
            {
                self.select_task(&id, cx);
                self.navigate(Page::Task, cx);
                return;
            }
        }
        let components = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
            .and_then(|task| task.artifact.as_ref().map(|path| (task, path)))
            .map(|(task, path)| {
                task_component_failures(task, path)
                    .into_iter()
                    .map(|(component, _, _)| component)
                    .filter(|component| matches!(component.as_str(), "proofreading" | "summary"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if components.is_empty() {
            self.message = Some("这份笔记没有需要修复的校对或摘要".into());
            cx.notify();
            return;
        }
        self.open_reprocess_service_editor(id, components, window, cx);
    }

    pub(crate) fn reprocess_task_with_service(
        &mut self,
        id: String,
        components: Vec<String>,
        ai_service: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = (|| -> Result<String> {
            ensure!(
                self.active_task.as_deref() != Some(&id) || self.job.is_none(),
                "正在保存当前结果，请等待任务停止后再补做"
            );
            let task = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.task(&id))
                .context("原任务记录暂时不可用")?;
            let mut base = task.plan.config.clone();
            base.defaults.transcript_source = Some(course2md::config::TranscriptSource::Subtitle);
            base.llm.enabled = components.iter().any(|part| part == "proofreading");
            base.llm.summarize = components.iter().any(|part| part == "summary");
            let config = self.preferences.config_for_refs(
                &base,
                &ServiceRefs {
                    asr: None,
                    llm: Some(ai_service.clone()),
                },
            )?;
            self.workspace
                .as_mut()
                .context("任务记录暂时不可用")?
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
                self.workspace_error = Some(format!("服务已保存，补做任务尚未建立：{error:#}"));
                cx.notify();
                false
            }
        }
    }

    pub fn adjust_task(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_task.as_deref() == Some(&id) && self.job.is_some() {
            self.message = Some("当前任务还在处理，请先暂停并等候当前步骤结束。".into());
            cx.notify();
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let result = workspace.transaction(|state| state.adjust_task(&id));
        match result {
            Ok(()) => {
                self.following_conversion = None;
                self.invalidate_source();
                self.restore_draft(window, cx);
                self.page = Page::New;
            }
            Err(error) => self.workspace_error = Some(format!("尚未打开任务调整选项：{error:#}")),
        }
        cx.notify();
    }

    pub fn record_task_event(&mut self, event: &Event) {
        let Some(id) = &self.active_task else {
            return;
        };
        let Some(task) = self.workspace.as_mut().and_then(|w| w.state.task_mut(id)) else {
            return;
        };
        // High-frequency log/progress events keep the persisted timestamp;
        // only state the user would act on moves it (and rewrites the mirror).
        match event {
            Event::Log { message } => {
                task.logs.push(message.clone());
                if task.logs.len() > 200 {
                    task.logs.drain(..task.logs.len() - 200);
                }
            }
            Event::Stage { stage, status } => {
                task.updated = workspace::now();
                if stage.starts_with("scenes/") {
                    // Old workers combined scanning and extraction under this
                    // key. Do not retain its completed counter on continuation.
                    task.stages.remove("scenes");
                    if stage == "scenes/scan" && status == "start" {
                        task.stages.remove("scenes/extract");
                    }
                }
                let value = task.stages.entry(stage.clone()).or_default();
                if status == "start" {
                    value.begin();
                } else {
                    value.status = status.clone();
                }
            }
            Event::Progress {
                stage,
                current,
                total,
                message,
            } => {
                let value = task.stages.entry(stage.clone()).or_default();
                value.current = *current;
                value.total = *total;
                value.detail = message.clone();
            }
            Event::Error { message } => {
                task.updated = workspace::now();
                task.error = Some(message.clone());
            }
            Event::Blocked {
                reason,
                request_id,
                purpose,
                description,
                message,
            } => {
                task.updated = workspace::now();
                if reason == "uncertain" {
                    task.state = TaskState::Uncertain;
                }
                let blocked = workspace::BlockedRequest {
                    reason: reason.clone(),
                    request_id: request_id.clone(),
                    purpose: purpose.clone(),
                    description: description.clone().unwrap_or_default(),
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
        self.clear_queued_message();
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
            let task = state.task_mut(id).context("任务记录不存在")?;
            task.updated = workspace::now();
            task.unread = !visible;
            task.outcomes = done
                .as_ref()
                .and_then(|done| done.outcomes.clone())
                .or_else(|| {
                    manifest
                        .as_ref()
                        .and_then(|manifest| serde_json::to_value(&manifest.outcomes).ok())
                })
                .unwrap_or(serde_json::Value::Null);
            workspace::reconcile_receipts(task)?;
            if let (Some(done), Some(manifest)) = (&done, &manifest) {
                task.artifact = Some(done.out_dir.clone());
                let partial = done.partial.unwrap_or(manifest.partial);
                task.state = if task.blocked.iter().any(|b| b.reason == "uncertain") {
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
                    Intent::Run if task.blocked.iter().any(|b| b.reason == "uncertain") => {
                        TaskState::Uncertain
                    }
                    Intent::Run => TaskState::NeedsAttention,
                };
                if task.error.is_none() && !cancelled && task.intent == Intent::Run {
                    task.error = Some(
                        if success {
                            "处理已结束，但尚未发布可读笔记。任务材料和进度已保留。"
                        } else {
                            "生成中断，已保存的进度仍保留。可以继续任务，或查看技术详情。"
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
                if let Some(done) = &done {
                    self.completed = Some(done.clone());
                    if let Some(root) = self
                        .workspace
                        .as_ref()
                        .and_then(|w| w.state.library(&task.plan.library_id))
                        .map(|l| l.root.clone())
                    {
                        let _ = source::save_cover(&task.plan.source, &done.out_dir).map_err(|e| {
                            self.message = Some(format!("笔记已保存，封面暂未保存：{e:#}"))
                        });
                        if let Err(error) = organize::Library::edit(&root, |library| {
                            library.assign(
                                &root,
                                &Course::from_completed(done).storage_dir(),
                                task.plan.folder,
                            )
                        }) {
                            self.message =
                                Some(format!("笔记已保存，文件夹归属暂未更新：{error:#}"));
                        }
                    }
                    if let Some(manifest) = &manifest {
                        let cover = done.out_dir.join("cover.jpg");
                        let thumbnail = cover.is_file().then_some(cover).or_else(|| {
                            manifest.frames.iter().find_map(|frame| {
                                course2md::artifact::safe_asset_path(&done.out_dir, &frame.image)
                                    .ok()
                            })
                        });
                        update_published_course(&mut self.courses, done, manifest, thumbnail);
                    }
                    self.refresh_library(cx);
                    if !task.exports_only() && (follow_input_completion || follow_reader_completion)
                    {
                        self.open_completed_conversion(Course::from_completed(done), cx);
                    }
                }
                self.task_status = task.state.label().into();
                self.transient_task_result = Some((id.to_owned(), std::time::Instant::now()));
            }
            Err(error) => {
                self.workspace_error =
                    Some(format!("完成状态尚未保存：{error:#}。已发布的笔记仍保留。"))
            }
        }
        cx.notify();
    }

    pub(crate) fn open_task_export_location(&mut self, id: String, cx: &mut Context<Self>) {
        let destination = (|| {
            let state = &self.workspace.as_ref().context("任务记录暂时不可用")?.state;
            let task = state.task(&id).context("任务记录不存在")?;
            let location = state
                .library(&task.plan.library_id)
                .context("任务保存位置暂时不可用")?;
            workspace::available_task_export(task, location)
        })();
        match destination {
            Ok(path) => cx.reveal_path(&path),
            Err(error) => self.message = Some(format!("无法打开导出位置：{error:#}")),
        }
        cx.notify();
    }

    pub fn queue_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rem = f32::from(window.rem_size());
        let mut items = Vec::new();
        let mut tasks: Vec<TaskRecord> = Vec::new();
        match &self.workspace {
            None => items.push(QueueItem::Unavailable),
            Some(workspace) if workspace.state.tasks.is_empty() => {
                items.push(QueueItem::Empty);
            }
            Some(workspace) => {
                tasks = workspace.state.tasks.clone();
                tasks.sort_by_key(|task| (task_group(task), std::cmp::Reverse(task.created)));
                let history_open = window.use_keyed_state("task-history-open", cx, |_, _| false);
                // 只有一种状态组时组头与页头汇总重复：此时不再重复分组行（M6）
                let distinct_groups = tasks
                    .iter()
                    .map(task_group)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                let mut previous_group = None;
                for (index, task) in tasks.iter().enumerate() {
                    let group = task_group(task);
                    if previous_group != Some(group) {
                        previous_group = Some(group);
                        let count = tasks
                            .iter()
                            .filter(|task| task_group(task) == group)
                            .count();
                        if group == TaskGroup::History {
                            items.push(QueueItem::HistoryToggle {
                                count,
                                open: history_open.clone(),
                            });
                        } else if distinct_groups > 1 {
                            items.push(QueueItem::GroupLabel { group, count });
                        }
                    }
                    if group == TaskGroup::History && !*history_open.read(cx) {
                        continue;
                    }
                    items.push(QueueItem::Task(index));
                }
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
        let header = (!tasks.is_empty()).then(|| {
            let current_count = tasks
                .iter()
                .filter(|task| task.handled_by.is_none())
                .count();
            let pending = actionable_task_count(&tasks);
            self.queue_page_header(current_count, pending)
        });
        let focus: Rc<Vec<FocusHandle>> = Rc::new(self.queue_focus.clone());
        let state = self.queue_list.clone();
        let items = Rc::new(items);
        let tasks = Rc::new(tasks);
        let desktop = cx.weak_entity();
        let queue = list(state, move |index, _window, cx| {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = index + 1 == items.len();
            desktop
                .update(cx, |this, cx| {
                    this.queue_item(item, index, &tasks, last, focus.get(index), cx)
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

    fn queue_page_header(&self, current_count: usize, pending: usize) -> Div {
        h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .items_center()
            .flex_wrap()
            .child(theme::page_heading(
                "tasks-page-title",
                icons::task().size(px(24.)).text_color(color(ACCENT_STRONG)),
                "任务",
            ))
            .child(
                accessible_text(
                    "tasks-page-summary",
                    if pending == 0 {
                        format!("{current_count} 个任务 · 全部已结束")
                    } else {
                        format!("{current_count} 个任务 · {pending} 个进行中或待处理")
                    },
                )
                .text_sm()
                .text_color(color(MUTED)),
            )
    }

    /// One row of the virtualized queue: every row keeps the spacing the old
    /// single-column layout had, and a focus container keeps a focused control
    /// mounted while it is scrolled out of the viewport.
    fn queue_item(
        &mut self,
        item: &QueueItem,
        index: usize,
        tasks: &[TaskRecord],
        last: bool,
        focus: Option<&FocusHandle>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = match item {
            QueueItem::Unavailable => v_flex()
                .gap_3()
                .child(icons::warning().size_8().text_color(color(WARNING)))
                .child("任务记录暂时无法读取，已保存的笔记仍可打开。")
                .child(
                    outline_pill("task-records-open-library")
                        .icon(icons::book_open())
                        .label("前往我的笔记")
                        .self_start()
                        .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::Library, cx))),
                )
                .into_any_element(),
            QueueItem::Empty => v_flex()
                .gap_3()
                .py_12()
                .items_center()
                .child(icons::task().size_8().text_color(color(MUTED)))
                .child(
                    accessible_text("task-empty-title", "还没有生成任务")
                        .role(Role::Heading)
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(accessible_text(
                    "task-empty-description",
                    "开始生成后，可以在这里查看处理进度和生成结果",
                ))
                .child(
                    primary_pill("task-new")
                        .icon(icons::plus())
                        .label("导入视频")
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.prepare_workbench_input(window, cx) {
                                this.navigate(Page::New, cx);
                            }
                        })),
                )
                .into_any_element(),
            QueueItem::GroupLabel { group, count } => semantic_label(
                SharedString::from(format!("task-group-{group:?}")),
                format!("{} · {count}", group.label()),
                match group {
                    TaskGroup::Attention => icons::warning(),
                    TaskGroup::Processing => icons::play_arrow(),
                    _ => icons::check_circle(),
                },
            )
            .pt_4()
            .text_size(TEXT_TITLE)
            .into_any_element(),
            QueueItem::HistoryToggle { count, open } => {
                let toggle = open.clone();
                quiet("toggle-task-history")
                    .self_start()
                    .icon(if *open.read(cx) {
                        icons::chevron_up()
                    } else {
                        icons::chevron_down()
                    })
                    .label(format!("此前处理记录 · {count}"))
                    .on_click(move |_, _, cx| {
                        toggle.update(cx, |open, cx| {
                            *open = !*open;
                            cx.notify();
                        });
                    })
                    .into_any_element()
            }
            QueueItem::Task(task_index) => {
                let task = &tasks[*task_index];
                let is_selected = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspace.state.selected_task.as_ref())
                    == Some(&task.id);
                self.queue_task_card(task, task_group(task), is_selected, cx)
            }
        };
        let mut wrapper = v_flex()
            .w_full()
            .min_w_0()
            .when(!last, |view| view.mb_4())
            .child(content)
            .id(("queue-item", index))
            .tab_stop(false);
        if let Some(focus) = focus {
            wrapper = wrapper.track_focus(focus);
        }
        wrapper.into_any_element()
    }

    /// One task card in the virtualized queue: the heading row, live progress
    /// or attention summary, and the expanded detail while selected.
    fn queue_task_card(
        &mut self,
        task: &TaskRecord,
        group: TaskGroup,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = task.id.clone();
        let cover = self.task_cover(task);
        let heading = control(SharedString::from(format!("select-{id}")))
            .ghost()
            .accessibility_label(format!(
                "{}，{}，{}",
                task.plan.title,
                task_status_label(task),
                if is_selected {
                    "收起详情"
                } else {
                    "查看详情"
                }
            ))
            .w_full()
            .h_auto()
            .min_h(px(0.))
            .gap_2()
            .items_center()
            .cursor_pointer()
            .rounded(RADIUS_SMALL)
            .p_0()
            .when_some(cover, |heading, cover| {
                heading.child(
                    img(cover)
                        .w(rems(6.))
                        .h(rems(3.375))
                        .object_fit(ObjectFit::Cover)
                        .rounded(RADIUS_SMALL)
                        .flex_shrink_0(),
                )
            })
            .when(self.task_cover(task).is_none(), |heading| {
                heading.child(
                    icons::task()
                        .size(rems(20. / 14.))
                        .flex_shrink_0()
                        .text_color(color(MUTED)),
                )
            })
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
                        accessible_text(
                            SharedString::from(format!("task-created-{id}")),
                            crate::reader_navigation::timestamp_local(task.created * 1000),
                        )
                        .text_sm()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(color(MUTED)),
                    ),
            )
            .child(badge(task_badge_kind(task.state)).child(task_status_label(task)))
            .child(
                if is_selected {
                    icons::chevron_up()
                } else {
                    icons::chevron_down()
                }
                .size_4()
                .flex_shrink_0()
                .text_color(color(MUTED)),
            )
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| {
                    this.show_logs = false;
                    if is_selected {
                        this.clear_task_card_motion(&id);
                        if let Some(workspace) = &mut this.workspace {
                            if let Err(error) = workspace.transaction(|state| {
                                state.selected_task = None;
                                Ok(())
                            }) {
                                this.workspace_error = Some(format!("任务选择尚未保存：{error:#}"));
                            }
                        }
                        cx.notify();
                    } else {
                        this.select_task(&id, cx);
                    }
                }
            }));
        let mut row = h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .items_center()
            .flex_wrap()
            .child(div().flex_1().min_w(rems(14.)).max_w_full().child(heading));
        let has_exports = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.library(&task.plan.library_id))
            .is_some_and(|location| !workspace::task_export_files(task, location).is_empty());
        if let Some(path) = &task.artifact {
            let path = path.clone();
            let title = task.plan.title.clone();
            row = row.child(
                control(SharedString::from(format!("task-read-direct-{id}")))
                    .icon(icons::book_open())
                    .label("阅读笔记")
                    .when(!(task.exports_only() && has_exports), |button| {
                        button.primary()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_course(
                            Course::from_completed(&Completed {
                                out_dir: path.clone(),
                                title: title.clone(),
                                ..Default::default()
                            }),
                            cx,
                        );
                    })),
            );
        }
        if has_exports {
            let task_id = id.clone();
            row = row.child(
                control(SharedString::from(format!("task-open-exports-{id}")))
                    .icon(icons::folder_open())
                    .label("打开导出位置")
                    .when(task.exports_only(), |button| button.primary())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_task_export_location(task_id.clone(), cx)
                    })),
            );
        }
        let mut card = v_flex()
            .gap_3()
            .p_4()
            .w_full()
            .bg(color(SURFACE))
            .border_1()
            .border_color(color(if is_selected { BLUE } else { LINE }))
            .rounded(RADIUS_CARD)
            .child(row);
        if is_selected {
            let mut details = v_flex()
                .gap_4()
                .pt_3()
                .border_t_1()
                .border_color(color(LINE));
            let destination = self
                .workspace
                .as_ref()
                .and_then(|w| w.state.library(&task.plan.library_id))
                .map(|lib| lib.name.clone())
                .unwrap_or_else(|| "保存位置暂时不可用".into());
            let facts = v_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(detail_row(
                    SharedString::from(format!("task-destination-{id}")),
                    "保存到",
                    icons::folder_open()
                        .size(rems(20. / 14.))
                        .text_color(color(GRAY)),
                    div()
                        .text_size(TEXT_BODY)
                        .whitespace_normal()
                        .child(destination),
                ))
                .child(self.task_processing_facts(task));
            let (progress, history) = self.task_progress_sections(task, cx);
            details = details.child(facts).when_some(
                progress.filter(|_| task.state != TaskState::Partial),
                |view, progress| view.child(progress),
            );
            let uncertain: Vec<_> = task
                .blocked
                .iter()
                .filter(|b| b.reason == "uncertain")
                .collect();
            if let Some(message) = self
                .task_feedback_view(task)
                .filter(|_| task.state != TaskState::Partial)
            {
                details = details.child(message);
            }
            if let Some(block) = self.uncertain_block(task, cx) {
                details = details.child(block);
            }
            let mut actions = h_flex().gap_2().flex_wrap();
            if let Some(library) = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.library(&task.plan.library_id))
                && self
                    .cached_location_check(library)
                    .is_some_and(|check| check.needs_reassociation)
            {
                let library_id = library.id.clone();
                actions = actions.child(
                    control(SharedString::from(format!("reassociate-{id}")))
                        .icon(icons::storage())
                        .label("重新关联此保存位置")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.begin_library_reassociation(library_id.clone(), window, cx)
                        })),
                );
            }
            if let Some(followup) = &task.handled_by {
                let next = followup.clone();
                actions = actions.child(
                    control(SharedString::from(format!("followup-{id}")))
                        .icon(icons::arrow_forward())
                        .label("查看后续处理任务")
                        .on_click(cx.listener(move |this, _, _, cx| this.select_task(&next, cx))),
                );
            }
            if matches!(task.state, TaskState::Running | TaskState::Queued)
                || (self.active_task.as_deref() == Some(&id) && self.job.is_some())
            {
                let cancelling = task.state == TaskState::Pausing && task.intent == Intent::Cancel;
                actions = actions
                    .when(!cancelling, |actions| {
                        actions.child(
                            control(SharedString::from(format!("pause-{id}")))
                                .label(if task.state == TaskState::Pausing {
                                    "正在暂停…"
                                } else {
                                    "暂停"
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
                        control(SharedString::from(format!("cancel-{id}")))
                            .icon(icons::close())
                            .label(if cancelling {
                                "正在取消…"
                            } else {
                                "取消任务"
                            })
                            .loading(cancelling)
                            .disabled(cancelling)
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |this, _, _, cx| {
                                    this.set_task_intent(id.clone(), Intent::Cancel, cx)
                                }
                            })),
                    );
            }
            if task.handled_by.is_none()
                && matches!(task.state, TaskState::Paused | TaskState::NeedsAttention)
                && uncertain.is_empty()
            {
                actions = actions.child(
                    control(SharedString::from(format!("resume-{id}")))
                        .icon(icons::play_arrow())
                        .label("继续任务")
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| this.set_task_intent(id.clone(), Intent::Run, cx)
                        })),
                );
            }
            if let Some(path) = &task.artifact {
                let requires_repair = task_requires_service_repair(task);
                if let Some(reason) = task_service_repair_reason(task) {
                    details = details.child(
                        accessible_text(
                            SharedString::from(format!("service-repair-reason-{id}")),
                            reason,
                        )
                        .text_sm(),
                    );
                }
                let repairable =
                    task_component_failures(task, path)
                        .iter()
                        .any(|(component, _, _)| {
                            matches!(component.as_str(), "proofreading" | "summary")
                        });
                if repairable && uncertain.is_empty() && task.handled_by.is_none() {
                    let repair_id = id.clone();
                    actions = actions.child(
                        outline_pill(SharedString::from(format!("repair-ai-task-{id}")))
                            .icon(icons::settings())
                            .label("修复 AI 服务并补做")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.repair_task_service(repair_id.clone(), window, cx);
                            })),
                    );
                }
                for (component, label, outcome) in task_component_failures(task, path) {
                    let unknown_component = uncertain.iter().any(|request| {
                        let purpose = request.purpose.as_deref().unwrap_or_default();
                        (component == "proofreading" && purpose.contains("proof"))
                            || (component == "summary" && purpose.contains("summary"))
                    });
                    if unknown_component {
                        continue;
                    }
                    let reason =
                        activity::component_failure_message(&label, outcome.message.as_deref());
                    let ai_component = matches!(component.as_str(), "proofreading" | "summary");
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
                        let task_id = id.clone();
                        actions = actions.child(
                            control(SharedString::from(format!("retry-{id}-{component}")))
                                .icon(icons::refresh())
                                .label(format!("仅补{label}"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.reprocess_task(
                                        task_id.clone(),
                                        vec![component.clone()],
                                        Vec::new(),
                                        cx,
                                    )
                                })),
                        );
                    }
                }
            }
            if task.handled_by.is_none()
                && !matches!(
                    task.state,
                    TaskState::Running | TaskState::Pausing | TaskState::Queued
                )
            {
                actions = actions.child(
                    outline_pill(SharedString::from(format!("adjust-{id}")))
                        .icon(icons::tune())
                        .label(if task.artifact.is_some() {
                            "调整并生成新版"
                        } else if task.state == TaskState::Paused {
                            "调整选项"
                        } else {
                            "调整后重试"
                        })
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, window, cx| this.adjust_task(id.clone(), window, cx)
                        })),
                );
            }
            details = details.child(actions);
            if matches!(
                task.state,
                TaskState::NeedsAttention | TaskState::Partial | TaskState::Uncertain
            ) {
                details = details.when_some(history, |view, history| view.child(history));
                if let Some(logs) = self.task_log_view(task, cx) {
                    details = details.child(logs);
                }
            }
            let detail_id = format!("task-detail-{id}");
            card = card.child(if self.enter_once(detail_id.clone()) {
                crate::motion::enter(SharedString::from(detail_id), details)
            } else {
                details.into_any_element()
            });
        } else if group == TaskGroup::Processing {
            if let Some(progress) = self.task_progress_sections(task, cx).0 {
                card = card.child(progress);
            }
        } else if group == TaskGroup::Attention {
            if let Some(feedback) = self.task_feedback_view(task) {
                card = card.child(feedback);
            }
            if task.state == TaskState::Partial {
                if let Some(path) = &task.artifact {
                    let failures = task_component_failures(task, path);
                    if !failures.is_empty() {
                        card = card.child(
                            accessible_text(
                                SharedString::from(format!("task-partial-summary-{id}")),
                                format!(
                                    "正文已保存，{}尚未完成",
                                    failures
                                        .iter()
                                        .map(|(_, label, _)| label.as_str())
                                        .collect::<Vec<_>>()
                                        .join("、")
                                ),
                            )
                            .text_size(TEXT_BODY)
                            .text_color(color(GRAY)),
                        );
                    }
                }
            }
        }
        card.into_any_element()
    }

    /// Collapsing a card unmounts its detail; expanding it later starts fresh.
    fn clear_task_card_motion(&mut self, id: &str) {
        self.entered.remove(&format!("task-detail-{id}"));
        self.entered.remove(&format!("task-stage-details-{id}"));
        self.entered
            .remove(&format!("task-raw-log-disclosure-{id}"));
        self.task_panels_open
            .remove(&format!("task-stage-details-state-{id}"));
        self.task_panels_open
            .remove(&format!("task-raw-log-state-{id}"));
    }

    /// Live and retained stages use the same labels, status column and icon slot.
    fn task_stage_rows(&self, task: &TaskRecord) -> Vec<TaskStage> {
        let active = self.active_task.as_deref() == Some(&task.id) && self.job.is_some();
        let mut names: std::collections::BTreeSet<_> = task.stages.keys().cloned().collect();
        if active {
            names.extend(self.progress.keys().cloned());
        }
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort_by_key(|name| task_stage_order(name));
        names
            .into_iter()
            .map(|name| {
                let stored = task.stages.get(&name);
                let live = active.then(|| self.progress.get(&name)).flatten();
                let done = live
                    .map(|stage| stage.done)
                    .unwrap_or_else(|| stored.is_some_and(|stage| stage.status == "done"));
                let outcomes = if active {
                    self.pending_done
                        .as_ref()
                        .and_then(|done| done.outcomes.as_ref())
                } else {
                    Some(&task.outcomes)
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

    fn task_progress_sections(
        &mut self,
        task: &TaskRecord,
        cx: &mut Context<Self>,
    ) -> (Option<Div>, Option<TaskProcessingDetails>) {
        let (completed, current): (Vec<_>, Vec<_>) = self
            .task_stage_rows(task)
            .into_iter()
            .partition(|stage| stage.status == StageStatus::Complete);
        let active = self.active_task.as_deref() == Some(&task.id) && self.job.is_some();
        let waiting = worker_wait_state(
            active,
            self.pending_done.is_some(),
            task.intent,
            self.progress
                .iter()
                .map(|(name, item)| (name.as_str(), item.done)),
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
                        waiting.label(),
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
            let task_id = task.id.clone();
            let open = self
                .task_panels_open
                .contains(&format!("task-stage-details-state-{task_id}"));
            // An opened detail enters once; scrolling the row out of the
            // virtualized list and back must not replay the entrance.
            let animate = open && self.enter_once(format!("task-stage-details-{task_id}"));
            TaskProcessingDetails {
                task_id,
                rows: completed,
                open,
                animate,
                desktop: cx.weak_entity(),
            }
        });
        (progress, history)
    }

    fn task_log_view(&mut self, task: &TaskRecord, cx: &mut Context<Self>) -> Option<TaskRawLogs> {
        let text = task
            .logs
            .iter()
            .chain(task.error.iter())
            .chain(task.blocked.iter().map(|request| &request.message))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then(|| {
            let task_id = task.id.clone();
            let open = self
                .task_panels_open
                .contains(&format!("task-raw-log-state-{task_id}"));
            let animate = open && self.enter_once(format!("task-raw-log-disclosure-{task_id}"));
            TaskRawLogs {
                task_id,
                text,
                open,
                animate,
                desktop: cx.weak_entity(),
            }
        })
    }

    fn task_feedback_view(&self, task: &TaskRecord) -> Option<AnyElement> {
        if task
            .blocked
            .iter()
            .any(|request| request.reason == "uncertain")
        {
            return None;
        }
        let id = SharedString::from(format!("task-feedback-{}", task.id));
        task_feedback(task.state, task.error.as_deref()).map(|feedback| match feedback {
            TaskFeedback::Information(message) => info_callout(id, message).into_any_element(),
            TaskFeedback::Failure(message) => accessible_text(id, message)
                .role(Role::Alert)
                .text_size(TEXT_BODY)
                .whitespace_normal()
                .text_color(color(DANGER))
                .into_any_element(),
        })
    }

    /// Uncertain-outcome block with the explicit resend action; shared by the
    /// queue page and the workbench attention card.
    fn uncertain_block(&self, task: &TaskRecord, cx: &mut Context<Self>) -> Option<Div> {
        let id = task.id.clone();
        let uncertain: Vec<_> = task
            .blocked
            .iter()
            .filter(|b| b.reason == "uncertain")
            .collect();
        if uncertain.is_empty() || task.handled_by.is_some() {
            return None;
        }
        let active = self.active_task.as_deref() == Some(&id) && self.job.is_some();
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
                    "服务可能已处理这些内容，再次提交可能产生额外费用。其他进度已保留。",
                ))
                .when(active, |view| {
                    view.child(accessible_text(
                        SharedString::from(format!("unknown-saving-{id}")),
                        "正在保存当前结果，完成后可以选择重新发送。",
                    ))
                })
                .child(
                    primary_pill(SharedString::from(format!("resend-{id}")))
                        .self_start()
                        .icon(icons::refresh())
                        .label("重新发送以上内容")
                        .disabled(active)
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| this.resend_uncertain(id.clone(), cx)
                        })),
                ),
        )
    }

    fn task_cover(&self, task: &TaskRecord) -> Option<PathBuf> {
        task.plan.source.cover.clone().or_else(|| {
            self.courses
                .iter()
                .find(|course| {
                    task.artifact.as_ref() == Some(&course.dir)
                        || course
                            .manifest
                            .as_ref()
                            .is_some_and(|manifest| manifest.source_id == task.plan.source_id)
                })
                .and_then(|course| course.thumbnail.clone())
        })
    }

    fn task_heading(&self, task: &TaskRecord) -> Div {
        h_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .items_start()
            .when_some(self.task_cover(task), |row, cover| {
                row.child(
                    img(cover)
                        .w(rems(8.))
                        .h(rems(4.5))
                        .object_fit(ObjectFit::Cover)
                        .rounded(RADIUS_SMALL)
                        .flex_shrink_0(),
                )
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
                        h_flex()
                            .gap_3()
                            .flex_wrap()
                            .items_center()
                            .child(
                                badge(task_badge_kind(task.state)).child(task_status_label(task)),
                            )
                            .child(
                                accessible_text(
                                    SharedString::from(format!(
                                        "workbench-task-created-{}",
                                        task.id
                                    )),
                                    crate::reader_navigation::timestamp_local(task.created * 1000),
                                )
                                .text_size(TEXT_AUX)
                                .text_color(color(GRAY)),
                            ),
                    ),
            )
    }

    fn task_processing_facts(&self, task: &TaskRecord) -> Div {
        let source =
            if task.plan.options.source_mode != 2 && task.plan.source.selected_subtitle.is_some() {
                "使用视频字幕"
            } else if task.plan.options.uses_cloud_provider() {
                "通过所选服务识别视频声音"
            } else {
                "在这台电脑上识别视频声音"
            };
        let mut facts = v_flex().w_full().min_w_0().gap_1().child(detail_row(
            SharedString::from(format!("task-text-source-{}", task.id)),
            "文字来源",
            icons::subtitles()
                .size(rems(20. / 14.))
                .text_color(color(GRAY)),
            div().text_size(TEXT_BODY).whitespace_normal().child(source),
        ));
        if (task.plan.options.llm || task.plan.options.summarize)
            && let Some(version) = task
                .plan
                .ai_service
                .as_ref()
                .and_then(|id| self.preferences.version(id))
        {
            facts = facts.child(detail_row(
                SharedString::from(format!("task-ai-destination-{}", task.id)),
                "AI 处理",
                icons::auto_fix()
                    .size(rems(20. / 14.))
                    .text_color(color(GRAY)),
                div()
                    .text_size(TEXT_BODY)
                    .whitespace_normal()
                    .child(format!(
                        "{}发送到「{}」",
                        if task.plan.options.vision {
                            "文字与截图"
                        } else {
                            "文字"
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
                    "笔记已生成，正在打开",
                    crate::motion::spinner(
                        SharedString::from(format!("task-opening-result-spinner-{}", task.id)),
                        cx,
                    ),
                )
                .text_color(color(SUCCESS))))
    }

    /// The current task and its progress retain one reading position as stages change.
    pub fn box_task_running(
        &mut self,
        task: &TaskRecord,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = task.id.clone();
        let active = self.active_task.as_deref() == Some(&id) && self.job.is_some();
        let mut card = v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(self.task_heading(task));
        if task.plan.options.source_mode == 0
            && task.plan.source.selected_subtitle.is_none()
            && matches!(
                task.plan.source.subtitles,
                course2md::subtitle::SubtitleEvidence::Failed { .. }
            )
        {
            card = card.child(info_callout(
                SharedString::from(format!("task-subtitle-fallback-{id}")),
                "字幕暂时不可用，已按默认设置识别视频声音",
            ));
        }
        if task.state == TaskState::Queued && !active {
            let waiting = self
                .workspace
                .as_ref()
                .map(|workspace| {
                    workspace
                        .state
                        .tasks
                        .iter()
                        .take_while(|other| other.id != id)
                        .filter(|other| {
                            other.handled_by.is_none() && other.state == TaskState::Queued
                        })
                        .count()
                })
                .unwrap_or(0);
            card = card.child(info_callout(
                SharedString::from(format!("task-queued-{id}")),
                if waiting > 0 {
                    format!("前面还有 {waiting} 个任务，轮到后会自动开始")
                } else {
                    "即将开始处理".into()
                },
            ));
        }
        let (progress, _) = self.task_progress_sections(task, cx);
        card = card.when_some(progress, |view, progress| view.child(progress));
        let mut actions = h_flex().gap_2().flex_wrap();
        if matches!(
            task.state,
            TaskState::Running | TaskState::Queued | TaskState::Pausing
        ) || active
        {
            let cancelling = task.state == TaskState::Pausing && task.intent == Intent::Cancel;
            actions = actions
                .when(!cancelling, |actions| {
                    actions.child(
                        outline_pill(SharedString::from(format!("box-pause-{id}")))
                            .icon(icons::pause())
                            .label(if task.state == TaskState::Pausing {
                                "正在暂停…"
                            } else {
                                "暂停生成"
                            })
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
                    quiet(SharedString::from(format!("box-cancel-{id}")))
                        .icon(icons::close())
                        .label(if cancelling {
                            "正在取消…"
                        } else {
                            "取消任务"
                        })
                        .loading(cancelling)
                        .disabled(cancelling)
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                this.set_task_intent(id.clone(), Intent::Cancel, cx)
                            }
                        })),
                );
        }
        card = card.child(actions).child(
            theme::supporting_info(
                SharedString::from(format!("task-hint-{id}")),
                if task.state == TaskState::Pausing {
                    if task.intent == Intent::Cancel {
                        "取消后已生成的内容仍会保留。"
                    } else {
                        "暂停后可以继续，已完成的步骤会保留。"
                    }
                } else {
                    "关闭窗口后任务会继续；退出应用会暂停任务。"
                },
            )
            .text_size(TEXT_AUX),
        );
        let more = self
            .workspace
            .as_ref()
            .map(|w| {
                w.state
                    .tasks
                    .iter()
                    .filter(|other| {
                        other.id != id
                            && other.handled_by.is_none()
                            && other.state == TaskState::Queued
                    })
                    .count()
            })
            .unwrap_or(0);
        if active && more > 0 {
            card = card.child(
                div()
                    .text_size(TEXT_AUX)
                    .text_color(color(GRAY))
                    .child(format!("队列中还有 {more} 个任务等待处理。")),
            );
        }
        card
    }

    /// Workbench box card for a task that needs the user: paused, failed or
    /// uncertain outcomes, each with its real recovery action.
    pub fn box_task_attention(&mut self, task: &TaskRecord, cx: &mut Context<Self>) -> Div {
        let id = task.id.clone();
        let uncertain = task
            .blocked
            .iter()
            .any(|request| request.reason == "uncertain")
            && task.handled_by.is_none();
        // 部分完成：正文已发布，逐项标明未完成的附加处理并可只补做该项。
        let partial_failures: Vec<(String, String, String)> = if task.state == TaskState::Partial {
            task.artifact
                .as_ref()
                .map(|path| {
                    task_component_failures(task, path)
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
        if let Some(reason) = task_service_repair_reason(task) {
            let missing = partial_failures
                .iter()
                .map(|(_, label, _)| label.as_str())
                .collect::<Vec<_>>()
                .join("、");
            card = card.child(
                accessible_text(
                    SharedString::from(format!("box-service-repair-reason-{id}")),
                    format!("{missing}尚未完成。{reason}"),
                )
                .text_sm(),
            );
        }
        for (component, _, reason) in partial_failures.iter().filter(|(component, _, _)| {
            !requires_repair || !matches!(component.as_str(), "proofreading" | "summary")
        }) {
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
                        .label("阅读笔记")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_course(
                                Course::from_completed(&Completed {
                                    out_dir: path.clone(),
                                    title: title.clone(),
                                    ..Default::default()
                                }),
                                cx,
                            );
                        })),
                );
            }
            if partial_failures
                .iter()
                .any(|(component, _, _)| matches!(component.as_str(), "proofreading" | "summary"))
            {
                let repair_id = id.clone();
                actions = actions.child(
                    outline_pill(SharedString::from(format!("box-repair-ai-{id}")))
                        .icon(icons::settings())
                        .label("修复 AI 服务并补做")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.repair_task_service(repair_id.clone(), window, cx);
                        })),
                );
            }
            for (component, label, _) in &partial_failures {
                if requires_repair && matches!(component.as_str(), "proofreading" | "summary") {
                    continue;
                }
                let component = component.clone();
                actions = actions.child(
                    quiet(SharedString::from(format!("box-retry-{id}-{component}")))
                        .icon(icons::refresh())
                        .label(format!("仅补{label}"))
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                this.reprocess_task(
                                    id.clone(),
                                    vec![component.clone()],
                                    Vec::new(),
                                    cx,
                                )
                            }
                        })),
                );
            }
            let details_id = id.clone();
            actions = actions.child(
                quiet(SharedString::from(format!("box-task-details-{id}")))
                    .icon(icons::task())
                    .label("查看任务详情")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_task(&details_id, cx);
                        this.navigate(Page::Task, cx);
                    })),
            );
        } else if !uncertain {
            actions = actions.child(
                primary_pill(SharedString::from(format!("box-resume-{id}")))
                    .icon(icons::play_arrow())
                    .self_start()
                    .label(if task.state == TaskState::Paused {
                        "继续生成"
                    } else {
                        "继续任务"
                    })
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.set_task_intent(id.clone(), Intent::Run, cx)
                    })),
            );
        }
        if partial_failures.is_empty() {
            if let Some(library) = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.library(&task.plan.library_id))
                && self
                    .cached_location_check(library)
                    .is_some_and(|check| check.needs_reassociation)
            {
                let library_id = library.id.clone();
                actions = actions.child(
                    outline_pill(SharedString::from(format!("box-reassociate-{id}")))
                        .icon(icons::storage())
                        .label("重新关联此保存位置")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.begin_library_reassociation(library_id.clone(), window, cx)
                        })),
                );
            }
            actions = actions
                .child(
                    outline_pill(SharedString::from(format!("box-adjust-{id}")))
                        .icon(icons::tune())
                        .label(if task.state == TaskState::Paused {
                            "调整选项"
                        } else {
                            "调整后重试"
                        })
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, window, cx| this.adjust_task(id.clone(), window, cx)
                        })),
                )
                .child(
                    quiet(SharedString::from(format!("box-cancel-{id}")))
                        .icon(icons::close())
                        .label("取消任务")
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
        use course2md::config::{AsrProvider, OutputFormat, TranscriptSource};
        config.defaults.provider = match self.provider {
            1 => Some(AsrProvider::Coreml),
            2 => Some(AsrProvider::Gpu),
            3 => Some(AsrProvider::Cpu),
            4 => Some(AsrProvider::Npu),
            5 => Some(AsrProvider::Api),
            _ => None,
        };
        config.defaults.transcript_source = Some(match self.source_mode {
            1 => TranscriptSource::Subtitle,
            2 => TranscriptSource::Asr,
            _ => TranscriptSource::Auto,
        });
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
        config.llm.disable_hint = true;
    }
}

fn task_status_label(task: &TaskRecord) -> String {
    if task.handled_by.is_some() {
        return "已有后续处理任务".into();
    }
    if task
        .blocked
        .iter()
        .any(|request| request.reason == "uncertain")
    {
        let purpose: std::collections::BTreeSet<_> = task
            .blocked
            .iter()
            .filter(|request| request.reason == "uncertain")
            .filter_map(|request| request.purpose.as_deref())
            .collect();
        let description = if !purpose.is_empty()
            && purpose.iter().all(|purpose| purpose.contains("proof"))
        {
            "AI 校对结果未确认"
        } else if !purpose.is_empty() && purpose.iter().all(|purpose| purpose.contains("summary")) {
            "摘要结果未确认"
        } else if purpose
            .iter()
            .any(|purpose| purpose.contains("transcription"))
        {
            "语音识别结果未确认"
        } else {
            "部分处理结果未确认"
        };
        return if task.artifact.is_some() {
            format!("笔记可读，{description}")
        } else {
            description.into()
        };
    }
    if task.exports_only() && task.state == TaskState::Complete {
        "文件已导出".into()
    } else {
        task.state.label().into()
    }
}

fn request_scope(request: &workspace::BlockedRequest) -> String {
    if !request.description.trim().is_empty() {
        request.description.clone()
    } else {
        let purpose = request.purpose.as_deref().unwrap_or_default();
        let name = if purpose.contains("proof") {
            "AI 校对"
        } else if purpose.contains("summary") {
            "生成摘要"
        } else if purpose.contains("transcription") {
            "语音识别"
        } else {
            "外部处理"
        };
        format!("{name}：此历史请求未保存具体内容范围，可在技术详情中查看请求记录。")
    }
}

fn task_stage_order(stage: &str) -> usize {
    activity::stage_order(stage)
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

#[cfg(test)]
mod tests {
    use super::{
        PlanValidation, QueueItem, StageStatus, TaskFeedback, TaskGroup, WorkerWait,
        ai_stage_outcome, task_feedback, task_stage_progress, update_draft_source_title,
        update_input_form, validate_plan_config, validate_plan_storage, worker_wait_state,
    };

    #[gpui::test]
    fn queue_item_keys_track_identity_not_content(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| {
            let tasks = vec![task_record("task-a"), task_record("task-b")];
            let open = cx.new(|_| false);
            let items = [
                QueueItem::GroupLabel {
                    group: TaskGroup::Processing,
                    count: 1,
                },
                QueueItem::Task(0),
                QueueItem::HistoryToggle { count: 1, open },
                QueueItem::Task(1),
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
            assert_eq!(QueueItem::Task(0).key(&moved), "task-task-b");
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
            outcomes: serde_json::Value::Null,
            unread: false,
            logs: Vec::new(),
            blocked: Vec::new(),
            resend: Vec::new(),
        }
    }

    #[test]
    fn a_published_repair_replaces_the_stale_card_before_a_library_scan() {
        use crate::{backend::Completed, notes::Course};
        use course2md::artifact::{Manifest, Outcome, Outcomes};
        use std::{
            path::PathBuf,
            time::{Duration, UNIX_EPOCH},
        };

        let mut outcomes = Outcomes::default();
        outcomes.transcript = Outcome::succeeded();
        outcomes.proofreading = Outcome::failed("服务拒绝凭据");
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
            segments: 1,
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
            segments: 4,
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
        assert_eq!(current.segments, 4);
        assert_eq!(
            current.thumbnail.as_deref(),
            Some(std::path::Path::new("/new-cover.jpg"))
        );
        assert!(!current.description().contains("部分内容待补全"));
        assert_eq!(current.manifest.as_ref().unwrap().version_id, "repair");
        assert_eq!(courses[1].dir, other_library.dir);
        assert!(courses[1].description().contains("部分内容待补全"));
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
            worker_wait_state(true, false, Intent::Run, [("subtitle", true)]),
            Some(WorkerWait::BetweenStages),
        );
        assert_eq!(
            worker_wait_state(
                true,
                false,
                Intent::Run,
                [("subtitle", true), ("download", false)]
            ),
            None,
        );
        assert_eq!(
            worker_wait_state(
                true,
                false,
                Intent::Run,
                [("subtitle", true), ("download", true)]
            ),
            Some(WorkerWait::BetweenStages),
        );
    }

    #[test]
    fn finishing_uses_worker_evidence_and_stops_with_the_process() {
        let stages = [("subtitle", true), ("render", true)];
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
                [("render", true), ("summary", false)]
            ),
            None,
        );
        assert_eq!(worker_wait_state(false, true, Intent::Run, stages), None);
        assert_eq!(worker_wait_state(false, false, Intent::Run, []), None);
    }

    #[test]
    fn ai_dispatch_counts_do_not_claim_received_results() {
        for name in ["llm", "summary", "summarize"] {
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
            activity.done = true;
            let (detail, fraction) = task_stage_progress(name, true, Some(&activity), 1, 1);
            assert!(detail.is_empty());
            assert!(fraction.is_none());
        }
        // Quantities that measure completed work keep their determinate progress.
        let mut screenshots = crate::activity::Activity::new();
        screenshots.update(2, 6, None);
        let (detail, fraction) =
            task_stage_progress("scenes/extract", false, Some(&screenshots), 2, 6);
        assert!(detail.contains("2 / 6"));
        assert_eq!(fraction, Some(2. / 6.));
    }

    #[test]
    fn pause_and_cancel_intents_do_not_report_continued_generation() {
        for intent in [Intent::Pause, Intent::Quit, Intent::Cancel] {
            let expected = if intent == Intent::Cancel {
                WorkerWait::Cancelling
            } else {
                WorkerWait::Pausing
            };
            assert_eq!(worker_wait_state(true, false, intent, []), Some(expected));
            assert_eq!(
                worker_wait_state(true, false, intent, [("llm", true)]),
                Some(expected),
            );
            assert_eq!(
                worker_wait_state(true, true, intent, [("render", true)]),
                Some(expected),
            );
            assert_eq!(worker_wait_state(false, false, intent, []), None);
        }
    }

    #[test]
    fn completed_ai_attempts_use_component_results_to_classify_success() {
        use course2md::artifact::{Outcome, Outcomes, Status};
        let mut outcomes = Outcomes::default();
        outcomes.proofreading = Outcome::failed("服务拒绝凭据");
        outcomes.summary = Outcome::succeeded();
        let value = serde_json::to_value(&outcomes).unwrap();
        assert_eq!(
            ai_stage_outcome("llm", Some(&value)),
            Some(StageStatus::Failed)
        );
        assert_eq!(
            ai_stage_outcome("summary", Some(&value)),
            Some(StageStatus::Complete)
        );
        outcomes.proofreading.status = Status::Partial;
        let value = serde_json::to_value(&outcomes).unwrap();
        assert_eq!(
            ai_stage_outcome("llm", Some(&value)),
            Some(StageStatus::Failed)
        );
        outcomes.proofreading = Outcome::succeeded();
        let value = serde_json::to_value(&outcomes).unwrap();
        assert_eq!(
            ai_stage_outcome("llm", Some(&value)),
            Some(StageStatus::Complete)
        );
        assert_eq!(ai_stage_outcome("llm", None), None);
        assert_eq!(ai_stage_outcome("scenes/extract", Some(&value)), None);
    }

    #[test]
    fn user_pause_is_information_and_worker_failures_have_a_localized_summary() {
        let paused = "生成已暂停，进度已保留 / Generation paused; progress retained";
        assert!(matches!(
            task_feedback(TaskState::Paused, Some(paused)),
            Some(TaskFeedback::Information(_))
        ));
        assert!(matches!(
            task_feedback(TaskState::Cancelled, Some("任务已取消 / Task cancelled")),
            Some(TaskFeedback::Information(_))
        ));
        assert_eq!(
            task_feedback(
                TaskState::NeedsAttention,
                Some("服务拒绝凭据，请检查 API Key / Unauthorized: provider response\nraw details")
            ),
            Some(TaskFeedback::Failure("服务拒绝凭据，请检查 API Key".into())),
        );
        let Some(TaskFeedback::Failure(message)) = task_feedback(
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
    }

    #[test]
    fn temporary_speech_service_keeps_the_explicit_local_engine_for_the_same_input() {
        let mut draft = crate::workspace::Draft::new(true, "library".into(), Default::default());
        draft.change_source("video".into());
        let mut options = draft.options.clone();
        options.provider = 3;
        update_input_form(
            &mut draft,
            "video".into(),
            "".into(),
            None,
            options.clone(),
            None,
            0.,
        );
        assert_eq!(draft.local_provider, Some(3));
        options.provider = 5;
        update_input_form(
            &mut draft,
            "video".into(),
            "".into(),
            None,
            options,
            None,
            0.,
        );
        assert_eq!(draft.options.provider, crate::CLOUD_PROVIDER_INDEX);
        assert_eq!(draft.local_provider, Some(3));
        let next = crate::workspace::Draft::new(true, "library".into(), Default::default());
        assert_eq!(next.local_provider, Some(0));
    }

    #[test]
    fn plan_preview_uses_loaded_storage_but_submission_rechecks_identity_and_folders() {
        let directory = tempfile::tempdir().unwrap();
        let location = crate::workspace::LibraryLocation {
            id: "library".into(),
            name: "课程库".into(),
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
            Workspace::open_at(file.clone(), root.clone(), Default::default()).unwrap();
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
        workspace = Workspace::open_at(file.clone(), root.clone(), Default::default()).unwrap();
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
        let restored = Workspace::open_at(file, root, Default::default()).unwrap();
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
        assert!(draft.accept_source(revision, b));
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

        let mut outcomes = Outcomes::default();
        outcomes.transcript = Outcome::succeeded();
        outcomes.screenshots = Outcome::succeeded();
        outcomes.proofreading = Outcome {
            status: Status::Partial,
            message: Some("部分校对请求尚未完成".into()),
            completed: Some(1),
            total: Some(2),
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
            outcomes: serde_json::to_value(&outcomes).unwrap(),
            unread: false,
            logs: Vec::new(),
            blocked: Vec::new(),
            resend: Vec::new(),
        };
        // The first recovery action in Recent Notes and every action in the
        // task cards must target actual incomplete work, never the first success.
        let failures = super::task_component_failures(&task, &path);
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

        for (message, requires_repair) in [
            (
                "服务未接受此任务保存的凭据，请检查对应服务的 API Key。",
                true,
            ),
            ("此任务使用的凭据没有访问该服务或模型的权限。", true),
            (
                "服务未找到此任务指定的接口或模型，请检查服务地址和模型。",
                true,
            ),
            (
                "服务拒绝了请求参数（HTTP 422），请检查此任务使用的模型与服务设置。",
                true,
            ),
            ("服务请求次数达到限制，请稍后重试。", false),
            ("服务未完成此请求（HTTP 500）。", false),
            ("请求超时，请稍后重试。", false),
        ] {
            outcomes.proofreading.message = Some(message.into());
            task.outcomes = serde_json::to_value(&outcomes).unwrap();
            assert_eq!(
                super::task_requires_service_repair(&task),
                requires_repair,
                "{message}"
            );
        }
        outcomes.proofreading.message = Some("服务拒绝凭据".into());
        task.outcomes = serde_json::to_value(&outcomes).unwrap();
        task.blocked.push(crate::workspace::BlockedRequest {
            reason: "uncertain".into(),
            request_id: Some("pending".into()),
            purpose: Some("summary".into()),
            description: "摘要".into(),
            message: "结果尚未确认".into(),
        });
        assert!(!super::task_requires_service_repair(&task));
        task.blocked.clear();
        task.handled_by = Some("recovery".into());
        assert!(!super::task_requires_service_repair(&task));
        task.handled_by = None;

        outcomes.proofreading = Outcome::succeeded();
        outcomes.exports.insert("html".into(), Outcome::succeeded());
        task.outcomes = serde_json::to_value(outcomes).unwrap();
        assert!(super::task_component_failures(&task, &path).is_empty());
        task.error = Some("服务拒绝凭据".into());
        assert!(!super::task_requires_service_repair(&task));
        task.outcomes = serde_json::Value::Null;
        assert!(super::task_component_failures(&task, &path).is_empty());

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
pub(crate) fn task_component_failures(
    task: &TaskRecord,
    _path: &std::path::Path,
) -> Vec<(String, String, course2md::artifact::Outcome)> {
    let value = &task.outcomes;
    let mut results = Vec::new();
    for (key, label) in [
        ("screenshots", "截图"),
        ("proofreading", "校对"),
        ("summary", "摘要"),
    ] {
        if let Some(outcome) = value
            .get(key)
            .and_then(|v| serde_json::from_value::<course2md::artifact::Outcome>(v.clone()).ok())
            .filter(|outcome| {
                matches!(
                    outcome.status,
                    course2md::artifact::Status::Failed | course2md::artifact::Status::Partial
                )
            })
        {
            results.push((key.into(), label.into(), outcome));
        }
    }
    if let Some(exports) = value.get("exports").and_then(|v| v.as_object()) {
        let failures: Vec<course2md::artifact::Outcome> = exports
            .values()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .filter(|o: &course2md::artifact::Outcome| {
                matches!(
                    o.status,
                    course2md::artifact::Status::Failed | course2md::artifact::Status::Partial
                )
            })
            .collect();
        if !failures.is_empty() {
            results.push((
                "exports".into(),
                "导出".into(),
                course2md::artifact::Outcome::failed(
                    failures
                        .iter()
                        .filter_map(|o| o.message.clone())
                        .collect::<Vec<_>>()
                        .join("；"),
                ),
            ));
        }
    }
    results
}
