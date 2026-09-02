//! 外部依赖链的自动解析与安装。
//!
//! 设计要点（对应 README 安装指南的"漫长依赖链"）：
//! - **需求驱动**：根据本次运行的后端与输入类型算出最小安装集，绝不预装全家桶
//! - **包管理器优先**：系统能装的引导用户自己装；缺包管理器时下载预编译二进制兜底
//! - **私有工具目录**：下载落盘到 [`crate::runtime::tools_dir`]，免 root、免 PATH 配置，
//!   `COURSE2MD_TOOLS_DIR` 可整体重定向；删除该目录即完全卸载
//! - **固定版本 + sha256**：所有预编译资产逐字节校验；yt-dlp/uv 用官方校验和，
//!   llama.cpp/ffmpeg-static 由 release 工程预计算并固化在本文件
//! - **带库工具的子目录布局**：llama-server 依赖同目录的 .so/.dylib/.dll，
//!   安装到 `tools_dir/llama-server/` 并整体替换，`which` 会查到该位置
//!
//! 入口：
//! - [`ensure_for_run`]：pipeline 预检（按需自动安装）
//! - [`setup_cmd`]：`course2md setup`（体检 + 交互式安装）

use crate::config::AsrProvider;
use crate::net::{self, Verify};
use crate::runtime;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─────────────────────────── manifest ───────────────────────────

/// 资产压缩形态。Single = 可执行文件本体；Tar/Zip 解压后平铺/落子目录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Single,
    Tar,
    Zip,
}

/// 资产变体：同一工具不同加速目标（目前仅 llama-server 需要）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Auto,
    Cpu,
    /// linux/win = Vulkan 构建，macOS = Metal 构建
    Accel,
}

impl Variant {
    fn as_str(self) -> &'static str {
        match self {
            Variant::Auto => "auto",
            Variant::Cpu => "cpu",
            Variant::Accel => "accel",
        }
    }
}

/// 一个平台 × 变体的下载资产。
pub struct AssetEntry {
    /// 平台键，见 [`platform_key`]
    pub key: &'static str,
    pub variant: Variant,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub kind: AssetKind,
}

/// 依赖的必需条件。
pub enum Req {
    /// 任何运行都需要
    Always,
    /// 在线视频输入需要（URL 且未 --no-download）
    UrlInput,
    /// 列出的后端需要
    Providers(&'static [AsrProvider]),
}

/// 一个外部工具的完整描述。
pub struct ToolSpec {
    /// 可执行文件名（Windows 自动追加 .exe）
    pub name: &'static str,
    pub version: &'static str,
    pub purpose_en: &'static str,
    pub purpose_zh: &'static str,
    pub req: Req,
    /// 版本探测参数（--version 之类）
    pub probe: &'static [&'static str],
    /// 手动安装提示（包管理器路径）
    pub hint: &'static str,
    /// true = 解压到 `tools_dir/<name>/` 子目录（带自带动态库的工具）
    pub bundle_dir: bool,
    pub assets: &'static [AssetEntry],
}


/// 资产表条目宏：URL 由两个字面量拼接（concat! 只接受字面量）。
macro_rules! asset {
    ($key:literal, $variant:expr, $base:literal, $path:literal, $sha:literal, $size:literal, $kind:expr) => {
        AssetEntry {
            key: $key,
            variant: $variant,
            url: concat!($base, $path),
            sha256: $sha,
            size: $size,
            kind: $kind,
        }
    };
}


/// ffmpeg / ffprobe：静态单文件（ffmpeg-static b6.1.1，GPL 构建；运行时下载
/// 而非随包分发，规避 MIT 项目的 GPL 再分发义务）。sha256 由本项目 release
/// 工程预计算固化。
const FFMPEG_ASSETS: &[AssetEntry] = &[
    asset!("linux-x86_64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffmpeg-linux-x64", "e7e7fb30477f717e6f55f9180a70386c62677ef8a4d4d1a5d948f4098aa3eb99", 79_826_272, AssetKind::Single),
    asset!("linux-aarch64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffmpeg-linux-arm64", "6bb182d0d75d23028db82e9e4f723ca69b853d055698486e6984ddb2c06fb8ce", 51_134_160, AssetKind::Single),
    asset!("macos-aarch64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffmpeg-darwin-arm64", "a90e3db6a3fd35f6074b013f948b1aa45b31c6375489d39e572bea3f18336584", 45_568_216, AssetKind::Single),
    asset!("macos-x86_64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffmpeg-darwin-x64", "ebdddc936f61e14049a2d4b549a412b8a40deeff6540e58a9f2a2da9e6b18894", 78_862_176, AssetKind::Single),
    asset!("windows-x86_64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffmpeg-win32-x64", "04e1307997530f9cf2fe35cba2ca7e8875ca91da02f89d6c7243df819c94ad00", 82_797_568, AssetKind::Single),
];

const FFPROBE_ASSETS: &[AssetEntry] = &[
    asset!("linux-x86_64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffprobe-linux-x64", "4f231a1960d83e403d08f7971e271707bec278a9ae18e21b8b5b03186668450d", 79_665_792, AssetKind::Single),
    asset!("linux-aarch64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffprobe-linux-arm64", "d17ae9b4c297d48e2521ba14e417bb0537c6ff77c584cdbcd6bb0d8d0307a2e8", 50_994_160, AssetKind::Single),
    asset!("macos-aarch64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffprobe-darwin-arm64", "bb2db6f5d8cef919da12fbf592119a987202a8c060a886f3cab091f9cab90b64", 45_528_808, AssetKind::Single),
    asset!("macos-x86_64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffprobe-darwin-x64", "fa3add0ce901f7241abe0dfc0155d958fc834aca3f8ce61f87cc712ae669c1e0", 78_780_408, AssetKind::Single),
    asset!("windows-x86_64", Variant::Auto, "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1", "/ffprobe-win32-x64", "3a7e2dc003dc2cd1472827e4c7c4f056ae1ae0ae7c5bbc580c99b49827351ba4", 82_668_032, AssetKind::Single),
];

/// yt-dlp：官方单文件二进制（macOS 资产为 universal2，覆盖两种架构）。
/// sha256 取自官方 SHA2-256SUMS。
const YTDLP_ASSETS: &[AssetEntry] = &[
    asset!("linux-x86_64", Variant::Auto, "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19", "/yt-dlp_linux", "58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a", 40_446_224, AssetKind::Single),
    asset!("linux-aarch64", Variant::Auto, "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19", "/yt-dlp_linux_aarch64", "b16e4dab368a816cd05d477d698a605a6ae87ccee1c8ffd38fa21d7254141fcc", 40_167_448, AssetKind::Single),
    asset!("macos-aarch64", Variant::Auto, "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19", "/yt-dlp_macos", "0f192b7ec147ab6288885d6351d9ab67367640029b4377576ef46dd79cf7b202", 37_146_048, AssetKind::Single),
    asset!("macos-x86_64", Variant::Auto, "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19", "/yt-dlp_macos", "0f192b7ec147ab6288885d6351d9ab67367640029b4377576ef46dd79cf7b202", 37_146_048, AssetKind::Single),
    asset!("windows-x86_64", Variant::Auto, "https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19", "/yt-dlp.exe", "66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a", 17_840_399, AssetKind::Single),
];

/// llama-server（llama.cpp 官方 release 归档，含全部 ggml 动态库 → 子目录布局）。
/// Vulkan 构建一张通吃 NVIDIA/AMD/Intel；CPU 构建是通用兜底。
/// sha256 由本项目预计算固化（上游不发布校验和）。
const LLAMA_ASSETS: &[AssetEntry] = &[
    asset!("linux-x86_64", Variant::Cpu, "https://github.com/ggml-org/llama.cpp/releases/download/b10742", "/llama-b10742-bin-ubuntu-x64.tar.gz", "f9c533a7f9000aac7b7f3ddf37fe3d18f943a0b9b7310e7f23a136764cf2d9fb", 16_704_783, AssetKind::Tar),
    asset!("linux-x86_64", Variant::Accel, "https://github.com/ggml-org/llama.cpp/releases/download/b10742", "/llama-b10742-bin-ubuntu-vulkan-x64.tar.gz", "d32aacf12aac2bcce66357701e5342f645ef789a927bd908eceba4d9d7523eb5", 33_772_362, AssetKind::Tar),
    asset!("linux-aarch64", Variant::Cpu, "https://github.com/ggml-org/llama.cpp/releases/download/b10742", "/llama-b10742-bin-ubuntu-arm64.tar.gz", "e90ce0baf9cffb8e36774e6b495118111a3d4fad39747905b0f412ecb3f39083", 13_349_362, AssetKind::Tar),
    asset!("macos-aarch64", Variant::Accel, "https://github.com/ggml-org/llama.cpp/releases/download/b10742", "/llama-b10742-bin-macos-arm64.tar.gz", "785190542f4a2265158f6a495487d6ac6a0ba093662d6d3612444f55dbaa0bbb", 11_073_346, AssetKind::Tar),
    asset!("macos-x86_64", Variant::Accel, "https://github.com/ggml-org/llama.cpp/releases/download/b10742", "/llama-b10742-bin-macos-x64.tar.gz", "9f4d5b42ef8d1d3df01d88f9c6b159c09b549d711ee3bfb5c29aa50e262ae6ef", 11_136_549, AssetKind::Tar),
    asset!("windows-x86_64", Variant::Cpu, "https://github.com/ggml-org/llama.cpp/releases/download/b10742", "/llama-b10742-bin-win-cpu-x64.zip", "a923d80953d618335ae0073233fdcfb93760dfde646e957786894259aba87d72", 18_373_032, AssetKind::Zip),
    asset!("windows-x86_64", Variant::Accel, "https://github.com/ggml-org/llama.cpp/releases/download/b10742", "/llama-b10742-bin-win-vulkan-x64.zip", "029a8b4495a3b9672752f70ec33a0be788ba0446a67581f544deae4a9ccb165f", 35_183_545, AssetKind::Zip),
];

/// uv：单文件静态二进制（tar.gz/zip 归档平铺出 uv/uvx）。官方 .sha256。
const UV_ASSETS: &[AssetEntry] = &[
    asset!("linux-x86_64", Variant::Auto, "https://github.com/astral-sh/uv/releases/download/0.12.8", "/uv-x86_64-unknown-linux-gnu.tar.gz", "2e2b37e9811e17675a9e70bed5e1a58fc8c0388be63d751d72cc735188c149ff", 0, AssetKind::Tar),
    asset!("linux-aarch64", Variant::Auto, "https://github.com/astral-sh/uv/releases/download/0.12.8", "/uv-aarch64-unknown-linux-gnu.tar.gz", "ba8661f4fd207c8e94814191598e619b355ac10d5014e851e21eb800f9ef2b00", 0, AssetKind::Tar),
    asset!("macos-x86_64", Variant::Auto, "https://github.com/astral-sh/uv/releases/download/0.12.8", "/uv-x86_64-apple-darwin.tar.gz", "bfcd4407de99e0a2c1904df0902fa1795653d4edd145358e6561527e746a4f16", 0, AssetKind::Tar),
    asset!("macos-aarch64", Variant::Auto, "https://github.com/astral-sh/uv/releases/download/0.12.8", "/uv-aarch64-apple-darwin.tar.gz", "8ce083658dbff20143607ca7af8e0c1d64b6fd7bf03a5cdcb62bf3d47d991b5f", 0, AssetKind::Tar),
    asset!("windows-x86_64", Variant::Auto, "https://github.com/astral-sh/uv/releases/download/0.12.8", "/uv-x86_64-pc-windows-msvc.zip", "e07acf3f8a29fe41f9e04b799c3325cb0e0893836bb222bf102829b45c679ad6", 0, AssetKind::Zip),
];

pub const SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "ffmpeg",
        version: "7.0.2 (ffmpeg-static)",
        purpose_en: "audio & frame extraction (required)",
        purpose_zh: "音视频抽取与画面采样（必需）",
        req: Req::Always,
        probe: &["-version", "-hide_banner"],
        hint: "brew install ffmpeg / apt install ffmpeg / pacman -S ffmpeg / winget install Gyan.FFmpeg",
        bundle_dir: false,
        assets: FFMPEG_ASSETS,
    },
    ToolSpec {
        name: "ffprobe",
        version: "7.0.2 (ffmpeg-static)",
        purpose_en: "media probing (required)",
        purpose_zh: "媒体元信息探测（必需）",
        req: Req::Always,
        probe: &["-version", "-hide_banner"],
        hint: "brew install ffmpeg / apt install ffmpeg / pacman -S ffmpeg / winget install Gyan.FFmpeg",
        bundle_dir: false,
        assets: FFPROBE_ASSETS,
    },
    ToolSpec {
        name: "yt-dlp",
        version: "2026.08.19",
        purpose_en: "online video fetching (needed for URLs)",
        purpose_zh: "在线视频解析与下载（URL 输入需要）",
        req: Req::UrlInput,
        probe: &["--version"],
        hint: "brew install yt-dlp / pip install yt-dlp / winget install yt-dlp.yt-dlp",
        bundle_dir: false,
        assets: YTDLP_ASSETS,
    },
    ToolSpec {
        name: "llama-server",
        version: "b10742 (llama.cpp)",
        purpose_en: "ASR inference server for gpu/cpu backends",
        purpose_zh: "gpu/cpu 识别后端的推理服务",
        req: Req::Providers(&[AsrProvider::Gpu, AsrProvider::Cpu]),
        probe: &["--version"],
        hint: "brew install llama.cpp / winget install ggml.llamacpp；Debian 见 README 编译安装",
        bundle_dir: true,
        assets: LLAMA_ASSETS,
    },
    ToolSpec {
        name: "uv",
        version: "0.12.8",
        purpose_en: "python env manager for the npu backend",
        purpose_zh: "npu 后端的 Python 环境管理",
        req: Req::Providers(&[AsrProvider::Npu]),
        probe: &["--version"],
        hint: "curl -LsSf https://astral.sh/uv/install.sh | sh（Windows: irm https://astral.sh/uv/install.ps1 | iex）",
        bundle_dir: false,
        assets: UV_ASSETS,
    },
];

// ─────────────────────────── 平台与资产选择 ───────────────────────────

/// 当前平台的 manifest 键；None = 无预编译资产覆盖。
pub fn platform_key() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("macos", "aarch64") => Some("macos-aarch64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        _ => None,
    }
}

/// windows 下可执行文件名补 .exe。
fn tool_exe(name: &str) -> String {
    if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// 在某平台键下挑选资产：优先精确变体，其次 Auto 变体（单变体工具满足任何
/// 偏好，不告警），最后回落任意变体。返回 (entry, 是否无降级)。
fn pick_asset<'a>(
    spec: &'a ToolSpec,
    key: &str,
    want: Variant,
) -> Option<(&'a AssetEntry, bool)> {
    let candidates: Vec<&AssetEntry> = spec.assets.iter().filter(|a| a.key == key).collect();
    if candidates.is_empty() {
        return None;
    }
    // 精确变体（want==Auto 时 Auto 资产即精确命中）
    if let Some(exact) = candidates.iter().copied().find(|a| a.variant == want) {
        return Some((exact, true));
    }
    // 单变体（Auto）工具：任何偏好都视为匹配
    if let Some(auto) = candidates
        .iter()
        .copied()
        .find(|a| a.variant == Variant::Auto)
    {
        return Some((auto, true));
    }
    Some((candidates[0], false))
}

/// 按 name 查 spec（测试辅助）。
#[cfg(test)]
fn spec_by_name(name: &str) -> Option<&'static ToolSpec> {
    SPECS.iter().find(|s| s.name == name)
}

/// 本次运行需要的工具集合（需求驱动的最小安装集）。
pub fn specs_for_run(provider: AsrProvider, url_input: bool) -> Vec<&'static ToolSpec> {
    SPECS.iter()
        .filter(|s| match &s.req {
            Req::Always => true,
            Req::UrlInput => url_input,
            Req::Providers(ps) => ps.contains(&provider),
        })
        .collect()
}

/// 按后端挑 llama-server 变体。
fn want_variant(provider: AsrProvider) -> Variant {
    match provider {
        AsrProvider::Gpu => Variant::Accel,
        AsrProvider::Cpu => Variant::Cpu,
        _ => Variant::Auto,
    }
}

// ─────────────────────────── 状态与安装 ───────────────────────────

/// 安装后的落盘记录（版本/变体/哈希），用于体检展示与变体切换判断。
#[derive(Debug, Serialize, Deserialize)]
pub struct Stamp {
    version: String,
    variant: String,
    sha256: String,
    installed_at: u64,
}

fn stamp_path(name: &str) -> PathBuf {
    runtime::tools_dir().join(".stamps").join(format!("{name}.json"))
}

fn read_stamp(name: &str) -> Option<Stamp> {
    let s = std::fs::read_to_string(stamp_path(name)).ok()?;
    serde_json::from_str(&s).ok()
}

fn write_stamp(spec: &ToolSpec, entry: &AssetEntry) -> Result<()> {
    let p = stamp_path(spec.name);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let st = Stamp {
        version: spec.version.to_string(),
        variant: entry.variant.as_str().into(),
        sha256: entry.sha256.into(),
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    crate::checkpoint::atomic_write(&p, serde_json::to_string_pretty(&st)?.as_bytes())?;
    Ok(())
}

/// 已装安装（ours）是否落后于当前 manifest（新版可升级）。
fn is_upgradable(spec: &ToolSpec, want: Variant, rep: &ToolReport) -> bool {
    let Some(stamp) = &rep.stamp else { return false };
    if !rep.ours {
        return false;
    }
    platform_key()
        .and_then(|k| pick_asset(spec, k, want))
        .map(|(a, _)| !a.sha256.eq_ignore_ascii_case(&stamp.sha256))
        .unwrap_or(false)
}

fn tool_version(cmd: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
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
            .take(72)
            .collect(),
    )
}

/// 单个工具的体检结果。
pub struct ToolReport {
    pub spec: &'static ToolSpec,
    /// 解析到的可执行路径（私有目录或系统 PATH）
    pub path: Option<PathBuf>,
    /// 探测到的版本（探测失败则 None）
    pub version: Option<String>,
    /// 安装在我们的私有工具目录
    pub ours: bool,
    /// 我们记录的安装 stamp（仅 ours）
    pub stamp: Option<Stamp>,
}

pub fn report(spec: &'static ToolSpec) -> ToolReport {
    match runtime::which(spec.name) {
        Some(p) => {
            let ours = p.starts_with(runtime::tools_dir());
            let stamp = if ours { read_stamp(spec.name) } else { None };
            let version = tool_version(&p, spec.probe);
            ToolReport { spec, path: Some(p), version, ours, stamp }
        }
        None => ToolReport { spec, path: None, version: None, ours: false, stamp: None },
    }
}

/// 安装一个工具到私有工具目录。
/// `want` 指定变体偏好（仅多变体工具如 llama-server 有意义）。
pub async fn install(spec: &'static ToolSpec, want: Variant) -> Result<PathBuf> {
    let key = platform_key()
        .with_context(|| format!("当前平台不支持自动安装 {}", spec.name))?;
    let (entry, exact) = pick_asset(spec, key, want)
        .with_context(|| format!("{} 在 {key} 无预编译资产；{}", spec.name, spec.hint))?;
    if !exact {
        tracing::warn!(
            "{} 没有 {want:?} 变体，回落到 {:?} 构建",
            spec.name,
            entry.variant
        );
    }

    let dir = runtime::tools_dir();
    std::fs::create_dir_all(&dir)?;
    // bundle 工具的旧安装目录：下载/解压全部成功后才替换（失败不毁现有安装）
    let dest_dir = if spec.bundle_dir {
        dir.join(spec.name)
    } else {
        dir.clone()
    };

    match entry.kind {
        AssetKind::Single => {
            let dest = dest_dir.join(tool_exe(spec.name));
            net::download_file(&net::Download {
                url: entry.url.into(),
                dest,
                label: spec.name.into(),
                verify: Verify::Sha256(entry.sha256),
            })
            .await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    dest_dir.join(tool_exe(spec.name)),
                    std::fs::Permissions::from_mode(0o755),
                );
            }
        }
        AssetKind::Tar | AssetKind::Zip => {
            let tmp = std::env::temp_dir()
                .join(format!("course2md-deps-{}-{}", spec.name, std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(tmp.join("pkg"))?;
            let archive = tmp.join("asset.bin");
            net::download_file(&net::Download {
                url: entry.url.into(),
                dest: archive.clone(),
                label: format!("{} (archive)", spec.name),
                verify: Verify::Sha256(entry.sha256),
            })
            .await?;
            let extract = tmp.join("out");
            std::fs::create_dir_all(&extract)?;
            let res = match entry.kind {
                AssetKind::Tar => extract_tar_gz(&archive, &extract),
                _ => extract_zip(&archive, &extract),
            };
            if let Err(e) = res {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
            // 全部成功才替换旧安装：bundle 工具整体清空子目录（避免跨版本残留旧 .so）
            if spec.bundle_dir && dest_dir.is_dir() {
                std::fs::remove_dir_all(&dest_dir)?;
            }
            std::fs::create_dir_all(&dest_dir)?;
            let moved = flatten_move(&extract, &dest_dir);
            let _ = std::fs::remove_dir_all(&tmp);
            let moved = moved?;
            tracing::debug!(tool = spec.name, files = moved, "extracted");
        }
    }

    write_stamp(spec, entry)?;
    let bin = dest_dir.join(tool_exe(spec.name));
    println!(
        "{}",
        crate::i18n::tr(
            &format!("✓ installed {} ({}) → {}", spec.name, spec.version, bin.display()),
            &format!("✓ 已安装 {}（{}）→ {}", spec.name, spec.version, bin.display()),
        )
    );
    Ok(bin)
}

/// 解压 .tar.gz（纯 Rust：tar + flate2，不依赖系统 tar/gzip）。
fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let f = std::fs::File::open(archive)
        .with_context(|| format!("打开归档 {}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(dest)
        .with_context(|| format!("tar.gz 解压失败：{}", archive.display()))
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let f = std::fs::File::open(archive)
        .with_context(|| format!("打开归档 {}", archive.display()))?;
    let mut z = zip::ZipArchive::new(f).context("读取 zip 归档失败")?;
    z.extract(dest).context("zip 解压失败")?;
    Ok(())
}

/// 把解压目录里的所有条目（剥掉顶层目录）平铺移动到 `dest`。
/// - 符号链接原样重建（libX.so → libX.so.0 → libX.so.N 版本链必须保留，
///   否则动态链接器按 SONAME 找不到库）；目标文件在同一批里移动，最终必然可解析
/// - 普通文件优先 rename（同盘零拷贝），跨盘回落 copy+remove；Unix 上 chmod 0755
fn flatten_move(src: &Path, dest: &Path) -> Result<usize> {
    let mut n = 0;
    for entry in walk(src)? {
        let name = entry
            .file_name()
            .and_then(|s| s.to_str())
            .context("归档内出现非 UTF-8 文件名")?;
        anyhow::ensure!(!name.is_empty() && !name.contains(".."), "归档内出现可疑路径：{name}");
        let target = dest.join(name);

        #[cfg(unix)]
        if std::fs::symlink_metadata(&entry)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            let link = std::fs::read_link(&entry)?;
            std::os::unix::fs::symlink(&link, &target)
                .with_context(|| format!("重建符号链接 {name} → {}", link.display()))?;
            let _ = std::fs::remove_file(&entry);
            n += 1;
            continue;
        }

        if std::fs::rename(&entry, &target).is_err() {
            // 跨文件系统：copy + remove
            std::fs::copy(&entry, &target)
                .with_context(|| format!("搬运 {} → {}", entry.display(), target.display()))?;
            let _ = std::fs::remove_file(&entry);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
        }
        n += 1;
    }
    Ok(n)
}

fn walk(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let p = e?.path();
            // 符号链接不跟随：原样收集（含悬空链接），绝不深入链接目录
            #[cfg(unix)]
            if std::fs::symlink_metadata(&p)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                out.push(p);
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    Ok(out)
}

// ─────────────────────────── 运行预检（pipeline 接入点） ───────────────────────────

/// pipeline 预检：按本次运行需求确保外部工具就绪。
/// 缺失且允许自动安装时按需下载；否则报错并给出包管理器提示。
pub async fn ensure_for_run(provider: AsrProvider, url_input: bool, auto_install: bool) -> Result<()> {
    let want = want_variant(provider);
    for spec in specs_for_run(provider, url_input) {
        let rep = report(spec);
        if rep.path.is_some() {
            // 已有安装与本机当前需求不一致时按需更新：
            // 1) 变体切换（bundle 工具，如 llama-server 的 vulkan ↔ cpu）
            let variant_mismatch = rep.ours
                && spec.bundle_dir
                && want != Variant::Auto
                && rep.stamp.as_ref().map(|s| s.variant.as_str()) != Some(want.as_str());
            // 2) manifest 升级（stamp sha 与当前清单资产不一致 → 新版可用）
            let upgradable = is_upgradable(spec, want, &rep);
            if variant_mismatch || upgradable {
                if auto_install {
                    if upgradable && !variant_mismatch {
                        println!(
                            "{}",
                            crate::i18n::tr(
                                &format!("updating {} to {}…", spec.name, spec.version),
                                &format!("检测到 {} 新版本（{}），正在升级…", spec.name, spec.version),
                            )
                        );
                    }
                    install(spec, want).await?;
                } else if variant_mismatch {
                    tracing::warn!(
                        "{} 当前为 {} 构建（--provider {provider} 建议 {:?}）",
                        spec.name,
                        rep.stamp.as_ref().map(|s| s.variant.as_str()).unwrap_or("unknown"),
                        want
                    );
                } else {
                    tracing::info!(
                        "{} 有新版本（{}）；运行 course2md setup --yes 升级",
                        spec.name,
                        spec.version
                    );
                }
            }
            continue;
        }
        if !auto_install {
            bail!(
                "{}\n{}: {}\n或运行 course2md setup 自动安装（下载到 {}）",
                crate::i18n::tr(&format!("missing {name}", name = spec.name), &format!("未找到 {}", spec.name)),
                crate::i18n::tr("install manually", "手动安装"),
                spec.hint,
                runtime::tools_dir().display()
            );
        }
        let size_mb = pick_asset(spec, platform_key().unwrap_or(""), want)
            .map(|(a, _)| a.size)
            .unwrap_or(0) as f64
            / (1024.0 * 1024.0);
        println!(
            "{}",
            crate::i18n::tr(
                &format!(
                    "{name} not found, auto-installing to {dir} (~{size:.0} MB)…",
                    name = spec.name,
                    dir = runtime::tools_dir().display(),
                    size = size_mb
                ),
                &format!(
                    "未找到 {name}，正在自动安装到 {dir}（约 {size:.0} MB），请不要退出…",
                    name = spec.name,
                    dir = runtime::tools_dir().display(),
                    size = size_mb
                ),
            )
        );
        install(spec, want).await?;
    }

    // coreml 回落提示（与旧 require_cmd 行为一致：best-effort 提前告知）
    if provider == AsrProvider::Coreml && runtime::which("llama-server").is_none() {
        tracing::warn!("未找到 llama-server：CoreML 若失败将无法回退到 gpu 后端（best-effort）");
    }
    Ok(())
}

// ─────────────────────────── course2md setup ───────────────────────────

/// `course2md setup`：体检 + 安装缺失的核心工具。
/// - `check_only`：只报告
/// - `yes`：跳过确认直接安装
/// - `all`：连可选工具（llama-server/uv）一起安装
pub async fn setup_cmd(check_only: bool, yes: bool, all: bool) -> Result<()> {
    let zh = crate::i18n::is_zh();
    let td = runtime::tools_dir();
    println!(
        "{}",
        if zh {
            format!("course2md 依赖体检（平台 {key:?}，工具目录 {}）", td.display(), key = platform_key())
        } else {
            format!("course2md dependency check (platform {key:?}, tools dir {})", td.display(), key = platform_key())
        }
    );
    println!();

    let core: Vec<&'static ToolSpec> = SPECS.iter().filter(|s| matches!(s.req, Req::Always | Req::UrlInput)).collect();
    let optional: Vec<&'static ToolSpec> =
        SPECS.iter().filter(|s| matches!(s.req, Req::Providers(_))).collect();

    let mut todo: Vec<&'static ToolSpec> = Vec::new();
    let mut core_missing = false;
    // 安装变体：跟随配置的默认后端（gpu→accel 构建，cpu→cpu 构建）
    let provider = crate::settings::load()
        .ok()
        .and_then(|c| c.defaults.provider)
        .unwrap_or_else(crate::config::default_provider_hint);
    let want = want_variant(provider);

    for spec in core.iter().copied().chain(optional.iter().copied()) {
        let rep = report(spec);
        let optional_mark = matches!(spec.req, Req::Providers(_));
        let upg = is_upgradable(spec, want, &rep);
        let ours_mark = if rep.ours {
            if zh { "  (自动安装)" } else { "  (auto-installed)" }
        } else {
            ""
        };
        let upg_mark = if upg {
            if zh {
                format!("  ↻ 可升级 → {}", spec.version)
            } else {
                format!("  ↻ update available → {}", spec.version)
            }
        } else {
            String::new()
        };
        match (&rep.path, &rep.version) {
            (Some(p), Some(v)) => println!(
                "✓ {:<12} {:<40} {}{}{}",
                spec.name,
                v,
                p.display(),
                ours_mark,
                upg_mark
            ),
            (Some(p), None) => {
                println!("✓ {:<12} {:<40} {}{}", spec.name, "-", p.display(), upg_mark)
            }
            (None, _) => {
                let purpose = if zh { spec.purpose_zh } else { spec.purpose_en };
                let tag = if optional_mark && !all {
                    if zh { "可选" } else { "optional" }
                } else {
                    if zh { "缺" } else { "missing" }
                };
                println!("✗ {:<12} [{}] {}", spec.name, tag, purpose);
                if !optional_mark {
                    core_missing = true;
                }
                if !optional_mark || all {
                    todo.push(spec);
                }
            }
        }
        // 已安装但落后于 manifest：核心工具直接排入安装队列（含 --all 时的可选工具）
        if rep.path.is_some() && upg && (!optional_mark || all) {
            todo.push(spec);
        }
    }

    if check_only {
        if core_missing {
            eprintln!(
                "{}",
                if zh {
                    "\n核心依赖缺失（退出码 1）。去掉 --check 或加 --yes 可自动安装。"
                } else {
                    "\nCore dependencies missing (exit code 1). Re-run without --check (or with --yes) to auto-install."
                }
            );
            std::process::exit(1);
        }
        if todo.is_empty() {
            println!(
                "\n{}",
                if zh { "核心依赖齐全。" } else { "All core dependencies present." }
            );
        }
        return Ok(());
    }
    if todo.is_empty() {
        println!(
            "\n{}",
            if zh { "核心依赖齐全。" } else { "All core dependencies present." }
        );
        return Ok(());
    }

    let interactive = !yes && std::io::IsTerminal::is_terminal(&std::io::stdin());
    println!();
    for spec in todo {
        if interactive {
            use dialoguer::Confirm;
            let size = pick_asset(spec, platform_key().unwrap_or(""), want)
                .map(|(a, _)| a.size as f64 / (1024.0 * 1024.0))
                .unwrap_or(0.0);
            let ok = Confirm::new()
                .with_prompt(format!(
                    "{} {name}（约 {size:.0} MB）→ {dir}?",
                    if zh { "下载并安装" } else { "Download & install" },
                    name = spec.name,
                    size = size,
                    dir = td.display()
                ))
                .default(true)
                .interact()?;
            if !ok {
                continue;
            }
        }
        if let Err(e) = install(spec, want).await {
            eprintln!("✗ {}: {e:#}", spec.name);
        }
    }
    println!(
        "\n{}",
        if zh {
            format!("提示：重新运行 course2md setup 可复查；删除 {} 即完全卸载", td.display())
        } else {
            format!("Re-run course2md setup to verify; delete {} to uninstall everything", td.display())
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_key_is_known_on_test_platform() {
        assert!(platform_key().is_some());
    }

    #[test]
    fn llama_variant_selection() {
        let spec = spec_by_name("llama-server").unwrap();
        let (accel, exact) = pick_asset(spec, "linux-x86_64", Variant::Accel).unwrap();
        assert!(exact && accel.url.ends_with("vulkan-x64.tar.gz"));
        let (cpu, exact) = pick_asset(spec, "linux-x86_64", Variant::Cpu).unwrap();
        assert!(exact && cpu.url.ends_with("bin-ubuntu-x64.tar.gz"));
        // linux-aarch64 只有 cpu 变体：要 accel 回落 cpu 且标记不精确
        let (fallback, exact) = pick_asset(spec, "linux-aarch64", Variant::Accel).unwrap();
        assert!(!exact && fallback.variant == Variant::Cpu);
    }

    #[test]
    fn ytdlp_macos_asset_covers_both_arches() {
        let spec = spec_by_name("yt-dlp").unwrap();
        for key in ["macos-aarch64", "macos-x86_64"] {
            let (a, _) = pick_asset(spec, key, Variant::Auto).unwrap();
            assert!(a.url.ends_with("yt-dlp_macos"));
        }
    }

    #[test]
    fn windows_exe_naming() {
        // 本测试在非 Windows 平台也验证规则：tool_exe 只在 cfg!(windows) 时补后缀
        let n = tool_exe("ffmpeg");
        if cfg!(windows) {
            assert_eq!(n, "ffmpeg.exe");
        } else {
            assert_eq!(n, "ffmpeg");
        }
    }

    #[test]
    fn specs_cover_required_set() {
        // 任何运行至少需要 ffmpeg + ffprobe
        let s = specs_for_run(AsrProvider::Api, false);
        assert!(s.iter().any(|x| x.name == "ffmpeg"));
        assert!(s.iter().any(|x| x.name == "ffprobe"));
        assert!(!s.iter().any(|x| x.name == "yt-dlp"));
        assert!(!s.iter().any(|x| x.name == "llama-server"));
        // gpu 需要 llama-server；URL 输入需要 yt-dlp
        let s = specs_for_run(AsrProvider::Gpu, true);
        assert!(s.iter().any(|x| x.name == "yt-dlp"));
        assert!(s.iter().any(|x| x.name == "llama-server"));
        // npu 需要 uv
        let s = specs_for_run(AsrProvider::Npu, false);
        assert!(s.iter().any(|x| x.name == "uv"));
    }

    #[test]
    fn all_assets_have_sha256_and_unique_platform_variant() {
        for spec in SPECS {
            let mut seen = std::collections::HashSet::new();
            for a in spec.assets {
                assert_eq!(a.sha256.len(), 64, "{} asset sha256 malformed", spec.name);
                assert!(!a.url.is_empty());
                assert!(
                    seen.insert((a.key, a.variant.as_str())),
                    "{}/{} duplicate platform+variant",
                    spec.name,
                    a.key
                );
            }
            assert!(!spec.assets.is_empty());
        }
    }
}
