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

The desktop is the `course2md-desktop` binary in `desktop/`, over the library crates beside it in `desktop/` (`docs/CRATES.md` lists them); the scripts build and license it through `desktop/Cargo.toml`, which reaches them all.

Desktop archives are self-contained. Pins and build options live in `desktop/scripts/` (`ffmpeg_bundle.py`, `ytdlp_bundle.py`, `prebuilt.py`, `package.py`).

`python3 desktop/scripts/package.py` (Python 3.11 or later, with `desktop/scripts/requirements-packaging.txt` installed) builds and packages for the current platform into `target/packages/`, or `$CARGO_TARGET_DIR/packages/` when that is set. Without `APPLE_SIGNING_IDENTITY` it signs ad hoc and skips notarization, so a local run publishes nothing. Downloads are retried, and the ffmpeg tarball falls back to Debian's and Gentoo's copies of it, verified by the same sha256.

| Tool | Platforms | Notes |
| --- | --- | --- |
| ffmpeg, ffprobe | All | LGPL-2.1, built from the pinned release with the system's TLS (on Linux, the user's shared GnuTLS). Windows binaries are cross-compiled with llvm-mingw; in CI the `windows-ffmpeg` job builds them |
| yt-dlp | All | Official onedir build, not the one-file build, which unpacks itself on every call. On macOS it sits in `Contents/Resources`, thinned to arm64 with each file signed |
| aria2 | Windows | Upstream publishes no macOS or Linux build; without it models download over one connection |
| uv | Windows, Linux | Runs the Intel NPU worker, whose packages are pinned in `crates/c2m-asr/src/npu.rs`, and fetches OpenVINO on first use |
| llama-server | Windows, Linux | Vulkan and CPU backends; CUDA, ROCm and SYCL builds remain a separate install |

System requirements the archives do not carry: on Linux, `libvulkan.so.1`, `libgomp.so.1` and OpenSSL 3 for llama-server, and GnuTLS for ffmpeg; on Windows, the Microsoft Visual C++ runtime, which is redistributable only under the distributor's own Visual Studio licence. **Run checks** and setup detect it and link Microsoft's installer.

Every package carries `THIRD-PARTY-LICENSES.txt`, the notices of its Rust crates (from `cargo-about` 0.9.3, configured in `packaging/licenses/`) and, on Apple Silicon, of its Swift packages; each bundled tool's `LICENSE-*` sits beside it, and yt-dlp's own notices stay at `yt-dlp/_internal/THIRD_PARTY_LICENSES.txt`. `desktop/scripts/licenses.py` writes it. CLI releases publish it as the asset `course2md-third-party-licenses.txt`, which the formula, the AUR package and `install.sh` install. A crate under a license missing from `about.toml` stops the release; add the license only after reading it. Release builds stop without `cargo-about` (`cargo install cargo-about --locked --features cli --version 0.9.3`); `--debug` builds warn unless `--require-tools` is given. Bump the version in `desktop-deps` and `release.yml` together.

Inside an app package the engine runs a bundled tool before one on PATH. `COURSE2MD_FFMPEG`, `COURSE2MD_FFPROBE` and `COURSE2MD_YTDLP` name a binary explicitly; `course2md doctor` and **Run checks** show where each came from.

Building the tools needs `meson`, `ninja`, `make`, `nasm` (x86-64) and `pkg-config`, and on Linux `libgnutls28-dev`. Results are cached in `desktop/.cache/tools` and re-verified on reuse. Release builds stop if a tool cannot be bundled or fails its check; `--debug` builds fall back to PATH unless `--require-tools` is given, as CI does.

On macOS arm64 `cargo build --release` stops if the Apple speech module (`native/apple-asr`, built by `crates/c2m-asr/build.rs`) cannot be built, which needs Xcode's Metal compiler. `COURSE2MD_NO_APPLE=1` leaves it out on purpose; a development build warns and continues without it.

## Windows installer

On Windows `package.py` also builds `course2md-desktop-windows-x64-setup.exe` with Inno Setup 6.3 or later (`ISCC`, the default install folders or PATH; the hosted runners have it, and `desktop-deps` installs it from Chocolatey if an image drops it). `desktop/installer/windows/course2md.iss` installs the directory the portable ZIP holds, unchanged, so the app finds its engine and tools as it does in the ZIP; `desktop/scripts/windows_installer.py` passes it the version and the `AppId`.

- Per user, without administrator rights: `%LOCALAPPDATA%\Programs\course2md`, a Start menu shortcut and an optional desktop shortcut. The wizard follows the system language (English, Simplified and Traditional Chinese, Japanese).
- An upgrade closes a running app (`CloseApplications=force`: the Restart Manager terminates the app, its engine and any bundled tool running from the install, since the app's window does not answer its session-end messages), installs over the earlier version found through the `AppId` (never change it), and replaces the `yt-dlp` and `llama.cpp` directories whole. A task stopped this way loses only the step in progress: the engine keeps checkpoints and writes its records atomically, model downloads resume, and a note is replaced only when a run finishes, so the task resumes on the next start.
- Uninstalling removes only installed files. Settings (`%APPDATA%\course2md`), model caches (`%LOCALAPPDATA%\course2md`) and note libraries stay.

`python desktop/scripts/windows_installer.py smoke <setup.exe> <package directory>` installs into a temporary directory, checks every file, the registered version and the engine (`--version`, and `doctor` must report each bundled tool from the install directory), upgrades in place while the installed app and a long-running bundled ffmpeg run (both must be closed and their files replaced), uninstalls and checks that user data stays. The `native desktop` workflow runs it on every desktop change, and the release workflow on the installer it publishes.

The installer is unsigned unless the repository has the secrets `WINDOWS_CERT_PFX` (a base64 code-signing .pfx) and `WINDOWS_CERT_PASS`; then the release workflow's **Sign Windows installer** step signs it with Authenticode. Unsigned, Windows SmartScreen warns on first run (**More info → Run anyway**); release notes should say so. The ZIP remains the portable build.

## Publishing a version

1. Set the SemVer version (`2.0.0`, or `2.0.0-rc.N` / `-beta.N` / `-alpha.N`) in `[workspace.package]` of the root `Cargo.toml`, which every crate and the desktop inherit, update `Cargo.lock`, and write `docs/releases/v<version>.md`, which becomes the release body.
2. Move the GUI download links in `readme.md` and `readme.zh.md` to the new tag (`releases/download/v<version>/…`): the Download badges, the plain-text line under them, and the Install section. `releases/latest/download` cannot be used, since it reaches only the newest stable release. Change the Homebrew cask in the Download and Install sections to the release's channel (`course2md-gui@alpha`, or `course2md-gui` for a stable release). `packaging/test_readme_downloads.py` fails until both READMEs match, and the release workflow runs it before building. From 3.0.0-alpha.5 the Windows badge and link point at `course2md-desktop-windows-x64-setup.exe`, and the Install section's Windows steps describe the installer first and the ZIP as the portable option.
3. Push tag `v<version>`, or run the `release` workflow with the tag as its input, leading `v` included (`v3.0.0-alpha.1`). `dry_run: true` builds, signs and notarizes every asset on any branch without publishing. The workflow builds every asset first, then publishes the GitHub Release with `SHA256SUMS`. Prereleases do not replace Latest.
4. `brew-tap` and `aur` run after a successful release. Homebrew updates only the release's channel; AUR skips prereleases and checks the GUI package on Arch before pushing. Secrets: `HOMEBREW_TAP_DEPLOY_KEY` (a deploy key with write access to `mizorewww/homebrew-tap` only) and `AUR_SSH_PRIVATE_KEY`.

The release notes end with an install section: one sentence that the GUI carries its own CLI engine, the Homebrew commands for the release's channel, then a table of the direct downloads, one row per platform, each linking the versioned asset. The Windows row lists the installer first and the portable ZIP after it:

| Platform | Download | How to install |
| --- | --- | --- |
| macOS Apple Silicon, macOS 15+ | [DMG](https://github.com/mizorewww/course2md/releases/download/v<version>/course2md-gui-macos-arm64.dmg) | Quit the old app and drag the whole `course2md.app` to Applications |
| Windows x64 | [Installer](https://github.com/mizorewww/course2md/releases/download/v<version>/course2md-desktop-windows-x64-setup.exe) · [portable ZIP](https://github.com/mizorewww/course2md/releases/download/v<version>/course2md-desktop-windows-AMD64.zip) | Run the installer; it needs no administrator rights and upgrades the earlier install in place. While it is unsigned, SmartScreen asks first (**More info → Run anyway**). Or extract the whole ZIP and open `course2md-desktop.exe` |
| Linux x64 | [tar.gz](https://github.com/mizorewww/course2md/releases/download/v<version>/course2md-desktop-linux-x86_64.tar.gz) | Extract it and run `./course2md-desktop`; built on Ubuntu 24.04 |

Follow it with the line that settings, model caches and libraries are kept, and how to confirm the version. The Chinese notes use the same rows (安装程序 · 便携 ZIP), and the English summary points at the table.

To retry a channel, run `brew-tap` or `aur` manually with `version`; `aur` accepts `dry_run: true`, and `packaging/aur/update-aur.sh <version> --dry-run` runs the same validation locally on Arch.

## macOS runners

CI and releases run only on GitHub-hosted runners; do not add a self-hosted one. Every macOS job (`ci`, `native desktop`, `release`) runs on `macos-26`. A release job that finds no Metal compiler fetches Xcode's Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`) and fails if it still has none, since without it the CLI would ship without CoreML recognition. Signing and notarization use the repository secrets `MACOS_CERT_P12`, `MACOS_CERT_PASS`, `APPLE_API_KEY`, `APPLE_API_KEY_ID` and `APPLE_API_ISSUER`, imported into a temporary keychain for each job.

User documentation lives in the [GitHub Wiki](https://github.com/mizorewww/course2md/wiki) (`git clone git@github.com:mizorewww/course2md.wiki.git`). Keep the Chinese and English pages aligned.
