//! What a task plan is built from, and the checks it has to pass.
use crate::{
    backend::{Done, Environment},
    conversion::ConversionOptions,
    locations::LocationCheck,
    notes::Course,
    organize, source, workspace,
};
use anyhow::{Context as _, Result, ensure};
use std::{path::PathBuf, rc::Rc};

/// A stable validation identity for the missing-title case. Rendering compares
/// the typed error instead of the displayed sentence, which changes with the
/// interface language.
#[derive(Debug)]
pub struct TitleRequired;

impl std::fmt::Display for TitleRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(course2md::tr!("请填写笔记名称"))
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
pub fn plan_environment(environment: Option<&Environment>) -> Result<&Environment> {
    Ok(environment.ok_or(PlanPending(course2md::tr!("正在检查所需组件…")))?)
}

pub fn validate_plan_storage(
    validation: PlanValidation,
    library: &workspace::LibraryLocation,
    folder: Option<u64>,
    cached: Option<&LocationCheck>,
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

/// Apply the visible form without treating a save attempt as a user edit.
/// Navigation often calls this with identical values; keep timestamps and disk
/// records unchanged unless an actual input or option changed.
pub fn update_input_form(
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
        Some(collection) => course2md::tr!("{} 视频 {}", collection, index + 1),
        None => course2md::tr!("视频 {}", index + 1),
    }
}

/// Local recognition check; build_plan and build_batch_plan share its exact messages.
pub fn ensure_asr_capability(
    environment: &Environment,
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
pub fn fill_default_asr_model(
    config: &mut course2md::settings::ConfigFile,
    provider: course2md::config::AsrProvider,
) {
    use course2md::config::AsrProvider;
    // Automatic engine selection keeps the saved model, which may be a
    // Qwen3 choice from 2.0.0 that the NPU this machine resolved to never ran.
    course2md::config::migrate_retired_npu_model(Some(provider), &mut config.defaults.asr_model);
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

pub fn validate_plan_config(source: &str, config: &course2md::settings::ConfigFile) -> Result<()> {
    let resolved = course2md::options::resolve(source.to_owned(), &Default::default(), config)?;
    resolved.validate()?;
    if resolved.transcript_source != course2md::config::TranscriptSource::Subtitle {
        // Credentials belong to the service vault, not this static model/provider check.
        resolved.validate_asr_with_auth(false, false)?;
    }
    Ok(())
}

/// Published task state and its visible card change together. A full library
/// scan can include slow locations, so its old manifest cannot be the only
/// source of the card while the newly published version is already readable.
pub fn update_published_course(
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

#[cfg(test)]
pub mod tests {
    use super::{
        PlanValidation, apply_draft_options, batch_note_title, edit_current_draft,
        update_draft_source_title, update_input_form, validate_plan_config, validate_plan_storage,
    };

    #[test]
    fn an_automatic_engine_resolving_to_the_npu_does_not_keep_qwen3() {
        use course2md::config::AsrProvider;
        let mut config = course2md::settings::ConfigFile::default();
        config.defaults.asr_model = Some("qwen3-1.7b".into());
        super::fill_default_asr_model(&mut config, AsrProvider::Npu);
        assert_eq!(config.defaults.asr_model.as_deref(), Some("whisper"));
        config.defaults.asr_model = Some("qwen3-1.7b".into());
        super::fill_default_asr_model(&mut config, AsrProvider::Cpu);
        assert_eq!(config.defaults.asr_model.as_deref(), Some("qwen3-1.7b"));
    }

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
        let cached = crate::locations::LocationCheck {
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
