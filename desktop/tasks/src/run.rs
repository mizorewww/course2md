//! Running a queued task: the engine request and control file it is sent with, the
//! checks its start reads from the library, and how its record changes when the start
//! fails, when it runs and when it ends. The app decides when; this says what.
use crate::{backend::Done, source, task_view};
use anyhow::{Context as _, Result};
use c2m_artifact::artifact::Manifest;
use c2m_config::{
    config::{AsrProvider, TranscriptSource},
    settings::ConfigFile,
};
use c2m_engine::execution::{Component, Operation, Request};
use desktop_prefs::{conversion::ConversionOptions, preferences};
use desktop_store::{
    organize,
    workspace::{
        self, Intent, LibraryLocation, LibraryWrites, RunSettings, TaskPlan, TaskRecord, TaskState,
    },
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// The text a generated note is made from: Settings' 文字来源 (`mode`). A subtitle
/// confirmed for the video is used unless Settings say speech recognition, and a
/// video found to have none at preview goes straight to speech recognition.
pub fn text_source(plan: &TaskPlan, mode: TranscriptSource) -> TranscriptSource {
    use c2m_source::subtitle::SubtitleEvidence;
    let source = &plan.source;
    if mode == TranscriptSource::Asr {
        TranscriptSource::Asr
    } else if source.selected_subtitle.is_some() || plan.subtitle.is_some() {
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

/// The configuration `plan` runs with now: Settings as they are, with what the
/// plan itself decides (its video's text, the parts a repair redoes), and the
/// recognition engine and its model filled in where Settings leave them
/// automatic, `fallback` being this machine's engine. Its services are
/// `task_settings`.
pub fn current_config(
    preferences: &preferences::Store,
    plan: &TaskPlan,
    fallback: AsrProvider,
) -> ConfigFile {
    let mut config = preferences.defaults_config();
    // Settings keep only what takes effect (vision only with proofreading), as a run reads it.
    ConversionOptions::from_config(&config).apply_to(&mut config);
    config.defaults.model_dir = Some(c2m_config::config::model_dir_from(
        config.defaults.model_dir.as_deref(),
    ));
    match &plan.operation {
        Operation::Generate => {
            let settings = config.defaults.transcript_source.unwrap_or_default();
            config.defaults.transcript_source = Some(text_source(plan, settings));
        }
        Operation::Reprocess { components, .. } => {
            // The note's own text, with proofreading, translation and summary as the repair asks.
            let repair = plan.repair.clone().unwrap_or_default();
            config.defaults.transcript_source = Some(TranscriptSource::Subtitle);
            config.llm.enabled = components.contains(&Component::Proofreading);
            config.llm.summarize = components.contains(&Component::Summary);
            config.llm.vision &= config.llm.enabled;
            config.llm.translate = repair.translate;
            if let Some(formats) = repair.formats {
                config.defaults.formats = Some(formats);
            }
        }
    }
    if config.defaults.transcript_source != Some(TranscriptSource::Subtitle) {
        let provider = config.defaults.provider.unwrap_or(fallback);
        config.defaults.provider = Some(provider);
        crate::plan::fill_default_asr_model(&mut config, provider);
    }
    config
}

/// The settings an unsettled paid request was made with, which a task keeps until
/// it is settled: a resend the person authorized, or a request whose outcome is not
/// known. A request's identity includes its payload and service version, so sent
/// with other settings the old request would never be settled.
fn pinned(task: &TaskRecord) -> Option<&RunSettings> {
    task.ran_with
        .as_ref()
        .filter(|_| !task.resend.is_empty() || task.has_uncertain())
}

/// The configuration and services a task runs with: the current settings and
/// default services, or, while it has an unsettled paid request, exactly what that
/// request was made with.
pub fn task_settings(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> (ConfigFile, preferences::ServiceRefs) {
    match pinned(task) {
        Some(ran_with) => (ran_with.config.clone(), refs_of(ran_with)),
        None => (
            current_config(preferences, &task.plan, fallback),
            preferences.default_refs(),
        ),
    }
}

fn refs_of(settings: &RunSettings) -> preferences::ServiceRefs {
    preferences::ServiceRefs {
        asr: settings.asr_service.clone(),
        llm: settings.ai_service.clone(),
    }
}

/// `settings` with the names its services have now, where it does not know them.
fn named(preferences: &preferences::Store, mut settings: RunSettings) -> RunSettings {
    let name = |id: &Option<String>| {
        id.as_deref()
            .and_then(|id| preferences.version(id))
            .map(|version| version.config.name.clone())
    };
    if settings.asr_service_name.is_none() {
        settings.asr_service_name = name(&settings.asr_service);
    }
    if settings.ai_service_name.is_none() {
        settings.ai_service_name = name(&settings.ai_service);
    }
    settings
}

/// Paid work the run `before` finished (`paid`) that a run with `next` would do and
/// pay for again, as the person is asked about it. What a run redoes is the engine's
/// own rule ([`c2m_engine::execution::redone_work`]): a transcript or screenshots
/// made again change every AI request built from them. A free service (a local
/// Ollama server, a ChatGPT plan through Codex: the engine's own rule for resending)
/// never asks, nor does work that replays: requests whose payload is unchanged, under
/// any version of their service (a new key or name). `source` is the task's video.
pub fn rework(
    source: &str,
    before: Option<&RunSettings>,
    next: &RunSettings,
    paid: workspace::PaidWork,
) -> Option<workspace::Rework> {
    use c2m_config::{config::AsrProvider::Api, llm_settings::LlmProvider};
    let before = before?;
    // What the earlier run's work would be redone of. Service versions are left out:
    // a request to another version with the same payload replays.
    let binding = |settings: &RunSettings| {
        c2m_engine::options::resolve(source.to_owned(), &Default::default(), &settings.config)
            .ok()
            .map(|cfg| c2m_engine::execution::settings_binding(&cfg))
    };
    let redone = match (binding(before), binding(next)) {
        (Some(old), Some(new)) => c2m_engine::execution::redone_work(&old, &new),
        // Unknown: as if everything were redone, so nothing paid is redone unasked.
        _ => c2m_engine::execution::RedoneWork {
            media: true,
            screenshots: true,
            transcript: true,
        },
    };
    let mut changes = Vec::new();
    let change =
        |label: &str, old: &Option<String>, new: &Option<String>, other: &str| match (old, new) {
            (Some(old), Some(new)) if old != new => {
                c2m_i18n::tr!("{}：「{}」→「{}」", label, old, new)
            }
            _ => other.to_owned(),
        };
    // AI: what the answered requests would be sent with, and what they are built from.
    let request = |settings: &RunSettings| {
        let llm = &settings.config.llm;
        (
            llm.provider,
            llm.base_url.clone(),
            llm.model.clone(),
            llm.prompt.clone(),
            llm.vision,
        )
    };
    let free = matches!(
        next.config.llm.provider,
        LlmProvider::Ollama | LlmProvider::Codex
    );
    if paid.ai && before.ai_service.is_some() && next.ai_service.is_some() && !free {
        if request(before) != request(next)
            // Another translation language is translated again; none at all is not.
            || (before.config.llm.translate.is_some()
                && next.config.llm.translate.is_some()
                && before.config.llm.translate != next.config.llm.translate)
            // Translation and summary are made from the proofread text: proofreading
            // turned on or off changes what they are sent.
            || (paid.after_proofreading
                && before.config.llm.enabled != next.config.llm.enabled)
        {
            changes.push(change(
                c2m_i18n::tr!("AI 服务"),
                &before.ai_service_name,
                &next.ai_service_name,
                c2m_i18n::tr!("AI 处理的设置已更改"),
            ));
        } else if redone.transcript || redone.screenshots {
            changes.push(c2m_i18n::tr!("文字或截图会重新生成，AI 处理随之重新发送").to_owned());
        }
    }
    // Speech recognition in the cloud, redone when what recognizes it changed.
    if paid.transcription
        && before.config.defaults.provider == Some(Api)
        && next.config.defaults.provider == Some(Api)
        && redone.transcript
    {
        changes.push(change(
            c2m_i18n::tr!("语音识别服务"),
            &before.asr_service_name,
            &next.asr_service_name,
            c2m_i18n::tr!("语音识别的设置已更改"),
        ));
    }
    (!changes.is_empty()).then(|| workspace::Rework {
        changes,
        token: next.token(),
    })
}

/// The paid redo continuing `task` now would cause, checked before it is resumed so
/// the person is asked at once: the same check its start applies, with the same
/// settings (`RunSettings::token`), so agreeing here lets that start go on.
/// Blocking: it reads the task's request records.
pub fn resume_rework(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> Option<workspace::Rework> {
    resume_check(preferences, task, fallback)?.run()
}

/// What [`resume_rework`] compares, taken from the settings on the UI thread; its
/// `run` reads the task's records and belongs off it.
pub struct ResumeCheck {
    task: TaskRecord,
    before: RunSettings,
    next: RunSettings,
}

pub fn resume_check(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> Option<ResumeCheck> {
    let before = task
        .ran_with
        .clone()
        .filter(|_| pinned(task).is_none())
        .map(|before| named(preferences, before))?;
    let (base, refs) = task_settings(preferences, task, fallback);
    let config = preferences.config_for_refs(&base, &refs).ok()?;
    let next = named(
        preferences,
        RunSettings::new(&config, refs.asr.clone(), refs.llm.clone()),
    );
    Some(ResumeCheck {
        task: task.clone(),
        before,
        next,
    })
}

impl ResumeCheck {
    /// Blocking: it reads the task's request records.
    pub fn run(self) -> Option<workspace::Rework> {
        rework(
            &self.task.plan.source.input,
            Some(&self.before),
            &self.next,
            workspace::paid_work(&self.task),
        )
    }
}

/// Records that the person was asked about `rework` for task `id`.
pub fn ask_rework(state: &mut workspace::State, id: &str, rework: workspace::Rework) -> Result<()> {
    state
        .task_mut(id)
        .context(c2m_i18n::tr!("任务不存在"))?
        .rework = Some(rework);
    Ok(())
}

/// Continuing would redo paid work under the current settings and the person has
/// not agreed to that yet; the start stops and the task waits for them.
#[derive(Debug)]
pub struct NeedsRework(pub workspace::Rework);

impl std::fmt::Display for NeedsRework {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(c2m_i18n::tr!(
            "设置已更改，继续生成会重新处理上次已完成的部分，可能产生额外费用。请确认后继续。"
        ))
    }
}

impl std::error::Error for NeedsRework {}

/// A start that `NeedsRework`: the task waits, paused, for the person's word.
pub fn await_rework(
    state: &mut workspace::State,
    id: &str,
    rework: workspace::Rework,
) -> Result<()> {
    let record = state.task_mut(id).context(c2m_i18n::tr!("任务不存在"))?;
    record.intent = Intent::Pause;
    record.state = TaskState::Paused;
    record.events.error = Some(NeedsRework(rework.clone()).to_string());
    record.rework = Some(rework);
    record.unread = true;
    record.updated = workspace::now();
    Ok(())
}

/// The person agreed to redo the paid work `rework` names: the task continues.
pub fn accept_rework(state: &mut workspace::State, id: &str) -> Result<()> {
    let record = state.task_mut(id).context(c2m_i18n::tr!("任务不存在"))?;
    record.rework_accepted = record.rework.take().map(|rework| rework.token);
    record.events.error = None;
    state.set_intent(id, Intent::Run)
}

/// Whether a service the task would run with now is no longer saved or was deleted,
/// so that starting it again could only stop with the same reason.
pub fn services_missing(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> bool {
    let (config, refs) = task_settings(preferences, task, fallback);
    preferences.services_missing(&config, &refs)
}

/// The credential references a task needs resolved before it can start.
pub fn needed_credentials(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> Result<Vec<String>> {
    let (config, refs) = task_settings(preferences, task, fallback);
    preferences.credential_references_for(&config, &refs)
}

/// The engine request a task runs as, with the current settings and services resolved
/// and `secrets` in place, and what it runs with for the record. A local source that
/// moved is read from where the reader last found it.
pub fn execution_request(
    preferences: &preferences::Store,
    state: &workspace::State,
    task: &TaskRecord,
    secrets: &HashMap<String, String>,
    fallback: AsrProvider,
) -> Result<(Request, RunSettings)> {
    let location = state
        .library(&task.plan.library_id)
        .context(c2m_i18n::tr!("任务保存位置暂时不可用"))?;
    let (base, refs) = task_settings(preferences, task, fallback);
    let config = preferences
        .resolve_for_execution_with_secrets(&base, &refs, secrets)?
        .into_config();
    let ran_with = named(
        preferences,
        RunSettings::new(&config, refs.asr.clone(), refs.llm.clone()),
    );
    let course_id = workspace::course_dir_name(&task.plan.source_id);
    let service_versions = [("asr", refs.asr.clone()), ("llm", refs.llm)]
        .into_iter()
        .filter_map(|(purpose, reference)| reference.map(|r| (purpose.to_owned(), r)))
        .collect();
    let source = state
        .reader_sources
        .get(&task.plan.source_id)
        .filter(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|_| !task.plan.source.online && !Path::new(&task.plan.source.input).is_file())
        .unwrap_or_else(|| task.plan.source.input.clone());
    let subtitle = (config.defaults.transcript_source != Some(TranscriptSource::Asr))
        .then(|| task.plan.subtitle.clone())
        .flatten();
    Ok((
        Request {
            schema: 1,
            operation: task.plan.operation.clone(),
            task_id: task.id.clone(),
            version_id: None,
            course_dir: location.root.join(&course_id),
            course_id,
            source,
            source_id: task.plan.source_id.clone(),
            title: task.plan.title.clone(),
            author: task.plan.source.author.clone(),
            duration: task.plan.source.duration,
            subtitle,
            subtitle_events: None,
            config,
            work_dir: task.work_dir.clone(),
            allow_unauthenticated_asr: refs
                .asr
                .as_deref()
                .and_then(|id| preferences.version(id))
                .is_some_and(|version| {
                    version.config.authentication == preferences::Authentication::None
                }),
            control_path: Some(task.work_dir.join("control.json")),
            service_versions,
        },
        ran_with,
    ))
}

/// What a task's control file should say: the engine reads it at every safe boundary.
/// A service the run started with that was removed or retired since is stopped.
pub fn task_control(preferences: &preferences::Store, task: &TaskRecord) -> Result<Vec<u8>> {
    control_for(preferences, task, task.ran_with.as_ref())
}

fn control_for(
    preferences: &preferences::Store,
    task: &TaskRecord,
    ran_with: Option<&RunSettings>,
) -> Result<Vec<u8>> {
    let stopped: Vec<_> = ran_with
        .into_iter()
        .flat_map(|ran_with| [&ran_with.asr_service, &ran_with.ai_service])
        .flatten()
        .filter(|id| {
            preferences
                .version(id)
                .is_none_or(|version| preferences.is_service_retired(&version.service_id))
        })
        .cloned()
        .collect();
    Ok(serde_json::to_vec(&serde_json::json!({
        "intent": task.intent, "stopped_services": stopped, "resend": task.resend,
    }))?)
}

/// Everything a start needs from the record, taken before the library is read.
pub struct StartRequest {
    pub request: Request,
    /// What the run starts with, for `mark_running`.
    pub ran_with: RunSettings,
    /// What the earlier run used, when the start may redo its paid work.
    before: Option<RunSettings>,
    control: Vec<u8>,
    location: LibraryLocation,
    writes: LibraryWrites,
}

/// The request, control and library a queued task starts with.
pub fn start_request(
    preferences: &preferences::Store,
    workspace: Option<&workspace::Workspace>,
    task: &TaskRecord,
    secrets: &HashMap<String, String>,
    fallback: AsrProvider,
) -> Result<StartRequest> {
    let workspace = workspace.context(c2m_i18n::tr!("任务记录不可用"))?;
    let (request, ran_with) =
        execution_request(preferences, &workspace.state, task, secrets, fallback)?;
    let control = control_for(preferences, task, Some(&ran_with))?;
    let before = task
        .ran_with
        .clone()
        .filter(|_| pinned(task).is_none())
        .map(|before| named(preferences, before));
    let location = workspace
        .state
        .library(&task.plan.library_id)
        .cloned()
        .context(c2m_i18n::tr!("任务保存位置尚未恢复"))?;
    Ok(StartRequest {
        request,
        ran_with,
        before,
        control,
        location,
        writes: workspace.library_writes(),
    })
}

impl StartRequest {
    /// Checks the library and puts the task's subtitles and control file in place.
    /// A start that would redo paid work the person has not agreed to redo stops with
    /// [`NeedsRework`]. Blocking: all of it touches the library's disk, which may be
    /// slow.
    pub fn prepare(self, task: &TaskRecord) -> Result<(Request, RunSettings)> {
        let Self {
            mut request,
            ran_with,
            before,
            control,
            location,
            writes,
        } = self;
        workspace::check_library(&location)?;
        if let Some(rework) = rework(
            &task.plan.source.input,
            before.as_ref(),
            &ran_with,
            workspace::paid_work(task),
        ) && task.rework_accepted.as_deref() != Some(rework.token.as_str())
        {
            return Err(NeedsRework(rework).into());
        }
        if request.config.defaults.transcript_source != Some(TranscriptSource::Asr)
            && task.plan.subtitle.is_none()
        {
            request.subtitle_events = workspace::task_subtitle_events(task)?;
        }
        workspace::store_task_subtitles(task)?;
        writes.write_control_now(&task.work_dir.join("control.json"), &control)?;
        request.validate()?;
        Ok((request, ran_with))
    }
}

/// Whether `id` is still the queued, runnable task a start was claimed for.
pub fn still_runnable(state: &workspace::State, id: &str) -> bool {
    state
        .task(id)
        .is_some_and(|record| record.state == TaskState::Queued && record.intent == Intent::Run)
}

/// The record of a task whose engine is about to start with `ran_with`.
pub fn mark_running(state: &mut workspace::State, id: &str, ran_with: RunSettings) -> Result<()> {
    let record = state.task_mut(id).context(c2m_i18n::tr!("任务不存在"))?;
    record.state = TaskState::Running;
    record.ran_with = Some(ran_with);
    record.rework = None;
    record.rework_accepted = None;
    record.events.begin_run();
    workspace::forget_task_subtitles(record);
    record.updated = workspace::now();
    Ok(())
}

/// A start that ended before its job ran: the task waits for the person. If the record
/// cannot be saved the task is marked in memory anyway, so the scheduler does not take it
/// for running; the next open reconciles with the disk. Returns the save's failure.
pub fn fail_before_start(
    workspace: &mut workspace::Workspace,
    id: &str,
    error: &anyhow::Error,
) -> Result<()> {
    let message = format!("{error:#}");
    let mark = |record: &mut TaskRecord| {
        record.state = TaskState::NeedsAttention;
        record.events.error = Some(message.clone());
        record.unread = true;
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

/// How a run that ended leaves its task: the outcome the engine reported or the note's
/// manifest records, then a finished state, or one that waits for the person.
/// `visible` says whether the person is looking at the task, which leaves it read.
pub fn record_finish(
    state: &mut workspace::State,
    id: &str,
    done: Option<&Done>,
    manifest: Option<&Manifest>,
    visible: bool,
    success: bool,
    cancelled: bool,
) -> Result<TaskRecord> {
    let task = state
        .task_mut(id)
        .context(c2m_i18n::tr!("任务记录不存在"))?;
    task.updated = workspace::now();
    task.unread = !visible;
    task.events.outcomes = done
        .and_then(|done| done.outcomes.clone())
        .or_else(|| manifest.map(|manifest| manifest.outcomes.clone()));
    workspace::reconcile_receipts(task)?;
    // The note is this task's now, unless it left the note as it was.
    let replaced = done
        .zip(manifest)
        .filter(|(_, manifest)| manifest.task_id == id && !task.exports_only())
        .map(|(done, _)| done.out_dir.clone());
    if let (Some(done), Some(manifest)) = (done, manifest) {
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
            task.events.error = None;
        }
    } else {
        task.state = match task.intent {
            Intent::Cancel => TaskState::Cancelled,
            Intent::Pause | Intent::Quit => TaskState::Paused,
            Intent::Run if task.has_uncertain() => TaskState::Uncertain,
            Intent::Run => TaskState::NeedsAttention,
        };
        if task.events.error.is_none() && !cancelled && task.intent == Intent::Run {
            task.events.error = Some(
                if success {
                    c2m_i18n::tr!("未生成可读笔记")
                } else {
                    c2m_i18n::tr!("生成中断")
                }
                .into(),
            );
        }
    }
    let record = task.clone();
    if let Some(note) = replaced {
        state.note_replaced(&note, id);
    }
    if record.state == TaskState::Complete && state.selected_task.as_deref() == Some(id) {
        state.selected_task = None;
    }
    Ok(record)
}

/// What filing a published note leaves to do: its cover, its folder, the card's picture.
pub struct Publication {
    root: Option<PathBuf>,
    source: source::Source,
    folder: Option<u64>,
    /// The note folder.
    note: PathBuf,
    frames: Vec<c2m_core::timeline::FrameEvent>,
    /// Hand-edited files of the replaced note, kept beside it under another name.
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
            source: task.plan.source.clone(),
            folder: task.plan.folder,
            note: done.out_dir.clone(),
            kept_edits: done.kept_edits.clone(),
            frames: manifest
                .map(|manifest| manifest.frames.clone())
                .unwrap_or_default(),
        }
    }

    /// Saves the note's cover beside it (a file of its own, which the next task
    /// replacing the note keeps), files the note in its folder and finds the card's
    /// picture.
    /// Returns what could not be done, in the interface language, and the picture.
    /// Blocking: it touches the library's disk.
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
            self.frames.iter().find_map(|frame| {
                c2m_artifact::artifact::safe_asset_path(&self.note, &frame.image).ok()
            })
        });
        (issues, thumbnail)
    }
}

/// What the person is told when a task replaced a note they had edited: the edited
/// files were kept beside the new note under these names.
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

/// The paid requests of a task whose answers never arrived, by id.
pub fn uncertain_requests(task: &TaskRecord) -> Vec<String> {
    task.events
        .blocked
        .iter()
        .filter(|b| b.is_uncertain())
        .filter_map(|b| b.request_id.clone())
        .collect()
}

/// The proofreading, translation and summary of a published note that failed, for a service repair.
pub fn repairable_components(task: &TaskRecord) -> Vec<Component> {
    if task.artifact.is_none() {
        return Vec::new();
    }
    task_view::task_component_failures(task)
        .into_iter()
        .filter_map(|(component, _, _)| Component::parse(&component))
        .filter(|component| task_view::ai_component(component.as_str()).is_some())
        .collect()
}

/// Where a task's exports are, when they are still there.
pub fn task_export_destination(state: &workspace::State, id: &str) -> Result<PathBuf> {
    let task = state.task(id).context(c2m_i18n::tr!("任务记录不存在"))?;
    let location = state
        .library(&task.plan.library_id)
        .context(c2m_i18n::tr!("任务保存位置暂时不可用"))?;
    workspace::available_task_export(task, location)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued_task() -> (tempfile::TempDir, workspace::Workspace, String) {
        let root = tempfile::tempdir().unwrap();
        let mut workspace = workspace::Workspace::open_at_inline(
            root.path().join("workspace.json"),
            root.path().join("library"),
        )
        .unwrap();
        let state = &mut workspace.state;
        let source = crate::plan::tests::source("https://www.bilibili.com/video/BVrun", "课程");
        let plan = workspace::TaskPlan {
            operation: Default::default(),
            source_id: source.identity.clone(),
            source,
            title: "课程".into(),
            library_id: state.default_library.clone(),
            folder: None,
            subtitle: None,
            repair: None,
        };
        let (id, _) = state.enqueue(plan, None).unwrap();
        (root, workspace, id)
    }

    #[test]
    fn a_start_that_fails_waits_for_the_person_and_a_run_is_recorded() {
        let (_root, mut workspace, id) = queued_task();
        assert!(still_runnable(&workspace.state, &id));
        workspace
            .transaction(|state| {
                mark_running(
                    state,
                    &id,
                    RunSettings::new(&Default::default(), None, None),
                )
            })
            .unwrap();
        assert_eq!(workspace.state.task(&id).unwrap().state, TaskState::Running);
        assert!(workspace.state.task(&id).unwrap().ran_with.is_some());
        assert!(!still_runnable(&workspace.state, &id));
        fail_before_start(&mut workspace, &id, &anyhow::anyhow!("凭据读取失败")).unwrap();
        let task = workspace.state.task(&id).unwrap();
        assert_eq!(task.state, TaskState::NeedsAttention);
        assert!(task.unread);
        assert_eq!(task.events.error.as_deref(), Some("凭据读取失败"));
        assert!(fail_before_start(&mut workspace, "missing", &anyhow::anyhow!("x")).is_err());
    }

    #[test]
    fn a_run_without_a_note_says_why_by_what_the_person_asked() {
        let (_root, mut workspace, id) = queued_task();
        let state = &mut workspace.state;
        let task = record_finish(state, &id, None, None, true, true, false).unwrap();
        assert_eq!(task.state, TaskState::NeedsAttention);
        assert!(!task.unread, "the person is looking at it");
        assert_eq!(
            task.events.error.as_deref(),
            Some(c2m_i18n::tr!("未生成可读笔记"))
        );
        state.task_mut(&id).unwrap().events.error = None;
        let task = record_finish(state, &id, None, None, false, false, true).unwrap();
        assert!(task.unread);
        assert!(task.events.error.is_none(), "a cancelled run says nothing");
        state.set_intent(&id, Intent::Pause).unwrap();
        let task = record_finish(state, &id, None, None, false, false, false).unwrap();
        assert_eq!(task.state, TaskState::Paused);
    }

    #[test]
    fn a_control_file_stops_a_service_that_is_gone() {
        let (_root, workspace, id) = queued_task();
        let mut task = workspace.state.task(&id).unwrap().clone();
        task.ran_with = Some(RunSettings::new(
            &Default::default(),
            None,
            Some("removed".into()),
        ));
        let preferences = desktop_prefs::preferences::Store::open(
            _root.path().join("preferences"),
            std::sync::Arc::new(desktop_prefs::credentials::MemoryCredentialVault::new()),
        );
        let control: serde_json::Value =
            serde_json::from_slice(&task_control(&preferences, &task).unwrap()).unwrap();
        assert_eq!(control["stopped_services"], serde_json::json!(["removed"]));
        assert_eq!(control["intent"], serde_json::json!("run"));
    }

    fn preferences_with_ai_service(
        root: &std::path::Path,
        model: &str,
    ) -> (desktop_prefs::preferences::Store, String) {
        use desktop_prefs::preferences::{
            Authentication, BindingScope, ServiceDraft, ServiceProtocol, ServicePurpose, Store,
        };
        let mut preferences = Store::open(
            root.join("preferences"),
            std::sync::Arc::new(desktop_prefs::credentials::MemoryCredentialVault::new()),
        );
        let mut draft = ServiceDraft::new(ServicePurpose::Ai);
        draft.protocol = ServiceProtocol::AiChat;
        draft.authentication = Authentication::None;
        draft.address = "http://127.0.0.1:9".into();
        draft.model = model.into();
        let draft = preferences.commit_service_draft(draft, None).unwrap();
        let verified = Store::verify_credential(
            preferences.vault().as_ref(),
            preferences.publication_credential(&draft.id).unwrap(),
        )
        .unwrap();
        let version = preferences
            .commit_publication(&draft.id, BindingScope::Defaults, verified)
            .unwrap();
        (preferences, version.id)
    }

    /// Settings `model` for local speech recognition and proofreading on.
    fn save_speech_model(preferences: &mut desktop_prefs::preferences::Store, model: &str) {
        let mut next = preferences.generation().clone();
        next.options.provider = Some(AsrProvider::Cpu);
        next.options.transcript_source = Some(TranscriptSource::Asr);
        next.options.asr_model = Some(model.into());
        next.ai.enabled = true;
        preferences.save_generation(next).unwrap();
    }

    /// The reported bug: a task queued before the model was changed in Settings
    /// still ran with the old one. A task holds no settings of its own: it runs
    /// with the speech model and AI service Settings name when it starts, and its
    /// record then says what that run used.
    #[test]
    fn a_task_queued_before_a_settings_change_runs_with_the_new_model() {
        use desktop_prefs::preferences::{BindingScope, ServiceDraft};
        let (root, mut workspace, id) = queued_task();
        let (mut preferences, old_service) = preferences_with_ai_service(root.path(), "old-ai");
        save_speech_model(&mut preferences, "old-speech");
        let task = workspace.state.task(&id).unwrap().clone();
        let (before, _) = execution_request(
            &preferences,
            &workspace.state,
            &task,
            &HashMap::new(),
            AsrProvider::Cpu,
        )
        .unwrap();
        assert_eq!(
            before.config.defaults.asr_model.as_deref(),
            Some("old-speech")
        );
        assert_eq!(before.config.llm.model, "old-ai");

        // Settings change while the task waits: another speech model, and the AI
        // service edited to another model.
        save_speech_model(&mut preferences, "new-speech");
        let mut edit = ServiceDraft::from_version(preferences.version(&old_service).unwrap());
        edit.model = "new-ai".into();
        let edit = preferences.commit_service_draft(edit, None).unwrap();
        let verified = desktop_prefs::preferences::Store::verify_credential(
            preferences.vault().as_ref(),
            preferences.publication_credential(&edit.id).unwrap(),
        )
        .unwrap();
        let new_service = preferences
            .commit_publication(&edit.id, BindingScope::Defaults, verified)
            .unwrap()
            .id;
        let task = workspace.state.task(&id).unwrap().clone();
        let start = start_request(
            &preferences,
            Some(&workspace),
            &task,
            &HashMap::new(),
            AsrProvider::Cpu,
        )
        .unwrap();
        assert_eq!(
            start.request.config.defaults.asr_model.as_deref(),
            Some("new-speech")
        );
        assert_eq!(start.request.config.llm.model, "new-ai");
        assert_eq!(
            start.request.service_versions.get("llm"),
            Some(&new_service)
        );
        let ran_with = start.ran_with.clone();
        workspace
            .transaction(|state| mark_running(state, &id, ran_with))
            .unwrap();
        let ran_with = workspace.state.task(&id).unwrap().ran_with.clone().unwrap();
        assert_eq!(
            ran_with.config.defaults.asr_model.as_deref(),
            Some("new-speech")
        );
        assert_eq!(ran_with.ai_service.as_deref(), Some(new_service.as_str()));
        assert!(ran_with.config.llm.api_key.is_empty());

        // A retry or resume runs with Settings as they are by then, too.
        save_speech_model(&mut preferences, "third-speech");
        let task = workspace.state.task(&id).unwrap().clone();
        let (again, _) = execution_request(
            &preferences,
            &workspace.state,
            &task,
            &HashMap::new(),
            AsrProvider::Cpu,
        )
        .unwrap();
        assert_eq!(
            again.config.defaults.asr_model.as_deref(),
            Some("third-speech")
        );
    }

    /// A resend the person authorized goes to the service version its uncertain
    /// requests were made with, whatever the default is now: a request's identity
    /// includes it, and the old request is settled only by its own resend.
    #[test]
    fn an_authorized_resend_keeps_the_service_its_requests_were_made_with() {
        use desktop_prefs::preferences::{BindingScope, ServiceDraft, Store};
        let (root, mut workspace, id) = queued_task();
        let (mut preferences, old_service) = preferences_with_ai_service(root.path(), "old-ai");
        save_speech_model(&mut preferences, "speech");
        let mut edit = ServiceDraft::from_version(preferences.version(&old_service).unwrap());
        edit.model = "new-ai".into();
        let edit = preferences.commit_service_draft(edit, None).unwrap();
        let verified = Store::verify_credential(
            preferences.vault().as_ref(),
            preferences.publication_credential(&edit.id).unwrap(),
        )
        .unwrap();
        let new_service = preferences
            .commit_publication(&edit.id, BindingScope::Defaults, verified)
            .unwrap()
            .id;
        // The earlier run: another speech model, the old AI version, its own prompt.
        let mut earlier = c2m_config::settings::ConfigFile::default();
        earlier.defaults.provider = Some(AsrProvider::Cpu);
        earlier.defaults.transcript_source = Some(TranscriptSource::Asr);
        earlier.defaults.asr_model = Some("earlier-speech".into());
        earlier.llm.enabled = true;
        earlier.llm.prompt = Some("earlier rule".into());
        workspace
            .transaction(|state| {
                let task = state.task_mut(&id).unwrap();
                task.ran_with = Some(RunSettings::new(&earlier, None, Some(old_service.clone())));
                Ok(())
            })
            .unwrap();
        let run = |workspace: &workspace::Workspace| {
            let task = workspace.state.task(&id).unwrap();
            execution_request(
                &preferences,
                &workspace.state,
                task,
                &HashMap::new(),
                AsrProvider::Cpu,
            )
            .unwrap()
            .0
        };
        // Resumed without a resend: the current default.
        assert_eq!(
            run(&workspace).service_versions.get("llm"),
            Some(&new_service)
        );
        workspace
            .transaction(|state| {
                state.task_mut(&id).unwrap().resend = vec!["llm-request.1".into()];
                Ok(())
            })
            .unwrap();
        // The resend is the same request: every setting it was made with, not only
        // its service.
        let resend = run(&workspace);
        assert_eq!(resend.service_versions.get("llm"), Some(&old_service));
        assert_eq!(resend.config.llm.model, "old-ai");
        assert_eq!(resend.config.llm.prompt.as_deref(), Some("earlier rule"));
        assert_eq!(
            resend.config.defaults.asr_model.as_deref(),
            Some("earlier-speech")
        );
        // A request whose outcome is not known yet (a crash while it was sent) keeps
        // them too, so a later resend can still settle it.
        workspace
            .transaction(|state| {
                let task = state.task_mut(&id).unwrap();
                task.resend.clear();
                task.events.blocked.push(workspace::BlockedRequest {
                    reason: c2m_protocol::progress::BlockReason::Uncertain,
                    request_id: Some("llm-request.1".into()),
                    purpose: Some("proofreading".into()),
                    description: "校对".into(),
                    kind: Some(c2m_artifact::artifact::FailureKind::Uncertain),
                    message: "结果未知".into(),
                });
                Ok(())
            })
            .unwrap();
        let unsettled = run(&workspace);
        assert_eq!(unsettled.service_versions.get("llm"), Some(&old_service));
        assert_eq!(
            unsettled.config.defaults.asr_model.as_deref(),
            Some("earlier-speech")
        );
    }

    const VIDEO: &str = "https://www.bilibili.com/video/BVrun";

    fn settings(edit: impl FnOnce(&mut RunSettings)) -> RunSettings {
        let mut settings = RunSettings::default();
        settings.config.llm.enabled = true;
        settings.config.llm.model = "model-a".into();
        settings.ai_service = Some("ai-a".into());
        settings.ai_service_name = Some("Service A".into());
        edit(&mut settings);
        settings
    }

    /// Redoing paid work under changed settings is asked first, naming what changed;
    /// a free service, unchanged requests and work never paid for ask nothing.
    #[test]
    fn redoing_paid_work_under_changed_settings_is_asked_first() {
        use workspace::PaidWork;
        let ai = PaidWork {
            ai: true,
            after_proofreading: false,
            transcription: false,
        };
        let before = settings(|_| {});
        // Unchanged requests replay: nothing to ask, even with an edited key or name.
        let renamed = settings(|s| {
            s.ai_service = Some("ai-a2".into());
            s.ai_service_name = Some("Renamed".into());
        });
        assert_eq!(rework(VIDEO, Some(&before), &renamed, ai), None);
        // Another model: asked, with the old name kept though that version is gone.
        let other = settings(|s| {
            s.config.llm.model = "model-b".into();
            s.ai_service = Some("ai-b".into());
            s.ai_service_name = Some("Service B".into());
        });
        let asked = rework(VIDEO, Some(&before), &other, ai).unwrap();
        assert_eq!(asked.changes, ["AI 服务：「Service A」→「Service B」"]);
        assert_eq!(asked.token, other.token());
        // Another proofreading rule on the same service is a change too.
        let rule = settings(|s| s.config.llm.prompt = Some("new rule".into()));
        assert_eq!(
            rework(VIDEO, Some(&before), &rule, ai).unwrap().changes,
            ["AI 处理的设置已更改"]
        );
        // Proofreading turned off after a translation or summary was answered: they
        // are made from the proofread text, so they would be sent again.
        let downstream = workspace::PaidWork {
            ai: true,
            after_proofreading: true,
            transcription: false,
        };
        let unproofread = settings(|s| s.config.llm.enabled = false);
        assert!(rework(VIDEO, Some(&before), &unproofread, downstream).is_some());
        // With only proofreading answered, turning it off sends nothing again.
        assert_eq!(rework(VIDEO, Some(&before), &unproofread, ai), None);
        // Turning translation off sends nothing again; another language does.
        let english = settings(|s| s.config.llm.translate = Some("en".into()));
        let no_translation = settings(|s| s.config.llm.translate = None);
        assert_eq!(
            rework(VIDEO, Some(&english), &no_translation, downstream),
            None
        );
        let japanese = settings(|s| s.config.llm.translate = Some("ja".into()));
        assert!(rework(VIDEO, Some(&english), &japanese, downstream).is_some());
        // Nothing answered, or nothing run before: nothing to pay for again.
        assert_eq!(
            rework(VIDEO, Some(&before), &other, PaidWork::default()),
            None
        );
        assert_eq!(rework(VIDEO, None, &other, ai), None);
        // A free service resends without asking.
        let ollama = settings(|s| {
            s.config.llm.model = "local".into();
            s.config.llm.provider = c2m_config::llm_settings::LlmProvider::Ollama;
        });
        assert_eq!(rework(VIDEO, Some(&before), &ollama, ai), None);
        // Cloud speech recognition that changed redoes a paid transcript; local does not.
        let speech = PaidWork {
            ai: false,
            after_proofreading: false,
            transcription: true,
        };
        let cloud = |model: &str, name: &str| {
            settings(|s| {
                s.config.defaults.provider = Some(AsrProvider::Api);
                s.config.asr_api.model = model.into();
                s.asr_service = Some(format!("asr-{model}"));
                s.asr_service_name = Some(name.into());
            })
        };
        assert_eq!(
            rework(
                VIDEO,
                Some(&cloud("a", "Speech A")),
                &cloud("b", "Speech B"),
                speech
            )
            .unwrap()
            .changes,
            ["语音识别服务：「Speech A」→「Speech B」"]
        );
        let local = settings(|s| s.config.defaults.provider = Some(AsrProvider::Cpu));
        assert_eq!(
            rework(VIDEO, Some(&cloud("a", "Speech A")), &local, speech),
            None
        );
        // A new key or name for the same speech service: its requests replay.
        let mut rotated = cloud("a", "Speech A renamed");
        rotated.asr_service = Some("asr-a-version-2".into());
        assert_eq!(
            rework(VIDEO, Some(&cloud("a", "Speech A")), &rotated, speech),
            None
        );
    }

    /// The reported flow: AI proofreading was paid for, then another local speech
    /// model is chosen. The transcript is redone, so every proofreading request
    /// changes and nothing replays: continuing asks first. So do screenshots made
    /// again, and another text source.
    #[test]
    fn work_redone_under_paid_ai_is_asked_first() {
        let ai = workspace::PaidWork {
            ai: true,
            after_proofreading: false,
            transcription: false,
        };
        let local = |edit: &dyn Fn(&mut RunSettings)| {
            settings(|s| {
                s.config.defaults.provider = Some(AsrProvider::Cpu);
                s.config.defaults.asr_model = Some("whisper".into());
                s.config.defaults.transcript_source = Some(TranscriptSource::Asr);
                edit(s);
            })
        };
        let before = local(&|_| {});
        let another_model = local(&|s| s.config.defaults.asr_model = Some("qwen3-1.7b".into()));
        assert_eq!(
            rework(VIDEO, Some(&before), &another_model, ai)
                .unwrap()
                .changes,
            ["文字或截图会重新生成，AI 处理随之重新发送"]
        );
        let subtitles =
            local(&|s| s.config.defaults.transcript_source = Some(TranscriptSource::Subtitle));
        assert!(rework(VIDEO, Some(&before), &subtitles, ai).is_some());
        let height = local(&|s| s.config.defaults.max_height = Some(720));
        assert!(rework(VIDEO, Some(&before), &height, ai).is_some());
        // Nothing the AI requests are built from changed: they replay.
        let formats = local(&|s| s.config.defaults.formats = Some(Vec::new()));
        assert_eq!(rework(VIDEO, Some(&before), &formats, ai), None);
    }

    /// Every way a task starts passes the same check: one that would redo paid work
    /// stops before its job runs and waits, paused, until the person agrees to
    /// exactly these settings; then it starts.
    #[test]
    fn a_start_that_would_pay_again_waits_for_the_person() {
        let (root, mut workspace, id) = queued_task();
        let (mut preferences, old_service) = preferences_with_ai_service(root.path(), "old-ai");
        save_speech_model(&mut preferences, "speech");
        let mut earlier = c2m_config::settings::ConfigFile::default();
        earlier.llm.enabled = true;
        earlier.llm.model = "older-model".into();
        let work_dir = workspace.state.task(&id).unwrap().work_dir.clone();
        workspace
            .transaction(|state| {
                state.task_mut(&id).unwrap().ran_with =
                    Some(RunSettings::new(&earlier, None, Some(old_service.clone())));
                Ok(())
            })
            .unwrap();
        // An answered proofreading request of the earlier run.
        std::fs::create_dir_all(work_dir.join("requests")).unwrap();
        std::fs::write(
            work_dir.join("requests/answered.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1, "stable_id": "answered", "request_id": "answered.1",
                "purpose": "proofreading", "description": "", "service_version": old_service,
                "attempt": 1, "state": "completed", "http_status": 200,
                "response": {}, "message": null,
            }))
            .unwrap(),
        )
        .unwrap();
        let start = |workspace: &workspace::Workspace| {
            let task = workspace.state.task(&id).unwrap().clone();
            start_request(
                &preferences,
                Some(workspace),
                &task,
                &HashMap::new(),
                AsrProvider::Cpu,
            )
            .unwrap()
            .prepare(&task)
        };
        let Err(error) = start(&workspace) else {
            panic!("a start that would pay again must wait");
        };
        let NeedsRework(asked) = error.downcast::<NeedsRework>().unwrap();
        workspace
            .transaction(|state| await_rework(state, &id, asked.clone()))
            .unwrap();
        let task = workspace.state.task(&id).unwrap();
        assert_eq!(task.state, TaskState::Paused);
        assert_eq!(task.rework.as_ref(), Some(&asked));
        // Resumed by any way but the person's answer, it stops at the same check.
        workspace
            .transaction(|state| state.set_intent(&id, Intent::Run))
            .unwrap();
        assert!(
            matches!(start(&workspace), Err(error) if error.downcast_ref::<NeedsRework>().is_some())
        );
        workspace
            .transaction(|state| accept_rework(state, &id))
            .unwrap();
        let Ok((_, ran_with)) = start(&workspace) else {
            panic!("an accepted redo starts");
        };
        workspace
            .transaction(|state| mark_running(state, &id, ran_with))
            .unwrap();
        let task = workspace.state.task(&id).unwrap();
        assert!(task.rework.is_none() && task.rework_accepted.is_none());
        assert_eq!(task.ran_with.as_ref().unwrap().config.llm.model, "old-ai");
        // The reported flow: with that proofreading paid for, another speech model is
        // chosen in Settings. The transcript would be redone and every request with it,
        // so continuing waits for the person again.
        workspace
            .transaction(|state| {
                let task = state.task_mut(&id).unwrap();
                task.state = TaskState::Paused;
                state.set_intent(&id, Intent::Run)
            })
            .unwrap();
        assert!(start(&workspace).is_ok(), "nothing changed: it continues");
        save_speech_model(&mut preferences, "another-speech-model");
        let start = |workspace: &workspace::Workspace| {
            let task = workspace.state.task(&id).unwrap().clone();
            start_request(
                &preferences,
                Some(workspace),
                &task,
                &HashMap::new(),
                AsrProvider::Cpu,
            )
            .unwrap()
            .prepare(&task)
        };
        assert!(
            matches!(start(&workspace), Err(error) if error.downcast_ref::<NeedsRework>().is_some())
        );
    }

    /// Continuing a task asks at once when it would pay again, with the settings its
    /// start then checks: agreeing lets that start go on without asking twice, and a
    /// task with nothing to redo continues without asking.
    #[test]
    fn resuming_asks_once_with_the_settings_the_start_checks() {
        let (root, mut workspace, id) = queued_task();
        let (mut preferences, old_service) = preferences_with_ai_service(root.path(), "old-ai");
        save_speech_model(&mut preferences, "speech");
        let task = workspace.state.task(&id).unwrap().clone();
        assert_eq!(resume_rework(&preferences, &task, AsrProvider::Cpu), None);
        let mut earlier = c2m_config::settings::ConfigFile::default();
        earlier.llm.enabled = true;
        earlier.llm.model = "older-model".into();
        let work_dir = task.work_dir.clone();
        std::fs::create_dir_all(work_dir.join("requests")).unwrap();
        std::fs::write(
            work_dir.join("requests/answered.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1, "stable_id": "answered", "request_id": "answered.1",
                "purpose": "proofreading", "description": "", "service_version": old_service,
                "attempt": 1, "state": "completed", "http_status": 200,
                "response": {}, "message": null,
            }))
            .unwrap(),
        )
        .unwrap();
        workspace
            .transaction(|state| {
                let task = state.task_mut(&id).unwrap();
                task.ran_with = Some(RunSettings::new(&earlier, None, Some(old_service.clone())));
                task.state = TaskState::Paused;
                task.intent = Intent::Pause;
                Ok(())
            })
            .unwrap();
        let task = workspace.state.task(&id).unwrap().clone();
        let asked = resume_rework(&preferences, &task, AsrProvider::Cpu).unwrap();
        workspace
            .transaction(|state| {
                ask_rework(state, &id, asked.clone())?;
                accept_rework(state, &id)
            })
            .unwrap();
        let task = workspace.state.task(&id).unwrap().clone();
        assert_eq!(task.state, TaskState::Queued);
        let started = start_request(
            &preferences,
            Some(&workspace),
            &task,
            &HashMap::new(),
            AsrProvider::Cpu,
        )
        .unwrap()
        .prepare(&task);
        assert!(started.is_ok(), "agreed once, the start goes on");
    }

    /// The UI review's case: proofreading answered by service A, the task continued
    /// with service B whose requests are the same, so A's answers were replayed and
    /// nothing was sent to B. The task's results name A, never B; while it works
    /// they name the service its run uses.
    #[test]
    fn finished_ai_results_name_the_service_that_answered_them() {
        let (_root, mut workspace, id) = queued_task();
        let work_dir = workspace.state.task(&id).unwrap().work_dir.clone();
        std::fs::create_dir_all(work_dir.join("requests")).unwrap();
        std::fs::write(
            work_dir.join("requests/answered.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1, "stable_id": "answered", "request_id": "answered.1",
                "purpose": "proofreading", "description": "", "service_version": "service-a",
                "attempt": 1, "state": "completed", "http_status": 200,
                "response": {}, "message": null,
            }))
            .unwrap(),
        )
        .unwrap();
        let mut b = c2m_config::settings::ConfigFile::default();
        b.llm.enabled = true;
        b.llm.base_url = "https://ai.example.test/v1".into();
        b.llm.model = "model".into();
        workspace
            .transaction(|state| {
                let task = state.task_mut(&id).unwrap();
                task.ran_with = Some(RunSettings::new(&b, None, Some("service-b".into())));
                task.state = TaskState::Running;
                Ok(())
            })
            .unwrap();
        let task = workspace.state.task(&id).unwrap();
        assert_eq!(crate::task_view::ai_producers(task), ["service-b"]);
        let task = record_finish(&mut workspace.state, &id, None, None, true, true, false).unwrap();
        assert_eq!(task.ai_answered_by, ["service-a"]);
        let mut finished = task.clone();
        finished.state = TaskState::Complete;
        assert_eq!(crate::task_view::ai_producers(&finished), ["service-a"]);
    }

    /// A task an older version queued fixed the model it would use; it now runs with
    /// the one Settings name.
    #[test]
    fn a_task_an_older_version_queued_runs_with_the_current_settings() {
        let (root, workspace, id) = queued_task();
        let (mut preferences, _) = preferences_with_ai_service(root.path(), "current-ai");
        save_speech_model(&mut preferences, "current-speech");
        let mut record = serde_json::to_value(workspace.state.task(&id).unwrap()).unwrap();
        let mut old = c2m_config::settings::ConfigFile::default();
        old.defaults.asr_model = Some("old-speech".into());
        old.defaults.provider = Some(AsrProvider::Cpu);
        old.llm.model = "old-ai".into();
        record["plan"]["config"] = serde_json::to_value(&old).unwrap();
        record["plan"]["ai_service"] = "service-version-old".into();
        let mut task: TaskRecord = serde_json::from_value(record).unwrap();
        task.work_dir = workspace.state.task(&id).unwrap().work_dir.clone();
        let (request, _) = execution_request(
            &preferences,
            &workspace.state,
            &task,
            &HashMap::new(),
            AsrProvider::Cpu,
        )
        .unwrap();
        assert_eq!(
            request.config.defaults.asr_model.as_deref(),
            Some("current-speech")
        );
        assert_eq!(request.config.llm.model, "current-ai");
    }

    /// Settings' 文字来源 decides; within it, the video's confirmed subtitle is used,
    /// and a video found to have none goes to speech recognition.
    #[test]
    fn the_text_source_follows_settings() {
        use c2m_source::subtitle::SubtitleEvidence;
        let (_root, workspace, id) = queued_task();
        let mut plan = workspace.state.task(&id).unwrap().plan.clone();
        assert_eq!(
            text_source(&plan, TranscriptSource::Auto),
            TranscriptSource::Auto
        );
        assert_eq!(
            text_source(&plan, TranscriptSource::Asr),
            TranscriptSource::Asr
        );
        plan.source.subtitles = SubtitleEvidence::NoneFound;
        assert_eq!(
            text_source(&plan, TranscriptSource::Auto),
            TranscriptSource::Asr
        );
        plan.subtitle = Some("captions.srt".into());
        assert_eq!(
            text_source(&plan, TranscriptSource::Auto),
            TranscriptSource::Subtitle
        );
        // Settings that say speech recognition only win over a subtitle found.
        assert_eq!(
            text_source(&plan, TranscriptSource::Asr),
            TranscriptSource::Asr
        );
    }

    /// A note the person had edited is replaced; the task's result tells them where
    /// their edits were kept, then and after a restart.
    #[test]
    fn edits_kept_beside_a_replaced_note_are_reported_with_the_result() {
        use c2m_protocol::progress::Event;
        let (_root, mut workspace, id) = queued_task();
        let state = &mut workspace.state;
        let note = state
            .library(&state.default_library)
            .unwrap()
            .root
            .join("course");
        let kept = vec![PathBuf::from("course.edited.md")];
        let done = Done {
            task_id: id.clone(),
            out_dir: note.clone(),
            kept_edits: kept.clone(),
            ..Default::default()
        };
        let event = Event::Done(Box::new(done.clone()));
        state.apply_task_event(&id, &event);
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "schema": 2, "task_id": id, "course_id": "course", "source_id": "source",
            "title": "课程", "created_at_ms": 1, "document": "document.json",
            "markdown": "course.md", "frames": [], "assets": [], "outputs": [],
            "outcomes": {}, "partial": false,
        }))
        .unwrap();
        let task =
            record_finish(state, &id, Some(&done), Some(&manifest), true, true, false).unwrap();
        assert_eq!(task.state, TaskState::Complete);
        assert_eq!(task.artifact.as_ref(), Some(&note));
        let notice = task_view::task_kept_edits(&task).unwrap();
        assert!(notice.contains("course.edited.md"), "{notice}");
        // Filing the note says so too, with what it could not do.
        let (issues, _) = Publication::new(&task, None, &done, Some(&manifest)).file();
        assert_eq!(issues, vec![notice.clone()]);
        // A restart reads it back from the saved events; a later run of the same
        // task, which finds its note already published, reports none and changes
        // nothing.
        let mut events = desktop_store::activity::JobEvents::default();
        let mut again = done.clone();
        again.kept_edits.clear();
        let stream = [event, Event::Done(Box::new(again))]
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        events.replay(stream.as_bytes());
        assert_eq!(events.kept_edits, kept);
        assert!(kept_edits_notice(&[]).is_none());
    }
    /// A task that replaces a note leaves the task whose note it was replaced, at
    /// once, as the next opening would; one that left the note as it was does not.
    #[test]
    fn a_published_note_replaces_the_earlier_task_s_result() {
        let (_root, mut workspace, first) = queued_task();
        let state = &mut workspace.state;
        let note = state
            .library(&state.default_library)
            .unwrap()
            .root
            .join("course");
        let manifest = |task: &str| -> Manifest {
            serde_json::from_value(serde_json::json!({
                "schema": 2, "task_id": task, "course_id": "course", "source_id": "source",
                "title": "课程", "created_at_ms": 1, "document": "document.json",
                "markdown": "course.md", "frames": [], "assets": [], "outputs": [],
                "outcomes": {}, "partial": false,
            }))
            .unwrap()
        };
        let done = |task: &str| Done {
            task_id: task.into(),
            out_dir: note.clone(),
            ..Default::default()
        };
        record_finish(
            state,
            &first,
            Some(&done(&first)),
            Some(&manifest(&first)),
            true,
            true,
            false,
        )
        .unwrap();
        let mut plan = state.task(&first).unwrap().plan.clone();
        plan.title = "重新生成".into();
        let (second, _) = state.enqueue(plan.clone(), None).unwrap();
        // A run that left the note as the first task wrote it replaces nothing.
        record_finish(
            state,
            &second,
            Some(&done(&second)),
            Some(&manifest(&first)),
            true,
            true,
            false,
        )
        .unwrap();
        assert_eq!(state.task(&first).unwrap().replaced_by, None);
        let (third, _) = state.enqueue(plan, None).unwrap();
        record_finish(
            state,
            &third,
            Some(&done(&third)),
            Some(&manifest(&third)),
            true,
            true,
            false,
        )
        .unwrap();
        let replaced = state.task(&first).unwrap();
        assert_eq!(replaced.replaced_by.as_deref(), Some(third.as_str()));
        assert_eq!(replaced.artifact, None);
        assert_eq!(replaced.state, TaskState::Complete);
        assert_eq!(
            state.task(&second).unwrap().replaced_by.as_deref(),
            Some(third.as_str())
        );
        assert_eq!(state.task(&third).unwrap().artifact.as_ref(), Some(&note));
        assert_eq!(
            task_view::task_status_label(state.task(&first).unwrap()),
            c2m_i18n::tr!("笔记已被之后的任务更新")
        );
    }
}
