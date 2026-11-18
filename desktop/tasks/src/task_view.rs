//! Task presentation logic: the 4 task groups, 5-state badges, actionable failure
//! reasons, in-place actions, and stage progress rows.
use crate::job::Work;
use c2m_engine::execution::Component;
use c2m_protocol::progress::Stage;
use desktop_store::{
    activity,
    workspace::{self, StagePhase, TaskRecord, TaskStatus},
};

/// A non-running task waiting for user attention or action.
pub fn needs_person(task: &TaskRecord) -> bool {
    matches!(task.status, TaskStatus::Failed | TaskStatus::Paused)
}

/// A paused task that can resume directly in place.
pub fn resumable(task: &TaskRecord) -> bool {
    task.status == TaskStatus::Paused
}

/// Task groups on the Tasks page (zero `History` group).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskGroup {
    Running,
    Attention,
    Queued,
    Finished,
}

impl TaskGroup {
    pub const ORDER: [Self; 4] = [Self::Running, Self::Attention, Self::Queued, Self::Finished];

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => c2m_i18n::tr!("正在处理"),
            Self::Attention => c2m_i18n::tr!("需要处理"),
            Self::Queued => c2m_i18n::tr!("排队中"),
            Self::Finished => c2m_i18n::tr!("已结束"),
        }
    }

    pub fn settled(self) -> bool {
        self == Self::Finished
    }
}

pub fn task_group(task: &TaskRecord) -> TaskGroup {
    match task.status {
        TaskStatus::Running => TaskGroup::Running,
        TaskStatus::Failed => TaskGroup::Attention,
        TaskStatus::Queued | TaskStatus::Paused => TaskGroup::Queued,
        TaskStatus::Completed => TaskGroup::Finished,
    }
}

/// Summary counters for the Tasks page header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskCounts {
    pub running: usize,
    pub attention: usize,
    pub queued: usize,
    pub resumable: usize,
    pub pausable: usize,
    pub clearable: usize,
}

pub fn task_counts(tasks: &[TaskRecord]) -> TaskCounts {
    tasks
        .iter()
        .fold(TaskCounts::default(), |mut counts, task| {
            match task.status {
                TaskStatus::Running => {
                    counts.running += 1;
                    counts.pausable += 1;
                }
                TaskStatus::Failed => {
                    counts.attention += 1;
                }
                TaskStatus::Queued => {
                    counts.queued += 1;
                    counts.pausable += 1;
                }
                TaskStatus::Paused => {
                    counts.resumable += 1;
                }
                TaskStatus::Completed => {
                    counts.clearable += 1;
                }
            }
            counts
        })
}

/// App bar badge summary: `(unfinished_count, has_failed_attention)`.
pub fn task_navigation_summary(tasks: &[TaskRecord]) -> (usize, bool) {
    tasks.iter().fold((0, false), |(count, attention), task| {
        let unfinished = task.status != TaskStatus::Completed;
        let failed = task.status == TaskStatus::Failed;
        (count + usize::from(unfinished), attention || failed)
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
            Self::Starting => c2m_i18n::tr!("正在启动任务…"),
            Self::BetweenStages => c2m_i18n::tr!("正在继续处理…"),
            Self::Finishing => c2m_i18n::tr!("正在完成笔记…"),
        }
    }
}

pub fn worker_wait_state<'a>(
    active: bool,
    result_received: bool,
    stages: impl IntoIterator<Item = (&'a Stage, bool)>,
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
    Paused,
    Waiting,
    Failed,
}

impl StageStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => c2m_i18n::tr!("处理中"),
            Self::Complete => c2m_i18n::tr!("已完成"),
            Self::Paused => c2m_i18n::tr!("已暂停"),
            Self::Waiting => c2m_i18n::tr!("等待继续"),
            Self::Failed => c2m_i18n::tr!("未完成"),
        }
    }

    pub fn running(self) -> bool {
        self == Self::Running
    }
}

/// One step of a task, as rendered in a card or stage list.
pub struct TaskStage {
    pub id: String,
    pub title: String,
    pub status: StageStatus,
    pub detail: String,
    pub compact_detail: String,
    pub fraction: Option<f32>,
}

pub(crate) fn stage_progress(
    name: &Stage,
    done: bool,
    live: Option<&activity::Activity>,
    stored: &activity::Activity,
    compact: bool,
) -> (String, Option<f32>) {
    let total = stored.total;
    if name.is_ai() {
        if let Some(live) = live.filter(|_| !done)
            && let Some(detail) = live.ai_detail(compact)
        {
            return (detail, live.fraction());
        }
        return (
            live.map(|live| live.live_counter())
                .or_else(|| stored.tokens_label())
                .unwrap_or_default(),
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
        stored.quantity(name)
    } else {
        String::new()
    };
    let fraction = if done {
        (total > 0).then_some(1.)
    } else {
        live.and_then(|stage| stage.fraction())
    };
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

fn ai_stage_outcome(
    name: &Stage,
    outcomes: Option<&c2m_artifact::artifact::Outcomes>,
) -> Option<StageStatus> {
    let outcome = match name {
        Stage::Llm => &outcomes?.proofreading,
        Stage::Translate => &outcomes?.translation,
        Stage::Summary => &outcomes?.summary,
        _ => return None,
    };
    match outcome.status {
        c2m_artifact::artifact::Status::Succeeded => Some(StageStatus::Complete),
        _ if outcome.is_incomplete() => Some(StageStatus::Failed),
        _ => None,
    }
}

/// Derives stage rows from live `Work` + `task.events` when available, falling back
/// cleanly to durable `task.stages`.
pub fn task_stage_rows(task: &TaskRecord, work: &Work) -> Vec<TaskStage> {
    let active = work.is_running(&task.id) && task.status == TaskStatus::Running;
    task_stage_rows_active(task, active)
}

pub fn task_stage_rows_active(task: &TaskRecord, active: bool) -> Vec<TaskStage> {
    if !task.events.stages.is_empty() {
        return task
            .events
            .stages
            .iter()
            .map(|(name, stored)| {
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
                    task.outcomes.as_ref().or(task.events.outcomes.as_ref())
                };
                let status = if let Some(outcome) = ai_stage_outcome(name, outcomes) {
                    outcome
                } else if done {
                    StageStatus::Complete
                } else if active {
                    StageStatus::Running
                } else {
                    match task.status {
                        TaskStatus::Paused => StageStatus::Paused,
                        TaskStatus::Queued => StageStatus::Waiting,
                        TaskStatus::Running => StageStatus::Running,
                        TaskStatus::Failed => StageStatus::Failed,
                        TaskStatus::Completed => StageStatus::Complete,
                    }
                };
                let (detail, fraction) = stage_progress(name, done, live, stored, false);
                let (compact_detail, _) = stage_progress(name, done, live, stored, true);
                TaskStage {
                    id: format!("task-stage-{}-{name}", task.id),
                    title: activity::title(name),
                    status,
                    detail,
                    compact_detail,
                    fraction,
                }
            })
            .collect();
    }

    task.stages
        .iter()
        .map(|(key, stage)| {
            let status = match stage.phase {
                StagePhase::Completed => StageStatus::Complete,
                StagePhase::Running if active => StageStatus::Running,
                StagePhase::Running if task.status == TaskStatus::Paused => StageStatus::Paused,
                StagePhase::Running => StageStatus::Failed,
                StagePhase::Failed => StageStatus::Failed,
                StagePhase::Pending => match task.status {
                    TaskStatus::Paused => StageStatus::Paused,
                    _ => StageStatus::Waiting,
                },
            };
            let detail = stage.detail.clone().unwrap_or_default();
            TaskStage {
                id: format!("task-stage-{}-{key:?}", task.id),
                title: key.label().into(),
                status,
                compact_detail: detail.clone(),
                detail,
                fraction: (status == StageStatus::Complete).then_some(1.),
            }
        })
        .collect()
}

pub fn task_status_label(task: &TaskRecord) -> String {
    if task.status == TaskStatus::Failed && task.note_dir.is_some() {
        c2m_i18n::tr!("部分未完成").into()
    } else {
        task.status.label().into()
    }
}

const LEGACY_LOGIN_HINT: &str = "提示：请在设置的账号中重新登录 B 站后重试。 / Hint: sign in to Bilibili again in Settings and retry.";

pub fn format_failure_summary(raw: &str, lang: c2m_i18n::i18n::Lang) -> String {
    let summary = raw
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
    let summary = summary
        .strip_prefix(LEGACY_LOGIN_HINT)
        .map_or(summary, |rest| rest.trim_start_matches([':', ' ']));
    if let Some(model) = summary
        .strip_prefix("识别模型 ")
        .and_then(|rest| rest.strip_suffix(" 尚未下载，请先下载并准备模型，再继续"))
    {
        return c2m_i18n::i18n::t_for("识别模型「{}」还没有下载", lang).replacen(
            "{}",
            c2m_config::config::model_label(model),
            1,
        );
    }
    let summary = c2m_i18n::i18n::engine_message_for(summary, lang);
    let summary = summary.as_str();
    if summary.is_empty() {
        c2m_i18n::i18n::t_for("转换未完成", lang).into()
    } else if summary.chars().count() <= 140 {
        summary.into()
    } else {
        format!("{}…", summary.chars().take(139).collect::<String>())
    }
}

pub fn task_error_summary(task: &TaskRecord) -> Option<String> {
    if task.status != TaskStatus::Failed {
        return None;
    }
    let lang = c2m_i18n::i18n::lang();
    if let Some(err) = &task.error {
        if let Some((_, model)) = &err.missing_model {
            return Some(
                c2m_i18n::i18n::t_for("识别模型「{}」还没有下载", lang).replacen(
                    "{}",
                    c2m_config::config::model_label(model),
                    1,
                ),
            );
        }
        return Some(format_failure_summary(&err.message, lang));
    }
    task.events
        .error
        .as_deref()
        .map(|raw| format_failure_summary(raw, lang))
}

pub fn task_attention_summary(task: &TaskRecord) -> String {
    task_error_summary(task).unwrap_or_else(|| task_status_label(task))
}

/// Unfinished parts of a published note: `(component_key, NotePart, Outcome)`.
pub fn task_component_failures(
    task: &TaskRecord,
) -> Vec<(String, activity::NotePart, c2m_artifact::artifact::Outcome)> {
    let Some(outcomes) = task.outcomes.as_ref().or(task.events.outcomes.as_ref()) else {
        return Vec::new();
    };
    use activity::NotePart;
    let mut results: Vec<_> = [
        (
            Component::Screenshots,
            NotePart::Screenshots,
            &outcomes.screenshots,
        ),
        (
            Component::Proofreading,
            NotePart::Proofreading,
            &outcomes.proofreading,
        ),
        (
            Component::Translation,
            NotePart::Translation,
            &outcomes.translation,
        ),
        (Component::Summary, NotePart::Summary, &outcomes.summary),
    ]
    .into_iter()
    .filter(|(.., outcome)| outcome.is_incomplete())
    .map(|(component, part, outcome)| (component.as_str().into(), part, outcome.clone()))
    .collect();
    let exports: Vec<_> = outcomes
        .exports
        .values()
        .filter(|o| o.is_incomplete())
        .collect();
    if !exports.is_empty() {
        let messages: Vec<_> = exports.iter().filter_map(|o| o.message.clone()).collect();
        results.push((
            Component::Exports.as_str().into(),
            NotePart::Exports,
            c2m_artifact::artifact::Outcome::failed(messages.join(c2m_i18n::tr!("；"))),
        ));
    }
    results
}

pub fn task_note_failures(
    task: &TaskRecord,
) -> Vec<(String, activity::NotePart, c2m_artifact::artifact::Outcome)> {
    if task.note_dir.is_some() {
        task_component_failures(task)
    } else {
        Vec::new()
    }
}

/// Actionable failure lines shown on an attention card.
pub fn task_reasons(task: &TaskRecord) -> Vec<String> {
    if task.status != TaskStatus::Failed {
        return Vec::new();
    }
    let failures = task_note_failures(task);
    if !failures.is_empty() {
        let items: Vec<_> = failures
            .iter()
            .map(|(_, part, outcome)| (*part, outcome))
            .collect();
        return activity::join_outcome_failure_messages(&items)
            .into_iter()
            .collect();
    }
    task_error_summary(task).into_iter().collect()
}

/// Component key -> retry button label.
pub fn retry_component_label(component: &str) -> &'static str {
    match Component::parse(component) {
        Some(Component::Screenshots) => c2m_i18n::tr!("重新生成截图"),
        Some(Component::Proofreading) => c2m_i18n::tr!("重试 AI 校对"),
        Some(Component::Translation) => c2m_i18n::tr!("重试 AI 翻译"),
        Some(Component::Summary) => c2m_i18n::tr!("重新生成摘要"),
        Some(Component::Exports) => c2m_i18n::tr!("重新导出"),
        None => c2m_i18n::tr!("补全未完成部分"),
    }
}

pub fn retry_components_label(components: &[String]) -> &'static str {
    if components.len() == 1 {
        retry_component_label(&components[0])
    } else {
        c2m_i18n::tr!("补全未完成部分")
    }
}

/// Button label for `TaskAction::Retry` on `task`.
pub fn task_retry_action_label(task: &TaskRecord) -> &'static str {
    let failures = task_note_failures(task);
    match failures.as_slice() {
        [(key, _, outcome)] if outcome.status == c2m_artifact::artifact::Status::Partial => {
            match Component::parse(key) {
                Some(Component::Proofreading) => c2m_i18n::tr!("重试未完成的校对部分"),
                Some(Component::Translation) => c2m_i18n::tr!("重试未完成的翻译部分"),
                _ => retry_component_label(key),
            }
        }
        [(key, _, _)] => retry_component_label(key),
        [_, _, ..] => c2m_i18n::tr!("补全未完成部分"),
        [] => c2m_i18n::tr!("重试"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskSurface {
    Menu,
    Card,
    Home,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskAction {
    Pause,
    Resume,
    Retry,
    Regenerate,
    PrepareModel,
    OpenSettings,
    OpenNote,
    Cancel,
    Remove,
    OpenNoteFolder,
    OpenExports,
    OpenSource,
    CopyLink,
    Details,
    Log,
    Move {
        label: &'static str,
        up: bool,
        to: Option<usize>,
    },
    Reassociate(String),
    ViewTask,
}

impl TaskAction {
    pub fn group(&self) -> u8 {
        match self {
            Self::OpenNote
            | Self::OpenNoteFolder
            | Self::OpenExports
            | Self::OpenSource
            | Self::CopyLink => 0,
            Self::Details | Self::Log | Self::ViewTask => 1,
            Self::Move { .. } => 2,
            Self::Cancel | Self::Remove => 4,
            _ => 3,
        }
    }
}

pub struct TaskFacts<'a> {
    pub active: bool,
    pub expanded: bool,
    pub library: Option<&'a workspace::LibraryLocation>,
    pub needs_reassociation: bool,
    pub queue: Vec<&'a str>,
}

pub fn lead_action<'a>(
    actions: &'a [TaskAction],
    task: &TaskRecord,
    surface: TaskSurface,
) -> Option<&'a TaskAction> {
    if surface == TaskSurface::Home
        && task.note_dir.is_some()
        && let Some(open) = actions.iter().find(|a| **a == TaskAction::OpenNote)
    {
        return Some(open);
    }
    actions.iter().find(|action| {
        matches!(
            action,
            TaskAction::PrepareModel
                | TaskAction::Resume
                | TaskAction::Retry
                | TaskAction::OpenNote
        )
    })
}

pub fn runs_in_card(task: &TaskRecord) -> bool {
    matches!(task.status, TaskStatus::Running | TaskStatus::Queued)
}

pub fn task_actions(
    task: &TaskRecord,
    surface: TaskSurface,
    facts: &TaskFacts<'_>,
) -> Vec<TaskAction> {
    use TaskAction::*;
    let has_note = task.note_dir.is_some();
    let has_log = task_log_text(task).is_some();
    let missing_model = task
        .error
        .as_ref()
        .is_some_and(|err| err.missing_model.is_some());
    let needs_settings = task.error.as_ref().is_some_and(|err| err.needs_settings);

    let mut recovery = Vec::new();
    match task.status {
        TaskStatus::Paused => recovery.push(Resume),
        TaskStatus::Failed => {
            if missing_model {
                recovery.push(PrepareModel);
            }
            if let Some(library) = facts.library.filter(|_| facts.needs_reassociation) {
                recovery.push(Reassociate(library.id.clone()));
            }
            recovery.push(Retry);
            if needs_settings {
                recovery.push(OpenSettings);
            }
        }
        _ => {}
    }

    let mut actions = Vec::new();
    match surface {
        TaskSurface::Menu => {
            if has_note {
                actions.extend([OpenNote, OpenNoteFolder]);
            }
            if facts
                .library
                .is_some_and(|location| !workspace::task_export_files(task, location).is_empty())
            {
                actions.push(OpenExports);
            }
            if task.source.online {
                actions.extend([OpenSource, CopyLink]);
            }
            if task.status != TaskStatus::Running {
                actions.push(Details);
            }
            if has_log {
                actions.push(Log);
            }
            actions.extend(task_moves(task, &facts.queue));
            if matches!(task.status, TaskStatus::Running | TaskStatus::Queued) {
                actions.push(Pause);
            }
            actions.extend(recovery);
            if task.status != TaskStatus::Running && task.status != TaskStatus::Queued && has_note {
                actions.push(Regenerate);
            }
            if matches!(
                task.status,
                TaskStatus::Running | TaskStatus::Queued | TaskStatus::Paused
            ) {
                actions.push(Cancel);
            } else if !facts.active {
                actions.push(Remove);
            }
        }
        TaskSurface::Card => match task.status {
            TaskStatus::Running => {
                if has_log {
                    actions.push(Log);
                }
                actions.push(Pause);
                actions.push(Cancel);
            }
            TaskStatus::Queued => {
                actions.push(Pause);
                actions.push(Cancel);
            }
            TaskStatus::Paused => {
                actions.extend(recovery);
                actions.push(Cancel);
                if has_note {
                    actions.push(OpenNote);
                }
                actions.push(Details);
                if has_log && facts.expanded {
                    actions.push(Log);
                }
            }
            TaskStatus::Failed | TaskStatus::Completed => {
                actions.extend(recovery);
                if has_note {
                    actions.push(OpenNote);
                }
                actions.push(Details);
                if has_log && facts.expanded {
                    actions.push(Log);
                }
            }
        },
        TaskSurface::Home => {
            match task.status {
                TaskStatus::Running | TaskStatus::Queued => {
                    actions.push(Pause);
                    actions.push(Cancel);
                }
                TaskStatus::Paused => {
                    if has_note {
                        actions.push(OpenNote);
                    }
                    actions.push(Resume);
                    actions.push(Cancel);
                }
                TaskStatus::Failed => {
                    if has_note {
                        actions.push(OpenNote);
                    }
                    actions.extend(recovery);
                }
                TaskStatus::Completed => {
                    if has_note {
                        actions.push(OpenNote);
                    }
                    actions.push(Regenerate);
                }
            }
            actions.push(ViewTask);
        }
    }
    actions
}

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
        step(c2m_i18n::tr!("移到最前"), true, 0, position > 0),
        step(
            c2m_i18n::tr!("上移"),
            true,
            position.saturating_sub(1),
            position > 0,
        ),
        step(c2m_i18n::tr!("下移"), false, position + 1, position < last),
        step(c2m_i18n::tr!("移到最后"), false, last, position < last),
    ]
}

pub fn task_kept_edits(task: &TaskRecord) -> Option<String> {
    task.note_dir.as_ref().and_then(|_| {
        let edits = if !task.kept_edits.is_empty() {
            &task.kept_edits
        } else {
            &task.events.kept_edits
        };
        crate::run::kept_edits_notice(edits)
    })
}

pub fn task_log_text(task: &TaskRecord) -> Option<String> {
    let logs = if !task.logs.is_empty() {
        &task.logs
    } else {
        &task.events.logs
    };
    let text = logs
        .iter()
        .chain(task.error.iter().map(|e| &e.message))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

/// Resolves the speech model `task` would use right now under current `Preferences`.
pub fn task_model_request(
    preferences: &desktop_prefs::preferences::Store,
    task: &TaskRecord,
    fallback: c2m_config::config::AsrProvider,
) -> desktop_prefs::model_status::Request {
    if let Some((provider, model)) = task.error.as_ref().and_then(|e| e.missing_model.as_ref()) {
        let config = preferences.defaults_config();
        return desktop_prefs::model_status::Request::new(
            *provider,
            Some(model.as_str()),
            &c2m_core::paths::model_dir_from(config.defaults.model_dir.as_deref()),
        );
    }
    let config = crate::run::current_config_for(
        preferences,
        &task.source,
        task.subtitle.as_deref(),
        fallback,
    );
    let defaults = &config.defaults;
    desktop_prefs::model_status::Request::new(
        defaults.provider.unwrap_or(fallback),
        defaults.asr_model.as_deref(),
        &c2m_core::paths::model_dir_from(defaults.model_dir.as_deref()),
    )
}

/// Failed tasks waiting on `request`'s local speech model so they auto-retry once prepared.
pub fn tasks_awaiting_model(
    preferences: &desktop_prefs::preferences::Store,
    tasks: &[TaskRecord],
    request: &desktop_prefs::model_status::Request,
    fallback: c2m_config::config::AsrProvider,
) -> Vec<String> {
    tasks
        .iter()
        .filter(|task| {
            task.status == TaskStatus::Failed
                && task
                    .error
                    .as_ref()
                    .is_some_and(|err| err.missing_model.is_some())
                && task_model_request(preferences, task, fallback) == *request
        })
        .map(|task| task.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use c2m_config::config::AsrProvider;
    use desktop_store::workspace::{RunMode, StageKey, StageState, TaskError, course_dir_name};

    fn sample_task(status: TaskStatus) -> TaskRecord {
        TaskRecord {
            id: "task-1".into(),
            library_id: "lib-1".into(),
            course_id: course_dir_name("video-1"),
            source_id: "video-1".into(),
            source: crate::plan::tests::source("https://example.test/v1", "课程"),
            title: "课程".into(),
            title_from_probe: false,
            author: String::new(),
            duration: 60.,
            subtitle: None,
            subtitle_events: None,
            target_folder: None,
            note_dir: None,
            work_dir: "/tmp/work".into(),
            legacy_work_dir: None,
            status,
            run_mode: RunMode::Resume,
            stages: Default::default(),
            error: None,
            logs: Vec::new(),
            unread: false,
            created: 0,
            updated: 0,
            outcomes: None,
            kept_video: false,
            kept_edits: Vec::new(),
            events: Default::default(),
        }
    }

    #[test]
    fn groups_and_actions_cover_all_five_states_without_history_chains() {
        assert_eq!(
            task_group(&sample_task(TaskStatus::Running)),
            TaskGroup::Running
        );
        assert_eq!(
            task_group(&sample_task(TaskStatus::Failed)),
            TaskGroup::Attention
        );
        assert_eq!(
            task_group(&sample_task(TaskStatus::Queued)),
            TaskGroup::Queued
        );
        assert_eq!(
            task_group(&sample_task(TaskStatus::Paused)),
            TaskGroup::Queued
        );
        assert_eq!(
            task_group(&sample_task(TaskStatus::Completed)),
            TaskGroup::Finished
        );

        let facts = TaskFacts {
            active: false,
            expanded: false,
            library: None,
            needs_reassociation: false,
            queue: Vec::new(),
        };

        // Missing local model -> PrepareModel + Retry.
        let mut model_task = sample_task(TaskStatus::Failed);
        model_task.error = Some(TaskError {
            message: "模型未下载".into(),
            failed_stages: Vec::new(),
            missing_model: Some((AsrProvider::Cpu, "qwen3-0.6b".into())),
            needs_settings: false,
        });
        let actions = task_actions(&model_task, TaskSurface::Card, &facts);
        assert!(actions.contains(&TaskAction::PrepareModel));
        assert!(actions.contains(&TaskAction::Retry));
        assert_eq!(
            lead_action(&actions, &model_task, TaskSurface::Card),
            Some(&TaskAction::PrepareModel)
        );

        // Missing/invalid AI service -> Retry + OpenSettings.
        let mut settings_task = sample_task(TaskStatus::Failed);
        settings_task.note_dir = Some("/tmp/note".into());
        settings_task.error = Some(TaskError {
            message: "HTTP 401".into(),
            failed_stages: vec![Component::Summary],
            missing_model: None,
            needs_settings: true,
        });
        let card_actions = task_actions(&settings_task, TaskSurface::Card, &facts);
        assert!(card_actions.contains(&TaskAction::Retry));
        assert!(card_actions.contains(&TaskAction::OpenSettings));
        assert!(card_actions.contains(&TaskAction::OpenNote));
        assert_eq!(task_status_label(&settings_task), "部分未完成");

        // Completed task offers OpenNote on Card and Regenerate on Home/Menu.
        let mut done_task = sample_task(TaskStatus::Completed);
        done_task.note_dir = Some("/tmp/note".into());
        let home_actions = task_actions(&done_task, TaskSurface::Home, &facts);
        assert!(home_actions.contains(&TaskAction::OpenNote));
        assert!(home_actions.contains(&TaskAction::Regenerate));
    }

    #[test]
    fn stage_rows_derive_from_persisted_stages_when_live_events_are_absent() {
        let mut task = sample_task(TaskStatus::Failed);
        task.stages.insert(
            StageKey::Transcription,
            StageState {
                phase: StagePhase::Completed,
                detail: None,
            },
        );
        task.stages.insert(
            StageKey::Summary,
            StageState {
                phase: StagePhase::Failed,
                detail: Some("摘要未生成：服务暂时无法完成请求".into()),
            },
        );
        let work = Work::new();
        let rows = task_stage_rows(&task, &work);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, StageStatus::Complete);
        assert_eq!(rows[1].status, StageStatus::Failed);
        assert!(rows[1].detail.contains("摘要未生成"));
    }
}
