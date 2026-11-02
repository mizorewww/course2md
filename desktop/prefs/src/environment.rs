//! What this machine offers the desktop: the search path for the engine and its tools,
//! and which recognition methods the installed components support.
use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// The search path for engine and helper tools. The environment it is built
/// from does not change while the app runs, so it is computed once.
pub fn tool_path() -> std::ffi::OsString {
    static PATH: std::sync::OnceLock<std::ffi::OsString> = std::sync::OnceLock::new();
    PATH.get_or_init(compute_tool_path).clone()
}

fn compute_tool_path() -> std::ffi::OsString {
    if let Some(path) = std::env::var_os("COURSE2MD_TOOL_PATH").filter(|path| !path.is_empty()) {
        return path;
    }
    // 随包自带的工具（yt-dlp、ffmpeg、llama.cpp/ 等）所在的应用目录排在最前：
    // 安装包行为不随用户 PATH 漂移。目录清单与引擎的 tools::bundled_dirs 同一份。
    let mut dirs: Vec<PathBuf> = course2md::tools::bundled_dirs();
    dirs.extend(
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    #[cfg(unix)]
    {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(&home).join(".local/bin"));
            dirs.push(PathBuf::from(home).join(".cargo/bin"));
        }
        dirs.extend([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
    }
    std::env::join_paths(dirs).unwrap_or_default()
}
/// Presence of a llama.cpp runtime is independent of `--list-devices` succeeding.
/// That flag can fail on CPU-only builds or older binaries that are still installed.
pub fn llama_binary_on_path(path: &std::ffi::OsStr) -> bool {
    // The working directory only matters for names with a path in them; these have none.
    ["llama-server", "llama-cli"]
        .iter()
        .any(|name| which::which_in(name, Some(path), ".").is_ok())
}

pub fn resolve_cli() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("COURSE2MD_BIN") {
        let path = PathBuf::from(path);
        anyhow::ensure!(
            path.is_file(),
            course2md::tr!("COURSE2MD_BIN 指向的文件不存在：{}", path.display())
        );
        return Ok(path);
    }
    let name = format!("course2md{}", std::env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()?.parent() {
        candidates.push(dir.join(&name));
    }
    // A development build runs beside the workspace's own engine build; an
    // older engine on PATH cannot read today's task format.
    if cfg!(debug_assertions) {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        candidates.extend(["debug", "release"].map(|profile| workspace.join(profile).join(&name)));
    }
    candidates.extend(std::env::split_paths(&tool_path()).map(|dir| dir.join(&name)));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .context(course2md::tr!(
            "找不到 course2md 引擎。请将 CLI 放在应用旁，或设置 COURSE2MD_BIN。"
        ))
}

#[derive(Clone)]
pub struct Environment {
    pub engine: bool,
    pub ffmpeg: bool,
    pub ffprobe: bool,
    pub ytdlp: bool,
    pub llama: bool,
    pub apple: bool,
    pub gpu: Option<String>,
    pub npu_parts: NpuParts,
    /// Windows: the Microsoft Visual C++ runtime llama-server links against
    /// is installed (always true elsewhere).
    pub vc_runtime: bool,
    /// Where each found media tool runs from, by the names the checks show.
    pub origins: std::collections::BTreeMap<&'static str, course2md::tools::Origin>,
}

/// The two independent requirements of Intel NPU recognition.
#[derive(Clone, Copy, Default)]
pub struct NpuParts {
    pub device: bool,
    pub runtime: bool,
}

impl Environment {
    /// Intel NPU recognition needs both the device and its Python runtime.
    pub fn npu(&self) -> bool {
        self.npu_parts.device && self.npu_parts.runtime
    }
    pub fn npu_device(&self) -> bool {
        self.npu_parts.device
    }
    pub fn npu_runtime(&self) -> bool {
        self.npu_parts.runtime
    }
    /// 本机环境是否支持指定的识别方式。Api 不依赖本机组件。
    pub fn supports(&self, provider: course2md::config::AsrProvider) -> bool {
        use course2md::config::AsrProvider;
        match provider {
            AsrProvider::Coreml => self.apple,
            AsrProvider::Gpu => self.llama && self.vc_runtime && self.gpu.is_some(),
            AsrProvider::Cpu => self.llama && self.vc_runtime,
            AsrProvider::Npu => self.npu(),
            AsrProvider::Api => true,
        }
    }

    pub fn detect() -> Self {
        let cli = resolve_cli().ok();
        // yt-dlp 走共享搜索清单（随包自带 → PATH → 常见安装目录），定位到
        // 绝对路径再探测；找不到时退回裸名，probe 失败与「未安装」同义。
        let ytdlp = course2md::ytdlp::resolve_in(Some(&tool_path()))
            .unwrap_or_else(|| PathBuf::from("yt-dlp"));
        // ffmpeg/ffprobe/llama-server: the engine's search (bundled with the app, then PATH).
        let media = |name: &str| course2md::tools::locate_in(name, Some(&tool_path()));
        let (ffmpeg, ffprobe) = (media("ffmpeg"), media("ffprobe"));
        let llama_server = media("llama-server");
        let mut origins = std::collections::BTreeMap::new();
        for (name, tool) in [
            ("ffmpeg", &ffmpeg),
            ("ffprobe", &ffprobe),
            ("llama-server", &llama_server),
        ] {
            if let Some(tool) = tool {
                origins.insert(name, tool.origin);
            }
        }
        if ytdlp.is_absolute() {
            let origin = course2md::tools::origin_of(&ytdlp, course2md::ytdlp::ENV_OVERRIDE);
            origins.insert("yt-dlp", origin);
        }
        let program = |tool: Option<course2md::tools::Tool>, name: &str| {
            tool.map_or_else(|| PathBuf::from(name), |tool| tool.path)
        };
        let (ffmpeg, ffprobe) = (program(ffmpeg, "ffmpeg"), program(ffprobe, "ffprobe"));
        let llama_server = program(llama_server, "llama-server");
        // (字段名, 命令, 参数) 与回填写在一起：数组重排不会造成静默错配
        let checks = std::thread::scope(|scope| {
            let commands: Vec<(&'static str, PathBuf, &'static str)> = vec![
                (
                    "engine",
                    cli.clone().unwrap_or_else(|| PathBuf::from("course2md")),
                    "--version",
                ),
                ("ffmpeg", ffmpeg, "-version"),
                ("ffprobe", ffprobe, "-version"),
                ("ytdlp", ytdlp, "--version"),
                ("llama-devices", llama_server, "--list-devices"),
            ];
            let handles: Vec<_> = commands
                .into_iter()
                .map(|(name, bin, arg)| {
                    // GPU 首次枚举可能触发驱动初始化（实测偶发 17s），给足与引擎一致的余量
                    let timeout = if name == "llama-devices" {
                        Duration::from_secs(20)
                    } else {
                        Duration::from_secs(5)
                    };
                    (name, scope.spawn(move || probe(&bin, &[arg], timeout)))
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|(name, task)| task.join().unwrap_or(None).map(|out| (name, out)))
                .collect::<std::collections::BTreeMap<_, _>>()
        });
        let llama = llama_binary_on_path(&tool_path());
        let gpu = checks.get("llama-devices").and_then(|output| {
            let devices = course2md::asr::parse_gpu_devices(output).ok()?;
            let (_, description) = devices.first()?.split_once(':')?;
            let name = description
                .split_once(" (")
                .map_or(description, |(name, _)| name);
            Some(name.trim().to_owned())
        });
        let apple = course2md::config::apple_native_available()
            && cli
                .as_deref()
                .and_then(course2md::config::find_metallib)
                .is_some();
        // Both checks run commands (PowerShell, a Python import), so they run
        // side by side; Intel NPUs only exist on Linux and Windows PCs.
        let (npu_device, npu_runtime) = if cfg!(any(target_os = "linux", windows)) {
            std::thread::scope(|scope| {
                let device = scope.spawn(course2md::npu::device_present);
                let runtime = course2md::npu::find_launcher(Some(&tool_path())).is_some();
                (device.join().unwrap_or(false), runtime)
            })
        } else {
            (false, false)
        };
        Self {
            engine: cli.is_some() && checks.contains_key("engine"),
            ffmpeg: checks.contains_key("ffmpeg"),
            ffprobe: checks.contains_key("ffprobe"),
            ytdlp: checks.contains_key("ytdlp"),
            llama,
            apple,
            gpu,
            npu_parts: NpuParts {
                device: npu_device,
                runtime: npu_runtime,
            },
            vc_runtime: vc_runtime_present(),
            origins,
        }
    }
}

/// The Visual C++ runtime DLLs that llama.cpp's Windows build imports beyond
/// what the app itself needs, in System32 (where Microsoft's installer puts
/// them) or app-local beside llama-server.
fn vc_runtime_present() -> bool {
    if !cfg!(windows) {
        return true;
    }
    let system = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32"));
    let local = course2md::tools::bundled_dirs()
        .into_iter()
        .filter(|dir| dir.ends_with(course2md::tools::LLAMA_DIR));
    let dirs: Vec<PathBuf> = std::iter::once(system).chain(local).collect();
    ["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll"]
        .iter()
        .all(|dll| dirs.iter().any(|dir| dir.join(dll).is_file()))
}

/// Bounded, executable checks: the tool's output when it runs and succeeds.
fn probe(bin: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut command = Command::new(bin);
    command.args(args).env("PATH", tool_path());
    course2md::runtime::run_ok(&mut command, timeout).map(|out| out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::time::Instant;

    #[test]
    fn probe_reports_output_and_times_out() {
        #[cfg(unix)]
        {
            let started = Instant::now();
            assert_eq!(
                probe(Path::new("/bin/echo"), &["ready"], Duration::from_secs(5)).as_deref(),
                Some("ready\n")
            );
            assert!(probe(Path::new("/bin/sleep"), &["5"], Duration::from_millis(200)).is_none());
            assert!(started.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    fn llama_runtime_is_present_when_the_binary_exists_even_if_device_listing_fails() {
        let missing = tempfile::tempdir().unwrap();
        assert!(!llama_binary_on_path(missing.path().as_os_str()));
        let dir = tempfile::tempdir().unwrap();
        let bin = dir
            .path()
            .join(format!("llama-server{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&bin, b"not-executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(llama_binary_on_path(dir.path().as_os_str()));
    }
}
