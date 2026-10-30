# course2md desktop development

GPUI and GPUI Component client for macOS, Windows and Linux. It runs the CLI from the same package as its conversion engine and shares the CLI configuration file. Users install it as described in the [README](../readme.md).

## Build and run

Requires the Rust toolchain named in `desktop/rust-toolchain.toml`, Python 3.11+ and Git; Xcode Command Line Tools on macOS; Visual Studio C++ Build Tools and LLVM on Windows; on Linux, the packages listed in `.github/actions/desktop-deps/action.yml`. Converting videos needs ffmpeg, and links need yt-dlp.

```sh
cargo build
cargo build --manifest-path desktop/Cargo.toml
# Point the app at the engine you just built (PowerShell: $env:COURSE2MD_BIN)
COURSE2MD_BIN="$PWD/target/debug/course2md" cargo run --manifest-path desktop/Cargo.toml
```

`just install` (macOS) installs the current checkout as `/Applications/course2md.app`, keeping existing notes and settings. It finds a Python 3.11+ (macOS's own `python3` is too old) and builds the app.

The UI stack comes from [`gpui-kit-material`](https://github.com/mizorewww/gpui-kit-material), which vendors patched GPUI and GPUI Base sources. `desktop/Cargo.toml` repeats that repository's `[patch]` tables, because Cargo applies patches only in the workspace that declares them; keep the two equal when updating. While `docs/split/PLAN.md` is in progress the dependencies are paths into a sibling `gpui-kit-material-course2md` worktree of its `course2md-integration` branch.

The app looks for the engine in `COURSE2MD_BIN`, then beside its own executable, then on PATH.

## Test

CI runs the same checks (`.github/workflows/ci.yml` and `desktop.yml`). At minimum:

```sh
cargo test --lib
cargo test --manifest-path desktop/Cargo.toml
```

UI changes follow the [design skill](../.agents/skills/course2md-design/SKILL.md) and are verified in the running app. Use a separate `XDG_CONFIG_HOME` so tests do not touch your own configuration.

For native UX checks on macOS, `desktop/scripts/ux_fixture.py` builds an isolated validation app with its own configuration, synthetic media and a local fake AI service. Synthetic `BV1UX…` links and fault modes come from `ux_ytdlp.py` and `ux_ffmpeg.py`; every other request uses the real tools.

```sh
cargo build && cargo build --manifest-path desktop/Cargo.toml
python3 desktop/scripts/ux_fixture.py prepare
python3 desktop/scripts/ux_fixture.py serve &   # fake OpenAI-compatible service
python3 desktop/scripts/ux_fixture.py bundle --label check1
```

The fake service answers from [`tests/fixtures/fake_ai.json`](../tests/fixtures/fake_ai.json), which the engine's task tests also use. It recognizes the service-test requests by phrases quoted from `desktop/src/service_test.rs`; keep the two in step.

Test-only environment variables read by the desktop binary (the validation bundle sets the first three):

| Variable | Effect |
| --- | --- |
| `COURSE2MD_VALIDATION_WINDOW` | Debug builds: initial window size `WIDTHxHEIGHT`, at least 720×620. |
| `COURSE2MD_VALIDATION_PANIC_LOG` | Debug builds on Unix: append panics to this file. |
| `COURSE2MD_VALIDATION_OPEN_NOTE` | Debug builds: after the first library scan, open the note whose title or version directory contains this text. |
| `COURSE2MD_VALIDATION_READER` | Debug builds, with the note above: reader panels to show once it loads, comma-separated (`find`, `info`). Lets native layout be checked without synthetic input. |
| `COURSE2MD_CAPTURE` | Builds with `--features capture`: render pages to PNG files (`page=path.png;…`) and quit; see `src/capture.rs` for pages and the palette/scale overrides. Needs no screen-recording permission. |
| `COURSE2MD_TOOL_PATH` | Search path for ffmpeg, yt-dlp and other tools, replacing the bundled directory and PATH. |
| `COURSE2MD_PERFORMANCE_LOG` | Builds with `--features performance`: write per-second frame and input-latency histograms to this file. |
| `COURSE2MD_FRAME_TRACE` | With the performance log: also record GPUI's per-frame trace. |
| `COURSE2MD_MOTION_TRACE=1` | With the performance log: also record animation events. |

## Package

```sh
python3 -m pip install -r desktop/scripts/requirements-packaging.txt
python3 desktop/scripts/package.py --debug
```

`package.py --help` lists the switches for bundled tools, the Apple speech module, signing and notarization; each defaults to the environment variable named there, which the release workflow sets.

Output goes to `desktop/target/packages/`: on macOS a `.app` with the CLI and `mlx.metallib`, plus a DMG and ZIP; on Windows and Linux an archive holding both executables, which must stay in the same folder. Local builds are signed ad hoc; the release workflow signs, notarizes and adds licenses. The DMG layout is described in [`assets/dmg/`](assets/dmg/README.md) and the app icon source in [`assets/icon-design/`](assets/icon-design/README.md). On Windows, packaging fails if the `.exe` does not embed every size of `assets/icon.ico`.

Releases are described in [docs/PACKAGING.md](../docs/PACKAGING.md).

## Notes for contributors

- Long lists (reader article flow, task list, library grid) are virtualized. Keep reading-position restore, in-page find and image jumps working, and do no file or network IO while rendering.
- Icons are embedded SVGs: Lucide outline icons for shared navigation, actions and settings ([sources](assets/outline/SOURCES.md)), Material Icons Rounded for other meanings ([sources](assets/material/README.md)), and platform brand marks ([sources](assets/brands/SOURCES.md)). Theme palettes and their licenses are listed in [`assets/themes/`](assets/themes/SOURCES.md).
