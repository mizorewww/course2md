//! The workbench: a link, its video and options, and one start intent carried through
//! preparation to a readable note.
use crate::{
    ConversionOptions, Desktop, Field, Page, notes::Course, preferences::ServicePurpose, source,
    symbols, task_ui::CardPlace, ui, workspace,
};
use anyhow::Context as _;
use course2md::config::TranscriptSource;
use course2md::subtitle::{SubtitleEvidence, SubtitleReadError, SubtitleTrack};
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{
    color::ColorRole,
    icons::{Icon, SymbolIcon},
    motion,
    prelude::*,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The workbench's one column.
const WORKBENCH_WIDTH: Rems = rem(760.);
/// Rhythm inside the column: title, link, video and options.
const HERO_GAP: Rems = rem(16.);

fn workbench_column() -> Div {
    div()
        .w_full()
        .min_w_0()
        .max_w(WORKBENCH_WIDTH)
        .mx_auto()
        .flex()
        .flex_col()
}

/// How far the hero sits below the bar: with nothing under it, near the optical center.
fn hero_top(quiet: bool, compact: bool) -> f32 {
    match (quiet, compact) {
        (true, true) => 96.,
        (true, false) => 160.,
        (false, true) => 24.,
        (false, false) => 48.,
    }
}

/// The mark on a cover slot whose video has no cover image.
pub(crate) fn cover_placeholder(size: Rems) -> Icon {
    Icon::from(symbols::movie())
        .size(size)
        .color(color(ColorRole::ON_SECONDARY_CONTAINER))
}

pub(crate) fn cover_width(window: &Window) -> Rems {
    rem(if is_compact(window) { 200. } else { 256. })
}

/// A video's 16:9 cover, or its mark while it has none.
pub(crate) fn video_cover(cover: Option<PathBuf>, width: Rems) -> Div {
    div()
        .relative()
        .flex_shrink_0()
        .w(width)
        .max_w_full()
        .aspect_ratio(16. / 9.)
        .rounded(RADIUS_LARGE)
        .overflow_hidden()
        .bg(color(ColorRole::SECONDARY_CONTAINER))
        .flex()
        .items_center()
        .justify_center()
        .map(|slot| match cover {
            Some(cover) => slot.child(img(cover).size_full().object_fit(ObjectFit::Cover)),
            None => slot.child(cover_placeholder(rem(32.))),
        })
}

/// A video's title, on the workbench and on a task's card.
pub(crate) fn video_title(id: String, title: impl Into<SharedString>) -> Stateful<Div> {
    let role = TypeRole::TitleLarge.scale();
    accessible_text(ElementId::Name(id.into()), title)
        .min_w_0()
        .text_size(role.font_size)
        .line_height(role.line_height)
        .font_weight(FontWeight::MEDIUM)
        .whitespace_normal()
        .text_ellipsis()
        .line_clamp(2)
}

/// Where a video comes from and who made it.
pub(crate) fn source_metadata(source: &source::Source) -> String {
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
    [platform, source.author.clone()]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Text that names one thing beside a control.
fn supporting(id: &'static str, text: impl Into<SharedString>) -> Stateful<Div> {
    accessible_text(id, text)
        .min_w_0()
        .text_size(TEXT_BODY)
        .text_color(color(ColorRole::ON_SURFACE_VARIANT))
        .whitespace_normal()
}

/// A button that shows or hides the technical text under it.
fn details_toggle(id: impl Into<ElementId>, open: bool) -> MaterialButton {
    material_button(id, ButtonVariant::Text)
        .icon(disclosure_chevron(open))
        .child(if open {
            course2md::tr!("收起技术详情")
        } else {
            course2md::tr!("技术详情")
        })
}

/// An icon-only action; its tooltip names it.
fn tip_button(
    id: &'static str,
    icon: SymbolIcon,
    label: &'static str,
    configure: impl FnOnce(IconButton) -> IconButton,
) -> MaterialTooltip {
    MaterialTooltip::plain(SharedString::from(format!("{id}-tooltip")), label)
        .trigger(configure(IconButton::new(id, icon, label)))
}

/// The Bilibili brand mark, for the login that recovers a read.
fn bilibili_mark() -> Icon {
    Icon::new("icons/bilibili.svg").color(rgb(0x00a1d6))
}

/// The first line of a message when it is already readable. A raw download
/// tool line (`WARNING:` / `ERROR:`) is not, and the caller supplies a summary.
fn readable_first_line(message: &str) -> Option<&str> {
    let first = message.lines().next().unwrap_or(message);
    (!first.contains("WARNING:") && !first.contains("ERROR:")).then_some(first)
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

/// What a subtitle read belongs to. Its late result counts only while all of this holds.
struct SubtitleRead {
    generation: u64,
    source_generation: u64,
    identity: String,
    input: Option<(String, u64)>,
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
            self.source_validation = Some(
                if self.online() {
                    course2md::tr!("先粘贴视频链接")
                } else {
                    course2md::tr!("先选择一个视频")
                }
                .into(),
            );
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

    fn import_base_config(&self) -> course2md::settings::ConfigFile {
        self.workspace
            .as_ref()
            .and_then(|workspace| workspace.state.draft())
            .and_then(|draft| draft.base_config.clone())
            .unwrap_or_else(|| self.preferences.defaults_config())
    }

    fn subtitle_read_is_current(&self, read: &SubtitleRead) -> bool {
        self.subtitle_generation == read.generation
            && self.preview_generation == read.source_generation
            && self
                .source_preview
                .as_ref()
                .is_some_and(|source| source.identity == read.identity)
            && read.input.as_ref().is_none_or(|(id, revision)| {
                self.workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.state.matches_input(id, *revision))
            })
    }

    /// Runs one subtitle read off the UI thread: `mark` records what was asked for
    /// before any I/O, and `finish` takes the result unless the source moved on.
    fn read_subtitles<T: Send + 'static>(
        &mut self,
        mark: impl FnOnce(&mut Self, &mut Context<Self>),
        work: impl FnOnce(&source::Source, Arc<AtomicBool>) -> T + Send + 'static,
        lost: fn() -> T,
        finish: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.source_preview.clone() else {
            return;
        };
        let generation = self.stop_subtitle_read();
        let cancel = Arc::new(AtomicBool::new(false));
        self.subtitle_cancel = Some(cancel.clone());
        self.subtitle_loading = true;
        self.subtitle_error = None;
        mark(self, cx);
        if !self.save_current_draft(cx) {
            self.subtitle_loading = false;
            self.subtitle_cancel = None;
            return;
        }
        let read = SubtitleRead {
            generation,
            source_generation: self.preview_generation,
            identity: source.identity.clone(),
            input: self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.draft())
                .map(|draft| (draft.id.clone(), draft.revision)),
        };
        self.preview_workers += 1;
        let task = crate::spawn_blocking_io(move || work(&source, cancel));
        cx.spawn(async move |this, cx| {
            let result = task.recv().await.unwrap_or_else(|_| lost());
            let _ = this.update(cx, |this, cx| {
                this.preview_workers = this.preview_workers.saturating_sub(1);
                if !this.subtitle_read_is_current(&read) {
                    return;
                }
                this.subtitle_loading = false;
                this.subtitle_cancel = None;
                finish(this, result, cx);
                this.advance_conversion_when_ready(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn confirm_subtitle(
        &mut self,
        track: SubtitleTrack,
        explicit: bool,
        cx: &mut Context<Self>,
    ) {
        let requested = track.clone();
        self.read_subtitles(
            move |this, cx| {
                if explicit {
                    // The requested text source takes effect before I/O. A failed
                    // caption read must never silently keep the previous ASR choice.
                    this.update_task_options(|o| o.source_mode = TranscriptSource::Subtitle, cx);
                }
                if let Some(source) = &mut this.source_preview {
                    source.subtitle_request = Some(requested);
                    source.subtitle_read_error = None;
                }
            },
            move |source, cancel| source::read_subtitle(source, &track, cancel),
            || {
                Err(SubtitleReadError::Failed {
                    message: course2md::tr!("字幕读取意外中断").into(),
                })
            },
            move |this, result, cx| {
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
                    }
                    Err(SubtitleReadError::Cancelled) => {
                        this.pending_conversion = None;
                        return;
                    }
                    Err(error) => {
                        this.subtitle_error = Some(error.to_string());
                        if let Some(source) = &mut this.source_preview {
                            source.subtitle_read_error = Some(error);
                        }
                    }
                }
                this.save_current_draft(cx);
            },
            cx,
        );
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
                            this.attach_subtitle(path, cx);
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

    fn attach_subtitle(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !course2md::subtitle::is_subtitle_file(&path) {
            self.subtitle_error = Some(course2md::tr!("请选择 SRT 或 VTT 字幕文件").into());
            return;
        }
        let track = course2md::subtitle::file_track(path, None);
        if let Some(source) = &mut self.source_preview {
            match &mut source.subtitles {
                SubtitleEvidence::Found { tracks, .. } => {
                    if !tracks.iter().any(|candidate| candidate.id == track.id) {
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
        self.confirm_subtitle(track, true, cx);
    }

    fn retry_subtitles(&mut self, cx: &mut Context<Self>) {
        self.read_subtitles(
            |this, _| {
                if let Some(source) = &mut this.source_preview {
                    source.subtitle_read_error = Some(SubtitleReadError::Failed {
                        message: course2md::tr!("字幕重新读取未完成").into(),
                    });
                }
            },
            source::refresh_subtitles,
            || Err(anyhow::anyhow!(course2md::tr!("字幕读取意外中断"))),
            |this, result, cx| this.finish_subtitle_refresh(result, cx),
            cx,
        );
    }

    fn finish_subtitle_refresh(
        &mut self,
        result: anyhow::Result<SubtitleEvidence>,
        cx: &mut Context<Self>,
    ) {
        let mut evidence = match result {
            Ok(evidence) => evidence,
            Err(error) => {
                self.subtitle_error = Some(format!("{error:#}"));
                if let Some(source) = &mut self.source_preview {
                    source.subtitle_read_error = self
                        .subtitle_error
                        .clone()
                        .map(|message| SubtitleReadError::Failed { message });
                }
                self.save_current_draft(cx);
                return;
            }
        };
        if let SubtitleEvidence::Found { tracks, .. } = &mut evidence {
            let original = self
                .source_preview
                .as_ref()
                .and_then(|source| source.original_language.as_deref());
            course2md::subtitle::sort_tracks(
                tracks,
                &self.preferences.generation().preferred_subtitle_languages,
                course2md::i18n::lang().code(),
                original,
            );
        }
        let old_id = self
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
            self.subtitle_error = Some(course2md::tr!("原字幕无法重新读取").into());
        }
        if let Some(source) = &mut self.source_preview {
            source.subtitles = evidence;
            source.subtitle_read_error = self
                .subtitle_error
                .clone()
                .map(|message| SubtitleReadError::Failed { message });
        }
        self.save_current_draft(cx);
        if let Some(track) = track {
            self.confirm_subtitle(track, false, cx);
        }
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

    /// The person settled the text source: drop what was in flight and go on.
    fn settle_text_source(&mut self, mode: TranscriptSource, cx: &mut Context<Self>) {
        self.abort_subtitle_read();
        self.subtitle_error = None;
        self.update_task_options(|o| o.source_mode = mode, cx);
        self.advance_conversion_when_ready(cx);
        cx.notify();
    }

    fn use_speech(&mut self, cx: &mut Context<Self>) {
        self.settle_text_source(TranscriptSource::Asr, cx);
    }

    fn use_confirmed_subtitle(&mut self, cx: &mut Context<Self>) {
        if self
            .source_preview
            .as_ref()
            .is_some_and(|source| source.selected_subtitle.is_some())
        {
            self.settle_text_source(TranscriptSource::Subtitle, cx);
        }
    }

    fn select_source_input(&mut self, input: String, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs[&Field::Source].update(cx, |state, cx| state.set_value(input, window, cx));
        self.inspect_source(window, cx);
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

    /// The link and what can be done with it, or the local file in its place.
    fn source_input(&self, cx: &mut Context<Self>) -> Div {
        let local = (!self.online()).then(|| self.local_source_input(cx));
        if let Some(local) = local {
            return v_flex()
                .w_full()
                .min_w_0()
                .gap(HERO_GAP)
                .child(
                    h_flex().w_full().justify_center().child(
                        ConnectedButtonGroup::new("source-kind", course2md::tr!("视频来源"))
                            .options([
                                ("online", course2md::tr!("视频链接")),
                                ("local", course2md::tr!("本地文件")),
                            ])
                            .icon("online", symbols::link())
                            .icon("local", symbols::movie())
                            .selected("local")
                            .on_activate(cx.listener(|this, value: &SharedString, window, cx| {
                                this.switch_source_kind(value.as_ref() == "online", window, cx);
                            })),
                    ),
                )
                .children(local);
        }
        let validation = self
            .source_preview
            .is_none()
            .then(|| self.source_validation.clone())
            .flatten();
        let failed = validation.is_some() || self.preview_error.is_some();
        // The start action owns the submit focus once it is on screen.
        let start_shown = self.source_preview.is_some() && self.can_start_input(cx);
        let preview_label = course2md::tr!("预览视频");
        let mut preview = IconButton::new("preview-source", symbols::visibility(), preview_label)
            .variant(IconButtonVariant::Tonal)
            .disabled(
                self.preview_cancel.is_some()
                    || self.value(Field::Source, cx).is_empty()
                    || self.owning_task(cx).is_some(),
            )
            .on_click(cx.listener(|this, _, window, cx| this.inspect_source(window, cx)));
        if !start_shown {
            preview = preview.track_focus(&self.import_submit_focus);
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap(HERO_GAP)
            .child(
                TextField::new("source-link", &self.inputs[&Field::Source])
                    .w_full()
                    .outlined()
                    .label(course2md::tr!("视频链接"))
                    .float_label(true)
                    .leading_icon(Icon::from(symbols::link()))
                    .trailing_icon(
                        MaterialTooltip::plain("preview-source-tooltip", preview_label)
                            .trigger(preview),
                    )
                    .error(failed)
                    .when_some(validation, |field, message| field.supporting_text(message)),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_center()
                    .items_center()
                    .flex_wrap()
                    .gap(rem(8.))
                    .child(supporting(
                        "import-drop-hint",
                        course2md::tr!("或把本地视频拖进窗口"),
                    ))
                    .child(
                        material_button("choose-local-video", ButtonVariant::Text)
                            .icon(symbols::file_upload())
                            .child(course2md::tr!("选择文件"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.switch_source_kind(false, window, cx);
                                this.pick_video(window, cx);
                            })),
                    ),
            )
    }

    /// The local source: a drop target while empty, the file once chosen; the preview
    /// card takes its place when the file has been read.
    fn local_source_input(&self, cx: &mut Context<Self>) -> Option<Div> {
        let input = self.value(Field::Source, cx);
        let choose = |id: &'static str, variant, label: &'static str, cx: &mut Context<Self>| {
            material_button(id, variant)
                .icon(symbols::folder_open())
                .child(label)
                .on_click(cx.listener(|this, _, window, cx| this.pick_video(window, cx)))
        };
        if input.is_empty() {
            return Some(
                tile([true; 4])
                    .flex()
                    .flex_col()
                    .gap(rem(16.))
                    .p(rem(32.))
                    .items_center()
                    .border_2()
                    .border_dashed()
                    .border_color(color(ColorRole::OUTLINE))
                    .child(
                        Icon::from(symbols::file_upload())
                            .size(rem(32.))
                            .color(color(ColorRole::PRIMARY)),
                    )
                    .child(
                        accessible_text("import-drop-instruction", course2md::tr!("拖入本地视频"))
                            .text_size(TEXT_BODY)
                            .font_weight(FontWeight::MEDIUM),
                    )
                    .child(choose(
                        "choose-video",
                        ButtonVariant::Filled,
                        course2md::tr!("选择视频"),
                        cx,
                    )),
            );
        }
        if self.source_preview.is_some() {
            return None;
        }
        let filename = PathBuf::from(&input)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| input.clone());
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .child(
                    tile([true; 4])
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(rem(12.))
                        .p(rem(16.))
                        .child(
                            semantic_label(
                                "selected-local-video-name",
                                filename,
                                Icon::from(symbols::movie()).color(color(ColorRole::PRIMARY)),
                            )
                            .flex_1()
                            .flex_basis(rem(240.)),
                        )
                        .child(choose(
                            "choose-video",
                            ButtonVariant::Outlined,
                            course2md::tr!("更换视频"),
                            cx,
                        ))
                        .when(self.can_start_input(cx), |row| {
                            row.child(
                                material_button("preview-source", ButtonVariant::Tonal)
                                    .icon(symbols::visibility())
                                    .child(course2md::tr!("预览视频"))
                                    .track_focus(&self.import_submit_focus)
                                    .disabled(self.preview_cancel.is_some())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.inspect_source(window, cx)
                                    })),
                            )
                        }),
                )
                .when_some(self.source_validation.clone(), |view, error| {
                    view.child(error_text("import-preview-error", error).whitespace_normal())
                }),
        )
    }

    /// While the video is read, a card in the shape its result will take.
    fn source_reading(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = course2md::tr!("取消读取视频");
        motion::state_enter(
            "source-reading-enter",
            tile([true; 4])
                .flex()
                .flex_col()
                .p(rem(16.))
                .gap(rem(16.))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap(rem(8.))
                        .child(
                            accessible_text(
                                "import-reading-state",
                                course2md::tr!("正在读取视频信息…"),
                            )
                            .role(Role::Status)
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal(),
                        )
                        .child(tip_button(
                            "cancel-source-read",
                            symbols::close(),
                            label,
                            |button| {
                                button.on_click(cx.listener(|this, _, _, cx| {
                                    this.invalidate_source();
                                    cx.notify();
                                }))
                            },
                        )),
                )
                .child(LinearProgress::new(
                    "source-reading-progress",
                    course2md::tr!("正在读取视频信息…"),
                )),
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

    /// The candidate list. Online lists queue several at once, but only candidates
    /// with a confirmed identity can be checked; the rest, and dropped local files
    /// (a local identity needs a content hash), open one at a time.
    fn candidate_list(&self, title: &str, cx: &mut Context<Self>) -> Stateful<Div> {
        let batch = self.online();
        let selectable = self.selectable_candidates().count();
        let selectable_selected = self
            .selectable_candidates()
            .filter(|candidate| self.selected_candidates.contains(&candidate.input))
            .count();
        // A checked candidate that already has a note gets a new version; say so beside the action.
        let noted_selected = self
            .source_candidates
            .iter()
            .filter(|candidate| {
                self.candidate_has_note(candidate)
                    && self.selected_candidates.contains(&candidate.input)
            })
            .count();
        let any_batchable = self
            .source_candidates
            .iter()
            .any(|candidate| candidate.identity.is_some());
        let selected_count = self.selected_candidates.len();
        let all_selected = selectable > 0 && selectable_selected == selectable;
        let rows = self
            .source_candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| self.candidate_row(index, candidate, batch, cx));
        let list = contain_scroll(
            div()
                .id("source-candidates")
                .max_h(rem(280.))
                .overflow_y_scroll(),
            &self.candidate_scroll,
        )
        .child(segmented_list(rows.collect::<Vec<_>>()));
        v_flex()
            .id("import-candidates")
            .w_full()
            .min_w_0()
            .gap(rem(12.))
            .child(
                div()
                    .px(ITEM_INSET)
                    .child(section_title("import-collection-title", title.to_owned())),
            )
            .child(list)
            .when(batch && any_batchable, |view| {
                view.child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .items_center()
                        .gap(rem(12.))
                        .child(
                            material_button("toggle-all-candidates", ButtonVariant::Text)
                                .child(if all_selected {
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
                                            .map(|candidate| candidate.input.clone())
                                            .collect();
                                        this.selected_candidates.extend(inputs);
                                    }
                                    cx.notify();
                                })),
                        )
                        .when(noted_selected > 0, |row| {
                            row.child(
                                accessible_text(
                                    "candidates-new-version",
                                    course2md::tr!("{} 个已有笔记，将生成新版", noted_selected),
                                )
                                .flex_1()
                                .min_w_0()
                                .text_size(TEXT_AUX)
                                .text_color(color(ColorRole::WARNING))
                                .whitespace_normal(),
                            )
                        })
                        .child(
                            // An empty selection cannot run, as with 开始转换.
                            material_button("start-batch-conversion", ButtonVariant::Filled)
                                .ml_auto()
                                .icon(symbols::arrow_forward())
                                .child(if selected_count > 0 {
                                    course2md::tr!("整理选中的 {} 个视频", selected_count)
                                } else {
                                    course2md::tr!("整理选中的视频").to_owned()
                                })
                                .disabled(selected_count == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.enqueue_selected_candidates(cx)
                                })),
                        ),
                )
            })
    }

    fn candidate_row(
        &self,
        index: usize,
        candidate: &source::SourceCandidate,
        batch: bool,
        cx: &mut Context<Self>,
    ) -> ListItem {
        let input = candidate.input.clone();
        let untitled =
            candidate.title.trim().is_empty() || candidate.title.trim() == candidate.input.trim();
        let title = if untitled {
            course2md::tr!("视频 {}", index + 1)
        } else {
            candidate.title.clone()
        };
        let duration = candidate
            .duration
            .filter(|value| *value > 0. && value.is_finite() && !untitled)
            .map(course2md::render::fmt_ts);
        let cover = self.source_candidate_covers.get(&candidate.input).cloned();
        let thumbnail = match cover {
            Some(cover) => img(cover)
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => h_flex()
                .size_full()
                .justify_center()
                .child(cover_placeholder(rem(20.)))
                .into_any_element(),
        };
        let row = ListItem::new(("source-candidate", index), title)
            .leading(ListLeading::Video(Some(thumbnail)))
            .supporting(duration.unwrap_or_else(|| candidate.input.clone()));
        if batch && candidate.identity.is_some() {
            let selected = self.selected_candidates.contains(&input);
            row.when(self.candidate_has_note(candidate), |row| {
                row.trailing_text(course2md::tr!("已有笔记"))
            })
            .trailing(ListTrailing::Checkbox(selected))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.selected_candidates.remove(&input) {
                    this.selected_candidates.insert(input.clone());
                }
                cx.notify();
            }))
        } else {
            row.trailing(ListTrailing::Icon(symbols::arrow_forward().into()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_source_input(input.clone(), window, cx)
                }))
        }
    }

    /// A source failure beside the field, with retry, login and details still reachable.
    fn source_preview_error(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let error = self.preview_error.as_ref()?;
        if !self.source_candidates.is_empty() {
            return Some(supporting("import-source-error", error.clone()).into_any_element());
        }
        let needs_login = source::legacy_login_failure_from_text(error);
        let message = if needs_login {
            course2md::tr!("视频需要登录后读取，请登录后重试预览")
        } else {
            readable_first_line(error)
                .unwrap_or(course2md::tr!("暂时无法读取这个视频，请检查链接或稍后重试"))
        };
        let open = self.show_preview_details;
        Some(motion::state_enter(
            "source-error",
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .child(
                    notice_banner(
                        "import-source-failure",
                        NoticeKind::Danger,
                        message.to_owned(),
                    )
                    .child(
                        h_flex()
                            .gap(rem(8.))
                            .flex_wrap()
                            .child(
                                material_button("retry-source", ButtonVariant::Outlined)
                                    .icon(symbols::refresh())
                                    .child(course2md::tr!("重试预览"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.inspect_source(window, cx)
                                    })),
                            )
                            .when(
                                self.online()
                                    && needs_login
                                    && course2md::auth::is_bilibili_url(
                                        &self.value(Field::Source, cx),
                                    ),
                                |row| {
                                    row.child(
                                        material_button(
                                            "source-login-repair",
                                            ButtonVariant::Outlined,
                                        )
                                        .icon(bilibili_mark())
                                        .child(course2md::tr!("登录 Bilibili"))
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.open_account_dialog(window, cx)
                                            }),
                                        ),
                                    )
                                },
                            )
                            .child(details_toggle("source-error-details", open).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.show_preview_details = !this.show_preview_details;
                                    cx.notify();
                                }),
                            )),
                    ),
                )
                .child(motion::disclosure(
                    "source-error-disclosure",
                    open,
                    v_flex().child(supporting_info(
                        "import-source-error-details",
                        error.clone(),
                    )),
                )),
        ))
    }

    /// The previewed video: cover, identity, what its text will come from and the one
    /// action that submits it. Decisions about this source stay in the card.
    fn selected_video(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let source = self.source_preview.clone()?;
        let duration = (source.duration.is_finite() && source.duration > 0.)
            .then(|| course2md::render::fmt_ts(source.duration));
        let details_open = self.show_preview_details;
        let metadata = source_metadata(&source);
        let cover = video_cover(source.cover.clone(), cover_width(window))
            .self_start()
            .when_some(duration, |cover, duration| {
                cover.child(
                    div()
                        .absolute()
                        .right(rem(8.))
                        .bottom(rem(8.))
                        .px(rem(8.))
                        .py(rem(2.))
                        .rounded(RADIUS_SMALL)
                        .bg(color(ColorRole::ON_SURFACE))
                        .text_color(color(ColorRole::SURFACE))
                        .text_size(TEXT_AUX)
                        .child(duration),
                )
            });
        let details = v_flex()
            .gap(rem(12.))
            .pt(rem(16.))
            .child(supporting_info("import-source-input", source.input.clone()))
            .when(
                source.online && course2md::auth::is_bilibili_url(&source.input),
                |view| view.child(self.source_account_row(cx)),
            );
        let actions = h_flex()
            .w_full()
            .min_w_0()
            .mt_auto()
            .gap(rem(8.))
            .items_center()
            .flex_wrap()
            .when(self.online(), |row| {
                row.child(tip_button(
                    "reread-source",
                    symbols::refresh(),
                    course2md::tr!("重新读取"),
                    |button| {
                        button.on_click(
                            cx.listener(|this, _, window, cx| this.inspect_source(window, cx)),
                        )
                    },
                ))
            })
            // The trailing actions wrap together and stay at the end.
            .child(
                h_flex()
                    .ml_auto()
                    .gap(rem(8.))
                    .items_center()
                    .child(
                        material_button("replace-source", ButtonVariant::Outlined)
                            .child(course2md::tr!("更换视频"))
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
                    .when(self.can_start_input(cx), |row| {
                        row.child(
                            material_button("start-conversion", ButtonVariant::Filled)
                                .icon(symbols::play_arrow())
                                .child(course2md::tr!("开始转换"))
                                .track_focus(&self.import_submit_focus)
                                .disabled(
                                    self.pending_conversion.is_some()
                                        || self.preview_cancel.is_some(),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.start_conversion(window, cx)
                                })),
                        )
                    }),
            );
        let info = v_flex()
            .flex_1()
            .flex_basis(rem(240.))
            .min_w_0()
            .gap(rem(8.))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap(rem(8.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(rem(4.))
                            .child(video_title(
                                "import-selected-title".to_owned(),
                                source.title.clone(),
                            ))
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap(rem(4.))
                                    .items_center()
                                    .when(!metadata.is_empty(), |row| {
                                        row.child(supporting("import-source-meta", metadata))
                                    })
                                    .child(tip_button(
                                        "source-details",
                                        if details_open {
                                            symbols::keyboard_arrow_up()
                                        } else {
                                            symbols::keyboard_arrow_down()
                                        },
                                        course2md::tr!("来源详情"),
                                        |button| {
                                            button.button_size(ButtonSize::ExtraSmall).on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.show_preview_details =
                                                        !this.show_preview_details;
                                                    cx.notify();
                                                }),
                                            )
                                        },
                                    )),
                            ),
                    )
                    .child(tip_button(
                        "clear-source-preview",
                        symbols::close(),
                        course2md::tr!("移除预览"),
                        |button| {
                            button.on_click(cx.listener(|this, _, _, cx| {
                                this.invalidate_source();
                                this.save_current_draft(cx);
                                cx.notify();
                            }))
                        },
                    )),
            )
            .child(self.text_source_control(&source, window, cx))
            .child(actions);
        let recovery = self.conversion_recovery(cx);
        Some(motion::state_enter(
            "import-selected-source-enter",
            tile([true, true, false, false])
                .id("import-selected-source")
                .flex()
                .flex_col()
                .p(rem(16.))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .items_stretch()
                        .gap(rem(20.))
                        .child(cover)
                        .child(info),
                )
                .child(motion::disclosure(
                    "source-details-disclosure",
                    details_open,
                    details,
                ))
                .children(recovery.map(|recovery| recovery.pt(rem(16.)))),
        ))
    }

    /// What this video's text will come from, and everything that changes it, in one
    /// chip and its menu; a read in flight or a failure takes over beneath it.
    fn text_source_control(
        &self,
        source: &source::Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let speech = self.import_uses_speech();
        let mode = self.task_options().source_mode;
        let failure = self.subtitle_error.as_deref().or_else(|| {
            match (&source.subtitles, subtitle_needs_confirmation(source, mode)) {
                (SubtitleEvidence::Failed { message }, true) => Some(message.as_str()),
                _ => None,
            }
        });
        let pending = source.subtitle_request.is_some() || source.subtitle_read_error.is_some();
        let value: SharedString = if self.subtitle_loading {
            course2md::tr!("正在读取字幕…").into()
        } else if !speech && pending {
            course2md::tr!("字幕还没读取完成").into()
        } else if speech {
            course2md::tr!("语音识别").into()
        } else if let Some(subtitle) = &source.selected_subtitle {
            subtitle.label.clone().into()
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
        let trigger = AssistChip::new("text-source", value).icon(if speech {
            symbols::mic()
        } else {
            symbols::subtitles()
        });
        let chip = ui::menu(
            "text-source-menu",
            trigger,
            self.text_source_menu(source, cx),
            window,
            cx,
        );
        let login = source.online
            && course2md::auth::is_bilibili_url(&source.input)
            && self
                .subtitle_error
                .as_deref()
                .or(match &source.subtitles {
                    SubtitleEvidence::Failed { message } => Some(message.as_str()),
                    _ => None,
                })
                .is_some_and(source::legacy_login_failure_from_text);
        let notice = match &source.subtitles {
            _ if !speech || self.subtitle_loading => None,
            SubtitleEvidence::NoneFound => Some(course2md::tr!("未找到字幕").to_owned()),
            SubtitleEvidence::Unsupported { message } => Some(message.clone()),
            _ => None,
        };
        let warning = match &source.subtitles {
            SubtitleEvidence::Found {
                warning: Some(message),
                ..
            } if self.subtitle_error.is_none() => {
                Some(course2md::tr!("部分字幕无法读取：{}", message))
            }
            _ => None,
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(8.))
            .child(h_flex().min_w_0().child(chip))
            .when_some(notice, |view, notice| {
                view.child(supporting_info("import-subtitle-message", notice))
            })
            .when(self.subtitle_loading, |view| {
                view.children(ui::async_status(
                    "subtitle-reading",
                    &ui::AsyncStatus::Busy(course2md::tr!("正在读取字幕…").into()),
                    None,
                    cx,
                ))
                .child(
                    material_button("cancel-subtitle-read", ButtonVariant::Text)
                        .icon(symbols::close())
                        .self_start()
                        .child(course2md::tr!("取消读取字幕"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pending_conversion = None;
                            this.abort_subtitle_read();
                            this.save_current_draft(cx);
                            cx.notify();
                        })),
                )
            })
            .when_some(failure.map(str::to_owned).or(warning), |view, message| {
                view.child(self.subtitle_issue(&message, failure.is_some(), source, login, cx))
            })
    }

    /// The tracks, speech recognition, and the ways to get more subtitles.
    fn text_source_menu(&self, source: &source::Source, cx: &mut Context<Self>) -> MaterialMenu {
        let speech = self.import_uses_speech();
        let cached = source
            .selected_subtitle
            .as_ref()
            .map(|subtitle| subtitle.track_id.clone());
        let mut tracks = source.subtitles.tracks().to_vec();
        if let Some(pending) = &source.subtitle_request
            && !tracks.iter().any(|track| track.id == pending.id)
        {
            tracks.push(pending.clone());
        }
        let selected = source
            .subtitle_request
            .as_ref()
            .map(|track| track.id.clone())
            .or_else(|| cached.clone())
            .filter(|_| !speech);
        let mut items: Vec<MaterialMenuItem> = tracks
            .into_iter()
            .map(|track| {
                let id = track.id.clone();
                let label = course2md::subtitle::track_label(&track);
                let confirmed = cached.as_ref() == Some(&id);
                MaterialMenuItem::new(label)
                    .leading(Icon::from(symbols::subtitles()))
                    .selected(selected.as_ref() == Some(&id))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if confirmed {
                            this.use_confirmed_subtitle(cx);
                        } else {
                            this.confirm_subtitle(track.clone(), true, cx);
                        }
                    }))
            })
            .collect();
        if let Some(subtitle) = source.selected_subtitle.as_ref().filter(|subtitle| {
            !source
                .subtitles
                .tracks()
                .iter()
                .any(|track| track.id == subtitle.track_id)
        }) {
            items.push(
                MaterialMenuItem::new(subtitle.label.clone())
                    .leading(Icon::from(symbols::subtitles()))
                    .selected(selected.as_ref() == Some(&subtitle.track_id))
                    .on_click(cx.listener(|this, _, _, cx| this.use_confirmed_subtitle(cx))),
            );
        }
        items.push(
            MaterialMenuItem::new(course2md::tr!("语音识别"))
                .leading(Icon::from(symbols::mic()))
                .selected(speech)
                .divider(true)
                .on_click(cx.listener(|this, _, _, cx| this.use_speech(cx))),
        );
        items.push(
            MaterialMenuItem::new(course2md::tr!("重新读取字幕"))
                .leading(Icon::from(symbols::refresh()))
                .disabled(self.subtitle_loading)
                .on_click(cx.listener(|this, _, _, cx| this.retry_subtitles(cx))),
        );
        if !source.online {
            items.push(
                MaterialMenuItem::new(course2md::tr!("选择字幕文件"))
                    .leading(Icon::from(symbols::file_upload()))
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.choose_attached_subtitle(window, cx)
                        }),
                    ),
            );
        }
        MaterialMenu::new("text-source-items").items(items)
    }

    /// A subtitle that could not be read: what it means, and the ways on from here.
    fn subtitle_issue(
        &self,
        message: &str,
        failed: bool,
        source: &source::Source,
        login: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let summary = if source::legacy_login_failure_from_text(message) {
            course2md::tr!("字幕需要登录，或改用语音识别")
        } else {
            readable_first_line(message)
                .unwrap_or(course2md::tr!("无法读取字幕，可重试或改用语音识别"))
        };
        let open = self.expanded_subtitle_issue.is_some();
        let details = (summary != message).then(|| message.to_owned());
        let retry = material_button("retry-subtitles", ButtonVariant::Text)
            .icon(symbols::refresh())
            .child(course2md::tr!("重新读取字幕"))
            .disabled(self.subtitle_loading)
            .on_click(cx.listener(|this, _, _, cx| this.retry_subtitles(cx)))
            .into_any_element();
        let status = if failed {
            ui::AsyncStatus::Warning(course2md::tr!("字幕未读取成功：{}", summary).into())
        } else {
            ui::AsyncStatus::Warning(summary.to_owned().into())
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(8.))
            .children(ui::async_status("subtitle-issue", &status, Some(retry), cx))
            .child(
                h_flex()
                    .gap(rem(8.))
                    .flex_wrap()
                    .when_some(
                        source.selected_subtitle.as_ref().filter(|_| failed),
                        |row, previous| {
                            row.child(
                                MaterialTooltip::plain(
                                    "use-previous-subtitle-tooltip",
                                    previous.label.clone(),
                                )
                                .trigger(
                                    material_button(
                                        "use-previous-subtitle",
                                        ButtonVariant::Outlined,
                                    )
                                    .icon(symbols::subtitles())
                                    .child(course2md::tr!("继续使用之前的字幕"))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.use_confirmed_subtitle(cx)
                                        }),
                                    ),
                                ),
                            )
                        },
                    )
                    .when(login, |row| {
                        row.child(
                            material_button("login-for-subtitles", ButtonVariant::Outlined)
                                .icon(bilibili_mark())
                                .child(course2md::tr!("登录 Bilibili"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_account_dialog(window, cx)
                                })),
                        )
                    })
                    .when(details.is_some(), |row| {
                        row.child(details_toggle("subtitle-issue-details", open).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.expanded_subtitle_issue =
                                    (!open).then(|| "subtitle-issue".into());
                                cx.notify();
                            }),
                        ))
                    }),
            )
            .when_some(details, |view, details| {
                view.child(motion::disclosure(
                    "subtitle-technical",
                    open,
                    v_flex().child(supporting_info("import-subtitle-issue-details", details)),
                ))
            })
    }

    /// 文字来源: the route, and the speech recognition route beneath it when speech
    /// may be used. The hint describes the mode; the video card says what this video
    /// will actually use.
    fn import_text_mode_options(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ui::FieldTable {
        let mode = self.task_options().source_mode;
        let hint = match mode {
            TranscriptSource::Subtitle => course2md::tr!("只使用视频自带的字幕"),
            TranscriptSource::Asr => course2md::tr!("将使用语音识别"),
            TranscriptSource::Auto => {
                course2md::tr!("有字幕时直接用字幕，没有字幕时使用语音识别")
            }
        };
        let selector =
            ConnectedButtonGroup::new("conversion-text-mode", course2md::tr!("文字来源方式"))
                .full_width()
                .options(text_source_choices().map(|(mode, label)| (text_source_id(mode), label)))
                .selected(text_source_id(mode))
                .on_activate(cx.listener(|this, value: &SharedString, _, cx| {
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
        // Speech settings matter only when this video may need speech:
        // a previewed video whose subtitles will be used does not.
        let speech_relevant = mode == TranscriptSource::Asr
            || (mode == TranscriptSource::Auto
                && self.source_preview.as_ref().is_none_or(|source| {
                    self.subtitle_loading || subtitle_needs_confirmation(source, mode)
                }));
        let mut fields = vec![
            ui::Field::below(
                "conversion-text-mode-field",
                course2md::tr!("文字来源方式"),
                selector,
            )
            .hint(hint),
        ];
        if speech_relevant {
            fields.extend(self.import_speech_options(window, cx));
        }
        ui::field_table(
            "conversion-text-mode-label",
            course2md::tr!("文字来源"),
            fields,
        )
    }

    /// AI 处理: one switch per step, then the translation target and the service
    /// the text is sent to.
    fn import_ai_options(&self, cx: &mut Context<Self>) -> ui::FieldTable {
        let custom_rule = self.task_options().llm
            && self
                .import_base_config()
                .llm
                .prompt
                .as_ref()
                .is_some_and(|prompt| !prompt.trim().is_empty());
        let mut fields: Vec<ui::Field> = [
            ConversionAiOption::Proofread,
            ConversionAiOption::Summary,
            ConversionAiOption::Vision,
        ]
        .into_iter()
        .map(|option| {
            let hint = if option == ConversionAiOption::Proofread && custom_rule {
                course2md::tr!("使用自定义校对规则")
            } else {
                ""
            };
            ui::Field::switch(
                format!("import-ai-option-{option:?}"),
                conversion_ai_option_label(option),
                conversion_ai_option_enabled(self.task_options(), option),
                cx.listener(move |this, enabled: &bool, _, cx| {
                    let enabled = *enabled;
                    this.set_task_option(|o| apply_conversion_ai_option(o, option, enabled), cx);
                }),
            )
            .hint(hint)
            .disabled(option == ConversionAiOption::Vision && !self.task_options().llm)
        })
        .collect();
        // Bilingual translation is a per-task choice; the global default is only
        // the starting value inherited by this input.
        let translated = self
            .task_options()
            .translate
            .as_deref()
            .and_then(course2md::language::target);
        let targets = std::iter::once((None, course2md::tr!("不翻译"))).chain(
            course2md::language::TARGETS
                .iter()
                .map(|(code, _)| (Some(*code), course2md::language::native_name(code))),
        );
        fields.push(
            ui::Field::menu(
                "import-ai-translate",
                course2md::tr!("双语翻译"),
                translated
                    .map(course2md::language::native_name)
                    .unwrap_or(course2md::tr!("不翻译")),
                MaterialMenu::new("import-ai-translate-items").items(ui::choice_items(
                    targets,
                    &translated,
                    cx.listener(|this, code: &Option<&'static str>, _, cx| {
                        let code = *code;
                        this.set_task_option(|o| o.translate = code.map(str::to_owned), cx);
                    }),
                )),
            )
            .hint(if translated.is_some() {
                course2md::tr!("每段文字下方附上译文；已是该语言的视频不翻译")
            } else {
                ""
            }),
        );
        if self.task_uses_ai() {
            let consequence = self
                .selected_task_service(ServicePurpose::Ai)
                .map(|selected| {
                    course2md::tr!(
                        "{}会发送到 {}",
                        if self.task_options().llm && self.task_options().vision {
                            course2md::tr!("文字及对应截图")
                        } else {
                            course2md::tr!("文字")
                        },
                        selected.config.host()
                    )
                });
            fields.push(
                ui::Field::below(
                    "import-ai-service",
                    course2md::tr!("AI 服务"),
                    self.task_service_picker(ServicePurpose::Ai, cx),
                )
                .hint(consequence.unwrap_or_default()),
            );
        }
        fields.push(ui::Field::inline(
            "reset-task-overrides-field",
            course2md::tr!("恢复 AI 默认"),
            material_button("reset-task-overrides", ButtonVariant::Text)
                .icon(symbols::restart_alt())
                .child(course2md::tr!("恢复默认"))
                .on_click(cx.listener(|this, _, _, cx| this.reset_task_ai_overrides(cx))),
        ));
        ui::field_table("import-ai-label", course2md::tr!("AI 处理"), fields)
    }

    fn reset_task_ai_overrides(&mut self, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
        if self
            .transact(
                course2md::tr!("本次选项尚未恢复默认：{}"),
                |state| {
                    state
                        .draft_mut()
                        .context(course2md::tr!("当前视频输入暂时不可用"))?
                        .reset_ai_overrides(&defaults);
                    Ok(())
                },
            )
            .is_some()
        {
            self.advance_conversion_when_ready(cx);
        }
        cx.notify();
    }

    /// Where speech recognition runs, and the service or model that takes.
    fn import_speech_options(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<ui::Field> {
        use course2md::config::AsrProvider;
        let cloud = self.task_options().uses_cloud_provider();
        let (provider, model, root) = self.import_model_request();
        let shown = if cloud {
            course2md::tr!("语音识别服务").to_owned()
        } else {
            format!(
                "{} · {}",
                course2md::tr!("本机 · {}", course2md::config::model_label(&model)),
                self.local_engine_name()
            )
        };
        let current = self.task_options().provider;
        let locations: Vec<_> = crate::providers()
            .iter()
            .filter_map(|(id, label)| {
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
        let location = ui::Field::menu(
            "import-speech-location",
            course2md::tr!("语音识别"),
            shown,
            MaterialMenu::new("import-speech-location-items").items(ui::choice_items(
                locations,
                &current,
                cx.listener(|this, provider: &Option<AsrProvider>, _, cx| {
                    let provider = *provider;
                    this.set_task_option(|o| o.provider = provider, cx);
                }),
            )),
        );
        let route = if cloud {
            ui::Field::below(
                "import-speech-service",
                course2md::tr!("语音识别服务"),
                self.task_service_picker(ServicePurpose::Speech, cx),
            )
            .hint(course2md::tr!("音频会发送到所选语音识别服务"))
        } else {
            // One quiet status line and its action; Settings keeps the table.
            ui::Field::below(
                "import-speech-model",
                course2md::config::model_label(&model).to_owned(),
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(8.))
                    .child(self.model_status_line(provider, Some(&model), &root, window, cx))
                    .child(
                        material_button("import-model-settings", ButtonVariant::Text)
                            .self_start()
                            .icon(symbols::settings())
                            .child(course2md::tr!("模型设置"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                            ),
                    ),
            )
        };
        vec![location, route]
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
    fn import_destination(&self, cx: &mut Context<Self>) -> ui::FieldTable {
        let current = self.draft().map(|draft| draft.library_id.clone());
        let libraries = self
            .workspace
            .as_ref()
            .map(|workspace| workspace.state.libraries.clone())
            .unwrap_or_default();
        let library_name = libraries
            .iter()
            .find(|library| Some(&library.id) == current.as_ref())
            .map(|library| library.name.clone())
            .unwrap_or_else(|| course2md::tr!("选择保存位置").to_owned());
        let library_items = libraries.iter().map(|library| {
            (
                Some(library.id.clone()),
                format!("{} · {}", library.name, library.root.display()),
            )
        });
        let mut fields = vec![
            ui::Field::menu(
                "import-library",
                course2md::tr!("笔记库"),
                library_name,
                MaterialMenu::new("import-library-items").items(ui::choice_items(
                    library_items,
                    &current,
                    cx.listener(|this, id: &Option<String>, _, cx| {
                        if let Some(id) = id {
                            this.set_import_library(id.clone(), cx);
                        }
                    }),
                )),
            )
            .disabled(libraries.len() < 2),
        ];
        if self.source_preview.is_some() {
            fields.push(ui::Field::below(
                "import-title",
                course2md::tr!("笔记名称"),
                RevealFocus::new(
                    ("import-title-focus", self.validation_attempt),
                    TextField::new("import-title-field", &self.inputs[&Field::Title])
                        .w_full()
                        .outlined()
                        .error(self.title_required)
                        .when_some(
                            self.source_validation
                                .clone()
                                .filter(|_| self.title_required),
                            |field, message| field.supporting_text(message),
                        ),
                    self.scrolls[Page::New as usize].clone(),
                ),
            ));
        }
        fields.push(ui::Field::below(
            "import-folder",
            course2md::tr!("文件夹"),
            h_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .items_center()
                .child(div().flex_1().min_w_0().child(self.folder_picker(cx)))
                .child(tip_button(
                    "import-new-folder",
                    symbols::create_new_folder(),
                    course2md::tr!("新建文件夹"),
                    |button| {
                        button.variant(IconButtonVariant::Tonal).on_click(
                            cx.listener(|this, _, window, cx| this.begin_folder(None, window, cx)),
                        )
                    },
                )),
        ));
        let chips = ["Markdown", course2md::tr!("网页"), "JSON"]
            .into_iter()
            .enumerate()
            .map(|(index, label)| {
                ChipSetItem::new(
                    FilterChip::new(("import-export", index), label)
                        .selected(self.task_options().formats[index])
                        .on_toggle(cx.listener(move |this, on: &bool, _, cx| {
                            let on = *on;
                            this.update_task_options(|o| o.formats[index] = on, cx);
                            cx.notify();
                        })),
                )
            });
        fields.push(ui::Field::below(
            "import-exports",
            course2md::tr!("同时导出"),
            ChipSet::new("import-export-chips", course2md::tr!("同时导出")).items(chips),
        ));
        if self.online() {
            fields.push(
                ui::Field::switch(
                    "import-keep-video",
                    course2md::tr!("保留原视频"),
                    self.task_options().keep_video,
                    cx.listener(|this, value: &bool, _, cx| {
                        let value = *value;
                        this.update_task_options(|o| o.keep_video = value, cx);
                        cx.notify();
                    }),
                )
                .hint(course2md::tr!("供离线播放，会占用更多磁盘空间")),
            );
        }
        ui::field_table("import-save-label", course2md::tr!("保存"), fields)
    }

    fn set_import_library(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        if self
            .transact(course2md::tr!("保存位置尚未更新：{}"), |state| {
                if let Some(draft) = state.draft_mut() {
                    draft.library_id = id;
                    draft.folder = None;
                }
                Ok(())
            })
            .is_some()
        {
            self.advance_conversion_when_ready(cx);
        }
        cx.notify();
    }

    /// The preview card is on screen (not replaced by reading or a task).
    fn preview_card_shown(&self, cx: &App) -> bool {
        self.preview_cancel.is_none()
            && self.source_preview.is_some()
            && self.workbench_linked_task(cx).is_none()
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
        let state = &self.workspace.as_ref()?.state;
        let input = self.value(Field::Source, cx);
        state
            .draft()
            .and_then(|draft| locked_input_task(draft, &state.tasks, &input))
            .or_else(|| self.workbench_linked_task(cx))
    }

    /// The processing options: a header tile that shows or hides them, joined to the
    /// video card above so they read as that video's, then one section per subject.
    /// A submitted task's options are its saved plan, read-only.
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
        let top = !attached;
        let scale = TileScale::Regular;
        // The plan joins the header; editable options stand below it as sections.
        let joined = open && owner.is_some();
        let header = scale
            .button("generation-options-toggle", [top, top, !joined, !joined])
            .accessibility_label(if open {
                course2md::tr!("收起处理选项")
            } else {
                course2md::tr!("展开处理选项")
            })
            .on_click(cx.listener(move |this, _, _, cx| this.set_home_options(!open, cx)))
            .child(
                scale
                    .row()
                    .child(Icon::from(symbols::tune()).color(color(ColorRole::ON_SURFACE_VARIANT)))
                    .child(scale.text(
                        "generation-options-title",
                        course2md::tr!("处理选项"),
                        Some(applies_to),
                    ))
                    .child(disclosure_chevron(open)),
            );
        let body = match &owner {
            Some(task) => v_flex().w_full().min_w_0().child(
                tile([false, false, true, true])
                    .flex()
                    .flex_col()
                    .gap(rem(12.))
                    .p(ITEM_INSET)
                    .children(self.task_plan_rows(task)),
            ),
            None => v_flex()
                .w_full()
                .min_w_0()
                .pt(HERO_GAP)
                .gap(rem(24.))
                .child(self.import_text_mode_options(window, cx))
                .child(self.import_ai_options(cx))
                .child(self.import_destination(cx)),
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(TILE_GAP)
            .child(header)
            .child(motion::disclosure("generation-options-body", open, body))
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
        let Some(workspace) = &self.workspace else {
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
        let reset = self.transact(
            course2md::tr!("新的视频输入尚未建立：{}"),
            |state| {
                state.reset_input(online, defaults, destination);
                Ok(())
            },
        );
        if reset.is_none() {
            cx.notify();
            return false;
        }
        self.completed_source = None;
        self.invalidate_source();
        self.restore_draft(window, cx);
        self.source_editor_open = true;
        true
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
        let compact = is_compact(window);
        let opening_result = self.reading && self.following_conversion.is_some();
        let linked_task = self.workbench_linked_task(cx).cloned();
        let cancelled = self
            .current_input_task(cx)
            .filter(|task| task.state == workspace::TaskState::Cancelled)
            .map(|task| task.id.clone());
        // The current input's own task takes the preview card's place and shape: a
        // running task shows its live status, a settled one what it needs or what opens.
        let card = match linked_task {
            Some(task) if opening_result && task.state.finished() => {
                Some(self.task_opening_result(&task, cx))
            }
            Some(task) => Some(self.task_card(&task, CardPlace::Home, window, cx)),
            None if self.preview_card_shown(cx) => self.selected_video(window, cx),
            None => None,
        };
        let reading = self
            .preview_cancel
            .is_some()
            .then(|| self.source_reading(cx));
        let attached = card.is_some();
        let options = motion::state_enter(
            "generation-options-panel",
            self.workbench_options(attached, window, cx),
        );
        let candidates = self
            .source_collection_title
            .clone()
            .map(|title| self.candidate_list(&title, cx));
        let title = TypeRole::DisplaySmall.scale();
        let hero = workbench_column()
            .gap(HERO_GAP)
            .child(
                accessible_text("workbench-title", course2md::tr!("把视频整理成笔记"))
                    .role(Role::Heading)
                    .w_full()
                    .text_center()
                    .text_size(title.font_size)
                    .line_height(title.line_height),
            )
            .child(self.source_input(cx))
            .children(reading)
            .children(candidates)
            .children(self.source_preview_error(cx))
            .child(
                // The options belong to one input: joined to its video card (the
                // preview or the task in its place), otherwise on their own.
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(TILE_GAP)
                    .children(card)
                    .child(options),
            )
            .when_some(cancelled, |view, id| {
                view.child(info_callout(
                    SharedString::from(format!("cancelled-input-task-{id}")),
                    course2md::tr!("任务已取消"),
                ))
            });
        let active = self.workbench_active_tasks_section(window, cx);
        let recent = self
            .recent_notes_section(cx)
            .map(|recent| motion::enter("home-recent-notes", recent));
        // With nothing below it, the input moves toward the optical center.
        let quiet = recent.is_none() && active.is_none() && !self.desktop_settings.home_options;
        let top = motion::value("home-hero-top", hero_top(quiet, compact), window, cx);
        v_flex()
            .w_full()
            .min_w_0()
            .pt(rem(top))
            .pb(rem(72.))
            .gap(rem(if compact { 36. } else { 48. }))
            .child(hero)
            .when_some(active, |view, active| {
                view.child(workbench_column().child(active))
            })
            .children(recent.map(|recent| workbench_column().child(recent)))
            // The whole workbench accepts a dropped video, not just the local tab.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_source_files(paths.paths(), window, cx)
            }))
            .into_any_element()
    }

    /// A followed result opening where its task ran.
    fn task_opening_result(
        &self,
        task: &workspace::TaskRecord,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = &task.id;
        motion::state_enter(
            SharedString::from(format!("task-opening-result-{id}")),
            tile([true, true, false, false])
                .flex()
                .flex_col()
                .gap(rem(12.))
                .p(rem(16.))
                .child(video_title(
                    format!("workbench-task-title-{id}"),
                    task.plan.title.clone(),
                ))
                .children(ui::async_status(
                    SharedString::from(format!("task-opening-{id}")),
                    &ui::AsyncStatus::Busy(course2md::tr!("正在打开…").into()),
                    None,
                    cx,
                )),
        )
    }

    /// Attach last to the full-window `workbench-drop` group, outside scrolling content.
    pub(super) fn workbench_drop_overlay(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("workbench-drop-overlay")
            .absolute()
            .inset_0()
            .invisible()
            .group_drag_over::<ExternalPaths>("workbench-drop", |style| style.visible())
            .bg(color(ColorRole::SURFACE_CONTAINER).opacity(0.88))
            .p(rem(24.))
            .child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap(rem(16.))
                    .rounded(RADIUS_HERO)
                    .border_2()
                    .border_dashed()
                    .border_color(color(ColorRole::PRIMARY))
                    .child(tinted_icon(symbols::file_upload(), Tint::Teal, rem(88.)))
                    .child(
                        div()
                            .text_size(TEXT_HEADLINE)
                            .font_weight(FontWeight::MEDIUM)
                            .child(course2md::tr!("松开以添加视频")),
                    ),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_source_files(paths.paths(), window, cx);
            }))
    }

    /// Derived once per draft, preference and source revision, not on every render.
    fn cached_preview_plan(&mut self) -> Result<workspace::TaskPlan, (String, bool)> {
        let key = (
            self.workspace
                .as_ref()
                .map_or(0, |workspace| workspace.state.revision),
            self.preferences
                .revision(crate::preferences::PreferenceGroup::Generation),
            self.preview_generation,
        );
        if let Some((workspace, preferences, source, plan)) = &self.preview_plan_cache
            && (*workspace, *preferences, *source) == key
        {
            return plan.clone();
        }
        let plan = self.preview_plan().map_err(|error| {
            let pending = error
                .downcast_ref::<crate::task_runner::PlanPending>()
                .is_some();
            (format!("{error:#}"), pending)
        });
        self.preview_plan_cache = Some((key.0, key.1, key.2, plan.clone()));
        plan
    }

    /// What stands between this video and a conversion, each obstacle with its one
    /// way forward. Only unresolved choices interrupt a submitted conversion.
    fn conversion_recovery(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let busy = self.job.is_some()
            || self.workspace.as_ref().is_some_and(|workspace| {
                workspace.state.tasks.iter().any(|task| {
                    matches!(
                        task.state,
                        workspace::TaskState::Queued | workspace::TaskState::Running
                    )
                })
            });
        let reading = self.preview_cancel.is_some() || self.subtitle_loading;
        let preview_plan = self.cached_preview_plan();
        let plan_pending = matches!(&preview_plan, Err((_, true)));
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
                let settled = self.source_preview.is_some()
                    && !reading
                    && !self.subtitle_attention_required();
                preview_plan
                    .as_ref()
                    .err()
                    .filter(|_| settled)
                    .map(|(message, _)| message.clone())
            });
        // Waiting for the component or location checks is progress, not a failure.
        let checking = !preferences_blocked && (self.environment.is_none() || plan_pending);
        let mut facts: Vec<AnyElement> = Vec::new();
        let mut actions: Vec<MaterialButton> = Vec::new();
        let button = |id: &'static str, variant, icon, label: &'static str| {
            material_button(id, variant).icon(icon).child(label)
        };
        if self.environment.as_ref().is_some_and(|environment| {
            !environment.engine
                || !environment.ffmpeg
                || !environment.ffprobe
                || (self.online() && !environment.ytdlp)
        }) {
            actions.push(
                button(
                    "repair-conversion-components",
                    ButtonVariant::Outlined,
                    symbols::settings(),
                    course2md::tr!("检查所需组件"),
                )
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
            actions.push(
                button(
                    "repair-generation-preferences",
                    ButtonVariant::Outlined,
                    symbols::refresh(),
                    if issue.can_retry {
                        course2md::tr!("重试保存处理选项")
                    } else {
                        course2md::tr!("重置处理选项")
                    },
                )
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
        if let Some(library) = self
            .workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .state
                    .library(&workspace.state.draft()?.library_id)
            })
            .filter(|library| {
                self.cached_location_check(library)
                    .is_some_and(|check| check.needs_reassociation)
            })
        {
            let id = library.id.clone();
            actions.push(
                button(
                    "reassociate-import-location",
                    ButtonVariant::Outlined,
                    symbols::storage(),
                    course2md::tr!("重新关联此保存位置"),
                )
                .disabled(self.storage_ui.busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.begin_library_reassociation(id.clone(), window, cx)
                })),
            );
        }
        let has_match = matching_task.is_some();
        if let Some((id, title, location)) = matching_task {
            let location = location.unwrap_or_else(|| course2md::tr!("原保存位置").into());
            facts.push(
                supporting_info(
                    "conversion-fact-consequence",
                    course2md::tr!("已有相同处理任务：《{}》（{}）", title, location),
                )
                .into_any_element(),
            );
            actions.push(
                button(
                    "show-matching-task",
                    ButtonVariant::Filled,
                    symbols::task(),
                    course2md::tr!("查看任务"),
                )
                .track_focus(&self.import_submit_focus)
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
            facts.push(
                supporting_info(
                    "existing-note-notice",
                    course2md::tr!("已有笔记，保存在「{}」", location),
                )
                .into_any_element(),
            );
            if busy {
                facts.push(
                    supporting_info(
                        "conversion-fact-queued",
                        course2md::tr!("将排在当前任务之后"),
                    )
                    .into_any_element(),
                );
            }
            actions.push(
                button(
                    "open-existing-note",
                    ButtonVariant::Filled,
                    symbols::menu_book(),
                    course2md::tr!("打开已有笔记"),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.save_current_draft(cx);
                    this.open_course(course.clone(), cx);
                })),
            );
            actions.push(
                button(
                    "generate-new-version",
                    ButtonVariant::Outlined,
                    symbols::refresh(),
                    course2md::tr!("生成新版笔记"),
                )
                .track_focus(&self.import_submit_focus)
                .disabled(reading || self.workspace.is_none() || preferences_blocked)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.following_conversion =
                        Some(ConversionFollow::Preparing(this.preview_generation));
                    this.enqueue_current(window, cx);
                })),
            );
        } else if let Some(purpose) = self.missing_import_service() {
            actions.push(
                button(
                    "configure-conversion-service",
                    ButtonVariant::Filled,
                    symbols::settings(),
                    match purpose {
                        ServicePurpose::Speech => course2md::tr!("设置语音识别服务"),
                        ServicePurpose::Ai => course2md::tr!("设置 AI 服务"),
                    },
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_task_service_editor(purpose, window, cx);
                })),
            );
        }
        if self.pending_conversion.is_some() && !reading {
            if self.source_validation.is_some()
                && self.missing_import_service().is_none()
                && self.existing_source_note().is_none()
                && !has_match
            {
                actions.push(
                    button(
                        "retry-conversion-validation",
                        ButtonVariant::Outlined,
                        symbols::refresh(),
                        course2md::tr!("重试转换"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.start_conversion(window, cx))),
                );
            }
            actions.push(
                button(
                    "cancel-pending-conversion",
                    ButtonVariant::Text,
                    symbols::close(),
                    course2md::tr!("取消转换"),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.pending_conversion = None;
                    this.following_conversion = None;
                    cx.notify();
                })),
            );
        }
        if let Some(message) = submission_error {
            let status = if checking {
                ui::AsyncStatus::Busy(message.into())
            } else {
                ui::AsyncStatus::Failed(message.into())
            };
            facts.extend(
                ui::async_status("import-submit-status", &status, None, cx)
                    .map(IntoElement::into_any_element),
            );
        }
        if facts.is_empty() && actions.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(12.))
                .children(facts)
                .child(
                    h_flex()
                        .w_full()
                        .gap(rem(8.))
                        .flex_wrap()
                        .justify_end()
                        .children(actions),
                ),
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
