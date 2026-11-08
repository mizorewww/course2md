//! Running a queued task: the engine request and control file it is sent with, the
//! checks its start reads from the library, and how its record changes when the start
//! fails, when it runs and when it ends. The app decides when; this says what.
use crate::{backend::Done, source, task_view};
use anyhow::{Context as _, Result};
use c2m_artifact::artifact::Manifest;
use c2m_config::{config::TranscriptSource, settings::ConfigFile};
use c2m_engine::execution::{Component, Request};
use desktop_prefs::preferences::{self, ServiceRefs};
use desktop_store::{
    organize,
    workspace::{self, Intent, LibraryLocation, LibraryWrites, TaskRecord, TaskState},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// The services a task's plan names.
fn service_refs(task: &TaskRecord) -> ServiceRefs {
    ServiceRefs {
        asr: task.plan.asr_service.clone(),
        llm: task.plan.ai_service.clone(),
    }
}

/// Whether a service the task needs is no longer saved or was deleted, so that
/// starting it again could only stop with the same reason.
pub fn services_missing(preferences: &preferences::Store, task: &TaskRecord) -> bool {
    preferences.services_missing(&task.plan.config, &service_refs(task))
}

/// The credential references a task needs resolved before it can start.
pub fn needed_credentials(
    preferences: &preferences::Store,
    task: &TaskRecord,
) -> Result<Vec<String>> {
    preferences.credential_references_for(&task.plan.config, &service_refs(task))
}

/// The engine request a task runs as, with its services resolved and `secrets` in place.
/// A local source that moved is read from where the reader last found it.
pub fn execution_request(
    preferences: &preferences::Store,
    state: &workspace::State,
    task: &TaskRecord,
    secrets: &HashMap<String, String>,
) -> Result<Request> {
    let location = state
        .library(&task.plan.library_id)
        .context(c2m_i18n::tr!("任务保存位置暂时不可用"))?;
    let refs = service_refs(task);
    let config = preferences
        .resolve_for_execution_with_secrets(&task.plan.config, &refs, secrets)?
        .into_config();
    let course_id = workspace::course_dir_name(&task.plan.source_id);
    let service_versions = [("asr", refs.asr), ("llm", refs.llm)]
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
    Ok(Request {
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
        subtitle: if task.plan.options().source_mode != TranscriptSource::Asr {
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
            .and_then(|id| preferences.version(id))
            .is_some_and(|version| {
                version.config.authentication == preferences::Authentication::None
            }),
        control_path: Some(task.work_dir.join("control.json")),
        service_versions,
    })
}

/// What a task's control file should say: the engine reads it at every safe boundary.
/// A service removed or retired since the plan was made is stopped.
pub fn task_control(preferences: &preferences::Store, task: &TaskRecord) -> Result<Vec<u8>> {
    let stopped: Vec<_> = [&task.plan.asr_service, &task.plan.ai_service]
        .into_iter()
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
) -> Result<StartRequest> {
    let workspace = workspace.context(c2m_i18n::tr!("任务记录不可用"))?;
    let request = execution_request(preferences, &workspace.state, task, secrets)?;
    let control = task_control(preferences, task)?;
    let location = workspace
        .state
        .library(&task.plan.library_id)
        .cloned()
        .context(c2m_i18n::tr!("任务保存位置尚未恢复"))?;
    Ok(StartRequest {
        request,
        control,
        location,
        writes: workspace.library_writes(),
    })
}

impl StartRequest {
    /// Checks the library and puts the task's subtitles and control file in place.
    /// Blocking: all of it touches the library's disk, which may be slow.
    pub fn prepare(self, task: &TaskRecord) -> Result<Request> {
        let Self {
            mut request,
            control,
            location,
            writes,
        } = self;
        workspace::check_library(&location)?;
        if task.plan.options().source_mode != TranscriptSource::Asr && task.plan.subtitle.is_none()
        {
            request.subtitle_events = workspace::task_subtitle_events(task)?;
        }
        workspace::store_task_subtitles(task)?;
        writes.write_control_now(&task.work_dir.join("control.json"), &control)?;
        request.validate()?;
        Ok(request)
    }
}

/// Whether `id` is still the queued, runnable task a start was claimed for.
pub fn still_runnable(state: &workspace::State, id: &str) -> bool {
    state
        .task(id)
        .is_some_and(|record| record.state == TaskState::Queued && record.intent == Intent::Run)
}

/// The record of a task whose engine is about to start.
pub fn mark_running(state: &mut workspace::State, id: &str) -> Result<()> {
    let record = state.task_mut(id).context(c2m_i18n::tr!("任务不存在"))?;
    record.state = TaskState::Running;
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

/// The configuration a repair of `components` runs with on `ai_service`: the note's own
/// text, with proofreading, translation and summary as the repair asks.
pub fn reprocess_config(
    preferences: &preferences::Store,
    task: &TaskRecord,
    components: &[Component],
    ai_service: &str,
) -> Result<ConfigFile> {
    let mut base = task.plan.config.clone();
    base.defaults.transcript_source = Some(TranscriptSource::Subtitle);
    base.llm.enabled = components.contains(&Component::Proofreading);
    base.llm.summarize = components.contains(&Component::Summary);
    // Translation is redone on its own, or follows the proofread paragraphs it is redone with.
    if !base.llm.enabled && !components.contains(&Component::Translation) {
        base.llm.translate = None;
    }
    preferences.config_for_refs(
        &base,
        &ServiceRefs {
            asr: None,
            llm: Some(ai_service.to_owned()),
        },
    )
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
            Default::default(),
        )
        .unwrap();
        let state = &mut workspace.state;
        let source = crate::plan::tests::source("https://www.bilibili.com/video/BVrun", "课程");
        let mut config = c2m_config::settings::ConfigFile::default();
        desktop_prefs::conversion::ConversionOptions::default().apply_to(&mut config);
        let plan = workspace::TaskPlan {
            operation: Default::default(),
            source_id: source.identity.clone(),
            source,
            title: "课程".into(),
            library_id: state.default_library.clone(),
            folder: None,
            subtitle: None,
            config,
            asr_service: None,
            ai_service: None,
        };
        let (id, _) = state.enqueue(plan, None).unwrap();
        (root, workspace, id)
    }

    #[test]
    fn a_start_that_fails_waits_for_the_person_and_a_run_is_recorded() {
        let (_root, mut workspace, id) = queued_task();
        assert!(still_runnable(&workspace.state, &id));
        workspace
            .transaction(|state| mark_running(state, &id))
            .unwrap();
        assert_eq!(workspace.state.task(&id).unwrap().state, TaskState::Running);
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
        task.plan.ai_service = Some("removed".into());
        let preferences = desktop_prefs::preferences::Store::open(
            _root.path().join("preferences"),
            std::sync::Arc::new(desktop_prefs::credentials::MemoryCredentialVault::new()),
        );
        let control: serde_json::Value =
            serde_json::from_slice(&task_control(&preferences, &task).unwrap()).unwrap();
        assert_eq!(control["stopped_services"], serde_json::json!(["removed"]));
        assert_eq!(control["intent"], serde_json::json!("run"));
    }

    /// A task planned with a service that is no longer saved stops before it starts,
    /// offers no resume that could only stop again, leads with adjusting it, and the
    /// new attempt follows the current default while the task keeps its own version.
    #[test]
    fn a_task_whose_service_is_gone_goes_on_as_a_new_attempt_with_the_default() {
        use crate::task_view::{TaskAction, TaskFacts, TaskSurface, lead_action, task_actions_for};
        use desktop_prefs::preferences::{
            Authentication, BindingScope, ServiceDraft, ServiceProtocol, ServicePurpose, Store,
        };
        let (root, mut workspace, id) = queued_task();
        let mut preferences = Store::open(
            root.path().join("preferences"),
            std::sync::Arc::new(desktop_prefs::credentials::MemoryCredentialVault::new()),
        );
        let mut draft = ServiceDraft::new(ServicePurpose::Ai);
        draft.protocol = ServiceProtocol::OllamaChat;
        draft.authentication = Authentication::None;
        draft.address = "http://127.0.0.1:9".into();
        draft.model = "default-model".into();
        let draft = preferences.commit_service_draft(draft, None).unwrap();
        let verified = Store::verify_credential(
            preferences.vault().as_ref(),
            preferences.publication_credential(&draft.id).unwrap(),
        )
        .unwrap();
        let default = preferences
            .commit_publication(&draft.id, BindingScope::Defaults, verified)
            .unwrap();

        workspace
            .transaction(|state| {
                let task = state.task_mut(&id).unwrap();
                task.plan.config.llm.enabled = true;
                task.plan.ai_service = Some("service-version-gone".into());
                Ok(())
            })
            .unwrap();
        let task = workspace.state.task(&id).unwrap().clone();
        let error = needed_credentials(&preferences, &task).unwrap_err();
        assert_eq!(
            error.to_string(),
            c2m_i18n::tr!("任务使用的服务已不在设置中，请调整后重试并选择服务")
        );
        assert!(services_missing(&preferences, &task));
        fail_before_start(&mut workspace, &id, &error).unwrap();
        let task = workspace.state.task(&id).unwrap().clone();
        let facts = TaskFacts {
            active: false,
            expanded: false,
            library: None,
            needs_reassociation: false,
            queue: Vec::new(),
        };
        for surface in [TaskSurface::Home, TaskSurface::Card, TaskSurface::Menu] {
            let actions = task_actions_for(&task, surface, &facts, true);
            assert!(!actions.contains(&TaskAction::Resume));
            assert!(actions.contains(&TaskAction::Adjust));
        }
        let home = task_actions_for(&task, TaskSurface::Home, &facts, true);
        assert_eq!(
            lead_action(&home, &task, TaskSurface::Home),
            Some(&TaskAction::Adjust)
        );
        // With another way to recover offered too, the workbench keeps adjusting.
        let mut model = task.clone();
        model.events.model_required = true;
        let home = task_actions_for(&model, TaskSurface::Home, &facts, true);
        assert!(home.contains(&TaskAction::PrepareModel));
        assert!(home.contains(&TaskAction::Adjust));
        // With its service there, continuing stays the way on.
        let home = task_actions_for(&task, TaskSurface::Home, &facts, false);
        assert_eq!(
            lead_action(&home, &task, TaskSurface::Home),
            Some(&TaskAction::Resume)
        );

        crate::plan::adjust_task(&mut workspace.state, &preferences, &id, None).unwrap();
        let input = workspace.state.draft().unwrap();
        assert_eq!(input.retry_of.as_deref(), Some(id.as_str()));
        assert_eq!(input.ai_service, None);
        assert_eq!(
            preferences
                .plan_refs(&desktop_prefs::preferences::ServiceRefs {
                    asr: input.asr_service.clone(),
                    llm: input.ai_service.clone(),
                })
                .llm,
            Some(default.id.clone())
        );
        assert_eq!(
            workspace
                .state
                .task(&id)
                .unwrap()
                .plan
                .ai_service
                .as_deref(),
            Some("service-version-gone"),
            "the task keeps the version its requests were made with"
        );

        // While the services record cannot be read, nothing is forgotten.
        let services = root.path().join("preferences").join("services.json");
        std::fs::write(&services, b"{damaged").unwrap();
        std::fs::write(desktop_prefs::store::backup_path(&services), b"{damaged").unwrap();
        let unreadable = Store::open(
            root.path().join("preferences"),
            std::sync::Arc::new(desktop_prefs::credentials::MemoryCredentialVault::new()),
        );
        let mut input = workspace.state.draft().unwrap().clone();
        input.ai_service = Some("service-version-gone".into());
        assert!(!crate::plan::forget_missing_draft_services(
            &unreadable,
            &mut input
        ));
        assert_eq!(input.ai_service.as_deref(), Some("service-version-gone"));

        // A choice still saved is kept.
        let mut input = workspace.state.draft().unwrap().clone();
        input.ai_service = Some(default.id.clone());
        assert!(!crate::plan::forget_missing_draft_services(
            &preferences,
            &mut input
        ));
        assert_eq!(input.ai_service, Some(default.id));
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
