//! yt-dlp 二进制定位：`COURSE2MD_YTDLP` 覆盖 → 可执行文件旁（随包自带）→
//! PATH → 常见安装目录兜底。CLI（fetch/pipeline/doctor）与桌面端（环境检测、
//! 在线读取）共用同一份清单，保证「检测到可用」与「实际能启动」不互相矛盾。
//! 常见安装目录是因为 Finder/桌面会话启动的进程拿不到登录 shell 的 PATH。

use crate::tools;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// 显式指定 yt-dlp 二进制的环境变量；指向不可执行文件时忽略并继续按清单搜索。
pub const ENV_OVERRIDE: &str = "COURSE2MD_YTDLP";

const NAME: &str = "yt-dlp";

/// 随包自带/同目录：桌面安装包的 `yt-dlp/` 目录（onedir 构建，见
/// [`tools::bundled_dirs`]）、exe 旁、`.app` 的 Contents/Resources[/bin]。
fn bundled_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = tools::bundled_dirs()
        .into_iter()
        .filter(|dir| dir.file_name() == Some(OsStr::new(tools::YTDLP_DIR)))
        .collect();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.extend([
            dir.to_path_buf(),
            dir.join("../Resources"),
            dir.join("../Resources/bin"),
        ]);
    }
    dirs
}

/// `base` 下形如 `3.x`/`Python3xx` 的解释器目录：macOS `~/Library/Python/3.*/bin`
/// 与 Windows `Python3*/Scripts` 同为 `pip --user` 的 yt-dlp 落点。
fn interpreter_dirs(base: &Path, leaf: &str, prefix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(base)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                .map(|e| e.path().join(leaf))
                .collect()
        })
        .unwrap_or_default()
}

/// PATH 之外的常见安装位置：覆盖「终端里有、桌面会话里没有」的安装方式。
fn fallback_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = crate::config::home_dir();
    #[cfg(unix)]
    {
        if let Some(home) = &home {
            for rel in [
                ".local/bin", // pipx / uv tool / pip --user（Linux）
                ".local/share/mise/shims",
                ".asdf/shims",
                ".nix-profile/bin",
                "anaconda3/bin",
                "miniconda3/bin",
                "miniforge3/bin",
                "mambaforge/bin",
                "opt/anaconda3/bin",
                "opt/miniconda3/bin",
            ] {
                dirs.push(home.join(rel));
            }
            dirs.extend(interpreter_dirs(&home.join("Library/Python"), "bin", "3."));
        }
        for abs in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/opt/local/bin", // MacPorts
            "/opt/anaconda3/bin",
            "/opt/miniconda3/bin",
            "/nix/var/nix/profiles/default/bin",
        ] {
            dirs.push(PathBuf::from(abs));
        }
    }
    #[cfg(windows)]
    {
        let var = |name| std::env::var_os(name).map(PathBuf::from);
        if let Some(local) = var("LOCALAPPDATA") {
            dirs.push(local.join(r"Microsoft\WinGet\Links"));
            dirs.extend(interpreter_dirs(
                &local.join(r"Programs\Python"),
                "Scripts",
                "Python3",
            ));
        }
        if let Some(home) = &home {
            dirs.push(home.join(r"scoop\shims"));
        }
        if let Some(data) = var("ProgramData") {
            dirs.push(data.join(r"chocolatey\bin"));
        }
        if let Some(roaming) = var("APPDATA") {
            dirs.extend(interpreter_dirs(
                &roaming.join("Python"),
                "Scripts",
                "Python3",
            ));
        }
    }
    dirs
}

/// A candidate that exits successfully for `--version`. A bundled one-file
/// PyInstaller build can be present and executable yet fail after macOS signing
/// (the extracted Python has a different Team ID); treat that as unavailable and
/// continue to the user's PATH/Homebrew installation.
///
/// A candidate that ran once is not run again in this process: every preview
/// and download resolves yt-dlp, and each check is a full Python start. The
/// limit allows for the first run after an install, which macOS spends
/// scanning the new code; a broken build fails long before it.
fn runs(path: &Path) -> bool {
    static RAN: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    if RAN.lock().is_ok_and(|ran| ran.contains(path)) {
        return true;
    }
    let mut command = std::process::Command::new(path);
    command.arg("--version");
    let ok =
        crate::runtime::run_bounded("yt-dlp", &mut command, std::time::Duration::from_secs(30))
            .is_ok_and(|output| output.status.success());
    if ok && let Ok(mut ran) = RAN.lock() {
        ran.insert(path.to_path_buf());
    }
    ok
}

/// 按优先级返回第一个存在、可执行且能运行 `--version` 的 yt-dlp。
/// `path` 为 None 时用进程继承的 PATH；桌面端传入 `tool_path()` 的扩展版本。
pub fn resolve_in(path: Option<&OsStr>) -> Option<PathBuf> {
    let found = tools::candidates(
        NAME,
        ENV_OVERRIDE,
        &bundled_dirs(),
        &tools::search_path(path),
    );
    let elsewhere = fallback_dirs()
        .into_iter()
        .map(|dir| dir.join(tools::file_name(NAME)));
    let mut seen = std::collections::BTreeSet::new();
    found
        .into_iter()
        .map(|tool| tool.path)
        .chain(elsewhere.filter(|path| tools::is_executable(path)))
        .filter(|candidate| seen.insert(candidate.clone()))
        .find(|candidate| runs(candidate))
}

/// 继承 PATH 的版本（CLI/终端调用）。
pub fn resolve() -> Option<PathBuf> {
    resolve_in(None)
}

/// 给 `Command::new` 的程序名：解析成功为绝对路径，否则退回裸名
///（`require_cmd` 会在更早处给出安装提示）。
pub fn program() -> PathBuf {
    resolve().unwrap_or_else(|| PathBuf::from(tools::file_name(NAME)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_exec(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn interpreter_dirs_filters_by_prefix() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("3.12/bin")).unwrap();
        std::fs::create_dir_all(root.path().join("2.7/bin")).unwrap();
        let dirs = interpreter_dirs(root.path(), "bin", "3.");
        assert_eq!(dirs, vec![root.path().join("3.12/bin")]);
    }

    #[cfg(unix)]
    #[test]
    fn broken_first_path_entry_falls_back_to_a_runnable_one() {
        use std::os::unix::fs::PermissionsExt;
        let broken = tempfile::tempdir().unwrap();
        let working = tempfile::tempdir().unwrap();
        let broken_path = broken.path().join("yt-dlp");
        std::fs::write(&broken_path, b"#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&broken_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let working_path = make_exec(working.path(), "yt-dlp");
        let path = std::env::join_paths([broken.path(), working.path()]).unwrap();
        assert_eq!(
            resolve_in(Some(path.as_os_str())),
            Some(working_path),
            "a broken bundled/earlier candidate must not hide a working installation"
        );
    }
}
