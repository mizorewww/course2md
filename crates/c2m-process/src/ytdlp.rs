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
    let home = crate::paths::home_dir();
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

/// Parses a yt-dlp release version (`YYYY.MM.DD`) for comparison and feature gates.
pub fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<u32> = version
        .trim()
        .split(['.', '-'])
        .take(3)
        .filter_map(|part| part.parse().ok())
        .collect();
    (parts.len() == 3).then(|| (parts[0], parts[1], parts[2]))
}

/// Runs `path --version` at most once in this process and caches its parsed version.
/// Returns `None` when `path` cannot run `--version` cleanly (for example, a PyInstaller
/// binary rejected by macOS code signing); `Some(None)` when it runs without a standard
/// `YYYY.MM.DD` string (such as a test stub).
fn probe_version(path: &Path) -> Option<Option<(u32, u32, u32)>> {
    type VersionMap = std::collections::BTreeMap<PathBuf, Option<Option<(u32, u32, u32)>>>;
    static PROBED: std::sync::Mutex<VersionMap> =
        std::sync::Mutex::new(std::collections::BTreeMap::new());
    if let Some(cached) = PROBED
        .lock()
        .ok()
        .and_then(|probed| probed.get(path).copied())
    {
        return cached;
    }
    let mut command = std::process::Command::new(path);
    command.arg("--version");
    let result = crate::runtime::run_ok(&mut command, std::time::Duration::from_secs(30))
        .map(|out| parse_version(&out.stdout));
    if let Ok(mut probed) = PROBED.lock() {
        probed.insert(path.to_path_buf(), result);
    }
    result
}

/// `--js-runtimes` arrived in yt-dlp 2025.11.12. Reuses the cached `--version` probe.
pub fn supports_js_runtimes(program: &Path) -> bool {
    probe_version(program)
        .flatten()
        .is_some_and(|version| version >= (2025, 11, 12))
}

/// Finds an installed JavaScript runtime (`deno`, `node` or `bun`) across `path`,
/// common package-manager directories and runtime homes (`~/.deno/bin`, `~/.bun/bin`),
/// so Finder-launched `.app` sessions can pass an explicit `--js-runtimes name:path`.
pub fn locate_js_runtime(path: Option<&OsStr>) -> Option<(&'static str, PathBuf)> {
    let search = tools::search_path(path);
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut extra = fallback_dirs();
    if let Some(home) = crate::paths::home_dir() {
        extra.push(home.join(".deno/bin"));
        extra.push(home.join(".bun/bin"));
    }
    for name in ["deno", "node", "bun"] {
        if let Some(found) = which::which_in_all(name, Some(&search), &cwd)
            .into_iter()
            .flatten()
            .chain(extra.iter().map(|dir| dir.join(tools::file_name(name))))
            .find(|candidate| tools::is_executable(candidate))
        {
            return Some((name, found));
        }
    }
    None
}

/// 按优先级返回可运行的 yt-dlp：`COURSE2MD_YTDLP` 显式覆盖始终最优先；否则在随包自带、
/// PATH 与常见安装目录中定位可运行候选，且当用户系统安装了比随包版本更新的正式版
/// yt-dlp 时优先使用更新版本（避免旧版提取器因平台改版读不到字幕或元数据）。
pub fn resolve_in(path: Option<&OsStr>) -> Option<PathBuf> {
    resolve_with(
        &bundled_dirs(),
        &tools::search_path(path),
        &fallback_dirs(),
        std::env::var_os("COURSE2MD_TOOL_PATH").is_some_and(|v| !v.is_empty()),
    )
}

fn resolve_with(
    bundled: &[PathBuf],
    search: &OsStr,
    fallbacks: &[PathBuf],
    pinned_tool_path: bool,
) -> Option<PathBuf> {
    let found = tools::candidates(NAME, ENV_OVERRIDE, bundled, search);
    let elsewhere = fallbacks
        .iter()
        .map(|dir| dir.join(tools::file_name(NAME)))
        .filter(|candidate| tools::is_executable(candidate))
        .map(|path| tools::Tool {
            path,
            origin: tools::Origin::System,
        });
    let mut seen = std::collections::BTreeSet::new();
    let mut first_bundled: Option<(PathBuf, (u32, u32, u32))> = None;
    for tool in found.into_iter().chain(elsewhere) {
        if !seen.insert(tool.path.clone()) {
            continue;
        }
        let Some(version) = probe_version(&tool.path) else {
            continue;
        };
        if tool.origin == tools::Origin::Override || pinned_tool_path {
            return Some(tool.path);
        }
        match &first_bundled {
            None => match (tool.origin, version) {
                (tools::Origin::Bundled, Some(ver)) => first_bundled = Some((tool.path, ver)),
                _ => return Some(tool.path),
            },
            Some((_, bundled_ver)) => {
                if version.is_some_and(|candidate_ver| candidate_ver > *bundled_ver) {
                    return Some(tool.path);
                }
                if tool.origin != tools::Origin::Bundled {
                    // Checked the first runnable system candidate; keep bundled when >= system.
                    break;
                }
            }
        }
    }
    first_bundled.map(|(path, _)| path)
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

    #[cfg(unix)]
    #[test]
    fn newer_system_ytdlp_beats_stale_bundled_copy() {
        use std::os::unix::fs::PermissionsExt;
        let versioned = |dir: &Path, ver: &str| {
            let path = dir.join("yt-dlp");
            std::fs::write(&path, format!("#!/bin/sh\necho {ver}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let old_bundled = tempfile::tempdir().unwrap();
        let new_system = tempfile::tempdir().unwrap();
        let old_system = tempfile::tempdir().unwrap();
        let bundled_path = versioned(old_bundled.path(), "2026.08.19");
        let newer_path = versioned(new_system.path(), "2026.10.26");
        let older_path = versioned(old_system.path(), "2026.05.01");

        assert_eq!(
            resolve_with(
                &[old_bundled.path().to_path_buf()],
                new_system.path().as_os_str(),
                &[],
                false,
            ),
            Some(newer_path),
        );
        assert_eq!(
            resolve_with(
                &[old_bundled.path().to_path_buf()],
                old_system.path().as_os_str(),
                &[],
                false,
            ),
            Some(bundled_path),
        );
        let _ = older_path;
    }
}
