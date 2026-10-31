//! Where the program keeps its files, and path spellings that stay usable on Windows.

use std::path::{Path, PathBuf};

/// 用户主目录：HOME，Windows 上 HOME 可能缺席时退回 USERPROFILE。
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// 配置目录（XDG_CONFIG_HOME / Windows %APPDATA% / ~/.config）。
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(d).join("course2md");
    }
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("APPDATA") {
            return PathBuf::from(d).join("course2md");
        }
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("course2md")
}

/// 缓存目录（模型等）。
pub fn cache_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(d).join("course2md");
    }
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(d).join("course2md");
        }
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cache")
        .join("course2md")
}

pub fn model_dir_from(opt: Option<&Path>) -> PathBuf {
    opt.map(|p| expand_tilde(p.to_path_buf()))
        .unwrap_or_else(|| cache_dir().join("models"))
}

/// 展开 `~` / `~/...`（仅 Unix 主目录约定；无 HOME 时原样返回）。
/// 防止配置里的 "~/cache" 真的在当前目录创建名为 `~` 的子目录。
pub fn expand_tilde(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    if s == "~" {
        if let Some(h) = home_dir() {
            return h;
        }
    } else if let Some(rest) = s
        .strip_prefix("~/")
        .or_else(|| s.strip_prefix(r"~\").filter(|_| cfg!(windows)))
        && let Some(h) = home_dir()
    {
        return h.join(rest);
    }
    p
}

/// `fs::canonicalize` without Windows' verbatim prefix where it is not needed.
///
/// On Windows the canonical form is `\\?\C:\...` (or `\\?\UNC\server\share\...`).
/// Such a path is stored with the library, shown to people, and handed to
/// ffmpeg, yt-dlp and the shell, many of which reject it. The prefix is kept
/// only when the plain spelling would mean something else: a path past the
/// classic 260-character limit, or a component Win32 would alter (a trailing
/// dot or space, a reserved device name such as `CON`). Elsewhere this is
/// `fs::canonicalize`.
pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(simplify_verbatim)
}

/// The non-verbatim spelling of a canonical path, when it names the same file
/// (see [`canonicalize`]); other paths are returned unchanged.
pub fn simplify_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        let plain = match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::VerbatimDisk(drive) => format!("{}:", drive as char),
                Prefix::VerbatimUNC(server, share) => format!(
                    r"\\{}\{}",
                    server.to_string_lossy(),
                    share.to_string_lossy()
                ),
                _ => return path,
            },
            _ => return path,
        };
        let mut out = PathBuf::from(format!(r"{plain}\"));
        for component in components {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let Some(name) = name.to_str() else {
                        return path;
                    };
                    if name.ends_with(['.', ' ']) || windows_reserved_name(name) {
                        return path;
                    }
                    out.push(name);
                }
                _ => return path,
            }
        }
        if out.as_os_str().len() >= 260 {
            return path;
        }
        out
    }
    #[cfg(not(windows))]
    path
}

/// Whether Win32 maps a file name to a device (`CON`, `nul.txt`, `COM1.md`):
/// the stem before the first dot, ignoring case and trailing spaces, is
/// reserved. Such a name cannot be created as an ordinary file on Windows.
pub fn windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let mut chars = upper.chars();
            let last = chars.next_back();
            matches!(chars.as_str(), "COM" | "LPT")
                && matches!(last, Some('1'..='9' | '¹' | '²' | '³'))
        }
    }
}
