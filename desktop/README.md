# course2md desktop development

Native client for macOS, Windows and Linux, built on GPUI Kit and `gpui-kit-material`. It runs the CLI from the same package as its conversion engine and shares the CLI configuration file. Users install it as described in the [README](../readme.md).

## Build and run

Requires the Rust toolchain named in `rust-toolchain.toml` and Python 3.11+ for the scripts; on macOS, Xcode with its Metal toolchain for the Apple speech module (Command Line Tools alone build a development engine without it); Visual Studio C++ Build Tools on Windows; on Linux, the packages listed in `.github/actions/desktop-deps/action.yml`. Converting videos needs ffmpeg, and links need yt-dlp.

```sh
cargo build
cargo build --manifest-path desktop/Cargo.toml
cargo run --manifest-path desktop/Cargo.toml
```

`just install` (macOS) installs the current checkout as `/Applications/course2md.app`, keeping existing notes and settings.

The UI stack comes from [`gpui-kit-material`](https://github.com/mizorewww/gpui-kit-material), which vendors patched GPUI and GPUI Base sources. `desktop/Cargo.toml` pins one revision of it in the dependencies, and the root `Cargo.toml` in a `[patch]` table holding only what the build needs; bump them together.

The app looks for the engine in `COURSE2MD_BIN`, then beside its own executable, then in a debug build `target/` of this repository, then on PATH.

## Test

CI runs `.github/workflows/ci.yml` and `desktop.yml`. UI changes follow the [design skill](../.agents/skills/course2md-design/SKILL.md) and are verified in the running app. Use a separate `XDG_CONFIG_HOME` so tests do not touch your own configuration.

For native UX checks on macOS, `desktop/scripts/ux_fixture.py` builds an isolated validation app with its own configuration, synthetic media and a local fake AI service. Synthetic `BV1UX…` links and fault modes come from `ux_ytdlp.py` and `ux_ffmpeg.py`; every other request uses the real tools.

```sh
cargo build && cargo build --manifest-path desktop/Cargo.toml
python3 desktop/scripts/ux_fixture.py prepare
python3 desktop/scripts/ux_fixture.py serve &   # fake OpenAI-compatible service
python3 desktop/scripts/ux_fixture.py bundle --label check1
```

The fake service answers from [`crates/course2md/tests/fixtures/fake_ai.json`](../crates/course2md/tests/fixtures/fake_ai.json), which the engine's task tests also use. It recognizes the service-test requests by phrases quoted from `desktop/src/service_test.rs`; keep the two in step.

Test-only environment variables:

| Variable | Effect |
| --- | --- |
| `COURSE2MD_VALIDATION_WINDOW`, `_PANIC_LOG`, `_OPEN_NOTE`, `_READER` | Debug builds: initial window size, panic file, a note to open on launch and its reader panels. See `src/main.rs` and `src/a11y.rs`. |
| `COURSE2MD_CAPTURE` | `--features capture`: render pages to PNG files (`page=path.png;…`) and quit, with no screen-recording permission. See `src/capture.rs`. |
| `COURSE2MD_TOOL_PATH` | Search path for ffmpeg, yt-dlp and other tools, replacing the bundled directory and PATH. |
| `COURSE2MD_PERFORMANCE_LOG`, `COURSE2MD_FRAME_TRACE`, `COURSE2MD_MOTION_TRACE=1` | `--features performance`: frame and input-latency histograms, GPUI's frame trace, animation events. See `src/performance.rs`. |

## Package

```sh
python3 -m pip install -r desktop/scripts/requirements-packaging.txt
python3 desktop/scripts/package.py --debug
```

`package.py --help` lists the switches; each defaults to the environment variable named there, which the release workflow sets. Output goes to `target/packages/`. Local builds are signed ad hoc. On Windows and Linux both executables must stay in the same folder. Releases are described in [docs/PACKAGING.md](../docs/PACKAGING.md).

## Notes for contributors

Long lists (reader article flow, task list, library grid) are virtualized. Keep reading-position restore, in-page find and image jumps working, and do no file or network IO while rendering.

Bundled artwork records its provenance beside it in [`assets/`](assets/).
