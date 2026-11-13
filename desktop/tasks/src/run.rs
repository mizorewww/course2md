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

/// The text a generated note is made from: the video's own choice on its card,
/// otherwise `settings`. A subtitle confirmed for the video is used unless speech
/// recognition is chosen, and a video found to have none at preview goes straight
/// to speech recognition.
pub fn text_source(plan: &TaskPlan, settings: TranscriptSource) -> TranscriptSource {
    use c2m_source::subtitle::SubtitleEvidence;
    let mode = plan.text_source.unwrap_or(settings);
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
/// `task_refs`.
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

/// The services a task runs with: Settings' defaults, except for a resend the person
/// authorized, which goes to the services its uncertain requests were made with.
/// A request's identity includes its service version: sent elsewhere, the old
/// request would never be settled and the task would stay unconfirmed.
pub fn task_refs(preferences: &preferences::Store, task: &TaskRecord) -> preferences::ServiceRefs {
    match task.ran_with.as_ref().filter(|_| !task.resend.is_empty()) {
        Some(ran_with) => preferences::ServiceRefs {
            asr: ran_with.asr_service.clone(),
            llm: ran_with.ai_service.clone(),
        },
        None => preferences.default_refs(),
    }
}

/// The AI service a task would continue with, when it is another than the one
/// its earlier run used and its requests differ (another model, address or kind of
/// service): what that run already had answered is then sent, and paid for, again.
/// The old and new services' names. `answered` says whether the earlier run has
/// answered AI requests (`workspace::has_answered_ai_requests`). Identical requests
/// to an edited service (a new key or name) reuse their answers and ask nothing.
pub fn ai_work_sent_again(
    preferences: &preferences::Store,
    task: &TaskRecord,
    answered: bool,
) -> Option<(String, String)> {
    let before = task.ran_with.as_ref()?.ai_service.as_deref()?;
    let now = task_refs(preferences, task).llm?;
    if !answered || before == now {
        return None;
    }
    let now = preferences.version(&now)?;
    let changed = preferences.version(before).is_none_or(|before| {
        before.config.model != now.config.model
            || before.config.endpoint != now.config.endpoint
            || before.config.protocol != now.config.protocol
    });
    changed.then(|| {
        let name = preferences.version(before).map_or_else(
            || now.config.name.clone(),
            |before| before.config.name.clone(),
        );
        (name, now.config.name.clone())
    })
}

/// Whether a service the task would run with now is no longer saved or was deleted,
/// so that starting it again could only stop with the same reason.
pub fn services_missing(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> bool {
    preferences.services_missing(
        &current_config(preferences, &task.plan, fallback),
        &task_refs(preferences, task),
    )
}

/// The credential references a task needs resolved before it can start.
pub fn needed_credentials(
    preferences: &preferences::Store,
    task: &TaskRecord,
    fallback: AsrProvider,
) -> Result<Vec<String>> {
    preferences.credential_references_for(
        &current_config(preferences, &task.plan, fallback),
        &task_refs(preferences, task),
    )
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
    let refs = task_refs(preferences, task);
    let base = current_config(preferences, &task.plan, fallback);
    let config = preferences
        .resolve_for_execution_with_secrets(&base, &refs, secrets)?
        .into_config();
    let ran_with = RunSettings::new(&config, refs.asr.clone(), refs.llm.clone());
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
    let location = workspace
        .state
        .library(&task.plan.library_id)
        .cloned()
        .context(c2m_i18n::tr!("任务保存位置尚未恢复"))?;
    Ok(StartRequest {
        request,
        ran_with,
        control,
        location,
        writes: workspace.library_writes(),
    })
}

impl StartRequest {
    /// Checks the library and puts the task's subtitles and control file in place.
    /// Blocking: all of it touches the library's disk, which may be slow.
    pub fn prepare(self, task: &TaskRecord) -> Result<(Request, RunSettings)> {
        let Self {
            mut request,
            ran_with,
            control,
            location,
            writes,
        } = self;
        workspace::check_library(&location)?;
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
            text_source: None,
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
        draft.protocol = ServiceProtocol::OllamaChat;
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
        workspace
            .transaction(|state| {
                let task = state.task_mut(&id).unwrap();
                task.ran_with = Some(RunSettings::new(
                    &Default::default(),
                    None,
                    Some(old_service.clone()),
                ));
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
        let resend = run(&workspace);
        assert_eq!(resend.service_versions.get("llm"), Some(&old_service));
        assert_eq!(resend.config.llm.model, "old-ai");
    }

    /// Continuing a task with another AI model or service than its earlier run
    /// would pay again for what that run had answered: that is asked first. An edit
    /// that leaves the requests as they were (a new key or name) reuses the answers
    /// and asks nothing, and nothing is asked before any request was answered.
    #[test]
    fn continuing_with_a_changed_ai_service_asks_before_paying_again() {
        use desktop_prefs::preferences::{BindingScope, ServiceDraft, Store};
        let (root, mut workspace, id) = queued_task();
        let (mut preferences, first) = preferences_with_ai_service(root.path(), "model-a");
        let publish = |preferences: &mut Store, base: &str, edit: &dyn Fn(&mut ServiceDraft)| {
            let mut draft = ServiceDraft::from_version(preferences.version(base).unwrap());
            edit(&mut draft);
            let draft = preferences.commit_service_draft(draft, None).unwrap();
            let verified = Store::verify_credential(
                preferences.vault().as_ref(),
                preferences.publication_credential(&draft.id).unwrap(),
            )
            .unwrap();
            preferences
                .commit_publication(&draft.id, BindingScope::Defaults, verified)
                .unwrap()
                .id
        };
        workspace
            .transaction(|state| {
                state.task_mut(&id).unwrap().ran_with = Some(RunSettings::new(
                    &Default::default(),
                    None,
                    Some(first.clone()),
                ));
                Ok(())
            })
            .unwrap();
        let task = workspace.state.task(&id).unwrap().clone();
        assert_eq!(ai_work_sent_again(&preferences, &task, true), None);
        // Renamed only: the same requests, whose answers are reused.
        publish(&mut preferences, &first, &|draft| {
            draft.name = "renamed".into()
        });
        assert_eq!(ai_work_sent_again(&preferences, &task, true), None);
        // Another model: the answered work would be sent again.
        let default = preferences.default_refs().llm.unwrap();
        publish(&mut preferences, &default, &|draft| {
            draft.model = "model-b".into()
        });
        assert!(ai_work_sent_again(&preferences, &task, true).is_some());
        assert_eq!(ai_work_sent_again(&preferences, &task, false), None);
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

    /// The video's own text choice and its confirmed subtitle decide the text
    /// source; otherwise Settings do, and a repair works from the note's text.
    #[test]
    fn the_text_source_follows_the_video_choice_then_settings() {
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
        // The person's choice on the card wins over Settings.
        plan.text_source = Some(TranscriptSource::Subtitle);
        assert_eq!(
            text_source(&plan, TranscriptSource::Asr),
            TranscriptSource::Subtitle
        );
        plan.text_source = Some(TranscriptSource::Asr);
        assert_eq!(
            text_source(&plan, TranscriptSource::Subtitle),
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
