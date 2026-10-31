//! ffmpeg 子进程封装：音频抽取（16k 单声道 PCM wav）与媒体探测。

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// ffprobe 读元数据的上限：损坏或卡住的输入不能挂住流程。
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// 有界运行 ffprobe；超时或启动失败返回 None（探测失败均非致命）。
async fn probe_output(cmd: &mut Command) -> Option<std::process::Output> {
    let output = cmd
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .output();
    tokio::time::timeout(PROBE_TIMEOUT, output).await.ok()?.ok()
}

/// 跑子进程并检查退出码；失败时返回带 stderr 尾部摘要的错误（供各模块复用）。
pub(crate) async fn run_cmd(cmd: &mut Command, what: &str) -> Result<std::process::Output> {
    let out = cmd
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| crate::tr!("无法启动 {} / Could not start {}", what, what))?;
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
        Command::new(crate::tools::program("ffmpeg"))
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

/// 视频宽高与时长。用 `-of json` 结构化输出，取代按逗号/空白切分的文本解析。
pub async fn probe_video(media: &Path) -> Option<VideoInfo> {
    let out = probe_output(
        Command::new(crate::tools::program("ffprobe"))
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
            .arg(media),
    )
    .await?;
    if !out.status.success() {
        return None;
    }
    // ffprobe 的 duration 是字符串（如 "95.040000"）
    let probe: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let stream = &probe["streams"][0];
    Some(VideoInfo {
        width: stream["width"].as_u64()? as u32,
        height: stream["height"].as_u64()? as u32,
        duration: probe["format"]["duration"].as_str()?.parse().ok()?,
    })
}

/// 用 ffprobe 拿时长（秒）。失败时返回 None（非致命）。
pub async fn probe_duration(media: &Path) -> Option<f64> {
    let out = probe_output(
        Command::new(crate::tools::program("ffprobe"))
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(media),
    )
    .await?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .flatten()
}
