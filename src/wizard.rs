//! 首次使用向导：无配置文件且处于交互终端时，引导选择语音转写方式并写盘。
//!
//! 触发条件见 [`is_first_run`]；非交互/CI 环境不触发，走既有默认逻辑
//!（provider 回落 `config::default_provider_hint()`）。
//! 交互风格与 llm.rs 的 setup_interactive 一致：dialoguer Select/Input +
//! `interact_opt` 取消（Esc 退出，不写配置或开始下载）。

use crate::config::AsrProvider;
use anyhow::Result;
use std::io::IsTerminal as _;

/// 是否首次运行：配置文件不存在 && 用户没显式传 --provider && stdin 是终端。
pub fn is_first_run(opts_provider_is_none: bool) -> bool {
    opts_provider_is_none
        && !crate::settings::config_path().is_file()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
}

/// 满足首次运行条件时执行向导并返回写盘后的新配置；否则原样返回。
pub fn maybe_run(
    opts: &crate::cli::RunOpts,
    file: &crate::settings::ConfigFile,
) -> Result<crate::settings::ConfigFile> {
    if opts.quiet
        || opts.json
        || matches!(
            opts.transcript_source.or(file.defaults.transcript_source),
            Some(crate::config::TranscriptSource::Subtitle)
        )
        || !is_first_run(opts.provider.is_none())
    {
        return Ok(file.clone());
    }
    crate::error::require_cmd("ffmpeg")?;
    crate::error::require_cmd("ffprobe")?;
    run_wizard(file.clone(), opts.model_dir.as_deref())
}

/// Esc 取消向导，不接受默认选项。
fn select(prompt: &str, items: &[String], default: usize) -> Result<usize> {
    selected_or_cancelled(
        dialoguer::Select::new()
            .with_prompt(prompt)
            .items(items)
            .default(default)
            .interact_opt()?,
    )
}

fn selected_or_cancelled(choice: Option<usize>) -> Result<usize> {
    choice.ok_or_else(|| anyhow::anyhow!("已取消设置，未保存配置。重新运行视频命令可再次设置。 / Setup cancelled; no configuration saved. Run your video command again to restart."))
}

fn run_wizard(
    mut cfg: crate::settings::ConfigFile,
    model_dir_override: Option<&std::path::Path>,
) -> Result<crate::settings::ConfigFile> {
    println!(
        "欢迎使用 course2md！选择字幕不可用时的语音转写方式。 / Welcome! Choose how to transcribe when subtitles are unavailable."
    );
    println!();

    println!(
        "↑↓ 选择，Enter 确认，Esc 取消。 / Use ↑↓ to select, Enter to confirm, Esc to cancel."
    );
    println!(
        "仅使用字幕可加 --transcript-source subtitle，无需下载识别模型。 / For subtitles only, use --transcript-source subtitle; no speech model is needed."
    );

    let top = select(
        "语音转写方式 / Transcription",
        &[
            "本地识别（首次下载模型） / Local transcription (requires a model download)".to_string(),
            "云端 API（需服务端点和 API Key，可能收费） / Cloud API (endpoint and API key required; charges may apply)".to_string(),
        ],
        0,
    )?;
    // use_cloud：顶层直选云端，或本地分支里反悔（「← 返回改用云端」/ 暂不下载）
    let mut use_cloud = top == 1;
    let mut provider: Option<AsrProvider> = None;

    if !use_cloud {
        match pick_local_provider()? {
            LocalPick::Provider(AsrProvider::Coreml) => {
                // 不预下载：Swift 侧首次识别时才下载；模型三选一沿用首次识别时的交互
                println!(
                    "首次识别时选择并下载 Apple 模型（约 1–2.3 GB）。 / Choose and download an Apple model at first transcription (about 1–2.3 GB)."
                );
                provider = Some(AsrProvider::Coreml);
            }
            LocalPick::Provider(p @ (AsrProvider::Gpu | AsrProvider::Cpu)) => {
                let root = crate::config::model_dir_from(
                    model_dir_override.or(cfg.defaults.model_dir.as_deref()),
                );
                if crate::models::llama_ready(&root) {
                    println!(
                        "使用已下载模型 / Using downloaded models: {}",
                        root.display()
                    );
                    provider = Some(p);
                } else {
                    let dl = select(
                        &format!(
                            "下载识别模型（约 2.4 GB） / Download speech model (about 2.4 GB): {}",
                            root.display()
                        ),
                        &[
                            "现在下载 / Download now".to_string(),
                            "改用云端 API / Use cloud API".to_string(),
                            "退出，稍后下载 / Exit and download later".to_string(),
                        ],
                        0,
                    )?;
                    match dl {
                        0 => {
                            let rt = tokio::runtime::Runtime::new()?;
                            rt.block_on(crate::models::download_models(&root))?;
                            provider = Some(p);
                        }
                        1 => use_cloud = true,
                        _ => {
                            println!(
                                "稍后下载模型，再重新运行视频命令： / Download the model, then rerun your video command: course2md models download"
                            );
                            std::process::exit(0);
                        }
                    }
                }
            }
            LocalPick::Provider(p) => provider = Some(p), // npu
            LocalPick::Cloud => use_cloud = true,
        }
    }

    if use_cloud {
        configure_cloud(&mut cfg)?;
        provider = Some(AsrProvider::Api);
    }
    cfg.defaults.provider = provider;

    let path = crate::settings::save(&cfg)?;
    println!();
    println!("✓ 已保存配置 / Configuration saved: {}", path.display());
    println!(
        "现在开始处理视频。用 --provider 临时切换转写方式。 / Processing your video now. Use --provider to change transcription for a single run."
    );
    Ok(cfg)
}

/// 本地分支的选择结果：某个本地后端，或反悔回云端。
enum LocalPick {
    Provider(AsrProvider),
    Cloud,
}

/// 按本机能力动态生成本地后端候选：推荐项（default_provider_hint）置顶并标注。
fn pick_local_provider() -> Result<LocalPick> {
    let mut cands: Vec<AsrProvider> = vec![];
    if cfg!(apple_native) {
        cands.push(AsrProvider::Coreml);
    }
    if crate::runtime::which("llama-server").is_some() {
        cands.push(AsrProvider::Gpu);
    }
    if std::path::Path::new("/dev/accel/accel0").exists() {
        cands.push(AsrProvider::Npu);
    }
    if crate::runtime::which("llama-server").is_some() {
        cands.push(AsrProvider::Cpu);
    }
    if cands.is_empty() {
        println!(
            "本地识别依赖尚未安装。可先退出运行 course2md doctor，或继续配置云端 API。 / Local transcription is unavailable. Exit and run course2md doctor, or configure cloud API."
        );
        return select(
            "下一步 / Next step",
            &[
                "配置云端 API / Configure cloud API".into(),
                "退出 / Exit".into(),
            ],
            0,
        )
        .and_then(|i| {
            if i == 0 {
                Ok(LocalPick::Cloud)
            } else {
                anyhow::bail!("已取消设置，未保存配置。 / Setup cancelled; no configuration saved.")
            }
        });
    }

    // 推荐项置顶；推荐项不在候选里时（如无 llama-server 却推荐 gpu）退到首个候选
    let mut rec = crate::config::default_provider_hint();
    if !cands.contains(&rec) {
        rec = cands[0];
    }
    cands.retain(|&p| p != rec);
    cands.insert(0, rec);

    let mut items: Vec<String> = cands
        .iter()
        .map(|&p| {
            let tag = if p == rec {
                "（推荐 / recommended）"
            } else {
                ""
            };
            format!("{}{tag}", provider_label(p))
        })
        .collect();
    items.push("改用云端 API / Use cloud API".to_string());

    let idx = select("本地识别方式 / Local transcription engine", &items, 0)?;
    Ok(match cands.get(idx) {
        Some(&p) => LocalPick::Provider(p),
        None => LocalPick::Cloud,
    })
}

fn provider_label(p: AsrProvider) -> &'static str {
    match p {
        AsrProvider::Coreml => "coreml — Apple 原生识别 / Apple native transcription",
        AsrProvider::Gpu => "gpu — GPU 加速 / GPU acceleration (Metal / CUDA / Vulkan)",
        AsrProvider::Npu => "npu — Intel NPU 加速 / Intel NPU acceleration (OpenVINO)",
        AsrProvider::Cpu => "cpu — CPU 识别（速度较慢） / CPU transcription (slower)",
        AsrProvider::Api => "api — 云端语音转写 / Cloud transcription",
    }
}

/// 云端分支：base_url / api_key / model 三项，缺省值与 settings::AsrApi 默认对齐。
fn configure_cloud(cfg: &mut crate::settings::ConfigFile) -> Result<()> {
    cfg.asr_api.base_url = dialoguer::Input::new()
        .with_prompt("服务地址 / Base URL (OpenAI-compatible)")
        .default("https://openrouter.ai/api/v1".to_string())
        .interact_text()?;
    cfg.asr_api.api_key = dialoguer::Password::new()
        .with_prompt("API Key（输入隐藏；留空使用环境变量） / API key (hidden; leave blank to use COURSE2MD_ASR_API_KEY)")
        .allow_empty_password(true)
        .interact()?;
    cfg.asr_api.model = dialoguer::Input::new()
        .with_prompt("模型名 / Model name")
        .default("qwen/qwen3-asr-flash-2026-02-10".to_string())
        .interact_text()?;
    cfg.asr_api.base_url = cfg.asr_api.base_url.trim().to_string();
    cfg.asr_api.model = cfg.asr_api.model.trim().to_string();
    if !cfg.asr_api.base_url.contains("://") {
        cfg.asr_api.base_url = format!("https://{}", cfg.asr_api.base_url);
    }
    let endpoint = url::Url::parse(&cfg.asr_api.base_url)?;
    anyhow::ensure!(
        matches!(endpoint.scheme(), "http" | "https") && endpoint.host_str().is_some(),
        "请输入完整的 HTTP(S) 服务地址。 / Enter a valid HTTP(S) base URL."
    );
    anyhow::ensure!(
        !cfg.asr_api.api_key.trim().is_empty() || crate::config::asr_api_key_from_env().is_some(),
        "未提供 API Key。请设置 COURSE2MD_ASR_API_KEY 后重试，或在向导中输入。 / No API key provided. Set COURSE2MD_ASR_API_KEY or enter a key in setup."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_does_not_accept_the_default() {
        assert!(selected_or_cancelled(None).is_err());
        assert_eq!(selected_or_cancelled(Some(0)).unwrap(), 0);
    }

    #[test]
    fn explicit_subtitle_skips_asr_setup() {
        let opts = crate::cli::RunOpts {
            transcript_source: Some(crate::config::TranscriptSource::Subtitle),
            ..Default::default()
        };
        let cfg = crate::settings::ConfigFile::default();
        let result = maybe_run(&opts, &cfg).unwrap();
        assert_eq!(result.defaults.provider, cfg.defaults.provider);
    }
}
