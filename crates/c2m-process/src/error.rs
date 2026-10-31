pub use crate::invalid::{
    InvalidConfiguration, InvalidInput, invalid_configuration, invalid_input,
};
use anyhow::Result;

/// 取多行文本的最后 n 行（保持原有先后顺序），供错误摘要附带日志尾部。
pub fn tail_lines(s: &str, n: usize) -> String {
    s.lines()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

/// 子进程失败时附带 stderr 摘要的便捷构造。
pub fn cmd_error(program: &str, code: Option<i32>, stderr: &str) -> anyhow::Error {
    let tail = tail_lines(stderr, 5);
    anyhow::anyhow!("{program} 运行失败 / failed (code={code:?}):\n{tail}")
}

/// 校验 PATH 上的外部工具存在（yt-dlp 请用 [`require_ytdlp`]）。
pub fn require_cmd(cmd: &str) -> Result<()> {
    if crate::tools::locate(cmd).is_none() {
        return Err(missing_tool(cmd));
    }
    Ok(())
}

/// 校验 yt-dlp 可用（随包自带 / PATH / 常见安装目录，见 [`crate::ytdlp::resolve`]）。
pub fn require_ytdlp() -> Result<()> {
    if crate::ytdlp::resolve().is_none() {
        return Err(missing_tool("yt-dlp"));
    }
    Ok(())
}

/// A required external program is not installed.
#[derive(Debug, thiserror::Error)]
#[error("未找到 {tool}，请先安装 / {tool} is not installed. {}", install_hint(.tool))]
pub struct MissingTool {
    pub tool: String,
}

fn missing_tool(cmd: &str) -> anyhow::Error {
    anyhow::Error::new(MissingTool {
        tool: cmd.to_owned(),
    })
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
