//! yt-dlp 子进程封装：元数据抓取 + 视频下载。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// 我们关心的元数据字段子集。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMeta {
    pub title: String,
    #[serde(default)]
    pub uploader: String,
    #[serde(default)]
    pub duration: f64,
    pub webpage_url: String,
    #[serde(default)]
    pub extractor: String,
    #[serde(default)]
    pub id: String,
}

impl VideoMeta {
    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// 抓取元数据（不下载）。
pub async fn fetch_meta(url: &str) -> Result<VideoMeta> {
    let out = run(Command::new("yt-dlp")
        .args(["-J", "--no-warnings", "--no-playlist"])
        .arg(url))
    .await?;
    let meta: VideoMeta = serde_json::from_str(&out)
        .context("无法读取视频信息 / Could not parse video metadata from yt-dlp")?;
    Ok(meta)
}

/// 抓取的平台字幕（yt-dlp 产物）。
pub struct SubtitleFetch {
    pub path: PathBuf,
    /// true = 平台自动生成字幕（auto-caption）
    pub auto: bool,
}

/// 用 yt-dlp 获取平台字幕并转为 srt：先人工字幕，再自动字幕。
/// 平台不提供字幕时 yt-dlp 正常退出但不产出文件 → 返回 None。
pub async fn fetch_subtitle(url: &str, out_dir: &Path) -> Result<Option<SubtitleFetch>> {
    let dir = out_dir.join(".subs");
    // 每次重新抓取，避免读到上次运行残留的旧字幕
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir).await?;
    let tmpl = dir.join("sub");
    for auto in [false, true] {
        let mut cmd = Command::new("yt-dlp");
        cmd.args([
            "--skip-download",
            "--no-playlist",
            // 转为 srt：pick_subtitle_file（subtitle.rs）只认 .srt，两处约定需保持一致
            "--convert-subs",
            "srt",
            "--sub-format",
            "srt/vtt/best",
            "--sub-langs",
            // 语言偏好与 subtitle.rs 的 lang_rank 同源
            crate::subtitle::SUB_LANGS,
            "-o",
        ])
        .arg(&tmpl);
        if auto {
            cmd.arg("--write-auto-subs");
        } else {
            cmd.arg("--write-subs");
        }
        cmd.arg(url);
        // 命令失败（yt-dlp 缺失/网络错误）：记 warn（错误内含 stderr 尾部摘要）后继续尝试 auto；
        // 命令成功但无产物（平台无字幕，yt-dlp 打 warning 后正常退出）不算错误
        if let Err(e) = run(&mut cmd).await {
            tracing::warn!(auto, error = %e, "字幕获取失败，将尝试其他字幕 / Subtitle fetch failed; trying other captions");
            continue;
        }
        if let Some(path) = crate::subtitle::pick_subtitle_file(&dir) {
            return Ok(Some(SubtitleFetch { path, auto }));
        }
    }
    Ok(None)
}

/// 本地视频的同名字幕 sidecar（lecture.mp4 → lecture.srt/.vtt）。
pub fn sidecar_subtitle(video: &Path) -> Option<SubtitleFetch> {
    crate::subtitle::sidecar_subtitle(video).map(|path| SubtitleFetch { path, auto: false })
}

/// 下载视频到 `dest`（默认 1080p 上限，mp4 合并）。已存在则跳过。
pub async fn download(url: &str, dest: &Path, max_height: u32, verbose: bool) -> Result<()> {
    if dest.is_file() {
        tracing::info!(path = %dest.display(), "media exists, skip download");
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        tokio::fs::create_dir_all(p).await?;
    }
    let tmp: PathBuf = dest.with_extension("mp4.part");
    // 网络类错误重试 2 次
    let mut last_err = None;
    for attempt in 0..3 {
        if attempt > 0 {
            tracing::warn!(attempt, "视频下载失败，正在重试 / Retrying video download");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        let mut cmd = Command::new("yt-dlp");
        let fmt = format!("bv*[height<={max_height}]+ba/b[height<={max_height}]/b");
        cmd.args([
            "-f",
            &fmt,
            "-S",
            "ext:mp4:m4a",
            "--merge-output-format",
            "mp4",
            "--no-playlist",
            "--no-part",
            "--no-progress",
            "-o",
        ])
        .arg(&tmp)
        .arg(url);
        if verbose {
            cmd.arg("-v");
        }
        match run_status(&mut cmd).await {
            Ok(()) => {
                // 新版 yt-dlp 在 merge 时会按 --merge-output-format 再补后缀：
                // -o media.mp4.part 实际产出 media.mp4.part.mp4。两种命名都兼容。
                // OsString 拼接而非 format!("{}", display())：非 UTF-8 路径也能正确处理
                let merged = {
                    let mut s = tmp.clone().into_os_string();
                    s.push(".mp4");
                    PathBuf::from(s)
                };
                let produced = if merged.is_file() {
                    merged
                } else if tmp.is_file() {
                    tmp
                } else {
                    anyhow::bail!(
                        "未找到下载的视频 / Downloaded video missing (expected {} or {})",
                        tmp.display(),
                        merged.display()
                    );
                };
                tokio::fs::rename(&produced, dest).await?;
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("视频下载失败 / Video download failed")))
}

async fn run(cmd: &mut Command) -> Result<String> {
    let out = crate::media::run_cmd(cmd, "yt-dlp").await?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn run_status(cmd: &mut Command) -> Result<()> {
    let out = crate::media::run_cmd(cmd, "yt-dlp").await?;
    if !out.stderr.is_empty() {
        tracing::debug!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
