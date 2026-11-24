//! Workbench preview state, input validation, subtitle selection rules, and single-issue
//! obstacle reporting.
use crate::source;
use c2m_config::config::TranscriptSource;
use c2m_source::subtitle::{SubtitleEvidence, SubtitleReadError};
use desktop_prefs::conversion::ConversionOptions;
use desktop_store::workspace;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Where a video comes from and who made it.
pub fn source_metadata(source: &source::Source) -> String {
    let platform = if !source.online {
        c2m_i18n::tr!("本地视频").to_owned()
    } else {
        match source::video_site_name(&source.input) {
            Some(name) => name.to_owned(),
            None => url::Url::parse(&source.input)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default(),
        }
    };
    [platform, source.author.clone()]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionAiOption {
    Proofread,
    Vision,
    Summary,
}

pub fn conversion_ai_option_label(option: ConversionAiOption) -> &'static str {
    match option {
        ConversionAiOption::Proofread => c2m_i18n::tr!("校对文字和标点"),
        ConversionAiOption::Vision => c2m_i18n::tr!("校对时附带对应截图"),
        ConversionAiOption::Summary => c2m_i18n::tr!("在开头生成摘要"),
    }
}

pub fn conversion_ai_option_enabled(
    options: &ConversionOptions,
    option: ConversionAiOption,
) -> bool {
    match option {
        ConversionAiOption::Proofread => options.llm,
        ConversionAiOption::Vision => options.vision,
        ConversionAiOption::Summary => options.summarize,
    }
}

pub fn text_source_choices() -> [(TranscriptSource, &'static str); 3] {
    [
        (TranscriptSource::Auto, c2m_i18n::tr!("自动")),
        (TranscriptSource::Subtitle, c2m_i18n::tr!("仅字幕")),
        (TranscriptSource::Asr, c2m_i18n::tr!("语音识别")),
    ]
}

pub fn text_source_id(mode: TranscriptSource) -> &'static str {
    mode.into()
}

pub fn uses_speech(source: &source::Source, source_mode: TranscriptSource) -> bool {
    source_mode == TranscriptSource::Asr
        || (source_mode == TranscriptSource::Auto
            && source.selected_subtitle.is_none()
            && source.subtitle_request.is_none()
            && source.subtitle_read_error.is_none()
            && matches!(
                source.subtitles,
                SubtitleEvidence::NoneFound
                    | SubtitleEvidence::Unsupported { .. }
                    | SubtitleEvidence::Failed { .. }
            ))
}

pub fn automatic_subtitle_fallback(
    source: &mut source::Source,
    source_mode: TranscriptSource,
    error: Option<&str>,
) -> bool {
    if source_mode != TranscriptSource::Auto
        || source.selected_subtitle.is_some()
        || matches!(
            source.subtitle_read_error,
            Some(SubtitleReadError::Cancelled)
        )
    {
        return false;
    }
    let failure = error.map(str::to_owned).or_else(|| {
        source
            .subtitle_read_error
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| match &source.subtitles {
                SubtitleEvidence::Failed { message } => Some(message.clone()),
                _ => None,
            })
    });
    let Some(message) = failure else {
        return false;
    };
    source.subtitles = SubtitleEvidence::Failed { message };
    source.subtitle_request = None;
    source.subtitle_read_error = None;
    true
}

pub fn default_track_to_read(
    source: &source::Source,
    mode: TranscriptSource,
) -> Option<c2m_source::subtitle::SubtitleTrack> {
    (mode != TranscriptSource::Asr
        && source.selected_subtitle.is_none()
        && source.subtitle_request.is_none()
        && source.subtitle_read_error.is_none())
    .then(|| source.subtitles.tracks().first().cloned())
    .flatten()
}

pub fn subtitles_required_but_missing(
    source: &source::Source,
    source_mode: TranscriptSource,
    read_failed: bool,
) -> bool {
    source_mode == TranscriptSource::Subtitle
        && (read_failed
            || source.subtitle_read_error.is_some()
            || (source.selected_subtitle.is_none()
                && source.subtitle_request.is_none()
                && source.subtitles.tracks().is_empty()))
}

pub fn subtitle_needs_confirmation(source: &source::Source, source_mode: TranscriptSource) -> bool {
    !uses_speech(source, source_mode)
        && (source.selected_subtitle.is_none()
            || source.subtitle_request.is_some()
            || source.subtitle_read_error.is_some())
}

/// Returns the single existing task in the default library for the workbench's current
/// previewed course, if any.
pub fn current_course_task<'a>(
    draft: &workspace::Draft,
    state: &'a workspace::State,
) -> Option<&'a workspace::TaskRecord> {
    let source = draft.source.as_ref()?;
    if source.identity.trim().is_empty() || source.online != draft.online {
        return None;
    }
    state.task_for_source(&state.default_library, &source.identity)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkbenchIssueKind {
    Input,
    Preview,
    Subtitle,
    Plan,
}

/// Single clean obstacle/validation error shown on the workbench.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkbenchIssue {
    pub kind: WorkbenchIssueKind,
    pub message: String,
    pub needs_settings: bool,
}

impl WorkbenchIssue {
    pub fn input(message: impl Into<String>) -> Self {
        Self {
            kind: WorkbenchIssueKind::Input,
            message: message.into(),
            needs_settings: false,
        }
    }

    pub fn preview(message: impl Into<String>, needs_settings: bool) -> Self {
        Self {
            kind: WorkbenchIssueKind::Preview,
            message: message.into(),
            needs_settings,
        }
    }

    pub fn subtitle(message: impl Into<String>) -> Self {
        Self {
            kind: WorkbenchIssueKind::Subtitle,
            message: message.into(),
            needs_settings: false,
        }
    }

    pub fn plan(message: impl Into<String>, needs_settings: bool) -> Self {
        Self {
            kind: WorkbenchIssueKind::Plan,
            message: message.into(),
            needs_settings,
        }
    }

    pub fn from_error(kind: WorkbenchIssueKind, error: &anyhow::Error) -> Self {
        let message = format!("{error:#}");
        let needs_settings = crate::run::message_indicates_settings(&message);
        Self {
            kind,
            message,
            needs_settings,
        }
    }
}

/// Validates a URL or local file path entered on the workbench before probing.
pub fn validate_workbench_input(online: bool, input: &str) -> Result<(), WorkbenchIssue> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(WorkbenchIssue::input(if online {
            c2m_i18n::tr!("请输入视频链接（http:// 或 https://）、BV 号或 AV 号")
        } else {
            c2m_i18n::tr!("先选择一个视频")
        }));
    }
    if online {
        if !source::video_links(trimmed).is_empty() {
            return Ok(());
        }
        source::validate_url(trimmed)
            .map(drop)
            .map_err(|error| WorkbenchIssue::input(error.to_string()))
    } else {
        if Path::new(trimmed).is_file() {
            Ok(())
        } else {
            Err(WorkbenchIssue::input(c2m_i18n::tr!(
                "原视频已移动或无法读取，请重新选择视频"
            )))
        }
    }
}

/// Cache key for deriving a preview plan on Home.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PlanKey {
    pub workspace: u64,
    pub preferences: u64,
    pub services: u64,
    pub services_blocked: bool,
    pub source: u64,
    pub reading: bool,
    pub subtitle_failed: bool,
    pub environment_ready: bool,
    pub library: u64,
    pub library_loading: bool,
}

/// A derived preview plan, or `(issue, is_pending_background_check)`.
pub type PreviewPlan = Result<workspace::TaskPlan, (WorkbenchIssue, bool)>;

/// Ephemeral workbench preview state for the current input.
#[derive(Default)]
pub struct ImportSession {
    pub source_preview: Option<source::Source>,
    pub source_candidates: Vec<source::SourceCandidate>,
    pub source_collection_title: Option<String>,
    pub source_collection_name: Option<String>,
    pub selected_candidates: BTreeSet<String>,
    pub source_candidate_covers: HashMap<String, PathBuf>,
    pub source_covers_cancel: Option<Arc<AtomicBool>>,
    pub subtitle_loading: bool,
    pub subtitle_cancel: Option<Arc<AtomicBool>>,
    pub subtitle_generation: u64,
    pub preview_cancel: Option<Arc<AtomicBool>>,
    pub preview_generation: u64,
    pub preview_workers: usize,
    pub issue: Option<WorkbenchIssue>,
    pub show_preview_details: bool,
    pub subtitle_issue_open: bool,
    pub preview_plan_cache: Option<(PlanKey, PreviewPlan)>,
}

impl ImportSession {
    pub fn reading(&self) -> bool {
        self.preview_cancel.is_some() || self.subtitle_loading
    }

    pub fn subtitle_failed(&self) -> bool {
        self.issue
            .as_ref()
            .is_some_and(|issue| issue.kind == WorkbenchIssueKind::Subtitle)
    }

    pub fn set_issue(&mut self, issue: WorkbenchIssue) {
        self.issue = Some(issue);
    }

    pub fn clear_issue_of(&mut self, kind: WorkbenchIssueKind) {
        if self.issue.as_ref().is_some_and(|issue| issue.kind == kind) {
            self.issue = None;
        }
    }

    pub fn cancel_background_reads(&self) {
        if let Some(cancel) = &self.preview_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = &self.subtitle_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn invalidate_source(&mut self) {
        self.stop_subtitle_read();
        for cancel in [self.preview_cancel.take(), self.source_covers_cancel.take()]
            .into_iter()
            .flatten()
        {
            cancel.store(true, Ordering::Relaxed);
        }
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self.source_preview = None;
        self.source_candidates.clear();
        self.source_candidate_covers.clear();
        self.source_collection_title = None;
        self.source_collection_name = None;
        self.selected_candidates.clear();
        self.issue = None;
        self.show_preview_details = false;
        self.subtitle_issue_open = false;
        self.preview_plan_cache = None;
    }

    pub fn stop_subtitle_read(&mut self) -> u64 {
        if let Some(cancel) = self.subtitle_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.subtitle_generation = self.subtitle_generation.wrapping_add(1);
        self.subtitle_loading = false;
        self.clear_issue_of(WorkbenchIssueKind::Subtitle);
        self.subtitle_generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn forgetting_a_source_stops_its_reads_and_clears_the_single_issue() {
        let preview = Arc::new(AtomicBool::new(false));
        let subtitles = Arc::new(AtomicBool::new(false));
        let mut session = ImportSession {
            preview_cancel: Some(preview.clone()),
            subtitle_cancel: Some(subtitles.clone()),
            subtitle_loading: true,
            issue: Some(WorkbenchIssue::input("missing input")),
            ..Default::default()
        };
        assert!(session.reading());
        session.invalidate_source();
        assert!(!session.reading());
        assert!(preview.load(Ordering::Relaxed) && subtitles.load(Ordering::Relaxed));
        assert_eq!(session.preview_generation, 1);
        assert_eq!(session.subtitle_generation, 1);
        assert!(session.issue.is_none());
    }

    #[test]
    fn input_validation_checks_online_links_and_local_files() {
        assert_eq!(
            validate_workbench_input(true, "  ").unwrap_err().kind,
            WorkbenchIssueKind::Input
        );
        for input in [
            "https://www.bilibili.com/video/BV1xx",
            "BV1WCyGzqE3h",
            "AV170001",
        ] {
            assert!(validate_workbench_input(true, input).is_ok(), "{input}");
        }
        assert!(validate_workbench_input(true, "not a url").is_err());

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("video.mp4");
        assert!(validate_workbench_input(false, &file.display().to_string()).is_err());
        std::fs::write(&file, "bytes").unwrap();
        assert!(validate_workbench_input(false, &file.display().to_string()).is_ok());
    }

    #[test]
    fn current_course_task_matches_default_library_by_course_identity() {
        let mut state = workspace::State::initial(PathBuf::from("/library"));
        let lib = state.default_library.clone();
        let src = source::Source {
            input: "https://example.test/v1".into(),
            identity: "online:v1".into(),
            title: "课程 1".into(),
            online: true,
            ..Default::default()
        };
        let task_id = state
            .enqueue(workspace::NewTask::from_source(
                &lib,
                src.clone(),
                "课程 1",
                workspace::RunMode::Resume,
            ))
            .unwrap();
        let draft = state.draft_mut().unwrap();
        draft.input = src.input.clone();
        draft.source = Some(src);
        assert_eq!(
            current_course_task(state.draft().unwrap(), &state).map(|t| t.id.as_str()),
            Some(task_id.as_str())
        );
    }

    #[test]
    fn plan_key_detects_completed_prerequisite_updates() {
        let waiting = PlanKey {
            workspace: 3,
            preferences: 2,
            services: 5,
            services_blocked: false,
            source: 1,
            reading: true,
            subtitle_failed: false,
            environment_ready: false,
            library: 4,
            library_loading: true,
        };
        assert!(waiting == PlanKey { ..waiting });
        assert!(
            waiting
                != PlanKey {
                    environment_ready: true,
                    ..waiting
                }
        );
        assert!(
            waiting
                != PlanKey {
                    services: 6,
                    ..waiting
                }
        );
    }
}
