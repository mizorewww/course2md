//! Real source metadata and cached covers, outside the UI thread.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Source {
    pub input: String,
    pub title: String,
    pub author: String,
    pub duration: f64,
    pub cover: Option<PathBuf>,
    pub cover_error: Option<String>,
}
impl Source {
    pub fn detail(&self) -> String {
        let seconds = self.duration.max(0.) as u64;
        let duration = if seconds > 0 {
            format!(
                "{}:{:02}:{:02}",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            )
        } else {
            "时长未知".into()
        };
        format!("{} · {}", self.author, duration)
    }
}

fn command(name: &str, args: &[&str], cancel: &AtomicBool) -> Result<Vec<u8>> {
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    let mut command = Command::new(name);
    command
        .args(args)
        .env("PATH", crate::backend::tool_path())
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("无法启动 {name}，请在设置中检查运行环境"))?;
    let start = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(45) {
            crate::backend::terminate(&mut child);
            let _ = child.wait();
            anyhow::bail!("预览已取消或超时，请检查链接及网络后重试");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                crate::backend::terminate(&mut child);
                let _ = child.wait();
                return Err(e.into());
            }
        }
    };
    use std::io::{Read, Seek};
    let read = |mut file: std::fs::File| -> Result<Vec<u8>> {
        file.rewind()?;
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let error_bytes = read(stderr)?;
    let error_text = String::from_utf8_lossy(&error_bytes);
    if !status.success() && error_text.contains("HTTP Error 412") {
        anyhow::bail!(
            "视频平台暂时限制了请求。请稍后重试，也可以导入已下载的本地视频。（HTTP 412）"
        );
    }
    ensure!(
        status.success(),
        "{}",
        error_text.chars().take(700).collect::<String>()
    );
    read(stdout)
}

pub fn inspect(input: String, online: bool, cancel: Arc<AtomicBool>) -> Result<Source> {
    let cache = course2md::config::cache_dir().join("covers");
    std::fs::create_dir_all(&cache)?;
    let mut source = Source {
        input: input.clone(),
        title: String::new(),
        author: String::new(),
        duration: 0.,
        cover: None,
        cover_error: None,
    };
    if online {
        let url = url::Url::parse(&input)
            .context("请输入完整的视频链接，例如 https://www.youtube.com/watch?v=…")?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "请输入 http 或 https 视频链接"
        );
        #[derive(Deserialize)]
        struct Meta {
            title: String,
            #[serde(default)]
            uploader: Option<String>,
            #[serde(default)]
            duration: Option<f64>,
            #[serde(default)]
            thumbnail: Option<String>,
        }
        let bytes = command(
            "yt-dlp",
            &[
                "--simulate",
                "--dump-single-json",
                "--no-playlist",
                "--no-warnings",
                "--socket-timeout",
                "12",
                "--retries",
                "1",
                "--",
                &input,
            ],
            &cancel,
        )?;
        let meta: Meta = serde_json::from_slice(&bytes).context("无法读取视频信息")?;
        source.title = meta.title;
        source.author = meta
            .uploader
            .unwrap_or_else(|| url.host_str().unwrap_or("在线课程").into());
        source.duration = meta.duration.unwrap_or(0.);
        let cover = (|| -> Result<PathBuf> {
            let url = meta.thumbnail.context("该视频没有提供封面")?;
            let response = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(12))
                .build()
                .get(&url)
                .call()?;
            use std::io::Read;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(12 * 1024 * 1024)
                .read_to_end(&mut bytes)?;
            let image = image::load_from_memory(&bytes).context("封面格式无法识别")?;
            let file = tempfile::Builder::new()
                .suffix(".jpg")
                .tempfile_in(&cache)?;
            image
                .thumbnail(1280, 720)
                .to_rgb8()
                .save_with_format(file.path(), image::ImageFormat::Jpeg)?;
            Ok(file.keep()?.1)
        })();
        match cover {
            Ok(path) => source.cover = Some(path),
            Err(e) => source.cover_error = Some(format!("封面暂不可用：{e}")),
        }
    } else {
        let path = course2md::config::expand_tilde(input.clone().into());
        ensure!(path.is_file(), "视频文件不存在，请重新选择");
        source.input = path.display().to_string();
        let bytes = command(
            "ffprobe",
            &[
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
                &source.input,
            ],
            &cancel,
        )?;
        let meta: serde_json::Value = serde_json::from_slice(&bytes)?;
        ensure!(
            meta["streams"]
                .as_array()
                .is_some_and(|streams| streams.iter().any(|s| s["codec_type"] == "video")),
            "所选文件不包含视频画面"
        );
        source.title = meta["format"]["tags"]["title"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        source.author = "本地视频".into();
        source.duration = meta["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.);
        let file = tempfile::Builder::new()
            .suffix(".jpg")
            .tempfile_in(&cache)?;
        let offset = (source.duration * 0.1).min(10.).to_string();
        let cover = command(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-y",
                "-ss",
                &offset,
                "-i",
                &source.input,
                "-frames:v",
                "1",
                "-vf",
                "scale=960:-2",
                file.path().to_str().context("封面缓存路径无法编码")?,
            ],
            &cancel,
        );
        match cover {
            Ok(_) => source.cover = Some(file.keep()?.1),
            Err(e) => source.cover_error = Some(format!("封面暂不可用：{e}")),
        }
    }
    Ok(source)
}

pub fn save_cover(source: &Source, dir: &Path) -> Result<()> {
    if let Some(path) = &source.cover {
        course2md::checkpoint::atomic_write(&dir.join("cover.jpg"), &std::fs::read(path)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires yt-dlp and public network access"]
    fn online_previews_resolve_real_titles_and_decodable_covers() {
        for url in [
            "https://www.youtube.com/watch?v=YE7VzlLtp-4",
            "https://www.bilibili.com/video/BV1pb8o6yE8f",
        ] {
            let source = inspect(url.into(), true, Arc::new(AtomicBool::new(false))).unwrap();
            assert!(!source.title.is_empty());
            assert!(!source.author.is_empty());
            assert!(source.duration > 0.);
            let cover = source
                .cover
                .as_ref()
                .unwrap_or_else(|| panic!("{url}: {:?}", source.cover_error));
            let image = image::open(cover).unwrap();
            assert!(image.width() > 100 && image.height() > 100);
            let output = tempfile::tempdir().unwrap();
            save_cover(&source, output.path()).unwrap();
            assert!(image::open(output.path().join("cover.jpg")).is_ok());
            std::fs::remove_file(cover).unwrap();
        }
    }
    #[test]
    #[ignore = "requires ffmpeg and ffprobe"]
    fn local_preview_reads_video_and_rejects_non_video_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Lecture.mp4");
        let cancel = Arc::new(AtomicBool::new(false));
        command(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=320x180:d=1",
                "-metadata",
                "title=Local lecture",
                "-y",
                path.to_str().unwrap(),
            ],
            &cancel,
        )
        .unwrap();
        let source = inspect(path.display().to_string(), false, cancel.clone()).unwrap();
        assert_eq!(source.title, "Local lecture");
        assert!(source.duration > 0.);
        let cover = source.cover.unwrap();
        assert!(image::open(&cover).is_ok());
        std::fs::remove_file(cover).unwrap();
        let text = dir.path().join("not-video.txt");
        std::fs::write(&text, "not a video").unwrap();
        assert!(inspect(text.display().to_string(), false, cancel).is_err());
    }
    #[test]
    fn unsupported_source_is_rejected_before_running_tools() {
        assert!(
            inspect(
                "file:///private/file".into(),
                true,
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
    }
}
