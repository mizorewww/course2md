//! Scaffolding shared by the integration test binaries (`mod common;`).
#![allow(dead_code)]

/// Whether every named tool is on PATH. Locally a missing tool skips the test
/// with a note; in CI (`CI` is set) it fails, so a green run means the test ran.
pub fn tools_available(names: &[&str]) -> bool {
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| course2md::runtime::which(name).is_none())
        .collect();
    if missing.is_empty() {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI runners must install {}; see the ffmpeg step in .github/workflows/ci.yml",
        missing.join(", ")
    );
    eprintln!("skip: {} not found", missing.join(", "));
    false
}

/// Whether ffmpeg and ffprobe are available (see [`tools_available`]).
pub fn ffmpeg_available() -> bool {
    tools_available(&["ffmpeg", "ffprobe"])
}
