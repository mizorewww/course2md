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
fn parse_duration_text(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok()
}

fn parse_duration(out: &std::process::Output) -> Option<f64> {
    out.status
        .success()
        .then(|| parse_duration_text(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// 用 ffprobe 拿时长（秒）。失败时返回 None（非致命）。
pub async fn probe_duration(media: &Path) -> Option<f64> {
    let out = probe_output(
        Command::new(crate::tools::program("ffprobe"))
            .args(duration_args())
            .arg(media),
    )
    .await?;
    parse_duration(&out)
}

/// [`probe_duration`] 的同步版（供阻塞线程使用）。
pub fn probe_duration_blocking(media: &Path) -> Option<f64> {
    let mut cmd = std::process::Command::new(crate::tools::program("ffprobe"));
    cmd.args(duration_args()).arg(media);
    let out = crate::runtime::run_bounded("ffprobe", &mut cmd, PROBE_TIMEOUT).ok()?;
    out.status
        .success()
        .then(|| parse_duration_text(&out.stdout))
        .flatten()
}
