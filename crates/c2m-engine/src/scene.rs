//! 幻灯片抽帧：检测与截图在 `slide-detect`，这里是任务进度、取消与错误文字。

use crate::config::{PipelineConfig, SlideMode};
use crate::dispatch::TaskControl;
use crate::media;
use crate::progress::{self, Bar, Stage};
use crate::timeline::FrameEvent;
use anyhow::{Context, Result};
use slide_detect::{Error, Event, ExtractError, Roi, SlideParams};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 场景阶段入口。
pub async fn run(
    cfg: &PipelineConfig,
    video: &std::path::Path,
    job: &crate::job::Job,
) -> Result<Vec<FrameEvent>> {
    let t0 = std::time::Instant::now();
    progress::start(Stage::ScenesScan);
    let probed = media::probe_video(video)
        .await
        .context(crate::tr!("无法读取视频尺寸，请检查文件是否可播放 / Could not read video dimensions; check that the file plays correctly"))?;
    let params = SlideParams {
        sample_interval: cfg.sample_interval,
        similarity: cfg.similarity,
        cooldown: cfg.cooldown,
        stable_secs: match cfg.slide_mode {
            SlideMode::Stable => cfg.stable_secs,
            _ => 0.0,
        },
        roi: cfg.roi.map(|roi| Roi {
            x1: roi.x1,
            y1: roi.y1,
            x2: roi.x2,
            y2: roi.y2,
        }),
    };
    tracing::info!(
        w = probed.width,
        h = probed.height,
        duration = format_args!("{:.0}s", probed.duration),
        interval = params.sample_interval,
        similarity = params.similarity,
        cooldown = params.cooldown,
        "slide sample"
    );
    let info = slide_detect::VideoInfo {
        width: probed.width,
        height: probed.height,
        duration: probed.duration,
    };
    let (ffmpeg, video, dir) = (
        crate::tools::program("ffmpeg"),
        video.to_path_buf(),
        job.frames_dir(),
    );
    let control = job.control.clone();
    // The pipeline drops this future to stop early; the blocking work must follow.
    let abandoned = AbandonOnDrop::default();
    let stop = abandoned.0.clone();
    let slides = tokio::task::spawn_blocking(move || {
        let mut bar = None;
        let slides = slide_detect::detect(
            &ffmpeg,
            &video,
            info,
            &params,
            &dir,
            &|| stop.load(Ordering::Relaxed) || control.check().is_err(),
            &mut |event| report(event, &mut bar),
        )?;
        bar.inspect(Bar::finish);
        progress::done(Stage::ScenesExtract);
        Ok::<_, Error>(slides)
    })
    .await
    .context(crate::tr!(
        "截图任务异常终止 / Slide extraction stopped unexpectedly"
    ))?
    .map_err(|error| error_text(error, &job.control))?;
    tracing::info!(
        frames = slides.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "slides extracted"
    );
    Ok(slides
        .into_iter()
        .map(|slide| FrameEvent {
            t: slide.onset,
            image: format!("frames/{}", slide.file_name),
        })
        .collect())
}

#[derive(Default)]
struct AbandonOnDrop(Arc<AtomicBool>);

impl Drop for AbandonOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn report(event: Event, bar: &mut Option<Bar>) {
    // Emit before the first result: the first image may take a long time.
    let started = |stage, len, template: &str| {
        let bar = Bar::new(stage, len).with_template(template);
        bar.set_position(0);
        Some(bar)
    };
    match event {
        Event::ScanStarted { estimate } => {
            let template = "{spinner:.green} sample {pos}/{len} frames {msg}";
            *bar = started(Stage::ScenesScan, estimate, template);
        }
        Event::Sampled { .. } | Event::Extracted { .. } => {
            bar.as_ref().inspect(|bar| bar.inc(1));
        }
        Event::CandidateFound { slides } => {
            bar.as_ref().inspect(|bar| {
                bar.set_message(crate::tr!(
                    "已找到 {} 张候选截图 / Found {} candidate slides",
                    slides,
                    slides
                ))
            });
        }
        Event::ScanFinished { frames, slides } => {
            bar.as_ref().inspect(|bar| bar.finish());
            tracing::info!(slides, frames, "ssim scan done");
            progress::done(Stage::ScenesScan);
        }
        Event::ExtractStarted { slides } => {
            progress::start(Stage::ScenesExtract);
            let template = "{spinner:.green} extract {pos}/{len} [{bar:32.cyan/blue}] {msg}";
            *bar = started(Stage::ScenesExtract, slides as u64, template);
        }
    }
}

fn error_text(error: Error, control: &TaskControl) -> anyhow::Error {
    match error {
        Error::Cancelled => control.check().err().unwrap_or_else(|| {
            anyhow::anyhow!(crate::tr!("截图已停止 / Slide extraction stopped"))
        }),
        Error::Spawn(source) => anyhow::Error::new(source).context(crate::tr!(
            "无法读取视频画面 / Could not start ffmpeg frame sampling"
        )),
        Error::ScanFailed { status, stderr } => {
            anyhow::anyhow!("读取视频画面失败 / Frame sampling failed ({status}): {stderr}")
        }
        Error::NoFrames => anyhow::anyhow!(crate::tr!("未采样到任何帧 / No frames sampled")),
        Error::Extract {
            number: n,
            onset,
            source,
        } => {
            let cause = match source {
                ExtractError::Spawn(source) => anyhow::Error::new(source).context(crate::tr!(
                    "无法启动 {} / Could not start {}",
                    "ffmpeg",
                    "ffmpeg"
                )),
                ExtractError::Failed { code, stderr } => {
                    crate::error::cmd_error("ffmpeg", code, &stderr)
                }
                ExtractError::Missing(path) => anyhow::anyhow!(
                    "ffmpeg 未生成截图 {0} / ffmpeg did not produce screenshot {0}",
                    path.display()
                ),
            };
            // 中文只写具体原因：界面已用「截图未生成」命名这项失败
            let secs = onset.max(0.) as u64;
            cause.context(format!(
                "第 {n} 张截图（{}:{:02}）提取失败 / Could not extract slide {n} (t={onset:.1}s)",
                secs / 60,
                secs % 60,
            ))
        }
        Error::Io(source) => source.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_image_names_its_slide_and_keeps_the_cause() {
        let error = error_text(
            Error::Extract {
                number: 2,
                onset: 75.5,
                source: ExtractError::Failed {
                    code: Some(1),
                    stderr: "bad frame".into(),
                },
            },
            &TaskControl::default(),
        );
        let text = format!("{error:#}");
        assert!(text.contains("第 2 张截图（1:15）"), "{text}");
        assert!(
            text.contains("Could not extract slide 2 (t=75.5s)"),
            "{text}"
        );
        assert!(text.contains("bad frame"), "{text}");
    }

    #[test]
    fn a_failed_scan_reports_ffmpegs_words() {
        let status = std::process::Command::new(if cfg!(windows) { "cmd" } else { "false" })
            .args(if cfg!(windows) {
                &["/C", "exit 1"][..]
            } else {
                &[][..]
            })
            .status()
            .unwrap();
        let text = error_text(
            Error::ScanFailed {
                status,
                stderr: "no such stream".into(),
            },
            &TaskControl::default(),
        )
        .to_string();
        assert!(text.contains("Frame sampling failed"), "{text}");
        assert!(text.contains("no such stream"), "{text}");
    }
}
