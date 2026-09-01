//! 编排：元数据 → 目录 → 下载 → (截图 ∥ 音频) → 识别 → 渲染。

use crate::asr;
use crate::config::{self, PipelineConfig};
use crate::fetch::{self, VideoMeta};
use crate::media;
use crate::render;
use crate::scene;
use crate::timeline;
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;

pub async fn run(cfg: &PipelineConfig) -> Result<()> {
    let t_total = Instant::now();
    cfg.validate().context("配置预检失败")?;
    crate::error::require_cmd("ffmpeg")?;
    crate::error::require_cmd("ffprobe")?;
    use config::AsrProvider;
    if !matches!(
        cfg.provider,
        AsrProvider::Coreml | AsrProvider::Api | AsrProvider::Npu
    ) {
        crate::error::require_cmd("llama-server")?;
    } else if cfg.provider == AsrProvider::Coreml
        && crate::error::require_cmd("llama-server").is_err()
    {
        // fallback 是 best-effort：提前告知而不是失败后才发现
        tracing::warn!("未找到 llama-server：CoreML 若失败将无法回退到 gpu 后端（best-effort）");
    }

    // LLM 预检：配置错误应在跑完昂贵的下载/识别之前暴露
    if cfg.llm.enabled {
        crate::llm::validate(&cfg.llm)?;
    }

    let local = Path::new(&cfg.url);
    let is_local = local.is_file();
    if !is_local && !cfg.no_download {
        crate::error::require_cmd("yt-dlp")?;
    }

    let mut cfg = cfg.clone();

    let meta = if is_local {
        let dur = media::probe_duration(local).await.unwrap_or(0.0);
        let stem = local
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("local")
            .to_string();
        VideoMeta {
            title: stem.clone(),
            uploader: String::new(),
            duration: dur,
            webpage_url: local.display().to_string(),
            extractor: "local".into(),
            id: stem,
        }
    } else {
        tracing::info!("fetch metadata");
        fetch::fetch_meta(&cfg.url).await?
    };

    let id = if is_local {
        // 本地文件：stem + 内容指纹短哈希，避免同名不同目录的课件互相覆盖
        let fp = local_fingerprint(local);
        format!("{}-{fp}", sanitize_stem(local))
    } else if meta.id.is_empty() {
        config::infer_slug(&cfg.url)
    } else {
        meta.id.clone()
    };
    let title = if meta.title.is_empty() {
        id.clone()
    } else {
        meta.title.clone()
    };
    let platform = config::platform_from(&cfg.url, &meta.extractor);
    cfg.out_dir = config::course_dir(&cfg.out_root, &platform, &title, &id);
    tokio::fs::create_dir_all(&cfg.out_dir).await?;
    meta.save(&cfg.meta_path())?;
    tracing::info!(
        title = %meta.title,
        platform = %platform,
        id = %id,
        out = %cfg.out_dir.display(),
        duration = format_args!("{:.0}s", meta.duration),
        "video"
    );

    let dest = cfg.media_path();
    // 文件所有权：只有本次运行真正下载的文件才允许结束时清理。
    // --no-download 复用的既有 media.mp4 属于用户资产，永远不删。
    let media_existed = !is_local && dest.is_file();
    // 本地文件直接原地处理，不拷贝；下载类输入落到 dest。
    let media: std::path::PathBuf = if is_local {
        tracing::info!(path = %local.display(), "local video");
        local.to_path_buf()
    } else if media_existed {
        tracing::info!(path = %dest.display(), "media exists, skip download");
        dest
    } else if !cfg.no_download {
        tracing::info!("download video");
        fetch::download(
            &cfg.url,
            &dest,
            cfg.max_height,
            tracing::enabled!(tracing::Level::DEBUG),
        )
        .await?;
        dest
    } else {
        anyhow::ensure!(dest.is_file(), "--no-download 但 {} 不存在", dest.display());
        dest
    };

    // —— 转写来源：平台字幕优先（人工 > 自动），无字幕再走本地 ASR ——
    // 有字幕时完全不抽音频、不加载模型（issue #1）
    let subtitle: Option<(Vec<timeline::TranscriptEvent>, &'static str)> = match cfg
        .transcript_source
    {
        config::TranscriptSource::Asr => None,
        _ => {
            let fetched = if is_local {
                fetch::sidecar_subtitle(local)
            } else {
                tracing::info!("probe platform subtitles");
                fetch::fetch_subtitle(&cfg.url, &cfg.out_dir)
                    .await
                    .ok()
                    .flatten()
            };
            match fetched {
                Some(f) => {
                    let content = std::fs::read_to_string(&f.path)
                        .with_context(|| format!("读取字幕 {}", f.path.display()))?;
                    let events = crate::subtitle::parse_subtitle(&content);
                    if events.is_empty() {
                        tracing::warn!(path = %f.path.display(), "字幕解析为空，回落 ASR");
                        None
                    } else {
                        let source: &'static str = if f.auto { "auto-caption" } else { "subtitle" };
                        tracing::info!(
                            events = events.len(),
                            source,
                            path = %f.path.display(),
                            "subtitle transcript"
                        );
                        Some((events, source))
                    }
                }
                None => None,
            }
        }
    };
    if cfg.transcript_source == config::TranscriptSource::Subtitle && subtitle.is_none() {
        anyhow::bail!(
            "--transcript-source subtitle：未获取到字幕（平台未提供人工/自动字幕，本地输入无同名 .srt/.vtt）"
        );
    }

    let transcript_source_used = subtitle.as_ref().map_or("asr", |(_, s)| *s);
    let (frames, events) = if let Some((events, _source)) = subtitle {
        // 字幕路径：只跑场景检测，跳过音频与 ASR
        let frames = scene::run(&cfg, &media).await?;
        (frames, events)
    } else {
        tracing::info!("extract slides and audio");
        let audio_path = cfg.audio_path();
        let (frames_res, audio_res) = tokio::join!(
            scene::run(&cfg, &media),
            media::extract_audio(&media, &audio_path)
        );
        let frames = frames_res?;
        audio_res?;
        tracing::info!(device = %cfg.provider, "transcribe");
        let events = asr::run(&cfg, &cfg.audio_path()).await?;
        (frames, events)
    };
    anyhow::ensure!(!frames.is_empty(), "没有截到任何画面");
    // 转写可能合法返回空（静音课件）；由后续正常渲染成"无语音"讲义
    // timeline.jsonl 始终保存 ASR 的原始细粒度事件，供追溯、调试或二次处理。
    timeline::write_jsonl(&cfg.timeline_path(), &frames, &events)?;

    let mut sections = timeline::merge(frames.clone(), events, meta.duration);
    timeline::coalesce_sections(&mut sections);

    // 可选 LLM 在已组织好的连续段落上校对，避免 VAD 碎片限制上下文。
    if cfg.llm.enabled {
        tracing::info!(model = %cfg.llm.model, "llm polish paragraphs");
        let ev = cfg.llm.clone();
        let joined = tokio::task::spawn_blocking(move || {
            crate::llm::polish_sections(&mut sections, &ev);
            sections
        })
        .await;
        sections = joined.context("LLM 线程 join 失败")?;
    }
    tracing::info!(sections = sections.len(), "merged");

    render::write_outputs(&cfg.out_dir, &meta, &sections, &cfg.formats).await?;
    // 只删自己下载的视频；本地输入与既有工作区文件不动。
    let media_deleted =
        should_delete_media(is_local, cfg.no_download, media_existed, cfg.keep_video);
    if media_deleted {
        let _ = tokio::fs::remove_file(&media).await;
    }

    #[cfg(unix)]
    let (peak_mb, child_peak_mb) = (
        peak_rss_mb(libc::RUSAGE_SELF),
        peak_rss_mb(libc::RUSAGE_CHILDREN),
    );
    #[cfg(not(unix))]
    let (peak_mb, child_peak_mb) = (peak_rss_mb(0), peak_rss_mb(0));
    let stats = RunStats {
        elapsed_secs: t_total.elapsed().as_secs_f64(),
        peak_mb,
        child_peak_mb,
    };
    print_summary(
        &cfg,
        &meta,
        &sections,
        &stats,
        &media,
        is_local,
        media_deleted,
    );

    // run.json：本次运行溯源（版本/源/转写来源/后端/模型/统计/耗时）。
    // 「这份文稿到底是什么模型跑的」从此可查；issue 报告请附上此文件。
    let speech_n: usize = sections.iter().map(|s| s.speech.len()).sum();
    let chars: usize = sections
        .iter()
        .flat_map(|s| s.speech.iter())
        .map(|e| e.text.chars().count())
        .sum();
    let run_info = serde_json::json!({
        "course2md_version": env!("CARGO_PKG_VERSION"),
        "source": {
            "kind": if is_local { "local" } else { "remote" },
            "platform": platform,
            "id": id,
            "url": cfg.url,
        },
        "provider": cfg.provider.as_str(),
        "transcript_source": transcript_source_used,
        "asr_model": cfg.asr_model.clone().unwrap_or_else(|| "backend-default".into()),
        "resume": cfg.resume,
        "formats": cfg.formats.iter().map(|f| f.to_string()).collect::<Vec<_>>(),
        "llm_polish": cfg.llm.enabled,
        "sections": sections.len(),
        "speech_segments": speech_n,
        "chars": chars,
        "elapsed_secs": (stats.elapsed_secs * 100.0).round() / 100.0,
    });
    if let Err(e) = crate::checkpoint::atomic_write(
        &cfg.out_dir.join("run.json"),
        serde_json::to_string_pretty(&run_info)?.as_bytes(),
    ) {
        tracing::warn!("写 run.json 失败（不影响其他产物）：{e:#}");
    }
    Ok(())
}

struct RunStats {
    elapsed_secs: f64,
    peak_mb: Option<f64>,
    child_peak_mb: Option<f64>,
}

fn print_summary(
    cfg: &PipelineConfig,
    meta: &VideoMeta,
    sections: &[timeline::Section],
    stats: &RunStats,
    media: &Path,
    is_local: bool,
    media_deleted: bool,
) {
    let out = &cfg.out_dir;
    let speech_n: usize = sections.iter().map(|s| s.speech.len()).sum();
    let chars: usize = sections
        .iter()
        .flat_map(|s| s.speech.iter())
        .map(|e| e.text.chars().count())
        .sum();

    eprintln!();
    eprintln!(
        "{}",
        crate::i18n::tr(
            "──────── course2md done ────────",
            "──────── course2md 完成 ────────"
        )
    );
    eprintln!("{}: {}", crate::i18n::tr("Title", "标题"), meta.title);
    eprintln!(
        "{}: {}",
        crate::i18n::tr("Output dir", "输出目录"),
        out.display()
    );
    eprintln!();
    eprintln!("{}:", crate::i18n::tr("Documents", "文稿"));
    for f in &cfg.formats {
        let p = out.join(f.output_name());
        if p.is_file() {
            eprintln!("  {}", p.display());
        }
    }
    eprintln!(
        "{}: {}/frames/  ({} {})",
        crate::i18n::tr("Screenshots", "截图"),
        out.display(),
        sections.len(),
        crate::i18n::tr("images", "张")
    );
    eprintln!("音频：{}", cfg.audio_path().display());
    if is_local {
        eprintln!(
            "{}: {}  ({})",
            crate::i18n::tr("Video", "视频"),
            media.display(),
            crate::i18n::tr("local input, untouched", "本地输入，未改动")
        );
    } else if media_deleted {
        eprintln!(
            "{}: {} ({})",
            crate::i18n::tr("Video", "视频"),
            crate::i18n::tr("downloaded this run, deleted", "本次下载，已删除"),
            crate::i18n::tr("use --keep-video to keep", "用 --keep-video 可保留")
        );
    } else {
        eprintln!(
            "{}: {}  ({})",
            crate::i18n::tr("Video", "视频"),
            media.display(),
            crate::i18n::tr("kept", "已保留")
        );
    }
    eprintln!(
        "{}: {}",
        crate::i18n::tr("Timeline", "时间线"),
        cfg.timeline_path().display()
    );
    eprintln!();
    eprintln!(
        "{}: {} {} / {} {} / {} {}",
        crate::i18n::tr("Stats", "统计"),
        sections.len(),
        crate::i18n::tr("screenshots", "张截图"),
        speech_n,
        crate::i18n::tr("speech segments", "段语音"),
        chars,
        crate::i18n::tr("chars", "字")
    );
    eprintln!(
        "{}: {}",
        crate::i18n::tr("Elapsed", "耗时"),
        fmt_duration(stats.elapsed_secs)
    );
    match (stats.peak_mb, stats.child_peak_mb) {
        (Some(mb), Some(c)) => eprintln!(
            "{}: {mb:.0} MB (course2md) + {} {c:.0} MB (llama-server/ffmpeg)",
            crate::i18n::tr("Peak memory", "峰值内存"),
            crate::i18n::tr("largest child", "最大子进程")
        ),
        (Some(mb), None) => eprintln!(
            "{}: {mb:.0} MB",
            crate::i18n::tr("Peak memory (process RSS)", "峰值内存（本进程 RSS）")
        ),
        _ => eprintln!(
            "{}: {}",
            crate::i18n::tr("Peak memory", "峰值内存"),
            crate::i18n::tr("unavailable", "不可用")
        ),
    }
    eprintln!(
        "{}: {}",
        crate::i18n::tr("Model dir", "模型目录"),
        cfg.model_dir.display()
    );
    eprintln!("──────────────────────────────");
    if !cfg.llm.enabled && !cfg.llm.disable_hint {
        crate::llm::write_hint_note(&crate::settings::config_path());
    }
}

fn sanitize_stem(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("local")
        .chars()
        .take(40)
        .collect()
}

/// canonical_path + size + mtime 的稳定指纹（8 hex，FNV-1a）。
/// 不用 std DefaultHasher：官方明确不保证跨版本稳定，会破坏 resume/cache 键。
fn local_fingerprint(p: &Path) -> String {
    const FNV_OFFS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFS;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
    };
    feed(
        p.canonicalize()
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .as_bytes(),
    );
    if let Ok(md) = std::fs::metadata(p) {
        feed(&md.len().to_le_bytes());
        if let Ok(m) = md.modified()
            && let Ok(d) = m.duration_since(std::time::UNIX_EPOCH)
        {
            feed(&d.as_secs().to_le_bytes());
        }
    }
    format!("{h:016x}")[..8].to_string()
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

fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m{:02}s", s / 3600, (s % 3600) / 60, s % 60)
    }
}

/// 峰值常驻集（Linux 为 KB，macOS 为字节）。RUSAGE_CHILDREN 口径含 llama-server/ffmpeg。
#[cfg(unix)]
fn peak_rss_mb(who: libc::c_int) -> Option<f64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let rc = unsafe { libc::getrusage(who, usage.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let rss = usage.ru_maxrss as f64;
    #[cfg(target_os = "macos")]
    {
        Some(rss / (1024.0 * 1024.0))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(rss / 1024.0)
    }
}

#[cfg(not(unix))]
fn peak_rss_mb(_who: i32) -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::should_delete_media;

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
}
