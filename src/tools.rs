//! Where external tools (ffmpeg, ffprobe, aria2c) are run from.
//!
//! Desktop archives carry their own ffmpeg and ffprobe beside the engine, so
//! a downloaded app works without a separate install. The search is:
//! `COURSE2MD_<NAME>` (an explicit binary) → the app package's own
//! directories ([`bundled_dirs_for`]) → PATH. Package managers that supply
//! system tools (Homebrew formula, AUR) simply leave the bundled copies out,
//! and PATH is used.
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

/// The desktop executable: an app package keeps it beside the engine, which
/// tells a package's own directory apart from one the CLI shares with other
/// programs (`/usr/local/bin`, Homebrew's `bin`).
const DESKTOP: &str = "course2md-desktop";

/// Directories that hold tools shipped with the app whose executable is
/// `exe`: none outside an app package. A macOS bundle also has
/// `Contents/Resources/bin`, looked at only from inside `Contents/MacOS`.
pub fn bundled_dirs_for(exe: &Path) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    if !dir.join(file_name(DESKTOP)).is_file() {
        return Vec::new();
    }
    let mut dirs = vec![dir.to_path_buf()];
    let in_bundle = dir.file_name() == Some(OsStr::new("MacOS"))
        && dir.parent().and_then(Path::file_name) == Some(OsStr::new("Contents"));
    if in_bundle {
        dirs.push(dir.join("../Resources/bin"));
    }
    dirs.push(dir.join(LLAMA_DIR));
    dirs
}

/// [`bundled_dirs_for`] the running executable.
pub fn bundled_dirs() -> Vec<PathBuf> {
    std::env::current_exe()
        .map(|exe| bundled_dirs_for(&exe))
        .unwrap_or_default()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn search_path(path: Option<&OsStr>) -> OsString {
    path.map(OsStr::to_os_string)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default()
}

/// How a tool at `path` was found: named by `override_var`, shipped in the
/// app package, or installed on the system. The app's search path lists the
/// package's directories first, so whether a directory is on PATH says
/// nothing about where a tool came from.
pub fn origin_of(path: &Path, override_var: &str) -> Origin {
    if std::env::var_os(override_var).is_some_and(|value| Path::new(&value) == path) {
        return Origin::Override;
    }
    match path.parent() {
        Some(dir) if bundled_dirs().iter().any(|bundled| same_dir(bundled, dir)) => Origin::Bundled,
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
        return Some(Tool {
            path,
            origin: Origin::Bundled,
        });
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
    fn a_bundled_copy_stays_bundled_when_the_search_path_lists_its_directory() {
        // The desktop puts the package directory first on the engine's PATH.
        let package = tempfile::tempdir().unwrap();
        make_exec(package.path(), "c2md-test-tool");
        let tool = locate_with(
            "c2md-test-tool",
            &[package.path().to_path_buf()],
            package.path().as_os_str(),
        )
        .unwrap();
        assert_eq!(tool.origin, Origin::Bundled);
    }

    #[test]
    fn only_an_app_package_has_bundled_directories() {
        // The CLI in /usr/local/bin shares that directory with other programs.
        let shared = tempfile::tempdir().unwrap();
        let cli = make_exec(shared.path(), "course2md");
        assert!(bundled_dirs_for(&cli).is_empty());
        make_exec(shared.path(), "course2md-desktop");
        assert_eq!(
            bundled_dirs_for(&cli),
            vec![shared.path().to_path_buf(), shared.path().join(LLAMA_DIR)]
        );
    }

    #[test]
    fn resources_bin_counts_only_inside_a_macos_bundle() {
        let root = tempfile::tempdir().unwrap();
        let macos = root.path().join("course2md.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        make_exec(&macos, "course2md-desktop");
        let cli = make_exec(&macos, "course2md");
        assert!(bundled_dirs_for(&cli).contains(&macos.join("../Resources/bin")));
        // Elsewhere ../Resources/bin is an arbitrary directory (/tmp/Resources/bin).
        let loose = root.path().join("bin");
        std::fs::create_dir_all(&loose).unwrap();
        make_exec(&loose, "course2md-desktop");
        let cli = make_exec(&loose, "course2md");
        assert!(!bundled_dirs_for(&cli).contains(&loose.join("../Resources/bin")));
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
