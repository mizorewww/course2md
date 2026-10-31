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
    let mut cfg: ConfigFile = toml::from_str(&s)
        .with_context(|| {
            format!(
                "配置格式有误，请修正后重试 / Fix the configuration syntax and retry: {}",
                p.display()
            )
        })
        .map_err(crate::error::invalid_configuration)?;
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

/// Replaces the configuration with the commented template (`config init`).
pub fn write_template() -> Result<PathBuf> {
    write_config(template().as_bytes())
}

/// Writes the configuration atomically, keeping the previous file beside it.
fn write_config(bytes: &[u8]) -> Result<PathBuf> {
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

/// `config init` 写入的带注释模板。`{field}` 占位符由内置默认值渲染（JSON 字面量也是
/// 合法的 TOML），注释不会与实际默认脱节。
pub fn template() -> String {
    let resolved = crate::options::resolve(String::new(), &Default::default(), &Default::default());
    let mut defaults = resolved
        .and_then(|cfg| Ok(serde_json::to_value(cfg)?))
        .unwrap_or_default();
    let provider = crate::config::default_provider_hint();
    defaults["asr_model"] = crate::config::default_model(provider).into();
    let mut text = TEMPLATE.to_string();
    for (prefix, values) in [
        ("", &defaults),
        ("llm.", &defaults["llm"]),
        ("asr_api.", &defaults["asr_api"]),
    ] {
        for (key, value) in values.as_object().into_iter().flatten() {
            text = text.replace(&format!("{{{prefix}{key}}}"), &value.to_string());
        }
    }
    text
}

const TEMPLATE: &str = include_str!("config_template.toml");

/// 打印配置文件与内置默认合并后的设置（不含本次命令行与环境变量覆盖）。
/// 默认值经 [`crate::options::resolve`] 得出，与实际转换使用同一套解析。
pub fn print_effective(cfg: &ConfigFile) -> Result<()> {
    let d = &cfg.defaults;
    let r = crate::options::resolve(String::new(), &crate::cli::RunOpts::default(), cfg)?;
    println!("配置文件 / Configuration: {}", config_path().display());
    crate::println_tr!(
        "文件设置与内置默认值；不含本次命令行或环境变量覆盖 / File settings and built-in defaults, before CLI and environment overrides"
    );
    let values = serde_json::to_value(&r)?;
    let plain = |value: &serde_json::Value| match value {
        serde_json::Value::Null => "-".to_owned(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => {
            let items: Vec<String> = items
                .iter()
                .filter_map(|i| i.as_str())
                .map(str::to_owned)
                .collect();
            items.join(",")
        }
        other => other.to_string(),
    };
    println!("[defaults]");
    println!("  {:<17}: {}", "out", r.out_root.display());
    for key in ["similarity", "sample_interval", "cooldown", "max_height"] {
        println!("  {key:<17}: {}", plain(&values[key]));
    }
    println!("  {:<17}: {}", "roi", d.roi.as_deref().unwrap_or("-"));
    println!("  {:<17}: {}", "threads", r.threads);
    match d.provider {
        Some(provider) => println!("  {:<17}: {provider}", "provider"),
        None => println!(
            "  {:<17}: {}",
            "provider",
            crate::tr!("{}（自动选择）", r.provider)
        ),
    }
    for key in [
        "slide_mode",
        "stable_secs",
        "asr_model",
        "transcript_source",
        "max_speech",
        "formats",
        "model_dir",
        "keep_video",
        "no_download",
        "resume",
        "gpu_layers",
        "mmproj_offload",
    ] {
        println!("  {key:<17}: {}", plain(&values[key]));
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
