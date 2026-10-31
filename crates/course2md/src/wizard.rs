//! 首次使用向导：没有可用的转写配置且处于交互终端时，引导选择语音转写方式并写盘。
//! 非交互/CI 环境不触发（provider 回落 `config::default_provider_hint()`）；
//! Esc 取消，不写配置也不开始下载。

use crate::config::AsrProvider;
use crate::{println_tr, tr};
use anyhow::Result;

/// 是否首次运行：用户没显式传 --provider && 交互终端 &&
///（配置文件不存在，或配置存在但未设置 provider——例如只运行过 config init）。
pub fn is_first_run(opts_provider_is_none: bool, file: &crate::settings::ConfigFile) -> bool {
    opts_provider_is_none && crate::runtime::interactive() && file.defaults.provider.is_none()
}

/// 满足首次运行条件时执行向导并返回写盘后的新配置；否则原样返回。
/// 用户选择「退出，稍后下载」：向导正常结束，不带错误码。由 main 识别并干净退出。
#[derive(Debug, thiserror::Error)]
#[error("{}", tr!("已退出首次使用向导 / Exited the first-run wizard"))]
pub struct ExitWizard;

/// 调用方已排除 --json/--quiet（stdout 必须保持纯输出，不能混进交互提示）。
pub fn maybe_run(
    opts: &crate::cli::RunOpts,
    file: &crate::settings::ConfigFile,
) -> Result<crate::settings::ConfigFile> {
    if matches!(
        opts.transcript_source.or(file.defaults.transcript_source),
        Some(crate::config::TranscriptSource::Subtitle)
    ) || !is_first_run(opts.provider.is_none(), file)
    {
        return Ok(file.clone());
    }
    crate::error::require_cmd("ffmpeg")?;
    crate::error::require_cmd("ffprobe")?;
    run_wizard(file.clone(), opts.model_dir.as_deref())
}

/// Esc 取消向导，不接受默认选项。
fn select(prompt: &str, items: &[String], default: usize) -> Result<usize> {
    crate::runtime::choose(
        prompt,
        items,
        default,
        tr!(
            "已取消设置，未保存配置。重新运行视频命令可再次设置。 / Setup cancelled; no configuration saved. Run your video command again to restart."
        ),
    )
}

fn run_wizard(
    mut cfg: crate::settings::ConfigFile,
    model_dir_override: Option<&std::path::Path>,
) -> Result<crate::settings::ConfigFile> {
    println_tr!(
        "欢迎使用 course2md！选择字幕不可用时的语音转写方式。 / Welcome! Choose how to transcribe when subtitles are unavailable."
    );
    println_tr!(
        "↑↓ 选择，Enter 确认，Esc 取消。 / Use ↑↓ to select, Enter to confirm, Esc to cancel."
    );
    println_tr!(
        "只想用视频自带字幕？按 Esc 退出，加 --transcript-source subtitle 重新运行。 / Subtitles only? Press Esc and rerun with --transcript-source subtitle."
    );

    let top = select(
        tr!("语音转写方式 / Transcription"),
        &[
            tr!("本地识别（首次下载模型） / Local transcription (requires a model download)").to_string(),
            tr!("云端 API（需服务端点和 API Key，可能收费） / Cloud API (endpoint and API key required; charges may apply)").to_string(),
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
                println_tr!(
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
                            tr!("现在下载 / Download now").to_string(),
                            tr!("改用云端 API / Use cloud API").to_string(),
                            tr!("退出，稍后下载 / Exit and download later").to_string(),
                        ],
                        0,
                    )?;
                    match dl {
                        0 => {
                            crate::runtime::block_on(crate::models::download_models(
                                &root,
                                &crate::dispatch::TaskControl::default(),
                            ))??;
                            provider = Some(p);
                        }
                        1 => use_cloud = true,
                        _ => {
                            println_tr!(
                                "稍后下载模型，再重新运行视频命令： / Download the model, then rerun your video command: course2md models download"
                            );
                            return Err(ExitWizard.into());
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
    println_tr!("开始处理视频… / Processing your video…");
    Ok(cfg)
}

/// 本地分支的选择结果：某个本地后端，或反悔回云端。
enum LocalPick {
    Provider(AsrProvider),
    Cloud,
}

/// 按本机能力动态生成本地后端候选：推荐项（default_provider_hint）置顶并标注。
fn pick_local_provider() -> Result<LocalPick> {
    let llama = crate::tools::locate("llama-server").is_some();
    let mut cands: Vec<AsrProvider> = vec![];
    if crate::asr::apple_native_available() {
        cands.push(AsrProvider::Coreml);
    }
    if llama {
        cands.push(AsrProvider::Gpu);
    }
    if crate::npu::device_present() {
        cands.push(AsrProvider::Npu);
    }
    if llama {
        cands.push(AsrProvider::Cpu);
    }
    if cands.is_empty() {
        println_tr!(
            "本机识别所需的组件尚未安装。可退出后运行 course2md doctor 查看，或改用云端识别。 / Local transcription is unavailable. Exit and run course2md doctor, or use a cloud service."
        );
        return select(
            tr!("下一步 / Next step"),
            &[
                tr!("配置云端 API / Configure cloud API").into(),
                tr!("退出 / Exit").into(),
            ],
            0,
        )
        .and_then(|i| {
            if i == 0 {
                Ok(LocalPick::Cloud)
            } else {
                Err(ExitWizard.into())
            }
        });
    }

    // 推荐项置顶；推荐项不在候选里时（如无 llama-server 却推荐 gpu）退到首个候选
    let mut rec = crate::asr::default_provider_hint();
    if !cands.contains(&rec) {
        rec = cands[0];
    }
    cands.retain(|&p| p != rec);
    cands.insert(0, rec);

    let mut items: Vec<String> = cands
        .iter()
        .map(|&p| {
            let tag = if p == rec {
                tr!("（推荐 / recommended）")
            } else {
                ""
            };
            let (zh, en) = p.label();
            format!("{zh}{tag} / {en}")
        })
        .collect();
    items.push(tr!("改用云端 API / Use cloud API").to_string());

    let idx = select(tr!("本地识别方式 / Local transcription engine"), &items, 0)?;
    Ok(match cands.get(idx) {
        Some(&p) => LocalPick::Provider(p),
        None => LocalPick::Cloud,
    })
}

/// 云端分支：base_url / api_key / model 三项，缺省值直接取自 settings::AsrApi::default()。
fn configure_cloud(cfg: &mut crate::settings::ConfigFile) -> Result<()> {
    let defaults = crate::settings::AsrApi::default();
    cfg.asr_api.base_url = dialoguer::Input::new()
        .with_prompt(tr!(
            "服务地址（兼容 OpenAI 接口）/ Service URL (OpenAI-compatible)"
        ))
        .default(defaults.base_url)
        .interact_text()?;
    cfg.asr_api.api_key = dialoguer::Password::new()
        .with_prompt(tr!("API 密钥（输入不显示）/ API key (hidden)"))
        .allow_empty_password(true)
        .interact()?;
    cfg.asr_api.model = dialoguer::Input::new()
        .with_prompt(tr!("模型名 / Model name"))
        .default(defaults.model)
        .interact_text()?;
    cfg.asr_api.base_url = cfg.asr_api.base_url.trim().to_string();
    cfg.asr_api.model = cfg.asr_api.model.trim().to_string();
    if !cfg.asr_api.base_url.contains("://") {
        cfg.asr_api.base_url = format!("https://{}", cfg.asr_api.base_url);
    }
    crate::config::ensure_http_url(&cfg.asr_api.base_url)?;
    anyhow::ensure!(
        !cfg.asr_api.api_key.trim().is_empty() || crate::config::asr_api_key_from_env().is_some(),
        tr!(
            "未提供 API 密钥。请在向导中输入，或设置环境变量 COURSE2MD_ASR_API_KEY 后重试。 / No API key provided. Enter one in setup, or set COURSE2MD_ASR_API_KEY and retry."
        )
    );
    Ok(())
}
