//! Durable user work. Views never supply the identity or settings of a retry.
use crate::conversion::ConversionOptions;
use anyhow::{Context, Result, bail, ensure};
use course2md::execution::Component;
use course2md::progress::BlockReason;
use course2md::settings::ConfigFile;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub use crate::activity::{BlockedRequest, JobEvents};
pub use crate::store::{new_id, now};

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
        &course2md::execution::digest(source_id.as_bytes())[..32]
    )
}

/// The text source choice is stored as its historical index (0 auto,
/// 1 subtitles, 2 speech recognition).
pub mod text_source_index {
    use course2md::config::TranscriptSource;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &TranscriptSource,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(match value {
            TranscriptSource::Auto => 0,
            TranscriptSource::Subtitle => 1,
            TranscriptSource::Asr => 2,
        })
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<TranscriptSource, D::Error> {
        Ok(match u64::deserialize(deserializer)? {
            1 => TranscriptSource::Subtitle,
            2 => TranscriptSource::Asr,
            _ => TranscriptSource::Auto,
        })
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Override {
    Provider,
    TextSource,
    Proofread,
    Summary,
    Vision,
    Translate,
    KeepVideo,
    Formats,
}

/// The single current input form. Its serialized type/field names are retained
/// so existing workspaces can be upgraded without changing task records.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Draft {
    pub id: String,
    pub revision: u64,
    pub online: bool,
    pub input: String,
    /// Subtitle events live in a separate file beside the workspace record.
    #[serde(default, serialize_with = "serialize_draft_source")]
    pub source: Option<crate::source::Source>,
    pub title: String,
    pub custom_title: bool,
    pub library_id: String,
    pub folder: Option<u64>,
    pub options: ConversionOptions,
    /// Remember the local branch when temporarily choosing a speech service.
    #[serde(default)]
    #[serde(with = "crate::provider_serde")]
    pub local_provider: Option<course2md::config::AsrProvider>,
    #[serde(default)]
    pub overrides: BTreeSet<Override>,
    #[serde(default)]
    pub subtitle: Option<PathBuf>,
    #[serde(default)]
    pub asr_service: Option<String>,
    #[serde(default)]
    pub ai_service: Option<String>,
    #[serde(default)]
    pub retry_of: Option<String>,
    #[serde(default, serialize_with = "serialize_optional_public_config")]
    pub base_config: Option<ConfigFile>,
    #[serde(default)]
    pub submitted_task: Option<String>,
    #[serde(default)]
    pub scroll: f32,
    pub updated: u64,
}

impl Draft {
    pub fn new(online: bool, library_id: String, defaults: ConversionOptions) -> Self {
        Self {
            id: new_id("input"),
            revision: 0,
            online,
            input: String::new(),
            source: None,
            title: String::new(),
            custom_title: false,
            library_id,
            folder: None,
            local_provider: (!defaults.uses_cloud_provider())
                .then_some(defaults.provider)
                .flatten(),
            options: defaults,
            overrides: BTreeSet::new(),
            subtitle: None,
            asr_service: None,
            ai_service: None,
            retry_of: None,
            base_config: None,
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
        self.submitted_task = None;
        self.updated = now();
        if !self.custom_title {
            self.title.clear();
        }
    }

    pub fn inherit(&mut self, defaults: &ConversionOptions) {
        let v = &mut self.options;
        if !self.overrides.contains(&Override::Provider) {
            v.provider = defaults.provider;
        }
        if !self.overrides.contains(&Override::TextSource) {
            v.source_mode = defaults.source_mode;
        }
        if !self.overrides.contains(&Override::Proofread) {
            v.llm = defaults.llm;
        }
        if !self.overrides.contains(&Override::Summary) {
            v.summarize = defaults.summarize;
        }
        if !self.overrides.contains(&Override::Vision) {
            v.vision = defaults.vision;
        }
        if !self.overrides.contains(&Override::Translate) {
            v.translate = defaults.translate.clone();
        }
        if !self.overrides.contains(&Override::KeepVideo) {
            v.keep_video = defaults.keep_video;
        }
        if !self.overrides.contains(&Override::Formats) {
            v.formats = defaults.formats;
        }
    }

    /// Resume inheriting AI processing choices without changing other task choices.
    pub fn reset_ai_overrides(&mut self, defaults: &ConversionOptions) {
        self.options.llm = defaults.llm;
        self.options.summarize = defaults.summarize;
        self.options.vision = defaults.vision;
        self.options.translate = defaults.translate.clone();
        for field in [
            Override::Proofread,
            Override::Summary,
            Override::Vision,
            Override::Translate,
        ] {
            self.overrides.remove(&field);
        }
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
        if let Some(config) = &mut self.base_config {
            relocate_config(config, old, new);
        }
    }
}

impl crate::source::Source {
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
            course2md::subtitle::SubtitleEvidence::Found { tracks, .. } => tracks.iter_mut(),
            _ => [].iter_mut(),
        };
        for track in tracks.chain(&mut self.subtitle_request) {
            if let course2md::subtitle::SubtitleOrigin::File { path } = &mut track.origin {
                relocate_path(path, old, new);
            }
        }
    }
}

pub fn relocate_path(path: &mut PathBuf, old: &Path, new: &Path) {
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
    source: &Option<crate::source::Source>,
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
fn serialize_optional_public_config<S: serde::Serializer>(
    config: &Option<ConfigFile>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    config.as_ref().map(public_config).serialize(serializer)
}

pub fn public_config(config: &ConfigFile) -> ConfigFile {
    let mut config = config.clone();
    config.asr_api.api_key.clear();
    config.llm.api_key.clear();
    config
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskPlan {
    #[serde(default)]
    pub operation: course2md::execution::Operation,
    pub source: crate::source::Source,
    pub source_id: String,
    pub title: String,
    pub library_id: String,
    pub folder: Option<u64>,
    pub subtitle: Option<PathBuf>,
    #[serde(serialize_with = "serialize_public_config")]
    pub config: ConfigFile,
    pub asr_service: Option<String>,
    pub ai_service: Option<String>,
}

impl TaskPlan {
    /// The processing choices this plan's configuration carries. Records
    /// written before this was derived also stored an `options` copy, which
    /// loading now ignores.
    pub fn options(&self) -> ConversionOptions {
        ConversionOptions::from_config(&self.config)
    }
    pub fn same_work(&self, other: &Self) -> bool {
        if self.operation != other.operation {
            return false;
        }
        if self.library_id != other.library_id
            || self.source_id != other.source_id
            || self.subtitle != other.subtitle
            || self.asr_service != other.asr_service
            || self.ai_service != other.ai_service
        {
            return false;
        }
        let mut left = public_config(&self.config);
        let mut right = public_config(&other.config);
        // Display and organization are not processing parameters.
        left.defaults.out = None;
        right.defaults.out = None;
        left.desktop = Default::default();
        right.desktop = Default::default();
        left == right
    }
}

pub use course2md::dispatch::Intent;

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
            Self::Queued => course2md::tr!("等待处理"),
            Self::Running => course2md::tr!("正在生成笔记"),
            Self::Pausing => course2md::tr!("正在保存进度"),
            Self::Paused => course2md::tr!("已暂停"),
            Self::NeedsAttention => course2md::tr!("需要处理"),
            Self::Uncertain => course2md::tr!("结果尚未确认"),
            Self::Complete => course2md::tr!("笔记已生成"),
            Self::Partial => course2md::tr!("部分未完成"),
            Self::Cancelled => course2md::tr!("已取消"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
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
    /// The later attempt that replaced this one: the task whose `parent` this is.
    #[serde(skip)]
    pub handled_by: Option<String>,
    /// Always `<library>/.course2md/work/<id>`.
    #[serde(skip)]
    pub work_dir: PathBuf,
    #[serde(flatten)]
    pub events: JobEvents,
}

impl TaskRecord {
    /// One engine event from the running job.
    pub fn apply_event(&mut self, event: &course2md::progress::Event) -> bool {
        use course2md::progress::Event;
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
            course2md::tr!("正在取消")
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

    fn relocate(&mut self, old: &Path, new: &Path) {
        relocate_path(&mut self.work_dir, old, new);
        if let Some(path) = &mut self.artifact {
            relocate_path(path, old, new);
        }
        if let Some(path) = &mut self.plan.subtitle {
            relocate_path(path, old, new);
        }
        self.plan.source.relocate(old, new);
        relocate_config(&mut self.plan.config, old, new);
        if let course2md::execution::Operation::Reprocess {
            base_version_dir,
            prior_work_dir,
            ..
        } = &mut self.plan.operation
        {
            relocate_path(base_version_dir, old, new);
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
            self.events.error = Some(course2md::tr!("任务记录已重建").into());
            self.unread = true;
        }
    }

    pub fn exports_only(&self) -> bool {
        matches!(
            &self.plan.operation,
            course2md::execution::Operation::Reprocess { components, .. }
                if !components.is_empty() && components.iter().all(|part| *part == Component::Exports)
        )
    }
}

/// Export publication has two destinations. Derive them from the frozen task
/// and current library location, so moving a library needs no new path record.
fn task_export_directory(task: &TaskRecord, location: &LibraryLocation) -> Option<PathBuf> {
    if task.plan.library_id != location.id {
        return None;
    }
    if task.exports_only() {
        let course = crate::workspace::course_dir_name(&task.plan.source_id);
        let course2md::execution::Operation::Reprocess {
            base_version_dir, ..
        } = &task.plan.operation
        else {
            return None;
        };
        Some(
            location
                .root
                .join(course)
                .join("exports")
                .join(base_version_dir.file_name()?)
                .join(&task.id),
        )
    } else {
        task.artifact
            .as_ref()
            .map(|version| version.join("exports"))
    }
}

/// No filesystem access: rendering can offer the exact successful outputs from
/// the durable outcome record. Existence is checked when the user opens them.
pub(crate) fn task_export_files(task: &TaskRecord, location: &LibraryLocation) -> Vec<PathBuf> {
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
    use course2md::config::OutputFormat;
    [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json]
        .into_iter()
        .filter(|format| {
            outcomes
                .exports
                .get(&format.to_string())
                .is_some_and(|outcome| outcome.status == course2md::artifact::Status::Succeeded)
        })
        .map(|format| directory.join(course2md::portable::file_name(format)))
        .collect()
}

pub(crate) fn available_task_export(
    task: &TaskRecord,
    location: &LibraryLocation,
) -> Result<PathBuf> {
    task_export_files(task, location)
        .into_iter()
        .find(|path| path.is_file())
        .context(course2md::tr!(
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
}

impl State {
    fn initial(root: PathBuf, options: ConversionOptions) -> Self {
        let library = LibraryLocation {
            id: new_id("library"),
            name: course2md::tr!("笔记库").into(),
            root,
            previous_roots: Vec::new(),
        };
        let draft = Draft::new(true, library.id.clone(), options);
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
        }
    }

    /// A new record over a folder that is already a library takes on its identity.
    fn adopt_marker(&mut self, root: &Path) -> bool {
        let Ok(marker) = std::fs::read_to_string(root.join(".course2md-library-id")) else {
            return false;
        };
        if course2md::execution::valid_id(marker.trim()) {
            self.default_library = marker.trim().into();
            self.libraries[0].id = self.default_library.clone();
            self.drafts[0].library_id = self.default_library.clone();
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
    pub fn next_task(&self) -> Option<&TaskRecord> {
        if self.reconciling {
            return None;
        }
        self.tasks.iter().find(|t| {
            t.state == TaskState::Queued && t.intent == Intent::Run && t.handled_by.is_none()
        })
    }

    /// Replace the current input; submitted task plans stay frozen.
    pub fn reset_input(
        &mut self,
        online: bool,
        options: ConversionOptions,
        destination: Option<(String, u64)>,
    ) -> String {
        let mut draft = Draft::new(online, self.default_library.clone(), options);
        if let Some((library, folder)) = destination {
            draft.library_id = library;
            draft.folder = Some(folder);
        }
        self.replace_input(draft);
        self.current_draft.clone()
    }

    pub fn switch_source_kind(&mut self, online: bool, options: ConversionOptions) {
        let Some(previous) = self.draft() else {
            self.reset_input(online, options, None);
            return;
        };
        if previous.online == online {
            return;
        }
        if previous.submitted_task.is_some() {
            let mut input = Draft::new(online, self.default_library.clone(), options);
            if previous.library_id == input.library_id {
                input.folder = previous.folder;
            }
            self.replace_input(input);
            return;
        }
        // Source modes belong to the same form. Keep common choices, clear the
        // incompatible source and detach any submitted/retry task association.
        let mut input = Draft::new(
            online,
            previous.library_id.clone(),
            previous.options.clone(),
        );
        input.folder = previous.folder;
        input.overrides = previous.overrides.clone();
        input.asr_service = previous.asr_service.clone();
        input.ai_service = previous.ai_service.clone();
        input.base_config = previous.base_config.clone();
        input.local_provider = previous.local_provider;
        self.replace_input(input);
    }

    /// Replacing a submitted source starts another conversion with saved defaults.
    /// Edits to an unsubmitted source keep the user's in-progress choices.
    pub fn prepare_next_import(&mut self, value: &str, defaults: ConversionOptions) -> bool {
        let Some(previous) = self.draft() else {
            return false;
        };
        if previous.submitted_task.is_none() || previous.input == value {
            return false;
        }
        let submitted = previous.submitted_task.clone();
        let mut input = Draft::new(previous.online, self.default_library.clone(), defaults);
        if previous.library_id == input.library_id {
            input.folder = previous.folder;
        }
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

    /// Replace the current form with a task's options without mutating that task.
    /// `subtitle_events` comes from `task_subtitle_events`; without it the form
    /// asks for the subtitle to be read again.
    pub fn adjust_task(
        &mut self,
        id: &str,
        subtitle_events: Option<Vec<course2md::timeline::TranscriptEvent>>,
    ) -> Result<()> {
        let task = self.task(id).context(course2md::tr!("任务不存在"))?.clone();
        ensure!(
            task.handled_by.is_none(),
            course2md::tr!("此任务已有后续处理，请打开对应任务继续")
        );
        ensure!(
            !matches!(
                task.state,
                TaskState::Running | TaskState::Pausing | TaskState::Queued
            ),
            course2md::tr!("当前任务还在处理，请先暂停并等候当前步骤结束")
        );
        let mut draft = Draft::new(
            task.plan.source.online,
            task.plan.library_id.clone(),
            task.plan.options(),
        );
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
        draft.title = task.plan.title;
        draft.custom_title = true;
        draft.folder = task.plan.folder;
        draft.subtitle = task.plan.subtitle;
        draft.asr_service = task.plan.asr_service;
        draft.ai_service = task.plan.ai_service;
        draft.retry_of = Some(id.to_owned());
        draft.overrides = [
            Override::Provider,
            Override::TextSource,
            Override::Proofread,
            Override::Summary,
            Override::Vision,
            Override::KeepVideo,
            Override::Formats,
        ]
        .into_iter()
        .collect();
        draft.base_config = Some(task.plan.config);
        self.replace_input(draft);
        Ok(())
    }

    pub fn enqueue(&mut self, plan: TaskPlan, parent: Option<String>) -> Result<(String, bool)> {
        ensure!(
            !plan.source_id.is_empty() && !plan.title.trim().is_empty(),
            course2md::tr!("请先确认视频和笔记名称")
        );
        if let Some(id) = parent.as_deref() {
            let original = self.task(id).context(course2md::tr!("原任务记录不存在"))?;
            if let Some(followup) = &original.handled_by {
                ensure!(
                    self.task(followup).is_some(),
                    course2md::tr!("后续任务记录缺失，请先恢复任务记录")
                );
                return Ok((followup.clone(), false));
            }
            ensure!(
                !matches!(
                    original.state,
                    TaskState::Running | TaskState::Pausing | TaskState::Queued
                ),
                course2md::tr!("原任务仍在处理，请先暂停并等候当前步骤结束")
            );
        }
        let location = self
            .library(&plan.library_id)
            .context(course2md::tr!("所选保存位置未登记"))?;
        if let Some(existing) = self
            .tasks
            .iter()
            .find(|t| !t.state.finished() && t.handled_by.is_none() && t.plan.same_work(&plan))
        {
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
            work_dir,
            artifact: None,
            unread: false,
            resend: Vec::new(),
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
        let task = self
            .task_mut(id)
            .context(course2md::tr!("任务记录不存在"))?;
        ensure!(
            !task.state.finished(),
            course2md::tr!("这项任务已结束，可在任务卡中补做或生成新版")
        );
        ensure!(
            task.handled_by.is_none(),
            course2md::tr!("这项任务已有后续处理，请打开对应任务继续。")
        );
        ensure!(
            intent != Intent::Run || !matches!(task.state, TaskState::Running | TaskState::Pausing),
            course2md::tr!("这项任务正在处理当前操作")
        );
        ensure!(
            intent != Intent::Run || !task.has_uncertain(),
            course2md::tr!("仍有请求结果尚未确认，请查看请求范围后选择是否重新发送")
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
            .context(course2md::tr!("任务记录不存在"))?;
        ensure!(
            self.tasks[from].waiting(),
            course2md::tr!("只能调整排队中的任务")
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
        let task = self
            .task_mut(id)
            .context(course2md::tr!("任务记录不存在"))?;
        ensure!(
            task.handled_by.is_none(),
            course2md::tr!("这项任务已有后续处理，请打开对应任务继续")
        );
        ensure!(
            !matches!(
                task.state,
                TaskState::Running | TaskState::Pausing | TaskState::Queued
            ) && !task.state.finished(),
            course2md::tr!("这项任务的状态已变化，请查看最新状态")
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
            course2md::tr!("待确认的请求已变化，请查看最新范围后再选择是否重新发送")
        );
        ensure!(
            selected.iter().all(|id| !task.resend.contains(id)),
            course2md::tr!("这些请求已获得重新发送授权，请等待对应任务处理")
        );
        task.resend.extend(requests.iter().cloned());
        task.intent = Intent::Run;
        task.state = TaskState::Queued;
        task.events.error = None;
        task.updated = now();
        Ok(())
    }

    pub fn reprocess(
        &mut self,
        id: &str,
        components: Vec<Component>,
        resend: Vec<String>,
    ) -> Result<String> {
        self.reprocess_with_service(id, components, resend, None)
    }

    /// Repair creates a new attempt with an explicitly chosen service. The
    /// original task and the unrelated current input remain unchanged.
    pub fn reprocess_with_service(
        &mut self,
        id: &str,
        mut components: Vec<Component>,
        resend: Vec<String>,
        ai_service: Option<(String, ConfigFile)>,
    ) -> Result<String> {
        let mut original = self.task(id).context(course2md::tr!("任务不存在"))?.clone();
        if let Some(next) = &original.handled_by {
            ensure!(
                self.task(next).is_some(),
                course2md::tr!("后续任务记录缺失，请先恢复任务记录")
            );
            return Ok(next.clone());
        }
        ensure!(
            !matches!(
                original.state,
                TaskState::Running | TaskState::Pausing | TaskState::Queued
            ),
            course2md::tr!("当前任务仍在处理，请先等候当前步骤结束")
        );
        let base = original
            .artifact
            .clone()
            .context(course2md::tr!("此任务尚无可补做的笔记正文"))?;
        let manifest = course2md::artifact::read_manifest(&base.join("manifest.json"))?;
        ensure!(
            manifest.source_id == original.plan.source_id,
            course2md::tr!("笔记来源与任务不匹配")
        );
        let outcomes = original.events.outcomes.clone();
        components.sort();
        components.dedup();
        ensure!(
            !components.is_empty(),
            course2md::tr!("请选择需要补做的内容")
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
            ensure!(failed, course2md::tr!("这部分无需补做"));
        }
        if !resend.is_empty() {
            reconcile_receipts(&mut original)?;
            let unknown: BTreeSet<_> = original
                .events
                .blocked
                .iter()
                .filter(|request| request.is_uncertain())
                .filter_map(|request| request.request_id.as_ref())
                .collect();
            ensure!(
                !unknown.is_empty() && unknown == resend.iter().collect(),
                course2md::tr!("待确认请求已变化，请查看最新范围后再选择是否重新发送")
            );
            // The user is continuing the original task, including a requested
            // summary that the uncertain proofreading prevented from starting.
            // Any summary receipt means it has its own attempt to recover; do
            // not infer permission to repeat it from a proofreading decision.
            let summary = Component::Summary;
            if components.contains(&Component::Proofreading)
                && !components.contains(&Component::Summary)
                && original.plan.config.llm.summarize
                && outcomes
                    .as_ref()
                    .is_some_and(|outcomes| failed_outcome(&outcomes.summary))
                && original.events.blocked.iter().any(|request| {
                    request.is_uncertain()
                        && request.purpose.as_deref() == Some(Component::Proofreading.as_str())
                })
                && !course2md::dispatch::receipts(&original.work_dir)?
                    .iter()
                    .any(|receipt| receipt.purpose == summary.as_str())
            {
                components.push(Component::Summary);
                components.sort();
            }
        } else {
            ensure!(
                !original.has_uncertain(),
                course2md::tr!("仍有请求结果尚未确认，请先查看请求范围")
            );
        }
        let mut plan = original.plan;
        if let Some((service, config)) = ai_service {
            ensure!(
                components
                    .iter()
                    .all(|part| matches!(part, Component::Proofreading | Component::Summary)),
                course2md::tr!("服务修复只能用于未完成的校对或摘要")
            );
            plan.ai_service = Some(service);
            plan.config.llm = config.llm;
        }
        let only_exports = components
            .iter()
            .all(|component| *component == Component::Exports);
        plan.config.llm.enabled = components.contains(&Component::Proofreading);
        plan.config.llm.summarize = components.contains(&Component::Summary);
        plan.config.llm.vision &= plan.config.llm.enabled;
        if only_exports {
            use course2md::config::OutputFormat;
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
                course2md::tr!("需要补做的导出格式记录不完整，请从笔记中重新选择导出格式")
            );
            plan.config.defaults.formats = Some(formats);
        }
        plan.operation = course2md::execution::Operation::Reprocess {
            base_version_dir: base,
            components,
            prior_work_dir: Some(original.work_dir),
        };
        plan.config.defaults.transcript_source =
            Some(course2md::config::TranscriptSource::Subtitle);
        let (next, created) = self.enqueue(plan, Some(id.to_owned()))?;
        if created && let Some(task) = self.task_mut(&next) {
            task.resend = resend;
        }
        Ok(next)
    }

    /// Points a library at a new folder and rewrites every saved path inside the old
    /// ones. Moving, locating and restoring a library all come through here; keeping
    /// its id keeps folder ids, reading positions and service bindings stable.
    pub fn rebind_library(
        &mut self,
        library_id: &str,
        old_roots: &[PathBuf],
        destination: &Path,
    ) -> Result<()> {
        ensure!(
            self.library(library_id).is_some(),
            course2md::tr!("这个笔记库已被移除")
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
            .context(course2md::tr!("这个笔记库已被移除"))?;
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
}

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
pub(crate) struct LibraryWrites {
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
        course2md::runtime::lock_recover(&self.pending).insert(path, (bytes, mirror_of));
        let this = self.clone();
        crate::spawn_blocking_io(move || this.drain());
    }

    /// Performs everything queued so far, waiting for any write already running.
    pub(crate) fn drain(&self) {
        let _writing = course2md::runtime::lock_recover(&self.writing);
        let writes = std::mem::take(&mut *course2md::runtime::lock_recover(&self.pending));
        for (path, (bytes, mirror_of)) in writes {
            let control = mirror_of.is_none();
            let result = match mirror_of {
                Some(location) => write_mirror(&path, &bytes, &location),
                None => write_control(&path, &bytes),
            };
            if let Err(error) = result {
                *course2md::runtime::lock_recover(&self.failure) = Some(WriteFailure {
                    message: format!("{error:#}"),
                    control,
                });
                if let Some(wake) = course2md::runtime::lock_recover(&self.wake).as_ref() {
                    let _ = wake.try_send(());
                }
            }
        }
    }

    /// Writes a task's control file now, for a job about to start.
    pub(crate) fn write_control_now(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let _writing = course2md::runtime::lock_recover(&self.writing);
        write_control(path, bytes)
    }
}

fn write_control(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .context(course2md::tr!("任务记录缺少保存目录"))?,
    )?;
    course2md::checkpoint::atomic_write(path, bytes)
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
        course2md::checkpoint::atomic_write(path, bytes)
            .context(course2md::tr!("任务恢复副本未能保存，原任务记录仍保留"))?;
    }
    Ok(())
}

fn validate_record_location(task: &TaskRecord, libraries: &[LibraryLocation]) -> Result<()> {
    ensure!(
        course2md::execution::valid_id(&task.id),
        course2md::tr!("任务身份无效")
    );
    let library = libraries
        .iter()
        .find(|library| library.id == task.plan.library_id)
        .context(course2md::tr!("任务保存位置记录缺失"))?;
    ensure!(
        library.root.is_absolute(),
        course2md::tr!("任务工作目录与记录的笔记库不匹配")
    );
    ensure!(
        !task.plan.source_id.trim().is_empty() && !task.plan.title.trim().is_empty(),
        course2md::tr!("任务来源身份或名称记录缺失")
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
                course2md::tr!("任务工作目录被其他文件或链接占用，原内容已保留")
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
        let bytes = crate::store::read(&path)?;
        course2md::checkpoint::atomic_write(
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
    course2md::checkpoint::atomic_write(&path, &serde_json::to_vec(&control)?)
}

fn failed_outcome(outcome: &course2md::artifact::Outcome) -> bool {
    matches!(
        outcome.status,
        course2md::artifact::Status::Failed | course2md::artifact::Status::Partial
    )
}

fn component_outcome(
    outcomes: &course2md::artifact::Outcomes,
    component: Component,
) -> &course2md::artifact::Outcome {
    match component {
        Component::Screenshots => &outcomes.screenshots,
        Component::Proofreading => &outcomes.proofreading,
        Component::Summary => &outcomes.summary,
        Component::Exports => &outcomes.transcript,
    }
}

pub fn check_library(location: &LibraryLocation) -> Result<()> {
    ensure!(
        location.root.is_dir(),
        course2md::tr!("保存位置不可访问，请连接对应磁盘")
    );
    let identity = std::fs::read_to_string(location.root.join(".course2md-library-id"))
        .context(course2md::tr!("保存位置需要重新关联"))?;
    ensure!(
        identity.trim() == location.id,
        course2md::tr!("保存位置对应其他笔记库，请恢复原位置或在存储设置中重新登记")
    );
    Ok(())
}

pub(crate) fn create_library_marker(location: &LibraryLocation) -> Result<()> {
    use std::io::Write;
    ensure!(
        location.root.is_dir(),
        course2md::tr!("保存位置暂时不可访问")
    );
    let target = location.root.join(".course2md-library-id");
    if target.exists() {
        return check_library(location);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&location.root)?;
    temporary.write_all(location.id.as_bytes())?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&target) {
        Ok(_) => course2md::artifact::sync_dir(&location.root)?,
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
    Published {
        version: PathBuf,
        outcomes: Box<course2md::artifact::Outcomes>,
        partial: bool,
    },
    Exports {
        base: PathBuf,
        outcomes: BTreeMap<String, course2md::artifact::Outcome>,
    },
}

/// The published result of a task, if any. Reads and hashes files; the
/// result is applied by `apply_artifact`.
fn probe_artifact(task: &TaskRecord, location: &LibraryLocation) -> Result<ArtifactFact> {
    if task.artifact.is_some() && !task.exports_only() {
        return Ok(ArtifactFact::Nothing);
    }
    let course = course_dir_name(&task.plan.source_id);
    let version = location.root.join(&course).join("versions").join(&task.id);
    if version.join("manifest.json").is_file() {
        let manifest = course2md::artifact::read_manifest(&version.join("manifest.json"))?;
        ensure!(
            manifest.task_id == task.id && manifest.source_id == task.plan.source_id,
            course2md::tr!("已发布笔记与任务身份不匹配，原文件已保留")
        );
        course2md::artifact::validate_version(&version, &manifest)?;
        return Ok(ArtifactFact::Published {
            version,
            outcomes: Box::new(manifest.outcomes),
            partial: manifest.partial,
        });
    }
    if let course2md::execution::Operation::Reprocess {
        base_version_dir,
        components,
        ..
    } = &task.plan.operation
        && components
            .iter()
            .all(|component| *component == Component::Exports)
        && task.work_dir.join("export-result.json").is_file()
    {
        #[derive(serde::Deserialize)]
        struct ExportResult {
            #[serde(default)]
            outcomes: BTreeMap<String, course2md::artifact::Outcome>,
        }
        let mut record: ExportResult =
            serde_json::from_slice(&std::fs::read(task.work_dir.join("export-result.json"))?)?;
        ensure!(
            !record.outcomes.is_empty(),
            course2md::tr!("导出结果记录为空")
        );
        let export_directory = task_export_directory(task, location)
            .context(course2md::tr!("导出位置与任务记录不匹配"))?;
        for (format, outcome) in &mut record.outcomes {
            if outcome.status == course2md::artifact::Status::Succeeded {
                let Some(format) = output_format(format) else {
                    continue;
                };
                let expected = export_directory.join(course2md::portable::file_name(format));
                if !expected.is_file() {
                    *outcome = course2md::artifact::Outcome::failed(course2md::tr!(
                        "导出文件已移动或无法读取，可以重新导出"
                    ));
                }
            }
        }
        return Ok(ArtifactFact::Exports {
            base: base_version_dir.clone(),
            outcomes: record.outcomes,
        });
    }
    Ok(ArtifactFact::Nothing)
}

fn apply_artifact(task: &mut TaskRecord, fact: ArtifactFact) {
    match fact {
        ArtifactFact::Nothing => {}
        // A result recorded meanwhile is newer than what startup found.
        ArtifactFact::Published { .. } if task.artifact.is_some() && !task.exports_only() => {}
        ArtifactFact::Published {
            version,
            outcomes,
            partial,
        } => {
            task.artifact = Some(version);
            task.events.outcomes = Some(*outcomes);
            task.state = if partial {
                TaskState::Partial
            } else {
                TaskState::Complete
            };
            task.events.error = None;
            task.unread = true;
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
                .get_or_insert_with(course2md::artifact::Outcomes::default)
                .exports = outcomes;
            task.unread |= previous_artifact != task.artifact || task.state != next_state;
            task.state = next_state;
        }
    }
}

/// The output format a saved export outcome is keyed by.
fn output_format(key: &str) -> Option<course2md::config::OutputFormat> {
    key.parse().ok()
}

/// Requests whose result is unknown, read from the task's receipts.
fn uncertain_requests(task: &TaskRecord) -> Result<Vec<BlockedRequest>> {
    let receipts = course2md::dispatch::receipts(&task.work_dir)?;
    Ok(receipts
        .into_iter()
        .filter(|r| {
            matches!(
                r.state,
                course2md::dispatch::State::Sending | course2md::dispatch::State::Uncertain
            )
        })
        .map(|receipt| BlockedRequest {
            reason: BlockReason::Uncertain,
            request_id: Some(receipt.request_id),
            purpose: Some(receipt.purpose),
            description: receipt.description.clone(),
            kind: Some(course2md::artifact::FailureKind::Uncertain),
            message: if receipt.description.trim().is_empty() {
                receipt
                    .message
                    .unwrap_or_else(|| course2md::tr!("请求已发送，结果未确认").into())
            } else {
                course2md::tr!(
                    "{}：{}",
                    receipt.description,
                    receipt
                        .message
                        .unwrap_or_else(|| { course2md::tr!("请求结果未确认").into() })
                )
            },
        })
        .collect())
}

fn apply_receipts(task: &mut TaskRecord, uncertain: Vec<BlockedRequest>) {
    task.events
        .blocked
        .retain(|request| !request.is_uncertain());
    task.events.blocked.extend(uncertain);
    if task.has_uncertain() && task.intent == Intent::Run {
        task.state = TaskState::Uncertain;
        task.unread = true;
    }
}

pub fn reconcile_receipts(task: &mut TaskRecord) -> Result<()> {
    let uncertain = uncertain_requests(task)?;
    apply_receipts(task, uncertain);
    Ok(())
}

/// What a task said before streams were saved, kept in the mirror of that time.
const OLD_MIRROR_FILE: &str = "task-record-v1.json";

/// The events a task saved, read back: what an older mirror held, then its stream.
fn read_task_events(task: &TaskRecord) -> Result<JobEvents> {
    let old_mirror: Option<TaskMirror> = crate::store::read(&task.work_dir.join(OLD_MIRROR_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let mut events = old_mirror.map_or_else(JobEvents::default, |mirror| mirror.task.events);
    match crate::store::read(&task.work_dir.join(TASK_EVENTS_FILE)) {
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

/// A mirror written before streams were saved is the only place left for what the task
/// said, once the record has been rewritten: it moves aside rather than be overwritten.
fn keep_old_mirror(directory: &Path) -> Result<()> {
    let path = directory.join("task-record.json");
    let Ok(bytes) = crate::store::read(&path) else {
        return Ok(());
    };
    #[derive(Deserialize)]
    struct Schema {
        schema: u32,
    }
    if serde_json::from_slice::<Schema>(&bytes).is_ok_and(|mirror| mirror.schema == 1) {
        std::fs::rename(&path, directory.join(OLD_MIRROR_FILE))?;
    }
    Ok(())
}

/// What the startup check found on disk for one task.
enum TaskFacts {
    /// The library is not reachable; the task keeps what it has.
    Unavailable,
    Found {
        artifact: ArtifactFact,
        uncertain: Vec<BlockedRequest>,
        events: Box<JobEvents>,
        outcomes: Option<Box<course2md::artifact::Outcomes>>,
    },
    Unreadable(String),
}

impl TaskFacts {
    fn apply(self, task: &mut TaskRecord) {
        match self {
            Self::Unavailable => {}
            Self::Found {
                artifact,
                uncertain,
                events,
                outcomes,
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
                task.events.error = Some(course2md::tr!(
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
}

/// The findings of a [`Reconciliation`], applied by `Workspace::apply_reconciliation`.
pub struct Reconciled {
    workspace: u64,
    facts: Vec<(String, TaskFacts)>,
}

fn reconcile_task(task: &TaskRecord, location: &LibraryLocation) -> Result<TaskFacts> {
    keep_old_mirror(&task.work_dir)?;
    let artifact = probe_artifact(task, location)?;
    let uncertain = uncertain_requests(task)?;
    let events = read_task_events(task)?;
    store_task_subtitles(task)?;
    // A task that stopped before reporting its result still has the published note's.
    let outcomes = task
        .artifact
        .as_ref()
        .filter(|_| events.outcomes.is_none() && !task.exports_only())
        .and_then(|version| course2md::artifact::read_manifest(&version.join("manifest.json")).ok())
        .map(|manifest| Box::new(manifest.outcomes));
    // Catches up on writes made while disconnected.
    write_mirror(
        &task.work_dir.join("task-record.json"),
        &mirror_bytes(task, location)?,
        location,
    )?;
    Ok(TaskFacts::Found {
        artifact,
        uncertain,
        events: Box::new(events),
        outcomes,
    })
}

impl Reconciliation {
    /// Blocking: run through `crate::spawn_blocking_io`.
    pub fn run(self) -> Reconciled {
        Reconciled {
            workspace: self.workspace,
            facts: self
                .tasks
                .into_iter()
                .map(|(task, location)| {
                    let facts = match location.filter(|location| check_library(location).is_ok()) {
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
        course2md::checkpoint::atomic_write(
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
        let restored = crate::store::read(&snapshot).and_then(|bytes| {
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            let mirror: TaskMirror = serde_json::from_value(value.clone())?;
            ensure!(
                mirror.schema <= SCHEMA
                    && course2md::execution::valid_id(&mirror.task.id)
                    && entry.file_name().to_str() == Some(mirror.task.id.as_str()),
                course2md::tr!("任务镜像身份不匹配")
            );
            let mut task = mirror.task;
            // A mirror older than `root` names its folder in its own work directory.
            let root = mirror.root.or_else(|| {
                Path::new(value["task"]["work_dir"].as_str()?)
                    .ancestors()
                    .nth(3)
                    .map(Path::to_path_buf)
            });
            if let Some(root) = root {
                task.relocate(&root, &location.root);
            }
            task.plan.library_id = location.id.clone();
            task.work_dir = entry.path();
            Ok(task)
        });
        match restored {
            Ok(task) => tasks.push(task),
            Err(_) => issues.push(course2md::tr!(
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
) -> Result<Option<Vec<course2md::timeline::TranscriptEvent>>> {
    let Some(subtitle) = &task.plan.source.selected_subtitle else {
        return Ok(None);
    };
    if !subtitle.events.is_empty() {
        return Ok(Some(subtitle.events.clone()));
    }
    let path = task.work_dir.join(TASK_SUBTITLE_FILE);
    if path.is_file() {
        return Ok(Some(serde_json::from_slice(&crate::store::read(&path)?)?));
    }
    ensure!(
        matches!(
            task.plan.operation,
            course2md::execution::Operation::Reprocess { .. }
        ),
        course2md::tr!("所选字幕的内容已丢失，请重新读取字幕后再试")
    );
    Ok(None)
}

fn draft_subtitle_path(record: &Path) -> PathBuf {
    record.with_extension("subtitle.json")
}

/// Restore the draft's subtitle events from the file beside the record. A
/// missing or mismatched file leaves the subtitle to be read again.
fn hydrate_draft_subtitles(state: &mut State, record: &Path) {
    let stored = crate::store::read(&draft_subtitle_path(record))
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<course2md::subtitle::CachedSubtitle>(&bytes).ok()
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
        course2md::checkpoint::atomic_write(
            &draft_subtitle_path(path),
            &serde_json::to_vec(&selected)?,
        )?;
    }
    crate::store::save(path, &serde_json::to_vec(next)?)
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
    pub fn rebuild(root: PathBuf, options: ConversionOptions) -> Result<Self> {
        Self::rebuild_at(
            course2md::config::config_dir().join("desktop-workspace.json"),
            root,
            options,
        )
    }

    pub fn rebuild_at(path: PathBuf, root: PathBuf, options: ConversionOptions) -> Result<Self> {
        let parent = path
            .parent()
            .context(course2md::tr!("任务记录缺少保存目录"))?;
        std::fs::create_dir_all(parent)?;
        let backup = path.with_extension("json.bak");
        let originals: Vec<_> = [&path, &backup]
            .into_iter()
            .filter(|p| p.exists())
            .map(|p| Ok((p.clone(), crate::store::read(p)?)))
            .collect::<Result<_>>()?;
        ensure!(
            !originals.is_empty(),
            course2md::tr!("原任务记录不存在，请重新打开应用建立新记录")
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
            course2md::tr!("这些任务由较新版本创建，请使用对应版本恢复，原文件已保留")
        );
        let archive = parent.join(new_id("workspace-recovery"));
        std::fs::create_dir(&archive).context(course2md::tr!("无法建立原始记录备份，尚未重建"))?;
        for (original, bytes) in &originals {
            let name = original
                .file_name()
                .context(course2md::tr!("原记录缺少文件名"))?;
            course2md::checkpoint::atomic_write(&archive.join(name), bytes)
                .context(course2md::tr!("原始记录备份未完成，尚未重建"))?;
        }
        let mut initial = State::initial(root.clone(), options);
        initial.adopt_marker(&root);
        let mut state = Self::read(&path)
            .or_else(|_| Self::read(&backup))
            .unwrap_or_else(|_| initial.clone());
        // A record that no longer reads as a whole can still name what it knew.
        for location in salvage::<LibraryLocation>(&records, "libraries") {
            if course2md::execution::valid_id(&location.id)
                && location.root.is_absolute()
                && !state
                    .libraries
                    .iter()
                    .any(|l| l.id == location.id || l.root == location.root)
            {
                state.libraries.push(location);
            }
        }
        if !state
            .libraries
            .iter()
            .any(|l| l.root == root || l.id == initial.default_library)
        {
            state.libraries.append(&mut initial.libraries);
        }
        let mut issues = Vec::new();
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
                issues.push(course2md::tr!(
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
                pause_recovered_control(task).context(course2md::tr!(
                    "无法保存恢复后的暂停指令，尚未替换原任务记录"
                ))?;
            }
        }
        state.retain_current_input();
        state.recover();
        hydrate_draft_subtitles(&mut state, &path);
        state.reconciling = !state.tasks.is_empty();
        course2md::checkpoint::atomic_write(&path, &serde_json::to_vec(&state)?)?;
        let recovery = course2md::tr!(
            "已重建 {} 项任务记录，未启动处理。原始记录保存在 {}。{}",
            state.tasks.len(),
            archive.display(),
            if issues.is_empty() {
                String::new()
            } else {
                issues.join(course2md::tr!("；"))
            }
        );
        Ok(Self::assemble(state, Some(recovery), path))
    }

    fn assemble(state: State, recovery: Option<String>, path: PathBuf) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Self {
            state,
            recovery,
            path,
            library_writes: LibraryWrites::default(),
            identity: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn open(root: PathBuf, options: ConversionOptions) -> Result<Self> {
        Self::open_at(
            course2md::config::config_dir().join("desktop-workspace.json"),
            root,
            options,
        )
    }
    /// Opens the record without reconciling it with the libraries; call
    /// `reconciliation` and `apply_reconciliation` before work is dispatched.
    pub fn open_at(path: PathBuf, root: PathBuf, options: ConversionOptions) -> Result<Self> {
        let mut recovery = None;
        if path.is_file()
            && let Ok(bytes) = crate::store::read(&path)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && value
                .get("schema")
                .and_then(|v| v.as_u64())
                .is_some_and(|schema| schema > SCHEMA as u64)
        {
            bail!(course2md::tr!(
                "任务记录由较新版本创建，请使用对应版本打开。原记录和备份均保留，尚未启动任务。"
            ));
        }
        // The first rewrite drops what the record used to carry for each task, and its
        // mirror in the library normally holds the same. This copy covers the rest.
        if crate::store::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|record| record["schema"] == 1)
        {
            let _ = std::fs::copy(&path, path.with_extension("schema1.json"));
        }
        let mut state = if path.exists() {
            match Self::read(&path) {
                Ok(state) => state,
                Err(error) => {
                    let backup = path.with_extension("json.bak");
                    match Self::read(&backup) {
                        Ok(mut state) => {
                            let saved =
                                path.with_extension(format!("{}.damaged", new_id("backup")));
                            std::fs::copy(&path, &saved)
                                .context(course2md::tr!("无法保留损坏的任务记录"))?;
                            recovery = Some(
                                course2md::tr!("已从备份恢复输入和任务。无法确认最后一次操作，未完成任务已暂停。")
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
                                .context(course2md::tr!("输入和任务记录无法读取，原文件已保留"));
                        }
                    }
                }
            }
        } else {
            let mut initial = State::initial(root.clone(), options.clone());
            match std::fs::create_dir_all(&root) {
                Ok(()) => {
                    if !initial.adopt_marker(&root)
                        && let Err(error) = course2md::checkpoint::atomic_write(
                            &root.join(".course2md-library-id"),
                            initial.default_library.as_bytes(),
                        )
                    {
                        recovery = Some(course2md::tr!(
                            "保存位置尚未准备好：{}。可以在设置的存储中选择其他位置。",
                            format!("{error:#}")
                        ));
                    }
                }
                Err(error) => {
                    recovery = Some(course2md::tr!(
                        "无法创建默认保存位置 {}：{}。可以在设置的存储中选择其他位置。",
                        root.display(),
                        error
                    ))
                }
            }
            initial
        };
        state.recover();
        // Defaults may have changed while the app was closed, and drafts saved
        // before an option existed carry no choice of their own for it.
        for draft in &mut state.drafts {
            if draft.submitted_task.is_none() {
                draft.inherit(&options);
            }
        }
        hydrate_draft_subtitles(&mut state, &path);
        state.reconciling = !state.tasks.is_empty();
        Ok(Self::assemble(state, recovery, path))
    }

    /// The checks still owed, to run on a blocking thread. A running task is telling
    /// the app what it is doing, and has nothing to read back.
    pub fn reconciliation(&self) -> Option<Reconciliation> {
        self.state.reconciling.then(|| Reconciliation {
            workspace: self.identity,
            tasks: self
                .state
                .tasks
                .iter()
                .filter(|task| {
                    !task.events.loaded
                        && !matches!(task.state, TaskState::Running | TaskState::Pausing)
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

    /// Tasks of a library that has come back still have to read their events.
    pub fn recheck_library(&mut self, library_id: &str) {
        self.state.reconciling |= self
            .state
            .tasks
            .iter()
            .any(|task| !task.events.loaded && task.plan.library_id == library_id);
    }

    /// Applies the findings to the current records (which may have changed meanwhile)
    /// and allows dispatch again. When the record cannot be saved the findings still
    /// apply in memory, so the error is reported without holding every task back.
    pub fn apply_reconciliation(&mut self, reconciled: Reconciled) -> Result<()> {
        if reconciled.workspace != self.identity {
            return Ok(());
        }
        fn apply(state: &mut State, facts: Vec<(String, TaskFacts)>) {
            for (id, facts) in facts {
                if let Some(task) = state.task_mut(&id)
                    && !matches!(task.state, TaskState::Running | TaskState::Pausing)
                {
                    facts.apply(task);
                }
            }
            state.reconciling = false;
        }
        let mut facts = Some(reconciled.facts);
        let result = self.transaction(|state| {
            apply(state, facts.take().unwrap_or_default());
            Ok(())
        });
        if let Some(facts) = facts.take().filter(|_| result.is_err()) {
            apply(&mut self.state, facts);
        }
        result
    }

    /// `open_at` for tests, with the startup checks applied.
    #[cfg(test)]
    pub(crate) fn open_at_inline(
        path: PathBuf,
        root: PathBuf,
        options: ConversionOptions,
    ) -> Result<Self> {
        let mut workspace = Self::open_at(path, root, options)?;
        workspace.reconcile_now();
        Ok(workspace)
    }

    #[cfg(test)]
    pub(crate) fn reconcile_now(&mut self) {
        if let Some(reconciliation) = self.reconciliation() {
            self.apply_reconciliation(reconciliation.run()).unwrap();
        }
    }

    fn read(path: &Path) -> Result<State> {
        let mut state: State = serde_json::from_slice(
            &crate::store::read(path).with_context(|| course2md::tr!("读取 {}", path.display()))?,
        )?;
        ensure!(
            state.schema <= SCHEMA,
            course2md::tr!("任务记录版本不受支持，请使用创建这些任务的应用版本")
        );
        state.schema = SCHEMA;
        ensure!(
            state.library(&state.default_library).is_some(),
            course2md::tr!("默认保存位置记录缺失")
        );
        ensure!(state.draft().is_some(), course2md::tr!("当前输入记录缺失"));
        let ids: BTreeSet<_> = state.tasks.iter().map(|t| &t.id).collect();
        ensure!(
            ids.len() == state.tasks.len(),
            course2md::tr!("任务记录包含重复身份")
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
        crate::store::save(&self.path, &serde_json::to_vec(&self.state)?)?;
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

    pub(crate) fn library_writes(&self) -> LibraryWrites {
        self.library_writes.clone()
    }

    /// A library write that failed since the last call. The queue wakes the event loop
    /// (see `set_wake_sender`) when one occurs.
    pub fn take_write_error(&self) -> Option<WriteFailure> {
        course2md::runtime::lock_recover(&self.library_writes.failure).take()
    }
    pub fn set_wake_sender(&self, wake: smol::channel::Sender<()>) {
        *course2md::runtime::lock_recover(&self.library_writes.wake) = Some(wake);
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
            course2md::tr!("请选择已有文件夹；新建笔记库请使用新建操作")
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
            .context(course2md::tr!("所选位置不能写入"))?;
        drop(probe);
        let identity = canonical.join(".course2md-library-id");
        let id = if identity.is_file() {
            let id = std::fs::read_to_string(&identity)?.trim().to_owned();
            if id.is_empty() {
                bail!(course2md::tr!("笔记库身份记录为空，请检查所选位置"));
            }
            if existing_ids.iter().any(|existing| existing == &id) {
                bail!(course2md::tr!(
                    "这个库已经登记在另一个位置，请使用移动笔记库操作"
                ));
            }
            id
        } else {
            let id = new_id("library");
            course2md::checkpoint::atomic_write(&identity, id.as_bytes())?;
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
            state.reconciling |= state.tasks.iter().any(|task| !task.events.loaded);
            Ok(id)
        })
    }

    #[cfg(test)]
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
#[cfg(test)]
pub(crate) fn accept_source(
    draft: &mut Draft,
    revision: u64,
    source: crate::source::Source,
) -> bool {
    if draft.revision != revision {
        return false;
    }
    if !draft.custom_title {
        draft.title = source.title.clone();
    }
    draft.source = Some(source);
    draft.updated = now();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use course2md::execution::Component;
    use course2md::progress::Stage;
    fn source(input: &str, title: &str) -> crate::source::Source {
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
            config: ConfigFile::default(),
            asr_service: None,
            ai_service: None,
        }
    }
    #[test]
    fn moving_a_queued_task_changes_which_runs_next() {
        let mut state = State::initial(PathBuf::from("/library"), ConversionOptions::default());
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
        let mut ws = Workspace::open_at_inline(
            target.clone(),
            dir.path().join("library"),
            Default::default(),
        )
        .unwrap();
        std::fs::create_dir(&target).unwrap();
        let task = plan(&ws.state.default_library);
        assert!(ws.transaction(|state| state.enqueue(task, None)).is_err());
        assert!(ws.state.tasks.is_empty());
    }
    #[test]
    fn frozen_task_survives_input_changes_without_storing_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let mut ws =
            Workspace::open_at_inline(file.clone(), dir.path().join("library"), Default::default())
                .unwrap();
        let mut task = plan(&ws.state.default_library);
        task.config.asr_api.api_key = "asr-secret-do-not-store".into();
        task.config.llm.api_key = "ai-secret-do-not-store".into();
        let id = ws.transaction(|state| state.enqueue(task, None)).unwrap().0;
        ws.transaction(|s| {
            s.draft_mut().unwrap().change_source("B.mp4".into());
            s.task_mut(&id).unwrap().state = TaskState::Running;
            Ok(())
        })
        .unwrap();
        let restored =
            Workspace::open_at_inline(file.clone(), dir.path().into(), Default::default()).unwrap();
        assert_eq!(
            restored.state.task(&id).unwrap().plan.source.input,
            "lecture.mp4"
        );
        assert_eq!(restored.state.task(&id).unwrap().state, TaskState::Queued);
        assert_eq!(restored.state.draft().unwrap().input, "B.mp4");
        for path in [file.clone(), file.with_extension("json.bak")] {
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
    fn a_commit_holds_only_what_the_record_owns_and_events_come_back_from_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let task = plan(&ws.state.default_library);
        let id = ws.transaction(|state| state.enqueue(task, None)).unwrap().0;
        let work = ws.state.task(&id).unwrap().work_dir.clone();
        std::fs::create_dir_all(&work).unwrap();
        let event = course2md::progress::Event::Log {
            level: course2md::progress::LogLevel::Info,
            message: "实际进度".into(),
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
    fn records_and_mirrors_from_before_event_streams_keep_everything_they_said() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::NeedsAttention;
        let root = ws.state.libraries[0].root.clone();
        let work = root.join(".course2md/work").join(&id);
        std::fs::create_dir_all(&work).unwrap();
        // The shape the previous version wrote, derived fields and all.
        let mut old = serde_json::to_value(&ws.state).unwrap();
        old["schema"] = serde_json::json!(1);
        let task = old["tasks"][0].as_object_mut().unwrap();
        task.insert("work_dir".into(), serde_json::json!(work));
        task.insert("handled_by".into(), serde_json::Value::Null);
        task.insert("error".into(), serde_json::json!("服务拒绝凭据"));
        task.insert("logs".into(), serde_json::json!(["一行日志"]));
        task.insert(
            "stages".into(),
            serde_json::json!({"transcribe":
                {"status": "done", "current": 7, "total": 7, "detail": null}}),
        );
        task.insert(
            "blocked".into(),
            serde_json::json!([{"reason": "paused", "request_id": null,
                "purpose": null, "message": "已暂停"}]),
        );
        let mirror = serde_json::json!({"schema": 1, "task": old["tasks"][0]});
        let path = ws.storage_path().to_owned();
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        // A library that is offline when the old record is first rewritten keeps
        // its tasks' details in the mirror beside their materials.
        let parked = dir.path().join("offline");
        std::fs::write(work.join("task-record.json"), mirror.to_string()).unwrap();
        std::fs::rename(&root, &parked).unwrap();
        let options = ConversionOptions::default();
        let mut opened =
            Workspace::open_at_inline(path.clone(), root.clone(), options.clone()).unwrap();
        assert!(
            !root.exists(),
            "reading must not recreate an offline library"
        );
        let task = opened.state.task(&id).unwrap();
        assert_eq!(task.events.logs, ["一行日志"]);
        assert!(!task.events.loaded);
        opened.transaction(|_| Ok(())).unwrap();
        let kept = std::fs::read_to_string(path.with_extension("schema1.json")).unwrap();
        assert!(kept.contains("一行日志"));
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(!rewritten.contains("一行日志") && rewritten.contains("服务拒绝凭据"));
        assert!(serde_json::from_str::<serde_json::Value>(&rewritten).unwrap()["schema"] == SCHEMA);

        // Back online: the stream is written from the mirror, and nothing was lost.
        std::fs::rename(&parked, &root).unwrap();
        let reopened = Workspace::open_at_inline(path, root.clone(), options).unwrap();
        let task = reopened.state.task(&id).unwrap();
        assert!(task.events.loaded);
        assert_eq!(task.events.logs, ["一行日志"]);
        assert_eq!(task.events.error.as_deref(), Some("服务拒绝凭据"));
        assert_eq!(task.events.blocked[0].message, "已暂停");
        assert!(task.events.stages[&Stage::Transcribe].done);
        assert_eq!(task.work_dir, work);
        let upgraded = std::fs::read_to_string(work.join("task-record.json")).unwrap();
        assert!(!upgraded.contains("一行日志"));
        assert!(work.join(OLD_MIRROR_FILE).is_file());
        // What it says from now on follows what it said then.
        std::fs::write(
            work.join(TASK_EVENTS_FILE),
            "{\"type\":\"log\",\"level\":\"info\",\"message\":\"下一行\"}\n",
        )
        .unwrap();
        let again =
            Workspace::open_at_inline(reopened.path.clone(), root, Default::default()).unwrap();
        assert_eq!(
            again.state.task(&id).unwrap().events.logs,
            ["一行日志", "下一行"]
        );
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
            Default::default(),
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
        let events = vec![course2md::timeline::TranscriptEvent {
            start: 0.,
            end: 1.,
            text: "字幕正文".into(),
            raw: None,
            translation: None,
        }];
        let mut task = plan(&ws.state.default_library);
        task.source.identity = "video-a".into();
        task.source.selected_subtitle = Some(course2md::subtitle::CachedSubtitle {
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
    fn reopened_drafts_follow_defaults_for_options_they_never_chose() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        let mut ws =
            Workspace::open_at_inline(file.clone(), dir.path().join("library"), Default::default())
                .unwrap();
        ws.transaction(|state| {
            state
                .draft_mut()
                .unwrap()
                .overrides
                .insert(Override::Summary);
            state.draft_mut().unwrap().options.summarize = true;
            Ok(())
        })
        .unwrap();
        // A draft saved before per-task translation existed has no such field.
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        legacy["drafts"][0]["options"]
            .as_object_mut()
            .unwrap()
            .remove("translate");
        std::fs::write(&file, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let _ = std::fs::remove_file(file.with_extension("json.bak"));

        let defaults = ConversionOptions {
            summarize: false,
            translate: Some("en".into()),
            ..Default::default()
        };
        let reopened =
            Workspace::open_at_inline(file, dir.path().join("library"), defaults).unwrap();
        let options = &reopened.state.draft().unwrap().options;
        assert_eq!(options.translate.as_deref(), Some("en"));
        assert!(options.summarize);
    }

    #[test]
    fn only_recent_cancelled_attempts_without_notes_are_kept() {
        let mut state = State::initial("/library".into(), Default::default());
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
    fn source_revisions_and_default_overrides_are_independent() {
        let mut draft = Draft::new(true, "lib".into(), Default::default());
        draft.change_source("A".into());
        let old = draft.revision;
        draft.change_source("B".into());
        assert!(!accept_source(&mut draft, old, source("A", "title-a")));
        let revision = draft.revision;
        assert!(accept_source(&mut draft, revision, source("B", "title-b")));
        draft.options.llm = true;
        draft.overrides.insert(Override::Proofread);
        let defaults = ConversionOptions {
            summarize: true,
            ..Default::default()
        };
        draft.inherit(&defaults);
        assert!(draft.options.llm && draft.options.summarize);
    }

    #[test]
    fn resetting_ai_choices_resumes_inheritance_without_changing_other_overrides_or_services() {
        let mut draft = Draft::new(true, "lib".into(), Default::default());
        draft.change_source("current-video".into());
        draft.title = "Current note".into();
        draft.custom_title = true;
        draft.folder = Some(42);
        draft.options.provider = Some(course2md::config::AsrProvider::Gpu);
        draft.options.source_mode = course2md::config::TranscriptSource::Subtitle;
        draft.options.keep_video = true;
        draft.options.formats = [true, false, true];
        draft.options.llm = true;
        draft.options.summarize = false;
        draft.options.vision = true;
        draft.options.translate = Some("ja".into());
        draft.asr_service = Some("fixed-speech-service".into());
        draft.ai_service = Some("fixed-ai-service".into());
        draft.overrides = [
            Override::Provider,
            Override::TextSource,
            Override::Proofread,
            Override::Summary,
            Override::Vision,
            Override::Translate,
            Override::KeepVideo,
            Override::Formats,
        ]
        .into_iter()
        .collect();
        let defaults = ConversionOptions {
            llm: false,
            summarize: true,
            vision: false,
            translate: Some("de".into()),
            ..Default::default()
        };
        let mut expected = draft.clone();
        expected.options.llm = false;
        expected.options.summarize = true;
        expected.options.vision = false;
        expected.options.translate = Some("de".into());
        expected.overrides = [
            Override::Provider,
            Override::TextSource,
            Override::KeepVideo,
            Override::Formats,
        ]
        .into_iter()
        .collect();
        draft.reset_ai_overrides(&defaults);
        assert_eq!(draft, expected);

        let later_defaults = ConversionOptions {
            llm: true,
            summarize: false,
            vision: true,
            translate: Some("fr".into()),
            ..Default::default()
        };
        draft.inherit(&later_defaults);
        expected.options.llm = true;
        expected.options.summarize = false;
        expected.options.vision = true;
        expected.options.translate = Some("fr".into());
        assert_eq!(draft, expected);
    }

    #[test]
    fn a_second_video_uses_defaults_without_mutating_the_submitted_plan() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let submitted = plan(&ws.state.default_library);
        let task = ws.state.enqueue(submitted.clone(), None).unwrap().0;
        let input = ws.state.draft_mut().unwrap();
        input.input = "first.mp4".into();
        input.title = "Only this video".into();
        input.custom_title = true;
        input.folder = Some(42);
        input.options.llm = true;
        input.overrides.insert(Override::Proofread);
        input.ai_service = Some("task-specific-service".into());
        input.base_config = Some(ConfigFile::default());
        input.submitted_task = Some(task.clone());
        let first_id = input.id.clone();
        let defaults = ConversionOptions::default();
        assert!(!ws.state.prepare_next_import("first.mp4", defaults.clone()));
        assert_eq!(ws.state.draft().unwrap().id, first_id);
        assert!(ws.state.prepare_next_import("second.mp4", defaults.clone()));
        let input = ws.state.draft().unwrap();
        assert_ne!(input.id, first_id);
        assert_eq!(input.input, "second.mp4");
        assert_eq!(input.options, defaults);
        assert_eq!(input.folder, Some(42));
        assert!(input.overrides.is_empty());
        assert!(input.ai_service.is_none() && input.base_config.is_none());
        assert!(input.title.is_empty() && !input.custom_title);
        assert!(ws.state.task(&task).unwrap().plan == submitted);
        let input = ws.state.draft_mut().unwrap();
        input.options.llm = true;
        input.overrides.insert(Override::Proofread);
        assert!(!ws.state.prepare_next_import("revised-second.mp4", defaults));
        assert!(ws.state.draft().unwrap().options.llm);
    }

    #[test]
    fn submitted_input_routes_use_the_new_default_without_moving_existing_tasks() {
        let original_root = tempfile::tempdir().unwrap();
        let next_root = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(original_root.path());
        let original_library = ws.state.default_library.clone();
        let mut submitted = plan(&original_library);
        submitted.folder = Some(42);
        let task = ws.state.enqueue(submitted.clone(), None).unwrap().0;
        let input = ws.state.draft_mut().unwrap();
        input.input = "first.mp4".into();
        input.folder = Some(42);
        input.submitted_task = Some(task.clone());
        let next_library = ws
            .register_library(next_root.path().to_owned(), "Next library".into(), true)
            .unwrap();
        let unchanged_input = ws.state.draft().unwrap().clone();
        assert!(
            !ws.state
                .prepare_next_import("first.mp4", Default::default())
        );
        assert_eq!(ws.state.draft().unwrap(), &unchanged_input);

        for switch_kind in [false, true] {
            let mut state = ws.state.clone();
            if switch_kind {
                state.switch_source_kind(!unchanged_input.online, Default::default());
            } else {
                assert!(state.prepare_next_import("second.mp4", Default::default()));
            }
            let next = state.draft().unwrap();
            assert_eq!(next.library_id, next_library);
            assert_eq!(next.folder, None, "folder IDs belong to their library");
            assert!(state.task(&task).unwrap().plan == submitted);
        }
    }

    #[test]
    fn changing_the_default_keeps_an_unsubmitted_destination_and_folder() {
        let original_root = tempfile::tempdir().unwrap();
        let next_root = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(original_root.path());
        let original_library = ws.state.default_library.clone();
        let input = ws.state.draft_mut().unwrap();
        input.input = "current.mp4".into();
        input.folder = Some(42);
        let online = input.online;
        ws.register_library(next_root.path().to_owned(), "Next library".into(), true)
            .unwrap();
        assert!(
            !ws.state
                .prepare_next_import("revised.mp4", Default::default())
        );
        assert_eq!(ws.state.draft().unwrap().library_id, original_library);
        assert_eq!(ws.state.draft().unwrap().folder, Some(42));
        ws.state.switch_source_kind(!online, Default::default());
        assert_eq!(ws.state.draft().unwrap().library_id, original_library);
        assert_eq!(ws.state.draft().unwrap().folder, Some(42));
    }

    #[test]
    fn replacing_the_form_keeps_submitted_task_plans() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let submitted = plan(&library);
        let id = ws.state.enqueue(submitted.clone(), None).unwrap().0;
        let original_input = ws.state.current_draft.clone();
        let input = ws.state.draft_mut().unwrap();
        input.change_source("old-video.mp4".into());
        input.title = "旧输入名称".into();
        input.custom_title = true;
        input.folder = Some(42);
        input.subtitle = Some("old.srt".into());
        input.submitted_task = Some(id.clone());
        input.options.llm = true;
        input.overrides.insert(Override::Proofread);
        let defaults = ConversionOptions::default();
        for _ in 0..8 {
            ws.state.reset_input(true, defaults.clone(), None);
            let input = ws.state.draft().unwrap();
            assert_ne!(input.id, original_input);
            assert_eq!(ws.state.drafts.len(), 1);
            assert!(input.input.is_empty() && input.title.is_empty());
            assert!(input.source.is_none() && input.subtitle.is_none());
            assert!(input.submitted_task.is_none() && input.retry_of.is_none());
            assert!(input.overrides.is_empty());
            assert_eq!(input.options, defaults);
            assert_eq!(input.folder, None);
            assert!(ws.state.task(&id).unwrap().plan == submitted);
        }
        ws.state
            .reset_input(false, defaults, Some((library.clone(), 7)));
        assert_eq!(ws.state.draft().unwrap().folder, Some(7));
        assert_eq!(ws.state.draft().unwrap().library_id, library);
        ws.transaction(|_| Ok(())).unwrap();
        let restored = test_workspace(dir.path());
        assert_eq!(restored.state.drafts.len(), 1);
        assert_eq!(restored.state.draft().unwrap(), ws.state.draft().unwrap());
        assert!(restored.state.task(&id).unwrap().plan == submitted);
    }

    #[test]
    fn source_mode_switch_preserves_unsubmitted_choices_but_resets_submitted_overrides() {
        let mut state = State::initial("/tmp/library".into(), Default::default());
        let input = state.draft_mut().unwrap();
        input.change_source("https://example.test/online-a".into());
        input.title = "在线课程 A".into();
        input.custom_title = true;
        input.source = Some(source("https://example.test/online-a", "A"));
        input.subtitle = Some("captions.srt".into());
        input.folder = Some(42);
        input.options.formats = [false, true, true];
        input.options.llm = true;
        input.overrides.insert(Override::Formats);
        input.ai_service = Some("ai-service".into());
        input.retry_of = Some("older-task".into());
        let before = input.clone();
        state.switch_source_kind(true, Default::default());
        assert_eq!(state.draft().unwrap(), &before);
        state.switch_source_kind(false, Default::default());
        let local = state.draft().unwrap();
        assert_ne!(local.id, before.id);
        assert!(!local.online);
        assert!(local.input.is_empty() && local.title.is_empty());
        assert!(local.source.is_none() && local.subtitle.is_none());
        assert!(local.retry_of.is_none() && local.submitted_task.is_none());
        assert_eq!(local.options, before.options);
        assert_eq!(local.folder, before.folder);
        assert_eq!(local.ai_service, before.ai_service);
        assert_eq!(local.overrides, before.overrides);
        state
            .draft_mut()
            .unwrap()
            .change_source("local-b.mp4".into());
        state.switch_source_kind(true, Default::default());
        assert_eq!(state.drafts.len(), 1);
        assert!(state.draft().unwrap().online);
        assert!(state.draft().unwrap().input.is_empty());
        assert_eq!(state.draft().unwrap().options, before.options);
        state.draft_mut().unwrap().submitted_task = Some("submitted-task".into());
        let defaults = ConversionOptions::default();
        state.switch_source_kind(false, defaults.clone());
        let fresh = state.draft().unwrap();
        assert_eq!(fresh.options, defaults);
        assert!(fresh.overrides.is_empty() && fresh.ai_service.is_none());
        assert!(fresh.retry_of.is_none() && fresh.submitted_task.is_none());
        assert_eq!(fresh.folder, before.folder);
    }

    #[test]
    fn delayed_results_cannot_cross_new_note_or_mode_changes_even_with_the_same_revision() {
        let mut state = State::initial("/tmp/library".into(), Default::default());
        state
            .draft_mut()
            .unwrap()
            .change_source("same-input".into());
        let pending = state.draft().unwrap().clone();
        assert!(state.matches_input(&pending.id, pending.revision));
        state.reset_input(true, Default::default(), None);
        state
            .draft_mut()
            .unwrap()
            .change_source("same-input".into());
        assert_eq!(state.draft().unwrap().revision, pending.revision);
        assert!(!state.matches_input(&pending.id, pending.revision));
        let after_new = state.draft().unwrap().clone();
        state.switch_source_kind(false, Default::default());
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
        assert_eq!(state.draft().unwrap().title, "当前读取结果");
        assert_eq!(state.drafts.len(), 1);
    }

    #[test]
    fn explicit_stop_survives_restart_while_duplicate_names_do_not_create_work() {
        let mut state = State::initial("/tmp/library".into(), Default::default());
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
    fn adjusting_a_failed_task_replaces_the_form_and_preserves_the_task_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let mut original = plan(&ws.state.default_library);
        original.title = "A 的手工名称".into();
        original.folder = Some(42);
        original.subtitle = Some(dir.path().join("french.srt"));
        original.config.llm.enabled = true;
        original.config.llm.vision = true;
        original.config.defaults.formats = Some(vec![course2md::config::OutputFormat::Json]);
        original.asr_service = Some("asr-version-a".into());
        original.ai_service = Some("ai-version-a".into());
        original.config.defaults.asr_model = Some("qwen3-0.6b".into());
        let id = ws.state.enqueue(original.clone(), None).unwrap().0;
        ws.state.task_mut(&id).unwrap().state = TaskState::Running;
        ws.state.reset_input(true, Default::default(), None);
        let b = ws.state.draft_mut().unwrap();
        b.change_source("online-B".into());
        b.title = "B 的未提交计划".into();
        b.folder = Some(7);
        b.options.summarize = true;
        let b = b.clone();
        assert!(ws.state.adjust_task(&id, None).is_err());
        assert_eq!(ws.state.draft().unwrap(), &b);

        ws.state.task_mut(&id).unwrap().state = TaskState::NeedsAttention;
        ws.state.set_intent(&id, Intent::Run).unwrap();
        assert!(ws.state.next_task().unwrap().plan == original);
        assert_eq!(ws.state.draft().unwrap(), &b);
        ws.state.set_intent(&id, Intent::Pause).unwrap();
        ws.transaction(|state| state.adjust_task(&id, None))
            .unwrap();
        let revised = ws.state.draft_mut().unwrap();
        let options = revised.options.clone();
        revised.inherit(&ConversionOptions::default());
        assert_eq!(revised.options, options);
        assert_eq!(revised.input, original.source.input);
        assert_eq!(revised.source.as_ref(), Some(&original.source));
        assert_eq!(revised.title, original.title);
        assert_eq!(revised.folder, original.folder);
        assert_eq!(revised.library_id, original.library_id);
        assert_eq!(revised.subtitle, original.subtitle);
        assert_eq!(revised.asr_service, original.asr_service);
        assert_eq!(revised.ai_service, original.ai_service);
        assert_eq!(revised.retry_of.as_deref(), Some(id.as_str()));
        assert!(revised.base_config.as_ref() == Some(&original.config));
        let revision = revised.clone();
        drop(ws);
        let mut reopened = test_workspace(dir.path());
        assert_eq!(reopened.state.draft().unwrap(), &revision);
        assert_eq!(reopened.state.drafts.len(), 1);
        assert!(reopened.state.drafts.iter().all(|input| input.id != b.id));
        assert!(reopened.state.task(&id).unwrap().plan == original);
        assert_eq!(reopened.state.task(&id).unwrap().state, TaskState::Paused);
        let mut adjusted = original;
        adjusted.config.llm.summarize = true;
        let (followup, created) = reopened
            .state
            .enqueue(adjusted.clone(), Some(id.clone()))
            .unwrap();
        assert!(created);
        assert_eq!(
            reopened.state.enqueue(adjusted, Some(id.clone())).unwrap(),
            (followup, false)
        );
        assert!(reopened.state.adjust_task(&id, None).is_err());
        assert_eq!(reopened.state.drafts.len(), 1);
        assert!(reopened.state.drafts.iter().all(|input| input.id != b.id));
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

    #[test]
    fn re_adding_a_library_saved_under_a_non_canonical_root_finds_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("library")).unwrap();
        std::fs::create_dir_all(dir.path().join("other")).unwrap();
        // Saved as typed, with a `..` component: not the canonical spelling.
        let saved = dir.path().join("other").join("..").join("library");
        let mut ws = Workspace::open_at_inline(
            dir.path().join("workspace.json"),
            saved.clone(),
            Default::default(),
        )
        .unwrap();
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

    fn test_workspace(dir: &Path) -> Workspace {
        Workspace::open_at_inline(
            dir.join("workspace.json"),
            dir.join("library"),
            Default::default(),
        )
        .unwrap()
    }

    fn publish_note(state: &State, task_id: &str, partial: bool) -> PathBuf {
        let mut outcomes = course2md::artifact::Outcomes {
            transcript: course2md::artifact::Outcome::succeeded(),
            ..Default::default()
        };
        if partial {
            outcomes.proofreading = course2md::artifact::Outcome::failed("校对未完成");
        }
        publish_note_with_outcomes(state, task_id, outcomes)
    }

    fn publish_note_with_outcomes(
        state: &State,
        task_id: &str,
        outcomes: course2md::artifact::Outcomes,
    ) -> PathBuf {
        publish_note_with_exports(state, task_id, outcomes, &[])
    }

    fn publish_note_with_exports(
        state: &State,
        task_id: &str,
        outcomes: course2md::artifact::Outcomes,
        formats: &[course2md::config::OutputFormat],
    ) -> PathBuf {
        use course2md::{artifact, timeline};
        let task = state.task(task_id).unwrap();
        let course_id = crate::workspace::course_dir_name(&task.plan.source_id);
        let target = artifact::Target {
            task_id: task.id.clone(),
            version_id: task.id.clone(),
            source_id: task.plan.source_id.clone(),
            course_id: course_id.clone(),
            course_dir: state
                .library(&task.plan.library_id)
                .unwrap()
                .root
                .join(course_id),
        };
        let meta = course2md::fetch::VideoMeta {
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
        target.version_dir()
    }

    fn write_unknown(task: &TaskRecord, attempt: u32) -> String {
        let id = format!("stable-request.{attempt}");
        let receipt = course2md::dispatch::Receipt {
            schema: 1,
            stable_id: "stable-request".into(),
            request_id: id.clone(),
            purpose: "proofreading".into(),
            description: "校对 00:00–00:20 的文字".into(),
            service_version: "version-a".into(),
            attempt,
            state: course2md::dispatch::State::Uncertain,
            http_status: None,
            response: None,
            message: Some("连接断开 / connection lost".into()),
            unsupported_response_format: false,
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

    fn uncertain_proofreading_note(
        ws: &mut Workspace,
        summarize: bool,
        summary: course2md::artifact::Outcome,
    ) -> (String, String) {
        let mut snapshot = plan(&ws.state.default_library);
        snapshot.config.llm.enabled = true;
        snapshot.config.llm.summarize = summarize;
        let id = ws.state.enqueue(snapshot, None).unwrap().0;
        let outcomes = course2md::artifact::Outcomes {
            transcript: course2md::artifact::Outcome::succeeded(),
            proofreading: course2md::artifact::Outcome::failed("校对请求结果尚不确定"),
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

    #[test]
    fn proofreading_resend_continues_an_authorized_summary_that_was_never_sent() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let (id, request) = uncertain_proofreading_note(
            &mut ws,
            true,
            course2md::artifact::Outcome::failed("前一步请求结果尚不确定；没有继续发送"),
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
        let course2md::execution::Operation::Reprocess {
            components,
            base_version_dir,
            prior_work_dir,
        } = &child.plan.operation
        else {
            panic!("expected component recovery");
        };
        assert_eq!(components, &[Component::Proofreading, Component::Summary]);
        assert_eq!(Some(base_version_dir), original.artifact.as_ref());
        assert_eq!(prior_work_dir.as_ref(), Some(&original.work_dir));
        assert!(child.plan.config.llm.enabled && child.plan.config.llm.summarize);
        assert!(child.plan.options().llm && child.plan.options().summarize);
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
        use course2md::artifact::Outcome;
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
            let course2md::execution::Operation::Reprocess { components, .. } =
                &child.plan.operation
            else {
                panic!("expected component recovery");
            };
            assert_eq!(components, &[Component::Proofreading]);
            assert!(!child.plan.config.llm.summarize && !child.plan.options().summarize);
        }
    }

    #[test]
    fn an_existing_summary_attempt_needs_its_own_recovery_decision() {
        use course2md::dispatch::{Receipt, State as ReceiptState};
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
                course2md::artifact::Outcome::failed("摘要未完成"),
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
                let course2md::execution::Operation::Reprocess { components, .. } =
                    &child.plan.operation
                else {
                    panic!("expected component recovery");
                };
                assert_eq!(components, &[Component::Proofreading], "{receipt_state:?}");
                assert!(!child.plan.config.llm.summarize && !child.plan.options().summarize);
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
        ws.state.task_mut(&id).unwrap().state = TaskState::Uncertain;
        assert!(
            ws.state
                .authorize_uncertain(&id, std::slice::from_ref(&first))
                .is_err()
        );
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
        use course2md::config::OutputFormat;
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
                Default::default(),
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
        let outcomes = course2md::artifact::read_manifest(&version.join("manifest.json"))
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
        assert_eq!(
            child.plan.config.defaults.transcript_source,
            Some(course2md::config::TranscriptSource::Subtitle)
        );
    }

    #[test]
    fn service_repair_creates_a_scoped_attempt_and_preserves_the_old_plan_and_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let mut snapshot = plan(&ws.state.default_library);
        snapshot.ai_service = Some("old-ai-version".into());
        snapshot.config.llm.base_url = "https://old.example.test/v1".into();
        snapshot.config.llm.model = "old-model".into();
        let id = ws.state.enqueue(snapshot.clone(), None).unwrap().0;
        let version = publish_note(&ws.state, &id, true);
        let outcomes = course2md::artifact::read_manifest(&version.join("manifest.json"))
            .unwrap()
            .outcomes;
        let original = ws.state.task_mut(&id).unwrap();
        original.state = TaskState::Partial;
        original.artifact = Some(version.clone());
        original.events.outcomes = Some(outcomes.clone());
        ws.state
            .draft_mut()
            .unwrap()
            .change_source("another-video.mp4".into());
        let input = ws.state.draft().unwrap().clone();
        let mut repaired = ConfigFile::default();
        repaired.llm.base_url = "https://repaired.example.test/v1".into();
        repaired.llm.model = "repaired-model".into();
        // A service repair cannot silently rerun successful work or non-AI stages.
        assert!(
            ws.state
                .reprocess_with_service(
                    &id,
                    vec![Component::Screenshots],
                    vec![],
                    Some(("new-ai-version".into(), repaired.clone()))
                )
                .is_err()
        );
        let followup = ws
            .state
            .reprocess_with_service(
                &id,
                vec![Component::Proofreading],
                vec![],
                Some(("new-ai-version".into(), repaired.clone())),
            )
            .unwrap();
        assert!(ws.state.task(&id).unwrap().plan == snapshot);
        assert_eq!(
            ws.state.task(&id).unwrap().artifact.as_ref(),
            Some(&version)
        );
        assert!(ws.state.draft().unwrap() == &input);
        let child = ws.state.task(&followup).unwrap();
        assert_eq!(child.plan.ai_service.as_deref(), Some("new-ai-version"));
        assert_eq!(child.plan.config.llm.model, "repaired-model");
        assert_eq!(
            child.plan.config.llm.base_url,
            "https://repaired.example.test/v1"
        );
        assert!(child.plan.config.llm.enabled);
        assert!(!child.plan.config.llm.summarize);
        assert!(matches!(
            &child.plan.operation,
            course2md::execution::Operation::Reprocess { base_version_dir, components, .. }
                if base_version_dir == &version && components == &[Component::Proofreading]
        ));
        let again = ws
            .state
            .reprocess_with_service(
                &id,
                vec![Component::Proofreading],
                vec![],
                Some(("new-ai-version".into(), repaired)),
            )
            .unwrap();
        assert_eq!(again, followup);
        assert_eq!(ws.state.tasks.len(), 2);
    }

    #[test]
    fn damaged_primary_and_backup_are_preserved_before_paused_mirror_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let mut snapshot = plan(&ws.state.default_library);
        snapshot.config.llm.api_key = "fake-test-credential".into();
        let id = ws.state.enqueue(snapshot, None).unwrap().0;
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
            ws.storage_path().with_extension("json.bak"),
            b"{broken backup",
        )
        .unwrap();
        assert!(
            Workspace::open_at_inline(
                ws.path.clone(),
                dir.path().join("library"),
                Default::default()
            )
            .is_err()
        );
        let rebuilt = Workspace::rebuild_at(
            ws.path.clone(),
            dir.path().join("library"),
            Default::default(),
        )
        .unwrap();
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
        let reopened = Workspace::open_at_inline(
            ws.path.clone(),
            dir.path().join("library"),
            Default::default(),
        )
        .unwrap();
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
        changed.config.llm.summarize = true;
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
        use course2md::config::OutputFormat;
        let dir = tempfile::tempdir().unwrap();
        let mut ws = test_workspace(dir.path());
        let library = ws.state.default_library.clone();
        let id = ws.state.enqueue(plan(&library), None).unwrap().0;
        let version = publish_note(&ws.state, &id, false);
        let original = ws.state.task_mut(&id).unwrap();
        original.state = TaskState::Partial;
        original.artifact = Some(version);
        original.plan.config.llm.enabled = true;
        original.plan.config.llm.summarize = true;
        original.plan.config.defaults.formats = Some(vec![OutputFormat::Md, OutputFormat::Html]);
        original.events.outcomes = Some(course2md::artifact::Outcomes {
            exports: BTreeMap::from([
                ("md".into(), course2md::artifact::Outcome::succeeded()),
                (
                    "html".into(),
                    course2md::artifact::Outcome::failed("disk full"),
                ),
            ]),
            ..Default::default()
        });
        let next = ws
            .state
            .reprocess(&id, vec![Component::Exports], vec![])
            .unwrap();
        let child = ws.state.task(&next).unwrap();
        assert!(!child.plan.config.llm.enabled && !child.plan.config.llm.summarize);
        assert_eq!(
            child.plan.config.defaults.formats,
            Some(vec![OutputFormat::Html])
        );
        let refs = crate::preferences::ServiceRefs {
            asr: Some("stopped-asr".into()),
            llm: Some("stopped-ai".into()),
        };
        let needed = refs.required_for(&child.plan.config);
        assert!(needed.asr.is_none() && needed.llm.is_none());
    }

    #[test]
    fn published_exports_remain_reachable_after_a_lost_completion_event() {
        use course2md::{artifact, config::OutputFormat, portable};
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
        use course2md::{artifact, config::OutputFormat, portable};
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
        let manifest_bytes = std::fs::read(base.join("manifest.json")).unwrap();
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
        portable::export(&base, OutputFormat::Html, &output).unwrap();
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
        assert!(!output.starts_with(&base));
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
            retry.plan.config.defaults.formats,
            Some(vec![OutputFormat::Html])
        );
        assert_eq!(std::fs::read(old_markdown).unwrap(), old_bytes);
        assert_eq!(
            std::fs::read(base.join("manifest.json")).unwrap(),
            manifest_bytes
        );
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
        assert!(
            Workspace::open_at_inline(
                ws.path.clone(),
                dir.path().join("library"),
                Default::default()
            )
            .is_err()
        );
        assert!(
            Workspace::rebuild_at(
                ws.path.clone(),
                dir.path().join("library"),
                Default::default()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(ws.storage_path()).unwrap(), bytes);
    }
}
