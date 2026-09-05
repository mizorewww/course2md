//! 环境体检：一次性报告依赖工具、平台后端可用性、配置与模型缓存状态。
//! 用户报 issue 时附上 `course2md doctor` 输出即可定位大多数环境问题。

use crate::config::AsrProvider;
use anyhow::Result;
use std::path::Path;
use std::process::Command;

/// 版本首行展示的最大字符数（防止某些工具输出超长首行撑爆版面）
const VERSION_LINE_MAX_CHARS: usize = 72;

fn tool_version(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Some(
        s.lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(VERSION_LINE_MAX_CHARS)
            .collect(),
    )
}

fn check(line: &mut Vec<String>, ok: bool, name: &str, detail: &str) {
    let mark = if ok { "✓" } else { "✗" };
    line.push(format!("{mark} {name}{detail}"));
}

pub fn run() -> Result<()> {
    let mut out = vec![format!(
        "course2md {} ({}/{})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )];
    let mut failures = 0;
    out.push("\n必需工具 / Required tools".into());
    for tool in ["ffmpeg", "ffprobe"] {
        match tool_version(tool, &["-version", "-hide_banner"]) {
            Some(version) => check(&mut out, true, tool, &format!("  {version}")),
            None => {
                failures += 1;
                check(
                    &mut out,
                    false,
                    tool,
                    "  缺失 / Missing: brew install ffmpeg (macOS); sudo apt install ffmpeg (Debian/Ubuntu)",
                );
            }
        }
    }
    out.push("\n按用途选装 / Optional tools by use".into());
    match tool_version("yt-dlp", &["--version"]) {
        Some(version) => check(&mut out, true, "yt-dlp", &format!("  {version}")),
        None => out.push("- yt-dlp  远程视频需要，本地文件不需要 / Required for URLs, optional for local files: brew install yt-dlp (macOS); pipx install yt-dlp".into()),
    }
    #[cfg(apple_native)]
    {
        let exe = std::env::current_exe().unwrap_or_default();
        let dir = exe.parent().unwrap_or(Path::new("."));
        let shaders = ["mlx.metallib", "default.metallib"]
            .iter()
            .any(|name| dir.join(name).is_file());
        if shaders {
            check(
                &mut out,
                true,
                "coreml",
                "  Apple 原生后端可用 / Apple native backend available",
            );
        } else {
            out.push("! coreml  缺少 Metal 着色器资源，请重新安装 / Metal shader resources missing; reinstall course2md".into());
        }
    }
    #[cfg(not(apple_native))]
    out.push("- coreml  本构建不包含 Apple 原生后端 / Apple native backend is not included in this build".into());
    match crate::runtime::which("llama-server") {
        Some(path) => check(&mut out, true, "gpu/cpu", &format!("  {}", path.display())),
        None => out.push("- gpu/cpu  缺少 llama-server / llama-server missing. 安装 llama.cpp 后重试 / Install llama.cpp to use these backends.".into()),
    }
    if cfg!(target_os = "linux") {
        let device = Path::new("/dev/accel/accel0").exists();
        out.push(if device {
            "✓ npu  检测到 Intel NPU；仍需 OpenVINO 运行环境 / Intel NPU detected; OpenVINO runtime also required"
        } else {
            "- npu  未检测到 Intel NPU 设备 / No Intel NPU device detected"
        }.into());
        if crate::runtime::which("uv").is_none()
            && crate::runtime::which("python3").is_none()
            && crate::runtime::which("python").is_none()
        {
            out.push(
                "- npu  缺少 Python/uv / Python or uv is required for NPU transcription".into(),
            );
        }
    }
    out.push("\n配置与模型 / Configuration and models".into());
    let path = crate::settings::config_path();
    let loaded = crate::settings::load();
    match &loaded {
        Ok(cfg) => {
            if path.is_file() {
                check(&mut out, true, "config", &format!("  {}", path.display()));
            } else {
                out.push(format!(
                    "- config  使用内置默认值 / Using built-in defaults: {}",
                    path.display()
                ));
                out.push("  可选配置 / Optional setup: course2md config init".into());
            }
            // Validate the same merged defaults as conversion, without requiring a source or network.
            match crate::options::resolve(String::new(), &crate::cli::RunOpts::default(), cfg)
                .and_then(|cfg| cfg.validate())
            {
                Ok(()) => {}
                Err(error) => {
                    failures += 1;
                    check(&mut out, false, "settings", &format!("  {error:#}"));
                }
            }
            let root = crate::config::model_dir_from(cfg.defaults.model_dir.as_deref());
            if crate::models::llama_ready(&root) {
                check(
                    &mut out,
                    true,
                    "gpu/cpu models",
                    &format!("  {}", root.display()),
                );
            } else {
                out.push(format!(
                    "- gpu/cpu models  未下载或不完整 / Missing or incomplete: {}",
                    root.display()
                ));
                out.push(format!("  仅 gpu/cpu 需要；下载约 2.4GB / Only needed for gpu/cpu (~2.4GB): course2md models download --dir {:?}", root));
            }
            let key = !cfg.asr_api.api_key.trim().is_empty()
                || crate::config::asr_api_key_from_env().is_some();
            out.push(if key { "✓ api  已设置密钥（隐藏）/ API key set (hidden)" } else { "- api  未设置密钥；仅云端识别需要 / API key not set; only required for cloud transcription: COURSE2MD_ASR_API_KEY" }.into());
            let provider = cfg
                .defaults
                .provider
                .unwrap_or_else(crate::config::default_provider_hint);
            let label = match provider {
                AsrProvider::Coreml => "Apple native",
                AsrProvider::Gpu => "llama.cpp GPU",
                AsrProvider::Cpu => "llama.cpp CPU",
                AsrProvider::Npu => "Intel NPU / OpenVINO",
                AsrProvider::Api => "Cloud speech API",
            };
            out.push(format!(
                "\n当前识别后端 / Configured speech backend: {provider} ({label})"
            ));
            out.push("字幕可用时无需语音识别；可用 --provider 覆盖后端 / Subtitles do not require speech recognition; override the backend with --provider.".into());
        }
        Err(error) => {
            failures += 1;
            check(&mut out, false, "config", &format!("  {error:#}"));
        }
    }
    out.push("\n✓ 可用 / available · - 可选或未设置 / optional or unset · ! 需要检查 / needs attention · ✗ 必须修复 / must fix".into());
    for line in out {
        println!("{line}");
    }
    anyhow::ensure!(
        failures == 0,
        "发现 {failures} 项必需依赖或配置问题；请按上方提示修复 / Fix the {failures} required-tool or configuration issues listed above"
    );
    println!(
        "基础检查通过；可选服务未进行联网测试 / Basic checks passed; optional services were not tested online."
    );
    Ok(())
}
