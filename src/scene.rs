//! 幻灯片抽帧：对齐 yt-slide-mark
//! 每 `sample_interval` 秒取一帧，与上一张保留帧做 SSIM；
//! 低于 `similarity` 则保存，并跳过 `cooldown` 秒。

use crate::config::{PipelineConfig, Roi};
use crate::media;
use crate::timeline::FrameEvent;
use anyhow::{Context, Result};
use image::GrayImage;
use image_compare::Algorithm;
use std::borrow::Cow;
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// SSIM 采样宽度：足够区分版面变化，同时控制 rawvideo 带宽与 SSIM 计算开销
const SAMPLE_WIDTH: u32 = 640;
/// 全分辨率抽帧并发度：每帧一个 ffmpeg 进程，4 路即可利用并行度又不至于进程风暴
const EXTRACT_CONCURRENCY: usize = 4;

fn scaled_wh(ow: u32, oh: u32, target_w: u32) -> (u32, u32) {
    let h = ((oh as u64 * target_w as u64) / ow.max(1) as u64) as u32;
    (target_w, (h & !1).max(2))
}

/// 无 ROI 时借用原图（零拷贝）；有 ROI 时返回裁剪后的新图。
fn crop_roi(img: &GrayImage, roi: Option<Roi>) -> Cow<'_, GrayImage> {
    let Some(r) = roi else {
        return Cow::Borrowed(img);
    };
    let (w, h) = img.dimensions();
    let (x1, y1, x2, y2) = r.pixels(w, h);
    Cow::Owned(
        image::imageops::crop_imm(img, x1, y1, (x2 - x1).max(1), (y2 - y1).max(1)).to_image(),
    )
}

fn ssim(a: &GrayImage, b: &GrayImage) -> f64 {
    image_compare::gray_similarity_structure(&Algorithm::MSSIMSimple, a, b)
        .map(|s| s.score)
        .unwrap_or(0.0)
}

/// 单点精确抽帧（全分辨率 JPEG）。
async fn extract_frame(media: &Path, t: f64, dest: &Path) -> Result<()> {
    let ss = format!("{t:.3}");
    // -q:v 2：JPEG 高质量档（2~31，越小越好），讲义文字可读性优先
    media::run_cmd(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &ss])
            .arg("-i")
            .arg(media)
            .args(["-frames:v", "1", "-q:v", "2"])
            .arg(dest),
        "ffmpeg",
    )
    .await?;
    Ok(())
}

/// 1fps（或 sample_interval）灰度流 + SSIM，得到新幻灯片时间点。
async fn sample_timestamps(cfg: &PipelineConfig, media: &Path) -> Result<Vec<(f64, f64)>> {
    let info = media::probe_video(media)
        .await
        .context("ffprobe 无法读取视频宽高")?;
    // sample_interval 下限 0.2s（即采样率上限 5fps）；配置被收紧时必须告知用户
    let interval = if cfg.sample_interval < 0.2 {
        tracing::warn!(
            configured = cfg.sample_interval,
            "sample_interval 小于下限 0.2s，按 0.2s 采样"
        );
        0.2
    } else {
        cfg.sample_interval
    };
    let (tw, th) = scaled_wh(info.width, info.height, SAMPLE_WIDTH);
    let fps = 1.0 / interval;
    let vf = format!("fps={fps:.6},scale={tw}:{th},format=gray");
    let total = ((info.duration / interval).ceil() as u64).max(1);

    tracing::info!(
        w = info.width,
        h = info.height,
        duration = format_args!("{:.0}s", info.duration),
        interval,
        similarity = cfg.similarity,
        cooldown = cfg.cooldown,
        "slide sample"
    );

    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-an"])
        .arg("-i")
        .arg(media)
        .args(["-vf", &vf, "-f", "rawvideo", "pipe:1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("启动 ffmpeg 采样失败")?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout")?;
    // stderr 必须全程 drain：piped 而不读的话，写满 64KB 管道缓冲会死锁。
    // 缓存尾部供失败诊断，debug 时逐行转发。
    let mut stderr_buf: Vec<u8> = Vec::new();
    let mut stderr = child.stderr.take().context("ffmpeg stderr")?;
    let stderr_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let _ = stderr.read_to_end(&mut stderr_buf).await;
        stderr_buf
    });
    let frame_len = (tw as usize) * (th as usize);
    let mut buf = vec![0u8; frame_len];
    let pb = crate::progress::Bar::new("scenes", total)
        .with_template("{spinner:.green} sample {pos}/{len} [{bar:32.cyan/blue}] {msg}");

    // 三状态检测：检测永不休眠（cooldown 只限制「发射」，不再造成盲区）。
    //   last_emitted  已输出的视觉状态
    //   candidate     正在观察的候选画面（含首次出现时间，用于真实时间戳）
    // 发射条件：候选与上一输出差异显著 + 稳定时长足够 + 距上次发射 >= cooldown。
    let stable_for = if matches!(cfg.slide_mode, crate::config::SlideMode::Stable) {
        cfg.stable_secs
    } else {
        0.0
    };
    let mut times: Vec<(f64, f64)> = vec![];
    let mut last_emitted: Option<GrayImage> = None;
    let mut candidate: Option<GrayImage> = None;
    let mut candidate_first_t: Option<f64> = None;
    let mut candidate_last_t: Option<f64> = None;
    let mut last_emit_t: f64 = -f64::INFINITY;
    let mut i: u64 = 0;
    loop {
        match stdout.read_exact(&mut buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        // 理想采样时刻 t = i * interval：fps 滤镜的取整与 VFR 补帧会让真实帧
        // 时间略有漂移（通常 < 一个采样间隔）；场景检测只需秒级精度，可接受，
        // 故不逐帧解析 pts。
        let t = i as f64 * interval;
        i += 1;
        pb.inc(1);
        let gray = GrayImage::from_raw(tw, th, buf.clone())
            .ok_or_else(|| anyhow::anyhow!("灰度帧尺寸不匹配 {tw}x{th}"))?;
        let cmp = crop_roi(&gray, cfg.roi);

        let differs_from_emitted = match &last_emitted {
            None => true,
            Some(prev) => {
                prev.dimensions() != cmp.dimensions() || ssim(prev, &cmp) < cfg.similarity
            }
        };
        if !differs_from_emitted {
            // 画面回到已输出状态：候选是过渡帧（动画/抖动），丢弃
            candidate = None;
            candidate_first_t = None;
            continue;
        }
        // 与已输出状态不同：跟踪候选（若候选本身又变了，说明动画进行中，重置起点）
        let candidate_changed = match &candidate {
            None => true,
            Some(c) => c.dimensions() != cmp.dimensions() || ssim(c, &cmp) < cfg.similarity,
        };
        if candidate_changed {
            // 仅候选变更时才物化为拥有所有权的图；其余路径 cmp 只借不拷
            candidate = Some(cmp.into_owned());
            candidate_first_t = Some(t);
            candidate_last_t = Some(t);
        } else if candidate_last_t.is_some() {
            // 同一视觉状态的后续采样：更新代表帧时间（取稳定后的最后一帧）
            candidate_last_t = Some(t);
        }
        let onset_t = candidate_first_t.unwrap_or(t);
        let capture_t = candidate_last_t.unwrap_or(t);
        let stable = t - onset_t >= stable_for;
        let gap_ok = t - last_emit_t >= cfg.cooldown;
        if stable && gap_ok {
            // 发射两个时间戳：onset 用于时间线对齐，capture 用于全分辨率截帧
            // （stable 模式下 capture 取确认稳定时的代表帧，避开 transition 早期态）
            times.push((onset_t, capture_t));
            last_emitted = candidate.take();
            candidate_first_t = None;
            candidate_last_t = None;
            last_emit_t = t;
            pb.set_message(format!("slides={} t={onset_t:.1}s", times.len()));
        }
    }
    // EOF 冲刷：最后一个已稳定的候选即使还没过 cooldown 也补发（否则尾页永远丢失）
    if let (Some(_), Some(onset), Some(capture)) = (&candidate, candidate_first_t, candidate_last_t)
    {
        let stable = capture - onset >= stable_for;
        if stable {
            times.push((onset, capture));
        }
    }
    pb.finish();
    let status = child.wait().await?;
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    if !status.success() {
        let tail = String::from_utf8_lossy(&stderr_bytes)
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("ffmpeg 采样进程异常退出（{status}）：{tail}");
    }
    tracing::info!(slides = times.len(), frames = i, mode = %cfg.slide_mode, "ssim scan done");
    Ok(times)
}

/// 场景阶段入口。
pub async fn run(cfg: &PipelineConfig, media: &Path) -> Result<Vec<FrameEvent>> {
    let frames_dir = cfg.frames_dir();
    tokio::fs::create_dir_all(&frames_dir).await?;
    let t0 = std::time::Instant::now();
    let times = sample_timestamps(cfg, media).await?;
    anyhow::ensure!(!times.is_empty(), "未采样到任何帧");

    let pb = crate::progress::Bar::new("scenes", times.len() as u64)
        .with_template("{spinner:.green} extract {pos}/{len} [{bar:32.cyan/blue}] {msg}");
    // JoinSet 限流并发抽帧：最多 EXTRACT_CONCURRENCY 个 ffmpeg 进程并行；
    // 按帧索引收集结果后排序，保证输出文件名与结果顺序与串行版完全一致。
    let mut set: tokio::task::JoinSet<(usize, Result<()>)> = tokio::task::JoinSet::new();
    let mut pending = times.iter().copied().enumerate();
    let mut results: Vec<(usize, Result<()>)> = Vec::with_capacity(times.len());
    loop {
        while set.len() < EXTRACT_CONCURRENCY {
            let Some((i, (_, capture_t))) = pending.next() else {
                break;
            };
            // 截帧用代表帧时间（稳定后），时间线用 onset（首次出现）
            let path = frames_dir.join(format!("slide_{:04}.jpg", i + 1));
            let media = media.to_path_buf();
            set.spawn(async move { (i, extract_frame(&media, capture_t, &path).await) });
        }
        match set.join_next().await {
            Some(Ok(item)) => {
                pb.set_message(format!("t={:.1}s", times[item.0].0));
                pb.inc(1);
                results.push(item);
            }
            Some(Err(e)) => return Err(e).context("抽帧任务异常终止"),
            None => break,
        }
    }
    pb.finish();
    results.sort_by_key(|(i, _)| *i);
    let mut frames = Vec::with_capacity(results.len());
    for (i, r) in results {
        let (onset_t, _) = times[i];
        r.with_context(|| format!("抽取第 {} 帧（t={onset_t:.1}s）失败", i + 1))?;
        let name = format!("slide_{:04}.jpg", i + 1);
        frames.push(FrameEvent {
            t: onset_t,
            image: format!("frames/{name}"),
        });
    }
    tracing::info!(
        frames = frames.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "slides extracted"
    );
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_keeps_even_height() {
        let (w, h) = scaled_wh(1280, 410, 640);
        assert_eq!(w, 640);
        assert_eq!(h % 2, 0);
    }
}
