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
    let mut cmd = Command::new("yt-dlp");
    let _cookies = crate::auth::configure_ytdlp(cmd.as_std_mut(), url)?;
    let out = run(cmd
        .args(["-J", "--no-warnings", "--no-playlist", "--"])
        .arg(url))
    .await?;
    let meta: VideoMeta = serde_json::from_str(&out).context("解析 yt-dlp 元数据 JSON 失败")?;
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
        let _cookies = crate::auth::configure_ytdlp(cmd.as_std_mut(), url)?;
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
            tracing::warn!(auto, error = %e, "yt-dlp 字幕抓取失败");
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
            tracing::warn!(attempt, "retry yt-dlp");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        let mut cmd = Command::new("yt-dlp");
        let _cookies = crate::auth::configure_ytdlp(cmd.as_std_mut(), url)?;
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
                        "yt-dlp 结束但未找到产物（期望 {} 或 {}）",
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
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("yt-dlp 下载失败")))
}

/// Bilibili can reject a request transiently at either the webpage or API stage.
/// Match the extractor and status, not an arbitrary occurrence of "412" in a URL.
pub fn is_bilibili_412(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("[bilibili")
        && (lower.contains("http error 412")
            || lower.contains("http 412")
            || lower.contains("request is blocked by server (412)"))
}

/// Shared by CLI extraction and the desktop preview. At most three attempts.
pub fn bilibili_retry_delay(stderr: &str, retries: usize) -> Option<std::time::Duration> {
    if !is_bilibili_412(stderr) {
        return None;
    }
    [2, 5]
        .get(retries)
        .copied()
        .map(std::time::Duration::from_secs)
}

pub const BILIBILI_412_HINT: &str = "Bilibili 暂时限制了请求（HTTP 412）。请稍后重试；若持续失败，请运行 course2md --login bilibili 登录或重新登录，更新 yt-dlp，并确认该链接能在浏览器播放，也可以导入已下载的本地视频。";

async fn run(cmd: &mut Command) -> Result<String> {
    let mut retries = 0;
    loop {
        let out = cmd
            .kill_on_drop(true)
            .output()
            .await
            .context("启动 yt-dlp 失败")?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(delay) = bilibili_retry_delay(&stderr, retries) {
            retries += 1;
            tracing::warn!(retries, "Bilibili HTTP 412，等待后重试");
            tokio::time::sleep(delay).await;
            continue;
        }
        let error = crate::error::cmd_error("yt-dlp", out.status.code(), &stderr);
        return if is_bilibili_412(&stderr) {
            Err(error.context(BILIBILI_412_HINT))
        } else {
            Err(error)
        };
    }
}

async fn run_status(cmd: &mut Command) -> Result<()> {
    use tokio::io::AsyncBufReadExt;
    let mut child = cmd
        .kill_on_drop(true)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("启动子进程失败")?;
    let stderr = child.stderr.take().context("无法读取 yt-dlp 输出")?;
    let (status, tail) = tokio::join!(child.wait(), async {
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        let mut tail = std::collections::VecDeque::new();
        while let Some(line) = lines.next_line().await? {
            eprintln!("{line}");
            if tail.len() == 5 {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        Ok::<_, std::io::Error>(tail.into_iter().collect::<Vec<_>>().join("\n"))
    });
    let status = status?;
    let tail = tail?;
    if !status.success() {
        let error = crate::error::cmd_error("yt-dlp", status.code(), &tail);
        return if is_bilibili_412(&tail) {
            Err(error.context(BILIBILI_412_HINT))
        } else {
            Err(error)
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bilibili_rejections_are_retried_with_a_limit() {
        for error in [
            "ERROR: [BiliBili] BVabc: HTTP Error 412: Precondition Failed",
            "ERROR: [BilibiliSpaceVideo] Request is blocked by server (412), please wait",
        ] {
            assert_eq!(bilibili_retry_delay(error, 0).unwrap().as_secs(), 2);
            assert_eq!(bilibili_retry_delay(error, 1).unwrap().as_secs(), 5);
            assert!(bilibili_retry_delay(error, 2).is_none());
        }
        for error in [
            "ERROR: [BiliBili] BV412abc: HTTP Error 404",
            "ERROR: [youtube] HTTP Error 412",
            "ERROR: [BiliBili] login required",
        ] {
            assert!(bilibili_retry_delay(error, 0).is_none());
        }
    }

    #[tokio::test]
    #[ignore = "requires yt-dlp and Bilibili network access"]
    async fn live_example_metadata() {
        let meta = fetch_meta("https://www.bilibili.com/video/BV1pb8o6yE8f")
            .await
            .unwrap();
        assert!(meta.title.contains("欢迎来到未来"));
        assert!(meta.duration > 0.);
        assert!(!meta.uploader.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn download_errors_retain_stderr_and_login_guidance() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "echo 'ERROR: [BiliBili] HTTP Error 412: Precondition Failed' >&2; exit 1",
        ]);
        let error = run_status(&mut cmd).await.unwrap_err();
        assert!(error.to_string().contains("--login bilibili"));
        assert!(format!("{error:#}").contains("Precondition Failed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transient_rejection_recovers_and_permanent_failure_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("attempts");
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            r#"
            if [ ! -f "$1" ]; then
                touch "$1"
                echo 'ERROR: [BiliBili] HTTP Error 412: Precondition Failed' >&2
                exit 1
            fi
            echo '{"title":"recovered"}'
        "#,
            "test",
        ])
        .arg(&count);
        assert!(run(&mut cmd).await.unwrap().contains("recovered"));

        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            r#"
            echo attempt >> "$1"
            echo 'ERROR: [BiliBili] HTTP Error 412: Precondition Failed' >&2
            exit 1
        "#,
            "test",
        ])
        .arg(&count);
        let error = run(&mut cmd).await.unwrap_err();
        assert!(error.to_string().contains(BILIBILI_412_HINT));
        assert!(format!("{error:#}").contains("Precondition Failed"));
        assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 3);
    }
}
