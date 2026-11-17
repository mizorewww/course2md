//! Running a queued course task: resolving its `execution::Request` from the current
//! `Preferences`, lazily migrating any `legacy_work_dir`, writing `control.json`, and
//! recording preflight or execution outcomes in the 5-state model.
use crate::{backend::Done, source};
use anyhow::{Context as _, Result};
use c2m_artifact::artifact::{self, FailureKind, Manifest, Outcomes};
use c2m_asr::models::status::{CacheState, inspect as inspect_local_model};
use c2m_config::{
    config::{AsrProvider, OutputFormat, TranscriptSource},
    settings::ConfigFile,
};
use c2m_core::speech::{ModelRequired, Remedy};
use c2m_engine::execution::{Component, Operation, Request, RunMode, valid_id};
use c2m_protocol::progress::ErrorCode;
use desktop_prefs::{conversion::ConversionOptions, preferences};
use desktop_store::{
    activity::NotePart,
    organize,
    workspace::{
        self, ClearedTaskWork, Control, Intent, LibraryLocation, StagePhase, Task, TaskError,
        TaskPlan, TaskRecord, TaskStatus,
    },
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Effective transcript source for `source` under Settings' `mode`.
pub fn text_source(
    source: &workspace::Source,
    subtitle: Option<&Path>,
    mode: TranscriptSource,
) -> TranscriptSource {
    use c2m_source::subtitle::SubtitleEvidence;
    if mode == TranscriptSource::Asr {
        TranscriptSource::Asr
    } else if source.selected_subtitle.is_some() || subtitle.is_some() {
        TranscriptSource::Subtitle
    } else if mode == TranscriptSource::Auto
        && matches!(
            source.subtitles,
            SubtitleEvidence::NoneFound
                | SubtitleEvidence::Unsupported { .. }
                | SubtitleEvidence::Failed { .. }
        )
    {
        TranscriptSource::Asr
    } else {
        mode
    }
}

/// Resolves the non-secret `ConfigFile` for a source from current `Preferences`.
pub fn current_config_for(
    preferences: &preferences::Store,
    source: &workspace::Source,
    subtitle: Option<&Path>,
    fallback: AsrProvider,
) -> ConfigFile {
    let mut config = preferences.defaults_config();
    ConversionOptions::from_config(&config).apply_to(&mut config);
    config.defaults.model_dir = Some(c2m_config::config::model_dir_from(
        config.defaults.model_dir.as_deref(),
    ));
    let mode = config.defaults.transcript_source.unwrap_or_default();
    config.defaults.transcript_source = Some(text_source(source, subtitle, mode));
    if config.defaults.transcript_source != Some(TranscriptSource::Subtitle) {
        let provider = config.defaults.provider.unwrap_or(fallback);
        config.defaults.provider = Some(provider);
        crate::plan::fill_default_asr_model(&mut config, provider);
    }
    config
}

/// Resolves the non-secret `ConfigFile` for a planned submission (`NewTask`).
pub fn current_config(
    preferences: &preferences::Store,
    plan: &TaskPlan,
    fallback: AsrProvider,
) -> ConfigFile {
    current_config_for(
        preferences,
        &plan.source,
        plan.subtitle.as_deref(),
        fallback,
    )
}

/// Adjusts `config` when an in-place `Resume` run only reprocesses incomplete optional
/// components of an already-published note.
fn apply_reprocess_scope(
    config: &mut ConfigFile,
    components: &[Component],
    outcomes: Option<&Outcomes>,
) {
    config.defaults.transcript_source = Some(TranscriptSource::Subtitle);
    config.llm.enabled = components.contains(&Component::Proofreading);
    config.llm.summarize = components.contains(&Component::Summary);
    config.llm.vision &= config.llm.enabled;
    if !components.contains(&Component::Translation)
        && !components.contains(&Component::Proofreading)
    {
        config.llm.translate = None;
    }
    if components.contains(&Component::Exports)
        && let Some(outcomes) = outcomes
    {
        let mut formats = config.defaults.formats.clone().unwrap_or_default();
        for format in [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json] {
            if outcomes
                .exports
                .get(&format.to_string())
                .is_some_and(|o| o.is_incomplete())
                && !formats.contains(&format)
            {
                formats.push(format);
            }
        }
        config.defaults.formats = Some(formats);
    }
}

/// Base `ConfigFile` for `task` under current `Preferences`, accounting for known
/// in-place reprocess scope when `task.run_mode == RunMode::Resume`.
pub fn task_base_config(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> ConfigFile {
    let mut config = current_config_for(
        preferences,
        &task.source,
        task.subtitle.as_deref(),
        fallback,
    );
    if task.run_mode == RunMode::Resume && task.note_dir.is_some() {
        let incomplete = task.incomplete_components();
        if !incomplete.is_empty() {
            apply_reprocess_scope(&mut config, &incomplete, task.outcomes.as_ref());
        }
    }
    config
}

/// Credential references needed from the keychain before starting `task`.
pub fn needed_credentials(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> Result<Vec<String>> {
    let base = task_base_config(preferences, task, fallback);
    let refs = preferences.default_refs();
    preferences.credential_references_for(&base, &refs)
}

/// Builds the initial `execution::Request` from current `Preferences` and `task`.
pub fn execution_request(
    preferences: &preferences::Store,
    state: &workspace::State,
    task: &TaskRecord,
    secrets: &HashMap<String, String>,
    fallback: AsrProvider,
) -> Result<(Request, bool)> {
    let location = state
        .library(&task.library_id)
        .context(c2m_i18n::tr!("任务保存位置暂时不可用"))?;
    let base = task_base_config(preferences, task, fallback);
    let refs = preferences.default_refs();
    let config = preferences
        .resolve_for_execution_with_secrets(&base, &refs, secrets)?
        .into_config();
    let keep_video = config.defaults.keep_video.unwrap_or(false);
    let course_dir = workspace::note_dir_for(&location.root, &task.course_id);
    let work_dir = workspace::work_dir_for(&location.root, &task.course_id);

    let operation = if task.run_mode == RunMode::Resume && task.note_dir.is_some() {
        let incomplete = task.incomplete_components();
        if incomplete.is_empty() {
            Operation::Generate
        } else {
            Operation::Reprocess {
                note_dir: course_dir.clone(),
                base_task_id: task.id.clone(),
                components: incomplete,
                prior_work_dir: None,
            }
        }
    } else {
        Operation::Generate
    };

    let service_versions = [("asr", refs.asr.clone()), ("llm", refs.llm)]
        .into_iter()
        .filter_map(|(purpose, reference)| reference.map(|r| (purpose.to_owned(), r)))
        .collect();
    let source = state
        .reader_sources
        .get(&task.source_id)
        .filter(|path| path.is_file())
        .map(|path| path.to_string_y_or_display())
        .filter(|_| !task.source.online && !Path::new(&task.source.input).is_file())
        .unwrap_or_else(|| task.source.input.clone());
    let subtitle = (config.defaults.transcript_source != Some(TranscriptSource::Asr))
        .then(|| task.subtitle.clone())
        .flatten();

    Ok((
        Request {
            schema: c2m_engine::execution::REQUEST_SCHEMA,
            operation,
            run_mode: task.run_mode,
            task_id: task.id.clone(),
            version_id: None,
            course_id: task.course_id.clone(),
            course_dir,
            source,
            source_id: task.source_id.clone(),
            title: task.title.clone(),
            title_from_probe: task.title_from_probe,
            author: task.author.clone(),
            duration: task.duration,
            subtitle,
            subtitle_events: None,
            config,
            allow_unauthenticated_asr: refs
                .asr
                .as_deref()
                .and_then(|id| preferences.version(id))
                .is_some_and(|version| {
                    version.config.authentication == preferences::Authentication::None
                }),
            work_dir: work_dir.clone(),
            control_path: Some(work_dir.join("control.json")),
            service_versions,
        },
        keep_video,
    ))
}

trait PathToString {
    fn to_string_y_or_display(&self) -> String;
}

impl PathToString for Path {
    fn to_string_y_or_display(&self) -> String {
        self.to_string_lossy().into_owned()
    }
}

/// Serializes a simple `Control { intent }` payload (`control.json`).
pub fn control_bytes(intent: Intent) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Control { intent })?)
}

/// Writes `Control { intent }` atomically to `<work_dir>/control.json`. Blocking.
pub fn write_control(work_dir: &Path, intent: Intent) -> Result<()> {
    std::fs::create_dir_all(work_dir)?;
    c2m_core::fsio::atomic_write(&work_dir.join("control.json"), &control_bytes(intent)?)
}

/// Everything a task start needs before entering the blocking preflight thread.
pub struct StartRequest {
    pub request: Request,
    pub keep_video: bool,
    location: LibraryLocation,
}

pub fn start_request(
    preferences: &preferences::Store,
    workspace: Option<&workspace::Workspace>,
    task: &TaskRecord,
    secrets: &HashMap<String, String>,
    fallback: AsrProvider,
) -> Result<StartRequest> {
    let workspace = workspace.context(c2m_i18n::tr!("任务记录不可用"))?;
    let (request, keep_video) =
        execution_request(preferences, &workspace.state, task, secrets, fallback)?;
    let location = workspace
        .state
        .library(&task.library_id)
        .cloned()
        .context(c2m_i18n::tr!("任务保存位置尚未恢复"))?;
    Ok(StartRequest {
        request,
        keep_video,
        location,
    })
}

pub struct PreparedStart {
    pub request: Request,
    pub keep_video: bool,
    pub adopted_legacy_work_dir: bool,
}

impl StartRequest {
    /// Blocking preflight: checks library marker, lazily adopts `legacy_work_dir` -> `work_dir`,
    /// inspects `note_dir/manifest.json` for in-place `Reprocess` vs `Generate`, checks local
    /// speech model readiness, writes subtitles + `control.json`, and validates `Request`.
    pub fn prepare(self, task: &TaskRecord) -> Result<PreparedStart> {
        let Self {
            mut request,
            keep_video,
            location,
        } = self;
        workspace::check_library(&location)?;
        artifact::refuse_newer(&request.course_dir)?;

        let adopted_legacy_work_dir = adopt_legacy_work_dir(
            task.legacy_work_dir.as_deref(),
            &request.work_dir,
            &task.course_id,
            &task.source_id,
        )?;

        // Check `manifest.json` on disk so `RunMode::Resume` repairs incomplete optional
        // stages in place even if `task.outcomes` was not hydrated yet.
        if request.run_mode == RunMode::Resume {
            if let Ok(Some(manifest)) = artifact::note_manifest(&request.course_dir)
                && manifest.course_id == request.course_id
                && manifest.source_id == request.source_id
            {
                let incomplete = workspace::incomplete_components_from_outcomes(&manifest.outcomes);
                if !incomplete.is_empty() {
                    let base_task_id = if valid_id(&manifest.task_id) {
                        manifest.task_id.clone()
                    } else {
                        request.task_id.clone()
                    };
                    apply_reprocess_scope(
                        &mut request.config,
                        &incomplete,
                        Some(&manifest.outcomes),
                    );
                    request.subtitle = None;
                    request.operation = Operation::Reprocess {
                        note_dir: request.course_dir.clone(),
                        base_task_id,
                        components: incomplete,
                        prior_work_dir: None,
                    };
                } else {
                    request.operation = Operation::Generate;
                }
            } else {
                request.operation = Operation::Generate;
            }
        } else {
            request.operation = Operation::Generate;
        }

        // Preflight local speech model check when speech recognition is definitely required.
        if matches!(request.operation, Operation::Generate)
            && request.config.defaults.transcript_source == Some(TranscriptSource::Asr)
            && let Some(provider) = request.config.defaults.provider
            && provider != AsrProvider::Api
        {
            let model = c2m_config::config::effective_model(
                provider,
                request.config.defaults.asr_model.as_deref(),
            )
            .to_owned();
            let model_root =
                c2m_config::config::model_dir_from(request.config.defaults.model_dir.as_deref());
            if let Ok(status) = inspect_local_model(provider, &model, &model_root)
                && !matches!(status.state, CacheState::Cached | CacheState::Loaded)
            {
                return Err(ModelRequired {
                    provider,
                    model,
                    remedy: Remedy::Prepare,
                }
                .into());
            }
        }

        if matches!(request.operation, Operation::Generate)
            && request.config.defaults.transcript_source != Some(TranscriptSource::Asr)
            && request.subtitle.is_none()
        {
            request.subtitle_events = workspace::task_subtitle_events(task)?;
        }

        let mut stored_task = task.clone();
        stored_task.work_dir = request.work_dir.clone();
        workspace::store_task_subtitles(&stored_task)?;
        write_control(&request.work_dir, Intent::Run)?;
        request.validate()?;

        Ok(PreparedStart {
            request,
            keep_video,
            adopted_legacy_work_dir,
        })
    }
}

/// Lazily renames `<library>/.course2md/work/<old_task_id>` to `<library>/.course2md/work/<course_id>`,
/// preserving cached `media.mp4`, `asr.jsonl`, `screenshots.json`, and `requests/*.json`.
pub fn adopt_legacy_work_dir(
    legacy_work_dir: Option<&Path>,
    work_dir: &Path,
    course_id: &str,
    source_id: &str,
) -> Result<bool> {
    let Some(legacy) = legacy_work_dir.filter(|legacy| *legacy != work_dir) else {
        return Ok(false);
    };
    if !legacy.is_dir() || work_dir.exists() {
        return Ok(false);
    }
    if let Some(parent) = work_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(legacy, work_dir)?;

    // Backfill `course_id` on pre-3.0 `task-identity.json` records that only stored `source_id`.
    let identity_path = work_dir.join("task-identity.json");
    if let Ok(bytes) = std::fs::read(&identity_path)
        && let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && value.get("course_id").is_none()
        && value.get("source_id").and_then(|v| v.as_str()) == Some(source_id)
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert(
            "course_id".into(),
            serde_json::Value::String(course_id.to_owned()),
        );
        if let Ok(updated) = serde_json::to_vec_pretty(&value) {
            let _ = c2m_core::fsio::atomic_write(&identity_path, &updated);
        }
    }
    Ok(true)
}

pub fn still_runnable(state: &workspace::State, id: &str) -> bool {
    state
        .task(id)
        .is_some_and(|record| record.status == TaskStatus::Queued)
}

/// Marks a queued task `Running` just before spawning the worker.
pub fn mark_running(
    state: &mut workspace::State,
    id: &str,
    adopted_legacy_work_dir: bool,
) -> Result<()> {
    let record = state.task_mut(id).context(c2m_i18n::tr!("任务不存在"))?;
    record.status = TaskStatus::Running;
    record.error = None;
    if adopted_legacy_work_dir {
        record.legacy_work_dir = None;
    }
    record.events.begin_run();
    workspace::forget_task_subtitles(record);
    record.updated = workspace::now();
    Ok(())
}

/// Classifies an `anyhow::Error` from preflight (`start_request` / `prepare`) into a `TaskError`.
pub fn classify_preflight_error(error: &anyhow::Error) -> TaskError {
    if let Some(mr) = error.downcast_ref::<ModelRequired>() {
        let label = c2m_config::config::model_label(&mr.model);
        return TaskError {
            message: c2m_i18n::tr!("识别模型「{}」还没有下载", label),
            failed_stages: Vec::new(),
            missing_model: Some((mr.provider, mr.model.clone())),
            needs_settings: false,
        };
    }
    let code = ErrorCode::of(error);
    let message = format!("{error:#}");
    let needs_settings = matches!(
        code,
        ErrorCode::InvalidConfiguration
            | ErrorCode::MissingTool
            | ErrorCode::Authentication
            | ErrorCode::Model
            | ErrorCode::Parameters
    ) || message_indicates_settings(&message);
    TaskError {
        message,
        failed_stages: Vec::new(),
        missing_model: None,
        needs_settings,
    }
}

pub fn message_indicates_settings(message: &str) -> bool {
    source::legacy_login_failure_from_text(message)
        || [
            "设置中还没有可用的",
            "服务已从设置中删除",
            "此服务已删除",
            "服务凭据尚未就绪",
            "不支持语音识别",
            "不支持 AI 校对",
            "API Key",
            "HTTP 401",
            "HTTP 403",
            "HTTP 404",
            "需要选择翻译语言",
            "Codex",
        ]
        .iter()
        .any(|needle| message.contains(needle))
}

/// Marks a task `Failed` when preflight failed before a worker process could start.
pub fn fail_before_start(
    workspace: &mut workspace::Workspace,
    id: &str,
    error: &anyhow::Error,
) -> Result<()> {
    let task_error = classify_preflight_error(error);
    let mark = |record: &mut Task| {
        record.status = TaskStatus::Failed;
        record.error = Some(task_error.clone());
        record.unread = true;
        record.updated = workspace::now();
    };
    let saved = workspace.transaction(|state| {
        mark(state.task_mut(id).context(c2m_i18n::tr!("任务不存在"))?);
        Ok(())
    });
    if saved.is_err()
        && let Some(record) = workspace.state.task_mut(id)
    {
        mark(record);
    }
    saved
}

fn outcomes_need_settings(outcomes: &Outcomes) -> bool {
    [
        &outcomes.transcript,
        &outcomes.screenshots,
        &outcomes.proofreading,
        &outcomes.translation,
        &outcomes.summary,
    ]
    .into_iter()
    .chain(outcomes.exports.values())
    .filter(|outcome| outcome.is_incomplete())
    .any(|outcome| {
        matches!(
            outcome.kind,
            Some(FailureKind::Authentication | FailureKind::Model | FailureKind::Parameters)
        ) || outcome
            .message
            .as_deref()
            .is_some_and(message_indicates_settings)
    })
}

fn build_partial_failure_message(outcomes: &Outcomes) -> String {
    let mut messages = Vec::new();
    for (part, outcome) in [
        (NotePart::Screenshots, &outcomes.screenshots),
        (NotePart::Proofreading, &outcomes.proofreading),
        (NotePart::Translation, &outcomes.translation),
        (NotePart::Summary, &outcomes.summary),
    ] {
        if outcome.is_incomplete() {
            messages.push(desktop_store::activity::outcome_failure_message(
                part, outcome,
            ));
        }
    }
    for outcome in outcomes.exports.values() {
        if outcome.is_incomplete() {
            messages.push(desktop_store::activity::outcome_failure_message(
                NotePart::Exports,
                outcome,
            ));
        }
    }
    if messages.is_empty() {
        c2m_i18n::tr!("部分未完成").into()
    } else {
        messages.join(c2m_i18n::tr!("；"))
    }
}

/// Settles a task's state when its worker process exits.
/// Returns `(settled_task, optional_work_dir_cleanup)`.
#[allow(clippy::too_many_arguments)]
pub fn record_finish(
    state: &mut workspace::State,
    preferences: &preferences::Store,
    fallback: AsrProvider,
    id: &str,
    done: Option<&Done>,
    manifest: Option<&Manifest>,
    keep_video: bool,
    visible: bool,
    success: bool,
    cancelled: bool,
) -> Result<(TaskRecord, Option<ClearedTaskWork>)> {
    let task = state
        .task_mut(id)
        .context(c2m_i18n::tr!("任务记录不存在"))?;
    task.updated = workspace::now();

    // Case 1: A note was published or exports updated on an existing note.
    if let (Some(done), Some(manifest)) = (done, manifest) {
        task.unread = !visible;
        task.note_dir = Some(done.out_dir.clone());
        if task.title_from_probe && !manifest.title.trim().is_empty() {
            task.title = manifest.title.clone();
            task.source.title = manifest.title.clone();
            task.title_from_probe = false;
        }
        if !done.kept_edits.is_empty() {
            task.kept_edits.clone_from(&done.kept_edits);
        }

        let mut outcomes = manifest.outcomes.clone();
        if done.operation.as_deref() == Some("exports")
            && let Some(export_done) = &done.outcomes
        {
            outcomes.exports = export_done.exports.clone();
        } else if let Some(done_outcomes) = &done.outcomes {
            outcomes = done_outcomes.clone();
        }

        let failed_stages = workspace::incomplete_components_from_outcomes(&outcomes);
        let partial = done.partial.unwrap_or(manifest.partial) || !failed_stages.is_empty();
        task.outcomes = Some(outcomes.clone());
        task.sync_stages_from_outcomes(&outcomes, partial);
        task.kept_video = keep_video && task.source.online;

        let cleanup = if partial {
            task.status = TaskStatus::Failed;
            task.run_mode = RunMode::Resume;
            task.error = Some(TaskError {
                message: build_partial_failure_message(&outcomes),
                failed_stages,
                missing_model: None,
                needs_settings: outcomes_need_settings(&outcomes),
            });
            None
        } else {
            task.status = TaskStatus::Completed;
            task.run_mode = RunMode::Resume;
            task.error = None;
            task.trim_finished_logs();
            Some(ClearedTaskWork::from_task(task, true))
        };

        let record = task.clone();
        if record.status == TaskStatus::Completed && state.selected_task.as_deref() == Some(id) {
            state.selected_task = None;
        }
        return Ok((record, cleanup));
    }

    // Case 2: Cooperative pause or immediate re-queue while the worker was winding down.
    if matches!(task.status, TaskStatus::Paused | TaskStatus::Queued) {
        for stage in task.stages.values_mut() {
            if stage.phase == StagePhase::Running {
                stage.phase = StagePhase::Pending;
            }
        }
        return Ok((task.clone(), None));
    }

    // Case 3: Worker exited without publishing a note -> `Failed`.
    task.unread = !visible;
    task.status = TaskStatus::Failed;
    for stage in task.stages.values_mut() {
        if stage.phase == StagePhase::Running {
            stage.phase = StagePhase::Failed;
        }
    }

    let raw_message = task.events.error.clone().unwrap_or_else(|| {
        if cancelled {
            c2m_i18n::tr!("已停止").into()
        } else if success {
            c2m_i18n::tr!("未生成可读笔记").into()
        } else {
            c2m_i18n::tr!("生成中断").into()
        }
    });

    let missing_model = if task.events.model_required {
        let model_req = crate::task_view::task_model_request(preferences, task, fallback);
        Some((model_req.provider, model_req.model))
    } else {
        None
    };

    let needs_settings = missing_model.is_none()
        && (matches!(
            task.events.error_code,
            Some(
                ErrorCode::InvalidConfiguration
                    | ErrorCode::MissingTool
                    | ErrorCode::Authentication
                    | ErrorCode::Model
                    | ErrorCode::Parameters
            )
        ) || task.events.blocked.iter().any(|b| {
            matches!(
                b.kind,
                Some(FailureKind::Authentication | FailureKind::Model | FailureKind::Parameters)
            )
        }) || message_indicates_settings(&raw_message));

    task.error = Some(TaskError {
        message: raw_message,
        failed_stages: Vec::new(),
        missing_model,
        needs_settings,
    });

    Ok((task.clone(), None))
}

/// Post-publication filing (saving cover, assigning target folder, resolving card thumbnail).
pub struct Publication {
    root: Option<PathBuf>,
    source: source::Source,
    folder: Option<u64>,
    note: PathBuf,
    frames: Vec<c2m_core::timeline::FrameEvent>,
    kept_edits: Vec<PathBuf>,
}

impl Publication {
    pub fn new(
        task: &TaskRecord,
        root: Option<PathBuf>,
        done: &Done,
        manifest: Option<&Manifest>,
    ) -> Self {
        Self {
            root,
            source: task.source.clone(),
            folder: task.target_folder,
            note: done.out_dir.clone(),
            kept_edits: done.kept_edits.clone(),
            frames: manifest
                .map(|manifest| manifest.frames.clone())
                .unwrap_or_default(),
        }
    }

    /// Blocking: touches the library's disk.
    pub fn file(self) -> (Vec<String>, Option<PathBuf>) {
        let mut issues: Vec<String> = kept_edits_notice(&self.kept_edits).into_iter().collect();
        if let Some(root) = &self.root {
            if let Err(error) = source::save_cover(&self.source, &self.note) {
                issues.push(c2m_i18n::tr!("封面暂未保存：{}", format!("{error:#}")));
            }
            if let Err(error) = organize::Library::edit(root, |library| {
                library.assign(root, &self.note, self.folder)
            }) {
                issues.push(c2m_i18n::tr!(
                    "文件夹归属暂未更新：{}",
                    format!("{error:#}")
                ));
            }
        }
        let cover = self.note.join("cover.jpg");
        let thumbnail = cover.is_file().then_some(cover).or_else(|| {
            self.frames
                .iter()
                .find_map(|frame| artifact::safe_asset_path(&self.note, &frame.image).ok())
        });
        (issues, thumbnail)
    }
}

pub fn kept_edits_notice(kept: &[PathBuf]) -> Option<String> {
    (!kept.is_empty()).then(|| {
        c2m_i18n::tr!(
            "你改过的文件已另存为 {}",
            kept.iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(c2m_i18n::tr!("、"))
        )
    })
}

pub fn task_export_destination(state: &workspace::State, id: &str) -> Result<PathBuf> {
    let task = state.task(id).context(c2m_i18n::tr!("任务记录不存在"))?;
    let location = state
        .library(&task.library_id)
        .context(c2m_i18n::tr!("任务保存位置暂时不可用"))?;
    workspace::available_task_export(task, location)
}

#[cfg(test)]
mod tests {
    use super::*;
    use c2m_artifact::artifact::Outcome;

    fn setup() -> (
        tempfile::TempDir,
        workspace::Workspace,
        preferences::Store,
        String,
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut ws = workspace::Workspace::open_at_inline(
            root.path().join("workspace.json"),
            root.path().join("library"),
        )
        .unwrap();
        let prefs = preferences::Store::open(
            root.path().join("preferences"),
            std::sync::Arc::new(desktop_prefs::credentials::MemoryCredentialVault::new()),
        );
        let source = crate::plan::tests::source("https://www.bilibili.com/video/BVrun", "课程");
        let plan = workspace::NewTask::from_source(
            &ws.state.default_library,
            source,
            "课程",
            RunMode::Resume,
        );
        let id = ws.transaction(|state| state.enqueue(plan)).unwrap();
        (root, ws, prefs, id)
    }

    #[test]
    fn queued_task_always_resolves_current_preferences_without_pinning() {
        let (root, ws, mut prefs, id) = setup();
        let mut generation = prefs.generation().clone();
        generation.options.provider = Some(AsrProvider::Cpu);
        generation.options.transcript_source = Some(TranscriptSource::Asr);
        generation.options.asr_model = Some("qwen3-0.6b".into());
        prefs.save_generation(generation).unwrap();

        let task = ws.state.task(&id).unwrap();
        let (first, _) =
            execution_request(&prefs, &ws.state, task, &HashMap::new(), AsrProvider::Cpu).unwrap();
        assert_eq!(
            first.config.defaults.asr_model.as_deref(),
            Some("qwen3-0.6b")
        );

        // Change model in Preferences while task sits in queue -> next request uses new model!
        let mut gen2 = prefs.generation().clone();
        gen2.options.asr_model = Some("qwen3-1.7b".into());
        prefs.save_generation(gen2).unwrap();
        let (second, _) =
            execution_request(&prefs, &ws.state, task, &HashMap::new(), AsrProvider::Cpu).unwrap();
        assert_eq!(
            second.config.defaults.asr_model.as_deref(),
            Some("qwen3-1.7b")
        );

        // Simple control.json has only `intent`, no `stopped_services` or `resend`.
        let work = root.path().join("work-ctrl");
        write_control(&work, Intent::Pause).unwrap();
        let control: serde_json::Value =
            serde_json::from_slice(&std::fs::read(work.join("control.json")).unwrap()).unwrap();
        assert_eq!(control, serde_json::json!({"intent": "pause"}));
    }

    #[test]
    fn legacy_work_dir_is_lazily_adopted_preserving_cached_files() {
        let (root, mut ws, _prefs, id) = setup();
        let lib_root = root.path().join("library");
        let legacy = lib_root.join(".course2md/work/task-legacy-123");
        std::fs::create_dir_all(legacy.join("requests")).unwrap();
        std::fs::write(legacy.join("media.mp4"), "cached-video").unwrap();
        std::fs::write(legacy.join("asr.jsonl"), "cached-asr").unwrap();
        std::fs::write(legacy.join("requests/r1.json"), "{}").unwrap();
        let task = ws.state.task_mut(&id).unwrap();
        task.legacy_work_dir = Some(legacy.clone());
        let canonical = task.work_dir.clone();
        let course_id = task.course_id.clone();
        let source_id = task.source_id.clone();

        std::fs::write(
            legacy.join("task-identity.json"),
            serde_json::to_vec(&serde_json::json!({ "source_id": source_id })).unwrap(),
        )
        .unwrap();

        let adopted =
            adopt_legacy_work_dir(Some(&legacy), &canonical, &course_id, &source_id).unwrap();
        assert!(adopted);
        assert!(!legacy.exists());
        assert!(canonical.join("media.mp4").is_file());
        assert!(canonical.join("asr.jsonl").is_file());
        assert!(canonical.join("requests/r1.json").is_file());
        let migrated_id: serde_json::Value =
            serde_json::from_slice(&std::fs::read(canonical.join("task-identity.json")).unwrap())
                .unwrap();
        assert_eq!(migrated_id["course_id"], course_id);
    }

    #[test]
    fn partial_publication_marks_failed_with_note_dir_and_full_publication_completes() {
        let (root, mut ws, prefs, id) = setup();
        let note_dir = root.path().join("library/course-test");
        let done = Done {
            out_dir: note_dir.clone(),
            title: "课程".into(),
            partial: Some(true),
            ..Default::default()
        };
        let mut manifest = Manifest {
            schema: 2,
            task_id: id.clone(),
            course_id: ws.state.task(&id).unwrap().course_id.clone(),
            source_id: ws.state.task(&id).unwrap().source_id.clone(),
            title: "课程".into(),
            created_at_ms: 1000,
            document: "document.json".into(),
            markdown: "course.md".into(),
            frames: Vec::new(),
            assets: Vec::new(),
            outputs: Vec::new(),
            outcomes: Outcomes {
                transcript: Outcome::succeeded(),
                summary: Outcome {
                    kind: Some(FailureKind::Authentication),
                    ..Outcome::failed("API Key 无效")
                },
                ..Default::default()
            },
            partial: true,
            publication: String::new(),
            version_id: String::new(),
            revision: 0,
        };

        let (settled, cleanup) = record_finish(
            &mut ws.state,
            &prefs,
            AsrProvider::Cpu,
            &id,
            Some(&done),
            Some(&manifest),
            false,
            false,
            true,
            false,
        )
        .unwrap();
        assert!(cleanup.is_none(), "partial note keeps work_dir for retry");
        assert_eq!(settled.status, TaskStatus::Failed);
        assert_eq!(settled.note_dir.as_ref(), Some(&note_dir));
        let err = settled.error.as_ref().unwrap();
        assert_eq!(err.failed_stages, vec![Component::Summary]);
        assert!(err.needs_settings);

        // Retry finishes summary -> Completed, error cleared, cleanup returned!
        manifest.partial = false;
        manifest.outcomes.summary = Outcome::succeeded();
        let done_ok = Done {
            partial: Some(false),
            ..done
        };
        let (completed, cleanup) = record_finish(
            &mut ws.state,
            &prefs,
            AsrProvider::Cpu,
            &id,
            Some(&done_ok),
            Some(&manifest),
            true,
            true,
            true,
            false,
        )
        .unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);
        assert!(completed.error.is_none());
        assert!(completed.kept_video);
        let cleanup = cleanup.expect("completed task cleans scratch work_dir");
        assert!(cleanup.keep_offline_media);
    }
}
