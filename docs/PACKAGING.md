# Releases and package channels

| Channel | GUI | CLI |
| --- | --- | --- |
| Homebrew tap `mizorewww/tap` | Cask `course2md-gui` (Apple Silicon, macOS 15+) | Formula `course2md` |
| Homebrew prerelease channels | Cask `course2md-gui@{alpha,beta,rc}` | Formula `course2md-{alpha,beta,rc}` (keg-only) |
| AUR (stable only) | `course2md-gui-bin` (x86_64) | `course2md-bin` (x86_64, aarch64) |

GUI packages contain a matching private CLI engine and do not install a terminal `course2md` command, so they coexist with the CLI package.

Desktop archives (DMG/app ZIP, Windows ZIP, Linux tar.gz) are self-contained: they bundle ffmpeg and ffprobe, the yt-dlp version pinned in `desktop/scripts/package.py`, and on Windows the official aria2 build pinned there. Windows and Linux archives also bundle the official uv (beside the engine; it runs the Intel NPU worker and fetches OpenVINO on first use) and llama.cpp's Vulkan `llama-server` with its CPU and Vulkan backends (in `llama.cpp/`), pinned in `desktop/scripts/prebuilt.py`; CUDA, ROCm and SYCL builds of llama.cpp remain a separate install. On Linux the bundled llama-server needs the system's `libvulkan.so.1`, `libgomp.so.1` and OpenSSL 3; on Windows it needs the Microsoft Visual C++ runtime, as the app does. The engine runs a tool beside its executable before one on PATH (`COURSE2MD_FFMPEG`, `COURSE2MD_FFPROBE` and `COURSE2MD_YTDLP` name a binary explicitly), and `course2md doctor` and **Run checks** show where each came from. ffmpeg is LGPL throughout, pinned in `desktop/scripts/ffmpeg_bundle.py`: macOS arm64 and Linux x86_64 build the upstream release and dav1d from source with autodetection off, and Windows x64 uses BtbN's LGPL shared build from a monthly autobuild (kept for two years). Release builds stop if a tool cannot be bundled; `--debug` builds fall back to PATH. The macOS source build needs `meson`, `ninja` and `pkg-config` from `requirements-packaging.txt`; Linux also needs `nasm`. CI caches built tools in `desktop/.cache/tools`. aria2 is not bundled on macOS or Linux, where upstream publishes no build; without it models download over one connection.

The Homebrew formula and both AUR packages depend on the system ffmpeg, yt-dlp and aria2; the AUR GUI package installs only the app binaries and yt-dlp from the archive, so it uses Arch's ffmpeg. The Homebrew cask installs the app as built, bundled ffmpeg included, so it depends only on yt-dlp and aria2.

## Publishing a version

1. Set the same SemVer version (`2.0.0`, or `2.0.0-rc.N` / `-beta.N` / `-alpha.N`) in `Cargo.toml` and `desktop/Cargo.toml`, update both lock files, and write `docs/releases/v<version>.md`, which becomes the release body.
2. Freeze the GPUI revisions you tested: `python3 desktop/scripts/sources.py --freeze`, then commit `desktop/sources.lock.json`.
3. Push tag `v<version>`, or run the `release` workflow with that version. Running the workflow with `dry_run: true` builds, signs and notarizes every asset on any branch without publishing, for checking packaging changes before a release. It builds every CLI and desktop asset first, then publishes the GitHub Release with `SHA256SUMS`. Prerelease versions are marked as pre-releases and do not replace Latest.
4. `brew-tap` and `aur` run after a successful release. Homebrew updates only the release's channel; AUR skips prereleases and builds and checks the GUI package on Arch before pushing. Each pushes with its own key: `brew-tap` uses the `HOMEBREW_TAP_DEPLOY_KEY` secret (a deploy key with write access to `mizorewww/homebrew-tap` only) and `aur` uses `AUR_SSH_PRIVATE_KEY`.

macOS jobs of `ci`, `native desktop` and `release` run on the maintainer's self-hosted Mac (runner labels `self-hosted, macOS, ARM64, course2md`; installed under `~/actions-runners/course2md` as a launchd service). Pull requests from forks never reach it and use GitHub's `macos-26` instead. Release builds require Xcode's Metal compiler on that Mac and stop otherwise, because without it `build.rs` leaves out the Apple speech module: the runner's `.env` sets `DEVELOPER_DIR` to the installed Xcode, whose Metal Toolchain component comes from `xcodebuild -downloadComponent MetalToolchain`. Desktop jobs there skip `actions/setup-python`, whose macOS builds only install under `/Users/runner`, and create a virtualenv from the Homebrew Python 3.11+ instead. The signing step prepends a temporary keychain to the user's keychain search list and restores the list afterwards.

macOS bundles keep the full SemVer in `Course2mdVersion` and use Apple's numeric form for bundle versions (RC2 → `2.0.0` / `2.0.0fc2`).

Recipes are rendered from `packaging/homebrew/` and `packaging/aur/`. Change these templates, not the external repositories, which later releases overwrite. To retry a channel, run `brew-tap` or `aur` manually with `version`; `aur` accepts `dry_run: true`, and `packaging/aur/update-aur.sh <version> --dry-run` runs the same validation locally on Arch.


User documentation lives in the [GitHub Wiki](https://github.com/mizorewww/course2md/wiki) (`git clone git@github.com:mizorewww/course2md.wiki.git`). Keep the Chinese and English pages aligned.
