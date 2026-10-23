//! Scaffolding shared by the integration test binaries (`mod common;`).
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

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

/// Write a `secs`-second flat blue 320x240 1 fps video with lavfi, so no media
/// fixture is committed. Requires ffmpeg; call [`ffmpeg_available`] first.
pub fn fixture_video(path: &Path, secs: u32) {
    let generated = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args(["-i", "color=c=blue:s=320x240:r=1", "-t", &secs.to_string()])
        .args(["-pix_fmt", "yuv420p"])
        .arg(path)
        .output()
        .expect("spawn ffmpeg");
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
}

/// Request markers and canned responses of the fake AI service, shared with
/// `desktop/scripts/ux_fixture.py` so both fakes answer with the same shapes.
pub fn fake_ai() -> serde_json::Value {
    serde_json::from_str(include_str!("../fixtures/fake_ai.json")).expect("valid fixture")
}
