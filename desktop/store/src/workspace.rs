//! Durable user work. Views never supply the identity or settings of a retry.
use anyhow::{Context, Result, bail, ensure};
use c2m_config::{config::TranscriptSource, settings::ConfigFile};
use c2m_engine::execution::Component;
use c2m_protocol::progress::BlockReason;
use desktop_prefs::conversion::ConversionOptions;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub use crate::activity::{BlockedRequest, JobEvents};
pub use desktop_prefs::store::{new_id, now};

const SCHEMA: u32 = 2;
/// Parsed subtitle text of a submitted task, kept in its work directory so the
/// workspace record stays small. Older records carry the events inline.
const TASK_SUBTITLE_FILE: &str = "subtitle-events.json";
/// What a running task said, one engine event per line.
pub const TASK_EVENTS_FILE: &str = "events.jsonl";
/// Log lines kept once a task has finished.
const FINISHED_TASK_LOG_LINES: usize = 50;
/// Cancelled attempts without a note kept in the task list.
const KEPT_CANCELLED_TASKS: usize = 30;

/// 课程目录名：`course-{source_id 摘要前 32 字符}`——32 是与引擎 artifact 布局对齐的固定截断。
pub fn course_dir_name(source_id: &str) -> String {
    format!(
        "course-{}",
        &c2m_core::digest::digest(source_id.as_bytes())[..32]
    )
}

/// Where a draft or task's video comes from, as the import preview found it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LibraryLocation {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    #[serde(default)]
    pub previous_roots: Vec<PathBuf>,
}

impl LibraryLocation {
    /// The name to show. The built-in default was saved in whatever language the app
    /// spoke when the library was made; it reads in today's. A name the person chose is
    /// shown as saved, and nothing renames the stored record.
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

/// The single current input form: the link or file, and what was read from it.
/// Processing choices are not part of it: every task runs with the settings
/// current when it starts. Its serialized type/field names are retained so
/// existing workspaces can be upgraded; fields older versions wrote (per-input
/// options, services, title and folder) are ignored when it is read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Draft {
    pub id: String,
    pub revision: u64,
    pub online: bool,
    pub input: String,
    /// Subtitle events live in a separate file beside the workspace record.
    #[serde(default, serialize_with = "serialize_draft_source")]
    pub source: Option<Source>,
    /// The text source the person chose on this video's card: one of its
    /// subtitles, or speech recognition. `None` follows Settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_source: Option<TranscriptSource>,
    #[serde(default)]
    pub subtitle: Option<PathBuf>,
    #[serde(default)]
    pub retry_of: Option<String>,
    #[serde(default)]
    pub submitted_task: Option<String>,
    #[serde(default)]
    pub scroll: f32,
    pub updated: u64,
}

impl Draft {
    pub fn new(online: bool) -> Self {
        Self {
            id: new_id("input"),
            revision: 0,
            online,
            input: String::new(),
            source: None,
            text_source: None,
            subtitle: None,
            retry_of: None,
            submitted_task: None,
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
        self.text_source = None;
        self.submitted_task = None;
        self.updated = now();
    }
}

impl Draft {
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
        // Track identity refers to the original confirmed choice. Its content is stored
        // in the task; relocating a file does not reselect a track.
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

pub(crate) fn relocate_path(path: &mut PathBuf, old: &Path, new: &Path) {
    if path.is_absolute()
        && let Ok(relative) = path.strip_prefix(old)
    {
        *path = new.join(relative);
    }
}

fn relocate_config(config: &mut ConfigFile, old: &Path, new: &Path) {
    for path in [&mut config.defaults.out, &mut config.defaults.model_dir]
        .into_iter()
        .flatten()
    {
        relocate_path(path, old, new);
    }
}

/// Extra defense at the persistence boundary: even a caller-provided resolved
/// config cannot put its API keys in a task file or its backup.
fn serialize_public_config<S: serde::Serializer>(
    config: &ConfigFile,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    public_config(config).serialize(serializer)
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
pub(crate) fn public_config(config: &ConfigFile) -> ConfigFile {
    let mut config = config.clone();
    config.asr_api.api_key.clear();
    config.llm.api_key.clear();
    config
}

/// What a task is about: the video, where its note goes, and, for a repair, what
/// it redoes. How the note is made is not part of it: a task runs with the
/// settings current when it starts (see `RunSettings`).
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskPlan {
    #[serde(default)]
    pub operation: c2m_engine::execution::Operation,
    pub source: Source,
    pub source_id: String,
    pub title: String,
    pub library_id: String,
    pub folder: Option<u64>,
    pub subtitle: Option<PathBuf>,
    /// The text source the person chose on the video's card: one of its subtitles,
    /// or speech recognition. `None` follows Settings when the task runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_source: Option<TranscriptSource>,
    /// What a repair keeps of the note it repairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<RepairScope>,
}

/// What a repair keeps of the note it repairs, whatever Settings say now: the
/// language its translation is in, and the exports it redoes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RepairScope {
    #[serde(default)]
    pub translate: Option<String>,
    #[serde(default)]
    pub formats: Option<Vec<c2m_config::config::OutputFormat>>,
}

/// What a task's latest run started with: the settings current then, with the
/// service versions they named. Written when a run starts, so a task that has
/// not run has none; the next run replaces it.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct RunSettings {
    #[serde(serialize_with = "serialize_public_config")]
    pub config: ConfigFile,
    #[serde(default)]
    pub asr_service: Option<String>,
    #[serde(default)]
    pub ai_service: Option<String>,
}

impl RunSettings {
    pub fn new(
        config: &ConfigFile,
        asr_service: Option<String>,
        ai_service: Option<String>,
    ) -> Self {
        Self {
            config: public_config(config),
            asr_service,
            ai_service,
        }
    }

    pub fn options(&self) -> ConversionOptions {
        ConversionOptions::from_config(&self.config)
    }
}

/// A task plan as any version wrote it. Versions before 3.0.0-alpha.8 fixed each
/// task's whole configuration and services when it was submitted; that is read
/// only to keep what a task that already ran used.
#[derive(Deserialize)]
struct TaskPlanWire {
    #[serde(flatten)]
    plan: TaskPlan,
    #[serde(default, deserialize_with = "lenient_config")]
    config: Option<ConfigFile>,
    #[serde(default)]
    asr_service: Option<String>,
    #[serde(default)]
    ai_service: Option<String>,
}

/// An older plan's configuration, or none where it no longer reads.
fn lenient_config<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ConfigFile>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

pub use c2m_dispatch::dispatch::Intent;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    Pausing,
    Paused,
    NeedsAttention,
    Uncertain,
    Complete,
    Partial,
    Cancelled,
}

impl TaskState {
    pub fn finished(self) -> bool {
        matches!(self, Self::Complete | Self::Partial | Self::Cancelled)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => c2m_i18n::tr!("等待处理"),
            Self::Running => c2m_i18n::tr!("正在生成笔记"),
            Self::Pausing => c2m_i18n::tr!("正在保存进度"),
            Self::Paused => c2m_i18n::tr!("已暂停"),
            Self::NeedsAttention => c2m_i18n::tr!("需要处理"),
            Self::Uncertain => c2m_i18n::tr!("结果尚未确认"),
            Self::Complete => c2m_i18n::tr!("笔记已生成"),
            Self::Partial => c2m_i18n::tr!("部分未完成"),
            Self::Cancelled => c2m_i18n::tr!("已取消"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(from = "TaskRecordWire")]
pub struct TaskRecord {
    pub id: String,
    pub plan: TaskPlan,
    pub state: TaskState,
    pub intent: Intent,
    pub created: u64,
    pub updated: u64,
    pub parent: Option<String>,
    #[serde(default)]
    pub artifact: Option<PathBuf>,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub resend: Vec<String>,
    /// The task whose note replaced this task's: its result is gone, and it never
    /// runs or is repaired again. Its note was a version an older app kept, or another
    /// task regenerated or repaired the note since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
    /// What the latest run started with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_with: Option<RunSettings>,
    /// The later attempt that replaced this one: the task whose `parent` this is.
    #[serde(skip)]
    pub handled_by: Option<String>,
    /// A worker from an earlier session, which outlived the app, still holds the task's
    /// folder. Until it lets go, what its requests did is unknown and nothing else may
    /// run there; see `Reconciliation`.
    #[serde(skip)]
    pub held: bool,
    /// Always `<library>/.course2md/work/<id>`.
    #[serde(skip)]
    pub work_dir: PathBuf,
    #[serde(flatten)]
    pub events: JobEvents,
}

/// A task record as any version wrote it; see `TaskPlanWire`.
#[derive(Deserialize)]
struct TaskRecordWire {
    id: String,
    plan: TaskPlanWire,
    state: TaskState,
    intent: Intent,
    created: u64,
    updated: u64,
    parent: Option<String>,
    #[serde(default)]
    artifact: Option<PathBuf>,
    #[serde(default)]
    unread: bool,
    #[serde(default)]
    resend: Vec<String>,
    #[serde(default)]
    replaced_by: Option<String>,
    #[serde(default)]
    ran_with: Option<RunSettings>,
    #[serde(flatten)]
    events: JobEvents,
}

impl From<TaskRecordWire> for TaskRecord {
    fn from(wire: TaskRecordWire) -> Self {
        let TaskPlanWire {
            mut plan,
            config,
            asr_service,
            ai_service,
        } = wire.plan;
        // A repair an older version planned kept the note's translation and the
        // exports it redoes in its configuration.
        if plan.repair.is_none()
            && matches!(
                plan.operation,
                c2m_engine::execution::Operation::Reprocess { .. }
            )
            && let Some(config) = &config
        {
            plan.repair = Some(RepairScope {
                translate: config.llm.translate.clone(),
                formats: config.defaults.formats.clone(),
            });
        }
        // What a task that already ran was made with stays known; one still waiting
        // to start runs with the settings current then instead.
        let ran_with = wire.ran_with.or_else(|| {
            config
                .filter(|_| wire.state != TaskState::Queued)
                .map(|config| RunSettings::new(&config, asr_service, ai_service))
        });
        Self {
            id: wire.id,
            plan,
            state: wire.state,
            intent: wire.intent,
            created: wire.created,
            updated: wire.updated,
            parent: wire.parent,
            artifact: wire.artifact,
            unread: wire.unread,
            resend: wire.resend,
            replaced_by: wire.replaced_by,
            ran_with,
            handled_by: None,
            held: false,
            work_dir: PathBuf::new(),
            events: wire.events,
        }
    }
}

impl TaskRecord {
    /// One engine event from the running job.
    pub fn apply_event(&mut self, event: &c2m_protocol::progress::Event) -> bool {
        use c2m_protocol::progress::Event;
        if matches!(
            event,
            Event::Stage { .. } | Event::Error { .. } | Event::Blocked { .. }
        ) {
            self.updated = now();
        }
        if matches!(
            event,
            Event::Blocked {
                reason: BlockReason::Uncertain,
                ..
            }
        ) {
            self.state = TaskState::Uncertain;
        }
        self.events.apply(event)
    }

    /// The state label, telling a cancel apart from a pause while the worker stops.
    pub fn status_label(&self) -> &'static str {
        if self.state == TaskState::Pausing && self.intent == Intent::Cancel {
            c2m_i18n::tr!("正在取消")
        } else {
            self.state.label()
        }
    }

    /// Queued, or paused by the person, and not replaced by a later attempt.
    pub fn waiting(&self) -> bool {
        self.handled_by.is_none()
            && (self.state == TaskState::Queued
                || self.state == TaskState::Paused && !self.has_uncertain())
    }

    /// 是否有结果尚未确认的请求（BlockedRequest::is_uncertain 的任务级汇总）。
    pub fn has_uncertain(&self) -> bool {
        self.events.blocked.iter().any(BlockedRequest::is_uncertain)
    }

    /// The parts of this task's published note that resending its uncertain requests
    /// redoes: the unfinished part each request was made for. Resending is offered
    /// only when this succeeds; the error says why it cannot, and the repair checks
    /// the same thing, so the card never offers what the repair would refuse.
    pub fn uncertain_components(&self) -> Result<Vec<Component>> {
        let outcomes = self.events.outcomes.as_ref();
        let mut components = Vec::new();
        for request in self.events.blocked.iter().filter(|r| r.is_uncertain()) {
            let component = request
                .purpose
                .as_deref()
                .and_then(Component::parse)
                .filter(|component| component.is_ai())
                .context(c2m_i18n::tr!(
                    "待确认的请求不属于笔记中可补做的部分，请调整后重新生成"
                ))?;
            ensure!(
                outcomes.is_some_and(|outcomes| {
                    failed_outcome(component_outcome(outcomes, component))
                }),
                c2m_i18n::tr!("待确认请求所属的部分已经完成，无需重新发送")
            );
            components.push(component);
        }
        // Continuing the original task includes a requested summary that the
        // uncertain proofreading or translation before it kept from starting: a
        // first send the person asked for with the note. Any summary attempt has its
        // own recovery; permission to repeat it is never inferred from this decision.
        let held_back = self
            .ran_with
            .as_ref()
            .is_some_and(|ran_with| ran_with.config.llm.summarize)
            && !components.contains(&Component::Summary)
            && components
                .iter()
                .any(|part| matches!(part, Component::Proofreading | Component::Translation))
            && outcomes.is_some_and(|outcomes| failed_outcome(&outcomes.summary))
            && !self.events.attempted.contains(Component::Summary.as_str());
        if held_back {
            components.push(Component::Summary);
        }
        components.sort();
        components.dedup();
        Ok(components)
    }

    /// What a resend sends beyond the uncertain requests listed on the card, as the
    /// repair decides it: the unfinished rest of each part it redoes (batches the
    /// uncertain ones held back or that failed; finished ones come back from their
    /// receipts or the saved note), the translation that follows proofreading, and
    /// the summary they held back.
    pub fn resend_also_sends(&self) -> Vec<String> {
        use c2m_artifact::artifact::{Outcome, Status};
        let (Ok(components), Some(outcomes)) =
            (self.uncertain_components(), self.events.outcomes.as_ref())
        else {
            return Vec::new();
        };
        let uncertain = |part: Component| {
            self.events.blocked.iter().any(|request| {
                request.is_uncertain() && request.purpose.as_deref() == Some(part.as_str())
            })
        };
        let unfinished = |outcome: &Outcome| match outcome.status {
            Status::Succeeded | Status::NotRequested => None,
            _ => Some(
                outcome
                    .total
                    .map(|total| total.saturating_sub(outcome.completed.unwrap_or(0))),
            ),
        };
        let mut also = Vec::new();
        for (part, outcome, label) in [
            (
                Component::Proofreading,
                &outcomes.proofreading,
                crate::activity::NotePart::Proofreading.label(),
            ),
            (
                Component::Translation,
                &outcomes.translation,
                crate::activity::NotePart::Translation.label(),
            ),
        ] {
            if components.contains(&part)
                && uncertain(part)
                && let Some(Some(count)) = unfinished(outcome).filter(|count| *count != Some(0))
            {
                also.push(if count == 1 {
                    c2m_i18n::tr!("{}未完成的 1 段会发送，包括以上内容", label)
                } else {
                    c2m_i18n::tr!("{}未完成的 {} 段都会发送，包括以上内容", label, count)
                });
            }
        }
        let translation = &outcomes.translation;
        if components.contains(&Component::Proofreading)
            && !components.contains(&Component::Translation)
            && self
                .ran_with
                .as_ref()
                .and_then(|ran_with| ran_with.config.llm.translation_target())
                .is_some()
        {
            // Translation follows the proofread paragraphs. What it had made is made
            // again only where proofreading changes the text; what it had not yet made
            // (held back by the uncertain proofreading) is a first send.
            let made = translation.status == Status::Succeeded
                || translation.completed.is_some_and(|completed| completed > 0);
            if made {
                also.push(c2m_i18n::tr!("校对后有变化的段落会重新翻译").to_owned());
            }
            match unfinished(translation) {
                Some(Some(1)) => also.push(c2m_i18n::tr!("还会翻译此前未能完成的 1 段").to_owned()),
                Some(Some(count)) if count > 0 => {
                    also.push(c2m_i18n::tr!("还会翻译此前未能完成的 {} 段", count))
                }
                Some(None) => also.push(c2m_i18n::tr!("还会生成此前未能开始的翻译").to_owned()),
                _ => {}
            }
        }
        if components.contains(&Component::Summary) && !uncertain(Component::Summary) {
            also.push(c2m_i18n::tr!("还会生成此前因此未能开始的摘要").to_owned());
        }
        also
    }

    fn relocate(&mut self, old: &Path, new: &Path) {
        relocate_path(&mut self.work_dir, old, new);
        if let Some(path) = &mut self.artifact {
            relocate_path(path, old, new);
        }
        if let Some(path) = &mut self.plan.subtitle {
            relocate_path(path, old, new);
        }
        self.plan.source.relocate(old, new);
        if let Some(ran_with) = &mut self.ran_with {
            relocate_config(&mut ran_with.config, old, new);
        }
        if let c2m_engine::execution::Operation::Reprocess {
            note_dir,
            prior_work_dir,
            ..
        } = &mut self.plan.operation
        {
            relocate_path(note_dir, old, new);
            if let Some(path) = prior_work_dir {
                relocate_path(path, old, new);
            }
        }
    }

    /// A task read back from outside the record loses any evidence of the last
    /// intent, so nothing about it may send or run until someone says so.
    fn pause_as_restored(&mut self) {
        self.resend.clear();
        if !self.state.finished() {
            self.intent = Intent::Pause;
            self.state = TaskState::Paused;
            self.events.error = Some(c2m_i18n::tr!("任务记录已重建").into());
            self.unread = true;
        }
    }

    pub fn exports_only(&self) -> bool {
        matches!(
            &self.plan.operation,
            c2m_engine::execution::Operation::Reprocess { components, .. }
                if !components.is_empty() && components.iter().all(|part| *part == Component::Exports)
        )
    }

    /// The task whose note this task's result is: its own, or for one that only
    /// wrote exports, the task whose note it wrote them into.
    pub fn note_owner(&self) -> &str {
        match &self.plan.operation {
            c2m_engine::execution::Operation::Reprocess { base_task_id, .. }
                if self.exports_only() =>
            {
                base_task_id
            }
            _ => &self.id,
        }
    }
}

/// A task's exports are in its note's `exports/`. Derived from the frozen task and
/// the current library location, so moving a library needs no new path record.
fn task_export_directory(task: &TaskRecord, location: &LibraryLocation) -> Option<PathBuf> {
    (task.plan.library_id == location.id).then(|| task_note(task, location).join("exports"))
}

/// The note a task writes: the folder its source names in its library.
pub fn task_note(task: &TaskRecord, location: &LibraryLocation) -> PathBuf {
    location.root.join(course_dir_name(&task.plan.source_id))
}

/// No filesystem access: rendering can offer the exact successful outputs from
/// the durable outcome record. Existence is checked when the user opens them.
pub fn task_export_files(task: &TaskRecord, location: &LibraryLocation) -> Vec<PathBuf> {
    if task.artifact.is_none()
        || matches!(
            task.state,
            TaskState::Queued | TaskState::Running | TaskState::Pausing
        )
    {
        return Vec::new();
    }
    let Some(outcomes) = task.events.outcomes.as_ref() else {
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
                .is_some_and(|outcome| outcome.status == c2m_artifact::artifact::Status::Succeeded)
        })
        .map(|format| directory.join(c2m_artifact::portable::file_name(format)))
        .collect()
}

pub fn available_task_export(task: &TaskRecord, location: &LibraryLocation) -> Result<PathBuf> {
    task_export_files(task, location)
        .into_iter()
        .find(|path| path.is_file())
        .context(c2m_i18n::tr!(
            "导出文件已移动或无法读取，可打开笔记后从「导出」菜单重新保存。"
        ))
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

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub schema: u32,
    /// In-memory change counter; never serialized as a protocol identity.
    #[serde(default)]
    pub revision: u64,
    pub libraries: Vec<LibraryLocation>,
    pub default_library: String,
    pub drafts: Vec<Draft>,
    pub current_draft: String,
    pub tasks: Vec<TaskRecord>,
    pub selected_task: Option<String>,
    pub positions: BTreeMap<String, ReadingPosition>,
    pub reader_sources: BTreeMap<String, PathBuf>,
    pub collapsed: BTreeSet<String>,
    pub storage_backups: Vec<crate::storage::BackupRecord>,
    /// Startup reconciliation has not been applied yet; nothing is dispatched.
    #[serde(skip)]
    pub reconciling: bool,
    /// In-memory counter of what running tasks reported (stages, counts, logs) since
    /// the record was read. It changes without `revision`, so a view of the task list
    /// keys on both.
    #[serde(skip)]
    pub progress: u64,
}

impl State {
    fn initial(root: PathBuf) -> Self {
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

    /// A new record over a folder that is already a library takes on its identity.
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
    fn retain_current_input(&mut self) {
        if let Some(input) = self.draft().cloned() {
            self.replace_input(input);
        }
    }
    pub fn library(&self, id: &str) -> Option<&LibraryLocation> {
        self.libraries.iter().find(|l| l.id == id)
    }
    pub fn task(&self, id: &str) -> Option<&TaskRecord> {
        self.tasks.iter().find(|t| t.id == id)
    }
    pub fn task_mut(&mut self, id: &str) -> Option<&mut TaskRecord> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }
    /// Applies what running task `id` reported to its record, counting it in
    /// `progress`. Whether the task's state visibly changed, as
    /// [`TaskRecord::apply_event`] tells.
    pub fn apply_task_event(&mut self, id: &str, event: &c2m_protocol::progress::Event) -> bool {
        let Some(task) = self.task_mut(id) else {
            return false;
        };
        let visible = task.apply_event(event);
        self.progress = self.progress.wrapping_add(1);
        visible
    }
    pub fn next_task(&self) -> Option<&TaskRecord> {
        if self.reconciling {
            return None;
        }
        // A worker from an earlier session that still runs holds its note: a task
        // for the same note waits for it instead of stopping at its claim.
        let note_held = |task: &TaskRecord| {
            self.tasks.iter().any(|other| {
                other.held
                    && other.id != task.id
                    && other.plan.library_id == task.plan.library_id
                    && other.plan.source_id == task.plan.source_id
            })
        };
        self.tasks.iter().find(|t| {
            t.state == TaskState::Queued
                && t.intent == Intent::Run
                && t.handled_by.is_none()
                && !t.held
                && !note_held(t)
        })
    }

    /// Replace the current input; submitted tasks are unchanged.
    pub fn reset_input(&mut self, online: bool) -> String {
        self.replace_input(Draft::new(online));
        self.current_draft.clone()
    }

    pub fn switch_source_kind(&mut self, online: bool) {
        if self
            .draft()
            .is_none_or(|previous| previous.online != online)
        {
            // Source modes belong to the same form; the incompatible source and any
            // submitted or retried task association are cleared.
            self.reset_input(online);
        }
    }

    /// Replacing a submitted source starts another input. Edits to an unsubmitted
    /// source keep it.
    pub fn prepare_next_import(&mut self, value: &str) -> bool {
        let Some(previous) = self.draft() else {
            return false;
        };
        if previous.submitted_task.is_none() || previous.input == value {
            return false;
        }
        let submitted = previous.submitted_task.clone();
        let mut input = Draft::new(previous.online);
        input.change_source(value.to_owned());
        if let Some(task) = submitted.as_deref().and_then(|id| self.task_mut(id))
            && task.state == TaskState::Complete
        {
            // Choosing the next video acknowledges the result just presented.
            task.unread = false;
        }
        self.replace_input(input);
        true
    }

    /// Put a task's video back in the current form, as a new attempt at it, without
    /// mutating that task. `subtitle_events` comes from `task_subtitle_events`;
    /// without it the form asks for the subtitle to be read again.
    pub fn adjust_task(
        &mut self,
        id: &str,
        subtitle_events: Option<Vec<c2m_core::timeline::TranscriptEvent>>,
    ) -> Result<()> {
        let task = self.task(id).context(c2m_i18n::tr!("任务不存在"))?.clone();
        ensure!(
            task.handled_by.is_none(),
            c2m_i18n::tr!("此任务已有后续处理，请打开对应任务继续")
        );
        ensure!(
            !matches!(
                task.state,
                TaskState::Running | TaskState::Pausing | TaskState::Queued
            ) && !task.held,
            c2m_i18n::tr!("当前任务还在处理，请先暂停并等候当前步骤结束")
        );
        let mut draft = Draft::new(task.plan.source.online);
        draft.input = task.plan.source.input.clone();
        let mut source = task.plan.source;
        match subtitle_events.filter(|events| !events.is_empty()) {
            Some(events) => {
                if let Some(subtitle) = &mut source.selected_subtitle {
                    subtitle.events = events;
                }
            }
            None => source.selected_subtitle = None,
        }
        draft.source = Some(source);
        draft.text_source = task.plan.text_source;
        draft.subtitle = task.plan.subtitle;
        draft.retry_of = Some(id.to_owned());
        self.replace_input(draft);
        Ok(())
    }

    /// Task `by` has replaced the note in `note`: the other tasks whose result it was
    /// are replaced, as the next opening would find them.
    pub fn note_replaced(&mut self, note: &Path, by: &str) {
        for task in &mut self.tasks {
            if task.id != by && task.artifact.as_deref() == Some(note) {
                apply_artifact(task, ArtifactFact::Replaced { by: by.to_owned() });
            }
        }
    }

    /// The unfinished task already writing `plan`'s note, which `enqueue` returns
    /// instead of a new one: a note has one unfinished task at a time, since each
    /// replaces the note the last one left. The attempt `parent` names, the one
    /// `plan` replaces, is not one.
    pub fn duplicate_of(&self, plan: &TaskPlan, parent: Option<&str>) -> Option<&TaskRecord> {
        self.tasks.iter().find(|t| {
            !t.state.finished()
                && t.handled_by.is_none()
                && parent != Some(t.id.as_str())
                && t.plan.library_id == plan.library_id
                && t.plan.source_id == plan.source_id
        })
    }

    pub fn enqueue(&mut self, plan: TaskPlan, parent: Option<String>) -> Result<(String, bool)> {
        ensure!(
            !plan.source_id.is_empty() && !plan.title.trim().is_empty(),
            c2m_i18n::tr!("请先确认视频和笔记名称")
        );
        if let Some(id) = parent.as_deref() {
            let original = self.task(id).context(c2m_i18n::tr!("原任务记录不存在"))?;
            if let Some(followup) = &original.handled_by {
                ensure!(
                    self.task(followup).is_some(),
                    c2m_i18n::tr!("后续任务记录缺失，请先恢复任务记录")
                );
                return Ok((followup.clone(), false));
            }
            ensure!(
                !matches!(
                    original.state,
                    TaskState::Running | TaskState::Pausing | TaskState::Queued
                ),
                c2m_i18n::tr!("原任务仍在处理，请先暂停并等候当前步骤结束")
            );
        }
        let location = self
            .library(&plan.library_id)
            .context(c2m_i18n::tr!("所选保存位置未登记"))?;
        if let Some(existing) = self.duplicate_of(&plan, parent.as_deref()) {
            return Ok((existing.id.clone(), false));
        }
        let id = new_id("task");
        let work_dir = location.root.join(".course2md/work").join(&id);
        let now = now();
        self.tasks.push(TaskRecord {
            id: id.clone(),
            plan,
            state: TaskState::Queued,
            intent: Intent::Run,
            created: now,
            updated: now,
            parent: parent.clone(),
            handled_by: None,
            held: false,
            work_dir,
            artifact: None,
            unread: false,
            resend: Vec::new(),
            replaced_by: None,
            ran_with: None,
            // A new task has said nothing yet.
            events: JobEvents {
                loaded: true,
                ..Default::default()
            },
        });
        if let Some(parent) = parent.and_then(|id| self.task_mut(&id)) {
            parent.handled_by = Some(id.clone());
        }
        self.selected_task = Some(id.clone());
        Ok((id, true))
    }

    /// Fills in what a task record derives from the rest of the state: the work
    /// directory from its library, the reverse link from its follow-up's `parent`.
    fn link_tasks(&mut self) {
        let roots: BTreeMap<_, _> = self
            .libraries
            .iter()
            .map(|library| (library.id.clone(), library.root.clone()))
            .collect();
        for task in &mut self.tasks {
            task.handled_by = None;
            if let Some(root) = roots.get(&task.plan.library_id) {
                task.work_dir = root.join(".course2md/work").join(&task.id);
            }
        }
        let mut followups = self
            .tasks
            .iter()
            .filter_map(|task| {
                task.parent
                    .as_ref()
                    .map(|parent| (task.created, task.updated, task.id.clone(), parent.clone()))
            })
            .collect::<Vec<_>>();
        followups.sort();
        for (_, _, child, parent) in followups {
            if child != parent
                && let Some(task) = self.task_mut(&parent)
            {
                task.handled_by = Some(child);
            }
        }
    }

    /// Reconcile only process-owned states. Confirmed user stops survive restarts.
    /// The engine still checks its durable remote-request ledger before any send.
    pub fn recover(&mut self) {
        self.link_tasks();
        for task in &mut self.tasks {
            if task.handled_by.is_some() && !task.state.finished() {
                task.intent = Intent::Pause;
                task.state = TaskState::Paused;
                task.unread = false;
                continue;
            }
            if matches!(task.state, TaskState::Running | TaskState::Pausing) {
                task.state = match task.intent {
                    Intent::Run => TaskState::Queued,
                    Intent::Pause | Intent::Quit => TaskState::Paused,
                    Intent::Cancel => TaskState::Cancelled,
                };
                task.updated = now();
            }
            if task.state == TaskState::Queued && task.intent != Intent::Run {
                task.state = if task.intent == Intent::Cancel {
                    TaskState::Cancelled
                } else {
                    TaskState::Paused
                };
            }
        }
    }

    pub fn set_intent(&mut self, id: &str, intent: Intent) -> Result<()> {
        let task = self.task_mut(id).context(c2m_i18n::tr!("任务记录不存在"))?;
        ensure!(
            !task.state.finished(),
            c2m_i18n::tr!("这项任务已结束，可在任务卡中补做或重新生成")
        );
        ensure!(
            task.handled_by.is_none(),
            c2m_i18n::tr!("这项任务已有后续处理，请打开对应任务继续。")
        );
        ensure!(
            intent != Intent::Run || !matches!(task.state, TaskState::Running | TaskState::Pausing),
            c2m_i18n::tr!("这项任务正在处理当前操作")
        );
        ensure!(
            intent != Intent::Run || !task.has_uncertain(),
            c2m_i18n::tr!("仍有请求结果尚未确认，请查看请求范围后选择是否重新发送")
        );
        task.intent = intent;
        task.updated = now();
        task.state = match intent {
            Intent::Run => TaskState::Queued,
            Intent::Quit
                if matches!(task.state, TaskState::NeedsAttention | TaskState::Uncertain) =>
            {
                task.state
            }
            Intent::Cancel if matches!(task.state, TaskState::Running | TaskState::Pausing) => {
                TaskState::Pausing
            }
            Intent::Cancel => TaskState::Cancelled,
            _ if matches!(task.state, TaskState::Running | TaskState::Pausing) => {
                TaskState::Pausing
            }
            _ => TaskState::Paused,
        };
        Ok(())
    }

    /// Waiting tasks run in list order. Put one at `position` among them,
    /// taking the place of the task that was there.
    pub fn move_waiting_task(&mut self, id: &str, position: usize) -> Result<()> {
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
        let queued: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| task.waiting())
            .map(|(index, _)| index)
            .collect();
        let at = match queued.get(position) {
            Some(&index) => index,
            None => queued.last().map_or(self.tasks.len(), |&index| index + 1),
        };
        self.tasks.insert(at, task);
        Ok(())
    }

    /// Queued tasks in the order they will run.
    pub fn queued_tasks(&self) -> impl Iterator<Item = &TaskRecord> {
        self.tasks
            .iter()
            .filter(|task| task.state == TaskState::Queued)
    }

    /// Queued and paused tasks in queue order; a paused task resumes in its place.
    pub fn waiting_tasks(&self) -> impl Iterator<Item = &TaskRecord> {
        self.tasks.iter().filter(|task| task.waiting())
    }

    pub fn authorize_uncertain(&mut self, id: &str, requests: &[String]) -> Result<()> {
        let task = self.task_mut(id).context(c2m_i18n::tr!("任务记录不存在"))?;
        ensure!(
            task.handled_by.is_none(),
            c2m_i18n::tr!("这项任务已有后续处理，请打开对应任务继续")
        );
        ensure!(
            !matches!(
                task.state,
                TaskState::Running | TaskState::Pausing | TaskState::Queued
            ) && !task.state.finished()
                && !task.held,
            c2m_i18n::tr!("这项任务的状态已变化，请查看最新状态")
        );
        reconcile_receipts(task)?;
        let current: BTreeSet<_> = task
            .events
            .blocked
            .iter()
            .filter(|request| request.is_uncertain())
            .filter_map(|request| request.request_id.as_ref())
            .collect();
        let selected: BTreeSet<_> = requests.iter().collect();
        ensure!(
            !selected.is_empty() && current == selected,
            c2m_i18n::tr!("待确认的请求已变化，请查看最新范围后再选择是否重新发送")
        );
        // The task is neither waiting nor running, so an earlier authorization of these
        // requests was never used: its start failed or the task was paused. A request
        // that was resent is no longer uncertain under this id, so confirming again
        // cannot send anything twice.
        task.resend.retain(|id| !selected.contains(&id));
        task.resend.extend(requests.iter().cloned());
        task.intent = Intent::Run;
        task.state = TaskState::Queued;
        task.events.error = None;
        task.updated = now();
        Ok(())
    }

    /// Repair creates a new attempt that redoes `components` of the task's note with
    /// the settings current when it runs. The original task and the unrelated
    /// current input remain unchanged.
    pub fn reprocess(
        &mut self,
        id: &str,
        mut components: Vec<Component>,
        resend: Vec<String>,
    ) -> Result<String> {
        let mut original = self.task(id).context(c2m_i18n::tr!("任务不存在"))?.clone();
        if let Some(next) = &original.handled_by {
            ensure!(
                self.task(next).is_some(),
                c2m_i18n::tr!("后续任务记录缺失，请先恢复任务记录")
            );
            return Ok(next.clone());
        }
        ensure!(
            !matches!(
                original.state,
                TaskState::Running | TaskState::Pausing | TaskState::Queued
            ) && !original.held,
            c2m_i18n::tr!("当前任务仍在处理，请先等候当前步骤结束")
        );
        ensure!(
            original.replaced_by.is_none(),
            c2m_i18n::tr!("这份笔记已被之后的任务更新，请从最新的任务补做")
        );
        let base = original
            .artifact
            .clone()
            .context(c2m_i18n::tr!("此任务尚无可补做的笔记正文"))?;
        let manifest = c2m_artifact::artifact::note_manifest(&base)?
            .context(c2m_i18n::tr!("此任务尚无可补做的笔记正文"))?;
        ensure!(
            manifest.source_id == original.plan.source_id,
            c2m_i18n::tr!("笔记来源与任务不匹配")
        );
        // A repair redoes what this task's note lacks; a note another task has
        // replaced since lacks other things.
        ensure!(
            manifest.task_id == original.note_owner(),
            c2m_i18n::tr!("这份笔记已被之后的任务更新，请从最新的任务补做")
        );
        if !resend.is_empty() {
            reconcile_receipts(&mut original)?;
            // Resending redoes the parts the requests were made for, whatever else
            // was chosen with them.
            components.extend(original.uncertain_components()?);
        }
        let outcomes = original.events.outcomes.clone();
        components.sort();
        components.dedup();
        ensure!(
            !components.is_empty(),
            c2m_i18n::tr!("请选择需要补做的内容")
        );
        for component in &components {
            let failed = match (component, outcomes.as_ref()) {
                (Component::Exports, Some(outcomes)) => {
                    outcomes.exports.values().any(failed_outcome)
                }
                (component, Some(outcomes)) => {
                    failed_outcome(component_outcome(outcomes, *component))
                }
                (_, None) => false,
            };
            ensure!(failed, c2m_i18n::tr!("这部分无需补做"));
        }
        if !resend.is_empty() {
            let unknown: BTreeSet<_> = original
                .events
                .blocked
                .iter()
                .filter(|request| request.is_uncertain())
                .filter_map(|request| request.request_id.as_ref())
                .collect();
            ensure!(
                !unknown.is_empty() && unknown == resend.iter().collect(),
                c2m_i18n::tr!("待确认请求已变化，请查看最新范围后再选择是否重新发送")
            );
        } else {
            ensure!(
                !original.has_uncertain(),
                c2m_i18n::tr!("仍有请求结果尚未确认，请先查看请求范围")
            );
        }
        let only_exports = components
            .iter()
            .all(|component| *component == Component::Exports);
        // Translation is redone on its own or after the proofreading it follows, into
        // the language the note is in.
        let translate = components
            .iter()
            .any(|part| matches!(part, Component::Proofreading | Component::Translation))
            .then(|| {
                original
                    .ran_with
                    .as_ref()
                    .and_then(|ran_with| ran_with.config.llm.translate.clone())
            })
            .flatten();
        let formats = if only_exports {
            use c2m_config::config::OutputFormat;
            let formats: Vec<_> = [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json]
                .into_iter()
                .filter(|format| {
                    outcomes
                        .as_ref()
                        .and_then(|outcomes| outcomes.exports.get(&format.to_string()))
                        .is_some_and(failed_outcome)
                })
                .collect();
            ensure!(
                !formats.is_empty(),
                c2m_i18n::tr!("需要补做的导出格式记录不完整，请从笔记中重新选择导出格式")
            );
            Some(formats)
        } else {
            None
        };
        // A resend goes to the services the uncertain requests were made with.
        let resend_services = (!resend.is_empty())
            .then(|| original.ran_with.clone())
            .flatten();
        let mut plan = original.plan;
        plan.repair = Some(RepairScope { translate, formats });
        plan.operation = c2m_engine::execution::Operation::Reprocess {
            note_dir: base,
            base_task_id: manifest.task_id.clone(),
            components,
            prior_work_dir: Some(original.work_dir),
        };
        let (next, created) = self.enqueue(plan, Some(id.to_owned()))?;
        if created && let Some(task) = self.task_mut(&next) {
            task.resend = resend;
            if resend_services.is_some() {
                task.ran_with = resend_services;
            }
        }
        Ok(next)
    }

    /// Points a library at a new folder and rewrites every saved path inside the old
    /// ones. Moving, locating and restoring a library all come through here; keeping
    /// its id keeps folder ids, reading positions and service bindings stable.
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
        self.link_tasks();
        Ok(())
    }

    /// Adds tasks a library carries that this record has not seen. They are evidence of
    /// earlier work, never permission to run it.
    pub(crate) fn adopt_tasks(&mut self, tasks: Vec<TaskRecord>) {
        for mut task in tasks {
            if self.task(&task.id).is_none() {
                task.pause_as_restored();
                self.tasks.push(task);
            }
        }
        self.tasks.sort_by_key(|task| task.created);
        self.link_tasks();
    }

    pub fn stop_session(&mut self) {
        let ids: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| !t.state.finished() && t.intent == Intent::Run)
            .map(|t| t.id.clone())
            .collect();
        for id in ids {
            let _ = self.set_intent(&id, Intent::Quit);
        }
    }
}

pub struct PreparedLibrary {
    pub root: PathBuf,
    pub name: String,
    pub id: String,
    pub duplicate: bool,
    pub make_default: bool,
    /// Tasks the folder already carries.
    tasks: Vec<TaskRecord>,
}

pub struct Workspace {
    pub state: State,
    pub recovery: Option<String>,
    path: PathBuf,
    library_writes: LibraryWrites,
    /// Identifies this opened record, so findings for a record that was
    /// replaced meanwhile are not applied to its successor.
    identity: u64,
    /// When held tasks were last checked; see `held_check_due`.
    held_checked: Option<std::time::Instant>,
    /// A reconciliation was handed out and its findings are not applied yet. While
    /// it runs no held check is due: its result is what the next check waits for.
    checking: bool,
    /// Libraries whose courses saved as versions are not all flattened yet; see
    /// `crate::migrate`. Every library is, once, when the record opens.
    migration_due: BTreeSet<String>,
    /// What the last migration means for the person: once older versions were
    /// removed, the notice; whether the library must be read again.
    migrated: Option<MigrationOutcome>,
}

/// What a migration applied by `Workspace::apply_reconciliation` changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MigrationOutcome {
    /// Notes changed on disk: the library listing is out of date.
    pub notes_changed: bool,
    /// Older versions were removed: the notice to show once.
    pub notice: Option<String>,
}

/// How often a task held by a worker from an earlier session is checked again.
const HELD_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Serialize, Deserialize)]
struct TaskMirror {
    schema: u32,
    /// The library root when this was written, so paths can follow a moved library.
    #[serde(default)]
    root: Option<PathBuf>,
    task: TaskRecord,
}

/// Files that live in a library, which can be a slow or disconnected disk, so they
/// never block the interface: the record itself is what a commit waits for. Writes
/// run in order on the blocking pool, and a newer write of a file replaces one still
/// waiting. A mirror is recovery evidence and reading the library rewrites it; a failed
/// control write stops the job, which then sends nothing new.
#[derive(Clone, Default)]
pub struct LibraryWrites {
    pending: Arc<Mutex<BTreeMap<PathBuf, LibraryWrite>>>,
    writing: Arc<Mutex<()>>,
    failure: Arc<Mutex<Option<WriteFailure>>>,
    wake: Arc<Mutex<Option<smol::channel::Sender<()>>>>,
}

/// A file's bytes, and for a mirror the library it belongs to.
type LibraryWrite = (Vec<u8>, Option<LibraryLocation>);

#[derive(Debug)]
pub struct WriteFailure {
    pub message: String,
    /// The running job did not get its latest instruction.
    pub control: bool,
}

impl LibraryWrites {
    fn queue(&self, path: PathBuf, bytes: Vec<u8>, mirror_of: Option<LibraryLocation>) {
        c2m_process::runtime::lock_recover(&self.pending).insert(path, (bytes, mirror_of));
        let this = self.clone();
        drop(desktop_prefs::spawn_blocking_io(move || this.drain()));
    }

    /// Performs everything queued so far, waiting for any write already running.
    pub fn drain(&self) {
        let _writing = c2m_process::runtime::lock_recover(&self.writing);
        let writes = std::mem::take(&mut *c2m_process::runtime::lock_recover(&self.pending));
        for (path, (bytes, mirror_of)) in writes {
            let control = mirror_of.is_none();
            let result = match mirror_of {
                Some(location) => write_mirror(&path, &bytes, &location),
                None => write_control(&path, &bytes),
            };
            if let Err(error) = result {
                *c2m_process::runtime::lock_recover(&self.failure) = Some(WriteFailure {
                    message: format!("{error:#}"),
                    control,
                });
                if let Some(wake) = c2m_process::runtime::lock_recover(&self.wake).as_ref() {
                    let _ = wake.try_send(());
                }
            }
        }
    }

    /// Writes a task's control file now, for a job about to start.
    pub fn write_control_now(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let _writing = c2m_process::runtime::lock_recover(&self.writing);
        write_control(path, bytes)
    }
}

fn write_control(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .context(c2m_i18n::tr!("任务记录缺少保存目录"))?,
    )?;
    c2m_core::fsio::atomic_write(path, bytes)
}

/// A task record as its library keeps it.
fn mirror_bytes(task: &TaskRecord, location: &LibraryLocation) -> Result<Vec<u8>> {
    let mut task = task.clone();
    forget_task_subtitles(&mut task);
    Ok(serde_json::to_vec(&TaskMirror {
        schema: SCHEMA,
        root: Some(location.root.clone()),
        task,
    })?)
}

/// A disconnected library catches up when it is read again.
fn write_mirror(path: &Path, bytes: &[u8], location: &LibraryLocation) -> Result<()> {
    if check_library(location).is_err() {
        return Ok(());
    }
    if let Some(directory) = path.parent() {
        ensure_work_directory(directory, location)?;
    }
    if std::fs::read(path).ok().as_deref() != Some(bytes) {
        c2m_core::fsio::atomic_write(path, bytes)
            .context(c2m_i18n::tr!("任务恢复副本未能保存，原任务记录仍保留"))?;
    }
    Ok(())
}

fn validate_record_location(task: &TaskRecord, libraries: &[LibraryLocation]) -> Result<()> {
    ensure!(
        c2m_engine::execution::valid_id(&task.id),
        c2m_i18n::tr!("任务身份无效")
    );
    let library = libraries
        .iter()
        .find(|library| library.id == task.plan.library_id)
        .context(c2m_i18n::tr!("任务保存位置记录缺失"))?;
    ensure!(
        library.root.is_absolute(),
        c2m_i18n::tr!("任务工作目录与记录的笔记库不匹配")
    );
    ensure!(
        !task.plan.source_id.trim().is_empty() && !task.plan.title.trim().is_empty(),
        c2m_i18n::tr!("任务来源身份或名称记录缺失")
    );
    Ok(())
}

/// Creates a task's work directory under its library, refusing anything that has taken
/// its place on the way there.
fn ensure_work_directory(directory: &Path, library: &LibraryLocation) -> Result<()> {
    check_library(library)?;
    let mut current = library.root.clone();
    for component in directory.strip_prefix(&library.root)? {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                c2m_i18n::tr!("任务工作目录被其他文件或链接占用，原内容已保留")
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A starting job may be creating the same directories.
                if let Err(error) = std::fs::create_dir(&current)
                    && error.kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(error.into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn pause_recovered_control(task: &TaskRecord) -> Result<()> {
    if !task.work_dir.is_dir() {
        return Ok(());
    }
    let path = task.work_dir.join("control.json");
    let mut control = if path.is_file() {
        let bytes = desktop_prefs::store::read(&path)?;
        c2m_core::fsio::atomic_write(
            &task
                .work_dir
                .join(format!("{}.json", new_id("control-before-recovery"))),
            &bytes,
        )?;
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .filter(|value| value.is_object())
            .unwrap_or_else(|| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    control["intent"] = serde_json::to_value(Intent::Pause)?;
    control["resend"] = serde_json::json!([]);
    c2m_core::fsio::atomic_write(&path, &serde_json::to_vec(&control)?)
}

fn failed_outcome(outcome: &c2m_artifact::artifact::Outcome) -> bool {
    matches!(
        outcome.status,
        c2m_artifact::artifact::Status::Failed | c2m_artifact::artifact::Status::Partial
    )
}

fn component_outcome(
    outcomes: &c2m_artifact::artifact::Outcomes,
    component: Component,
) -> &c2m_artifact::artifact::Outcome {
    match component {
        Component::Screenshots => &outcomes.screenshots,
        Component::Proofreading => &outcomes.proofreading,
        Component::Translation => &outcomes.translation,
        Component::Summary => &outcomes.summary,
        Component::Exports => &outcomes.transcript,
    }
}

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

/// What the library holds for a task, read without changing the record.
#[derive(Clone)]
enum ArtifactFact {
    Nothing,
    /// The task published its note, which the record does not know yet.
    Published {
        note: PathBuf,
        outcomes: Box<c2m_artifact::artifact::Outcomes>,
        partial: bool,
    },
    /// The task's result is still its note, recorded elsewhere: a version folder an
    /// older app kept, since flattened into the note.
    Moved {
        note: PathBuf,
    },
    /// Another task's note replaced this task's, or the note this task was to
    /// repair: it never runs again.
    Replaced {
        by: String,
    },
    Exports {
        base: PathBuf,
        outcomes: BTreeMap<String, c2m_artifact::artifact::Outcome>,
    },
}

/// The task the note in `note` is the result of, read without checking its files.
/// `None` when there is no note this app reads, or it is still saved as versions.
fn note_task(note: &Path) -> Option<String> {
    if c2m_artifact::artifact::is_legacy(note) {
        return None;
    }
    c2m_artifact::artifact::note_manifest(note)
        .ok()
        .flatten()
        .map(|manifest| manifest.task_id)
}

/// The published result of a task, if any. Reads and hashes files; the
/// result is applied by `apply_artifact`.
fn probe_artifact(task: &TaskRecord, location: &LibraryLocation) -> Result<ArtifactFact> {
    let note = task_note(task, location);
    if task.artifact.is_some() && !task.exports_only() {
        // Finished: only whose note it is now is read, never hashed.
        return Ok(match note_task(&note) {
            Some(owner) if owner == task.id => {
                if task.artifact.as_ref() == Some(&note) {
                    ArtifactFact::Nothing
                } else {
                    ArtifactFact::Moved { note }
                }
            }
            Some(owner) => ArtifactFact::Replaced { by: owner },
            None => ArtifactFact::Nothing,
        });
    }
    // The note is this task's result while its manifest names the task.
    if !task.exports_only()
        && let Some(manifest) = c2m_artifact::artifact::published_by(&note, &task.id)?
    {
        ensure!(
            manifest.source_id == task.plan.source_id,
            c2m_i18n::tr!("已发布笔记与任务身份不匹配，原文件已保留")
        );
        return Ok(ArtifactFact::Published {
            note,
            outcomes: Box::new(manifest.outcomes),
            partial: manifest.partial,
        });
    }
    if let c2m_engine::execution::Operation::Reprocess {
        note_dir,
        base_task_id,
        ..
    } = &task.plan.operation
    {
        // A repair, or exports, of a note another task has replaced since: running
        // it would stop before any request, every time.
        if let Some(owner) =
            note_task(&note).filter(|owner| owner != base_task_id && *owner != task.id)
        {
            return Ok(ArtifactFact::Replaced { by: owner });
        }
        if task.exports_only() && task.work_dir.join("export-result.json").is_file() {
            #[derive(serde::Deserialize)]
            struct ExportResult {
                #[serde(default)]
                outcomes: BTreeMap<String, c2m_artifact::artifact::Outcome>,
            }
            let mut record: ExportResult =
                serde_json::from_slice(&std::fs::read(task.work_dir.join("export-result.json"))?)?;
            ensure!(
                !record.outcomes.is_empty(),
                c2m_i18n::tr!("导出结果记录为空")
            );
            let export_directory = task_export_directory(task, location)
                .context(c2m_i18n::tr!("导出位置与任务记录不匹配"))?;
            for (format, outcome) in &mut record.outcomes {
                if outcome.status == c2m_artifact::artifact::Status::Succeeded {
                    let Some(format) = output_format(format) else {
                        continue;
                    };
                    let expected = export_directory.join(c2m_artifact::portable::file_name(format));
                    if !expected.is_file() {
                        *outcome = c2m_artifact::artifact::Outcome::failed(c2m_i18n::tr!(
                            "导出文件已移动或无法读取，可以重新导出"
                        ));
                    }
                }
            }
            return Ok(ArtifactFact::Exports {
                base: note_dir.clone(),
                outcomes: record.outcomes,
            });
        }
        // Exports an older app wrote beside a version, now in the flat note.
        if task
            .artifact
            .as_ref()
            .is_some_and(|artifact| *artifact != note)
            && note_task(&note).as_ref() == Some(base_task_id)
        {
            return Ok(ArtifactFact::Moved { note });
        }
    }
    Ok(ArtifactFact::Nothing)
}

/// Why a task stopped for good when another task's note replaced the one it was for.
pub fn replaced_message() -> &'static str {
    c2m_i18n::tr!("笔记已被另一项任务更新，这项任务不会再运行")
}

fn apply_artifact(task: &mut TaskRecord, fact: ArtifactFact) {
    match fact {
        ArtifactFact::Nothing => {}
        // A result recorded meanwhile is newer than what startup found.
        ArtifactFact::Published { .. } if task.artifact.is_some() && !task.exports_only() => {}
        ArtifactFact::Published {
            note,
            outcomes,
            partial,
        } => {
            task.artifact = Some(note);
            task.events.outcomes = Some(*outcomes);
            task.state = if partial {
                TaskState::Partial
            } else {
                TaskState::Complete
            };
            task.events.error = None;
            task.unread = true;
        }
        ArtifactFact::Moved { note } => task.artifact = Some(note),
        ArtifactFact::Replaced { by } => {
            task.replaced_by = Some(by);
            task.artifact = None;
            task.resend.clear();
            if !task.state.finished() {
                task.intent = Intent::Cancel;
                task.state = TaskState::Cancelled;
                task.events.error = Some(replaced_message().into());
                task.unread = true;
            }
        }
        ArtifactFact::Exports { base, outcomes } => {
            let next_state = if outcomes.values().any(failed_outcome) {
                TaskState::Partial
            } else {
                TaskState::Complete
            };
            let previous_artifact = task.artifact.replace(base);
            task.events
                .outcomes
                .get_or_insert_with(c2m_artifact::artifact::Outcomes::default)
                .exports = outcomes;
            task.unread |= previous_artifact != task.artifact || task.state != next_state;
            task.state = next_state;
        }
    }
}

/// The output format a saved export outcome is keyed by.
fn output_format(key: &str) -> Option<c2m_config::config::OutputFormat> {
    key.parse().ok()
}

/// What a task's receipts say: the requests whose result is unknown, and the
/// purposes that have any attempt at all.
type ReceiptFacts = (Vec<BlockedRequest>, BTreeSet<String>);

/// Requests whose result is unknown, read from the task's receipts.
fn uncertain_requests(task: &TaskRecord) -> Result<ReceiptFacts> {
    let receipts = c2m_dispatch::dispatch::receipts(&task.work_dir)?;
    let attempted = receipts
        .iter()
        .map(|receipt| receipt.purpose.clone())
        .collect();
    let uncertain = receipts
        .into_iter()
        .filter(|r| {
            matches!(
                r.state,
                c2m_dispatch::dispatch::State::Sending | c2m_dispatch::dispatch::State::Uncertain
            )
        })
        .map(|receipt| BlockedRequest {
            reason: BlockReason::Uncertain,
            request_id: Some(receipt.request_id),
            purpose: Some(receipt.purpose),
            description: receipt.description.clone(),
            kind: Some(c2m_artifact::artifact::FailureKind::Uncertain),
            message: if receipt.description.trim().is_empty() {
                receipt
                    .message
                    .unwrap_or_else(|| c2m_i18n::tr!("请求已发送，结果未确认").into())
            } else {
                c2m_i18n::tr!(
                    "{}：{}",
                    receipt.description,
                    receipt
                        .message
                        .unwrap_or_else(|| { c2m_i18n::tr!("请求结果未确认").into() })
                )
            },
        })
        .collect();
    Ok((uncertain, attempted))
}

fn apply_receipts(task: &mut TaskRecord, (uncertain, attempted): ReceiptFacts) {
    task.events
        .blocked
        .retain(|request| !request.is_uncertain());
    task.events.blocked.extend(uncertain);
    task.events.attempted = attempted;
    if task.has_uncertain() && task.intent == Intent::Run {
        task.state = TaskState::Uncertain;
        task.unread = true;
    } else if !task.has_uncertain() && task.state == TaskState::Uncertain {
        // A worker that outlived the app settled them: nothing is left to decide,
        // and a completed request is read back, never sent again.
        task.state = if task.intent == Intent::Run {
            TaskState::Queued
        } else {
            TaskState::Paused
        };
        task.events.error = None;
        task.unread = true;
    }
}

/// Whether a worker still holds the task's folder: the lock every worker takes on entry
/// (`execution::enter_work_dir`). A missing lock file means no worker ever got that far.
fn worker_holds(work_dir: &Path) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(work_dir.join(".task.lock"))
        .is_ok_and(|file| matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}

/// Whether the task's work directory holds AI requests that were answered: work
/// already paid for. Blocking: it reads the task's request records.
pub fn has_answered_ai_requests(task: &TaskRecord) -> bool {
    c2m_dispatch::dispatch::receipts(&task.work_dir).is_ok_and(|receipts| {
        receipts.iter().any(|receipt| {
            receipt.state == c2m_dispatch::dispatch::State::Completed
                && Component::parse(&receipt.purpose).is_some_and(Component::is_ai)
        })
    })
}

pub fn reconcile_receipts(task: &mut TaskRecord) -> Result<()> {
    let uncertain = uncertain_requests(task)?;
    apply_receipts(task, uncertain);
    Ok(())
}

/// Where a mirror written before streams were saved is set aside. It is not read.
const OLD_MIRROR_FILE: &str = "task-record-v1.json";

/// The events a task saved, read back from its stream.
fn read_task_events(task: &TaskRecord) -> Result<JobEvents> {
    let mut events = JobEvents::default();
    let old_mirror = task.work_dir.join(OLD_MIRROR_FILE);
    if old_mirror.is_file() {
        events.logs.push(c2m_i18n::tr!(
            "此任务在旧版本中的进度记录不再读取，原文件保留在 {}",
            old_mirror.display()
        ));
    }
    match desktop_prefs::store::read(&task.work_dir.join(TASK_EVENTS_FILE)) {
        Ok(bytes) => events.replay(&bytes),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    // Whether an error still stands is the record's to say: the task may have moved on.
    events.error = task.events.error.clone();
    if task.state.finished() && events.logs.len() > FINISHED_TASK_LOG_LINES {
        events
            .logs
            .drain(..events.logs.len() - FINISHED_TASK_LOG_LINES);
    }
    events.loaded = true;
    Ok(events)
}

/// A mirror written before streams were saved is not read, and moves aside, unchanged,
/// rather than be overwritten by this task's next mirror.
fn keep_old_mirror(directory: &Path) -> Result<()> {
    let path = directory.join("task-record.json");
    if desktop_prefs::store::read(&path).is_ok_and(|bytes| older_record(&bytes)) {
        std::fs::rename(&path, directory.join(OLD_MIRROR_FILE))?;
    }
    Ok(())
}

/// The library folders a record from before 3.0 names, read for their `root` alone. A
/// folder that is there and carries a library identity comes back under that identity
/// and its own name; any other is returned without one. Nothing is written.
fn older_library_roots(bytes: &[u8]) -> Vec<(PathBuf, Option<LibraryLocation>)> {
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return Vec::new();
    };
    let roots = record["libraries"].as_array().into_iter().flatten();
    roots
        .filter_map(|library| Some(PathBuf::from(library["root"].as_str()?)))
        .filter(|root| root.is_absolute())
        .map(|root| {
            let id = std::fs::read_to_string(root.join(crate::storage::LIBRARY_ID_FILE))
                .ok()
                .map(|id| id.trim().to_owned())
                .filter(|id| root.is_dir() && c2m_engine::execution::valid_id(id));
            let location = id.map(|id| LibraryLocation {
                id,
                name: root.file_name().map_or_else(
                    || root.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                ),
                root: root.clone(),
                previous_roots: Vec::new(),
            });
            (root, location)
        })
        .collect()
}

/// A record or mirror an app before 3.0 wrote (schema 1), which this one no longer reads.
fn older_record(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .is_ok_and(|record| record.get("schema").and_then(|v| v.as_u64()) == Some(1))
}

/// What the startup check found on disk for one task.
enum TaskFacts {
    /// The library is not reachable; the task keeps what it has.
    Unavailable,
    /// A worker from an earlier session still holds the folder; checked again later.
    Held,
    Found {
        artifact: ArtifactFact,
        uncertain: ReceiptFacts,
        events: Box<JobEvents>,
        outcomes: Option<Box<c2m_artifact::artifact::Outcomes>>,
        /// The library copy of the record could not be brought up to date. What was
        /// read stands; the copy goes to the library writes, which retry and report.
        mirror_failed: bool,
    },
    Unreadable(String),
}

impl TaskFacts {
    fn apply(self, task: &mut TaskRecord) {
        task.held = matches!(self, Self::Held);
        match self {
            Self::Unavailable | Self::Held => {}
            Self::Found {
                artifact,
                uncertain,
                events,
                outcomes,
                mirror_failed: _,
            } => {
                forget_task_subtitles(task);
                task.events = *events;
                task.events.outcomes = task.events.outcomes.take().or(outcomes.map(|o| *o));
                apply_artifact(task, artifact);
                apply_receipts(task, uncertain);
            }
            Self::Unreadable(error) => {
                task.events.loaded = true;
                task.state = TaskState::NeedsAttention;
                task.events.error = Some(c2m_i18n::tr!(
                    "请求记录暂时无法读取，尚未发送新请求：{}",
                    error
                ));
            }
        }
    }
}

/// Checks of published results and request receipts, and reading back what each task
/// said. They read every task's files in its library, so they run on a blocking thread
/// after the window opens; meanwhile the UI shows the last saved state and no task is
/// dispatched (`State::next_task` waits for them).
pub struct Reconciliation {
    workspace: u64,
    tasks: Vec<(TaskRecord, Option<LibraryLocation>)>,
    /// Libraries to migrate first, with the notes and work folders of their
    /// unfinished tasks.
    migrations: Vec<(LibraryLocation, Vec<(PathBuf, PathBuf)>)>,
}

/// The findings of a [`Reconciliation`], applied by `Workspace::apply_reconciliation`.
pub struct Reconciled {
    workspace: u64,
    facts: Vec<(String, TaskFacts)>,
    migrations: Vec<crate::migrate::Migration>,
}

fn reconcile_task(task: &TaskRecord, location: &LibraryLocation) -> Result<TaskFacts> {
    keep_old_mirror(&task.work_dir)?;
    let artifact = probe_artifact(task, location)?;
    let uncertain = uncertain_requests(task)?;
    let events = read_task_events(task)?;
    store_task_subtitles(task)?;
    // A task that stopped before reporting its result still has its note's, while
    // the note is still its own.
    let outcomes = task
        .artifact
        .as_ref()
        .filter(|_| events.outcomes.is_none() && !task.exports_only())
        .and_then(|_| {
            c2m_artifact::artifact::note_manifest(&task_note(task, location))
                .ok()
                .flatten()
        })
        .filter(|manifest| manifest.task_id == task.id)
        .map(|manifest| Box::new(manifest.outcomes));
    // Catches up on writes made while disconnected. A failure here says nothing about
    // the receipts just read, so it must not turn an uncertain task into a failed one.
    let mirror_failed = write_mirror(
        &task.work_dir.join("task-record.json"),
        &mirror_bytes(task, location)?,
        location,
    )
    .is_err();
    Ok(TaskFacts::Found {
        artifact,
        uncertain,
        events: Box::new(events),
        outcomes,
        mirror_failed,
    })
}

impl Reconciliation {
    /// Blocking: run through `desktop_prefs::spawn_blocking_io`.
    pub fn run(self) -> Reconciled {
        // Before the tasks are read: a task's record follows from the flat note.
        let migrations = self
            .migrations
            .iter()
            .filter(|(location, _)| check_library(location).is_ok())
            .map(|(location, writers)| {
                // A worker from an earlier session may be an older app's, which
                // writes versions: its course waits until it lets go.
                let busy: Vec<PathBuf> = writers
                    .iter()
                    .filter(|(_, work_dir)| worker_holds(work_dir))
                    .map(|(note, _)| note.clone())
                    .collect();
                crate::migrate::migrate_library(location, &busy)
            })
            .collect();
        Reconciled {
            workspace: self.workspace,
            migrations,
            facts: self
                .tasks
                .into_iter()
                .map(|(task, location)| {
                    let facts = match location.filter(|location| check_library(location).is_ok()) {
                        // Its receipts may still say Sending for a request in flight.
                        Some(_) if !task.state.finished() && worker_holds(&task.work_dir) => {
                            TaskFacts::Held
                        }
                        Some(location) => reconcile_task(&task, &location)
                            .unwrap_or_else(|error| TaskFacts::Unreadable(format!("{error:#}"))),
                        None => TaskFacts::Unavailable,
                    };
                    (task.id, facts)
                })
                .collect(),
        }
    }
}

/// Saves the parsed subtitle a queued task carries inline beside its materials, so
/// the record need not keep it once the task starts. Blocking.
pub fn store_task_subtitles(task: &TaskRecord) -> Result<()> {
    if let Some(subtitle) = &task.plan.source.selected_subtitle
        && !subtitle.events.is_empty()
    {
        c2m_core::fsio::atomic_write(
            &task.work_dir.join(TASK_SUBTITLE_FILE),
            &serde_json::to_vec(&subtitle.events)?,
        )?;
    }
    Ok(())
}

/// The record stops carrying subtitle text once `store_task_subtitles` has saved it.
pub fn forget_task_subtitles(task: &mut TaskRecord) {
    if let Some(subtitle) = &mut task.plan.source.selected_subtitle {
        subtitle.events = Vec::new();
    }
}

/// Task records a library's folder carries, with their paths moved to where the folder
/// is now. Unreadable ones are named in the second list; their materials stay. Blocking.
pub(crate) fn read_library_tasks(location: &LibraryLocation) -> (Vec<TaskRecord>, Vec<String>) {
    let mut tasks = Vec::new();
    let mut issues = Vec::new();
    let Ok(entries) = std::fs::read_dir(location.root.join(".course2md/work")) else {
        return (tasks, issues);
    };
    for entry in entries.flatten() {
        let snapshot = entry.path().join("task-record.json");
        if entry.file_type().is_ok_and(|kind| kind.is_symlink()) || !snapshot.is_file() {
            continue;
        }
        let bytes = desktop_prefs::store::read(&snapshot).unwrap_or_default();
        let restored = (|| {
            let mirror: TaskMirror = serde_json::from_slice(&bytes)?;
            ensure!(
                mirror.schema == SCHEMA
                    && c2m_engine::execution::valid_id(&mirror.task.id)
                    && entry.file_name().to_str() == Some(mirror.task.id.as_str()),
                c2m_i18n::tr!("任务镜像身份不匹配")
            );
            let mut task = mirror.task;
            if let Some(root) = mirror.root {
                task.relocate(&root, &location.root);
            }
            task.plan.library_id = location.id.clone();
            task.work_dir = entry.path();
            anyhow::Ok(task)
        })();
        match restored {
            Ok(task) => tasks.push(task),
            Err(_) if older_record(&bytes) => issues.push(c2m_i18n::tr!(
                "{} 的任务记录来自 3.0 之前的版本，此版本不再读取，原文件已保留",
                entry.file_name().to_string_lossy()
            )),
            Err(_) => issues.push(c2m_i18n::tr!(
                "{} 的任务参数仍无法读取，材料已保留",
                entry.file_name().to_string_lossy()
            )),
        }
    }
    (tasks, issues)
}

/// Finished tasks keep a short log tail, and only recent cancelled attempts
/// without a note remain listed.
fn compact_history(state: &mut State) {
    let referenced: BTreeSet<String> = state
        .tasks
        .iter()
        .flat_map(|task| task.parent.iter().chain(task.handled_by.iter()))
        .chain(state.selected_task.iter())
        .chain(
            state
                .drafts
                .iter()
                .flat_map(|draft| draft.retry_of.iter().chain(draft.submitted_task.iter())),
        )
        .cloned()
        .collect();
    let mut cancelled: Vec<_> = state
        .tasks
        .iter()
        .filter(|task| {
            task.state == TaskState::Cancelled
                && task.artifact.is_none()
                && !referenced.contains(&task.id)
        })
        .map(|task| (task.updated, task.id.clone()))
        .collect();
    if cancelled.len() <= KEPT_CANCELLED_TASKS {
        return;
    }
    cancelled.sort();
    let pruned: BTreeSet<String> = cancelled
        .into_iter()
        .rev()
        .skip(KEPT_CANCELLED_TASKS)
        .map(|(_, id)| id)
        .collect();
    state.tasks.retain(|task| !pruned.contains(&task.id));
}

/// Parsed subtitle events the task was submitted with. A repair attempt reuses
/// its base note's transcript and has none of its own.
pub fn task_subtitle_events(
    task: &TaskRecord,
) -> Result<Option<Vec<c2m_core::timeline::TranscriptEvent>>> {
    let Some(subtitle) = &task.plan.source.selected_subtitle else {
        return Ok(None);
    };
    if !subtitle.events.is_empty() {
        return Ok(Some(subtitle.events.clone()));
    }
    let path = task.work_dir.join(TASK_SUBTITLE_FILE);
    if path.is_file() {
        return Ok(Some(serde_json::from_slice(&desktop_prefs::store::read(
            &path,
        )?)?));
    }
    ensure!(
        matches!(
            task.plan.operation,
            c2m_engine::execution::Operation::Reprocess { .. }
        ),
        c2m_i18n::tr!("所选字幕的内容已丢失，请重新读取字幕后再试")
    );
    Ok(None)
}

fn draft_subtitle_path(record: &Path) -> PathBuf {
    record.with_extension("subtitle.json")
}

/// Restore the draft's subtitle events from the file beside the record. A
/// missing or mismatched file leaves the subtitle to be read again.
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

/// Writes the record; the draft's subtitle goes first, because the record refers to it.
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

/// A list salvaged from a record that no longer reads as a whole.
fn salvage<T: serde::de::DeserializeOwned>(records: &[serde_json::Value], key: &str) -> Vec<T> {
    records
        .iter()
        .filter_map(|record| record.get(key)?.as_array())
        .flatten()
        .filter_map(|item| serde_json::from_value(item.clone()).ok())
        .collect()
}

impl Workspace {
    /// Explicit repair: originals are archived, then the state comes back from the last
    /// record that reads, plus what each library carries. No restored task may dispatch work.
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
        // A record from before 3.0 is archived with the rest, and nothing is read from it.
        let records: Vec<serde_json::Value> = originals
            .iter()
            .filter(|(_, bytes)| !older_record(bytes))
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
        // A record that no longer reads as a whole can still name what it knew.
        for location in salvage::<LibraryLocation>(&records, "libraries") {
            if c2m_engine::execution::valid_id(&location.id)
                && location.root.is_absolute()
                && !state
                    .libraries
                    .iter()
                    .any(|l| l.id == location.id || l.root == location.root)
            {
                state.libraries.push(location);
            }
        }
        // A record from before 3.0 gives back only where its libraries are, and only a
        // folder that names its library itself; another is for the person to add again.
        let mut issues = Vec::new();
        for (_, bytes) in originals.iter().filter(|(_, bytes)| older_record(bytes)) {
            for (folder, found) in older_library_roots(bytes) {
                match found {
                    Some(location)
                        if !state
                            .libraries
                            .iter()
                            .any(|l| l.id == location.id || l.root == location.root) =>
                    {
                        state.libraries.push(location)
                    }
                    Some(_) => {}
                    None => issues.push(c2m_i18n::tr!(
                        "旧版本记录中的笔记库 {} 需要在存储设置中重新添加",
                        folder.display()
                    )),
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
        state.adopt_tasks(salvage(&records, "tasks"));
        for library in state.libraries.clone() {
            if check_library(&library).is_ok() {
                let (tasks, unreadable) = read_library_tasks(&library);
                issues.extend(unreadable);
                state.adopt_tasks(tasks);
            }
        }
        let libraries = state.libraries.clone();
        state.tasks.retain(|task| {
            let kept = validate_record_location(task, &libraries).is_ok();
            if !kept {
                issues.push(c2m_i18n::tr!(
                    "《{}》的位置或身份记录不完整，原记录已保留",
                    task.plan.title
                ));
            }
            kept
        });
        for task in &mut state.tasks {
            task.pause_as_restored();
            if libraries
                .iter()
                .any(|l| l.id == task.plan.library_id && check_library(l).is_ok())
            {
                pause_recovered_control(task).context(c2m_i18n::tr!(
                    "无法保存恢复后的暂停指令，尚未替换原任务记录"
                ))?;
            }
        }
        state.retain_current_input();
        state.recover();
        hydrate_draft_subtitles(&mut state, &path);
        c2m_core::fsio::atomic_write(&path, &serde_json::to_vec(&state)?)?;
        let rebuilt = |count: usize, archive: &std::path::Path, issues: String| {
            // English counts its noun; the catalog keeps the singular apart.
            if count == 1 {
                c2m_i18n::tr!(
                    "已重建 1 项任务记录，未启动处理。原始记录保存在 {}。{}",
                    archive.display(),
                    issues
                )
            } else {
                c2m_i18n::tr!(
                    "已重建 {} 项任务记录，未启动处理。原始记录保存在 {}。{}",
                    count,
                    archive.display(),
                    issues
                )
            }
        };
        let recovery = rebuilt(
            state.tasks.len(),
            &archive,
            if issues.is_empty() {
                String::new()
            } else {
                issues.join(c2m_i18n::tr!("；"))
            },
        );
        Ok(Self::assemble(state, Some(recovery), path))
    }

    fn assemble(state: State, recovery: Option<String>, path: PathBuf) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut state = state;
        // The startup checks also migrate every library: nothing is dispatched first.
        state.reconciling = true;
        Self {
            recovery,
            path,
            library_writes: LibraryWrites::default(),
            identity: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            held_checked: None,
            checking: false,
            migration_due: state.libraries.iter().map(|l| l.id.clone()).collect(),
            migrated: None,
            state,
        }
    }

    pub fn open(root: PathBuf) -> Result<Self> {
        Self::open_at(
            c2m_config::config::config_dir().join("desktop-workspace.json"),
            root,
        )
    }
    /// Opens the record without reconciling it with the libraries; call
    /// `reconciliation` and `apply_reconciliation` before work is dispatched.
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
        // Left exactly as it is: rebuilding the record archives it first.
        if schema.is_some_and(|schema| schema < SCHEMA as u64) {
            bail!(c2m_i18n::tr!(
                "任务记录来自 3.0 之前的版本，此版本不再读取。原记录已原样保留：{}。可以选择「备份并重建任务记录」开始新的记录。",
                path.display()
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
                            if !task.state.finished() {
                                task.intent = Intent::Pause;
                                task.state = TaskState::Paused;
                                task.resend.clear();
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

    /// The checks still owed, to run on a blocking thread. A running task is telling
    /// the app what it is doing, and has nothing to read back. Until its findings are
    /// applied no other check is handed out.
    pub fn reconciliation(&mut self) -> Option<Reconciliation> {
        self.reconciliation_at(std::time::Instant::now())
    }

    /// `reconciliation` as of `now`.
    pub fn reconciliation_at(&mut self, now: std::time::Instant) -> Option<Reconciliation> {
        // `held_check_due` is `None` while a check runs, so a held one is never handed
        // out twice; the startup checks may be, for a record that was replaced.
        let held =
            !self.state.reconciling && self.held_check_due(now).is_some_and(|due| due <= now);
        if !(self.state.reconciling || held) {
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
                    .filter(|task| !task.state.finished() && task.plan.library_id == library.id)
                    .map(|task| (task_note(task, library), task.work_dir.clone()))
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
                .filter(|task| {
                    !task.events.loaded
                        && !matches!(task.state, TaskState::Running | TaskState::Pausing)
                        && (!held || task.held)
                })
                .map(|task| {
                    (
                        task.clone(),
                        self.state.library(&task.plan.library_id).cloned(),
                    )
                })
                .collect(),
        })
    }

    /// When a task held by a worker from an earlier session is next checked, if any
    /// is: a second after the last check, never while one runs. `now` when none ran yet.
    pub fn held_check_due(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        (!self.checking && self.state.tasks.iter().any(|task| task.held)).then(|| {
            self.held_checked
                .map_or(now, |checked| checked + HELD_CHECK_INTERVAL)
        })
    }

    /// Tasks of a library that has come back still have to read their events, and
    /// the library may still have to be migrated.
    pub fn recheck_library(&mut self, library_id: &str) {
        self.state.reconciling |= self.migration_due.contains(library_id)
            || self
                .state
                .tasks
                .iter()
                .any(|task| !task.events.loaded && task.plan.library_id == library_id);
    }

    /// What the last migration changed, once.
    pub fn take_migrated(&mut self) -> Option<MigrationOutcome> {
        self.migrated.take()
    }

    /// Applies the findings to the current records (which may have changed meanwhile)
    /// and allows dispatch again. When the record cannot be saved the findings still
    /// apply in memory, so the error is reported without holding every task back.
    pub fn apply_reconciliation(&mut self, reconciled: Reconciled) -> Result<()> {
        if reconciled.workspace != self.identity {
            return Ok(());
        }
        fn apply(
            state: &mut State,
            facts: Vec<(String, TaskFacts)>,
            migrations: &[crate::migrate::Migration],
        ) {
            for migration in migrations {
                crate::migrate::rekey_positions(&mut state.positions, &migration.notes);
            }
            let checked: BTreeSet<String> = facts.iter().map(|(id, _)| id.clone()).collect();
            for (id, facts) in facts {
                if let Some(task) = state.task_mut(&id)
                    && !matches!(task.state, TaskState::Running | TaskState::Pausing)
                {
                    facts.apply(task);
                }
            }
            state.reconciling = false;
            // A course flattened once its worker let go: the library's other tasks
            // were read before, and are read again for the note it is now.
            for migration in migrations.iter().filter(|m| m.flattened > 0) {
                for task in state.tasks.iter_mut().filter(|task| {
                    task.plan.library_id == migration.library_id
                        && !checked.contains(&task.id)
                        && !matches!(task.state, TaskState::Running | TaskState::Pausing)
                }) {
                    task.events.loaded = false;
                    state.reconciling = true;
                }
            }
        }
        self.held_checked = Some(std::time::Instant::now());
        self.checking = false;
        let mirror_failed: BTreeSet<String> = reconciled
            .facts
            .iter()
            .filter(|(_, facts)| {
                matches!(
                    facts,
                    TaskFacts::Found {
                        mirror_failed: true,
                        ..
                    }
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
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
            // Told even when the older app saved only one version.
            let resaved: usize = migrations.iter().map(|migration| migration.resaved).sum();
            if resaved > 0 {
                notices.push(crate::migrate::resaved_notice(resaved));
            }
            if !notices.is_empty() {
                outcome.notice = Some(notices.join(" "));
            }
        }
        // Nothing to record: a library migrated with no tasks and no positions to move.
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
        let mut facts = Some(reconciled.facts);
        let result = self.transaction(|state| {
            apply(state, facts.take().unwrap_or_default(), &migrations);
            Ok(())
        });
        if let Some(facts) = facts.take().filter(|_| result.is_err()) {
            apply(&mut self.state, facts, &migrations);
        }
        self.queue_mirrors(
            self.state
                .tasks
                .iter()
                .filter(|task| mirror_failed.contains(&task.id)),
        );
        result
    }

    /// `open_at` for tests, with the startup checks applied.
    ///
    /// The app opens its record once per run, and an earlier run's library writes have
    /// finished or died with it. A test reopens a record in the same process, where an
    /// earlier opening's writes may still be on the blocking pool: they would race the
    /// startup checks for the same task mirrors, which Windows refuses to replace while
    /// another writer has them open. So each opening first waits for those of the
    /// openings before it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_at_inline(path: PathBuf, root: PathBuf) -> Result<Self> {
        static OPENED: Mutex<Vec<(PathBuf, LibraryWrites)>> = Mutex::new(Vec::new());
        let earlier: Vec<_> = c2m_process::runtime::lock_recover(&OPENED)
            .iter()
            .filter(|(opened, _)| *opened == path)
            .map(|(_, writes)| writes.clone())
            .collect();
        for writes in earlier {
            writes.drain();
        }
        let mut workspace = Self::open_at(path.clone(), root)?;
        c2m_process::runtime::lock_recover(&OPENED).push((path, workspace.library_writes()));
        workspace.reconcile_now();
        Ok(workspace)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn reconcile_now(&mut self) {
        if let Some(reconciliation) = self.reconciliation() {
            self.apply_reconciliation(reconciliation.run()).unwrap();
        }
    }

    fn read(path: &Path) -> Result<State> {
        let state: State = serde_json::from_slice(
            &desktop_prefs::store::read(path)
                .with_context(|| c2m_i18n::tr!("读取 {}", path.display()))?,
        )?;
        ensure!(
            state.schema == SCHEMA,
            c2m_i18n::tr!("任务记录版本不受支持，请使用创建这些任务的应用版本")
        );
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
    /// Edits a copy of the state and commits it: when this returns the record is on
    /// disk, or nothing changed. Library copies follow off the UI thread.
    pub fn transaction<T>(&mut self, edit: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let mut next = self.state.clone();
        let result = edit(&mut next)?;
        compact_history(&mut next);
        next.revision = next.revision.wrapping_add(1);
        write_record(&self.path, &self.state, &next)?;
        let previous = std::mem::replace(&mut self.state, next);
        self.queue_mirrors(self.state.tasks.iter().filter(|task| {
            previous.task(&task.id) != Some(task)
                || previous.library(&task.plan.library_id)
                    != self.state.library(&task.plan.library_id)
        }));
        Ok(result)
    }

    /// Saves the state as it stands, for direct edits such as a job's own events.
    pub fn commit(&self) -> Result<()> {
        desktop_prefs::store::save(&self.path, &serde_json::to_vec(&self.state)?)?;
        self.queue_mirrors(self.state.tasks.iter());
        Ok(())
    }

    /// Commits and waits for the library copies, when the app is about to close or
    /// someone asked to save again.
    pub fn save(&self) -> Result<()> {
        self.commit()?;
        self.library_writes.drain();
        match self.take_write_error() {
            Some(error) => Err(anyhow::anyhow!(error.message)),
            None => Ok(()),
        }
    }

    /// A task that has not read its events back may still have an old mirror to read
    /// them from, so its mirror waits for that.
    fn queue_mirrors<'a>(&self, tasks: impl Iterator<Item = &'a TaskRecord>) {
        for task in tasks.filter(|task| task.events.loaded) {
            if let Some(location) = self.state.library(&task.plan.library_id)
                && let Ok(bytes) = mirror_bytes(task, location)
            {
                self.library_writes.queue(
                    task.work_dir.join("task-record.json"),
                    bytes,
                    Some(location.clone()),
                );
            }
        }
    }

    /// Queues a task's control file; failures come back through `take_write_error`.
    pub fn write_control(&self, task: &TaskRecord, bytes: Vec<u8>) {
        self.library_writes
            .queue(task.work_dir.join("control.json"), bytes, None);
    }

    pub fn library_writes(&self) -> LibraryWrites {
        self.library_writes.clone()
    }

    /// A library write that failed since the last call. The queue wakes the event loop
    /// (see `set_wake_sender`) when one occurs.
    pub fn take_write_error(&self) -> Option<WriteFailure> {
        c2m_process::runtime::lock_recover(&self.library_writes.failure).take()
    }
    pub fn set_wake_sender(&self, wake: smol::channel::Sender<()>) {
        *c2m_process::runtime::lock_recover(&self.library_writes.wake) = Some(wake);
    }

    pub fn storage_path(&self) -> &Path {
        &self.path
    }
    /// File work for adding a library, safe to run on a blocking thread.
    /// The commit half only edits in-memory state and never touches the filesystem.
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
                    // The root as saved, which may predate canonical roots:
                    // registering then matches it without touching the disk.
                    root: existing.clone(),
                    name,
                    id: String::new(),
                    duplicate: true,
                    make_default,
                    tasks: Vec::new(),
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
        // Tasks travel with their library. A task the folder cannot show is named where
        // the record is rebuilt; adding a library does not fail over it.
        let location = LibraryLocation {
            id: id.clone(),
            name: name.clone(),
            root: canonical.clone(),
            previous_roots: Vec::new(),
        };
        let (tasks, _) = read_library_tasks(&location);
        Ok(PreparedLibrary {
            root: canonical,
            name,
            id,
            duplicate: false,
            make_default,
            tasks,
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
        let tasks = prepared.tasks.clone();
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
            state.adopt_tasks(tasks);
            // A library added is migrated like one opened.
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

/// Apply a preview result the way the import page does, for tests.
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
    use c2m_engine::execution::Component;

    #[test]
    fn the_default_library_name_reads_in_the_interface_language() {
        use c2m_i18n::i18n::Lang;
        // Saved under Chinese, English or Japanese, the built-in name follows the app.
        for saved in ["笔记库", "Library", "ノートライブラリ"] {
            assert_eq!(super::label_in(saved, Lang::En), "Library", "{saved}");
            assert_eq!(
                super::label_in(saved, Lang::Ja),
                "ノートライブラリ",
                "{saved}"
            );
            assert_eq!(super::label_in(saved, Lang::ZhHans), "笔记库", "{saved}");
            assert_eq!(super::label_in(saved, Lang::ZhHant), "筆記庫", "{saved}");
        }
        // A name the person chose stays theirs.
        assert_eq!(super::label_in("课程资料", Lang::En), "课程资料");
    }
    fn source(input: &str, title: &str) -> Source {
        serde_json::from_value(serde_json::json!({"input":input,"title":title,"author":"","duration":60.0,"cover":null,"cover_error":null})).unwrap()
    }
    fn plan(library: &str) -> TaskPlan {
        TaskPlan {
            operation: Default::default(),
            source: source("lecture.mp4", "课程 A"),
            source_id: "video-a".into(),
            title: "课程 A".into(),
            library_id: library.into(),
            folder: None,
            subtitle: None,
            text_source: None,
            repair: None,
        }
    }
    #[test]
    fn moving_a_queued_task_changes_which_runs_next() {
        let mut state = State::initial(PathBuf::from("/library"));
        let library = state.default_library.clone();
        for title in ["a", "b", "c", "d"] {
            let mut plan = plan(&library);
            plan.source_id = title.into();
            plan.title = title.into();
            state.enqueue(plan, None).unwrap();
        }
        let order = |state: &State| -> Vec<String> {
            state
                .queued_tasks()
                .map(|task| task.plan.title.clone())
                .collect()
        };
        let id = |state: &State, title: &str| {
            state
                .tasks
                .iter()
                .find(|task| task.plan.title == title)
                .unwrap()
                .id
                .clone()
        };
        let d = id(&state, "d");
        state.move_waiting_task(&d, 0).unwrap();
        assert_eq!(order(&state), ["d", "a", "b", "c"]);
        assert_eq!(state.next_task().unwrap().plan.title, "d");
        let d = id(&state, "d");
        state.move_waiting_task(&d, 2).unwrap();
        assert_eq!(order(&state), ["a", "b", "d", "c"]);
        let a = id(&state, "a");
        state.move_waiting_task(&a, 99).unwrap();
        assert_eq!(order(&state), ["b", "d", "c", "a"]);
        // A paused task keeps its place and moves with the queue.
        let b = id(&state, "b");
        state.set_intent(&b, Intent::Pause).unwrap();
        state.move_waiting_task(&b, 3).unwrap();
        assert_eq!(order(&state), ["d", "c", "a"]);
        assert_eq!(state.waiting_tasks().last().unwrap().plan.title, "b");
        state.set_intent(&b, Intent::Cancel).unwrap();
        assert!(state.move_waiting_task(&b, 0).is_err());
    }

    #[test]
    fn failed_commit_does_not_publish_an_unpersisted_task() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state");
        let mut ws = Workspace::open_at_inline(target.clone(), dir.path().join("library")).unwrap();
        std::fs::create_dir(&target).unwrap();
        let task = plan(&ws.state.default_library);
        assert!(ws.transaction(|state| state.enqueue(task, None)).is_err());
        assert!(ws.state.tasks.is_empty());
    }
    #[test]
    fn frozen_task_survives_input_changes_without_storing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let mut ws = Workspace::open_at_inline(file.clone(), dir.path().join("library")).unwrap();
        let task = plan(&ws.state.default_library);
        let mut config = ConfigFile::default();
        config.asr_api.api_key = "asr-secret-do-not-store".into();
        config.llm.api_key = "ai-secret-do-not-store".into();
        let id = ws.transaction(|state| state.enqueue(task, None)).unwrap().0;
        ws.transaction(|s| {
            s.draft_mut().unwrap().change_source("B.mp4".into());
            let task = s.task_mut(&id).unwrap();
            task.state = TaskState::Running;
            // Even a run record handed the resolved keys does not write them.
            task.ran_with = Some(RunSettings {
                config,
                asr_service: None,
                ai_service: None,
            });
            Ok(())
        })
        .unwrap();
        let restored = Workspace::open_at_inline(file.clone(), dir.path().into()).unwrap();
        assert_eq!(
            restored.state.task(&id).unwrap().plan.source.input,
            "lecture.mp4"
        );
        assert_eq!(restored.state.task(&id).unwrap().state, TaskState::Queued);
        assert_eq!(restored.state.draft().unwrap().input, "B.mp4");
        for path in [file.clone(), desktop_prefs::store::backup_path(&file)] {
            assert!(
                !std::fs::read_to_string(path)
                    .unwrap()
                    .contains("secret-do-not-store")
            );
        }
    }

    #[test]
    fn input_only_save_does_not_touch_an_unchanged_task_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let task = plan(&ws.state.default_library);
        let id = ws.transaction(|state| state.enqueue(task, None)).unwrap().0;
        ws.library_writes.drain();
        let mirror = ws
            .state
            .task(&id)
            .unwrap()
            .work_dir
            .join("task-record.json");
        std::fs::remove_file(&mirror).unwrap();
        std::fs::create_dir(&mirror).unwrap();
        ws.transaction(|state| {
            state
                .draft_mut()
                .unwrap()
                .change_source("next-video.mp4".into());
            Ok(())
        })
        .unwrap();
        ws.library_writes.drain();
        assert!(ws.take_write_error().is_none());
    }

    #[test]
    fn a_failed_mirror_keeps_the_commit_and_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.save().unwrap();
        let mirror = ws
            .state
            .task(&id)
            .unwrap()
            .work_dir
            .join("task-record.json");
        std::fs::remove_file(&mirror).unwrap();
        std::fs::create_dir(&mirror).unwrap();
        let (wake, woken) = smol::channel::bounded(1);
        ws.set_wake_sender(wake);
        ws.transaction(|state| state.set_intent(&id, Intent::Pause))
            .unwrap();
        ws.library_writes.drain();
        assert!(woken.try_recv().is_ok(), "a failure wakes the event loop");
        let failure = ws.take_write_error().unwrap();
        assert!(!failure.control);
        assert!(ws.take_write_error().is_none());
        let reopened = test_workspace(dir.path());
        assert_eq!(reopened.state.task(&id).unwrap().intent, Intent::Pause);
    }

    /// A library on a stuck disk must not reach the interface: a commit returns while
    /// the mirror behind it cannot be written.
    #[test]
    fn a_commit_does_not_wait_for_a_stuck_library() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let writes = ws.library_writes();
        let stuck = writes.writing.lock().unwrap();
        let task = plan(&ws.state.default_library);
        let id = ws.transaction(|state| state.enqueue(task, None)).unwrap().0;
        ws.transaction(|state| state.set_intent(&id, Intent::Pause))
            .unwrap();
        let mirror = ws
            .state
            .task(&id)
            .unwrap()
            .work_dir
            .join("task-record.json");
        assert!(!mirror.exists());
        let record = std::fs::read_to_string(ws.storage_path()).unwrap();
        assert!(record.contains(&id));
        drop(stuck);
        ws.library_writes.drain();
        let saved: TaskMirror = serde_json::from_slice(&std::fs::read(&mirror).unwrap()).unwrap();
        assert_eq!(saved.task.intent, Intent::Pause);
    }

    /// `cargo test commit_timing -- --ignored --nocapture` reports what a commit costs
    /// the interface with a large record.
    #[test]
    #[ignore]
    fn commit_timing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        for index in 0..500 {
            let mut task = plan(&library);
            task.source_id = format!("video-{index}");
            ws.state.enqueue(task, None).unwrap();
        }
        ws.save().unwrap();
        let mut times: Vec<_> = (0..50)
            .map(|index| {
                let start = std::time::Instant::now();
                ws.transaction(|state| {
                    state.draft_mut().unwrap().scroll = index as f32;
                    Ok(())
                })
                .unwrap();
                start.elapsed()
            })
            .collect();
        times.sort();
        println!(
            "record {} KB, commit median {:?}, worst {:?}",
            std::fs::metadata(ws.storage_path()).unwrap().len() / 1024,
            times[25],
            times[49]
        );
    }

    #[test]
    fn a_failed_control_write_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.save().unwrap();
        let task = ws.state.task(&id).unwrap().clone();
        std::fs::create_dir(task.work_dir.join("control.json")).unwrap();
        ws.write_control(&task, b"{}".to_vec());
        ws.library_writes.drain();
        assert!(ws.take_write_error().unwrap().control);
    }

    #[test]
    fn a_running_task_s_progress_counts_without_changing_the_revision() {
        use c2m_protocol::progress::{Event, Stage, StageStatus};
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        let (revision, progress) = (ws.state.revision, ws.state.progress);
        let start = Event::Stage {
            stage: Stage::ScenesScan,
            status: StageStatus::Start,
        };
        assert!(ws.state.apply_task_event(&id, &start));
        let counted = Event::Progress {
            stage: Stage::ScenesScan,
            current: 215,
            total: 1800,
            message: None,
            phase: None,
            unit: None,
            found: None,
        };
        ws.state.apply_task_event(&id, &counted);
        assert_eq!(ws.state.revision, revision);
        assert_eq!(ws.state.progress, progress + 2);
        // A task the record no longer holds changes nothing.
        assert!(!ws.state.apply_task_event("gone", &counted));
        assert_eq!(ws.state.progress, progress + 2);
    }

    #[test]
    fn a_commit_holds_only_what_the_record_owns_and_events_come_back_from_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let task = plan(&ws.state.default_library);
        let id = ws.transaction(|state| state.enqueue(task, None)).unwrap().0;
        let work = ws.state.task(&id).unwrap().work_dir.clone();
        std::fs::create_dir_all(&work).unwrap();
        let event = c2m_protocol::progress::Event::Log {
            level: c2m_protocol::progress::LogLevel::Info,
            message: "实际进度".into(),
            fields: Default::default(),
        };
        std::fs::write(
            work.join(TASK_EVENTS_FILE),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();
        ws.state.task_mut(&id).unwrap().apply_event(&event);
        ws.save().unwrap();
        let record = std::fs::read_to_string(ws.storage_path()).unwrap();
        assert!(!record.contains("实际进度"));
        let mirror = work.join("task-record.json");
        assert!(
            !std::fs::read_to_string(&mirror)
                .unwrap()
                .contains("实际进度")
        );
        std::fs::remove_file(&mirror).unwrap();
        let reopened = test_workspace(dir.path());
        assert_eq!(reopened.state.task(&id).unwrap().events.logs, ["实际进度"]);
        // Reading a library back also repairs a missing mirror.
        assert!(mirror.is_file());
    }

    #[test]
    fn a_record_from_before_3_0_is_left_as_it_is_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let ws = test_workspace(dir.path());
        let path = ws.storage_path().to_owned();
        let mut old = serde_json::to_value(&ws.state).unwrap();
        old["schema"] = serde_json::json!(1);
        let bytes = serde_json::to_vec(&old).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let backup = desktop_prefs::store::backup_path(&path);
        let _ = std::fs::remove_file(&backup);
        let root = ws.state.libraries[0].root.clone();
        for _ in 0..2 {
            let error = Workspace::open_at_inline(path.clone(), root.clone())
                .err()
                .expect("an old record is not opened");
            let message = format!("{error:#}");
            assert!(message.contains(&path.display().to_string()), "{message}");
            assert!(message.contains("3.0"), "{message}");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "the record is unchanged"
            );
            assert!(!backup.exists(), "nothing replaces it, not even a backup");
        }
        // Rebuilding, which a person chooses, archives it before writing a new record. Of
        // the old record it reads only where the libraries are: one that names itself
        // comes back, and one that does not is for the person to add again.
        let marked = dir.path().join("marked");
        let unmarked = dir.path().join("unmarked");
        std::fs::create_dir_all(&marked).unwrap();
        std::fs::create_dir_all(&unmarked).unwrap();
        std::fs::write(marked.join(".course2md-library-id"), "library-two").unwrap();
        old["libraries"] = serde_json::json!([
            {"id": "ignored-id", "name": "Ignored", "root": marked, "extra": 1},
            {"id": "library-three", "name": "Three", "root": unmarked},
            {"id": "library-gone", "name": "Gone", "root": dir.path().join("gone")},
        ]);
        let bytes = serde_json::to_vec(&old).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let rebuilt = Workspace::rebuild_at(path.clone(), root).unwrap();
        assert_eq!(rebuilt.state.schema, SCHEMA);
        let restored = rebuilt.state.library("library-two").unwrap();
        assert_eq!(
            (restored.root.as_path(), restored.name.as_str()),
            (marked.as_path(), "marked")
        );
        assert!(check_library(restored).is_ok());
        assert!(rebuilt.state.libraries.iter().all(|l| l.root != unmarked));
        let message = rebuilt.recovery.clone().unwrap();
        assert!(
            message.contains(&unmarked.display().to_string()),
            "{message}"
        );
        assert!(
            message.contains(&dir.path().join("gone").display().to_string()),
            "{message}"
        );
        assert!(!unmarked.join(".course2md-library-id").exists());
        let archived = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("workspace-recovery")
            })
            .map(|entry| std::fs::read(entry.path().join("workspace.json")).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(archived, [bytes]);
    }

    #[test]
    fn a_task_mirror_from_before_event_streams_is_not_read_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::NeedsAttention;
        ws.save().unwrap();
        ws.library_writes().drain();
        let location = ws.state.libraries[0].clone();
        let work = ws.state.task(&id).unwrap().work_dir.clone();
        let mut task = serde_json::to_value(ws.state.task(&id).unwrap()).unwrap();
        task["logs"] = serde_json::json!(["一行日志"]);
        let old = serde_json::to_vec(&serde_json::json!({"schema": 1, "task": task})).unwrap();
        std::fs::write(work.join("task-record.json"), &old).unwrap();

        // A library read back for its tasks names it, and leaves it.
        let (tasks, issues) = read_library_tasks(&location);
        assert!(tasks.is_empty());
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].contains(&id) && issues[0].contains("3.0"),
            "{issues:?}"
        );
        assert_eq!(std::fs::read(work.join("task-record.json")).unwrap(), old);

        // Opening the record moves it aside unchanged, reads none of it, and says where.
        let reopened = test_workspace(dir.path());
        reopened.library_writes().drain();
        let logs = &reopened.state.task(&id).unwrap().events.logs;
        assert!(!logs.iter().any(|line| line == "一行日志"), "{logs:?}");
        assert!(
            logs.iter()
                .any(|line| line.contains(&work.join(OLD_MIRROR_FILE).display().to_string())),
            "{logs:?}"
        );
        assert_eq!(std::fs::read(work.join(OLD_MIRROR_FILE)).unwrap(), old);
        let mirror = std::fs::read(work.join("task-record.json")).unwrap();
        assert!(!older_record(&mirror));
    }

    #[test]
    fn tasks_and_folders_travel_with_a_library_to_another_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let root = ws.state.libraries[0].root.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        let paused = {
            let mut other = plan(&library);
            other.source_id = "video-b".into();
            ws.state.enqueue(other, None).unwrap().0
        };
        ws.state.task_mut(&paused).unwrap().resend = vec!["attempt.1".into()];
        let version = publish_note(&ws.state, &id, false);
        let task = ws.state.task_mut(&id).unwrap();
        task.artifact = Some(version.clone());
        task.state = TaskState::Complete;
        let folder =
            crate::organize::Library::edit(&root, |library| library.create(None, "课程").map(drop))
                .unwrap()
                .tree()[0]
                .0;
        crate::organize::Library::edit(&root, |library| {
            library.assign(&root, &version, Some(folder))
        })
        .unwrap();
        ws.save().unwrap();

        // Another machine: same folder, somewhere else, and a record of its own.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let moved = elsewhere.join("carried").clone();
        std::fs::rename(&root, &moved).unwrap();
        let moved = moved.canonicalize().unwrap();
        let mut other = Workspace::open_at_inline(
            elsewhere.join("workspace.json"),
            elsewhere.join("own-library"),
        )
        .unwrap();
        other
            .register_library(moved.clone(), "Carried".into(), false)
            .unwrap();
        other.reconcile_now();
        assert_eq!(other.state.library(&library).unwrap().root, moved);
        let finished = other.state.task(&id).unwrap();
        assert_eq!(finished.state, TaskState::Complete);
        assert_eq!(finished.work_dir, moved.join(".course2md/work").join(&id));
        let carried = finished.artifact.clone().unwrap();
        assert!(carried.starts_with(&moved) && carried.join("manifest.json").is_file());
        assert!(finished.events.outcomes.is_some());
        // Unfinished work comes along stopped, with no permission to send anything.
        let waiting = other.state.task(&paused).unwrap();
        assert_eq!(waiting.state, TaskState::Paused);
        assert!(waiting.resend.is_empty());
        assert!(other.state.next_task().is_none());
        let folders = crate::organize::Library::load(&moved).unwrap();
        assert_eq!(folders.folders[&folder], "课程");
        assert_eq!(folders.folder(&moved, &carried), Some(folder));
    }

    #[test]
    fn subtitle_text_and_finished_logs_stay_out_of_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let events = vec![c2m_core::timeline::TranscriptEvent {
            start: 0.,
            end: 1.,
            text: "字幕正文".into(),
            raw: None,
            translation: None,
        }];
        let mut task = plan(&ws.state.default_library);
        task.source.identity = "video-a".into();
        task.source.selected_subtitle = Some(c2m_source::subtitle::CachedSubtitle {
            source_identity: "video-a".into(),
            track_id: "zh".into(),
            label: "中文".into(),
            path: "lesson.srt".into(),
            events: events.clone(),
        });
        // A record from an earlier version carries the events inline.
        ws.state.enqueue(task.clone(), None).unwrap();
        ws.state.draft_mut().unwrap().source = Some(task.source.clone());
        let id = ws.state.tasks[0].id.clone();
        ws.state.tasks[0].state = TaskState::Complete;
        let mut legacy = serde_json::to_value(&ws.state).unwrap();
        legacy["drafts"][0]["source"]["selected_subtitle"]["events"] =
            serde_json::to_value(&events).unwrap();
        std::fs::write(ws.storage_path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
        let legacy = std::fs::read_to_string(ws.storage_path()).unwrap();
        assert!(legacy.contains("字幕正文"));

        let mut reopened = test_workspace(dir.path());
        assert_eq!(
            task_subtitle_events(reopened.state.task(&id).unwrap()).unwrap(),
            Some(events.clone())
        );
        reopened.transaction(|_| Ok(())).unwrap();
        let saved = std::fs::read_to_string(reopened.storage_path()).unwrap();
        assert!(!saved.contains("字幕正文"));

        let again = test_workspace(dir.path());
        let draft_subtitle = again
            .state
            .draft()
            .and_then(|draft| draft.source.as_ref())
            .and_then(|source| source.selected_subtitle.as_ref())
            .unwrap();
        assert_eq!(draft_subtitle.events, events);
        assert_eq!(
            task_subtitle_events(again.state.task(&id).unwrap()).unwrap(),
            Some(events)
        );
    }

    #[test]
    fn only_recent_cancelled_attempts_without_notes_are_kept() {
        let mut state = State::initial("/library".into());
        let library = state.default_library.clone();
        for n in 0..KEPT_CANCELLED_TASKS + 5 {
            let mut task = plan(&library);
            task.source_id = format!("video-{n}");
            let (id, _) = state.enqueue(task, None).unwrap();
            let record = state.task_mut(&id).unwrap();
            record.state = TaskState::Cancelled;
            record.updated = n as u64;
        }
        let selected = state.tasks[0].id.clone();
        state.selected_task = Some(selected.clone());
        compact_history(&mut state);
        assert_eq!(state.tasks.len(), KEPT_CANCELLED_TASKS + 1);
        assert!(state.task(&selected).is_some());
        assert!(state.task(&state.tasks[1].id.clone()).unwrap().updated >= 5);
    }

    #[test]
    fn source_revisions_decide_which_preview_is_accepted() {
        let mut draft = Draft::new(true);
        draft.change_source("A".into());
        let old = draft.revision;
        draft.change_source("B".into());
        assert!(!accept_source(&mut draft, old, source("A", "title-a")));
        let revision = draft.revision;
        assert!(accept_source(&mut draft, revision, source("B", "title-b")));
        // A new link forgets the previous video's text choice.
        draft.text_source = Some(c2m_config::config::TranscriptSource::Asr);
        draft.change_source("C".into());
        assert_eq!(draft.text_source, None);
    }

    #[test]
    fn a_second_video_starts_a_fresh_input_without_touching_the_submitted_task() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let submitted = plan(&ws.state.default_library);
        let task = ws.state.enqueue(submitted.clone(), None).unwrap().0;
        let input = ws.state.draft_mut().unwrap();
        input.input = "first.mp4".into();
        input.text_source = Some(c2m_config::config::TranscriptSource::Asr);
        input.submitted_task = Some(task.clone());
        let first_id = input.id.clone();
        assert!(!ws.state.prepare_next_import("first.mp4"));
        assert_eq!(ws.state.draft().unwrap().id, first_id);
        assert!(ws.state.prepare_next_import("second.mp4"));
        let input = ws.state.draft().unwrap();
        assert_ne!(input.id, first_id);
        assert_eq!(input.input, "second.mp4");
        assert_eq!(input.text_source, None);
        assert!(ws.state.task(&task).unwrap().plan == submitted);
        // An unsubmitted input is edited in place.
        assert!(!ws.state.prepare_next_import("revised-second.mp4"));
    }

    #[test]
    fn replacing_the_form_and_switching_source_kind_start_a_fresh_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let submitted = plan(&ws.state.default_library);
        let id = ws.state.enqueue(submitted.clone(), None).unwrap().0;
        let original_input = ws.state.current_draft.clone();
        let input = ws.state.draft_mut().unwrap();
        input.change_source("old-video.mp4".into());
        input.subtitle = Some("old.srt".into());
        input.submitted_task = Some(id.clone());
        let online = input.online;
        for _ in 0..4 {
            ws.state.reset_input(true);
            let input = ws.state.draft().unwrap();
            assert_ne!(input.id, original_input);
            assert_eq!(ws.state.drafts.len(), 1);
            assert!(input.input.is_empty() && input.source.is_none() && input.subtitle.is_none());
            assert!(input.submitted_task.is_none() && input.retry_of.is_none());
            assert!(ws.state.task(&id).unwrap().plan == submitted);
        }
        let before = ws.state.draft().unwrap().clone();
        ws.state.switch_source_kind(online);
        assert_eq!(ws.state.draft().unwrap(), &before);
        ws.state.switch_source_kind(!online);
        assert_ne!(ws.state.draft().unwrap().id, before.id);
        assert_eq!(ws.state.draft().unwrap().online, !online);
        ws.transaction(|_| Ok(())).unwrap();
        let restored = test_workspace(dir.path());
        assert_eq!(restored.state.drafts.len(), 1);
        assert_eq!(restored.state.draft().unwrap(), ws.state.draft().unwrap());
        assert!(restored.state.task(&id).unwrap().plan == submitted);
    }

    #[test]
    fn adjusting_a_failed_task_puts_its_video_back_and_preserves_the_task_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let mut original = plan(&ws.state.default_library);
        original.title = "A 的名称".into();
        original.subtitle = Some(dir.path().join("french.srt"));
        original.text_source = Some(c2m_config::config::TranscriptSource::Subtitle);
        let id = ws.state.enqueue(original.clone(), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::Running;
        ws.state.reset_input(true);
        let b = ws.state.draft_mut().unwrap();
        b.change_source("online-B".into());
        let b = b.clone();
        assert!(ws.state.adjust_task(&id, None).is_err());
        assert_eq!(ws.state.draft().unwrap(), &b);

        ws.state.task_mut(&id).unwrap().state = TaskState::NeedsAttention;
        ws.state.set_intent(&id, Intent::Run).unwrap();
        assert!(ws.state.next_task().unwrap().plan == original);
        ws.state.set_intent(&id, Intent::Pause).unwrap();
        ws.transaction(|state| state.adjust_task(&id, None))
            .unwrap();
        let revised = ws.state.draft().unwrap();
        assert_eq!(revised.input, original.source.input);
        assert_eq!(revised.source.as_ref(), Some(&original.source));
        assert_eq!(revised.subtitle, original.subtitle);
        assert_eq!(revised.text_source, original.text_source);
        assert_eq!(revised.retry_of.as_deref(), Some(id.as_str()));
        let revision = revised.clone();
        drop(ws);
        let mut reopened = test_workspace(dir.path());
        assert_eq!(reopened.state.draft().unwrap(), &revision);
        assert!(reopened.state.task(&id).unwrap().plan == original);
        assert_eq!(reopened.state.task(&id).unwrap().state, TaskState::Paused);
        let (followup, created) = reopened
            .state
            .enqueue(original.clone(), Some(id.clone()))
            .unwrap();
        assert!(created);
        assert_eq!(
            reopened.state.enqueue(original, Some(id.clone())).unwrap(),
            (followup, false)
        );
        assert!(reopened.state.adjust_task(&id, None).is_err());
    }

    /// Records written before tasks ran with the current settings carry each
    /// input's and task's own options, services and a full configuration. They
    /// still open: the input drops them, a task that waits to start runs with the
    /// settings current then, and one that ran keeps what it ran with.
    #[test]
    fn a_record_with_per_task_options_and_plans_still_opens() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let library = dir.path().join("library");
        let mut ws = Workspace::open_at_inline(file.clone(), library.clone()).unwrap();
        let library_id = ws.state.default_library.clone();
        let queued = ws
            .transaction(|state| state.enqueue(plan(&library_id), None))
            .unwrap()
            .0;
        let mut finished = plan(&library_id);
        finished.source_id = "video-b".into();
        let finished = ws
            .transaction(|state| state.enqueue(finished, None))
            .unwrap()
            .0;
        ws.transaction(|state| {
            state.task_mut(&finished).unwrap().state = TaskState::Complete;
            Ok(())
        })
        .unwrap();
        drop(ws);
        let mut old_config = ConfigFile::default();
        old_config.defaults.asr_model = Some("old-model".into());
        old_config.defaults.keep_video = Some(true);
        old_config.llm.enabled = true;
        old_config.llm.model = "old-ai-model".into();
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        for task in legacy["tasks"].as_array_mut().unwrap() {
            let plan = task["plan"].as_object_mut().unwrap();
            plan.insert("config".into(), serde_json::to_value(&old_config).unwrap());
            plan.insert("asr_service".into(), serde_json::Value::Null);
            plan.insert("ai_service".into(), "ai-version-old".into());
            plan.insert("options".into(), serde_json::json!({"llm": true}));
        }
        let draft = &mut legacy["drafts"][0];
        for (key, value) in [
            ("title", serde_json::json!("custom title")),
            ("custom_title", serde_json::json!(true)),
            ("library_id", serde_json::json!(library_id)),
            ("folder", serde_json::json!(7)),
            (
                "options",
                serde_json::json!({"provider": "", "source_mode": 2, "llm": true, "keep_video": false, "formats": [true, true, false]}),
            ),
            ("overrides", serde_json::json!(["proofread", "text_source"])),
            ("asr_service", serde_json::json!("asr-version-old")),
            ("ai_service", serde_json::json!("ai-version-old")),
            ("base_config", serde_json::to_value(&old_config).unwrap()),
            ("local_provider", serde_json::json!("cpu")),
        ] {
            draft[key] = value;
        }
        std::fs::write(&file, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let _ = std::fs::remove_file(desktop_prefs::store::backup_path(&file));

        let reopened = Workspace::open_at_inline(file.clone(), library).unwrap();
        let draft = reopened.state.draft().unwrap();
        assert_eq!(draft.text_source, None);
        let waiting = reopened.state.task(&queued).unwrap();
        assert!(
            waiting.ran_with.is_none(),
            "a waiting task runs with the current settings"
        );
        let ran = reopened.state.task(&finished).unwrap();
        let ran_with = ran.ran_with.as_ref().unwrap();
        assert_eq!(
            ran_with.config.defaults.asr_model.as_deref(),
            Some("old-model")
        );
        assert_eq!(ran_with.ai_service.as_deref(), Some("ai-version-old"));
        assert!(ran_with.options().keep_video);
        // Written again, the record carries none of the old per-task fields.
        let mut reopened = reopened;
        reopened.transaction(|_| Ok(())).unwrap();
        reopened.save().unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        for task in written["tasks"].as_array().unwrap() {
            for key in ["config", "asr_service", "ai_service", "options"] {
                assert!(task["plan"].get(key).is_none(), "{key}: {task}");
            }
        }
        for key in [
            "options",
            "overrides",
            "base_config",
            "title",
            "folder",
            "library_id",
        ] {
            assert!(written["drafts"][0].get(key).is_none(), "{key}");
        }
    }

    /// An older repair kept the note's translation and the exports it redoes in
    /// its configuration; they are what it repairs, whatever Settings say now.
    #[test]
    fn an_older_repair_keeps_its_translation_and_exports() {
        let mut config = ConfigFile::default();
        config.llm.translate = Some("en".into());
        config.defaults.formats = Some(vec![c2m_config::config::OutputFormat::Json]);
        let record = serde_json::json!({
            "id": "task-repair",
            "plan": {
                "operation": {
                    "kind": "reprocess",
                    "note_dir": "/library/course-x",
                    "base_task_id": "task-base",
                    "components": ["translation"],
                },
                "source": source("lecture.mp4", "课程"),
                "source_id": "video-a",
                "title": "课程",
                "library_id": "library",
                "folder": null,
                "subtitle": null,
                "config": config,
                "asr_service": null,
                "ai_service": "ai-version-old",
            },
            "state": "queued",
            "intent": "run",
            "created": 1,
            "updated": 1,
            "parent": "task-base",
        });
        let task: TaskRecord = serde_json::from_value(record).unwrap();
        let repair = task.plan.repair.unwrap();
        assert_eq!(repair.translate.as_deref(), Some("en"));
        assert_eq!(
            repair.formats,
            Some(vec![c2m_config::config::OutputFormat::Json])
        );
        assert!(task.ran_with.is_none());
    }

    #[test]
    fn delayed_results_cannot_cross_new_note_or_mode_changes_even_with_the_same_revision() {
        let mut state = State::initial("/tmp/library".into());
        state
            .draft_mut()
            .unwrap()
            .change_source("same-input".into());
        let pending = state.draft().unwrap().clone();
        assert!(state.matches_input(&pending.id, pending.revision));
        state.reset_input(true);
        state
            .draft_mut()
            .unwrap()
            .change_source("same-input".into());
        assert_eq!(state.draft().unwrap().revision, pending.revision);
        assert!(!state.matches_input(&pending.id, pending.revision));
        let after_new = state.draft().unwrap().clone();
        state.switch_source_kind(false);
        state
            .draft_mut()
            .unwrap()
            .change_source("same-input".into());
        assert_eq!(state.draft().unwrap().revision, after_new.revision);
        assert!(!state.matches_input(&after_new.id, after_new.revision));
        let current = state.draft().unwrap().clone();
        assert!(state.matches_input(&current.id, current.revision));
        assert!(accept_source(
            state.draft_mut().unwrap(),
            current.revision,
            source("same-input", "当前读取结果"),
        ));
        assert_eq!(
            state.draft().unwrap().source.as_ref().unwrap().title,
            "当前读取结果"
        );
        assert_eq!(state.drafts.len(), 1);
    }

    #[test]
    fn explicit_stop_survives_restart_while_duplicate_names_do_not_create_work() {
        let mut state = State::initial("/tmp/library".into());
        let mut task = plan(&state.default_library);
        let id = state.enqueue(task.clone(), None).unwrap().0;
        task.title = "另一个显示名".into();
        task.folder = Some(42);
        assert_eq!(state.enqueue(task, None).unwrap(), (id.clone(), false));
        state.task_mut(&id).unwrap().state = TaskState::Running;
        state.stop_session();
        state.recover();
        assert_eq!(state.task(&id).unwrap().state, TaskState::Paused);
        assert!(state.next_task().is_none());
    }

    #[test]
    fn cold_start_resumes_only_unstopped_work_and_keeps_unknown_requests_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let mut ids = Vec::new();
        for (index, intent) in [
            Intent::Run,
            Intent::Pause,
            Intent::Quit,
            Intent::Cancel,
            Intent::Run,
            Intent::Run,
        ]
        .into_iter()
        .enumerate()
        {
            let mut plan = plan(&ws.state.default_library);
            plan.source_id = format!("source-{index}");
            plan.source.input = format!("video-{index}.mp4");
            let id = ws.state.enqueue(plan, None).unwrap().0;
            let task = ws.state.task_mut(&id).unwrap();
            task.state = TaskState::Running;
            task.intent = intent;
            if index == 4 {
                write_unknown(task, 1);
            }
            if index == 5 {
                task.state = TaskState::NeedsAttention;
                task.events.error = Some("保留这次失败原因".into());
                task.unread = true;
            }
            ids.push(id);
        }
        ws.transaction(|_| Ok(())).unwrap();
        let plans = ws
            .state
            .tasks
            .iter()
            .map(|task| task.plan.clone())
            .collect::<Vec<_>>();
        drop(ws);
        let mut reopened = test_workspace(dir.path());
        for ((id, expected), plan) in ids
            .iter()
            .zip([
                TaskState::Queued,
                TaskState::Paused,
                TaskState::Paused,
                TaskState::Cancelled,
                TaskState::Uncertain,
                TaskState::NeedsAttention,
            ])
            .zip(plans)
        {
            let task = reopened.state.task(id).unwrap();
            assert_eq!(task.state, expected);
            assert!(task.plan == plan);
        }
        assert_eq!(reopened.state.next_task().unwrap().id, ids[0]);
        let unknown = reopened.state.task(&ids[4]).unwrap();
        assert_eq!(unknown.events.blocked.len(), 1);
        assert!(unknown.unread && unknown.resend.is_empty());
        reopened
            .transaction(|state| {
                state.stop_session();
                Ok(())
            })
            .unwrap();
        drop(reopened);
        let stopped = test_workspace(dir.path());
        assert!(stopped.state.next_task().is_none());
        let unknown = stopped.state.task(&ids[4]).unwrap();
        assert_eq!(unknown.state, TaskState::Uncertain);
        assert_eq!(unknown.intent, Intent::Quit);
        assert!(unknown.unread && unknown.resend.is_empty());
        let failed = stopped.state.task(&ids[5]).unwrap();
        assert_eq!(failed.state, TaskState::NeedsAttention);
        assert_eq!(failed.intent, Intent::Quit);
        assert_eq!(failed.events.error.as_deref(), Some("保留这次失败原因"));
        assert!(failed.unread);
    }

    /// The startup check also brings the library copy of each record up to date. When
    /// that write fails, the receipts it read still stand: a stopped task with an
    /// unconfirmed paid request stays uncertain, and the copy is retried and reported
    /// like any other library write.
    #[test]
    fn a_failed_mirror_catch_up_keeps_an_uncertain_task_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let id = ws
            .state
            .enqueue(plan(&ws.state.default_library), None)
            .unwrap()
            .0;
        ws.transaction(|state| {
            let task = state.task_mut(&id).unwrap();
            task.state = TaskState::Uncertain;
            task.intent = Intent::Quit;
            write_unknown(task, 1);
            Ok(())
        })
        .unwrap();
        ws.library_writes.drain();
        let mirror = ws
            .state
            .task(&id)
            .unwrap()
            .work_dir
            .join("task-record.json");
        drop(ws);
        // Nothing can replace a directory, on any platform.
        std::fs::remove_file(&mirror).unwrap();
        std::fs::create_dir(&mirror).unwrap();

        let ws = test_workspace(dir.path());
        let task = ws.state.task(&id).unwrap();
        assert_eq!(task.state, TaskState::Uncertain);
        assert_eq!(task.intent, Intent::Quit);
        assert!(task.has_uncertain() && task.events.error.is_none());
        assert!(ws.state.next_task().is_none());
        ws.library_writes.drain();
        let failure = ws
            .take_write_error()
            .expect("the copy is retried and reported");
        assert!(!failure.control);

        std::fs::remove_dir(&mirror).unwrap();
        ws.save().unwrap();
        assert!(mirror.is_file());
        let task = test_workspace(dir.path()).state.task(&id).unwrap().clone();
        assert_eq!(task.state, TaskState::Uncertain);
        assert!(task.has_uncertain());
    }

    fn receipt(task: &TaskRecord, state: c2m_dispatch::dispatch::State) {
        let completed = state == c2m_dispatch::dispatch::State::Completed;
        let receipt = c2m_dispatch::dispatch::Receipt {
            schema: 1,
            stable_id: "stable-request".into(),
            request_id: "stable-request.1".into(),
            purpose: "proofreading".into(),
            description: "校对 00:00–00:20 的文字".into(),
            service_version: "version-a".into(),
            attempt: 1,
            state,
            http_status: completed.then_some(200),
            response: completed.then(|| serde_json::json!({"choices": []})),
            message: None,
            unsupported_response_format: false,
            refused_parameter: None,
            repeats: false,
            retry_authorized: None,
        };
        std::fs::create_dir_all(task.work_dir.join("requests")).unwrap();
        std::fs::write(
            task.work_dir.join("requests/stable-request.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }

    /// The app was killed during a paid request and its worker lived on to save the
    /// answer. Until it lets go, its Sending receipt is no question for the person;
    /// once it has, the task continues from the saved answer.
    #[test]
    fn a_worker_outliving_a_killed_app_is_waited_for_and_its_answer_used() {
        use c2m_dispatch::dispatch::State as Receipt;
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let id = ws
            .state
            .enqueue(plan(&ws.state.default_library), None)
            .unwrap()
            .0;
        ws.transaction(|state| {
            state.task_mut(&id).unwrap().state = TaskState::Running;
            Ok(())
        })
        .unwrap();
        let task = ws.state.task(&id).unwrap().clone();
        receipt(&task, Receipt::Sending);
        drop(ws);

        let worker = c2m_process::runtime::lock_file(&task.work_dir.join(".task.lock")).unwrap();
        let mut reopened = test_workspace(dir.path());
        let held = reopened.state.task(&id).unwrap();
        assert!(held.held && !held.has_uncertain(), "not judged yet");
        assert_ne!(held.state, TaskState::Uncertain);
        assert!(reopened.state.next_task().is_none(), "not run beside it");
        assert!(reopened.held_check_due(std::time::Instant::now()).is_some());
        reopened.held_checked = None;
        reopened.reconcile_now();
        assert!(reopened.state.task(&id).unwrap().held, "still held");

        receipt(&task, Receipt::Completed);
        drop(worker);
        reopened.held_checked = None;
        reopened.reconcile_now();
        let task = reopened.state.task(&id).unwrap();
        assert!(!task.held && !task.has_uncertain());
        assert_eq!(task.state, TaskState::Queued);
        assert_eq!(reopened.state.next_task().unwrap().id, id);
        assert!(reopened.held_check_due(std::time::Instant::now()).is_none());
    }

    /// The app's event loop sleeps until `held_check_due`. A check handed out to a
    /// blocking thread must not leave that instant in the past while it runs: the loop
    /// would wake at once, again and again, and never let the check's result in.
    #[test]
    fn a_held_check_in_flight_is_not_due_again_until_its_findings_are_applied() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let id = ws
            .state
            .enqueue(plan(&ws.state.default_library), None)
            .unwrap()
            .0;
        ws.transaction(|state| {
            state.task_mut(&id).unwrap().state = TaskState::Running;
            Ok(())
        })
        .unwrap();
        let task = ws.state.task(&id).unwrap().clone();
        std::fs::create_dir_all(&task.work_dir).unwrap();
        drop(ws);
        let worker = c2m_process::runtime::lock_file(&task.work_dir.join(".task.lock")).unwrap();
        let mut reopened = test_workspace(dir.path());
        assert!(reopened.state.task(&id).unwrap().held);

        let t0 = Instant::now();
        reopened.held_checked = Some(t0);
        assert_eq!(reopened.held_check_due(t0), Some(t0 + HELD_CHECK_INTERVAL));
        assert!(reopened.reconciliation_at(t0).is_none(), "not due yet");

        // Due: the check is handed out, and while it runs nothing else is due, however
        // long it takes.
        let later = t0 + HELD_CHECK_INTERVAL;
        let check = reopened.reconciliation_at(later).expect("due");
        for elapsed in [0, 1, 5, 60] {
            let now = later + Duration::from_secs(elapsed);
            assert_eq!(reopened.held_check_due(now), None);
            assert!(reopened.reconciliation_at(now).is_none());
        }

        // Its findings applied (the worker still holds the folder), the next check is a
        // second later, not in the past.
        let applied = Instant::now();
        reopened.apply_reconciliation(check.run()).unwrap();
        assert!(reopened.state.task(&id).unwrap().held);
        let due = reopened.held_check_due(applied).unwrap();
        assert!(due >= applied + HELD_CHECK_INTERVAL);

        // Once the worker lets go, the next check frees the task.
        drop(worker);
        let check = reopened.reconciliation_at(due).expect("due again");
        reopened.apply_reconciliation(check.run()).unwrap();
        assert!(!reopened.state.task(&id).unwrap().held);
        assert_eq!(reopened.held_check_due(due), None);
        assert_eq!(reopened.state.next_task().unwrap().id, id);
    }

    /// Adjusting and retrying an uncertain task unchanged starts a new attempt; the
    /// original it replaces is not a duplicate in the way.
    #[test]
    fn retrying_an_uncertain_task_unchanged_is_not_refused_as_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::Uncertain;
        // The workbench's notice asks the same question before the retry is started.
        assert!(ws.state.duplicate_of(&plan(&library), Some(&id)).is_none());
        assert_eq!(
            ws.state.duplicate_of(&plan(&library), None).map(|t| &t.id),
            Some(&id)
        );
        let (retry, created) = ws.state.enqueue(plan(&library), Some(id.clone())).unwrap();
        assert!(created);
        assert_ne!(retry, id);
        assert_eq!(
            ws.state.task(&id).unwrap().handled_by.as_deref(),
            Some(retry.as_str())
        );
        // Without a parent the same work is still not added twice.
        assert_eq!(
            ws.state.enqueue(plan(&library), None).unwrap(),
            (retry, false)
        );
    }

    /// A task judged uncertain whose request was settled afterwards does not stay stuck.
    #[test]
    fn an_uncertain_task_whose_requests_settled_goes_back_to_its_intent() {
        use c2m_dispatch::dispatch::State as Receipt;
        for (intent, expected) in [
            (Intent::Run, TaskState::Queued),
            (Intent::Quit, TaskState::Paused),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut ws = test_workspace(dir.path());
            let id = ws
                .state
                .enqueue(plan(&ws.state.default_library), None)
                .unwrap()
                .0;
            let task = ws.state.task_mut(&id).unwrap();
            task.state = TaskState::Uncertain;
            task.intent = intent;
            receipt(task, Receipt::Uncertain);
            reconcile_receipts(task).unwrap();
            assert!(task.has_uncertain());
            receipt(task, Receipt::Completed);
            reconcile_receipts(task).unwrap();
            assert!(!task.has_uncertain());
            assert_eq!(task.state, expected, "{intent:?}");
        }
    }

    #[test]
    fn re_adding_a_library_saved_under_a_non_canonical_root_finds_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("library")).unwrap();
        std::fs::create_dir_all(dir.path().join("other")).unwrap();
        // Saved as typed, with a `..` component: not the canonical spelling.
        let saved = dir.path().join("other").join("..").join("library");
        let mut ws =
            Workspace::open_at_inline(dir.path().join("workspace.json"), saved.clone()).unwrap();
        let id = ws.state.default_library.clone();
        assert_eq!(ws.state.library(&id).unwrap().root, saved);
        let roots: Vec<_> = ws.state.libraries.iter().map(|l| l.root.clone()).collect();
        let ids: Vec<_> = ws.state.libraries.iter().map(|l| l.id.clone()).collect();
        let prepared = Workspace::prepare_library(
            dir.path().join("library"),
            "Again".into(),
            false,
            &roots,
            &ids,
        )
        .unwrap();
        assert!(prepared.duplicate);
        assert_eq!(ws.register_prepared_library(&prepared).unwrap(), id);
    }

    /// A worker from an earlier session still holds its note: another task for the
    /// same note waits rather than start and stop at the note's claim.
    #[test]
    fn a_task_for_a_note_an_earlier_worker_holds_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        ws.state.reconciling = false;
        let library = ws.state.default_library.clone();
        let (first, _) = ws.state.enqueue(plan(&library), None).unwrap();
        let mut second = ws.state.task(&first).unwrap().clone();
        second.id = "second".into();
        ws.state.tasks.push(second);
        let mut other = ws.state.task(&first).unwrap().clone();
        other.id = "other-video".into();
        other.plan.source_id = "video-b".into();
        ws.state.tasks.push(other);
        let held = ws.state.task_mut(&first).unwrap();
        held.held = true;
        held.state = TaskState::Running;
        // Only the task for another note may start.
        assert_eq!(ws.state.next_task().unwrap().id, "other-video");
        ws.state.task_mut("other-video").unwrap().state = TaskState::Complete;
        assert!(ws.state.next_task().is_none());
        let released = ws.state.task_mut(&first).unwrap();
        released.held = false;
        released.state = TaskState::Complete;
        assert_eq!(ws.state.next_task().unwrap().id, "second");
    }

    fn test_workspace(dir: &Path) -> Workspace {
        Workspace::open_at_inline(dir.join("workspace.json"), dir.join("library")).unwrap()
    }

    fn publish_note(state: &State, task_id: &str, partial: bool) -> PathBuf {
        let mut outcomes = c2m_artifact::artifact::Outcomes {
            transcript: c2m_artifact::artifact::Outcome::succeeded(),
            ..Default::default()
        };
        if partial {
            outcomes.proofreading = c2m_artifact::artifact::Outcome::failed("校对未完成");
        }
        publish_note_with_outcomes(state, task_id, outcomes)
    }

    fn publish_note_with_outcomes(
        state: &State,
        task_id: &str,
        outcomes: c2m_artifact::artifact::Outcomes,
    ) -> PathBuf {
        publish_note_with_exports(state, task_id, outcomes, &[])
    }

    fn publish_note_with_exports(
        state: &State,
        task_id: &str,
        outcomes: c2m_artifact::artifact::Outcomes,
        formats: &[c2m_config::config::OutputFormat],
    ) -> PathBuf {
        use c2m_artifact::artifact;
        use c2m_core::timeline;
        let task = state.task(task_id).unwrap();
        let course_id = crate::workspace::course_dir_name(&task.plan.source_id);
        let target = artifact::Target {
            task_id: task.id.clone(),
            replaces: None,
            source_id: task.plan.source_id.clone(),
            course_id: course_id.clone(),
            course_dir: state
                .library(&task.plan.library_id)
                .unwrap()
                .root
                .join(course_id),
        };
        let meta = c2m_source::fetch::VideoMeta {
            title: task.plan.title.clone(),
            uploader: String::new(),
            duration: 20.,
            webpage_url: task.plan.source.input.clone(),
            extractor: String::new(),
            id: String::new(),
        };
        let sections = vec![timeline::Section {
            t: 0.,
            end: 20.,
            image: String::new(),
            speech: vec![timeline::TranscriptEvent {
                start: 0.,
                end: 20.,
                text: "Preserved readable notes.".into(),
                raw: None,
                translation: None,
            }],
        }];
        artifact::publish_blocking(
            &target,
            &task.work_dir,
            &meta,
            &sections,
            None,
            formats,
            outcomes,
        )
        .unwrap();
        target.course_dir
    }

    fn write_unknown(task: &TaskRecord, attempt: u32) -> String {
        let id = format!("stable-request.{attempt}");
        let receipt = c2m_dispatch::dispatch::Receipt {
            schema: 1,
            stable_id: "stable-request".into(),
            request_id: id.clone(),
            purpose: "proofreading".into(),
            description: "校对 00:00–00:20 的文字".into(),
            service_version: "version-a".into(),
            attempt,
            state: c2m_dispatch::dispatch::State::Uncertain,
            http_status: None,
            response: None,
            message: Some("连接断开 / connection lost".into()),
            unsupported_response_format: false,
            refused_parameter: None,
            repeats: false,
            retry_authorized: None,
        };
        std::fs::create_dir_all(task.work_dir.join("requests")).unwrap();
        std::fs::write(
            task.work_dir.join("requests/stable-request.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        id
    }

    /// Records that task `id`'s run used `edit` of the default settings.
    fn ran_with(ws: &mut Workspace, id: &str, edit: impl FnOnce(&mut ConfigFile)) {
        let mut config = ConfigFile::default();
        edit(&mut config);
        ws.state.task_mut(id).unwrap().ran_with = Some(RunSettings::new(&config, None, None));
    }

    fn uncertain_proofreading_note(
        ws: &mut Workspace,
        summarize: bool,
        summary: c2m_artifact::artifact::Outcome,
    ) -> (String, String) {
        let snapshot = plan(&ws.state.default_library);
        let id = ws.state.enqueue(snapshot, None).unwrap().0;
        ran_with(ws, &id, |config| {
            config.llm.enabled = true;
            config.llm.summarize = summarize;
        });
        let outcomes = c2m_artifact::artifact::Outcomes {
            transcript: c2m_artifact::artifact::Outcome::succeeded(),
            proofreading: c2m_artifact::artifact::Outcome::failed("校对请求结果尚不确定"),
            summary,
            ..Default::default()
        };
        let version = publish_note_with_outcomes(&ws.state, &id, outcomes.clone());
        let task = ws.state.task_mut(&id).unwrap();
        task.state = TaskState::Uncertain;
        task.artifact = Some(version);
        task.events.outcomes = Some(outcomes.clone());
        let request = write_unknown(task, 1);
        (id, request)
    }

    /// A published note whose `purpose` request is uncertain while `outcomes` say how
    /// each part ended: the state a lost response leaves once the note is saved.
    fn uncertain_note(
        ws: &mut Workspace,
        purpose: &str,
        outcomes: c2m_artifact::artifact::Outcomes,
    ) -> (String, String) {
        let snapshot = plan(&ws.state.default_library);
        let id = ws.state.enqueue(snapshot, None).unwrap().0;
        ran_with(ws, &id, |config| {
            config.llm.enabled = true;
            config.llm.summarize = true;
            config.llm.translate = Some("zh-Hans".into());
        });
        let version = publish_note_with_outcomes(&ws.state, &id, outcomes.clone());
        let task = ws.state.task_mut(&id).unwrap();
        task.state = TaskState::Uncertain;
        task.artifact = Some(version);
        task.events.outcomes = Some(outcomes);
        let request = format!("stable-{purpose}.3");
        let receipt = c2m_dispatch::dispatch::Receipt {
            schema: 1,
            stable_id: format!("stable-{purpose}"),
            request_id: request.clone(),
            purpose: purpose.into(),
            description: "翻译 00:00–06:38 的文字".into(),
            service_version: "version-a".into(),
            attempt: 3,
            state: c2m_dispatch::dispatch::State::Uncertain,
            http_status: None,
            response: None,
            message: Some("未完整收到服务响应".into()),
            unsupported_response_format: false,
            refused_parameter: None,
            repeats: false,
            retry_authorized: None,
        };
        std::fs::create_dir_all(task.work_dir.join("requests")).unwrap();
        std::fs::write(
            task.work_dir
                .join(format!("requests/stable-{purpose}.json")),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        reconcile_receipts(task).unwrap();
        (id, request)
    }

    fn finished_except(
        component: Component,
        outcome: c2m_artifact::artifact::Outcome,
    ) -> c2m_artifact::artifact::Outcomes {
        use c2m_artifact::artifact::Outcome;
        let mut outcomes = c2m_artifact::artifact::Outcomes {
            transcript: Outcome::succeeded(),
            screenshots: Outcome::succeeded(),
            proofreading: Outcome::succeeded(),
            translation: Outcome::succeeded(),
            summary: Outcome::succeeded(),
            ..Default::default()
        };
        *match component {
            Component::Proofreading => &mut outcomes.proofreading,
            Component::Translation => &mut outcomes.translation,
            Component::Summary => &mut outcomes.summary,
            _ => unreachable!(),
        } = outcome;
        outcomes
    }

    /// The owner's deadlock: two translation batches lost their responses while
    /// proofreading and the summary finished. Resending them redoes the translation
    /// alone — never the finished proofreading the old mapping chose, which the
    /// repair refused with 「这部分无需补做」.
    #[test]
    fn resending_an_uncertain_translation_redoes_only_the_translation() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let partial = c2m_artifact::artifact::Outcome {
            status: c2m_artifact::artifact::Status::Partial,
            completed: Some(95),
            total: Some(135),
            ..c2m_artifact::artifact::Outcome::failed("翻译未全部完成（40/135 失败）")
        };
        let (id, request) = uncertain_note(
            &mut ws,
            "translation",
            finished_except(Component::Translation, partial),
        );
        let task = ws.state.task(&id).unwrap().clone();
        assert_eq!(
            task.uncertain_components().unwrap(),
            vec![Component::Translation]
        );
        let next = ws
            .state
            .reprocess(
                &id,
                task.uncertain_components().unwrap(),
                vec![request.clone()],
            )
            .unwrap();
        let child = ws.state.task(&next).unwrap();
        let c2m_engine::execution::Operation::Reprocess { components, .. } = &child.plan.operation
        else {
            panic!("expected component recovery");
        };
        assert_eq!(components, &[Component::Translation]);
        // The translation is redone into the language the note is in.
        assert_eq!(
            child.plan.repair.as_ref().unwrap().translate.as_deref(),
            Some("zh-Hans")
        );
        assert_eq!(child.resend, vec![request]);
    }

    /// The card says what a resend sends beyond the uncertain requests it lists: the
    /// summary they held back (a first send the note asked for), and for proofreading
    /// the translation of paragraphs it changes. The repair sends exactly that.
    #[test]
    fn the_card_names_what_a_resend_sends_beyond_the_uncertain_requests() {
        use c2m_artifact::artifact::Outcome;
        let held = || Outcome {
            kind: Some(c2m_artifact::artifact::FailureKind::Uncertain),
            ..Outcome::failed("前一步请求结果尚不确定；没有继续发送")
        };
        let partial = |completed, total| Outcome {
            status: c2m_artifact::artifact::Status::Partial,
            completed: Some(completed),
            total: Some(total),
            ..Outcome::failed("未全部完成")
        };
        let label = |part: crate::activity::NotePart, count: usize| {
            c2m_i18n::tr!(
                "{}未完成的 {} 段都会发送，包括以上内容",
                part.label(),
                count
            )
        };
        let summary_line = c2m_i18n::tr!("还会生成此前因此未能开始的摘要").to_owned();
        let changed_line = c2m_i18n::tr!("校对后有变化的段落会重新翻译").to_owned();
        use crate::activity::NotePart;
        // (uncertain purpose, proofreading, translation, summary) → what the card adds.
        let cases = [
            // Paid translation: two batches uncertain, the rest and the summary held back.
            (
                Component::Translation,
                Outcome::succeeded(),
                partial(40, 135),
                held(),
                vec![label(NotePart::Translation, 95), summary_line.clone()],
            ),
            // Paid proofreading: the whole translation and the summary were held back,
            // so the translation is a first send, not a repeat of changed paragraphs.
            (
                Component::Proofreading,
                partial(60, 135),
                Outcome {
                    total: Some(135),
                    completed: Some(0),
                    ..held()
                },
                held(),
                vec![
                    label(NotePart::Proofreading, 75),
                    c2m_i18n::tr!("还会翻译此前未能完成的 {} 段", 135),
                    summary_line.clone(),
                ],
            ),
            // A free service went on: the translation was made, and only paragraphs
            // proofreading changes are translated again.
            (
                Component::Proofreading,
                partial(60, 135),
                Outcome::succeeded(),
                Outcome::succeeded(),
                vec![label(NotePart::Proofreading, 75), changed_line.clone()],
            ),
            // The owner's task: two uncertain translation batches, nothing held back.
            (
                Component::Translation,
                Outcome::succeeded(),
                partial(95, 135),
                Outcome::succeeded(),
                vec![label(NotePart::Translation, 40)],
            ),
        ];
        for (part, proofreading, translation, summary, expected) in cases {
            let dir = tempfile::tempdir().unwrap();
            let mut ws = test_workspace(dir.path());
            let outcomes = c2m_artifact::artifact::Outcomes {
                proofreading,
                translation,
                summary: summary.clone(),
                ..finished_except(part, Outcome::failed("结果尚不确定"))
            };
            let (id, request) = uncertain_note(&mut ws, part.as_str(), outcomes);
            let task = ws.state.task(&id).unwrap().clone();
            let mut parts = vec![part];
            if summary.status != c2m_artifact::artifact::Status::Succeeded {
                parts.push(Component::Summary);
            }
            assert_eq!(task.uncertain_components().unwrap(), parts, "{part:?}");
            assert_eq!(task.resend_also_sends(), expected, "{part:?}");
            let next = ws.state.reprocess(&id, Vec::new(), vec![request]).unwrap();
            let c2m_engine::execution::Operation::Reprocess { components, .. } =
                &ws.state.task(&next).unwrap().plan.operation
            else {
                panic!("expected component recovery");
            };
            assert_eq!(components, &parts);
        }
        // A summary that has an attempt of its own is never sent on this decision.
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let mut outcomes = finished_except(Component::Translation, Outcome::failed("不确定"));
        outcomes.summary = held();
        let (id, _) = uncertain_note(&mut ws, "translation", outcomes);
        let task = ws.state.task_mut(&id).unwrap();
        let mut summary: c2m_dispatch::dispatch::Receipt = serde_json::from_slice(
            &std::fs::read(task.work_dir.join("requests/stable-translation.json")).unwrap(),
        )
        .unwrap();
        summary.stable_id = "stable-summary".into();
        summary.request_id = "stable-summary.1".into();
        summary.purpose = "summary".into();
        summary.state = c2m_dispatch::dispatch::State::Failed;
        std::fs::write(
            task.work_dir.join("requests/stable-summary.json"),
            serde_json::to_vec(&summary).unwrap(),
        )
        .unwrap();
        reconcile_receipts(task).unwrap();
        assert_eq!(
            task.uncertain_components().unwrap(),
            vec![Component::Translation]
        );
        assert!(task.resend_also_sends().is_empty());
    }

    /// Every AI purpose resends through the part it was made for, so the card's
    /// choice and the repair's check are one and the same.
    #[test]
    fn every_uncertain_ai_purpose_maps_to_the_part_the_repair_accepts() {
        for component in [
            Component::Proofreading,
            Component::Translation,
            Component::Summary,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut ws = test_workspace(dir.path());
            let (id, request) = uncertain_note(
                &mut ws,
                component.as_str(),
                finished_except(
                    component,
                    c2m_artifact::artifact::Outcome::failed("结果尚不确定"),
                ),
            );
            let task = ws.state.task(&id).unwrap().clone();
            let chosen = task.uncertain_components().unwrap();
            assert!(chosen.contains(&component), "{component:?}: {chosen:?}");
            let next = ws.state.reprocess(&id, chosen, vec![request]).unwrap();
            assert_ne!(next, id, "{component:?}");
        }
    }

    /// When nothing unfinished explains an uncertain request, there is nothing a
    /// repair can resend, and the reason is the same one the repair gives.
    #[test]
    fn an_uncertain_request_with_no_unfinished_part_says_why_it_cannot_be_resent() {
        for purpose in ["translation", "transcription"] {
            let dir = tempfile::tempdir().unwrap();
            let mut ws = test_workspace(dir.path());
            let (id, request) = uncertain_note(
                &mut ws,
                purpose,
                finished_except(
                    Component::Summary,
                    c2m_artifact::artifact::Outcome::succeeded(),
                ),
            );
            let task = ws.state.task(&id).unwrap().clone();
            let reason = format!("{:#}", task.uncertain_components().unwrap_err());
            let refused = ws
                .state
                .reprocess(&id, Vec::new(), vec![request])
                .unwrap_err();
            assert_eq!(format!("{refused:#}"), reason, "{purpose}");
        }
    }

    #[test]
    fn proofreading_resend_continues_an_authorized_summary_that_was_never_sent() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let (id, request) = uncertain_proofreading_note(
            &mut ws,
            true,
            c2m_artifact::artifact::Outcome::failed("前一步请求结果尚不确定；没有继续发送"),
        );
        let original = ws.state.task(&id).unwrap().clone();
        let manifest_path = original.artifact.as_ref().unwrap().join("manifest.json");
        let manifest = std::fs::read(&manifest_path).unwrap();
        ws.state
            .draft_mut()
            .unwrap()
            .change_source("next-video.mp4".into());
        let draft = ws.state.draft().unwrap().clone();
        let next = ws
            .state
            .reprocess(&id, vec![Component::Proofreading], vec![request.clone()])
            .unwrap();
        let child = ws.state.task(&next).unwrap();
        let c2m_engine::execution::Operation::Reprocess {
            components,
            note_dir,
            prior_work_dir,
            ..
        } = &child.plan.operation
        else {
            panic!("expected component recovery");
        };
        assert_eq!(components, &[Component::Proofreading, Component::Summary]);
        assert_eq!(Some(note_dir), original.artifact.as_ref());
        assert_eq!(prior_work_dir.as_ref(), Some(&original.work_dir));
        assert_eq!(child.resend, vec![request.clone()]);
        assert!(ws.state.task(&id).unwrap().plan == original.plan);
        assert!(ws.state.draft().unwrap() == &draft);
        assert_eq!(std::fs::read(manifest_path).unwrap(), manifest);
        assert_eq!(
            ws.state
                .reprocess(&id, vec![Component::Proofreading], vec![request])
                .unwrap(),
            next
        );
        assert_eq!(ws.state.tasks.len(), 2);
    }

    #[test]
    fn proofreading_resend_does_not_invent_or_repeat_a_summary() {
        use c2m_artifact::artifact::Outcome;
        for (requested, summary) in [
            (false, Outcome::failed("未要求生成摘要")),
            (true, Outcome::not_requested()),
            (true, Outcome::succeeded()),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut ws = test_workspace(dir.path());
            let (id, request) = uncertain_proofreading_note(&mut ws, requested, summary);
            let next = ws
                .state
                .reprocess(&id, vec![Component::Proofreading], vec![request])
                .unwrap();
            let child = ws.state.task(&next).unwrap();
            let c2m_engine::execution::Operation::Reprocess { components, .. } =
                &child.plan.operation
            else {
                panic!("expected component recovery");
            };
            assert_eq!(components, &[Component::Proofreading]);
        }
    }

    #[test]
    fn an_existing_summary_attempt_needs_its_own_recovery_decision() {
        use c2m_dispatch::dispatch::{Receipt, State as ReceiptState};
        for receipt_state in [
            ReceiptState::Completed,
            ReceiptState::NotSent,
            ReceiptState::Rejected,
            ReceiptState::Failed,
            ReceiptState::Sending,
            ReceiptState::Uncertain,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut ws = test_workspace(dir.path());
            let (id, request) = uncertain_proofreading_note(
                &mut ws,
                true,
                c2m_artifact::artifact::Outcome::failed("摘要未完成"),
            );
            let task = ws.state.task(&id).unwrap();
            let receipt = Receipt {
                schema: 1,
                stable_id: "summary-request".into(),
                request_id: "summary-request.1".into(),
                purpose: "summary".into(),
                description: "生成摘要".into(),
                service_version: "version-a".into(),
                attempt: 1,
                state: receipt_state.clone(),
                http_status: None,
                response: None,
                message: None,
                unsupported_response_format: false,
                refused_parameter: None,
                repeats: false,
                retry_authorized: None,
            };
            std::fs::write(
                task.work_dir.join("requests/summary-request.json"),
                serde_json::to_vec(&receipt).unwrap(),
            )
            .unwrap();
            let recovery = ws
                .state
                .reprocess(&id, vec![Component::Proofreading], vec![request]);
            if matches!(
                receipt_state,
                ReceiptState::Sending | ReceiptState::Uncertain
            ) {
                assert!(
                    recovery.is_err(),
                    "{receipt_state:?} needs exact authorization"
                );
                assert_eq!(ws.state.tasks.len(), 1);
                assert!(ws.state.task(&id).unwrap().handled_by.is_none());
            } else {
                let next = recovery.unwrap();
                let child = ws.state.task(&next).unwrap();
                let c2m_engine::execution::Operation::Reprocess { components, .. } =
                    &child.plan.operation
                else {
                    panic!("expected component recovery");
                };
                assert_eq!(components, &[Component::Proofreading], "{receipt_state:?}");
            }
        }
    }

    #[test]
    fn unknown_authorization_is_exact_and_cannot_be_reused_by_an_old_button() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::Uncertain;
        let first = write_unknown(ws.state.task(&id).unwrap(), 1);
        ws.state
            .authorize_uncertain(&id, std::slice::from_ref(&first))
            .unwrap();
        assert!(
            ws.state
                .authorize_uncertain(&id, std::slice::from_ref(&first))
                .is_err()
        );
        // Its start failed: the task needs attention again, the request still uncertain
        // under the same id, so the authorization was never used and may be given again.
        ws.state.task_mut(&id).unwrap().state = TaskState::NeedsAttention;
        ws.state
            .authorize_uncertain(&id, std::slice::from_ref(&first))
            .unwrap();
        assert_eq!(ws.state.task(&id).unwrap().resend, vec![first.clone()]);
        assert_eq!(ws.state.task(&id).unwrap().state, TaskState::Queued);
        ws.state.task_mut(&id).unwrap().state = TaskState::Uncertain;
        let second = write_unknown(ws.state.task(&id).unwrap(), 2);
        assert!(ws.state.authorize_uncertain(&id, &[first]).is_err());
        ws.state.authorize_uncertain(&id, &[second]).unwrap();
        assert_eq!(ws.state.task(&id).unwrap().resend.len(), 2);
        assert!(
            ws.state.task(&id).unwrap().events.blocked[0]
                .message
                .contains("00:00–00:20")
        );
    }

    #[test]
    fn published_result_after_crash_is_not_enqueued_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::Running;
        ws.save().unwrap();
        let version = publish_note(&ws.state, &id, false);
        let restored = test_workspace(dir.path());
        let task = restored.state.task(&id).unwrap();
        assert_eq!(task.state, TaskState::Complete);
        assert_eq!(task.artifact.as_ref(), Some(&version));
        assert!(restored.state.next_task().is_none());
    }

    #[test]
    fn export_keys_parse_as_the_engine_spells_them() {
        use c2m_config::config::OutputFormat;
        for format in [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json] {
            assert_eq!(output_format(&format.to_string()), Some(format));
        }
        assert_eq!(output_format("pdf"), None);
        assert_eq!(output_format("MD"), None);
    }

    #[test]
    fn startup_checks_run_later_and_hold_dispatch_until_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let published = ws.state.enqueue(plan(&library), None).unwrap().0;
        let mut other = plan(&library);
        other.source_id = "video-b".into();
        other.title = "课程 B".into();
        let queued = ws.state.enqueue(other, None).unwrap().0;
        ws.state.task_mut(&published).unwrap().state = TaskState::Running;
        ws.save().unwrap();
        let version = publish_note(&ws.state, &published, false);
        let open = || {
            Workspace::open_at(
                dir.path().join("workspace.json"),
                dir.path().join("library"),
            )
            .unwrap()
        };
        let mut reopened = open();
        // Opening does not hash the library; it dispatches nothing meanwhile.
        assert!(reopened.state.task(&published).unwrap().artifact.is_none());
        assert!(reopened.state.next_task().is_none());
        let reconciliation = reopened.reconciliation().unwrap();
        // Findings for a record that was replaced meanwhile are not applied.
        let mut replaced = open();
        let stale = replaced.reconciliation().unwrap();
        reopened.apply_reconciliation(stale.run()).unwrap();
        assert!(reopened.state.reconciling);
        reopened.apply_reconciliation(reconciliation.run()).unwrap();
        let task = reopened.state.task(&published).unwrap();
        assert_eq!(task.state, TaskState::Complete);
        assert_eq!(task.artifact.as_ref(), Some(&version));
        assert_eq!(reopened.state.next_task().unwrap().id, queued);
        // Its mirrors are the ones `replaced` is about to check.
        reopened.library_writes.drain();
        replaced.reconcile_now();
        assert!(!replaced.state.reconciling);
    }

    #[test]
    fn partial_followup_is_single_and_does_not_rewrite_another_draft_or_successful_stage() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        let version = publish_note(&ws.state, &id, true);
        let outcomes = c2m_artifact::artifact::read_manifest(&version.join("manifest.json"))
            .unwrap()
            .outcomes;
        let original = ws.state.task_mut(&id).unwrap();
        original.state = TaskState::Partial;
        original.artifact = Some(version);
        original.events.outcomes = Some(outcomes.clone());
        ws.state
            .draft_mut()
            .unwrap()
            .change_source("draft-b.mp4".into());
        let draft = ws.state.draft().unwrap().clone();
        assert!(
            ws.state
                .reprocess(&id, vec![Component::Screenshots], vec![])
                .is_err()
        );
        let followup = ws
            .state
            .reprocess(&id, vec![Component::Proofreading], vec![])
            .unwrap();
        assert_eq!(
            ws.state
                .reprocess(&id, vec![Component::Proofreading], vec![])
                .unwrap(),
            followup
        );
        assert_eq!(ws.state.tasks.len(), 2);
        assert!(ws.state.draft().unwrap() == &draft);
        assert!(ws.state.set_intent(&id, Intent::Run).is_err());
        let child = ws.state.task(&followup).unwrap();
        assert_eq!(child.parent.as_deref(), Some(id.as_str()));
        assert_eq!(child.plan.source.input, "lecture.mp4");
    }

    #[test]
    fn damaged_primary_and_backup_are_preserved_before_paused_mirror_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let snapshot = plan(&ws.state.default_library);
        let id = ws.state.enqueue(snapshot, None).unwrap().0;
        ws.state.task_mut(&id).unwrap().ran_with = Some(RunSettings {
            config: ConfigFile {
                llm: c2m_config::llm_settings::LlmSettings {
                    api_key: "fake-test-credential".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            asr_service: None,
            ai_service: None,
        });
        ws.state.task_mut(&id).unwrap().state = TaskState::Running;
        ws.state.task_mut(&id).unwrap().resend = vec!["old-attempt.1".into()];
        ws.save().unwrap();
        let work = ws.state.task(&id).unwrap().work_dir.clone();
        let mirror = std::fs::read_to_string(work.join("task-record.json")).unwrap();
        assert!(!mirror.contains("fake-test-credential"));
        std::fs::write(work.join("partial-material.txt"), "saved words").unwrap();
        std::fs::write(
            work.join("control.json"),
            r#"{"intent":"run","resend":["old-attempt.1"]}"#,
        )
        .unwrap();
        std::fs::write(ws.storage_path(), b"{broken primary").unwrap();
        std::fs::write(
            desktop_prefs::store::backup_path(ws.storage_path()),
            b"{broken backup",
        )
        .unwrap();
        assert!(Workspace::open_at_inline(ws.path.clone(), dir.path().join("library")).is_err());
        let rebuilt = Workspace::rebuild_at(ws.path.clone(), dir.path().join("library")).unwrap();
        assert_eq!(rebuilt.state.task(&id).unwrap().state, TaskState::Paused);
        assert_eq!(rebuilt.state.task(&id).unwrap().intent, Intent::Pause);
        assert!(rebuilt.state.task(&id).unwrap().resend.is_empty());
        assert!(rebuilt.state.next_task().is_none());
        assert_eq!(
            std::fs::read_to_string(work.join("partial-material.txt")).unwrap(),
            "saved words"
        );
        let control: serde_json::Value =
            serde_json::from_slice(&std::fs::read(work.join("control.json")).unwrap()).unwrap();
        assert_eq!(control["intent"], "pause");
        assert_eq!(control["resend"], serde_json::json!([]));
        let archive = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("workspace-recovery-")
            })
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read(archive.join("workspace.json")).unwrap(),
            b"{broken primary"
        );
        assert_eq!(
            std::fs::read(archive.join("workspace.json.bak")).unwrap(),
            b"{broken backup"
        );
        let reopened =
            Workspace::open_at_inline(ws.path.clone(), dir.path().join("library")).unwrap();
        assert!(reopened.state.next_task().is_none());
        assert_eq!(reopened.state.tasks.len(), 1);
    }

    #[test]
    fn an_offline_location_is_not_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.save().unwrap();
        let root = ws.state.libraries[0].root.clone();
        std::fs::rename(&root, dir.path().join("disconnected-library")).unwrap();
        ws.transaction(|state| state.set_intent(&id, Intent::Pause))
            .unwrap();
        ws.library_writes.drain();
        assert!(ws.take_write_error().is_none());
        let reopened = test_workspace(dir.path());
        assert!(!root.exists());
        assert_eq!(reopened.state.task(&id).unwrap().state, TaskState::Paused);
    }

    #[test]
    fn backup_recovery_does_not_assume_a_lost_final_run_intent() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::Running;
        ws.save().unwrap();
        ws.save().unwrap();
        std::fs::write(ws.storage_path(), b"damaged").unwrap();
        let recovered = test_workspace(dir.path());
        assert_eq!(recovered.state.task(&id).unwrap().state, TaskState::Paused);
        assert!(recovered.state.next_task().is_none());
    }

    #[test]
    fn older_parent_links_recover_to_latest_followup_without_requeuing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let parent = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.state.task_mut(&parent).unwrap().state = TaskState::NeedsAttention;
        let mut changed = plan(&ws.state.default_library);
        changed.title = "changed display title".into();
        let next = ws.state.enqueue(changed, Some(parent.clone())).unwrap().0;
        ws.state.task_mut(&parent).unwrap().handled_by = None;
        ws.state.task_mut(&parent).unwrap().state = TaskState::Running;
        ws.state.recover();
        let original = ws.state.task(&parent).unwrap();
        assert_eq!(original.handled_by.as_deref(), Some(next.as_str()));
        assert_eq!(original.intent, Intent::Pause);
        assert_eq!(ws.state.next_task().unwrap().id, next);
    }

    #[test]
    fn export_only_followup_needs_no_ai_service_and_retries_only_failed_formats() {
        use c2m_config::config::OutputFormat;
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        let version = publish_note(&ws.state, &id, false);
        let original = ws.state.task_mut(&id).unwrap();
        original.state = TaskState::Partial;
        original.artifact = Some(version);
        original.events.outcomes = Some(c2m_artifact::artifact::Outcomes {
            exports: BTreeMap::from([
                ("md".into(), c2m_artifact::artifact::Outcome::succeeded()),
                (
                    "html".into(),
                    c2m_artifact::artifact::Outcome::failed("disk full"),
                ),
            ]),
            ..Default::default()
        });
        let next = ws
            .state
            .reprocess(&id, vec![Component::Exports], vec![])
            .unwrap();
        let child = ws.state.task(&next).unwrap();
        let repair = child.plan.repair.as_ref().unwrap();
        assert_eq!(repair.formats, Some(vec![OutputFormat::Html]));
        assert_eq!(repair.translate, None);
    }

    #[test]
    fn published_exports_remain_reachable_after_a_lost_completion_event() {
        use c2m_artifact::{artifact, portable};
        use c2m_config::config::OutputFormat;
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::Running;
        ws.save().unwrap();
        let outcomes = artifact::Outcomes {
            transcript: artifact::Outcome::succeeded(),
            ..Default::default()
        };
        let version = publish_note_with_exports(
            &ws.state,
            &id,
            outcomes,
            &[OutputFormat::Md, OutputFormat::Html],
        );
        let restored = test_workspace(dir.path());
        assert_eq!(restored.state.task(&id).unwrap().state, TaskState::Complete);
        let task = restored.state.task(&id).unwrap();
        let location = restored.state.library(&task.plan.library_id).unwrap();
        let markdown = version
            .join("exports")
            .join(portable::file_name(OutputFormat::Md));
        let html = version
            .join("exports")
            .join(portable::file_name(OutputFormat::Html));
        assert_eq!(task.state, TaskState::Complete);
        assert_eq!(
            task_export_files(task, location),
            vec![markdown.clone(), html.clone()]
        );
        assert_eq!(available_task_export(task, location).unwrap(), markdown);
        assert!(restored.state.next_task().is_none());

        // A missing first format must not hide another successful output, and
        // the readable body's directory is never substituted for missing exports.
        std::fs::remove_file(&markdown).unwrap();
        assert_eq!(available_task_export(task, location).unwrap(), html);
        std::fs::remove_file(&html).unwrap();
        assert!(available_task_export(task, location).is_err());
        assert!(version.join("course.md").is_file());
        assert!(task.plan == ws.state.task(&id).unwrap().plan);
    }

    #[test]
    fn export_only_completion_reopens_its_own_files_and_recovers_missing_outputs() {
        use c2m_artifact::{artifact, portable};
        use c2m_config::config::OutputFormat;
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        let mut outcomes = artifact::Outcomes {
            transcript: artifact::Outcome::succeeded(),
            ..Default::default()
        };
        outcomes
            .exports
            .insert("html".into(), artifact::Outcome::failed("disk full"));
        let base = publish_note_with_exports(&ws.state, &id, outcomes, &[OutputFormat::Md]);
        let manifest = artifact::read_manifest(&base.join("manifest.json")).unwrap();
        let original = ws.state.task_mut(&id).unwrap();
        original.state = TaskState::Partial;
        original.artifact = Some(base.clone());
        original.events.outcomes = Some(manifest.outcomes.clone());
        let original_plan = original.plan.clone();
        let old_markdown = base
            .join("exports")
            .join(portable::file_name(OutputFormat::Md));
        let old_bytes = std::fs::read(&old_markdown).unwrap();
        let next = ws
            .state
            .reprocess(&id, vec![Component::Exports], vec![])
            .unwrap();
        ws.state.task_mut(&next).unwrap().state = TaskState::Running;
        ws.save().unwrap();
        let child = ws.state.task(&next).unwrap();
        let next_plan = child.plan.clone();
        let location = ws.state.library(&child.plan.library_id).unwrap();
        let output = task_export_directory(child, location)
            .unwrap()
            .join(portable::file_name(OutputFormat::Html));
        // The engine writes the export into the note and lists it as the note's own.
        let scratch = dir.path().join("scratch.html");
        portable::export(&base, OutputFormat::Html, &scratch).unwrap();
        let written =
            artifact::write_export(&base, OutputFormat::Html, &std::fs::read(&scratch).unwrap())
                .unwrap();
        assert_eq!(written.path, output);
        std::fs::write(
            child.work_dir.join("export-result.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "outputs": [output],
                "outcomes": {"html": {"status": "succeeded"}}
            }))
            .unwrap(),
        )
        .unwrap();

        let mut restored = test_workspace(dir.path());
        let task = restored.state.task(&next).unwrap();
        let location = restored.state.library(&task.plan.library_id).unwrap();
        assert_eq!(task.state, TaskState::Complete);
        assert_eq!(task.artifact.as_ref(), Some(&base));
        assert_eq!(task_export_files(task, location), vec![output.clone()]);
        assert_eq!(available_task_export(task, location).unwrap(), output);
        assert!(output.starts_with(&base));
        assert!(task.plan == next_plan);
        assert!(restored.state.task(&id).unwrap().plan == original_plan);
        assert!(restored.state.next_task().is_none());
        restored.state.task_mut(&next).unwrap().unread = false;
        restored.save().unwrap();

        // Opening the app does not announce a previously read result again.
        let reopened = test_workspace(dir.path());
        let task = reopened.state.task(&next).unwrap();
        assert!(!task.unread);
        assert_eq!(
            available_task_export(task, reopened.state.library(&task.plan.library_id).unwrap())
                .unwrap(),
            output
        );

        std::fs::remove_file(&output).unwrap();
        let mut missing = test_workspace(dir.path());
        let task = missing.state.task(&next).unwrap();
        let location = missing.state.library(&task.plan.library_id).unwrap();
        assert_eq!(task.state, TaskState::Partial);
        assert!(task.unread && task_export_files(task, location).is_empty());
        assert!(available_task_export(task, location).is_err());
        let retry = missing
            .state
            .reprocess(&next, vec![Component::Exports], vec![])
            .unwrap();
        let retry = missing.state.task(&retry).unwrap();
        assert!(retry.exports_only());
        assert_eq!(
            retry.plan.repair.as_ref().unwrap().formats,
            Some(vec![OutputFormat::Html])
        );
        assert_eq!(std::fs::read(old_markdown).unwrap(), old_bytes);
        // The note is still the original task's.
        assert_eq!(artifact::note_manifest(&base).unwrap().unwrap().task_id, id);
        assert!(missing.state.task(&next).unwrap().plan == next_plan);
        assert!(missing.state.task(&id).unwrap().plan == original_plan);
    }

    #[test]
    fn explicit_library_reassociation_retains_identity_and_never_overwrites_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        ws.save().unwrap();
        let marker = ws.state.libraries[0].root.join(".course2md-library-id");
        let id = ws.state.default_library.clone();
        std::fs::remove_file(&marker).unwrap();
        let root = ws.state.libraries[0].root.clone();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let link = crate::storage::inspect_link(&ws.state, &id, &root, &cancel).unwrap();
        assert!(link.unmarked.is_some() && !marker.exists());
        let tasks = crate::storage::finish_link(&link).unwrap();
        ws.transaction(|state| crate::storage::publish_link(state, &link, tasks))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), id);
        std::fs::write(&marker, "a-different-library").unwrap();
        assert!(crate::storage::inspect_link(&ws.state, &id, &root, &cancel).is_err());
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "a-different-library"
        );
    }

    #[test]
    fn a_future_primary_schema_is_not_replaced_by_an_older_valid_backup() {
        let dir = tempfile::tempdir().unwrap();
        let ws = test_workspace(dir.path());
        ws.save().unwrap();
        ws.save().unwrap();
        let mut future = serde_json::to_value(&ws.state).unwrap();
        future["schema"] = serde_json::json!(SCHEMA + 1);
        let bytes = serde_json::to_vec(&future).unwrap();
        std::fs::write(ws.storage_path(), &bytes).unwrap();
        assert!(Workspace::open_at_inline(ws.path.clone(), dir.path().join("library")).is_err());
        assert!(Workspace::rebuild_at(ws.path.clone(), dir.path().join("library")).is_err());
        assert_eq!(std::fs::read(ws.storage_path()).unwrap(), bytes);
    }

    /// Saves `task_id`'s note the way an older app did: as the version named after
    /// the task in its course's `versions/`.
    fn legacy_version_of(state: &State, task_id: &str, revision: u64) -> PathBuf {
        let task = state.task(task_id).unwrap();
        let location = state.library(&task.plan.library_id).unwrap();
        let target = c2m_artifact::artifact::Target {
            task_id: task.id.clone(),
            course_id: course_dir_name(&task.plan.source_id),
            source_id: task.plan.source_id.clone(),
            course_dir: task_note(task, location),
            replaces: None,
        };
        crate::notes::fixtures::legacy_version(
            &target,
            revision,
            &[10.],
            c2m_artifact::artifact::Outcomes {
                transcript: c2m_artifact::artifact::Outcome::succeeded(),
                ..Default::default()
            },
        )
    }

    /// A task that finished with `artifact` as its result.
    fn finished_task(ws: &mut Workspace, artifact: Option<PathBuf>) -> String {
        let library = ws.state.default_library.clone();
        let (id, created) = ws.state.enqueue(plan(&library), None).unwrap();
        assert!(created);
        let task = ws.state.task_mut(&id).unwrap();
        task.state = TaskState::Complete;
        task.intent = Intent::Run;
        task.artifact = artifact;
        id
    }

    /// A course an older app saved with two versions, one per task: `a` (revision 1)
    /// and `b` (revision 2), with each task's record pointing at its version, and
    /// reading positions saved per version.
    struct Versioned {
        dir: tempfile::TempDir,
        a: String,
        b: String,
        note: PathBuf,
        course: String,
    }

    fn position(offset: f32) -> ReadingPosition {
        ReadingPosition {
            offset,
            ..Default::default()
        }
    }

    /// `pointed`: `current.json` names version `a`, the older one; otherwise it is
    /// damaged.
    fn versioned(pointed: bool) -> Versioned {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let a = finished_task(&mut ws, None);
        let version_a = legacy_version_of(&ws.state, &a, 1);
        ws.state.task_mut(&a).unwrap().artifact = Some(version_a.clone());
        let b = finished_task(&mut ws, None);
        let version_b = legacy_version_of(&ws.state, &b, 2);
        ws.state.task_mut(&b).unwrap().artifact = Some(version_b);
        let note = version_a.parent().unwrap().parent().unwrap().to_path_buf();
        let course = course_dir_name("video-a");
        if pointed {
            crate::notes::fixtures::point_at(&note, &course, &a);
        } else {
            std::fs::write(note.join("current.json"), b"{damaged").unwrap();
        }
        for (version, tab, offset) in [(&a, 0, 1.), (&b, 0, 2.), (&a, 1, 3.)] {
            ws.state
                .positions
                .insert(format!("{course}:{version}:{tab}"), position(offset));
        }
        ws.save().unwrap();
        ws.library_writes.drain();
        Versioned {
            dir,
            a,
            b,
            note,
            course,
        }
    }

    /// After the migration: one flat note, `kept`'s, and every record follows it.
    fn assert_flat(ws: &Workspace, fixture: &Versioned, kept: &str, other: &str) {
        let note = &fixture.note;
        assert!(!c2m_artifact::artifact::is_legacy(note));
        assert!(!note.join("versions").exists() && !note.join("current.json").exists());
        let manifest = c2m_artifact::artifact::note_manifest(note)
            .unwrap()
            .unwrap();
        assert_eq!(manifest.task_id, kept);
        let kept = ws.state.task(kept).unwrap();
        assert_eq!(kept.artifact.as_ref(), Some(note));
        assert_eq!(kept.replaced_by, None);
        assert_eq!(kept.state, TaskState::Complete);
        // The other version was deleted: its task's result is gone.
        let other = ws.state.task(other).unwrap();
        assert_eq!(
            other.replaced_by.as_deref(),
            Some(manifest.task_id.as_str())
        );
        assert_eq!(other.artifact, None);
        assert_eq!(other.state, TaskState::Complete);
        assert!(ws.state.next_task().is_none());
        assert!(!ws.state.reconciling);
    }

    /// An older app run after the migration saved one more version into the flat
    /// note: the next opening makes it the note, keeps the flat note as a copy, and
    /// says so once, though no other version was deleted.
    #[test]
    fn a_version_an_older_app_saved_after_the_migration_is_told_once() {
        let fixture = versioned(true);
        let mut ws = test_workspace(fixture.dir.path());
        assert_flat(&ws, &fixture, &fixture.a, &fixture.b);
        ws.take_migrated();
        let c = finished_task(&mut ws, None);
        let version_c = legacy_version_of(&ws.state, &c, 3);
        ws.state.task_mut(&c).unwrap().artifact = Some(version_c);
        crate::notes::fixtures::point_at(&fixture.note, &fixture.course, &c);
        ws.save().unwrap();
        drop(ws);
        let mut again = test_workspace(fixture.dir.path());
        let manifest = c2m_artifact::artifact::note_manifest(&fixture.note)
            .unwrap()
            .unwrap();
        assert_eq!(manifest.task_id, c);
        let copies = c2m_artifact::artifact::kept_copies(&fixture.note);
        assert_eq!(copies.len(), 1);
        assert!(c2m_artifact::artifact::is_older_app_copy(&copies[0]));
        let migrated = again.take_migrated().unwrap();
        assert_eq!(migrated.notice, Some(crate::migrate::resaved_notice(1)));
        assert!(test_workspace(fixture.dir.path()).take_migrated().is_none());
    }

    #[test]
    fn the_migration_keeps_the_version_the_pointer_names_and_rewrites_the_records() {
        // The pointer names the older version.
        let fixture = versioned(true);
        let mut ws = test_workspace(fixture.dir.path());
        assert_flat(&ws, &fixture, &fixture.a, &fixture.b);
        let course = &fixture.course;
        // Reading positions move to the note, the kept version's first.
        let positions: Vec<_> = ws
            .state
            .positions
            .iter()
            .map(|(key, position)| (key.clone(), position.offset))
            .collect();
        assert_eq!(
            positions,
            [(format!("{course}:0"), 1.), (format!("{course}:1"), 3.)]
        );
        let migrated = ws.take_migrated().unwrap();
        assert!(migrated.notes_changed);
        assert_eq!(
            migrated.notice.as_deref(),
            Some(crate::migrate::removed_versions_notice())
        );
        assert!(ws.take_migrated().is_none());
        // The rewritten records are saved, and a second opening has nothing to do.
        let mut again = test_workspace(fixture.dir.path());
        assert_flat(&again, &fixture, &fixture.a, &fixture.b);
        assert!(again.take_migrated().is_none());
        assert_eq!(again.state.positions, ws.state.positions);
        // The kept note is listed; a repair of the replaced task is refused.
        let scan = crate::notes::scan_library(&fixture.dir.path().join("library")).unwrap();
        assert_eq!(scan.courses.len(), 1);
        assert!(scan.issues.is_empty(), "{:?}", scan.issues);
        assert!(
            again
                .state
                .reprocess(&fixture.b, vec![Component::Exports], vec![])
                .is_err()
        );
    }

    #[test]
    fn a_damaged_pointer_keeps_the_newest_intact_version() {
        let fixture = versioned(false);
        let ws = test_workspace(fixture.dir.path());
        assert_flat(&ws, &fixture, &fixture.b, &fixture.a);
        assert_eq!(
            ws.state.positions[&format!("{}:0", fixture.course)].offset,
            2.
        );
    }

    /// Exports an older app saved beside the course (`exports/<version>/<task>/`)
    /// join the note's own `exports/`, and the task that wrote them finds them there.
    #[test]
    fn exports_saved_beside_the_course_move_into_the_note_with_their_task() {
        use c2m_config::config::OutputFormat;
        let fixture = versioned(true);
        let mut ws = Workspace::open_at(
            fixture.dir.path().join("workspace.json"),
            fixture.dir.path().join("library"),
        )
        .unwrap();
        let version_a = fixture.note.join("versions").join(&fixture.a);
        let mut export = ws.state.task(&fixture.a).unwrap().clone();
        export.id = "task-export".into();
        export.created += 1;
        export.plan.operation = serde_json::from_value(serde_json::json!({
            "kind": "reprocess", "base_version_dir": version_a,
            "components": ["exports"], "prior_work_dir": null,
        }))
        .unwrap();
        export.artifact = Some(version_a.clone());
        let html = c2m_artifact::portable::file_name(OutputFormat::Html);
        let old = fixture
            .note
            .join("exports")
            .join(&fixture.a)
            .join(&export.id);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join(html), b"<p>exported</p>").unwrap();
        let work = fixture
            .dir
            .path()
            .join("library/.course2md/work")
            .join(&export.id);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            work.join("export-result.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1, "outputs": [old.join(html)],
                "outcomes": {"html": {"status": "succeeded"}},
            }))
            .unwrap(),
        )
        .unwrap();
        ws.state.tasks.push(export);
        ws.save().unwrap();
        ws.library_writes.drain();
        let ws = test_workspace(fixture.dir.path());
        assert_flat(&ws, &fixture, &fixture.a, &fixture.b);
        let export = ws.state.task("task-export").unwrap();
        let location = ws.state.library(&export.plan.library_id).unwrap();
        let moved = fixture.note.join("exports").join(html);
        assert_eq!(std::fs::read(&moved).unwrap(), b"<p>exported</p>");
        assert_eq!(export.artifact.as_ref(), Some(&fixture.note));
        assert_eq!(export.replaced_by, None);
        assert_eq!(task_export_files(export, location), vec![moved.clone()]);
        assert_eq!(available_task_export(export, location).unwrap(), moved);
    }

    /// A crash after the course was flattened but before the records were saved, or
    /// between the flattening's two renames, is finished by the next opening.
    #[test]
    fn an_interrupted_migration_is_finished_by_the_next_opening() {
        for between_renames in [false, true] {
            let fixture = versioned(true);
            if between_renames {
                let swap = fixture
                    .dir
                    .path()
                    .join("library/.course2md/swap")
                    .join(fixture.note.file_name().unwrap());
                std::fs::create_dir_all(&swap).unwrap();
                // A replacement names the copy that is the note before it moves it.
                std::fs::write(swap.join("live"), "old-flatten").unwrap();
                std::fs::rename(&fixture.note, swap.join("old-flatten")).unwrap();
            } else {
                c2m_artifact::artifact::flatten_legacy(&fixture.note)
                    .unwrap()
                    .unwrap();
            }
            let ws = test_workspace(fixture.dir.path());
            assert_flat(&ws, &fixture, &fixture.a, &fixture.b);
            assert_eq!(
                ws.state.positions[&format!("{}:0", fixture.course)].offset,
                1.,
                "{between_renames}"
            );
            assert!(
                !fixture
                    .dir
                    .path()
                    .join("library/.course2md/swap")
                    .join(fixture.note.file_name().unwrap())
                    .exists()
            );
        }
    }

    /// Tasks from the older app keep their receipts: a repair of the kept version
    /// still asks about its uncertain request, and one of a deleted version stops for
    /// good with its receipts left where they were.
    #[test]
    fn in_flight_repairs_keep_their_receipts_and_a_repair_of_a_deleted_version_stops() {
        let fixture = versioned(true);
        let mut ws = Workspace::open_at(
            fixture.dir.path().join("workspace.json"),
            fixture.dir.path().join("library"),
        )
        .unwrap();
        let mut repairs = Vec::new();
        for (id, base) in [("repair-kept", &fixture.a), ("repair-gone", &fixture.b)] {
            let mut repair = ws.state.task(base).unwrap().clone();
            repair.id = id.into();
            repair.created += 10;
            repair.state = TaskState::Uncertain;
            repair.intent = Intent::Pause;
            repair.artifact = None;
            repair.plan.operation = serde_json::from_value(serde_json::json!({
                "kind": "reprocess",
                "base_version_dir": fixture.note.join("versions").join(base),
                "components": ["proofreading"], "prior_work_dir": null,
            }))
            .unwrap();
            repair.work_dir = fixture.dir.path().join("library/.course2md/work").join(id);
            receipt(&repair, c2m_dispatch::dispatch::State::Uncertain);
            repairs.push(repair);
        }
        ws.state.tasks.extend(repairs);
        ws.save().unwrap();
        ws.library_writes.drain();
        let ws = test_workspace(fixture.dir.path());
        let kept = ws.state.task("repair-kept").unwrap();
        assert_eq!(kept.state, TaskState::Uncertain);
        assert!(kept.has_uncertain());
        assert_eq!(kept.replaced_by, None);
        assert!(matches!(
            &kept.plan.operation,
            c2m_engine::execution::Operation::Reprocess { note_dir, base_task_id, .. }
                if *note_dir == fixture.note && *base_task_id == fixture.a
        ));
        let gone = ws.state.task("repair-gone").unwrap();
        assert_eq!(gone.state, TaskState::Cancelled);
        assert_eq!(gone.replaced_by.as_deref(), Some(fixture.a.as_str()));
        assert_eq!(gone.events.error.as_deref(), Some(replaced_message()));
        assert!(gone.work_dir.join("requests/stable-request.json").is_file());
        assert!(ws.state.next_task().is_none());
    }

    /// A course a worker from an earlier session still writes is left as versions
    /// until the worker lets go, then flattened, and its tasks read again.
    #[test]
    fn a_course_a_live_worker_writes_is_flattened_once_it_lets_go() {
        let fixture = versioned(true);
        let mut ws = Workspace::open_at(
            fixture.dir.path().join("workspace.json"),
            fixture.dir.path().join("library"),
        )
        .unwrap();
        let library = ws.state.default_library.clone();
        let (writer, created) = ws.state.enqueue(plan(&library), None).unwrap();
        assert!(created);
        ws.state.task_mut(&writer).unwrap().state = TaskState::Running;
        ws.save().unwrap();
        ws.library_writes.drain();
        let work = ws.state.task(&writer).unwrap().work_dir.clone();
        std::fs::create_dir_all(&work).unwrap();
        let worker = c2m_process::runtime::lock_file(&work.join(".task.lock")).unwrap();
        let mut ws = test_workspace(fixture.dir.path());
        assert!(ws.state.task(&writer).unwrap().held);
        assert!(c2m_artifact::artifact::is_legacy(&fixture.note));
        assert!(fixture.note.join("versions").join(&fixture.b).is_dir());
        assert!(ws.take_migrated().is_none());
        assert!(ws.state.next_task().is_none());
        drop(worker);
        let later = std::time::Instant::now() + 2 * HELD_CHECK_INTERVAL;
        let check = ws.reconciliation_at(later).unwrap();
        ws.apply_reconciliation(check.run()).unwrap();
        assert!(!c2m_artifact::artifact::is_legacy(&fixture.note));
        assert!(ws.take_migrated().unwrap().notes_changed);
        // The tasks read before are read again, for the note as it is now.
        assert!(ws.state.reconciling);
        ws.reconcile_now();
        assert_flat_except_queue(&ws, &fixture);
        assert!(!ws.state.task(&writer).unwrap().held);
        assert_eq!(ws.state.task(&writer).unwrap().replaced_by, None);
    }

    fn assert_flat_except_queue(ws: &Workspace, fixture: &Versioned) {
        let a = ws.state.task(&fixture.a).unwrap();
        assert_eq!(a.artifact.as_ref(), Some(&fixture.note));
        let b = ws.state.task(&fixture.b).unwrap();
        assert_eq!(b.replaced_by.as_deref(), Some(fixture.a.as_str()));
        assert_eq!(b.artifact, None);
    }

    /// A note has one unfinished task at a time: starting another focuses it.
    #[test]
    fn a_note_takes_one_unfinished_task_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let (first, created) = ws.state.enqueue(plan(&library), None).unwrap();
        assert!(created);
        // Other options for the same video are still the same note.
        let mut other = plan(&library);
        other.title = "另一个名字".into();
        other.text_source = Some(c2m_config::config::TranscriptSource::Asr);
        assert_eq!(
            ws.state.duplicate_of(&other, None).map(|t| t.id.clone()),
            Some(first.clone())
        );
        assert_eq!(
            ws.state.enqueue(other.clone(), None).unwrap(),
            (first.clone(), false)
        );
        assert_eq!(ws.state.tasks.len(), 1);
        // Another video is another note.
        let mut video = plan(&library);
        video.source_id = "video-b".into();
        assert!(ws.state.enqueue(video, None).unwrap().1);
        // Once the first has finished, the note takes the next one.
        ws.state.task_mut(&first).unwrap().state = TaskState::Complete;
        let (next, created) = ws.state.enqueue(other, None).unwrap();
        assert!(created && next != first);
    }

    /// A repair whose note another task replaced since stops for good on opening and
    /// is never queued again; a finished task whose note was replaced is not repaired.
    #[test]
    fn a_replaced_task_never_runs_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        let note = publish_note(&ws.state, &id, true);
        let original = ws.state.task_mut(&id).unwrap();
        original.state = TaskState::Partial;
        original.artifact = Some(note.clone());
        original.events.outcomes = Some(
            c2m_artifact::artifact::note_manifest(&note)
                .unwrap()
                .unwrap()
                .outcomes,
        );
        let repair = ws
            .state
            .reprocess(&id, vec![Component::Proofreading], vec![])
            .unwrap();
        assert_eq!(ws.state.task(&repair).unwrap().state, TaskState::Queued);
        ws.save().unwrap();
        ws.library_writes.drain();
        // Another app, or the command line, regenerated the note meanwhile.
        let task = ws.state.task(&id).unwrap();
        let target = c2m_artifact::artifact::Target {
            task_id: "task-elsewhere".into(),
            course_id: course_dir_name(&task.plan.source_id),
            source_id: task.plan.source_id.clone(),
            course_dir: note.clone(),
            replaces: None,
        };
        crate::notes::fixtures::publish(&target, 3, &[5.], Default::default(), None);
        for _ in 0..2 {
            let mut reopened = test_workspace(dir.path());
            let stale = reopened.state.task(&repair).unwrap();
            assert_eq!(stale.state, TaskState::Cancelled);
            assert_eq!(stale.replaced_by.as_deref(), Some("task-elsewhere"));
            assert!(reopened.state.next_task().is_none());
            assert!(reopened.state.set_intent(&repair, Intent::Run).is_err());
            let original = reopened.state.task(&id).unwrap();
            assert_eq!(original.replaced_by.as_deref(), Some("task-elsewhere"));
            assert_eq!(original.artifact, None);
            // Its repair stays the one it had; none is made again.
            assert_eq!(
                reopened
                    .state
                    .reprocess(&id, vec![Component::Proofreading], vec![])
                    .unwrap(),
                repair
            );
            assert!(
                reopened
                    .state
                    .reprocess(&repair, vec![Component::Proofreading], vec![])
                    .is_err()
            );
            reopened.library_writes.drain();
        }
    }

    /// A finished task whose note another task replaced, not yet seen by a startup
    /// check, is refused a repair that would redo another task's note.
    #[test]
    fn a_repair_is_planned_only_from_the_task_whose_note_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        let note = publish_note(&ws.state, &id, true);
        let original = ws.state.task_mut(&id).unwrap();
        original.state = TaskState::Partial;
        original.artifact = Some(note.clone());
        original.events.outcomes = Some(
            c2m_artifact::artifact::note_manifest(&note)
                .unwrap()
                .unwrap()
                .outcomes,
        );
        let task = ws.state.task(&id).unwrap();
        let target = c2m_artifact::artifact::Target {
            task_id: "task-elsewhere".into(),
            course_id: course_dir_name(&task.plan.source_id),
            source_id: task.plan.source_id.clone(),
            course_dir: note,
            replaces: None,
        };
        crate::notes::fixtures::publish(&target, 3, &[5.], Default::default(), None);
        let error = ws
            .state
            .reprocess(&id, vec![Component::Proofreading], vec![])
            .unwrap_err();
        assert!(error.to_string().contains("之后的任务"), "{error:#}");
        assert_eq!(ws.state.tasks.len(), 1);
    }
}
