#[path = "../build_support/git.rs"]
mod git;

fn main() {
    emit_commit();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/windows.rc");
        println!("cargo:rerun-if-changed=assets/icon.ico");
        // GPUI loads icon resource 1 for the window class. Embedding the same
        // resource also gives Explorer and shortcuts the application icon.
        embed_resource::compile_for(
            "assets/windows.rc",
            ["course2md-desktop"],
            embed_resource::ParamsIncludeDirs(["assets"]),
        )
        .manifest_required()
        .expect("Windows application icon must be embedded");
    }
    // The shared core can link the native Apple speech module. A library's
    // rustc-link-arg is not inherited by its consumers, so each final executable
    // must supply the system Swift runtime search path.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}

fn emit_commit() {
    let desktop = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let commit = git::git_commit(desktop.parent().unwrap());
    println!(
        "cargo:rustc-env=COURSE2MD_DESKTOP_COMMIT={}",
        commit.as_deref().unwrap_or("")
    );
}
