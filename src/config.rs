use anyhow::Result as AnyhowResult;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// ASR 后端。typed enum 取代散落各处的字符串比较（`eq_ignore_ascii_case`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AsrProvider {
    Coreml,
    Gpu,
    Cpu,
    Npu,
    Api,
}

impl AsrProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Coreml => "coreml",
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
            Self::Npu => "npu",
            Self::Api => "api",
        }
    }
}

impl fmt::Display for AsrProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 截图发射模式：first（首个不同帧）| stable（等待画面稳定，适合动画课件）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlideMode {
    First,
    #[default]
    Stable,
}

impl fmt::Display for SlideMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::First => "first",
            Self::Stable => "stable",
        })
    }
}

/// 转写来源策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptSource {
    /// 字幕优先：平台人工字幕 > 平台自动字幕 > 本地 ASR
    #[default]
    Auto,
    /// 强制字幕：获取不到即报错（远程走 yt-dlp，本地查同名 .srt/.vtt）
    Subtitle,
    /// 强制本地 ASR（不查询平台字幕）
    Asr,
}

/// 输出格式。非法值在配置解析期即报错，不再拖到渲染阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Md,
    Html,
    Json,
}

impl OutputFormat {
    pub fn output_name(&self) -> &'static str {
        match self {
            Self::Md => "course.md",
            Self::Html => "course.html",
            Self::Json => "structured.json",
        }
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Md => "md",
            Self::Html => "html",
            Self::Json => "json",
        })
    }
}

/// 运行期管线配置（由 CLI 参数归一而来）。
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub url: String,
    pub out_dir: PathBuf,
    pub similarity: f64,
    pub sample_interval: f64,
    pub cooldown: f64,
    /// 下载视频的高度上限（默认 1080；讲义截图质量优先）
    pub max_height: u32,
    /// 截图发射模式：first（首个不同帧）| stable（等待画面稳定，适合动画课件）
    pub slide_mode: SlideMode,
    /// stable 模式下画面需保持不变的秒数
    pub stable_secs: f64,
    pub roi: Option<Roi>,
    pub threads: i32,
    pub provider: AsrProvider,
    pub max_speech: f32,
    pub formats: Vec<OutputFormat>,
    pub model_dir: PathBuf,
    pub keep_video: bool,
    pub no_download: bool,
    /// 断点续跑：跳过输出目录中已完成的 ASR chunk
    pub resume: bool,
    /// LLM 字幕润色（已合并 CLI 覆盖后的生效配置）
    pub llm: crate::llm::LlmSettings,
    /// 云端 STT（provider=api；已合并 CLI 覆盖）
    pub asr_api: crate::settings::AsrApi,
    /// 转写来源：字幕优先 / 强制字幕 / 强制 ASR
    pub transcript_source: TranscriptSource,
    /// coreml 后端模型选择：qwen3 | whisper
    pub asr_model: Option<String>,
    /// `-o` 根目录，实际课程目录是 `{out_root}/{platform}/{title}/{id}/`
    pub out_root: PathBuf,
    /// 缺失外部工具时允许按需自动安装（--no-install / [deps].auto_install 控制）
    pub auto_install_deps: bool,
}

/// 感兴趣区域；坐标可为像素或比例（0.0-1.0）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roi {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl Roi {
    /// 解析 "x1,y1-x2,y2"，坐标可带 `%` 或为像素。
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let (a, b) = s
            .split_once('-')
            .ok_or_else(|| anyhow::anyhow!("ROI 格式应为 x1,y1-x2,y2，收到 {s:?}"))?;
        let (x1, y1) = parse_xy(a)?;
        let (x2, y2) = parse_xy(b)?;
        let (x1, x2) = (x1.min(x2), x1.max(x2));
        let (y1, y2) = (y1.min(y2), y1.max(y2));
        if x2 <= x1 || y2 <= y1 {
            anyhow::bail!("ROI 为空矩形: {s:?}");
        }
        Ok(Self { x1, y1, x2, y2 })
    }

    /// 按帧尺寸换算为像素矩形。约定：坐标值 ≤1.0 视为比例，>1.0 视为像素。
    pub fn pixels(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let sx = |v: f64| {
            if v <= 1.0 {
                (v * w as f64).round() as u32
            } else {
                (v.round() as u32).min(w)
            }
        };
        let sy = |v: f64| {
            if v <= 1.0 {
                (v * h as f64).round() as u32
            } else {
                (v.round() as u32).min(h)
            }
        };
        let (x1, x2) = (
            sx(self.x1).min(w.saturating_sub(1)),
            sx(self.x2).clamp(1, w),
        );
        let (y1, y2) = (
            sy(self.y1).min(h.saturating_sub(1)),
            sy(self.y2).clamp(1, h),
        );
        (x1, y1, x2.max(x1 + 1), y2.max(y1 + 1))
    }
}

fn parse_xy(pair: &str) -> anyhow::Result<(f64, f64)> {
    let (a, b) = pair
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("ROI 坐标对格式应为 x,y，收到 {pair:?}"))?;
    Ok((parse_coord(a)?, parse_coord(b)?))
}

fn parse_coord(s: &str) -> anyhow::Result<f64> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        let v: f64 = p.trim().parse()?;
        Ok(v / 100.0)
    } else {
        Ok(s.parse()?)
    }
}

impl PipelineConfig {
    /// 全量预检：所有配置错误必须在任何昂贵操作（下载/抽帧/模型加载）之前暴露。
    /// 返回 Err 的送进 main 后直接退出，不会碰网络与模型。
    pub fn validate(&self) -> AnyhowResult<()> {
        let finite = |name: &str, v: f64| -> AnyhowResult<()> {
            anyhow::ensure!(v.is_finite(), "{name} 必须是有限数值（收到 {v}）");
            Ok(())
        };
        finite("similarity", self.similarity)?;
        finite("sample_interval", self.sample_interval)?;
        finite("cooldown", self.cooldown)?;
        finite("stable_secs", self.stable_secs)?;
        finite("max_speech", self.max_speech as f64)?;

        anyhow::ensure!(
            self.similarity > 0.0 && self.similarity <= 1.0,
            "similarity 必须在 (0, 1]：SSIM 阈值越高越敏感（截图更多），收到 {}",
            self.similarity
        );
        anyhow::ensure!(
            self.sample_interval > 0.0,
            "sample_interval 必须 > 0 秒（收到 {}）",
            self.sample_interval
        );
        anyhow::ensure!(
            self.cooldown >= 0.0,
            "cooldown 必须 >= 0 秒（收到 {}）",
            self.cooldown
        );
        anyhow::ensure!(
            self.stable_secs >= 0.0,
            "stable_secs 必须 >= 0 秒（收到 {}）",
            self.stable_secs
        );
        // 上限防御 split_smart 的 clamp panic（0 会让 clamp(min>max) abort）
        anyhow::ensure!(
            self.max_speech >= 1.0 && self.max_speech <= 600.0,
            "max_speech 必须在 1..=600 秒（收到 {}）；切分算法不允许 0/负值",
            self.max_speech
        );
        anyhow::ensure!(
            self.threads >= 1,
            "threads 必须 >= 1（收到 {}）",
            self.threads
        );
        anyhow::ensure!(
            self.max_height >= 144,
            "max_height 必须 >= 144（收到 {}）",
            self.max_height
        );
        anyhow::ensure!(
            !self.formats.is_empty(),
            "formats 不能为空（至少 md/html/json 之一）"
        );

        // provider × 模型兼容性：静默忽略用户指定的模型属于静默错误结果
        if let Some(m) = self
            .asr_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let lower = m.to_ascii_lowercase();
            match self.provider {
                AsrProvider::Gpu | AsrProvider::Cpu => {
                    anyhow::ensure!(
                        lower.contains("qwen"),
                        "provider {:?} 只支持 qwen3 系模型（当前缓存仅有 Qwen3-ASR GGUF）；
                         要用 Whisper 请选 --provider coreml / npu / api",
                        self.provider
                    );
                }
                AsrProvider::Coreml => {
                    anyhow::ensure!(
                        lower.contains("qwen") || lower.contains("whisper"),
                        "provider coreml 只支持 qwen3 / whisper（收到 {m:?}）"
                    );
                }
                AsrProvider::Npu => {
                    let known = [
                        "qwen3",
                        "qwen3-1.7b",
                        "qwen3-0.6b",
                        "1.7b",
                        "0.6b",
                        "whisper",
                        "turbo",
                        "whisper-turbo",
                        "whisper-large",
                        "large",
                        "tiny",
                        "whisper-tiny",
                        "base",
                        "whisper-base",
                        "small",
                        "whisper-small",
                    ];
                    anyhow::ensure!(
                        known.contains(&lower.as_str()) || lower.contains('/'),
                        "provider npu 的 asr_model 需是已知别名或 HuggingFace 仓库 id（含 /，收到 {m:?}）"
                    );
                }
                AsrProvider::Api => {}
            }
        }

        // api 后端：key 缺失时立即报错，而不是切完音频才发现
        if self.provider == AsrProvider::Api {
            let env_key = std::env::var("OPENROUTER_API_KEY")
                .ok()
                .is_some_and(|k| !k.trim().is_empty());
            anyhow::ensure!(
                !self.asr_api.api_key.trim().is_empty() || env_key,
                "provider api 需要 API key：配置文件 [asr_api].api_key、--asr-api-key 或环境变量 OPENROUTER_API_KEY"
            );
        }

        Ok(())
    }

    pub fn media_path(&self) -> PathBuf {
        self.out_dir.join("media.mp4")
    }
    pub fn audio_path(&self) -> PathBuf {
        self.out_dir.join("audio.wav")
    }
    pub fn frames_dir(&self) -> PathBuf {
        self.out_dir.join("frames")
    }
    pub fn timeline_path(&self) -> PathBuf {
        self.out_dir.join("timeline.jsonl")
    }
    pub fn meta_path(&self) -> PathBuf {
        self.out_dir.join("meta.json")
    }
}

/// 配置目录（XDG_CONFIG_HOME / Windows %APPDATA% / ~/.config）。
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(d).join("course2md");
    }
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("APPDATA") {
            return PathBuf::from(d).join("course2md");
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("course2md")
}

/// 缓存目录（模型等）。
pub fn cache_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(d).join("course2md");
    }
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(d).join("course2md");
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("course2md")
}

pub fn model_dir_from(opt: Option<&Path>) -> PathBuf {
    opt.map(|p| expand_tilde(p.to_path_buf()))
        .unwrap_or_else(|| cache_dir().join("models"))
}

/// 展开 `~` / `~/...`（仅 Unix 主目录约定；无 HOME 时原样返回）。
/// 防止配置里的 "~/cache" 真的在当前目录创建名为 `~` 的子目录。
pub fn expand_tilde(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    if s == "~" {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h);
        }
    } else if let Some(rest) = s.strip_prefix("~/")
        && let Some(h) = std::env::var_os("HOME")
    {
        return PathBuf::from(h).join(rest);
    }
    p
}

/// resume 三态解析：`--no-resume` > `--resume` > 配置文件 > 默认关闭。
/// 两级 CLI flag 互斥（clap conflicts_with），不会同时为 true。
pub fn resolve_resume(cli_resume: bool, cli_no_resume: bool, file_resume: Option<bool>) -> bool {
    if cli_no_resume {
        return false;
    }
    if cli_resume {
        return true;
    }
    file_resume.unwrap_or(false)
}

/// 平台默认后端提示（doctor 展示与 main 实际选择保持同一逻辑）。
pub fn default_provider_hint() -> AsrProvider {
    if cfg!(apple_native) {
        AsrProvider::Coreml
    } else if cfg!(target_os = "linux")
        && Path::new("/dev/accel/accel0").exists()
        && crate::error::require_cmd("llama-server").is_err()
    {
        AsrProvider::Npu
    } else {
        AsrProvider::Gpu
    }
}

/// 像 URL 或已存在的本地文件才当作输入；否则视为没传参数。
pub fn looks_like_source(s: &str) -> bool {
    let p = Path::new(s);
    if p.is_file() {
        return true;
    }
    let t = s.trim();
    t.starts_with("http://")
        || t.starts_with("https://")
        || t.contains("bilibili.com/")
        || t.contains("youtube.com/")
        || t.contains("youtu.be/")
}

/// `{root}/{platform}/{title}/{id}/`
pub fn course_dir(root: &Path, platform: &str, title: &str, id: &str) -> PathBuf {
    root.join(sanitize_component(platform))
        .join(sanitize_component(title))
        .join(sanitize_component(id))
}

pub fn platform_from(source: &str, extractor: &str) -> String {
    let e = extractor.to_ascii_lowercase();
    let s = source.to_ascii_lowercase();
    if Path::new(source).is_file() || e == "local" {
        "local".into()
    } else if e.contains("bili") || s.contains("bilibili.com") {
        "bilibili".into()
    } else if e.contains("youtube") || s.contains("youtube.com") || s.contains("youtu.be") {
        "youtube".into()
    } else if !e.is_empty() {
        sanitize_component(&e)
    } else {
        "web".into()
    }
}

pub fn infer_slug(source: &str) -> String {
    let p = Path::new(source);
    if p.is_file() {
        return sanitize_component(p.file_stem().and_then(|s| s.to_str()).unwrap_or("local"));
    }
    if let Some(id) = bvid(source) {
        return id;
    }
    if let Some(id) = youtube_id(source) {
        return id;
    }
    sanitize_component(source)
}

fn bvid(s: &str) -> Option<String> {
    let i = s.find("BV")?;
    let id: String = s[i..].chars().take(12).collect();
    if id.len() >= 6 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
        Some(id)
    } else {
        None
    }
}

fn youtube_id(s: &str) -> Option<String> {
    if let Some(rest) = s.split_once("v=").map(|(_, r)| r) {
        let id = rest.split(['&', '#', '/']).next()?;
        if id.len() >= 6 {
            return Some(id.to_string());
        }
    }
    if let Some(rest) = s.split_once("youtu.be/").map(|(_, r)| r) {
        let id = rest.split(['?', '&', '#', '/']).next()?;
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

/// 保留中文等标题字符，去掉路径非法符。
pub fn sanitize_component(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        let bad =
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
        if bad || c.is_whitespace() {
            if !prev_dash && !out.is_empty() {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    let out = out.trim_matches(['-', '.', ' ']).to_string();
    let out: String = out.chars().take(80).collect();
    if out.is_empty() {
        "untitled".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_gate() {
        assert!(!looks_like_source("course2md"));
        assert!(looks_like_source(
            "https://www.bilibili.com/video/BV1pb8o6yE8f"
        ));
        assert!(looks_like_source("https://youtu.be/dQw4w9WgXcQ"));
    }

    #[test]
    fn slug_and_layout() {
        assert_eq!(
            infer_slug("https://www.bilibili.com/video/BV1pb8o6yE8f/?spm=1"),
            "BV1pb8o6yE8f"
        );
        assert_eq!(infer_slug("https://youtu.be/dQw4w9WgXcQ"), "dQw4w9WgXcQ");
        let p = course_dir(Path::new("out"), "bilibili", "欢迎来到未来", "BV1pb8o6yE8f");
        assert_eq!(p, PathBuf::from("out/bilibili/欢迎来到未来/BV1pb8o6yE8f"));
    }

    #[test]
    fn expand_tilde_paths() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(expand_tilde("~/m".into()), PathBuf::from(&home).join("m"));
            assert_eq!(expand_tilde("~".into()), PathBuf::from(&home));
        }
        assert_eq!(expand_tilde("rel/x".into()), PathBuf::from("rel/x"));
        assert_eq!(expand_tilde("/abs/x".into()), PathBuf::from("/abs/x"));
    }

    fn valid_cfg() -> PipelineConfig {
        PipelineConfig {
            url: "https://youtu.be/x".into(),
            out_dir: "out".into(),
            out_root: "out".into(),
            similarity: 0.85,
            sample_interval: 1.0,
            cooldown: 10.0,
            max_height: 1080,
            slide_mode: SlideMode::Stable,
            stable_secs: 0.8,
            roi: None,
            threads: 4,
            provider: AsrProvider::Gpu,
            max_speech: 20.0,
            formats: vec![OutputFormat::Md],
            model_dir: "/tmp/m".into(),
            keep_video: false,
            no_download: false,
            resume: false,
            llm: Default::default(),
            asr_api: Default::default(),
            asr_model: None,
            transcript_source: TranscriptSource::Auto,
            auto_install_deps: false,
        }
    }

    #[test]
    fn validate_accepts_defaults() {
        valid_cfg().validate().unwrap();
    }

    #[test]
    fn validate_rejects_out_of_range() {
        let mut c = valid_cfg();
        // max_speech=0 曾导致 split_smart 中 clamp(min>max) panic
        c.max_speech = 0.0;
        assert!(c.validate().is_err());
        c.max_speech = f32::INFINITY;
        assert!(c.validate().is_err());
        c.max_speech = 20.0;

        c.similarity = 0.0;
        assert!(c.validate().is_err());
        c.similarity = 1.5;
        assert!(c.validate().is_err());
        c.similarity = 0.85;

        c.sample_interval = 0.0;
        assert!(c.validate().is_err());
        c.sample_interval = 1.0;

        c.threads = 0;
        assert!(c.validate().is_err());
        c.threads = 4;

        c.formats = vec![];
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_model_provider_mismatch() {
        // gpu/cpu 只有 Qwen3 GGUF：显式要 whisper 必须报错而不是静默用 qwen
        let mut c = valid_cfg();
        c.asr_model = Some("whisper".into());
        assert!(c.validate().is_err());
        c.provider = AsrProvider::Coreml;
        c.asr_model = Some("whisper".into());
        c.validate().unwrap();
        c.provider = AsrProvider::Npu;
        c.asr_model = Some("org/custom-ov-model".into());
        c.validate().unwrap();
        c.asr_model = Some("not-a-repo-id".into());
        assert!(c.validate().is_err());
    }

    #[test]
    fn resume_priority() {
        // --no-resume 压倒一切
        assert!(!resolve_resume(true, true, Some(true)));
        assert!(!resolve_resume(false, true, Some(true)));
        // --resume 次之
        assert!(resolve_resume(true, false, Some(false)));
        // 都没传：回落配置文件
        assert!(resolve_resume(false, false, Some(true)));
        assert!(!resolve_resume(false, false, Some(false)));
        assert!(!resolve_resume(false, false, None));
    }

    #[test]
    fn roi_parse() {
        let r = Roi::parse("25%,0%-100%,100%").unwrap();
        assert_eq!(r.pixels(1000, 800), (250, 0, 1000, 800));
        assert!(Roi::parse("nonsense").is_err());
    }
}
