//! yt-dlp 子进程封装：元数据抓取 + 视频下载。

pub use crate::meta::VideoMeta;
use crate::tr;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// yt-dlp 调用的 socket 超时（秒）：兜底挂死的连接，三处调用与桌面预览统一
pub const YTDLP_SOCKET_TIMEOUT: &str = "12";
/// yt-dlp 尝试次数（首次 + 2 次重试）
const YTDLP_ATTEMPTS: u32 = 3;
/// 网络类错误的重试间隔
const TRANSIENT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// A yt-dlp command with what every run shares: the saved login (a private cookie
/// snapshot the caller keeps alive until the process exits), no user configuration
/// (`--ignore-config`: it would make one command behave differently per machine), a
/// socket timeout, and the ffmpeg shipped with the app for merging video and audio.
fn ytdlp_command(url: &str) -> Result<(Command, Option<tempfile::NamedTempFile>)> {
    let mut cmd = Command::new(crate::ytdlp::program());
    let cookies = crate::auth::configure_ytdlp(cmd.as_std_mut(), url)?;
    cmd.args(["--ignore-config", "--socket-timeout", YTDLP_SOCKET_TIMEOUT]);
    if let Some(tool) =
        crate::tools::locate("ffmpeg").filter(|tool| tool.origin != crate::tools::Origin::System)
    {
        cmd.arg("--ffmpeg-location").arg(tool.path);
    }
    Ok((cmd, cookies))
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceCandidate {
    /// The extractor's concrete video URL, never the original playlist URL; a
    /// YouTube video keeps the playlist it plays in (`list`), not its identity.
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

/// Keeps the extractor's language order, which `serde_json::Value` maps would sort.
type OrderedTracks = indexmap::IndexMap<String, Vec<serde_json::Value>>;

#[derive(Debug, Default, Deserialize)]
struct ExtractorInfo {
    #[serde(rename = "_type", default)]
    kind: String,
    id: Option<String>,
    title: Option<String>,
    uploader: Option<String>,
    duration: Option<f64>,
    webpage_url: Option<String>,
    url: Option<String>,
    extractor: Option<String>,
    extractor_key: Option<String>,
    ie_key: Option<String>,
    thumbnail: Option<String>,
    language: Option<String>,
    subtitles: Option<OrderedTracks>,
    automatic_captions: Option<OrderedTracks>,
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
pub(crate) fn online_identity(extractor: &str, id: &str) -> Option<String> {
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
    let info: ExtractorInfo = serde_json::from_slice(bytes).context(tr!(
        "无法读取视频信息，请重新读取 / Cannot read video info; probe again"
    ))?;
    if info.kind == "multi_video" {
        return Ok(OnlineProbe::Unresolved {
            message: tr!("这个视频被分成了多段，暂不支持。请下载完整视频后导入。 / This video is split into several parts and is not supported yet. Download the full video and import it.")
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
            let Some(entry_input) = concrete_url(&entry) else {
                unavailable_entries += 1;
                continue;
            };
            let input = crate::playback::entry_url(&entry_input, input);
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
            message: tr!("还无法确定要处理哪个视频。请复制具体视频的链接。 / Cannot determine which video to process yet. Paste the link to a specific video.").into(),
        });
    }
    let extractor = extractor_name(&info).to_owned();
    let id = info
        .id
        .as_deref()
        .context(tr!("无法识别这个链接对应的视频，请粘贴具体视频的链接 / Cannot identify the video for this link; paste the link to a specific video"))?;
    let identity = online_identity(&extractor, id)
        .context(tr!("无法识别这个链接对应的视频，请粘贴具体视频的链接 / Cannot identify the video for this link; paste the link to a specific video"))?;
    let input = match concrete_url(&info) {
        Some(concrete) => crate::playback::video_url(&concrete, input, id),
        None => input.to_owned(),
    };
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
                    .context(tr!("来源没有返回视频标题，请重新读取 / The source returned no video title; probe again"))?,
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
/// 分集信息补充的并发请求数
const BILIBILI_ENRICH_CONCURRENCY: usize = 4;

/// 链接里的 `?p=N` 分集号；没有时按第 1 集处理。
fn bilibili_part_number(url: &str) -> Option<u32> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == "p")
        .and_then(|(_, value)| value.parse().ok())
}

/// 候选条目的稳定身份：优先使用探测给出的身份；Bilibili 分 P 的 flat 探测
/// 不带条目 id，从具体分集链接推导（与单集探测的 `BV…_pN` 身份一致），保证
/// 批量任务与单独转换同一视频时去重、已有笔记判断命中同一身份。
pub fn candidate_identity(candidate: &SourceCandidate) -> Option<String> {
    if let Some(identity) = candidate
        .identity
        .as_deref()
        .map(str::trim)
        .filter(|identity| !identity.is_empty())
    {
        return Some(identity.to_owned());
    }
    let bvid = crate::config::bilibili_bvid(&candidate.input)?;
    let id = match bilibili_part_number(&candidate.input) {
        Some(part) => format!("{bvid}_p{part}"),
        None => bvid,
    };
    online_identity("bilibili", &id)
}

fn bilibili_view(agent: &ureq::Agent, bvid: &str) -> Result<BilibiliViewData> {
    let mut response = agent
        .get(&format!(
            "https://api.bilibili.com/x/web-interface/view?bvid={bvid}"
        ))
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
        )
        .header("Referer", "https://www.bilibili.com")
        .call()
        .context(tr!("读取 Bilibili 分集信息失败"))?;
    let view: BilibiliView = serde_json::from_str(&response.body_mut().read_to_string()?)?;
    view.data.context(tr!("Bilibili 没有返回分集信息"))
}

/// 把一个 BV 的分集信息填进候选：只补缺失字段，提取器已有信息保持权威。
fn apply_bilibili_view(candidates: &mut [SourceCandidate], bvid: &str, view: &BilibiliViewData) {
    let filled = |value: &Option<String>| {
        Some(value.as_deref()?.trim().to_owned()).filter(|text| !text.is_empty())
    };
    let pages = view.pages.as_deref().unwrap_or_default();
    for candidate in candidates
        .iter_mut()
        .filter(|candidate| crate::config::bilibili_bvid(&candidate.input).as_deref() == Some(bvid))
    {
        let number = bilibili_part_number(&candidate.input).unwrap_or(1);
        let Some(page) = pages.iter().find(|page| page.page == number) else {
            continue;
        };
        let untitled =
            candidate.title.trim().is_empty() || candidate.title.trim() == candidate.input.trim();
        if untitled {
            // 分集标题为空时回落到合集标题（同一 BV 只有一个标题）
            if let Some(title) = filled(&page.part).or_else(|| filled(&view.title)) {
                candidate.title = title;
            }
        }
        if candidate.thumbnail.is_none() {
            candidate.thumbnail = filled(&page.first_frame).or_else(|| view.pic.clone());
        }
        if candidate.duration.is_none() {
            candidate.duration = page
                .duration
                .filter(|value| value.is_finite() && *value > 0.);
        }
    }
}

/// 用 Bilibili web API 补全分 P 候选的分集标题、首帧预览图与时长。
/// 辅助探测：任何失败都保留原候选，不阻断任务。
pub fn enrich_bilibili_candidates(candidates: &mut [SourceCandidate]) {
    let mut bvids = Vec::new();
    for candidate in candidates.iter() {
        if let Some(bvid) = crate::config::bilibili_bvid(&candidate.input)
            && !bvids.contains(&bvid)
        {
            bvids.push(bvid);
        }
        if bvids.len() >= BILIBILI_ENRICH_LIMIT {
            break;
        }
    }
    let agent = crate::bilibili_agent_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(3)))
        .timeout_per_read(Some(std::time::Duration::from_secs(3)))
        .timeout_global(Some(std::time::Duration::from_secs(8)))
        .build()
        .new_agent();
    // 每个请求最长 8s：并发补全，避免几十个分集串行等待。单个失败只记日志。
    let views = crate::pool::map_pool(
        bvids.iter().collect(),
        BILIBILI_ENRICH_CONCURRENCY,
        &Default::default(),
        |bvid| Ok(bilibili_view(&agent, bvid)),
    )
    .unwrap_or_default();
    for (bvid, view) in bvids.iter().zip(views) {
        match view {
            Ok(view) => apply_bilibili_view(candidates, bvid, &view),
            Err(error) => tracing::debug!(bvid, "分集信息补充失败：{error:#}"),
        }
    }
}

/// Bilibili's own hosts and image CDNs, which reset some rustls handshakes
/// (see [`crate::bilibili_agent_builder`]).
fn bilibili_host(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            [
                "bilibili.com",
                "hdslb.com",
                "biliimg.com",
                "bilivideo.com",
                "bilivideo.cn",
            ]
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
        })
}

/// A small remote file such as a cover image, at most `limit` bytes, over the TLS
/// stack its host accepts. Stops between reads once `cancel` is set.
pub fn read_remote(
    url: &str,
    limit: usize,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::sync::atomic::Ordering;
    let builder = if bilibili_host(url) {
        crate::bilibili_agent_builder()
    } else {
        crate::runtime::agent_builder()
    };
    // Image CDNs redirect (http to https, a mirror); covers always followed them.
    let agent = builder
        .max_redirects(3)
        .timeout_connect(Some(std::time::Duration::from_secs(3)))
        .timeout_per_read(Some(std::time::Duration::from_secs(2)))
        .timeout_global(Some(std::time::Duration::from_secs(6)))
        .build()
        .new_agent();
    let mut response = agent.get(url).call()?;
    let mut reader = response.body_mut().as_reader();
    let mut bytes = Vec::new();
    let mut buffer = [0; 32 * 1024];
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), tr!("已取消读取"));
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
        ensure!(bytes.len() <= limit, tr!("封面超过读取上限"));
    }
}

fn subtitle_evidence(
    info: &ExtractorInfo,
    identity: &str,
    diagnostics: &str,
) -> crate::subtitle::SubtitleEvidence {
    use crate::subtitle::{SubtitleEvidence, SubtitleKind, SubtitleOrigin, SubtitleTrack};
    const FAILURE: [&str; 6] = ["error", "unable", "failed", "login", "logged in", "sign in"];
    let warning = diagnostics
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            (lower.contains("subtitle") || lower.contains("caption"))
                && FAILURE.iter().any(|word| lower.contains(word))
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
        for (language_key, formats) in channel.iter().flatten() {
            // Danmaku/chat are timed comments, not a transcript of the video.
            if matches!(language_key.as_str(), "danmaku" | "live_chat") {
                continue;
            }
            let readable: Vec<_> = formats
                .iter()
                .filter(|format| {
                    const READABLE: [&str; 9] = [
                        "srt", "vtt", "ttml", "srv1", "srv2", "srv3", "json3", "ass", "lrc",
                    ];
                    format["ext"]
                        .as_str()
                        .is_some_and(|ext| READABLE.contains(&ext))
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
            // yt-dlp asks YouTube for a machine translation with `tlang`; the
            // original track's URL has none, whatever its key or name says.
            let translated = automatic
                && readable.iter().any(|format| {
                    format["url"]
                        .as_str()
                        .and_then(|url| url.split_once('?'))
                        .is_some_and(|(_, query)| {
                            query.split('&').any(|pair| pair.starts_with("tlang="))
                        })
                });
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
                translated,
                origin: SubtitleOrigin::Online {
                    language_key: language_key.clone(),
                    automatic,
                    inline_text,
                },
                source_order: tracks.len(),
            });
        }
    }
    // YouTube lists its captions twice, as `en-orig` and `en`: one text, which the
    // fallback already reads as one. Offer it once, under the plain key.
    let plain_captions: Vec<String> = tracks
        .iter()
        .filter(|track| !track.translated)
        .filter_map(|track| match &track.origin {
            SubtitleOrigin::Online {
                language_key,
                automatic: true,
                ..
            } => Some(language_key.clone()),
            _ => None,
        })
        .collect();
    tracks.retain(|track| match &track.origin {
        SubtitleOrigin::Online {
            language_key,
            automatic: true,
            ..
        } => !language_key
            .strip_suffix("-orig")
            .is_some_and(|plain| plain_captions.iter().any(|key| key == plain)),
        _ => true,
    });
    if !tracks.is_empty() {
        SubtitleEvidence::Found { tracks, warning }
    } else if let Some(message) = warning {
        SubtitleEvidence::Failed { message }
    } else if info.subtitles.is_none() && info.automatic_captions.is_none() {
        SubtitleEvidence::Unsupported {
            message: tr!("此来源暂不支持读取字幕，可以识别视频声音 / This source does not support reading subtitles yet; you can transcribe the video audio instead").into(),
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
    let metadata = |path: &Path| {
        std::fs::metadata(path).with_context(|| {
            tr!(
                "无法读取视频文件 {} / Cannot read video file {}",
                path.display(),
                path.display()
            )
        })
    };
    let before = metadata(path)?;
    let digest = crate::digest::file_digest_cancellable(path, cancel)?;
    ensure!(
        !cancel.load(std::sync::atomic::Ordering::Relaxed),
        tr!("已取消读取文件 / File reading cancelled")
    );
    let after = metadata(path)?;
    ensure!(
        before.len() == after.len() && before.modified().ok() == after.modified().ok(),
        tr!(
            "视频文件在读取时发生变化，请重新读取 / The video file changed while reading; read it again"
        )
    );
    Ok(format!("local:sha256:{digest}"))
}

/// Probe once and return the single video; collection/unresolved links are errors.
pub async fn probe_video(url: &str, control: &crate::dispatch::TaskControl) -> Result<OnlineVideo> {
    match probe_online(url, control).await? {
        OnlineProbe::Video { video } => Ok(video),
        OnlineProbe::Collection { .. } => {
            anyhow::bail!(tr!(
                "这个链接包含多个视频，请选择具体单集后生成笔记 / This link contains multiple videos; choose a specific episode before generating notes"
            ))
        }
        OnlineProbe::Unresolved { message } => anyhow::bail!("{message}"),
    }
}

/// Metadata and all available subtitle channels only; no media/AI requests.
pub(crate) async fn probe_online(
    url: &str,
    control: &crate::dispatch::TaskControl,
) -> Result<OnlineProbe> {
    let (mut cmd, _cookies) = ytdlp_command(url)?;
    cmd.args(probe_args(true));
    if crate::playback::names_youtube_video(url) {
        cmd.arg("--no-playlist");
    }
    let out = run_ytdlp(url, &mut cmd, control, false).await?;
    parse_online_probe(&out.stdout, url, &String::from_utf8_lossy(&out.stderr))
}

/// What a read-only probe asks yt-dlp for, here and in the desktop preview: one JSON
/// document, a playlist only listed, one retry. `--simulate` writes neither media nor
/// subtitle files, while `with_subtitles` makes extractors inspect both caption channels.
pub fn probe_args(with_subtitles: bool) -> Vec<&'static str> {
    let mut args = vec!["--simulate", "--dump-single-json", "--flat-playlist"];
    args.extend(["--retries", "1"]);
    if with_subtitles {
        args.extend(["--write-subs", "--write-auto-subs", "--sub-langs", "all"]);
    }
    args
}

/// 抓取的平台字幕（yt-dlp 产物）。
pub struct SubtitleFetch {
    pub path: PathBuf,
    /// true = 平台自动生成字幕（auto-caption）
    pub auto: bool,
}

/// CLI compatibility path. `video` comes from the run's single probe so discovery
/// stays consistent; select from every language, then fetch that track. When it
/// cannot be read, the next ones in order are tried, as the desktop does for its
/// default choice ([`crate::subtitle::fallback_tracks`]), and each fallback is logged.
pub async fn fetch_subtitle(
    video: &OnlineVideo,
    out_dir: &Path,
    control: &crate::dispatch::TaskControl,
) -> Result<Option<SubtitleFetch>> {
    use crate::subtitle::SubtitleEvidence;
    match &video.subtitles {
        SubtitleEvidence::Found { tracks, .. } => {
            let mut tracks = tracks.clone();
            let language = subtitle_language(out_dir)?;
            crate::subtitle::sort_tracks(
                &mut tracks,
                &[],
                &language,
                video.original_language.as_deref(),
            );
            let track = tracks.first().context(tr!(
                "字幕列表为空，请重新读取字幕 / Subtitle list is empty; probe subtitles again"
            ))?;
            let mut first_error = None;
            for candidate in crate::subtitle::fallback_tracks(
                &tracks,
                track,
                &language,
                video.original_language.as_deref(),
            ) {
                control.check()?;
                if first_error.is_some() {
                    let message = tr!("改读字幕「{}」", crate::subtitle::track_label(candidate));
                    if c2m_protocol::progress::is_json() {
                        tracing::warn!(track = %candidate.id, "{message}");
                    } else {
                        tracing::warn!("{message}");
                    }
                }
                match fetch_selected_subtitle(
                    &video.meta.webpage_url,
                    &video.identity,
                    candidate,
                    out_dir,
                    control,
                )
                .await
                {
                    Ok(fetched) => return Ok(Some(fetched)),
                    Err(error) => {
                        control.check()?;
                        let raw = format!("{error:#}");
                        let message = tr!(
                            "无法读取字幕「{}」：{}",
                            crate::subtitle::track_label(candidate),
                            failure_reason(&error)
                        );
                        // The terminal gets one line; machines get the track and
                        // the whole error; yt-dlp's full output is in the debug log.
                        if c2m_protocol::progress::is_json() {
                            tracing::warn!(track = %candidate.id, error = %raw, "{message}");
                        } else {
                            tracing::warn!("{message}");
                            tracing::debug!(track = %candidate.id, error = %raw);
                        }
                        first_error.get_or_insert(error);
                    }
                }
            }
            Err(first_error.expect("the chosen track is always tried"))
        }
        SubtitleEvidence::Failed { message } => {
            anyhow::bail!("读取字幕失败 / Could not read subtitles: {message}")
        }
        SubtitleEvidence::Unchecked => anyhow::bail!(tr!(
            "尚未检查字幕，请重新读取视频信息 / Subtitles not checked yet; probe the video info again"
        )),
        SubtitleEvidence::NoneFound | SubtitleEvidence::Unsupported { .. } => Ok(None),
    }
}

/// One line saying why a track could not be read: yt-dlp's own words when it
/// was yt-dlp that failed, else the error's first line.
fn failure_reason(error: &anyhow::Error) -> String {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<YtDlpFailure>())
        .map(|failure| failure.cause.clone())
        .filter(|cause| !cause.is_empty())
        .unwrap_or_else(|| {
            format!("{error:#}")
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
}

/// The language a task's online subtitle choice prefers, as the desktop's does:
/// the person's interface language. It is fixed at the task's first choice, so
/// that continuing the task in another language reads the same track and its
/// paid requests keep their ids. A work folder from a version that preferred
/// Chinese whatever the language (it has subtitle downloads, but no record)
/// keeps preferring Chinese.
fn subtitle_language(out_dir: &Path) -> Result<String> {
    let dir = out_dir.join(".subs");
    let record = dir.join("language");
    if let Ok(saved) = std::fs::read_to_string(&record)
        && !saved.trim().is_empty()
    {
        return Ok(saved.trim().to_owned());
    }
    let language = if dir.is_dir() {
        "zh"
    } else {
        c2m_i18n::i18n::person_lang().code()
    };
    std::fs::create_dir_all(&dir)?;
    crate::fsio::atomic_write(&record, language.as_bytes())?;
    Ok(language.to_owned())
}

/// Escape the complete language key as an exact yt-dlp subtitle regular expression.
pub fn exact_subtitle_language(key: &str) -> String {
    let mut escaped = String::from("^");
    for character in key.chars() {
        if ".+*?^$()[]{}|\\".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('$');
    escaped
}

/// Fetch only the confirmed online track, rejecting a changed video identity.
pub(crate) async fn fetch_selected_subtitle(
    url: &str,
    source_identity: &str,
    track: &crate::subtitle::SubtitleTrack,
    out_dir: &Path,
    control: &crate::dispatch::TaskControl,
) -> Result<SubtitleFetch> {
    use crate::subtitle::SubtitleOrigin;
    let SubtitleOrigin::Online {
        language_key,
        automatic,
        inline_text,
    } = &track.origin
    else {
        anyhow::bail!(tr!(
            "所选字幕不是在线字幕 / The selected subtitle is not an online subtitle"
        ));
    };
    ensure!(
        track
            .id
            .starts_with(&format!("{source_identity}:subtitle:")),
        tr!(
            "所选字幕与当前视频不一致，请重新选择字幕 / The selected subtitle no longer matches the current video; select the subtitle again"
        )
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
        let (mut cmd, _cookies) = ytdlp_command(url)?;
        cmd.args([
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
        let out = run_ytdlp(url, &mut cmd, control, false).await?;
        match parse_online_probe(&out.stdout, url, &String::from_utf8_lossy(&out.stderr))? {
            OnlineProbe::Video { video } => ensure!(
                video.identity == source_identity,
                tr!(
                    "视频来源发生变化，请重新读取并选择字幕 / The video source changed; probe again and reselect the subtitle"
                )
            ),
            _ => anyhow::bail!(tr!(
                "无法确认字幕对应的视频，请重新读取 / Cannot confirm the video for the subtitle; probe again"
            )),
        }
        let path = crate::subtitle::pick_subtitle_file(temp.path())
            .context(tr!("所选字幕下载失败，请重新读取字幕，或改用语音识别 / The selected subtitle could not be downloaded; read subtitles again or use speech recognition"))?;
        crate::subtitle::read_subtitle_text(&path)?
    };
    let events = crate::subtitle::parse_subtitle(&text);
    ensure!(
        !events.is_empty(),
        tr!(
            "这份字幕未包含可读取的文字，请选择其他字幕 / This subtitle contains no readable text; choose another subtitle"
        )
    );
    let path = temp.path().join("selected.srt");
    crate::fsio::atomic_write(&path, crate::subtitle::to_srt(&events).as_bytes())?;
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

/// 下载视频到 `dest`（默认 1080p 上限，mp4 合并）。
/// 调用方先处理已有文件（校验或归档）：这里绝不把未校验的旧文件当作下载结果。
pub async fn download(
    url: &str,
    dest: &Path,
    max_height: u32,
    verbose: bool,
    control: &crate::dispatch::TaskControl,
) -> Result<()> {
    ensure!(
        !dest.exists(),
        tr!("视频位置已有未校验的文件 / An unverified file already exists at the video location")
    );
    let parent = dest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    // yt-dlp names the result itself: a merged download takes the merge format's
    // extension, and a single-file one its own (with `-o media.mp4.part` that
    // was `media.mp4`, the destination, left behind unverified). A private
    // directory beside the destination holds whatever it writes; only the one
    // finished file in it is moved into place.
    let work = tempfile::Builder::new()
        .prefix(".download-")
        .tempdir_in(parent)?;
    let (mut cmd, _cookies) = ytdlp_command(url)?;
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
        // 结构化进度（2021.11 起的 yt-dlp 接口）：前缀行由 run_once 解析转发
        "--newline",
        "--progress-template",
        "download:[C2MD] %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s",
        "-o",
    ]);
    cmd.arg(work.path().join("media.%(ext)s"));
    if verbose {
        cmd.arg("-v");
    }
    run_ytdlp(url, &mut cmd, control, true).await?;
    let produced = downloaded_file(work.path())?;
    tokio::fs::rename(&produced, dest).await?;
    Ok(())
}

/// The one finished media file yt-dlp left in its private directory. Format
/// fragments (`media.f137.mp4`), partial files and subtitles are not a result.
fn downloaded_file(dir: &Path) -> Result<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        // `media.<ext>` only: fragments are `media.f137.mp4`, partials `media.mp4.part`.
        let finished = name
            .strip_prefix("media.")
            .is_some_and(|ext| !ext.is_empty() && !ext.contains('.'));
        if finished && path.is_file() {
            found.push(path);
        }
    }
    // A merge names its output by the merge format, so that one wins.
    found.sort_by_key(|path| path.extension().is_none_or(|ext| ext != "mp4"));
    found.into_iter().next().context(tr!(
        "yt-dlp 已结束，但没有留下视频文件 / yt-dlp finished without leaving a video file"
    ))
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

/// The site's edge dropped the connection mid-handshake or mid-request. Bilibili's
/// CDN does this intermittently to yt-dlp's TLS stack as it does to rustls (see
/// [`crate::bilibili_agent_builder`]); the next attempt usually gets through, so
/// even a probe, which retries nothing else, tries again.
pub fn is_connection_reset(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    [
        "connection reset by peer",
        "connectionreseterror",
        "connection aborted",
        "unexpected_eof_while_reading",
        "eof occurred in violation of protocol",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Whether yt-dlp itself failed on a dropped connection: its final reason, not
/// a warning about an earlier attempt it retried on its own.
fn ended_by_connection_reset(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<YtDlpFailure>()
        .is_some_and(|failure| is_connection_reset(&failure.cause))
}

/// Shared by CLI extraction and the desktop preview. At most three attempts.
pub fn bilibili_retry_delay(stderr: &str, retries: usize) -> Option<std::time::Duration> {
    is_bilibili_412(stderr)
        .then(|| bilibili_retry_delay_steps(retries))
        .flatten()
}

fn bilibili_retry_delay_steps(retries: usize) -> Option<std::time::Duration> {
    [2, 5]
        .get(retries)
        .copied()
        .map(std::time::Duration::from_secs)
}

/// Machine consumers (the desktop task protocol) get desktop-specific guidance.
pub(crate) fn bilibili_hint(error: anyhow::Error) -> anyhow::Error {
    if crate::progress::is_json() {
        error.context(BILIBILI_412_DESKTOP_HINT)
    } else {
        error.context(BILIBILI_412_HINT)
    }
}

/// Marker in the error chain: yt-dlp was rejected by Bilibili with HTTP 412.
#[derive(Debug, thiserror::Error)]
#[error("bilibili http 412")]
pub(crate) struct Bilibili412;

pub const BILIBILI_412_DESKTOP_HINT: &str =
    "B 站暂时限制了访问。请稍后重试；若持续失败，请在设置的账号中重新登录 B 站，或改用本地视频。";

pub(crate) const BILIBILI_412_HINT: &str = "B 站暂时限制了访问。请稍后重试；若持续失败，运行 course2md --login bilibili 重新登录，或导入本地视频。 / Bilibili is temporarily limiting access. Retry later; if it keeps failing, run course2md --login bilibili to log in again, or import a local video.";

/// What a failed yt-dlp run suggests. Only a run that yt-dlp itself reports as
/// refused can be helped by an account; a stopped task did not fail, a 412 carries
/// its own advice, and a yt-dlp that never started says nothing about the site.
fn login_tip(url: &str, mut error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<Bilibili412>().is_none()
        && let Some(failure) = error.downcast_mut::<YtDlpFailure>()
    {
        failure.hint = crate::auth::login_hint(url, &failure.cause);
    }
    error
}

/// A failed yt-dlp run. Its own last `ERROR:` line comes first: that is what a
/// person can act on, and the line the desktop shows for a failed task. A login
/// suggestion, when one applies, follows it rather than replacing it, and the
/// end of yt-dlp's output follows for the log.
#[derive(Debug)]
pub struct YtDlpFailure {
    /// yt-dlp's own words, without the `ERROR:` prefix.
    pub cause: String,
    hint: Option<&'static str>,
    code: Option<i32>,
    tail: String,
}

impl YtDlpFailure {
    fn new(code: Option<i32>, stderr: &str) -> Self {
        let lines = || stderr.lines().rev().map(str::trim);
        let cause = lines()
            .find_map(|line| line.strip_prefix("ERROR:"))
            .or_else(|| lines().find(|line| !line.is_empty()))
            .unwrap_or_default()
            .trim()
            .to_owned();
        Self {
            cause,
            hint: None,
            code,
            tail: crate::error::tail_lines(stderr, 5),
        }
    }
}

impl std::fmt::Display for YtDlpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.cause.is_empty() {
            writeln!(f, "yt-dlp: {}", self.cause)?;
        }
        if let Some(hint) = self.hint {
            writeln!(f, "{hint}")?;
        }
        write!(
            f,
            "{}\n{}",
            crate::error::failure_header("yt-dlp", self.code),
            self.tail
        )
    }
}

impl std::error::Error for YtDlpFailure {}

/// Runs yt-dlp, the URL last (`--` keeps it from reading as an option).
async fn run_ytdlp(
    url: &str,
    cmd: &mut Command,
    control: &crate::dispatch::TaskControl,
    retry_transient: bool,
) -> Result<std::process::Output> {
    run_retrying(cmd.arg("--").arg(url), control, retry_transient)
        .await
        .map_err(|error| login_tip(url, error))
}

/// Bilibili's transient 412 is retried with its own delays. With `retry_transient`
/// (a download, where nobody is waiting on a screen) other failures are too, unless
/// the error says retrying is pointless. At most [`YTDLP_ATTEMPTS`] in all; the wait
/// and the run stop with the task, and dropping the run kills yt-dlp.
async fn run_retrying(
    cmd: &mut Command,
    control: &crate::dispatch::TaskControl,
    retry_transient: bool,
) -> Result<std::process::Output> {
    let mut rejections = 0;
    for attempt in 1.. {
        control.check()?;
        let error = tokio::select! {
            run = run_once(cmd) => match run {
                Ok(output) => return Ok(output),
                Err(error) => error,
            },
            stop = control.watch() => return Err(stop),
        };
        let delay = if error.downcast_ref::<Bilibili412>().is_some() {
            rejections += 1;
            bilibili_retry_delay_steps(rejections - 1)
        } else {
            let text = format!("{error:#}");
            ((retry_transient && !is_deterministic_download_error(&text))
                || ended_by_connection_reset(&error))
            .then_some(TRANSIENT_RETRY_DELAY)
        };
        let Some(delay) = delay.filter(|_| attempt < YTDLP_ATTEMPTS) else {
            return Err(error);
        };
        tracing::warn!(attempt, "yt-dlp 失败，正在重试 / yt-dlp failed; retrying");
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            stop = control.watch() => return Err(stop),
        }
    }
    unreachable!("the attempts above are unbounded")
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

/// Reads a child's output to its end, one line at a time. Bytes that are not
/// UTF-8 (a title in a legacy code page) are replaced instead of ending the read:
/// a reader that stops leaves the pipe full and the child blocked on its next write.
async fn for_each_line(reader: impl tokio::io::AsyncRead + Unpin, mut line: impl FnMut(String)) {
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(reader);
    let mut buf = Vec::new();
    while matches!(reader.read_until(b'\n', &mut buf).await, Ok(1..)) {
        let text = buf.strip_suffix(b"\n").unwrap_or(&buf);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        line(String::from_utf8_lossy(text).into_owned());
        buf.clear();
    }
}

/// One yt-dlp run, both pipes read as it goes and byte progress forwarded. Its stdout
/// is what is left once progress lines are taken out. On failure the error carries
/// the end of stderr, which is where yt-dlp puts the actionable message.
async fn run_once(cmd: &mut Command) -> Result<std::process::Output> {
    let mut child = cmd
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context(tr!("启动 yt-dlp 失败 / Failed to start yt-dlp"))?;
    let stdout = child.stdout.take().expect(tr!("stdout 管道已配置"));
    let stderr = child.stderr.take().expect(tr!("stderr 管道已配置"));
    let report = |line: &str| match ytdlp_progress(line) {
        Some((downloaded, total)) => {
            crate::progress::download_progress(downloaded, total);
            true
        }
        None => false,
    };
    let out = async {
        let mut kept = Vec::new();
        for_each_line(stdout, |line| {
            if !report(&line) {
                kept.extend_from_slice(line.as_bytes());
                kept.push(b'\n');
            }
        })
        .await;
        kept
    };
    let err = async {
        const STDERR_TAIL_BYTES: usize = 64 * 1024;
        let mut tail: std::collections::VecDeque<String> = Default::default();
        let mut bytes = 0usize;
        for_each_line(stderr, |line| {
            report(&line);
            bytes += line.len() + 1;
            tail.push_back(line);
            while bytes > STDERR_TAIL_BYTES {
                let Some(old) = tail.pop_front() else { break };
                bytes = bytes.saturating_sub(old.len() + 1);
            }
        })
        .await;
        tail.into_iter().collect::<Vec<_>>().join("\n")
    };
    let (status, stdout, stderr) = tokio::join!(child.wait(), out, err);
    let status = status.context(tr!(
        "等待 yt-dlp 结束失败 / Failed waiting for yt-dlp to exit"
    ))?;
    if status.success() {
        return Ok(std::process::Output {
            status,
            stdout,
            stderr: stderr.into_bytes(),
        });
    }
    let error = anyhow::Error::new(YtDlpFailure::new(status.code(), &stderr));
    Err(if is_bilibili_412(&stderr) {
        bilibili_hint(error.context(Bilibili412))
    } else {
        error
    })
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    async fn run(cmd: &mut tokio::process::Command) -> anyhow::Result<String> {
        let out = super::run_retrying(cmd, &crate::dispatch::TaskControl::default(), false).await?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    use super::*;

    /// Bytes that are not UTF-8 must not stop the reader: a child that fills
    /// the now-unread pipe blocks forever. The test's deadline is that hang.
    #[cfg(unix)]
    #[tokio::test]
    async fn output_that_is_not_utf8_cannot_hang_the_download() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            r#"
            printf '\377\376 gbk\n' >&2
            printf '\377\376 gbk\n'
            i=0; while [ $i -lt 4000 ]; do
              echo 'padding padding padding padding padding padding padding' >&2
              echo 'padding padding padding padding padding padding padding'
              i=$((i+1))
            done
            echo 'ERROR: the actionable last line' >&2
            exit 1
        "#,
        ]);
        let result = tokio::time::timeout(std::time::Duration::from_secs(20), run_once(&mut cmd))
            .await
            .expect("the reader stopped draining the pipe");
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("the actionable last line"), "{message}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stopped_task_does_not_wait_for_a_probe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.json");
        let control = crate::dispatch::TaskControl::open(
            dir.path(),
            crate::dispatch::ControlSource {
                control_path: Some(path.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::fsio::atomic_write(
            &path,
            &serde_json::to_vec(&crate::dispatch::Control {
                intent: crate::dispatch::Intent::Cancel,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let started = std::time::Instant::now();
        let error = run_retrying(&mut cmd, &control, false).await.unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        // The stop stays recognisable through the wrapping every probe applies,
        // and nothing suggests logging in to a task the person cancelled.
        let error = login_tip("https://www.bilibili.com/video/BV1", error);
        assert!(error.downcast_ref::<crate::dispatch::Failure>().is_some());
        assert!(!format!("{error:#}").contains("--login"), "{error:#}");
    }

    #[test]
    fn a_login_tip_keeps_the_markers_retries_look_for() {
        let url = "https://www.bilibili.com/video/BV1";
        let error = login_tip(url, anyhow::anyhow!("yt-dlp failed").context(Bilibili412));
        assert!(error.downcast_ref::<Bilibili412>().is_some());
        // A yt-dlp that did not start is not a refusal: no account would help.
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let error = login_tip(
            url,
            anyhow::Error::new(missing).context("启动 yt-dlp 失败 / Failed to start yt-dlp"),
        );
        let message = format!("{error:#}");
        assert!(!message.contains("--login"), "{message}");
        assert!(!message.contains("提示"), "{message}");
        assert!(message.starts_with("启动 yt-dlp 失败"), "{message}");
    }

    #[test]
    fn a_download_is_the_one_finished_file_whatever_yt_dlp_named_it() {
        let dir = tempfile::tempdir().unwrap();
        assert!(downloaded_file(dir.path()).is_err());
        // A single-file format keeps its own extension (ACCEPTANCE C3).
        std::fs::write(dir.path().join("media.m4a"), b"audio").unwrap();
        assert!(downloaded_file(dir.path()).unwrap().ends_with("media.m4a"));
        for leftover in [
            "media.f137.mp4",
            "media.mp4.part",
            "media.zh.srt",
            "other.mp4",
        ] {
            std::fs::write(dir.path().join(leftover), b"x").unwrap();
        }
        std::fs::write(dir.path().join("media.mp4"), b"video").unwrap();
        assert!(downloaded_file(dir.path()).unwrap().ends_with("media.mp4"));
    }

    #[test]
    fn a_failed_run_puts_yt_dlps_reason_first_and_the_login_hint_after_it() {
        let stderr = "WARNING: [BiliBili] something\nERROR: [BiliBili] BV1: Unable to download webpage: HTTP Error 403: Forbidden\n";
        let error = login_tip(
            "https://www.bilibili.com/video/BV1",
            anyhow::Error::new(YtDlpFailure::new(Some(1), stderr)),
        );
        let message = format!("{error:#}");
        let mut lines = message.lines();
        assert_eq!(
            lines.next(),
            Some("yt-dlp: [BiliBili] BV1: Unable to download webpage: HTTP Error 403: Forbidden")
        );
        assert!(lines.next().unwrap().contains("--login bilibili"));
        assert!(message.contains("WARNING: [BiliBili] something"));
        assert!(error.downcast_ref::<YtDlpFailure>().is_some());
        // YouTube suggests its browser login only when YouTube asked for one.
        let bot = "ERROR: [youtube] x: Sign in to confirm you’re not a bot.";
        let error = login_tip(
            "https://www.youtube.com/watch?v=x",
            anyhow::Error::new(YtDlpFailure::new(Some(1), bot)),
        );
        assert!(format!("{error:#}").contains("--login youtube"));
        let gone = "ERROR: [youtube] x: Video unavailable";
        let error = login_tip(
            "https://www.youtube.com/watch?v=x",
            anyhow::Error::new(YtDlpFailure::new(Some(1), gone)),
        );
        assert!(!format!("{error:#}").contains("--login"));
    }

    #[test]
    fn a_dropped_connection_is_worth_another_attempt() {
        for text in [
            "ERROR: [BiliBili] 1xx411c7mD: Unable to download webpage: ('Connection aborted.', ConnectionResetError(54, 'Connection reset by peer'))",
            "ERROR: [youtube] x: Unable to download API page: [SSL: UNEXPECTED_EOF_WHILE_READING] EOF occurred in violation of protocol",
        ] {
            assert!(is_connection_reset(text), "{text}");
        }
        for text in [
            "ERROR: [BiliBili] BV1: HTTP Error 404: Not Found",
            "ERROR: Unsupported URL: https://example.com/",
        ] {
            assert!(!is_connection_reset(text), "{text}");
        }
    }

    #[test]
    fn only_a_run_that_ended_on_a_dropped_connection_is_retried_for_it() {
        let failure = |stderr: &str| anyhow::Error::new(YtDlpFailure::new(Some(1), stderr));
        assert!(ended_by_connection_reset(&failure(
            "ERROR: [BiliBili] BV1: Unable to download webpage: ('Connection aborted.', ConnectionResetError(54, 'Connection reset by peer'))"
        )));
        // yt-dlp retried the reset itself; its final reason is a refusal.
        let refused = failure(
            "WARNING: [download] Got error: ('Connection aborted.', ConnectionResetError(54, 'Connection reset by peer')). Retrying (1/10)...\nERROR: unable to download video data: HTTP Error 403: Forbidden",
        );
        assert!(!ended_by_connection_reset(&refused));
        assert!(!ended_by_connection_reset(&anyhow::anyhow!(
            "Connection reset by peer"
        )));
    }

    #[test]
    fn bilibili_image_hosts_use_the_platform_tls_stack() {
        for url in [
            "https://i0.hdslb.com/bfs/archive/x.jpg",
            "http://i2.hdslb.com/x.jpg",
            "https://api.bilibili.com/x",
        ] {
            assert!(bilibili_host(url), "{url}");
        }
        for url in [
            "https://i.ytimg.com/vi/x/hq.jpg",
            "https://hdslb.com.evil.test/x",
        ] {
            assert!(!bilibili_host(url), "{url}");
        }
    }

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
            crate::config::bilibili_bvid("https://www.bilibili.com/video/BV1CAxaeHEeH?p=3")
                .as_deref(),
            Some("BV1CAxaeHEeH")
        );
        assert_eq!(
            crate::config::bilibili_bvid("https://www.bilibili.com/video/BV1xx411c7mD/").as_deref(),
            Some("BV1xx411c7mD")
        );
        assert!(
            crate::config::bilibili_bvid("https://www.bilibili.com/bangumi/play/ep123").is_none()
        );
        assert!(crate::config::bilibili_bvid("https://example.com/video/BV1CAxaeHEeH").is_none());
        assert!(crate::config::bilibili_bvid("not a url").is_none());
        assert_eq!(
            bilibili_part_number("https://www.bilibili.com/video/BV1CAxaeHEeH?p=12"),
            Some(12)
        );
        assert_eq!(
            bilibili_part_number("https://www.bilibili.com/video/BV1CAxaeHEeH"),
            None
        );
    }

    /// The playlist a YouTube video was opened in stays where it plays; which
    /// video it is, and so every identity built from it, does not change.
    #[test]
    fn a_youtube_video_keeps_its_playlist_but_not_a_new_identity() {
        let video = br#"{"id":"abc123","title":"Lecture 3","extractor":"youtube","webpage_url":"https://www.youtube.com/watch?v=abc123"}"#;
        let read = |input: &str| {
            let OnlineProbe::Video { video } = parse_online_probe(video, input, "").unwrap() else {
                panic!("expected video")
            };
            video
        };
        let plain = read("https://www.youtube.com/watch?v=abc123");
        let listed = read("https://www.youtube.com/watch?v=abc123&list=PLcourse&index=3&si=x");
        assert_eq!(
            plain.meta.webpage_url,
            "https://www.youtube.com/watch?v=abc123"
        );
        assert_eq!(
            listed.meta.webpage_url,
            "https://www.youtube.com/watch?v=abc123&list=PLcourse&index=3"
        );
        assert_eq!(listed.identity, plain.identity);
        assert_eq!(listed.identity, "online:youtube:6:abc123");
        assert_eq!(
            (&listed.meta.id, &listed.meta.extractor),
            (&plain.meta.id, &plain.meta.extractor)
        );

        let playlist = br#"{"_type":"playlist","id":"PLcourse","title":"Course","extractor":"youtube:tab","entries":[{"_type":"url","id":"abc123","url":"https://www.youtube.com/watch?v=abc123","ie_key":"Youtube","title":"Lecture 3"}]}"#;
        let OnlineProbe::Collection { candidates, .. } = parse_online_probe(
            playlist,
            "https://www.youtube.com/watch?v=first&list=PLcourse&index=1",
            "",
        )
        .unwrap() else {
            panic!("expected collection")
        };
        assert_eq!(
            candidates[0].input,
            "https://www.youtube.com/watch?v=abc123&list=PLcourse"
        );
        assert_eq!(candidates[0].identity, Some(plain.identity));
    }

    #[test]
    fn candidate_identity_prefers_probe_and_derives_bilibili_parts() {
        let probed = SourceCandidate {
            input: "https://www.bilibili.com/video/BV1CAxaeHEeH?p=3".into(),
            title: "t".into(),
            identity: Some("online:bilibili:15:BV1CAxaeHEeH_p3".into()),
            duration: None,
            thumbnail: None,
        };
        assert_eq!(
            candidate_identity(&probed).as_deref(),
            Some("online:bilibili:15:BV1CAxaeHEeH_p3")
        );
        // 与单集探测（yt-dlp 对 ?p=N 返回 id `BV…_pN`）逐字节一致，批量去重才能命中
        let part = SourceCandidate {
            identity: None,
            ..probed.clone()
        };
        assert_eq!(
            candidate_identity(&part).as_deref(),
            Some("online:bilibili:15:BV1CAxaeHEeH_p3")
        );
        let bare = SourceCandidate {
            input: "https://www.bilibili.com/video/BV1CAxaeHEeH".into(),
            identity: None,
            ..probed.clone()
        };
        assert_eq!(
            candidate_identity(&bare).as_deref(),
            Some("online:bilibili:12:BV1CAxaeHEeH")
        );
        let unknown = SourceCandidate {
            input: "https://example.com/watch?v=abc".into(),
            identity: None,
            ..probed.clone()
        };
        assert_eq!(candidate_identity(&unknown), None);
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

    /// yt-dlp's YouTube list for an English video with English subtitles, in
    /// yt-dlp 2026.08.19's shape (`--write-subs --write-auto-subs`): translations
    /// of the subtitles (`zh-Hans-en`, "… from English") and of the captions
    /// (`zh-Hans`), whose URLs carry `tlang`, and the captions as `en-orig` and `en`.
    #[test]
    fn machine_translations_are_marked_and_never_chosen_first() {
        let OnlineProbe::Video { video } = parse_online_probe(
            include_bytes!("../tests/fixtures/source/youtube-translations.json"),
            "https://www.youtube.com/watch?v=r2vYObllqJU",
            "",
        )
        .unwrap() else {
            panic!("expected video")
        };
        let key = |track: &crate::subtitle::SubtitleTrack| match &track.origin {
            crate::subtitle::SubtitleOrigin::Online {
                language_key,
                automatic,
                ..
            } => format!(
                "{} {language_key}",
                if *automatic { "auto" } else { "manual" }
            ),
            _ => unreachable!(),
        };
        let original: Vec<_> = video
            .subtitles
            .tracks()
            .iter()
            .filter(|track| !track.translated)
            .map(key)
            .collect();
        // The captions' `en-orig` is the same text as `en`, offered once.
        assert_eq!(original, ["manual en", "auto en"]);
        assert_eq!(video.subtitles.tracks().len(), 13);
        for interface in ["zh-Hans", "zh", "zh-Hant", "ja", "en", "fr"] {
            let mut tracks = video.subtitles.tracks().to_vec();
            crate::subtitle::sort_tracks(
                &mut tracks,
                &[],
                interface,
                video.original_language.as_deref(),
            );
            let tried: Vec<_> = crate::subtitle::fallback_tracks(
                &tracks,
                &tracks[0],
                interface,
                video.original_language.as_deref(),
            )
            .into_iter()
            .map(key)
            .collect();
            // The subtitles, the captions once, then a translation into the
            // person's language when there is one, never into another.
            assert_eq!(tried[..2], ["manual en", "auto en"], "{interface}");
            match interface {
                "en" | "fr" => assert_eq!(tried.len(), 2, "{interface}"),
                _ => {
                    let family = interface.split('-').next().unwrap();
                    assert!(tried[2].starts_with(&format!("auto {family}")), "{tried:?}");
                }
            }
        }
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
        let meta = probe_video(
            "https://www.bilibili.com/video/BV1pb8o6yE8f",
            &crate::dispatch::TaskControl::default(),
        )
        .await
        .unwrap()
        .meta;
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
        let error = run_once(&mut cmd).await.unwrap_err();
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
