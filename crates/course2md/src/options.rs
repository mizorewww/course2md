//! CLI 与桌面端共享的配置合并规则。
use crate::{config, llm, println_tr, settings, tr};
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

/// 内置默认输出格式。settings 的展示与配置模板也由它渲染，避免第二份真相。
pub fn default_formats() -> Vec<config::OutputFormat> {
    vec![config::OutputFormat::Md, config::OutputFormat::Html]
}

/// 配置文件 + CLI 覆盖 -> 生效 LLM 设置。
fn resolve_llm(opts: &RunOpts, file: &settings::ConfigFile) -> llm::LlmSettings {
    let mut s = file.llm.clone();
    s.enabled = config::resolve_flag(opts.llm, opts.no_llm, None, s.enabled);
    s.vision = config::resolve_flag(opts.llm_vision, opts.no_llm_vision, None, s.vision);
    if let Some(v) = &opts.llm_base_url {
        s.base_url = v.clone();
    }
    if let Some(v) = &opts.llm_api_key {
        s.api_key = v.clone();
    }
    if let Some(v) = &opts.llm_model {
        s.model = v.clone();
    }
    if let Some(v) = &opts.llm_prompt {
        s.prompt = Some(v.clone());
    }
    if opts.no_llm_hint {
        s.disable_hint = true;
    }
    s.summarize = config::resolve_flag(opts.summarize, opts.no_summarize, None, s.summarize);
    if opts.no_translate {
        s.translate = None;
    } else if let Some(target) = &opts.translate {
        s.translate = Some(target.clone());
    }
    s
}

/// 优先级：CLI 显式参数 > 配置文件 [defaults] > 内置默认。
pub fn resolve(
    source: String,
    opts: &RunOpts,
    file: &settings::ConfigFile,
) -> anyhow::Result<config::PipelineConfig> {
    let d = &file.defaults;
    use config::SlideMode;
    let out_root = opts
        .out
        .clone()
        .or_else(|| d.out.clone())
        .map(config::expand_tilde)
        .unwrap_or_else(|| config::DEFAULT_OUT_DIR.into());
    // CLI 显式参数 > 配置文件 > 内置默认
    macro_rules! pick {
        ($field:ident, $default:expr) => {
            opts.$field.or(d.$field).unwrap_or($default)
        };
    }
    let mut cfg = config::PipelineConfig {
        url: source,
        out_root,
        similarity: pick!(similarity, config::DEFAULT_SIMILARITY),
        sample_interval: pick!(sample_interval, config::DEFAULT_SAMPLE_INTERVAL),
        cooldown: pick!(cooldown, config::DEFAULT_COOLDOWN),
        max_height: pick!(max_height, config::DEFAULT_MAX_HEIGHT),
        slide_mode: pick!(slide_mode, SlideMode::Stable),
        stable_secs: pick!(stable_secs, config::DEFAULT_STABLE_SECS),
        roi: opts
            .roi
            .as_ref()
            .or(d.roi.as_ref())
            .map(|s| config::Roi::parse(s))
            .transpose()?,
        threads: pick!(threads, config::DEFAULT_THREADS),
        provider: opts
            .provider
            .or(d.provider)
            .unwrap_or_else(crate::asr::default_provider_hint),
        max_speech: pick!(max_speech, config::DEFAULT_MAX_SPEECH),
        formats: opts
            .formats
            .clone()
            .or_else(|| d.formats.clone())
            .unwrap_or_else(default_formats),
        model_dir: config::model_dir_from(opts.model_dir.as_deref().or(d.model_dir.as_deref())),
        keep_video: config::resolve_flag(opts.keep_video, opts.no_keep_video, d.keep_video, false),
        no_download: opts.no_download || d.no_download.unwrap_or(false),
        resume: config::resolve_flag(opts.resume, opts.no_resume, d.resume, true),
        llm: resolve_llm(opts, file),
        asr_api: resolve_asr_api(opts, file),
        asr_model: opts.asr_model.clone().or_else(|| d.asr_model.clone()),
        gpu_layers: pick!(gpu_layers, config::DEFAULT_GPU_LAYERS),
        mmproj_offload: config::resolve_flag(
            opts.mmproj_offload,
            opts.no_mmproj_offload,
            d.mmproj_offload,
            true,
        ),
        transcript_source: pick!(transcript_source, Default::default()),
    };
    // Without a configured provider the backend is chosen here, and may be the
    // NPU: a Qwen3 model saved for another backend is not run on it. A model
    // given on the command line stays the user's explicit (refused) choice.
    if opts.asr_model.is_none()
        && config::migrate_retired_npu_model(Some(cfg.provider), &mut cfg.asr_model)
    {
        config::announce_retired_npu_model(None);
    }
    Ok(cfg)
}

/// 云端 STT 配置合并：CLI > 配置文件 > 默认（OpenRouter）。
fn resolve_asr_api(opts: &RunOpts, file: &settings::ConfigFile) -> crate::settings::AsrApi {
    let mut a = file.asr_api.clone();
    if let Some(v) = &opts.asr_api_base_url {
        a.base_url = v.clone();
    }
    if let Some(v) = &opts.asr_api_key {
        a.api_key = v.clone();
    }
    if let Some(v) = &opts.asr_api_model {
        a.model = v.clone();
    }
    if let Some(v) = opts.asr_api_mode {
        a.mode = v;
    }
    a
}

#[derive(Args, Clone, Debug, Default)]
pub struct RunOpts {
    /// 输出根目录（按平台/标题/ID 建目录）/ Output root (platform/title/ID)
    #[arg(short, long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub out: Option<PathBuf>,

    /// 画面相似度阈值 (0,1]，越高截图越多 / Similarity threshold; higher captures more slides
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub similarity: Option<f64>,

    /// 画面采样间隔（秒）/ Frame sampling interval in seconds
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub sample_interval: Option<f64>,

    /// 截图最短间隔（秒）/ Minimum seconds between screenshots
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub cooldown: Option<f64>,

    /// 下载视频最大高度（推荐 1080）/ Maximum download height (1080 recommended)
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub max_height: Option<u32>,

    /// 截图时机：first 立即，stable 等画面稳定 / Capture immediately or wait for stable slides
    #[arg(long, value_enum)]
    #[arg(help_heading = "截图 / Slides")]
    pub slide_mode: Option<config::SlideMode>,

    /// stable 模式的画面稳定时间（秒）/ Seconds to wait for a stable frame
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub stable_secs: Option<f64>,

    /// 只看画面的某块区域判断翻页，格式：左%,上%-右%,下% / Detect slide changes in a region: left%,top%-right%,bottom%
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub roi: Option<String>,

    /// 语音识别线程数（至少 1）/ Speech recognition threads (minimum 1)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub threads: Option<i32>,

    /// 识别后端：coreml（Apple 芯片）、gpu、cpu、npu（Intel）、api（云端）/ Speech backend: coreml (Apple Silicon), gpu, cpu, npu (Intel), api (cloud)
    #[arg(long, value_enum)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub provider: Option<config::AsrProvider>,

    /// 识别模型（需与后端兼容）/ Speech model (must match the backend)
    /// gpu/cpu: qwen3-1.7b；coreml: qwen3-1.7b、qwen3-0.6b、whisper；npu: whisper 或 Hugging Face 仓库名 / or a Hugging Face repository
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_model: Option<String>,

    /// 云端识别地址 / Cloud speech API base URL (OpenAI-compatible)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_base_url: Option<String>,

    /// 云端识别密钥；建议改用 COURSE2MD_ASR_API_KEY 环境变量，避免留在命令历史 / Cloud API key; prefer COURSE2MD_ASR_API_KEY to keep it out of shell history
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_key: Option<String>,

    /// 云端识别模型名称 / Cloud speech model name
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_model: Option<String>,

    /// 云端请求方式 / Cloud request mode: transcriptions (default) or chat (audio-capable models)
    #[arg(long, value_enum)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_mode: Option<crate::settings::AsrApiMode>,

    /// 文字来源：auto 优先字幕，subtitle 仅字幕，asr 语音识别 / Transcript source: auto, subtitle, asr
    #[arg(long, value_enum)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub transcript_source: Option<crate::config::TranscriptSource>,

    /// 放到 GPU 上运行的层数（0–99，默认全部）；核显不稳定时调低 / Layers run on the GPU (0–99, all by default); lower on unstable integrated GPUs
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=99))]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub gpu_layers: Option<u32>,

    /// 音频编码部分也用 GPU（默认）/ Also run the audio encoder on the GPU (default)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub mmproj_offload: bool,

    /// 音频编码部分改用 CPU（显存不足时）/ Run the audio encoder on the CPU (when GPU memory is low)
    #[arg(long, conflicts_with = "mmproj_offload")]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub no_mmproj_offload: bool,

    /// 每段语音最长秒数 (1–600) / Maximum speech segment length in seconds
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub max_speech: Option<f32>,

    /// 输出格式，逗号分隔 / Output formats, comma-separated: md,html,json
    #[arg(long, value_delimiter = ',', value_enum)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub formats: Option<Vec<crate::config::OutputFormat>>,

    /// 本地模型目录（默认使用系统缓存）/ Local model directory (default: platform cache)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub model_dir: Option<PathBuf>,

    /// 保留下载的视频 / Keep the downloaded video
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub keep_video: bool,

    /// 成功后删除本次下载的视频 / Delete newly downloaded video after success
    #[arg(long, conflicts_with = "keep_video")]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub no_keep_video: bool,

    /// 不重新下载，使用上次下载的视频 / Reuse the previously downloaded video
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub no_download: bool,

    /// 本次启用 AI 校对 / Enable AI proofreading for this run
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm: bool,

    /// 本次禁用 AI 校对 / Disable AI proofreading for this run
    #[arg(long, conflicts_with = "llm")]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_llm: bool,

    /// AI 校对服务地址 / AI proofreading base URL (OpenAI-compatible)
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_base_url: Option<String>,

    /// AI 校对密钥（建议用 llm setup 设置，避免留在命令历史）/ AI proofreading key (prefer llm setup to keep it out of shell history)
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_api_key: Option<String>,

    /// AI 校对模型名称 / AI proofreading model name
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_model: Option<String>,

    /// AI 校对提示词 / AI proofreading instructions
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_prompt: Option<String>,

    /// 附截图辅助校对（需要视觉模型）/ Attach slides for proofreading (requires image support)
    #[arg(long, conflicts_with = "no_llm_vision")]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_vision: bool,

    /// 本次禁用截图辅助校对 / Disable image-assisted proofreading
    #[arg(long, conflicts_with = "llm_vision")]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_llm_vision: bool,

    /// 隐藏完成后的 AI 校对提示 / Hide the optional AI proofreading tip
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_llm_hint: bool,

    /// 本次生成视频摘要并写入笔记 / Generate a video summary and add it to the notes
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub summarize: bool,

    /// 本次禁用视频摘要 / Disable video summary for this run
    #[arg(long, conflicts_with = "summarize")]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_summarize: bool,

    /// 双语笔记：把文字翻译为该语言（zh-Hans、zh-Hant、en、ja、ko、fr、de、es） / Bilingual notes: translate the text into this language (zh-Hans, zh-Hant, en, ja, ko, fr, de, es)
    #[arg(long, value_name = "LANG", conflicts_with = "no_translate", value_parser = parse_translation_target)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub translate: Option<String>,

    /// 本次不翻译 / Do not translate this run
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_translate: bool,

    /// 显示诊断日志；-vv 显示调试细节 / Diagnostic logs; -vv for debug details
    #[arg(short, long, action = clap::ArgAction::Count)]
    #[arg(help_heading = "终端输出 / Terminal output")]
    pub verbose: u8,

    /// 继续上次中断的识别（默认开启）/ Continue interrupted transcription (on by default)
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub resume: bool,

    /// 从头重新识别 / Transcribe again from the start
    #[arg(long, conflicts_with = "resume")]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub no_resume: bool,

    /// 静默模式，仅报告错误 / Quiet mode: report errors only
    #[arg(short, long)]
    #[arg(help_heading = "终端输出 / Terminal output")]
    pub quiet: bool,

    /// 以 NDJSON 输出进度，禁用交互 / Stream NDJSON progress without prompts
    #[arg(long)]
    #[arg(help_heading = "终端输出 / Terminal output")]
    pub json: bool,
}

/// Replaces the configuration with the commented template (`config init`).
pub fn write_template() -> Result<PathBuf> {
    crate::settings::write_config(template().as_bytes())
}

/// `config init` 写入的带注释模板。`{field}` 占位符由内置默认值渲染（JSON 字面量也是
/// 合法的 TOML），注释不会与实际默认脱节。
pub fn template() -> String {
    let resolved = resolve(String::new(), &Default::default(), &Default::default());
    let mut defaults = resolved
        .and_then(|cfg| Ok(serde_json::to_value(cfg)?))
        .unwrap_or_default();
    let provider = crate::asr::default_provider_hint();
    defaults["asr_model"] = config::default_model(provider).into();
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
/// 默认值经 [`resolve`] 得出，与实际转换使用同一套解析。
pub fn print_effective(cfg: &settings::ConfigFile) -> Result<()> {
    let d = &cfg.defaults;
    let r = resolve(String::new(), &RunOpts::default(), cfg)?;
    println!(
        "配置文件 / Configuration: {}",
        settings::config_path().display()
    );
    println_tr!(
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
            tr!("{}（自动选择）", r.provider)
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
            tr!("(已设置，隐藏 / set, hidden)")
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

fn parse_translation_target(value: &str) -> Result<String, String> {
    crate::language::target(value)
        .map(str::to_owned)
        .ok_or_else(|| {
            let codes: Vec<_> = crate::language::TARGETS
                .iter()
                .map(|(code, _)| *code)
                .collect();
            tr!("支持的语言：{}", codes.join(", "))
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn offload_settings_reach_shared_pipeline_and_cli_overrides_config() {
        let mut file = crate::settings::ConfigFile::default();
        file.defaults.gpu_layers = Some(8);
        file.defaults.mmproj_offload = Some(false);
        let cfg = super::resolve("lecture.mp4".into(), &RunOpts::default(), &file).unwrap();
        assert_eq!(cfg.gpu_layers, 8);
        assert!(!cfg.mmproj_offload);
        let opts = RunOpts {
            gpu_layers: Some(4),
            mmproj_offload: true,
            ..Default::default()
        };
        let cfg = super::resolve("lecture.mp4".into(), &opts, &file).unwrap();
        assert_eq!(cfg.gpu_layers, 4);
        assert!(cfg.mmproj_offload);
    }

    use super::*;

    #[test]
    fn a_qwen3_default_is_not_run_on_the_npu() {
        let mut file = settings::ConfigFile::default();
        file.defaults.asr_model = Some("qwen3-1.7b".into());
        let npu = RunOpts {
            provider: Some(config::AsrProvider::Npu),
            ..Default::default()
        };
        let cfg = resolve("video".into(), &npu, &file).unwrap();
        assert_eq!(cfg.asr_model, None);
        cfg.validate_asr_with_auth(true, true).unwrap();
        // Another backend keeps it, and an explicit --asr-model stays as given.
        let cpu = RunOpts {
            provider: Some(config::AsrProvider::Cpu),
            ..Default::default()
        };
        let cfg = resolve("video".into(), &cpu, &file).unwrap();
        assert_eq!(cfg.asr_model.as_deref(), Some("qwen3-1.7b"));
        let explicit = RunOpts {
            asr_model: Some("qwen3-1.7b".into()),
            ..npu
        };
        let cfg = resolve("video".into(), &explicit, &file).unwrap();
        assert_eq!(cfg.asr_model.as_deref(), Some("qwen3-1.7b"));
        assert!(cfg.validate_asr_with_auth(true, true).is_err());
    }

    #[test]
    fn explicit_false_overrides_config_and_invalid_values_are_not_clamped() {
        let mut file = settings::ConfigFile::default();
        file.defaults.keep_video = Some(true);
        file.defaults.resume = Some(true);
        let opts = RunOpts {
            no_keep_video: true,
            no_resume: true,
            stable_secs: Some(-1.0),
            ..Default::default()
        };
        let cfg = resolve("video".into(), &opts, &file).unwrap();
        assert!(!cfg.keep_video && !cfg.resume);
        assert_eq!(cfg.stable_secs, -1.0);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn subtitle_configuration_does_not_require_api_credentials() {
        let opts = RunOpts {
            provider: Some(config::AsrProvider::Api),
            transcript_source: Some(config::TranscriptSource::Subtitle),
            ..Default::default()
        };
        resolve("video".into(), &opts, &Default::default())
            .unwrap()
            .validate()
            .unwrap();
    }

    #[test]
    fn cli_summarize_flag_overrides_config() {
        let mut file = settings::ConfigFile::default();
        file.llm.summarize = false;
        let opts = RunOpts {
            summarize: true,
            ..Default::default()
        };
        let cfg = resolve("video".into(), &opts, &file).unwrap();
        assert!(cfg.llm.summarize);

        let mut file = settings::ConfigFile::default();
        file.llm.summarize = true;
        let opts = RunOpts {
            no_summarize: true,
            ..Default::default()
        };
        let cfg = resolve("video".into(), &opts, &file).unwrap();
        assert!(!cfg.llm.summarize);
    }

    #[test]
    fn template_paths_are_escaped_toml_strings() {
        let quoted = toml::Value::String(r"C:\Users\me\models".into()).to_string();
        let parsed: toml::Table = toml::from_str(&format!("model_dir = {quoted}")).unwrap();
        assert_eq!(parsed["model_dir"].as_str(), Some(r"C:\Users\me\models"));
    }

    #[test]
    fn template_parses_to_built_in_defaults_when_uncommented() {
        let text: String = template()
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
        let file: settings::ConfigFile = toml::from_str(&text).unwrap();
        let from_template = resolve(String::new(), &Default::default(), &file).unwrap();
        let built_in = resolve(String::new(), &Default::default(), &Default::default()).unwrap();
        assert_eq!(from_template.resume, built_in.resume);
        assert_eq!(from_template.stable_secs, built_in.stable_secs);
        assert_eq!(from_template.similarity, built_in.similarity);
        assert_eq!(from_template.cooldown, built_in.cooldown);
        assert_eq!(from_template.max_speech, built_in.max_speech);
        assert_eq!(from_template.gpu_layers, built_in.gpu_layers);
        assert_eq!(from_template.formats, built_in.formats);
        assert_eq!(file.asr_api, settings::AsrApi::default());
    }
}
