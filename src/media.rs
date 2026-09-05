//! ffmpeg 子进程封装：音频抽取（16k 单声道 PCM wav）与媒体探测。

use anyhow::{Context, Result};
use std::path::Path;
use tokio::process::Command;

/// 跑子进程并检查退出码；失败时返回带 stderr 尾部摘要的错误（供各模块复用）。
pub(crate) async fn run_cmd(cmd: &mut Command, what: &str) -> Result<std::process::Output> {
    let out = cmd
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("启动 {what} 失败"))?;
    if !out.status.success() {
        return Err(crate::error::cmd_error(
            what,
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(out)
}

/// 抽取 16kHz 单声道 s16 wav。已存在则跳过。
pub async fn extract_audio(media: &Path, dest: &Path) -> Result<()> {
    if dest.is_file() {
        tracing::info!(path = %dest.display(), "audio exists, skip extract");
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        tokio::fs::create_dir_all(p).await?;
    }
    let temporary = tempfile::Builder::new()
        .suffix(".wav")
        .tempfile_in(dest.parent().unwrap_or(Path::new(".")))?;
    run_cmd(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(media)
            .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
            .arg(temporary.path()),
        "ffmpeg",
    )
    .await?;
    temporary.persist(dest)?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
}

/// ffprobe `-of json` 输出中我们关心的字段子集。
#[derive(serde::Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(serde::Deserialize)]
struct ProbeStream {
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(serde::Deserialize)]
struct ProbeFormat {
    /// ffprobe 的 duration 是字符串（如 "95.040000"）
    duration: Option<String>,
}

/// 视频宽高与时长。用 `-of json` 结构化输出，取代按逗号/空白切分的文本解析。
pub async fn probe_video(media: &Path) -> Option<VideoInfo> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height:format=duration",
            "-of",
            "json",
        ])
        .arg(media)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let probe: ProbeJson = serde_json::from_slice(&out.stdout).ok()?;
    let stream = probe.streams.first()?;
    let duration = probe
        .format
        .and_then(|f| f.duration)
        .and_then(|d| d.parse::<f64>().ok())?;
    Some(VideoInfo {
        width: stream.width?,
        height: stream.height?,
        duration,
    })
}

/// `probe_duration` 两个入口共享的 ffprobe 参数（只输出时长数字）。
fn duration_args() -> [&'static str; 6] {
    [
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
    ]
}

/// 从 ffprobe 输出解析时长（秒）；进程失败或输出非法均为 None（非致命）。
fn parse_duration(out: &std::process::Output) -> Option<f64> {
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .ok()
}

/// 用 ffprobe 拿时长（秒）。失败时返回 None（非致命）。
pub async fn probe_duration(media: &Path) -> Option<f64> {
    let out = Command::new("ffprobe")
        .args(duration_args())
        .arg(media)
        .output()
        .await
        .ok()?;
    parse_duration(&out)
}

/// [`probe_duration`] 的同步版（供阻塞线程使用）。
pub fn probe_duration_blocking(media: &Path) -> Option<f64> {
    let out = std::process::Command::new("ffprobe")
        .args(duration_args())
        .arg(media)
        .output()
        .ok()?;
    parse_duration(&out)
}
