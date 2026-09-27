//! yt-dlp 子进程封装：元数据抓取 + 视频下载。

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// yt-dlp 调用的 socket 超时（秒）：兜底挂死的连接，三处调用统一
const YTDLP_SOCKET_TIMEOUT: &str = "12";
/// 视频下载尝试次数（首次 + 2 次重试）：网络类错误由外层循环重试
const DOWNLOAD_ATTEMPTS: u32 = 3;
/// 视频下载重试间隔
const DOWNLOAD_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// yt-dlp 公共参数基座：--ignore-config（用户全局配置会让同一命令在不同机器上
/// 行为漂移）、socket 超时。差异参数由调用方追加，最后以 [`ytdlp_url`] 收尾。
fn ytdlp_base(cmd: &mut Command) -> &mut Command {
    cmd.args(["--ignore-config", "--socket-timeout", YTDLP_SOCKET_TIMEOUT])
}

/// 轮询任务控制文件，意图不再是 run 时返回 Err（已提升为 dispatch::watch_control）。
/// 配合 `tokio::select!` 打断长时间运行的子进程分支。
async fn watch_control() -> anyhow::Error {
    crate::dispatch::watch_control().await
}

/// 确定性错误（重试无意义）：4xx 拒绝、不支持的 URL、私有/不可用视频等。
/// 只匹配 yt-dlp 的错误文本；未列出的错误按网络类处理，仍由外层循环重试。
fn is_deterministic_download_error(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    [
        "http error 401",
        "http error 403",
        "http error 404",
        "http error 410",
        "unsupported url",
        "private video",
        "video unavailable",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// yt-dlp 命令收尾：`--` 分隔符保证 URL 不被当作选项解析。
fn ytdlp_url<'a>(cmd: &'a mut Command, url: &str) -> &'a mut Command {
    cmd.arg("--").arg(url)
}

/// 我们关心的元数据字段子集。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceCandidate {
    /// The extractor's concrete video URL, never the original playlist URL.
    pub input: String,
    pub title: String,
    pub identity: Option<String>,
    pub duration: Option<f64>,
    /// 预览图 URL（分集首帧或视频封面）；本地缓存由桌面端负责
    #[serde(default)]
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OnlineVideo {
    pub meta: VideoMeta,
    pub identity: String,
    pub thumbnail: Option<String>,
    pub original_language: Option<String>,
    pub subtitles: crate::subtitle::SubtitleEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OnlineProbe {
    Video {
        video: OnlineVideo,
    },
    Collection {
        title: String,
        candidates: Vec<SourceCandidate>,
        unavailable_entries: usize,
    },
    Unresolved {
        message: String,
    },
}

/// Preserve the extractor's language order; serde_json::Value maps sort keys
/// unless a crate-wide feature is enabled, which would silently change defaults.
#[derive(Debug, Default)]
struct OrderedTracks(Vec<(String, Vec<serde_json::Value>)>);

impl<'de> Deserialize<'de> for OrderedTracks {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = OrderedTracks;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("subtitle language map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Vec<serde_json::Value>>()? {
                    entries.push((key, value));
                }
                Ok(OrderedTracks(entries))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Debug, Default, Deserialize)]
struct ExtractorInfo {
    #[serde(rename = "_type", default)]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    uploader: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    webpage_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    extractor: Option<String>,
    #[serde(default)]
    extractor_key: Option<String>,
    #[serde(default)]
    ie_key: Option<String>,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    subtitles: Option<OrderedTracks>,
    #[serde(default)]
    automatic_captions: Option<OrderedTracks>,
    #[serde(default)]
    entries: Option<Vec<Option<ExtractorInfo>>>,
}

fn extractor_name(info: &ExtractorInfo) -> &str {
    info.extractor
        .as_deref()
        .or(info.extractor_key.as_deref())
        .or(info.ie_key.as_deref())
        .unwrap_or_default()
}

/// Including the complete extractor ID preserves Bilibili `_pN` and interactive
/// segment IDs. Display titles and URL tracking parameters are not identity.
pub fn online_identity(extractor: &str, id: &str) -> Option<String> {
    let extractor = extractor.trim().to_ascii_lowercase();
    let id = id.trim();
    if extractor.is_empty() || id.is_empty() {
        return None;
    }
    Some(format!("online:{extractor}:{}:{}", id.len(), id))
}

fn concrete_url(info: &ExtractorInfo) -> Option<String> {
    info.webpage_url
        .as_deref()
        .or(info.url.as_deref())
        .filter(|value| {
            url::Url::parse(value).ok().is_some_and(|url| {
                matches!(url.scheme(), "https" | "http") && url.host_str().is_some()
            })
        })
        .map(str::to_owned)
}

/// Parse one metadata response from a subtitle-enabled, flat-playlist extraction.
/// Successful extraction can still contain explicit subtitle permission warnings.
pub fn parse_online_probe(bytes: &[u8], input: &str, diagnostics: &str) -> Result<OnlineProbe> {
    let info: ExtractorInfo =
        serde_json::from_slice(bytes).context("无法读取视频信息，请重新读取 / Cannot read video info; probe again")?;
    if info.kind == "multi_video" {
        return Ok(OnlineProbe::Unresolved {
            message: "这个来源由多个媒体片段组成，暂时无法确认完整的视频范围。请选择本地完整视频。 / This source consists of multiple media segments, so the full video range cannot be confirmed yet. Choose a complete local video."
                .into(),
        });
    }
    if info.kind == "playlist" || info.entries.is_some() {
        let mut candidates = Vec::new();
        let mut unavailable_entries = 0;
        for entry in info.entries.unwrap_or_default() {
            let Some(entry) = entry else {
                unavailable_entries += 1;
                continue;
            };
            let Some(input) = concrete_url(&entry) else {
                unavailable_entries += 1;
                continue;
            };
            if matches!(entry.kind.as_str(), "playlist" | "multi_video") || entry.entries.is_some()
            {
                unavailable_entries += 1;
                continue;
            }
            if candidates
                .iter()
                .any(|candidate: &SourceCandidate| candidate.input == input)
            {
                continue;
            }
            let identity = entry
                .id
                .as_deref()
                .and_then(|id| online_identity(extractor_name(&entry), id));
            candidates.push(SourceCandidate {
                title: entry
                    .title
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| input.clone()),
                input,
                identity,
                duration: entry
                    .duration
                    .filter(|value| value.is_finite() && *value > 0.),
                thumbnail: entry.thumbnail.filter(|url| !url.trim().is_empty()),
            });
        }
        return Ok(OnlineProbe::Collection {
            title: info.title.unwrap_or_default(),
            candidates,
            unavailable_entries,
        });
    }
    if matches!(info.kind.as_str(), "url" | "url_transparent") {
        return Ok(OnlineProbe::Unresolved {
            message: "还无法确定要处理哪个视频。请复制具体视频的链接。 / Cannot determine which video to process yet. Paste the link to a specific video.".into(),
        });
    }
    let extractor = extractor_name(&info).to_owned();
    let id = info
        .id
        .as_deref()
        .context("来源没有提供可确认的视频身份，请复制具体视频的链接 / The source did not provide a verifiable video identity; paste the link to a specific video")?;
    let identity = online_identity(&extractor, id)
        .context("来源没有提供可确认的视频身份，请复制具体视频的链接 / The source did not provide a verifiable video identity; paste the link to a specific video")?;
    let input = concrete_url(&info).unwrap_or_else(|| input.to_owned());
    // The original concrete part link is authoritative if the extractor returns a
    // canonical base URL: do not erase an explicitly selected Bilibili part.
    let input = if extractor.to_ascii_lowercase().starts_with("bilibili") {
        if let Some((_, part)) = id
            .rsplit_once("_p")
            .filter(|(_, part)| part.parse::<u32>().is_ok())
        {
            let mut url = url::Url::parse(&input)?;
            let pairs: Vec<_> = url
                .query_pairs()
                .filter(|(key, _)| key != "p")
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect();
            url.set_query(None);
            url.query_pairs_mut()
                .extend_pairs(pairs)
                .append_pair("p", part);
            url.to_string()
        } else {
            input
        }
    } else {
        input
    };
    let subtitles = subtitle_evidence(&info, &identity, diagnostics);
    Ok(OnlineProbe::Video {
        video: OnlineVideo {
            identity,
            meta: VideoMeta {
                title: info
                    .title
                    .filter(|title| !title.trim().is_empty())
                    .context("来源没有返回视频标题，请重新读取 / The source returned no video title; probe again")?,
                uploader: info.uploader.unwrap_or_default(),
                duration: info
                    .duration
                    .filter(|value| value.is_finite() && *value >= 0.)
                    .unwrap_or_default(),
                webpage_url: input,
                extractor,
                id: id.to_owned(),
            },
            thumbnail: info.thumbnail,
            original_language: info.language,
            subtitles,
        },
    })
}

/// Bilibili 分 P 视频的 flat-playlist 探测只返回裸链接；真实分集标题、首帧预览
/// 与时长来自公开的 web API。一个 BV 一次请求即可覆盖全部分集。
#[derive(Debug, Deserialize)]
struct BilibiliView {
    data: Option<BilibiliViewData>,
}

#[derive(Debug, Deserialize)]
struct BilibiliViewData {
    title: Option<String>,
    pic: Option<String>,
    pages: Option<Vec<BilibiliPage>>,
}

#[derive(Debug, Deserialize)]
struct BilibiliPage {
    page: u32,
    part: Option<String>,
    duration: Option<f64>,
    first_frame: Option<String>,
}

/// 单个探测最多补充的不同 BV 数：异常巨大的合集列表不放大请求量
const BILIBILI_ENRICH_LIMIT: usize = 32;

/// `/video/BVxxxxxxxxxx` 路径段中的 BV 号（"BV" + 10 位）。
fn bilibili_bvid(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if !host.ends_with("bilibili.com") {
        return None;
    }
    url.path_segments()?.find_map(|segment| {
        (segment.len() == 12 && segment.starts_with("BV")).then(|| segment.to_string())
    })
}

/// 链接里的 `?p=N` 分集号；没有时按第 1 集处理。
fn bilibili_part_number(url: &str) -> Option<u32> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == "p")
        .and_then(|(_, value)| value.parse().ok())
}

fn bilibili_view(bvid: &str) -> Result<BilibiliViewData> {
    let response = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(3))
        .timeout_read(std::time::Duration::from_secs(3))
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .get(&format!(
            "https://api.bilibili.com/x/web-interface/view?bvid={bvid}"
        ))
        .set(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
        )
        .set("Referer", "https://www.bilibili.com")
        .call()
        .context("读取 Bilibili 分集信息失败")?;
    let view: BilibiliView = serde_json::from_str(&response.into_string()?)?;
    view.data.context("Bilibili 没有返回分集信息")
}

/// 把一个 BV 的分集信息填进候选：只补缺失字段，提取器已有信息保持权威。
fn apply_bilibili_view(candidates: &mut [SourceCandidate], bvid: &str, view: &BilibiliViewData) {
    let pages = view.pages.as_deref().unwrap_or_default();
    for candidate in candidates
        .iter_mut()
        .filter(|candidate| bilibili_bvid(&candidate.input).as_deref() == Some(bvid))
    {
        let number = bilibili_part_number(&candidate.input).unwrap_or(1);
        let Some(page) = pages.iter().find(|page| page.page == number) else {
            continue;
        };
        let untitled =
            candidate.title.trim().is_empty() || candidate.title.trim() == candidate.input.trim();
        if untitled {
            // 分集标题为空时回落到合集标题（同一 BV 只有一个标题）
            let title = page
                .part
                .as_deref()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .or_else(|| view.title.as_deref().map(str::trim).filter(|t| !t.is_empty()));
            if let Some(title) = title {
                candidate.title = title.to_string();
            }
        }
        if candidate.thumbnail.is_none() {
            candidate.thumbnail = page
                .first_frame
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_string)
                .or_else(|| view.pic.clone());
        }
        if candidate.duration.is_none() {
            candidate.duration = page
                .duration
                .filter(|value| value.is_finite() && *value > 0.);
        }
    }
}

/// 用 Bilibili web API 补全分 P 候选的分集标题、首帧预览图与时长。
/// 辅助探测：任何失败都保留原候选，不阻断任务（见 interaction.md）。
pub fn enrich_bilibili_candidates(candidates: &mut [SourceCandidate]) {
    let mut bvids = Vec::new();
    for candidate in candidates.iter() {
        if let Some(bvid) = bilibili_bvid(&candidate.input)
            && !bvids.contains(&bvid)
        {
            bvids.push(bvid);
        }
        if bvids.len() >= BILIBILI_ENRICH_LIMIT {
            break;
        }
    }
    for bvid in bvids {
        match bilibili_view(&bvid) {
            Ok(view) => apply_bilibili_view(candidates, &bvid, &view),
            Err(error) => tracing::debug!(bvid, "分集信息补充失败：{error:#}"),
        }
    }
}

fn subtitle_evidence(
    info: &ExtractorInfo,
    identity: &str,
    diagnostics: &str,
) -> crate::subtitle::SubtitleEvidence {
    use crate::subtitle::{SubtitleEvidence, SubtitleKind, SubtitleOrigin, SubtitleTrack};
    let warning = diagnostics
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            (lower.contains("subtitle") || lower.contains("caption"))
                && (lower.contains("error")
                    || lower.contains("unable")
                    || lower.contains("failed")
                    || lower.contains("login")
                    || lower.contains("logged in")
                    || lower.contains("sign in"))
        })
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    let warning = (!warning.is_empty()).then_some(warning);
    let mut tracks = Vec::new();
    let bilibili = extractor_name(info)
        .to_ascii_lowercase()
        .starts_with("bilibili");
    for (automatic, channel) in [(false, &info.subtitles), (true, &info.automatic_captions)] {
        for (language_key, formats) in channel
            .as_ref()
            .map(|channel| channel.0.as_slice())
            .unwrap_or_default()
        {
            // Danmaku/chat are timed comments, not a transcript of the video.
            if matches!(language_key.as_str(), "danmaku" | "live_chat") {
                continue;
            }
            let readable: Vec<_> = formats
                .iter()
                .filter(|format| {
                    matches!(
                        format["ext"].as_str(),
                        Some(
                            "srt"
                                | "vtt"
                                | "ttml"
                                | "srv1"
                                | "srv2"
                                | "srv3"
                                | "json3"
                                | "ass"
                                | "lrc"
                        )
                    )
                })
                .collect();
            if readable.is_empty() {
                continue;
            }
            let bilibili_auto = bilibili && language_key.starts_with("ai-");
            let language = if bilibili_auto {
                language_key.trim_start_matches("ai-").to_owned()
            } else {
                language_key.clone()
            };
            let kind = if automatic || bilibili_auto {
                SubtitleKind::Automatic
            } else if bilibili {
                SubtitleKind::Unknown
            } else {
                SubtitleKind::Manual
            };
            let name = readable
                .iter()
                .find_map(|format| format["name"].as_str())
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
            let inline_text = readable
                .iter()
                .find(|format| matches!(format["ext"].as_str(), Some("srt" | "vtt")))
                .and_then(|format| format["data"].as_str())
                .map(str::to_owned);
            tracks.push(SubtitleTrack {
                id: format!(
                    "{identity}:subtitle:{}:{language_key}",
                    if automatic { "auto" } else { "provided" }
                ),
                language: Some(language),
                name,
                kind,
                origin: SubtitleOrigin::Online {
                    language_key: language_key.clone(),
                    automatic,
                    inline_text,
                },
                source_order: tracks.len(),
            });
        }
    }
    if !tracks.is_empty() {
        SubtitleEvidence::Found { tracks, warning }
    } else if let Some(message) = warning {
        SubtitleEvidence::Failed { message }
    } else if info.subtitles.is_none() && info.automatic_captions.is_none() {
        SubtitleEvidence::Unsupported {
            message: "此来源暂不支持读取字幕，可以识别视频声音 / This source does not support reading subtitles yet; you can transcribe the video audio instead".into(),
        }
    } else {
        SubtitleEvidence::NoneFound
    }
}

/// Local identity is content-based. Call on a worker and before matching a
/// previous task/result; paths, names, timestamps and file size are not identity.
pub fn local_content_identity(
    path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::sync::atomic::Ordering;
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("无法读取视频文件 {0} / Cannot read video file {0}", path.display()))?;
    let before = file.metadata()?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "已取消读取视频 / Video reading cancelled");
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    ensure!(!cancel.load(Ordering::Relaxed), "已取消读取视频 / Video reading cancelled");
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len() && before.modified().ok() == after.modified().ok(),
        "视频文件在读取时发生变化，请重新读取 / The video file changed while reading; read it again"
    );
    Ok(format!("local:sha256:{:x}", hash.finalize()))
}

impl VideoMeta {
    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// 抓取元数据（不下载）。
pub async fn fetch_meta(url: &str) -> Result<VideoMeta> {
    Ok(probe_video(url).await?.meta)
}

/// Probe once and return the single video; collection/unresolved links are errors.
pub async fn probe_video(url: &str) -> Result<OnlineVideo> {
    match probe_online(url).await? {
        OnlineProbe::Video { video } => Ok(video),
        OnlineProbe::Collection { .. } => {
            anyhow::bail!("这个链接包含多个视频，请选择具体单集后生成笔记 / This link contains multiple videos; choose a specific episode before generating notes")
        }
        OnlineProbe::Unresolved { message } => anyhow::bail!("{message}"),
    }
}

/// Metadata and all available subtitle channels only; no media/AI requests.
pub async fn probe_online(url: &str) -> Result<OnlineProbe> {
    let mut cmd = Command::new("yt-dlp");
    let _cookies = crate::auth::configure_ytdlp(cmd.as_std_mut(), url)?;
    ytdlp_base(&mut cmd).args([
        "--simulate",
        "--dump-single-json",
        "--flat-playlist",
        "--write-subs",
        "--write-auto-subs",
        "--sub-langs",
        "all",
        "--retries",
        "1",
    ]);
    let out = run_output(ytdlp_url(&mut cmd, url))
        .await
        .map_err(|e| crate::auth::with_bilibili_login_tip(url, e))?;
    parse_online_probe(&out.stdout, url, &String::from_utf8_lossy(&out.stderr))
}

/// 抓取的平台字幕（yt-dlp 产物）。
pub struct SubtitleFetch {
    pub path: PathBuf,
    /// true = 平台自动生成字幕（auto-caption）
    pub auto: bool,
}

/// CLI compatibility path. `video` comes from the run's single probe so discovery
/// stays consistent; select from every language, then fetch that exact track
/// without silent fallback.
pub async fn fetch_subtitle(video: &OnlineVideo, out_dir: &Path) -> Result<Option<SubtitleFetch>> {
    use crate::subtitle::SubtitleEvidence;
    match &video.subtitles {
        SubtitleEvidence::Found { tracks, .. } => {
            let mut tracks = tracks.clone();
            crate::subtitle::sort_tracks(
                &mut tracks,
                &[],
                "zh",
                video.original_language.as_deref(),
            );
            let track = tracks.first().context("字幕列表为空，请重新读取字幕 / Subtitle list is empty; probe subtitles again")?;
            fetch_selected_subtitle(&video.meta.webpage_url, &video.identity, track, out_dir)
                .await
                .map(Some)
        }
        SubtitleEvidence::Failed { message } => {
            anyhow::bail!("字幕未读取成功，尚不能确认是否可用 / Subtitles were not read successfully, so availability cannot be confirmed yet: {message}")
        }
        SubtitleEvidence::Unchecked => anyhow::bail!("尚未检查字幕，请重新读取视频信息 / Subtitles not checked yet; probe the video info again"),
        SubtitleEvidence::NoneFound | SubtitleEvidence::Unsupported { .. } => Ok(None),
    }
}

/// Escape the complete language key as an exact yt-dlp subtitle regular expression.
pub fn exact_subtitle_language(key: &str) -> String {
    let mut escaped = String::from("^");
    for character in key.chars() {
        if matches!(
            character,
            '.' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('$');
    escaped
}

/// Fetch only the confirmed online track, rejecting a changed video identity.
pub async fn fetch_selected_subtitle(
    url: &str,
    source_identity: &str,
    track: &crate::subtitle::SubtitleTrack,
    out_dir: &Path,
) -> Result<SubtitleFetch> {
    use crate::subtitle::SubtitleOrigin;
    let SubtitleOrigin::Online {
        language_key,
        automatic,
        inline_text,
    } = &track.origin
    else {
        anyhow::bail!("所选字幕不是在线字幕 / The selected subtitle is not an online subtitle");
    };
    ensure!(
        track
            .id
            .starts_with(&format!("{source_identity}:subtitle:")),
        "所选字幕与当前视频不一致，请重新选择字幕 / The selected subtitle no longer matches the current video; select the subtitle again"
    );
    let dir = out_dir.join(".subs");
    tokio::fs::create_dir_all(&dir).await?;
    let temp = tempfile::Builder::new()
        .prefix("selected-")
        .tempdir_in(&dir)?;
    let text = if let Some(text) = inline_text {
        text.clone()
    } else {
        let template = temp.path().join("subtitle");
        let mut cmd = Command::new("yt-dlp");
        let _cookies = crate::auth::configure_ytdlp(cmd.as_std_mut(), url)?;
        ytdlp_base(&mut cmd).args([
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
            &exact_subtitle_language(language_key),
            "--retries",
            "1",
            "-o",
        ]);
        cmd.arg(&template).arg(if *automatic {
            "--write-auto-subs"
        } else {
            "--write-subs"
        });
        let out = run_output(ytdlp_url(&mut cmd, url))
            .await
            .map_err(|error| crate::auth::with_bilibili_login_tip(url, error))?;
        match parse_online_probe(&out.stdout, url, &String::from_utf8_lossy(&out.stderr))? {
            OnlineProbe::Video { video } => ensure!(
                video.identity == source_identity,
                "视频来源发生变化，请重新读取并选择字幕 / The video source changed; probe again and reselect the subtitle"
            ),
            _ => anyhow::bail!("无法确认字幕对应的视频，请重新读取 / Cannot confirm the video for the subtitle; probe again"),
        }
        let path = crate::subtitle::pick_subtitle_file(temp.path())
            .context("所选字幕没有下载成功，请重新读取字幕或明确选择其他文字来源 / The selected subtitle was not downloaded; probe subtitles again or explicitly choose another text source")?;
        crate::subtitle::read_subtitle_text(&path)?
    };
    let events = crate::subtitle::parse_subtitle(&text);
    ensure!(
        !events.is_empty(),
        "这份字幕未包含可读取的文字，请选择其他字幕 / This subtitle contains no readable text; choose another subtitle"
    );
    let path = temp.path().join("selected.srt");
    crate::checkpoint::atomic_write(&path, crate::subtitle::to_srt(&events).as_bytes())?;
    let _ = temp.keep();
    Ok(SubtitleFetch {
        path,
        auto: track.kind == crate::subtitle::SubtitleKind::Automatic,
    })
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
    // 瞬时网络错误由外层循环重试（共 DOWNLOAD_ATTEMPTS 次）；412 走专用退避；
    // 确定性错误（4xx/私有视频等）立即返回；每轮都可被任务控制文件中止。
    let mut last_err = None;
    let mut next_delay = None;
    let mut bilibili_412_retries = 0;
    for attempt in 0..DOWNLOAD_ATTEMPTS {
        crate::dispatch::check_control()?;
        if attempt > 0 {
            tracing::warn!(attempt, "视频下载失败，正在重试 / Retrying video download");
            let delay = next_delay.unwrap_or(DOWNLOAD_RETRY_DELAY);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                e = watch_control() => return Err(e),
            }
        }
        let mut cmd = Command::new("yt-dlp");
        let _cookies = crate::auth::configure_ytdlp(cmd.as_std_mut(), url)?;
        let fmt = format!("bv*[height<={max_height}]+ba/b[height<={max_height}]/b");
        ytdlp_base(&mut cmd).args([
            "-f",
            &fmt,
            "-S",
            "ext:mp4:m4a",
            "--merge-output-format",
            "mp4",
            "--no-playlist",
            "--no-part",
            // 结构化进度（2021.11 起的 yt-dlp 接口）：前缀行由 run_download 解析转发
            "--newline",
            "--progress-template",
            "download:[C2MD] %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s",
            "-o",
        ]);
        cmd.arg(&tmp);
        if verbose {
            cmd.arg("-v");
        }
        let outcome = tokio::select! {
            status = run_download(ytdlp_url(&mut cmd, url)) => {
                status.map_err(|e| crate::auth::with_bilibili_login_tip(url, e))
            }
            // 取消/暂停：drop run_download 分支即 kill_on_drop 终止当前 yt-dlp
            e = watch_control() => return Err(e),
        };
        match outcome {
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
            Err(e) => {
                let text = format!("{e:#}");
                if is_bilibili_412(&text) {
                    match bilibili_retry_delay(&text, bilibili_412_retries) {
                        Some(delay) => {
                            bilibili_412_retries += 1;
                            next_delay = Some(delay);
                        }
                        // 412 重试耗尽，不再进下一轮
                        None => return Err(e),
                    }
                } else if is_deterministic_download_error(&text) {
                    return Err(e);
                } else {
                    next_delay = None;
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("视频下载失败 / Video download failed")))
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

pub const BILIBILI_412_HINT: &str = "Bilibili 暂时限制了请求（HTTP 412）。请稍后重试；若持续失败，请运行 course2md --login bilibili 登录或重新登录，更新 yt-dlp，并确认该链接能在浏览器播放，也可以导入已下载的本地视频。 / Bilibili is rate-limiting requests (HTTP 412). Retry later; if it keeps failing, run course2md --login bilibili to log in again, update yt-dlp, and confirm the link plays in a browser, or import a downloaded local video.";

#[cfg(all(test, unix))]
async fn run(cmd: &mut Command) -> Result<String> {
    let out = run_output(cmd).await?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn run_output(cmd: &mut Command) -> Result<std::process::Output> {
    let mut retries = 0;
    loop {
        let out = cmd
            .kill_on_drop(true)
            .output()
            .await
            .context("启动 yt-dlp 失败 / Failed to start yt-dlp")?;
        if out.status.success() {
            return Ok(out);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(delay) = bilibili_retry_delay(&stderr, retries) {
            retries += 1;
            tracing::warn!(retries, "Bilibili HTTP 412，等待后重试 / Bilibili HTTP 412; retrying after a wait");
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

/// 解析一行 yt-dlp 进度模板输出：(downloaded, total)。
/// 模板字段：<downloaded> <total> <estimate>，未知值为 NA；
/// 精确总字节未知时回落到估算值，都没有则 total=0（GUI 显示不确定进度）。
fn ytdlp_progress(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("[C2MD] ")?;
    let mut fields = rest.split_whitespace();
    let number = |value: Option<&str>| {
        value
            .filter(|value| *value != "NA")
            .and_then(|value| value.parse::<u64>().ok())
    };
    let downloaded = number(fields.next())?;
    let total = number(fields.next())
        .or_else(|| number(fields.next()))
        .unwrap_or(0);
    Some((downloaded, total))
}

fn parse_ytdlp_progress(line: &str) {
    if let Some((downloaded, total)) = ytdlp_progress(line) {
        crate::progress::download_progress(downloaded, total);
    }
}

/// 运行 yt-dlp 下载：流式读取 stdout/stderr 并实时转发字节进度。
/// 错误语义与 media::run_cmd 一致（stderr 尾部进错误，412 附登录提示）；
/// 三个读取分支同属一个 future，drop 即整体取消（kill_on_drop 杀子进程）。
async fn run_download(cmd: &mut Command) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut child = cmd
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("启动 yt-dlp 失败 / Failed to start yt-dlp")?;
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout 管道已配置")).lines();
    let mut stderr_pipe = BufReader::new(child.stderr.take().expect("stderr 管道已配置")).lines();
    let wait = child.wait();
    let out = async {
        while let Ok(Some(line)) = stdout.next_line().await {
            parse_ytdlp_progress(&line);
        }
    };
    let err = async {
        let mut text = String::new();
        while let Ok(Some(line)) = stderr_pipe.next_line().await {
            parse_ytdlp_progress(&line);
            if text.len() < 64 * 1024 {
                text.push_str(&line);
                text.push('\n');
            }
        }
        text
    };
    let (status, (), stderr) = tokio::join!(wait, out, err);
    let status = status.context("等待 yt-dlp 结束失败")?;
    if status.success() {
        if !stderr.trim().is_empty() {
            tracing::debug!("{}", stderr.trim());
        }
        return Ok(());
    }
    let error = crate::error::cmd_error("yt-dlp", status.code(), &stderr);
    Err(if is_bilibili_412(&stderr) {
        error.context(BILIBILI_412_HINT)
    } else {
        error
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_download_errors_skip_the_retry_loop() {
        for text in [
            "ERROR: HTTP Error 404: Not Found",
            "HTTP Error 403: Forbidden",
            "ERROR: Unsupported URL: https://example.com/x",
            "ERROR: Private video. Use --cookies for authentication",
        ] {
            assert!(is_deterministic_download_error(text), "{text}");
        }
        for text in [
            "ERROR: [bilibili] HTTP Error 412: Precondition Failed",
            "network unreachable",
            "timed out",
        ] {
            assert!(!is_deterministic_download_error(text), "{text}");
        }
    }

    #[test]
    fn collection_is_never_implicitly_the_first_part() {
        let result = parse_online_probe(
            include_bytes!("../tests/fixtures/source/bilibili-parts.json"),
            "https://www.bilibili.com/video/BV-fixture",
            "",
        )
        .unwrap();
        let OnlineProbe::Collection {
            candidates,
            unavailable_entries,
            ..
        } = result
        else {
            panic!("expected collection")
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[1].input,
            "https://www.bilibili.com/video/BV-fixture?p=2"
        );
        assert_eq!(unavailable_entries, 1);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.identity.is_none())
        );
    }

    #[test]
    fn bilibili_ids_come_from_video_paths_and_part_query() {
        assert_eq!(
            bilibili_bvid("https://www.bilibili.com/video/BV1CAxaeHEeH?p=3").as_deref(),
            Some("BV1CAxaeHEeH")
        );
        assert_eq!(
            bilibili_bvid("https://www.bilibili.com/video/BV1xx411c7mD/").as_deref(),
            Some("BV1xx411c7mD")
        );
        assert!(bilibili_bvid("https://www.bilibili.com/bangumi/play/ep123").is_none());
        assert!(bilibili_bvid("https://example.com/video/BV1CAxaeHEeH").is_none());
        assert!(bilibili_bvid("not a url").is_none());
        assert_eq!(
            bilibili_part_number("https://www.bilibili.com/video/BV1CAxaeHEeH?p=12"),
            Some(12)
        );
        assert_eq!(
            bilibili_part_number("https://www.bilibili.com/video/BV1CAxaeHEeH"),
            None
        );
    }

    #[test]
    fn bilibili_view_fills_only_missing_part_metadata() {
        let view: BilibiliViewData = serde_json::from_value(serde_json::json!({
            "title": "《高等数学》全程教学视频",
            "pic": "https://i2.hdslb.com/bfs/archive/cover.jpg",
            "pages": [
                {"page": 1, "part": "1 映射", "duration": 2076, "first_frame": "https://i1.hdslb.com/bfs/storyff/p1.jpg"},
                {"page": 2, "part": "", "duration": 307, "first_frame": null}
            ]
        }))
        .unwrap();
        let mut candidates = vec![
            SourceCandidate {
                input: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=1".into(),
                title: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=1".into(),
                identity: None,
                duration: None,
                thumbnail: None,
            },
            SourceCandidate {
                input: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=2".into(),
                title: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=2".into(),
                identity: None,
                duration: Some(42.),
                thumbnail: Some("https://example.invalid/keep.jpg".into()),
            },
            SourceCandidate {
                input: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=9".into(),
                title: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=9".into(),
                identity: None,
                duration: None,
                thumbnail: None,
            },
            SourceCandidate {
                input: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=2".into(),
                title: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=2".into(),
                identity: None,
                duration: None,
                thumbnail: None,
            },
        ];
        apply_bilibili_view(&mut candidates, "BV1CAxaeHEeH", &view);
        // 裸链接候选：补真实分集标题与首帧
        assert_eq!(candidates[0].title, "1 映射");
        assert_eq!(
            candidates[0].thumbnail.as_deref(),
            Some("https://i1.hdslb.com/bfs/storyff/p1.jpg")
        );
        assert_eq!(candidates[0].duration, Some(2076.));
        // 已有信息保持权威；空 part 回落到合集标题；已有时长/预览图不被改写
        assert_eq!(candidates[1].title, "《高等数学》全程教学视频");
        assert_eq!(candidates[1].duration, Some(42.));
        assert_eq!(
            candidates[1].thumbnail.as_deref(),
            Some("https://example.invalid/keep.jpg")
        );
        // 没有对应分页的候选保持原样
        assert_eq!(
            candidates[2].title,
            "https://www.bilibili.com/video/BV1CAxaeHEeH?p=9"
        );
        assert!(candidates[2].thumbnail.is_none());
        // 首帧缺失时回落到视频封面
        assert_eq!(candidates[3].title, "《高等数学》全程教学视频");
        assert_eq!(
            candidates[3].thumbnail.as_deref(),
            Some("https://i2.hdslb.com/bfs/archive/cover.jpg")
        );
        assert_eq!(candidates[3].duration, Some(307.));
    }

    #[test]
    fn exact_part_identity_and_all_language_evidence_survive_serialization() {
        let OnlineProbe::Video { video } = parse_online_probe(
            include_bytes!("../tests/fixtures/source/multilingual-video.json"),
            "https://www.bilibili.com/video/fixture?p=2",
            "",
        )
        .unwrap() else {
            panic!("expected video")
        };
        assert!(video.identity.ends_with("fixture_p2"));
        assert!(video.meta.webpage_url.ends_with("?p=2"));
        let tracks = video.subtitles.tracks();
        assert_eq!(
            tracks
                .iter()
                .map(|track| track.language.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["fr", "ja", "zh"]
        );
        assert_eq!(tracks[0].kind, crate::subtitle::SubtitleKind::Unknown);
        assert_eq!(tracks[2].kind, crate::subtitle::SubtitleKind::Automatic);
        assert_eq!(
            video,
            serde_json::from_str::<OnlineVideo>(&serde_json::to_string(&video).unwrap()).unwrap()
        );
    }

    #[test]
    fn permission_failure_never_means_no_captions() {
        let bytes = include_bytes!("../tests/fixtures/source/no-captions.json");
        let OnlineProbe::Video { video } =
            parse_online_probe(bytes, "https://example.invalid", "").unwrap()
        else {
            panic!()
        };
        assert!(matches!(
            video.subtitles,
            crate::subtitle::SubtitleEvidence::NoneFound
        ));
        let OnlineProbe::Video { video } = parse_online_probe(
            bytes,
            "https://example.invalid",
            "WARNING: Subtitles are only available when logged in",
        )
        .unwrap() else {
            panic!()
        };
        assert!(matches!(
            video.subtitles,
            crate::subtitle::SubtitleEvidence::Failed { .. }
        ));
        let OnlineProbe::Video { video } = parse_online_probe(
            br#"{"id":"one","title":"One","extractor":"test"}"#,
            "https://example.invalid/one",
            "",
        )
        .unwrap() else {
            panic!()
        };
        assert!(matches!(
            video.subtitles,
            crate::subtitle::SubtitleEvidence::Unsupported { .. }
        ));
    }

    #[test]
    fn fragmented_media_does_not_become_a_selectable_first_fragment() {
        let result = parse_online_probe(br#"{"_type":"multi_video","id":"a","title":"Video","entries":[{"id":"a_0","url":"https://example.invalid/fragment.flv"}]}"#, "https://example.invalid/video", "").unwrap();
        assert!(matches!(result, OnlineProbe::Unresolved { .. }));
    }

    #[test]
    fn local_identity_depends_on_content_not_filename_and_can_cancel() {
        use std::sync::atomic::AtomicBool;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("lecture.mp4");
        let b = dir.path().join("renamed.mp4");
        std::fs::write(&a, b"video A").unwrap();
        std::fs::write(&b, b"video A").unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            local_content_identity(&a, &cancel).unwrap(),
            local_content_identity(&b, &cancel).unwrap()
        );
        std::fs::write(&b, b"video B").unwrap();
        assert_ne!(
            local_content_identity(&a, &cancel).unwrap(),
            local_content_identity(&b, &cancel).unwrap()
        );
        assert!(
            local_content_identity(&a, &AtomicBool::new(true))
                .unwrap_err()
                .to_string()
                .contains("已取消")
        );
    }

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
        let error = run_download(&mut cmd).await.unwrap_err();
        assert!(error.to_string().contains("--login bilibili"));
        assert!(format!("{error:#}").contains("Precondition Failed"));
    }

    #[test]
    fn ytdlp_progress_lines_parse_bytes_with_estimate_fallback() {
        assert_eq!(
            ytdlp_progress("[C2MD] 120326 10485760 NA"),
            Some((120326, 10485760))
        );
        assert_eq!(
            ytdlp_progress("[C2MD] 120326 NA 20971520"),
            Some((120326, 20971520))
        );
        assert_eq!(ytdlp_progress("[C2MD] 120326 NA NA"), Some((120326, 0)));
        assert_eq!(ytdlp_progress("[C2MD] NA NA NA"), None);
        assert_eq!(ytdlp_progress("[download] 45.3% of 10MiB"), None);
        assert_eq!(ytdlp_progress(""), None);
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
