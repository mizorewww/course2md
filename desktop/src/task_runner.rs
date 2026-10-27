//! Task execution: draft persistence, plan construction, dispatch to the
//! engine process and recording its events. The task page lives in task_ui.
use super::*;
use crate::task_ui::{TaskGroup, ai_component, task_component_failures, task_group};
use crate::{
    notes::default_output,
    preferences::ServiceRefs,
    workspace::{Intent, TaskPlan, TaskRecord, TaskState},
};
use anyhow::{Context as _, Result, ensure};
use course2md::config::TranscriptSource;
use course2md::execution::Component;
use course2md::progress::{BlockReason, Stage, StageStatus as EngineStageStatus};
use std::collections::HashMap;

/// A stable validation identity for the missing-title case. Rendering compares
/// the typed error instead of the displayed sentence, which changes with the
/// interface language.
#[derive(Debug)]
pub(crate) struct TitleRequired;

impl std::fmt::Display for TitleRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(course2md::tr!("请填写笔记名称"))
    }
}

impl std::error::Error for TitleRequired {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlanValidation {
    Preview,
    Submission,
}

/// Published task state and its visible card change together. A full library
/// scan can include slow locations, so its old manifest cannot be the only
/// source of the card while the newly published version is already readable.
fn update_published_course(
    courses: &mut Vec<Rc<Course>>,
    done: &Done,
    manifest: &course2md::artifact::Manifest,
    thumbnail: Option<PathBuf>,
) {
    let mut course = Course {
        dir: done.out_dir.clone(),
        title: manifest.title.clone(),
        modified: std::time::UNIX_EPOCH + std::time::Duration::from_millis(manifest.created_at_ms),
        slides: manifest.frames.len(),
        thumbnail,
        manifest: Some(manifest.clone()),
        warning: None,
    };
    let storage = course.storage_dir();
    if let Some(previous) = courses
        .iter()
        .find(|previous| previous.storage_dir() == storage)
    {
        // A user-facing library alias belongs to the course across versions.
        if previous
            .manifest
            .as_ref()
            .is_some_and(|manifest| previous.title != manifest.title)
        {
            course.title = previous.title.clone();
        }
    }
    courses.retain(|previous| previous.storage_dir() != storage);
    courses.push(Rc::new(course));
    courses.sort_by_key(|course| std::cmp::Reverse(course.modified));
}

/// A plan check still waiting for background work (a location check, a
/// folder index); the workbench shows it as progress, not as a failure.
#[derive(Debug)]
pub(crate) struct PlanPending(pub(crate) &'static str);

impl std::fmt::Display for PlanPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for PlanPending {}

fn validate_plan_storage(
    validation: PlanValidation,
    library: &workspace::LibraryLocation,
    folder: Option<u64>,
    cached: Option<&crate::storage_ui::LocationCheck>,
    index: Option<&organize::Library>,
) -> Result<()> {
    let folder_exists = match validation {
        PlanValidation::Preview => {
            let check = cached.ok_or(PlanPending(course2md::tr!("正在检查保存位置，请稍候")))?;
            ensure!(
                check.available,
                course2md::tr!("保存位置不可访问，请连接磁盘或选择其他位置")
            );
            ensure!(
                !check.needs_reassociation,
                course2md::tr!("保存位置需要重新关联")
            );
            if let Some(problem) = &check.problem {
                anyhow::bail!("{problem}");
            }
            match folder {
                Some(folder) => index
                    .ok_or(PlanPending(course2md::tr!("正在读取文件夹信息，请稍候")))?
                    .folders
                    .contains_key(&folder),
                None => true,
            }
        }
        PlanValidation::Submission => {
            ensure!(
                library.root.is_dir(),
                course2md::tr!("保存位置不可访问，请连接磁盘或选择其他位置")
            );
            workspace::check_library(library)?;
            match folder {
                Some(folder) => organize::Library::load(&library.root)?
                    .folders
                    .contains_key(&folder),
                None => true,
            }
        }
    };
    ensure!(
        folder_exists,
        course2md::tr!("文件夹已删除。请选择其他文件夹，或保存到未分类。")
    );
    Ok(())
}

fn update_draft_source_title(
    draft: &mut workspace::Draft,
    input: String,
    title: String,
    source: Option<source::Source>,
) {
    // A source edit can clear Draft::title while the input still contains the
    // previous automatic title. Capture its provenance before changing sources;
    // repeated autosaves while the new metadata is pending must retain it too.
    let unchanged_automatic_title = !draft.custom_title && draft.title == title;
    draft.change_source(input);
    if let Some(source) = source {
        draft.source = Some(source);
    }
    if unchanged_automatic_title {
        draft.title = title;
    } else if draft.title != title {
        draft.custom_title = !title.is_empty()
            && draft
                .source
                .as_ref()
                .is_none_or(|source| source.title != title);
        draft.title = title;
    }
}

/// Apply the visible form without treating a save attempt as a user edit.
/// Navigation often calls this with identical values; keep timestamps and disk
/// records unchanged unless an actual input or option changed.
fn update_input_form(
    draft: &mut workspace::Draft,
    input: String,
    title: String,
    source: Option<source::Source>,
    options: ConversionOptions,
    folder: Option<u64>,
    scroll: f32,
) -> bool {
    let previous = draft.clone();
    update_draft_source_title(draft, input, title, source);
    apply_draft_options(draft, options);
    draft.folder = folder;
    draft.scroll = scroll;
    draft.updated = previous.updated;
    if *draft == previous {
        return false;
    }
    draft.updated = workspace::now();
    true
}

/// Replace the draft's processing options, recording each changed choice as an
/// explicit override so later default changes no longer reach it.
fn apply_draft_options(draft: &mut workspace::Draft, options: ConversionOptions) {
    use workspace::Override;
    for (changed, field) in [
        (
            draft.options.provider != options.provider,
            Override::Provider,
        ),
        (
            draft.options.source_mode != options.source_mode,
            Override::TextSource,
        ),
        (draft.options.llm != options.llm, Override::Proofread),
        (
            draft.options.summarize != options.summarize,
            Override::Summary,
        ),
        (draft.options.vision != options.vision, Override::Vision),
        (
            draft.options.translate != options.translate,
            Override::Translate,
        ),
        (
            draft.options.keep_video != options.keep_video,
            Override::KeepVideo,
        ),
        (draft.options.formats != options.formats, Override::Formats),
    ] {
        if changed {
            draft.overrides.insert(field);
        }
    }
    if !options.uses_cloud_provider() {
        draft.local_provider = options.provider;
    } else if !draft.options.uses_cloud_provider() {
        draft.local_provider = draft.options.provider;
    }
    draft.options = options;
}

/// The single write path for the current draft: apply `edit` to a copy and
/// commit it in one workspace transaction. An edit that changes nothing
/// neither writes nor moves the timestamp. Returns whether a write happened.
fn edit_current_draft(
    workspace: &mut workspace::Workspace,
    edit: impl FnOnce(&mut workspace::Draft),
) -> Result<bool> {
    let current = workspace
        .state
        .draft()
        .context(course2md::tr!("当前输入记录暂时不可用"))?;
    let mut next = current.clone();
    edit(&mut next);
    next.updated = current.updated;
    if next == *current {
        return Ok(false);
    }
    next.updated = workspace::now();
    workspace.transaction(|state| {
        *state
            .draft_mut()
            .context(course2md::tr!("没有可保存的输入"))? = next;
        Ok(())
    })?;
    Ok(true)
}

/// Options shown before the workspace has restored a draft.
static NO_DRAFT_OPTIONS: std::sync::LazyLock<ConversionOptions> =
    std::sync::LazyLock::new(ConversionOptions::default);

impl Desktop {
    /// The current input form. It is the only copy of its choices: views read
    /// it and option controls write through `update_draft`.
    pub(crate) fn draft(&self) -> Option<&workspace::Draft> {
        self.workspace.as_ref().and_then(|w| w.state.draft())
    }

    /// Whether the current input is an online link (the default before a
    /// workspace is restored) rather than a local file.
    pub(crate) fn online(&self) -> bool {
        self.draft().is_none_or(|draft| draft.online)
    }

    pub(crate) fn task_options(&self) -> &ConversionOptions {
        self.draft()
            .map(|draft| &draft.options)
            .unwrap_or(&NO_DRAFT_OPTIONS)
    }

    pub(crate) fn target_folder(&self) -> Option<u64> {
        self.draft().and_then(|draft| draft.folder)
    }

    /// Change the current draft and record it. A failed write is reported
    /// beside the workspace and returns false.
    pub(crate) fn update_draft(
        &mut self,
        edit: impl FnOnce(&mut workspace::Draft),
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(workspace) = &mut self.workspace else {
            return false;
        };
        match edit_current_draft(workspace, edit) {
            Ok(_) => {
                self.workspace_error = None;
                true
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("当前输入尚未保存：{}", format!("{error:#}")));
                cx.notify();
                false
            }
        }
    }

    /// Change this task's processing options; changed choices become overrides.
    pub(crate) fn update_task_options(
        &mut self,
        edit: impl FnOnce(&mut ConversionOptions),
        cx: &mut Context<Self>,
    ) -> bool {
        self.update_draft(
            |draft| {
                let mut options = draft.options.clone();
                edit(&mut options);
                apply_draft_options(draft, options);
            },
            cx,
        )
    }

    pub fn retry_workspace_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = &self.workspace {
            match workspace.save() {
                Ok(()) => self.workspace_error = None,
                Err(error) => {
                    self.workspace_error =
                        Some(course2md::tr!("记录仍未保存：{}", format!("{error:#}")))
                }
            }
        } else {
            let root = self
                .config
                .defaults
                .out
                .clone()
                .unwrap_or_else(default_output);
            match workspace::Workspace::open(
                root,
                ConversionOptions::from_config(&self.preferences.defaults_config()),
            ) {
                Ok(workspace) => {
                    self.message = workspace.recovery.clone();
                    self.workspace = Some(workspace);
                    self.workspace_error = None;
                    self.restore_draft(window, cx);
                    self.refresh_library(cx);
                }
                Err(error) => {
                    self.workspace_error = Some(course2md::tr!(
                        "记录仍无法读取，原件已保留：{}",
                        format!("{error:#}")
                    ))
                }
            }
        }
        if self.preference_defaults_pending {
            self.refresh_preference_defaults(cx);
        }
        if self.workspace_error.is_none() {
            self.advance_conversion_when_ready(cx);
        }
        cx.notify();
    }

    /// Called only by the explicit global recovery action; rebuilding never starts the queue.
    pub fn rebuild_workspace_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() || self.storage_ui.busy {
            self.workspace_error = Some(
                course2md::tr!("当前还有处理或文件移动在进行，请等候停止后再重建记录。").into(),
            );
            cx.notify();
            return;
        }
        let root = self
            .config
            .defaults
            .out
            .clone()
            .unwrap_or_else(default_output);
        let options = ConversionOptions::from_config(&self.preferences.defaults_config());
        let result = if let Some(workspace) = &self.workspace {
            workspace::Workspace::rebuild_at(workspace.storage_path().to_owned(), root, options)
        } else {
            workspace::Workspace::rebuild(root, options)
        };
        match result {
            Ok(workspace) => {
                self.message = workspace.recovery.clone();
                self.workspace = Some(workspace);
                self.workspace_error = None;
                self.restore_draft(window, cx);
                self.refresh_library(cx);
            }
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "记录尚未重建，原件仍保留：{}",
                    format!("{error:#}")
                ))
            }
        }
        cx.notify();
    }

    pub fn recommended_local_provider(&self) -> course2md::config::AsrProvider {
        use course2md::config::AsrProvider;
        match &self.environment {
            Some(environment) if environment.supports(AsrProvider::Coreml) => AsrProvider::Coreml,
            Some(environment) if environment.supports(AsrProvider::Gpu) => AsrProvider::Gpu,
            Some(environment) if environment.supports(AsrProvider::Npu) && !environment.llama => {
                AsrProvider::Npu
            }
            Some(_) => AsrProvider::Cpu,
            // Detection runs in the background. A temporary display hint must
            // not scan Linux devices or PATH while the interface is rendering.
            None if course2md::config::apple_native_available() => AsrProvider::Coreml,
            None => AsrProvider::Cpu,
        }
    }

    /// Whether this task holds the running background job (its record may lag the event stream).
    pub(crate) fn is_running_task(&self, id: &str) -> bool {
        self.active_task.as_deref() == Some(id) && self.job.is_some()
    }

    /// The library location a task's plan saves to; the record may name a removed library.
    pub(crate) fn task_library(&self, task: &TaskRecord) -> Option<&workspace::LibraryLocation> {
        self.workspace
            .as_ref()
            .and_then(|w| w.state.library(&task.plan.library_id))
    }

    /// Record the text fields, preview and scroll position of the visible form.
    pub fn save_current_draft(&mut self, cx: &mut Context<Self>) -> bool {
        let input = self.value(Field::Source, cx);
        // A source edit must establish the next form's defaults before autosave
        // can detach the submitted task. Failed preparation cannot fall through
        // to saving the previous video's title and overrides against a new URL.
        if self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.draft())
            .is_some_and(|draft| draft.submitted_task.is_some() && draft.input != input)
        {
            return false;
        }
        let title = self.value(Field::Title, cx);
        let source = self.source_preview.clone();
        let scroll = f32::from(self.scrolls[Page::New as usize].offset().y);
        let Some(workspace) = &mut self.workspace else {
            return false;
        };
        let Some(mut next) = workspace.state.draft().cloned() else {
            self.workspace_error = Some(course2md::tr!("当前输入记录暂时不可用").into());
            return false;
        };
        self.draft_deadline = None;
        let (options, folder) = (next.options.clone(), next.folder);
        if !update_input_form(&mut next, input, title, source, options, folder, scroll) {
            return true;
        }
        let result = workspace.transaction(|state| {
            *state
                .draft_mut()
                .context(course2md::tr!("没有可保存的输入"))? = next;
            Ok(())
        });
        if let Err(error) = result {
            self.workspace_error =
                Some(course2md::tr!("当前输入尚未保存：{}", format!("{error:#}")));
            cx.notify();
            return false;
        }
        self.workspace_error = None;
        true
    }

    pub fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.draft())
            .cloned()
        else {
            return;
        };
        self.source_preview = draft.source.clone();
        self.preview_error = None;
        self.source_validation = None;
        self.title_required = false;
        self.source_candidates.clear();
        self.source_collection_title = None;
        self.source_collection_name = None;
        self.selected_candidates.clear();
        self.subtitle_error = draft.source.as_ref().and_then(|source| {
            source
                .subtitle_read_error
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| {
                    source.subtitle_request.as_ref().map(|_| {
                        course2md::tr!("所选字幕还未读取完成，请继续读取或选择其他文字来源。")
                            .into()
                    })
                })
        });
        self.subtitle_loading = false;
        self.inputs[&Field::Source].update(cx, |s, cx| s.set_value(draft.input, window, cx));
        self.inputs[&Field::Title].update(cx, |s, cx| s.set_value(draft.title, window, cx));
        self.scrolls[Page::New as usize].set_offset(point(px(0.), px(draft.scroll)));
        self.draft_deadline = None;
        cx.notify();
    }

    pub fn switch_source_kind(
        &mut self,
        online: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.online() == online {
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
        if let Some(workspace) = &mut self.workspace {
            match workspace.transaction(|state| {
                state.switch_source_kind(online, defaults);
                Ok(())
            }) {
                Ok(()) => {
                    self.following_conversion = None;
                    self.invalidate_source();
                    self.completed_source = None;
                    self.message = None;
                    self.restore_draft(window, cx);
                }
                Err(error) => {
                    self.workspace_error =
                        Some(course2md::tr!("无法切换来源：{}", format!("{error:#}")))
                }
            }
        }
    }

    /// The plan builders' shared preamble: the workspace and draft they both require.
    fn plan_draft(&self) -> Result<(&workspace::Workspace, &workspace::Draft)> {
        let workspace = self
            .workspace
            .as_ref()
            .context(course2md::tr!("输入与任务记录尚未恢复"))?;
        let draft = workspace
            .state
            .draft()
            .context(course2md::tr!("当前输入尚未准备好"))?;
        Ok((workspace, draft))
    }

    /// The plan builders' shared tool check: the engine and FFmpeg always; an `online` source also needs
    /// yt-dlp (batch plans are always online; a single plan follows its confirmed source).
    fn ensure_plan_environment(&self, online: bool) -> Result<&crate::backend::Environment> {
        let environment = self
            .environment
            .as_ref()
            .context(course2md::tr!("正在检查所需组件…"))?;
        ensure!(environment.engine, course2md::tr!("生成引擎无法启动"));
        ensure!(
            environment.ffmpeg && environment.ffprobe,
            course2md::tr!("缺少视频处理组件，请在设置中查看")
        );
        ensure!(
            !online || environment.ytdlp,
            course2md::tr!("缺少在线视频下载组件，请在设置中查看")
        );
        Ok(environment)
    }

    /// The destination is still a registered, writable library; both plan builders share these checks and messages.
    fn ensure_plan_storage(
        &self,
        workspace: &workspace::Workspace,
        draft: &workspace::Draft,
        validation: PlanValidation,
    ) -> Result<()> {
        let library = workspace
            .state
            .library(&draft.library_id)
            .context(course2md::tr!("保存位置已不在笔记库中，请选择保存位置"))?;
        validate_plan_storage(
            validation,
            library,
            draft.folder,
            self.cached_location_check(library),
            self.library_indexes.get(&library.root),
        )?;
        Ok(())
    }

    /// Draft options override the base config and normalize the model directory; both plan builders build it this way.
    fn plan_config(&self, draft: &workspace::Draft) -> course2md::settings::ConfigFile {
        let mut config = draft
            .base_config
            .clone()
            .unwrap_or_else(|| self.preferences.defaults_config());
        draft.options.apply_to(&mut config);
        config.defaults.model_dir = Some(course2md::config::model_dir_from(
            config.defaults.model_dir.as_deref(),
        ));
        config
    }

    /// The draft's service references, falling back to the default services.
    fn plan_refs(&self, draft: &workspace::Draft) -> ServiceRefs {
        let defaults = self.preferences.default_refs();
        ServiceRefs {
            asr: draft.asr_service.clone().or(defaults.asr),
            llm: draft.ai_service.clone().or(defaults.llm),
        }
    }

    fn build_plan(&self, validation: PlanValidation) -> Result<TaskPlan> {
        self.ordinary_preferences_ready_for_submit()?;
        ensure!(
            !self.preference_defaults_pending,
            course2md::tr!("默认设置已保存，当前视频的选项尚未同步。请重试。")
        );
        let (workspace, draft) = self.plan_draft()?;
        ensure!(
            !draft.input.is_empty(),
            if draft.online {
                course2md::tr!("先粘贴视频链接")
            } else {
                course2md::tr!("先选择一个视频")
            }
        );
        ensure!(
            !self.subtitle_loading && self.preview_cancel.is_none(),
            course2md::tr!("正在确认来源，请等待读取完成")
        );
        ensure!(
            self.source_preview.is_some(),
            course2md::tr!("还没读取到视频信息，请重新预览")
        );
        ensure!(
            self.subtitle_error.is_none(),
            course2md::tr!("字幕还没读取完成，请重新读取，或改用语音识别")
        );
        let source = draft
            .source
            .clone()
            .context(course2md::tr!("请先读取并确认视频"))?;
        let environment = self.ensure_plan_environment(source.online)?;
        ensure!(
            draft.options.source_mode == TranscriptSource::Asr
                || (source.subtitle_request.is_none() && source.subtitle_read_error.is_none()),
            course2md::tr!("所选字幕还未确认，请继续读取或明确选择其他文字来源。")
        );
        ensure!(
            !source.identity.is_empty(),
            course2md::tr!("无法识别这个视频，请重新预览")
        );
        if draft.title.trim().is_empty() {
            return Err(TitleRequired.into());
        }
        self.ensure_plan_storage(workspace, draft, validation)?;
        let mut config = self.plan_config(draft);
        let using_subtitle = draft.options.source_mode != TranscriptSource::Asr
            && (source.selected_subtitle.is_some() || draft.subtitle.is_some());
        if using_subtitle {
            if let Some(subtitle) = &source.selected_subtitle {
                ensure!(
                    subtitle.source_identity == source.identity,
                    course2md::tr!("所选字幕与当前视频不匹配，请重新读取字幕")
                );
            }
            config.defaults.transcript_source = Some(course2md::config::TranscriptSource::Subtitle);
        } else {
            use course2md::subtitle::SubtitleEvidence;
            ensure!(
                draft.options.source_mode == TranscriptSource::Asr
                    || (draft.options.source_mode == TranscriptSource::Auto
                        && matches!(
                            source.subtitles,
                            SubtitleEvidence::NoneFound
                                | SubtitleEvidence::Unsupported { .. }
                                | SubtitleEvidence::Failed { .. }
                        )),
                course2md::tr!("字幕还未确认。请读取字幕，或明确选择语音识别。")
            );
            config.defaults.transcript_source = Some(course2md::config::TranscriptSource::Asr);
            let provider = config
                .defaults
                .provider
                .unwrap_or_else(|| self.recommended_local_provider());
            ensure_asr_capability(environment, provider)?;
            config.defaults.provider = Some(provider);
            fill_default_asr_model(&mut config, provider);
        }
        let refs = self.plan_refs(draft);
        let config = match validation {
            PlanValidation::Preview => self.preferences.config_for_preview(&config, &refs)?,
            PlanValidation::Submission => self.preferences.config_for_refs(&config, &refs)?,
        };
        let mut validation_config = config.clone();
        validation_config
            .defaults
            .provider
            .get_or_insert_with(|| self.recommended_local_provider());
        validate_plan_config(&source.input, &validation_config)?;
        if validation == PlanValidation::Submission && !source.online {
            ensure!(
                std::path::Path::new(&source.input).is_file(),
                course2md::tr!("原视频已移动或无法读取，请重新选择视频")
            );
        }
        Ok(TaskPlan {
            operation: Default::default(),
            source_id: source.identity.clone(),
            source,
            title: draft.title.clone(),
            library_id: draft.library_id.clone(),
            folder: draft.folder,
            subtitle: draft.subtitle.clone(),
            config,
            asr_service: refs.asr,
            ai_service: refs.llm,
        })
    }

    /// The batch plan for selected candidates. Tools, destination and config resolve as in build_plan;
    /// subtitles are found when each task runs (the pipeline probes by transcript_source), so the desktop
    /// need not read every video first. Titles are the episode titles shown in the candidate rows.
    fn build_batch_plan(
        &self,
        candidate: &source::SourceCandidate,
        title: String,
    ) -> Result<TaskPlan> {
        self.ordinary_preferences_ready_for_submit()?;
        ensure!(
            !self.preference_defaults_pending,
            course2md::tr!("默认设置已保存，当前选项尚未同步。请重试。")
        );
        let (workspace, draft) = self.plan_draft()?;
        let identity = course2md::fetch::candidate_identity(candidate)
            .context(course2md::tr!("还无法确认这个视频的身份，请单独打开后转换"))?;
        let environment = self.ensure_plan_environment(true)?;
        self.ensure_plan_storage(workspace, draft, PlanValidation::Submission)?;
        let mut config = self.plan_config(draft);
        // Subtitles-only never reaches speech recognition; other modes may fall back to ASR at run time,
        // so, as in build_plan, check local recognition and fill in the default model first.
        if draft.options.source_mode != TranscriptSource::Subtitle {
            let provider = config
                .defaults
                .provider
                .unwrap_or_else(|| self.recommended_local_provider());
            let capability = ensure_asr_capability(environment, provider);
            if draft.options.source_mode == TranscriptSource::Auto {
                capability.context(course2md::tr!(
                    "部分视频可能没有字幕，需要语音识别能力作为后备"
                ))?;
            } else {
                capability?;
            }
            config.defaults.provider = Some(provider);
            fill_default_asr_model(&mut config, provider);
        }
        let refs = self.plan_refs(draft);
        let config = self.preferences.config_for_refs(&config, &refs)?;
        let mut validation_config = config.clone();
        validation_config
            .defaults
            .provider
            .get_or_insert_with(|| self.recommended_local_provider());
        validate_plan_config(&candidate.input, &validation_config)?;
        Ok(TaskPlan {
            operation: Default::default(),
            source_id: identity.clone(),
            source: source::Source {
                input: candidate.input.clone(),
                title: title.clone(),
                duration: candidate
                    .duration
                    .filter(|value| value.is_finite() && *value > 0.)
                    .unwrap_or_default(),
                cover: self.source_candidate_covers.get(&candidate.input).cloned(),
                identity,
                online: true,
                ..source::Source::default()
            },
            title,
            library_id: draft.library_id.clone(),
            folder: draft.folder,
            subtitle: None,
            config,
            asr_service: refs.asr,
            ai_service: refs.llm,
        })
    }

    /// Queue the selected candidates at once, reusing per-candidate deduplication (an identical task in
    /// progress is not created again), in one transaction; then select the first task and start the queue.
    pub fn enqueue_selected_candidates(&mut self, cx: &mut Context<Self>) {
        let selected: Vec<(usize, source::SourceCandidate)> = self
            .source_candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| self.selected_candidates.contains(&candidate.input))
            .map(|(index, candidate)| (index, candidate.clone()))
            .collect();
        if selected.is_empty() {
            return;
        }
        self.message = None;
        let collection = self.source_collection_name.clone();
        let mut plans = Vec::new();
        let mut skipped = 0usize;
        for (index, candidate) in selected {
            if course2md::fetch::candidate_identity(&candidate).is_none() {
                skipped += 1;
                continue;
            }
            let title = batch_note_title(&candidate, collection.as_deref(), index);
            match self.build_batch_plan(&candidate, title) {
                Ok(plan) => plans.push(plan),
                Err(error) => {
                    self.message = Some(course2md::tr!(
                        "选中的视频尚未加入队列：{}",
                        format!("{error:#}")
                    ));
                    cx.notify();
                    return;
                }
            }
        }
        if plans.is_empty() {
            self.message = Some(if skipped > 0 {
                course2md::tr!("选中的视频无法识别，请单独打开后转换").into()
            } else {
                course2md::tr!("选中的视频尚未加入队列，请重试").into()
            });
            cx.notify();
            return;
        }
        let total = plans.len();
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let result = workspace.transaction(|state| {
            let mut first_id = None;
            let mut duplicates = 0usize;
            for plan in plans {
                let (id, created) = state.enqueue(plan, None)?;
                first_id.get_or_insert_with(|| id.clone());
                if !created {
                    duplicates += 1;
                }
            }
            Ok((first_id, duplicates))
        });
        match result {
            Ok((first_id, duplicates)) => {
                self.selected_candidates.clear();
                let created = total - duplicates;
                let mut parts = Vec::new();
                if created > 0 {
                    parts.push(course2md::tr!("已把 {} 个视频加入队列", created));
                }
                if duplicates > 0 {
                    parts.push(course2md::tr!(
                        "{} 个与进行中的任务相同，未重复加入",
                        duplicates
                    ));
                }
                if skipped > 0 {
                    parts.push(course2md::tr!("{} 个无法识别，已跳过", skipped));
                }
                self.message = Some(parts.join(course2md::tr!("；")));
                if let Some(id) = first_id {
                    self.select_task(&id, cx);
                }
                self.start_next_task(cx);
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("任务尚未加入队列：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

    /// The plan the form would submit, checked against loaded snapshots.
    /// Submission rechecks live storage, local source and dispatch markers
    /// before a task can enter the queue.
    pub fn preview_plan(&self) -> Result<TaskPlan> {
        self.build_plan(PlanValidation::Preview)
    }

    /// An unfinished task already doing the same work as `plan`.
    pub fn matching_task(&self, plan: &TaskPlan) -> Option<&TaskRecord> {
        self.workspace.as_ref()?.state.tasks.iter().find(|task| {
            !task.state.finished() && task.handled_by.is_none() && task.plan.same_work(plan)
        })
    }

    pub fn enqueue_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        self.message = None;
        let plan = match self.build_plan(PlanValidation::Submission) {
            Ok(plan) => plan,
            Err(error) => {
                self.title_required = error.downcast_ref::<TitleRequired>().is_some();
                self.source_validation = Some(format!("{error:#}"));
                self.validation_attempt = self.validation_attempt.wrapping_add(1);
                let subtitle_attention = self.subtitle_attention_required();
                let field = if self.source_preview.is_some()
                    && !subtitle_attention
                    && self.value(Field::Title, cx).trim().is_empty()
                {
                    Some(Field::Title)
                } else if self.source_preview.is_none() && self.online() {
                    Some(Field::Source)
                } else {
                    None
                };
                if let Some(field) = field {
                    if field == Field::Title {
                        // The name field is in the processing options; show
                        // them for this fix without changing the saved choice.
                        self.desktop_settings.home_options = true;
                        self.pending_conversion = None;
                        self.following_conversion = None;
                    }
                    let input = self.inputs[&field].clone();
                    window.on_next_frame(move |window, cx| {
                        input.update(cx, |input, cx| input.focus(window, cx));
                    });
                }
                cx.notify();
                return;
            }
        };
        // Starting again acknowledges only the terminal feedback belonging to
        // the current foreground input, before the new task replaces its link.
        let acknowledged_input = (self.page == Page::New)
            .then(|| self.current_input_task(cx))
            .flatten()
            .filter(|task| task.state.finished())
            .map(|task| task.id.clone());
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let parent = workspace
            .state
            .draft()
            .and_then(|draft| draft.retry_of.clone());
        let result = workspace.transaction(|state| {
            let (id, created) = state.enqueue(plan, parent)?;
            if let Some(previous) = acknowledged_input
                .as_deref()
                .and_then(|id| state.task_mut(id))
            {
                previous.unread = false;
            }
            if created && let Some(draft) = state.draft_mut() {
                draft.submitted_task = Some(id.clone());
            }
            Ok((id, created))
        });
        match result {
            Ok((id, created)) => {
                self.pending_conversion = None;
                self.source_validation = None;
                self.title_required = false;
                self.message =
                    (!created).then(|| course2md::tr!("已有相同任务，未重复加入").into());
                if created {
                    self.following_conversion = self
                        .following_conversion
                        .take()
                        .filter(|_| self.page == Page::New)
                        .and_then(|follow| follow.submitted(self.preview_generation, id.clone()));
                    self.source_editor_open = false;
                    self.scrolls[Page::New as usize].set_offset(point(px(0.), px(0.)));
                }
                self.select_task(&id, cx);
                // Preparation may finish after navigation; keep the user's current page.
                self.start_next_task(cx);
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("任务尚未加入队列：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

    fn request_for(
        &self,
        task: &TaskRecord,
        secrets: &HashMap<String, String>,
    ) -> Result<course2md::execution::Request> {
        let location = self
            .task_library(task)
            .context(course2md::tr!("任务保存位置暂时不可用"))?;
        workspace::check_library(location)?;
        ensure!(
            task.work_dir == location.root.join(".course2md/work").join(&task.id),
            course2md::tr!("任务位置与笔记库记录不匹配，请先恢复存储位置")
        );
        let refs = ServiceRefs {
            asr: task.plan.asr_service.clone(),
            llm: task.plan.ai_service.clone(),
        };
        let config = self
            .preferences
            .resolve_for_execution_with_secrets(&task.plan.config, &refs, secrets)?
            .into_config();
        let course_id = workspace::course_dir_name(&task.plan.source_id);
        let service_versions = [("asr", refs.asr), ("llm", refs.llm)]
            .into_iter()
            .filter_map(|(purpose, reference)| reference.map(|r| (purpose.to_owned(), r)))
            .collect();
        let source = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.reader_sources.get(&task.plan.source_id))
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
            .filter(|_| {
                !task.plan.source.online && !std::path::Path::new(&task.plan.source.input).is_file()
            })
            .unwrap_or_else(|| task.plan.source.input.clone());
        Ok(course2md::execution::Request {
            schema: 1,
            operation: task.plan.operation.clone(),
            task_id: task.id.clone(),
            version_id: task.id.clone(),
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
                .and_then(|id| self.preferences.version(id))
                .is_some_and(|version| {
                    version.config.authentication == preferences::Authentication::None
                }),
            control_path: Some(task.work_dir.join("control.json")),
            service_versions,
        })
    }

    fn write_task_control(&self, task: &TaskRecord, resend: Vec<String>) -> Result<()> {
        let library = self
            .task_library(task)
            .context(course2md::tr!("任务保存位置尚未恢复"))?;
        workspace::check_library(library)?;
        ensure!(
            task.work_dir == library.root.join(".course2md/work").join(&task.id),
            course2md::tr!("任务工作目录与保存位置不匹配")
        );
        let mut authorizations = task.resend.clone();
        for id in resend {
            if !authorizations.contains(&id) {
                authorizations.push(id);
            }
        }
        let stopped: Vec<_> = [&task.plan.asr_service, &task.plan.ai_service]
            .into_iter()
            .flatten()
            .filter(|id| {
                self.preferences
                    .version(id)
                    .is_none_or(|version| self.preferences.is_service_retired(&version.service_id))
            })
            .cloned()
            .collect();
        std::fs::create_dir_all(&task.work_dir)?;
        course2md::checkpoint::atomic_write(
            &task.work_dir.join("control.json"),
            &serde_json::to_vec(&serde_json::json!({
                "intent": task.intent, "stopped_services": stopped, "resend": authorizations,
            }))?,
        )
    }

    pub fn refresh_dispatch_controls(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self
            .active_task
            .as_deref()
            .and_then(|id| self.workspace.as_ref()?.state.task(id))
            .cloned()
            && let Err(error) = self.write_task_control(&task, Vec::new())
        {
            self.workspace_error = Some(course2md::tr!(
                "停止新请求的指令尚未保存：{}",
                format!("{error:#}")
            ));
            if let Some(job) = &self.job {
                job.cancel();
            }
            cx.notify();
        }
    }

    pub fn start_next_task(&mut self, cx: &mut Context<Self>) {
        if self.job.is_some() || self.closing || self.storage_ui.busy || self.start_pending {
            return;
        }
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.next_task())
            .cloned()
        else {
            return;
        };
        let refs = ServiceRefs {
            asr: task.plan.asr_service.clone(),
            llm: task.plan.ai_service.clone(),
        };
        let needed = match self
            .preferences
            .credential_references_for(&task.plan.config, &refs)
        {
            Ok(needed) => needed,
            Err(error) => {
                self.fail_task_before_start(&task.id, format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        if needed.is_empty() {
            self.finish_task_start(&task.id, HashMap::new(), cx);
            return;
        }
        // Keychain resolution may surface a system authorization dialog whose queue can even
        // be held by another application; resolving on the UI thread would freeze the whole
        // interface, so the secrets are gathered on a background executor first.
        self.start_pending = true;
        let vault = self.preferences.vault();
        let task_id = task.id.clone();
        let secrets_rx = crate::spawn_blocking_io(move || -> Result<HashMap<String, String>> {
            let mut secrets = HashMap::new();
            for reference in needed {
                let secret = vault.resolve(&reference).map(|s| s.expose().to_owned())?;
                secrets.insert(reference, secret);
            }
            Ok(secrets)
        });
        cx.spawn(async move |this, cx| {
            let secrets = secrets_rx
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(course2md::tr!("凭据读取意外中断"))));
            let _ = this.update(cx, |this, cx| {
                this.start_pending = false;
                match secrets {
                    Ok(secrets) => this.finish_task_start(&task_id, secrets, cx),
                    Err(error) => {
                        this.fail_task_before_start(&task_id, format!("{error:#}"));
                        cx.notify();
                    }
                }
                this.start_next_task(cx);
            });
        })
        .detach();
    }

    fn finish_task_start(
        &mut self,
        task_id: &str,
        secrets: HashMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some() || self.closing {
            return;
        }
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.task(task_id))
            .cloned()
        else {
            return;
        };
        // The queue may have changed while credentials were being resolved off-thread.
        if task.state != TaskState::Queued || task.intent != Intent::Run {
            return;
        }
        let request = match self.request_for(&task, &secrets) {
            Ok(request) => request,
            Err(error) => {
                self.fail_task_before_start(&task.id, format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let read_task = task.clone();
        let read = crate::spawn_blocking_io(move || {
            let mut request = request;
            if read_task.plan.options().source_mode != TranscriptSource::Asr
                && read_task.plan.subtitle.is_none()
            {
                request.subtitle_events = workspace::task_subtitle_events(&read_task)?;
            }
            request.validate()?;
            Ok::<_, anyhow::Error>(request)
        });
        let desktop = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let request = match read.recv().await {
                Ok(Ok(request)) => request,
                Ok(Err(error)) => {
                    let _ = desktop.update(cx, |this, cx| {
                        this.fail_task_before_start(&task.id, format!("{error:#}"));
                        cx.notify();
                    });
                    return;
                }
                Err(error) => {
                    let _ = desktop.update(cx, |this, cx| {
                        this.fail_task_before_start(&task.id, error.to_string());
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = desktop.update(cx, |this, cx| {
                this.start_task_request(task, request, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn start_task_request(
        &mut self,
        task: TaskRecord,
        request: course2md::execution::Request,
        cx: &mut Context<Self>,
    ) {
        if self.job.is_some()
            || self.closing
            || task.state != TaskState::Queued
            || task.intent != Intent::Run
        {
            return;
        }
        if let Err(error) = self.write_task_control(&task, Vec::new()) {
            self.fail_task_before_start(&task.id, format!("{error:#}"));
            cx.notify();
            return;
        }
        let running = self
            .workspace
            .as_mut()
            .context(course2md::tr!("任务记录不可用"))
            .and_then(|workspace| {
                workspace.transaction(|state| {
                    let record = state
                        .task_mut(&task.id)
                        .context(course2md::tr!("任务不存在"))?;
                    record.state = TaskState::Running;
                    record.error = None;
                    record.updated = workspace::now();
                    Ok(())
                })
            });
        if let Err(error) = running {
            self.fail_task_before_start(&task.id, format!("{error:#}"));
            cx.notify();
            return;
        }
        // Starting the child and writing the full request to stdin can block on a pipe;
        // keep it off the UI thread.
        let wake = self.event_wake_tx.clone();
        let started = crate::spawn_blocking_io(move || Job::start_task(&request, wake));
        let desktop = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let job = started.recv().await;
            let _ = desktop.update(cx, |this, cx| match job {
                Ok(Ok(job)) => {
                    this.job = Some(job);
                    this.active_task = Some(task.id.clone());
                    this.kind = Kind::Convert;
                    this.cancelling = false;
                    this.progress.clear();
                    this.token_totals = (0, 0);
                    this.job_started = Some(std::time::Instant::now());
                    this.logs.clear();
                    this.pending_done = None;
                    this.task_error = None;
                    this.task_status = course2md::tr!("正在生成《{}》", task.plan.title);
                    cx.notify();
                }
                Ok(Err(error)) => {
                    this.fail_task_before_start(&task.id, format!("{error:#}"));
                    cx.notify();
                }
                Err(_) => {
                    this.fail_task_before_start(&task.id, crate::interrupted().to_owned());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn fail_task_before_start(&mut self, task_id: &str, message: String) {
        if let Some(workspace) = &mut self.workspace {
            let compensated = workspace.transaction(|state| {
                let record = state
                    .task_mut(task_id)
                    .context(course2md::tr!("任务不存在"))?;
                record.state = TaskState::NeedsAttention;
                record.error = Some(message.clone());
                record.unread = true;
                Ok(())
            });
            if let Err(error) = compensated {
                // Even if recording the compensation fails, the task must not stay Running and be ignored by the
                // scheduler: mark it NeedsAttention in memory too; recover() reconciles with the disk on restart.
                if let Some(record) = workspace.state.task_mut(task_id) {
                    record.state = TaskState::NeedsAttention;
                    record.error = Some(message);
                    record.unread = true;
                }
                self.workspace_error =
                    Some(course2md::tr!("任务状态尚未保存：{}", format!("{error:#}")));
            }
        }
    }

    pub fn set_task_intent(&mut self, id: String, intent: Intent, cx: &mut Context<Self>) {
        let active = self.is_running_task(&id);
        if active && intent == Intent::Run {
            self.message = Some(course2md::tr!("当前任务还在处理，请等候当前操作结束。").into());
            cx.notify();
            return;
        }
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        match workspace.transaction(|state| {
            state.set_intent(&id, intent)?;
            if active && intent != Intent::Run {
                state
                    .task_mut(&id)
                    .context(course2md::tr!("任务记录不存在"))?
                    .state = TaskState::Pausing;
            }
            Ok(())
        }) {
            Ok(()) => {
                if intent == Intent::Run {
                    self.follow_resumed_input_task(&id, cx);
                } else if intent == Intent::Cancel {
                    self.following_conversion = None;
                }
                if let Some(task) = self
                    .workspace
                    .as_ref()
                    .and_then(|w| w.state.task(&id))
                    .cloned()
                    && let Err(error) = self.write_task_control(&task, Vec::new())
                {
                    self.workspace_error =
                        Some(course2md::tr!("任务指令尚未保存：{}", format!("{error:#}")));
                    if self.active_task.as_ref() == Some(&id)
                        && let Some(job) = &self.job
                    {
                        job.cancel();
                    }
                }
                self.start_next_task(cx);
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("任务状态尚未保存：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

    pub fn select_task(&mut self, id: &str, cx: &mut Context<Self>) {
        let previous = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.selected_task.clone());
        if previous.as_deref() != Some(id)
            && let Some(previous) = previous
        {
            self.clear_task_card_motion(&previous);
        }
        if let Some(workspace) = &mut self.workspace
            && let Err(error) = workspace.transaction(|state| {
                state.selected_task = Some(id.to_owned());
                if let Some(task) = state.task_mut(id) {
                    task.unread = false;
                }
                Ok(())
            })
        {
            self.workspace_error =
                Some(course2md::tr!("任务选择尚未保存：{}", format!("{error:#}")));
        }
        // Going to a task opens its details when there is something to read
        // or decide; new and running work stays a compact row.
        let waiting = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(id))
            .is_some_and(|task| matches!(task_group(task), TaskGroup::Running | TaskGroup::Queued));
        self.expanded_task = (!waiting).then(|| id.to_owned());
        cx.notify();
    }

    fn follow_resumed_input_task(&mut self, id: &str, cx: &App) {
        if self.page == Page::New
            && self
                .current_input_task(cx)
                .is_some_and(|task| task.id == id)
        {
            self.following_conversion = Some(crate::import_ui::ConversionFollow::Task {
                id: id.to_owned(),
                source_revision: self.preview_generation,
            });
        } else if self.page == Page::Result && !self.reading {
            self.following_conversion = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.task(id))
                .zip(self.preview.as_ref())
                .and_then(|(task, preview)| {
                    crate::import_ui::ConversionFollow::reader_reprocess(
                        task,
                        &preview.course.dir,
                        self.read_generation,
                    )
                });
        }
    }

    pub fn resend_uncertain(&mut self, id: String, cx: &mut Context<Self>) {
        if self.is_running_task(&id) {
            self.message = Some(
                course2md::tr!("正在保存当前结果，请等候当前任务停止后再选择重新发送。").into(),
            );
            cx.notify();
            return;
        }
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.task(&id))
            .cloned()
        else {
            return;
        };
        if let Some(followup) = &task.handled_by {
            self.select_task(followup, cx);
            return;
        }
        let requests: Vec<_> = task
            .blocked
            .iter()
            .filter(|b| b.is_uncertain())
            .filter_map(|b| b.request_id.clone())
            .collect();
        if requests.is_empty() {
            return;
        }
        if task.artifact.is_some() {
            let components = task
                .blocked
                .iter()
                .filter(|b| b.is_uncertain())
                .filter_map(|b| b.purpose.as_deref())
                .map(|purpose| ai_component(purpose).unwrap_or(Component::Proofreading))
                .collect();
            self.reprocess_task(id, components, requests, cx);
            return;
        }
        if let Some(workspace) = &mut self.workspace {
            let result = workspace.transaction(|state| state.authorize_uncertain(&id, &requests));
            if let Err(error) = result {
                self.workspace_error = Some(course2md::tr!(
                    "重新发送的选择尚未保存：{}",
                    format!("{error:#}")
                ));
                return;
            }
        }
        self.follow_resumed_input_task(&id, cx);
        self.start_next_task(cx);
        cx.notify();
    }

    pub fn reprocess_task(
        &mut self,
        id: String,
        mut components: Vec<Component>,
        resend: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.is_running_task(&id) {
            self.message =
                Some(course2md::tr!("正在保存当前结果，请等候当前任务停止后再补做。").into());
            cx.notify();
            return;
        }
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        components.sort();
        components.dedup();
        let result = workspace.transaction(|state| state.reprocess(&id, components, resend));
        match result {
            Ok(id) => {
                self.select_task(&id, cx);
                self.follow_resumed_input_task(&id, cx);
                self.start_next_task(cx);
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("补做任务尚未建立：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }

    pub(super) fn repair_task_service(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
        {
            if let Some(next) = task.handled_by.clone() {
                self.select_task(&next, cx);
                self.navigate(Page::Task, cx);
                return;
            }
            if task.has_uncertain() {
                self.select_task(&id, cx);
                self.navigate(Page::Task, cx);
                return;
            }
        }
        let components = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
            .and_then(|task| task.artifact.as_ref().map(|_| task))
            .map(|task| {
                task_component_failures(task)
                    .into_iter()
                    .filter_map(|(component, _, _)| Component::parse(&component))
                    .filter(|component| ai_component(component.as_str()).is_some())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if components.is_empty() {
            self.message = Some(course2md::tr!("这份笔记没有需要修复的校对或摘要").into());
            cx.notify();
            return;
        }
        self.open_reprocess_service_editor(id, components, window, cx);
    }

    pub(crate) fn reprocess_task_with_service(
        &mut self,
        id: String,
        components: Vec<Component>,
        ai_service: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = (|| -> Result<String> {
            ensure!(
                !self.is_running_task(&id),
                course2md::tr!("正在保存当前结果，请等待任务停止后再补做")
            );
            let task = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.task(&id))
                .context(course2md::tr!("原任务记录暂时不可用"))?;
            let mut base = task.plan.config.clone();
            base.defaults.transcript_source = Some(course2md::config::TranscriptSource::Subtitle);
            base.llm.enabled = components.contains(&Component::Proofreading);
            base.llm.summarize = components.contains(&Component::Summary);
            // Translation follows the proofread paragraphs it is redone with.
            if !base.llm.enabled {
                base.llm.translate = None;
            }
            let config = self.preferences.config_for_refs(
                &base,
                &ServiceRefs {
                    asr: None,
                    llm: Some(ai_service.clone()),
                },
            )?;
            self.workspace
                .as_mut()
                .context(course2md::tr!("任务记录暂时不可用"))?
                .transaction(|state| {
                    state.reprocess_with_service(
                        &id,
                        components,
                        Vec::new(),
                        Some((ai_service, config)),
                    )
                })
        })();
        match result {
            Ok(next) => {
                self.workspace_error = None;
                self.select_task(&next, cx);
                self.follow_resumed_input_task(&next, cx);
                self.start_next_task(cx);
                cx.notify();
                true
            }
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "服务已保存，补做任务尚未建立：{}",
                    format!("{error:#}")
                ));
                cx.notify();
                false
            }
        }
    }

    pub fn adjust_task(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running_task(&id) {
            self.message =
                Some(course2md::tr!("当前任务还在处理，请先暂停并等候当前步骤结束。").into());
            cx.notify();
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
            .cloned()
        else {
            return;
        };
        let events =
            crate::spawn_blocking_io(move || workspace::task_subtitle_events(&task).ok().flatten());
        cx.spawn_in(window, async move |this, cx| {
            let events = events.recv().await.ok().flatten();
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(workspace) = &mut this.workspace else {
                    return;
                };
                match workspace.transaction(|state| state.adjust_task(&id, events)) {
                    Ok(()) => {
                        this.following_conversion = None;
                        this.invalidate_source();
                        this.restore_draft(window, cx);
                        this.page = Page::New;
                    }
                    Err(error) => {
                        this.workspace_error = Some(course2md::tr!(
                            "尚未打开任务调整选项：{}",
                            format!("{error:#}")
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn record_task_event(&mut self, event: &EngineEvent) {
        let Some(id) = &self.active_task else {
            return;
        };
        let Some(task) = self.workspace.as_mut().and_then(|w| w.state.task_mut(id)) else {
            return;
        };
        // High-frequency log/progress events keep the persisted timestamp;
        // only state the user would act on moves it (and rewrites the mirror).
        match event {
            EngineEvent::Log { message, .. } => {
                task.logs.push(message.clone());
                if task.logs.len() > 200 {
                    task.logs.drain(..task.logs.len() - 200);
                }
            }
            EngineEvent::Stage { stage, status } => {
                task.updated = workspace::now();
                if matches!(stage, Stage::ScenesScan | Stage::ScenesExtract) {
                    // Old workers combined scanning and extraction under this
                    // key. Do not retain its completed counter on continuation.
                    task.stages.remove(&Stage::Scenes);
                    if *stage == Stage::ScenesScan && *status == EngineStageStatus::Start {
                        task.stages.remove(&Stage::ScenesExtract);
                    }
                }
                let value = task.stages.entry(stage.clone()).or_default();
                match status {
                    EngineStageStatus::Start => value.begin(),
                    EngineStageStatus::Done => value.status = EngineStageStatus::Done,
                }
            }
            EngineEvent::Progress {
                stage,
                current,
                total,
                message,
                phase,
                ..
            } => {
                let value = task.stages.entry(stage.clone()).or_default();
                value.current = *current;
                value.total = *total;
                value.detail = message.clone().or_else(|| {
                    phase.map(|phase| crate::activity::model_transfer_phase(stage, Some(phase)))
                });
            }
            EngineEvent::Error { message, .. } => {
                task.updated = workspace::now();
                task.error = Some(message.clone());
            }
            EngineEvent::Blocked {
                reason,
                request_id,
                purpose,
                description,
                kind,
                message,
            } => {
                task.updated = workspace::now();
                if *reason == BlockReason::Uncertain {
                    task.state = TaskState::Uncertain;
                }
                let blocked = workspace::BlockedRequest {
                    reason: *reason,
                    request_id: request_id.clone(),
                    purpose: purpose.clone(),
                    description: description.clone().unwrap_or_default(),
                    kind: Some(*kind),
                    message: message.clone(),
                };
                if !task.blocked.contains(&blocked) {
                    task.blocked.push(blocked);
                }
                task.error = Some(message.clone());
            }
            _ => {}
        }
    }

    pub fn finish_task(
        &mut self,
        id: &str,
        success: bool,
        cancelled: bool,
        cx: &mut Context<Self>,
    ) {
        let done = self.pending_done.take();
        let manifest = done.as_ref().and_then(|d| {
            course2md::artifact::read_manifest(&d.out_dir.join("manifest.json")).ok()
        });
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let visible =
            self.page == Page::Task && workspace.state.selected_task.as_deref() == Some(id);
        let result = workspace.transaction(|state| {
            let task = state
                .task_mut(id)
                .context(course2md::tr!("任务记录不存在"))?;
            task.updated = workspace::now();
            task.unread = !visible;
            task.outcomes = done
                .as_ref()
                .and_then(|done| done.outcomes.clone())
                .or_else(|| manifest.as_ref().map(|manifest| manifest.outcomes.clone()));
            workspace::reconcile_receipts(task)?;
            if let (Some(done), Some(manifest)) = (&done, &manifest) {
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
                    task.error = None;
                }
            } else {
                task.state = match task.intent {
                    Intent::Cancel => TaskState::Cancelled,
                    Intent::Pause | Intent::Quit => TaskState::Paused,
                    Intent::Run if task.has_uncertain() => TaskState::Uncertain,
                    Intent::Run => TaskState::NeedsAttention,
                };
                if task.error.is_none() && !cancelled && task.intent == Intent::Run {
                    task.error = Some(
                        if success {
                            course2md::tr!("未生成可读笔记")
                        } else {
                            course2md::tr!("生成中断")
                        }
                        .into(),
                    );
                }
            }
            let record = task.clone();
            if record.state == TaskState::Complete && state.selected_task.as_deref() == Some(id) {
                state.selected_task = None;
            }
            Ok(record)
        });
        match result {
            Ok(task) => {
                if self.reader_viewer_open() {
                    crate::import_ui::ConversionFollow::interrupt_for_reader_viewer(
                        &mut self.following_conversion,
                    );
                }
                let follow_input_completion = self
                    .following_conversion
                    .as_ref()
                    .zip(self.workspace.as_ref())
                    .and_then(|(follow, workspace)| {
                        follow.completed_task(
                            self.page == Page::New,
                            self.preview_generation,
                            workspace.state.draft()?,
                            &workspace.state.tasks,
                            &self.value(Field::Source, cx),
                        )
                    })
                    .is_some_and(|task| task.id == id);
                let follow_reader_completion = !self.reading
                    && self.following_conversion.as_ref().is_some_and(|follow| {
                        follow.completed_reader_task(
                            &task,
                            self.page,
                            self.read_generation,
                            self.preview
                                .as_ref()
                                .map(|preview| preview.course.dir.as_path()),
                        )
                    });
                if let Some(done) = done {
                    self.completed = Some(done.clone());
                    let root = self.task_library(&task).map(|l| l.root.clone());
                    let open = !task.exports_only()
                        && (follow_input_completion || follow_reader_completion);
                    let source = task.plan.source.clone();
                    let folder = task.plan.folder;
                    let out_dir = done.out_dir.clone();
                    let storage_dir = Course::from_completed(&done).storage_dir();
                    let frames = manifest
                        .as_ref()
                        .map(|manifest| manifest.frames.clone())
                        .unwrap_or_default();
                    // Cover copy, folder assignment and thumbnail lookup touch the
                    // library disk; the record above is already committed.
                    let published = crate::spawn_blocking_io(move || {
                        let mut issues = Vec::new();
                        if let Some(root) = &root {
                            if let Err(error) = source::save_cover(&source, &out_dir) {
                                issues
                                    .push(course2md::tr!("封面暂未保存：{}", format!("{error:#}")));
                            }
                            if let Err(error) = organize::Library::edit(root, |library| {
                                library.assign(root, &storage_dir, folder)
                            }) {
                                issues.push(course2md::tr!(
                                    "文件夹归属暂未更新：{}",
                                    format!("{error:#}")
                                ));
                            }
                        }
                        let cover = out_dir.join("cover.jpg");
                        let thumbnail = cover.is_file().then_some(cover).or_else(|| {
                            frames.iter().find_map(|frame| {
                                course2md::artifact::safe_asset_path(&out_dir, &frame.image).ok()
                            })
                        });
                        (issues, thumbnail)
                    });
                    cx.spawn(async move |this, cx| {
                        let Ok((issues, thumbnail)) = published.recv().await else {
                            return;
                        };
                        let _ = this.update(cx, |this, cx| {
                            if !issues.is_empty() {
                                this.message = Some(course2md::tr!(
                                    "笔记已保存，{}",
                                    issues.join(course2md::tr!("；"))
                                ));
                            }
                            if let Some(manifest) = &manifest {
                                update_published_course(
                                    &mut this.courses,
                                    &done,
                                    manifest,
                                    thumbnail,
                                );
                            }
                            this.refresh_library(cx);
                            if open {
                                this.open_completed_conversion(Course::from_completed(&done), cx);
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
                self.task_status = task.status_label().into();
                self.transient_task_result = Some((id.to_owned(), std::time::Instant::now()));
                self.schedule_wake(std::time::Duration::from_secs(8));
            }
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "完成状态尚未保存：{}。已发布的笔记仍保留。",
                    format!("{error:#}")
                ))
            }
        }
        cx.notify();
    }

    pub(crate) fn open_task_export_location(&mut self, id: String, cx: &mut Context<Self>) {
        let destination = (|| {
            let state = &self
                .workspace
                .as_ref()
                .context(course2md::tr!("任务记录暂时不可用"))?
                .state;
            let task = state.task(&id).context(course2md::tr!("任务记录不存在"))?;
            let location = state
                .library(&task.plan.library_id)
                .context(course2md::tr!("任务保存位置暂时不可用"))?;
            workspace::available_task_export(task, location)
        })();
        match destination {
            Ok(path) => cx.reveal_path(&path),
            Err(error) => {
                self.message = Some(course2md::tr!("无法打开导出位置：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }
}

/// A batch task's note title: the episode title shown in the candidate row; a bare link falls back to
/// 「合集 视频 N」 (the collection name comes from the probe; placeholder hints never enter a title).
fn batch_note_title(
    candidate: &source::SourceCandidate,
    collection: Option<&str>,
    index: usize,
) -> String {
    let title = candidate.title.trim();
    if !title.is_empty() && title != candidate.input.trim() {
        return title.to_owned();
    }
    match collection.map(str::trim).filter(|title| !title.is_empty()) {
        Some(collection) => course2md::tr!("{} 视频 {}", collection, index + 1),
        None => course2md::tr!("视频 {}", index + 1),
    }
}

/// Local recognition check; build_plan and build_batch_plan share its exact messages.
fn ensure_asr_capability(
    environment: &crate::backend::Environment,
    provider: course2md::config::AsrProvider,
) -> Result<()> {
    use course2md::config::AsrProvider;
    match provider {
        AsrProvider::Coreml => ensure!(
            environment.apple,
            course2md::tr!("Apple 识别组件不可用，请选择其他识别方式")
        ),
        AsrProvider::Gpu => ensure!(
            environment.llama && environment.gpu.is_some(),
            course2md::tr!("没有检测到可用的 GPU 识别引擎。请选择 CPU 或其他本机识别方式。")
        ),
        AsrProvider::Cpu => ensure!(
            environment.llama,
            course2md::tr!("CPU 识别引擎尚未安装。请在设置的「应用」中查看安装方法。")
        ),
        AsrProvider::Npu => ensure!(
            environment.npu(),
            course2md::tr!("没有检测到可用的 Intel NPU 识别环境。请选择其他本机识别方式。")
        ),
        AsrProvider::Api => (),
    }
    Ok(())
}

/// Without an explicit model, fill in the default for the recognition engine.
/// import_ui's import_model_request and both plan builders share this rule.
pub(crate) fn fill_default_asr_model(
    config: &mut course2md::settings::ConfigFile,
    provider: course2md::config::AsrProvider,
) {
    use course2md::config::AsrProvider;
    if provider != AsrProvider::Api
        && config
            .defaults
            .asr_model
            .as_deref()
            .is_none_or(|model| model.trim().is_empty())
    {
        config.defaults.asr_model = Some(course2md::config::default_model(provider).into());
    }
}

fn validate_plan_config(source: &str, config: &course2md::settings::ConfigFile) -> Result<()> {
    let resolved = course2md::options::resolve(source.to_owned(), &Default::default(), config)?;
    resolved.validate()?;
    if resolved.transcript_source != course2md::config::TranscriptSource::Subtitle {
        // Credentials belong to the service vault, not this static model/provider check.
        resolved.validate_asr_with_auth(false, false)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        PlanValidation, apply_draft_options, batch_note_title, edit_current_draft,
        update_draft_source_title, update_input_form, validate_plan_config, validate_plan_storage,
    };

    #[test]
    fn a_changed_option_is_the_draft_without_a_separate_save() {
        use crate::workspace::{Override, Workspace};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("workspace.json");
        let root = dir.path().join("library");
        let mut workspace =
            Workspace::open_at_inline(file.clone(), root.clone(), Default::default()).unwrap();
        let updated = workspace.state.draft().unwrap().updated;
        let keep_video = !workspace.state.draft().unwrap().options.keep_video;

        // Re-applying the same options neither writes nor counts as an override.
        let same = workspace.state.draft().unwrap().options.clone();
        assert!(!edit_current_draft(&mut workspace, |d| apply_draft_options(d, same)).unwrap());
        assert!(workspace.state.draft().unwrap().overrides.is_empty());

        let wrote = edit_current_draft(&mut workspace, |draft| {
            let mut options = draft.options.clone();
            options.keep_video = keep_video;
            apply_draft_options(draft, options);
        })
        .unwrap();
        assert!(wrote);
        let draft = workspace.state.draft().unwrap();
        assert_eq!(draft.options.keep_video, keep_video);
        assert!(draft.overrides.contains(&Override::KeepVideo));
        assert!(draft.updated >= updated);
        workspace.save().unwrap();
        drop(workspace);

        let reopened = Workspace::open_at_inline(file, root, Default::default()).unwrap();
        let draft = reopened.state.draft().unwrap();
        assert_eq!(draft.options.keep_video, keep_video);
        assert!(draft.overrides.contains(&Override::KeepVideo));
    }

    #[test]
    fn batch_titles_come_from_candidate_rows() {
        let candidate = crate::source::SourceCandidate {
            input: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=2".into(),
            title: "视频与图书介绍".into(),
            identity: None,
            duration: None,
            thumbnail: None,
        };
        assert_eq!(
            batch_note_title(&candidate, Some("合集"), 1),
            "视频与图书介绍"
        );
        // A bare link candidate falls back to 「合集 视频 N」, as its list row shows it.
        let bare = crate::source::SourceCandidate {
            title: candidate.input.clone(),
            ..candidate.clone()
        };
        assert_eq!(
            batch_note_title(&bare, Some("《高等数学》"), 1),
            "《高等数学》 视频 2"
        );
        assert_eq!(batch_note_title(&bare, None, 0), "视频 1");
        let blank = crate::source::SourceCandidate {
            title: "  ".into(),
            ..candidate.clone()
        };
        assert_eq!(
            batch_note_title(&blank, Some("《高等数学》"), 4),
            "《高等数学》 视频 5"
        );
    }

    #[test]
    fn a_published_repair_replaces_the_stale_card_before_a_library_scan() {
        use crate::{backend::Done as Completed, notes::Course};
        use course2md::artifact::{Manifest, Outcome, Outcomes};
        use std::{
            path::PathBuf,
            time::{Duration, UNIX_EPOCH},
        };

        let outcomes = Outcomes {
            transcript: Outcome::succeeded(),
            proofreading: Outcome::failed("服务拒绝凭据"),
            ..Default::default()
        };
        let old_manifest = Manifest {
            schema: 1,
            task_id: "old".into(),
            course_id: "course".into(),
            source_id: "same-source".into(),
            version_id: "old".into(),
            title: "原始课程名".into(),
            created_at_ms: 1_000,
            revision: 1,
            document: "document.json".into(),
            markdown: "course.md".into(),
            frames: Vec::new(),
            assets: Vec::new(),
            outputs: Vec::new(),
            outcomes,
            partial: true,
        };
        let old = Course {
            dir: "/library/course/versions/old".into(),
            title: "用户命名".into(),
            modified: UNIX_EPOCH + Duration::from_secs(1),
            slides: 0,
            thumbnail: Some("/old-cover.jpg".into()),
            manifest: Some(old_manifest.clone()),
            warning: None,
        };
        let mut other_library = old.clone();
        other_library.dir = "/other-library/course/versions/old".into();
        let mut courses = vec![
            std::rc::Rc::new(old.clone()),
            std::rc::Rc::new(other_library.clone()),
        ];
        let mut repaired = old_manifest;
        repaired.task_id = "repair".into();
        repaired.version_id = "repair".into();
        repaired.revision = 2;
        repaired.created_at_ms = 2_000;
        repaired.partial = false;
        repaired.outcomes.proofreading = Outcome::succeeded();
        let done = Completed {
            out_dir: "/library/course/versions/repair".into(),
            title: repaired.title.clone(),
            ..Default::default()
        };
        super::update_published_course(
            &mut courses,
            &done,
            &repaired,
            Some(PathBuf::from("/new-cover.jpg")),
        );
        assert_eq!(courses.len(), 2);
        let current = &courses[0];
        assert_eq!(current.dir, done.out_dir);
        assert_eq!(current.title, "用户命名");
        assert_eq!(
            current.thumbnail.as_deref(),
            Some(std::path::Path::new("/new-cover.jpg"))
        );
        assert!(!current.incomplete());
        assert_eq!(current.manifest.as_ref().unwrap().version_id, "repair");
        assert_eq!(courses[1].dir, other_library.dir);
        assert!(courses[1].incomplete());
        assert!(old.manifest.as_ref().unwrap().partial);
    }

    #[test]
    fn unchanged_form_values_do_not_request_a_save_or_update_the_timestamp() {
        let mut input = crate::workspace::Draft::new(true, "library".into(), Default::default());
        let source = source("https://example.test/a", "课程 A");
        update_draft_source_title(
            &mut input,
            source.input.clone(),
            source.title.clone(),
            Some(source),
        );
        input.updated = 123;
        input.folder = Some(7);
        input.scroll = -42.;
        let expected = input.clone();
        for _ in 0..8 {
            assert!(!update_input_form(
                &mut input,
                expected.input.clone(),
                expected.title.clone(),
                expected.source.clone(),
                expected.options.clone(),
                expected.folder,
                expected.scroll,
            ));
            assert_eq!(input, expected);
        }
        let mut options = expected.options.clone();
        options.formats[0] = !options.formats[0];
        assert!(update_input_form(
            &mut input,
            expected.input,
            expected.title,
            expected.source,
            options.clone(),
            expected.folder,
            expected.scroll,
        ));
        assert_eq!(input.options, options);
        assert!(
            input
                .overrides
                .contains(&crate::workspace::Override::Formats)
        );
        assert_ne!(input.updated, 123);

        let input_text = input.input.clone();
        let title = input.title.clone();
        let source_value = input.source.clone();
        let folder = input.folder;
        let scroll = input.scroll;
        let mut translated = input.options.clone();
        translated.translate = Some("en".into());
        assert!(update_input_form(
            &mut input,
            input_text,
            title,
            source_value,
            translated,
            folder,
            scroll,
        ));
        assert_eq!(input.options.translate.as_deref(), Some("en"));
        assert!(
            input
                .overrides
                .contains(&crate::workspace::Override::Translate)
        );
    }

    #[test]
    fn temporary_speech_service_keeps_the_explicit_local_engine_for_the_same_input() {
        let mut draft = crate::workspace::Draft::new(true, "library".into(), Default::default());
        draft.change_source("video".into());
        let mut options = draft.options.clone();
        options.provider = Some(course2md::config::AsrProvider::Cpu);
        update_input_form(
            &mut draft,
            "video".into(),
            "".into(),
            None,
            options.clone(),
            None,
            0.,
        );
        assert_eq!(
            draft.local_provider,
            Some(course2md::config::AsrProvider::Cpu)
        );
        options.provider = Some(course2md::config::AsrProvider::Api);
        update_input_form(
            &mut draft,
            "video".into(),
            "".into(),
            None,
            options,
            None,
            0.,
        );
        assert_eq!(
            draft.options.provider,
            Some(course2md::config::AsrProvider::Api)
        );
        assert_eq!(
            draft.local_provider,
            Some(course2md::config::AsrProvider::Cpu)
        );
        let next = crate::workspace::Draft::new(true, "library".into(), Default::default());
        assert_eq!(next.local_provider, None);
    }

    #[test]
    fn plan_preview_uses_loaded_storage_but_submission_rechecks_identity_and_folders() {
        let directory = tempfile::tempdir().unwrap();
        let location = crate::workspace::LibraryLocation {
            id: "library".into(),
            name: "笔记库".into(),
            root: directory.path().to_owned(),
            previous_roots: Vec::new(),
        };
        let cached = crate::storage_ui::LocationCheck {
            available: true,
            needs_reassociation: false,
            problem: None,
        };
        let mut index = crate::organize::Library::default();
        index.folders.insert(7, "复习".into());
        std::fs::write(
            location.root.join(".course2md-library-id"),
            "different-library",
        )
        .unwrap();
        validate_plan_storage(
            PlanValidation::Preview,
            &location,
            Some(7),
            Some(&cached),
            Some(&index),
        )
        .unwrap();
        assert!(
            validate_plan_storage(
                PlanValidation::Submission,
                &location,
                Some(7),
                Some(&cached),
                Some(&index)
            )
            .is_err()
        );
        std::fs::write(location.root.join(".course2md-library-id"), &location.id).unwrap();
        validate_plan_storage(
            PlanValidation::Submission,
            &location,
            None,
            Some(&cached),
            Some(&index),
        )
        .unwrap();
        assert!(
            validate_plan_storage(
                PlanValidation::Submission,
                &location,
                Some(7),
                Some(&cached),
                Some(&index)
            )
            .is_err(),
            "a removed folder cannot be authorized by an old preview"
        );
        let waiting = validate_plan_storage(PlanValidation::Preview, &location, None, None, None)
            .unwrap_err();
        assert!(waiting.to_string().contains("正在检查"));
        assert!(
            waiting.downcast_ref::<super::PlanPending>().is_some(),
            "an unfinished location check is waiting, not a failure"
        );
    }

    #[test]
    fn changing_source_keeps_automatic_title_provenance_and_frozen_task() {
        use crate::workspace::{TaskPlan, Workspace};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("workspace.json");
        let root = dir.path().join("library");
        let mut workspace =
            Workspace::open_at_inline(file.clone(), root.clone(), Default::default()).unwrap();
        let a = source("https://example.test/video-a", "课程 A");
        let b = source("https://example.test/video-b", "课程 B");
        let id = workspace
            .transaction(|state| {
                update_draft_source_title(
                    state.draft_mut().unwrap(),
                    a.input.clone(),
                    a.title.clone(),
                    Some(a.clone()),
                );
                state.enqueue(
                    TaskPlan {
                        operation: Default::default(),
                        source: a.clone(),
                        source_id: a.identity.clone(),
                        title: a.title.clone(),
                        library_id: state.default_library.clone(),
                        folder: None,
                        subtitle: None,
                        config: Default::default(),
                        asr_service: None,
                        ai_service: None,
                    },
                    None,
                )
            })
            .unwrap()
            .0;
        // The visible title input still contains A until B's metadata arrives.
        // More than one autosave, and a restart in between, must not make it custom.
        for _ in 0..3 {
            workspace
                .transaction(|state| {
                    update_draft_source_title(
                        state.draft_mut().unwrap(),
                        b.input.clone(),
                        a.title.clone(),
                        None,
                    );
                    Ok(())
                })
                .unwrap();
        }
        workspace =
            Workspace::open_at_inline(file.clone(), root.clone(), Default::default()).unwrap();
        assert!(!workspace.state.draft().unwrap().custom_title);
        assert!(workspace.state.draft().unwrap().source.is_none());
        workspace
            .transaction(|state| {
                update_draft_source_title(
                    state.draft_mut().unwrap(),
                    b.input.clone(),
                    b.title.clone(),
                    Some(b.clone()),
                );
                Ok(())
            })
            .unwrap();
        let restored = Workspace::open_at_inline(file, root, Default::default()).unwrap();
        let draft = restored.state.draft().unwrap();
        assert_eq!(draft.title, "课程 B");
        assert!(!draft.custom_title);
        assert_eq!(draft.source.as_ref().unwrap(), &b);
        let task = restored.state.task(&id).unwrap();
        assert_eq!(task.plan.source, a);
        assert_eq!(task.plan.title, "课程 A");
    }

    #[test]
    fn manual_title_survives_a_source_change_and_late_metadata() {
        let mut draft = crate::workspace::Draft::new(true, "library".into(), Default::default());
        let a = source("https://example.test/a", "课程 A");
        let b = source("https://example.test/b", "课程 B");
        update_draft_source_title(&mut draft, a.input.clone(), a.title.clone(), Some(a));
        // Source and title may both change before the autosave deadline.
        update_draft_source_title(&mut draft, b.input.clone(), "我的复习笔记".into(), None);
        assert!(draft.custom_title);
        let revision = draft.revision;
        assert!(crate::workspace::accept_source(&mut draft, revision, b));
        assert_eq!(draft.title, "我的复习笔记");
        assert!(draft.custom_title);
    }

    #[test]
    fn submit_validates_the_selected_model_only_when_speech_recognition_is_needed() {
        use course2md::config::{AsrProvider, TranscriptSource};
        let mut config = course2md::settings::ConfigFile::default();
        config.defaults.provider = Some(AsrProvider::Cpu);
        config.defaults.asr_model = Some("whisper".into());
        config.defaults.transcript_source = Some(TranscriptSource::Asr);
        assert!(validate_plan_config("video.mp4", &config).is_err());
        config.defaults.transcript_source = Some(TranscriptSource::Subtitle);
        validate_plan_config("video.mp4", &config).unwrap();
        config.defaults.provider = Some(AsrProvider::Coreml);
        config.defaults.asr_model = Some("qwen3-0.6b".into());
        config.defaults.transcript_source = Some(TranscriptSource::Asr);
        validate_plan_config("video.mp4", &config).unwrap();
    }

    pub(crate) fn source(input: &str, title: &str) -> crate::source::Source {
        crate::source::Source {
            input: input.into(),
            title: title.into(),
            identity: format!("online:{input}"),
            online: true,
            ..Default::default()
        }
    }
}
