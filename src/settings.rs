//! 配置文件（~/.config/course2md/config.toml）。
//! 优先级：命令行参数 > 配置文件 > 内置默认值。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// `[defaults]`：命令行参数的默认值。全部可选，未设置的项回落内置默认。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    pub out: Option<PathBuf>,
    pub similarity: Option<f64>,
    pub sample_interval: Option<f64>,
    pub cooldown: Option<f64>,
    pub slide_mode: Option<crate::config::SlideMode>,
    pub stable_secs: Option<f64>,
    pub max_height: Option<u32>,
    pub roi: Option<String>,
    pub threads: Option<i32>,
    pub provider: Option<crate::config::AsrProvider>,
    /// coreml 后端的模型：qwen3-1.7b（默认）| qwen3-0.6b | whisper（首次使用可交互选择）
    pub asr_model: Option<String>,
    /// 转写来源：auto（字幕优先）| subtitle | asr
    pub transcript_source: Option<crate::config::TranscriptSource>,
    pub max_speech: Option<f32>,
    pub formats: Option<Vec<crate::config::OutputFormat>>,
    pub model_dir: Option<PathBuf>,
    pub keep_video: Option<bool>,
    pub no_download: Option<bool>,
    pub resume: Option<bool>,
    /// GPU 卸载层数（llama-server -ngl，0-99；仅 gpu 后端生效）
    pub gpu_layers: Option<u32>,
    /// 是否把多模态 projector（mmproj）卸载到 GPU
    pub mmproj_offload: Option<bool>,
}

/// 云端 STT 的请求模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AsrApiMode {
    /// POST {base_url}/audio/transcriptions（OpenAI 兼容转录端点，如 OpenRouter 的 qwen3-asr）
    #[default]
    Transcriptions,
    /// POST {base_url}/chat/completions（支持音频输入的多模态 LLM，
    /// 如 gpt-4o-audio-preview、Gemini、Qwen2-Audio 等 OpenAI 兼容端点）
    Chat,
}

impl std::fmt::Display for AsrApiMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Transcriptions => "transcriptions",
            Self::Chat => "chat",
        })
    }
}

/// 云端 STT（provider = "api"，OpenAI 兼容端点，如 OpenRouter）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AsrApi {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub mode: AsrApiMode,
}

impl Default for AsrApi {
    fn default() -> Self {
        Self {
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: String::new(),
            model: "qwen/qwen3-asr-flash-2026-02-10".into(),
            mode: AsrApiMode::Transcriptions,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopSettings {
    pub library_cards: bool,
    pub library_group_folders: bool,
    pub setup_completed: bool,
    pub system_titlebar: bool,
    pub reduce_motion: bool,
    /// The home page lists recent notes below the input.
    pub home_recent_notes: bool,
}

/// New libraries open as a card grid; a saved choice is kept as written.
impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            library_cards: true,
            library_group_folders: false,
            setup_completed: false,
            system_titlebar: false,
            reduce_motion: false,
            home_recent_notes: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    pub desktop: DesktopSettings,
    pub defaults: Defaults,
    pub llm: crate::llm::LlmSettings,
    pub asr_api: AsrApi,
}

pub fn config_path() -> PathBuf {
    crate::config::config_dir().join("config.toml")
}

/// 读取配置文件。文件不存在 → 默认值；存在但无法解析 → 硬错误（带位置信息），
/// 避免用户写错一个引号后静默回落默认、甚至开始下载 2.4GB 模型。
pub fn load() -> anyhow::Result<ConfigFile> {
    let p = config_path();
    if !p.is_file() {
        return Ok(ConfigFile::default());
    }
    let s = std::fs::read_to_string(&p).with_context(|| {
        format!(
            "无法读取配置文件 / Cannot read configuration: {}",
            p.display()
        )
    })?;
    toml::from_str(&s)
        .with_context(|| {
            format!(
                "配置格式有误，请修正后重试 / Fix the configuration syntax and retry: {}",
                p.display()
            )
        })
        .map_err(crate::error::invalid_configuration)
}

pub fn save(cfg: &ConfigFile) -> Result<PathBuf> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 覆盖前备份旧配置（用户可能有手改内容），失败只告警不阻断
    if p.is_file() {
        let bak = p.with_extension("toml.bak");
        if let Err(e) = std::fs::read(&p)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| crate::checkpoint::atomic_write(&bak, &bytes))
        {
            tracing::warn!(
                "无法备份旧配置 / Could not back up configuration to {}: {e}",
                bak.display()
            );
        }
    }
    let bytes = toml::to_string_pretty(cfg)?;
    crate::checkpoint::atomic_write(&p, bytes.as_bytes())?;
    Ok(p)
}

/// `config init` 写入的带注释模板。默认值占位符由内置默认渲染，注释不会与实际默认脱节。
pub fn template() -> String {
    use crate::config as c;
    let formats = crate::options::default_formats()
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let api = AsrApi::default();
    let llm = crate::llm::LlmSettings::default();
    [
        ("{OUT}", c::DEFAULT_OUT_DIR.to_string()),
        ("{SIMILARITY}", c::DEFAULT_SIMILARITY.to_string()),
        (
            "{SAMPLE_INTERVAL}",
            format!("{:?}", c::DEFAULT_SAMPLE_INTERVAL),
        ),
        ("{COOLDOWN}", format!("{:?}", c::DEFAULT_COOLDOWN)),
        ("{MAX_HEIGHT}", c::DEFAULT_MAX_HEIGHT.to_string()),
        ("{STABLE_SECS}", format!("{:?}", c::DEFAULT_STABLE_SECS)),
        ("{THREADS}", c::DEFAULT_THREADS.to_string()),
        ("{GPU_LAYERS}", c::DEFAULT_GPU_LAYERS.to_string()),
        ("{ASR_MODEL}", c::DEFAULT_ASR_MODEL.to_string()),
        ("{MAX_SPEECH}", format!("{:?}", c::DEFAULT_MAX_SPEECH)),
        ("{FORMATS}", formats),
        ("{API_BASE_URL}", api.base_url),
        ("{API_MODEL}", api.model),
        ("{SLIDE_MODE}", c::SlideMode::default().to_string()),
        (
            "{PROVIDER}",
            c::default_provider_hint().as_str().to_string(),
        ),
        ("{MMPROJ_OFFLOAD}", "true".to_string()),
        ("{KEEP_VIDEO}", "false".to_string()),
        ("{RESUME}", "true".to_string()),
        (
            "{TRANSCRIPT_SOURCE}",
            c::TranscriptSource::default().to_string(),
        ),
        // A TOML string, escaped: Windows paths contain backslashes.
        (
            "{MODEL_DIR}",
            toml::Value::String(c::model_dir_from(None).display().to_string()).to_string(),
        ),
        ("{LLM_ENABLED}", llm.enabled.to_string()),
        ("{LLM_BASE_URL}", llm.base_url),
        ("{LLM_MODEL}", llm.model),
        ("{LLM_VISION}", llm.vision.to_string()),
        ("{LLM_SUMMARIZE}", llm.summarize.to_string()),
        ("{LLM_DISABLE_HINT}", llm.disable_hint.to_string()),
        ("{LLM_CONCURRENCY}", llm.concurrency.to_string()),
    ]
    .into_iter()
    .fold(TEMPLATE.to_string(), |text, (key, value)| {
        text.replace(key, &value)
    })
}

const TEMPLATE: &str = r#"# course2md 配置 / Configuration
# 优先级：命令行 > 本文件 > 内置默认 / Priority: CLI > this file > built-in defaults
# 取消注释以设置默认值 / Uncomment values to set defaults
# 查看当前设置：course2md config show / View settings: course2md config show

[defaults]
# 输出根目录，按平台/标题/ID 分类 / Output root, organized by platform/title/ID
#out = "{OUT}"
# 截图相似度 (0,1]，越高截图越多 / Similarity (0,1]; higher captures more slides
#similarity = {SIMILARITY}
# 画面采样间隔（秒）/ Frame sampling interval (seconds)
#sample_interval = {SAMPLE_INTERVAL}
# 截图最短间隔（秒）/ Minimum interval between slides (seconds)
#cooldown = {COOLDOWN}
# 下载视频最大高度 / Maximum downloaded video height
#max_height = {MAX_HEIGHT}
# first 立即截图，stable 等动画结束 / first: immediate; stable: wait for animations
#slide_mode = "{SLIDE_MODE}"
#stable_secs = {STABLE_SECS}
# 仅比较指定画面区域 / Compare only this region
#roi = "40%,0%-100%,100%"
# 识别线程数 / Speech recognition threads
#threads = {THREADS}
# coreml: Apple Silicon 原生构建 / Apple Silicon native build
# gpu/cpu: 需要 llama-server 和约 2.4GB 模型 / requires llama-server and ~2.4GB model
# npu: Intel NPU，需要 OpenVINO / Intel NPU, requires OpenVINO
# api: 云端识别，需要 API 密钥 / cloud transcription, requires an API key
# 留空自动选择后端；course2md doctor 查看可用性 / Leave unset for automatic selection; check with course2md doctor
#provider = "{PROVIDER}"
# GPU 卸载层数（0–99）；核显不稳定时可降低 / GPU offload layers; lower on unstable integrated GPUs
#gpu_layers = {GPU_LAYERS}
# 将多模态 projector 放到 GPU；false 留在 CPU / Offload projector to GPU; false keeps it on CPU
#mmproj_offload = {MMPROJ_OFFLOAD}
# 识别模型，需与后端兼容 / Speech model; must match the backend
# gpu/cpu: qwen3-1.7b；coreml: qwen3-1.7b, qwen3-0.6b, whisper；npu: whisper 或 Hugging Face 仓库名 / or a Hugging Face repository
#asr_model = "{ASR_MODEL}"
# auto 优先字幕，无字幕再识别 / auto: subtitles first, then speech recognition
# subtitle 仅字幕；asr 仅识别 / subtitle: subtitles only; asr: speech recognition only
#transcript_source = "{TRANSCRIPT_SOURCE}"
# 每段语音最长秒数 (1–600) / Maximum speech segment length in seconds (1–600)
#max_speech = {MAX_SPEECH}
# 输出格式 / Output formats: md, html, json
#formats = [{FORMATS}]
# gpu/cpu 模型目录；默认使用系统缓存 / gpu/cpu model directory; platform cache by default
#model_dir = {MODEL_DIR}
# 成功后保留下载的视频 / Keep downloaded video after success
#keep_video = {KEEP_VIDEO}
# 继续上次中断的识别 / Continue interrupted transcription
#resume = {RESUME}

[asr_api]
# OpenAI 兼容语音服务 / OpenAI-compatible speech service
# transcriptions: /audio/transcriptions; chat: /chat/completions
# 请求方式和模型需由服务商支持 / The provider must support the selected mode and model
#mode = "transcriptions"
#base_url = "{API_BASE_URL}"
# 推荐通过 COURSE2MD_ASR_API_KEY 环境变量提供密钥 / Prefer the COURSE2MD_ASR_API_KEY environment variable
#api_key = ""
#model = "{API_MODEL}"

[llm]
# 可选 AI 校对；运行 course2md llm setup 配置 / Optional AI proofreading; configure with course2md llm setup
enabled = {LLM_ENABLED}
#base_url = "{LLM_BASE_URL}"
#api_key = ""
#model = "{LLM_MODEL}"
# 自定义校对要求 / Custom proofreading instructions
#prompt = ""
# 隐藏完成后的提示 / Hide the completion tip
#disable_hint = {LLM_DISABLE_HINT}
# 附截图辅助校对，需要视觉模型 / Attach slides; requires an image-capable model
#vision = {LLM_VISION}
# 并发请求数 / Concurrent requests
#concurrency = {LLM_CONCURRENCY}
# 在笔记中加入 AI 摘要，需要 enabled = true / Add AI summaries; requires enabled = true
#summarize = {LLM_SUMMARIZE}
# 双语笔记的目标语言（zh-Hans、zh-Hant、en、ja、ko、fr、de、es）；已是该语言时不翻译
# Bilingual notes: target language; skipped when the text is already in it
#translate = "en"
"#;

/// 打印配置文件与内置默认合并后的设置（不含本次命令行与环境变量覆盖）。
/// 默认值经 [`crate::options::resolve`] 得出，与实际转换使用同一套解析。
pub fn print_effective(cfg: &ConfigFile) -> Result<()> {
    let d = &cfg.defaults;
    let r = crate::options::resolve(String::new(), &crate::cli::RunOpts::default(), cfg)?;
    println!("配置文件 / Configuration: {}", config_path().display());
    crate::println_tr!(
        "文件设置与内置默认值；不含本次命令行或环境变量覆盖 / File settings and built-in defaults, before CLI and environment overrides"
    );
    let rows: Vec<(&str, String)> = vec![
        ("out", r.out_root.display().to_string()),
        ("similarity", r.similarity.to_string()),
        ("sample_interval", r.sample_interval.to_string()),
        ("cooldown", r.cooldown.to_string()),
        ("max_height", r.max_height.to_string()),
        ("roi", d.roi.clone().unwrap_or_else(|| "-".into())),
        ("threads", r.threads.to_string()),
        (
            "provider",
            match d.provider {
                Some(p) => p.to_string(),
                None => crate::tr!("{}（自动选择）", r.provider),
            },
        ),
        ("slide_mode", r.slide_mode.to_string()),
        ("stable_secs", r.stable_secs.to_string()),
        (
            "asr_model",
            r.asr_model.clone().unwrap_or_else(|| "-".into()),
        ),
        ("transcript_source", r.transcript_source.to_string()),
        ("max_speech", r.max_speech.to_string()),
        (
            "formats",
            r.formats
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(","),
        ),
        ("model_dir", r.model_dir.display().to_string()),
        ("keep_video", r.keep_video.to_string()),
        ("no_download", r.no_download.to_string()),
        ("resume", r.resume.to_string()),
        ("gpu_layers", r.gpu_layers.to_string()),
        ("mmproj_offload", r.mmproj_offload.to_string()),
    ];
    println!("[defaults]");
    for (key, value) in rows {
        println!("  {key:<17}: {value}");
    }
    let hidden = |key: &str| {
        if key.is_empty() {
            "-"
        } else {
            crate::tr!("(已设置，隐藏 / set, hidden)")
        }
    };
    println!("[asr_api]");
    println!("  {:<17}: {}", "base_url", cfg.asr_api.base_url);
    println!("  {:<17}: {}", "model", cfg.asr_api.model);
    println!("  {:<17}: {}", "mode", cfg.asr_api.mode);
    println!("  {:<17}: {}", "api_key", hidden(&cfg.asr_api.api_key));
    println!("[llm]");
    println!("  {:<17}: {}", "enabled", cfg.llm.enabled);
    let model = if cfg.llm.model.is_empty() {
        "-"
    } else {
        &cfg.llm.model
    };
    println!("  {:<17}: {model}", "model");
    println!("  {:<17}: {}", "vision", cfg.llm.vision);
    println!("  {:<17}: {}", "concurrency", cfg.llm.concurrency);
    println!("  {:<17}: {}", "summarize", cfg.llm.summarize);
    println!(
        "  {:<17}: {}",
        "translate",
        cfg.llm.translate.as_deref().unwrap_or("-")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn template_paths_are_escaped_toml_strings() {
        let quoted = toml::Value::String(r"C:\Users\me\models".into()).to_string();
        let parsed: toml::Table = toml::from_str(&format!("model_dir = {quoted}")).unwrap();
        assert_eq!(parsed["model_dir"].as_str(), Some(r"C:\Users\me\models"));
    }

    #[test]
    fn template_parses_to_built_in_defaults_when_uncommented() {
        let text: String = super::template()
            .lines()
            .map(|line| {
                line.strip_prefix('#')
                    .filter(|l| {
                        l.starts_with(|c: char| c.is_ascii_lowercase()) && l.contains(" = ")
                    })
                    .unwrap_or(line)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let file: super::ConfigFile = toml::from_str(&text).unwrap();
        let from_template =
            crate::options::resolve(String::new(), &Default::default(), &file).unwrap();
        let built_in =
            crate::options::resolve(String::new(), &Default::default(), &Default::default())
                .unwrap();
        assert_eq!(from_template.resume, built_in.resume);
        assert_eq!(from_template.stable_secs, built_in.stable_secs);
        assert_eq!(from_template.similarity, built_in.similarity);
        assert_eq!(from_template.cooldown, built_in.cooldown);
        assert_eq!(from_template.max_speech, built_in.max_speech);
        assert_eq!(from_template.gpu_layers, built_in.gpu_layers);
        assert_eq!(from_template.formats, built_in.formats);
        assert_eq!(file.asr_api, super::AsrApi::default());
    }
}
