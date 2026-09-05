//! CLI 与桌面端共享的配置合并规则。
use crate::{config, llm, settings};
use crate::cli::RunOpts;

/// 配置文件 + CLI 覆盖 -> 生效 LLM 设置。
fn resolve_llm(opts: &RunOpts, file: &settings::ConfigFile) -> llm::LlmSettings {
    let mut s = file.llm.clone();
    if opts.no_llm {
        s.enabled = false;
    } else if opts.llm {
        s.enabled = true;
    }
    if opts.no_llm_vision {
        s.vision = false;
    } else if opts.llm_vision {
        s.vision = true;
    }
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
    s
}

/// 优先级：CLI 显式参数 > 配置文件 [defaults] > 内置默认。
pub fn resolve(
    source: String,
    opts: &RunOpts,
    file: &settings::ConfigFile,
) -> anyhow::Result<config::PipelineConfig> {
    let d = &file.defaults;
    use config::{OutputFormat, SlideMode};
    let out_root = opts
        .out
        .clone()
        .or_else(|| d.out.clone())
        .map(config::expand_tilde)
        .unwrap_or_else(|| config::DEFAULT_OUT_DIR.into());
    Ok(config::PipelineConfig {
        url: source,
        // out_dir 此处只是初值；pipeline 里会改写为 {out_root}/{platform}/{title}/{id}/
        out_dir: out_root.clone(),
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
            .unwrap_or_else(|| vec![OutputFormat::Md, OutputFormat::Html]),
        model_dir: config::model_dir_from(opts.model_dir.as_deref().or(d.model_dir.as_deref())),
        keep_video: !opts.no_keep_video && (opts.keep_video || d.keep_video.unwrap_or(false)),
        no_download: opts.no_download || d.no_download.unwrap_or(false),
        resume: config::resolve_resume(opts.resume, opts.no_resume, d.resume),
        llm: resolve_llm(opts, file),
        asr_api: resolve_asr_api(opts, file),
        asr_model: opts.asr_model.clone().or_else(|| d.asr_model.clone()),
        transcript_source: opts
            .transcript_source
            .or(d.transcript_source)
            .unwrap_or_default(),
    })
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
    use super::*;

    #[test]
    fn explicit_false_overrides_config_and_invalid_values_are_not_clamped() {
        let mut file = settings::ConfigFile::default();
        file.defaults.keep_video = Some(true);
        file.defaults.resume = Some(true);
        let opts = RunOpts { no_keep_video: true, no_resume: true, stable_secs: Some(-1.0), ..Default::default() };
        let cfg = resolve("video".into(), &opts, &file).unwrap();
        assert!(!cfg.keep_video && !cfg.resume);
        assert_eq!(cfg.stable_secs, -1.0);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn subtitle_configuration_does_not_require_api_credentials() {
        let opts = RunOpts { provider: Some(config::AsrProvider::Api), transcript_source: Some(config::TranscriptSource::Subtitle), ..Default::default() };
        resolve("video".into(), &opts, &Default::default()).unwrap().validate().unwrap();
    }
}
