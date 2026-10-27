//! Convert a fixed task into a separately published, immutable note version.

use crate::progress::Stage;
use crate::{
    artifact::{self, Outcome, Outcomes, Status, Target},
    asr,
    config::{self, PipelineConfig},
    dispatch::{ControlSource, ParentProcess, TaskControl},
    execution::{self, Job},
    fetch::{self, VideoMeta},
    media, progress, scene, timeline,
};
use anyhow::{Context, Result};
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

/// One message for an empty screenshot result.
const NO_SCREENSHOTS: &str = "未提取到截图";

/// Traditional CLI entry. Its generated task directory is stable for compatible recovery.
/// New settings or --no-resume use another work directory and never overwrite old notes.
pub async fn run(cfg: &PipelineConfig) -> Result<()> {
    let started = Instant::now();
    cfg.validate()
        .context(crate::tr!("配置预检失败 / Invalid settings"))?;
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
    // 本地视频先探时长：成功进 meta；失败不当 0 静默（merge 会失真），
    // 延迟到诊断作用域内返回，失败同样写 run.json
    let local_duration = if is_local {
        Some(media::probe_duration(local).await.context(crate::tr!(
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
    let local_duration_error = local_duration.and_then(|probe| probe.err());
    progress::done(Stage::Fetch);
    let platform = config::platform_from(&cfg.url, &meta.extractor);
    let source_id = if is_local {
        format!("local:sha256:{}", meta.id)
    } else {
        format!(
            "{}:{}",
            platform,
            if meta.id.is_empty() {
                config::infer_slug(&cfg.url)
            } else {
                meta.id.clone()
            }
        )
    };
    let course_id = execution::digest(source_id.as_bytes());
    let course_dir = cfg.out_root.join(&platform).join(&course_id);
    let binding = cli_binding(&source_id, cfg, &meta.title);
    let mut task_id = execution::digest(&serde_json::to_vec(&binding)?);
    if !cfg.resume {
        task_id = format!(
            "{}-{}",
            &task_id[..TASK_ID_PREFIX_LEN],
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
    }
    let target = Target {
        task_id: task_id.clone(),
        course_id,
        source_id,
        version_id: task_id.clone(),
        course_dir,
    };
    let work = execution::enter_work_dir(
        &target.course_dir.join(".work").join(&task_id),
        &binding,
        ControlSource::default(),
    )?;
    let job = work.job();
    let record = FailureRecord {
        cfg,
        work_dir: &job.dir,
        is_local,
        platform: &platform,
        id: &meta.id,
        started,
    };
    record
        .around(async {
            if let Some(error) = local_duration_error {
                return Err(error);
            }
            run_prepared(
                cfg,
                job,
                &meta,
                &target,
                is_local,
                SubtitleInput::Discover(probed),
                Credentials::Cli,
                started,
            )
            .await
        })
        .await
}

/// The identity a CLI task binds its work directory to; its digest names the task.
/// Identities record the output root as `out_dir`, the value the settings once
/// carried there before the pipeline chose the work directory.
pub(crate) fn cli_binding(source_id: &str, cfg: &PipelineConfig, title: &str) -> serde_json::Value {
    serde_json::json!({"source_id": source_id, "config": cfg.redacted_for_identity(&cfg.out_root), "title": title})
}

/// Desktop entry: all mutable configuration and credentials have already been resolved.
pub(crate) async fn run_task(
    request: &execution::Request,
    cfg: &PipelineConfig,
    parent: ParentProcess,
) -> Result<()> {
    // The caller (`execution::run`) validates through `Request::resolve`.
    let started = Instant::now();
    let target = Target::from_request(request);
    let binding = {
        let (request, cfg) = (request.clone(), cfg.clone());
        blocking(move || request.binding(&cfg)).await?
    };
    let work = execution::enter_work_dir(
        &request.work_dir,
        &binding,
        ControlSource {
            control_path: request.control_path.clone(),
            service_versions: request.service_versions.clone(),
            parent: Some(parent),
        },
    )?;
    let job = work.job();
    let is_local = Path::new(&request.source).is_file();
    let meta = VideoMeta {
        title: request.title.clone(),
        uploader: request.author.clone(),
        duration: request.duration,
        webpage_url: request.source.clone(),
        extractor: if is_local {
            "local".into()
        } else {
            config::platform_from(&request.source, "")
        },
        id: request.source_id.clone(),
    };
    // The record outlives `meta`, which moves into the task.
    let platform = meta.extractor.clone();
    let record = FailureRecord {
        cfg,
        work_dir: &job.dir,
        is_local,
        platform: &platform,
        id: &request.source_id,
        started,
    };
    record
        .around(run_task_inner(
            request, cfg, job, &target, meta, is_local, started,
        ))
        .await
}

async fn run_task_inner(
    request: &execution::Request,
    cfg: &PipelineConfig,
    job: &Job,
    target: &Target,
    mut meta: VideoMeta,
    is_local: bool,
    started: Instant,
) -> Result<()> {
    if let execution::Operation::Reprocess {
        base_version_dir,
        components,
        prior_work_dir,
    } = &request.operation
    {
        return reprocess(
            request,
            cfg,
            job,
            target,
            base_version_dir,
            components,
            prior_work_dir.as_deref(),
            started,
        )
        .await;
    }
    let subtitle = if let Some(events) = &request.subtitle_events {
        Some(events.clone())
    } else if let Some(path) = &request.subtitle {
        let content = std::fs::read_to_string(path).context(crate::tr!(
            "无法读取已选字幕 / Cannot read selected subtitles"
        ))?;
        Some(crate::subtitle::parse_subtitle(&content))
    } else {
        None
    };
    // 批量入队的任务不带预选字幕：Auto/强制字幕模式下在任务内探测一次并自行发现
    // 字幕，语义与 CLI 一致。探测补齐笔记所需的 UP 主与时长，标题保持入队时的候选标题。
    let subtitle_input = if discovers_subtitles_at_runtime(&subtitle, cfg, is_local) {
        progress::start(Stage::Fetch);
        let video = fetch::probe_video(&request.source, &job.control).await?;
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
    run_prepared(
        cfg,
        job,
        &meta,
        target,
        is_local,
        subtitle_input,
        Credentials::Task {
            allow_unauthenticated_asr: request.allow_unauthenticated_asr,
        },
        started,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn reprocess(
    request: &execution::Request,
    cfg: &PipelineConfig,
    job: &Job,
    target: &Target,
    base_dir: &Path,
    components: &[execution::Component],
    prior_work: Option<&Path>,
    started: Instant,
) -> Result<()> {
    job.control.check()?;
    let base_dir_owned = base_dir.to_path_buf();
    let base_control = job.control.clone();
    let base = blocking(move || {
        let base = artifact::read_manifest(&base_dir_owned.join("manifest.json"))?;
        // Reprocessing consumes the internal document, timeline and image assets.
        // A manually edited presentation file must stay intact, and does not make
        // these verified inputs unusable.
        let mut inputs = base.clone();
        inputs.assets.retain(|asset| {
            asset.path != base.markdown
                || asset.path == base.document
                || base.frames.iter().any(|frame| frame.image == asset.path)
        });
        artifact::validate_version_in_task(&base_dir_owned, &inputs, &base_control)?;
        Ok(base)
    })
    .await?;
    if let Some(markdown) = base.assets.iter().find(|asset| asset.path == base.markdown) {
        let unchanged = artifact::safe_asset_path(base_dir, &markdown.path)
            .and_then(|path| execution::file_digest(&path))
            .is_ok_and(|digest| digest == markdown.sha256);
        if !unchanged {
            tracing::info!(
                "笔记文件已被手动修改；这次补做以原始记录为准，你修改的文件不会被覆盖 / The note file was edited by hand; this run uses the original record and leaves your edited file untouched"
            );
        }
    }
    anyhow::ensure!(
        base.source_id == request.source_id && base.course_id == request.course_id,
        crate::tr!("补做任务与原笔记来源不一致 / Reprocessing source does not match the base note")
    );
    use execution::Component;
    let has = |component: Component| components.contains(&component);
    let mut document: artifact::Document =
        serde_json::from_slice(&std::fs::read(base_dir.join(&base.document))?)?;
    if components
        .iter()
        .all(|&component| component == Component::Exports)
    {
        return export_only(
            request, cfg, job, target, base_dir, &base, &document, started,
        );
    }
    if let Some(existing) = artifact::published(target)? {
        emit_done(target, &existing, &document.sections, started);
        return Ok(());
    }
    if has(Component::Proofreading) || has(Component::Summary) {
        crate::llm::validate(&cfg.llm)?;
    }
    if let Some(prior) = prior_work {
        import_prior_work(request, job, &base, components, prior).await?;
    }
    // Copy verified base images into a separate namespace; a failed new screenshot pass
    // cannot mix old and new frames or mutate the old version through shared hard links.
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
            crate::runtime::map_pool(images, 4, &control, |image| {
                let source = artifact::safe_asset_path(&base_dir, &image)?;
                let dest = out_dir.join("base").join(&image);
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
            section.image = format!("base/{}", section.image);
        }
    }
    let mut outcomes = base.outcomes.clone();
    outcomes.exports.clear();
    document.meta.title = request.title.clone();
    if has(Component::Screenshots) {
        job.control.check()?;
        crate::error::require_cmd("ffmpeg")?;
        crate::error::require_cmd("ffprobe")?;
        let media_path = if Path::new(&request.source).is_file() {
            PathBuf::from(&request.source)
        } else {
            if !prepare_cached_media_async(cfg, job).await? {
                anyhow::ensure!(
                    !Path::new(&request.source).is_absolute()
                        && !request.source_id.starts_with("local:"),
                    crate::tr!(
                        "原视频已移动或删除，无法补做截图；原笔记仍可阅读 / Original video is unavailable; the saved note remains readable"
                    )
                );
                anyhow::ensure!(
                    !cfg.no_download,
                    crate::tr!(
                        "找不到截图所需的视频 / Video required for screenshots is unavailable"
                    )
                );
                // 与主路径同一纪律：无法校验的现存文件已由 prepare_cached_media 归档，
                // 此处只会真正下载新内容，不会把未校验文件标记为已校验
                crate::error::require_ytdlp()?;
                progress::start(Stage::Download);
                fetch::download(
                    &cfg.url,
                    &job.media_path(),
                    cfg.max_height,
                    false,
                    &job.control,
                )
                .await?;
                save_file_digest_async(job.media_path(), job.dir.join("media.sha256")).await?;
                progress::done(Stage::Download);
            }
            job.media_path()
        };
        match cached_frames(cfg, job, &media_path).await {
            Ok(frames) if !frames.is_empty() => {
                let speech = all_speech(&document.sections);
                document.sections = timeline::merge(frames, speech, document.meta.duration);
                outcomes.screenshots = Outcome::succeeded();
            }
            result => {
                outcomes.screenshots = Outcome::failed(match result {
                    Err(error) => format!("{error:#}"),
                    _ => crate::tr!(NO_SCREENSHOTS).into(),
                });
                if components.len() == 1 {
                    anyhow::bail!(crate::tr!(
                        "截图失败，原笔记不受影响 / Screenshot extraction failed; the original note is unaffected"
                    ));
                }
            }
        }
    }
    if has(Component::Proofreading) {
        progress::start(Stage::Llm);
        job.control.check()?;
        // The original complete transcript is an immutable version asset. Rebuild the
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
            }
        }
        let (sections, report) =
            polish_with_rollback(std::mem::take(&mut document.sections), cfg, job).await?;
        document.sections = sections;
        outcomes.proofreading = Outcome::from_report(&report);
        progress::done(Stage::Llm);
        // Proofreading rebuilt the paragraphs from the original transcript.
        if let Some(target) = cfg.llm.translation_target() {
            document.sections = translate_step(
                std::mem::take(&mut document.sections),
                cfg,
                job,
                target,
                &mut outcomes,
            )
            .await?;
        }
    }
    if has(Component::Summary) {
        document.summary =
            summarize_step(cfg, job, &document.sections, &document.meta, &mut outcomes).await?;
        // As with screenshots: when the summary was the only thing asked for,
        // a failure publishes no new version identical to the old one.
        if components.len() == 1 && document.summary.is_none() {
            let message = outcomes.summary.message.clone().unwrap_or_else(|| {
                crate::tr!(
                    "生成摘要未完成，原笔记不受影响 / Summary generation failed; the original note is unaffected"
                )
                .into()
            });
            // Keep the recorded cause so the error event's code matches it.
            return Err(crate::dispatch::Failure {
                kind: outcomes
                    .summary
                    .kind
                    .unwrap_or(crate::artifact::FailureKind::Local),
                ..crate::dispatch::Failure::local(message)
            }
            .into());
        }
    }
    job.control.check()?;
    let formats = if has(Component::Exports) {
        cfg.formats.as_slice()
    } else {
        &[]
    };
    let manifest = artifact::publish(
        target,
        &job.dir,
        &document.meta,
        &document.sections,
        document.summary.as_ref(),
        formats,
        outcomes,
        &job.control,
    )
    .await?;
    emit_done(target, &manifest, &document.sections, started);
    Ok(())
}

/// Export-only reprocessing writes beside the base version and never publishes a new one.
#[allow(clippy::too_many_arguments)]
fn export_only(
    request: &execution::Request,
    cfg: &PipelineConfig,
    job: &Job,
    target: &Target,
    base_dir: &Path,
    base: &artifact::Manifest,
    document: &artifact::Document,
    started: Instant,
) -> Result<()> {
    anyhow::ensure!(
        !cfg.formats.is_empty(),
        crate::tr!("请选择需要导出的文件格式 / Select an export format")
    );
    let export_root = target
        .course_dir
        .join("exports")
        .join(&base.version_id)
        .join(&target.task_id);
    let mut exports = std::collections::BTreeMap::new();
    let mut outputs = Vec::new();
    for format in &cfg.formats {
        let destination = export_root.join(crate::portable::file_name(*format));
        match export_for_task(base_dir, *format, &destination, job) {
            Ok(path) => {
                outputs.push(path);
                exports.insert(format.to_string(), Outcome::succeeded());
            }
            Err(error) => {
                exports.insert(format.to_string(), Outcome::from_error(&error));
            }
        }
    }
    let partial = exports
        .values()
        .any(|outcome| outcome.status == Status::Failed);
    crate::checkpoint::atomic_write(
        &job.dir.join("export-result.json"),
        &serde_json::to_vec_pretty(
            &serde_json::json!({"schema":1,"outputs":outputs,"outcomes":exports}),
        )?,
    )?;
    let (segments, chars) = done_stats(&document.sections);
    progress::emit(&progress::Event::Done(Box::new(progress::Done {
        operation: Some("exports".into()),
        task_id: request.task_id.clone(),
        course_id: base.course_id.clone(),
        version_id: base.version_id.clone(),
        out_dir: base_dir.to_path_buf(),
        manifest: Some(base_dir.join("manifest.json")),
        title: document.meta.title.clone(),
        slides: base.frames.len(),
        segments,
        chars,
        outputs,
        partial: Some(partial),
        outcomes: Some(Outcomes {
            exports,
            ..Default::default()
        }),
        elapsed_secs: started.elapsed().as_secs_f64(),
    })));
    Ok(())
}

fn export_for_task(
    base: &Path,
    format: config::OutputFormat,
    destination: &Path,
    job: &Job,
) -> Result<PathBuf> {
    let work = &job.dir;
    let marker = work.join(format!("export-{format}.json"));
    if destination.exists() {
        if marker.exists() {
            let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
            anyhow::ensure!(
                saved["sha256"].as_str() == Some(execution::file_digest(destination)?.as_str()),
                crate::tr!(
                    "导出文件已被修改，原文件已保留 / Export file changed; original retained"
                )
            );
        } else {
            // Publication can finish just before a crash loses its receipt. Compare a
            // fresh local rendering and claim ownership only when the bytes match.
            let scratch = tempfile::tempdir_in(work)?;
            let comparison = scratch.path().join(crate::portable::file_name(format));
            crate::portable::export_in_task(base, format, &comparison, &job.control)?;
            anyhow::ensure!(
                execution::file_digest(&comparison)? == execution::file_digest(destination)?,
                crate::tr!(
                    "导出位置已有不同文件，未覆盖 / Existing export differs; not overwritten"
                )
            );
        }
    } else {
        crate::portable::export_in_task(base, format, destination, &job.control)?;
    }
    crate::checkpoint::atomic_write(
        &marker,
        &serde_json::to_vec_pretty(
            &serde_json::json!({"sha256":execution::file_digest(destination)?}),
        )?,
    )?;
    Ok(destination.to_path_buf())
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

async fn subtitles(
    cfg: &PipelineConfig,
    job: &Job,
    local: bool,
    input: SubtitleInput,
) -> Result<Option<(Vec<timeline::TranscriptEvent>, TranscriptOrigin)>> {
    let probed = match input {
        SubtitleInput::Selected(selected) => {
            // Selected events were validated with the request.
            if let Some(events) = selected {
                return Ok(Some((events, TranscriptOrigin::SelectedSubtitle)));
            }
            anyhow::ensure!(
                cfg.transcript_source != config::TranscriptSource::Subtitle,
                crate::tr!("任务缺少已选字幕 / Selected subtitles are missing from this task")
            );
            return Ok(None);
        }
        SubtitleInput::Discover(probed) => probed,
    };
    if cfg.transcript_source == config::TranscriptSource::Asr {
        return Ok(None);
    }
    let found = if local {
        fetch::sidecar_subtitle(Path::new(&cfg.url))
    } else {
        let video = probed.context(crate::tr!(
            "缺少视频信息，请重新读取 / Missing video info; please probe again"
        ))?;
        fetch::fetch_subtitle(&video, &job.dir, &job.control)
            .await
            .context(crate::tr!(
                "读取视频字幕失败 / Could not read video subtitles"
            ))?
    };
    match found {
        Some(file) => {
            let content = std::fs::read_to_string(&file.path)
                .context(crate::tr!("无法读取字幕文件 / Cannot read subtitle file"))?;
            let events = crate::subtitle::parse_subtitle(&content);
            execution::validate_events(&events)?;
            Ok(Some((
                events,
                if file.auto {
                    TranscriptOrigin::AutoCaption
                } else {
                    TranscriptOrigin::Subtitle
                },
            )))
        }
        None if cfg.transcript_source == config::TranscriptSource::Subtitle => {
            anyhow::bail!(crate::tr!("未找到字幕。请选择已有字幕，或改用语音识别"))
        }
        None => Ok(None),
    }
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
enum Credentials {
    Cli,
    Task { allow_unauthenticated_asr: bool },
}

#[allow(clippy::too_many_arguments)]
async fn run_prepared(
    cfg: &PipelineConfig,
    job: &Job,
    meta: &VideoMeta,
    target: &Target,
    is_local: bool,
    input: SubtitleInput,
    credentials: Credentials,
    started: Instant,
) -> Result<()> {
    job.control.check()?;
    let published_target = target.clone();
    if let Some(manifest) = blocking(move || artifact::published(&published_target)).await? {
        // Also finish a publication interrupted between the directory and pointer commits.
        let document: artifact::Document = serde_json::from_slice(&std::fs::read(
            target.version_dir().join(&manifest.document),
        )?)?;
        let manifest = artifact::publish(
            target,
            &job.dir,
            &document.meta,
            &document.sections,
            document.summary.as_ref(),
            &cfg.formats,
            manifest.outcomes,
            &job.control,
        )
        .await?;
        emit_done(target, &manifest, &document.sections, started);
        return Ok(());
    }
    if cfg.llm.uses_service() {
        crate::llm::validate(&cfg.llm)?;
    }
    // Subtitle evidence and cloud static validation precede any full media download.
    progress::start(Stage::Subtitle);
    let selected = subtitles(cfg, job, is_local, input).await?;
    progress::done(Stage::Subtitle);
    check_tools(cfg, job, is_local, selected.is_none(), credentials).await?;
    meta.save(&job.meta_path())?;
    let (media_path, media_existed) = acquire_media(cfg, job, is_local).await?;
    job.control.check()?;
    let mut outcomes = Outcomes::default();
    // 两路并行但都受 control.watch 打断：取消时任一路不再等到自身阶段边界才停。
    // 被 dropped 的子进程分支由 kill_on_drop 终止；转写跑在 spawn_blocking 里，
    // drop 不会停止它，它在下一段开始前的 control.check 处退出。
    let (frames_result, transcript_result) = tokio::join!(
        async {
            tokio::select! {
                r = cached_frames(cfg, job, &media_path) => r,
                e = job.control.watch() => Err(e),
            }
        },
        async {
            tokio::select! {
                r = transcript(cfg, job, &media_path, selected) => r,
                e = job.control.watch() => Err(e),
            }
        }
    );
    let frames = match frames_result {
        Ok(frames) if !frames.is_empty() => {
            outcomes.screenshots = Outcome::succeeded();
            frames
        }
        Ok(_) => {
            outcomes.screenshots = Outcome::failed(crate::tr!(NO_SCREENSHOTS));
            Vec::new()
        }
        Err(error) => {
            outcomes.screenshots = Outcome::from_error(&error);
            Vec::new()
        }
    };
    let transcript = match transcript_result {
        Ok(cache) if crate::timeline::has_readable_events(&cache.events) => {
            outcomes.transcript = Outcome::succeeded();
            cache
        }
        Ok(_) => {
            outcomes.transcript =
                Outcome::failed(crate::tr!("没有获得可读文字 / No readable transcript"));
            save_timeline(job, &frames, &[]).await?;
            anyhow::bail!(crate::tr!(
                "没有获得可读文字；已保留可用截图 / No readable transcript; available screenshots were retained"
            ));
        }
        Err(error) => {
            let completed = match crate::checkpoint::Checkpoint::saved_events(&job.dir) {
                Ok(events) => events,
                Err(read_error) => {
                    tracing::warn!(
                        "已保存的部分文字无法读取，原文件已保留 / Saved partial text could not be read; original files retained: {read_error:#}"
                    );
                    Vec::new()
                }
            };
            outcomes.transcript = if completed.is_empty() {
                Outcome::from_error(&error)
            } else {
                Outcome {
                    status: Status::Partial,
                    message: Some(format!("{error:#}")),
                    completed: Some(completed.len()),
                    total: None,
                    kind: Some(
                        error
                            .downcast_ref::<crate::dispatch::Failure>()
                            .map_or(crate::artifact::FailureKind::Local, |failure| failure.kind),
                    ),
                }
            };
            save_timeline(job, &frames, &completed).await?;
            return Err(error);
        }
    };
    save_timeline(job, &frames, &transcript.events).await?;
    let mut sections = build_sections(frames, transcript.events, meta.duration);
    timeline::coalesce_sections(&mut sections);
    let (sections, summary) = enrich(sections, cfg, job, meta, &mut outcomes).await?;
    job.control.check()?;
    progress::start(Stage::Render);
    let manifest = artifact::publish(
        target,
        &job.dir,
        meta,
        &sections,
        summary.as_ref(),
        &cfg.formats,
        outcomes.clone(),
        &job.control,
    )
    .await?;
    progress::done(Stage::Render);
    // Diagnostics remain in work space; a failure record can never become a library entry.
    crate::checkpoint::atomic_write(
        &job.dir.join("run.json"),
        &serde_json::to_vec_pretty(
            &serde_json::json!({"success":true,"task_id":target.task_id,"version_id":target.version_id,"transcript_source":transcript.source,"outcomes":manifest.outcomes,"out_dir":target.version_dir()}),
        )?,
    )?;
    if should_delete_media(is_local, cfg.no_download, media_existed, cfg.keep_video) {
        let _ = std::fs::remove_file(&media_path);
    }
    emit_done(target, &manifest, &sections, started);
    if !progress::is_json() && !progress::is_quiet() {
        crate::eprintln_tr!(
            "{}：{}",
            if manifest.partial {
                crate::tr!("笔记已保存，部分处理未完成 / Notes saved with unfinished work")
            } else {
                crate::tr!("笔记已生成 / Notes ready")
            },
            target.version_dir().display()
        );
        if !cfg.llm.enabled && !cfg.llm.disable_hint {
            crate::eprintln_tr!(
                "提示：运行 course2md llm setup 可开启 AI 校对与摘要 / Tip: run course2md llm setup to enable AI proofreading and summaries"
            );
        }
    }
    Ok(())
}

/// Reuses what an earlier task for the same note already paid for: its
/// receipts for the requested AI components (each failed one authorized for
/// one more attempt) and its verified video download.
async fn import_prior_work(
    request: &execution::Request,
    job: &Job,
    base: &artifact::Manifest,
    components: &[execution::Component],
    prior: &Path,
) -> Result<()> {
    use execution::Component;
    let has = |component: Component| components.contains(&component);
    let identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(prior.join("task-identity.json"))?)?;
    anyhow::ensure!(
        identity["source_id"].as_str() == Some(request.source_id.as_str()),
        crate::tr!("旧进度与此视频来源不符 / Prior work belongs to another source")
    );
    anyhow::ensure!(
        identity["task_id"]
            .as_str()
            .is_none_or(|id| id == base.task_id),
        crate::tr!("旧进度与原笔记任务不符 / Prior work belongs to another task")
    );
    let ledger_dir = job.dir.join("requests");
    std::fs::create_dir_all(&ledger_dir)?;
    for mut receipt in crate::dispatch::receipts(prior)? {
        let needed = Component::parse(&receipt.purpose)
            .is_some_and(|c| matches!(c, Component::Proofreading | Component::Summary) && has(c));
        let matching_service = request.service_versions.get("llm").map_or(
            receipt
                .service_version
                .starts_with(crate::dispatch::SNAPSHOT_PREFIX),
            |version| version == &receipt.service_version,
        );
        if needed && matching_service && execution::valid_id(&receipt.stable_id) {
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
                crate::checkpoint::atomic_write(
                    &destination,
                    &serde_json::to_vec_pretty(&receipt)?,
                )?;
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
async fn check_tools(
    cfg: &PipelineConfig,
    job: &Job,
    is_local: bool,
    needs_speech: bool,
    credentials: Credentials,
) -> Result<()> {
    if needs_speech {
        let (require_key, allow_environment) = match credentials {
            Credentials::Cli => (true, true),
            Credentials::Task {
                allow_unauthenticated_asr,
            } => (!allow_unauthenticated_asr, false),
        };
        cfg.validate_asr_with_auth(require_key, allow_environment)?;
        if !matches!(
            cfg.provider,
            config::AsrProvider::Coreml | config::AsrProvider::Api | config::AsrProvider::Npu
        ) {
            crate::error::require_cmd("llama-server")?;
        }
    }
    crate::error::require_cmd("ffmpeg")?;
    crate::error::require_cmd("ffprobe")?;
    if !is_local {
        crate::error::require_ytdlp()?;
    }
    if needs_speech && cfg.provider != config::AsrProvider::Api {
        progress::start(Stage::ModelPrepare);
        crate::models::ensure_cache(
            cfg.provider,
            cfg.asr_model
                .as_deref()
                .unwrap_or(config::default_model(cfg.provider)),
            &cfg.model_dir,
            &job.control,
        )
        .await?;
        progress::done(Stage::ModelPrepare);
    }
    Ok(())
}

/// The video to process, and whether a verified download already existed.
async fn acquire_media(cfg: &PipelineConfig, job: &Job, is_local: bool) -> Result<(PathBuf, bool)> {
    if is_local {
        return Ok((PathBuf::from(&cfg.url), false));
    }
    let media_path = job.media_path();
    let existed = prepare_cached_media_async(cfg, job).await?;
    job.control.check()?;
    if !existed {
        anyhow::ensure!(
            !cfg.no_download,
            crate::tr!(
                "找不到已下载的视频，请允许重新下载 / The downloaded video is missing; allow downloading it again"
            )
        );
        progress::start(Stage::Download);
        fetch::download(
            &cfg.url,
            &media_path,
            cfg.max_height,
            tracing::enabled!(tracing::Level::DEBUG),
            &job.control,
        )
        .await?;
        save_file_digest_async(media_path.clone(), job.dir.join("media.sha256")).await?;
        progress::done(Stage::Download);
    }
    Ok((media_path, existed))
}

/// The transcript: a saved one, the selected subtitles, or speech recognition.
async fn transcript(
    cfg: &PipelineConfig,
    job: &Job,
    media_path: &Path,
    selected: Option<(Vec<timeline::TranscriptEvent>, TranscriptOrigin)>,
) -> Result<TranscriptCache> {
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
                save_file_digest_async(audio.clone(), marker).await?;
            }
            progress::done(Stage::Audio);
            job.control.check()?;
            progress::start(Stage::Transcribe);
            let events = asr::run(cfg, &audio, job).await?;
            progress::done(Stage::Transcribe);
            TranscriptCache {
                source: TranscriptOrigin::Asr,
                events,
            }
        }
    };
    // 全空转写不入缓存：空缓存会在下次运行时挡住重新识别
    if crate::timeline::has_readable_events(&cache.events) {
        let bytes = serde_json::to_vec_pretty(&cache)?;
        let cache_path = cache_path.clone();
        blocking(move || crate::checkpoint::atomic_write(&cache_path, &bytes)).await?;
    }
    Ok(cache)
}

/// Optional AI work on the assembled note: proofreading, translation, summary.
async fn enrich(
    mut sections: Vec<timeline::Section>,
    cfg: &PipelineConfig,
    job: &Job,
    meta: &VideoMeta,
    outcomes: &mut Outcomes,
) -> Result<(Vec<timeline::Section>, Option<crate::summarize::Summary>)> {
    job.control.check()?;
    if cfg.llm.enabled {
        progress::start(Stage::Llm);
        let (polished, report) = polish_with_rollback(sections, cfg, job).await?;
        sections = polished;
        outcomes.proofreading = Outcome::from_report(&report);
        progress::done(Stage::Llm);
    }
    job.control.check()?;
    if let Some(target) = cfg.llm.translation_target() {
        sections = translate_step(sections, cfg, job, target, outcomes).await?;
    }
    job.control.check()?;
    let summary = if cfg.llm.summarize {
        summarize_step(cfg, job, &sections, meta, outcomes).await?
    } else {
        None
    };
    Ok((sections, summary))
}

/// The timeline a later reprocess task starts from.
async fn save_timeline(
    job: &Job,
    frames: &[timeline::FrameEvent],
    events: &[timeline::TranscriptEvent],
) -> Result<()> {
    let (path, frames, events) = (job.timeline_path(), frames.to_vec(), events.to_vec());
    blocking(move || timeline::write_jsonl(&path, &frames, &events)).await
}

#[derive(Serialize, Deserialize)]
struct FramesCache {
    frames: Vec<timeline::FrameEvent>,
    files: Vec<(String, String)>,
}
async fn cached_frames(
    cfg: &PipelineConfig,
    job: &Job,
    media: &Path,
) -> Result<Vec<timeline::FrameEvent>> {
    let cache_path = job.dir.join("screenshots.json");
    if cache_path.is_file() {
        let cache: FramesCache = serde_json::from_slice(&std::fs::read(&cache_path)?).context(
            crate::tr!("截图进度损坏，原文件已保留 / Screenshot checkpoint is damaged"),
        )?;
        if !cache.frames.is_empty() {
            let (out_dir, control) = (job.dir.clone(), job.control.clone());
            if blocking(move || Ok(verified_cache_files(&out_dir, &cache.files, &control))).await? {
                return Ok(cache.frames);
            }
        }
    }
    job.control.check()?;
    let frames = scene::run(cfg, media, job).await?;
    let files = {
        let (out_dir, images) = (
            job.dir.clone(),
            frames.iter().map(|f| f.image.clone()).collect::<Vec<_>>(),
        );
        blocking(move || {
            images
                .into_iter()
                .map(|image| {
                    let digest =
                        execution::file_digest(&artifact::safe_asset_path(&out_dir, &image)?)?;
                    Ok((image, digest))
                })
                .collect::<Result<Vec<_>>>()
        })
        .await?
    };
    crate::checkpoint::atomic_write(
        &cache_path,
        &serde_json::to_vec_pretty(&FramesCache {
            frames: frames.clone(),
            files,
        })?,
    )?;
    Ok(frames)
}
/// Content hashing reads whole media files; keep it off the async workers so the
/// sibling screenshot/transcript branch and control polling keep running.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.context(crate::tr!(
        "后台文件校验中断 / Background file check interrupted"
    ))?
}
async fn save_file_digest_async(path: PathBuf, marker: PathBuf) -> Result<()> {
    blocking(move || save_file_digest(&path, &marker)).await
}
async fn prepare_cached_media_async(cfg: &PipelineConfig, job: &Job) -> Result<bool> {
    let (job, no_download) = (job.clone(), cfg.no_download);
    blocking(move || prepare_cached_media(&job, no_download)).await
}
fn verified_file(path: &Path, marker: &Path) -> Result<bool> {
    if !path.is_file() || !marker.is_file() {
        return Ok(false);
    }
    Ok(std::fs::read_to_string(marker)? == execution::file_digest(path)?)
}
fn save_file_digest(path: &Path, marker: &Path) -> Result<()> {
    crate::checkpoint::atomic_write(marker, execution::file_digest(path)?.as_bytes())
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
        crate::tr!(
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
    crate::artifact::sync_dir(&archive_path)?;
    crate::artifact::sync_dir(&job.dir)?;
    Ok(false)
}

fn emit_done(
    target: &Target,
    manifest: &artifact::Manifest,
    sections: &[timeline::Section],
    started: Instant,
) {
    let (segments, chars) = done_stats(sections);
    progress::emit(&progress::Event::Done(Box::new(progress::Done {
        operation: None,
        task_id: target.task_id.clone(),
        course_id: target.course_id.clone(),
        version_id: target.version_id.clone(),
        out_dir: target.version_dir(),
        manifest: Some(target.version_dir().join("manifest.json")),
        title: manifest.title.clone(),
        slides: manifest.frames.len(),
        segments,
        chars,
        outputs: manifest.outputs.iter().map(PathBuf::from).collect(),
        partial: Some(manifest.partial),
        outcomes: Some(manifest.outcomes.clone()),
        elapsed_secs: started.elapsed().as_secs_f64(),
    })));
}

/// Every transcript event in document order.
pub fn all_speech(sections: &[timeline::Section]) -> Vec<timeline::TranscriptEvent> {
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

/// Transcript scale shared by every "done" event.
fn done_stats(sections: &[timeline::Section]) -> (usize, usize) {
    (
        sections.iter().map(|section| section.speech.len()).sum(),
        sections
            .iter()
            .flat_map(|section| &section.speech)
            .map(|event| event.text.chars().count())
            .sum(),
    )
}

/// 读取转写缓存；全空的缓存是上次失败运行的残留（端点持续返回空文本却
/// 无报错），丢弃并重新识别，而不是用"所选字幕没有可读文字"误导用户。
fn cached_transcript(cache_path: &Path) -> Result<Option<TranscriptCache>> {
    if !cache_path.is_file() {
        return Ok(None);
    }
    let cache: TranscriptCache = serde_json::from_slice(&std::fs::read(cache_path)?).context(
        crate::tr!("已保存的文字损坏，原文件已保留 / Saved transcript is damaged"),
    )?;
    if crate::timeline::has_readable_events(&cache.events) {
        execution::validate_events(&cache.events)?;
        return Ok(Some(cache));
    }
    tracing::warn!(
        "已保存的文字为空，丢弃并重新识别 / Saved transcript is empty; discarding it and transcribing again"
    );
    let _ = std::fs::remove_file(cache_path);
    Ok(None)
}

/// Polish off-thread. If the polished body is unreadable, roll back to the
/// originals and report the whole attempt as failed: nothing was kept.
async fn polish_with_rollback(
    mut sections: Vec<timeline::Section>,
    cfg: &PipelineConfig,
    job: &Job,
) -> Result<(Vec<timeline::Section>, crate::llm::PolishReport)> {
    let original = sections.clone();
    let llm = cfg.llm.clone();
    let root = job.dir.clone();
    let control = job.control.clone();
    let (mut sections, mut report) = tokio::task::spawn_blocking(move || {
        crate::llm::polish_sections_report(&mut sections, &root, &llm, &control)
            .map(|report| (sections, report))
    })
    .await
    .context(crate::tr!(
        "AI 校对工作进程中断 / Proofreading worker interrupted"
    ))??;
    if !artifact::has_readable_body(&sections) {
        sections = original;
        report.succeeded = 0;
        report.failed = report.attempted;
    }
    Ok((sections, report))
}

impl Outcome {
    /// Map a polish run onto the version outcome. The impl lives next to its
    /// only callers; the type itself belongs to the version manifest.
    pub fn from_report(report: &crate::llm::PolishReport) -> Self {
        Self {
            kind: None,
            status: if report.failed == 0 {
                Status::Succeeded
            } else if report.succeeded > 0 {
                Status::Partial
            } else {
                Status::Failed
            },
            message: (report.failed > 0).then(|| {
                let base = crate::tr!(
                    "校对未全部完成（{}/{} 失败），原文已保留",
                    report.failed,
                    report.attempted
                );
                match &report.note {
                    Some(note) => crate::tr!("{}；首个错误：{}", base, note),
                    None => base,
                }
            }),
            completed: Some(report.succeeded),
            total: Some(report.attempted),
        }
    }
}

/// How much transcript the language guess reads.
const LANGUAGE_SAMPLE_CHARS: usize = 4000;

/// Adds translations for a bilingual note, unless the transcript is already
/// in the target language. Failed batches stay untranslated.
async fn translate_step(
    mut sections: Vec<timeline::Section>,
    cfg: &PipelineConfig,
    job: &Job,
    target: &'static str,
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
    if crate::language::already_in(detected, target) {
        tracing::info!(
            target,
            "文字已是目标语言，无需翻译 / The transcript is already in the target language; not translating"
        );
        outcomes.translation = Outcome::not_requested();
        return Ok(sections);
    }
    progress::start(Stage::Translate);
    let llm = cfg.llm.clone();
    let control = job.control.clone();
    let (sections, report) = tokio::task::spawn_blocking(move || {
        crate::llm::translate_sections_report(&mut sections, &llm, target, &control)
            .map(|report| (sections, report))
    })
    .await
    .context(crate::tr!(
        "AI 翻译工作进程中断 / Translation worker interrupted"
    ))??;
    outcomes.translation = Outcome {
        message: (report.failed > 0).then(|| {
            let base = crate::tr!(
                "翻译未全部完成（{}/{} 失败）",
                report.failed,
                report.attempted
            );
            match &report.note {
                Some(note) => crate::tr!("{}；首个错误：{}", base, note),
                None => base,
            }
        }),
        ..Outcome::from_report(&report)
    };
    progress::done(Stage::Translate);
    Ok(sections)
}

async fn summarize_step(
    cfg: &PipelineConfig,
    job: &Job,
    sections: &[timeline::Section],
    meta: &VideoMeta,
    outcomes: &mut Outcomes,
) -> Result<Option<crate::summarize::Summary>> {
    job.control.check()?;
    progress::start(Stage::Summary);
    let speech = all_speech(sections);
    match crate::summarize::summarize(&cfg.llm, &speech, meta, &job.control).await {
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
                Outcome::from_error_with_message(&error, crate::summarize::failure_message(&error));
            Ok(None)
        }
    }
}

/// Screenshot checkpoints are verified by full-content digests, in parallel so
/// resuming a large cache is not a serial wait.
fn verified_cache_files(out_dir: &Path, files: &[(String, String)], control: &TaskControl) -> bool {
    crate::runtime::map_pool(
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

/// 失败诊断 run.json 的上下文：任何在工作目录确定后失败的路径都写一份。
struct FailureRecord<'a> {
    cfg: &'a PipelineConfig,
    work_dir: &'a Path,
    is_local: bool,
    platform: &'a str,
    id: &'a str,
    started: Instant,
}

impl FailureRecord<'_> {
    async fn around(&self, work: impl std::future::Future<Output = Result<()>>) -> Result<()> {
        let result = work.await;
        if let Err(error) = &result {
            self.write_run_json(error);
        }
        result
    }

    /// Record version/source/final provider/asr_model/error text/elapsed time.
    /// A failed write only warns: the original error is the outcome.
    fn write_run_json(&self, err: &anyhow::Error) {
        let cfg = self.cfg;
        let info = serde_json::json!({
            "success": false,
            "course2md_version": env!("CARGO_PKG_VERSION"),
            "source": {
                "kind": if self.is_local { "local" } else { "remote" },
                "platform": self.platform,
                "id": self.id,
                "url": cfg.url,
            },
            "provider": cfg.provider.as_str(),
            "asr_model": cfg.asr_model.clone().unwrap_or_else(|| "backend-default".into()),
            "error": format!("{err:#}"),
            "elapsed_secs": (self.started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
        });
        match serde_json::to_string_pretty(&info) {
            Ok(text) => {
                if let Err(error) = crate::checkpoint::atomic_write(
                    &self.work_dir.join("run.json"),
                    text.as_bytes(),
                ) {
                    tracing::warn!(
                        "写失败诊断 run.json 失败 / Failed to write failure diagnostics run.json: {error:#}"
                    );
                }
            }
            Err(error) => tracing::warn!(
                "序列化失败诊断失败 / Failed to serialize failure diagnostics: {error:#}"
            ),
        }
    }
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
        let job = crate::execution::Job::detached(root.path());
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
                crate::runtime::which(tool).is_some(),
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
        let r = tokio::runtime::Runtime::new().unwrap().block_on(run(&cfg));
        assert!(r.is_err(), "损坏视频必须失败");

        let source_id = format!(
            "local:sha256:{}",
            crate::execution::file_digest(&video).unwrap()
        );
        let course = dir
            .join("local")
            .join(crate::execution::digest(source_id.as_bytes()));
        let work = std::fs::read_dir(course.join(".work"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = std::fs::read_to_string(work.join("run.json")).expect("失败也应写 run.json");
        assert!(!course.join("current.json").exists());
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
        };
        let outcome = super::Outcome::from_report(&report);
        assert_eq!(outcome.status, crate::artifact::Status::Partial);
        let msg = outcome.message.unwrap();
        assert!(msg.contains("137/200"), "应包含失败计数: {msg}");
        assert!(msg.contains("HTTP 404"), "应包含首个错误原因: {msg}");

        let clean = crate::llm::PolishReport {
            attempted: 3,
            succeeded: 3,
            failed: 0,
            note: None,
        };
        let outcome = super::Outcome::from_report(&clean);
        assert_eq!(outcome.status, crate::artifact::Status::Succeeded);
        assert!(outcome.message.is_none());
    }
}
