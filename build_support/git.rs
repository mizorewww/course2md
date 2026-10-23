//! Build provenance shared by `build.rs` and `desktop/build.rs` (`#[path]` include).
//!
//! Reads `.git/HEAD` and the ref it names directly, so neither build script
//! needs a git binary or a git library. Handles linked worktrees, whose `.git`
//! is a `gitdir:` pointer file and whose branch refs live in the common gitdir.

use std::path::{Path, PathBuf};

/// The 12-character commit prefix of `HEAD` in the checkout at `root`, and
/// `cargo:rerun-if-changed` lines for the files that move when HEAD does.
///
/// Returns `None` outside a git checkout, so source archives embed a fallback
/// instead of attributing an enclosing repository's commit.
pub fn git_commit(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let gitdir = if dot_git.is_dir() {
        dot_git
    } else {
        // A linked worktree's .git is a pointer file: "gitdir: <private gitdir>".
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        let path = PathBuf::from(pointer.trim().strip_prefix("gitdir:")?.trim());
        if path.is_absolute() { path } else { root.join(path) }
    };
    // Branch refs and packed-refs live in the common gitdir; for a linked
    // worktree, `commondir` names it relative to the private gitdir.
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(relative) => gitdir.join(relative.trim()),
        Err(_) => gitdir.clone(),
    };
    let head_file = gitdir.join("HEAD");
    watch(&head_file);
    let head = std::fs::read_to_string(&head_file).ok()?;
    let head = head.trim();
    let hash = match head.strip_prefix("ref:").map(str::trim) {
        None => head.to_owned(),
        Some(name) => resolve_ref(&gitdir, &common, name)?,
    };
    let valid = hash.len() >= 12 && hash.bytes().all(|b| b.is_ascii_hexdigit());
    valid.then(|| hash[..12].to_owned())
}

/// Resolve a symbolic ref to its hash: a loose ref file (per-worktree first,
/// then common) or else its line in `packed-refs`.
fn resolve_ref(gitdir: &Path, common: &Path, name: &str) -> Option<String> {
    for base in [gitdir, common] {
        let loose = base.join(name);
        if let Ok(hash) = std::fs::read_to_string(&loose) {
            watch(&loose);
            return Some(hash.trim().to_owned());
        }
    }
    // A commit writes a new loose ref beside the packed one: watch its directory.
    if let Some(dir) = common.join(name).parent().filter(|dir| dir.is_dir()) {
        watch(dir);
    }
    let packed = common.join("packed-refs");
    watch(&packed);
    std::fs::read_to_string(&packed)
        .ok()?
        .lines()
        .find_map(|line| {
            let (hash, reference) = line.split_once(' ')?;
            (reference == name).then(|| hash.to_owned())
        })
}

fn watch(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
