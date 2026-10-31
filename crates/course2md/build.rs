//! The commit shown by `--version`, and what the Apple speech module needs here.

#[path = "../../build_support/git.rs"]
mod git;

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let commit = git::git_commit(&manifest.join("../..").canonicalize().unwrap());
    println!(
        "cargo:rustc-env=COURSE2MD_COMMIT_HASH={}",
        commit.as_deref().unwrap_or("unknown")
    );
    if std::env::var_os("DEP_C2M_APPLE_ASR_APPLE_NATIVE").is_some() {
        println!("cargo:rustc-cfg=apple_native");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
