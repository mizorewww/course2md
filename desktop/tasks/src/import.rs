//! The workbench's decisions: which task owns the input, when a start intent goes
//! through, and what follows a conversion to its note.
use crate::source;
use c2m_config::config::TranscriptSource;
use c2m_source::subtitle::{SubtitleEvidence, SubtitleReadError};
use desktop_prefs::conversion::ConversionOptions;
use desktop_store::{page::Page, workspace};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
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

/// One AI processing switch in the processing options.
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

pub fn apply_conversion_ai_option(
    options: &mut ConversionOptions,
    option: ConversionAiOption,
    enabled: bool,
) {
    match option {
        ConversionAiOption::Proofread => options.llm = enabled,
        ConversionAiOption::Vision => options.vision = enabled,
        ConversionAiOption::Summary => options.summarize = enabled,
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

pub fn subtitle_needs_confirmation(source: &source::Source, source_mode: TranscriptSource) -> bool {
    !uses_speech(source, source_mode)
        && (source.selected_subtitle.is_none()
            || source.subtitle_request.is_some()
            || source.subtitle_read_error.is_some())
}

/// Follow only this input's submitted recovery chain. A matching URL or the
/// selected queue item alone cannot establish that a task belongs to this form.
pub fn submitted_input_task<'a>(
    draft: &workspace::Draft,
    tasks: &'a [workspace::TaskRecord],
    input: &str,
) -> Option<&'a workspace::TaskRecord> {
    if draft.input != input || input.trim().is_empty() {
        return None;
    }
    let find = |id: &str| {
        let mut matches = tasks.iter().filter(|task| task.id == id);
        let task = matches.next()?;
        matches.next().is_none().then_some(task)
    };
    let mut task = find(draft.submitted_task.as_deref()?)?;
    let source_id = &task.plan.source_id;
    let online = task.plan.source.online;
    if source_id.is_empty()
        || task.plan.source.input != input
        || online != draft.online
        || draft
            .source
            .as_ref()
            .is_some_and(|source| source.identity != *source_id || source.online != online)
    {
        return None;
    }
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(task.id.as_str())
            || task.plan.source_id != *source_id
            || task.plan.source.identity != *source_id
            || task.plan.source.online != online
        {
            return None;
        }
        let Some(next) = task.handled_by.as_deref() else {
            return Some(task);
        };
        let next = find(next)?;
        if next.parent.as_deref() != Some(task.id.as_str()) {
            return None;
        }
        task = next;
    }
}

/// The submitted task still owns this input until it finishes. Its plan
/// copied the options at submission, so the form cannot change them.
pub fn locked_input_task<'a>(
    draft: &workspace::Draft,
    tasks: &'a [workspace::TaskRecord],
    input: &str,
) -> Option<&'a workspace::TaskRecord> {
    submitted_input_task(draft, tasks, input).filter(|task| !task.state.finished())
}

pub(crate) fn completed_input_task<'a>(
    draft: &workspace::Draft,
    tasks: &'a [workspace::TaskRecord],
    input: &str,
) -> Option<&'a workspace::TaskRecord> {
    submitted_input_task(draft, tasks, input).filter(|task| {
        matches!(
            task.state,
            workspace::TaskState::Complete | workspace::TaskState::Partial
        ) && task.artifact.is_some()
    })
}

/// A saved result is history when entering the workbench. A running or paused
/// recovery, an uncertain request, and any changes after submission stay in place.
pub fn retired_input_task<'a>(
    draft: &workspace::Draft,
    tasks: &'a [workspace::TaskRecord],
    input: &str,
) -> Option<&'a workspace::TaskRecord> {
    let task = submitted_input_task(draft, tasks, input)?;
    if !task.state.finished()
        || task.has_uncertain()
        || (task.state != workspace::TaskState::Cancelled && task.artifact.is_none())
    {
        return None;
    }
    let submitted = tasks
        .iter()
        .find(|task| Some(task.id.as_str()) == draft.submitted_task.as_deref())?;
    (draft.title == submitted.plan.title
        && draft.options.as_planned() == submitted.plan.options()
        && draft.library_id == submitted.plan.library_id
        && draft.folder == submitted.plan.folder
        && draft.subtitle == submitted.plan.subtitle
        && draft
            .asr_service
            .as_ref()
            .is_none_or(|id| Some(id) == submitted.plan.asr_service.as_ref())
        && draft
            .ai_service
            .as_ref()
            .is_none_or(|id| Some(id) == submitted.plan.ai_service.as_ref()))
    .then_some(task)
}

/// A live navigation intent is separate from the saved task. Leaving its
/// originating input or note ends following without cancelling the work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversionFollow {
    Preparing(u64),
    Task {
        id: String,
        source_revision: u64,
    },
    /// A repair started from the reader, which reopens its note once the repair
    /// has replaced it, while the reader still shows the note as it was.
    ReaderTask {
        id: String,
        note: PathBuf,
        reader_revision: u64,
    },
}

impl ConversionFollow {
    /// Opening a viewer owns the note as it is until the person closes it. Do not
    /// restore this intent afterwards, even if an in-flight read is still pending.
    pub fn interrupt_for_reader_viewer(follow: &mut Option<Self>) {
        if matches!(follow, Some(Self::ReaderTask { .. })) {
            *follow = None;
        }
    }

    pub fn submitted(self, revision: u64, id: String) -> Option<Self> {
        matches!(self, Self::Preparing(current) if current == revision).then_some(Self::Task {
            id,
            source_revision: revision,
        })
    }

    pub fn follows(&self, id: &str, revision: u64) -> bool {
        matches!(self, Self::Task { id: followed, source_revision }
            if followed == id && *source_revision == revision)
    }

    pub fn reader_reprocess(
        task: &workspace::TaskRecord,
        note: &std::path::Path,
        revision: u64,
    ) -> Option<Self> {
        let c2m_engine::execution::Operation::Reprocess {
            note_dir,
            components,
            ..
        } = &task.plan.operation
        else {
            return None;
        };
        (note_dir == note && !components.is_empty() && !task.exports_only()).then(|| {
            Self::ReaderTask {
                id: task.id.clone(),
                note: note_dir.clone(),
                reader_revision: revision,
            }
        })
    }

    /// Check both before starting an automatic read and after its asynchronous
    /// load. The latter also retains the caller's existing read-generation guard.
    pub fn context_is_current(
        &self,
        page: Page,
        source_revision: u64,
        reader_revision: u64,
        note: Option<&std::path::Path>,
    ) -> bool {
        match self {
            Self::Preparing(_) => false,
            Self::Task {
                source_revision: expected,
                ..
            } => page == Page::New && *expected == source_revision,
            Self::ReaderTask {
                note: followed,
                reader_revision: expected,
                ..
            } => {
                page == Page::Result
                    && *expected == reader_revision
                    && note == Some(followed.as_path())
            }
        }
    }

    /// The followed repair finished with the note replaced, and the reader still
    /// shows the note `opened_task` wrote before it: the note is read again.
    pub fn completed_reader_task(
        &self,
        task: &workspace::TaskRecord,
        page: Page,
        reader_revision: u64,
        note: Option<&std::path::Path>,
        opened_task: Option<&str>,
    ) -> bool {
        let Self::ReaderTask {
            id, note: followed, ..
        } = self
        else {
            return false;
        };
        task.id == *id
            && self.context_is_current(page, 0, reader_revision, note)
            && Self::reader_reprocess(task, followed, reader_revision).as_ref() == Some(self)
            && matches!(
                task.state,
                workspace::TaskState::Complete | workspace::TaskState::Partial
            )
            && task.artifact.as_ref() == Some(followed)
            && opened_task.is_some_and(|opened| opened != task.id)
    }

    pub fn completed_task<'a>(
        &self,
        on_workbench: bool,
        revision: u64,
        draft: &workspace::Draft,
        tasks: &'a [workspace::TaskRecord],
        input: &str,
    ) -> Option<&'a workspace::TaskRecord> {
        let task = completed_input_task(draft, tasks, input)?;
        (on_workbench && self.follows(&task.id, revision)).then_some(task)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionGate {
    Wait,
    Stop,
    Submit,
}

/// Normal asynchronous stages retain the first Start intent. Only an abandoned
/// source or an unresolved material choice ends this automatic continuation.
/// What a pending Start depends on.
#[derive(Clone, Copy)]
pub struct GateInputs {
    pub requested_revision: Option<u64>,
    pub current_revision: u64,
    pub reading: bool,
    pub has_source: bool,
    pub needs_text_choice: bool,
    pub environment_ready: bool,
    pub library_ready: bool,
    pub existing_note: bool,
}

pub fn conversion_gate(gate: GateInputs) -> ConversionGate {
    if gate.requested_revision != Some(gate.current_revision) {
        ConversionGate::Stop
    } else if gate.reading {
        ConversionGate::Wait
    } else if !gate.has_source {
        ConversionGate::Stop
    } else if gate.needs_text_choice || !gate.environment_ready || !gate.library_ready {
        ConversionGate::Wait
    } else if gate.existing_note {
        ConversionGate::Stop
    } else {
        ConversionGate::Submit
    }
}

/// Everything a derived preview plan reads: any change derives it again.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PlanKey {
    pub workspace: u64,
    pub preferences: u64,
    pub defaults_pending: bool,
    pub source: u64,
    pub reading: bool,
    pub subtitle_failed: bool,
    pub environment_ready: bool,
    pub library: u64,
    pub library_loading: bool,
}

/// What the workbench knows about the current input while no task owns it: the source
/// being read, its candidates and subtitles, and a start intent on its way.
#[derive(Default)]
pub struct ImportSession {
    pub completed_source: Option<String>,
    pub source_preview: Option<source::Source>,
    pub source_candidates: Vec<source::SourceCandidate>,
    pub source_collection_title: Option<String>,
    /// The real collection name from the probe (the batch note title prefix); placeholder
    /// hints are not names, see source_collection_title.
    pub source_collection_name: Option<String>,
    /// Inputs of the candidates checked in an online list; cleared when the candidates change.
    pub selected_candidates: BTreeSet<String>,
    /// Local cache of candidate row previews (candidate.input → cached file), downloaded
    /// once per URL in the background.
    pub source_candidate_covers: HashMap<String, PathBuf>,
    pub source_covers_cancel: Option<Arc<AtomicBool>>,
    pub subtitle_loading: bool,
    pub subtitle_cancel: Option<Arc<AtomicBool>>,
    pub subtitle_error: Option<String>,
    pub subtitle_generation: u64,
    pub preview_cancel: Option<Arc<AtomicBool>>,
    pub preview_generation: u64,
    pub pending_conversion: Option<u64>,
    pub following_conversion: Option<ConversionFollow>,
    pub preview_workers: usize,
    pub preview_error: Option<String>,
    pub source_validation: Option<String>,
    /// The current validation error is the missing-title case (typed, not text-matched).
    pub title_required: bool,
    pub show_preview_details: bool,
    pub subtitle_issue_open: bool,
    pub source_editor_open: bool,
    pub validation_attempt: usize,
    /// The derived preview plan; an error carries whether it is only waiting on background
    /// checks (`plan::PlanPending`).
    pub preview_plan_cache: Option<(PlanKey, PreviewPlan)>,
}

/// A derived preview plan, or its message and whether it is only waiting.
pub(crate) type PreviewPlan = Result<workspace::TaskPlan, (String, bool)>;

impl ImportSession {
    /// A source or its subtitles are being read.
    pub fn reading(&self) -> bool {
        self.preview_cancel.is_some() || self.subtitle_loading
    }

    /// Stops the background preview and subtitle readers.
    pub fn cancel_background_reads(&self) {
        if let Some(cancel) = &self.preview_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = &self.subtitle_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Forgets the source: running reads stop, their late results no longer count.
    pub fn invalidate_source(&mut self) {
        self.pending_conversion = None;
        self.stop_subtitle_read();
        for cancel in [self.preview_cancel.take(), self.source_covers_cancel.take()]
            .into_iter()
            .flatten()
        {
            cancel.store(true, Ordering::Relaxed);
        }
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self.source_preview = None;
        self.source_editor_open = false;
        self.source_candidates.clear();
        self.source_candidate_covers.clear();
        self.source_collection_title = None;
        self.source_collection_name = None;
        self.selected_candidates.clear();
        self.subtitle_error = None;
        self.preview_error = None;
        self.source_validation = None;
        self.title_required = false;
        self.show_preview_details = false;
        self.subtitle_issue_open = false;
    }

    /// Stops a subtitle read; its late result no longer counts. Returns the new generation.
    pub fn stop_subtitle_read(&mut self) -> u64 {
        if let Some(cancel) = self.subtitle_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.subtitle_generation = self.subtitle_generation.wrapping_add(1);
        self.subtitle_loading = false;
        self.subtitle_generation
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn forgetting_a_source_stops_its_reads_and_outdates_their_results() {
        use super::ImportSession;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let (preview, subtitles) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let mut session = ImportSession {
            preview_cancel: Some(preview.clone()),
            subtitle_cancel: Some(subtitles.clone()),
            subtitle_loading: true,
            pending_conversion: Some(0),
            source_validation: Some("先粘贴视频链接".into()),
            ..Default::default()
        };
        assert!(session.reading());
        session.invalidate_source();
        assert!(!session.reading());
        assert!(preview.load(Ordering::Relaxed) && subtitles.load(Ordering::Relaxed));
        assert_eq!(session.preview_generation, 1);
        assert_eq!(session.subtitle_generation, 1);
        assert_eq!(session.pending_conversion, None);
        assert_eq!(session.source_validation, None);
        assert_eq!(session.stop_subtitle_read(), 2);
    }

    use super::{
        ConversionFollow, ConversionGate, GateInputs, PlanKey, automatic_subtitle_fallback,
        completed_input_task, conversion_gate, locked_input_task, submitted_input_task,
        subtitle_needs_confirmation, uses_speech,
    };
    use crate::source;
    use c2m_config::config::TranscriptSource;
    use c2m_source::subtitle::{CachedSubtitle, SubtitleEvidence, SubtitleReadError};
    use desktop_store::workspace;

    /// A plan's configuration as build_plan writes it from default options.
    fn planned_config() -> c2m_config::settings::ConfigFile {
        let mut config = c2m_config::settings::ConfigFile::default();
        desktop_prefs::conversion::ConversionOptions::default().apply_to(&mut config);
        config
    }

    /// A pending Start for revision 7 with every prerequisite met.
    fn ready() -> GateInputs {
        GateInputs {
            requested_revision: Some(7),
            current_revision: 7,
            reading: false,
            has_source: true,
            needs_text_choice: false,
            environment_ready: true,
            library_ready: true,
            existing_note: false,
        }
    }

    #[test]
    fn a_preview_plan_is_derived_again_once_what_it_waited_for_has_finished() {
        let waiting = PlanKey {
            workspace: 3,
            preferences: 2,
            defaults_pending: false,
            source: 1,
            reading: true,
            subtitle_failed: false,
            environment_ready: false,
            library: 4,
            library_loading: true,
        };
        assert!(waiting == PlanKey { ..waiting });
        for finished in [
            PlanKey {
                environment_ready: true,
                ..waiting
            },
            PlanKey {
                library_loading: false,
                ..waiting
            },
            PlanKey {
                reading: false,
                ..waiting
            },
        ] {
            assert!(waiting != finished);
        }
    }

    #[test]
    fn preview_completion_without_confirmation_never_submits() {
        for reading in [true, false] {
            for environment_ready in [true, false] {
                for library_ready in [true, false] {
                    let gate = GateInputs {
                        requested_revision: None,
                        reading,
                        environment_ready,
                        library_ready,
                        ..ready()
                    };
                    assert_eq!(conversion_gate(gate), ConversionGate::Stop);
                }
            }
        }
    }

    #[test]
    fn confirmed_start_waits_for_remaining_prerequisites() {
        // The same Start revision survives each independently completing prerequisite.
        for (reading, has_source, needs_text_choice, environment_ready) in [
            (true, false, false, false),
            (true, true, true, true),
            (false, true, false, false),
            (false, true, true, true),
        ] {
            let gate = GateInputs {
                reading,
                has_source,
                needs_text_choice,
                environment_ready,
                ..ready()
            };
            assert_eq!(conversion_gate(gate), ConversionGate::Wait);
        }
        assert_eq!(conversion_gate(ready()), ConversionGate::Submit);
        // Source replacement and duplicate content require a fresh intent/choice.
        let replaced = GateInputs {
            current_revision: 8,
            reading: true,
            has_source: false,
            ..ready()
        };
        assert_eq!(conversion_gate(replaced), ConversionGate::Stop);
        assert_eq!(
            conversion_gate(GateInputs {
                existing_note: true,
                ..ready()
            }),
            ConversionGate::Stop
        );
        assert_eq!(
            conversion_gate(GateInputs {
                has_source: false,
                ..ready()
            }),
            ConversionGate::Stop
        );
    }

    #[test]
    fn first_start_waits_for_library_scan_before_existing_note_decision() {
        // A restored source and the environment can be ready before the async
        // library scan replaces its initially empty (or stale) note list.
        for existing_note in [false, true] {
            let gate = GateInputs {
                library_ready: false,
                existing_note,
                ..ready()
            };
            assert_eq!(conversion_gate(gate), ConversionGate::Wait);
        }
        // The original Start is still pending when the scan reports a result.
        assert_eq!(
            conversion_gate(GateInputs {
                existing_note: true,
                ..ready()
            }),
            ConversionGate::Stop
        );
        assert_eq!(conversion_gate(ready()), ConversionGate::Submit);
        let replaced = GateInputs {
            current_revision: 8,
            library_ready: false,
            ..ready()
        };
        assert_eq!(conversion_gate(replaced), ConversionGate::Stop);
    }

    #[test]
    fn an_explicit_regenerate_follows_the_new_task_instead_of_the_previous_result() {
        let (mut draft, mut tasks) = submitted_recovery_chain();
        tasks[0].state = workspace::TaskState::Complete;
        tasks[0].artifact = Some("note".into());
        tasks[0].handled_by = None;
        tasks[0].events.error = None;
        tasks[1].id = "new-version".into();
        tasks[1].parent = None;
        tasks[1].plan.operation = Default::default();
        draft.submitted_task = Some(tasks[1].id.clone());
        let following = ConversionFollow::Preparing(7)
            .submitted(7, tasks[1].id.clone())
            .unwrap();
        // The note as it was remains readable but is not the explicit new intent.
        assert!(
            following
                .completed_task(true, 7, &draft, &tasks, &draft.input)
                .is_none()
        );
        tasks[1].state = workspace::TaskState::Complete;
        tasks[1].artifact = Some("note".into());
        assert_eq!(
            following
                .completed_task(true, 7, &draft, &tasks, &draft.input)
                .unwrap()
                .id,
            "new-version",
        );
    }

    #[test]
    fn completion_opens_only_the_result_still_followed_in_the_current_input() {
        let (draft, mut tasks) = submitted_recovery_chain();
        let followed = ConversionFollow::Preparing(7)
            .submitted(7, "recovery".into())
            .unwrap();
        assert!(
            ConversionFollow::Preparing(7)
                .submitted(8, "recovery".into())
                .is_none()
        );
        assert!(
            followed
                .completed_task(true, 7, &draft, &tasks, &draft.input)
                .is_none()
        );
        tasks[1].state = workspace::TaskState::Complete;
        assert!(
            followed
                .completed_task(true, 7, &draft, &tasks, &draft.input)
                .is_none()
        );
        tasks[1].artifact = Some("note".into());
        assert_eq!(
            followed
                .completed_task(true, 7, &draft, &tasks, &draft.input)
                .unwrap()
                .id,
            "recovery"
        );
        assert!(
            followed
                .completed_task(false, 7, &draft, &tasks, &draft.input)
                .is_none()
        );
        assert!(
            followed
                .completed_task(true, 8, &draft, &tasks, &draft.input)
                .is_none()
        );
        assert!(
            followed
                .completed_task(true, 7, &draft, &tasks, "new video")
                .is_none()
        );
        let other = ConversionFollow::Task {
            id: "original".into(),
            source_revision: 7,
        };
        assert!(
            other
                .completed_task(true, 7, &draft, &tasks, &draft.input)
                .is_none()
        );
        tasks[1].state = workspace::TaskState::Partial;
        assert!(
            followed
                .completed_task(true, 7, &draft, &tasks, &draft.input)
                .is_some()
        );
    }

    #[test]
    fn reader_repair_reopens_the_note_only_while_it_still_shows_the_note_before_it() {
        let (_, mut tasks) = submitted_recovery_chain();
        let note = std::path::Path::new("note");
        let before = Some("original");
        let follow = ConversionFollow::reader_reprocess(&tasks[1], note, 12).unwrap();
        // Submitting a recovery is not enough: it must publish a readable result.
        assert!(!follow.completed_reader_task(
            &tasks[1],
            super::Page::Result,
            12,
            Some(note),
            before
        ));
        tasks[1].artifact = Some(note.into());
        for state in [
            workspace::TaskState::Complete,
            workspace::TaskState::Partial,
        ] {
            tasks[1].state = state;
            assert!(follow.completed_reader_task(
                &tasks[1],
                super::Page::Result,
                12,
                Some(note),
                before
            ));
        }
        // The reader already shows what the repair wrote, or shows no manifest.
        for opened in [Some("recovery"), None] {
            assert!(!follow.completed_reader_task(
                &tasks[1],
                super::Page::Result,
                12,
                Some(note),
                opened
            ));
        }
        for page in [
            super::Page::New,
            super::Page::Library,
            super::Page::Task,
            super::Page::Settings,
        ] {
            assert!(!follow.completed_reader_task(&tasks[1], page, 12, Some(note), before));
        }
        // Leaving and returning to the same note has a new read generation.
        assert!(!follow.completed_reader_task(
            &tasks[1],
            super::Page::Result,
            13,
            Some(note),
            before
        ));
        assert!(!follow.completed_reader_task(
            &tasks[1],
            super::Page::Result,
            12,
            Some(std::path::Path::new("another-note")),
            before,
        ));
        assert!(!follow.completed_reader_task(&tasks[1], super::Page::Result, 12, None, before));
        let mut unrelated = tasks[1].clone();
        unrelated.id = "another-repair".into();
        assert!(!follow.completed_reader_task(
            &unrelated,
            super::Page::Result,
            12,
            Some(note),
            before
        ));
        for state in [
            workspace::TaskState::Running,
            workspace::TaskState::Paused,
            workspace::TaskState::Uncertain,
        ] {
            tasks[1].state = state;
            assert!(!follow.completed_reader_task(
                &tasks[1],
                super::Page::Result,
                12,
                Some(note),
                before
            ));
        }
    }

    #[test]
    fn reader_repair_guard_excludes_exports_and_rechecks_the_note_when_loading_finishes() {
        let (_, mut tasks) = submitted_recovery_chain();
        let note = std::path::Path::new("note");
        let before = Some("original");
        let follow = ConversionFollow::reader_reprocess(&tasks[1], note, 12).unwrap();
        assert!(follow.context_is_current(super::Page::Result, 99, 12, Some(note)));
        assert!(!follow.context_is_current(super::Page::Result, 99, 13, Some(note)));
        assert!(!follow.context_is_current(super::Page::Library, 99, 12, Some(note)));
        assert!(!follow.context_is_current(
            super::Page::Result,
            99,
            12,
            Some(std::path::Path::new("other")),
        ));

        let c2m_engine::execution::Operation::Reprocess { components, .. } =
            &mut tasks[1].plan.operation
        else {
            unreachable!();
        };
        *components = vec![c2m_engine::execution::Component::Exports];
        tasks[1].state = workspace::TaskState::Complete;
        tasks[1].artifact = Some(note.to_owned());
        assert!(ConversionFollow::reader_reprocess(&tasks[1], note, 12).is_none());
        // An export-only task never turns into navigation.
        assert!(!follow.completed_reader_task(
            &tasks[1],
            super::Page::Result,
            12,
            Some(note),
            before
        ));
        tasks[1].plan.operation = Default::default();
        assert!(ConversionFollow::reader_reprocess(&tasks[1], note, 12).is_none());
    }

    #[test]
    fn opening_a_reader_viewer_ends_repair_following_even_if_it_closes_before_the_read_finishes() {
        let (_, mut tasks) = submitted_recovery_chain();
        let note = std::path::Path::new("note");
        let before = Some("original");
        let captured = ConversionFollow::reader_reprocess(&tasks[1], note, 12).unwrap();
        tasks[1].state = workspace::TaskState::Complete;
        tasks[1].artifact = Some(note.into());
        for read_started in [false, true] {
            let mut active = Some(captured.clone());
            if read_started {
                assert!(active.as_ref().unwrap().completed_reader_task(
                    &tasks[1],
                    super::Page::Result,
                    12,
                    Some(note),
                    before,
                ));
            }
            ConversionFollow::interrupt_for_reader_viewer(&mut active);
            assert!(active.is_none());
            // Closing the viewer only restores its underlying note. The old
            // asynchronous callback still has a ticket, but no longer owns intent.
            assert_ne!(active.as_ref(), Some(&captured));
            assert!(
                !active
                    .as_ref()
                    .is_some_and(|follow| follow.completed_reader_task(
                        &tasks[1],
                        super::Page::Result,
                        12,
                        Some(note),
                        before,
                    ))
            );
        }
        // A viewer must not cancel a distinct workbench conversion.
        let mut input = Some(ConversionFollow::Task {
            id: "conversion".into(),
            source_revision: 7,
        });
        let expected = input.clone();
        ConversionFollow::interrupt_for_reader_viewer(&mut input);
        assert_eq!(input, expected);
    }

    #[test]
    fn a_submitted_input_locks_its_options_until_the_next_video_replaces_it() {
        let root = tempfile::tempdir().unwrap();
        let mut workspace = workspace::Workspace::open_at_inline(
            root.path().join("workspace.json"),
            root.path().join("library"),
            Default::default(),
        )
        .unwrap();
        let state = &mut workspace.state;
        let source = source::Source {
            input: "https://www.bilibili.com/video/BVfixture".into(),
            identity: "video-fixture".into(),
            online: true,
            ..Default::default()
        };
        let plan = workspace::TaskPlan {
            operation: Default::default(),
            source: source.clone(),
            source_id: source.identity.clone(),
            title: "已提交的标题".into(),
            library_id: state.default_library.clone(),
            folder: None,
            subtitle: None,
            config: planned_config(),
            asr_service: None,
            ai_service: None,
        };
        // Before submission the options belong to the form.
        let draft = state.draft_mut().unwrap();
        draft.change_source(source.input.clone());
        draft.source = Some(source.clone());
        assert!(locked_input_task(state.draft().unwrap(), &state.tasks, &source.input).is_none());
        let (id, _) = state.enqueue(plan.clone(), None).unwrap();
        state.draft_mut().unwrap().submitted_task = Some(id.clone());
        // Leaving for the task list and coming back home keeps an in-flight
        // input, so the options panel must offer the next video instead of
        // vanishing.
        for in_flight in [
            workspace::TaskState::Queued,
            workspace::TaskState::Running,
            workspace::TaskState::Pausing,
            workspace::TaskState::Paused,
            workspace::TaskState::NeedsAttention,
            workspace::TaskState::Uncertain,
        ] {
            state.task_mut(&id).unwrap().state = in_flight;
            let draft = state.draft().unwrap();
            assert!(super::retired_input_task(draft, &state.tasks, &source.input).is_none());
            assert_eq!(
                locked_input_task(draft, &state.tasks, &source.input)
                    .unwrap()
                    .id,
                id
            );
        }
        for finished in [
            workspace::TaskState::Complete,
            workspace::TaskState::Partial,
            workspace::TaskState::Cancelled,
        ] {
            state.task_mut(&id).unwrap().state = finished;
            assert!(
                locked_input_task(state.draft().unwrap(), &state.tasks, &source.input).is_none()
            );
        }
        // 设置下一个视频 clears the link: the next input starts from the
        // saved defaults and the submitted task keeps its own plan.
        state.task_mut(&id).unwrap().state = workspace::TaskState::Running;
        let defaults = desktop_prefs::conversion::ConversionOptions {
            summarize: !state.draft().unwrap().options.summarize,
            ..state.draft().unwrap().options.clone()
        };
        assert!(state.prepare_next_import("", defaults.clone()));
        let next = state.draft().unwrap();
        assert!(next.submitted_task.is_none());
        assert!(next.options == defaults);
        assert!(locked_input_task(next, &state.tasks, "").is_none());
        let task = state.task(&id).unwrap();
        assert_eq!(task.state, workspace::TaskState::Running);
        assert!(task.plan == plan);
    }

    #[test]
    fn completion_belongs_to_the_submitted_input_and_requires_a_readable_result() {
        let root = tempfile::tempdir().unwrap();
        let mut workspace = workspace::Workspace::open_at_inline(
            root.path().join("workspace.json"),
            root.path().join("library"),
            Default::default(),
        )
        .unwrap();
        let state = &mut workspace.state;
        let source = source::Source {
            input: "https://www.bilibili.com/video/BVfixture".into(),
            identity: "video-fixture".into(),
            online: true,
            ..Default::default()
        };
        let plan = workspace::TaskPlan {
            operation: Default::default(),
            source: source.clone(),
            source_id: source.identity.clone(),
            title: "已提交的标题".into(),
            library_id: state.default_library.clone(),
            folder: None,
            subtitle: None,
            config: planned_config(),
            asr_service: None,
            ai_service: None,
        };
        let (id, _) = state.enqueue(plan.clone(), None).unwrap();
        let draft = state.draft_mut().unwrap();
        draft.change_source(source.input.clone());
        draft.submitted_task = Some(id.clone());
        assert!(
            completed_input_task(state.draft().unwrap(), &state.tasks, &source.input).is_none()
        );
        state.task_mut(&id).unwrap().state = workspace::TaskState::Complete;
        assert!(
            completed_input_task(state.draft().unwrap(), &state.tasks, &source.input).is_none()
        );
        state.task_mut(&id).unwrap().artifact = Some(root.path().join("published-version"));
        assert_eq!(
            completed_input_task(state.draft().unwrap(), &state.tasks, &source.input)
                .unwrap()
                .id,
            id,
        );
        // The text can change before its observer replaces the submitted draft.
        // That next input must remain visible even during this intermediate state.
        for next in ["", "https://www.bilibili.com/video/BVnext"] {
            assert!(completed_input_task(state.draft().unwrap(), &state.tasks, next).is_none());
        }
        state.task_mut(&id).unwrap().state = workspace::TaskState::Partial;
        assert!(
            completed_input_task(state.draft().unwrap(), &state.tasks, &source.input).is_some()
        );
        state.task_mut(&id).unwrap().state = workspace::TaskState::Complete;
        state.prepare_next_import("https://www.bilibili.com/video/BVnext", Default::default());
        assert!(
            completed_input_task(state.draft().unwrap(), &state.tasks, &source.input).is_none()
        );
        assert!(state.task(&id).unwrap().plan == plan);
    }

    fn submitted_recovery_chain() -> (workspace::Draft, Vec<workspace::TaskRecord>) {
        let source = source::Source {
            input: "https://www.bilibili.com/video/BVfixture".into(),
            identity: "video-fixture".into(),
            online: true,
            ..Default::default()
        };
        let mut draft = workspace::Draft::new(true, "library".into(), Default::default());
        draft.change_source(source.input.clone());
        draft.source = Some(source.clone());
        draft.submitted_task = Some("original".into());
        let mut original = workspace::TaskRecord {
            id: "original".into(),
            plan: workspace::TaskPlan {
                operation: Default::default(),
                source_id: source.identity.clone(),
                source,
                title: "已提交的课程".into(),
                library_id: "library".into(),
                folder: None,
                subtitle: None,
                config: planned_config(),
                asr_service: None,
                ai_service: None,
            },
            state: workspace::TaskState::Uncertain,
            intent: workspace::Intent::Pause,
            created: 0,
            updated: 0,
            parent: None,
            handled_by: None,
            held: false,
            work_dir: "work/original".into(),
            events: desktop_store::activity::JobEvents {
                error: Some("原摘要请求结果未确认".into()),
                ..Default::default()
            },
            artifact: Some("note".into()),
            unread: false,
            resend: Vec::new(),
            replaced_by: None,
        };
        let mut recovery = original.clone();
        recovery.id = "recovery".into();
        recovery.parent = Some(original.id.clone());
        recovery.state = workspace::TaskState::Queued;
        recovery.intent = workspace::Intent::Run;
        recovery.events.error = None;
        recovery.artifact = None;
        recovery.work_dir = "work/recovery".into();
        recovery.plan.operation = c2m_engine::execution::Operation::Reprocess {
            note_dir: "note".into(),
            base_task_id: original.id.clone(),
            components: vec![c2m_engine::execution::Component::Summary],
            prior_work_dir: Some(original.work_dir.clone()),
        };
        original.handled_by = Some(recovery.id.clone());
        (draft, vec![original, recovery])
    }

    #[test]
    fn workbench_entry_retires_only_unchanged_terminal_inputs() {
        let (mut draft, mut tasks) = submitted_recovery_chain();
        draft.title = tasks[0].plan.title.clone();
        for state in [
            workspace::TaskState::Complete,
            workspace::TaskState::Partial,
            workspace::TaskState::Cancelled,
        ] {
            tasks[1].state = state;
            tasks[1].artifact = Some("note".into());
            assert_eq!(
                super::retired_input_task(&draft, &tasks, &draft.input)
                    .unwrap()
                    .id,
                "recovery"
            );
        }
        for state in [
            workspace::TaskState::Queued,
            workspace::TaskState::Running,
            workspace::TaskState::Pausing,
            workspace::TaskState::Paused,
            workspace::TaskState::NeedsAttention,
            workspace::TaskState::Uncertain,
        ] {
            tasks[1].state = state;
            assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
        }
        tasks[1].state = workspace::TaskState::Partial;
        let unchanged = draft.clone();
        draft.title = "尚未提交的新名称".into();
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
        draft = unchanged.clone();
        draft.options.keep_video = !draft.options.keep_video;
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
        // Vision without proofreading has no effect, so the plan does not record it.
        draft = unchanged.clone();
        draft.options.vision = !draft.options.vision;
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_some());
        draft = unchanged.clone();
        draft.folder = Some(42);
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
        draft = unchanged;
        assert!(super::retired_input_task(&draft, &tasks, "新视频").is_none());
        tasks[1].events.blocked.push(workspace::BlockedRequest {
            reason: c2m_protocol::progress::BlockReason::Uncertain,
            request_id: Some("pending-summary".into()),
            purpose: Some("summary".into()),
            description: "摘要".into(),
            kind: Some(c2m_artifact::artifact::FailureKind::Uncertain),
            message: "结果未知".into(),
        });
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
        tasks[1].events.blocked.clear();
        tasks[1].artifact = None;
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
    }

    #[test]
    fn workbench_follows_the_same_recovery_for_queued_running_and_completed_results() {
        let (draft, mut tasks) = submitted_recovery_chain();
        let original = tasks[0].clone();
        for state in [workspace::TaskState::Queued, workspace::TaskState::Running] {
            tasks[1].state = state;
            assert_eq!(
                submitted_input_task(&draft, &tasks, &draft.input)
                    .unwrap()
                    .id,
                "recovery"
            );
            assert!(completed_input_task(&draft, &tasks, &draft.input).is_none());
        }
        tasks[1].state = workspace::TaskState::Complete;
        assert!(completed_input_task(&draft, &tasks, &draft.input).is_none());
        tasks[1].artifact = Some("note".into());
        assert_eq!(
            completed_input_task(&draft, &tasks, &draft.input)
                .unwrap()
                .id,
            "recovery"
        );

        let mut final_attempt = tasks[1].clone();
        final_attempt.id = "final-attempt".into();
        final_attempt.parent = Some("recovery".into());
        final_attempt.state = workspace::TaskState::Running;
        final_attempt.artifact = None;
        tasks[1].state = workspace::TaskState::Uncertain;
        tasks[1].handled_by = Some(final_attempt.id.clone());
        tasks.push(final_attempt);
        assert_eq!(
            submitted_input_task(&draft, &tasks, &draft.input)
                .unwrap()
                .id,
            "final-attempt"
        );
        assert!(completed_input_task(&draft, &tasks, &draft.input).is_none());
        tasks[2].state = workspace::TaskState::Complete;
        tasks[2].artifact = Some("note".into());
        assert_eq!(
            completed_input_task(&draft, &tasks, &draft.input)
                .unwrap()
                .id,
            "final-attempt"
        );
        // A cancelled recovery still belongs to this input for inline feedback,
        // but must not fall back to an older readable result or follow new input.
        tasks[2].state = workspace::TaskState::Cancelled;
        tasks[2].artifact = None;
        assert_eq!(
            submitted_input_task(&draft, &tasks, &draft.input)
                .unwrap()
                .id,
            "final-attempt"
        );
        assert!(completed_input_task(&draft, &tasks, &draft.input).is_none());
        assert!(submitted_input_task(&draft, &tasks, "another-video").is_none());
        assert!(tasks[0] == original);
        assert_eq!(draft.submitted_task.as_deref(), Some("original"));
    }

    #[test]
    fn recovery_lookup_rejects_broken_cycles_or_unrelated_task_links() {
        for case in [
            "missing",
            "self-cycle",
            "cycle",
            "wrong-parent",
            "other-source",
            "inconsistent-source",
            "other-kind",
            "duplicate-id",
            "unrelated-submission",
        ] {
            let (mut draft, mut tasks) = submitted_recovery_chain();
            match case {
                "missing" => tasks[0].handled_by = Some("missing".into()),
                "self-cycle" => {
                    tasks[0].handled_by = Some("original".into());
                    tasks[0].parent = Some("original".into());
                }
                "cycle" => {
                    tasks[1].handled_by = Some("original".into());
                    tasks[0].parent = Some("recovery".into());
                }
                "wrong-parent" => tasks[1].parent = Some("unrelated".into()),
                "other-source" => {
                    tasks[1].plan.source_id = "other-video".into();
                    tasks[1].plan.source.identity = "other-video".into();
                }
                "inconsistent-source" => tasks[1].plan.source.identity = "other-video".into(),
                "other-kind" => tasks[1].plan.source.online = false,
                "duplicate-id" => tasks.push(tasks[1].clone()),
                "unrelated-submission" => {
                    tasks[1].plan.source_id = "other-video".into();
                    tasks[1].plan.source.identity = "other-video".into();
                    draft.submitted_task = Some("recovery".into());
                }
                _ => unreachable!(),
            }
            assert!(
                submitted_input_task(&draft, &tasks, &draft.input).is_none(),
                "{case}"
            );
            assert!(
                completed_input_task(&draft, &tasks, &draft.input).is_none(),
                "{case}"
            );
        }
    }

    #[test]
    fn a_recovered_result_cannot_replace_new_or_changed_input() {
        let (mut draft, mut tasks) = submitted_recovery_chain();
        tasks[1].state = workspace::TaskState::Complete;
        tasks[1].artifact = Some("note".into());
        for input in ["", "https://www.bilibili.com/video/BVnext"] {
            assert!(submitted_input_task(&draft, &tasks, input).is_none());
            assert!(completed_input_task(&draft, &tasks, input).is_none());
        }
        draft.source.as_mut().unwrap().identity = "newly-inspected-content".into();
        assert!(submitted_input_task(&draft, &tasks, &draft.input).is_none());
        draft.source = None;
        draft.input = "https://www.bilibili.com/video/BVnext".into();
        assert!(submitted_input_task(&draft, &tasks, &draft.input).is_none());
    }

    #[test]
    fn automatic_mode_uses_speech_after_login_or_download_failure() {
        let track = c2m_source::subtitle::file_track("lesson.srt".into(), None);
        for metadata_failed in [true, false] {
            let mut source = source::Source {
                subtitles: if metadata_failed {
                    SubtitleEvidence::Failed {
                        message: "WARNING: subtitles require login".into(),
                    }
                } else {
                    SubtitleEvidence::Found {
                        tracks: vec![track.clone()],
                        warning: None,
                    }
                },
                subtitle_request: (!metadata_failed).then(|| track.clone()),
                subtitle_read_error: (!metadata_failed).then(|| SubtitleReadError::Failed {
                    message: "字幕文件没有下载成功".into(),
                }),
                ..Default::default()
            };
            assert!(automatic_subtitle_fallback(
                &mut source,
                TranscriptSource::Auto,
                None
            ));
            assert!(uses_speech(&source, TranscriptSource::Auto));
            assert!(!subtitle_needs_confirmation(
                &source,
                TranscriptSource::Auto
            ));
            assert!(source.subtitle_request.is_none());
            assert!(source.subtitle_read_error.is_none());
        }
    }

    #[test]
    fn explicit_subtitle_and_cancelled_reads_never_fall_back_to_speech() {
        let mut source = source::Source {
            subtitles: SubtitleEvidence::Failed {
                message: "字幕需要登录".into(),
            },
            subtitle_read_error: Some(SubtitleReadError::Failed {
                message: "字幕需要登录".into(),
            }),
            ..Default::default()
        };
        let original = source.clone();
        assert!(!automatic_subtitle_fallback(
            &mut source,
            TranscriptSource::Subtitle,
            None
        ));
        assert_eq!(source, original);
        assert!(subtitle_needs_confirmation(
            &source,
            TranscriptSource::Subtitle
        ));

        source.subtitle_read_error = Some(SubtitleReadError::Cancelled);
        let cancelled = source.clone();
        assert!(!automatic_subtitle_fallback(
            &mut source,
            TranscriptSource::Auto,
            None
        ));
        assert_eq!(source, cancelled);
        assert!(!uses_speech(&source, TranscriptSource::Auto));
    }

    #[test]
    fn pending_or_failed_subtitles_remain_required_even_with_cached_text() {
        let track = c2m_source::subtitle::file_track("lesson.srt".into(), None);
        let mut source = source::Source {
            identity: "lesson".into(),
            subtitles: SubtitleEvidence::Found {
                tracks: vec![track.clone()],
                warning: None,
            },
            ..Default::default()
        };
        assert!(subtitle_needs_confirmation(&source, TranscriptSource::Auto));
        source.selected_subtitle = Some(CachedSubtitle {
            source_identity: source.identity.clone(),
            track_id: track.id.clone(),
            label: "已读取字幕".into(),
            path: "lesson.srt".into(),
            events: Vec::new(),
        });
        assert!(!subtitle_needs_confirmation(
            &source,
            TranscriptSource::Auto
        ));

        source.subtitle_request = Some(track);
        assert!(subtitle_needs_confirmation(&source, TranscriptSource::Auto));
        source.subtitle_request = None;
        source.subtitle_read_error = Some(SubtitleReadError::Failed {
            message: "无法读取新字幕，原正文仍保留".into(),
        });
        assert!(subtitle_needs_confirmation(&source, TranscriptSource::Auto));

        // Cancelling the replacement or choosing the cached text restores the
        // confirmed source without opening unrelated generation preferences.
        source.subtitle_read_error = None;
        assert!(!subtitle_needs_confirmation(
            &source,
            TranscriptSource::Subtitle
        ));
        source.selected_subtitle = None;
        assert!(subtitle_needs_confirmation(
            &source,
            TranscriptSource::Subtitle
        ));
        assert!(!subtitle_needs_confirmation(&source, TranscriptSource::Asr));
    }

    #[test]
    fn caption_intent_and_unconfirmed_reads_never_imply_speech_recognition() {
        let mut source = source::Source {
            subtitles: SubtitleEvidence::NoneFound,
            ..Default::default()
        };
        assert!(uses_speech(&source, TranscriptSource::Auto));
        assert!(!uses_speech(&source, TranscriptSource::Subtitle));
        source.subtitle_request = Some(c2m_source::subtitle::file_track("lesson.srt".into(), None));
        assert!(!uses_speech(&source, TranscriptSource::Auto));
        assert!(!uses_speech(&source, TranscriptSource::Subtitle));
        source.subtitle_request = None;
        source.subtitle_read_error = Some(SubtitleReadError::Failed {
            message: "字幕文件无法读取".into(),
        });
        assert!(!uses_speech(&source, TranscriptSource::Auto));
        assert!(uses_speech(&source, TranscriptSource::Asr));
        source.subtitle_read_error = None;
        source.subtitles = SubtitleEvidence::Unchecked;
        assert!(!uses_speech(&source, TranscriptSource::Auto));
        source.subtitles = SubtitleEvidence::Failed {
            message: "请求失败".into(),
        };
        assert!(uses_speech(&source, TranscriptSource::Auto));
        assert!(!uses_speech(&source, TranscriptSource::Subtitle));
    }
}
