//! The workbench: paste a link or drop a local video, preview its metadata and text
//! source, and submit an in-place conversion task.
use crate::Change as _;
use crate::{
    Desktop, Field, Page, notes::Course, source, symbols, task_ui::CardPlace, ui, workspace,
};
use course2md::config::TranscriptSource;
use course2md::subtitle::{SubtitleEvidence, SubtitleReadError, SubtitleTrack};
use desktop_accounts::youtube::GuideOrigin;
use desktop_tasks::import::*;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{color::ColorRole, icons::Icon, motion, prelude::*};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

/// The workbench's one column.
const WORKBENCH_WIDTH: Rems = rem(760.);
const HERO_GAP: Rems = rem(20.);
const HERO_HINT_GAP: Rems = rem(24.);
const SOURCE_KIND_WIDTH: Rems = rem(360.);

fn workbench_column() -> Div {
    div()
        .w_full()
        .min_w_0()
        .max_w(WORKBENCH_WIDTH)
        .mx_auto()
        .flex()
        .flex_col()
}

fn hero_top(quiet: bool, compact: bool) -> f32 {
    match (quiet, compact) {
        (true, true) => 96.,
        (true, false) => 160.,
        (false, true) => 24.,
        (false, false) => 48.,
    }
}

pub(crate) fn cover_placeholder(size: Rems) -> Icon {
    Icon::from(symbols::movie().filled(true))
        .size(size)
        .color(color(ColorRole::ON_SECONDARY_CONTAINER))
}

pub(crate) fn cover_width(window: &Window) -> Rems {
    rem(if is_compact(window) { 200. } else { 256. })
}

pub(crate) fn cover_fill(cover: PathBuf) -> Img {
    img(cover)
        .absolute()
        .inset_0()
        .size_full()
        .object_fit(ObjectFit::Cover)
}

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
        .child(cover_placeholder(rem(32.)))
        .children(cover.map(cover_fill))
}

pub(crate) fn video_title(id: String, title: impl Into<SharedString>) -> Stateful<Div> {
    let role = TypeRole::TitleLarge.scale();
    accessible_text(ElementId::Name(id.into()), title)
        .min_w_0()
        .text_size(role.font_size)
        .line_height(role.line_height)
        .font_weight(FontWeight(role.font_weight as f32))
        .whitespace_normal()
        .text_ellipsis()
        .line_clamp(2)
}

fn supporting(id: &'static str, text: impl Into<SharedString>) -> Stateful<Div> {
    accessible_text(id, text)
        .min_w_0()
        .text_size(TEXT_BODY)
        .text_color(color(ColorRole::ON_SURFACE_VARIANT))
        .whitespace_normal()
}

fn details_toggle(id: impl Into<ElementId>, open: bool) -> MaterialButton {
    material_button(id, ButtonVariant::Text)
        .tone(ButtonTone::Neutral)
        .icon(disclosure_chevron(open))
        .child(if open {
            course2md::tr!("收起技术详情")
        } else {
            course2md::tr!("技术详情")
        })
}

pub(crate) fn readable_first_line(message: &str) -> Option<&str> {
    let first = message.lines().next().unwrap_or(message);
    (!first.contains("WARNING:") && !first.contains("ERROR:")).then_some(first)
}

#[derive(Default)]
struct PreviewContext {
    info_line: Option<AnyElement>,
    actions: Vec<MaterialButton>,
    has_primary_override: bool,
    blocked: bool,
}

struct SubtitleRead {
    generation: u64,
    source_generation: u64,
    identity: String,
    input: Option<(String, u64)>,
}

impl Desktop {
    /// Explicit confirmation of a previewed source submits the conversion directly.
    pub fn start_conversion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page == Page::New {
            self.reader.change(cx, |reader, _| {
                if reader.opened.reading {
                    reader.opened.stop_reading();
                }
            });
        }
        let input = self.value(Field::Source, cx);
        if self.draft(cx).is_some_and(|draft| draft.input != input) {
            self.followed_task = None;
            self.invalidate_source();
        }
        if let Err(issue) = validate_workbench_input(self.online(cx), &input) {
            self.import.set_issue(issue);
            if self.online(cx) {
                self.inputs[&Field::Source].update(cx, |input, cx| input.focus(window, cx));
            }
            cx.notify();
            return;
        }
        if self.import.source_preview.is_none() {
            self.inspect_source(window, cx);
            return;
        }
        let source_mode = self.text_mode(cx);
        let subtitle_err = self
            .import
            .issue
            .as_ref()
            .filter(|i| i.kind == WorkbenchIssueKind::Subtitle)
            .map(|i| i.message.clone());
        if !self.import.reading()
            && let Some(source) = &mut self.import.source_preview
            && automatic_subtitle_fallback(source, source_mode, subtitle_err.as_deref())
        {
            self.import.issue = None;
            if !self.save_current_draft(cx) {
                return;
            }
        }
        self.enqueue_current(window, cx);
    }

    fn subtitle_read_is_current(&self, read: &SubtitleRead, cx: &App) -> bool {
        self.import.subtitle_generation == read.generation
            && self.import.preview_generation == read.source_generation
            && self
                .import
                .source_preview
                .as_ref()
                .is_some_and(|source| source.identity == read.identity)
            && read.input.as_ref().is_none_or(|(id, revision)| {
                self.workspace(cx)
                    .is_some_and(|workspace| workspace.state.matches_input(id, *revision))
            })
    }

    fn read_subtitles<T: Send + 'static>(
        &mut self,
        mark: impl FnOnce(&mut Self, &mut Context<Self>),
        work: impl FnOnce(&source::Source, Arc<AtomicBool>) -> T + Send + 'static,
        lost: fn() -> T,
        finish: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.import.source_preview.clone() else {
            return;
        };
        let generation = self.stop_subtitle_read();
        let cancel = Arc::new(AtomicBool::new(false));
        self.import.subtitle_cancel = Some(cancel.clone());
        self.import.subtitle_loading = true;
        self.import.clear_issue_of(WorkbenchIssueKind::Subtitle);
        mark(self, cx);
        if !self.save_current_draft(cx) {
            self.import.subtitle_loading = false;
            self.import.subtitle_cancel = None;
            return;
        }
        let read = SubtitleRead {
            generation,
            source_generation: self.import.preview_generation,
            identity: source.identity.clone(),
            input: self
                .workspace(cx)
                .and_then(|workspace| workspace.state.draft())
                .map(|draft| (draft.id.clone(), draft.revision)),
        };
        self.import.preview_workers += 1;
        let task = crate::spawn_blocking_io(move || work(&source, cancel));
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(lost);
            let _ = this.update(cx, |this, cx| {
                this.import.preview_workers = this.import.preview_workers.saturating_sub(1);
                if !this.subtitle_read_is_current(&read, cx) {
                    return;
                }
                this.import.subtitle_loading = false;
                this.import.subtitle_cancel = None;
                finish(this, result, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn confirm_subtitle(&mut self, track: SubtitleTrack, cx: &mut Context<Self>) {
        self.read_track(track, false, cx);
    }

    pub fn confirm_default_subtitle(&mut self, track: SubtitleTrack, cx: &mut Context<Self>) {
        self.read_track(track, true, cx);
    }

    fn read_track(&mut self, track: SubtitleTrack, fallback: bool, cx: &mut Context<Self>) {
        let requested = track.clone();
        self.read_subtitles(
            move |this, _| {
                if let Some(source) = &mut this.import.source_preview {
                    source.subtitle_request = Some(requested);
                    source.subtitle_read_error = None;
                }
            },
            move |source, cancel| {
                if fallback {
                    source::read_default_subtitle(source, &track, cancel)
                } else {
                    source::read_subtitle(source, &track, cancel)
                }
            },
            || {
                Err(SubtitleReadError::Failed {
                    message: course2md::tr!("字幕读取意外中断").into(),
                })
            },
            move |this, result, cx| {
                match result {
                    Ok(subtitle) => {
                        if let Some(source) = &mut this.import.source_preview {
                            source.selected_subtitle = Some(subtitle);
                            source.subtitle_request = None;
                            source.subtitle_read_error = None;
                        }
                    }
                    Err(SubtitleReadError::Cancelled) => return,
                    Err(error) => {
                        this.import
                            .set_issue(WorkbenchIssue::subtitle(error.to_string()));
                        if let Some(source) = &mut this.import.source_preview {
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
        let Some(source) = self.import.source_preview.clone() else {
            return;
        };
        let source_generation = self.import.preview_generation;
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(course2md::tr!("选择 SRT 或 VTT 字幕").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.import.preview_generation != source_generation
                    || !this
                        .import
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
                        this.import
                            .set_issue(WorkbenchIssue::subtitle(course2md::tr!(
                                "无法打开字幕选择器：{}",
                                format!("{error:#}")
                            )));
                    }
                    Err(error) => {
                        this.import
                            .set_issue(WorkbenchIssue::subtitle(error.to_string()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn attach_subtitle(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !course2md::subtitle::is_subtitle_file(&path) {
            self.import
                .set_issue(WorkbenchIssue::subtitle(course2md::tr!(
                    "请选择 SRT 或 VTT 字幕文件"
                )));
            return;
        }
        let track = course2md::subtitle::file_track(path, None);
        if let Some(source) = &mut self.import.source_preview {
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
        self.confirm_subtitle(track, cx);
    }

    fn retry_subtitles(&mut self, cx: &mut Context<Self>) {
        self.read_subtitles(
            |this, _| {
                if let Some(source) = &mut this.import.source_preview {
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
                let message = format!("{error:#}");
                self.import.set_issue(WorkbenchIssue::from_error(
                    WorkbenchIssueKind::Subtitle,
                    &error,
                ));
                if let Some(source) = &mut self.import.source_preview {
                    source.subtitle_read_error = Some(SubtitleReadError::Failed { message });
                }
                self.save_current_draft(cx);
                return;
            }
        };
        if let SubtitleEvidence::Found { tracks, .. } = &mut evidence {
            let original = self
                .import
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
            .import
            .source_preview
            .as_ref()
            .and_then(|source| source.selected_subtitle.as_ref())
            .map(|subtitle| subtitle.track_id.clone());
        let track = match &old_id {
            Some(id) => evidence.tracks().iter().find(|track| &track.id == id),
            None => evidence.tracks().first(),
        }
        .cloned();
        let mut subtitle_err_msg = None;
        if old_id.is_some() && track.is_none() {
            let msg = course2md::tr!("原字幕无法重新读取").to_owned();
            self.import.set_issue(WorkbenchIssue::subtitle(msg.clone()));
            subtitle_err_msg = Some(msg);
        }
        if let Some(source) = &mut self.import.source_preview {
            source.subtitles = evidence;
            source.subtitle_read_error =
                subtitle_err_msg.map(|message| SubtitleReadError::Failed { message });
        }
        self.save_current_draft(cx);
        match track {
            Some(track) if old_id.is_none() => self.confirm_default_subtitle(track, cx),
            Some(track) => self.confirm_subtitle(track, cx),
            None => {}
        }
    }

    fn stop_subtitle_read(&mut self) -> u64 {
        self.import.stop_subtitle_read()
    }

    fn abort_subtitle_read(&mut self) {
        self.stop_subtitle_read();
        if let Some(source) = &mut self.import.source_preview {
            source.subtitle_request = None;
            source.subtitle_read_error = None;
        }
    }

    fn settle_subtitle(&mut self, cx: &mut Context<Self>) {
        self.abort_subtitle_read();
        self.import.clear_issue_of(WorkbenchIssueKind::Subtitle);
        self.check_import_model(cx);
        cx.notify();
    }

    fn import_model_request(&self) -> Option<(course2md::config::AsrProvider, String, PathBuf)> {
        use course2md::config::AsrProvider;
        let mut config = self.preferences.defaults_config();
        let provider = match config.defaults.provider {
            Some(AsrProvider::Api) => return None,
            Some(provider) => provider,
            None => self.recommended_local_provider(),
        };
        desktop_tasks::plan::fill_default_asr_model(&mut config, provider);
        let model = config.defaults.asr_model.unwrap_or_default();
        let root = course2md::config::model_dir_from(config.defaults.model_dir.as_deref());
        Some((provider, model, root))
    }

    fn has_subtitle_issue(&self) -> bool {
        self.import
            .issue
            .as_ref()
            .is_some_and(|i| i.kind == WorkbenchIssueKind::Subtitle)
    }

    fn subtitle_choice_missing(&self, cx: &App) -> bool {
        let mode = self.text_mode(cx);
        !self.import.subtitle_loading
            && !self.has_subtitle_issue()
            && self.import.source_preview.as_ref().is_some_and(|source| {
                desktop_tasks::import::default_track_to_read(source, mode).is_some()
            })
    }

    pub(crate) fn confirm_default_subtitle_if_needed(&mut self, cx: &mut Context<Self>) {
        if self.text_mode(cx) == TranscriptSource::Asr || self.import.subtitle_loading {
            return;
        }
        let mode = self.text_mode(cx);
        let Some(track) = self
            .import
            .source_preview
            .as_ref()
            .and_then(|source| desktop_tasks::import::default_track_to_read(source, mode))
        else {
            return;
        };
        self.confirm_default_subtitle(track, cx);
    }

    pub(crate) fn check_import_model(&mut self, cx: &mut Context<Self>) {
        if self.import_uses_speech(cx)
            && let Some((provider, model, root)) = self.import_model_request()
        {
            self.ensure_model_diagnostic(provider, Some(&model), &root, cx);
        }
    }

    fn use_confirmed_subtitle(&mut self, cx: &mut Context<Self>) {
        if self
            .import
            .source_preview
            .as_ref()
            .is_some_and(|source| source.selected_subtitle.is_some())
        {
            self.settle_subtitle(cx);
        }
    }

    fn select_source_input(&mut self, input: String, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs[&Field::Source].update(cx, |state, cx| state.set_value(input, window, cx));
        self.inspect_source(window, cx);
    }

    fn clear_online_source_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs[&Field::Source].update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        self.followed_task = None;
        self.invalidate_source();
        self.save_current_draft(cx);
        let source_input = self.inputs[&Field::Source].clone();
        window.on_next_frame(move |window, cx| {
            source_input.update(cx, |state, cx| state.focus(window, cx));
        });
        cx.notify();
    }

    pub(crate) fn drop_source_files(
        &mut self,
        files: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.online(cx) {
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
                self.followed_task = None;
                self.invalidate_source();
                self.import.source_collection_title =
                    Some(course2md::tr!("选择要处理的视频").into());
                self.import.source_candidates = files
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

    pub(super) fn text_source_missing(&self, cx: &App) -> bool {
        !self.import.subtitle_loading
            && self.import.source_preview.as_ref().is_some_and(|source| {
                subtitles_required_but_missing(
                    source,
                    self.text_mode(cx),
                    self.has_subtitle_issue(),
                )
            })
    }

    pub(super) fn subtitle_attention_required(&self, cx: &App) -> bool {
        self.import.subtitle_loading
            || self.has_subtitle_issue()
            || self
                .import
                .source_preview
                .as_ref()
                .is_some_and(|source| subtitle_needs_confirmation(source, self.text_mode(cx)))
    }

    fn input_issue_message(&self) -> Option<String> {
        self.import
            .issue
            .as_ref()
            .filter(|issue| issue.kind == WorkbenchIssueKind::Input)
            .map(|issue| issue.message.clone())
    }

    /// The link and what can be done with it, or the local file in its place.
    fn source_input(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let local = (!self.online(cx)).then(|| self.local_source_input(cx));
        if let Some(local) = local {
            return v_flex()
                .w_full()
                .min_w_0()
                .gap(HERO_HINT_GAP)
                .child(
                    h_flex().w_full().justify_center().child(
                        div().w(SOURCE_KIND_WIDTH).max_w_full().child(
                            ConnectedButtonGroup::new("source-kind", course2md::tr!("视频来源"))
                                .segmented()
                                .tone(ButtonTone::Neutral)
                                .options([
                                    ("online", course2md::tr!("视频链接")),
                                    ("local", course2md::tr!("本地文件")),
                                ])
                                .icon("online", symbols::link())
                                .icon("local", symbols::movie())
                                .selected("local")
                                .on_change(cx.listener(
                                    |this, value: &SharedString, window, cx| {
                                        this.switch_source_kind(
                                            value.as_ref() == "online",
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                        ),
                    ),
                )
                .children(local);
        }
        let validation = self
            .import
            .source_preview
            .is_none()
            .then(|| self.input_issue_message())
            .flatten();
        let failed = validation.is_some();
        let start_shown = self.import.source_preview.is_some() && self.can_start_input(cx);
        let preview_label = course2md::tr!("预览视频");
        let preview_disabled = self.import.preview_cancel.is_some()
            || self.value(Field::Source, cx).is_empty()
            || self.followed_live_task(cx).is_some();
        let preview_click = cx.listener(|this, _, window, cx| this.inspect_source(window, cx));
        let preview = if !preview_label_fits(preview_label, window) {
            ui::tip_button(
                "preview-source",
                symbols::visibility(),
                preview_label,
                |button| {
                    button
                        .variant(IconButtonVariant::Tonal)
                        .button_size(ButtonSize::Medium)
                        .disabled(preview_disabled)
                        .on_click(preview_click)
                        .when(!start_shown, |b| b.track_focus(&self.import_submit_focus))
                },
            )
            .into_any_element()
        } else {
            material_button("preview-source", ButtonVariant::Tonal)
                .button_size(ButtonSize::Medium)
                .icon(symbols::visibility())
                .child(preview_label)
                .disabled(preview_disabled)
                .on_click(preview_click)
                .when(!start_shown, |b| b.track_focus(&self.import_submit_focus))
                .into_any_element()
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(HERO_HINT_GAP)
            .child(
                TextField::new("source-link", &self.inputs[&Field::Source])
                    .w_full()
                    .hero(course2md::tr!("视频链接"))
                    .leading_icon(Icon::from(symbols::link()))
                    .trailing_icon(preview)
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
                    .child(
                        Icon::from(symbols::file_upload())
                            .size(rem(20.))
                            .color(color(ColorRole::ON_SURFACE_VARIANT)),
                    )
                    .child(supporting(
                        "import-drop-hint",
                        course2md::tr!("或把本地视频拖进窗口"),
                    ))
                    .child(
                        material_button("choose-local-video", ButtonVariant::Text)
                            .child(course2md::tr!("选择文件"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.switch_source_kind(false, window, cx);
                                this.pick_video(window, cx);
                            })),
                    ),
            )
    }

    fn local_source_input(&self, cx: &mut Context<Self>) -> Option<Div> {
        let input = self.value(Field::Source, cx);
        let input_error = self.input_issue_message();
        let choose = |id: &'static str, variant, label: &'static str, cx: &mut Context<Self>| {
            material_button(id, variant)
                .when(variant == ButtonVariant::Outlined, |button| {
                    button.tone(ButtonTone::Accent)
                })
                .icon(symbols::folder_open())
                .child(label)
                .on_click(cx.listener(|this, _, window, cx| this.pick_video(window, cx)))
        };
        if input.is_empty() || self.followed_live_task(cx).is_some() {
            return Some(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(8.))
                    .child(
                        drop_target("import-drop-zone", course2md::tr!("拖入本地视频"), false)
                            .w_full()
                            .min_w_0()
                            .bg(color(ColorRole::SURFACE))
                            .rounded(RADIUS_HERO)
                            .flex()
                            .flex_col()
                            .gap(rem(16.))
                            .p(rem(32.))
                            .items_center()
                            .child(
                                Icon::from(symbols::file_upload())
                                    .size(rem(32.))
                                    .color(color(ColorRole::PRIMARY)),
                            )
                            .child(
                                accessible_text(
                                    "import-drop-instruction",
                                    course2md::tr!("拖入本地视频"),
                                )
                                .text_size(TEXT_BODY)
                                .font_weight(FontWeight::MEDIUM),
                            )
                            .child(choose(
                                "choose-video",
                                ButtonVariant::Filled,
                                course2md::tr!("选择视频"),
                                cx,
                            )),
                    )
                    .when_some(input_error, |view, error| {
                        view.child(
                            error_text("import-local-input-error", error).whitespace_normal(),
                        )
                    }),
            );
        }
        if self.import.source_preview.is_some() {
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
                        .rounded(RADIUS_HERO)
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
                                    .disabled(self.import.preview_cancel.is_some())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.inspect_source(window, cx)
                                    })),
                            )
                        }),
                )
                .when_some(input_error, |view, error| {
                    view.child(error_text("import-preview-error", error).whitespace_normal())
                }),
        )
    }

    fn source_reading(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let label = course2md::tr!("取消读取视频");
        let bar = |width: f32| {
            div()
                .w(relative(width))
                .h(rem(16.))
                .rounded_full()
                .bg(color(ColorRole::SURFACE_CONTAINER_HIGHEST))
        };
        motion::state_enter(
            "source-reading-enter",
            tile([true; 4])
                .rounded(RADIUS_HERO)
                .flex()
                .flex_col()
                .p(rem(16.))
                .gap(rem(16.))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .gap(rem(20.))
                        .child(
                            div()
                                .w(cover_width(window))
                                .max_w_full()
                                .flex_shrink_0()
                                .aspect_ratio(16. / 9.)
                                .rounded(RADIUS_LARGE)
                                .bg(color(ColorRole::SECONDARY_CONTAINER)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(rem(240.))
                                .gap(rem(12.))
                                .child(
                                    h_flex()
                                        .w_full()
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
                                        .child(ui::tip_button(
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
                                .child(bar(0.7))
                                .child(bar(0.45)),
                        ),
                )
                .child(LinearProgress::new(
                    "source-reading-progress",
                    course2md::tr!("正在读取视频信息…"),
                )),
        )
    }

    fn candidate_has_note(&self, candidate: &source::SourceCandidate, cx: &App) -> bool {
        candidate
            .identity
            .as_deref()
            .is_some_and(|identity| self.notes_for_source(identity, cx).next().is_some())
    }

    fn selectable_candidates<'a>(
        &'a self,
        cx: &'a App,
    ) -> impl Iterator<Item = &'a source::SourceCandidate> {
        let batch = self.online(cx);
        self.import
            .source_candidates
            .iter()
            .filter(move |candidate| batch && candidate.identity.is_some())
    }

    fn candidate_list(&self, title: &str, cx: &mut Context<Self>) -> Stateful<Div> {
        let batch = self.online(cx);
        let selectable = self.selectable_candidates(cx).count();
        let selectable_selected = self
            .selectable_candidates(cx)
            .filter(|candidate| self.import.selected_candidates.contains(&candidate.input))
            .count();
        let noted_selected = self
            .import
            .source_candidates
            .iter()
            .filter(|candidate| {
                self.candidate_has_note(candidate, cx)
                    && self.import.selected_candidates.contains(&candidate.input)
            })
            .count();
        let any_batchable = self
            .import
            .source_candidates
            .iter()
            .any(|candidate| candidate.identity.is_some());
        let selected_count = self.import.selected_candidates.len();
        let all_selected = selectable > 0 && selectable_selected == selectable;
        let rows = self
            .import
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
                                        this.import.selected_candidates.clear();
                                    } else {
                                        let inputs: Vec<String> = this
                                            .selectable_candidates(cx)
                                            .map(|candidate| candidate.input.clone())
                                            .collect();
                                        this.import.selected_candidates.extend(inputs);
                                    }
                                    cx.notify();
                                })),
                        )
                        .when(noted_selected > 0, |row| {
                            row.child(
                                accessible_text(
                                    "candidates-replace-notes",
                                    course2md::tr!("{} 个已有笔记将被覆盖", noted_selected),
                                )
                                .flex_1()
                                .min_w_0()
                                .text_size(TEXT_AUX)
                                .text_color(color(ColorRole::WARNING))
                                .whitespace_normal(),
                            )
                        })
                        .child(
                            material_button("start-batch-conversion", ButtonVariant::Filled)
                                .ml_auto()
                                .icon(symbols::arrow_forward())
                                .child(if selected_count == 1 {
                                    course2md::tr!("整理选中的 1 个视频").to_owned()
                                } else if selected_count > 0 {
                                    course2md::tr!("整理选中的 {} 个视频", selected_count)
                                } else {
                                    course2md::tr!("整理选中的视频").to_owned()
                                })
                                .disabled(selected_count == 0 || self.environment.is_none())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if noted_selected > 0 {
                                        ui::Confirm::new(
                                            course2md::tr!("覆盖已有笔记"),
                                            course2md::tr!("保留现有笔记"),
                                            course2md::tr!("覆盖并生成"),
                                        )
                                        .text(course2md::tr!(
                                            "选中的视频中有 {} 个已有笔记，继续转换将覆盖原有笔记。",
                                            noted_selected
                                        ))
                                        .ask(
                                            cx.listener(|this, _, _, cx| {
                                                this.enqueue_selected_candidates(cx)
                                            }),
                                            window,
                                            cx,
                                        );
                                    } else {
                                        this.enqueue_selected_candidates(cx);
                                    }
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
        let cover = self
            .import
            .source_candidate_covers
            .get(&candidate.input)
            .cloned();
        let thumbnail = h_flex()
            .relative()
            .size_full()
            .justify_center()
            .child(cover_placeholder(rem(20.)))
            .children(cover.map(cover_fill))
            .into_any_element();
        let row = ListItem::new(("source-candidate", index), title)
            .leading(ListLeading::Video(Some(thumbnail)))
            .supporting(duration.unwrap_or_else(|| candidate.input.clone()));
        if batch && candidate.identity.is_some() {
            let selected = self.import.selected_candidates.contains(&input);
            row.when(self.candidate_has_note(candidate, cx), |row| {
                row.trailing_text(course2md::tr!("已有笔记"))
            })
            .trailing(ListTrailing::Checkbox(selected))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.import.selected_candidates.remove(&input) {
                    this.import.selected_candidates.insert(input.clone());
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

    /// Resolves the single active `WorkbenchIssue` to display below the source/preview card.
    fn active_workbench_issue(&mut self, cx: &mut App) -> Option<WorkbenchIssue> {
        if let Some(issue) = &self.import.issue {
            if issue.kind == WorkbenchIssueKind::Input {
                return None;
            }
            return Some(issue.clone());
        }
        let source = self.import.source_preview.as_ref()?;
        if self.import.reading() || self.import.subtitle_loading {
            return None;
        }
        if let SubtitleEvidence::Found {
            warning: Some(message),
            ..
        } = &source.subtitles
        {
            return Some(WorkbenchIssue::subtitle(course2md::tr!(
                "部分字幕无法读取：{}",
                message
            )));
        }
        if self.text_source_missing(cx) {
            let unchecked = matches!(source.subtitles, SubtitleEvidence::Unchecked);
            let summary = if unchecked {
                course2md::tr!("还没读取这个视频的字幕，而文字来源设置为仅字幕")
            } else {
                course2md::tr!("这个视频没有字幕，而文字来源设置为仅字幕")
            };
            return Some(WorkbenchIssue {
                kind: WorkbenchIssueKind::Subtitle,
                message: summary.into(),
                needs_settings: true,
            });
        }
        if let Some(pref_issue) = self.ordinary_preferences_submit_issue() {
            return Some(WorkbenchIssue {
                kind: WorkbenchIssueKind::Plan,
                message: pref_issue.message,
                needs_settings: true,
            });
        }
        if !self.subtitle_attention_required(cx)
            && let Err((issue, false)) = self.cached_preview_plan(cx)
        {
            return Some(issue);
        }
        None
    }

    /// Single unified `notice_banner` rendered below the source/preview card on Home.
    fn workbench_issue_banner(
        &mut self,
        issue: Option<WorkbenchIssue>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let issue = issue?;
        if issue.kind == WorkbenchIssueKind::Preview && !self.import.source_candidates.is_empty() {
            return Some(supporting("import-source-error", issue.message).into_any_element());
        }
        let raw = issue.message.clone();
        let needs_login = source::legacy_login_failure_from_text(&raw);
        let components_missing = self.environment.as_ref().is_some_and(|environment| {
            !environment.engine
                || !environment.ffmpeg
                || !environment.ffprobe
                || (self.online(cx) && !environment.ytdlp)
        });
        let summary = match issue.kind {
            WorkbenchIssueKind::Preview if components_missing => {
                course2md::tr!("缺少读取视频所需的组件").to_owned()
            }
            WorkbenchIssueKind::Preview if needs_login => {
                course2md::tr!("视频需要登录后读取，请登录后重试预览").to_owned()
            }
            WorkbenchIssueKind::Preview => readable_first_line(&raw)
                .unwrap_or(course2md::tr!("暂时无法读取这个视频，请检查链接或稍后重试"))
                .to_owned(),
            WorkbenchIssueKind::Subtitle => {
                let first = readable_first_line(&raw)
                    .unwrap_or(course2md::tr!("无法读取字幕，可重试或改用语音识别"));
                first.to_owned()
            }
            _ => readable_first_line(&raw).unwrap_or(raw.as_str()).to_owned(),
        };

        let show_technical = summary != raw.trim();
        let open = if issue.kind == WorkbenchIssueKind::Subtitle {
            self.import.subtitle_issue_open
        } else {
            self.import.show_preview_details
        };

        let mut actions: Vec<MaterialButton> = Vec::new();
        let source_preview = self.import.source_preview.clone();
        let input_url = self.value(Field::Source, cx);

        if components_missing {
            actions.push(
                material_button("workbench-components-repair", ButtonVariant::Tonal)
                    .icon(symbols::settings())
                    .child(course2md::tr!("检查所需组件"))
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

        if self.online(cx) && needs_login && course2md::auth::is_bilibili_url(&input_url) {
            actions.push(
                material_button("workbench-login-bilibili", ButtonVariant::Tonal)
                    .icon(symbols::bilibili())
                    .child(course2md::tr!("登录 Bilibili"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_account_dialog(window, cx)),
                    ),
            );
        } else if self.online(cx) && needs_login && course2md::auth::is_youtube_url(&input_url) {
            actions.push(
                material_button("workbench-login-youtube", ButtonVariant::Tonal)
                    .icon(symbols::smart_display())
                    .child(course2md::tr!("连接 YouTube"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_youtube_dialog(window, cx)),
                    ),
            );
        }

        if issue.kind == WorkbenchIssueKind::Subtitle
            && let Some(src) = &source_preview
        {
            actions.push(
                material_button("workbench-retry-subtitles", ButtonVariant::Tonal)
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重新读取字幕"))
                    .disabled(self.import.subtitle_loading)
                    .on_click(cx.listener(|this, _, _, cx| this.retry_subtitles(cx))),
            );
            if src.selected_subtitle.is_some() {
                actions.push(
                    material_button("workbench-use-previous-subtitle", ButtonVariant::Outlined)
                        .tone(ButtonTone::Accent)
                        .icon(symbols::subtitles())
                        .child(course2md::tr!("继续使用之前的字幕"))
                        .on_click(cx.listener(|this, _, _, cx| this.use_confirmed_subtitle(cx))),
                );
            }
            if !src.online && self.text_mode(cx) == TranscriptSource::Subtitle {
                actions.push(
                    material_button("workbench-attach-subtitle", ButtonVariant::Outlined)
                        .tone(ButtonTone::Accent)
                        .icon(symbols::file_upload())
                        .child(course2md::tr!("选择字幕文件"))
                        .disabled(self.import.subtitle_loading)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.choose_attached_subtitle(window, cx)
                        })),
                );
            }
        }

        if let Some(pref_issue) = self.ordinary_preferences_submit_issue() {
            actions.push(
                material_button("workbench-repair-preferences", ButtonVariant::Tonal)
                    .icon(if pref_issue.can_retry {
                        symbols::refresh()
                    } else {
                        symbols::restart_alt()
                    })
                    .child(if pref_issue.can_retry {
                        course2md::tr!("重试保存生成笔记设置")
                    } else {
                        course2md::tr!("重置生成笔记设置")
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if !this.save_current_draft(cx) {
                            return;
                        }
                        if pref_issue.can_retry {
                            this.retry_ordinary_preferences(pref_issue.group, cx);
                        } else {
                            this.restore_ordinary_preferences(pref_issue.group, window, cx);
                        }
                    })),
            );
        }

        if let Some(library) = self
            .workspace(cx)
            .and_then(|workspace| workspace.state.library(&workspace.state.default_library))
            .filter(|library| {
                self.cached_location_check(library)
                    .is_some_and(|check| check.needs_reassociation)
            })
        {
            let id = library.id.clone();
            actions.push(
                material_button("workbench-reassociate-location", ButtonVariant::Tonal)
                    .icon(symbols::database())
                    .child(course2md::tr!("重新关联此保存位置"))
                    .disabled(self.storage_ui.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.begin_library_reassociation(id.clone(), window, cx)
                    })),
            );
        }

        if issue.needs_settings {
            let reason_for_tab = raw.clone();
            actions.push(
                material_button("workbench-open-settings", ButtonVariant::Tonal)
                    .icon(symbols::settings())
                    .child(course2md::tr!("打开设置"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.save_current_draft(cx);
                        this.open_settings_for_message(&reason_for_tab, window, cx);
                        if let Some((_, focus)) = &mut this.settings_origin {
                            *focus = Some(this.import_submit_focus.clone());
                        }
                    })),
            );
        }

        let kind = if issue.kind == WorkbenchIssueKind::Preview
            && !components_missing
            && !issue.needs_settings
        {
            NoticeKind::Danger
        } else if !actions.is_empty() || issue.kind == WorkbenchIssueKind::Subtitle {
            NoticeKind::Warning
        } else {
            NoticeKind::Danger
        };

        let is_subtitle = issue.kind == WorkbenchIssueKind::Subtitle;
        Some(motion::state_enter(
            "workbench-issue-banner-enter",
            notice_banner("workbench-issue-banner", kind, summary)
                .rounded(RADIUS_LARGE)
                .when(!actions.is_empty(), |banner| {
                    banner.child(h_flex().gap(rem(8.)).flex_wrap().children(actions))
                })
                .when(show_technical, |banner| {
                    banner.child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .child(h_flex().w_full().child(
                                details_toggle("workbench-issue-details", open).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        if is_subtitle {
                                            this.import.subtitle_issue_open = !open;
                                        } else {
                                            this.import.show_preview_details = !open;
                                        }
                                        cx.notify();
                                    }),
                                ),
                            ))
                            .child(motion::disclosure(
                                "workbench-issue-disclosure",
                                open,
                                v_flex().child(supporting_info("workbench-issue-raw", raw)),
                            )),
                    )
                }),
        ))
    }

    fn toggle_preview_details(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.import.show_preview_details = !self.import.show_preview_details;
        cx.notify();
    }

    fn selected_video(
        &mut self,
        has_blocking_issue: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let source = self.import.source_preview.clone()?;
        let context = self.preview_context(has_blocking_issue, cx);
        let duration = (source.duration.is_finite() && source.duration > 0.)
            .then(|| course2md::render::fmt_ts(source.duration));
        let details_open = self.import.show_preview_details;
        let metadata = source_metadata(&source);
        let cover = video_cover(source.cover.clone(), cover_width(window))
            .self_start()
            .when_some(duration, |cover, duration| {
                cover.child(
                    media_label("import-source-duration", duration)
                        .absolute()
                        .right(rem(8.))
                        .bottom(rem(8.)),
                )
            });
        let details = v_flex()
            .gap(rem(12.))
            .pt(rem(16.))
            .child(supporting_info("import-source-input", source.input.clone()))
            .when(
                source.online && course2md::auth::is_bilibili_url(&source.input),
                |view| {
                    view.child(
                        self.accounts
                            .update(cx, |accounts, cx| accounts.source_account_row(cx)),
                    )
                },
            );
        let actions = h_flex()
            .w_full()
            .min_w_0()
            .gap(rem(8.))
            .items_center()
            .flex_wrap()
            .when(self.online(cx), |row| {
                row.child(ui::tip_button(
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
            .child(
                h_flex()
                    .ml_auto()
                    .gap(rem(8.))
                    .items_center()
                    .flex_wrap()
                    .justify_end()
                    .child(
                        material_button("replace-source", ButtonVariant::Outlined)
                            .tone(ButtonTone::Accent)
                            .child(course2md::tr!("更换视频"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.online(cx) {
                                    this.clear_online_source_preview(window, cx);
                                } else {
                                    this.pick_video(window, cx);
                                }
                            })),
                    )
                    .children(context.actions)
                    .when(
                        !context.has_primary_override && self.can_start_input(cx),
                        |row| {
                            row.child(
                                material_button("start-conversion", ButtonVariant::Filled)
                                    .button_size(ButtonSize::Medium)
                                    .icon(symbols::play_arrow())
                                    .child(course2md::tr!("开始转换"))
                                    .track_focus(&self.import_submit_focus)
                                    .disabled(
                                        context.blocked
                                            || self.import.preview_cancel.is_some()
                                            || self.import.subtitle_loading
                                            || self.subtitle_choice_missing(cx)
                                            || self.text_source_missing(cx),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_conversion(window, cx)
                                    })),
                            )
                        },
                    ),
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
                                self.draft(cx)
                                    .and_then(desktop_tasks::plan::note_title)
                                    .unwrap_or_else(|| source.title.clone()),
                            ))
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap(rem(4.))
                                    .items_center()
                                    .when(!metadata.is_empty(), |row| {
                                        row.child(supporting("import-source-meta", metadata))
                                    })
                                    .child(ui::tip_button(
                                        "source-details",
                                        if details_open {
                                            symbols::keyboard_arrow_up()
                                        } else {
                                            symbols::keyboard_arrow_down()
                                        },
                                        course2md::tr!("来源详情"),
                                        |button| {
                                            button
                                                .button_size(ButtonSize::ExtraSmall)
                                                .on_click(cx.listener(Self::toggle_preview_details))
                                        },
                                    )),
                            ),
                    )
                    .child(ui::tip_button(
                        "clear-source-preview",
                        symbols::close(),
                        course2md::tr!("移除预览"),
                        |button| {
                            button.on_click(cx.listener(|this, _, window, cx| {
                                if this.online(cx) {
                                    this.clear_online_source_preview(window, cx);
                                } else {
                                    this.inputs[&Field::Source]
                                        .update(cx, |state, cx| state.set_value("", window, cx));
                                    this.followed_task = None;
                                    this.invalidate_source();
                                    this.save_current_draft(cx);
                                    cx.notify();
                                }
                            }))
                        },
                    )),
            )
            .child(self.text_source_control(&source, window, cx))
            .children(context.info_line);
        Some(motion::state_enter(
            "import-selected-source-enter",
            tile([true; 4])
                .rounded(RADIUS_HERO)
                .id("import-selected-source")
                .flex()
                .flex_col()
                .p(rem(16.))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .items_start()
                        .gap(rem(20.))
                        .child(cover)
                        .child(info),
                )
                .child(motion::disclosure(
                    "source-details-disclosure",
                    details_open,
                    details,
                ))
                .child(div().w_full().pt(rem(16.)).child(actions)),
        ))
    }

    fn text_source_control(
        &self,
        source: &source::Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let speech = self.import_uses_speech(cx);
        let mode = self.text_mode(cx);
        let has_sub_issue = self.has_subtitle_issue();
        let source_failed = matches!(source.subtitles, SubtitleEvidence::Failed { .. })
            && subtitle_needs_confirmation(source, mode);
        let failure = has_sub_issue || source_failed;
        let pending = source.subtitle_request.is_some() || source.subtitle_read_error.is_some();
        let value: SharedString = if self.import.subtitle_loading {
            course2md::tr!("正在读取字幕…").into()
        } else if !speech && failure {
            course2md::tr!("尚未选择文字来源").into()
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
        let trigger = FilterChip::new("text-source", value.clone())
            .icon(if speech {
                symbols::mic()
            } else {
                symbols::subtitles()
            })
            .trailing_icon(symbols::arrow_drop_down())
            .on_toggle(|_, _, _| {});
        let chip = if mode == TranscriptSource::Asr {
            semantic_label(
                "text-source",
                course2md::tr!("语音识别（文字来源设置）"),
                Icon::from(symbols::mic()).color(color(ColorRole::ON_SURFACE_VARIANT)),
            )
            .into_any_element()
        } else {
            let menu = self.text_source_menu(source, &value, window, cx);
            ui::menu("text-source-menu", trigger, menu, window, cx).into_any_element()
        };
        let notice = match &source.subtitles {
            _ if self.import.subtitle_loading => None,
            SubtitleEvidence::NoneFound if speech => Some(course2md::tr!("未找到字幕").to_owned()),
            SubtitleEvidence::Unsupported { message } if speech => Some(message.clone()),
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
            .when_some(
                self.import_model_request().filter(|_| speech),
                |view, (provider, model, root)| {
                    view.child(self.model_status_line(provider, Some(&model), &root, window, cx))
                },
            )
            .when(
                !speech
                    && !pending
                    && !self.import.subtitle_loading
                    && !failure
                    && source.selected_subtitle.is_some(),
                |view| {
                    view.children(ui::async_status(
                        "subtitle-confirmed",
                        &ui::AsyncStatus::Success(course2md::tr!("有字幕，将直接使用").into()),
                        None,
                        cx,
                    ))
                },
            )
            .when(self.import.subtitle_loading, |view| {
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
                            this.abort_subtitle_read();
                            this.save_current_draft(cx);
                            cx.notify();
                        })),
                )
            })
    }

    fn text_source_menu(
        &self,
        source: &source::Source,
        chip_value: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> MaterialMenu {
        let speech = self.import_uses_speech(cx);
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
        let mut texts = vec![];
        let mut items: Vec<MaterialMenuItem> = tracks
            .into_iter()
            .map(|track| {
                let id = track.id.clone();
                let name = course2md::subtitle::track_name(&track);
                let kind = course2md::subtitle::track_kind_label(&track);
                texts.push(name.clone());
                texts.extend(kind.clone());
                let confirmed = cached.as_ref() == Some(&id);
                let item = MaterialMenuItem::new(name);
                let item = match kind {
                    Some(kind) => item.supporting_text(kind),
                    None => item,
                };
                item.leading(Icon::from(symbols::subtitles()))
                    .selected(selected.as_ref() == Some(&id))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if confirmed {
                            this.use_confirmed_subtitle(cx);
                        } else {
                            this.confirm_subtitle(track.clone(), cx);
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
            texts.push(subtitle.label.clone());
            items.push(
                MaterialMenuItem::new(subtitle.label.clone())
                    .leading(Icon::from(symbols::subtitles()))
                    .selected(selected.as_ref() == Some(&subtitle.track_id))
                    .on_click(cx.listener(|this, _, _, cx| this.use_confirmed_subtitle(cx))),
            );
        }
        items.push(
            MaterialMenuItem::new(course2md::tr!("重新读取字幕"))
                .leading(Icon::from(symbols::refresh()))
                .disabled(self.import.subtitle_loading)
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
        MaterialMenu::new("text-source-items")
            .width(text_source_menu_width(chip_value, &texts, window))
            .items(items)
    }

    fn import_uses_speech(&self, cx: &App) -> bool {
        self.import
            .source_preview
            .as_ref()
            .is_some_and(|source| uses_speech(source, self.text_mode(cx)))
    }

    fn preview_card_shown(&self, cx: &App) -> bool {
        self.import.preview_cancel.is_none()
            && self.import.source_preview.is_some()
            && self.followed_live_task(cx).is_none()
    }

    fn existing_source_note<'a>(&self, cx: &'a App) -> Option<&'a Course> {
        let source = self.import.source_preview.as_ref()?;
        let preferred = self
            .workspace(cx)
            .and_then(|workspace| workspace.state.library(&workspace.state.default_library))
            .map(|library| library.root.as_path());
        self.library(cx).preferred_note(&source.identity, preferred)
    }

    fn current_preview_task<'a>(&'a self, cx: &'a App) -> Option<&'a workspace::TaskRecord> {
        let workspace = self.workspace(cx)?;
        current_course_task(workspace.state.draft()?, &workspace.state)
    }

    /// The task just submitted from Home while it is actively `Running` or `Queued`.
    fn followed_live_task<'a>(&'a self, cx: &'a App) -> Option<&'a workspace::TaskRecord> {
        let followed_id = self.followed_task.as_deref()?;
        self.current_preview_task(cx).filter(|task| {
            task.id == followed_id
                && matches!(
                    task.status,
                    workspace::TaskStatus::Running | workspace::TaskStatus::Queued
                )
        })
    }

    fn can_start_input(&self, cx: &App) -> bool {
        let preview_error = self
            .import
            .issue
            .as_ref()
            .is_some_and(|i| i.kind == WorkbenchIssueKind::Preview);
        let active_matching = self.current_preview_task(cx).is_some_and(|task| {
            matches!(
                task.status,
                workspace::TaskStatus::Running | workspace::TaskStatus::Queued
            )
        });
        self.import.source_collection_title.is_none()
            && (self.online(cx) || !self.value(Field::Source, cx).is_empty())
            && !preview_error
            && self.existing_source_note(cx).is_none()
            && !active_matching
    }

    #[cfg(feature = "capture")]
    fn capture_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering;
        static DONE: AtomicBool = AtomicBool::new(false);
        let Ok(link) = std::env::var("COURSE2MD_CAPTURE_SOURCE") else {
            return;
        };
        if DONE.swap(true, Ordering::Relaxed) {
            return;
        }
        if std::path::Path::new(&link).is_file() {
            self.drop_source_files(&[PathBuf::from(link)], window, cx);
            return;
        }
        self.inputs[&Field::Source].update(cx, |state, cx| state.set_value(link, window, cx));
        self.inspect_source(window, cx);
    }

    pub(super) fn prepare_workbench_input(
        &mut self,
        #[allow(unused_variables)] window: &mut Window,
        #[allow(unused_variables)] cx: &mut Context<Self>,
    ) -> bool {
        #[cfg(feature = "capture")]
        self.capture_source(window, cx);
        true
    }

    pub fn new_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(feature = "capture")]
        if let Some(offset) = std::env::var("COURSE2MD_CAPTURE_SCROLL")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
        {
            self.scrolls[Page::New as usize].set_offset(point(px(0.), px(-offset)));
        }
        let compact = is_compact(window);
        let followed_live = self.followed_live_task(cx).cloned();
        let active_issue = if followed_live.is_some() {
            None
        } else {
            self.active_workbench_issue(cx)
        };
        let has_blocking_issue = active_issue.is_some();

        let card = match followed_live {
            Some(task) => Some(self.tasks.update(cx, |page, cx| {
                page.task_card(&task, CardPlace::Home, window, cx)
            })),
            None if self.preview_card_shown(cx) => {
                self.selected_video(has_blocking_issue, window, cx)
            }
            None => None,
        };
        let issue_banner = self.workbench_issue_banner(active_issue, cx);
        let reading = self
            .import
            .preview_cancel
            .is_some()
            .then(|| self.source_reading(window, cx));
        let candidates = self
            .import
            .source_collection_title
            .clone()
            .map(|title| self.candidate_list(&title, cx));
        let something_below =
            card.is_some() || reading.is_some() || candidates.is_some() || issue_banner.is_some();
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
            .child(self.source_input(window, cx))
            .children(reading)
            .children(candidates)
            .children(card)
            .children(issue_banner);
        let active = self.workbench_active_tasks_section(window, cx);
        let recent = self
            .recent_notes_section(cx)
            .map(|recent| motion::enter("home-recent-notes", recent));
        let quiet = !something_below && recent.is_none() && active.is_none();
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
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_source_files(paths.paths(), window, cx)
            }))
            .into_any_element()
    }

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
                drop_target(
                    "workbench-drop-target",
                    course2md::tr!("松开以添加视频"),
                    true,
                )
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(rem(16.))
                .rounded(RADIUS_HERO)
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

    fn cached_preview_plan(&mut self, cx: &mut App) -> PreviewPlan {
        let key = PlanKey {
            workspace: self
                .workspace(cx)
                .map_or(0, |workspace| workspace.state.revision),
            preferences: self
                .preferences
                .revision(crate::preferences::PreferenceGroup::Generation),
            services: self
                .preferences
                .revision(crate::preferences::PreferenceGroup::Services),
            services_blocked: self
                .preferences
                .is_blocked(crate::preferences::PreferenceGroup::Services),
            source: self.import.preview_generation,
            reading: self.import.reading(),
            subtitle_failed: self.has_subtitle_issue(),
            environment_ready: self.environment.is_some(),
            library: self.library(cx).generation,
            library_loading: self.library(cx).loading,
        };
        if let Some((cached, plan)) = &self.import.preview_plan_cache
            && *cached == key
        {
            return plan.clone();
        }
        let plan = self.preview_plan(cx).map_err(|error| {
            let pending = error
                .downcast_ref::<desktop_tasks::plan::PlanPending>()
                .is_some();
            (
                WorkbenchIssue::from_error(WorkbenchIssueKind::Plan, &error),
                pending,
            )
        });
        self.import.preview_plan_cache = Some((key, plan.clone()));
        plan
    }

    /// Calm inline context when `current_course_task` is already `Running`/`Queued`
    /// or when an existing note matches the previewed source on Home.
    fn preview_context(
        &mut self,
        has_blocking_issue: bool,
        cx: &mut Context<Self>,
    ) -> PreviewContext {
        let reading = self.import.reading();
        let preview_plan = self.cached_preview_plan(cx);
        let plan_pending = matches!(&preview_plan, Err((_, true)));
        let checking = self.environment.is_none() || plan_pending;

        let button = |id: &'static str, variant, icon, label: &'static str| {
            material_button(id, variant)
                .when(variant == ButtonVariant::Outlined, |button| {
                    button.tone(ButtonTone::Accent)
                })
                .icon(icon)
                .child(label)
        };

        if let Some(active_task) = self.current_preview_task(cx).filter(|task| {
            matches!(
                task.status,
                workspace::TaskStatus::Running | workspace::TaskStatus::Queued
            )
        }) {
            let id = active_task.id.clone();
            let title = active_task.title.clone();
            let location = self
                .task_library(active_task, cx)
                .map(|lib| lib.label())
                .unwrap_or_else(|| course2md::tr!("原保存位置").into());
            let info_line = Some(
                supporting_info(
                    "conversion-fact-consequence",
                    course2md::tr!("已有相同处理任务：《{}》（{}）", title, location),
                )
                .into_any_element(),
            );
            let view_btn = button(
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
            }));
            return PreviewContext {
                info_line,
                actions: vec![view_btn],
                has_primary_override: true,
                blocked: has_blocking_issue || checking,
            };
        }

        if let Some(course) = self.existing_source_note(cx).cloned() {
            let location = self
                .course_location(&course, cx)
                .map(|library| library.label())
                .unwrap_or_else(|| course2md::tr!("笔记库").into());
            let info_line = Some(
                supporting_info(
                    "existing-note-notice",
                    course2md::tr!("已有笔记，保存在「{}」；重新生成会更新现有笔记", location),
                )
                .into_any_element(),
            );
            let title = self.library(cx).course_display_title(&course);
            let disabled = reading
                || checking
                || has_blocking_issue
                || self.records.read(cx).workspace.is_none()
                || self.text_source_missing(cx);

            let mut actions = Vec::new();
            if course.incomplete() {
                let resume_course = course.clone();
                actions.push(
                    button(
                        "resume-incomplete-note",
                        ButtonVariant::Outlined,
                        symbols::refresh(),
                        course2md::tr!("补全未完成部分"),
                    )
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.save_current_draft(cx);
                        this.retry_or_regenerate_course(
                            resume_course.clone(),
                            workspace::RunMode::Resume,
                            cx,
                        );
                    })),
                );
            }
            actions.push(
                button(
                    "regenerate-note",
                    ButtonVariant::Outlined,
                    symbols::restart_alt(),
                    course2md::tr!("重新生成"),
                )
                .track_focus(&self.import_submit_focus)
                .disabled(disabled)
                .on_click(cx.listener(move |_, _, window, cx| {
                    ui::Confirm::new(
                        course2md::tr!("重新生成笔记"),
                        course2md::tr!("保留当前笔记"),
                        course2md::tr!("重新生成"),
                    )
                    .text(course2md::tr!(
                        "《{}》将被新生成的笔记覆盖。你手动改过的文件会以 .edited 为名保留在笔记旁。",
                        title
                    ))
                    .ask(
                        cx.listener(|this, _, window, cx| {
                            this.enqueue_current_with_mode(workspace::RunMode::Fresh, window, cx);
                        }),
                        window,
                        cx,
                    );
                })),
            );
            actions.push(
                button(
                    "open-existing-note",
                    ButtonVariant::Filled,
                    symbols::menu_book(),
                    course2md::tr!("打开笔记"),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.save_current_draft(cx);
                    this.open_course(course.clone(), cx);
                })),
            );
            return PreviewContext {
                info_line,
                actions,
                has_primary_override: true,
                blocked: has_blocking_issue || checking,
            };
        }

        let info_line = match &preview_plan {
            Err((issue, true)) if checking => ui::async_status(
                "import-submit-status",
                &ui::AsyncStatus::Busy(issue.message.clone().into()),
                None,
                cx,
            )
            .map(IntoElement::into_any_element),
            _ => None,
        };

        PreviewContext {
            info_line,
            actions: Vec::new(),
            has_primary_override: false,
            blocked: has_blocking_issue || checking,
        }
    }
}

/// Sign-in dialogs for online sources on Home.
impl Desktop {
    fn sign_in_source(&self, subtitles: bool, cx: &App) -> bool {
        let preview_err = self
            .import
            .issue
            .as_ref()
            .is_some_and(|i| i.kind == WorkbenchIssueKind::Preview);
        !self.onboarding.active
            && matches!(self.page, Page::New)
            && self.online(cx)
            && (preview_err || (subtitles && self.subtitle_attention_required(cx)))
    }

    fn source_now(&self, cx: &App) -> (u64, String) {
        (
            self.import.preview_generation,
            self.value(Field::Source, cx),
        )
    }

    pub(crate) fn open_account_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let retry = (self.sign_in_source(true, cx)
            && course2md::auth::is_bilibili_url(&self.value(Field::Source, cx)))
        .then(|| self.source_now(cx));
        self.accounts.update(cx, |accounts, cx| {
            accounts.open_account_dialog(retry, window, cx)
        });
    }

    pub(crate) fn open_youtube_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let retry = (self.sign_in_source(false, cx)
            && course2md::auth::is_youtube_url(&self.value(Field::Source, cx)))
        .then(|| self.source_now(cx));
        let origin = if self.onboarding.active {
            GuideOrigin::Onboarding
        } else if self.page == Page::Settings {
            GuideOrigin::Settings
        } else {
            GuideOrigin::Elsewhere
        };
        self.accounts.update(cx, |accounts, cx| {
            if accounts.youtube.dialog.is_none() {
                accounts.youtube.origin = origin;
            }
            accounts.open_youtube_dialog(retry, window, cx)
        });
    }

    fn can_retry_account_source(&self, cx: &App) -> bool {
        let (generation, input) = self.source_now(cx);
        self.accounts.read(cx).account.can_resume_source(
            generation,
            &input,
            self.sign_in_source(true, cx),
        )
    }

    pub(crate) fn account_dialog(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let ready = self.can_retry_account_source(cx);
        self.accounts.update(cx, |accounts, cx| {
            accounts.account_dialog(ready, window, cx)
        })
    }

    pub(crate) fn youtube_dialog(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        self.accounts
            .update(cx, |accounts, cx| accounts.youtube_dialog(window, cx))
    }

    pub(crate) fn accounts_event(
        &mut self,
        _: &Entity<crate::account_ui::AccountsView>,
        event: &crate::account_ui::AccountsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::account_ui::AccountsEvent;
        match event {
            AccountsEvent::OpenAccount => self.open_account_dialog(window, cx),
            AccountsEvent::OpenYoutube => self.open_youtube_dialog(window, cx),
            AccountsEvent::SignedIn { source } => {
                if self.sign_in_source(true, cx) && *source == self.source_now(cx) {
                    self.inspect_source(window, cx);
                }
            }
            AccountsEvent::YoutubeConnected { source } => {
                if *source == self.source_now(cx) {
                    self.inspect_source(window, cx);
                }
            }
            AccountsEvent::Relaunch => self.relaunch_for_access(cx),
            AccountsEvent::ResumeYoutube { origin } => {
                self.return_to_guide_origin(*origin, window, cx);
                self.open_youtube_dialog(window, cx);
            }
        }
    }
}

fn preview_label_fits(label: &str, window: &Window) -> bool {
    let scale = f32::from(window.rem_size()) / gpui_kit_material::tokens::typography::BASE_PX;
    let width = f32::from(window.bounds().size.width) / scale;
    let field = (width - 32.).min(760.);
    let action =
        crate::views::shaped_width(label, 16. * scale, FontWeight::MEDIUM, window) / scale + 76.;
    field - action >= 240.
}

fn text_source_menu_width(chip_value: &str, texts: &[String], window: &Window) -> f32 {
    let scale = f32::from(window.rem_size()) / gpui_kit_material::tokens::typography::BASE_PX;
    let longest = std::iter::once(chip_value)
        .chain(texts.iter().map(String::as_str))
        .chain([
            course2md::tr!("重新读取字幕"),
            course2md::tr!("选择字幕文件"),
        ])
        .map(|text| {
            crate::views::shaped_width(text, 14. * scale, FontWeight::NORMAL, window) / scale
        })
        .fold(0_f32, f32::max);
    (longest + 88.).clamp(220., 400.)
}
