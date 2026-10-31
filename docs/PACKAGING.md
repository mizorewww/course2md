# Releases and package channels

| Channel | GUI | CLI |
| --- | --- | --- |
| Homebrew tap `mizorewww/tap` | Cask `course2md-gui` (Apple Silicon, macOS 15+) | Formula `course2md` |
| Homebrew prerelease channels | Cask `course2md-gui@{alpha,beta,rc}` | Formula `course2md-{alpha,beta,rc}` (keg-only) |
| AUR (stable only) | `course2md-gui-bin` (x86_64) | `course2md-bin` (x86_64, aarch64) |

GUI packages contain a matching private CLI engine and do not install a terminal `course2md` command, so they coexist with the CLI package.

The Homebrew formula and both AUR packages depend on the system ffmpeg, yt-dlp and aria2; the AUR GUI package installs only the app binaries from the archive. The Homebrew cask installs the app as built, bundled ffmpeg included, so it depends only on yt-dlp and aria2.

Recipes are rendered from `packaging/homebrew/` and `packaging/aur/`. Change these templates, not the external repositories, which later releases overwrite.

## Bundled tools

Desktop archives are self-contained. Pins and build options live in `desktop/scripts/` (`ffmpeg_bundle.py`, `ytdlp_bundle.py`, `prebuilt.py`, `package.py`).

| Tool | Platforms | Notes |
| --- | --- | --- |
| ffmpeg, ffprobe | All | LGPL-2.1, built from the pinned release with the system's TLS (on Linux, the user's shared GnuTLS). Windows binaries are cross-compiled with llvm-mingw; in CI the `windows-ffmpeg` job builds them |
| yt-dlp | All | Official onedir build, not the one-file build, which unpacks itself on every call. On macOS it sits in `Contents/Resources`, thinned to arm64 with each file signed |
| aria2 | Windows | Upstream publishes no macOS or Linux build; without it models download over one connection |
| uv | Windows, Linux | Runs the Intel NPU worker, whose packages are pinned in `src/npu.rs`, and fetches OpenVINO on first use |
| llama-server | Windows, Linux | Vulkan and CPU backends; CUDA, ROCm and SYCL builds remain a separate install |

System requirements the archives do not carry: on Linux, `libvulkan.so.1`, `libgomp.so.1` and OpenSSL 3 for llama-server, and GnuTLS for ffmpeg; on Windows, the Microsoft Visual C++ runtime, which is redistributable only under the distributor's own Visual Studio licence. **Run checks** and setup detect it and link Microsoft's installer.

Every package carries `THIRD-PARTY-LICENSES.txt`, the notices of its Rust crates (from `cargo-about` 0.9.3, configured in `packaging/licenses/`) and, on Apple Silicon, of its Swift packages; each bundled tool's `LICENSE-*` sits beside it, and yt-dlp's own notices stay at `yt-dlp/_internal/THIRD_PARTY_LICENSES.txt`. `desktop/scripts/licenses.py` writes it. CLI releases publish it as the asset `course2md-third-party-licenses.txt`, which the formula, the AUR package and `install.sh` install. A crate under a license missing from `about.toml` stops the release; add the license only after reading it. Release builds stop without `cargo-about` (`cargo install cargo-about --locked --features cli --version 0.9.3`); `--debug` builds warn unless `--require-tools` is given. Bump the version in `desktop-deps` and `release.yml` together.

Inside an app package the engine runs a bundled tool before one on PATH. `COURSE2MD_FFMPEG`, `COURSE2MD_FFPROBE` and `COURSE2MD_YTDLP` name a binary explicitly; `course2md doctor` and **Run checks** show where each came from.

Building the tools needs `meson`, `ninja`, `make`, `nasm` (x86-64) and `pkg-config`, and on Linux `libgnutls28-dev`. Results are cached in `desktop/.cache/tools` and re-verified on reuse. Release builds stop if a tool cannot be bundled or fails its check; `--debug` builds fall back to PATH unless `--require-tools` is given, as CI does.

On macOS arm64 `cargo build --release` stops if the Apple speech module cannot be built, which needs Xcode's Metal compiler. `COURSE2MD_NO_APPLE=1` leaves it out on purpose; a development build warns and continues without it.

## Publishing a version

1. Set the same SemVer version (`2.0.0`, or `2.0.0-rc.N` / `-beta.N` / `-alpha.N`) in `Cargo.toml` and `desktop/Cargo.toml`, update both lock files, and write `docs/releases/v<version>.md`, which becomes the release body.
2. Push tag `v<version>`, or run the `release` workflow with that version. `dry_run: true` builds, signs and notarizes every asset on any branch without publishing. The workflow builds every asset first, then publishes the GitHub Release with `SHA256SUMS`. Prereleases do not replace Latest.
3. `brew-tap` and `aur` run after a successful release. Homebrew updates only the release's channel; AUR skips prereleases and checks the GUI package on Arch before pushing. Secrets: `HOMEBREW_TAP_DEPLOY_KEY` (a deploy key with write access to `mizorewww/homebrew-tap` only) and `AUR_SSH_PRIVATE_KEY`.

To retry a channel, run `brew-tap` or `aur` manually with `version`; `aur` accepts `dry_run: true`, and `packaging/aur/update-aur.sh <version> --dry-run` runs the same validation locally on Arch.

## The self-hosted Mac

macOS jobs of `ci`, `native desktop` and `release` run on the maintainer's Mac (labels `self-hosted, macOS, ARM64, course2md`; installed under `~/actions-runners/course2md` as a launchd service). Pull requests from forks use GitHub's `macos-26` instead. The runner's `.env` sets `DEVELOPER_DIR` to the installed Xcode, whose Metal Toolchain comes from `xcodebuild -downloadComponent MetalToolchain`. Desktop jobs there create a virtualenv from the Homebrew Python 3.11+, because `actions/setup-python` only installs under `/Users/runner`.

User documentation lives in the [GitHub Wiki](https://github.com/mizorewww/course2md/wiki) (`git clone git@github.com:mizorewww/course2md.wiki.git`). Keep the Chinese and English pages aligned.
