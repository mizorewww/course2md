//! Where external tools (ffmpeg, ffprobe, aria2c) are run from.
//!
//! Desktop archives carry their own ffmpeg and ffprobe beside the engine, so
//! a downloaded app works without a separate install. The search is:
//! `COURSE2MD_<NAME>` (an explicit binary) → beside the executable (the
//! archive's copy; `.app` bundles also look in `../Resources/bin`) → PATH.
//! Package managers that supply system tools (Homebrew formula, AUR) simply
//! leave the bundled copies out, and PATH is used.
//!
//! yt-dlp has its own, wider search ([`crate::ytdlp`]) and reports its
//! origin through [`origin_of`].

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Where a tool was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Named by an environment variable such as `COURSE2MD_FFMPEG`.
    Override,
    /// Shipped with the app, beside its executables.
    Bundled,
    /// Installed on the system and found on PATH.
    System,
}

impl Origin {
    /// A short localized description, for `doctor` and the desktop checks.
    pub fn label(self) -> &'static str {
        match self {
            Origin::Override => crate::tr!("由环境变量指定"),
            Origin::Bundled => crate::tr!("随应用附带"),
            Origin::System => crate::tr!("系统安装"),
        }
    }
}

/// A located tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    pub path: PathBuf,
    pub origin: Origin,
}

fn file_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// `COURSE2MD_FFMPEG` for `ffmpeg`, `COURSE2MD_ARIA2C` for `aria2c`.
pub fn override_var(name: &str) -> String {
    format!("COURSE2MD_{}", name.to_ascii_uppercase().replace('-', "_"))
}

/// Exists and is executable (Windows: is a regular file).
fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Where Windows and Linux archives keep llama-server with its libraries,
/// relative to the executable.
pub const LLAMA_DIR: &str = "llama.cpp";

/// Directories that hold tools shipped with the app.
pub fn bundled_dirs() -> Vec<PathBuf> {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    vec![
        dir.clone(),
        dir.join("../Resources/bin"),
        dir.join(LLAMA_DIR),
    ]
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn on_path(dir: &Path, path: &OsStr) -> bool {
    std::env::split_paths(path).any(|entry| same_dir(&entry, dir))
}

fn search_path(path: Option<&OsStr>) -> OsString {
    path.map(OsStr::to_os_string)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default()
}

/// How a tool at `path` was found. A directory beside the executable that is
/// itself on PATH (`/usr/local/bin`, Homebrew's `bin`) is a system install:
/// the CLI shares it with every other program there.
pub fn origin_of(path: &Path, override_var: &str, search: Option<&OsStr>) -> Origin {
    if std::env::var_os(override_var).is_some_and(|value| Path::new(&value) == path) {
        return Origin::Override;
    }
    let search = search_path(search);
    match path.parent() {
        Some(dir)
            if bundled_dirs().iter().any(|bundled| same_dir(bundled, dir))
                && !on_path(dir, &search) =>
        {
            Origin::Bundled
        }
        _ => Origin::System,
    }
}

fn locate_with(name: &str, bundled: &[PathBuf], search: &OsStr) -> Option<Tool> {
    let var = override_var(name);
    if let Some(value) = std::env::var_os(&var).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if is_executable(&path) {
            return Some(Tool {
                path,
                origin: Origin::Override,
            });
        }
    }
    let file = file_name(name);
    if let Some(path) = bundled
        .iter()
        .map(|dir| dir.join(&file))
        .find(|path| is_executable(path))
    {
        let origin = match path.parent() {
            Some(dir) if on_path(dir, search) => Origin::System,
            _ => Origin::Bundled,
        };
        return Some(Tool { path, origin });
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    which::which_in(name, Some(search), cwd)
        .ok()
        .map(|path| Tool {
            path,
            origin: Origin::System,
        })
}

/// Finds `name` beside the app first, then on `path` (the process PATH when None).
pub fn locate_in(name: &str, path: Option<&OsStr>) -> Option<Tool> {
    locate_with(name, &bundled_dirs(), &search_path(path))
}

/// [`locate_in`] with the process PATH.
pub fn locate(name: &str) -> Option<Tool> {
    locate_in(name, None)
}

/// The program to hand `Command::new`: the located tool, or its bare name so
/// the spawn error names it (callers check [`crate::error::require_cmd`] first).
pub fn program(name: &str) -> PathBuf {
    locate(name).map_or_else(|| PathBuf::from(file_name(name)), |tool| tool.path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn make_exec(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn bundled_copy_comes_before_path() {
        let bundled = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let ours = make_exec(bundled.path(), "c2md-test-tool");
        make_exec(system.path(), "c2md-test-tool");
        let tool = locate_with(
            "c2md-test-tool",
            &[bundled.path().to_path_buf()],
            system.path().as_os_str(),
        )
        .unwrap();
        assert_eq!(
            tool,
            Tool {
                path: ours,
                origin: Origin::Bundled
            }
        );
    }

    #[test]
    fn falls_back_to_path_without_a_bundled_copy() {
        let bundled = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let theirs = make_exec(system.path(), "c2md-test-tool");
        let tool = locate_with(
            "c2md-test-tool",
            &[bundled.path().to_path_buf()],
            system.path().as_os_str(),
        )
        .unwrap();
        assert_eq!(tool.origin, Origin::System);
        assert!(same_dir(
            tool.path.parent().unwrap(),
            theirs.parent().unwrap()
        ));
    }

    #[test]
    fn a_directory_on_path_is_a_system_install() {
        // The CLI in /usr/local/bin finds the ffmpeg there: same file, but not ours.
        let shared = tempfile::tempdir().unwrap();
        make_exec(shared.path(), "c2md-test-tool");
        let tool = locate_with(
            "c2md-test-tool",
            &[shared.path().to_path_buf()],
            shared.path().as_os_str(),
        )
        .unwrap();
        assert_eq!(tool.origin, Origin::System);
    }

    #[test]
    fn a_non_executable_bundled_file_is_skipped() {
        let bundled = tempfile::tempdir().unwrap();
        std::fs::write(bundled.path().join("c2md-test-tool"), b"").unwrap();
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            locate_with(
                "c2md-test-tool",
                &[bundled.path().to_path_buf()],
                empty.path().as_os_str()
            ),
            None
        );
    }

    #[test]
    fn override_variable_names_follow_the_tool() {
        assert_eq!(override_var("ffmpeg"), "COURSE2MD_FFMPEG");
        assert_eq!(override_var("aria2c"), "COURSE2MD_ARIA2C");
    }
}
