//! What a task plan is built from, and the checks it has to pass.
use crate::{backend::Done, import::ImportSession, source};
use anyhow::{Context as _, Result, ensure};
use c2m_config::config::{AsrProvider, TranscriptSource};
use desktop_prefs::{
    conversion::ConversionOptions,
    environment::Environment,
    preferences::{self, ServiceRefs},
};
use desktop_store::{
    library::LibraryState,
    locations::{LocationCheck, LocationChecks},
    notes::Course,
    organize,
    workspace::{self, TaskPlan},
};
use std::{path::PathBuf, rc::Rc};

/// A stable validation identity for the missing-title case. Rendering compares
/// the typed error instead of the displayed sentence, which changes with the
/// interface language.
#[derive(Debug)]
pub struct TitleRequired;

impl std::fmt::Display for TitleRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(c2m_i18n::tr!("请填写笔记名称"))
    }
}

impl std::error::Error for TitleRequired {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PlanValidation {
    Preview,
    Submission,
}

/// A plan check still waiting for background work (a location check, a
/// folder index); the workbench shows it as progress, not as a failure.
#[derive(Debug)]
pub struct PlanPending(pub &'static str);

impl std::fmt::Display for PlanPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for PlanPending {}

/// The finished component check, or the wait for it.
pub(crate) fn plan_environment(environment: Option<&Environment>) -> Result<&Environment> {
    Ok(environment.ok_or(PlanPending(c2m_i18n::tr!("正在检查所需组件…")))?)
}

pub(crate) fn validate_plan_storage(
    validation: PlanValidation,
    library: &workspace::LibraryLocation,
    folder: Option<u64>,
    cached: Option<&LocationCheck>,
    index: Option<&organize::Library>,
) -> Result<()> {
    let folder_exists = match validation {
        PlanValidation::Preview => {
            let check = cached.ok_or(PlanPending(c2m_i18n::tr!("正在检查保存位置，请稍候")))?;
            ensure!(
                check.available,
                c2m_i18n::tr!("保存位置不可访问，请连接磁盘或选择其他位置")
            );
            ensure!(
                !check.needs_reassociation,
                c2m_i18n::tr!("保存位置需要重新关联")
            );
            if let Some(problem) = &check.problem {
                anyhow::bail!("{problem}");
            }
            match folder {
                Some(folder) => index
                    .ok_or(PlanPending(c2m_i18n::tr!("正在读取文件夹信息，请稍候")))?
                    .folders
                    .contains_key(&folder),
                None => true,
            }
        }
        PlanValidation::Submission => {
            ensure!(
                library.root.is_dir(),
                c2m_i18n::tr!("保存位置不可访问，请连接磁盘或选择其他位置")
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
        c2m_i18n::tr!("文件夹已删除。请选择其他文件夹，或保存到未分类。")
    );
    Ok(())
}

pub fn update_draft_source_title(
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

/// Replace the draft's processing options, recording each changed choice as an
/// explicit override so later default changes no longer reach it.
pub fn apply_draft_options(draft: &mut workspace::Draft, options: ConversionOptions) {
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
pub fn edit_current_draft(
    workspace: &mut workspace::Workspace,
    edit: impl FnOnce(&mut workspace::Draft),
) -> Result<bool> {
    let current = workspace
        .state
        .draft()
        .context(c2m_i18n::tr!("当前输入记录暂时不可用"))?;
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
            .context(c2m_i18n::tr!("没有可保存的输入"))? = next;
        Ok(())
    })?;
    Ok(true)
}

/// A batch task's note title: the episode title shown in the candidate row; a bare link falls back to
/// 「合集 视频 N」 (the collection name comes from the probe; placeholder hints never enter a title).
pub fn batch_note_title(
    candidate: &source::SourceCandidate,
    collection: Option<&str>,
    index: usize,
) -> String {
    let title = candidate.title.trim();
    if !title.is_empty() && title != candidate.input.trim() {
        return title.to_owned();
    }
    match collection.map(str::trim).filter(|title| !title.is_empty()) {
        Some(collection) => c2m_i18n::tr!("{} 视频 {}", collection, index + 1),
        None => c2m_i18n::tr!("视频 {}", index + 1),
    }
}

/// Local recognition check; build_plan and build_batch_plan share its exact messages.
pub(crate) fn ensure_asr_capability(
    environment: &Environment,
    provider: c2m_config::config::AsrProvider,
) -> Result<()> {
    use c2m_config::config::AsrProvider;
    match provider {
        AsrProvider::Coreml => ensure!(
            environment.apple,
            c2m_i18n::tr!("Apple 识别组件不可用，请选择其他识别方式")
        ),
        AsrProvider::Gpu => ensure!(
            environment.llama && environment.gpu.is_some(),
            c2m_i18n::tr!("没有检测到可用的 GPU 识别引擎。请选择 CPU 或其他本机识别方式。")
        ),
        AsrProvider::Cpu => ensure!(
            environment.llama,
            c2m_i18n::tr!("CPU 识别引擎尚未安装。请在设置的「应用」中查看安装方法。")
        ),
        AsrProvider::Npu => ensure!(
            environment.npu(),
            c2m_i18n::tr!("没有检测到可用的 Intel NPU 识别环境。请选择其他本机识别方式。")
        ),
        AsrProvider::Api => (),
    }
    Ok(())
}

/// Without an explicit model, fill in the default for the recognition engine.
/// import_ui's import_model_request and both plan builders share this rule.
pub fn fill_default_asr_model(
    config: &mut c2m_config::settings::ConfigFile,
    provider: c2m_config::config::AsrProvider,
) {
    use c2m_config::config::AsrProvider;
    // Automatic engine selection keeps the saved model, which may be a
    // Qwen3 choice from 2.0.0 that the NPU this machine resolved to never ran.
    c2m_config::config::migrate_retired_npu_model(Some(provider), &mut config.defaults.asr_model);
    if provider != AsrProvider::Api
        && config
            .defaults
            .asr_model
            .as_deref()
            .is_none_or(|model| model.trim().is_empty())
    {
        config.defaults.asr_model = Some(c2m_config::config::default_model(provider).into());
    }
}

/// Refuses a source whose note in `root` a newer app wrote: this app cannot replace
/// it. A preview asks the last scan; a submission also reads the note's manifest.
pub(crate) fn refuse_newer_note(
    library: &LibraryState,
    root: &std::path::Path,
    identity: &str,
    validation: PlanValidation,
) -> Result<()> {
    let dir = root.join(workspace::course_dir_name(identity));
    let newer = library.newer_notes.contains(&dir)
        || (validation == PlanValidation::Submission && desktop_store::notes::note_is_newer(&dir));
    ensure!(!newer, desktop_store::notes::newer_note_refusal());
    Ok(())
}

pub(crate) fn validate_plan_config(
    source: &str,
    config: &c2m_config::settings::ConfigFile,
) -> Result<()> {
    let resolved = c2m_engine::options::resolve(source.to_owned(), &Default::default(), config)?;
    resolved.validate()?;
    if resolved.transcript_source != c2m_config::config::TranscriptSource::Subtitle {
        // Credentials belong to the service vault, not this static model/provider check.
        resolved.validate_asr_with_auth(false, false)?;
    }
    Ok(())
}

/// Published task state and its visible card change together. A full library
/// scan can include slow locations, so its old manifest cannot be the only
/// source of the card while the note just replaced is already readable.
pub fn update_published_course(
    library: &mut LibraryState,
    done: &Done,
    manifest: &c2m_artifact::artifact::Manifest,
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
    if let Some(previous) = library
        .courses
        .iter()
        .find(|previous| previous.dir == course.dir)
    {
        // A user-facing library alias belongs to the note, whichever task wrote it.
        if previous
            .manifest
            .as_ref()
            .is_some_and(|manifest| previous.title != manifest.title)
        {
            course.title = previous.title.clone();
        }
    }
    let courses = &mut library.courses;
    courses.retain(|previous| previous.dir != course.dir);
    courses.push(Rc::new(course));
    courses.sort_by_key(|course| std::cmp::Reverse(course.modified));
    // The notes' positions changed: what was sorted from them no longer holds.
    library.touch();
}

/// The local engine to suggest for this machine.
pub fn recommended_local_provider(environment: Option<&Environment>) -> AsrProvider {
    match environment {
        Some(environment) if environment.supports(AsrProvider::Coreml) => AsrProvider::Coreml,
        Some(environment) if environment.supports(AsrProvider::Gpu) => AsrProvider::Gpu,
        Some(environment) if environment.supports(AsrProvider::Npu) && !environment.llama => {
            AsrProvider::Npu
        }
        Some(_) => AsrProvider::Cpu,
        // Detection runs in the background. A temporary display hint must
        // not scan Linux devices or PATH while the interface is rendering.
        None if c2m_asr::asr::apple_native_available() => AsrProvider::Coreml,
        None => AsrProvider::Cpu,
    }
}

/// What a plan is built from.
pub struct PlanInputs<'a> {
    pub preferences: &'a preferences::Store,
    pub workspace: Option<&'a workspace::Workspace>,
    pub environment: Option<&'a Environment>,
    pub import: &'a ImportSession,
    pub library: &'a LibraryState,
    pub locations: &'a LocationChecks,
    /// Saved defaults have not reached the current input yet.
    pub defaults_pending: bool,
    /// Why ordinary preferences stop a submission.
    pub preferences_issue: Option<String>,
}

impl PlanInputs<'_> {
    /// What both plan builders require first: no unsynced or invalid preferences, then
    /// the workspace and its draft. `unsynced` is the builder's own wording.
    fn plan_draft(
        &self,
        unsynced: &'static str,
    ) -> Result<(&workspace::Workspace, &workspace::Draft)> {
        if let Some(issue) = &self.preferences_issue {
            anyhow::bail!("{issue}");
        }
        ensure!(!self.defaults_pending, unsynced);
        let workspace = self
            .workspace
            .context(c2m_i18n::tr!("输入与任务记录尚未恢复"))?;
        let draft = workspace
            .state
            .draft()
            .context(c2m_i18n::tr!("当前输入尚未准备好"))?;
        Ok((workspace, draft))
    }

    /// The plan builders' shared tool check: the engine and FFmpeg always; an `online` source also needs
    /// yt-dlp (batch plans are always online; a single plan follows its confirmed source).
    /// A check still in progress is [`PlanPending`], not a failure.
    fn ensure_plan_environment(&self, online: bool) -> Result<&Environment> {
        let environment = plan_environment(self.environment)?;
        ensure!(environment.engine, c2m_i18n::tr!("生成引擎无法启动"));
        ensure!(
            environment.ffmpeg && environment.ffprobe,
            c2m_i18n::tr!("缺少视频处理组件，请在设置中查看")
        );
        ensure!(
            !online || environment.ytdlp,
            c2m_i18n::tr!("缺少在线视频下载组件，请在设置中查看")
        );
        Ok(environment)
    }

    /// The destination is still a registered, writable library; then the configuration
    /// the draft's options make of its base, with the model directory normalized.
    fn plan_storage_and_config(
        &self,
        workspace: &workspace::Workspace,
        draft: &workspace::Draft,
        validation: PlanValidation,
    ) -> Result<c2m_config::settings::ConfigFile> {
        let library = workspace
            .state
            .library(&draft.library_id)
            .context(c2m_i18n::tr!("保存位置已不在笔记库中，请选择保存位置"))?;
        validate_plan_storage(
            validation,
            library,
            draft.folder,
            self.locations.get(library),
            self.library.indexes.get(&library.root),
        )?;
        let mut config = draft
            .base_config
            .clone()
            .unwrap_or_else(|| self.preferences.defaults_config());
        draft.options.apply_to(&mut config);
        config.defaults.model_dir = Some(c2m_config::config::model_dir_from(
            config.defaults.model_dir.as_deref(),
        ));
        Ok(config)
    }

    /// Local recognition: the engine checked (with `fallback` as the reason it is
    /// needed, when it is only a fallback) and its default model filled in.
    fn plan_asr(
        &self,
        config: &mut c2m_config::settings::ConfigFile,
        environment: &Environment,
        fallback: Option<&'static str>,
    ) -> Result<()> {
        let provider = config
            .defaults
            .provider
            .unwrap_or_else(|| recommended_local_provider(self.environment));
        let capability = ensure_asr_capability(environment, provider);
        match fallback {
            Some(reason) => capability.context(reason)?,
            None => capability?,
        }
        config.defaults.provider = Some(provider);
        fill_default_asr_model(config, provider);
        Ok(())
    }

    /// What both builders end with: the draft's services (or the defaults) resolved
    /// into the configuration, which is then validated for `source`.
    fn finish_plan(
        &self,
        draft: &workspace::Draft,
        config: c2m_config::settings::ConfigFile,
        validation: PlanValidation,
        source: source::Source,
        title: String,
    ) -> Result<TaskPlan> {
        let defaults = self.preferences.default_refs();
        let refs = ServiceRefs {
            asr: draft.asr_service.clone().or(defaults.asr),
            llm: draft.ai_service.clone().or(defaults.llm),
        };
        let config = match validation {
            PlanValidation::Preview => self.preferences.config_for_preview(&config, &refs)?,
            PlanValidation::Submission => self.preferences.config_for_refs(&config, &refs)?,
        };
        let mut validation_config = config.clone();
        validation_config
            .defaults
            .provider
            .get_or_insert_with(|| recommended_local_provider(self.environment));
        validate_plan_config(&source.input, &validation_config)?;
        // A note a newer app wrote cannot take a new version: say so before anything
        // is downloaded or sent, not when publishing refuses after paid requests.
        if let Some(location) = self
            .workspace
            .and_then(|workspace| workspace.state.library(&draft.library_id))
        {
            refuse_newer_note(self.library, &location.root, &source.identity, validation)?;
        }
        if validation == PlanValidation::Submission && !source.online {
            ensure!(
                std::path::Path::new(&source.input).is_file(),
                c2m_i18n::tr!("原视频已移动或无法读取，请重新选择视频")
            );
        }
        Ok(TaskPlan {
            operation: Default::default(),
            source_id: source.identity.clone(),
            source,
            title,
            library_id: draft.library_id.clone(),
            folder: draft.folder,
            subtitle: None,
            config,
            asr_service: refs.asr,
            ai_service: refs.llm,
        })
    }

    pub fn build_plan(&self, validation: PlanValidation) -> Result<TaskPlan> {
        let (workspace, draft) = self.plan_draft(c2m_i18n::tr!(
            "默认设置已保存，当前视频的选项尚未同步。请重试。"
        ))?;
        ensure!(
            !draft.input.is_empty(),
            if draft.online {
                c2m_i18n::tr!("先粘贴视频链接")
            } else {
                c2m_i18n::tr!("先选择一个视频")
            }
        );
        ensure!(
            !self.import.subtitle_loading && self.import.preview_cancel.is_none(),
            c2m_i18n::tr!("正在确认来源，请等待读取完成")
        );
        ensure!(
            self.import.source_preview.is_some(),
            c2m_i18n::tr!("还没读取到视频信息，请重新预览")
        );
        ensure!(
            self.import.subtitle_error.is_none(),
            c2m_i18n::tr!("字幕还没读取完成，请重新读取，或改用语音识别")
        );
        let source = draft
            .source
            .clone()
            .context(c2m_i18n::tr!("请先读取并确认视频"))?;
        let environment = self.ensure_plan_environment(source.online)?;
        ensure!(
            draft.options.source_mode == TranscriptSource::Asr
                || (source.subtitle_request.is_none() && source.subtitle_read_error.is_none()),
            c2m_i18n::tr!("所选字幕还未确认，请继续读取或明确选择其他文字来源。")
        );
        ensure!(
            !source.identity.is_empty(),
            c2m_i18n::tr!("无法识别这个视频，请重新预览")
        );
        if draft.title.trim().is_empty() {
            return Err(TitleRequired.into());
        }
        let mut config = self.plan_storage_and_config(workspace, draft, validation)?;
        let using_subtitle = draft.options.source_mode != TranscriptSource::Asr
            && (source.selected_subtitle.is_some() || draft.subtitle.is_some());
        if using_subtitle {
            if let Some(subtitle) = &source.selected_subtitle {
                ensure!(
                    subtitle.source_identity == source.identity,
                    c2m_i18n::tr!("所选字幕与当前视频不匹配，请重新读取字幕")
                );
            }
            config.defaults.transcript_source = Some(TranscriptSource::Subtitle);
        } else {
            use c2m_source::subtitle::SubtitleEvidence;
            ensure!(
                draft.options.source_mode == TranscriptSource::Asr
                    || (draft.options.source_mode == TranscriptSource::Auto
                        && matches!(
                            source.subtitles,
                            SubtitleEvidence::NoneFound
                                | SubtitleEvidence::Unsupported { .. }
                                | SubtitleEvidence::Failed { .. }
                        )),
                c2m_i18n::tr!("字幕还未确认。请读取字幕，或明确选择语音识别。")
            );
            config.defaults.transcript_source = Some(TranscriptSource::Asr);
            self.plan_asr(&mut config, environment, None)?;
        }
        let plan = self.finish_plan(draft, config, validation, source, draft.title.clone())?;
        Ok(TaskPlan {
            subtitle: draft.subtitle.clone(),
            ..plan
        })
    }

    /// The batch plan for selected candidates. Tools, destination and config resolve as in build_plan;
    /// subtitles are found when each task runs (the pipeline probes by transcript_source), so the desktop
    /// need not read every video first. Titles are the episode titles shown in the candidate rows.
    pub fn build_batch_plan(
        &self,
        candidate: &source::SourceCandidate,
        title: String,
    ) -> Result<TaskPlan> {
        let (workspace, draft) =
            self.plan_draft(c2m_i18n::tr!("默认设置已保存，当前选项尚未同步。请重试。"))?;
        let identity = c2m_source::fetch::candidate_identity(candidate)
            .context(c2m_i18n::tr!("还无法确认这个视频的身份，请单独打开后转换"))?;
        let environment = self.ensure_plan_environment(true)?;
        let mut config =
            self.plan_storage_and_config(workspace, draft, PlanValidation::Submission)?;
        // Subtitles-only never reaches speech recognition; other modes may fall back to ASR at run time,
        // so, as in build_plan, check local recognition and fill in the default model first.
        if draft.options.source_mode != TranscriptSource::Subtitle {
            let fallback = (draft.options.source_mode == TranscriptSource::Auto).then_some(
                c2m_i18n::tr!("部分视频可能没有字幕，需要语音识别能力作为后备"),
            );
            self.plan_asr(&mut config, environment, fallback)?;
        }
        let source = source::Source {
            input: candidate.input.clone(),
            title: title.clone(),
            duration: candidate
                .duration
                .filter(|value| value.is_finite() && *value > 0.)
                .unwrap_or_default(),
            cover: self
                .import
                .source_candidate_covers
                .get(&candidate.input)
                .cloned(),
            identity,
            online: true,
            ..source::Source::default()
        };
        self.finish_plan(draft, config, PlanValidation::Submission, source, title)
    }
}

#[cfg(test)]
pub mod tests {
    use super::{
        PlanValidation, apply_draft_options, batch_note_title, edit_current_draft,
        update_draft_source_title, validate_plan_config, validate_plan_storage,
    };

    #[test]
    fn a_plan_tells_an_obstacle_from_a_check_still_running() {
        use super::{PlanInputs, PlanPending};
        use crate::import::ImportSession;
        use desktop_prefs::{credentials::MemoryCredentialVault, preferences};
        use desktop_store::{
            library::LibraryState, locations::LocationChecks, workspace::Workspace,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = preferences::Store::open(
            dir.path().join("preferences"),
            std::sync::Arc::new(MemoryCredentialVault::new()),
        );
        let root = dir.path().join("library");
        let mut workspace = Workspace::open_at_inline(
            dir.path().join("workspace.json"),
            root.clone(),
            Default::default(),
        )
        .unwrap();
        workspace
            .transaction(|state| {
                let draft = state.draft_mut().unwrap();
                draft.input = "https://example.test/video".into();
                draft.source = Some(source("https://example.test/video", "课程"));
                Ok(())
            })
            .unwrap();
        let import = ImportSession {
            source_preview: Some(source("https://example.test/video", "课程")),
            ..Default::default()
        };
        let library = LibraryState::new(root);
        let locations = LocationChecks::default();
        let inputs = |preferences_issue: Option<&str>| PlanInputs {
            preferences: &store,
            workspace: Some(&workspace),
            environment: None,
            import: &import,
            library: &library,
            locations: &locations,
            defaults_pending: false,
            preferences_issue: preferences_issue.map(str::to_owned),
        };
        let error = inputs(Some("生成选项暂时无法读取"))
            .build_plan(super::PlanValidation::Preview)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "生成选项暂时无法读取");
        assert!(error.downcast_ref::<PlanPending>().is_none());
        let waiting = inputs(None)
            .build_plan(super::PlanValidation::Preview)
            .err()
            .unwrap();
        assert!(waiting.downcast_ref::<PlanPending>().is_some());
    }

    #[test]
    fn an_automatic_engine_resolving_to_the_npu_does_not_keep_qwen3() {
        use c2m_config::config::AsrProvider;
        let mut config = c2m_config::settings::ConfigFile::default();
        config.defaults.asr_model = Some("qwen3-1.7b".into());
        super::fill_default_asr_model(&mut config, AsrProvider::Npu);
        assert_eq!(config.defaults.asr_model.as_deref(), Some("whisper"));
        config.defaults.asr_model = Some("qwen3-1.7b".into());
        super::fill_default_asr_model(&mut config, AsrProvider::Cpu);
        assert_eq!(config.defaults.asr_model.as_deref(), Some("qwen3-1.7b"));
    }

    #[test]
    fn a_changed_option_is_the_draft_without_a_separate_save() {
        use desktop_store::workspace::{Override, Workspace};
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
        use crate::backend::Done as Completed;
        use c2m_artifact::artifact::{Manifest, Outcome, Outcomes};
        use desktop_store::notes::Course;
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
            schema: 2,
            task_id: "old".into(),
            course_id: "course".into(),
            source_id: "same-source".into(),
            publication: String::new(),
            version_id: String::new(),
            title: "原始课程名".into(),
            created_at_ms: 1_000,
            revision: 0,
            document: "document.json".into(),
            markdown: "course.md".into(),
            frames: Vec::new(),
            assets: Vec::new(),
            outputs: Vec::new(),
            outcomes,
            partial: true,
        };
        let old = Course {
            dir: "/library/course".into(),
            title: "用户命名".into(),
            modified: UNIX_EPOCH + Duration::from_secs(1),
            slides: 0,
            thumbnail: Some("/old-cover.jpg".into()),
            manifest: Some(old_manifest.clone()),
            warning: None,
        };
        let mut other_library = old.clone();
        other_library.dir = "/other-library/course".into();
        let mut library = desktop_store::library::LibraryState::new("library".into());
        library.courses = vec![
            std::rc::Rc::new(old.clone()),
            std::rc::Rc::new(other_library.clone()),
        ];
        let revision = library.revision;
        let mut repaired = old_manifest;
        repaired.task_id = "repair".into();
        repaired.created_at_ms = 2_000;
        repaired.partial = false;
        repaired.outcomes.proofreading = Outcome::succeeded();
        let done = Completed {
            out_dir: "/library/course".into(),
            title: repaired.title.clone(),
            ..Default::default()
        };
        super::update_published_course(
            &mut library,
            &done,
            &repaired,
            Some(PathBuf::from("/new-cover.jpg")),
        );
        // A view that sorted the notes sorts them again.
        assert_ne!(library.revision, revision);
        let courses = &library.courses;
        assert_eq!(courses.len(), 2);
        let current = &courses[0];
        assert_eq!(current.dir, done.out_dir);
        assert_eq!(current.title, "用户命名");
        assert_eq!(
            current.thumbnail.as_deref(),
            Some(std::path::Path::new("/new-cover.jpg"))
        );
        assert!(!current.incomplete());
        assert_eq!(current.manifest.as_ref().unwrap().task_id, "repair");
        assert_eq!(courses[1].dir, other_library.dir);
        assert!(courses[1].incomplete());
        assert!(old.manifest.as_ref().unwrap().partial);
    }

    #[test]
    fn unchanged_form_values_leave_the_draft_as_it_was() {
        let mut input =
            desktop_store::workspace::Draft::new(true, "library".into(), Default::default());
        let source = source("https://example.test/a", "课程 A");
        update_draft_source_title(
            &mut input,
            source.input.clone(),
            source.title.clone(),
            Some(source),
        );
        input.updated = 123;
        let expected = input.clone();
        for _ in 0..8 {
            update_draft_source_title(
                &mut input,
                expected.input.clone(),
                expected.title.clone(),
                expected.source.clone(),
            );
            apply_draft_options(&mut input, expected.options.clone());
            assert_eq!(input, expected);
        }
        let mut options = expected.options.clone();
        options.formats[0] = !options.formats[0];
        options.translate = Some("en".into());
        apply_draft_options(&mut input, options.clone());
        assert_eq!(input.options, options);
        for field in [
            desktop_store::workspace::Override::Formats,
            desktop_store::workspace::Override::Translate,
        ] {
            assert!(input.overrides.contains(&field));
        }
    }

    #[test]
    fn temporary_speech_service_keeps_the_explicit_local_engine_for_the_same_input() {
        let mut draft =
            desktop_store::workspace::Draft::new(true, "library".into(), Default::default());
        draft.change_source("video".into());
        let mut options = draft.options.clone();
        options.provider = Some(c2m_config::config::AsrProvider::Cpu);
        apply_draft_options(&mut draft, options.clone());
        assert_eq!(
            draft.local_provider,
            Some(c2m_config::config::AsrProvider::Cpu)
        );
        options.provider = Some(c2m_config::config::AsrProvider::Api);
        apply_draft_options(&mut draft, options);
        assert_eq!(
            draft.options.provider,
            Some(c2m_config::config::AsrProvider::Api)
        );
        assert_eq!(
            draft.local_provider,
            Some(c2m_config::config::AsrProvider::Cpu)
        );
        let next = desktop_store::workspace::Draft::new(true, "library".into(), Default::default());
        assert_eq!(next.local_provider, None);
    }

    /// A note a newer app wrote is refused when the plan is made, before any
    /// download or request, in the interface language.
    #[test]
    fn a_note_a_newer_app_wrote_is_refused_before_any_work() {
        use desktop_store::{library::LibraryState, notes, workspace::course_dir_name};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        let mut library = LibraryState::new(root.clone());
        let check = |library: &LibraryState, validation| {
            super::refuse_newer_note(library, &root, "local:sha256:abc", validation)
        };
        check(&library, PlanValidation::Submission).unwrap();
        let note = root.join(course_dir_name("local:sha256:abc"));
        std::fs::create_dir_all(&note).unwrap();
        std::fs::write(
            note.join("current.json"),
            r#"{"schema":2,"course_id":"c","version_id":"v","manifest":"versions/v/manifest.json"}"#,
        )
        .unwrap();
        // Before a scan saw it, the submission's own read refuses it.
        check(&library, PlanValidation::Preview).unwrap();
        let error = check(&library, PlanValidation::Submission).unwrap_err();
        assert_eq!(error.to_string(), notes::newer_note_refusal());
        let scan = notes::scan_library(&root).unwrap();
        assert_eq!(scan.newer, std::slice::from_ref(&note));
        library.newer_notes.extend(scan.newer);
        assert!(check(&library, PlanValidation::Preview).is_err());
        // Another video's note is not this one.
        super::refuse_newer_note(
            &library,
            &root,
            "local:sha256:other",
            PlanValidation::Submission,
        )
        .unwrap();
    }

    #[test]
    fn plan_preview_uses_loaded_storage_but_submission_rechecks_identity_and_folders() {
        let directory = tempfile::tempdir().unwrap();
        let location = desktop_store::workspace::LibraryLocation {
            id: "library".into(),
            name: "笔记库".into(),
            root: directory.path().to_owned(),
            previous_roots: Vec::new(),
        };
        let cached = desktop_store::locations::LocationCheck {
            available: true,
            needs_reassociation: false,
            problem: None,
        };
        let mut index = desktop_store::organize::Library::default();
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
        let checking = super::plan_environment(None).err().unwrap();
        assert!(
            checking.downcast_ref::<super::PlanPending>().is_some(),
            "a component check still in progress is waiting, not a failure"
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
        use desktop_store::workspace::{TaskPlan, Workspace};
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
        let mut draft =
            desktop_store::workspace::Draft::new(true, "library".into(), Default::default());
        let a = source("https://example.test/a", "课程 A");
        let b = source("https://example.test/b", "课程 B");
        update_draft_source_title(&mut draft, a.input.clone(), a.title.clone(), Some(a));
        // Source and title may both change before the autosave deadline.
        update_draft_source_title(&mut draft, b.input.clone(), "我的复习笔记".into(), None);
        assert!(draft.custom_title);
        let revision = draft.revision;
        assert!(desktop_store::workspace::accept_source(
            &mut draft, revision, b
        ));
        assert_eq!(draft.title, "我的复习笔记");
        assert!(draft.custom_title);
    }

    #[test]
    fn submit_validates_the_selected_model_only_when_speech_recognition_is_needed() {
        use c2m_config::config::{AsrProvider, TranscriptSource};
        let mut config = c2m_config::settings::ConfigFile::default();
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

    pub fn source(input: &str, title: &str) -> crate::source::Source {
        crate::source::Source {
            input: input.into(),
            title: title.into(),
            identity: format!("online:{input}"),
            online: true,
            ..Default::default()
        }
    }
}
