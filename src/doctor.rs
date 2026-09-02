//! 环境体检：一次性报告依赖工具、平台后端可用性、配置与模型缓存状态。
//! 用户报 issue 时附上 `course2md doctor` 输出即可定位大多数环境问题。

use crate::config::{AsrProvider, cache_dir, config_dir};
use anyhow::Result;
use std::path::Path;

fn tool_version(cmd: &str, args: &[&str]) -> Option<String> {
    crate::runtime::probe_version(cmd, args)
}

fn check(line: &mut Vec<String>, ok: bool, name: &str, detail: &str) {
    let mark = if ok { "✓" } else { "✗" };
    line.push(format!("{mark} {name}{detail}"));
}

pub fn run() -> Result<()> {
    let mut out: Vec<String> = Vec::new();
    let platform = format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH);
    out.push(format!(
        "course2md {}  ({platform})",
        env!("CARGO_PKG_VERSION")
    ));

    // —— 核心依赖（必需）——
    for (tool, args) in [
        ("ffmpeg", &["-version", "-hide_banner"] as &[&str]),
        ("ffprobe", &["-version", "-hide_banner"]),
    ] {
        match tool_version(tool, args) {
            Some(v) => check(&mut out, true, tool, &format!("  {v}")),
            None => check(
                &mut out,
                false,
                tool,
                "  缺失（必需）：course2md setup 可自动安装；或 brew install ffmpeg / apt install ffmpeg",
            ),
        }
    }
    match tool_version("yt-dlp", &["--version"]) {
        Some(v) => check(&mut out, true, "yt-dlp", &format!("  {v}")),
        None => check(
            &mut out,
            false,
            "yt-dlp",
            "  缺失（远程视频需要）：course2md setup 可自动安装",
        ),
    }

    // —— 按平台的本地 ASR 后端 ——
    #[cfg(apple_native)]
    {
        check(
            &mut out,
            true,
            "coreml",
            "  本构建含 Apple 原生后端（CoreML/ANE + Silero VAD）",
        );
        let exe = std::env::current_exe().unwrap_or_default();
        let dir = exe.parent().unwrap_or(Path::new("."));
        let has_metallib = ["mlx.metallib", "default.metallib"]
            .iter()
            .any(|n| dir.join(n).is_file());
        if has_metallib {
            check(&mut out, true, "mlx.metallib", "  已就位（与二进制同目录）");
        } else {
            check(
                &mut out,
                false,
                "mlx.metallib",
                "  缺失：CoreML 推理会失败；重跑 install.sh 或从 native/apple-asr 复制",
            );
        }
    }
    #[cfg(not(apple_native))]
    {
        if cfg!(target_os = "macos") {
            out.push(
                "! coreml        本构建未包含 Apple 原生后端（仅 macOS arm64 原生构建支持）".into(),
            );
        }
    }

    match crate::runtime::which("llama-server") {
        Some(p) => check(
            &mut out,
            true,
            "llama-server",
            &format!("  {}", p.display()),
        ),
        None => {
            out.push(
                "! llama-server  未安装（gpu/cpu 后端需要；course2md setup 可自动安装）".into(),
            )
        }
    }

    // NPU：仅 Linux 且设备节点存在时报告
    if cfg!(target_os = "linux") {
        let npu_dev = Path::new("/dev/accel/accel0").exists();
        if npu_dev {
            check(
                &mut out,
                true,
                "npu",
                "  检测到 /dev/accel/accel0（Intel AI Boost）",
            );
        }
        let py = crate::runtime::which("python3").or_else(|| crate::runtime::which("python"));
        match (crate::runtime::which("uv"), py) {
            (Some(p), _) => check(&mut out, true, "uv", &format!("  {}", p.display())),
            (None, Some(p)) => out.push(format!(
                "! uv            未安装（NPU 后端推荐）；将回退 {}",
                p.display()
            )),
            (None, None) => out.push("✗ python/uv     均缺失（npu 后端需要）".into()),
        }
    }

    // —— 配置与模型缓存 ——
    let cfg_path = config_dir().join("config.toml");
    if cfg_path.is_file() {
        match crate::settings::load() {
            Ok(c) => {
                let perms_note = {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = std::fs::metadata(&cfg_path)
                            .map(|m| m.permissions().mode())
                            .unwrap_or(0);
                        if mode & 0o077 != 0 {
                            format!("（权限 {:o}，建议 0600：含 API key）", mode & 0o777)
                        } else {
                            String::new()
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        String::new()
                    }
                };
                check(
                    &mut out,
                    true,
                    "config",
                    &format!("  {}{perms_note}", cfg_path.display()),
                );
                let _ = c; // 已验证可解析
            }
            Err(e) => check(&mut out, false, "config", &format!("  解析失败：{e:#}")),
        }
    } else {
        out.push("- config        未创建（可选，course2md config init 生成）".into());
    }

    let model_root = crate::config::cache_dir().join("models");
    let llama = crate::models::llama_paths(&model_root);
    if crate::models::llama_ready(&model_root) {
        check(
            &mut out,
            true,
            "models",
            &format!("  Qwen3 GGUF 就绪（{}）", model_root.display()),
        );
    } else {
        out.push(format!(
            "- models        未下载（gpu/cpu 首次运行会自动下载约 2.4GB 到 {}）",
            cache_dir().display()
        ));
        let _ = &llama;
    }

    // api key 提示
    let has_key = crate::settings::load()
        .map(|c| !c.asr_api.api_key.trim().is_empty())
        .unwrap_or(false)
        || std::env::var("OPENROUTER_API_KEY")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false);
    if !has_key {
        out.push("- asr_api key   未配置（--provider api 时需要）".into());
    }

    // —— 默认后端结论 ——
    let default = crate::config::default_provider_hint();
    out.push(String::new());
    out.push(format!(
        "默认后端：{}（本机将使用 {}）",
        default,
        match default {
            AsrProvider::Coreml => "Apple Neural Engine / CoreML",
            AsrProvider::Gpu => "llama.cpp GPU（Metal/CUDA/Vulkan）",
            AsrProvider::Npu => "Intel NPU (OpenVINO)",
            AsrProvider::Cpu => "llama.cpp CPU",
            AsrProvider::Api => "云端 STT",
        }
    ));

    for line in &out {
        println!("{line}");
    }
    Ok(())
}
