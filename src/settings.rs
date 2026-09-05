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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopSettings {
    pub setup_completed: bool,
    pub system_titlebar: bool,
    pub reduce_motion: bool,
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
    toml::from_str(&s).with_context(|| {
        format!(
            "配置格式有误，请修正后重试 / Fix the configuration syntax and retry: {}",
            p.display()
        )
    })
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

/// `config init` 写入的带注释模板。
pub const TEMPLATE: &str = r#"# course2md 配置 / Configuration
# 优先级：命令行 > 本文件 > 内置默认 / Priority: CLI > this file > built-in defaults
# 取消注释以设置默认值 / Uncomment values to set defaults
# 查看当前设置：course2md config show / View settings: course2md config show

[defaults]
# 输出根目录，按平台/标题/ID 分类 / Output root, organized by platform/title/ID
#out = "out"
# 截图相似度 (0,1]，越高截图越多 / Similarity (0,1]; higher captures more slides
#similarity = 0.85
# 画面采样间隔（秒）/ Frame sampling interval (seconds)
#sample_interval = 1.0
# 截图最短间隔（秒）/ Minimum interval between slides (seconds)
#cooldown = 10.0
# 下载视频最大高度 / Maximum downloaded video height
#max_height = 1080
# first 立即截图，stable 等动画结束 / first: immediate; stable: wait for animations
#slide_mode = "stable"
#stable_secs = 1.0
# 仅比较指定画面区域 / Compare only this region
#roi = "40%,0%-100%,100%"
# 识别线程数 / Speech recognition threads
#threads = 4
# coreml: Apple Silicon 原生构建 / Apple Silicon native build
# gpu/cpu: 需要 llama-server 和约 2.4GB 模型 / requires llama-server and ~2.4GB model
# npu: Intel NPU，需要 OpenVINO / Intel NPU, requires OpenVINO
# api: 云端识别，需要 API 密钥 / cloud transcription, requires an API key
# 留空自动选择后端；course2md doctor 查看可用性 / Leave unset for automatic selection; check with course2md doctor
#provider = "gpu"
# 模型需与后端兼容 / Models must match the backend
# gpu/cpu: qwen3; coreml: qwen3-1.7b, qwen3-0.6b, whisper
#asr_model = "qwen3"
# auto 优先字幕，无字幕再识别 / auto: subtitles first, then speech recognition
# subtitle 仅字幕；asr 仅识别 / subtitle: subtitles only; asr: speech recognition only
#transcript_source = "auto"
# 每段语音最长秒数 (1–600) / Maximum speech segment length in seconds (1–600)
#max_speech = 20.0
# 输出格式 / Output formats: md, html, json
#formats = ["md", "html"]
# gpu/cpu 模型目录；默认使用系统缓存 / gpu/cpu model directory; platform cache by default
#model_dir = "~/.cache/course2md/models"
# 成功后保留下载的视频 / Keep downloaded video after success
#keep_video = false
# 从语音检查点恢复 / Resume from speech checkpoints
#resume = false

[asr_api]
# OpenAI 兼容语音服务 / OpenAI-compatible speech service
# transcriptions: /audio/transcriptions; chat: /chat/completions
# 请求方式和模型需由服务商支持 / The provider must support the selected mode and model
#mode = "transcriptions"
#base_url = "https://openrouter.ai/api/v1"
# 推荐通过 COURSE2MD_ASR_API_KEY 环境变量提供密钥 / Prefer the COURSE2MD_ASR_API_KEY environment variable
#api_key = ""
#model = "qwen/qwen3-asr-flash-2026-02-10"

[llm]
# 可选 AI 润色；运行 course2md llm setup 配置 / Optional AI proofreading; configure with course2md llm setup
enabled = false
#base_url = "https://api.deepseek.com/v1"
#api_key = ""
#model = "deepseek-chat"
# 自定义润色要求 / Custom proofreading instructions
#prompt = ""
# 隐藏完成后的提示 / Hide the completion tip
#disable_hint = false
# 附截图辅助润色，需要视觉模型 / Attach slides; requires an image-capable model
#vision = false
# 并发请求数 / Concurrent requests
#concurrency = 8
# 在笔记中加入 AI 总结，需要 enabled = true / Add AI summaries; requires enabled = true
#summarize = false
"#;

/// 打印生效配置（CLI 覆盖合并前，来自文件的值）。
pub fn print_effective(cfg: &ConfigFile) {
    use crate::config as c;
    let d = &cfg.defaults;
    println!("配置文件 / Configuration: {}", config_path().display());
    println!(
        "文件设置与内置默认值；不含本次命令行或环境变量覆盖 / File settings and built-in defaults, before CLI and environment overrides"
    );
    println!("[defaults]");
    let s = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    println!(
        "  out            : {}",
        d.out
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| c::DEFAULT_OUT_DIR.into())
    );
    println!(
        "  similarity     : {}",
        d.similarity.unwrap_or(c::DEFAULT_SIMILARITY)
    );
    println!(
        "  sample_interval: {}",
        d.sample_interval.unwrap_or(c::DEFAULT_SAMPLE_INTERVAL)
    );
    println!(
        "  cooldown       : {}",
        d.cooldown.unwrap_or(c::DEFAULT_COOLDOWN)
    );
    println!(
        "  max_height     : {}",
        d.max_height.unwrap_or(c::DEFAULT_MAX_HEIGHT)
    );
    println!("  roi            : {}", s(&d.roi));
    println!(
        "  threads        : {}",
        d.threads.unwrap_or(c::DEFAULT_THREADS)
    );
    println!(
        "  provider       : {}",
        d.provider
            .map(|p| p.to_string())
            .unwrap_or_else(|| "(自动选择 / automatic)".into())
    );
    println!("  slide_mode     : {}", d.slide_mode.unwrap_or_default());
    println!(
        "  stable_secs    : {}",
        d.stable_secs.unwrap_or(c::DEFAULT_STABLE_SECS)
    );
    println!("  asr_model      : {}", s(&d.asr_model));
    println!(
        "  transcript_source: {}",
        match d.transcript_source.unwrap_or_default() {
            c::TranscriptSource::Auto => "auto",
            c::TranscriptSource::Subtitle => "subtitle",
            c::TranscriptSource::Asr => "asr",
        }
    );
    println!(
        "  max_speech     : {}",
        d.max_speech.unwrap_or(c::DEFAULT_MAX_SPEECH)
    );
    println!(
        "  formats        : {}",
        d.formats
            .clone()
            .map(|f| f
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(","))
            .unwrap_or_else(|| "md,html".into())
    );
    println!(
        "  model_dir      : {}",
        d.model_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(系统缓存 / platform cache)".into())
    );
    println!("  keep_video     : {}", d.keep_video.unwrap_or(false));
    println!("  no_download    : {}", d.no_download.unwrap_or(false));
    println!("  resume         : {}", d.resume.unwrap_or(false));
    println!("[asr_api]");
    println!("  base_url       : {}", cfg.asr_api.base_url);
    println!("  model          : {}", cfg.asr_api.model);
    println!(
        "  api_key        : {}",
        if cfg.asr_api.api_key.is_empty() {
            "-"
        } else {
            "(已设置，隐藏 / set, hidden)"
        }
    );
    println!("[llm]");
    println!("  enabled        : {}", cfg.llm.enabled);
    println!(
        "  model          : {}",
        if cfg.llm.model.is_empty() {
            "-"
        } else {
            &cfg.llm.model
        }
    );
    println!("  vision         : {}", cfg.llm.vision);
    println!("  concurrency    : {}", cfg.llm.concurrency);
    println!("  summarize      : {}", cfg.llm.summarize);
}
