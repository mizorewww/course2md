//! CLI 与桌面端共享的配置合并规则。
use crate::cli::RunOpts;
use crate::{config, llm, settings};

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
    let mut cfg = config::PipelineConfig {
        url: source,
        out_root,
        similarity: opts
            .similarity
            .or(d.similarity)
            .unwrap_or(config::DEFAULT_SIMILARITY),
        sample_interval: opts
            .sample_interval
            .or(d.sample_interval)
            .unwrap_or(config::DEFAULT_SAMPLE_INTERVAL),
        cooldown: opts
            .cooldown
            .or(d.cooldown)
            .unwrap_or(config::DEFAULT_COOLDOWN),
        max_height: opts
            .max_height
            .or(d.max_height)
            .unwrap_or(config::DEFAULT_MAX_HEIGHT),
        slide_mode: opts
            .slide_mode
            .or(d.slide_mode)
            .unwrap_or(SlideMode::Stable),
        stable_secs: opts
            .stable_secs
            .or(d.stable_secs)
            .unwrap_or(config::DEFAULT_STABLE_SECS),
        roi: match &opts.roi {
            Some(s) => Some(config::Roi::parse(s)?),
            None => match &d.roi {
                Some(s) => Some(config::Roi::parse(s)?),
                None => None,
            },
        },
        threads: opts
            .threads
            .or(d.threads)
            .unwrap_or(config::DEFAULT_THREADS),
        provider: opts
            .provider
            .or(d.provider)
            .unwrap_or_else(config::default_provider_hint),
        max_speech: opts
            .max_speech
            .or(d.max_speech)
            .unwrap_or(config::DEFAULT_MAX_SPEECH),
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
        gpu_layers: opts
            .gpu_layers
            .or(d.gpu_layers)
            .unwrap_or(config::DEFAULT_GPU_LAYERS),
        mmproj_offload: config::resolve_flag(
            opts.mmproj_offload,
            opts.no_mmproj_offload,
            d.mmproj_offload,
            true,
        ),
        transcript_source: opts
            .transcript_source
            .or(d.transcript_source)
            .unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    #[test]
    fn offload_settings_reach_shared_pipeline_and_cli_overrides_config() {
        let mut file = crate::settings::ConfigFile::default();
        file.defaults.gpu_layers = Some(8);
        file.defaults.mmproj_offload = Some(false);
        let cfg =
            super::resolve("lecture.mp4".into(), &crate::cli::RunOpts::default(), &file).unwrap();
        assert_eq!(cfg.gpu_layers, 8);
        assert!(!cfg.mmproj_offload);
        let opts = crate::cli::RunOpts {
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
}
