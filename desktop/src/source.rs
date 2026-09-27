//! Real source metadata and cached covers, outside the UI thread.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Source {
    pub input: String,
    pub title: String,
    pub author: String,
    pub duration: f64,
    pub cover: Option<PathBuf>,
    pub cover_error: Option<String>,
    /// Stable extractor identity (including a part ID), or verified local content identity.
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub subtitles: course2md::subtitle::SubtitleEvidence,
    #[serde(default)]
    pub selected_subtitle: Option<course2md::subtitle::CachedSubtitle>,
    #[serde(default)]
    pub original_language: Option<String>,
    #[serde(default)]
    pub subtitle_request: Option<course2md::subtitle::SubtitleTrack>,
    #[serde(default)]
    pub subtitle_read_error: Option<course2md::subtitle::SubtitleReadError>,
}
impl Source {
    pub fn detail(&self) -> String {
        let seconds = self.duration.max(0.) as u64;
        let mut details = Vec::new();
        if !self.author.trim().is_empty() {
            details.push(self.author.clone());
        }
        if seconds > 0 {
            // 与 fmt_ts 同一记法（07:37，不带 0 小时位）：跨页时长一致（review2#8）
            details.push(course2md::render::fmt_ts(seconds as f64));
        }
        details.join(" · ")
    }
}

pub use course2md::fetch::SourceCandidate;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SourceProbe {
    Single(Source),
    Collection {
        title: String,
        candidates: Vec<SourceCandidate>,
        unavailable_entries: usize,
    },
    Unresolved {
        message: String,
    },
}

struct CommandOutput {
    stdout: Vec<u8>,
    stderr: String,
}

#[derive(Debug, PartialEq, Eq)]
enum FailureKind {
    NotFound,
    Forbidden,
    Timeout,
    RateLimited,
    LoginRequired,
    Other,
}

fn failure_kind(message: &str) -> FailureKind {
    let lower = message.to_ascii_lowercase();
    let http = |code: &str| {
        lower.lines().any(|line| {
            line.contains(code) && (line.contains("http") || line.contains("status code"))
        })
    };
    if http("404") || lower.contains("video not found") || lower.contains("视频不存在") {
        FailureKind::NotFound
    } else if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("超过 45 秒")
        || lower.contains("连接超时")
    {
        FailureKind::Timeout
    } else if http("429") || http("412") || lower.contains("too many requests") {
        FailureKind::RateLimited
    } else if http("403") || lower.contains("access denied") || lower.contains("权限不足") {
        FailureKind::Forbidden
    } else if [
        "sign in",
        "sign-in",
        "log in",
        "logged in",
        "please login",
        "login required",
        "authentication required",
        "请登录",
        "需要登录",
        "登录后",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
    {
        FailureKind::LoginRequired
    } else {
        FailureKind::Other
    }
}

/// Repair actions follow the actual response, never a platform-wide login tip.
pub fn is_login_failure(message: &str) -> bool {
    failure_kind(message) == FailureKind::LoginRequired
}

fn preview_command_error(program: &str, code: Option<i32>, stderr: &str) -> anyhow::Error {
    let mut lines = Vec::new();
    for line in stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    let cause = lines
        .iter()
        .rev()
        .find(|line| line.starts_with("ERROR:"))
        .or_else(|| lines.last())
        .copied()
        .unwrap_or_default();
    let summary = match failure_kind(stderr) {
        FailureKind::NotFound => "无法找到这个视频，请检查链接或确认视频仍可访问。".to_owned(),
        FailureKind::Forbidden => {
            "视频平台拒绝了这次访问。请确认该视频的访问权限，也可以选择本地视频。".to_owned()
        }
        FailureKind::Timeout => "读取视频信息超时，请检查网络后重新读取。".to_owned(),
        FailureKind::RateLimited => {
            "视频平台暂时限制了读取请求，请稍后重新读取，也可以选择本地视频。".to_owned()
        }
        FailureKind::LoginRequired => "视频平台要求登录后读取，请登录对应账号后重试。".to_owned(),
        FailureKind::Other if !cause.is_empty() => {
            cause.trim_start_matches("ERROR:").trim().to_owned()
        }
        FailureKind::Other => format!(
            "{program} 未能完成读取（退出码 {}），请重新读取。",
            code.map(|code| code.to_string())
                .unwrap_or_else(|| "未提供".into())
        ),
    };
    let details = lines
        .iter()
        .rev()
        .take(8)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if details.is_empty()
        || details == summary
        || details.trim_start_matches("ERROR:").trim() == summary
    {
        anyhow::anyhow!(summary)
    } else {
        anyhow::anyhow!("{summary}\n{details}")
    }
}

fn subtitle_failure_message(error: &anyhow::Error) -> String {
    let message = format!("{error:#}");
    let summary = match failure_kind(&message) {
        FailureKind::NotFound => "字幕链接暂时无法访问，请重新读取字幕，或选择其他文字来源。",
        FailureKind::Forbidden => {
            "平台拒绝了这次字幕读取，请确认视频的访问权限，或选择其他文字来源。"
        }
        FailureKind::Timeout => "读取字幕超时，请检查网络后重新读取字幕。",
        FailureKind::RateLimited => "平台暂时限制了字幕读取，请稍后重试，或选择其他文字来源。",
        FailureKind::LoginRequired => "平台要求登录后读取字幕，请登录对应账号后重试。",
        FailureKind::Other => return message,
    };
    let details = message.lines().skip(1).collect::<Vec<_>>().join("\n");
    if details.is_empty() {
        summary.into()
    } else {
        format!("{summary}\n{details}")
    }
}

fn command(name: &str, args: &[&str], cancel: &AtomicBool) -> Result<Vec<u8>> {
    command_output(name, args, cancel).map(|output| output.stdout)
}

fn command_output(name: &str, args: &[&str], cancel: &AtomicBool) -> Result<CommandOutput> {
    let start = Instant::now();
    let mut retries = 0;
    loop {
        let result = command_once(name, args, cancel, start);
        let Err(error) = &result else { return result };
        let message = error.to_string();
        if name != "yt-dlp"
            || cancel.load(Ordering::Relaxed)
            || start.elapsed() >= Duration::from_secs(45)
        {
            return result;
        }
        if !course2md::fetch::is_bilibili_412(&message) {
            return result;
        }
        let Some(delay) = course2md::fetch::bilibili_retry_delay(&message, retries) else {
            return result.context(course2md::fetch::BILIBILI_412_HINT);
        };
        retries += 1;
        let retry_at = Instant::now() + delay;
        while Instant::now() < retry_at {
            check_preview_deadline(cancel, start)?;
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn check_preview_deadline(cancel: &AtomicBool, start: Instant) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
    ensure!(
        start.elapsed() < Duration::from_secs(45),
        "读取超过 45 秒，请重新读取；也可以选择本地视频"
    );
    Ok(())
}

fn command_once(
    name: &str,
    args: &[&str],
    cancel: &AtomicBool,
    start: Instant,
) -> Result<CommandOutput> {
    check_preview_deadline(cancel, start)?;
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    let mut command = Command::new(name);
    let _cookies = if name == "yt-dlp" {
        course2md::auth::configure_ytdlp(&mut command, args.last().copied().unwrap_or_default())?
    } else {
        None
    };
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
    let status = loop {
        if cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(45) {
            crate::backend::terminate(&mut child);
            let _ = child.wait();
            check_preview_deadline(cancel, start)?;
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
        file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 32 * 1024 * 1024,
            "返回的视频信息超过读取上限，请复制具体单集的链接"
        );
        Ok(bytes)
    };
    let error_bytes = read(stderr)?;
    let error_text = String::from_utf8_lossy(&error_bytes);
    ensure!(
        status.success(),
        "{}",
        preview_command_error(name, status.code(), &error_text)
    );
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
    Ok(CommandOutput {
        stdout: read(stdout)?,
        stderr: error_text.into_owned(),
    })
}

pub fn validate_url(input: &str) -> Result<url::Url> {
    let url =
        url::Url::parse(input).context("请输入完整的视频链接，以 https:// 或 http:// 开头")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "请输入完整的视频链接，以 https:// 或 http:// 开头"
    );
    Ok(url)
}

/// Share text is data. Extract actual HTTP(S) links and require an explicit
/// choice when more than one is present; never execute surrounding text.
pub fn video_links(input: &str) -> Vec<String> {
    let mut links = Vec::new();
    for word in input.split_whitespace() {
        let Some(start) = word.find("https://").or_else(|| word.find("http://")) else {
            continue;
        };
        let candidate = word[start..].trim_end_matches([
            '。', '，', ',', '；', ';', '！', '!', '）', ')', '】', ']', '》', '>', '"', '”', '’',
            '\'',
        ]);
        if validate_url(candidate).is_ok() && !links.iter().any(|link| link == candidate) {
            links.push(candidate.to_owned());
        }
    }
    links
}

/// Compatibility wrapper. Call `probe` in the UI to present collection candidates.
#[cfg(test)]
pub fn inspect(input: String, online: bool, cancel: Arc<AtomicBool>) -> Result<Source> {
    match probe(input, online, cancel)? {
        SourceProbe::Single(source) => Ok(source),
        SourceProbe::Collection { .. } => {
            anyhow::bail!("这个链接包含多个视频，请选择具体单集后生成笔记")
        }
        SourceProbe::Unresolved { message } => anyhow::bail!("{message}"),
    }
}

/// Read metadata and subtitle candidates without downloading video or running AI.
/// The caller owns draft/revision matching; every successful return also checks
/// cancellation, including after optional cover work and local content hashing.
pub fn probe(input: String, online: bool, cancel: Arc<AtomicBool>) -> Result<SourceProbe> {
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
    if !online {
        return inspect_local(input, &cancel).map(SourceProbe::Single);
    }
    validate_url(&input)?;
    let output = online_metadata(&input, true, &cancel);
    let (metadata, subtitle_failure) = match output {
        Ok(output) => (
            course2md::fetch::parse_online_probe(&output.stdout, &input, &output.stderr)?,
            None,
        ),
        Err(error) => {
            ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
            // A subtitle channel can fail while basic metadata remains available.
            // Preserve that video and the failure evidence instead of claiming no captions.
            let basic = online_metadata(&input, false, &cancel)?;
            (
                course2md::fetch::parse_online_probe(&basic.stdout, &input, &basic.stderr)?,
                Some(subtitle_failure_message(&error)),
            )
        }
    };
    let result = match metadata {
        course2md::fetch::OnlineProbe::Collection {
            title,
            mut candidates,
            unavailable_entries,
        } => {
            // 分 P 视频的 flat 探测只有裸链接：补真实分集标题与首帧预览图。
            // 辅助探测失败不影响候选（interaction.md：辅助探测失败不阻断任务）。
            if !cancel.load(Ordering::Relaxed) {
                course2md::fetch::enrich_bilibili_candidates(&mut candidates);
            }
            SourceProbe::Collection {
                title,
                candidates,
                unavailable_entries,
            }
        }
        course2md::fetch::OnlineProbe::Unresolved { message } => {
            SourceProbe::Unresolved { message }
        }
        course2md::fetch::OnlineProbe::Video { video } => {
            let mut source = Source {
                input: video.meta.webpage_url,
                title: video.meta.title,
                author: video.meta.uploader,
                duration: video.meta.duration,
                identity: video.identity,
                online: true,
                original_language: video.original_language,
                subtitles: video.subtitles,
                ..Source::default()
            };
            if let Some(message) = subtitle_failure {
                source.subtitles = course2md::subtitle::SubtitleEvidence::Failed { message };
            }
            if let course2md::subtitle::SubtitleEvidence::Found { tracks, .. } =
                &mut source.subtitles
            {
                course2md::subtitle::sort_tracks(
                    tracks,
                    &[],
                    "zh",
                    source.original_language.as_deref(),
                );
            }
            if let Some(thumbnail) = video.thumbnail {
                match cache_remote_cover(&thumbnail, &cancel) {
                    Ok(path) => source.cover = Some(path),
                    Err(error) => source.cover_error = Some(format!("{error:#}")),
                }
            }
            SourceProbe::Single(source)
        }
    };
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
    Ok(result)
}

fn online_metadata(
    input: &str,
    with_subtitles: bool,
    cancel: &AtomicBool,
) -> Result<CommandOutput> {
    let mut args = vec![
        "--ignore-config",
        "--simulate",
        "--dump-single-json",
        "--flat-playlist",
        "--socket-timeout",
        "12",
        "--retries",
        "1",
    ];
    if with_subtitles {
        // --simulate prevents writing either media or subtitle files while these
        // flags ask extractors to actually inspect both caption channels.
        args.extend(["--write-subs", "--write-auto-subs", "--sub-langs", "all"]);
    }
    args.extend(["--", input]);
    command_output("yt-dlp", &args, cancel)
}

pub(crate) fn cache_remote_cover(url: &str, cancel: &AtomicBool) -> Result<PathBuf> {
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
    let response = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout_read(Duration::from_secs(2))
        .timeout(Duration::from_secs(6))
        .build()
        .get(url)
        .call()?;
    use std::io::Read;
    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    let mut buffer = [0; 32 * 1024];
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        ensure!(bytes.len() <= 12 * 1024 * 1024, "封面超过读取上限");
    }
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
    let image = image::load_from_memory(&bytes).context("封面格式无法识别")?;
    let cache = course2md::config::cache_dir().join("covers");
    std::fs::create_dir_all(&cache)?;
    let file = tempfile::Builder::new()
        .suffix(".jpg")
        .tempfile_in(&cache)?;
    image
        .thumbnail(1280, 720)
        .to_rgb8()
        .save_with_format(file.path(), image::ImageFormat::Jpeg)?;
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
    Ok(file.keep()?.1)
}

fn inspect_local(input: String, cancel: &AtomicBool) -> Result<Source> {
    let path = course2md::config::expand_tilde(input.into());
    ensure!(path.is_file(), "视频文件不存在，请重新选择");
    let path = std::fs::canonicalize(path).context("无法读取所选视频的位置，请重新选择")?;
    let before_probe = std::fs::metadata(&path).context("无法读取所选视频")?;
    let input = path
        .to_str()
        .context("视频路径无法编码，请为文件改用可读取的名称")?
        .to_owned();
    let bytes = command(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
            &input,
        ],
        cancel,
    )?;
    let meta: serde_json::Value =
        serde_json::from_slice(&bytes).context("无法读取所选视频的信息")?;
    ensure!(
        meta["streams"]
            .as_array()
            .is_some_and(|streams| streams.iter().any(|stream| stream["codec_type"] == "video")),
        "所选文件不包含视频画面"
    );
    let mut source = Source {
        input,
        title: meta["format"]["tags"]["title"]
            .as_str()
            .filter(|title| !title.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            }),
        author: ["artist", "author"]
            .iter()
            .find_map(|key| {
                meta["format"]["tags"][key]
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_default()
            .to_owned(),
        duration: meta["format"]["duration"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|duration| duration.is_finite() && *duration >= 0.)
            .unwrap_or_default(),
        identity: course2md::fetch::local_content_identity(&path, cancel)?,
        online: false,
        ..Source::default()
    };
    let after_probe = std::fs::metadata(&path).context("视频在读取期间已移动或无法访问")?;
    ensure!(
        before_probe.len() == after_probe.len()
            && before_probe.modified().ok() == after_probe.modified().ok(),
        "视频在读取期间发生变化，请等文件保存完成后重新读取"
    );
    source.subtitles = local_subtitle_evidence(&path, &meta, &source.identity);
    let cache = course2md::config::cache_dir().join("covers");
    std::fs::create_dir_all(&cache)?;
    let file = tempfile::Builder::new()
        .suffix(".jpg")
        .tempfile_in(&cache)?;
    let offset = (source.duration * 0.1).min(10.).to_string();
    match command(
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
        cancel,
    ) {
        Ok(_) => source.cover = Some(file.keep()?.1),
        Err(error) => source.cover_error = Some(format!("{error:#}")),
    }
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
    Ok(source)
}

fn local_subtitle_evidence(
    path: &Path,
    metadata: &serde_json::Value,
    identity: &str,
) -> course2md::subtitle::SubtitleEvidence {
    use course2md::subtitle::{SubtitleEvidence, SubtitleKind, SubtitleOrigin, SubtitleTrack};
    let mut tracks = Vec::new();
    let mut image_tracks = 0;
    for stream in metadata["streams"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if stream["codec_type"] != "subtitle" {
            continue;
        }
        let Some(index) = stream["index"]
            .as_u64()
            .and_then(|index| u32::try_from(index).ok())
        else {
            continue;
        };
        let codec = stream["codec_name"].as_str().unwrap_or_default();
        if !matches!(
            codec,
            "subrip" | "srt" | "webvtt" | "mov_text" | "ass" | "ssa" | "text" | "ttml"
        ) {
            image_tracks += 1;
            continue;
        }
        tracks.push(SubtitleTrack {
            id: format!("{identity}:embedded:{index}"),
            language: stream["tags"]["language"]
                .as_str()
                .filter(|language| !matches!(*language, "und" | ""))
                .map(str::to_owned),
            name: stream["tags"]["title"]
                .as_str()
                .filter(|title| !title.is_empty())
                .map(str::to_owned),
            kind: SubtitleKind::Unknown,
            origin: SubtitleOrigin::Embedded {
                stream_index: index,
                codec: codec.to_owned(),
            },
            source_order: tracks.len(),
        });
    }
    tracks.extend(course2md::subtitle::sidecar_tracks(path));
    for (index, track) in tracks.iter_mut().enumerate() {
        track.source_order = index;
    }
    course2md::subtitle::sort_tracks(&mut tracks, &[], "zh", None);
    if !tracks.is_empty() {
        SubtitleEvidence::Found {
            tracks,
            warning: None,
        }
    } else if image_tracks > 0 {
        SubtitleEvidence::Unsupported {
            message: "只发现目前无法读取的内嵌字幕。可以选择 SRT/VTT 字幕，或识别视频声音。".into(),
        }
    } else {
        SubtitleEvidence::NoneFound
    }
}

/// Refresh only the selected video's subtitle evidence. An extractor returning a
/// different identity is an error; old cached selection remains the caller's data.
pub fn refresh_subtitles(
    source: &Source,
    cancel: Arc<AtomicBool>,
) -> Result<course2md::subtitle::SubtitleEvidence> {
    if !source.online {
        let current_identity =
            course2md::fetch::local_content_identity(Path::new(&source.input), &cancel)?;
        ensure!(
            current_identity == source.identity,
            "视频文件内容发生变化，请重新读取视频"
        );
        let bytes = command(
            "ffprobe",
            &["-v", "error", "-show_streams", "-of", "json", &source.input],
            &cancel,
        )?;
        let metadata = serde_json::from_slice(&bytes)?;
        return Ok(local_subtitle_evidence(
            Path::new(&source.input),
            &metadata,
            &source.identity,
        ));
    }
    let output = online_metadata(&source.input, true, &cancel)
        .map_err(|error| anyhow::anyhow!(subtitle_failure_message(&error)))?;
    match course2md::fetch::parse_online_probe(&output.stdout, &source.input, &output.stderr)? {
        course2md::fetch::OnlineProbe::Video { mut video } => {
            ensure!(
                video.identity == source.identity,
                "视频来源发生变化，请重新读取视频"
            );
            if let course2md::subtitle::SubtitleEvidence::Found { tracks, .. } =
                &mut video.subtitles
            {
                course2md::subtitle::sort_tracks(
                    tracks,
                    &[],
                    "zh",
                    source.original_language.as_deref(),
                );
            }
            ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
            Ok(video.subtitles)
        }
        _ => anyhow::bail!("还无法确定字幕对应哪个视频，请重新读取视频"),
    }
}

/// Read one explicit track. This function never mutates the source or switches
/// language/ASR. The UI installs the returned value only if draft + revision match.
pub fn read_subtitle(
    source: &Source,
    track: &course2md::subtitle::SubtitleTrack,
    cancel: Arc<AtomicBool>,
) -> std::result::Result<course2md::subtitle::CachedSubtitle, course2md::subtitle::SubtitleReadError>
{
    use course2md::subtitle::{CachedSubtitle, SubtitleOrigin, SubtitleReadError};
    let read = || -> Result<CachedSubtitle> {
        ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
        ensure!(
            !source.identity.is_empty(),
            "请先重新读取视频，确认字幕对应的来源"
        );
        let cache = course2md::config::cache_dir().join("subtitles");
        std::fs::create_dir_all(&cache)?;
        let temp = tempfile::Builder::new()
            .prefix("read-")
            .tempdir_in(&cache)?;
        let text = match &track.origin {
            SubtitleOrigin::File { path } => {
                ensure!(
                    course2md::subtitle::is_subtitle_file(path),
                    "请选择 SRT 或 VTT 字幕文件"
                );
                course2md::subtitle::read_subtitle_text(path)?
            }
            SubtitleOrigin::Embedded { stream_index, .. } => {
                ensure!(
                    !source.online
                        && track
                            .id
                            .starts_with(&format!("{}:embedded:", source.identity)),
                    "所选内嵌字幕与当前视频不一致，请重新选择字幕"
                );
                ensure!(
                    course2md::fetch::local_content_identity(Path::new(&source.input), &cancel)?
                        == source.identity,
                    "视频内容发生变化，请重新读取视频"
                );
                let file = temp.path().join("embedded.srt");
                command(
                    "ffmpeg",
                    &[
                        "-v",
                        "error",
                        "-y",
                        "-i",
                        &source.input,
                        "-map",
                        &format!("0:{stream_index}"),
                        "-c:s",
                        "srt",
                        file.to_str().context("字幕缓存路径无法编码")?,
                    ],
                    &cancel,
                )?;
                course2md::subtitle::read_subtitle_text(&file)?
            }
            SubtitleOrigin::Online {
                language_key,
                automatic,
                inline_text,
            } => {
                ensure!(
                    source.online
                        && track
                            .id
                            .starts_with(&format!("{}:subtitle:", source.identity)),
                    "所选字幕与当前视频不一致，请重新选择字幕"
                );
                if let Some(text) = inline_text {
                    text.clone()
                } else {
                    let template = temp.path().join("subtitle");
                    let output = command_output(
                        "yt-dlp",
                        &[
                            "--ignore-config",
                            "--skip-download",
                            "--no-simulate",
                            "--dump-single-json",
                            "--no-playlist",
                            "--no-write-auto-subs",
                            "--no-write-subs",
                            "--convert-subs",
                            "srt",
                            "--sub-format",
                            "srt/vtt/best",
                            "--sub-langs",
                            &course2md::fetch::exact_subtitle_language(language_key),
                            "--socket-timeout",
                            "12",
                            "--retries",
                            "1",
                            "-o",
                            template.to_str().context("字幕缓存路径无法编码")?,
                            if *automatic {
                                "--write-auto-subs"
                            } else {
                                "--write-subs"
                            },
                            "--",
                            &source.input,
                        ],
                        &cancel,
                    )?;
                    match course2md::fetch::parse_online_probe(
                        &output.stdout,
                        &source.input,
                        &output.stderr,
                    )? {
                        course2md::fetch::OnlineProbe::Video { video } => ensure!(
                            video.identity == source.identity,
                            "视频来源发生变化，请重新读取并选择字幕"
                        ),
                        _ => anyhow::bail!("无法确认字幕对应的视频，请重新读取"),
                    }
                    let file = course2md::subtitle::pick_subtitle_file(temp.path())
                        .context("所选字幕没有下载成功，请重新读取字幕或选择其他文字来源")?;
                    course2md::subtitle::read_subtitle_text(&file)?
                }
            }
        };
        ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
        let events = course2md::subtitle::parse_subtitle(&text);
        if events.is_empty() {
            return Err(SubtitleReadError::NoReadableText {
                message: "这份字幕未包含可读取的文字，请选择其他字幕".into(),
            }
            .into());
        }
        let mut file = tempfile::Builder::new()
            .suffix(".srt")
            .tempfile_in(&cache)?;
        // The unique file is not published to a draft until keep() succeeds.
        // Write through its existing handle: Windows cannot replace the open file.
        use std::io::Write;
        file.write_all(course2md::subtitle::to_srt(&events).as_bytes())?;
        file.as_file().sync_all()?;
        ensure!(!cancel.load(Ordering::Relaxed), "已取消读取");
        Ok(CachedSubtitle {
            source_identity: source.identity.clone(),
            track_id: track.id.clone(),
            label: track.label(),
            path: file.keep()?.1,
            events,
        })
    };
    match read() {
        Ok(subtitle) => Ok(subtitle),
        Err(_) if cancel.load(Ordering::Relaxed) => Err(SubtitleReadError::Cancelled),
        Err(error) => match error.downcast::<SubtitleReadError>() {
            Ok(error) => Err(error),
            Err(error) => Err(SubtitleReadError::Failed {
                message: subtitle_failure_message(&error),
            }),
        },
    }
}

/// An attached file can have any name/location; it never replaces the video.
#[cfg(test)]
pub fn attach_subtitle(
    source: &Source,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
) -> std::result::Result<
    (
        course2md::subtitle::SubtitleTrack,
        course2md::subtitle::CachedSubtitle,
    ),
    course2md::subtitle::SubtitleReadError,
> {
    let track = course2md::subtitle::file_track(path, None);
    let subtitle = read_subtitle(source, &track, cancel)?;
    Ok((track, subtitle))
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
    fn actual_http_failures_do_not_turn_into_login_repairs_or_repeat_stderr() {
        let raw =
            "ERROR: [BiliBili] BVexample: Unable to download webpage: HTTP Error 404: Not Found";
        let error = preview_command_error("yt-dlp", Some(1), &format!("{raw}\n{raw}\n"));
        let message = error.to_string();
        assert_eq!(
            message.lines().next(),
            Some("无法找到这个视频，请检查链接或确认视频仍可访问。")
        );
        assert_eq!(message.matches(raw).count(), 1);
        assert!(!is_login_failure(&message));
        let subtitle_message = subtitle_failure_message(&error);
        assert!(subtitle_message.starts_with("字幕链接暂时无法访问"));
        assert!(!subtitle_message.contains("无法找到这个视频"));
        assert!(!is_login_failure(&format!(
            "{message}\nPlease log in to Bilibili"
        )));
        let forbidden =
            preview_command_error("yt-dlp", Some(1), "ERROR: HTTP Error 403: Forbidden")
                .to_string();
        assert!(forbidden.starts_with("视频平台拒绝了这次访问"));
        assert!(!is_login_failure(&forbidden));
        let timeout =
            preview_command_error("yt-dlp", Some(1), "ERROR: Connection timed out").to_string();
        assert!(timeout.starts_with("读取视频信息超时"));
        assert!(!is_login_failure(&timeout));
        let login =
            preview_command_error("yt-dlp", Some(1), "ERROR: Sign in to read these subtitles")
                .to_string();
        assert!(is_login_failure(&login));
        let unknown =
            preview_command_error("yt-dlp", Some(1), "ERROR: Unsupported URL: example.test")
                .to_string();
        assert_eq!(unknown, "Unsupported URL: example.test");
    }

    #[test]
    fn persisted_sources_round_trip_and_old_drafts_get_unchecked_evidence() {
        let source: Source = serde_json::from_str(r#"{"input":"old","title":"Lecture","author":"","duration":0,"cover":null,"cover_error":null}"#).unwrap();
        assert!(matches!(
            source.subtitles,
            course2md::subtitle::SubtitleEvidence::Unchecked
        ));
        assert_eq!(source.detail(), "");
        assert_eq!(
            source,
            serde_json::from_str::<Source>(&serde_json::to_string(&source).unwrap()).unwrap()
        );
    }

    #[test]
    fn arbitrary_sidecar_is_cached_without_mutating_the_video_or_old_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("different-name.FR.VTT");
        std::fs::write(
            &path,
            "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nBonjour le monde\n",
        )
        .unwrap();
        let source = Source {
            input: "original.mp4".into(),
            identity: "local:fixture".into(),
            title: "Lecture".into(),
            ..Source::default()
        };
        let (_, cached) =
            attach_subtitle(&source, path.clone(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(cached.source_identity, source.identity);
        assert_eq!(cached.events[0].text, "Bonjour le monde");
        assert_ne!(cached.path, path);
        assert!(
            course2md::subtitle::read_subtitle_text(&cached.path)
                .unwrap()
                .contains("Bonjour le monde")
        );
        assert!(
            course2md::subtitle::read_subtitle_text(&path)
                .unwrap()
                .starts_with("WEBVTT")
        );
        assert_eq!(source.input, "original.mp4");
        std::fs::write(&path, "invalid subtitles").unwrap();
        assert!(matches!(
            attach_subtitle(&source, path, Arc::new(AtomicBool::new(false))),
            Err(course2md::subtitle::SubtitleReadError::NoReadableText { .. })
        ));
        assert_eq!(cached.events[0].text, "Bonjour le monde");
        std::fs::remove_file(cached.path).unwrap();
    }

    #[test]
    fn local_discovery_distinguishes_text_subtitles_from_unsupported_images() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("lecture.mkv");
        let metadata = serde_json::json!({"streams":[{"index":0,"codec_type":"video"},{"index":2,"codec_type":"subtitle","codec_name":"hdmv_pgs_subtitle"}]});
        assert!(matches!(
            local_subtitle_evidence(&video, &metadata, "fixture"),
            course2md::subtitle::SubtitleEvidence::Unsupported { .. }
        ));
        let metadata = serde_json::json!({"streams":[{"index":3,"codec_type":"subtitle","codec_name":"subrip","tags":{"language":"fra"}}]});
        let evidence = local_subtitle_evidence(&video, &metadata, "fixture");
        assert_eq!(evidence.tracks().len(), 1);
        assert_eq!(evidence.tracks()[0].label(), "法语");
        assert!(matches!(
            evidence.tracks()[0].origin,
            course2md::subtitle::SubtitleOrigin::Embedded {
                stream_index: 3,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_a_running_read_terminates_the_child_and_rejects_its_result() {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || {
            command("sh", &["-c", "sleep 5; echo stale-result"], &worker_cancel)
        });
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("已取消"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

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
        assert!(source.author.is_empty());
        assert!(source.duration > 0.);
        let cover = source.cover.unwrap();
        assert!(image::open(&cover).is_ok());
        std::fs::remove_file(cover).unwrap();
        let text = dir.path().join("not-video.txt");
        std::fs::write(&text, "not a video").unwrap();
        assert!(inspect(text.display().to_string(), false, cancel).is_err());
    }
    #[test]
    fn cancelled_or_expired_preview_does_not_spawn() {
        let cancel = AtomicBool::new(true);
        assert!(
            command("tool-that-does-not-exist", &[], &cancel)
                .unwrap_err()
                .to_string()
                .contains("已取消")
        );
        assert!(
            check_preview_deadline(
                &AtomicBool::new(false),
                Instant::now() - Duration::from_secs(46)
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires yt-dlp and Bilibili network access"]
    fn bilibili_example_preview() {
        let source = inspect(
            "https://www.bilibili.com/video/BV1pb8o6yE8f".into(),
            true,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(source.title.contains("欢迎来到未来"));
        assert!(source.duration > 0.);
        let cover = source
            .cover
            .unwrap_or_else(|| panic!("{:?}", source.cover_error));
        assert!(image::open(&cover).is_ok());
        std::fs::remove_file(cover).unwrap();
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
