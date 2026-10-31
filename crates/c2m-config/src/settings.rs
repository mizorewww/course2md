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
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    clap::ValueEnum,
    strum::Display,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum AsrApiMode {
    /// POST {base_url}/audio/transcriptions（OpenAI 兼容转录端点，如 OpenRouter 的 qwen3-asr）
    #[default]
    Transcriptions,
    /// POST {base_url}/chat/completions（支持音频输入的多模态 LLM，
    /// 如 gpt-4o-audio-preview、Gemini、Qwen2-Audio 等 OpenAI 兼容端点）
    Chat,
}

/// 云端 STT（provider = "api"，OpenAI 兼容端点，如 OpenRouter）。
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AsrApi {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub mode: AsrApiMode,
}

/// Credentials never reach logs or error text: the key shows only as set or not.
impl std::fmt::Debug for AsrApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsrApi")
            .field("base_url", &self.base_url)
            .field("api_key", &crate::config::Redacted(&self.api_key))
            .field("model", &self.model)
            .field("mode", &self.mode)
            .finish()
    }
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
    /// The home page shows the processing options below the input.
    pub home_options: bool,
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
            home_options: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    pub desktop: DesktopSettings,
    pub defaults: Defaults,
    pub llm: crate::llm_settings::LlmSettings,
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
    let mut cfg: ConfigFile = toml::from_str(&s)
        .with_context(|| {
            format!(
                "配置格式有误，请修正后重试 / Fix the configuration syntax and retry: {}",
                p.display()
            )
        })
        .map_err(crate::invalid::invalid_configuration)?;
    // The file is the user's own text and is not rewritten: the notice
    // repeats (once per run) until its asr_model line is changed.
    if crate::config::migrate_retired_npu_model(cfg.defaults.provider, &mut cfg.defaults.asr_model)
    {
        crate::config::announce_retired_npu_model(Some(&p));
    }
    Ok(cfg)
}

pub fn save(cfg: &ConfigFile) -> Result<PathBuf> {
    write_config(toml::to_string_pretty(cfg)?.as_bytes())
}

/// Writes the configuration atomically, keeping the previous file beside it.
pub fn write_config(bytes: &[u8]) -> Result<PathBuf> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 覆盖前备份旧配置（用户可能有手改内容），失败只告警不阻断
    if p.is_file() {
        let bak = p.with_extension("toml.bak");
        if let Err(e) = std::fs::read(&p)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| crate::fsio::atomic_write(&bak, &bytes))
        {
            tracing::warn!(
                "无法备份旧配置 / Could not back up configuration to {}: {e}",
                bak.display()
            );
        }
    }
    crate::fsio::atomic_write(&p, bytes)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    #[test]
    fn debug_never_contains_the_api_key() {
        let api = super::AsrApi {
            api_key: "secret-key".into(),
            ..Default::default()
        };
        let debug = format!(
            "{:?}",
            super::ConfigFile {
                asr_api: api,
                ..Default::default()
            }
        );
        assert!(!debug.contains("secret-key"), "{debug}");
    }
}
