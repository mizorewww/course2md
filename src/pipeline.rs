//! Convert a fixed task into a separately published, immutable note version.

use crate::{
    artifact::{self, Outcome, Outcomes, Status, Target},
    asr,
    config::{self, PipelineConfig},
    execution,
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

/// Traditional CLI entry. Its generated task directory is stable for compatible recovery.
/// New settings or --no-resume use another work directory and never overwrite old notes.
pub async fn run(cfg: &PipelineConfig) -> Result<()> {
    let started = Instant::now();
    cfg.validate().context("配置预检失败 / Invalid settings")?;
    let local = Path::new(&cfg.url);
    let is_local = local.is_file();
    if !is_local {
        crate::error::require_cmd("yt-dlp")?;
    }
    progress::stage("fetch", "start");
    let probed = if is_local {
        None
    } else {
        Some(Box::new(fetch::probe_video(&cfg.url).await?))
    };
    // 本地视频先探时长：成功进 meta；失败不当 0 静默（merge 会失真），
    // 延迟到诊断作用域内返回，失败同样写 run.json
    let local_duration = if is_local {
        Some(
            media::probe_duration(local)
                .await
                .context("无法探测本地视频时长 / Could not probe the local video duration"),
        )
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
            id: execution::file_digest(local)?,
        }
    };
    let local_duration_error = local_duration.and_then(|probe| probe.err());
    progress::stage("fetch", "done");
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
    let mut identity_config = cfg.clone();
    identity_config.llm.api_key.clear();
    identity_config.asr_api.api_key.clear();
    let binding =
        serde_json::json!({"source_id": source_id, "config": identity_config, "title": meta.title});
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
    let mut cfg = cfg.clone();
    cfg.out_dir = target.course_dir.join(".work").join(&task_id);
    if cfg.provider == config::AsrProvider::Api
        && cfg.asr_api.api_key.is_empty()
        && let Some(key) = config::asr_api_key_from_env()
    {
        cfg.asr_api.api_key = key;
    }
    cfg.resume = true;
    let _dispatch = crate::dispatch::install(&cfg.out_dir, None, &Default::default())?;
    std::fs::create_dir_all(&cfg.out_dir)?;
    let _lock = crate::runtime::lock_file(&cfg.out_dir.join(".task.lock"))?;
    execution::bind_work_dir(&cfg.out_dir, &binding)?;
    if let Some(error) = local_duration_error {
        // 与 run_prepared 失败同等待遇：失败诊断 run.json 照样落盘
        write_failure_run_json(&cfg, is_local, &platform, &meta.id, &error, started);
        return Err(error);
    }
    let result = run_prepared(
        &cfg,
        &meta,
        &target,
        is_local,
        SubtitleInput::Discover(probed),
        false,
        false,
        started,
    )
    .await;
    if let Err(error) = &result {
        write_failure_run_json(&cfg, is_local, &platform, &meta.id, error, started);
    }
    result
}

/// Desktop entry: all mutable configuration and credentials have already been resolved.
pub async fn run_task(request: &execution::Request, cfg: &PipelineConfig) -> Result<()> {
    let started = Instant::now();
    request.validate()?;
    let target = Target::from_request(request);
    std::fs::create_dir_all(&cfg.out_dir)?;
    let _lock = crate::runtime::lock_file(&cfg.out_dir.join(".task.lock"))?;
    execution::bind_work_dir(&cfg.out_dir, &request.binding(cfg)?)?;
    let is_local = Path::new(&request.source).is_file();
    let mut meta = VideoMeta {
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
    if let execution::Operation::Reprocess {
        base_version_dir,
        components,
        prior_work_dir,
    } = &request.operation
    {
        let result = reprocess(
            request,
            cfg,
            &target,
            base_version_dir,
            components,
            prior_work_dir.as_deref(),
            started,
        )
        .await;
        if let Err(error) = &result {
            write_failure_run_json(cfg, is_local, &meta.extractor, &meta.id, error, started);
        }
        return result;
    }
    let subtitle = if let Some(events) = &request.subtitle_events {
        Some(events.clone())
    } else if let Some(path) = &request.subtitle {
        let content = std::fs::read_to_string(path)
            .context("无法读取已选字幕 / Cannot read selected subtitles")?;
        Some(crate::subtitle::parse_subtitle(&content))
    } else {
        None
    };
    // 批量入队的任务不带预选字幕：Auto/强制字幕模式下在任务内探测一次并自行发现
    // 字幕，语义与 CLI 一致；桌面单视频流程的 transcript_source 已被确定为
    // Subtitle/Asr 且必带字幕内容，不会进入此分支。探测补齐笔记所需的 UP 主与时长，
    // 标题保持入队时用户看到的候选标题。
    let mut discovered = None;
    if discovers_subtitles_at_runtime(&subtitle, cfg, is_local) {
        progress::stage("fetch", "start");
        match fetch::probe_video(&request.source).await {
            Ok(video) => {
                progress::stage("fetch", "done");
                if meta.uploader.trim().is_empty() {
                    meta.uploader = video.meta.uploader.clone();
                }
                if !meta.duration.is_finite() || meta.duration <= 0. {
                    meta.duration = video.meta.duration;
                }
                discovered = Some(Box::new(video));
            }
            Err(error) => {
                write_failure_run_json(cfg, is_local, &meta.extractor, &meta.id, &error, started);
                return Err(error);
            }
        }
    }
    let subtitle_input = match discovered {
        Some(video) => SubtitleInput::Discover(Some(video)),
        None => SubtitleInput::Selected(subtitle),
    };
    let result = run_prepared(
        cfg,
        &meta,
        &target,
        is_local,
        subtitle_input,
        true,
        request.allow_unauthenticated_asr,
        started,
    )
    .await;
    if let Err(error) = &result {
        write_failure_run_json(cfg, is_local, &meta.extractor, &meta.id, error, started);
    }
    result
}

async fn reprocess(
    request: &execution::Request,
    cfg: &PipelineConfig,
    target: &Target,
    base_dir: &Path,
    components: &[String],
    prior_work: Option<&Path>,
    started: Instant,
) -> Result<()> {
    crate::dispatch::check_control()?;
    let base = artifact::read_manifest(&base_dir.join("manifest.json"))?;
    // Reprocessing consumes the internal document, timeline and image assets.
    // A manually edited presentation file must stay intact, and does not make
    // these verified inputs unusable.
    let mut inputs = base.clone();
    inputs.assets.retain(|asset| {
        asset.path != base.markdown
            || asset.path == base.document
            || base.frames.iter().any(|frame| frame.image == asset.path)
    });
    artifact::validate_version(base_dir, &inputs)?;
    if let Some(markdown) = base.assets.iter().find(|asset| asset.path == base.markdown) {
        let unchanged = artifact::safe_asset_path(base_dir, &markdown.path)
            .and_then(|path| execution::file_digest(&path))
            .is_ok_and(|digest| digest == markdown.sha256);
        if !unchanged {
            tracing::info!("原版 Markdown 已有改动；本次补做使用软件保存的正文，原文件保留 / The original Markdown has been modified; this reprocessing uses the software-saved document, and the original file is kept");
        }
    }
    anyhow::ensure!(
        base.source_id == request.source_id && base.course_id == request.course_id,
        "补做任务与原笔记来源不一致 / Reprocessing source does not match the base note"
    );
    let has = |name: &str| components.iter().any(|component| component == name);
    let mut document: artifact::Document =
        serde_json::from_slice(&std::fs::read(base_dir.join(&base.document))?)?;
    if components.iter().all(|component| component == "exports") {
        anyhow::ensure!(
            !cfg.formats.is_empty(),
            "请选择需要导出的文件格式 / Select an export format"
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
            match export_for_task(base_dir, *format, &destination, &cfg.out_dir) {
                Ok(path) => {
                    outputs.push(path);
                    exports.insert(format.to_string(), Outcome::succeeded());
                }
                Err(error) => {
                    exports.insert(format.to_string(), Outcome::failed(format!("{error:#}")));
                }
            }
        }
        let partial = exports
            .values()
            .any(|outcome| outcome.status == Status::Failed);
        crate::checkpoint::atomic_write(
            &cfg.out_dir.join("export-result.json"),
            &serde_json::to_vec_pretty(
                &serde_json::json!({"schema":1,"outputs":outputs,"outcomes":exports}),
            )?,
        )?;
        let (segments, chars) = done_stats(&document.sections);
        progress::emit(
            serde_json::json!({"type":"done","operation":"exports","task_id":request.task_id,"course_id":base.course_id,"version_id":base.version_id,"out_dir":base_dir,"manifest":base_dir.join("manifest.json"),"title":document.meta.title,"slides":base.frames.len(),"segments":segments,"chars":chars,"outputs":outputs,"partial":partial,"outcomes":{"exports":exports},"elapsed_secs":started.elapsed().as_secs_f64()}),
        );
        return Ok(());
    }
    if let Some(existing) = artifact::published(target)? {
        emit_done(target, &existing, &document.sections, started);
        return Ok(());
    }
    if has("proofreading") || has("summary") {
        crate::llm::validate(&cfg.llm)?;
    }
    if let Some(prior) = prior_work {
        let identity: serde_json::Value =
            serde_json::from_slice(&std::fs::read(prior.join("task-identity.json"))?)?;
        anyhow::ensure!(
            identity["source_id"].as_str() == Some(request.source_id.as_str()),
            "旧进度与此视频来源不符 / Prior work belongs to another source"
        );
        anyhow::ensure!(
            identity["task_id"]
                .as_str()
                .is_none_or(|id| id == base.task_id),
            "旧进度与原笔记任务不符 / Prior work belongs to another task"
        );
        let ledger_dir = cfg.out_dir.join("requests");
        std::fs::create_dir_all(&ledger_dir)?;
        for mut receipt in crate::dispatch::receipts(prior)? {
            let needed = (has("proofreading") && receipt.purpose == "proofreading")
                || (has("summary") && receipt.purpose == "summary");
            let matching_service = request.service_versions.get("llm").map_or(
                receipt.service_version.starts_with("snapshot:"),
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
        if !cfg.media_path().exists()
            && verified_file(&prior.join("media.mp4"), &prior.join("media.sha256"))?
        {
            std::fs::copy(prior.join("media.mp4"), cfg.media_path())?;
            save_file_digest(&cfg.media_path(), &cfg.out_dir.join("media.sha256"))?;
        }
    }
    // Copy verified base images into a separate namespace; a failed new screenshot pass
    // cannot mix old and new frames or mutate the old version through shared hard links.
    for section in &mut document.sections {
        if !section.image.is_empty() {
            let source = artifact::safe_asset_path(base_dir, &section.image)?;
            let relative = format!("base/{}", section.image);
            let dest = cfg.out_dir.join(&relative);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            if !dest.exists() {
                std::fs::copy(source, &dest)?;
            }
            section.image = relative;
        }
    }
    if base_dir.join("timeline.jsonl").is_file() {
        std::fs::copy(base_dir.join("timeline.jsonl"), cfg.timeline_path())?;
    }
    let mut outcomes = base.outcomes.clone();
    outcomes.exports.clear();
    document.meta.title = request.title.clone();
    if has("screenshots") {
        crate::dispatch::check_control()?;
        crate::error::require_cmd("ffmpeg")?;
        crate::error::require_cmd("ffprobe")?;
        let media_path = if Path::new(&request.source).is_file() {
            PathBuf::from(&request.source)
        } else {
            if !prepare_cached_media(cfg)? {
                anyhow::ensure!(
                    !Path::new(&request.source).is_absolute()
                        && !request.source_id.starts_with("local:"),
                    "原视频已移动或删除，无法补做截图；原笔记仍可阅读 / Original video is unavailable; the saved note remains readable"
                );
                anyhow::ensure!(
                    !cfg.no_download,
                    "找不到截图所需的视频 / Video required for screenshots is unavailable"
                );
                // 与主路径同一纪律：无法校验的现存文件已由 prepare_cached_media 归档，
                // 此处只会真正下载新内容，不会把未校验文件标记为已校验
                crate::error::require_cmd("yt-dlp")?;
                progress::stage("download", "start");
                fetch::download(&cfg.url, &cfg.media_path(), cfg.max_height, false).await?;
                save_file_digest(&cfg.media_path(), &cfg.out_dir.join("media.sha256"))?;
                progress::stage("download", "done");
            }
            cfg.media_path()
        };
        match cached_frames(cfg, &media_path).await {
            Ok(frames) if !frames.is_empty() => {
                let speech = all_speech(&document.sections);
                document.sections = timeline::merge(frames, speech, document.meta.duration);
                outcomes.screenshots = Outcome::succeeded();
            }
            result => {
                outcomes.screenshots = Outcome::failed(match result {
                    Err(error) => format!("{error:#}"),
                    _ => "没有提取到截图 / No screenshots captured".into(),
                });
                if components.len() == 1 {
                    anyhow::bail!(
                        "截图尚未完成；原笔记保持可用 / Screenshot extraction failed; the original note remains available"
                    );
                }
            }
        }
    }
    if has("proofreading") {
        progress::stage("llm", "start");
        crate::dispatch::check_control()?;
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
            document.sections = if frames.is_empty() {
                vec![timeline::Section {
                    t: 0.,
                    end: document.meta.duration,
                    image: String::new(),
                    speech,
                }]
            } else {
                timeline::merge(frames, speech, document.meta.duration)
            };
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
            polish_with_rollback(std::mem::take(&mut document.sections), cfg).await?;
        document.sections = sections;
        outcomes.proofreading = Outcome::from_report(&report);
        progress::stage("llm", "done");
    }
    if has("summary") {
        document.summary =
            summarize_step(cfg, &document.sections, &document.meta, &mut outcomes).await?;
    }
    crate::dispatch::check_control()?;
    let formats = if has("exports") {
        cfg.formats.as_slice()
    } else {
        &[]
    };
    let manifest = artifact::publish(
        target,
        &cfg.out_dir,
        &document.meta,
        &document.sections,
        document.summary.as_ref(),
        formats,
        outcomes,
    )
    .await?;
    emit_done(target, &manifest, &document.sections, started);
    Ok(())
}

fn export_for_task(
    base: &Path,
    format: config::OutputFormat,
    destination: &Path,
    work: &Path,
) -> Result<PathBuf> {
    let marker = work.join(format!("export-{format}.json"));
    if destination.exists() {
        if marker.exists() {
            let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
            anyhow::ensure!(
                saved["sha256"].as_str() == Some(execution::file_digest(destination)?.as_str()),
                "导出文件已被修改，原文件已保留 / Export file changed; original retained"
            );
        } else {
            // Publication can finish just before a crash loses its receipt. Compare a
            // fresh local rendering and claim ownership only when the bytes match.
            let scratch = tempfile::tempdir_in(work)?;
            let comparison = scratch.path().join(crate::portable::file_name(format));
            crate::portable::export(base, format, &comparison)?;
            anyhow::ensure!(
                execution::file_digest(&comparison)? == execution::file_digest(destination)?,
                "导出位置已有不同文件，未覆盖 / Existing export differs; not overwritten"
            );
        }
    } else {
        crate::portable::export(base, format, destination)?;
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
    local: bool,
    input: SubtitleInput,
) -> Result<Option<(Vec<timeline::TranscriptEvent>, String)>> {
    let probed = match input {
        SubtitleInput::Selected(selected) => {
            if let Some(events) = selected {
                execution::validate_events(&events)?;
                return Ok(Some((events, "selected-subtitle".into())));
            }
            anyhow::ensure!(
                cfg.transcript_source != config::TranscriptSource::Subtitle,
                "任务缺少已选字幕 / Selected subtitles are missing from this task"
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
        let video = probed.context("缺少视频信息，请重新读取 / Missing video info; please probe again")?;
        fetch::fetch_subtitle(&video, &cfg.out_dir)
            .await
            .context("读取视频字幕失败 / Could not read video subtitles")?
    };
    match found {
        Some(file) => {
            let content = std::fs::read_to_string(&file.path)
                .context("无法读取字幕文件 / Cannot read subtitle file")?;
            let events = crate::subtitle::parse_subtitle(&content);
            execution::validate_events(&events)?;
            Ok(Some((
                events,
                if file.auto {
                    "auto-caption"
                } else {
                    "subtitle"
                }
                .into(),
            )))
        }
        None if cfg.transcript_source == config::TranscriptSource::Subtitle => anyhow::bail!(
            "未找到字幕 / No subtitles found. 请选择已有字幕，或改用语音识别 / Select subtitles or use speech recognition."
        ),
        None => Ok(None),
    }
}

#[derive(Serialize, Deserialize)]
struct TranscriptCache {
    source: String,
    events: Vec<timeline::TranscriptEvent>,
}

// These arguments keep resolved inputs and credential policy separate from mutable config.
#[allow(clippy::too_many_arguments)]
async fn run_prepared(
    cfg: &PipelineConfig,
    meta: &VideoMeta,
    target: &Target,
    is_local: bool,
    input: SubtitleInput,
    strict_credentials: bool,
    allow_unauthenticated_asr: bool,
    started: Instant,
) -> Result<()> {
    asr::reset_llama_spawn_args();
    crate::dispatch::check_control()?;
    if let Some(manifest) = artifact::published(target)? {
        // Also finish a publication interrupted between the directory and pointer commits.
        let document: artifact::Document = serde_json::from_slice(&std::fs::read(
            target.version_dir().join(&manifest.document),
        )?)?;
        let manifest = artifact::publish(
            target,
            &cfg.out_dir,
            &document.meta,
            &document.sections,
            document.summary.as_ref(),
            &cfg.formats,
            manifest.outcomes,
        )
        .await?;
        emit_done(target, &manifest, &document.sections, started);
        return Ok(());
    }
    if cfg.llm.enabled || cfg.llm.summarize {
        crate::llm::validate(&cfg.llm)?;
    }
    // Subtitle evidence and cloud static validation precede any full media download.
    progress::stage("subtitle", "start");
    let selected = subtitles(cfg, is_local, input).await?;
    progress::stage("subtitle", "done");
    if selected.is_none() {
        cfg.validate_asr_with_auth(!allow_unauthenticated_asr, !strict_credentials)?;
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
        crate::error::require_cmd("yt-dlp")?;
    }
    if selected.is_none() && cfg.provider != config::AsrProvider::Api {
        progress::stage("model/prepare", "start");
        crate::models::ensure_cache(
            cfg.provider,
            cfg.asr_model.as_deref().unwrap_or("qwen3-1.7b"),
            &cfg.model_dir,
        )
        .await?;
        progress::stage("model/prepare", "done");
    }
    meta.save(&cfg.meta_path())?;
    let media_path = if is_local {
        PathBuf::from(&cfg.url)
    } else {
        cfg.media_path()
    };
    let media_existed = !is_local && prepare_cached_media(cfg)?;
    crate::dispatch::check_control()?;
    if !is_local && !media_existed {
        anyhow::ensure!(
            !cfg.no_download,
            "找不到已保存的视频 / Cached video missing"
        );
        progress::stage("download", "start");
        fetch::download(
            &cfg.url,
            &media_path,
            cfg.max_height,
            tracing::enabled!(tracing::Level::DEBUG),
        )
        .await?;
        save_file_digest(&media_path, &cfg.out_dir.join("media.sha256"))?;
        progress::stage("download", "done");
    }
    crate::dispatch::check_control()?;
    let mut outcomes = Outcomes::default();
    // 两路并行但都受 watch_control 打断：取消时任一路不再等到自身阶段边界才停；
    // 被 dropped 的分支由 kill_on_drop 清理其子进程
    let (frames_result, transcript_result) = tokio::join!(
        async {
            tokio::select! {
                r = cached_frames(cfg, &media_path) => r,
                e = crate::dispatch::watch_control() => Err(e),
            }
        },
        async {
            let transcript_work = async {
                let cache_path = cfg.out_dir.join("transcript.json");
                let cache = match cached_transcript(&cache_path)?.or_else(|| {
                    selected.map(|(events, source)| TranscriptCache { source, events })
                }) {
                    Some(cache) => cache,
                    None => {
                        progress::stage("audio", "start");
                        let audio = cfg.audio_path();
                        // A verified marker proves audio extraction completed; a bare WAV may be truncated.
                        if !verified_file(&audio, &cfg.out_dir.join("audio.sha256"))? {
                            media::extract_audio(&media_path, &audio).await?;
                            save_file_digest(&audio, &cfg.out_dir.join("audio.sha256"))?;
                        }
                        progress::stage("audio", "done");
                        crate::dispatch::check_control()?;
                        progress::stage("transcribe", "start");
                        let events = asr::run(cfg, &audio).await?;
                        progress::stage("transcribe", "done");
                        TranscriptCache {
                            source: "asr".into(),
                            events,
                        }
                    }
                };
                // 全空转写不入缓存：空缓存会在下次运行时挡住重新识别
                if cache.events.iter().any(|e| !e.text.trim().is_empty()) {
                    crate::checkpoint::atomic_write(
                        &cache_path,
                        &serde_json::to_vec_pretty(&cache)?,
                    )?;
                }
                Ok::<_, anyhow::Error>(cache)
            };
            tokio::select! {
                r = transcript_work => r,
                e = crate::dispatch::watch_control() => Err(e),
            }
        });
    let frames = match frames_result {
        Ok(frames) if !frames.is_empty() => {
            outcomes.screenshots = Outcome::succeeded();
            frames
        }
        Ok(_) => {
            outcomes.screenshots = Outcome::failed("未提取到截图 / No screenshots were captured");
            Vec::new()
        }
        Err(error) => {
            outcomes.screenshots = Outcome::failed(format!("{error:#}"));
            Vec::new()
        }
    };
    let transcript = match transcript_result {
        Ok(cache) if cache.events.iter().any(|e| !e.text.trim().is_empty()) => {
            outcomes.transcript = Outcome::succeeded();
            cache
        }
        Ok(_) => {
            outcomes.transcript = Outcome::failed("没有获得可读文字 / No readable transcript");
            save_materials(cfg, &frames, &[], &outcomes)?;
            anyhow::bail!(
                "没有获得可读文字；已保留可用截图 / No readable transcript; available screenshots were retained"
            );
        }
        Err(error) => {
            let completed = match crate::checkpoint::Checkpoint::saved_events(&cfg.out_dir) {
                Ok(events) => events,
                Err(_) => {
                    tracing::warn!(
                        "已保存的部分文字无法读取，原文件已保留 / Saved partial text could not be read; original files retained"
                    );
                    Vec::new()
                }
            };
            outcomes.transcript = if completed.is_empty() {
                Outcome::failed(format!("{error:#}"))
            } else {
                Outcome {
                    status: Status::Partial,
                    message: Some(format!("{error:#}")),
                    completed: Some(completed.len()),
                    total: None,
                }
            };
            save_materials(cfg, &frames, &completed, &outcomes)?;
            return Err(error);
        }
    };
    save_materials(cfg, &frames, &transcript.events, &outcomes)?;
    let mut sections = if frames.is_empty() {
        vec![timeline::Section {
            t: 0.,
            end: meta.duration,
            image: String::new(),
            speech: transcript.events,
        }]
    } else {
        timeline::merge(frames, transcript.events, meta.duration)
    };
    timeline::coalesce_sections(&mut sections);
    // Save the complete raw body before any optional request.
    crate::checkpoint::atomic_write(
        &cfg.out_dir.join("raw-document.json"),
        &serde_json::to_vec_pretty(&artifact::Document {
            schema: 1,
            meta: meta.clone(),
            sections: sections.clone(),
            summary: None,
        })?,
    )?;
    crate::dispatch::check_control()?;
    if cfg.llm.enabled {
        progress::stage("llm", "start");
        let (polished, report) = polish_with_rollback(sections, cfg).await?;
        sections = polished;
        outcomes.proofreading = Outcome::from_report(&report);
        progress::stage("llm", "done");
    }
    crate::dispatch::check_control()?;
    let summary = if cfg.llm.summarize {
        summarize_step(cfg, &sections, meta, &mut outcomes).await?
    } else {
        None
    };
    crate::dispatch::check_control()?;
    progress::stage("render", "start");
    let manifest = artifact::publish(
        target,
        &cfg.out_dir,
        meta,
        &sections,
        summary.as_ref(),
        &cfg.formats,
        outcomes.clone(),
    )
    .await?;
    progress::stage("render", "done");
    // Diagnostics remain in work space; a failure record can never become a library entry.
    crate::checkpoint::atomic_write(
        &cfg.out_dir.join("run.json"),
        &serde_json::to_vec_pretty(
            &serde_json::json!({"success":true,"task_id":target.task_id,"version_id":target.version_id,"transcript_source":transcript.source,"outcomes":manifest.outcomes,"out_dir":target.version_dir()}),
        )?,
    )?;
    if should_delete_media(is_local, cfg.no_download, media_existed, cfg.keep_video) {
        let _ = std::fs::remove_file(&media_path);
    }
    emit_done(target, &manifest, &sections, started);
    if !progress::is_json() && !progress::is_quiet() {
        eprintln!(
            "{}：{}",
            if manifest.partial {
                "笔记已保存，部分处理未完成 / Notes saved with unfinished work"
            } else {
                "笔记已生成 / Notes ready"
            },
            target.version_dir().display()
        );
        if !cfg.llm.enabled && !cfg.llm.disable_hint {
            eprintln!(
                "提示：可运行 course2md llm setup 开启 AI 润色与总结；--no-llm-hint 可关闭此提示。 / Tip: run course2md llm setup to enable AI proofreading and summaries; use --no-llm-hint to hide this tip."
            );
        }
    }
    Ok(())
}

fn save_materials(
    cfg: &PipelineConfig,
    frames: &[timeline::FrameEvent],
    events: &[timeline::TranscriptEvent],
    outcomes: &Outcomes,
) -> Result<()> {
    timeline::write_jsonl(&cfg.timeline_path(), frames, events)?;
    crate::checkpoint::atomic_write(
        &cfg.out_dir.join("materials.json"),
        &serde_json::to_vec_pretty(
            &serde_json::json!({"schema":1,"frames":frames,"outcomes":outcomes}),
        )?,
    )
}

#[derive(Serialize, Deserialize)]
struct FramesCache {
    frames: Vec<timeline::FrameEvent>,
    files: Vec<(String, String)>,
}
async fn cached_frames(cfg: &PipelineConfig, media: &Path) -> Result<Vec<timeline::FrameEvent>> {
    let cache_path = cfg.out_dir.join("screenshots.json");
    if cache_path.is_file() {
        let cache: FramesCache = serde_json::from_slice(&std::fs::read(&cache_path)?)
            .context("截图进度损坏，原文件已保留 / Screenshot checkpoint is damaged")?;
        if verified_cache_files(&cfg.out_dir, &cache.files) && !cache.frames.is_empty() {
            return Ok(cache.frames);
        }
    }
    crate::dispatch::check_control()?;
    let frames = scene::run(cfg, media).await?;
    let files = frames
        .iter()
        .map(|frame| {
            Ok((
                frame.image.clone(),
                execution::file_digest(&artifact::safe_asset_path(&cfg.out_dir, &frame.image)?)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    crate::checkpoint::atomic_write(
        &cache_path,
        &serde_json::to_vec_pretty(&FramesCache {
            frames: frames.clone(),
            files,
        })?,
    )?;
    Ok(frames)
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
fn prepare_cached_media(cfg: &PipelineConfig) -> Result<bool> {
    let media = cfg.media_path();
    if !media.is_file() {
        return Ok(false);
    }
    if verified_file(&media, &cfg.out_dir.join("media.sha256"))? {
        return Ok(true);
    }
    anyhow::ensure!(
        !cfg.no_download,
        "已保存的视频无法校验；需要重新下载才能继续 / Cached video cannot be verified; downloading is required"
    );
    let archives = cfg.out_dir.join(".previous-media");
    std::fs::create_dir_all(&archives)?;
    let archive = tempfile::Builder::new()
        .prefix("unverified-")
        .tempdir_in(archives)?;
    std::fs::rename(&media, archive.path().join("media.mp4"))?;
    let archive_path = archive.keep();
    crate::artifact::sync_dir(&archive_path)?;
    crate::artifact::sync_dir(&cfg.out_dir)?;
    Ok(false)
}

fn emit_done(
    target: &Target,
    manifest: &artifact::Manifest,
    sections: &[timeline::Section],
    started: Instant,
) {
    let (segments, chars) = done_stats(sections);
    progress::emit(
        serde_json::json!({"type":"done","task_id":target.task_id,"course_id":target.course_id,"version_id":target.version_id,"out_dir":target.version_dir(),"manifest":target.version_dir().join("manifest.json"),"title":manifest.title,"slides":manifest.frames.len(),"segments":segments,"chars":chars,"outputs":manifest.outputs,"partial":manifest.partial,"outcomes":manifest.outcomes,"elapsed_secs":started.elapsed().as_secs_f64()}),
    );
}

/// Every transcript event in document order.
pub fn all_speech(sections: &[timeline::Section]) -> Vec<timeline::TranscriptEvent> {
    sections
        .iter()
        .flat_map(|section| section.speech.iter().cloned())
        .collect()
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
    let cache: TranscriptCache = serde_json::from_slice(&std::fs::read(cache_path)?)
        .context("已保存的文字损坏，原文件已保留 / Saved transcript is damaged")?;
    if cache.events.iter().any(|e| !e.text.trim().is_empty()) {
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
) -> Result<(Vec<timeline::Section>, crate::llm::PolishReport)> {
    let original = sections.clone();
    let llm = cfg.llm.clone();
    let root = cfg.out_dir.clone();
    let (mut sections, mut report) = tokio::task::spawn_blocking(move || {
        crate::llm::polish_sections_report(&mut sections, &root, &llm)
            .map(|report| (sections, report))
    })
    .await
    .context("AI 校对工作进程中断 / Proofreading worker interrupted")??;
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
            status: if report.failed == 0 {
                Status::Succeeded
            } else if report.succeeded > 0 {
                Status::Partial
            } else {
                Status::Failed
            },
            message: (report.failed > 0).then(|| {
                let base = format!(
                    "校对未全部完成（{}/{} 失败），原文已保留 / Proofreading incomplete ({}/{} failed); original text retained",
                    report.failed, report.attempted, report.failed, report.attempted
                );
                match &report.note {
                    Some(note) => format!("{base}；首个错误 / First error: {note}"),
                    None => base,
                }
            }),
            completed: Some(report.succeeded),
            total: Some(report.attempted),
        }
    }
}

async fn summarize_step(
    cfg: &PipelineConfig,
    sections: &[timeline::Section],
    meta: &VideoMeta,
    outcomes: &mut Outcomes,
) -> Result<Option<crate::summarize::Summary>> {
    crate::dispatch::check_control()?;
    progress::stage("summary", "start");
    let speech = all_speech(sections);
    match crate::summarize::summarize(&cfg.llm, &speech, meta).await {
        Ok(summary) => {
            outcomes.summary = Outcome::succeeded();
            progress::stage("summary", "done");
            Ok(Some(summary))
        }
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                "生成摘要未完成 / Summary generation incomplete"
            );
            outcomes.summary = Outcome::failed(crate::summarize::failure_message(&error));
            Ok(None)
        }
    }
}

/// Screenshot checkpoints are verified by full-content digests. Spread the files
/// over parallel lanes so resuming a large cache is not a serial wait.
fn verified_cache_files(out_dir: &Path, files: &[(String, String)]) -> bool {
    if files.is_empty() {
        return true;
    }
    let lanes = FRAME_DIGEST_LANES.min(files.len());
    let chunk_size = files.len().div_ceil(lanes);
    std::thread::scope(|scope| {
        files
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk.iter().all(|(path, digest)| {
                        artifact::safe_asset_path(out_dir, path)
                            .and_then(|p| execution::file_digest(&p))
                            .is_ok_and(|actual| actual == *digest)
                    })
                })
            })
            .all(|handle| handle.join().unwrap_or(false))
    })
}

/// 失败诊断 run.json（issue #12 复测：只有成功才写 run.json 是诊断缺口）。
/// 记录版本/来源/最终 provider/asr_model/错误全文/耗时；若 llama-server 已
/// spawn 过，附带实际启动参数（GPU hang 类问题的关键证据）。
/// 写失败只告警——原错误才是主角，诊断记录绝不能反过来搞砸错误路径。
fn write_failure_run_json(
    cfg: &PipelineConfig,
    is_local: bool,
    platform: &str,
    id: &str,
    err: &anyhow::Error,
    t_total: Instant,
) {
    let mut info = serde_json::json!({
        "success": false,
        "course2md_version": env!("CARGO_PKG_VERSION"),
        "source": {
            "kind": if is_local { "local" } else { "remote" },
            "platform": platform,
            "id": id,
            "url": cfg.url,
        },
        "provider": cfg.provider.as_str(),
        "asr_model": cfg.asr_model.clone().unwrap_or_else(|| "backend-default".into()),
        "error": format!("{err:#}"),
        "elapsed_secs": (t_total.elapsed().as_secs_f64() * 100.0).round() / 100.0,
    });
    if let Some(args) = crate::asr::last_llama_spawn_args() {
        info["llama_server_args"] = serde_json::json!(args);
    }
    match serde_json::to_string_pretty(&info) {
        Ok(s) => {
            if let Err(e) =
                crate::checkpoint::atomic_write(&cfg.out_dir.join("run.json"), s.as_bytes())
            {
                tracing::warn!("写失败诊断 run.json 失败 / Failed to write failure diagnostics run.json: {e:#}");
            }
        }
        Err(e) => tracing::warn!("序列化失败诊断 run.json 失败 / Failed to serialize failure diagnostics run.json: {e:#}"),
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
        assert!(!super::discovers_subtitles_at_runtime(&Some(Vec::new()), &cfg, false));
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
        cfg.out_dir = root.path().to_path_buf();
        cfg.no_download = false;
        let media = cfg.media_path();
        std::fs::write(&media, b"unverified original bytes").unwrap();
        assert!(!super::prepare_cached_media(&cfg).unwrap());
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
        assert!(super::prepare_cached_media(&cfg).unwrap());
        std::fs::write(&media, b"changed externally").unwrap();
        cfg.no_download = true;
        assert!(super::prepare_cached_media(&cfg).is_err());
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

    /// issue #12：失败也写诊断 run.json（success:false + 错误全文）。
    /// 用损坏的本地视频让 pipeline 在 out_dir 确定之后（场景检测阶段）必然失败。
    #[test]
    fn failure_writes_diagnostic_run_json() {
        // 缺任何一个工具都会在 out_dir 确定之前的预检失败（不写 run.json），跳过
        for tool in ["ffmpeg", "ffprobe", "llama-server"] {
            if crate::runtime::which(tool).is_none() {
                eprintln!("skip: {tool} not found");
                return;
            }
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
            out_dir: dir.clone(),
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
        };

        // 全空缓存（上次端点故障的残留）→ 丢弃并删除文件，ASR 得以重跑
        let empty = super::TranscriptCache {
            source: "asr".into(),
            events: vec![ev(""), ev("  ")],
        };
        std::fs::write(&path, serde_json::to_vec(&empty).unwrap()).unwrap();
        assert!(super::cached_transcript(&path).unwrap().is_none());
        assert!(!path.exists());

        // 有可读文字的缓存 → 正常复用
        let good = super::TranscriptCache {
            source: "asr".into(),
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
