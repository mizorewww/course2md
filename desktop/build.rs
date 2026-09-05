fn main() {
    emit_commit();
    // The shared core can link the native Apple speech module. A library's
    // rustc-link-arg is not inherited by its consumers, so each final executable
    // must supply the system Swift runtime search path.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}

fn emit_commit() {
    use std::{path::PathBuf, process::Command};

    let desktop = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = desktop.parent().unwrap();
    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        (!value.is_empty()).then_some(value)
    };
    // Archive builds have no repository provenance; omit the row instead of
    // attributing an enclosing repository's commit to this application.
    let mut commit = None;
    if root.join(".git").exists() {
        // A worktree's .git is a pointer file. Watching the repository
        // directory would also rebuild on index refreshes and unrelated refs.
        if root.join(".git").is_file() {
            println!("cargo:rerun-if-changed={}", root.join(".git").display());
        }
        let symbolic_ref = git(&["symbolic-ref", "--quiet", "HEAD"]);
        for name in [
            Some("HEAD"),
            symbolic_ref.as_deref(),
            Some("packed-refs"),
            Some("logs/HEAD"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(path) = git(&["rev-parse", "--git-path", name]) {
                let path = root.join(path);
                if path.is_file() {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
            }
        }
        commit = git(&["rev-parse", "--short=12", "HEAD"]);
    }
    println!(
        "cargo:rustc-env=COURSE2MD_DESKTOP_COMMIT={}",
        commit.as_deref().unwrap_or("")
    );
}
