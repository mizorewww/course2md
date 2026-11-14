//! Convert a fixed task into a note, published by replacing the course's note in place.

use crate::progress::Stage;
use crate::{
    artifact::{self, Outcome, Outcomes, Status, Target},
    asr,
    config::{self, PipelineConfig},
    dispatch::{ControlSource, ParentProcess, TaskControl},
    execution::{self, Job},
    fetch::{self, VideoMeta},
    media, progress, scene, summarize, timeline,
};
use crate::{eprintln_tr, tr};
use anyhow::{Context, Result};
use c2m_core::speech::{ModelRequired, Remedy};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

/// Stable digest prefix kept when a fresh run appends a timestamp to its task id.
const TASK_ID_PREFIX_LEN: usize = 24;
/// Local-file titles are truncated so directory names stay usable.
const STEM_MAX_CHARS: usize = 40;
/// Screenshot checkpoint files are digest-verified across this many parallel lanes.
const FRAME_DIGEST_LANES: usize = 4;
/// How much transcript the language guess reads.
const LANGUAGE_SAMPLE_CHARS: usize = 4000;

/// One message for an empty screenshot result.
const NO_SCREENSHOTS: &str = "未提取到截图";

/// A task in its locked work directory; every entry point ends up here.
struct Task<'a> {
    cfg: &'a PipelineConfig,
    job: &'a Job,
    target: &'a Target,
    is_local: bool,
    started: Instant,
}

/// Whether a command-line run may download a local speech model it lacks. A task the
/// desktop runs (`run-task`) never downloads one.
#[derive(Clone, Copy)]
pub enum ModelDownload<'a> {
    /// Download it: `--download-models`, or the person already agreed in this run.
    Allowed,
    /// Ask the person at the terminal; `Ok(true)` downloads.
    Ask(&'a (dyn Fn(&crate::models::ModelOffer) -> Result<bool> + Sync)),
    /// Stop with `ModelRequired`, naming the command that prepares it.
    Refused,
}

/// Command-line entry. The video is probed here; the run is then an ordinary task
/// (`execution::Request`). Its id is the digest of what the command line chose, so the
/// same settings continue the same work directory and do not make again a note the
/// task already published; `--no-resume` starts another task.
pub async fn run(cfg: &PipelineConfig, models: ModelDownload<'_>) -> Result<()> {
    cfg.validate()
        .context(tr!("配置预检失败 / Invalid settings"))?;
    let local = Path::new(&cfg.url);
    let is_local = local.is_file();
    if !is_local {
        crate::error::require_ytdlp()?;
    }
    progress::start(Stage::Fetch);
    let probed = if is_local {
        None
    } else {
        Some(Box::new(
            fetch::probe_video(&cfg.url, &TaskControl::default()).await?,
        ))
    };
    // A local video's duration is probed first; a failure is not taken as zero.
    let local_duration = if is_local {
        Some(media::probe_duration(local).await.context(tr!(
            "无法探测本地视频时长 / Could not probe the local video duration"
        )))
    } else {
        None
    };
    let meta = if let Some(video) = &probed {
        video.meta.clone()
    } else {
        VideoMeta {
            title: sanitize_stem(local),
            uploader: String::new(),
            duration: local_duration
                .as_ref()
                .and_then(|probe| probe.as_ref().ok().copied())
                .unwrap_or(0.),
            webpage_url: cfg.url.clone(),
            extractor: "local".into(),
            id: {
                let local = local.to_path_buf();
                blocking(move || execution::file_digest(&local)).await?
            },
        }
    };
    progress::done(Stage::Fetch);
    if let Some(Err(error)) = local_duration {
        return Err(error);
    }
    let platform = config::platform_from(&cfg.url, &meta.extractor);
    let source_id = if is_local {
        format!("local:sha256:{}", meta.id)
    } else {
        format!("{platform}:{}", meta.id)
    };
    let course_id = execution::digest(source_id.as_bytes());
    let course_dir = std::path::absolute(&cfg.out_root)?
        .join(&platform)
        .join(&course_id);
    let former = cli_binding(&source_id, cfg, &meta.title);
    let mut task_id = crate::digest::json_digest(&former)?;
    if !cfg.resume {
        task_id = format!(
            "{}-{}",
            &task_id[..TASK_ID_PREFIX_LEN],
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
    }
    let request = execution::Request {
        schema: execution::REQUEST_SCHEMA,
        operation: execution::Operation::Generate,
        work_dir: artifact::work_dir(&course_dir, &task_id)?,
        task_id,
        course_id,
        version_id: None,
        source: cfg.url.clone(),
        title: Some(meta.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| source_id.clone()),
        source_id,
        // Left out of the task's identity, as the former one left them out: a site may
        // report another uploader or duration on the next probe, and the task must still
        // continue. The note takes both from this run's probe (`Cli::meta`).
        author: String::new(),
        duration: 0.,
        subtitle: None,
        subtitle_events: None,
        config: crate::options::file_for(cfg),
        allow_unauthenticated_asr: false,
        course_dir,
        control_path: None,
        service_versions: Default::default(),
    };
    request.validate()?;
    let cfg = request.resolve_typed()?;
    let cli = Cli {
        meta,
        probed,
        models,
        former,
    };
    run_task(&request, &cfg, Caller::Cli(Box::new(cli))).await
}

/// The identity a CLI task bound its work directory to before the command line ran
/// tasks; its digest still names the task. Identities record the output root as
/// `out_dir`, the value the settings once carried there.
pub(crate) fn cli_binding(source_id: &str, cfg: &PipelineConfig, title: &str) -> serde_json::Value {
    serde_json::json!({"source_id": source_id, "config": cfg.redacted_for_identity(&cfg.out_root), "title": title})
}

/// Who runs a task: the desktop app (`run-task`), or the command line.
pub(crate) enum Caller<'a> {
    /// Stop as on quit once this process is gone.
    Task(ParentProcess),
    Cli(Box<Cli<'a>>),
}

/// What only a command-line run has.
pub(crate) struct Cli<'a> {
    /// The video as probed, which the note describes as it is.
    meta: VideoMeta,
    probed: Option<Box<fetch::OnlineVideo>>,
    models: ModelDownload<'a>,
    /// The identity this task's work directory had before the command line ran
    /// tasks ([`cli_binding`]): a task interrupted then continues where it was.
    former: serde_json::Value,
}

/// Every entry point ends here, with configuration and credentials resolved.
pub(crate) async fn run_task(
    request: &execution::Request,
    cfg: &PipelineConfig,
    caller: Caller<'_>,
) -> Result<()> {
    // The caller validates through `Request::resolve`.
    let started = Instant::now();
    let target = request.target();
    {
        let note = target.course_dir.clone();
        blocking(move || artifact::prepare_note(&note)).await?;
    }
    // Held until the task ends: a second task for this note stops before it pays.
    let _claim = artifact::claim_note(&target.course_dir)?;
    if let execution::Operation::Reprocess {
        note_dir,
        base_task_id,
        ..
    } = &request.operation
    {
        // A repair that already replaced the note is done: restarting it after a
        // crash sends nothing, whatever its saved progress says.
        let (note, task_id) = (note_dir.clone(), request.task_id.clone());
        if let Some(manifest) = blocking(move || artifact::published_by(&note, &task_id)).await? {
            let document = read_document(note_dir, &manifest)?;
            emit_done(&target, &manifest, &[], &document.sections, started);
            return Ok(());
        }
        let note = note_dir.clone();
        let current = blocking(move || artifact::note_manifest(&note)).await?;
        anyhow::ensure!(
            current.is_some_and(|manifest| &manifest.task_id == base_task_id),
            artifact::NoteChanged
        );
    }
    let binding = {
        let (request, cfg) = (request.clone(), cfg.clone());
        blocking(move || request.binding(&cfg)).await?
    };
    let (parent, former) = match &caller {
        Caller::Task(parent) => (Some(*parent), None),
        Caller::Cli(cli) => (None, Some(&cli.former)),
    };
    let work = execution::enter_work_dir(
        &request.work_dir,
        &binding,
        former,
        parent.is_some(),
        ControlSource {
            control_path: request.control_path.clone(),
            service_versions: request.service_versions.clone(),
            parent,
        },
    )?;
    let is_local = Path::new(&request.source).is_file();
    let platform = if is_local {
        "local".to_owned()
    } else {
        config::platform_from(&request.source, "")
    };
    let task = Task {
        cfg,
        job: work.job(),
        target: &target,
        is_local,
        started,
    };
    task.recording_failure(&platform, &request.source_id, async {
        match (&request.operation, caller) {
            (
                execution::Operation::Reprocess {
                    note_dir,
                    components,
                    prior_work_dir,
                    ..
                },
                _,
            ) => {
                task.reprocess(request, note_dir, components, prior_work_dir.as_deref())
                    .await
            }
            (execution::Operation::Generate, Caller::Cli(cli)) => {
                let Cli {
                    meta,
                    probed,
                    models,
                    ..
                } = *cli;
                let input = SubtitleInput::Discover(probed);
                task.generate(&meta, input, Credentials::Cli(models)).await
            }
            (execution::Operation::Generate, Caller::Task(_)) => {
                task.generate_requested(request, platform.clone()).await
            }
        }
    })
    .await
}

enum SubtitleInput {
    /// CLI path: the video probed once at fetch time; `None` for local files.
    Discover(Option<Box<fetch::OnlineVideo>>),
    Selected(Option<Vec<timeline::TranscriptEvent>>),
}

/// 批量任务不带预选字幕时：Auto/强制字幕模式在任务内探测并发现字幕。
fn discovers_subtitles_at_runtime(
    subtitle: &Option<Vec<timeline::TranscriptEvent>>,
    cfg: &PipelineConfig,
    is_local: bool,
) -> bool {
    subtitle.is_none() && cfg.transcript_source != config::TranscriptSource::Asr && !is_local
}

/// Where a note's transcript came from; recorded in run.json and the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum TranscriptOrigin {
    SelectedSubtitle,
    AutoCaption,
    Subtitle,
    Asr,
}

#[derive(Serialize, Deserialize)]
struct TranscriptCache {
    source: TranscriptOrigin,
    events: Vec<timeline::TranscriptEvent>,
}

/// Where cloud credentials come from: the CLI may read the environment; a desktop task
/// carries fully resolved credentials and an explicit no-auth decision.
#[derive(Clone, Copy)]
enum Credentials<'a> {
    Cli(ModelDownload<'a>),
    Task { allow_unauthenticated_asr: bool },
}

#[derive(Serialize, Deserialize)]
struct FramesCache {
    frames: Vec<timeline::FrameEvent>,
    files: Vec<(String, String)>,
}

/// The note's document, as its manifest names it.
fn read_document(note_dir: &Path, manifest: &artifact::Manifest) -> Result<artifact::Document> {
    Ok(serde_json::from_slice(&std::fs::read(
        artifact::safe_asset_path(note_dir, &manifest.document)?,
    )?)?)
}

/// Reports the note a task published.
fn emit_done(
    target: &Target,
    manifest: &artifact::Manifest,
    kept_edits: &[String],
    sections: &[timeline::Section],
    started: Instant,
) {
    let note = &target.course_dir;
    progress::emit(&progress::Event::Done(Box::new(progress::Done {
        task_id: target.task_id.clone(),
        course_id: target.course_id.clone(),
        out_dir: note.clone(),
        manifest: Some(note.join("manifest.json")),
        title: manifest.title.clone(),
        slides: manifest.frames.len(),
        outputs: manifest.outputs.iter().map(PathBuf::from).collect(),
        partial: Some(manifest.partial),
        outcomes: Some(manifest.outcomes.clone()),
        kept_edits: kept_edits.iter().map(PathBuf::from).collect(),
        ..done_counts(sections, started)
    })));
    if !kept_edits.is_empty() && !progress::is_json() && !progress::is_quiet() {
        for kept in kept_edits {
            eprintln_tr!(
                "你修改过的文件已另存为 / Your edited file was kept as: {}",
                note.join(kept).display()
            );
        }
    }
}

/// Transcript scale and elapsed time, shared by every "done" event.
fn done_counts(sections: &[timeline::Section], started: Instant) -> progress::Done {
    let speech = || sections.iter().flat_map(|section| &section.speech);
    progress::Done {
        segments: speech().count(),
        chars: speech().map(|event| event.text.chars().count()).sum(),
        elapsed_secs: started.elapsed().as_secs_f64(),
        ..Default::default()
    }
}

impl Task<'_> {
    /// Any failure once the work directory is known leaves a diagnostic run.json:
    /// version, source, provider, asr_model, error text, elapsed time. A failed write
    /// only warns, since the original error is the outcome.
    async fn recording_failure(
        &self,
        platform: &str,
        id: &str,
        work: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        let result = work.await;
        if let Err(error) = &result {
            let cfg = self.cfg;
            let info = serde_json::json!({
                "success": false,
                "course2md_version": env!("CARGO_PKG_VERSION"),
                "source": {
                    "kind": if self.is_local { "local" } else { "remote" },
                    "platform": platform,
                    "id": id,
                    "url": cfg.url,
                },
                "provider": cfg.provider.as_str(),
                "asr_model": cfg.asr_model.as_deref().unwrap_or("backend-default"),
                "error": format!("{error:#}"),
                "elapsed_secs": (self.started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
            });
            if let Err(error) = self.write_json("run.json", &info) {
                tracing::warn!(
                    "写失败诊断 run.json 失败 / Failed to write failure diagnostics run.json: {error:#}"
                );
            }
        }
        result
    }

    fn write_json(&self, name: &str, value: &impl Serialize) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(value)?;
        crate::fsio::atomic_write(&self.job.dir.join(name), &bytes)
    }

    /// `artifact::publish`, off the async workers: it copies, digests and syncs files.
    async fn publish(
        &self,
        meta: &VideoMeta,
        sections: &[timeline::Section],
        summary: Option<&summarize::Summary>,
        formats: &[config::OutputFormat],
        outcomes: Outcomes,
    ) -> Result<artifact::Publication> {
        let (target, work_dir, control) = (
            self.target.clone(),
            self.job.dir.clone(),
            self.job.control.clone(),
        );
        let (meta, sections, summary, formats) = (
            meta.clone(),
            sections.to_vec(),
            summary.cloned(),
            formats.to_vec(),
        );
        blocking(move || {
            artifact::publish(
                &target,
                &work_dir,
                &meta,
                &sections,
                summary.as_ref(),
                &formats,
                outcomes,
                &control,
            )
        })
        .await
    }

    fn emit_done(&self, publication: &artifact::Publication, sections: &[timeline::Section]) {
        emit_done(
            self.target,
            &publication.manifest,
            &publication.kept_edits,
            sections,
            self.started,
        );
    }

    /// A desktop generate task. A batch task carries no selected subtitles: in Auto or
    /// subtitle-only mode it probes once and discovers them as the CLI does. The probe
    /// fills in the author and duration; the title stays the one queued.
    async fn generate_requested(
        &self,
        request: &execution::Request,
        extractor: String,
    ) -> Result<()> {
        let mut meta = VideoMeta {
            title: request.title.clone(),
            uploader: request.author.clone(),
            duration: request.duration,
            webpage_url: request.source.clone(),
            extractor,
            id: request.source_id.clone(),
        };
        let subtitle = if let Some(events) = &request.subtitle_events {
            Some(events.clone())
        } else if let Some(path) = &request.subtitle {
            let content = crate::subtitle::read_subtitle_text(path)
                .context(tr!("无法读取已选字幕 / Cannot read selected subtitles"))?;
            Some(crate::subtitle::parse_subtitle(&content))
        } else {
            None
        };
        let input = if discovers_subtitles_at_runtime(&subtitle, self.cfg, self.is_local) {
            progress::start(Stage::Fetch);
            let video = fetch::probe_video(&request.source, &self.job.control).await?;
            progress::done(Stage::Fetch);
            if meta.uploader.trim().is_empty() {
                meta.uploader = video.meta.uploader.clone();
            }
            if !meta.duration.is_finite() || meta.duration <= 0. {
                meta.duration = video.meta.duration;
            }
            SubtitleInput::Discover(Some(Box::new(video)))
        } else {
            SubtitleInput::Selected(subtitle)
        };
        let credentials = Credentials::Task {
            allow_unauthenticated_asr: request.allow_unauthenticated_asr,
        };
        self.generate(&meta, input, credentials).await
    }

    async fn generate(
        &self,
        meta: &VideoMeta,
        input: SubtitleInput,
        credentials: Credentials<'_>,
    ) -> Result<()> {
        let Self {
            cfg, job, target, ..
        } = *self;
        job.control.check()?;
        let (note, task_id) = (target.course_dir.clone(), target.task_id.clone());
        if let Some(manifest) = blocking(move || artifact::published_by(&note, &task_id)).await? {
            // This task already replaced the note: a restart sends nothing again.
            let document = read_document(&target.course_dir, &manifest)?;
            emit_done(target, &manifest, &[], &document.sections, self.started);
            return Ok(());
        }
        if cfg.llm.uses_service() {
            crate::llm::validate(&cfg.llm)?;
        }
        // Subtitle evidence and cloud static validation precede any full media download.
        progress::start(Stage::Subtitle);
        let selected = self.subtitles(input).await?;
        progress::done(Stage::Subtitle);
        self.check_tools(selected.is_none(), credentials).await?;
        meta.save(&job.meta_path())?;
        let (media_path, media_existed) = self.acquire_media().await?;
        job.control.check()?;
        let mut outcomes = Outcomes::default();
        // 两路并行但都受 control.watch 打断：取消时任一路不再等到自身阶段边界才停。
        // 被 dropped 的子进程分支由 kill_on_drop 终止；转写跑在 spawn_blocking 里，
        // drop 不会停止它，它在下一段开始前的 control.check 处退出。
        let (frames_result, transcript_result) = tokio::join!(
            async {
                tokio::select! {
                    r = self.cached_frames(&media_path) => r,
                    e = job.control.watch() => Err(e),
                }
            },
            async {
                tokio::select! {
                    r = self.transcript(&media_path, selected) => r,
                    e = job.control.watch() => Err(e),
                }
            }
        );
        let (frames, screenshots) = frames_outcome(frames_result);
        outcomes.screenshots = screenshots;
        let transcript = match transcript_result {
            Ok(cache) if timeline::has_readable_events(&cache.events) => {
                outcomes.transcript = Outcome::succeeded();
                cache
            }
            Ok(_) => {
                self.save_timeline(&frames, &[]).await?;
                anyhow::bail!(tr!(
                    "没有获得可读文字；已保留可用截图 / No readable transcript; available screenshots were retained"
                ));
            }
            Err(error) => {
                let completed = crate::checkpoint::Checkpoint::saved_events(&job.dir)
                    .unwrap_or_else(|read_error| {
                        tracing::warn!(
                            "已保存的部分文字无法读取，原文件已保留 / Saved partial text could not be read; original files retained: {read_error:#}"
                        );
                        Vec::new()
                    });
                self.save_timeline(&frames, &completed).await?;
                return Err(error);
            }
        };
        self.save_timeline(&frames, &transcript.events).await?;
        let mut sections = build_sections(frames, transcript.events, meta.duration);
        timeline::coalesce_sections(&mut sections);
        let (sections, summary) = self.enrich(sections, meta, &mut outcomes).await?;
        job.control.check()?;
        progress::start(Stage::Render);
        let publication = self
            .publish(meta, &sections, summary.as_ref(), &cfg.formats, outcomes)
            .await?;
        let manifest = &publication.manifest;
        progress::done(Stage::Render);
        // Diagnostics remain in work space; a failure record can never become a library entry.
        self.write_json(
            "run.json",
            &serde_json::json!({"success":true,"task_id":target.task_id,"transcript_source":transcript.source,"outcomes":manifest.outcomes,"out_dir":target.course_dir}),
        )?;
        if should_delete_media(
            self.is_local,
            cfg.no_download,
            media_existed,
            cfg.keep_video,
        ) {
            let _ = std::fs::remove_file(&media_path);
        }
        self.emit_done(&publication, &sections);
        if !progress::is_json() && !progress::is_quiet() {
            eprintln_tr!(
                "{}：{}",
                if manifest.partial {
                    tr!("笔记已保存，部分处理未完成 / Notes saved with unfinished work")
                } else {
                    tr!("笔记已生成 / Notes ready")
                },
                target.course_dir.display()
            );
            if !cfg.llm.enabled && !cfg.llm.disable_hint {
                eprintln_tr!(
                    "提示：运行 course2md llm setup 可开启 AI 校对与摘要 / Tip: run course2md llm setup to enable AI proofreading and summaries"
                );
            }
        }
        Ok(())
    }

    async fn reprocess(
        &self,
        request: &execution::Request,
        base_dir: &Path,
        components: &[execution::Component],
        prior_work: Option<&Path>,
    ) -> Result<()> {
        // `base_dir` is the note itself, read until the repaired note replaces it.
        use execution::Component;
        let Self { cfg, job, .. } = *self;
        job.control.check()?;
        let base = {
            let (base_dir, control) = (base_dir.to_path_buf(), job.control.clone());
            blocking(move || {
                let base = artifact::read_manifest(&base_dir.join("manifest.json"))?;
                // Reprocessing consumes the internal document, timeline and image assets;
                // a hand-edited Markdown file does not make them unusable.
                artifact::validate_note_in_task(&base_dir, &base, &control)?;
                Ok(base)
            })
            .await?
        };
        if let Some(markdown) = base.assets.iter().find(|asset| asset.path == base.markdown) {
            let unchanged = artifact::safe_asset_path(base_dir, &markdown.path)
                .and_then(|path| execution::file_digest(&path))
                .is_ok_and(|digest| digest == markdown.sha256);
            if !unchanged {
                tracing::info!(
                    "笔记文件已被手动修改；这次补做以原始记录为准，你修改的文件会另存为 course.edited.md / The note file was edited by hand; this run uses the original record and keeps your edited file as course.edited.md"
                );
            }
        }
        anyhow::ensure!(
            base.source_id == request.source_id && base.course_id == request.course_id,
            tr!("补做任务与原笔记来源不一致 / Reprocessing source does not match the base note")
        );
        let has = |component: Component| components.contains(&component);
        let mut document = read_document(base_dir, &base)?;
        if components.iter().all(|&c| c == Component::Exports) {
            return self.export_only(request, base_dir, &base, &document);
        }
        if components.iter().any(|component| component.is_ai()) {
            crate::llm::validate(&cfg.llm)?;
        }
        let translation_target = cfg.llm.translation_target();
        anyhow::ensure!(
            !has(Component::Translation) || translation_target.is_some(),
            tr!("补做翻译需要选择翻译语言 / Choose a translation language to redo the translation")
        );
        if let Some(prior) = prior_work {
            self.import_prior_work(request, &base, components, prior)
                .await?;
        }
        // Copy verified base images into a separate namespace; a failed new screenshot pass
        // cannot mix old and new frames or change the note before it is replaced.
        let images: Vec<String> = document
            .sections
            .iter()
            .filter(|section| !section.image.is_empty())
            .map(|section| section.image.clone())
            .collect();
        {
            let (base_dir, out_dir, timeline, control) = (
                base_dir.to_path_buf(),
                job.dir.clone(),
                job.timeline_path(),
                job.control.clone(),
            );
            blocking(move || {
                crate::pool::map_pool(images, 4, &control, |image| {
                    let source = artifact::safe_asset_path(&base_dir, &image)?;
                    let dest = out_dir.join(base_image(&image));
                    std::fs::create_dir_all(dest.parent().context("asset path")?)?;
                    if !dest.exists() {
                        std::fs::copy(source, &dest)?;
                    }
                    Ok(())
                })?;
                if base_dir.join("timeline.jsonl").is_file() {
                    std::fs::copy(base_dir.join("timeline.jsonl"), timeline)?;
                }
                Ok(())
            })
            .await?;
        }
        for section in &mut document.sections {
            if !section.image.is_empty() {
                section.image = base_image(&section.image);
            }
        }
        let mut outcomes = base.outcomes.clone();
        outcomes.exports.clear();
        document.meta.title = request.title.clone();
        let mut new_screenshots = false;
        if has(Component::Screenshots) {
            job.control.check()?;
            crate::error::require_cmd("ffmpeg")?;
            crate::error::require_cmd("ffprobe")?;
            // A local source that has moved can only be replaced by its verified copy.
            if !self.is_local
                && (Path::new(&request.source).is_absolute()
                    || request.source_id.starts_with("local:"))
            {
                let (media, marker) = (job.media_path(), job.dir.join("media.sha256"));
                anyhow::ensure!(
                    blocking(move || verified_file(&media, &marker)).await?,
                    tr!(
                        "原视频已移动或删除，无法补做截图；原笔记仍可阅读 / Original video is unavailable; the saved note remains readable"
                    )
                );
            }
            let (media_path, _) = self.acquire_media().await?;
            let (frames, screenshots) = frames_outcome(self.cached_frames(&media_path).await);
            if !frames.is_empty() {
                let speech = all_speech(&document.sections);
                document.sections = timeline::merge(frames, speech, document.meta.duration);
                outcomes.screenshots = screenshots;
                new_screenshots = true;
            } else if components.len() == 1 {
                anyhow::bail!(tr!(
                    "截图失败，原笔记不受影响 / Screenshot extraction failed; the original note is unaffected"
                ));
            }
            // Without new screenshots the note keeps its own, and their record.
        }
        // What the note says before any AI part is redone: a part that finishes
        // nothing more falls back to it.
        let before_ai = (document.sections.clone(), document.summary.clone());
        if has(Component::Proofreading) {
            job.control.check()?;
            // The original complete transcript is a verified note asset. Rebuild the
            // original batches (including entries an earlier successful polish removed).
            if base_dir.join("timeline.jsonl").is_file() {
                let mut speech = Vec::new();
                for line in std::fs::read_to_string(base_dir.join("timeline.jsonl"))?
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                {
                    if let timeline::TimelineEvent::Speech(event) = serde_json::from_str(line)? {
                        speech.push(event);
                    }
                }
                execution::validate_events(&speech)?;
                let frames = document
                    .sections
                    .iter()
                    .filter(|section| !section.image.is_empty())
                    .map(|section| timeline::FrameEvent {
                        t: section.t,
                        image: section.image.clone(),
                    })
                    .collect::<Vec<_>>();
                document.sections = build_sections(frames, speech, document.meta.duration);
                timeline::coalesce_sections(&mut document.sections);
            } else {
                for event in document
                    .sections
                    .iter_mut()
                    .flat_map(|section| &mut section.speech)
                {
                    if let Some(raw) = &event.raw {
                        event.text = raw.clone();
                    }
                    // Proofreading changes the text; its translation is made again.
                    event.translation = None;
                }
            }
            let sections = std::mem::take(&mut document.sections);
            document.sections = self.polish_step(sections, &mut outcomes).await?;
        }
        // Proofreading rebuilt the paragraphs from the original transcript, so their
        // translation follows it. Alone, translation redoes the saved paragraphs: the
        // batches that finished are the same requests and come back from their receipts.
        if let Some(language) = translation_target
            .filter(|_| has(Component::Proofreading) || has(Component::Translation))
        {
            job.control.check()?;
            let sections = std::mem::take(&mut document.sections);
            document.sections = self
                .translate_step(sections, language, &mut outcomes)
                .await?;
        }
        if has(Component::Summary) {
            document.summary = self
                .summarize_step(&document.sections, &document.meta, &mut outcomes)
                .await?;
        }
        // A redone AI part that finished nothing more than the note had must not
        // replace what the note had: a failed summary never clears the old one.
        let finished = |outcome: &Outcome| match outcome.status {
            Status::Succeeded => usize::MAX,
            Status::Partial => outcome.completed.unwrap_or(0),
            Status::NotRequested | Status::Failed => 0,
        };
        let part = |outcomes: &Outcomes, component: Component| match component {
            Component::Proofreading => outcomes.proofreading.clone(),
            Component::Translation => outcomes.translation.clone(),
            _ => outcomes.summary.clone(),
        };
        // Making a finished part again (a new summary of a summarized note) is
        // something new too; only the parts that were unfinished must move on.
        let improved = |component: Component| {
            let (now, before) = (part(&outcomes, component), part(&base.outcomes, component));
            finished(&now) > finished(&before)
                || (has(component)
                    && now.status == Status::Succeeded
                    && before.status == Status::Succeeded)
        };
        let text_redone = has(Component::Proofreading)
            && (improved(Component::Proofreading)
                || (translation_target.is_some() && improved(Component::Translation)));
        let translation_redone = has(Component::Translation) && improved(Component::Translation);
        let summary_redone = has(Component::Summary) && improved(Component::Summary);
        // Why this run's parts ended as they did, before a part that finished
        // nothing more falls back to the note's own record.
        let this_run = outcomes.clone();
        if has(Component::Proofreading) && !text_redone {
            document.sections = before_ai.0;
            outcomes.proofreading = base.outcomes.proofreading.clone();
            outcomes.translation = base.outcomes.translation.clone();
        }
        if has(Component::Summary) && !summary_redone {
            document.summary = before_ai.1;
            outcomes.summary = base.outcomes.summary.clone();
        }
        if !(text_redone
            || translation_redone
            || summary_redone
            || new_screenshots
            || has(Component::Exports))
        {
            let mut redone: Vec<Component> =
                components.iter().copied().filter(|c| c.is_ai()).collect();
            if has(Component::Proofreading) && translation_target.is_some() {
                redone.push(Component::Translation);
            }
            let unfinished = redone
                .iter()
                .map(|&component| part(&this_run, component))
                .find(|outcome| outcome.status != Status::Succeeded);
            // A retry of a partial note's unfinished parts that finished nothing
            // more leaves the note as it was: still partial, for the same reasons,
            // with the same retry. It reports that note, unchanged, instead of an
            // error. A request left without a known result still stops the task
            // for the person's decision.
            if base.partial
                && !job.control.has_unresolved()
                && unfinished
                    .as_ref()
                    .is_none_or(|outcome| outcome.kind != Some(artifact::FailureKind::Uncertain))
            {
                tracing::warn!(
                    "补做没有完成更多内容，笔记未改动 / Nothing more was finished; the note is unchanged"
                );
                // The note's record, with each part retried now saying why it is
                // still unfinished this time (a rejected key, a rate limit).
                let mut reported = base.outcomes.clone();
                for &component in &redone {
                    let now = part(&this_run, component);
                    if now.status != Status::Succeeded && now.status != Status::NotRequested {
                        match component {
                            Component::Proofreading => reported.proofreading = now,
                            Component::Translation => reported.translation = now,
                            _ => reported.summary = now,
                        }
                    }
                }
                progress::emit(&progress::Event::Done(Box::new(progress::Done {
                    task_id: request.task_id.clone(),
                    course_id: base.course_id.clone(),
                    out_dir: base_dir.to_path_buf(),
                    manifest: Some(base_dir.join("manifest.json")),
                    title: base.title.clone(),
                    slides: base.frames.len(),
                    outputs: base.outputs.iter().map(PathBuf::from).collect(),
                    partial: Some(true),
                    outcomes: Some(reported),
                    ..done_counts(&document.sections, self.started)
                })));
                return Ok(());
            }
            let message = unfinished
                .as_ref()
                .and_then(|outcome| outcome.message.clone())
                .unwrap_or_else(|| {
                    tr!(
                        "补做没有完成更多内容，笔记未改动 / Nothing more was finished; the note is unchanged"
                    )
                    .into()
                });
            // Keep the recorded cause so the error event's code matches it.
            return Err(crate::dispatch::Failure {
                kind: unfinished
                    .and_then(|outcome| outcome.kind)
                    .unwrap_or(artifact::FailureKind::Local),
                ..crate::dispatch::Failure::local(message)
            }
            .into());
        }
        job.control.check()?;
        let formats: &[_] = if has(Component::Exports) {
            &cfg.formats
        } else {
            &[]
        };
        let publication = self
            .publish(
                &document.meta,
                &document.sections,
                document.summary.as_ref(),
                formats,
                outcomes,
            )
            .await?;
        self.emit_done(&publication, &document.sections);
        Ok(())
    }

    /// Export-only reprocessing writes each export into the note's `exports/`,
    /// one file at a time, and leaves the rest of the note as it is.
    fn export_only(
        &self,
        request: &execution::Request,
        note_dir: &Path,
        base: &artifact::Manifest,
        document: &artifact::Document,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.cfg.formats.is_empty(),
            tr!("请选择需要导出的文件格式 / Select an export format")
        );
        let mut exports = std::collections::BTreeMap::new();
        let mut outputs = Vec::new();
        let mut kept_edits = Vec::new();
        for format in &self.cfg.formats {
            let outcome = match export_for_task(note_dir, *format, self.job) {
                Ok(written) => {
                    outputs.push(written.path);
                    kept_edits.extend(written.kept_edit.map(PathBuf::from));
                    Outcome::succeeded()
                }
                Err(error) => Outcome::from_error(&error),
            };
            exports.insert(format.to_string(), outcome);
        }
        let partial = exports
            .values()
            .any(|outcome| outcome.status == Status::Failed);
        self.write_json(
            "export-result.json",
            &serde_json::json!({"schema":1,"outputs":outputs,"outcomes":exports}),
        )?;
        progress::emit(&progress::Event::Done(Box::new(progress::Done {
            operation: Some("exports".into()),
            task_id: request.task_id.clone(),
            course_id: base.course_id.clone(),
            out_dir: note_dir.to_path_buf(),
            manifest: Some(note_dir.join("manifest.json")),
            title: document.meta.title.clone(),
            slides: base.frames.len(),
            outputs,
            partial: Some(partial),
            outcomes: Some(Outcomes {
                exports,
                ..Default::default()
            }),
            kept_edits,
            ..done_counts(&document.sections, self.started)
        })));
        Ok(())
    }

    async fn subtitles(
        &self,
        input: SubtitleInput,
    ) -> Result<Option<(Vec<timeline::TranscriptEvent>, TranscriptOrigin)>> {
        let cfg = self.cfg;
        let probed = match input {
            SubtitleInput::Selected(selected) => {
                // Selected events were validated with the request.
                if let Some(events) = selected {
                    return Ok(Some((events, TranscriptOrigin::SelectedSubtitle)));
                }
                anyhow::ensure!(
                    cfg.transcript_source != config::TranscriptSource::Subtitle,
                    tr!("任务缺少已选字幕 / Selected subtitles are missing from this task")
                );
                return Ok(None);
            }
            SubtitleInput::Discover(probed) => probed,
        };
        if cfg.transcript_source == config::TranscriptSource::Asr {
            return Ok(None);
        }
        let found = if self.is_local {
            fetch::sidecar_subtitle(Path::new(&cfg.url))
        } else {
            let video = probed.context(tr!(
                "缺少视频信息，请重新读取 / Missing video info; please probe again"
            ))?;
            fetch::fetch_subtitle(&video, &self.job.dir, &self.job.control)
                .await
                .context(tr!("读取视频字幕失败 / Could not read video subtitles"))?
        };
        match found {
            Some(file) => {
                let content = crate::subtitle::read_subtitle_text(&file.path)
                    .context(tr!("无法读取字幕文件 / Cannot read subtitle file"))?;
                let events = crate::subtitle::parse_subtitle(&content);
                execution::validate_events(&events)?;
                let origin = if file.auto {
                    TranscriptOrigin::AutoCaption
                } else {
                    TranscriptOrigin::Subtitle
                };
                Ok(Some((events, origin)))
            }
            None if cfg.transcript_source == config::TranscriptSource::Subtitle => {
                anyhow::bail!(tr!("未找到字幕。请选择已有字幕，或改用语音识别"))
            }
            None => Ok(None),
        }
    }

    /// Reuses what an earlier task for the same note already paid for: its
    /// receipts for the requested AI components (each failed one authorized for
    /// one more attempt) and its verified video download.
    async fn import_prior_work(
        &self,
        request: &execution::Request,
        base: &artifact::Manifest,
        components: &[execution::Component],
        prior: &Path,
    ) -> Result<()> {
        use execution::Component;
        let job = self.job;
        let has = |component: Component| components.contains(&component);
        let identity: serde_json::Value =
            serde_json::from_slice(&std::fs::read(prior.join("task-identity.json"))?)?;
        anyhow::ensure!(
            identity["source_id"].as_str() == Some(request.source_id.as_str()),
            tr!("旧进度与此视频来源不符 / Prior work belongs to another source")
        );
        // The note's own task, or a repair of that note that left it as it was:
        // its receipts hold what the note's task paid for, and what it added.
        let repair_of_base = identity["operation"]["kind"] == "reprocess"
            && identity["operation"]["base_version_id"].as_str() == Some(base.task_id.as_str());
        anyhow::ensure!(
            repair_of_base
                || identity["task_id"]
                    .as_str()
                    .is_none_or(|id| id == base.task_id),
            tr!("旧进度与原笔记任务不符 / Prior work belongs to another task")
        );
        let ledger_dir = job.dir.join("requests");
        std::fs::create_dir_all(&ledger_dir)?;
        for mut receipt in crate::dispatch::receipts(prior)? {
            // Translation is redone with proofreading as well as on its own.
            let needed = Component::parse(&receipt.purpose).is_some_and(|c| {
                c.is_ai()
                    && (has(c)
                        || (c == Component::Translation
                            && has(Component::Proofreading)
                            && self.cfg.llm.translation_target().is_some()))
            });
            if needed
                && imports_prior_receipt(&receipt, request.service_versions.get("llm"))
                && execution::valid_id(&receipt.stable_id)
            {
                let destination = ledger_dir.join(format!("{}.json", receipt.stable_id));
                if !destination.exists() {
                    // This new task explicitly requests another attempt at selected
                    // failed components. Persist authorization only while importing;
                    // restarting this task must never refresh a consumed authorization.
                    receipt.retry_authorized = matches!(
                        receipt.state,
                        crate::dispatch::State::Failed | crate::dispatch::State::Rejected
                    )
                    .then(|| receipt.request_id.clone());
                    crate::fsio::atomic_write(&destination, &serde_json::to_vec_pretty(&receipt)?)?;
                }
            }
        }
        if !job.media_path().exists() {
            let (prior, media, marker) = (
                prior.to_path_buf(),
                job.media_path(),
                job.dir.join("media.sha256"),
            );
            blocking(move || {
                if verified_file(&prior.join("media.mp4"), &prior.join("media.sha256"))? {
                    std::fs::copy(prior.join("media.mp4"), &media)?;
                    save_file_digest(&media, &marker)?;
                }
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    /// Tools and credentials the chosen path needs, and the speech model cache,
    /// all checked before any full media download.
    async fn check_tools(&self, needs_speech: bool, credentials: Credentials<'_>) -> Result<()> {
        let cfg = self.cfg;
        if needs_speech {
            let (require_key, allow_environment) = match credentials {
                Credentials::Cli(_) => (true, true),
                Credentials::Task {
                    allow_unauthenticated_asr,
                } => (!allow_unauthenticated_asr, false),
            };
            cfg.validate_asr_with_auth(require_key, allow_environment)?;
            if matches!(
                cfg.provider,
                config::AsrProvider::Gpu | config::AsrProvider::Cpu
            ) {
                crate::error::require_cmd("llama-server")?;
            }
        }
        crate::error::require_cmd("ffmpeg")?;
        crate::error::require_cmd("ffprobe")?;
        if !self.is_local {
            crate::error::require_ytdlp()?;
        }
        if needs_speech && cfg.provider != config::AsrProvider::Api {
            let model = cfg
                .asr_model
                .as_deref()
                .unwrap_or(config::default_model(cfg.provider))
                .to_owned();
            let (provider, root) = (cfg.provider, cfg.model_dir.clone());
            match credentials {
                Credentials::Cli(download) => {
                    let offer = {
                        let (model, root) = (model.clone(), root.clone());
                        blocking(move || crate::models::missing(provider, &model, &root)).await?
                    };
                    // Asked before the stage starts, so no progress line covers the question.
                    let Some(offer) = offer else {
                        progress::start(Stage::ModelPrepare);
                        progress::done(Stage::ModelPrepare);
                        return Ok(());
                    };
                    let remedy = match download {
                        ModelDownload::Allowed => None,
                        ModelDownload::Ask(ask) => {
                            (!ask(&offer)?).then(|| Remedy::Declined(offer.prepare_command.clone()))
                        }
                        ModelDownload::Refused => {
                            Some(Remedy::Command(offer.prepare_command.clone()))
                        }
                    };
                    if let Some(remedy) = remedy {
                        return Err(ModelRequired {
                            provider,
                            model: offer.model,
                            remedy,
                        }
                        .into());
                    }
                    progress::start(Stage::ModelPrepare);
                    crate::models::ensure_cache(provider, &model, &root, &self.job.control).await?;
                    progress::done(Stage::ModelPrepare);
                }
                // A task never downloads a model on the way: the person agrees to that
                // download first, when the app asks them to prepare it.
                Credentials::Task { .. } => {
                    progress::start(Stage::ModelPrepare);
                    blocking(move || crate::models::require_cache(provider, &model, &root)).await?;
                    progress::done(Stage::ModelPrepare);
                }
            }
        }
        Ok(())
    }

    /// The video to process, and whether a verified download already existed.
    async fn acquire_media(&self) -> Result<(PathBuf, bool)> {
        let (cfg, job) = (self.cfg, self.job);
        if self.is_local {
            return Ok((PathBuf::from(&cfg.url), false));
        }
        let media_path = job.media_path();
        let existed = {
            let (job, no_download) = (job.clone(), cfg.no_download);
            blocking(move || prepare_cached_media(&job, no_download)).await?
        };
        job.control.check()?;
        if !existed {
            anyhow::ensure!(
                !cfg.no_download,
                tr!(
                    "找不到已下载的视频，请允许重新下载 / The downloaded video is missing; allow downloading it again"
                )
            );
            crate::error::require_ytdlp()?;
            progress::start(Stage::Download);
            fetch::download(
                &cfg.url,
                &media_path,
                cfg.max_height,
                tracing::enabled!(tracing::Level::DEBUG),
                &job.control,
            )
            .await?;
            let (media, marker) = (media_path.clone(), job.dir.join("media.sha256"));
            blocking(move || save_file_digest(&media, &marker)).await?;
            progress::done(Stage::Download);
        }
        Ok((media_path, existed))
    }

    /// The transcript: a saved one, the selected subtitles, or speech recognition.
    async fn transcript(
        &self,
        media_path: &Path,
        selected: Option<(Vec<timeline::TranscriptEvent>, TranscriptOrigin)>,
    ) -> Result<TranscriptCache> {
        let job = self.job;
        let cache_path = job.dir.join("transcript.json");
        let cache = match cached_transcript(&cache_path)?
            .or_else(|| selected.map(|(events, source)| TranscriptCache { source, events }))
        {
            Some(cache) => cache,
            None => {
                progress::start(Stage::Audio);
                let audio = job.audio_path();
                // A verified marker proves audio extraction completed; a bare WAV may be truncated.
                let marker = job.dir.join("audio.sha256");
                let verified = {
                    let (audio, marker) = (audio.clone(), marker.clone());
                    blocking(move || verified_file(&audio, &marker)).await?
                };
                if !verified {
                    media::extract_audio(media_path, &audio).await?;
                    let audio = audio.clone();
                    blocking(move || save_file_digest(&audio, &marker)).await?;
                }
                progress::done(Stage::Audio);
                job.control.check()?;
                progress::start(Stage::Transcribe);
                let events = asr::run(self.cfg, &audio, job).await?;
                progress::done(Stage::Transcribe);
                TranscriptCache {
                    source: TranscriptOrigin::Asr,
                    events,
                }
            }
        };
        // 全空转写不入缓存：空缓存会在下次运行时挡住重新识别
        if timeline::has_readable_events(&cache.events) {
            let bytes = serde_json::to_vec_pretty(&cache)?;
            let cache_path = cache_path.clone();
            blocking(move || crate::fsio::atomic_write(&cache_path, &bytes)).await?;
        }
        Ok(cache)
    }

    /// Optional AI work on the assembled note: proofreading, translation, summary.
    async fn enrich(
        &self,
        mut sections: Vec<timeline::Section>,
        meta: &VideoMeta,
        outcomes: &mut Outcomes,
    ) -> Result<(Vec<timeline::Section>, Option<summarize::Summary>)> {
        let (llm, control) = (&self.cfg.llm, &self.job.control);
        control.check()?;
        if llm.enabled {
            sections = self.polish_step(sections, outcomes).await?;
        }
        control.check()?;
        if let Some(language) = llm.translation_target() {
            sections = self.translate_step(sections, language, outcomes).await?;
        }
        control.check()?;
        let summary = if llm.summarize {
            self.summarize_step(&sections, meta, outcomes).await?
        } else {
            None
        };
        Ok((sections, summary))
    }

    /// The timeline a later reprocess task starts from.
    async fn save_timeline(
        &self,
        frames: &[timeline::FrameEvent],
        events: &[timeline::TranscriptEvent],
    ) -> Result<()> {
        let (path, frames, events) = (self.job.timeline_path(), frames.to_vec(), events.to_vec());
        blocking(move || timeline::write_jsonl(&path, &frames, &events)).await
    }

    async fn cached_frames(&self, media: &Path) -> Result<Vec<timeline::FrameEvent>> {
        let job = self.job;
        let cache_path = job.dir.join("screenshots.json");
        if cache_path.is_file() {
            let cache: FramesCache = serde_json::from_slice(&std::fs::read(&cache_path)?).context(
                tr!("截图进度损坏，原文件已保留 / Screenshot checkpoint is damaged"),
            )?;
            if !cache.frames.is_empty() {
                let (out_dir, control) = (job.dir.clone(), job.control.clone());
                if blocking(move || Ok(verified_cache_files(&out_dir, &cache.files, &control)))
                    .await?
                {
                    return Ok(cache.frames);
                }
            }
        }
        job.control.check()?;
        let frames = scene::run(self.cfg, media, job).await?;
        let files = {
            let (out_dir, images) = (
                job.dir.clone(),
                frames.iter().map(|f| f.image.clone()).collect::<Vec<_>>(),
            );
            blocking(move || {
                images
                    .into_iter()
                    .map(|image| {
                        let path = artifact::safe_asset_path(&out_dir, &image)?;
                        Ok((image, execution::file_digest(&path)?))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .await?
        };
        self.write_json(
            "screenshots.json",
            &FramesCache {
                frames: frames.clone(),
                files,
            },
        )?;
        Ok(frames)
    }

    /// Proofreads off-thread. If the polished body is unreadable, rolls back to the
    /// originals and reports the whole attempt as failed: nothing was kept.
    async fn polish_step(
        &self,
        mut sections: Vec<timeline::Section>,
        outcomes: &mut Outcomes,
    ) -> Result<Vec<timeline::Section>> {
        progress::start(Stage::Llm);
        let original = sections.clone();
        let (llm, root, control) = (
            self.cfg.llm.clone(),
            self.job.dir.clone(),
            self.job.control.clone(),
        );
        let (mut sections, mut report) = blocking(move || {
            crate::llm::polish_sections_report(&mut sections, &root, &llm, &control)
                .map(|report| (sections, report))
        })
        .await?;
        if !artifact::has_readable_body(&sections) {
            sections = original;
            report.succeeded = 0;
            report.failed = report.attempted;
        }
        outcomes.proofreading = outcome_from_report(&report);
        progress::done(Stage::Llm);
        Ok(sections)
    }

    /// Adds translations for a bilingual note, unless the transcript is already
    /// in the target language. Failed batches stay untranslated.
    async fn translate_step(
        &self,
        mut sections: Vec<timeline::Section>,
        language: &'static str,
        outcomes: &mut Outcomes,
    ) -> Result<Vec<timeline::Section>> {
        let mut sample = String::new();
        for event in sections.iter().flat_map(|section| &section.speech) {
            if sample.len() >= LANGUAGE_SAMPLE_CHARS {
                break;
            }
            sample.push_str(&event.text);
            sample.push(' ');
        }
        let detected = crate::language::detect(&sample);
        if crate::language::already_in(detected, language) {
            tracing::info!(
                language,
                "文字已是目标语言，无需翻译 / The transcript is already in the target language; not translating"
            );
            outcomes.translation = Outcome::not_requested();
            return Ok(sections);
        }
        progress::start(Stage::Translate);
        let (llm, control) = (self.cfg.llm.clone(), self.job.control.clone());
        let (sections, report) = blocking(move || {
            crate::llm::translate_sections_report(&mut sections, &llm, language, &control)
                .map(|report| (sections, report))
        })
        .await?;
        outcomes.translation = Outcome {
            message: report_message(
                tr!(
                    "翻译未全部完成（{}/{} 失败）",
                    report.failed,
                    report.attempted
                ),
                &report,
            ),
            ..outcome_from_report(&report)
        };
        progress::done(Stage::Translate);
        Ok(sections)
    }

    async fn summarize_step(
        &self,
        sections: &[timeline::Section],
        meta: &VideoMeta,
        outcomes: &mut Outcomes,
    ) -> Result<Option<summarize::Summary>> {
        self.job.control.check()?;
        progress::start(Stage::Summary);
        let speech = all_speech(sections);
        match summarize::summarize(&self.cfg.llm, &speech, meta, &self.job.control).await {
            Ok(summary) => {
                outcomes.summary = Outcome::succeeded();
                progress::done(Stage::Summary);
                Ok(Some(summary))
            }
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    "生成摘要未完成 / Summary generation incomplete"
                );
                outcomes.summary =
                    Outcome::from_error_with_message(&error, summarize::failure_message(&error));
                Ok(None)
            }
        }
    }
}

/// Screenshots are optional: a failure is recorded and the note goes on without them.
fn frames_outcome(
    result: Result<Vec<timeline::FrameEvent>>,
) -> (Vec<timeline::FrameEvent>, Outcome) {
    match result {
        Ok(frames) if !frames.is_empty() => (frames, Outcome::succeeded()),
        Ok(_) => (Vec::new(), Outcome::failed(tr!(NO_SCREENSHOTS))),
        Err(error) => (Vec::new(), Outcome::from_error(&error)),
    }
}

fn export_for_task(
    note_dir: &Path,
    format: config::OutputFormat,
    job: &Job,
) -> Result<artifact::ExportWrite> {
    // Rendered in the work directory, then written into the note in one step.
    let scratch = tempfile::tempdir_in(&job.dir)?;
    let rendered = scratch.path().join(crate::portable::file_name(format));
    crate::portable::export_in_task(note_dir, format, &rendered, &job.control)?;
    let bytes = std::fs::read(&rendered)?;
    artifact::write_export(note_dir, format, &bytes)
}

/// Where a repair keeps a screenshot of the note it repairs: under `base/`, once,
/// however many repairs the note has been through.
fn base_image(image: &str) -> String {
    let mut name = image;
    while let Some(rest) = name.strip_prefix("base/") {
        name = rest;
    }
    format!("base/{name}")
}

/// File hashing, publishing and AI batches block for long; off the async workers,
/// the sibling screenshot/transcript branch and control polling keep running.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context(tr!("后台工作中断 / Background work was interrupted"))?
}

fn verified_file(path: &Path, marker: &Path) -> Result<bool> {
    if !path.is_file() || !marker.is_file() {
        return Ok(false);
    }
    Ok(std::fs::read_to_string(marker)? == execution::file_digest(path)?)
}

fn save_file_digest(path: &Path, marker: &Path) -> Result<()> {
    crate::fsio::atomic_write(marker, execution::file_digest(path)?.as_bytes())
}

/// A completed download has a durable content marker. Preserve unverifiable files
/// separately before the caller downloads again, so recovery never consumes a partial
/// download or destroys the old bytes while trying to repair it.
fn prepare_cached_media(job: &Job, no_download: bool) -> Result<bool> {
    let media = job.media_path();
    if !media.is_file() {
        return Ok(false);
    }
    if verified_file(&media, &job.dir.join("media.sha256"))? {
        return Ok(true);
    }
    anyhow::ensure!(
        !no_download,
        tr!(
            "已保存的视频无法校验；需要重新下载才能继续 / Cached video cannot be verified; downloading is required"
        )
    );
    let archives = job.dir.join(".previous-media");
    std::fs::create_dir_all(&archives)?;
    let archive = tempfile::Builder::new()
        .prefix("unverified-")
        .tempdir_in(archives)?;
    std::fs::rename(&media, archive.path().join("media.mp4"))?;
    let archive_path = archive.keep();
    artifact::sync_dir(&archive_path)?;
    artifact::sync_dir(&job.dir)?;
    Ok(false)
}

/// Every transcript event in document order.
pub(crate) fn all_speech(sections: &[timeline::Section]) -> Vec<timeline::TranscriptEvent> {
    sections
        .iter()
        .flat_map(|section| section.speech.iter().cloned())
        .collect()
}

/// Build sections from optional frames and a transcript, keeping the single
/// "no screenshots" shape in one place.
fn build_sections(
    frames: Vec<timeline::FrameEvent>,
    speech: Vec<timeline::TranscriptEvent>,
    duration: f64,
) -> Vec<timeline::Section> {
    if frames.is_empty() {
        vec![timeline::Section {
            t: 0.,
            end: duration,
            image: String::new(),
            speech,
        }]
    } else {
        timeline::merge(frames, speech, duration)
    }
}

/// Whether a repair takes a request record of the work it repairs. A finished
/// request comes along whichever version of the service answered it: a repair runs
/// with the current service, and the same request to the same endpoint replays its
/// answer instead of being paid for again (see the ledger's earlier versions). An
/// unfinished one belongs to the version it was made with, whose retry it
/// authorizes.
fn imports_prior_receipt(receipt: &crate::dispatch::Receipt, current: Option<&String>) -> bool {
    receipt.state == crate::dispatch::State::Completed
        || current.map_or(
            receipt
                .service_version
                .starts_with(crate::dispatch::SNAPSHOT_PREFIX),
            |version| version == &receipt.service_version,
        )
}

/// 读取转写缓存；全空的缓存是上次失败运行的残留（端点持续返回空文本却
/// 无报错），丢弃并重新识别，而不是用"所选字幕没有可读文字"误导用户。
fn cached_transcript(cache_path: &Path) -> Result<Option<TranscriptCache>> {
    if !cache_path.is_file() {
        return Ok(None);
    }
    let cache: TranscriptCache = serde_json::from_slice(&std::fs::read(cache_path)?).context(
        tr!("已保存的文字损坏，原文件已保留 / Saved transcript is damaged"),
    )?;
    if timeline::has_readable_events(&cache.events) {
        execution::validate_events(&cache.events)?;
        return Ok(Some(cache));
    }
    tracing::warn!(
        "已保存的文字为空，丢弃并重新识别 / Saved transcript is empty; discarding it and transcribing again"
    );
    let _ = std::fs::remove_file(cache_path);
    Ok(None)
}

/// A batch report's message: nothing when every batch worked, else `base` and the
/// first error.
fn report_message(base: String, report: &crate::llm::PolishReport) -> Option<String> {
    (report.failed > 0).then(|| match &report.note {
        Some(note) => tr!("{}；首个错误：{}", base, note),
        None => base,
    })
}

/// Map a polish run onto the version outcome.
///
/// Its kind is the first unfinished part's, so the desktop words the cause in
/// its own language. A part the service refused while the rest was accepted is
/// about that part's text, not the key, model or settings, which work: it names
/// no kind that would send the person to repair the service.
fn outcome_from_report(report: &crate::llm::PolishReport) -> Outcome {
    let status = if report.failed == 0 {
        Status::Succeeded
    } else if report.succeeded > 0 {
        Status::Partial
    } else {
        Status::Failed
    };
    let kind = report
        .kind
        .filter(|_| status != Status::Succeeded)
        .filter(|kind| {
            status == Status::Failed
                || !matches!(
                    kind,
                    artifact::FailureKind::Authentication
                        | artifact::FailureKind::Model
                        | artifact::FailureKind::Parameters
                )
        });
    Outcome {
        kind,
        status,
        message: report_message(
            tr!(
                "校对未全部完成（{}/{} 失败），原文已保留",
                report.failed,
                report.attempted
            ),
            report,
        ),
        completed: Some(report.succeeded),
        total: Some(report.attempted),
        first_failure: report.first_failure.clone().filter(|_| report.failed > 0),
    }
}

/// Screenshot checkpoints are verified by full-content digests, in parallel so
/// resuming a large cache is not a serial wait.
fn verified_cache_files(out_dir: &Path, files: &[(String, String)], control: &TaskControl) -> bool {
    crate::pool::map_pool(
        files.iter().collect(),
        FRAME_DIGEST_LANES,
        control,
        |(path, digest)| {
            let actual = artifact::safe_asset_path(out_dir, path)
                .and_then(|p| execution::file_digest(&p))?;
            anyhow::ensure!(actual == *digest, "digest mismatch");
            Ok(())
        },
    )
    .is_ok()
}

fn sanitize_stem(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("local")
        .chars()
        .take(STEM_MAX_CHARS)
        .collect()
}

/// 结束时是否允许删除媒体文件：仅当「本次运行下载的」且未要求保留。
/// --no-download 复用的既有文件、本地输入永远不删。
fn should_delete_media(
    is_local: bool,
    no_download: bool,
    media_existed: bool,
    keep_video: bool,
) -> bool {
    !is_local && !no_download && !media_existed && !keep_video
}

#[cfg(test)]
mod tests {
    use super::{run, should_delete_media};

    /// A repair after the AI service was edited (a new key or name) takes the
    /// finished requests of the note it repairs, so their answers are replayed rather
    /// than paid for again; an unfinished one stays with its own version.
    #[test]
    fn a_repair_takes_finished_requests_of_another_service_version() {
        use crate::dispatch::{Receipt, State};
        let receipt = |state: State| Receipt {
            schema: 1,
            stable_id: "id".into(),
            request_id: "id.1".into(),
            purpose: "proofreading".into(),
            description: String::new(),
            service_version: "version-1".into(),
            attempt: 1,
            state,
            http_status: None,
            response: None,
            message: None,
            unsupported_response_format: false,
            refused_parameter: None,
            repeats: false,
            unusable: false,
            detail: None,
            retry_authorized: None,
        };
        let current = "version-2".to_string();
        assert!(super::imports_prior_receipt(
            &receipt(State::Completed),
            Some(&current)
        ));
        assert!(!super::imports_prior_receipt(
            &receipt(State::Failed),
            Some(&current)
        ));
        let same = "version-1".to_string();
        assert!(super::imports_prior_receipt(
            &receipt(State::Failed),
            Some(&same)
        ));
    }

    #[test]
    fn batch_tasks_discover_subtitles_at_runtime_only_without_confirmed_text() {
        let mut cfg = crate::options::resolve(
            "https://example.invalid/video".into(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        cfg.transcript_source = crate::config::TranscriptSource::Auto;
        // 批量 Auto：无预选字幕 → 任务内探测并发现
        assert!(super::discovers_subtitles_at_runtime(&None, &cfg, false));
        // 强制字幕（无预选内容）→ 任务内发现，找不到字幕再报错
        cfg.transcript_source = crate::config::TranscriptSource::Subtitle;
        assert!(super::discovers_subtitles_at_runtime(&None, &cfg, false));
        // 强制识别 / 已带字幕内容 / 本地文件：保持原路径（桌面单视频流程不变）
        cfg.transcript_source = crate::config::TranscriptSource::Asr;
        assert!(!super::discovers_subtitles_at_runtime(&None, &cfg, false));
        cfg.transcript_source = crate::config::TranscriptSource::Auto;
        assert!(!super::discovers_subtitles_at_runtime(
            &Some(Vec::new()),
            &cfg,
            false
        ));
        assert!(!super::discovers_subtitles_at_runtime(&None, &cfg, true));
    }

    #[test]
    fn recovery_archives_unverified_media_and_reuses_only_matching_content() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = crate::options::resolve(
            "https://example.invalid/video".into(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        let job = crate::job::Job::detached(root.path());
        cfg.no_download = false;
        let media = job.media_path();
        std::fs::write(&media, b"unverified original bytes").unwrap();
        assert!(!super::prepare_cached_media(&job, cfg.no_download).unwrap());
        assert!(!media.exists());
        let archive = std::fs::read_dir(root.path().join(".previous-media"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("media.mp4");
        assert_eq!(
            std::fs::read(archive).unwrap(),
            b"unverified original bytes"
        );
        std::fs::write(&media, b"completed replacement").unwrap();
        super::save_file_digest(&media, &root.path().join("media.sha256")).unwrap();
        assert!(super::prepare_cached_media(&job, cfg.no_download).unwrap());
        std::fs::write(&media, b"changed externally").unwrap();
        cfg.no_download = true;
        assert!(super::prepare_cached_media(&job, cfg.no_download).is_err());
        assert_eq!(std::fs::read(media).unwrap(), b"changed externally");
    }

    #[test]
    fn repaired_screenshots_keep_one_base_folder() {
        assert_eq!(super::base_image("frames/a.jpg"), "base/frames/a.jpg");
        assert_eq!(super::base_image("base/frames/a.jpg"), "base/frames/a.jpg");
        assert_eq!(
            super::base_image("base/base/frames/a.jpg"),
            "base/frames/a.jpg"
        );
    }

    #[test]
    fn never_delete_files_we_did_not_download() {
        // --no-download 复用既有文件：不删（旧行为会删！）
        assert!(!should_delete_media(false, true, true, false));
        // 上次运行已存在的文件（resume 场景）：不删
        assert!(!should_delete_media(false, false, true, false));
        // 本地输入：不删
        assert!(!should_delete_media(true, false, false, false));
        // 本次真下载 + keep_video：不删
        assert!(!should_delete_media(false, false, false, true));
        // 本次真下载 + 未要求保留：删
        assert!(should_delete_media(false, false, false, false));
    }

    /// 失败也写诊断 run.json（success:false + 错误全文）。
    /// 用损坏的本地视频让 pipeline 在 out_dir 确定之后（场景检测阶段）必然失败。
    #[test]
    #[ignore = "needs ffmpeg, ffprobe and llama-server on PATH; run with --ignored"]
    fn failure_writes_diagnostic_run_json() {
        // 缺任何一个工具都会在 out_dir 确定之前的预检失败（不写 run.json）
        for tool in ["ffmpeg", "ffprobe", "llama-server"] {
            assert!(
                which::which(tool).ok().is_some(),
                "{tool} is required by this test"
            );
        }
        let dir = std::env::temp_dir().join(format!("c2md-failrun-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("broken.mp4");
        std::fs::write(&video, b"not a real mp4").unwrap();
        // This test fails while opening media, before ASR loads any weights. Supply
        // cache-header fixtures so the earlier model preflight never makes a request.
        let models = crate::models::llama_paths(&dir);
        std::fs::create_dir_all(models.model.parent().unwrap()).unwrap();
        for path in [&models.model, &models.mmproj] {
            use std::io::Write as _;
            let mut file = std::fs::File::create(path).unwrap();
            file.set_len(1_100_000).unwrap();
            file.write_all(b"GGUF\x03\0\0\0").unwrap();
        }
        use crate::config as c;
        let cfg = c::PipelineConfig {
            url: video.display().to_string(),
            out_root: dir.clone(),
            similarity: 0.9,
            sample_interval: 0.5,
            cooldown: 10.0,
            slide_mode: c::SlideMode::First,
            stable_secs: 0.0,
            max_height: 1080,
            roi: None,
            threads: 2,
            provider: c::AsrProvider::Cpu,
            max_speech: 20.0,
            formats: vec![c::OutputFormat::Md],
            model_dir: dir.clone(),
            keep_video: true,
            no_download: true,
            resume: false,
            llm: Default::default(),
            asr_api: Default::default(),
            asr_model: None,
            gpu_layers: c::DEFAULT_GPU_LAYERS,
            mmproj_offload: true,
            transcript_source: c::TranscriptSource::Asr,
        };
        let r = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(run(&cfg, super::ModelDownload::Refused));
        assert!(r.is_err(), "损坏视频必须失败");

        let source_id = format!(
            "local:sha256:{}",
            crate::digest::file_digest(&video).unwrap()
        );
        let course = dir
            .join("local")
            .join(crate::digest::digest(source_id.as_bytes()));
        let work = std::fs::read_dir(course.parent().unwrap().join(".course2md/work"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = std::fs::read_to_string(work.join("run.json")).expect("失败也应写 run.json");
        assert!(!course.join("manifest.json").exists());
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["success"], false);
        assert!(!v["error"].as_str().unwrap_or_default().is_empty());
        assert_eq!(v["provider"], "cpu");
        assert!(v["course2md_version"].as_str().is_some());
        assert!(v["elapsed_secs"].is_number());
        // 失败发生在 llama-server spawn 之前 → 无 llama_server_args 字段
        assert!(v.get("llama_server_args").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_transcript_cache_is_discarded_and_retried() {
        use crate::timeline::TranscriptEvent;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.json");
        let ev = |text: &str| TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: text.into(),
            raw: None,
            translation: None,
        };

        // 全空缓存（上次端点故障的残留）→ 丢弃并删除文件，ASR 得以重跑
        let empty = super::TranscriptCache {
            source: super::TranscriptOrigin::Asr,
            events: vec![ev(""), ev("  ")],
        };
        std::fs::write(&path, serde_json::to_vec(&empty).unwrap()).unwrap();
        assert!(super::cached_transcript(&path).unwrap().is_none());
        assert!(!path.exists());

        // 有可读文字的缓存 → 正常复用
        let good = super::TranscriptCache {
            source: super::TranscriptOrigin::Asr,
            events: vec![ev("你好")],
        };
        std::fs::write(&path, serde_json::to_vec(&good).unwrap()).unwrap();
        assert!(super::cached_transcript(&path).unwrap().is_some());

        // 损坏的缓存 → 报错且原文件保留
        std::fs::write(&path, b"not json").unwrap();
        assert!(super::cached_transcript(&path).is_err());
        assert!(path.exists());

        // 不存在的缓存 → None
        let missing = dir.path().join("missing.json");
        assert!(super::cached_transcript(&missing).unwrap().is_none());
    }

    #[test]
    fn outcome_from_report_includes_counts_and_first_error() {
        let report = crate::llm::PolishReport {
            attempted: 200,
            succeeded: 63,
            failed: 137,
            note: Some("HTTP 404 model not found".into()),
            ..Default::default()
        };
        let outcome = super::outcome_from_report(&report);
        assert_eq!(outcome.status, crate::artifact::Status::Partial);
        let msg = outcome.message.unwrap();
        assert!(msg.contains("137/200"), "应包含失败计数: {msg}");
        assert!(msg.contains("HTTP 404"), "应包含首个错误原因: {msg}");

        // The kind is the first unfinished part's; a refusal of one part while the
        // rest was accepted does not say the settings are wrong.
        use crate::artifact::FailureKind;
        let with = |succeeded, kind| {
            super::outcome_from_report(&crate::llm::PolishReport {
                attempted: 165,
                succeeded,
                failed: 165 - succeeded,
                kind: Some(kind),
                ..Default::default()
            })
            .kind
        };
        assert_eq!(
            with(164, FailureKind::RateLimit),
            Some(FailureKind::RateLimit)
        );
        assert_eq!(with(164, FailureKind::Parameters), None);
        assert_eq!(
            with(0, FailureKind::Authentication),
            Some(FailureKind::Authentication)
        );

        let clean = crate::llm::PolishReport {
            attempted: 3,
            succeeded: 3,
            failed: 0,
            note: None,
            ..Default::default()
        };
        let outcome = super::outcome_from_report(&clean);
        assert_eq!(outcome.status, crate::artifact::Status::Succeeded);
        assert!(outcome.message.is_none());
    }
}
