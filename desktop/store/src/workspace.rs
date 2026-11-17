//! Durable workspace state: registered libraries, the single workbench draft, reading
//! positions, and at most one in-place task per `(library_id, course_id)`.
use anyhow::{Context, Result, bail, ensure};
use c2m_artifact::artifact::{Outcome, Outcomes, Status};
use c2m_engine::execution::Component;
use c2m_protocol::progress::{Stage, StageStatus as EngineStageStatus};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub use crate::activity::{BlockedRequest, JobEvents, LOG_LINES};
pub use c2m_dispatch::dispatch::{Control, Intent};
pub use c2m_engine::execution::RunMode;
pub use desktop_prefs::store::{new_id, now};

pub const SCHEMA: u32 = 2;
/// Parsed subtitle text of a submitted task, kept in its work directory so the
/// workspace record stays small.
pub const TASK_SUBTITLE_FILE: &str = "subtitle-events.json";
/// Engine NDJSON stream persisted inside a task's work directory.
pub const TASK_EVENTS_FILE: &str = "events.jsonl";
/// Log lines retained on a completed task.
const FINISHED_TASK_LOG_LINES: usize = 50;

/// Deterministic course directory / course ID: `course-<first 32 hex chars of sha256(source_id)>`.
pub fn course_dir_name(source_id: &str) -> String {
    format!(
        "course-{}",
        &c2m_core::digest::digest(source_id.as_bytes())[..32]
    )
}

/// Deterministic task ID per `(library_id, course_id)`.
pub fn task_id_for(library_id: &str, course_id: &str) -> String {
    let key = format!("{library_id}:{course_id}");
    format!("task-{}", &c2m_core::digest::digest(key.as_bytes())[..32])
}

/// Canonical work directory `<library>/.course2md/work/<course_id>`.
pub fn work_dir_for(library_root: &Path, course_id: &str) -> PathBuf {
    library_root.join(".course2md/work").join(course_id)
}

/// Canonical note directory `<library>/<course_id>`.
pub fn note_dir_for(library_root: &Path, course_id: &str) -> PathBuf {
    library_root.join(course_id)
}

/// Where a draft or task's video comes from, as the import preview found it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Source {
    pub input: String,
    pub title: String,
    pub author: String,
    pub duration: f64,
    pub cover: Option<PathBuf>,
    pub cover_error: Option<String>,
    /// Stable extractor identity (including a part ID), or verified local content identity.
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub subtitles: c2m_source::subtitle::SubtitleEvidence,
    #[serde(default)]
    pub selected_subtitle: Option<c2m_source::subtitle::CachedSubtitle>,
    #[serde(default)]
    pub original_language: Option<String>,
    #[serde(default)]
    pub subtitle_request: Option<c2m_source::subtitle::SubtitleTrack>,
    #[serde(default)]
    pub subtitle_read_error: Option<c2m_source::subtitle::SubtitleReadError>,
    /// When an online video was published, as `YYYY-MM-DD`, when its site says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

impl Source {
    fn relocate(&mut self, old: &Path, new: &Path) {
        if !self.online {
            let mut path = PathBuf::from(&self.input);
            relocate_path(&mut path, old, new);
            self.input = path.display().to_string();
        }
        if let Some(path) = &mut self.cover {
            relocate_path(path, old, new);
        }
        if let Some(subtitle) = &mut self.selected_subtitle {
            relocate_path(&mut subtitle.path, old, new);
        }
        let tracks = match &mut self.subtitles {
            c2m_source::subtitle::SubtitleEvidence::Found { tracks, .. } => tracks.iter_mut(),
            _ => [].iter_mut(),
        };
        for track in tracks.chain(&mut self.subtitle_request) {
            if let c2m_source::subtitle::SubtitleOrigin::File { path } = &mut track.origin {
                relocate_path(path, old, new);
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LibraryLocation {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    #[serde(default)]
    pub previous_roots: Vec<PathBuf>,
}

impl LibraryLocation {
    pub fn label(&self) -> String {
        label_in(&self.name, c2m_i18n::i18n::lang())
    }
}

fn label_in(name: &str, lang: c2m_i18n::i18n::Lang) -> String {
    use c2m_i18n::i18n::{Lang, t_for};
    let default = "笔记库";
    let built_in = [Lang::En, Lang::ZhHans, Lang::ZhHant, Lang::Ja]
        .into_iter()
        .any(|each| t_for(default, each) == name);
    if built_in {
        t_for(default, lang).to_owned()
    } else {
        name.to_owned()
    }
}

/// The single workbench input form: the link or file, and what was read from it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Draft {
    #[serde(default = "default_draft_id")]
    pub id: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default = "default_true")]
    pub online: bool,
    #[serde(default)]
    pub input: String,
    /// Subtitle events live in a separate file beside the workspace record.
    #[serde(default, serialize_with = "serialize_draft_source")]
    pub source: Option<Source>,
    #[serde(default)]
    pub subtitle: Option<PathBuf>,
    #[serde(default)]
    pub scroll: f32,
    #[serde(default)]
    pub updated: u64,
}

fn default_draft_id() -> String {
    new_id("input")
}

fn default_true() -> bool {
    true
}

impl Draft {
    pub fn new(online: bool) -> Self {
        Self {
            id: new_id("input"),
            revision: 0,
            online,
            input: String::new(),
            source: None,
            subtitle: None,
            scroll: 0.,
            updated: now(),
        }
    }

    pub fn change_source(&mut self, input: String) {
        if input == self.input {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        self.input = input;
        self.source = None;
        self.subtitle = None;
        self.updated = now();
    }

    fn relocate(&mut self, old: &Path, new: &Path) {
        if let Some(source) = &mut self.source {
            let before = source.input.clone();
            source.relocate(old, new);
            if !source.online && before != source.input {
                self.input = source.input.clone();
            }
        } else if !self.online {
            let mut path = PathBuf::from(&self.input);
            relocate_path(&mut path, old, new);
            self.input = path.display().to_string();
        }
        if let Some(path) = &mut self.subtitle {
            relocate_path(path, old, new);
        }
    }
}

pub(crate) fn relocate_path(path: &mut PathBuf, old: &Path, new: &Path) {
    if !path.is_absolute() {
        return;
    }
    if let Ok(relative) = path.strip_prefix(old) {
        *path = new.join(relative);
    } else if let (Ok(canon_path), Ok(canon_old)) = (path.canonicalize(), old.canonicalize())
        && let Ok(relative) = canon_path.strip_prefix(canon_old)
    {
        *path = new.join(relative);
    }
}

fn serialize_draft_source<S: serde::Serializer>(
    source: &Option<Source>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match source {
        Some(source)
            if source
                .selected_subtitle
                .as_ref()
                .is_some_and(|subtitle| !subtitle.events.is_empty()) =>
        {
            let mut light = source.clone();
            if let Some(subtitle) = &mut light.selected_subtitle {
                subtitle.events = Vec::new();
            }
            Some(light).serialize(serializer)
        }
        source => source.serialize(serializer),
    }
}

/// The 5 task states.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Queued,
    Running,
    Paused,
    Failed,
    Completed,
}

pub type TaskState = TaskStatus;

impl TaskStatus {
    pub fn is_waiting(self) -> bool {
        matches!(self, Self::Queued | Self::Paused)
    }

    pub fn is_finished(self) -> bool {
        self == Self::Completed
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => c2m_i18n::tr!("等待处理"),
            Self::Running => c2m_i18n::tr!("正在生成笔记"),
            Self::Paused => c2m_i18n::tr!("已暂停"),
            Self::Failed => c2m_i18n::tr!("需要处理"),
            Self::Completed => c2m_i18n::tr!("笔记已生成"),
        }
    }
}

/// High-level pipeline stages tracked on a course task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKey {
    Source,
    Transcription,
    Screenshots,
    Proofreading,
    Translation,
    Summary,
    Publish,
}

impl StageKey {
    pub const ALL: [Self; 7] = [
        Self::Source,
        Self::Transcription,
        Self::Screenshots,
        Self::Proofreading,
        Self::Translation,
        Self::Summary,
        Self::Publish,
    ];

    pub fn from_engine_stage(stage: &Stage) -> Option<Self> {
        Some(match stage {
            Stage::Fetch | Stage::Download => Self::Source,
            Stage::Subtitle
            | Stage::Audio
            | Stage::ModelPrepare
            | Stage::ModelFile(_)
            | Stage::ModelLoad
            | Stage::ModelApple
            | Stage::Transcribe => Self::Transcription,
            Stage::Scenes | Stage::ScenesScan | Stage::ScenesExtract => Self::Screenshots,
            Stage::Llm => Self::Proofreading,
            Stage::Translate => Self::Translation,
            Stage::Summary => Self::Summary,
            Stage::Render | Stage::Exports => Self::Publish,
            Stage::Other(_) => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Source => c2m_i18n::tr!("读取课程"),
            Self::Transcription => c2m_i18n::tr!("语音转写"),
            Self::Screenshots => c2m_i18n::tr!("生成截图"),
            Self::Proofreading => c2m_i18n::tr!("AI 校对"),
            Self::Translation => c2m_i18n::tr!("AI 翻译"),
            Self::Summary => c2m_i18n::tr!("生成摘要"),
            Self::Publish => c2m_i18n::tr!("生成笔记"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagePhase {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageState {
    pub phase: StagePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Actionable failure summary attached to a `Failed` task.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_stages: Vec<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_model: Option<(c2m_config::config::AsrProvider, String)>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_settings: bool,
}

impl TaskError {
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Default::default()
        }
    }
}

/// A single course conversion task; at most one exists per `(library_id, course_id)`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: String,
    pub library_id: String,
    pub course_id: String,
    pub source_id: String,
    pub source: Source,
    pub title: String,
    #[serde(default)]
    pub title_from_probe: bool,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_events: Option<Vec<c2m_core::timeline::TranscriptEvent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_folder: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_dir: Option<PathBuf>,
    pub work_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_work_dir: Option<PathBuf>,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub run_mode: RunMode,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stages: BTreeMap<StageKey, StageState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TaskError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
    #[serde(default)]
    pub unread: bool,
    pub created: u64,
    pub updated: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcomes: Option<Outcomes>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub kept_video: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_edits: Vec<PathBuf>,
    #[serde(skip)]
    pub events: JobEvents,
}

pub type TaskRecord = Task;

impl Task {
    pub fn waiting(&self) -> bool {
        self.status.is_waiting()
    }

    /// Work directory currently holding saved progress on disk before lazy migration.
    pub fn effective_work_dir(&self) -> &Path {
        if let Some(legacy) = &self.legacy_work_dir
            && legacy.is_dir()
            && !self.work_dir.exists()
        {
            return legacy;
        }
        &self.work_dir
    }

    /// Optional components of a published partial note that still need completion.
    pub fn incomplete_components(&self) -> Vec<Component> {
        if let Some(outcomes) = &self.outcomes {
            return incomplete_components_from_outcomes(outcomes);
        }
        self.error
            .as_ref()
            .map(|error| error.failed_stages.clone())
            .unwrap_or_default()
    }

    pub fn apply_event(&mut self, event: &c2m_protocol::progress::Event) -> bool {
        use c2m_protocol::progress::Event;
        match event {
            Event::Log { message, .. } => {
                self.logs.push(message.clone());
                if self.logs.len() > LOG_LINES {
                    self.logs.drain(..self.logs.len() - LOG_LINES);
                }
            }
            Event::Stage { stage, status } => {
                self.updated = now();
                if let Some(key) = StageKey::from_engine_stage(stage) {
                    let phase = match status {
                        EngineStageStatus::Start => StagePhase::Running,
                        EngineStageStatus::Done => StagePhase::Completed,
                    };
                    self.stages.entry(key).or_default().phase = phase;
                }
            }
            Event::Error { .. } | Event::Blocked { .. } => {
                self.updated = now();
            }
            Event::Done(done) => {
                self.updated = now();
                if !done.kept_edits.is_empty() {
                    self.kept_edits.clone_from(&done.kept_edits);
                }
            }
            _ => {}
        }
        self.events.apply(event)
    }

    pub fn sync_stages_from_outcomes(&mut self, outcomes: &Outcomes, partial: bool) {
        let mut set_outcome =
            |key: StageKey, outcome: &Outcome, part: Option<crate::activity::NotePart>| {
                match outcome.status {
                    Status::Succeeded => {
                        self.stages.insert(
                            key,
                            StageState {
                                phase: StagePhase::Completed,
                                detail: None,
                            },
                        );
                    }
                    Status::Failed | Status::Partial => {
                        let detail = match part {
                            Some(part) => {
                                Some(crate::activity::outcome_failure_message(part, outcome))
                            }
                            None => outcome.message.clone(),
                        };
                        self.stages.insert(
                            key,
                            StageState {
                                phase: StagePhase::Failed,
                                detail,
                            },
                        );
                    }
                    Status::NotRequested => {}
                }
            };
        set_outcome(StageKey::Transcription, &outcomes.transcript, None);
        set_outcome(
            StageKey::Screenshots,
            &outcomes.screenshots,
            Some(crate::activity::NotePart::Screenshots),
        );
        set_outcome(
            StageKey::Proofreading,
            &outcomes.proofreading,
            Some(crate::activity::NotePart::Proofreading),
        );
        set_outcome(
            StageKey::Translation,
            &outcomes.translation,
            Some(crate::activity::NotePart::Translation),
        );
        set_outcome(
            StageKey::Summary,
            &outcomes.summary,
            Some(crate::activity::NotePart::Summary),
        );
        let export_failures: Vec<String> = outcomes
            .exports
            .values()
            .filter(|outcome| outcome.is_incomplete())
            .filter_map(|outcome| outcome.message.clone())
            .collect();
        if !export_failures.is_empty() {
            self.stages.insert(
                StageKey::Publish,
                StageState {
                    phase: StagePhase::Failed,
                    detail: Some(export_failures.join(c2m_i18n::tr!("；"))),
                },
            );
        } else if !partial || outcomes.transcript.status == Status::Succeeded {
            self.stages.insert(
                StageKey::Publish,
                StageState {
                    phase: StagePhase::Completed,
                    detail: None,
                },
            );
        }
    }

    pub fn trim_finished_logs(&mut self) {
        if self.logs.len() > FINISHED_TASK_LOG_LINES {
            self.logs.drain(..self.logs.len() - FINISHED_TASK_LOG_LINES);
        }
    }

    fn relocate(&mut self, old: &Path, new: &Path) {
        relocate_path(&mut self.work_dir, old, new);
        if let Some(legacy) = &mut self.legacy_work_dir {
            relocate_path(legacy, old, new);
        }
        if let Some(note) = &mut self.note_dir {
            relocate_path(note, old, new);
        }
        if let Some(sub) = &mut self.subtitle {
            relocate_path(sub, old, new);
        }
        self.source.relocate(old, new);
    }
}

pub fn incomplete_components_from_outcomes(outcomes: &Outcomes) -> Vec<Component> {
    let mut components = Vec::new();
    for (component, outcome) in [
        (Component::Screenshots, &outcomes.screenshots),
        (Component::Proofreading, &outcomes.proofreading),
        (Component::Translation, &outcomes.translation),
        (Component::Summary, &outcomes.summary),
    ] {
        if outcome.is_incomplete() {
            components.push(component);
        }
    }
    if outcomes.exports.values().any(Outcome::is_incomplete) {
        components.push(Component::Exports);
    }
    components
}

/// Inputs for submitting or updating a course conversion task.
#[derive(Clone, Debug, PartialEq)]
pub struct NewTask {
    pub library_id: String,
    pub source: Source,
    pub source_id: String,
    pub title: String,
    pub title_from_probe: bool,
    pub subtitle: Option<PathBuf>,
    pub subtitle_events: Option<Vec<c2m_core::timeline::TranscriptEvent>>,
    pub target_folder: Option<u64>,
    pub note_dir: Option<PathBuf>,
    pub run_mode: RunMode,
}

pub type TaskPlan = NewTask;

impl NewTask {
    pub fn from_source(
        library_id: impl Into<String>,
        source: Source,
        title: impl Into<String>,
        run_mode: RunMode,
    ) -> Self {
        let source_id = source.identity.clone();
        let title_from_probe = source.online && source.title.trim().is_empty();
        let subtitle_events = source
            .selected_subtitle
            .as_ref()
            .filter(|sub| !sub.events.is_empty())
            .map(|sub| sub.events.clone());
        Self {
            library_id: library_id.into(),
            source_id,
            source,
            title: title.into(),
            title_from_probe,
            subtitle: None,
            subtitle_events,
            target_folder: None,
            note_dir: None,
            run_mode,
        }
    }
}

/// Scratch directory cleanup descriptor for a cancelled, completed, or cleared task.
/// Never touches any published `note_dir`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClearedTaskWork {
    pub work_dir: PathBuf,
    pub legacy_work_dir: Option<PathBuf>,
    pub keep_offline_media: bool,
}

impl ClearedTaskWork {
    pub fn from_task(task: &Task, preserve_offline_media: bool) -> Self {
        Self {
            work_dir: task.work_dir.clone(),
            legacy_work_dir: task.legacy_work_dir.clone(),
            keep_offline_media: preserve_offline_media
                && task.note_dir.is_some()
                && task.kept_video,
        }
    }
}

fn is_safe_work_dir(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(c2m_engine::execution::valid_id)
        && dir
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == "work")
        && dir
            .parent()
            .and_then(|parent| parent.parent())
            .and_then(|dot| dot.file_name())
            .is_some_and(|name| name == ".course2md")
}

fn clean_single_work_dir(dir: &Path, keep_offline_media: bool) {
    if !is_safe_work_dir(dir) {
        return;
    }
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return;
    }
    if keep_offline_media && dir.join("media.mp4").is_file() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name == "media.mp4" || name == "media.sha256" {
                    continue;
                }
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    let _ = std::fs::remove_dir_all(path);
                } else {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        return;
    }
    let _ = std::fs::remove_dir_all(dir);
}

pub fn clean_cleared_task_work(item: &ClearedTaskWork) {
    clean_single_work_dir(&item.work_dir, item.keep_offline_media);
    if let Some(legacy) = &item.legacy_work_dir
        && legacy != &item.work_dir
    {
        clean_single_work_dir(legacy, item.keep_offline_media);
    }
}

/// The note folder a task writes inside `location`.
pub fn task_note(task: &Task, location: &LibraryLocation) -> PathBuf {
    note_dir_for(&location.root, &task.course_id)
}

fn task_export_directory(task: &Task, location: &LibraryLocation) -> Option<PathBuf> {
    (task.library_id == location.id).then(|| task_note(task, location).join("exports"))
}

/// Successful export paths derived purely from durable in-memory outcomes (zero disk I/O).
pub fn task_export_files(task: &Task, location: &LibraryLocation) -> Vec<PathBuf> {
    if task.note_dir.is_none() || matches!(task.status, TaskStatus::Queued | TaskStatus::Running) {
        return Vec::new();
    }
    let Some(outcomes) = task.outcomes.as_ref() else {
        return Vec::new();
    };
    let Some(directory) = task_export_directory(task, location) else {
        return Vec::new();
    };
    use c2m_config::config::OutputFormat;
    [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json]
        .into_iter()
        .filter(|format| {
            outcomes
                .exports
                .get(&format.to_string())
                .is_some_and(|outcome| outcome.status == Status::Succeeded)
        })
        .map(|format| directory.join(c2m_artifact::portable::file_name(format)))
        .collect()
}

pub fn available_task_export(task: &Task, location: &LibraryLocation) -> Result<PathBuf> {
    task_export_files(task, location)
        .into_iter()
        .find(|path| path.is_file())
        .context(c2m_i18n::tr!(
            "导出文件已移动或无法读取，可打开笔记后从「导出」菜单重新保存。"
        ))
}

/// Saves inline parsed subtitle events into `<work_dir>/subtitle-events.json`. Blocking.
pub fn store_task_subtitles(task: &Task) -> Result<()> {
    let events = task.subtitle_events.as_deref().or_else(|| {
        task.source
            .selected_subtitle
            .as_ref()
            .map(|sub| sub.events.as_slice())
    });
    if let Some(events) = events.filter(|events| !events.is_empty()) {
        std::fs::create_dir_all(&task.work_dir)?;
        c2m_core::fsio::atomic_write(
            &task.work_dir.join(TASK_SUBTITLE_FILE),
            &serde_json::to_vec(events)?,
        )?;
    }
    Ok(())
}

/// Clears inline subtitle event arrays from memory once written to `<work_dir>/subtitle-events.json`.
pub fn forget_task_subtitles(task: &mut Task) {
    task.subtitle_events = None;
    if let Some(subtitle) = &mut task.source.selected_subtitle {
        subtitle.events.clear();
    }
}

/// Reads parsed subtitle events for a task (inline first, then `<work_dir>/subtitle-events.json`).
pub fn task_subtitle_events(
    task: &Task,
) -> Result<Option<Vec<c2m_core::timeline::TranscriptEvent>>> {
    if let Some(events) = task
        .subtitle_events
        .as_ref()
        .filter(|events| !events.is_empty())
    {
        return Ok(Some(events.clone()));
    }
    if let Some(selected) = &task.source.selected_subtitle
        && !selected.events.is_empty()
    {
        return Ok(Some(selected.events.clone()));
    }
    let has_selected_track = task.source.selected_subtitle.is_some();
    for dir in [Some(&task.work_dir), task.legacy_work_dir.as_ref()]
        .into_iter()
        .flatten()
    {
        let path = dir.join(TASK_SUBTITLE_FILE);
        if path.is_file() {
            return Ok(Some(serde_json::from_slice(&desktop_prefs::store::read(
                &path,
            )?)?));
        }
    }
    if has_selected_track {
        bail!(c2m_i18n::tr!("所选字幕的内容已丢失，请重新读取字幕后再试"));
    }
    Ok(None)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ReadingPosition {
    pub paragraph: Option<String>,
    pub seconds: Option<f64>,
    pub offset: f32,
    #[serde(default)]
    pub within: f32,
    #[serde(default)]
    pub fraction: Option<f32>,
}

#[derive(Clone, Serialize, PartialEq)]
pub struct State {
    pub schema: u32,
    /// In-memory change counter.
    pub revision: u64,
    pub libraries: Vec<LibraryLocation>,
    pub default_library: String,
    pub drafts: Vec<Draft>,
    pub current_draft: String,
    pub tasks: Vec<Task>,
    pub selected_task: Option<String>,
    pub positions: BTreeMap<String, ReadingPosition>,
    pub reader_sources: BTreeMap<String, PathBuf>,
    pub collapsed: BTreeSet<String>,
    pub storage_backups: Vec<crate::storage::BackupRecord>,
    /// Startup reconciliation has not been applied yet; nothing is dispatched.
    #[serde(skip)]
    pub reconciling: bool,
    /// In-memory counter of live progress updates.
    #[serde(skip)]
    pub progress: u64,
}

impl State {
    pub fn initial(root: PathBuf) -> Self {
        let library = LibraryLocation {
            id: new_id("library"),
            name: c2m_i18n::tr!("笔记库").into(),
            root,
            previous_roots: Vec::new(),
        };
        let draft = Draft::new(true);
        Self {
            schema: SCHEMA,
            revision: 0,
            default_library: library.id.clone(),
            libraries: vec![library],
            current_draft: draft.id.clone(),
            drafts: vec![draft],
            tasks: Vec::new(),
            selected_task: None,
            positions: BTreeMap::new(),
            reader_sources: BTreeMap::new(),
            collapsed: BTreeSet::new(),
            storage_backups: Vec::new(),
            reconciling: false,
            progress: 0,
        }
    }

    fn adopt_marker(&mut self, root: &Path) -> bool {
        let Ok(marker) = std::fs::read_to_string(root.join(".course2md-library-id")) else {
            return false;
        };
        if c2m_engine::execution::valid_id(marker.trim()) {
            self.default_library = marker.trim().into();
            self.libraries[0].id = self.default_library.clone();
        }
        true
    }

    pub fn draft(&self) -> Option<&Draft> {
        self.drafts.iter().find(|d| d.id == self.current_draft)
    }

    pub fn draft_mut(&mut self) -> Option<&mut Draft> {
        self.drafts.iter_mut().find(|d| d.id == self.current_draft)
    }

    pub fn matches_input(&self, id: &str, revision: u64) -> bool {
        self.draft()
            .is_some_and(|input| input.id == id && input.revision == revision)
    }

    fn replace_input(&mut self, input: Draft) {
        self.current_draft = input.id.clone();
        self.drafts = vec![input];
    }

    pub fn reset_input(&mut self, online: bool) -> String {
        self.replace_input(Draft::new(online));
        self.current_draft.clone()
    }

    pub fn switch_source_kind(&mut self, online: bool) {
        if self
            .draft()
            .is_none_or(|previous| previous.online != online)
        {
            self.reset_input(online);
        }
    }

    pub fn library(&self, id: &str) -> Option<&LibraryLocation> {
        self.libraries.iter().find(|l| l.id == id)
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn task_for_course(&self, library_id: &str, course_id: &str) -> Option<&Task> {
        self.tasks
            .iter()
            .find(|t| t.library_id == library_id && t.course_id == course_id)
    }

    pub fn task_for_source(&self, library_id: &str, source_id: &str) -> Option<&Task> {
        let course_id = course_dir_name(source_id);
        self.task_for_course(library_id, &course_id)
    }

    pub fn apply_task_event(&mut self, id: &str, event: &c2m_protocol::progress::Event) -> bool {
        let Some(task) = self.task_mut(id) else {
            return false;
        };
        let visible = task.apply_event(event);
        self.progress = self.progress.wrapping_add(1);
        visible
    }

    pub fn next_task(&self) -> Option<&Task> {
        if self.reconciling {
            return None;
        }
        self.tasks.iter().find(|t| t.status == TaskStatus::Queued)
    }

    pub fn queued_tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Queued)
    }

    pub fn waiting_tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(|task| task.waiting())
    }

    /// Enqueues or updates in place the single task for `(library_id, course_id)`.
    pub fn submit_conversion(&mut self, input: NewTask) -> Result<String> {
        self.enqueue(input)
    }

    /// Enqueues or updates in place the single task for `(library_id, course_id)`.
    pub fn enqueue(&mut self, input: NewTask) -> Result<String> {
        ensure!(
            !input.source_id.trim().is_empty() && !input.title.trim().is_empty(),
            c2m_i18n::tr!("请先确认视频和笔记名称")
        );
        let location = self
            .library(&input.library_id)
            .context(c2m_i18n::tr!("所选保存位置未登记"))?
            .clone();
        let course_id = course_dir_name(&input.source_id);
        let work_dir = work_dir_for(&location.root, &course_id);
        let now = now();
        let subtitle_events = input.subtitle_events.or_else(|| {
            input
                .source
                .selected_subtitle
                .as_ref()
                .filter(|sub| !sub.events.is_empty())
                .map(|sub| sub.events.clone())
        });

        if let Some(existing) = self
            .tasks
            .iter_mut()
            .find(|t| t.library_id == input.library_id && t.course_id == course_id)
        {
            ensure!(
                existing.status != TaskStatus::Running,
                c2m_i18n::tr!("这项任务正在处理中")
            );
            existing.author = input.source.author.clone();
            existing.duration = input.source.duration;
            existing.source = input.source;
            existing.source_id = input.source_id;
            existing.title = input.title;
            existing.title_from_probe = input.title_from_probe;
            existing.subtitle = input.subtitle;
            existing.subtitle_events = subtitle_events;
            if input.target_folder.is_some() {
                existing.target_folder = input.target_folder;
            }
            if input.note_dir.is_some() {
                existing.note_dir = input.note_dir;
            }
            existing.work_dir = work_dir;
            existing.run_mode = input.run_mode;
            existing.status = TaskStatus::Queued;
            existing.error = None;
            existing.unread = false;
            existing.updated = now;
            reset_stages_for_mode(&mut existing.stages, &mut existing.outcomes, input.run_mode);
            let id = existing.id.clone();
            self.selected_task = Some(id.clone());
            return Ok(id);
        }

        let id = task_id_for(&input.library_id, &course_id);
        self.tasks.push(Task {
            id: id.clone(),
            library_id: input.library_id,
            course_id,
            author: input.source.author.clone(),
            duration: input.source.duration,
            source_id: input.source_id,
            source: input.source,
            title: input.title,
            title_from_probe: input.title_from_probe,
            subtitle: input.subtitle,
            subtitle_events,
            target_folder: input.target_folder,
            note_dir: input.note_dir,
            work_dir,
            legacy_work_dir: None,
            status: TaskStatus::Queued,
            run_mode: input.run_mode,
            stages: BTreeMap::new(),
            error: None,
            logs: Vec::new(),
            unread: false,
            created: now,
            updated: now,
            outcomes: None,
            kept_video: false,
            kept_edits: Vec::new(),
            events: JobEvents {
                loaded: true,
                ..Default::default()
            },
        });
        self.selected_task = Some(id.clone());
        Ok(id)
    }

    /// Mutates a non-running task in place to retry (`Resume`) or regenerate (`Fresh`).
    pub fn retry_task(&mut self, id: &str, mode: RunMode) -> Result<()> {
        let task = self.task_mut(id).context(c2m_i18n::tr!("任务记录不存在"))?;
        ensure!(
            task.status != TaskStatus::Running,
            c2m_i18n::tr!("这项任务正在处理中")
        );
        task.error = None;
        task.run_mode = mode;
        task.status = TaskStatus::Queued;
        task.unread = false;
        task.updated = now();
        reset_stages_for_mode(&mut task.stages, &mut task.outcomes, mode);
        if mode == RunMode::Fresh {
            task.events = JobEvents {
                loaded: true,
                ..Default::default()
            };
        } else {
            task.events.begin_run();
        }
        self.selected_task = Some(id.to_owned());
        Ok(())
    }

    /// Pauses a `Queued` or `Running` task. Returns `true` if it was `Running` so the
    /// caller can write `Intent::Pause` to `control.json`.
    pub fn pause_task(&mut self, id: &str) -> Result<bool> {
        let task = self.task_mut(id).context(c2m_i18n::tr!("任务记录不存在"))?;
        ensure!(
            matches!(task.status, TaskStatus::Queued | TaskStatus::Running),
            c2m_i18n::tr!("这项任务的状态已变化，请查看最新状态")
        );
        let was_running = task.status == TaskStatus::Running;
        task.status = TaskStatus::Paused;
        task.updated = now();
        Ok(was_running)
    }

    /// Resumes a `Paused` task in place.
    pub fn resume_task(&mut self, id: &str) -> Result<()> {
        let task = self.task_mut(id).context(c2m_i18n::tr!("任务记录不存在"))?;
        ensure!(
            task.status == TaskStatus::Paused,
            c2m_i18n::tr!("这项任务的状态已变化，请查看最新状态")
        );
        task.status = TaskStatus::Queued;
        task.error = None;
        task.unread = false;
        task.updated = now();
        Ok(())
    }

    /// Pauses all `Queued` and `Running` tasks, returning `(task_id, was_running)` pairs.
    pub fn pause_all(&mut self) -> Vec<(String, bool)> {
        let now = now();
        let mut paused = Vec::new();
        for task in &mut self.tasks {
            if matches!(task.status, TaskStatus::Queued | TaskStatus::Running) {
                let was_running = task.status == TaskStatus::Running;
                task.status = TaskStatus::Paused;
                task.updated = now;
                paused.push((task.id.clone(), was_running));
            }
        }
        paused
    }

    /// Resumes all `Paused` tasks in queue order, returning their IDs.
    pub fn resume_all(&mut self) -> Vec<String> {
        let now = now();
        let mut resumed = Vec::new();
        for task in &mut self.tasks {
            if task.status == TaskStatus::Paused {
                task.status = TaskStatus::Queued;
                task.error = None;
                task.unread = false;
                task.updated = now;
                resumed.push(task.id.clone());
            }
        }
        resumed
    }

    /// Cancels or removes a task immediately, returning `(ClearedTaskWork, was_running)`.
    /// Any published note in `note_dir` is left untouched.
    pub fn cancel_or_remove_task(&mut self, id: &str) -> Result<(ClearedTaskWork, bool)> {
        let index = self
            .tasks
            .iter()
            .position(|t| t.id == id)
            .context(c2m_i18n::tr!("任务记录不存在"))?;
        let removed = self.tasks.remove(index);
        let was_running = removed.status == TaskStatus::Running;
        if self.selected_task.as_deref() == Some(id) {
            self.selected_task = self.tasks.last().map(|t| t.id.clone());
        }
        // Preserve retained offline media when a published note already exists on disk.
        let cleared = ClearedTaskWork::from_task(&removed, true);
        Ok((cleared, was_running))
    }

    /// Removes a non-running task record, returning its `ClearedTaskWork`.
    pub fn remove_task_record(&mut self, id: &str) -> Result<ClearedTaskWork> {
        let task = self.task(id).context(c2m_i18n::tr!("任务记录不存在"))?;
        ensure!(
            task.status != TaskStatus::Running,
            c2m_i18n::tr!("这项任务正在处理中")
        );
        Ok(self.cancel_or_remove_task(id)?.0)
    }

    /// Removes all `Completed` tasks, returning their `ClearedTaskWork` descriptors
    /// while never touching `note_dir`.
    pub fn clear_completed_tasks(&mut self) -> Vec<ClearedTaskWork> {
        let mut cleared = Vec::new();
        let mut removed_ids = BTreeSet::new();
        self.tasks.retain(|task| {
            if task.status == TaskStatus::Completed {
                removed_ids.insert(task.id.clone());
                cleared.push(ClearedTaskWork::from_task(task, true));
                false
            } else {
                true
            }
        });
        if self
            .selected_task
            .as_deref()
            .is_some_and(|selected| removed_ids.contains(selected))
        {
            self.selected_task = self.tasks.last().map(|t| t.id.clone());
        }
        cleared
    }

    /// Waiting tasks (`Queued | Paused`) run in list order. Moves `id` to `new_position`
    /// among waiting tasks.
    pub fn move_queued_task(&mut self, id: &str, new_position: usize) -> Result<()> {
        let from = self
            .tasks
            .iter()
            .position(|task| task.id == id)
            .context(c2m_i18n::tr!("任务记录不存在"))?;
        ensure!(
            self.tasks[from].waiting(),
            c2m_i18n::tr!("只能调整排队中的任务")
        );
        let task = self.tasks.remove(from);
        let waiting: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| task.waiting())
            .map(|(index, _)| index)
            .collect();
        let at = match waiting.get(new_position) {
            Some(&index) => index,
            None => waiting.last().map_or(self.tasks.len(), |&index| index + 1),
        };
        self.tasks.insert(at, task);
        Ok(())
    }

    pub fn move_waiting_task(&mut self, id: &str, position: usize) -> Result<()> {
        self.move_queued_task(id, position)
    }

    /// Normalizes process-owned `Running` states after a restart.
    pub fn recover(&mut self) {
        let roots: BTreeMap<_, _> = self
            .libraries
            .iter()
            .map(|lib| (lib.id.clone(), lib.root.clone()))
            .collect();
        for task in &mut self.tasks {
            if let Some(root) = roots.get(&task.library_id) {
                let expected = work_dir_for(root, &task.course_id);
                if task.work_dir != expected {
                    if task.legacy_work_dir.is_none() && !task.work_dir.as_os_str().is_empty() {
                        task.legacy_work_dir = Some(task.work_dir.clone());
                    }
                    task.work_dir = expected;
                }
            }
            if task.status == TaskStatus::Running {
                task.status = TaskStatus::Queued;
                task.updated = now();
            }
        }
    }

    pub fn stop_session(&mut self) {
        let now = now();
        for task in &mut self.tasks {
            if task.status == TaskStatus::Running {
                task.status = TaskStatus::Paused;
                task.updated = now;
            }
        }
    }

    /// Rewrites saved paths inside a moved or relinked library.
    pub(crate) fn rebind_library(
        &mut self,
        library_id: &str,
        old_roots: &[PathBuf],
        destination: &Path,
    ) -> Result<()> {
        ensure!(
            self.library(library_id).is_some(),
            c2m_i18n::tr!("这个笔记库已被移除")
        );
        for old in old_roots {
            for path in self.reader_sources.values_mut() {
                relocate_path(path, old, destination);
            }
            for draft in &mut self.drafts {
                draft.relocate(old, destination);
            }
            for task in &mut self.tasks {
                task.relocate(old, destination);
            }
        }
        for backup in &mut self.storage_backups {
            if backup.library_id == library_id {
                backup.current_root = destination.to_owned();
            }
        }
        let location = self
            .libraries
            .iter_mut()
            .find(|library| library.id == library_id)
            .context(c2m_i18n::tr!("这个笔记库已被移除"))?;
        for old in old_roots {
            if !location.previous_roots.contains(old) {
                location.previous_roots.push(old.clone());
            }
        }
        location.root = destination.to_owned();
        for task in &mut self.tasks {
            if task.library_id == library_id {
                task.work_dir = work_dir_for(destination, &task.course_id);
            }
        }
        Ok(())
    }
}

fn reset_stages_for_mode(
    stages: &mut BTreeMap<StageKey, StageState>,
    outcomes: &mut Option<Outcomes>,
    mode: RunMode,
) {
    match mode {
        RunMode::Fresh => {
            stages.clear();
            *outcomes = None;
        }
        RunMode::Resume => {
            for stage in stages.values_mut() {
                if stage.phase != StagePhase::Completed {
                    stage.phase = StagePhase::Pending;
                    stage.detail = None;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Seamless Deserialization & v1 / legacy-v2 Migration
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct StateWire {
    schema: u32,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    libraries: Vec<LibraryWire>,
    #[serde(default)]
    default_library: String,
    #[serde(default)]
    drafts: Vec<Draft>,
    #[serde(default)]
    current_draft: String,
    #[serde(default)]
    tasks: Vec<serde_json::Value>,
    #[serde(default)]
    selected_task: Option<String>,
    #[serde(default)]
    positions: BTreeMap<String, ReadingPosition>,
    #[serde(default)]
    reader_sources: BTreeMap<String, PathBuf>,
    #[serde(default)]
    collapsed: BTreeSet<String>,
    #[serde(default)]
    storage_backups: Vec<crate::storage::BackupRecord>,
}

#[derive(Deserialize)]
struct LibraryWire {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    root: PathBuf,
    #[serde(default)]
    previous_roots: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct LegacyTaskWire {
    id: String,
    #[serde(default)]
    plan: LegacyPlanWire,
    #[serde(default)]
    state: String,
    #[serde(default)]
    intent: Option<Intent>,
    #[serde(default)]
    created: u64,
    #[serde(default)]
    updated: u64,
    #[serde(default)]
    handled_by: Option<String>,
    #[serde(default)]
    replaced_by: Option<String>,
    #[serde(default)]
    artifact: Option<PathBuf>,
    #[serde(default)]
    note_dir: Option<PathBuf>,
    #[serde(default)]
    work_dir: Option<PathBuf>,
    #[serde(default)]
    unread: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    logs: Vec<String>,
    #[serde(default)]
    events: LegacyEventsWire,
    #[serde(default)]
    ran_with: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
struct LegacyPlanWire {
    #[serde(default)]
    source: Source,
    #[serde(default)]
    source_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    library_id: String,
    #[serde(default)]
    folder: Option<u64>,
    #[serde(default)]
    subtitle: Option<PathBuf>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

#[derive(Default, Deserialize)]
struct LegacyEventsWire {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    logs: Vec<String>,
}

impl<'de> Deserialize<'de> for State {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = StateWire::deserialize(deserializer)?;
        migrate_state_wire(wire).map_err(serde::de::Error::custom)
    }
}

fn migrate_state_wire(wire: StateWire) -> Result<State> {
    ensure!(
        wire.schema >= 1 && wire.schema <= SCHEMA,
        c2m_i18n::tr!("任务记录版本不受支持，请使用创建这些任务的应用版本")
    );
    let libraries: Vec<LibraryLocation> = wire
        .libraries
        .into_iter()
        .map(|lib| {
            let id = if !lib.id.trim().is_empty() && c2m_engine::execution::valid_id(lib.id.trim())
            {
                lib.id.trim().to_owned()
            } else {
                std::fs::read_to_string(lib.root.join(crate::storage::LIBRARY_ID_FILE))
                    .ok()
                    .map(|m| m.trim().to_owned())
                    .filter(|m| c2m_engine::execution::valid_id(m))
                    .unwrap_or_else(|| new_id("library"))
            };
            let name = if lib.name.trim().is_empty() {
                c2m_i18n::tr!("笔记库").to_owned()
            } else {
                lib.name
            };
            LibraryLocation {
                id,
                name,
                root: lib.root,
                previous_roots: lib.previous_roots,
            }
        })
        .collect();

    let default_library = if libraries.iter().any(|l| l.id == wire.default_library) {
        wire.default_library
    } else if wire.schema == 1
        && let Some(first) = libraries.first()
    {
        first.id.clone()
    } else {
        wire.default_library
    };

    let (drafts, current_draft) = if wire.drafts.iter().any(|d| d.id == wire.current_draft) {
        (wire.drafts, wire.current_draft)
    } else if wire.schema == 1 {
        let draft = wire
            .drafts
            .into_iter()
            .next()
            .unwrap_or_else(|| Draft::new(true));
        let id = draft.id.clone();
        (vec![draft], id)
    } else {
        (wire.drafts, wire.current_draft)
    };

    let tasks = migrate_tasks(wire.tasks, &libraries, &default_library)?;
    let selected_task = wire
        .selected_task
        .filter(|id| tasks.iter().any(|t| &t.id == id));

    Ok(State {
        schema: SCHEMA,
        revision: wire.revision,
        libraries,
        default_library,
        drafts,
        current_draft,
        tasks,
        selected_task,
        positions: wire.positions,
        reader_sources: wire.reader_sources,
        collapsed: wire.collapsed,
        storage_backups: wire.storage_backups,
        reconciling: false,
        progress: 0,
    })
}

fn is_legacy_task_json(value: &serde_json::Value) -> bool {
    value.get("plan").is_some()
        || (value.get("state").is_some() && value.get("course_id").is_none())
}

fn flatten_legacy_note_path(path: PathBuf) -> PathBuf {
    // `<note>/versions/<ver>` -> `<note>`
    if let Some(versions) = path.parent()
        && versions.file_name().and_then(|n| n.to_str()) == Some("versions")
        && let Some(note) = versions.parent()
    {
        return note.to_path_buf();
    }
    path
}

fn migrate_tasks(
    raw_tasks: Vec<serde_json::Value>,
    libraries: &[LibraryLocation],
    default_library: &str,
) -> Result<Vec<Task>> {
    let roots: BTreeMap<&str, &Path> = libraries
        .iter()
        .map(|l| (l.id.as_str(), l.root.as_path()))
        .collect();

    // First collect all parent pointers from legacy records so superseded tasks
    // without an explicit serialized `handled_by` are discarded too.
    let mut superseded_by_parent = BTreeSet::new();
    for raw in &raw_tasks {
        if let Some(parent) = raw.get("parent").and_then(serde_json::Value::as_str)
            && !parent.is_empty()
            && raw.get("id").and_then(serde_json::Value::as_str) != Some(parent)
        {
            superseded_by_parent.insert(parent.to_owned());
        }
    }

    let mut by_course: Vec<Task> = Vec::new();
    for raw in raw_tasks {
        let task = if is_legacy_task_json(&raw) {
            let legacy: LegacyTaskWire = serde_json::from_value(raw)?;
            let Some(migrated) =
                migrate_legacy_task(legacy, &roots, default_library, &superseded_by_parent)
            else {
                continue;
            };
            migrated
        } else {
            serde_json::from_value::<Task>(raw)?
        };

        if let Some(existing) = by_course
            .iter_mut()
            .find(|t| t.library_id == task.library_id && t.course_id == task.course_id)
        {
            // Keep the newest record per `(library_id, course_id)` while preserving an
            // earlier `note_dir` if the newer attempt had not published yet.
            let prev_note = existing.note_dir.clone();
            if (task.updated, task.created) >= (existing.updated, existing.created) {
                *existing = task;
                if existing.note_dir.is_none() {
                    existing.note_dir = prev_note;
                }
            } else if existing.note_dir.is_none() {
                existing.note_dir = task.note_dir;
            }
        } else {
            by_course.push(task);
        }
    }
    Ok(by_course)
}

fn migrate_legacy_task(
    legacy: LegacyTaskWire,
    roots: &BTreeMap<&str, &Path>,
    default_library: &str,
    superseded_by_parent: &BTreeSet<String>,
) -> Option<Task> {
    if legacy.handled_by.is_some()
        || legacy.replaced_by.is_some()
        || superseded_by_parent.contains(&legacy.id)
    {
        return None;
    }

    let note_dir = legacy
        .note_dir
        .or(legacy.artifact)
        .map(flatten_legacy_note_path);
    let intent = legacy.intent.unwrap_or(Intent::Run);
    let status = match legacy.state.as_str() {
        "cancelled" => {
            note_dir.as_ref()?;
            TaskStatus::Completed
        }
        "running" | "pausing" | "queued" if intent == Intent::Cancel => {
            note_dir.as_ref()?;
            TaskStatus::Completed
        }
        "complete" | "completed" => TaskStatus::Completed,
        "partial" | "needs_attention" | "uncertain" | "failed" => TaskStatus::Failed,
        "paused" | "pausing" => TaskStatus::Paused,
        "running" | "queued" | "" => {
            if matches!(intent, Intent::Pause | Intent::Quit) {
                TaskStatus::Paused
            } else {
                TaskStatus::Queued
            }
        }
        _ => TaskStatus::Paused,
    };

    let library_id = if !legacy.plan.library_id.trim().is_empty() {
        legacy.plan.library_id.clone()
    } else {
        default_library.to_owned()
    };
    let mut source = legacy.plan.source;
    let source_id = if !legacy.plan.source_id.trim().is_empty() {
        legacy.plan.source_id
    } else if !source.identity.trim().is_empty() {
        source.identity.clone()
    } else {
        source.input.clone()
    };
    if source_id.trim().is_empty() {
        return None;
    }
    if source.identity.is_empty() {
        source.identity = source_id.clone();
    }
    let title = if !legacy.plan.title.trim().is_empty() {
        legacy.plan.title
    } else if !source.title.trim().is_empty() {
        source.title.clone()
    } else {
        source.input.clone()
    };
    if title.trim().is_empty() {
        return None;
    }

    let course_id = course_dir_name(&source_id);
    let id = if c2m_engine::execution::valid_id(&legacy.id) {
        legacy.id.clone()
    } else {
        task_id_for(&library_id, &course_id)
    };
    let canonical_work = roots
        .get(library_id.as_str())
        .map(|root| work_dir_for(root, &course_id))
        .unwrap_or_else(|| PathBuf::from(&course_id));
    let old_work = legacy.work_dir.or_else(|| {
        roots
            .get(library_id.as_str())
            .map(|root| root.join(".course2md/work").join(&legacy.id))
    });
    let legacy_work_dir = old_work.filter(|old| *old != canonical_work);

    let mut logs = if !legacy.logs.is_empty() {
        legacy.logs
    } else {
        legacy.events.logs
    };
    if logs.len() > LOG_LINES {
        logs.drain(..logs.len() - LOG_LINES);
    }

    let raw_error = legacy.error.or(legacy.events.error);
    let error = (status == TaskStatus::Failed).then(|| {
        let message = raw_error.unwrap_or_else(|| {
            if legacy.state == "partial" {
                c2m_i18n::tr!("部分未完成").to_owned()
            } else {
                c2m_i18n::tr!("需要处理").to_owned()
            }
        });
        let needs_settings = message.contains("API Key")
            || message.contains("凭据")
            || message.contains("服务")
            || message.contains("HTTP 401")
            || message.contains("HTTP 403")
            || message.contains("HTTP 404");
        TaskError {
            message,
            failed_stages: Vec::new(),
            missing_model: None,
            needs_settings,
        }
    });

    let kept_video = legacy
        .ran_with
        .as_ref()
        .and_then(|v| v.pointer("/config/defaults/keep_video"))
        .or_else(|| {
            legacy
                .plan
                .config
                .as_ref()
                .and_then(|v| v.pointer("/defaults/keep_video"))
        })
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let subtitle_events = source
        .selected_subtitle
        .as_ref()
        .filter(|sub| !sub.events.is_empty())
        .map(|sub| sub.events.clone());

    Some(Task {
        id,
        library_id,
        course_id,
        source_id,
        author: source.author.clone(),
        duration: source.duration,
        title_from_probe: source.online && source.title.trim().is_empty(),
        source,
        title,
        subtitle: legacy.plan.subtitle,
        subtitle_events,
        target_folder: legacy.plan.folder,
        note_dir,
        work_dir: canonical_work,
        legacy_work_dir,
        status,
        run_mode: RunMode::Resume,
        stages: BTreeMap::new(),
        error,
        logs,
        unread: legacy.unread,
        created: legacy.created,
        updated: legacy.updated,
        outcomes: None,
        kept_video,
        kept_edits: Vec::new(),
        events: JobEvents::default(),
    })
}

// ---------------------------------------------------------------------------
// Library Checks, Reconciliation & Workspace Persistence
// ---------------------------------------------------------------------------

pub fn check_library(location: &LibraryLocation) -> Result<()> {
    ensure!(
        location.root.is_dir(),
        c2m_i18n::tr!("保存位置不可访问，请连接对应磁盘")
    );
    let identity = std::fs::read_to_string(location.root.join(".course2md-library-id"))
        .context(c2m_i18n::tr!("保存位置需要重新关联"))?;
    ensure!(
        identity.trim() == location.id,
        c2m_i18n::tr!("保存位置对应其他笔记库，请恢复原位置或在存储设置中重新登记")
    );
    Ok(())
}

pub(crate) fn create_library_marker(location: &LibraryLocation) -> Result<()> {
    use std::io::Write;
    ensure!(
        location.root.is_dir(),
        c2m_i18n::tr!("保存位置暂时不可访问")
    );
    let target = location.root.join(".course2md-library-id");
    if target.exists() {
        return check_library(location);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&location.root)?;
    temporary.write_all(location.id.as_bytes())?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&target) {
        Ok(_) => c2m_artifact::artifact::sync_dir(&location.root)?,
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            check_library(location)?
        }
        Err(error) => return Err(error.error.into()),
    }
    check_library(location)
}

fn validate_record_location(task: &Task, libraries: &[LibraryLocation]) -> Result<()> {
    ensure!(
        c2m_engine::execution::valid_id(&task.id)
            && c2m_engine::execution::valid_id(&task.course_id),
        c2m_i18n::tr!("任务身份无效")
    );
    let library = libraries
        .iter()
        .find(|library| library.id == task.library_id)
        .context(c2m_i18n::tr!("任务保存位置记录缺失"))?;
    ensure!(
        library.root.is_absolute(),
        c2m_i18n::tr!("任务工作目录与记录的笔记库不匹配")
    );
    ensure!(
        !task.source_id.trim().is_empty() && !task.title.trim().is_empty(),
        c2m_i18n::tr!("任务来源身份或名称记录缺失")
    );
    Ok(())
}

fn worker_holds(work_dir: &Path) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(work_dir.join(".task.lock"))
        .is_ok_and(|file| matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MigrationOutcome {
    pub notes_changed: bool,
    pub notice: Option<String>,
}

pub struct PreparedLibrary {
    pub root: PathBuf,
    pub name: String,
    pub id: String,
    pub duplicate: bool,
    pub make_default: bool,
}

enum TaskDiskFacts {
    Unavailable,
    Found {
        note: Option<(PathBuf, Box<c2m_artifact::artifact::Manifest>)>,
        events: Box<JobEvents>,
    },
}

pub struct Reconciliation {
    workspace: u64,
    tasks: Vec<(Task, Option<LibraryLocation>)>,
    migrations: Vec<(LibraryLocation, Vec<(PathBuf, PathBuf)>)>,
}

pub struct Reconciled {
    workspace: u64,
    facts: Vec<(String, TaskDiskFacts)>,
    migrations: Vec<crate::migrate::Migration>,
}

impl Reconciliation {
    /// Blocking: run via `desktop_prefs::spawn_blocking_io`.
    pub fn run(self) -> Reconciled {
        let migrations = self
            .migrations
            .iter()
            .filter(|(location, _)| check_library(location).is_ok())
            .map(|(location, writers)| {
                let busy: Vec<PathBuf> = writers
                    .iter()
                    .filter(|(_, work_dir)| worker_holds(work_dir))
                    .map(|(note, _)| note.clone())
                    .collect();
                crate::migrate::migrate_library(location, &busy)
            })
            .collect();

        let facts = self
            .tasks
            .into_iter()
            .map(|(task, location)| {
                let Some(location) = location.filter(|loc| check_library(loc).is_ok()) else {
                    return (task.id, TaskDiskFacts::Unavailable);
                };
                let note_path = task_note(&task, &location);
                let note = (!c2m_artifact::artifact::is_legacy(&note_path))
                    .then(|| {
                        c2m_artifact::artifact::note_manifest(&note_path)
                            .ok()
                            .flatten()
                    })
                    .flatten()
                    .filter(|manifest| {
                        manifest.course_id == task.course_id && manifest.source_id == task.source_id
                    })
                    .map(|manifest| (note_path, Box::new(manifest)));

                let mut events = JobEvents::default();
                let effective = task.effective_work_dir();
                if let Ok(bytes) = desktop_prefs::store::read(&effective.join(TASK_EVENTS_FILE)) {
                    events.replay(&bytes);
                }
                events.loaded = true;
                (
                    task.id,
                    TaskDiskFacts::Found {
                        note,
                        events: Box::new(events),
                    },
                )
            })
            .collect();

        Reconciled {
            workspace: self.workspace,
            facts,
            migrations,
        }
    }
}

fn apply_disk_facts(task: &mut Task, facts: TaskDiskFacts) {
    let TaskDiskFacts::Found { note, events } = facts else {
        return;
    };
    if task.logs.is_empty() && !events.logs.is_empty() {
        task.logs = events.logs.clone();
        if task.status == TaskStatus::Completed {
            task.trim_finished_logs();
        }
    }
    task.events = *events;
    if let Some((note_path, manifest)) = note {
        task.note_dir = Some(note_path);
        task.outcomes = Some(manifest.outcomes.clone());
        task.sync_stages_from_outcomes(&manifest.outcomes, manifest.partial);
        if manifest.partial {
            if task.status == TaskStatus::Completed {
                let failed = incomplete_components_from_outcomes(&manifest.outcomes);
                task.status = TaskStatus::Failed;
                task.error = Some(TaskError {
                    message: c2m_i18n::tr!("部分未完成").into(),
                    failed_stages: failed,
                    missing_model: None,
                    needs_settings: false,
                });
            } else if let Some(err) = &mut task.error
                && err.failed_stages.is_empty()
            {
                err.failed_stages = incomplete_components_from_outcomes(&manifest.outcomes);
            }
        }
    }
}

fn draft_subtitle_path(record: &Path) -> PathBuf {
    record.with_extension("subtitle.json")
}

fn hydrate_draft_subtitles(state: &mut State, record: &Path) {
    let stored = desktop_prefs::store::read(&draft_subtitle_path(record))
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<c2m_source::subtitle::CachedSubtitle>(&bytes).ok()
        });
    for source in state
        .drafts
        .iter_mut()
        .filter_map(|draft| draft.source.as_mut())
    {
        let Some(selected) = &source.selected_subtitle else {
            continue;
        };
        if !selected.events.is_empty() {
            continue;
        }
        source.selected_subtitle = stored
            .as_ref()
            .filter(|stored| {
                stored.source_identity == selected.source_identity
                    && stored.track_id == selected.track_id
                    && !stored.events.is_empty()
            })
            .cloned();
    }
}

fn write_record(path: &Path, previous: &State, next: &State) -> Result<()> {
    let subtitle = |state: &State| {
        state
            .draft()
            .and_then(|draft| draft.source.as_ref())
            .and_then(|source| source.selected_subtitle.clone())
    };
    if let Some(selected) = subtitle(next).filter(|selected| !selected.events.is_empty())
        && (subtitle(previous).as_ref() != Some(&selected) || !draft_subtitle_path(path).exists())
    {
        c2m_core::fsio::atomic_write(&draft_subtitle_path(path), &serde_json::to_vec(&selected)?)?;
    }
    desktop_prefs::store::save(path, &serde_json::to_vec(next)?)
}

pub struct Workspace {
    pub state: State,
    pub recovery: Option<String>,
    path: PathBuf,
    identity: u64,
    checking: bool,
    migration_due: BTreeSet<String>,
    migrated: Option<MigrationOutcome>,
}

impl Workspace {
    fn assemble(mut state: State, recovery: Option<String>, path: PathBuf) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        state.reconciling = true;
        let migration_due = state.libraries.iter().map(|l| l.id.clone()).collect();
        Self {
            state,
            recovery,
            path,
            identity: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            checking: false,
            migration_due,
            migrated: None,
        }
    }

    pub fn open(root: PathBuf) -> Result<Self> {
        Self::open_at(
            c2m_config::config::config_dir().join("desktop-workspace.json"),
            root,
        )
    }

    pub(crate) fn open_at(path: PathBuf, root: PathBuf) -> Result<Self> {
        let mut recovery = None;
        let schema = desktop_prefs::store::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|record| record.get("schema")?.as_u64());
        if schema.is_some_and(|schema| schema > SCHEMA as u64) {
            bail!(c2m_i18n::tr!(
                "任务记录由较新版本创建，请使用对应版本打开。原记录和备份均保留，尚未启动任务。"
            ));
        }

        let mut state = if path.exists() {
            match Self::read(&path) {
                Ok(state) => state,
                Err(error) => match Self::read(&desktop_prefs::store::backup_path(&path)) {
                    Ok(mut state) => {
                        std::fs::read(&path)
                            .map_err(anyhow::Error::from)
                            .and_then(|damaged| desktop_prefs::store::set_aside(&path, &damaged))
                            .context(c2m_i18n::tr!("无法保留损坏的任务记录"))?;
                        recovery = Some(
                            c2m_i18n::tr!(
                                "已从备份恢复输入和任务。无法确认最后一次操作，未完成任务已暂停。"
                            )
                            .into(),
                        );
                        for task in &mut state.tasks {
                            if matches!(task.status, TaskStatus::Queued | TaskStatus::Running) {
                                task.status = TaskStatus::Paused;
                            }
                        }
                        state
                    }
                    Err(_) => {
                        return Err(error)
                            .context(c2m_i18n::tr!("输入和任务记录无法读取，原文件已保留"));
                    }
                },
            }
        } else {
            let mut initial = State::initial(root.clone());
            match std::fs::create_dir_all(&root) {
                Ok(()) => {
                    if !initial.adopt_marker(&root)
                        && let Err(error) = c2m_core::fsio::atomic_write(
                            &root.join(".course2md-library-id"),
                            initial.default_library.as_bytes(),
                        )
                    {
                        recovery = Some(c2m_i18n::tr!(
                            "保存位置尚未准备好：{}。可以在设置的存储中选择其他位置。",
                            format!("{error:#}")
                        ));
                    }
                }
                Err(error) => {
                    recovery = Some(c2m_i18n::tr!(
                        "无法创建默认保存位置 {}：{}。可以在设置的存储中选择其他位置。",
                        root.display(),
                        error
                    ))
                }
            }
            initial
        };

        state.recover();
        hydrate_draft_subtitles(&mut state, &path);
        Ok(Self::assemble(state, recovery, path))
    }

    /// Explicit repair when the workspace JSON is damaged beyond backup recovery.
    pub fn rebuild(root: PathBuf) -> Result<Self> {
        Self::rebuild_at(
            c2m_config::config::config_dir().join("desktop-workspace.json"),
            root,
        )
    }

    pub fn rebuild_at(path: PathBuf, root: PathBuf) -> Result<Self> {
        let parent = path
            .parent()
            .context(c2m_i18n::tr!("任务记录缺少保存目录"))?;
        std::fs::create_dir_all(parent)?;
        let backup = desktop_prefs::store::backup_path(&path);
        let originals: Vec<_> = [&path, &backup]
            .into_iter()
            .filter(|p| p.exists())
            .map(|p| Ok((p.clone(), desktop_prefs::store::read(p)?)))
            .collect::<Result<_>>()?;
        ensure!(
            !originals.is_empty(),
            c2m_i18n::tr!("原任务记录不存在，请重新打开应用建立新记录")
        );
        let records: Vec<serde_json::Value> = originals
            .iter()
            .filter_map(|(_, bytes)| serde_json::from_slice(bytes).ok())
            .collect();
        ensure!(
            !records.iter().any(|v| v
                .get("schema")
                .and_then(|v| v.as_u64())
                .is_some_and(|schema| schema > SCHEMA as u64)),
            c2m_i18n::tr!("这些任务由较新版本创建，请使用对应版本恢复，原文件已保留")
        );
        let archive = parent.join(new_id("workspace-recovery"));
        std::fs::create_dir(&archive).context(c2m_i18n::tr!("无法建立原始记录备份，尚未重建"))?;
        for (original, bytes) in &originals {
            let name = original
                .file_name()
                .context(c2m_i18n::tr!("原记录缺少文件名"))?;
            c2m_core::fsio::atomic_write(&archive.join(name), bytes)
                .context(c2m_i18n::tr!("原始记录备份未完成，尚未重建"))?;
        }

        let mut initial = State::initial(root.clone());
        initial.adopt_marker(&root);
        let mut state = Self::read(&path)
            .or_else(|_| Self::read(&backup))
            .unwrap_or_else(|_| initial.clone());

        for record in &records {
            if let Some(libs) = record.get("libraries").and_then(|v| v.as_array()) {
                for item in libs {
                    if let Ok(wire) = serde_json::from_value::<LibraryWire>(item.clone())
                        && wire.root.is_absolute()
                    {
                        let id = if !wire.id.is_empty() && c2m_engine::execution::valid_id(&wire.id)
                        {
                            wire.id
                        } else if let Ok(marker) =
                            std::fs::read_to_string(wire.root.join(crate::storage::LIBRARY_ID_FILE))
                            && c2m_engine::execution::valid_id(marker.trim())
                        {
                            marker.trim().to_owned()
                        } else {
                            continue;
                        };
                        if !state
                            .libraries
                            .iter()
                            .any(|l| l.id == id || l.root == wire.root)
                        {
                            state.libraries.push(LibraryLocation {
                                id,
                                name: if wire.name.is_empty() {
                                    c2m_i18n::tr!("笔记库").into()
                                } else {
                                    wire.name
                                },
                                root: wire.root,
                                previous_roots: wire.previous_roots,
                            });
                        }
                    }
                }
            }
        }
        if !state
            .libraries
            .iter()
            .any(|l| l.root == root || l.id == initial.default_library)
        {
            state.libraries.append(&mut initial.libraries);
        }
        for task in &mut state.tasks {
            if matches!(task.status, TaskStatus::Queued | TaskStatus::Running) {
                task.status = TaskStatus::Paused;
            }
        }
        state.recover();
        hydrate_draft_subtitles(&mut state, &path);
        c2m_core::fsio::atomic_write(&path, &serde_json::to_vec(&state)?)?;
        let count = state.tasks.len();
        let recovery = if count == 1 {
            c2m_i18n::tr!(
                "已重建 1 项任务记录，未启动处理。原始记录保存在 {}。{}",
                archive.display(),
                ""
            )
        } else {
            c2m_i18n::tr!(
                "已重建 {} 项任务记录，未启动处理。原始记录保存在 {}。{}",
                count,
                archive.display(),
                ""
            )
        };
        Ok(Self::assemble(state, Some(recovery), path))
    }

    pub fn reconciliation(&mut self) -> Option<Reconciliation> {
        if !self.state.reconciling || self.checking {
            return None;
        }
        self.checking = true;
        let migrations = self
            .state
            .libraries
            .iter()
            .filter(|library| self.migration_due.contains(&library.id))
            .map(|library| {
                let writers = self
                    .state
                    .tasks
                    .iter()
                    .filter(|task| !task.status.is_finished() && task.library_id == library.id)
                    .map(|task| {
                        (
                            task_note(task, library),
                            task.effective_work_dir().to_path_buf(),
                        )
                    })
                    .collect();
                (library.clone(), writers)
            })
            .collect();
        Some(Reconciliation {
            workspace: self.identity,
            migrations,
            tasks: self
                .state
                .tasks
                .iter()
                .filter(|task| !task.events.loaded && task.status != TaskStatus::Running)
                .map(|task| (task.clone(), self.state.library(&task.library_id).cloned()))
                .collect(),
        })
    }

    pub fn recheck_library(&mut self, library_id: &str) {
        self.state.reconciling |= self.migration_due.contains(library_id)
            || self
                .state
                .tasks
                .iter()
                .any(|task| !task.events.loaded && task.library_id == library_id);
    }

    pub fn take_migrated(&mut self) -> Option<MigrationOutcome> {
        self.migrated.take()
    }

    pub fn apply_reconciliation(&mut self, reconciled: Reconciled) -> Result<()> {
        if reconciled.workspace != self.identity {
            return Ok(());
        }
        self.checking = false;
        let migrations = reconciled.migrations;
        for migration in &migrations {
            if migration.ran && migration.deferred.is_empty() {
                self.migration_due.remove(&migration.library_id);
            }
        }
        if migrations.iter().any(|migration| migration.flattened > 0) {
            let outcome = self.migrated.get_or_insert_with(Default::default);
            outcome.notes_changed = true;
            let mut notices = Vec::new();
            if migrations
                .iter()
                .any(|migration| migration.removed_versions > 0)
            {
                notices.push(crate::migrate::removed_versions_notice().to_owned());
            }
            let resaved: usize = migrations.iter().map(|migration| migration.resaved).sum();
            if resaved > 0 {
                notices.push(crate::migrate::resaved_notice(resaved));
            }
            if !notices.is_empty() {
                outcome.notice = Some(notices.join(" "));
            }
        }

        let mut positions = self.state.positions.clone();
        let moved = migrations
            .iter()
            .any(|migration| crate::migrate::rekey_positions(&mut positions, &migration.notes));
        if reconciled.facts.is_empty()
            && !moved
            && migrations.iter().all(|migration| migration.flattened == 0)
        {
            self.state.reconciling = false;
            return Ok(());
        }

        let apply = |state: &mut State,
                     facts: Vec<(String, TaskDiskFacts)>,
                     migrations: &[crate::migrate::Migration]| {
            for migration in migrations {
                crate::migrate::rekey_positions(&mut state.positions, &migration.notes);
            }
            for (id, fact) in facts {
                if let Some(task) = state.task_mut(&id)
                    && task.status != TaskStatus::Running
                {
                    apply_disk_facts(task, fact);
                }
            }
            state.reconciling = false;
        };

        let mut facts = Some(reconciled.facts);
        let result = self.transaction(|state| {
            apply(state, facts.take().unwrap_or_default(), &migrations);
            Ok(())
        });
        if let Some(facts) = facts.take().filter(|_| result.is_err()) {
            apply(&mut self.state, facts, &migrations);
        }
        result
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open_at_inline(path: PathBuf, root: PathBuf) -> Result<Self> {
        let mut workspace = Self::open_at(path, root)?;
        workspace.reconcile_now();
        Ok(workspace)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn reconcile_now(&mut self) {
        if let Some(reconciliation) = self.reconciliation() {
            let _ = self.apply_reconciliation(reconciliation.run());
        }
    }

    fn read(path: &Path) -> Result<State> {
        let state: State = serde_json::from_slice(
            &desktop_prefs::store::read(path)
                .with_context(|| c2m_i18n::tr!("读取 {}", path.display()))?,
        )?;
        ensure!(
            state.library(&state.default_library).is_some(),
            c2m_i18n::tr!("默认保存位置记录缺失")
        );
        ensure!(state.draft().is_some(), c2m_i18n::tr!("当前输入记录缺失"));
        let ids: BTreeSet<_> = state.tasks.iter().map(|t| &t.id).collect();
        ensure!(
            ids.len() == state.tasks.len(),
            c2m_i18n::tr!("任务记录包含重复身份")
        );
        for task in &state.tasks {
            validate_record_location(task, &state.libraries)?;
        }
        Ok(state)
    }

    pub fn transaction<T>(&mut self, edit: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let mut next = self.state.clone();
        let result = edit(&mut next)?;
        for task in &mut next.tasks {
            if task.status == TaskStatus::Completed {
                task.trim_finished_logs();
            }
        }
        next.revision = next.revision.wrapping_add(1);
        write_record(&self.path, &self.state, &next)?;
        self.state = next;
        Ok(result)
    }

    pub fn commit(&self) -> Result<()> {
        desktop_prefs::store::save(&self.path, &serde_json::to_vec(&self.state)?)
    }

    pub fn save(&self) -> Result<()> {
        self.commit()
    }

    pub fn storage_path(&self) -> &Path {
        &self.path
    }

    pub fn prepare_library(
        root: PathBuf,
        name: String,
        make_default: bool,
        existing_roots: &[PathBuf],
        existing_ids: &[String],
    ) -> Result<PreparedLibrary> {
        ensure!(
            root.is_dir(),
            c2m_i18n::tr!("请选择已有文件夹；新建笔记库请使用新建操作")
        );
        let canonical = root.canonicalize()?;
        for existing in existing_roots {
            if existing.canonicalize().ok().as_ref() == Some(&canonical) {
                return Ok(PreparedLibrary {
                    root: existing.clone(),
                    name,
                    id: String::new(),
                    duplicate: true,
                    make_default,
                });
            }
        }
        let probe = tempfile::NamedTempFile::new_in(&canonical)
            .context(c2m_i18n::tr!("所选位置不能写入"))?;
        drop(probe);
        let identity = canonical.join(".course2md-library-id");
        let id = if identity.is_file() {
            let id = std::fs::read_to_string(&identity)?.trim().to_owned();
            if id.is_empty() {
                bail!(c2m_i18n::tr!("笔记库身份记录为空，请检查所选位置"));
            }
            if existing_ids.iter().any(|existing| existing == &id) {
                bail!(c2m_i18n::tr!(
                    "这个库已经登记在另一个位置，请使用移动笔记库操作"
                ));
            }
            id
        } else {
            let id = new_id("library");
            c2m_core::fsio::atomic_write(&identity, id.as_bytes())?;
            id
        };
        Ok(PreparedLibrary {
            root: canonical,
            name,
            id,
            duplicate: false,
            make_default,
        })
    }

    pub fn register_prepared_library(&mut self, prepared: &PreparedLibrary) -> Result<String> {
        if prepared.duplicate {
            let existing = self
                .state
                .libraries
                .iter()
                .find(|l| l.root == prepared.root);
            let Some(id) = existing.map(|library| library.id.clone()) else {
                return Ok(String::new());
            };
            if prepared.make_default {
                self.transaction(|state| {
                    state.default_library = id.clone();
                    Ok(())
                })?;
            }
            return Ok(id);
        }
        let id = prepared.id.clone();
        let name = prepared.name.clone();
        let root = prepared.root.clone();
        let make_default = prepared.make_default;
        self.transaction(|state| {
            state.libraries.push(LibraryLocation {
                id: id.clone(),
                name,
                root,
                previous_roots: Vec::new(),
            });
            if make_default {
                state.default_library = id.clone();
            }
            state.reconciling = true;
            Ok(id)
        })
        .inspect(|id| {
            self.migration_due.insert(id.clone());
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn register_library(
        &mut self,
        root: PathBuf,
        name: String,
        make_default: bool,
    ) -> Result<String> {
        let roots: Vec<_> = self
            .state
            .libraries
            .iter()
            .map(|l| l.root.clone())
            .collect();
        let ids: Vec<_> = self.state.libraries.iter().map(|l| l.id.clone()).collect();
        let prepared = Self::prepare_library(root, name, make_default, &roots, &ids)?;
        self.register_prepared_library(&prepared)
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn accept_source(draft: &mut Draft, revision: u64, source: Source) -> bool {
    if draft.revision != revision {
        return false;
    }
    draft.source = Some(source);
    draft.updated = now();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_source(input: &str, identity: &str, title: &str) -> Source {
        Source {
            input: input.into(),
            identity: identity.into(),
            title: title.into(),
            author: "讲师".into(),
            duration: 120.0,
            online: true,
            ..Default::default()
        }
    }

    fn new_task(library_id: &str, id_key: &str, title: &str) -> NewTask {
        NewTask::from_source(
            library_id,
            fixture_source(&format!("https://example.test/{id_key}"), id_key, title),
            title,
            RunMode::Resume,
        )
    }

    #[test]
    fn default_library_name_reads_in_interface_language() {
        use c2m_i18n::i18n::Lang;
        for saved in ["笔记库", "Library", "ノートライブラリ"] {
            assert_eq!(label_in(saved, Lang::En), "Library", "{saved}");
            assert_eq!(label_in(saved, Lang::Ja), "ノートライブラリ", "{saved}");
            assert_eq!(label_in(saved, Lang::ZhHans), "笔记库", "{saved}");
            assert_eq!(label_in(saved, Lang::ZhHant), "筆記庫", "{saved}");
        }
        assert_eq!(label_in("课程资料", Lang::En), "课程资料");
    }

    #[test]
    fn one_course_has_at_most_one_task_per_library_and_updates_in_place() {
        let mut state = State::initial(PathBuf::from("/library"));
        let lib = state.default_library.clone();
        let first_id = state
            .enqueue(new_task(&lib, "video-1", "初版标题"))
            .unwrap();
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(
            state.task(&first_id).unwrap().work_dir,
            PathBuf::from("/library/.course2md/work").join(course_dir_name("video-1"))
        );

        // Mark failed with an existing partial note, then re-submit the same course:
        // updates the single task in place without creating a chain or duplicate.
        let note = PathBuf::from("/library").join(course_dir_name("video-1"));
        {
            let task = state.task_mut(&first_id).unwrap();
            task.status = TaskStatus::Failed;
            task.note_dir = Some(note.clone());
            task.error = Some(TaskError::plain("服务超时"));
        }
        let mut updated = new_task(&lib, "video-1", "更新后标题");
        updated.run_mode = RunMode::Fresh;
        let second_id = state.submit_conversion(updated).unwrap();
        assert_eq!(second_id, first_id);
        assert_eq!(state.tasks.len(), 1);
        let task = state.task(&first_id).unwrap();
        assert_eq!(task.title, "更新后标题");
        assert_eq!(task.status, TaskStatus::Queued);
        assert_eq!(task.run_mode, RunMode::Fresh);
        assert!(task.error.is_none());
        assert_eq!(task.note_dir.as_ref(), Some(&note));

        // While running, duplicate submission is refused cleanly.
        state.task_mut(&first_id).unwrap().status = TaskStatus::Running;
        assert!(
            state
                .enqueue(new_task(&lib, "video-1", "冲突提交"))
                .is_err()
        );
    }

    #[test]
    fn retry_pause_resume_cancel_and_clear_completed_follow_five_state_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        let mut state = State::initial(root.clone());
        let lib = state.default_library.clone();

        let a = state.enqueue(new_task(&lib, "a", "A")).unwrap();
        let b = state.enqueue(new_task(&lib, "b", "B")).unwrap();

        // Pause & resume single and all.
        assert!(!state.pause_task(&a).unwrap());
        assert_eq!(state.task(&a).unwrap().status, TaskStatus::Paused);
        state.resume_task(&a).unwrap();
        assert_eq!(state.task(&a).unwrap().status, TaskStatus::Queued);

        state.task_mut(&a).unwrap().status = TaskStatus::Running;
        let paused = state.pause_all();
        assert_eq!(paused, vec![(a.clone(), true), (b.clone(), false)]);
        assert_eq!(state.resume_all(), vec![a.clone(), b.clone()]);

        // Retry resets failed stages on Resume, and clears all stages on Fresh.
        {
            let task = state.task_mut(&a).unwrap();
            task.status = TaskStatus::Failed;
            task.error = Some(TaskError::plain("AI 失败"));
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
                    detail: Some("超时".into()),
                },
            );
        }
        state.retry_task(&a, RunMode::Resume).unwrap();
        let task_a = state.task(&a).unwrap();
        assert_eq!(task_a.status, TaskStatus::Queued);
        assert!(task_a.error.is_none());
        assert_eq!(
            task_a.stages[&StageKey::Transcription].phase,
            StagePhase::Completed
        );
        assert_eq!(task_a.stages[&StageKey::Summary].phase, StagePhase::Pending);

        state.retry_task(&a, RunMode::Fresh).unwrap();
        assert!(state.task(&a).unwrap().stages.is_empty());

        // Cancel cleans work_dir and removes the task while keeping any published note.
        let note_b = root.join(course_dir_name("b"));
        std::fs::create_dir_all(&note_b).unwrap();
        std::fs::write(note_b.join("course.md"), "正文").unwrap();
        let work_b = state.task(&b).unwrap().work_dir.clone();
        std::fs::create_dir_all(&work_b).unwrap();
        std::fs::write(work_b.join("asr.jsonl"), "partial").unwrap();
        state.task_mut(&b).unwrap().note_dir = Some(note_b.clone());

        let (cleared_b, was_running) = state.cancel_or_remove_task(&b).unwrap();
        assert!(!was_running);
        clean_cleared_task_work(&cleared_b);
        assert!(state.task(&b).is_none());
        assert!(!work_b.exists());
        assert!(note_b.join("course.md").is_file());

        // Clear completed keeps offline media when `kept_video` is true, and never touches `note_dir`.
        let note_a = root.join(course_dir_name("a"));
        std::fs::create_dir_all(&note_a).unwrap();
        let work_a = state.task(&a).unwrap().work_dir.clone();
        std::fs::create_dir_all(&work_a).unwrap();
        std::fs::write(work_a.join("media.mp4"), "video-bytes").unwrap();
        std::fs::write(work_a.join("media.sha256"), "digest").unwrap();
        std::fs::write(work_a.join("timeline.jsonl"), "scratch").unwrap();
        {
            let task = state.task_mut(&a).unwrap();
            task.status = TaskStatus::Completed;
            task.note_dir = Some(note_a.clone());
            task.kept_video = true;
        }
        let cleared = state.clear_completed_tasks();
        assert_eq!(cleared.len(), 1);
        for item in &cleared {
            clean_cleared_task_work(item);
        }
        assert!(state.tasks.is_empty());
        assert!(note_a.is_dir());
        assert!(work_a.join("media.mp4").is_file());
        assert!(work_a.join("media.sha256").is_file());
        assert!(!work_a.join("timeline.jsonl").exists());
    }

    #[test]
    fn moving_queued_and_paused_tasks_preserves_queue_order() {
        let mut state = State::initial(PathBuf::from("/library"));
        let lib = state.default_library.clone();
        for key in ["a", "b", "c", "d"] {
            state.enqueue(new_task(&lib, key, key)).unwrap();
        }
        let id_of = |state: &State, title: &str| {
            state
                .tasks
                .iter()
                .find(|t| t.title == title)
                .unwrap()
                .id
                .clone()
        };
        let d = id_of(&state, "d");
        state.move_queued_task(&d, 0).unwrap();
        let titles: Vec<_> = state.queued_tasks().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["d", "a", "b", "c"]);
        assert_eq!(state.next_task().unwrap().title, "d");

        let b = id_of(&state, "b");
        state.pause_task(&b).unwrap();
        state.move_queued_task(&b, 3).unwrap();
        let waiting: Vec<_> = state.waiting_tasks().map(|t| t.title.as_str()).collect();
        assert_eq!(waiting, ["d", "a", "c", "b"]);
    }

    #[test]
    fn legacy_v1_workspace_json_migrates_chains_states_notes_and_legacy_work_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".course2md-library-id"), "library-v1").unwrap();
        let path = dir.path().join("desktop-workspace.json");

        let v1 = serde_json::json!({
            "schema": 1,
            "libraries": [{ "root": root, "name": "笔记库" }],
            "tasks": [
                {
                    "id": "task-old-1",
                    "plan": {
                        "source": { "input": "https://b23.tv/1", "title": "课程一", "online": true },
                        "source_id": "bilibili:BV1",
                        "title": "课程一",
                        "library_id": "library-v1"
                    },
                    "state": "partial",
                    "intent": "run",
                    "created": 10,
                    "updated": 20,
                    "handled_by": "task-old-2",
                    "artifact": root.join("course-old/versions/task-old-1")
                },
                {
                    "id": "task-old-2",
                    "parent": "task-old-1",
                    "plan": {
                        "source": { "input": "https://b23.tv/1", "title": "课程一", "online": true },
                        "source_id": "bilibili:BV1",
                        "title": "课程一（补做）",
                        "library_id": "library-v1"
                    },
                    "state": "uncertain",
                    "intent": "run",
                    "created": 30,
                    "updated": 40,
                    "artifact": root.join(course_dir_name("bilibili:BV1")),
                    "error": "HTTP 401 API Key 无效",
                    "events": { "logs": ["line 1", "line 2"] }
                },
                {
                    "id": "task-replaced",
                    "plan": {
                        "source": { "input": "https://b23.tv/2", "title": "旧版", "online": true },
                        "source_id": "bilibili:BV2",
                        "title": "旧版",
                        "library_id": "library-v1"
                    },
                    "state": "complete",
                    "replaced_by": "task-current",
                    "created": 5,
                    "updated": 6
                },
                {
                    "id": "task-current",
                    "plan": {
                        "source": { "input": "https://b23.tv/2", "title": "课程二", "online": true },
                        "source_id": "bilibili:BV2",
                        "title": "课程二",
                        "library_id": "library-v1"
                    },
                    "state": "pausing",
                    "intent": "pause",
                    "created": 50,
                    "updated": 60
                },
                {
                    "id": "task-cancelled",
                    "plan": {
                        "source": { "input": "https://b23.tv/3", "title": "已取消", "online": true },
                        "source_id": "bilibili:BV3",
                        "title": "已取消",
                        "library_id": "library-v1"
                    },
                    "state": "cancelled",
                    "created": 70,
                    "updated": 80
                }
            ]
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&v1).unwrap()).unwrap();

        let ws = Workspace::open_at_inline(path, root.clone()).unwrap();
        assert_eq!(ws.state.schema, SCHEMA);
        assert_eq!(ws.state.default_library, "library-v1");
        assert_eq!(ws.state.tasks.len(), 2);

        let first = ws
            .state
            .task_for_source("library-v1", "bilibili:BV1")
            .unwrap();
        assert_eq!(first.id, "task-old-2");
        assert_eq!(first.status, TaskStatus::Failed);
        assert_eq!(
            first.note_dir.as_ref(),
            Some(&root.join(course_dir_name("bilibili:BV1")))
        );
        assert_eq!(
            first.work_dir,
            root.join(".course2md/work")
                .join(course_dir_name("bilibili:BV1"))
        );
        assert_eq!(
            first.legacy_work_dir.as_ref(),
            Some(&root.join(".course2md/work/task-old-2"))
        );
        assert_eq!(first.logs, vec!["line 1", "line 2"]);
        let err = first.error.as_ref().unwrap();
        assert!(err.message.contains("401"));
        assert!(err.needs_settings);

        let second = ws
            .state
            .task_for_source("library-v1", "bilibili:BV2")
            .unwrap();
        assert_eq!(second.id, "task-current");
        assert_eq!(second.status, TaskStatus::Paused);
    }

    #[test]
    fn newer_schema_is_refused_without_modifying_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-workspace.json");
        let bytes = br#"{"schema": 99, "libraries": []}"#;
        std::fs::write(&path, bytes).unwrap();
        assert!(Workspace::open_at(path.clone(), dir.path().join("lib")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
