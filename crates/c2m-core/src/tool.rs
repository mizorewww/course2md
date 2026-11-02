//! External programs the engine runs.

/// A required external program is not installed.
#[derive(Debug, thiserror::Error)]
#[error("未找到 {tool}，请先安装 / {tool} is not installed. {}", install_hint(.tool))]
pub struct MissingTool {
    pub tool: String,
}

/// 按平台给一行安装命令（常见包管理器；ffprobe 随 ffmpeg 包提供）。
pub fn install_hint(cmd: &str) -> String {
    let pkg = if cmd == "ffprobe" { "ffmpeg" } else { cmd };
    let media_tool = matches!(cmd, "ffmpeg" | "ffprobe" | "yt-dlp");
    if cfg!(target_os = "macos") {
        if media_tool {
            "macOS: brew install ffmpeg yt-dlp".into()
        } else {
            format!("macOS: brew install {pkg}")
        }
    } else if cfg!(target_os = "windows") {
        format!("Windows: winget install {pkg}")
    } else {
        crate::tr!(
            "Debian/Ubuntu: sudo apt install {}；Arch: sudo pacman -S {}",
            pkg,
            pkg
        )
    }
}
