//! One start intent continues source preparation through to a readable note.
use super::*;

/// Hero and preview share this ruler; the lower workbench reflows independently.
const WORKBENCH_WIDTH: f32 = 760.;
/// Spacing between workbench sections and the inset of its panels.
const SECTION_GAP: Rems = rems(24. / 14.);
/// Rhythm inside the hero: title, link pill, drop hint and preview card.
const HERO_GAP: Rems = rems(16. / 14.);
/// Square icon buttons beside the hero input.
const HERO_CONTROL: Rems = rems(48. / 14.);
/// The link field's text, which its placeholder is measured at.
const SOURCE_FIELD_TEXT: Rems = rems(17. / 14.);
/// Option labels and the selected video title.
const TEXT_LEAD: Rems = rems(16. / 14.);
use crate::{motion, preferences::ServicePurpose, theme::*};
use anyhow::Context as _;
use course2md::config::TranscriptSource;
use course2md::subtitle::{SubtitleEvidence, SubtitleReadError, SubtitleTrack};
use gpui_kit::component::{
    checkbox::Checkbox,
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A plain field label above its control.
fn import_option_label(
    id: impl Into<ElementId>,
    label: &'static str,
    content: impl IntoElement,
) -> Div {
    v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .child(
            accessible_text(id, label)
                .text_size(TEXT_AUX)
                .text_color(color(GRAY)),
        )
        .child(content)
}

/// A titled group of the workbench's connected option tiles.
fn option_group(id: &'static str, title: &'static str, tiles: Vec<Div>) -> TileGroup {
    TileGroup::new(id, title)
        .scale(TileScale::Compact)
        .children(tiles)
}

/// A label (with optional hint) on the left and a switch on the right.
fn option_switch_tile(
    id: impl Into<ElementId>,
    label: &'static str,
    hint: Option<&'static str>,
    switch: impl IntoElement,
) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(rems(if hint.is_some() { 68. } else { 60. } / 14.))
        .pl(rems(20. / 14.))
        .pr(rems(16. / 14.))
        .py(rems(12. / 14.))
        .gap(rems(12. / 14.))
        .items_center()
        .child(TileScale::Compact.text(id, label, hint.map(SharedString::from)))
        .child(div().flex_shrink_0().child(switch))
}

/// A queued or running task is shown by `box_running_task` in the video
/// card's place.
fn task_runs_in_card(task: &workspace::TaskRecord) -> bool {
    matches!(
        task.state,
        workspace::TaskState::Queued
            | workspace::TaskState::Running
            | workspace::TaskState::Pausing
    )
}

/// The mark on a cover slot whose video has no cover image, shared by the
/// home page's video card, running task and task rows.
pub(crate) fn cover_placeholder(size: Rems) -> gpui_kit::component::Icon {
    icons::movie()
        .size(size)
        .text_color(color(ON_SECONDARY_CONTAINER))
}

/// Secondary text below a control.
/// The preview action keeps its label until the link field itself runs out of
/// room: the field is the hero's measured width, not the window's breakpoint.
fn preview_field_tight(window: &Window) -> bool {
    let scale = f32::from(window.rem_size()) / 14.;
    let field = crate::views::shell_content_width(Page::New, window) / scale;
    field.min(WORKBENCH_WIDTH) < 520.
}

pub(crate) fn aux_text(content: impl IntoElement) -> Div {
    div()
        .text_size(TEXT_AUX)
        .text_color(color(GRAY))
        .child(content)
}

/// The first line of a message when it is already readable. A raw download
/// tool line (`WARNING:` / `ERROR:`) is not, and the caller supplies a summary.
fn readable_first_line(message: &str) -> Option<&str> {
    let first = message.lines().next().unwrap_or(message);
    (!first.contains("WARNING:") && !first.contains("ERROR:")).then_some(first)
}

fn help(id: &'static str, text: impl Into<SharedString>) -> Div {
    crate::theme::supporting_info(id, text).text_sm()
}
fn issue(id: &'static str, message: impl Into<SharedString>) -> Stateful<Div> {
    error_text(id, message).text_sm().whitespace_normal()
}
/// An element id that follows the text it shows: an entrance replays and an
/// expanded detail closes when a different message takes the same place.
fn text_id(kind: &str, value: &str) -> SharedString {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hash);
    format!("import-{kind}-{:x}", hash.finish()).into()
}

/// One AI processing switch in the processing options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConversionAiOption {
    Proofread,
    Vision,
    Summary,
}

pub(crate) fn conversion_ai_option_label(option: ConversionAiOption) -> &'static str {
    match option {
        ConversionAiOption::Proofread => course2md::tr!("校对文字和标点"),
        ConversionAiOption::Vision => course2md::tr!("校对时附带对应截图"),
        ConversionAiOption::Summary => course2md::tr!("在开头生成摘要"),
    }
}

pub(crate) fn conversion_ai_option_enabled(
    options: &ConversionOptions,
    option: ConversionAiOption,
) -> bool {
    match option {
        ConversionAiOption::Proofread => options.llm,
        ConversionAiOption::Vision => options.vision,
        ConversionAiOption::Summary => options.summarize,
    }
}

pub(crate) fn apply_conversion_ai_option(
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

fn text_source_choices() -> [(TranscriptSource, &'static str); 3] {
    [
        (TranscriptSource::Auto, course2md::tr!("自动")),
        (TranscriptSource::Subtitle, course2md::tr!("仅字幕")),
        (TranscriptSource::Asr, course2md::tr!("语音识别")),
    ]
}

fn text_source_id(mode: TranscriptSource) -> &'static str {
    match mode {
        TranscriptSource::Auto => "auto",
        TranscriptSource::Subtitle => "subtitle",
        TranscriptSource::Asr => "asr",
    }
}

/// A consequence or recovery fact kept beside its action.
pub(crate) fn conversion_fact(
    id: &'static str,
    value: impl Into<SharedString>,
    warning: bool,
) -> Stateful<Div> {
    accessible_text(id, value)
        .w_full()
        .min_w_0()
        .whitespace_normal()
        .text_size(TEXT_BODY)
        .text_color(color(if warning { WARNING } else { INK }))
        .when(warning, |row| row.font_weight(FontWeight::SEMIBOLD))
}

fn uses_speech(source: &source::Source, source_mode: TranscriptSource) -> bool {
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

fn automatic_subtitle_fallback(
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

fn subtitle_needs_confirmation(source: &source::Source, source_mode: TranscriptSource) -> bool {
    !uses_speech(source, source_mode)
        && (source.selected_subtitle.is_none()
            || source.subtitle_request.is_some()
            || source.subtitle_read_error.is_some())
}

/// Follow only this input's submitted recovery chain. A matching URL or the
/// selected queue item alone cannot establish that a task belongs to this form.
fn submitted_input_task<'a>(
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
fn locked_input_task<'a>(
    draft: &workspace::Draft,
    tasks: &'a [workspace::TaskRecord],
    input: &str,
) -> Option<&'a workspace::TaskRecord> {
    submitted_input_task(draft, tasks, input).filter(|task| !task.state.finished())
}

fn completed_input_task<'a>(
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
fn retired_input_task<'a>(
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
pub(crate) enum ConversionFollow {
    Preparing(u64),
    Task {
        id: String,
        source_revision: u64,
    },
    ReaderTask {
        id: String,
        base_version: PathBuf,
        reader_revision: u64,
    },
}

impl ConversionFollow {
    /// Opening a viewer owns the old version until the person closes it. Do not
    /// restore this intent afterwards, even if an in-flight read is still pending.
    pub(crate) fn interrupt_for_reader_viewer(follow: &mut Option<Self>) {
        if matches!(follow, Some(Self::ReaderTask { .. })) {
            *follow = None;
        }
    }

    pub(crate) fn submitted(self, revision: u64, id: String) -> Option<Self> {
        matches!(self, Self::Preparing(current) if current == revision).then_some(Self::Task {
            id,
            source_revision: revision,
        })
    }

    pub(crate) fn follows(&self, id: &str, revision: u64) -> bool {
        matches!(self, Self::Task { id: followed, source_revision }
            if followed == id && *source_revision == revision)
    }

    pub(crate) fn reader_reprocess(
        task: &workspace::TaskRecord,
        version: &std::path::Path,
        revision: u64,
    ) -> Option<Self> {
        let course2md::execution::Operation::Reprocess {
            base_version_dir,
            components,
            ..
        } = &task.plan.operation
        else {
            return None;
        };
        (base_version_dir == version && !components.is_empty() && !task.exports_only()).then(|| {
            Self::ReaderTask {
                id: task.id.clone(),
                base_version: base_version_dir.clone(),
                reader_revision: revision,
            }
        })
    }

    /// Check both before starting an automatic read and after its asynchronous
    /// load. The latter also retains the caller's existing read-generation guard.
    pub(crate) fn context_is_current(
        &self,
        page: Page,
        source_revision: u64,
        reader_revision: u64,
        version: Option<&std::path::Path>,
    ) -> bool {
        match self {
            Self::Preparing(_) => false,
            Self::Task {
                source_revision: expected,
                ..
            } => page == Page::New && *expected == source_revision,
            Self::ReaderTask {
                base_version,
                reader_revision: expected,
                ..
            } => {
                page == Page::Result
                    && *expected == reader_revision
                    && version == Some(base_version.as_path())
            }
        }
    }

    pub(crate) fn completed_reader_task(
        &self,
        task: &workspace::TaskRecord,
        page: Page,
        reader_revision: u64,
        version: Option<&std::path::Path>,
    ) -> bool {
        let Self::ReaderTask {
            id, base_version, ..
        } = self
        else {
            return false;
        };
        task.id == *id
            && self.context_is_current(page, 0, reader_revision, version)
            && Self::reader_reprocess(task, base_version, reader_revision).as_ref() == Some(self)
            && matches!(
                task.state,
                workspace::TaskState::Complete | workspace::TaskState::Partial
            )
            && task
                .artifact
                .as_ref()
                .is_some_and(|path| path != base_version)
    }

    pub(crate) fn completed_task<'a>(
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
enum ConversionGate {
    Wait,
    Stop,
    Submit,
}

/// Normal asynchronous stages retain the first Start intent. Only an abandoned
/// source or an unresolved material choice ends this automatic continuation.
/// What a pending Start depends on.
#[derive(Clone, Copy)]
struct GateInputs {
    requested_revision: Option<u64>,
    current_revision: u64,
    reading: bool,
    has_source: bool,
    needs_text_choice: bool,
    environment_ready: bool,
    library_ready: bool,
    existing_note: bool,
}

fn conversion_gate(gate: GateInputs) -> ConversionGate {
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

impl Desktop {
    /// Explicit confirmation of an already visible preview owns submission.
    pub fn start_conversion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading && self.page == Page::New {
            self.read_generation = self.read_generation.wrapping_add(1);
            self.reading = false;
        }
        let input = self.value(Field::Source, cx);
        if self.draft().is_some_and(|draft| draft.input != input) {
            if !self.prepare_next_import(&input, window, cx) {
                return;
            }
            self.following_conversion = None;
            self.invalidate_source();
        }
        if input.is_empty() {
            self.source_validation = Some(if self.online() {
                course2md::tr!("先粘贴视频链接").into()
            } else {
                course2md::tr!("先选择一个视频").into()
            });
            if self.online() {
                self.inputs[&Field::Source].update(cx, |input, cx| input.focus(window, cx));
            }
            cx.notify();
            return;
        }
        // Preview is a separate user step. Never queue an unseen source,
        // including callers returning from login or selecting a local file.
        if self.source_preview.is_none() {
            self.inspect_source(window, cx);
            return;
        }
        self.pending_conversion = Some(self.preview_generation);
        self.following_conversion = (self.page == Page::New)
            .then_some(ConversionFollow::Preparing(self.preview_generation));
        self.advance_conversion(window, cx);
    }

    pub fn advance_conversion_when_ready(&mut self, cx: &mut Context<Self>) {
        if self.pending_conversion.is_none() {
            return;
        }
        let Some(handle) = cx.windows().first().copied() else {
            return;
        };
        let desktop = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = desktop.update(cx, |desktop, cx| desktop.advance_conversion(window, cx));
            });
        });
    }

    pub fn advance_conversion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(generation) = self.pending_conversion else {
            return;
        };
        let reading = self.preview_cancel.is_some() || self.subtitle_loading;
        let source_mode = self.task_options().source_mode;
        if generation == self.preview_generation
            && !reading
            && let Some(source) = &mut self.source_preview
            && automatic_subtitle_fallback(source, source_mode, self.subtitle_error.as_deref())
        {
            self.subtitle_error = None;
            if !self.save_current_draft(cx) {
                self.pending_conversion = None;
                return;
            }
        }
        match conversion_gate(GateInputs {
            requested_revision: self.pending_conversion,
            current_revision: self.preview_generation,
            reading,
            has_source: self.source_preview.is_some(),
            needs_text_choice: self.subtitle_attention_required(),
            environment_ready: self.environment.is_some(),
            library_ready: !self.loading,
            existing_note: self.existing_source_note().is_some(),
        }) {
            ConversionGate::Wait => cx.notify(),
            ConversionGate::Stop => {
                self.pending_conversion = None;
                cx.notify();
            }
            ConversionGate::Submit => self.enqueue_current(window, cx),
        }
    }

    fn subtitle_issue(&self, kind: &str, message: &str, cx: &mut Context<Self>) -> Div {
        let summary = if source::legacy_login_failure_from_text(message) {
            course2md::tr!("字幕需要登录，或改用语音识别")
        } else {
            readable_first_line(message)
                .unwrap_or(course2md::tr!("无法读取字幕，可重试或改用语音识别"))
        };
        let details = if summary != message { message } else { "" };
        let id = text_id(kind, message);
        let expanded = self.expanded_subtitle_issue.as_ref() == Some(&id);
        v_flex()
            .min_w_0()
            .gap_2()
            .child(help("import-subtitle-issue-summary", summary.to_owned()))
            .when(!details.trim().is_empty(), |view| {
                view.child(
                    quiet(id.clone())
                        .icon(if expanded {
                            crate::icons::chevron_up()
                        } else {
                            crate::icons::chevron_down()
                        })
                        .self_start()
                        .label(if expanded {
                            course2md::tr!("收起技术详情")
                        } else {
                            course2md::tr!("技术详情")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.expanded_subtitle_issue =
                                if expanded { None } else { Some(id.clone()) };
                            cx.notify();
                        })),
                )
                .child(motion::disclosure(
                    text_id("subtitle-technical", message),
                    expanded,
                    v_flex().child(help("import-subtitle-issue-details", details.to_owned())),
                ))
            })
    }

    fn import_base_config(&self) -> course2md::settings::ConfigFile {
        self.workspace
            .as_ref()
            .and_then(|workspace| workspace.state.draft())
            .and_then(|draft| draft.base_config.clone())
            .unwrap_or_else(|| self.preferences.defaults_config())
    }

    pub fn confirm_subtitle(
        &mut self,
        track: SubtitleTrack,
        explicit: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.source_preview.clone() else {
            return;
        };
        let generation = self.stop_subtitle_read();
        let source_generation = self.preview_generation;
        let token = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.draft())
            .map(|draft| (draft.id.clone(), draft.revision));
        let source_id = source.identity.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.subtitle_cancel = Some(cancel.clone());
        self.subtitle_loading = true;
        self.subtitle_error = None;
        if explicit {
            // The requested text source takes effect before I/O. A failed
            // caption read must never silently keep the previous ASR choice.
            self.update_task_options(|o| o.source_mode = TranscriptSource::Subtitle, cx);
        }
        if let Some(source) = &mut self.source_preview {
            source.subtitle_request = Some(track.clone());
            source.subtitle_read_error = None;
        }
        if !self.save_current_draft(cx) {
            self.subtitle_loading = false;
            self.subtitle_cancel = None;
            return;
        }
        self.preview_workers += 1;
        let task = crate::spawn_blocking_io(move || source::read_subtitle(&source, &track, cancel));
        cx.spawn(async move |this, cx| {
            let result = task.recv().await.unwrap_or_else(|_| {
                Err(course2md::subtitle::SubtitleReadError::Failed {
                    message: course2md::tr!("字幕读取意外中断").into(),
                })
            });
            let _ = this.update(cx, |this, cx| {
                this.preview_workers = this.preview_workers.saturating_sub(1);
                if this.subtitle_generation != generation
                    || this.preview_generation != source_generation
                {
                    return;
                }
                if !this
                    .source_preview
                    .as_ref()
                    .is_some_and(|source| source.identity == source_id)
                {
                    return;
                }
                if let Some((id, revision)) = &token
                    && !this
                        .workspace
                        .as_ref()
                        .is_some_and(|workspace| workspace.state.matches_input(id, *revision))
                {
                    return;
                }
                this.subtitle_loading = false;
                this.subtitle_cancel = None;
                match result {
                    Ok(subtitle) => {
                        if let Some(source) = &mut this.source_preview {
                            source.selected_subtitle = Some(subtitle);
                            source.subtitle_request = None;
                            source.subtitle_read_error = None;
                        }
                        if explicit {
                            this.update_task_options(
                                |o| o.source_mode = TranscriptSource::Subtitle,
                                cx,
                            );
                        }
                        this.save_current_draft(cx);
                    }
                    Err(SubtitleReadError::Cancelled) => {
                        this.pending_conversion = None;
                    }
                    Err(error) => {
                        this.subtitle_error = Some(error.to_string());
                        if let Some(source) = &mut this.source_preview {
                            source.subtitle_read_error = Some(error);
                        }
                        this.save_current_draft(cx);
                    }
                }
                this.advance_conversion_when_ready(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_attached_subtitle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.source_preview.clone() else {
            return;
        };
        let source_generation = self.preview_generation;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(course2md::tr!("选择 SRT 或 VTT 字幕").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.preview_generation != source_generation
                    || !this
                        .source_preview
                        .as_ref()
                        .is_some_and(|current| current.identity == source.identity)
                {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            if !course2md::subtitle::is_subtitle_file(&path) {
                                this.subtitle_error =
                                    Some(course2md::tr!("请选择 SRT 或 VTT 字幕文件").into());
                            } else {
                                let track = course2md::subtitle::file_track(path, None);
                                if let Some(source) = &mut this.source_preview {
                                    match &mut source.subtitles {
                                        SubtitleEvidence::Found { tracks, .. } => {
                                            if !tracks
                                                .iter()
                                                .any(|candidate| candidate.id == track.id)
                                            {
                                                tracks.push(track.clone());
                                            }
                                        }
                                        _ => {
                                            source.subtitles = SubtitleEvidence::Found {
                                                tracks: vec![track.clone()],
                                                warning: None,
                                            }
                                        }
                                    }
                                }
                                this.confirm_subtitle(track, true, cx);
                            }
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        this.subtitle_error = Some(course2md::tr!(
                            "无法打开字幕选择器：{}",
                            format!("{error:#}")
                        ))
                    }
                    Err(error) => this.subtitle_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn retry_subtitles(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.source_preview.clone() else {
            return;
        };
        let generation = self.stop_subtitle_read();
        let source_generation = self.preview_generation;
        let identity = source.identity.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.subtitle_cancel = Some(cancel.clone());
        self.subtitle_loading = true;
        self.subtitle_error = None;
        if let Some(source) = &mut self.source_preview {
            source.subtitle_read_error = Some(SubtitleReadError::Failed {
                message: course2md::tr!("字幕重新读取未完成").into(),
            });
        }
        if !self.save_current_draft(cx) {
            self.subtitle_loading = false;
            self.subtitle_cancel = None;
            return;
        }
        let token = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.draft())
            .map(|draft| (draft.id.clone(), draft.revision));
        self.preview_workers += 1;
        let task = crate::spawn_blocking_io(move || source::refresh_subtitles(&source, cancel));
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(course2md::tr!("字幕读取意外中断"))));
            let _ = this.update(cx, |this, cx| {
                this.preview_workers = this.preview_workers.saturating_sub(1);
                if this.subtitle_generation != generation
                    || this.preview_generation != source_generation
                    || token.as_ref().is_some_and(|(id, revision)| {
                        !this
                            .workspace
                            .as_ref()
                            .is_some_and(|workspace| workspace.state.matches_input(id, *revision))
                    })
                    || !this
                        .source_preview
                        .as_ref()
                        .is_some_and(|source| source.identity == identity)
                {
                    return;
                }
                this.subtitle_loading = false;
                this.subtitle_cancel = None;
                match result {
                    Ok(mut evidence) => {
                        if let SubtitleEvidence::Found { tracks, .. } = &mut evidence {
                            let original = this
                                .source_preview
                                .as_ref()
                                .and_then(|source| source.original_language.as_deref());
                            course2md::subtitle::sort_tracks(
                                tracks,
                                &this.preferences.generation().preferred_subtitle_languages,
                                course2md::i18n::lang().code(),
                                original,
                            );
                        }
                        let old_id = this
                            .source_preview
                            .as_ref()
                            .and_then(|source| source.selected_subtitle.as_ref())
                            .map(|subtitle| subtitle.track_id.clone());
                        let track = match &old_id {
                            Some(id) => evidence.tracks().iter().find(|track| &track.id == id),
                            None => evidence.tracks().first(),
                        }
                        .cloned();
                        if old_id.is_some() && track.is_none() {
                            this.subtitle_error = Some(course2md::tr!("原字幕无法重新读取").into());
                        }
                        if let Some(source) = &mut this.source_preview {
                            source.subtitles = evidence;
                            source.subtitle_read_error = this
                                .subtitle_error
                                .clone()
                                .map(|message| SubtitleReadError::Failed { message });
                        }
                        this.save_current_draft(cx);
                        if let Some(track) = track {
                            this.confirm_subtitle(track, false, cx);
                        }
                    }
                    Err(error) => {
                        this.subtitle_error = Some(format!("{error:#}"));
                        if let Some(source) = &mut this.source_preview {
                            source.subtitle_read_error = this
                                .subtitle_error
                                .clone()
                                .map(|message| SubtitleReadError::Failed { message });
                        }
                        this.save_current_draft(cx);
                    }
                }
                this.advance_conversion_when_ready(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Cancel any subtitle read in flight and ignore its late result.
    fn stop_subtitle_read(&mut self) -> u64 {
        if let Some(cancel) = self.subtitle_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.subtitle_generation = self.subtitle_generation.wrapping_add(1);
        self.subtitle_loading = false;
        self.subtitle_generation
    }

    /// Stop reading subtitles and drop the unfinished request.
    fn abort_subtitle_read(&mut self) {
        self.stop_subtitle_read();
        if let Some(source) = &mut self.source_preview {
            source.subtitle_request = None;
            source.subtitle_read_error = None;
        }
    }

    fn use_speech(&mut self, cx: &mut Context<Self>) {
        self.abort_subtitle_read();
        self.subtitle_error = None;
        self.update_task_options(|o| o.source_mode = TranscriptSource::Asr, cx);
        self.advance_conversion_when_ready(cx);
        cx.notify();
    }

    fn use_confirmed_subtitle(&mut self, cx: &mut Context<Self>) {
        if !self
            .source_preview
            .as_ref()
            .is_some_and(|source| source.selected_subtitle.is_some())
        {
            return;
        }
        self.abort_subtitle_read();
        self.subtitle_error = None;
        self.update_task_options(|o| o.source_mode = TranscriptSource::Subtitle, cx);
        self.advance_conversion_when_ready(cx);
        cx.notify();
    }

    fn select_source_input(&mut self, input: String, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs[&Field::Source].update(cx, |state, cx| state.set_value(input, window, cx));
        self.inspect_source(window, cx);
    }

    fn toggle_candidate(&mut self, input: &str, cx: &mut Context<Self>) {
        if !self.selected_candidates.remove(input) {
            self.selected_candidates.insert(input.to_owned());
        }
        cx.notify();
    }

    fn set_candidate_selected(&mut self, input: &str, selected: bool, cx: &mut Context<Self>) {
        if selected {
            self.selected_candidates.insert(input.to_owned());
        } else {
            self.selected_candidates.remove(input);
        }
        cx.notify();
    }

    pub(crate) fn drop_source_files(
        &mut self,
        files: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.online() {
            self.switch_source_kind(false, window, cx);
        }
        match files {
            [path] => {
                self.inputs[&Field::Source].update(cx, |state, cx| {
                    state.set_value(path.display().to_string(), window, cx)
                });
                self.inspect_source(window, cx);
            }
            [] => {}
            _ => {
                self.invalidate_source();
                self.source_collection_title = Some(course2md::tr!("选择要处理的视频").into());
                self.source_candidates = files
                    .iter()
                    .map(|path| source::SourceCandidate {
                        input: path.display().to_string(),
                        title: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        identity: None,
                        duration: None,
                        thumbnail: None,
                    })
                    .collect();
                cx.notify();
            }
        }
    }

    /// Source navigation is separate from the editable surface.
    fn source_kind_tabs(&self, cx: &mut Context<Self>) -> Div {
        div().w_full().flex().justify_center().child(
            SingleChoiceGroup::new("source-kind", course2md::tr!("视频来源"))
                .options([
                    ("online", course2md::tr!("视频链接")),
                    ("local", course2md::tr!("本地文件")),
                ])
                .icon("online", icons::link())
                .icon("local", icons::movie())
                .selected(if self.online() { "online" } else { "local" })
                .on_change(cx.listener(|this, value: &SharedString, window, cx| {
                    this.switch_source_kind(value.as_ref() == "online", window, cx);
                })),
        )
    }

    pub(super) fn subtitle_attention_required(&self) -> bool {
        self.subtitle_loading
            || self.subtitle_error.is_some()
            || self.source_preview.as_ref().is_some_and(|source| {
                subtitle_needs_confirmation(source, self.task_options().source_mode)
            })
    }

    /// The shared path for every processing-option control: record the change
    /// in the draft, check the model it now needs, and continue a confirmed
    /// conversion that was waiting for it.
    fn set_task_option(
        &mut self,
        edit: impl FnOnce(&mut ConversionOptions),
        cx: &mut Context<Self>,
    ) {
        let saved = self.update_task_options(edit, cx);
        if !self.task_options().uses_cloud_provider()
            // The options panel shows this model's status.
            && (self.import_uses_speech()
                || self.task_options().source_mode != TranscriptSource::Subtitle)
        {
            let (provider, model, root) = self.import_model_request();
            self.ensure_model_diagnostic(provider, Some(&model), &root, cx);
        }
        if saved {
            self.advance_conversion_when_ready(cx);
        }
        cx.notify();
    }

    /// Visible field, local file target, and status belonging to this source.
    /// The link field's placeholder names the supported sites when the field,
    /// as last laid out at the current text scale, can hold it whole;
    /// otherwise a shorter one, so it is never cut off mid-word.
    fn fit_source_placeholder(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.inputs[&Field::Source].clone();
        let width = f32::from(input.read(cx).input_bounds().size.width);
        // The field's bounds come from the last layout. After the window or
        // text scale changed (perhaps while another page was shown), they
        // are stale for this frame, so ask for one more.
        let frame = (window.viewport_size(), window.rem_size());
        if self.source_placeholder_fit.replace(frame) != frame {
            window.request_animation_frame();
        }
        let candidates = [
            course2md::tr!("粘贴 YouTube 或 Bilibili 视频链接"),
            course2md::tr!("粘贴视频链接"),
            course2md::tr!("视频链接"),
        ];
        let size = f32::from(SOURCE_FIELD_TEXT.to_pixels(window.rem_size()));
        let chosen = if width <= 0. {
            // Not laid out yet: the full text, refined on the next frame.
            candidates[0]
        } else {
            candidates
                .into_iter()
                .find(|text| shaped_width(text, size, FontWeight::NORMAL, window) + 2. <= width)
                .unwrap_or(candidates[2])
        };
        if input.read(cx).presentation().placeholder().as_ref() != chosen {
            input.update(cx, |state, cx| state.set_placeholder(chosen, window, cx));
        }
    }

    pub fn box_source_input(
        &self,
        preview_extra: Option<Div>,
        options: Option<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut view = v_flex().w_full().min_w_0().gap(HERO_GAP);
        self.fit_source_placeholder(window, cx);
        let focused = self.inputs[&Field::Source]
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if self.online() {
            let validation = self
                .source_preview
                .is_none()
                .then(|| self.source_validation.clone())
                .flatten();
            let filled = !self.value(Field::Source, cx).trim().is_empty();
            let failed = validation.is_some() || self.preview_error.is_some();
            view = view.child(
                v_flex()
                    .gap_2()
                    // One pill owns the field and its preview action, so the
                    // action reads as part of the input.
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .min_h(rems(72. / 14.))
                            .py(rems(8. / 14.))
                            .pl(rems(24. / 14.))
                            .pr(rems(8. / 14.))
                            .gap(rems(12. / 14.))
                            .items_center()
                            .rounded(RADIUS_PILL)
                            .bg(color(SURFACE))
                            // The field's own ring is off inside the pill; the pill
                            // draws a 2px ring while focused or filled. The ring is
                            // always laid out so the text never moves.
                            .border_2()
                            .border_color(color(if failed {
                                DANGER
                            } else if focused || filled {
                                ACCENT
                            } else {
                                SURFACE
                            }))
                            .child(
                                Icon::new(icons::link())
                                    .size(rems(22. / 14.))
                                    .flex_shrink_0()
                                    .text_color(color(GRAY)),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    text_input(&self.inputs[&Field::Source])
                                        .appearance(false)
                                        .aria_label(course2md::tr!("视频链接"))
                                        .w_full()
                                        .min_w_0()
                                        .px_0()
                                        .text_size(SOURCE_FIELD_TEXT),
                                ),
                            )
                            .when(failed, |row| {
                                row.child(
                                    icons::warning()
                                        .size(rems(22. / 14.))
                                        .flex_shrink_0()
                                        .text_color(color(DANGER)),
                                )
                            })
                            .child(self.preview_source_button(preview_field_tight(window), cx)),
                    )
                    .when_some(validation, |view, message| {
                        view.child(issue("import-validation", message).px(rems(24. / 14.)))
                    }),
            );
            view = view.child(self.local_upload_hint(cx));
        } else {
            view = self.local_source_input(view, cx);
        }
        if self.preview_cancel.is_some() {
            // The reading state keeps the preview card's geometry so the result
            // replaces it in place.
            let bar = |width: f32, height: f32| {
                div()
                    .w(relative(width))
                    .h(rems(height / 14.))
                    .rounded_full()
                    .bg(color(SURFACE_HIGH))
            };
            view = view.child(motion::state_enter(
                "source-reading-enter",
                v_flex()
                    .w_full()
                    .min_w_0()
                    .p(rems(16. / 14.))
                    .gap(rems(16. / 14.))
                    .rounded(RADIUS_HERO)
                    .bg(color(SURFACE))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .flex_wrap()
                            .gap(rems(20. / 14.))
                            .child(
                                div()
                                    .w(rems(if is_compact(window) { 200. } else { 256. } / 14.))
                                    .max_w_full()
                                    .flex_shrink_0()
                                    .aspect_ratio(16. / 9.)
                                    .rounded(px(18.))
                                    .bg(color(SECONDARY_CONTAINER)),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(rems(240. / 14.))
                                    .gap(rems(12. / 14.))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_start()
                                            .gap_2()
                                            .child(
                                                accessible_text(
                                                    "import-reading-state",
                                                    course2md::tr!("正在读取视频信息…"),
                                                )
                                                .role(Role::Status)
                                                .flex_1()
                                                .min_w_0()
                                                .text_size(TEXT_LEAD)
                                                .line_height(rems(28. / 14.))
                                                .text_color(color(GRAY)),
                                            )
                                            .child(
                                                icon_button(
                                                    "cancel-source-read",
                                                    CONTROL_HEIGHT,
                                                    false,
                                                    cx,
                                                )
                                                .icon(crate::icons::close())
                                                .accessibility_label(course2md::tr!("取消读取视频"))
                                                .tooltip(course2md::tr!("取消读取视频"))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.invalidate_source();
                                                    cx.notify();
                                                })),
                                            ),
                                    )
                                    .child(bar(0.72, 14.))
                                    .child(bar(0.45, 12.)),
                            ),
                    )
                    .child(motion::indeterminate_progress(
                        "source-reading-progress",
                        cx,
                    )),
            ));
        } else if self.preview_card_shown(cx) {
            // The options tile joins the bottom of the card it applies to.
            view = view.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(TILE_GAP)
                    .child(self.box_selected_video(preview_extra, options.is_some(), window, cx))
                    .children(options),
            );
        }
        if let Some(title) = &self.source_collection_title {
            view = self.candidate_list_view(view, title, cx);
        }
        view = self.source_preview_error(view, cx);
        view
    }

    fn local_upload_hint(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .justify_center()
            .items_center()
            .flex_wrap()
            .gap(rems(8. / 14.))
            .text_color(color(GRAY))
            .child(icons::file_upload().size(rems(18. / 14.)))
            .child(
                accessible_text("import-drop-hint", course2md::tr!("或把本地视频拖进窗口"))
                    .text_size(TEXT_BODY),
            )
            .child(
                text_button("choose-local-video", cx)
                    .h(rems(36. / 14.))
                    .min_h(rems(36. / 14.))
                    .label(course2md::tr!("选择文件"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.switch_source_kind(false, window, cx);
                        this.pick_video(window, cx);
                    })),
            )
    }

    fn candidate_has_note(&self, candidate: &source::SourceCandidate) -> bool {
        candidate
            .identity
            .as_deref()
            .is_some_and(|identity| self.notes_for_source(identity).next().is_some())
    }

    /// Candidates that can be checked for a batch: identified, without a note yet.
    fn selectable_candidates(&self) -> impl Iterator<Item = &source::SourceCandidate> {
        let batch = self.online();
        self.source_candidates.iter().filter(move |candidate| {
            batch && candidate.identity.is_some() && !self.candidate_has_note(candidate)
        })
    }

    /// The candidate list: online sources queue several at once; several dropped local files stay single-choice.
    /// Only candidates with a confirmed identity can be checked; the rest open one at a time.
    fn candidate_list_view(&self, view: Div, title: &str, cx: &mut Context<Self>) -> Div {
        let mut view = view;
        // Online lists queue several at once; dropped local files stay single-choice (a local identity needs a
        // content hash, read and confirmed one by one). Only identified candidates can be checked.
        let batch = self.online();
        // One pass over the candidates: whether each already has a note, and
        // the counts the header shows.
        let mut noted = Vec::with_capacity(self.source_candidates.len());
        let (mut selectable, mut selectable_selected, mut noted_selected) = (0, 0, 0);
        let mut any_batchable = false;
        for candidate in &self.source_candidates {
            let has_note = self.candidate_has_note(candidate);
            let selected = self.selected_candidates.contains(&candidate.input);
            any_batchable |= candidate.identity.is_some();
            if batch && candidate.identity.is_some() && !has_note {
                selectable += 1;
                selectable_selected += usize::from(selected);
            }
            // A checked candidate that already has a note gets a new version; say so beside the action.
            noted_selected += usize::from(has_note && selected);
            noted.push(has_note);
        }
        let selected_count = self.selected_candidates.len();
        let all_selected = selectable > 0 && selectable_selected == selectable;
        let list = v_flex()
            .id("source-candidates")
            // The viewport scales with the text (about three rows and a glimpse of the fourth); the padding keeps
            // a row's hover/press layer off the container's hairline.
            .max_h(rems(20.))
            .overflow_y_scroll();
        let mut list = crate::focus_scroll::contain_scroll(list, &self.candidate_scroll)
            .p_1()
            .rounded(RADIUS_CARD)
            .border_1()
            .border_color(color(CARD_LINE));
        for (index, candidate) in self.source_candidates.iter().enumerate() {
            let input = candidate.input.clone();
            let untitled = candidate.title.trim().is_empty()
                || candidate.title.trim() == candidate.input.trim();
            let title = if untitled {
                course2md::tr!("视频 {}", index + 1)
            } else {
                candidate.title.clone()
            };
            let (preview, text) = self.candidate_row_parts(candidate, &title, untitled);
            let batchable = batch && candidate.identity.is_some();
            if batch {
                // The same multi-select pattern as the export row: the checkbox is clickable itself (Tab and Space
                // toggle it), the preview/title area is a sibling target, and rows no longer nest two on_click handlers.
                // Unidentified candidates cannot be checked: the row shows an arrow and opens the single-source flow.
                let mut content = h_flex()
                    .id(("source-candidate-text", index))
                    .flex_1()
                    .min_w_0()
                    .gap(px(12.))
                    .items_center()
                    .cursor_pointer();
                if batchable {
                    let toggle_input = candidate.input.clone();
                    content =
                        content.on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_candidate(&toggle_input, cx)
                        }));
                } else {
                    content = content.on_click(cx.listener(move |this, _, window, cx| {
                        this.select_source_input(input.clone(), window, cx)
                    }));
                }
                content = content.child(preview).child(text);
                if batchable {
                    content = content.when(noted[index], |row| {
                        row.child(badge(BadgeKind::Success).child(course2md::tr!("已有笔记")))
                    });
                } else {
                    content =
                        content.child(icons::arrow_forward().size(rems(18. / 14.)).flex_shrink_0());
                }
                let mut row = h_flex()
                    .id(("source-candidate", index))
                    .w_full()
                    .min_w_0()
                    .gap(px(12.))
                    .items_center()
                    .py_3()
                    .px_2()
                    .rounded(RADIUS_SMALL)
                    .hover(|style| style.bg(color(INSET)));
                if batchable {
                    let selected = self.selected_candidates.contains(&candidate.input);
                    let check_input = candidate.input.clone();
                    row = row.child(
                        Checkbox::new(("source-candidate-check", index))
                            .debug_selector(move || format!("source-candidate-checkbox-{index}"))
                            .accessibility_label(course2md::tr!("选择 {}", title))
                            .checked(selected)
                            .flex_shrink_0()
                            .on_click(cx.listener(move |this, value: &bool, _, cx| {
                                this.set_candidate_selected(&check_input, *value, cx)
                            })),
                    );
                }
                list = list.child(row.child(content));
            } else {
                list = list.child(
                    quiet(("source-candidate", index))
                        .w_full()
                        .h_auto()
                        .py_3()
                        .rounded(RADIUS_SMALL)
                        .justify_start()
                        .tooltip(candidate.input.clone())
                        .accessibility_label(course2md::tr!("选择 {}", title))
                        .child(preview)
                        .child(text)
                        .child(icons::arrow_forward().size(rems(18. / 14.)).flex_shrink_0())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_source_input(input.clone(), window, cx)
                        })),
                );
            }
        }
        view = view.child(motion::enter(
            text_id("source-candidates", title),
            v_flex()
                .gap_3()
                .child(
                    accessible_text("import-collection-title", title)
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(list)
                .when(batch && any_batchable, |view| {
                    view.child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .min_w_0()
                                    .child(
                                        quiet("toggle-all-candidates")
                                            .label(if all_selected {
                                                course2md::tr!("全不选")
                                            } else {
                                                course2md::tr!("全选")
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if all_selected {
                                                    this.selected_candidates.clear();
                                                } else {
                                                    let inputs: Vec<String> = this
                                                        .selectable_candidates()
                                                        .map(|c| c.input.clone())
                                                        .collect();
                                                    this.selected_candidates.extend(inputs);
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .when(noted_selected > 0, |row| {
                                        row.child(
                                            div()
                                                .min_w_0()
                                                .text_size(TEXT_AUX)
                                                .text_color(color(WARNING))
                                                .text_ellipsis()
                                                .child(course2md::tr!(
                                                    "{} 个已有笔记，将生成新版",
                                                    noted_selected
                                                )),
                                        )
                                    }),
                            )
                            .child(
                                primary_pill("start-batch-conversion")
                                    .icon(icons::arrow_forward())
                                    .label(if selected_count > 0 {
                                        course2md::tr!("整理选中的 {} 个视频", selected_count)
                                    } else {
                                        course2md::tr!("整理选中的视频").to_owned()
                                    })
                                    // An empty selection cannot run, as with 开始转换.
                                    .disabled(selected_count == 0)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.enqueue_selected_candidates(cx)
                                    })),
                            ),
                    )
                }),
        ));
        view
    }

    /// The candidate rows' shared leading preview and title; batch and single rows differ only in container and click.
    fn candidate_row_parts(
        &self,
        candidate: &source::SourceCandidate,
        title: &str,
        untitled: bool,
    ) -> (Div, Div) {
        let cover = self.source_candidate_covers.get(&candidate.input).cloned();
        // One leading preview slot: the cached first frame or cover,
        // or a movie icon until one is ready, so every row's text starts on the same ruler.
        let preview = div()
            .w(rems(5.6))
            .h(rems(3.15))
            .flex_shrink_0()
            .rounded(RADIUS_SMALL)
            .overflow_hidden()
            .bg(color(INSET))
            .flex()
            .items_center()
            .justify_center()
            .when_some(cover, |slot, cover| {
                slot.child(img(cover).w_full().h_full().object_fit(ObjectFit::Cover))
            })
            .when(
                !self.source_candidate_covers.contains_key(&candidate.input),
                |slot| slot.child(icons::movie().size(rems(18. / 14.)).text_color(color(GRAY))),
            );
        let text = v_flex()
            .flex_1()
            .min_w_0()
            .items_start()
            .gap_1()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_ellipsis()
                    .line_clamp(2)
                    .font_weight(FontWeight::MEDIUM)
                    // The title is the row's scanning anchor: primary ink.
                    .text_color(color(INK))
                    .child(title.to_owned()),
            )
            .when(untitled, |view| {
                view.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_size(TEXT_AUX)
                        .text_color(color(GRAY))
                        .text_ellipsis()
                        .child(candidate.input.clone()),
                )
            })
            .when_some(
                candidate
                    .duration
                    .filter(|value| *value > 0. && value.is_finite() && !untitled),
                |view, seconds| {
                    view.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_size(TEXT_AUX)
                            .text_color(color(GRAY))
                            .text_ellipsis()
                            .child(course2md::render::fmt_ts(seconds)),
                    )
                },
            );
        (preview, text)
    }

    /// Keep source failures beside the field, with retry/login/details still reachable.
    fn source_preview_error(&self, view: Div, cx: &mut Context<Self>) -> Div {
        let mut view = view;
        if let Some(error) = &self.preview_error
            && !self.source_candidates.is_empty()
        {
            view = view.child(help("import-source-error", error.clone()));
        }
        if let Some(error) = &self.preview_error
            && self.source_candidates.is_empty()
        {
            let details = motion::disclosure(
                "source-error-disclosure",
                self.show_preview_details,
                v_flex().child(help("import-source-error-details", error.clone())),
            );
            view = view.child(motion::state_enter(
                text_id("source-error", error),
                v_flex()
                    .w_full()
                    .min_w_0()
                    .p(rems(20. / 14.))
                    .gap_3()
                    .rounded(RADIUS_HERO)
                    .bg(color(SURFACE))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                crate::icons::error()
                                    .size(rems(20. / 14.))
                                    .text_color(color(DANGER))
                                    .flex_shrink_0(),
                            )
                            .child(
                                issue(
                                    "import-login-failure",
                                    if source::legacy_login_failure_from_text(error) {
                                        course2md::tr!("视频需要登录后读取，请登录后重试预览")
                                            .to_owned()
                                    } else {
                                        readable_first_line(error)
                                            .unwrap_or(course2md::tr!(
                                                "暂时无法读取这个视频，请检查链接或稍后重试"
                                            ))
                                            .to_owned()
                                    },
                                )
                                .flex_1()
                                .min_w_0(),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                outline_pill("retry-source", cx)
                                    .icon(icons::refresh())
                                    .label(course2md::tr!("重试预览"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.inspect_source(window, cx)
                                    })),
                            )
                            .when(
                                self.online()
                                    && source::legacy_login_failure_from_text(error)
                                    && course2md::auth::is_bilibili_url(
                                        &self.value(Field::Source, cx),
                                    ),
                                |row| {
                                    row.child(
                                        outline_pill("source-login-repair", cx)
                                            .icon(icons::bilibili())
                                            .label(course2md::tr!("登录 Bilibili"))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_account_dialog(window, cx)
                                            })),
                                    )
                                },
                            )
                            .child(
                                quiet("source-error-details")
                                    .icon(if self.show_preview_details {
                                        crate::icons::chevron_up()
                                    } else {
                                        crate::icons::chevron_down()
                                    })
                                    .label(if self.show_preview_details {
                                        course2md::tr!("收起技术详情")
                                    } else {
                                        course2md::tr!("技术详情")
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_preview_details = !this.show_preview_details;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(details),
            ));
        }
        view
    }

    /// The local source input: a drop zone while empty, a file card once a file is chosen.
    fn local_source_input(&self, view: Div, cx: &mut Context<Self>) -> Div {
        let mut view = view;
        let input = self.value(Field::Source, cx);
        if input.is_empty() {
            view = view.child(
                v_flex()
                    .id("video-drop-zone")
                    .gap_4()
                    .p(rems(32. / 14.))
                    .items_center()
                    .rounded(RADIUS_HERO)
                    .bg(color(SURFACE))
                    .border_2()
                    .border_dashed()
                    .border_color(color(CONTROL))
                    .child(
                        icons::file_upload()
                            .size(rems(32. / 14.))
                            .text_color(color(ACCENT)),
                    )
                    .child(
                        accessible_text("import-drop-instruction", course2md::tr!("拖入本地视频"))
                            .text_size(TEXT_BODY)
                            .font_weight(FontWeight::MEDIUM),
                    )
                    .child(
                        primary_pill("choose-video")
                            .icon(crate::icons::folder_open())
                            .label(course2md::tr!("选择视频"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick_video(window, cx)),
                            ),
                    )
                    .on_drop(
                        cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                            this.drop_source_files(paths.paths(), window, cx)
                        }),
                    ),
            );
        } else if self.source_preview.is_none() {
            // Once previewed, the preview card names the file and replaces it.
            let filename = PathBuf::from(&input)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| input.clone());
            view = view.child(
                h_flex()
                    .id("selected-local-video")
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .flex_wrap()
                    .gap_3()
                    .py(rems(12. / 14.))
                    .pl(rems(24. / 14.))
                    .pr(rems(12. / 14.))
                    .rounded(RADIUS_HERO)
                    .bg(color(SURFACE))
                    .child(
                        semantic_label(
                            "selected-local-video-name",
                            filename,
                            icons::movie()
                                .size(rems(20. / 14.))
                                .text_color(color(ACCENT)),
                        )
                        .flex_1()
                        .min_w(rems(18.))
                        .max_w_full(),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .items_center()
                            .child(
                                outline_pill("choose-video", cx)
                                    .icon(crate::icons::folder_open())
                                    .label(course2md::tr!("更换视频"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.pick_video(window, cx)
                                    })),
                            )
                            // Once the preview is on screen its own 「开始转换」
                            // is the one primary action.
                            .when(
                                self.can_start_input(cx) && self.source_preview.is_none(),
                                |row| row.child(self.preview_source_button(false, cx)),
                            ),
                    )
                    .on_drop(
                        cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                            this.drop_source_files(paths.paths(), window, cx)
                        }),
                    ),
            );
            if self.source_preview.is_none()
                && let Some(error) = &self.source_validation
            {
                view = view.child(issue("import-preview-error", error.clone()));
            }
        }
        view
    }

    /// The previewed video: cover, identity, text-source status and the one
    /// action that submits it. Decisions about this source (`extra`) stay
    /// inside the same card, beside the action they affect.
    fn box_selected_video(
        &self,
        extra: Option<Div>,
        attached: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(source) = &self.source_preview else {
            return v_flex();
        };
        let compact = is_compact(window);
        let duration = (source.duration.is_finite() && source.duration > 0.)
            .then(|| course2md::render::fmt_ts(source.duration));
        let pending = source.subtitle_request.is_some()
            || source.subtitle_read_error.is_some()
            || self.subtitle_error.is_some();
        let speech = self.import_uses_speech();
        let confirmed_subtitle =
            !self.subtitle_loading && !pending && !speech && source.selected_subtitle.is_some();
        let subtitle_status = if self.subtitle_loading {
            badge(BadgeKind::Progress).child(course2md::tr!("正在读取字幕…"))
        } else if pending {
            badge(BadgeKind::Warning).child(course2md::tr!("文字来源待确认"))
        } else if speech {
            badge(BadgeKind::Tonal)
                .child(icons::mic().size(rems(16. / 14.)))
                .child(course2md::tr!("将使用语音识别"))
        } else if confirmed_subtitle {
            badge(BadgeKind::Success).child(course2md::tr!("有字幕，将直接使用"))
        } else {
            badge(BadgeKind::Warning).child(course2md::tr!("文字来源待确认"))
        };
        let platform = if !source.online {
            course2md::tr!("本地视频").to_owned()
        } else {
            match crate::views::video_site_name(&source.input) {
                Some(name) => name.to_owned(),
                None => url::Url::parse(&source.input)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned))
                    .unwrap_or_default(),
            }
        };
        let metadata = [platform, source.author.clone()]
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let cover_radius = px(18.);
        let mut cover = cover_frame(cover_radius, source.cover.clone())
            .relative()
            .w(rems(if compact { 200. / 14. } else { 256. / 14. }))
            .max_w_full()
            .self_start();
        if source.cover.is_none() {
            cover = cover.child(cover_placeholder(rems(32. / 14.)));
        }
        if let Some(duration) = duration {
            cover = cover.child(
                div()
                    .absolute()
                    .right(px(10.))
                    .bottom(px(10.))
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .bg(color(INK))
                    .text_color(color(CANVAS))
                    .text_size(rems(12. / 14.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(duration),
            );
        }
        let details = v_flex()
            .gap_3()
            .pt(rems(16. / 14.))
            .child(help("import-source-input", source.input.clone()))
            .when(
                source.online && course2md::auth::is_bilibili_url(&source.input),
                |view| view.child(self.source_account_row(cx)),
            );
        let can_start = self.can_start_input(cx);
        let start_disabled = self.pending_conversion.is_some() || self.preview_cancel.is_some();
        let actions =
            h_flex()
                .w_full()
                .min_w_0()
                .mt_auto()
                .pt(rems(6. / 14.))
                .gap(rems(8. / 14.))
                .items_center()
                .flex_wrap()
                .when(self.online(), |row| {
                    row.child(
                        icon_button("reread-source", CONTROL_HEIGHT, false, cx)
                            .icon(icons::refresh())
                            .accessibility_label(course2md::tr!("重新读取"))
                            .tooltip(course2md::tr!("重新读取"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.inspect_source(window, cx)),
                            ),
                    )
                })
                .child(div().flex_1())
                // The trailing actions wrap together and stay at the end.
                .child(
                    h_flex()
                        .ml_auto()
                        .gap(rems(8. / 14.))
                        .items_center()
                        .child(
                            outline_pill("replace-source", cx)
                                .h(BUTTON_MEDIUM)
                                .min_h(BUTTON_MEDIUM)
                                .px(rems(20. / 14.))
                                .label_text_size(rems(15. / 14.))
                                .label(course2md::tr!("更换视频"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if this.online() {
                                        // A new link replaces this preview once it is read.
                                        this.inputs[&Field::Source]
                                            .update(cx, |input, cx| input.focus(window, cx));
                                    } else {
                                        this.pick_video(window, cx);
                                    }
                                })),
                        )
                        .when(can_start, |row| {
                            row.child(
                                theme::disabled_when(
                                    primary_pill("start-conversion"),
                                    start_disabled,
                                    cx,
                                )
                                .h(BUTTON_LARGE)
                                .min_h(BUTTON_LARGE)
                                .pl(rems(22. / 14.))
                                .pr(rems(28. / 14.))
                                .gap(rems(10. / 14.))
                                .label_text_size(TEXT_LEAD)
                                .icon(icons::play_arrow())
                                .label(course2md::tr!("开始转换"))
                                .track_focus(&self.import_submit_focus)
                                .disabled(start_disabled)
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.start_conversion(window, cx),
                                )),
                            )
                        }),
                );
        let info = v_flex()
            .flex_1()
            .min_w(rems(240. / 14.))
            .gap(rems(10. / 14.))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                accessible_text("import-selected-title", source.title.clone())
                                    .text_size(rems(20. / 14.))
                                    .line_height(rems(28. / 14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .whitespace_normal()
                                    .text_ellipsis()
                                    .line_clamp(2),
                            )
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .items_center()
                                    .when(!metadata.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .min_w_0()
                                                .text_size(TEXT_BODY)
                                                .text_color(color(GRAY))
                                                .child(metadata),
                                        )
                                    })
                                    .child(
                                        input_action("source-details")
                                            .icon(if self.show_preview_details {
                                                crate::icons::chevron_up()
                                            } else {
                                                crate::icons::chevron_down()
                                            })
                                            .accessibility_label(course2md::tr!("来源详情"))
                                            .tooltip(course2md::tr!("来源详情"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.show_preview_details =
                                                    !this.show_preview_details;
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        icon_button("clear-source-preview", CONTROL_HEIGHT, false, cx)
                            .icon(icons::close())
                            .tooltip(course2md::tr!("移除预览"))
                            .accessibility_label(course2md::tr!("移除预览"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.invalidate_source();
                                this.save_current_draft(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .child(subtitle_status.min_h(px(32.)).px(px(12.))),
            )
            .child(actions);
        v_flex().w_full().min_w_0().child(motion::state_enter(
            "import-selected-source-enter",
            v_flex()
                .id("import-selected-source")
                .w_full()
                .min_w_0()
                .p(rems(16. / 14.))
                .rounded(RADIUS_HERO)
                .when(attached, |card| {
                    card.rounded_bl(TILE_INNER_RADIUS)
                        .rounded_br(TILE_INNER_RADIUS)
                })
                .bg(color(SURFACE))
                .on_drop(
                    cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                        this.drop_source_files(paths.paths(), window, cx)
                    }),
                )
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .items_stretch()
                        .gap(rems(20. / 14.))
                        .child(cover)
                        .child(info),
                )
                .child(motion::disclosure(
                    "source-details-disclosure",
                    self.show_preview_details,
                    details,
                ))
                .when_some(extra, |card, extra| card.child(extra.pt(rems(20. / 14.)))),
        ))
    }

    fn text_source_view(&self, cx: &mut Context<Self>) -> Div {
        let Some(source) = &self.source_preview else {
            return v_flex();
        };
        let speech = self.import_uses_speech();
        let failure = self.subtitle_error.as_deref().or_else(|| {
            if subtitle_needs_confirmation(source, self.task_options().source_mode)
                && let SubtitleEvidence::Failed { message } = &source.subtitles
            {
                Some(message.as_str())
            } else {
                None
            }
        });
        let needs_text_choice =
            subtitle_needs_confirmation(source, self.task_options().source_mode)
                || failure.is_some();
        let description = if self.subtitle_loading {
            course2md::tr!("正在读取字幕…").to_owned()
        } else if !speech
            && (source.subtitle_request.is_some() || source.subtitle_read_error.is_some())
        {
            course2md::tr!("字幕还没读取完成").into()
        } else if speech {
            course2md::tr!("语音识别").into()
        } else if let Some(subtitle) = &source.selected_subtitle {
            // An automatic choice resolves to the current value; like an explicit one, only the subtitle name shows.
            subtitle.label.clone()
        } else {
            match &source.subtitles {
                SubtitleEvidence::Unchecked => course2md::tr!("未检查字幕").into(),
                SubtitleEvidence::Found { .. } => course2md::tr!("请选择字幕").into(),
                SubtitleEvidence::NoneFound | SubtitleEvidence::Unsupported { .. } => {
                    course2md::tr!("尚未选择文字来源").into()
                }
                SubtitleEvidence::Failed { .. } => course2md::tr!("字幕还没读取完成").into(),
            }
        };
        let mut view = v_flex().w_full().min_w_0().gap_3().child(
            h_flex()
                .gap_3()
                .items_center()
                .child(if self.subtitle_loading {
                    motion::spinner("subtitle-reading-spinner", cx)
                } else {
                    (if speech {
                        icons::mic()
                    } else {
                        icons::subtitles()
                    })
                    .size(rems(20. / 14.))
                    .text_color(color(GRAY))
                    .into_any_element()
                })
                .child(
                    accessible_text("import-text-source-state", description)
                        .font_weight(FontWeight::MEDIUM)
                        // The value and its 「更换」 action form one row instead of being pinned to opposite edges by flex_1.
                        .min_w_0()
                        .whitespace_normal(),
                )
                .when(!needs_text_choice, |row| {
                    row.child(
                        quiet("change-text-source")
                            .icon(if self.show_options {
                                crate::icons::chevron_up()
                            } else {
                                crate::icons::chevron_down()
                            })
                            .label(if self.show_options {
                                course2md::tr!("收起")
                            } else {
                                course2md::tr!("更换")
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_options = !this.show_options;
                                cx.notify();
                            })),
                    )
                }),
        );
        if self.subtitle_loading {
            view = view.child(
                quiet("cancel-subtitle-read")
                    .icon(crate::icons::close())
                    .self_start()
                    .label(course2md::tr!("取消读取字幕"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pending_conversion = None;
                        this.abort_subtitle_read();
                        this.save_current_draft(cx);
                        cx.notify();
                    })),
            );
        }
        if let Some(error) = failure {
            let failure_details = self.subtitle_issue("subtitle-read", error, cx);
            view = view.child(motion::enter(
                text_id("subtitle-failure", error),
                v_flex()
                    .gap_3()
                    .p_4()
                    .rounded(RADIUS_CARD)
                    .bg(color(WARNING_BG))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                crate::icons::warning()
                                    .size(rems(20. / 14.))
                                    .text_color(color(WARNING))
                                    .flex_shrink_0(),
                            )
                            .child(
                                accessible_text(
                                    "subtitle-failed",
                                    course2md::tr!("字幕未读取成功"),
                                )
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color(WARNING)),
                            ),
                    )
                    .child(failure_details),
            ));
            if let Some(old) = &source.selected_subtitle {
                view = view.child(
                    outline_pill("use-previous-subtitle", cx)
                        .self_start()
                        .icon(icons::subtitles())
                        .label(course2md::tr!("继续使用之前的字幕"))
                        .tooltip(old.label.clone())
                        .on_click(cx.listener(|this, _, _, cx| this.use_confirmed_subtitle(cx))),
                );
            }
        }
        let show_text_choices = self.show_options || needs_text_choice;
        if speech && !show_text_choices {
            match &source.subtitles {
                SubtitleEvidence::NoneFound => {
                    view = view.child(help("import-no-subtitles", course2md::tr!("未找到字幕")))
                }
                SubtitleEvidence::Unsupported { message } => {
                    view = view.child(help("import-subtitle-message", message.clone()))
                }
                _ => {}
            }
        }
        {
            let mut tracks = source.subtitles.tracks().to_vec();
            if let Some(pending) = &source.subtitle_request
                && !tracks.iter().any(|track| track.id == pending.id)
            {
                tracks.push(pending.clone());
            }
            let cached_id = source
                .selected_subtitle
                .as_ref()
                .map(|subtitle| subtitle.track_id.clone());
            let mut choices: Vec<_> = tracks
                .iter()
                .map(|track| {
                    (
                        format!("track:{}", track.id),
                        course2md::subtitle::track_label(track),
                    )
                })
                .collect();
            if let Some(cached) = &source.selected_subtitle
                && !tracks.iter().any(|track| track.id == cached.track_id)
            {
                choices.push((format!("track:{}", cached.track_id), cached.label.clone()));
            }
            choices.push(("speech".into(), course2md::tr!("语音识别").into()));
            let selected = if speech {
                Some("speech".to_owned())
            } else {
                source
                    .subtitle_request
                    .as_ref()
                    .map(|track| track.id.as_str())
                    .or(cached_id.as_deref())
                    .map(|id| format!("track:{id}"))
            };
            let mut options = v_flex().gap_2().child(
                div().child(
                    SingleChoiceGroup::new("import-text-source", course2md::tr!("笔记的文字来源"))
                        .map(|group| {
                            choices.iter().fold(group, |group, (value, _)| {
                                group.icon(
                                    value.clone(),
                                    if value == "speech" {
                                        icons::mic()
                                    } else {
                                        icons::subtitles()
                                    },
                                )
                            })
                        })
                        .options(choices)
                        .when_some(selected, |group, value| group.selected(value))
                        .on_change(cx.listener(move |this, value: &SharedString, _, cx| {
                            if value.as_ref() == "speech" {
                                this.use_speech(cx);
                            } else if let Some(id) = value.strip_prefix("track:") {
                                if cached_id.as_deref() == Some(id) {
                                    this.use_confirmed_subtitle(cx);
                                } else if let Some(track) =
                                    tracks.iter().find(|track| track.id == id)
                                {
                                    this.confirm_subtitle(track.clone(), true, cx);
                                }
                            }
                        })),
                ),
            );
            if self.subtitle_error.is_none()
                && let SubtitleEvidence::Found {
                    warning: Some(message),
                    ..
                } = &source.subtitles
            {
                options = options.child(self.subtitle_issue(
                    "subtitle-warning",
                    &course2md::tr!("部分字幕无法读取：{}", message),
                    cx,
                ));
            }
            if let SubtitleEvidence::Unsupported { message } = &source.subtitles {
                options = options.child(help("import-option-subtitle-message", message.clone()));
            }
            if matches!(source.subtitles, SubtitleEvidence::NoneFound) {
                options = options.child(help(
                    "import-option-no-subtitles",
                    course2md::tr!("未找到字幕"),
                ));
            }
            options = options.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        outline_pill("retry-subtitles", cx)
                            .icon(icons::refresh())
                            .label(course2md::tr!("重新读取字幕"))
                            .disabled(self.subtitle_loading)
                            .on_click(cx.listener(|this, _, _, cx| this.retry_subtitles(cx))),
                    )
                    .when(!source.online, |view| {
                        view.child(
                            outline_pill("attach-subtitles", cx)
                                .icon(icons::file_upload())
                                .label(course2md::tr!("选择字幕文件"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_attached_subtitle(window, cx)
                                })),
                        )
                    })
                    .when(
                        source.online
                            && course2md::auth::is_bilibili_url(&source.input)
                            && self
                                .subtitle_error
                                .as_deref()
                                .or(match &source.subtitles {
                                    SubtitleEvidence::Failed { message } => Some(message.as_str()),
                                    _ => None,
                                })
                                .is_some_and(source::legacy_login_failure_from_text),
                        |view| {
                            view.child(
                                outline_pill("login-for-subtitles", cx)
                                    .icon(icons::bilibili())
                                    .label(course2md::tr!("登录 Bilibili"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_account_dialog(window, cx)
                                    })),
                            )
                        },
                    ),
            );
            view = view.child(motion::disclosure(
                "text-source-choices",
                show_text_choices,
                options,
            ));
        }
        view
    }

    /// 文字来源: the route selector with a line saying what it will use, and
    /// the speech recognition route beneath it when speech may be used.
    fn import_text_mode_options(&self, window: &mut Window, cx: &mut Context<Self>) -> TileGroup {
        let mode = self.task_options().source_mode;
        let selector =
            SingleChoiceGroup::new("conversion-text-mode", course2md::tr!("文字来源方式"))
                .segmented()
                .options(text_source_choices().map(|(mode, label)| (text_source_id(mode), label)))
                .selected(text_source_id(mode))
                .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                    let Some(mode) = text_source_choices()
                        .iter()
                        .map(|(mode, _)| *mode)
                        .find(|mode| text_source_id(*mode) == value.as_ref())
                    else {
                        return;
                    };
                    if mode == TranscriptSource::Asr {
                        this.use_speech(cx);
                    } else {
                        this.set_task_option(|o| o.source_mode = mode, cx);
                    }
                }));
        // The hint describes the mode; the video card states what this
        // video will actually use.
        let hint = match mode {
            TranscriptSource::Subtitle => course2md::tr!("只使用视频自带的字幕"),
            TranscriptSource::Asr => course2md::tr!("将使用语音识别"),
            TranscriptSource::Auto => {
                course2md::tr!("有字幕时直接用字幕，没有字幕时使用语音识别")
            }
        };
        // Speech settings matter only when this video may need speech:
        // a previewed video whose subtitles will be used does not.
        let speech_relevant = mode == TranscriptSource::Asr
            || (mode == TranscriptSource::Auto
                && self.source_preview.as_ref().is_none_or(|source| {
                    self.subtitle_loading || subtitle_needs_confirmation(source, mode)
                }));
        let mut tiles = vec![
            v_flex()
                .p(rems(16. / 14.))
                .gap(rems(12. / 14.))
                .child(selector)
                .child(
                    div()
                        .px(rems(4. / 14.))
                        .child(supporting_info("conversion-text-mode-hint", hint)),
                ),
        ];
        if speech_relevant {
            tiles.push(
                v_flex()
                    .p(rems(16. / 14.))
                    .child(self.import_speech_options(window, cx)),
            );
        }
        option_group(
            "conversion-text-mode-label",
            course2md::tr!("文字来源"),
            tiles,
        )
    }

    /// AI 处理: one switch tile per step, then the translation target and
    /// the service the text is sent to.
    fn import_ai_options(&self, window: &mut Window, cx: &mut Context<Self>) -> TileGroup {
        let custom_rule = self.task_options().llm
            && self
                .import_base_config()
                .llm
                .prompt
                .as_ref()
                .is_some_and(|prompt| !prompt.trim().is_empty());
        let mut tiles = Vec::new();
        for (index, option) in [
            ConversionAiOption::Proofread,
            ConversionAiOption::Summary,
            ConversionAiOption::Vision,
        ]
        .into_iter()
        .enumerate()
        {
            let hint = (option == ConversionAiOption::Proofread && custom_rule)
                .then(|| course2md::tr!("使用自定义校对规则"));
            tiles.push(option_switch_tile(
                ("import-ai-option-label", index),
                conversion_ai_option_label(option),
                hint,
                processing_switch(Switch::new(("import-ai-option", index)))
                    .checked(conversion_ai_option_enabled(self.task_options(), option))
                    .disabled(option == ConversionAiOption::Vision && !self.task_options().llm)
                    .on_click(cx.listener(move |this, enabled, _, cx| {
                        let enabled = *enabled;
                        this.set_task_option(
                            |o| apply_conversion_ai_option(o, option, enabled),
                            cx,
                        );
                    })),
            ));
        }
        // Bilingual translation is a per-task choice; the global default is only
        // the starting value inherited by this input.
        let translated = self
            .task_options()
            .translate
            .as_deref()
            .and_then(course2md::language::target);
        let translate = self.translation_select(
            "import-ai-translate",
            translated,
            |this, code, cx| {
                this.set_task_option(|o| o.translate = code.map(str::to_owned), cx);
            },
            window,
            cx,
        );
        tiles.push(v_flex().p(rems(16. / 14.)).gap_2().child(translate).when(
            translated.is_some(),
            |tile| {
                tile.child(aux_text(course2md::tr!(
                    "每段文字下方附上译文；已是该语言的视频不翻译"
                )))
            },
        ));
        if self.task_uses_ai() {
            let mut service = v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(self.task_service_picker(ServicePurpose::Ai, window, cx));
            if let Some(selected) = self.selected_task_service(ServicePurpose::Ai) {
                service = service.child(aux_text(course2md::tr!(
                    "{}会发送到 {}",
                    if self.task_options().llm && self.task_options().vision {
                        course2md::tr!("文字及对应截图")
                    } else {
                        course2md::tr!("文字")
                    },
                    selected.config.host()
                )));
            }
            tiles.push(v_flex().p(rems(16. / 14.)).child(
                // The service select carries its own floating label.
                if self.selected_task_service(ServicePurpose::Ai).is_some() {
                    service
                } else {
                    import_option_label(
                        "import-ai-service-label",
                        course2md::tr!("AI 服务"),
                        service,
                    )
                },
            ));
        }
        option_group("import-ai-label", course2md::tr!("AI 处理"), tiles)
    }

    fn reset_task_ai_overrides(&mut self, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        if let Err(error) = workspace.transaction(|state| {
            state
                .draft_mut()
                .context(course2md::tr!("当前视频输入暂时不可用"))?
                .reset_ai_overrides(&defaults);
            Ok(())
        }) {
            self.workspace_error = Some(course2md::tr!(
                "本次选项尚未恢复默认：{}",
                format!("{error:#}")
            ));
            cx.notify();
            return;
        }
        self.advance_conversion_when_ready(cx);
        cx.notify();
    }

    fn import_speech_options(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let cloud = self.task_options().uses_cloud_provider();
        let label = if cloud {
            course2md::tr!("语音识别服务").to_owned()
        } else {
            let (_, model, _) = self.import_model_request();
            course2md::tr!("本机 · {}", course2md::config::model_label(&model))
        };
        let engine = if cloud {
            String::new()
        } else {
            self.local_engine_name().to_owned()
        };
        let current = self.task_options().provider;
        let entity = cx.entity().downgrade();
        let providers: Vec<_> = providers()
            .iter()
            .filter_map(|(id, label)| {
                use course2md::config::AsrProvider;
                let provider = crate::provider_from_id(id);
                // Automatic, CPU and the cloud service are always offered.
                (matches!(provider, None | Some(AsrProvider::Cpu | AsrProvider::Api))
                    || provider == current
                    || provider
                        .zip(self.environment.as_ref())
                        .is_some_and(|(provider, environment)| environment.supports(provider)))
                .then_some((provider, *label))
            })
            .collect();
        let selector = SelectField::new("import-speech-location", label, move |menu, _, _| {
            providers.iter().fold(menu, |menu, &(provider, label)| {
                let entity = entity.clone();
                menu.item(
                    PopupMenuItem::new(label)
                        .checked(provider == current)
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.set_task_option(|o| o.provider = provider, cx);
                            });
                        }),
                )
            })
        })
        .label(course2md::tr!("语音识别"))
        .when(!engine.is_empty(), |field| field.trailing(engine));
        let mut view = v_flex().w_full().min_w_0().gap_2().child(selector);
        if cloud {
            view = view
                .child(self.task_service_picker(ServicePurpose::Speech, window, cx))
                .child(aux_text(course2md::tr!("音频会发送到所选语音识别服务")));
        } else {
            let (provider, model, root) = self.import_model_request();
            let snapshot = self.setup_model_snapshot(provider, Some(&model), &root);
            let ready = snapshot.device_issue.is_none()
                && !snapshot.preparing
                && snapshot.result.as_ref().is_some_and(|result| {
                    result.as_ref().is_ok_and(|status| {
                        matches!(
                            status.state,
                            course2md::models::status::CacheState::Cached
                                | course2md::models::status::CacheState::Loaded
                        )
                    })
                });
            if ready {
                view = view.child(aux_text(course2md::tr!(
                    "模型已下载 · {}",
                    course2md::config::model_label(&model)
                )));
            } else if snapshot.device_issue.is_none()
                && !snapshot.checking
                && !snapshot.preparing
                && snapshot.notice.is_none()
                && !snapshot.cancelled
                && snapshot.result.as_ref().is_some_and(|result| {
                    result.as_ref().is_ok_and(|status| {
                        status.state == course2md::models::status::CacheState::Missing
                            && status.can_prepare
                            && status.last_error.is_none()
                    })
                })
            {
                // A routine first download needs one status line and one action.
                // Checks, failures and active transfers use the status line below.
                let prepare_model = model.clone();
                let prepare_root = root.clone();
                let details = cx.weak_entity();
                view = view
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .flex_wrap()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(TEXT_AUX)
                                    .text_color(color(GRAY))
                                    .child(course2md::tr!(
                                        "{} · 待下载",
                                        course2md::config::model_label(&model)
                                    )),
                            )
                            .child(
                                outline_pill("import-prepare-model", cx)
                                    .icon(icons::download())
                                    .label(course2md::tr!("下载模型"))
                                    .disabled(self.job.is_some())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.prepare_setup_model(
                                            provider,
                                            Some(&prepare_model),
                                            &prepare_root,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                quiet("import-model-details")
                                    .icon(icons::more_vertical())
                                    .w(CONTROL_HEIGHT)
                                    .px_0()
                                    .accessibility_label(course2md::tr!("模型检查与设置"))
                                    .tooltip(course2md::tr!("模型检查与设置"))
                                    .dropdown_menu(move |menu, _, _| {
                                        let recheck = details.clone();
                                        let settings = details.clone();
                                        menu.item(
                                            PopupMenuItem::new(course2md::tr!("重新检查模型"))
                                                .icon(icons::refresh())
                                                .on_click(move |_, _, cx| {
                                                    let _ = recheck.update(cx, |this, cx| {
                                                        this.refresh_model_diagnostics(cx)
                                                    });
                                                }),
                                        )
                                        .item(
                                            PopupMenuItem::new(course2md::tr!("模型设置"))
                                                .icon(icons::settings())
                                                .on_click(move |_, window, cx| {
                                                    let _ = settings.update(cx, |this, cx| {
                                                        this.open_settings(window, cx)
                                                    });
                                                }),
                                        )
                                    }),
                            ),
                    )
                    .when(self.job.is_some(), |view| {
                        view.child(aux_text(course2md::tr!("当前任务完成后才能下载模型")))
                    });
            } else {
                // One quiet status line and its action; Settings keeps the table.
                // On the source hint's ruler.
                view = view.child(div().px(rems(4. / 14.)).child(self.model_status_line(
                    provider,
                    Some(&model),
                    &root,
                    window,
                    cx,
                )));
            }
        }
        view
    }

    fn import_uses_speech(&self) -> bool {
        self.source_preview
            .as_ref()
            .is_some_and(|source| uses_speech(source, self.task_options().source_mode))
    }

    /// Proofreading, a summary or a bilingual translation sends text to the AI service.
    fn task_uses_ai(&self) -> bool {
        self.task_options().llm
            || self.task_options().summarize
            || self
                .task_options()
                .translate
                .as_deref()
                .and_then(course2md::language::target)
                .is_some()
    }

    fn missing_import_service(&self) -> Option<ServicePurpose> {
        let separate_subtitle = self.task_options().source_mode != TranscriptSource::Asr
            && self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.draft())
                .is_some_and(|draft| draft.subtitle.is_some());
        [
            (
                ServicePurpose::Speech,
                self.task_options().uses_cloud_provider()
                    && self.import_uses_speech()
                    && !separate_subtitle,
            ),
            (ServicePurpose::Ai, self.task_uses_ai()),
        ]
        .into_iter()
        .find_map(|(purpose, required)| {
            (required
                && self.selected_task_service(purpose).is_none_or(|version| {
                    self.preferences
                        .service_retired_in_snapshot(&version.service_id)
                }))
            .then_some(purpose)
        })
    }

    fn import_model_request(&self) -> (course2md::config::AsrProvider, String, PathBuf) {
        let provider = self.actual_local_provider();
        let mut config = self.import_base_config();
        // The plan builders' rule: an empty model takes the engine's default (NPU resolves its own).
        crate::task_runner::fill_default_asr_model(&mut config, provider);
        let model = config.defaults.asr_model.unwrap_or_default();
        let root = course2md::config::model_dir_from(config.defaults.model_dir.as_deref());
        (provider, model, root)
    }

    fn actual_local_provider(&self) -> course2md::config::AsrProvider {
        use course2md::config::AsrProvider;
        self.task_options()
            .provider
            .filter(|provider| *provider != AsrProvider::Api)
            .unwrap_or_else(|| self.recommended_local_provider())
    }
    fn local_engine_name(&self) -> &'static str {
        crate::provider_label(Some(self.actual_local_provider()))
    }

    /// 保存: the library, the note's name and folder, extra exports and
    /// whether the original video is kept.
    fn import_destination(&self, cx: &mut Context<Self>) -> TileGroup {
        let current = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.draft())
            .map(|draft| draft.library_id.clone());
        let libraries = self
            .workspace
            .as_ref()
            .map(|workspace| workspace.state.libraries.clone())
            .unwrap_or_default();
        let selected = libraries
            .iter()
            .find(|library| Some(&library.id) == current.as_ref())
            .cloned();
        let library_name: SharedString = selected
            .as_ref()
            .map(|library| library.name.clone().into())
            .unwrap_or_else(|| course2md::tr!("选择保存位置").into());
        let mut tiles: Vec<Div> = Vec::new();
        let library_label = v_flex()
            .flex_1()
            .min_w_0()
            .gap(rems(2. / 14.))
            .child(TileScale::Compact.label("import-library-label", course2md::tr!("笔记库")))
            .child(
                TileScale::Compact
                    .support("import-library-name", library_name.clone())
                    .whitespace_nowrap()
                    .text_ellipsis(),
            );
        if libraries.len() > 1 {
            let entity = cx.entity().downgrade();
            tiles.push(
                div().child(
                    TileScale::Compact
                        .button("import-library", [true, true, false, false], cx)
                        .accessibility_label(course2md::tr!("笔记库"))
                        .child(
                            TileScale::Compact
                                .row()
                                .child(library_label)
                                .child(tile_chevron()),
                        )
                        .dropdown_menu(move |menu, _, _| {
                            libraries.iter().fold(menu, |menu, library| {
                                let id = library.id.clone();
                                let entity = entity.clone();
                                menu.item(
                                    PopupMenuItem::new(format!(
                                        "{} · {}",
                                        library.name,
                                        library.root.display()
                                    ))
                                    .checked(current.as_ref() == Some(&id))
                                    .on_click(
                                        move |_, _, cx| {
                                            let _ = entity.update(cx, |this, cx| {
                                                this.set_import_library(id.clone(), cx)
                                            });
                                        },
                                    ),
                                )
                            })
                        }),
                ),
            );
        } else {
            tiles.push(
                h_flex()
                    .min_h(rems(68. / 14.))
                    .pl(rems(20. / 14.))
                    .pr(rems(16. / 14.))
                    .py(rems(12. / 14.))
                    .items_center()
                    .child(library_label),
            );
        }
        if self.source_preview.is_some() {
            tiles.push(v_flex().p(rems(16. / 14.)).child(import_option_label(
                "import-title-label",
                course2md::tr!("笔记名称"),
                crate::focus_scroll::RevealFocus::new(
                    ("import-title-focus", self.validation_attempt),
                    text_input(&self.inputs[&Field::Title]).w_full(),
                    self.scrolls[Page::New as usize].clone(),
                ),
            )));
        }
        tiles.push(
            h_flex()
                .p(rems(16. / 14.))
                .gap_2()
                // Centered on the outlined field, below its floating label.
                .items_end()
                .child(div().flex_1().min_w_0().child(self.folder_picker(cx)))
                .child(
                    icon_button("import-new-folder", HERO_CONTROL, true, cx)
                        .icon(icons::folder_plus())
                        .mb(rems(4. / 14.))
                        .accessibility_label(course2md::tr!("新建文件夹"))
                        .tooltip(course2md::tr!("新建文件夹"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.begin_folder(None, window, cx)),
                        ),
                ),
        );
        tiles.push(self.import_exports(cx));
        if self.online() {
            tiles.push(option_switch_tile(
                "import-keep-video-label",
                course2md::tr!("保留原视频"),
                Some(course2md::tr!("供离线播放，会占用更多磁盘空间")),
                processing_switch(Switch::new("import-keep-video"))
                    .checked(self.task_options().keep_video)
                    .on_click(cx.listener(|this, value, _, cx| {
                        let value = *value;
                        this.update_task_options(|o| o.keep_video = value, cx);
                        cx.notify();
                    })),
            ));
        }
        option_group("import-save-label", course2md::tr!("保存"), tiles)
    }

    fn set_import_library(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        if let Some(workspace) = &mut self.workspace {
            match workspace.transaction(|state| {
                if let Some(draft) = state.draft_mut() {
                    draft.library_id = id.clone();
                    draft.folder = None;
                }
                Ok(())
            }) {
                Ok(()) => {
                    self.advance_conversion_when_ready(cx);
                }
                Err(error) => {
                    self.workspace_error =
                        Some(course2md::tr!("保存位置尚未更新：{}", format!("{error:#}")))
                }
            }
        }
        cx.notify();
    }

    fn import_exports(&self, cx: &mut Context<Self>) -> Div {
        let mut choices = h_flex().w_full().min_w_0().gap_2().flex_wrap();
        for (index, label) in ["Markdown", course2md::tr!("网页"), "JSON"]
            .into_iter()
            .enumerate()
        {
            choices = choices.child(
                filter_chip(
                    ("import-export", index),
                    self.task_options().formats[index],
                    cx,
                )
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_task_options(|o| o.formats[index] = !o.formats[index], cx);
                    cx.notify();
                })),
            );
        }
        v_flex()
            .pl(rems(20. / 14.))
            .pr(rems(16. / 14.))
            .py(rems(1.))
            .gap(rems(10. / 14.))
            .child(TileScale::Compact.label("import-exports-label", course2md::tr!("同时导出")))
            .child(choices)
    }

    /// The preview card is on screen (not replaced by reading or a task).
    fn preview_card_shown(&self, cx: &App) -> bool {
        self.preview_cancel.is_none()
            && self.source_preview.is_some()
            && self.workbench_linked_task(cx).is_none()
    }

    /// The current input's submitted task while it still owns that input.
    /// Its plan was fixed at submission, so the options are not editable
    /// until the next video replaces the input.
    fn locked_input_task(&self, cx: &App) -> Option<&workspace::TaskRecord> {
        let state = &self.workspace.as_ref()?.state;
        locked_input_task(state.draft()?, &state.tasks, &self.value(Field::Source, cx))
    }

    /// Show or hide the processing options and remember the choice.
    pub(super) fn set_home_options(&mut self, open: bool, cx: &mut Context<Self>) {
        self.desktop_settings.home_options = open;
        let mut preferences = self.application_edit_base();
        preferences.desktop.home_options = open;
        self.commit_application(preferences, cx);
        cx.notify();
    }

    /// Clear the submitted link so the next video starts from the saved
    /// defaults. The submitted task keeps running.
    pub(super) fn set_up_next_video(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The link field's observer prepares the next input and stops
        // following the submitted task.
        self.inputs[&Field::Source].update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.scrolls[Page::New as usize].set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// The submitted task that owns the current input: still in flight, or
    /// shown in the video card's place (its result opening, or a result that
    /// needs attention). Its options were fixed when it was submitted.
    fn owning_task(&self, cx: &App) -> Option<&workspace::TaskRecord> {
        self.locked_input_task(cx)
            .or_else(|| self.workbench_linked_task(cx))
    }

    /// What a submitted task was planned with, read from its saved plan.
    fn submitted_options_summary(&self, task: &workspace::TaskRecord) -> Div {
        let options = task.plan.options();
        let source = crate::task_ui::task_text_source(task);
        let mut ai: Vec<String> = [
            ConversionAiOption::Proofread,
            ConversionAiOption::Summary,
            ConversionAiOption::Vision,
        ]
        .into_iter()
        .filter(|option| conversion_ai_option_enabled(&options, *option))
        .map(|option| conversion_ai_option_label(option).to_owned())
        .collect();
        if options.translate.is_some() {
            ai.push(course2md::tr!("双语翻译").to_owned());
        }
        let ai = if ai.is_empty() {
            course2md::tr!("不使用 AI").to_owned()
        } else {
            ai.join(course2md::tr!("、"))
        };
        let library = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.library(&task.plan.library_id));
        let folder = task
            .plan
            .folder
            .and_then(|id| {
                library
                    .and_then(|library| self.library_indexes.get(&library.root))
                    .and_then(|index| index.folders.get(&id))
                    .cloned()
            })
            .unwrap_or_else(|| course2md::tr!("未分类").to_owned());
        let destination = match library {
            Some(library) => format!("{} · {folder}", library.name),
            None => folder,
        };
        let row = |id: &'static str, label: &'static str, value: String| {
            v_flex()
                .w_full()
                .min_w_0()
                .pl(SETTING_ITEM_INSET)
                .pr(rems(16. / 14.))
                .py(rems(10. / 14.))
                .gap(rems(2. / 14.))
                .child(TileScale::Compact.label(id, label))
                .child(
                    accessible_text(SharedString::from(format!("{id}-value")), value)
                        .text_size(TEXT_BODY)
                        .text_color(color(INK))
                        .whitespace_normal(),
                )
        };
        v_flex()
            .w_full()
            .min_w_0()
            .py(rems(6. / 14.))
            .child(row(
                "submitted-text-source",
                course2md::tr!("文字来源"),
                source.to_owned(),
            ))
            .child(row("submitted-ai", course2md::tr!("AI 处理"), ai))
            .child(row(
                "submitted-destination",
                course2md::tr!("保存到"),
                destination,
            ))
    }

    /// The processing options as one panel: a header row that shows or hides
    /// them, then the options. Under a video card it joins the card's bottom
    /// (2px apart, inner corners), so the options read as that video's.
    /// While a submitted task owns the input, the panel shows what that task
    /// was planned with, read-only, and always offers the next video.
    fn workbench_options(
        &self,
        attached: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.desktop_settings.home_options;
        let owner = self.owning_task(cx).cloned();
        let applies_to: SharedString = match (&owner, &self.source_preview) {
            (Some(_), _) => course2md::tr!("提交时已确定，不能再更改").into(),
            (None, Some(source)) if attached => {
                course2md::tr!("仅对「{}」生效", source.title).into()
            }
            _ => course2md::tr!("仅对下一个视频生效").into(),
        };
        let toggle_label = if open {
            course2md::tr!("收起处理选项")
        } else {
            course2md::tr!("展开处理选项")
        };
        let top = !attached;
        let header_bottom = !open && owner.as_ref().is_none_or(task_runs_in_card);
        let header = TileScale::Compact
            .button(
                "generation-options-toggle",
                [top, top, header_bottom, header_bottom],
                cx,
            )
            .accessibility_label(toggle_label)
            .on_click(cx.listener(move |this, _, _, cx| this.set_home_options(!open, cx)))
            .child(
                TileScale::Compact
                    .row()
                    .child(
                        icons::tune()
                            .size(rems(22. / 14.))
                            .flex_shrink_0()
                            .text_color(color(GRAY)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(rems(2. / 14.))
                            .child(
                                accessible_text(
                                    "generation-options-title",
                                    course2md::tr!("处理选项"),
                                )
                                .role(Role::Heading)
                                .text_size(TileScale::Compact.label_size())
                                .line_height(TileScale::Compact.label_line_height())
                                .font_weight(FontWeight::MEDIUM),
                            )
                            .child(
                                TileScale::Compact
                                    .support("generation-options-applies-to", applies_to)
                                    .text_ellipsis()
                                    .line_clamp(2),
                            ),
                    )
                    .child(
                        if open {
                            icons::chevron_up()
                        } else {
                            icons::chevron_down()
                        }
                        .size(rems(20. / 14.))
                        .flex_shrink_0()
                        .text_color(color(GRAY)),
                    ),
            );
        let divider = || div().h(px(1.)).mx(SETTING_ITEM_INSET).bg(color(HAIRLINE));
        let panel = tile([top, top, true, true])
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header);
        if let Some(task) = owner {
            // The way to the next video stays visible whether or not the
            // summary is open: a running task's card carries it; otherwise it
            // sits right under this header.
            let summary = motion::disclosure(
                "generation-options-body",
                open,
                v_flex()
                    .w_full()
                    .min_w_0()
                    .child(divider())
                    .child(self.submitted_options_summary(&task)),
            );
            if task_runs_in_card(&task) {
                return panel.child(summary);
            }
            let next = TileScale::Compact
                .button("set-up-next-video", [false, false, true, true], cx)
                .on_click(cx.listener(|this, _, window, cx| this.set_up_next_video(window, cx)))
                .child(
                    TileScale::Compact
                        .row()
                        .child(
                            icons::plus()
                                .size(rems(22. / 14.))
                                .flex_shrink_0()
                                .text_color(color(ACCENT)),
                        )
                        .child(TileScale::Compact.text(
                            "set-up-next-video-label",
                            course2md::tr!("设置下一个视频"),
                            Some(course2md::tr!("清空链接，选项恢复为默认设置").into()),
                        )),
                );
            return panel.child(divider()).child(next).child(summary);
        }
        let body = v_flex()
            .w_full()
            .min_w_0()
            .pb(rems(8. / 14.))
            .child(divider())
            .child(
                div()
                    .pt(rems(16. / 14.))
                    .child(self.import_text_mode_options(window, cx)),
            )
            .child(div().py(rems(16. / 14.)).child(divider()))
            .child(self.import_ai_options(window, cx))
            .child(div().py(rems(16. / 14.)).child(divider()))
            .child(self.import_destination(cx))
            .child(
                h_flex().w_full().justify_end().px(rems(8. / 14.)).child(
                    text_button("reset-task-overrides", cx)
                        .icon(icons::restart())
                        .label(course2md::tr!("恢复 AI 默认"))
                        .on_click(cx.listener(|this, _, _, cx| this.reset_task_ai_overrides(cx))),
                ),
            );
        panel.child(motion::disclosure("generation-options-body", open, body))
    }

    /// The current input's own task while it runs, in the video card's place:
    /// cover, title, live status and cancel, with its options attached below.
    fn box_running_task(
        &self,
        task: &workspace::TaskRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let compact = is_compact(window);
        let source = &task.plan.source;
        let mut cover = cover_frame(px(18.), source.cover.clone())
            .w(rems(if compact { 200. / 14. } else { 256. / 14. }))
            .max_w_full()
            .self_start();
        if source.cover.is_none() {
            cover = cover.child(cover_placeholder(rems(32. / 14.)));
        }
        let platform = if !source.online {
            course2md::tr!("本地视频").to_owned()
        } else {
            crate::views::video_site_name(&source.input)
                .map(str::to_owned)
                .unwrap_or_default()
        };
        let metadata = [platform, source.author.clone()]
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let title = if task.plan.title.trim().is_empty() {
            source.title.clone()
        } else {
            task.plan.title.clone()
        };
        let id = task.id.clone();
        let cancelling = task.intent == workspace::Intent::Cancel;
        let details_id = task.id.clone();
        let info = v_flex()
            .flex_1()
            .min_w(rems(240. / 14.))
            .gap(rems(10. / 14.))
            .child(
                h_flex().w_full().min_w_0().items_start().gap_2().child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            accessible_text("running-task-title", title)
                                .text_size(rems(20. / 14.))
                                .line_height(rems(28. / 14.))
                                .font_weight(FontWeight::MEDIUM)
                                .whitespace_normal()
                                .text_ellipsis()
                                .line_clamp(2),
                        )
                        .when(!metadata.is_empty(), |view| {
                            view.child(
                                div()
                                    .text_size(TEXT_BODY)
                                    .text_color(color(GRAY))
                                    .child(metadata),
                            )
                        }),
                ),
            )
            .children(self.task_status_lines(task, "running-task", window, cx))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .mt_auto()
                    .pt(rems(6. / 14.))
                    .gap(rems(8. / 14.))
                    .items_center()
                    .flex_wrap()
                    .child(
                        quiet("running-task-cancel")
                            .h(BUTTON_MEDIUM)
                            .min_h(BUTTON_MEDIUM)
                            .icon(icons::close())
                            .label(course2md::tr!("取消任务"))
                            .disabled(cancelling)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_task_intent(id.clone(), workspace::Intent::Cancel, cx);
                            })),
                    )
                    .child(div().flex_1())
                    // The trailing actions wrap together and stay at the end.
                    .child(
                        h_flex()
                            .ml_auto()
                            .gap(rems(8. / 14.))
                            .items_center()
                            .child(
                                text_button("running-task-details", cx)
                                    .h(BUTTON_MEDIUM)
                                    .min_h(BUTTON_MEDIUM)
                                    .label_text_size(rems(15. / 14.))
                                    .label(course2md::tr!("查看任务"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_task(&details_id, cx);
                                        this.navigate(Page::Task, cx);
                                    })),
                            )
                            // The work continues in the background; the next
                            // video starts from the saved defaults.
                            .child(
                                primary_pill("set-up-next-video")
                                    .h(BUTTON_MEDIUM)
                                    .min_h(BUTTON_MEDIUM)
                                    .px(rems(20. / 14.))
                                    .label_text_size(rems(15. / 14.))
                                    .icon(icons::plus())
                                    .label(course2md::tr!("设置下一个视频"))
                                    .tooltip(course2md::tr!("清空链接，选项恢复为默认设置"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_up_next_video(window, cx)
                                    })),
                            ),
                    ),
            );
        v_flex()
            .w_full()
            .min_w_0()
            .p(rems(16. / 14.))
            .rounded(RADIUS_HERO)
            .bg(color(SURFACE))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_stretch()
                    .gap(rems(20. / 14.))
                    .child(cover)
                    .child(info),
            )
    }

    fn existing_source_note(&self) -> Option<&Course> {
        let source = self.source_preview.as_ref()?;
        let preferred = self
            .workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .state
                    .draft()
                    .and_then(|draft| workspace.state.library(&draft.library_id))
            })
            .map(|library| &library.root);
        self.notes_for_source(&source.identity)
            .min_by_key(|course| !preferred.is_some_and(|root| course.dir.starts_with(root)))
    }

    fn can_start_input(&self, cx: &App) -> bool {
        // While candidates show, batch and single actions live in the list's own action area; the input row's
        // 「开始转换」 would always fail for a collection link, so two primary actions never share the screen.
        self.source_collection_title.is_none()
            && (self.online() || !self.value(Field::Source, cx).is_empty())
            && self.preview_error.is_none()
            && self.existing_source_note().is_none()
            && self.current_input_task(cx).is_none_or(|task| {
                matches!(
                    task.state,
                    workspace::TaskState::Complete | workspace::TaskState::Cancelled
                ) && !(self.reading
                    && self
                        .following_conversion
                        .as_ref()
                        .is_some_and(|follow| follow.follows(&task.id, self.preview_generation)))
            })
    }

    /// The input's own action only reads metadata; conversion starts from
    /// the preview card's 「开始转换」.
    fn preview_source_button(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let disabled = self.preview_cancel.is_some()
            || self.value(Field::Source, cx).is_empty()
            || self.owning_task(cx).is_some();
        let label = course2md::tr!("预览视频");
        let start_shown = self.source_preview.is_some() && self.can_start_input(cx);
        h_flex().flex_shrink_0().items_center().child(
            theme::disabled_when(tonal_pill("preview-source", cx), disabled, cx)
                .h(BUTTON_LARGE)
                .min_h(BUTTON_LARGE)
                .pl(rems(18. / 14.))
                .pr(rems(22. / 14.))
                .label_text_size(rems(15. / 14.))
                .icon(icons::eye())
                .when(!compact, |button| button.label(label))
                .when(compact, |button| button.w(BUTTON_LARGE).px_0())
                .accessibility_label(label)
                .tooltip(label)
                // The start action owns the submit focus once it is on screen.
                .when(!start_shown, |button| {
                    button.track_focus(&self.import_submit_focus)
                })
                .disabled(disabled)
                .on_click(cx.listener(|this, _, window, cx| this.inspect_source(window, cx))),
        )
    }

    pub(super) fn current_input_task(&self, cx: &App) -> Option<&workspace::TaskRecord> {
        let state = &self.workspace.as_ref()?.state;
        submitted_input_task(state.draft()?, &state.tasks, &self.value(Field::Source, cx))
    }

    /// The source card and hero share one definition of the submitted task.
    fn workbench_linked_task(&self, cx: &App) -> Option<&workspace::TaskRecord> {
        let opening_result = self.reading && self.following_conversion.is_some();
        self.current_input_task(cx).filter(|task| {
            (opening_result
                && matches!(
                    task.state,
                    workspace::TaskState::Complete | workspace::TaskState::Partial
                ))
                || !task.state.finished()
                || (task.state == workspace::TaskState::Partial
                    && task.artifact.is_some()
                    && !crate::task_ui::task_component_failures(task).is_empty())
        })
    }

    /// Capture builds only: `COURSE2MD_CAPTURE_SOURCE` fills the link field
    /// once at startup and previews it, so preview states can be rendered.
    #[cfg(feature = "capture")]
    fn capture_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        static DONE: AtomicBool = AtomicBool::new(false);
        let Ok(link) = std::env::var("COURSE2MD_CAPTURE_SOURCE") else {
            return;
        };
        if DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        self.inputs[&Field::Source].update(cx, |state, cx| state.set_value(link, window, cx));
        self.inspect_source(window, cx);
    }

    /// Called by explicit workbench entry and initial restoration, never render.
    /// Terminal tasks remain in the task list and recent notes after their input
    /// has become a fresh form; an in-flight or edited input is never replaced.
    pub(super) fn prepare_workbench_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        #[cfg(feature = "capture")]
        self.capture_source(window, cx);
        if self.pending_conversion.is_some() || self.following_conversion.is_some() {
            return true;
        }
        let input = self.value(Field::Source, cx);
        let has_old_result = self.workspace.as_ref().is_some_and(|workspace| {
            workspace.state.draft().is_some_and(|draft| {
                retired_input_task(draft, &workspace.state.tasks, &input).is_some()
            })
        });
        if !has_old_result {
            return true;
        }
        if !self.save_current_draft(cx) {
            return false;
        }
        let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
        let online = self.online();
        let Some(workspace) = &mut self.workspace else {
            return true;
        };
        let Some(draft) = workspace.state.draft() else {
            return true;
        };
        if retired_input_task(draft, &workspace.state.tasks, &input).is_none() {
            return true;
        }
        let destination = (draft.library_id == workspace.state.default_library)
            .then(|| {
                draft
                    .folder
                    .map(|folder| (draft.library_id.clone(), folder))
            })
            .flatten();
        match workspace.transaction(|state| {
            state.reset_input(online, defaults, destination);
            Ok(())
        }) {
            Ok(()) => {
                self.completed_source = None;
                self.invalidate_source();
                self.restore_draft(window, cx);
                self.source_editor_open = true;
                true
            }
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "新的视频输入尚未建立：{}",
                    format!("{error:#}")
                ));
                cx.notify();
                false
            }
        }
    }

    pub fn new_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Capture builds only: hold the page at a scroll offset to render
        // its lower region.
        #[cfg(feature = "capture")]
        if let Some(offset) = std::env::var("COURSE2MD_CAPTURE_SCROLL")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
        {
            self.scrolls[Page::New as usize].set_offset(point(px(0.), px(-offset)));
        }
        let opening_result = self.reading && self.following_conversion.is_some();
        let linked_state = self.workbench_linked_task(cx).map(|task| task.state);
        let cancelled_notice = self
            .current_input_task(cx)
            .filter(|task| task.state == workspace::TaskState::Cancelled)
            .map(|task| (task.id.clone(), course2md::tr!("任务已取消").to_owned()));
        let compact = is_compact(window);
        let has_linked_task = linked_state.is_some();
        let text_required = !has_linked_task && self.subtitle_attention_required();
        // Text-source decisions and submission obstacles belong to the
        // previewed video, so they sit inside its card.
        let preview_extra = if !has_linked_task && self.source_preview.is_some() {
            let recovery = self.conversion_recovery(cx);
            (text_required || recovery.is_some()).then(|| {
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_4()
                    .when(text_required, |extra| {
                        extra.child(
                            v_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .pt_4()
                                .border_t_1()
                                .border_color(color(HAIRLINE))
                                .child(section_title(
                                    "import-text-source-title",
                                    course2md::tr!("文字来源"),
                                ))
                                .child(self.text_source_view(cx)),
                        )
                    })
                    .children(recovery)
            })
        } else {
            None
        };
        // The current input's own task takes the preview card's place and
        // shape: a running task shows its live status, a settled one what it
        // needs or what opens.
        let linked_task = self.workbench_linked_task(cx).cloned();
        let task_card = linked_task.map(|task| {
            let card = match task.state {
                _ if task_runs_in_card(&task) => self.box_running_task(&task, window, cx),
                workspace::TaskState::Complete | workspace::TaskState::Partial
                    if opening_result =>
                {
                    self.box_task_opening_result(&task, cx)
                        .p(rems(24. / 14.))
                        .rounded(RADIUS_HERO)
                        .bg(color(SURFACE))
                }
                _ => self
                    .box_task_attention(&task, cx)
                    .p(rems(24. / 14.))
                    .rounded(RADIUS_HERO)
                    .bg(color(SURFACE)),
            };
            motion::state_enter(
                SharedString::from(format!("workbench-task-{}", task.id)),
                card.rounded_bl(TILE_INNER_RADIUS)
                    .rounded_br(TILE_INNER_RADIUS),
            )
        });
        // The options belong to one input: joined to its video card (the
        // preview or the task in its place), otherwise at the end of the hero.
        let preview_card = self.preview_card_shown(cx);
        let attached = preview_card || task_card.is_some();
        let options = motion::state_enter(
            "generation-options-panel",
            self.workbench_options(attached, window, cx),
        );
        let (preview_options, options) = if preview_card {
            (Some(options), None)
        } else {
            (None, Some(options))
        };
        let input = v_flex()
            .w_full()
            .min_w_0()
            .gap(HERO_GAP)
            .when(!self.online(), |input| {
                input.child(self.source_kind_tabs(cx))
            })
            .child(self.box_source_input(preview_extra, preview_options, window, cx));
        let mut view = v_flex()
            .gap(HERO_GAP)
            .w_full()
            .min_w_0()
            .max_w(rems(WORKBENCH_WIDTH / 14.))
            .mx_auto()
            .child(
                h_flex().w_full().justify_center().pb(rems(4. / 14.)).child(
                    accessible_text("workbench-title", course2md::tr!("把视频整理成笔记"))
                        .role(Role::Heading)
                        .text_center()
                        .text_size(rems(if compact { 32. / 14. } else { 40. / 14. }))
                        .line_height(rems(if compact { 40. / 14. } else { 48. / 14. }))
                        .font_weight(FontWeight::NORMAL),
                ),
            )
            .child(input);
        if let Some((id, message)) = cancelled_notice {
            view = view.child(info_callout(
                SharedString::from(format!("cancelled-input-task-{id}")),
                message,
            ));
        }
        view = match task_card {
            Some(card) => view.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(TILE_GAP)
                    .child(card)
                    .children(options),
            ),
            None => view.children(options),
        };
        let active = self.workbench_active_tasks_section(window, cx);
        let recent = self
            .recent_notes_section(cx)
            .map(|recent| motion::enter("home-recent-notes", recent));
        // With nothing below it, the input moves toward the optical center.
        let quiet = recent.is_none() && active.is_none() && !self.desktop_settings.home_options;
        // Below the hero, everything shares its column, so the options stay
        // with the video they apply to rather than beside the recent notes.
        let lower_width = WORKBENCH_WIDTH;
        let lower = v_flex()
            .w_full()
            .min_w_0()
            .max_w(rems(lower_width / 14.))
            .mx_auto()
            .children(recent);
        let hero_top = motion::value("home-hero-top", hero_top(quiet, compact), window, cx);
        let mut view = v_flex()
            .w_full()
            .min_w_0()
            .pt(rems(hero_top / 14.))
            .pb(rems(72. / 14.))
            .gap(rems(if compact { 36. / 14. } else { 48. / 14. }))
            .child(view)
            .when_some(active, |view, active| {
                view.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .max_w(rems(lower_width / 14.))
                        .mx_auto()
                        .child(active),
                )
            })
            .child(lower);
        if let Some(error) = &self.workspace_error {
            view = view.child(motion::enter(
                text_id("workspace-error", error),
                v_flex()
                    .w_full()
                    .max_w(rems(lower_width / 14.))
                    .mx_auto()
                    .p_4()
                    .rounded(RADIUS_CARD)
                    .bg(color(DANGER_BG))
                    .child(issue("import-start-error", error.clone())),
            ));
        }
        // The whole workbench accepts a dropped video, not just the local tab.
        view.on_drop(
            cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                this.drop_source_files(paths.paths(), window, cx)
            }),
        )
        .into_any_element()
    }

    /// Attach last to the full-window `workbench-drop` group, outside scrolling content.
    pub(super) fn workbench_drop_overlay(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("workbench-drop-overlay")
            .absolute()
            .inset_0()
            .invisible()
            .group_drag_over::<gpui::ExternalPaths>("workbench-drop", |style| style.visible())
            .bg(color(CANVAS).opacity(0.88))
            .p(SECTION_GAP)
            .child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .rounded(RADIUS_HERO)
                    .border_2()
                    .border_dashed()
                    .border_color(color(ACCENT))
                    .child(
                        h_flex()
                            .size(rems(88. / 14.))
                            .rounded_full()
                            .items_center()
                            .justify_center()
                            .bg(color(SECONDARY_CONTAINER))
                            .child(
                                icons::file_upload()
                                    .size(rems(40. / 14.))
                                    .text_color(color(ACCENT)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(rems(28. / 14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(course2md::tr!("松开以添加视频")),
                    ),
            )
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.drop_source_files(paths.paths(), window, cx);
                }),
            )
    }

    /// Only unresolved choices interrupt a submitted conversion.
    fn conversion_recovery(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let busy = self.job.is_some()
            || self.workspace.as_ref().is_some_and(|workspace| {
                workspace.state.tasks.iter().any(|task| {
                    matches!(
                        task.state,
                        crate::workspace::TaskState::Queued | crate::workspace::TaskState::Running
                    )
                })
            });
        let reading = self.preview_cancel.is_some() || self.subtitle_loading;
        // Derived once per draft/preference/source revision, not on every render.
        let cache_key = (
            self.workspace
                .as_ref()
                .map_or(0, |workspace| workspace.state.revision),
            self.preferences
                .revision(crate::preferences::PreferenceGroup::Generation),
            self.preview_generation,
        );
        let preview_plan = match &self.preview_plan_cache {
            Some((workspace, preferences, source, plan))
                if (*workspace, *preferences, *source) == cache_key =>
            {
                plan.clone()
            }
            _ => {
                let plan = self.preview_plan().map_err(|error| {
                    let pending = error
                        .downcast_ref::<crate::task_runner::PlanPending>()
                        .is_some();
                    (format!("{error:#}"), pending)
                });
                self.preview_plan_cache =
                    Some((cache_key.0, cache_key.1, cache_key.2, plan.clone()));
                plan
            }
        };
        let plan_pending = matches!(&preview_plan, Err((_, true)));
        let preview_plan = preview_plan.map_err(|(message, _)| anyhow::Error::msg(message));
        let matching_task = preview_plan
            .as_ref()
            .ok()
            .and_then(|plan| self.matching_task(plan))
            .map(|task| {
                let location = self.task_library(task).map(|library| library.name.clone());
                (task.id.clone(), task.plan.title.clone(), location)
            });
        let preferences_issue = self.ordinary_preferences_submit_issue();
        let preferences_blocked = preferences_issue.is_some();
        let submission_error = preferences_issue
            .as_ref()
            .map(|issue| issue.message.clone())
            .or_else(|| {
                if self.source_preview.is_some() && !reading && !self.subtitle_attention_required()
                {
                    preview_plan
                        .as_ref()
                        .err()
                        .map(|error| format!("{error:#}"))
                } else {
                    None
                }
            });
        // Waiting for the component or location checks is progress, not a failure.
        let checking_components = !preferences_blocked
            && (self.environment.is_none() || plan_pending)
            && submission_error.is_some();
        let mut inset = v_flex().w_full().min_w_0().gap_3();
        // Whether anything needs showing; an empty block would add a gap.
        let mut any = false;
        let mut actions = h_flex().gap_2().flex_wrap();
        if self.environment.as_ref().is_some_and(|environment| {
            !environment.engine
                || !environment.ffmpeg
                || !environment.ffprobe
                || (self.online() && !environment.ytdlp)
        }) {
            any = true;
            actions = actions.child(
                outline_pill("repair-conversion-components", cx)
                    .icon(icons::settings())
                    .label(course2md::tr!("检查所需组件"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_settings(window, cx);
                        this.select_settings_tab(
                            crate::settings_ui::SettingsTab::Application,
                            window,
                            cx,
                        );
                    })),
            );
        }
        if let Some(issue) = preferences_issue {
            any = true;
            actions = actions.child(
                outline_pill("repair-generation-preferences", cx)
                    .icon(icons::refresh())
                    .label(if issue.can_retry {
                        course2md::tr!("重试保存处理选项")
                    } else {
                        course2md::tr!("重置处理选项")
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if !this.save_current_draft(cx) {
                            return;
                        }
                        let saved = if issue.can_retry {
                            this.retry_ordinary_preferences(issue.group, cx)
                        } else {
                            this.restore_ordinary_preferences(issue.group, window, cx)
                        };
                        if saved {
                            let focus = this.import_submit_focus.clone();
                            window.defer(cx, move |window, cx| focus.focus(window, cx));
                        }
                    })),
            );
        }
        let draft_library = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .state
                .draft()
                .and_then(|draft| workspace.state.library(&draft.library_id))
        });
        let reassociate = self.reassociate_action(
            draft_library,
            outline_pill("reassociate-import-location", cx),
            cx,
        );
        any |= reassociate.is_some();
        actions = actions.children(reassociate);
        if let Some((id, title, location)) = matching_task.clone() {
            let location = location.unwrap_or_else(|| course2md::tr!("原保存位置").into());
            inset = inset.child(conversion_fact(
                "conversion-fact-consequence",
                course2md::tr!("已有相同处理任务：《{}》（{}）", title, location),
                true,
            ));
            any = true;
            actions = actions.child(
                primary_pill("show-matching-task")
                    .track_focus(&self.import_submit_focus)
                    .self_start()
                    .icon(icons::task())
                    .label(course2md::tr!("查看任务"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.save_current_draft(cx);
                        this.select_task(&id, cx);
                        this.navigate(Page::Task, cx);
                        cx.notify();
                    })),
            );
        } else if let Some(course) = self.existing_source_note().cloned() {
            let location = self
                .course_location(&course)
                .map(|library| library.name.clone())
                .unwrap_or_else(|| course2md::tr!("笔记库").into());
            inset = inset.child(info_callout(
                "existing-note-notice",
                course2md::tr!("已有笔记，保存在「{}」", location),
            ));
            any = true;
            actions = actions.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        primary_pill("open-existing-note")
                            .icon(crate::icons::book_open())
                            .label(course2md::tr!("打开已有笔记"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save_current_draft(cx);
                                this.open_course(course.clone(), cx);
                            })),
                    )
                    .child(
                        outline_pill("generate-new-version", cx)
                            .icon(icons::refresh())
                            .track_focus(&self.import_submit_focus)
                            .label(course2md::tr!("生成新版笔记"))
                            .disabled(reading || self.workspace.is_none() || preferences_blocked)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.following_conversion =
                                    Some(ConversionFollow::Preparing(this.preview_generation));
                                this.enqueue_current(window, cx);
                            })),
                    ),
            );
            if busy {
                any = true;
                inset = inset.child(conversion_fact(
                    "conversion-fact-queued",
                    course2md::tr!("将排在当前任务之后"),
                    false,
                ));
            }
        } else if let Some(purpose) = self.missing_import_service() {
            any = true;
            actions = actions.child(
                primary_pill("configure-conversion-service")
                    .icon(icons::settings())
                    .label(match purpose {
                        ServicePurpose::Speech => course2md::tr!("设置语音识别服务"),
                        ServicePurpose::Ai => course2md::tr!("设置 AI 服务"),
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_task_service_editor(purpose, window, cx);
                    })),
            );
        }
        if self.pending_conversion.is_some()
            && self.source_validation.is_some()
            && !reading
            && self.missing_import_service().is_none()
            && self.existing_source_note().is_none()
            && matching_task.is_none()
        {
            any = true;
            actions = actions.child(
                outline_pill("retry-conversion-validation", cx)
                    .icon(icons::refresh())
                    .label(course2md::tr!("重试转换"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_conversion(window, cx);
                    })),
            );
        }
        if self.pending_conversion.is_some() && !reading {
            any = true;
            actions = actions.child(
                quiet("cancel-pending-conversion")
                    .icon(icons::close())
                    .label(course2md::tr!("取消转换"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pending_conversion = None;
                        this.following_conversion = None;
                        cx.notify();
                    })),
            );
        }
        any |= submission_error.is_some();
        if !any {
            return None;
        }
        Some(
            inset
                .when_some(submission_error, |view, message| {
                    if checking_components {
                        view.child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .items_center()
                                .child(motion::spinner("import-component-check-spinner", cx))
                                .child(
                                    accessible_text("import-component-check", message)
                                        .role(Role::Status)
                                        .text_sm()
                                        .font_weight(FontWeight::NORMAL)
                                        .whitespace_normal()
                                        .text_color(color(GRAY)),
                                ),
                        )
                    } else {
                        view.child(
                            accessible_text("import-submit-error", message)
                                .role(Role::Alert)
                                .text_sm()
                                .whitespace_normal()
                                .text_color(color(DANGER)),
                        )
                    }
                })
                .child(actions),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConversionFollow, ConversionGate, GateInputs, automatic_subtitle_fallback,
        completed_input_task, conversion_gate, locked_input_task, submitted_input_task,
        subtitle_needs_confirmation, uses_speech,
    };
    use crate::{source, workspace};
    use course2md::config::TranscriptSource;
    use course2md::subtitle::{CachedSubtitle, SubtitleEvidence, SubtitleReadError};

    /// A plan's configuration as build_plan writes it from default options.
    fn planned_config() -> course2md::settings::ConfigFile {
        let mut config = course2md::settings::ConfigFile::default();
        crate::ConversionOptions::default().apply_to(&mut config);
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
    fn explicit_new_version_follows_new_task_instead_of_previous_result() {
        let (mut draft, mut tasks) = submitted_recovery_chain();
        tasks[0].state = workspace::TaskState::Complete;
        tasks[0].artifact = Some("versions/previous".into());
        tasks[0].handled_by = None;
        tasks[0].events.error = None;
        tasks[1].id = "new-version".into();
        tasks[1].parent = None;
        tasks[1].plan.operation = Default::default();
        draft.submitted_task = Some(tasks[1].id.clone());
        let following = ConversionFollow::Preparing(7)
            .submitted(7, tasks[1].id.clone())
            .unwrap();
        // The old version remains readable but is not the explicit new intent.
        assert!(
            following
                .completed_task(true, 7, &draft, &tasks, &draft.input)
                .is_none()
        );
        tasks[1].state = workspace::TaskState::Complete;
        tasks[1].artifact = Some("versions/new-version".into());
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
        tasks[1].artifact = Some("versions/recovery".into());
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
    fn reader_repair_opens_the_new_version_only_while_the_original_note_is_still_open() {
        let (_, mut tasks) = submitted_recovery_chain();
        let original = std::path::Path::new("versions/original");
        let follow = ConversionFollow::reader_reprocess(&tasks[1], original, 12).unwrap();
        // Submitting a recovery is not enough: it must publish a readable result.
        assert!(!follow.completed_reader_task(&tasks[1], super::Page::Result, 12, Some(original)));
        tasks[1].artifact = Some("versions/recovery".into());
        for state in [
            workspace::TaskState::Complete,
            workspace::TaskState::Partial,
        ] {
            tasks[1].state = state;
            assert!(follow.completed_reader_task(
                &tasks[1],
                super::Page::Result,
                12,
                Some(original)
            ));
        }
        for page in [
            super::Page::New,
            super::Page::Library,
            super::Page::Task,
            super::Page::Settings,
        ] {
            assert!(!follow.completed_reader_task(&tasks[1], page, 12, Some(original)));
        }
        // Leaving and returning to the same note has a new read generation.
        assert!(!follow.completed_reader_task(&tasks[1], super::Page::Result, 13, Some(original)));
        assert!(!follow.completed_reader_task(
            &tasks[1],
            super::Page::Result,
            12,
            Some(std::path::Path::new("versions/another-note")),
        ));
        assert!(!follow.completed_reader_task(&tasks[1], super::Page::Result, 12, None));
        let mut unrelated = tasks[1].clone();
        unrelated.id = "another-repair".into();
        assert!(!follow.completed_reader_task(&unrelated, super::Page::Result, 12, Some(original)));
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
                Some(original)
            ));
        }
    }

    #[test]
    fn reader_repair_guard_excludes_exports_and_rechecks_the_note_when_loading_finishes() {
        let (_, mut tasks) = submitted_recovery_chain();
        let original = std::path::Path::new("versions/original");
        let follow = ConversionFollow::reader_reprocess(&tasks[1], original, 12).unwrap();
        assert!(follow.context_is_current(super::Page::Result, 99, 12, Some(original)));
        assert!(!follow.context_is_current(super::Page::Result, 99, 13, Some(original)));
        assert!(!follow.context_is_current(super::Page::Library, 99, 12, Some(original)));
        assert!(!follow.context_is_current(
            super::Page::Result,
            99,
            12,
            Some(std::path::Path::new("versions/other")),
        ));

        let course2md::execution::Operation::Reprocess { components, .. } =
            &mut tasks[1].plan.operation
        else {
            unreachable!();
        };
        *components = vec![course2md::execution::Component::Exports];
        tasks[1].state = workspace::TaskState::Complete;
        tasks[1].artifact = Some(original.to_owned());
        assert!(ConversionFollow::reader_reprocess(&tasks[1], original, 12).is_none());
        assert!(!follow.completed_reader_task(&tasks[1], super::Page::Result, 12, Some(original)));
        // Even a spurious new artifact cannot turn an export-only task into navigation.
        tasks[1].artifact = Some("versions/recovery".into());
        assert!(!follow.completed_reader_task(&tasks[1], super::Page::Result, 12, Some(original)));
        tasks[1].plan.operation = Default::default();
        assert!(ConversionFollow::reader_reprocess(&tasks[1], original, 12).is_none());
    }

    #[test]
    fn opening_a_reader_viewer_ends_repair_following_even_if_it_closes_before_the_read_finishes() {
        let (_, mut tasks) = submitted_recovery_chain();
        let original = std::path::Path::new("versions/original");
        let captured = ConversionFollow::reader_reprocess(&tasks[1], original, 12).unwrap();
        tasks[1].state = workspace::TaskState::Complete;
        tasks[1].artifact = Some("versions/recovery".into());
        for read_started in [false, true] {
            let mut active = Some(captured.clone());
            if read_started {
                assert!(active.as_ref().unwrap().completed_reader_task(
                    &tasks[1],
                    super::Page::Result,
                    12,
                    Some(original),
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
                        Some(original),
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
        let defaults = crate::ConversionOptions {
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
            work_dir: "work/original".into(),
            events: crate::activity::JobEvents {
                error: Some("原摘要请求结果未确认".into()),
                ..Default::default()
            },
            artifact: Some("versions/original".into()),
            unread: false,
            resend: Vec::new(),
        };
        let mut recovery = original.clone();
        recovery.id = "recovery".into();
        recovery.parent = Some(original.id.clone());
        recovery.state = workspace::TaskState::Queued;
        recovery.intent = workspace::Intent::Run;
        recovery.events.error = None;
        recovery.artifact = None;
        recovery.work_dir = "work/recovery".into();
        recovery.plan.operation = course2md::execution::Operation::Reprocess {
            base_version_dir: "versions/original".into(),
            components: vec![course2md::execution::Component::Summary],
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
            tasks[1].artifact = Some("versions/recovery".into());
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
            reason: course2md::progress::BlockReason::Uncertain,
            request_id: Some("pending-summary".into()),
            purpose: Some("summary".into()),
            description: "摘要".into(),
            kind: Some(course2md::artifact::FailureKind::Uncertain),
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
        tasks[1].artifact = Some("versions/recovery".into());
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
        tasks[2].artifact = Some("versions/final-attempt".into());
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
        tasks[1].artifact = Some("versions/recovery".into());
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
        let track = course2md::subtitle::file_track("lesson.srt".into(), None);
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
        let track = course2md::subtitle::file_track("lesson.srt".into(), None);
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
        source.subtitle_request = Some(course2md::subtitle::file_track("lesson.srt".into(), None));
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
