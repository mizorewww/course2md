//! 环境体检：一次性报告依赖工具、平台后端可用性、配置与模型缓存状态。
//! 用户报 issue 时附上 `course2md doctor` 输出即可定位大多数环境问题。

use anyhow::Result;
use std::process::Command;
use std::time::Duration;

/// 版本首行展示的最大字符数（防止某些工具输出超长首行撑爆版面）
const VERSION_LINE_MAX_CHARS: usize = 72;

/// 单个工具的版本探测上限：坏掉的二进制不能让 doctor 卡住。
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

fn tool_version(cmd: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Option<String> {
    let mut command = Command::new(cmd);
    command.args(args);
    let out = crate::runtime::run_bounded("doctor", &mut command, VERSION_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(VERSION_LINE_MAX_CHARS)
            .collect(),
    )
}

/// Where a tool runs from: its path and whether it ships with the app.
fn location(tool: &crate::tools::Tool) -> String {
    format!("{} · {}", tool.origin.label(), tool.path.display())
}

fn check(line: &mut Vec<String>, ok: bool, name: &str, detail: &str) {
    let mark = if ok { "✓" } else { "✗" };
    line.push(format!("{mark} {name}{detail}"));
}

/// One tool's lines: its version and location, or why it cannot be used. A required
/// tool's problem must be fixed; the return value says whether there is one.
fn report_tool(
    out: &mut Vec<String>,
    name: &str,
    tool: Option<crate::tools::Tool>,
    version_args: &[&str],
    missing: String,
    required: bool,
) -> bool {
    let problem = match &tool {
        None => missing,
        Some(found) => match tool_version(&found.path, version_args) {
            Some(version) => {
                check(out, true, name, &format!("  {version}"));
                out.push(format!("    {}", location(found)));
                return false;
            }
            None => crate::tr!("找到但无法运行：{}", found.path.display()),
        },
    };
    let mark = match (required, tool.is_some()) {
        (true, _) => "✗",
        (false, true) => "!",
        (false, false) => "-",
    };
    out.push(format!("{mark} {name}  {problem}"));
    required
}

pub fn run() -> Result<()> {
    let mut out = vec![format!(
        "course2md {} ({}/{})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )];
    let mut failures = 0;
    out.push(crate::tr!("\n必需工具").into());
    for tool in ["ffmpeg", "ffprobe"] {
        let missing = crate::tr!("缺失：{}", crate::error::install_hint(tool));
        let found = crate::tools::locate(tool);
        failures += report_tool(
            &mut out,
            tool,
            found,
            &["-version", "-hide_banner"],
            missing,
            true,
        ) as usize;
    }
    out.push(crate::tr!("\n按用途选装").into());
    let ytdlp = crate::ytdlp::resolve().map(|path| crate::tools::Tool {
        origin: crate::tools::origin_of(&path, crate::ytdlp::ENV_OVERRIDE),
        path,
    });
    let missing = format!(
        "{}；{}",
        crate::error::install_hint("yt-dlp"),
        crate::tr!("重跑 install.sh 可安装随包版本")
    );
    report_tool(&mut out, "yt-dlp", ytdlp, &["--version"], missing, false);
    let missing = crate::tr!(
        "未安装；模型改为单连接下载：{}",
        crate::error::install_hint("aria2")
    );
    let aria2 = crate::tools::locate("aria2c");
    report_tool(&mut out, "aria2c", aria2, &["--version"], missing, false);
    #[cfg(apple_native)]
    {
        // 与运行时 ensure_metallib 同一搜索清单（exe 同目录/Resources/上级 Resources/CWD）
        if crate::apple::find_metallib().is_some() {
            check(
                &mut out,
                true,
                "coreml",
                &format!(
                    "  {}",
                    crate::tr!("Apple 原生后端可用 / Apple native backend available")
                ),
            );
        } else {
            out.push(crate::tr!("! coreml  缺少 Metal 着色器资源，请重新安装 / Metal shader resources missing; reinstall course2md").into());
        }
    }
    #[cfg(not(apple_native))]
    out.push(crate::tr!("- coreml  本构建不包含 Apple 原生后端 / Apple native backend is not included in this build").into());
    match crate::tools::locate("llama-server") {
        Some(found) => {
            check(
                &mut out,
                true,
                "gpu/cpu",
                &format!("  {}", location(&found)),
            );
            match crate::asr::gpu_devices(&found.path) {
                Ok(devices) if !devices.is_empty() => {
                    check(&mut out, true, "GPU", &format!("  {}", devices.join(", ")))
                }
                Ok(_) => out.push(crate::tr!(
                    "- GPU  未检测到 GPU。{}",
                    crate::asr::GPU_SETUP_HINT
                )),
                Err(error) => out.push(crate::tr!("! GPU  无法检查设备：{}", format!("{error:#}"))),
            }
        }
        None => out.push(crate::tr!(
            "- gpu/cpu  显卡/CPU 识别需要 llama.cpp：{}",
            crate::error::install_hint("llama.cpp")
        )),
    }
    if cfg!(any(target_os = "linux", windows)) {
        let device = crate::npu::device_present();
        out.push(if device {
            crate::tr!("✓ npu  检测到 Intel NPU（首次使用会自动安装运行组件）/ Intel NPU detected (runtime components install on first use)")
        } else {
            crate::tr!("- npu  未检测到 Intel NPU 设备 / No Intel NPU device detected")
        }.into());
        if device && crate::npu::find_launcher(None).is_none() {
            out.push(
                crate::tr!(
                    "- npu  需要 uv，或装好 openvino-genai 的 Python / Intel NPU transcription needs uv, or Python with openvino-genai installed"
                )
                .into(),
            );
        }
    }
    out.push(crate::tr!("\n配置与模型").into());
    let path = crate::settings::config_path();
    let loaded = crate::settings::load();
    match &loaded {
        Ok(cfg) => {
            if path.is_file() {
                check(&mut out, true, "config", &format!("  {}", path.display()));
            } else {
                out.push(crate::tr!("- config  使用内置默认值：{}", path.display()));
                out.push(format!(
                    "  {}",
                    crate::tr!("可选配置：course2md config init")
                ));
            }
            // Validate the same merged defaults as conversion, without requiring a source or network.
            match crate::options::resolve(String::new(), &crate::cli::RunOpts::default(), cfg)
                .and_then(|cfg| cfg.validate())
            {
                Ok(()) => {}
                Err(error) => {
                    failures += 1;
                    check(
                        &mut out,
                        false,
                        crate::tr!("配置值"),
                        &format!("  {error:#}"),
                    );
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
                out.push(crate::tr!(
                    "- gpu/cpu models  未下载或不完整：{}",
                    root.display()
                ));
                out.push(crate::tr!(
                    "  仅 gpu/cpu 需要；下载约 2.4GB：course2md models download --dir \"{}\"",
                    root.display()
                ));
            }
            let key = !cfg.asr_api.api_key.trim().is_empty()
                || crate::config::asr_api_key_from_env().is_some();
            out.push(if key { crate::tr!("✓ api  已设置密钥（隐藏）/ API key set (hidden)") } else { crate::tr!("- api  未设置密钥；仅云端识别需要 / API key not set; only required for cloud transcription: COURSE2MD_ASR_API_KEY") }.into());
            let provider = cfg
                .defaults
                .provider
                .unwrap_or_else(crate::config::default_provider_hint);
            let (zh, en) = provider.label();
            out.push(format!(
                "{}\n{provider}（{zh} / {en}）",
                crate::tr!("\n当前识别方式")
            ));
        }
        Err(error) => {
            failures += 1;
            check(&mut out, false, "config", &format!("  {error:#}"));
        }
    }
    out.push(crate::tr!("\n✓ 可用 · - 可选或未设置 · ! 需要检查 · ✗ 必须修复").into());
    for line in out {
        println!("{line}");
    }
    anyhow::ensure!(
        failures == 0,
        crate::tr!("发现 {} 项必需依赖或配置问题；请按上方提示修复", failures)
    );
    crate::println_tr!(
        "基础检查通过；可选服务未进行联网测试 / Basic checks passed; optional services were not tested online."
    );
    Ok(())
}
