//! One start intent continues source preparation through to a readable note.
use super::*;
use crate::{motion, preferences::ServicePurpose, theme::*};
use anyhow::Context as _;
use course2md::subtitle::{SubtitleEvidence, SubtitleReadError, SubtitleTrack};
use gpui_component::{
    checkbox::Checkbox,
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn help(text: impl Into<SharedString>) -> Div {
    // 与设置页同一辅助信息处理：共享 ⓘ 图标 + 常规字重（review5 Note）
    let text = text.into();
    theme::supporting_info(text_id("help", &text), text).text_sm()
}
fn issue(message: impl Into<SharedString>) -> Div {
    let message = message.into();
    div()
        .text_sm()
        .text_color(color(DANGER))
        .whitespace_normal()
        .child(accessible_text(text_id("issue", &message), message))
}
fn text_id(kind: &str, value: &str) -> SharedString {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hash);
    format!("import-{kind}-{:x}", hash.finish()).into()
}

fn service_destination(config: &crate::preferences::ServiceConfiguration) -> String {
    let host = config.host();
    if config.name == host {
        format!("「{host}」")
    } else {
        format!("「{}」（{host}）", config.name)
    }
}
/// Platform marks are brand assets; they do not belong inside the editable field.
fn platform_mark(name: &'static str, icon: Icon) -> Div {
    // 品牌提示按 metadata 角色降级（workbench#12）：不再是正文级高饱和焦点
    h_flex()
        .gap_2()
        .items_center()
        .flex_shrink_0()
        .child(icon.size(px(16.)).flex_shrink_0())
        .child(
            div()
                .text_size(TEXT_AUX)
                .text_color(color(MUTED))
                .child(name),
        )
}

/// Idle 工作台 conversion-options chrome. 高级选项 is the only disclosure;
/// conversion defaults live there as real controls, not a standalone callout.
// 设计决定（此前由恒值函数 + 源码嗅探测试钉住，无法被编译器发现且阻碍重构）：
// idle 工作台不显示 conversion-defaults callout；识别/引擎控件直接放在「高级选项」层，
// 不再嵌套 disclosure；AI 选项行不组合前导图标列。改动这些决定请直接改代码与本注释。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConversionAiOption {
    Proofread,
    Vision,
    Summary,
}

pub(crate) fn conversion_ai_option_label(option: ConversionAiOption) -> &'static str {
    match option {
        ConversionAiOption::Proofread => "AI 校对",
        ConversionAiOption::Vision => "发送截图辅助校对",
        ConversionAiOption::Summary => "生成摘要",
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

pub(crate) fn apply_text_source_mode(options: &mut ConversionOptions, mode: usize) {
    options.source_mode = mode;
}

pub(crate) fn apply_speech_location(
    options: &mut ConversionOptions,
    cloud: bool,
    local_provider: usize,
) {
    options.provider = if cloud {
        crate::CLOUD_PROVIDER_INDEX
    } else {
        local_provider
    };
}

pub(crate) fn apply_local_engine(options: &mut ConversionOptions, provider: usize) {
    options.provider = provider;
}

fn conversion_ai_preference_row(
    option: ConversionAiOption,
    hint: &'static str,
    control: Switch,
) -> Div {
    // AI 选项行不组合前导图标列（见文件顶部设计决定注释）
    crate::settings_ui::preference(None, conversion_ai_option_label(option), hint, control)
}

/// A shared heading for related conversion options. 图标由调用方显式给出（不做文案子串匹配）。
pub(crate) fn box_section(icon: Icon, label: &'static str) -> Div {
    v_flex().w_full().min_w_0().gap_3().child(
        h_flex()
            .gap_2()
            .items_center()
            .child(icon.size(px(20.)).text_color(color(ACCENT_STRONG)))
            .child(
                accessible_text(text_id("box-section", label), label)
                    .text_size(TEXT_TITLE)
                    .font_weight(FontWeight::SEMIBOLD),
            ),
    )
}
/// A consequence or recovery fact kept beside its action.
pub(crate) fn conversion_fact(value: impl Into<SharedString>, warning: bool) -> Stateful<Div> {
    let value = value.into();
    accessible_text(text_id("conversion-fact", &value), value)
        .w_full()
        .min_w_0()
        .whitespace_normal()
        .text_size(TEXT_BODY)
        .text_color(color(if warning { WARNING } else { INK }))
        .when(warning, |row| row.font_weight(FontWeight::SEMIBOLD))
}

fn uses_speech(source: &source::Source, source_mode: usize) -> bool {
    source_mode == 2
        || (source_mode == 0
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
    source_mode: usize,
    error: Option<&str>,
) -> bool {
    if source_mode != 0
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

fn subtitle_needs_confirmation(source: &source::Source, source_mode: usize) -> bool {
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
        || task
            .blocked
            .iter()
            .any(|request| request.reason == "uncertain")
        || (task.state != workspace::TaskState::Cancelled && task.artifact.is_none())
    {
        return None;
    }
    let submitted = tasks
        .iter()
        .find(|task| Some(task.id.as_str()) == draft.submitted_task.as_deref())?;
    (draft.title == submitted.plan.title
        && draft.options == submitted.plan.options
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
fn conversion_gate(
    requested_revision: u64,
    current_revision: u64,
    reading: bool,
    has_source: bool,
    needs_text_choice: bool,
    environment_ready: bool,
    library_ready: bool,
    existing_note: bool,
) -> ConversionGate {
    if requested_revision != current_revision {
        ConversionGate::Stop
    } else if reading {
        ConversionGate::Wait
    } else if !has_source {
        ConversionGate::Stop
    } else if needs_text_choice || !environment_ready || !library_ready {
        ConversionGate::Wait
    } else if existing_note {
        ConversionGate::Stop
    } else {
        ConversionGate::Submit
    }
}

impl Desktop {
    /// A start command owns preparation and submission for this source revision.
    pub fn start_conversion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_queued_message();
        if self.reading && self.page == Page::New {
            self.read_generation = self.read_generation.wrapping_add(1);
            self.reading = false;
        }
        let input = self.value(Field::Source, cx);
        if input != self.last_source_input {
            if !self.prepare_next_import(&input, window, cx) {
                return;
            }
            self.following_conversion = None;
            self.last_source_input = input.clone();
            self.invalidate_source();
        }
        if input.is_empty() {
            self.source_validation = Some(if self.online {
                "先粘贴视频链接".into()
            } else {
                "先选择一个视频".into()
            });
            if self.online {
                self.inputs[&Field::Source].update(cx, |input, cx| input.focus(window, cx));
            }
            cx.notify();
            return;
        }
        if self.source_preview.is_none() && self.preview_cancel.is_none() {
            self.inspect_source(window, cx);
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
        if generation == self.preview_generation
            && !reading
            && let Some(source) = &mut self.source_preview
            && automatic_subtitle_fallback(
                source,
                self.task_options.source_mode,
                self.subtitle_error.as_deref(),
            )
        {
            self.subtitle_error = None;
            if !self.save_current_draft(cx) {
                self.pending_conversion = None;
                return;
            }
        }
        match conversion_gate(
            generation,
            self.preview_generation,
            reading,
            self.source_preview.is_some(),
            self.subtitle_attention_required(),
            self.environment.is_some(),
            !self.loading,
            self.existing_source_note().is_some(),
        ) {
            ConversionGate::Wait => cx.notify(),
            ConversionGate::Stop => {
                self.pending_conversion = None;
                cx.notify();
            }
            ConversionGate::Submit => self.enqueue_current(window, cx),
        }
    }

    fn subtitle_issue(
        &self,
        kind: &str,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let summary = if source::is_login_failure(message) {
            "字幕需要登录后才能读取；可以登录，或改用语音识别"
        } else if message.contains("WARNING:") || message.contains("ERROR:") {
            "暂时无法读取字幕；可以重试，或改用语音识别"
        } else {
            message.lines().next().unwrap_or(message)
        };
        let details = if summary != message { message } else { "" };
        let id = text_id(kind, message);
        let expanded = self.expanded_subtitle_issue.as_ref() == Some(&id);
        v_flex()
            .min_w_0()
            .gap_2()
            .child(help(summary.to_owned()))
            .when(!details.trim().is_empty(), |view| {
                view.child(
                    quiet(id.clone())
                        .icon(if expanded {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .self_start()
                        .label(if expanded {
                            "收起技术详情"
                        } else {
                            "技术详情"
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
                    v_flex().child(help(details.to_owned())),
                    window,
                    cx,
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
        if let Some(cancel) = self.subtitle_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.subtitle_generation = self.subtitle_generation.wrapping_add(1);
        let generation = self.subtitle_generation;
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
            self.task_options.source_mode = 1;
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
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| {
                    Err(course2md::subtitle::SubtitleReadError::Failed {
                        message: "读取字幕的工作线程意外结束".into(),
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
                            this.task_options.source_mode = 1;
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
            prompt: Some("选择 SRT 或 VTT 字幕".into()),
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
                                this.subtitle_error = Some("请选择 SRT 或 VTT 字幕文件".into());
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
                        this.subtitle_error = Some(format!("无法打开字幕选择器：{error:#}"))
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
        if let Some(cancel) = self.subtitle_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.subtitle_generation = self.subtitle_generation.wrapping_add(1);
        let generation = self.subtitle_generation;
        let source_generation = self.preview_generation;
        let identity = source.identity.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.subtitle_cancel = Some(cancel.clone());
        self.subtitle_loading = true;
        self.subtitle_error = None;
        if let Some(source) = &mut self.source_preview {
            source.subtitle_read_error = Some(SubtitleReadError::Failed {
                message: "字幕重新读取尚未完成。可以重试，或继续使用已确认的正文。".into(),
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
                .unwrap_or_else(|_| Err(anyhow::anyhow!("刷新字幕的工作线程意外结束")));
            let _ = this.update(cx, |this, cx| {
                this.preview_workers = this.preview_workers.saturating_sub(1);
                if this.subtitle_generation != generation
                    || this.preview_generation != source_generation
                    || token.as_ref().is_some_and(|(id, revision)| !this
                        .workspace.as_ref()
                        .is_some_and(|workspace| workspace.state.matches_input(id, *revision)))
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
                                "zh-Hans",
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
                        }.cloned();
                        if old_id.is_some() && track.is_none() {
                            this.subtitle_error = Some("原来选择的字幕已无法重新读取。已确认的正文仍保留；可以继续使用它，或明确选择其他文字来源。".into());
                        }
                        if let Some(source) = &mut this.source_preview {
                            source.subtitles = evidence;
                            source.subtitle_read_error = this.subtitle_error.clone().map(|message| SubtitleReadError::Failed { message });
                        }
                        this.save_current_draft(cx);
                        if let Some(track) = track {
                            this.confirm_subtitle(track, false, cx);
                        }
                    }
                    Err(error) => {
                        this.subtitle_error = Some(format!("{error:#}"));
                        if let Some(source) = &mut this.source_preview {
                            source.subtitle_read_error = this.subtitle_error.clone().map(|message| SubtitleReadError::Failed { message });
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

    fn use_speech(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.subtitle_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.subtitle_generation = self.subtitle_generation.wrapping_add(1);
        self.subtitle_loading = false;
        self.subtitle_error = None;
        self.task_options.source_mode = 2;
        if let Some(source) = &mut self.source_preview {
            source.subtitle_request = None;
            source.subtitle_read_error = None;
        }
        self.save_current_draft(cx);
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
        if let Some(cancel) = self.subtitle_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.subtitle_generation = self.subtitle_generation.wrapping_add(1);
        self.subtitle_loading = false;
        self.subtitle_error = None;
        self.task_options.source_mode = 1;
        if let Some(source) = &mut self.source_preview {
            source.subtitle_request = None;
            source.subtitle_read_error = None;
        }
        self.save_current_draft(cx);
        self.advance_conversion_when_ready(cx);
        cx.notify();
    }

    fn select_source_input(&mut self, input: String, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs[&Field::Source].update(cx, |state, cx| state.set_value(input, window, cx));
        self.start_conversion(window, cx);
    }

    fn drop_source_files(
        &mut self,
        files: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.online {
            self.switch_source_kind(false, window, cx);
        }
        match files {
            [path] => {
                self.inputs[&Field::Source].update(cx, |state, cx| {
                    state.set_value(path.display().to_string(), window, cx)
                });
            }
            [] => {}
            _ => {
                self.invalidate_source();
                self.source_collection_title = Some("拖入了多个文件，请选择本次处理的视频".into());
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
        div().w_full().child(
            SingleChoiceGroup::new("source-kind", "视频来源")
                .options([("online", "视频链接"), ("local", "本地文件")])
                .icon("online", icons::link())
                .icon("local", icons::movie())
                .selected(if self.online { "online" } else { "local" })
                .on_change(cx.listener(|this, value: &SharedString, window, cx| {
                    this.switch_source_kind(value.as_ref() == "online", window, cx);
                })),
        )
    }

    pub(super) fn subtitle_attention_required(&self) -> bool {
        self.subtitle_loading
            || self.subtitle_error.is_some()
            || self.source_preview.as_ref().is_some_and(|source| {
                subtitle_needs_confirmation(source, self.task_options.source_mode)
            })
    }

    /// Visible field, local file target, and status belonging to this source.
    pub fn box_source_input(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let mut view = v_flex().w_full().min_w_0().gap_4();
        if self.online {
            let validation = self
                .source_preview
                .is_none()
                .then(|| self.source_validation.clone())
                .flatten();
            view = view.child(
                v_flex()
                    .gap_2()
                    // 字段 label 与选中 tab 重复（workbench#10）：placeholder 已说明用途，
                    // 去除后两个 tab 的内容区起点一致
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap(px(12.))
                            .items_center()
                            .child(
                                div().flex_1().min_w_0().child(
                                    text_input(&self.inputs[&Field::Source])
                                        .aria_label("视频链接")
                                        .w_full()
                                        .min_w_0()
                                        .text_size(TEXT_BODY)
                                        .prefix(
                                            icons::link()
                                                .size(rems(18. / 14.))
                                                .text_color(color(GRAY)),
                                        )
                                        .when(validation.is_some(), |input| {
                                            input.border_color(color(DANGER))
                                        }),
                                ),
                            )
                            .when(self.can_start_input(cx), |row| {
                                row.child(self.box_bottom_row(cx))
                            }),
                    )
                    .when_some(validation, |view, message| view.child(issue(message))),
            );
            // 平台识别反馈：输入可识别链接时给出「已识别」的 supporting 证据；
            // 空输入或暂不可识别时保持两个品牌位（M4 反馈闭环）
            let input = self.value(Field::Source, cx);
            let recognized = (!input.trim().is_empty()).then(|| {
                course2md::config::platform_from(&input, "")
            });
            match recognized.as_deref() {
                Some("bilibili") | Some("youtube") => {
                    let (name, icon) = if recognized.as_deref() == Some("bilibili") {
                        ("Bilibili", icons::bilibili().text_color(rgb(0x00a1d6)))
                    } else {
                        ("YouTube", icons::youtube().text_color(rgb(0xff0000)))
                    };
                    // 品牌图标 + 一句识别反馈，不再品牌名/句中重复（review2#7）
                    view = view.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(icon.size(px(16.)).flex_shrink_0())
                            .child(
                                accessible_text(
                                    "source-platform-recognized",
                                    format!("已识别 {name} 链接"),
                                )
                                .text_size(TEXT_AUX)
                                .text_color(color(MUTED)),
                            ),
                    );
                }
                _ => {
                    view = view.child(
                        h_flex()
                            .gap_4()
                            .flex_wrap()
                            .items_center()
                            .child(platform_mark(
                                "YouTube",
                                icons::youtube().text_color(rgb(0xff0033)),
                            ))
                            .child(platform_mark(
                                "Bilibili",
                                icons::bilibili().text_color(rgb(0x00a1d6)),
                            )),
                    );
                }
            }
        } else {
            let input = self.value(Field::Source, cx);
            if input.is_empty() {
                view = view.child(
                    v_flex()
                        .id("video-drop-zone")
                        .gap_4()
                        .p_6()
                        .items_center()
                        .rounded(RADIUS_CARD)
                        .bg(color(INSET))
                        .border_1()
                        .border_color(color(CONTROL))
                        .child(
                            icons::file_upload()
                                .size(rems(32. / 14.))
                                .text_color(color(ACCENT_STRONG)),
                        )
                        .child(
                            accessible_text("import-drop-instruction", "拖入本地视频")
                                .text_size(TEXT_BODY)
                                .font_weight(FontWeight::MEDIUM),
                        )
                        .child(
                            primary_pill("choose-video")
                                .icon(IconName::FolderOpen)
                                .label("选择视频")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.pick(false, window, cx)),
                                ),
                        )
                        .on_drop(
                            cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                                this.drop_source_files(paths.paths(), window, cx)
                            }),
                        ),
                );
            } else {
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
                        .p_4()
                        .rounded(RADIUS_CARD)
                        .bg(color(SURFACE))
                        .border_1()
                        .border_color(color(HAIRLINE))
                        .child(
                            semantic_label(
                                "selected-local-video-name",
                                filename,
                                icons::movie()
                                    .size(rems(20. / 14.))
                                    .text_color(color(ACCENT_STRONG)),
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
                                    outline_pill("choose-video")
                                        .icon(IconName::FolderOpen)
                                        .label("更换视频")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.pick(false, window, cx)
                                        })),
                                )
                                .when(self.can_start_input(cx), |row| {
                                    row.child(self.box_bottom_row(cx))
                                }),
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
                    view = view.child(issue(error.clone()));
                }
            }
        }
        if self.preview_cancel.is_some() {
            view = view.child(motion::enter(
                text_id("reading", &self.preview_generation.to_string()),
                h_flex()
                    .w_full()
                    .gap_3()
                    .p_3()
                    .items_center()
                    .rounded(RADIUS_SMALL)
                    .bg(color(INSET))
                    .child(motion::spinner("source-reading-spinner", cx))
                    .child(
                        accessible_text("import-reading-state", "正在读取视频信息与字幕…")
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(
                        quiet("cancel-source-read")
                            .icon(IconName::Close)
                            .label("取消")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.invalidate_source();
                                cx.notify();
                            })),
                    )));
        }
        if let Some(title) = &self.source_collection_title {
            view = view.child(motion::enter(
                text_id("source-candidates", title),
                v_flex()
                    .gap_3()
                    .child(
                        accessible_text("import-collection-title", title.clone())
                            .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(help("选择本次要整理的视频"))
                    .child(
                        v_flex()
                            .id("source-candidates")
                            // 视口按字号缩放（约 3 行 + 露出第 4 行一角）；内边距让
                            // 行的 hover/press 涂层不贴容器发丝边（review#3/#4）
                            .max_h(rems(20.))
                            .overflow_y_scroll()
                            .p_1()
                            .rounded(RADIUS_CARD)
                            .border_1()
                            .border_color(color(CARD_LINE))
                            .children(self.source_candidates.iter().enumerate().map(
                                |(index, candidate)| {
                                    let input = candidate.input.clone();
                                    let untitled = candidate.title.trim().is_empty()
                                        || candidate.title.trim() == candidate.input.trim();
                                    let title = if untitled {
                                        format!("视频 {}", index + 1)
                                    } else {
                                        candidate.title.clone()
                                    };
                                    let cover =
                                        self.source_candidate_covers.get(&candidate.input).cloned();
                                    quiet(("source-candidate", index))
                                        .w_full()
                                        .h_auto()
                                        .py_3()
                                        .rounded(RADIUS_SMALL)
                                        .justify_start()
                                        .tooltip(candidate.input.clone())
                                        .accessibility_label(format!("选择 {title}"))
                                        // 统一的 leading 预览槽：已缓存的首帧/封面，
                                        // 未就绪或无预览时回退到影片图标，保证每行文字起点对齐
                                        .child(
                                            div()
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
                                                    slot.child(
                                                        img(cover)
                                                            .w_full()
                                                            .h_full()
                                                            .object_fit(ObjectFit::Cover),
                                                    )
                                                })
                                                .when(
                                                    !self
                                                        .source_candidate_covers
                                                        .contains_key(&candidate.input),
                                                    |slot| {
                                                        slot.child(
                                                            icons::movie()
                                                                .size(px(18.))
                                                                .text_color(color(MUTED)),
                                                        )
                                                    },
                                                ),
                                        )
                                        .child(
                                            v_flex()
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
                                                        // 标题是行的扫描锚点：主墨色，
                                                        // 不被 quiet 按钮的灰色吞掉（review#1）
                                                        .text_color(color(INK))
                                                        .child(title),
                                                )
                                                .when(untitled, |view| {
                                                    view.child(
                                                        div()
                                                            .w_full()
                                                            .min_w_0()
                                                            .text_size(TEXT_AUX)
                                                            .text_color(color(MUTED))
                                                            .text_ellipsis()
                                                            .child(candidate.input.clone()),
                                                    )
                                                })
                                                .when_some(
                                                    candidate.duration.filter(|value| {
                                                        *value > 0. && value.is_finite() && !untitled
                                                    }),
                                                    |view, seconds| {
                                                        view.child(
                                                            div()
                                                                .w_full()
                                                                .min_w_0()
                                                                .text_size(TEXT_AUX)
                                                                .text_color(color(MUTED))
                                                                .text_ellipsis()
                                                                .child(course2md::render::fmt_ts(
                                                                    seconds,
                                                                )),
                                                        )
                                                    },
                                                ),
                                        )
                                        .child(icons::arrow_forward().size(px(18.)).flex_shrink_0())
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.select_source_input(input.clone(), window, cx)
                                        }))
                                },
                            )),
                    )));
        }
        if let Some(error) = &self.preview_error
            && !self.source_candidates.is_empty()
        {
            view = view.child(help(error.clone()));
        }
        if let Some(error) = &self.preview_error
            && self.source_candidates.is_empty()
        {
            let details = motion::disclosure(
                "source-error-disclosure",
                self.show_preview_details,
                v_flex().child(help(error.clone())),
                window,
                cx,
            );
            view = view.child(motion::enter(
                text_id("source-error", error),
                v_flex()
                    .gap_3()
                    .p_4()
                    .rounded(RADIUS_CARD)
                    .bg(color(DANGER_BG))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                Icon::new(IconName::CircleX)
                                    .size(px(20.))
                                    .text_color(color(DANGER))
                                    .flex_shrink_0(),
                            )
                            .child(
                                issue(if source::is_login_failure(error) {
                                    "视频需要登录后读取，请登录后重试转换".to_owned()
                                } else if error.contains("WARNING:") || error.contains("ERROR:") {
                                    "暂时无法读取这个视频，请检查链接或稍后重试".to_owned()
                                } else {
                                    error.lines().next().unwrap_or(error).to_owned()
                                })
                                .flex_1()
                                .min_w_0(),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                outline_pill("retry-source")
                                    .icon(icons::refresh())
                                    .label("重试转换")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_conversion(window, cx)
                                    })),
                            )
                            .when(
                                self.online
                                    && source::is_login_failure(error)
                                    && course2md::auth::is_bilibili_url(
                                        &self.value(Field::Source, cx),
                                    ),
                                |row| {
                                    row.child(
                                        outline_pill("source-login-repair")
                                            .icon(icons::bilibili())
                                            .label("登录 Bilibili")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_account_dialog(window, cx)
                                            })),
                                    )
                                },
                            )
                            .child(
                                quiet("source-error-details")
                                    .icon(if self.show_preview_details {
                                        IconName::ChevronUp
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .label(if self.show_preview_details {
                                        "收起详情"
                                    } else {
                                        "技术详情"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_preview_details = !this.show_preview_details;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(details)));
        }
        view
    }

    /// The video identity remains compact; paths and account tools are details.
    fn box_selected_video(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(source) = &self.source_preview else {
            return v_flex();
        };
        let can_close_input =
            self.source_editor_open && self.value(Field::Source, cx) == source.input;
        let mut selected = h_flex().gap_4().items_start();
        if let Some(cover) = &source.cover {
            selected = selected.child(
                img(cover.clone())
                    .w(rems(8.))
                    .h(rems(4.5))
                    .object_fit(ObjectFit::Cover)
                    .rounded(RADIUS_SMALL)
                    .flex_shrink_0(),
            );
        }
        selected = selected.child(
            v_flex()
                .min_w_0()
                .flex_1()
                .gap_2()
                .child(
                    accessible_text("import-selected-title", source.title.clone())
                        .text_size(TEXT_TITLE)
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal()
                        .text_ellipsis()
                        .line_clamp(2),
                )
                .when(!source.detail().is_empty(), |view| {
                    view.child(help(source.detail()))
                }),
        );
        let details = v_flex().gap_3().child(help(source.input.clone())).when(
            source.online && course2md::auth::is_bilibili_url(&source.input),
            |view| view.child(self.source_account_row(cx)),
        );
        v_flex().w_full().min_w_0().child(
            v_flex()
                .id("import-selected-source")
                .gap_3()
                .on_drop(
                    cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                        this.drop_source_files(paths.paths(), window, cx)
                    }),
                )
                .child(selected)
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .when(
                            !self.source_editor_open && self.can_start_input(cx),
                            |row| row.child(self.box_bottom_row(cx)),
                        )
                        .child(
                            quiet("change-source")
                                .icon(if can_close_input {
                                    icons::chevron_up()
                                } else {
                                    icons::movie()
                                })
                                .label(if can_close_input {
                                    "收起输入"
                                } else {
                                    "更换视频"
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.source_editor_open = !can_close_input;
                                    if !can_close_input {
                                        this.scrolls[Page::New as usize]
                                            .set_offset(point(px(0.), px(0.)));
                                        if this.online {
                                            this.inputs[&Field::Source]
                                                .update(cx, |input, cx| input.focus(window, cx));
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            quiet("source-details")
                                .icon(if self.show_preview_details {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .label(if self.show_preview_details {
                                    "收起详情"
                                } else {
                                    "来源详情"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_preview_details = !this.show_preview_details;
                                    cx.notify();
                                })),
                        )
                        .when(self.online, |row| {
                            row.child(
                                quiet("reread-source")
                                    .icon(icons::refresh())
                                    .label("重新读取")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.inspect_source(window, cx)
                                    })),
                            )
                        }),
                )
                .child(motion::disclosure(
                    "source-details-disclosure",
                    self.show_preview_details,
                    details,
                    window,
                    cx,
                )),
        )
    }

    fn text_source_view(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(source) = &self.source_preview else {
            return v_flex();
        };
        let speech = self.import_uses_speech();
        let failure = self.subtitle_error.as_deref().or_else(|| {
            if subtitle_needs_confirmation(source, self.task_options.source_mode)
                && let SubtitleEvidence::Failed { message } = &source.subtitles
            {
                Some(message.as_str())
            } else {
                None
            }
        });
        let needs_text_choice =
            subtitle_needs_confirmation(source, self.task_options.source_mode) || failure.is_some();
        let description = if self.subtitle_loading {
            "正在读取字幕…".to_owned()
        } else if !speech
            && (source.subtitle_request.is_some() || source.subtitle_read_error.is_some())
        {
            match &source.selected_subtitle {
                Some(_) => "字幕尚未确认，已读内容仍保留".into(),
                None => "所选字幕尚未确认".into(),
            }
        } else if speech {
            "识别视频声音".into()
        } else if let Some(subtitle) = &source.selected_subtitle {
            if self.task_options.source_mode == 0 {
                // 自动选择的解析结果就是它的说明，不是可更换的平行设置（review2#4）
                format!("将优先使用「{}」。", subtitle.label)
            } else {
                subtitle.label.clone()
            }
        } else {
            match &source.subtitles {
                SubtitleEvidence::Unchecked => "尚未检查可读取的字幕".into(),
                SubtitleEvidence::Found { .. } => "找到字幕，请确认要使用的文字".into(),
                SubtitleEvidence::NoneFound | SubtitleEvidence::Unsupported { .. } => {
                    "尚未选择文字来源".into()
                }
                SubtitleEvidence::Failed { .. } => "字幕尚未确认".into(),
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
                    .size(px(20.))
                    .text_color(color(GRAY))
                    .into_any_element()
                })
                .child(
                    accessible_text("import-text-source-state", description)
                        .font_weight(FontWeight::MEDIUM)
                        // 值与其「更换」动作收成一个对象行，不用 flex_1 把两者钉到两端（review2#3）
                        .min_w_0()
                        .whitespace_normal(),
                )
                .when(!needs_text_choice, |row| {
                    row.child(
                        quiet("change-text-source")
                            .icon(if self.show_options {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .label(if self.show_options {
                                "收起"
                            } else {
                                "更换"
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
                    .icon(IconName::Close)
                    .self_start()
                    .label("取消读取字幕")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pending_conversion = None;
                        if let Some(cancel) = this.subtitle_cancel.take() {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        this.subtitle_generation = this.subtitle_generation.wrapping_add(1);
                        this.subtitle_loading = false;
                        if let Some(source) = &mut this.source_preview {
                            source.subtitle_request = None;
                            source.subtitle_read_error = None;
                        }
                        this.save_current_draft(cx);
                        cx.notify();
                    })),
            );
        }
        if let Some(error) = failure {
            let failure_details = self.subtitle_issue("subtitle-read", error, window, cx);
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
                                Icon::new(IconName::TriangleAlert)
                                    .size(px(20.))
                                    .text_color(color(WARNING))
                                    .flex_shrink_0(),
                            )
                            .child(
                                accessible_text("subtitle-failed", "字幕未读取成功")
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(color(WARNING)),
                            ),
                    )
                    .child(failure_details)));
            if let Some(old) = &source.selected_subtitle {
                view = view.child(
                    outline_pill("use-previous-subtitle")
                        .self_start()
                        .icon(icons::subtitles())
                        .label("使用已读字幕")
                        .tooltip(old.label.clone())
                        .on_click(cx.listener(|this, _, _, cx| this.use_confirmed_subtitle(cx))),
                );
            }
        }
        let show_text_choices = self.show_options || needs_text_choice;
        if speech && !show_text_choices {
            match &source.subtitles {
                SubtitleEvidence::NoneFound => view = view.child(help("未找到可直接读取的字幕")),
                SubtitleEvidence::Unsupported { message } => {
                    view = view.child(help(message.clone()))
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
                .map(|track| (format!("track:{}", track.id), track.label()))
                .collect();
            if let Some(cached) = &source.selected_subtitle
                && !tracks.iter().any(|track| track.id == cached.track_id)
            {
                choices.push((format!("track:{}", cached.track_id), cached.label.clone()));
            }
            choices.push(("speech".into(), "识别视频声音".into()));
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
                    SingleChoiceGroup::new("import-text-source", "笔记的文字来源")
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
                    &format!("部分字幕尚未确认：{message}"),
                    window,
                    cx,
                ));
            }
            if let SubtitleEvidence::Unsupported { message } = &source.subtitles {
                options = options.child(help(message.clone()));
            }
            if matches!(source.subtitles, SubtitleEvidence::NoneFound) {
                options = options.child(help("未找到可直接读取的字幕，可以识别视频声音。"));
            }
            options = options.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        outline_pill("retry-subtitles")
                            .icon(icons::refresh())
                            .label("重新读取字幕")
                            .disabled(self.subtitle_loading)
                            .on_click(cx.listener(|this, _, _, cx| this.retry_subtitles(cx))),
                    )
                    .when(!source.online, |view| {
                        view.child(
                            outline_pill("attach-subtitles")
                                .icon(icons::file_upload())
                                .label("选择字幕文件")
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
                                .is_some_and(source::is_login_failure),
                        |view| {
                            view.child(
                                outline_pill("login-for-subtitles")
                                    .icon(icons::bilibili())
                                    .label("登录 Bilibili")
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
                window,
                cx,
            ));
        }
        view
    }

    fn import_text_mode_options(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let speech = if self.source_preview.is_some() {
            self.import_uses_speech()
        } else {
            self.task_options.source_mode != 1
        };
        let speech_options = self.import_speech_options(window, cx);
        let view = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                accessible_text("conversion-text-mode-label", "文字来源")
                    .font_weight(FontWeight::MEDIUM),
            )
            .child(
                SingleChoiceGroup::new("conversion-text-mode", "文字来源方式")
                    .options([("0", "自动选择"), ("1", "仅字幕"), ("2", "语音识别")])
                    .selected(self.task_options.source_mode.to_string())
                    .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                        let Ok(mode) = value.parse::<usize>() else {
                            return;
                        };
                        if mode == 2 {
                            this.use_speech(cx);
                        } else {
                            apply_text_source_mode(&mut this.task_options, mode);
                            this.save_current_draft(cx);
                            this.advance_conversion_when_ready(cx);
                            cx.notify();
                        }
                    })),
            )
            .child(motion::disclosure(
                "speech-options",
                speech,
                speech_options,
                window,
                cx,
            ));
        view
    }

    fn import_ai_options(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let mut view = v_flex().w_full().min_w_0().gap_3();
        let vision_options = conversion_ai_preference_row(
            ConversionAiOption::Vision,
            "文字及对应截图会发送到所选服务",
            coral_switch(
                Switch::new("import-vision")
                    .checked(conversion_ai_option_enabled(
                        &self.task_options,
                        ConversionAiOption::Vision,
                    ))
                    .on_click(cx.listener(|this, value, _, cx| {
                        apply_conversion_ai_option(
                            &mut this.task_options,
                            ConversionAiOption::Vision,
                            *value,
                        );
                        if this.save_current_draft(cx) {
                            this.advance_conversion_when_ready(cx);
                        }
                        cx.notify();
                    })),
            ),
        );
        let vision_disclosure = motion::disclosure(
            "ai-vision-options",
            self.task_options.llm,
            vision_options,
            window,
            cx,
        );
        let ai_overridden = {
            let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
            (
                self.task_options.llm,
                self.task_options.summarize,
                self.task_options.vision,
            ) != (defaults.llm, defaults.summarize, defaults.vision)
        };
        view = view.child(
            v_flex()
                .gap_3()
                .pt_2()
                .child(conversion_ai_preference_row(
                    ConversionAiOption::Proofread,
                    "修正识别错误和标点，保留原意",
                    coral_switch(
                        Switch::new("import-proofread")
                            .checked(conversion_ai_option_enabled(
                                &self.task_options,
                                ConversionAiOption::Proofread,
                            ))
                            .on_click(cx.listener(|this, value, _, cx| {
                                apply_conversion_ai_option(
                                    &mut this.task_options,
                                    ConversionAiOption::Proofread,
                                    *value,
                                );
                                if this.save_current_draft(cx) {
                                    this.advance_conversion_when_ready(cx);
                                }
                                cx.notify();
                            })),
                    ),
                ))
                .child(vision_disclosure)
                .child(conversion_ai_preference_row(
                    ConversionAiOption::Summary,
                    "提炼课程要点，正文继续保留",
                    coral_switch(
                        Switch::new("import-summary")
                            .checked(conversion_ai_option_enabled(
                                &self.task_options,
                                ConversionAiOption::Summary,
                            ))
                            .on_click(cx.listener(|this, value, _, cx| {
                                apply_conversion_ai_option(
                                    &mut this.task_options,
                                    ConversionAiOption::Summary,
                                    *value,
                                );
                                if this.save_current_draft(cx) {
                                    this.advance_conversion_when_ready(cx);
                                }
                                cx.notify();
                            })),
                    ),
                )),
        );
        let ai_enabled = self.task_options.llm || self.task_options.summarize;
        let mut ai_options = v_flex().gap_3();
        {
            match self.selected_task_service(ServicePurpose::Ai) {
                Some(service) => {
                    if self.task_options.llm {
                        ai_options = ai_options.child(help(format!(
                            "校对文字将发送到{}。",
                            service_destination(&service.config)
                        )));
                    }
                }
                None => {
                    ai_options = ai_options.child(
                        v_flex()
                            .gap_2()
                            .p_3()
                            .rounded(RADIUS_CARD)
                            .bg(color(WARNING_BG))
                            .child(
                                accessible_text("ai-service-missing", "AI 服务尚未设置。")
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(color(WARNING)),
                            )
                            .child(help("选择用于校对和摘要的服务")),
                    );
                }
            }
            ai_options = ai_options.child(self.task_service_picker(ServicePurpose::Ai, cx));
            if self.task_options.llm {
                let prompt = self.import_base_config().llm.prompt;
                if prompt
                    .as_ref()
                    .is_some_and(|prompt| !prompt.trim().is_empty())
                {
                    ai_options = ai_options.child(help("本次校对使用已保存的自定义规则。"));
                }
            }
        }
        view = view.child(motion::disclosure(
            "ai-service-options",
            ai_enabled,
            ai_options,
            window,
            cx,
        ));
        if ai_overridden {
            view = view.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(badge(BadgeKind::Neutral).child("仅用于这次笔记"))
                    .child(
                        quiet("reset-task-overrides")
                            .icon(icons::refresh())
                            .label("恢复默认")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.reset_task_ai_overrides(cx)),
                            ),
                    ),
            );
        }
        view
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
                .context("当前视频输入暂时不可用")?
                .reset_ai_overrides(&defaults);
            Ok(())
        }) {
            self.workspace_error = Some(format!("本次选项尚未恢复默认：{error:#}"));
            cx.notify();
            return;
        }
        self.task_options.llm = defaults.llm;
        self.task_options.summarize = defaults.summarize;
        self.task_options.vision = defaults.vision;
        self.advance_conversion_when_ready(cx);
        cx.notify();
    }

    fn import_speech_options(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let cloud = self.task_options.uses_cloud_provider();
        let mut view = v_flex()
            .gap_3()
            .p_4()
            .rounded(RADIUS_CARD)
            .bg(color(INSET))
            .child(
                SingleChoiceGroup::new("import-speech-location", "在哪里识别视频声音")
                    .options([("local", "本机识别"), ("cloud", "识别服务")])
                    .selected(if cloud { "cloud" } else { "local" })
                    .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                        let local = this
                            .workspace
                            .as_ref()
                            .and_then(|workspace| workspace.state.draft())
                            .and_then(|draft| draft.local_provider)
                            .filter(|provider| *provider < crate::CLOUD_PROVIDER_INDEX)
                            .unwrap_or(0);
                        apply_speech_location(
                            &mut this.task_options,
                            value.as_ref() == "cloud",
                            local,
                        );
                        if this.save_current_draft(cx) {
                            this.advance_conversion_when_ready(cx);
                        }
                        cx.notify();
                    })),
            );
        if cloud {
            return view.child(motion::enter(
                "cloud-speech-service",
                v_flex()
                    .gap_3()
                    .child(theme::supporting_info(
                        "import-cloud-note",
                        "音频发送到所选识别服务",
                    ))
                    .child(self.task_service_picker(ServicePurpose::Speech, cx))));
        }
        view = view.child(
            // 共享 ⓘ 辅助信息，不再用对象图标冒充信息图标（review4#2）
            theme::supporting_info("import-local-note", "音频在这台电脑上处理"),
        );
        let engine_options = v_flex()
            .gap_2()
            .child(
                SingleChoiceGroup::new("import-local-engine", "本机识别方式")
                    .options(
                        PROVIDERS[..crate::CLOUD_PROVIDER_INDEX]
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| {
                                *index == 0
                                    || *index == 3
                                    || *index == self.task_options.provider
                                    || self.environment.as_ref().is_some_and(|environment| {
                                        match index {
                                            1 => environment.apple,
                                            2 => environment.gpu.is_some(),
                                            4 => environment.npu,
                                            _ => false,
                                        }
                                    })
                            })
                            .map(|(index, (_, label))| {
                                (
                                    index.to_string(),
                                    if index == 0 {
                                        "应用推荐方式"
                                    } else {
                                        *label
                                    },
                                )
                            }),
                    )
                    .selected(self.task_options.provider.to_string())
                    .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                        if let Ok(index) = value.parse::<usize>() {
                            apply_local_engine(&mut this.task_options, index);
                            if this.save_current_draft(cx) {
                                this.advance_conversion_when_ready(cx);
                            }
                            cx.notify();
                        }
                    })),
            );
        // 「应用推荐方式」的解析结果以 supporting text 附在选择器旁；
        // 显式选择时选择器本身就是唯一事实来源，不再重复一行「当前方式」
        let engine_options = if self.task_options.provider == 0 {
            engine_options.child(help(format!("当前方式：{}", self.local_engine_name())))
        } else {
            engine_options
        };
        // 识别/引擎控件直接放在「高级选项」层（见文件顶部设计决定注释）
        view = view.child(engine_options);
        let (provider, model, root) = self.import_model_request();
        // 设置单元 bound 到有用内容宽度，不做整版两端拉扯（layout-and-type.md#a-form-grid-is-not-two-distant-edges）
        let readiness = self
            .model_readiness_panel_with(provider, Some(&model), &root, false, window, cx)
            .max_w(rems(40.));
        view.child(motion::enter("local-speech-readiness", readiness))
    }

    fn import_uses_speech(&self) -> bool {
        self.source_preview
            .as_ref()
            .is_some_and(|source| uses_speech(source, self.task_options.source_mode))
    }

    fn missing_import_service(&self) -> Option<ServicePurpose> {
        let separate_subtitle = self.task_options.source_mode != 2
            && self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.draft())
                .is_some_and(|draft| draft.subtitle.is_some());
        [
            (
                ServicePurpose::Speech,
                self.task_options.uses_cloud_provider() && self.import_uses_speech() && !separate_subtitle,
            ),
            (
                ServicePurpose::Ai,
                self.task_options.llm || self.task_options.summarize,
            ),
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
        use course2md::config::AsrProvider;
        let provider = self.actual_local_provider();
        let config = self.import_base_config();
        let model = config
            .defaults
            .asr_model
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| {
                if provider == AsrProvider::Npu {
                    course2md::npu::resolve_npu_model(None)
                } else {
                    course2md::config::DEFAULT_ASR_MODEL.into()
                }
            });
        let root = course2md::config::model_dir_from(config.defaults.model_dir.as_deref());
        (provider, model, root)
    }

    fn actual_local_provider(&self) -> course2md::config::AsrProvider {
        use course2md::config::AsrProvider;
        crate::asr_provider_from_index(self.task_options.provider)
            .filter(|provider| *provider != AsrProvider::Api)
            .unwrap_or_else(|| self.recommended_local_provider())
    }
    fn local_engine_name(&self) -> &'static str {
        crate::provider_label(Some(self.actual_local_provider()))
    }

    fn import_destination(&self, cx: &mut Context<Self>) -> Div {
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
        let mut view = box_section(Icon::new(IconName::Folder), "名称与保存").child(crate::focus_scroll::RevealFocus::new(
            ("import-title-focus", self.validation_attempt),
            self.input(Field::Title, "笔记名称", cx),
            self.scrolls[Page::New as usize].clone(),
        ));
        view = view.child(
            accessible_text("import-destination-label", "保存到").font_weight(FontWeight::MEDIUM),
        );
        if libraries.len() > 1 {
            let entity = cx.entity().downgrade();
            let label = selected
                .as_ref()
                .map(|library| library.name.clone())
                .unwrap_or_else(|| "选择保存位置".into());
            view = view.child(
                control("import-library")
                    .icon(IconName::FolderOpen)
                    .w_full()
                    .min_w_0()
                    .accessibility_label(format!("保存到：{label}"))
                    .tooltip(label.clone())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(label),
                    )
                    .child(Icon::new(IconName::ChevronDown).size_4().flex_shrink_0())
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
                                .on_click(move |_, _, cx| {
                                    let _ = entity.update(cx, |this, cx| {
                                        if !this.save_current_draft(cx) {
                                            return;
                                        }
                                        if let Some(workspace) = &mut this.workspace {
                                            match workspace.transaction(|state| {
                                                if let Some(draft) = state.draft_mut() {
                                                    draft.library_id = id.clone();
                                                    draft.folder = None;
                                                }
                                                Ok(())
                                            }) {
                                                Ok(()) => {
                                                    this.target_folder = None;
                                                    this.advance_conversion_when_ready(cx);
                                                }
                                                Err(error) => {
                                                    this.workspace_error =
                                                        Some(format!("保存位置尚未更新：{error:#}"))
                                                }
                                            }
                                        }
                                        cx.notify();
                                    });
                                }),
                            )
                        })
                    }),
            );
        } else if let Some(library) = &selected {
            view = view.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(IconName::FolderOpen)
                            .size(px(18.))
                            .text_color(color(GRAY)),
                    )
                    .child(help(library.name.clone()).flex_1()),
            );
        }
        view = view.child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(self.folder_picker(None, 0, cx)),
                )
                .child(
                    outline_pill("import-new-folder")
                        .icon(icons::create_new_folder())
                        .label("新建文件夹")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.begin_folder(None, window, cx)),
                        ),
                ),
        );
        view
    }

    fn import_exports(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let labels = ["Markdown 包", "网页文件", "JSON 数据"];
        let selected: Vec<_> = labels
            .iter()
            .enumerate()
            .filter(|(index, _)| self.task_options.formats[*index])
            .map(|(_, label)| *label)
            .collect();
        let mut view = box_section(icons::download(), "导出与视频").child(
            h_flex()
                .gap_3()
                .items_center()
                .flex_wrap()
                .child(
                    quiet("show-export-options")
                        .label(if self.show_export_options {
                            "收起导出选项"
                        } else {
                            "同时导出文件"
                        })
                        .icon(if self.show_export_options {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_export_options = !this.show_export_options;
                            cx.notify();
                        })),
                )
                .when(!selected.is_empty(), |row| {
                    row.child(help(selected.join(" · ")))
                }),
        );
        let mut options = v_flex()
            .gap_3()
            .p_4()
            .rounded(RADIUS_CARD)
            .bg(color(INSET))
            .child(help("可同时导出到其他应用使用，也可以生成后再导出。"));
        for (index, (label, description)) in [
            ("Markdown 包", "便于在笔记软件中编辑，包含文稿和图片"),
            ("网页文件", "单个文件，便于阅读与分享"),
            ("JSON 数据", "供其他程序读取和处理"),
        ]
        .into_iter()
        .enumerate()
        {
            // The icon, indicator and title share a first-line height. The
            // description lives in the title's column, independent of the
            // Checkbox component's own indicator/label spacing.
            let first_line_height = rems(32. / 14.);
            let icon = match index {
                0 => Icon::new(IconName::File),
                1 => icons::web(),
                _ => icons::code(),
            };
            options = options.child(
                h_flex()
                    .min_w_0()
                    .gap_2()
                    .items_start()
                    .child(
                        h_flex()
                            .debug_selector(move || format!("import-export-icon-{index}"))
                            .h(first_line_height)
                            .flex_shrink_0()
                            .child(icon.size(px(20.)).text_color(color(GRAY))),
                    )
                    .child(
                        Checkbox::new(("import-export", index))
                            .debug_selector(move || {
                                format!("import-export-checkbox-{index}")
                            })
                            .accessibility_label(label)
                            .checked(self.task_options.formats[index])
                            .h(first_line_height)
                            .items_center()
                            .flex_shrink_0()
                            .on_click(cx.listener(move |this, value, _, cx| {
                                this.task_options.formats[index] = *value;
                                this.save_current_draft(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        v_flex()
                            .id(("import-export-text", index))
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.task_options.formats[index] =
                                    !this.task_options.formats[index];
                                this.save_current_draft(cx);
                                cx.notify();
                            }))
                            .child(
                                h_flex()
                                    .debug_selector(move || {
                                        format!("import-export-title-{index}")
                                    })
                                    .min_w_0()
                                    .min_h(first_line_height)
                                    .child(accessible_text(("import-export-label", index), label)),
                            )
                            .child(help(description).debug_selector(move || {
                                format!("import-export-description-{index}")
                            })),
                    ),
            );
        }
        view = view.child(motion::disclosure(
            "import-export-options",
            self.show_export_options,
            options,
            window,
            cx,
        ));
        if self.online {
            view = view.child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .line_height(rems(1.5))
                    .child(
                        crate::settings_ui::preference(
                            Some(icons::movie()),
                            "保留视频供离线播放",
                            "生成后保留下载的视频，会占用额外空间",
                            coral_switch(
                                Switch::new("import-keep-video")
                                    .checked(self.task_options.keep_video)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.task_options.keep_video = *value;
                                        this.save_current_draft(cx);
                                        cx.notify();
                                    })),
                            ),
                        )
                        .flex_1()
                        .min_w_0(),
                    ),
            );
        }
        view
    }

    fn existing_source_note(&self) -> Option<Course> {
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
        self.courses
            .iter()
            .filter(|course| {
                course
                    .manifest
                    .as_ref()
                    .is_some_and(|manifest| manifest.source_id == source.identity)
            })
            .min_by_key(|course| !preferred.is_some_and(|root| course.dir.starts_with(root)))
            .cloned()
    }

    fn can_start_input(&self, cx: &App) -> bool {
        (self.online || !self.value(Field::Source, cx).is_empty())
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

    /// The single start command owns source preparation and conversion.
    fn box_bottom_row(&self, cx: &mut Context<Self>) -> Div {
        h_flex().items_center().child(
            primary_pill("start-conversion")
                .track_focus(&self.import_submit_focus)
                .icon(icons::arrow_forward())
                .label("开始转换")
                // 空输入即不可执行：disabled 外观 + 已有字段级错误提示双保险
                .disabled(
                    self.pending_conversion.is_some()
                        || self.value(Field::Source, cx).is_empty(),
                )
                .on_click(cx.listener(|this, _, window, cx| this.start_conversion(window, cx))),
        )
    }

    fn generation_options_toggle(&self, cx: &mut Context<Self>) -> Div {
        h_flex().child(
            // 两态共用 quiet inline 样式，仅 chevron 方向变化（M16）
            quiet("generation-options")
                .icon(if self.generation_options_open {
                    icons::chevron_up()
                } else {
                    icons::chevron_down()
                })
                .label(if self.generation_options_open {
                    "收起高级选项"
                } else {
                    "高级选项"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.generation_options_open = !this.generation_options_open;
                    cx.notify();
                })),
        )
    }

    pub(super) fn current_input_task(&self, cx: &App) -> Option<&workspace::TaskRecord> {
        let state = &self.workspace.as_ref()?.state;
        submitted_input_task(state.draft()?, &state.tasks, &self.value(Field::Source, cx))
    }

    /// Called by explicit workbench entry and initial restoration, never render.
    /// Terminal tasks remain in the task list and recent notes after their input
    /// has become a fresh form; an in-flight or edited input is never replaced.
    pub(super) fn prepare_workbench_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
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
            state.reset_input(self.online, defaults, destination);
            Ok(())
        }) {
            Ok(()) => {
                self.completed_source = None;
                self.invalidate_source();
                self.restore_draft(window, cx);
                self.source_editor_open = true;
                self.generation_options_open = false;
                true
            }
            Err(error) => {
                self.workspace_error = Some(format!(
                    "新的视频输入尚未建立：{error:#}。原输入和任务仍保留。"
                ));
                cx.notify();
                false
            }
        }
    }

    pub fn new_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let opening_result = self.reading && self.following_conversion.is_some();
        let linked_task = self.current_input_task(cx).cloned().filter(|task| {
            (opening_result
                && matches!(
                    task.state,
                    workspace::TaskState::Complete | workspace::TaskState::Partial
                ))
                || !task.state.finished()
                || (task.state == workspace::TaskState::Partial
                    && task.artifact.as_ref().is_some_and(|path| {
                        !crate::task_ui::task_component_failures(task, path).is_empty()
                    }))
        });
        if linked_task.is_none()
            && !self.task_options.uses_cloud_provider()
            && (self.import_uses_speech()
                || (self.generation_options_open && self.task_options.source_mode != 1))
        {
            let (provider, model, root) = self.import_model_request();
            self.ensure_model_diagnostic(provider, Some(&model), &root, cx);
        }
        let cancelled_notice = self
            .current_input_task(cx)
            .filter(|task| task.state == workspace::TaskState::Cancelled)
            .map(|task| {
                (
                    task.id.clone(),
                    crate::task_ui::task_attention_summary(task),
                )
            });
        let show_source_input = self.source_preview.is_none() || self.source_editor_open;
        let input = v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(self.source_kind_tabs(cx))
            .child(self.box_source_input(window, cx));
        let mut view = v_flex()
            .pt(px(24.))
            .gap_6()
            .w_full()
            .min_w_0()
            .child(theme::page_heading(
                "workbench-title",
                icons::dashboard().size(px(24.)).text_color(color(ACCENT_STRONG)),
                "把视频整理成笔记",
            ))
            .when(show_source_input, |view| view.child(input));
        if let Some((id, message)) = cancelled_notice {
            view = view.child(info_callout(
                SharedString::from(format!("cancelled-input-task-{id}")),
                message,
            ));
        }
        if let Some(task) = linked_task {
            if show_source_input {
                view = view.child(
                    quiet("hide-source-editor")
                        .self_start()
                        .icon(icons::chevron_up())
                        .label("收起输入")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.source_editor_open = false;
                            cx.notify();
                        })),
                );
            }
            let task_view = match task.state {
                workspace::TaskState::Complete | workspace::TaskState::Partial
                    if opening_result =>
                {
                    self.box_task_opening_result(&task, cx)
                }
                workspace::TaskState::Queued
                | workspace::TaskState::Running
                | workspace::TaskState::Pausing => self.box_task_running(&task, window, cx),
                _ => self.box_task_attention(&task, cx),
            };
            view = view.child(
                task_view
                    .p_6()
                    .rounded(RADIUS_CARD)
                    .bg(color(SURFACE))
                    .border_1()
                    .border_color(color(HAIRLINE)),
            );
            if !show_source_input {
                view = view.child(
                    quiet("convert-another-video")
                        .self_start()
                        .icon(icons::plus())
                        .label("转换其他视频")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.import_video_from_action(window, cx);
                        })),
                );
            }
        } else {
            let text_required = self.subtitle_attention_required();
            if self.source_preview.is_some() {
                let mut source = v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_4()
                    .child(self.box_selected_video(window, cx));
                if text_required {
                    source = source
                        .child(box_section(icons::subtitles(), "文字来源").child(self.text_source_view(window, cx)));
                }
                source = source.child(self.conversion_recovery(cx));
                view = view.child(source);
            } else if text_required {
                view = view.child(box_section(icons::subtitles(), "文字来源").child(self.text_source_view(window, cx)));
            }
            if self.preview_cancel.is_none() && self.source_candidates.is_empty() {
                let options_open = self.generation_options_open;
                let mut recognition_box = box_section(icons::microphone(), "识别方式");
                if self.source_preview.is_some() && !text_required {
                    recognition_box = recognition_box.child(self.text_source_view(window, cx));
                }
                recognition_box = recognition_box.child(self.import_text_mode_options(window, cx));
                let options = v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_6()
                    .child(recognition_box)
                    .child(box_section(icons::subtitles(), "笔记内容").child(self.import_ai_options(window, cx)))
                    .child(self.import_destination(cx))
                    .child(self.import_exports(window, cx));
                // idle 工作台不显示 conversion-defaults callout（见文件顶部设计决定注释）
                let mut options_header = v_flex().w_full().min_w_0().gap_2();
                options_header = options_header.child(self.generation_options_toggle(cx));
                view = view.child(options_header).child(disclosure(
                    "generation-options-body",
                    options_open,
                    options,
                    window,
                    cx,
                ));
            }
        }
        if let Some(recent) = self.recent_notes_section(cx) {
            view = view.child(recent);
        }
        if let Some(error) = &self.workspace_error {
            view = view.child(motion::enter(
                text_id("workspace-error", error),
                v_flex()
                    .p_4()
                    .rounded(RADIUS_CARD)
                    .bg(color(DANGER_BG))
                    .child(issue(error.clone()))));
        }
        view.into_any_element()
    }

    /// Only unresolved choices interrupt a submitted conversion.
    fn conversion_recovery(&mut self, cx: &mut Context<Self>) -> Div {
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
        let preferences_issue = self.ordinary_preferences_submit_issue();
        let preferences_blocked = preferences_issue.is_some();
        let submission_error = preferences_issue
            .as_ref()
            .map(|issue| issue.message.clone())
            .or_else(|| {
                if self.source_preview.is_some() && !reading && !self.subtitle_attention_required()
                {
                    self.submission_issue()
                } else {
                    None
                }
            });
        let checking_components = !preferences_blocked
            && self.environment.is_none()
            && submission_error.as_deref() == Some("正在检查生成笔记需要的组件，请稍候");
        let mut inset = v_flex().w_full().min_w_0().gap_3();
        let mut actions = h_flex().gap_2().flex_wrap();
        if self.environment.as_ref().is_some_and(|environment| {
            !environment.engine
                || !environment.ffmpeg
                || !environment.ffprobe
                || (self.online && !environment.ytdlp)
        }) {
            actions = actions.child(
                outline_pill("repair-conversion-components")
                    .icon(icons::settings())
                    .label("检查所需组件")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.settings_tab = 3;
                        this.scrolls[Page::Settings as usize].set_offset(point(px(0.), px(0.)));
                        this.open_settings(window, cx);
                    })),
            );
        }
        if let Some(issue) = preferences_issue {
            actions = actions.child(
                outline_pill("repair-generation-preferences")
                    .icon(icons::refresh())
                    .label(if issue.can_retry {
                        "重试保存生成选项"
                    } else {
                        "保留原文件并重置生成选项"
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
        if let Some(library) = self
            .workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .state
                    .draft()
                    .and_then(|draft| workspace.state.library(&draft.library_id))
            })
            .filter(|library| {
                self.cached_location_check(library)
                    .is_some_and(|check| check.needs_reassociation)
            })
        {
            let id = library.id.clone();
            actions = actions.child(
                outline_pill("reassociate-import-location")
                    .icon(IconName::FolderOpen)
                    .label("重新关联此保存位置")
                    .disabled(self.storage_ui.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.begin_library_reassociation(id.clone(), window, cx)
                    })),
            );
        }
        if let Some(task) = self.matching_current_task().cloned() {
            let id = task.id.clone();
            let location = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.library(&task.plan.library_id))
                .map(|library| library.name.clone())
                .unwrap_or_else(|| "原保存位置".into());
            inset = inset
                .child(conversion_fact(
                    format!("已有相同处理任务：《{}》（{location}）", task.plan.title),
                    true,
                ))
                .child(conversion_fact(
                    "查看现有任务；当前输入与选项尚未应用。",
                    false,
                ));
            actions = actions.child(
                primary_pill("show-matching-task")
                    .track_focus(&self.import_submit_focus)
                    .self_start()
                    .icon(icons::task())
                    .label("查看任务")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.save_current_draft(cx);
                        this.select_task(&id, cx);
                        this.navigate(Page::Task, cx);
                        cx.notify();
                    })),
            );
        } else if let Some(course) = self.existing_source_note() {
            let location = self
                .course_location(&course)
                .map(|library| library.name.clone())
                .unwrap_or_else(|| "课程库".into());
            inset = inset.child(info_callout(
                "existing-note-notice",
                format!("这个视频已有笔记，保存在「{location}」。生成新版会保留原笔记。"),
            ));
            actions = actions.child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        primary_pill("open-existing-note")
                            .icon(IconName::BookOpen)
                            .label("打开已有笔记")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save_current_draft(cx);
                                this.open_course(course.clone(), cx);
                            })),
                    )
                    .child(
                        outline_pill("generate-new-version")
                            .icon(icons::refresh())
                            .track_focus(&self.import_submit_focus)
                            .label("生成新版笔记")
                            .disabled(reading || self.workspace.is_none() || preferences_blocked)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.following_conversion =
                                    Some(ConversionFollow::Preparing(this.preview_generation));
                                this.enqueue_current(window, cx);
                            })),
                    ),
            );
            if busy {
                inset = inset.child(conversion_fact(
                    "新版笔记会加入队列，等待当前任务完成。",
                    false,
                ));
            }
        } else if let Some(purpose) = self.missing_import_service() {
            actions = actions.child(
                primary_pill("configure-conversion-service")
                    .icon(icons::settings())
                    .label(match purpose {
                        ServicePurpose::Speech => "设置语音服务",
                        ServicePurpose::Ai => "设置 AI 服务",
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
            && self.matching_current_task().is_none()
        {
            actions = actions.child(
                outline_pill("retry-conversion-validation")
                    .icon(icons::refresh())
                    .label("重试转换")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_conversion(window, cx);
                    })),
            );
        }
        if self.pending_conversion.is_some() && !reading {
            actions = actions.child(
                quiet("cancel-pending-conversion")
                    .icon(icons::close())
                    .label("取消转换")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pending_conversion = None;
                        this.following_conversion = None;
                        cx.notify();
                    })),
            );
        }
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
            .child(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConversionAiOption, ConversionFollow, ConversionGate, apply_conversion_ai_option,
        apply_local_engine, apply_speech_location, apply_text_source_mode,
        automatic_subtitle_fallback, completed_input_task, conversion_ai_option_enabled,
        conversion_ai_option_label, conversion_gate, submitted_input_task,
        subtitle_needs_confirmation, uses_speech,
    };
    use crate::{ConversionOptions, source, workspace};
    use course2md::subtitle::{CachedSubtitle, SubtitleEvidence, SubtitleReadError};

    #[test]
    fn one_start_continues_metadata_subtitles_and_environment_without_a_confirmation_stage() {
        // The same Start revision survives each independently completing prerequisite.
        let start = 7;
        for (reading, source, text_choice, environment) in [
            (true, false, false, false),
            (true, true, true, true),
            (false, true, false, false),
            (false, true, true, true),
        ] {
            assert_eq!(
                conversion_gate(
                    start,
                    7,
                    reading,
                    source,
                    text_choice,
                    environment,
                    true,
                    false
                ),
                ConversionGate::Wait,
            );
        }
        assert_eq!(
            conversion_gate(start, 7, false, true, false, true, true, false),
            ConversionGate::Submit
        );
        // Source replacement and duplicate content require a fresh intent/choice.
        assert_eq!(
            conversion_gate(start, 8, true, false, false, true, true, false),
            ConversionGate::Stop
        );
        assert_eq!(
            conversion_gate(start, 7, false, true, false, true, true, true),
            ConversionGate::Stop
        );
        assert_eq!(
            conversion_gate(start, 7, false, false, false, true, true, false),
            ConversionGate::Stop
        );
    }

    #[test]
    fn first_start_waits_for_library_scan_before_existing_note_decision() {
        // A restored source and the environment can be ready before the async
        // library scan replaces its initially empty (or stale) note list.
        let start = 7;
        for cached_note in [false, true] {
            assert_eq!(
                conversion_gate(start, 7, false, true, false, true, false, cached_note),
                ConversionGate::Wait,
            );
        }
        // The original Start is still pending when the scan reports a result.
        assert_eq!(
            conversion_gate(start, 7, false, true, false, true, true, true),
            ConversionGate::Stop,
        );
        assert_eq!(
            conversion_gate(start, 7, false, true, false, true, true, false),
            ConversionGate::Submit,
        );
        assert_eq!(
            conversion_gate(start, 8, false, true, false, true, false, false),
            ConversionGate::Stop,
        );
    }

    #[test]
    fn explicit_new_version_follows_new_task_instead_of_previous_result() {
        let (mut draft, mut tasks) = submitted_recovery_chain();
        tasks[0].state = workspace::TaskState::Complete;
        tasks[0].artifact = Some("versions/previous".into());
        tasks[0].handled_by = None;
        tasks[0].error = None;
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
        *components = vec!["exports".into()];
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
    fn completion_belongs_to_the_submitted_input_and_requires_a_readable_result() {
        let root = tempfile::tempdir().unwrap();
        let mut workspace = workspace::Workspace::open_at(
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
            options: Default::default(),
            subtitle: None,
            config: Default::default(),
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
                options: Default::default(),
                subtitle: None,
                config: Default::default(),
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
            stages: Default::default(),
            error: Some("原摘要请求结果未确认".into()),
            artifact: Some("versions/original".into()),
            outcomes: serde_json::Value::Null,
            unread: false,
            logs: Vec::new(),
            blocked: Vec::new(),
            resend: Vec::new(),
        };
        let mut recovery = original.clone();
        recovery.id = "recovery".into();
        recovery.parent = Some(original.id.clone());
        recovery.state = workspace::TaskState::Queued;
        recovery.intent = workspace::Intent::Run;
        recovery.error = None;
        recovery.artifact = None;
        recovery.work_dir = "work/recovery".into();
        recovery.plan.operation = course2md::execution::Operation::Reprocess {
            base_version_dir: "versions/original".into(),
            components: vec!["summary".into()],
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
        draft.options.vision = !draft.options.vision;
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
        draft = unchanged.clone();
        draft.folder = Some(42);
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
        draft = unchanged;
        assert!(super::retired_input_task(&draft, &tasks, "新视频").is_none());
        tasks[1].blocked.push(workspace::BlockedRequest {
            reason: "uncertain".into(),
            request_id: Some("pending-summary".into()),
            purpose: Some("summary".into()),
            description: "摘要".into(),
            message: "结果未知".into(),
        });
        assert!(super::retired_input_task(&draft, &tasks, &draft.input).is_none());
        tasks[1].blocked.clear();
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
            assert!(automatic_subtitle_fallback(&mut source, 0, None));
            assert!(uses_speech(&source, 0));
            assert!(!subtitle_needs_confirmation(&source, 0));
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
        assert!(!automatic_subtitle_fallback(&mut source, 1, None));
        assert_eq!(source, original);
        assert!(subtitle_needs_confirmation(&source, 1));

        source.subtitle_read_error = Some(SubtitleReadError::Cancelled);
        let cancelled = source.clone();
        assert!(!automatic_subtitle_fallback(&mut source, 0, None));
        assert_eq!(source, cancelled);
        assert!(!uses_speech(&source, 0));
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
        assert!(subtitle_needs_confirmation(&source, 0));
        source.selected_subtitle = Some(CachedSubtitle {
            source_identity: source.identity.clone(),
            track_id: track.id.clone(),
            label: "已读取字幕".into(),
            path: "lesson.srt".into(),
            events: Vec::new(),
        });
        assert!(!subtitle_needs_confirmation(&source, 0));

        source.subtitle_request = Some(track);
        assert!(subtitle_needs_confirmation(&source, 0));
        source.subtitle_request = None;
        source.subtitle_read_error = Some(SubtitleReadError::Failed {
            message: "无法读取新字幕，原正文仍保留".into(),
        });
        assert!(subtitle_needs_confirmation(&source, 0));

        // Cancelling the replacement or choosing the cached text restores the
        // confirmed source without opening unrelated generation preferences.
        source.subtitle_read_error = None;
        assert!(!subtitle_needs_confirmation(&source, 1));
        source.selected_subtitle = None;
        assert!(subtitle_needs_confirmation(&source, 1));
        assert!(!subtitle_needs_confirmation(&source, 2));
    }

    #[test]
    fn caption_intent_and_unconfirmed_reads_never_imply_speech_recognition() {
        let mut source = source::Source {
            subtitles: SubtitleEvidence::NoneFound,
            ..Default::default()
        };
        assert!(uses_speech(&source, 0));
        assert!(!uses_speech(&source, 1));
        source.subtitle_request = Some(course2md::subtitle::file_track("lesson.srt".into(), None));
        assert!(!uses_speech(&source, 0));
        assert!(!uses_speech(&source, 1));
        source.subtitle_request = None;
        source.subtitle_read_error = Some(SubtitleReadError::Failed {
            message: "字幕文件无法读取".into(),
        });
        assert!(!uses_speech(&source, 0));
        assert!(uses_speech(&source, 2));
        source.subtitle_read_error = None;
        source.subtitles = SubtitleEvidence::Unchecked;
        assert!(!uses_speech(&source, 0));
        source.subtitles = SubtitleEvidence::Failed {
            message: "请求失败".into(),
        };
        assert!(uses_speech(&source, 0));
        assert!(!uses_speech(&source, 1));
    }

    #[test]
    fn open_advanced_options_applies_source_and_engine_choices() {
        let mut options = ConversionOptions::default();
        let initial_mode = options.source_mode;
        let next_mode = if initial_mode == 0 { 1 } else { 0 };
        apply_text_source_mode(&mut options, next_mode);
        assert_eq!(options.source_mode, next_mode);
        apply_text_source_mode(&mut options, 2);
        assert_eq!(options.source_mode, 2);

        let local_provider = 3;
        apply_speech_location(&mut options, true, local_provider);
        assert_eq!(options.provider, crate::CLOUD_PROVIDER_INDEX);
        apply_speech_location(&mut options, false, local_provider);
        assert_eq!(options.provider, local_provider);
        apply_local_engine(&mut options, 0);
        assert_eq!(options.provider, 0);
        apply_local_engine(&mut options, 1);
        assert_eq!(options.provider, 1);
    }

    #[test]
    fn conversion_ai_rows_have_no_leading_icon_column_and_toggles_update_options() {
        assert_eq!(
            conversion_ai_option_label(ConversionAiOption::Proofread),
            "AI 校对"
        );
        assert_eq!(
            conversion_ai_option_label(ConversionAiOption::Vision),
            "发送截图辅助校对"
        );
        assert_eq!(
            conversion_ai_option_label(ConversionAiOption::Summary),
            "生成摘要"
        );

        let mut options = ConversionOptions::default();
        for option in [
            ConversionAiOption::Proofread,
            ConversionAiOption::Vision,
            ConversionAiOption::Summary,
        ] {
            let before = conversion_ai_option_enabled(&options, option);
            apply_conversion_ai_option(&mut options, option, !before);
            assert_eq!(
                conversion_ai_option_enabled(&options, option),
                !before,
                "{} must update conversion options",
                conversion_ai_option_label(option)
            );
            apply_conversion_ai_option(&mut options, option, before);
            assert_eq!(
                conversion_ai_option_enabled(&options, option),
                before,
                "{} must restore conversion options",
                conversion_ai_option_label(option)
            );
        }
    }
}
