//! 首次使用向导：无配置文件且处于交互终端时，引导选择语音转写方式并写盘。
//!
//! 触发条件见 [`is_first_run`]；非交互/CI 环境不触发，走既有默认逻辑
//!（provider 回落 `config::default_provider_hint()`）。
//! 交互风格与 llm.rs 的 setup_interactive 一致：dialoguer Select/Input +
//! `interact_opt` 回落（Esc 视为选择默认项）。

use crate::config::AsrProvider;
use anyhow::Result;
use std::io::IsTerminal as _;

/// 是否首次运行：配置文件不存在 && 用户没显式传 --provider && stdin 是终端。
pub fn is_first_run(opts_provider_is_none: bool) -> bool {
    opts_provider_is_none
        && !crate::settings::config_path().is_file()
        && std::io::stdin().is_terminal()
}

/// 满足首次运行条件时执行向导并返回写盘后的新配置；否则原样返回。
pub fn maybe_run(
    opts: &crate::cli::RunOpts,
    file: &crate::settings::ConfigFile,
) -> Result<crate::settings::ConfigFile> {
    if !is_first_run(opts.provider.is_none()) {
        return Ok(file.clone());
    }
    run_wizard(file.clone())
}

/// Esc / 非交互回落：dialoguer 返回 None 时取默认项（与 llm setup 同款容错）。
fn select(prompt: &str, items: &[String], default: usize) -> Result<usize> {
    let idx = dialoguer::Select::new()
        .with_prompt(prompt)
        .items(items)
        .default(default)
        .interact_opt()?
        .unwrap_or(default);
    Ok(idx)
}

fn run_wizard(mut cfg: crate::settings::ConfigFile) -> Result<crate::settings::ConfigFile> {
    println!("欢迎使用 course2md！首次使用，先配置语音转写方式。");
    println!("{}", crate::auth::BILIBILI_SETUP_TIP);
    println!();

    let top = select(
        "语音转写方式",
        &[
            "本地识别（推荐：离线、免费、隐私；首次需下载模型）".to_string(),
            "云端 API（免下载模型；需 OpenAI 兼容端点 API key，按量计费）".to_string(),
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
                println!("coreml 首次识别时会自动下载约 1-2.3GB 模型（届时可交互选择模型）。");
                provider = Some(AsrProvider::Coreml);
            }
            LocalPick::Provider(p @ (AsrProvider::Gpu | AsrProvider::Cpu)) => {
                let root = crate::config::model_dir_from(cfg.defaults.model_dir.as_deref());
                let dl = select(
                    &format!(
                        "首次使用需下载识别模型（约 2.4GB）到 {}",
                        root.display()
                    ),
                    &[
                        "现在下载".to_string(),
                        "暂不下载，改用云端 API".to_string(),
                        "退出，稍后手动下载（course2md models download）".to_string(),
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
                        println!("稍后运行 course2md models download 下载模型后即可使用。");
                        std::process::exit(0);
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
    println!("✓ 已保存：{}", path.display());
    println!("以后可用 --provider 临时切换，或直接编辑配置文件。");
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
    cands.push(AsrProvider::Cpu); // 兜底总是可用

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
            let tag = if p == rec { "（推荐）" } else { "" };
            format!("{}{tag}", provider_label(p))
        })
        .collect();
    items.push("← 返回改用云端 API".to_string());

    let idx = select("本地识别后端", &items, 0)?;
    Ok(match cands.get(idx) {
        Some(&p) => LocalPick::Provider(p),
        None => LocalPick::Cloud,
    })
}

fn provider_label(p: AsrProvider) -> &'static str {
    match p {
        AsrProvider::Coreml => "coreml — Apple 原生（CoreML / Neural Engine，零外部依赖）",
        AsrProvider::Gpu => "gpu — llama.cpp GPU（Metal / CUDA / Vulkan）",
        AsrProvider::Npu => "npu — Intel NPU（OpenVINO 加速）",
        AsrProvider::Cpu => "cpu — llama.cpp 纯 CPU（通用兜底，速度较慢）",
        AsrProvider::Api => "api — 云端 STT",
    }
}

/// 云端分支：base_url / api_key / model 三项，缺省值与 settings::AsrApi 默认对齐。
fn configure_cloud(cfg: &mut crate::settings::ConfigFile) -> Result<()> {
    cfg.asr_api.base_url = dialoguer::Input::new()
        .with_prompt("Base URL（OpenAI 兼容端点）")
        .default("https://openrouter.ai/api/v1".to_string())
        .interact_text()?;
    cfg.asr_api.api_key = dialoguer::Input::<String>::new()
        .with_prompt("API Key（可留空，稍后用环境变量 COURSE2MD_ASR_API_KEY 提供）")
        .allow_empty(true)
        .interact_text()?;
    cfg.asr_api.model = dialoguer::Input::new()
        .with_prompt("模型名")
        .default("qwen/qwen3-asr-flash-2026-02-10".to_string())
        .interact_text()?;
    Ok(())
}
