# course2md

Turn **YouTube, Bilibili, or local videos** into illustrated Markdown / HTML notes. Add a video in the desktop app, generate notes, then organize and read them in your course library.

**English** · [中文](readme.zh.md) · [GitHub Wiki](https://github.com/mizorewww/course2md/wiki)

**Try 2.0 RC6:** [2.0.0-rc.6 release notes and installation](https://github.com/mizorewww/course2md/releases/tag/v2.0.0-rc.6). Use the `course2md-gui@rc` Homebrew cask; the default installation options below provide the stable release.

## Features

- Slide-style notes: screenshots are captured when the picture changes, the transcript is organized into paragraphs under each screenshot, and everything is exported as `course.md` and `course.html` (add JSON with `--formats`).
- YouTube, Bilibili and local files. Platform subtitles are preferred; speech recognition runs when no subtitles exist (`--transcript-source subtitle|asr` forces either side).
- Local speech recognition with a choice of backends: Apple Silicon CoreML (no extra runtime), Intel NPU, llama.cpp GPU/CPU, or a cloud API. Recognition data stays on your machine unless you choose the cloud backend.
- Optional AI proofreading and summaries through any OpenAI-compatible endpoint, a local Ollama server (`--login ollama`), or a ChatGPT subscription via Codex login (`--login codex`) — off by default; `course2md llm setup` configures a custom endpoint.
- Resumable conversions (speech checkpoints, `--resume`) and script-friendly output: NDJSON progress (`--json`), quiet mode, bilingual 中文 / English CLI help.
- The desktop app adds background tasks with pause and cancel, a course library with folders and search, a reader with outline, find, version history and export, and 10 built-in themes.

## Install the desktop app (GUI)

Choose one installation method for your system below. **The GUI includes its conversion engine; no separate CLI install is needed.**

### macOS: Apple Silicon, macOS 15 or later

**If you have Homebrew:** run this in Terminal to install the app and the video tools `ffmpeg` and `yt-dlp`:

```sh
brew install --cask mizorewww/tap/course2md-gui
```

After installation, open **course2md** from **Finder → Applications**.

**Manual installation:** [download the macOS installer (DMG)](https://github.com/mizorewww/course2md/releases/latest/download/course2md-gui-macos-arm64.dmg), open it, and drag **course2md.app** to Applications. This installs only the app; install video tools separately using the [macOS installation guide](https://github.com/mizorewww/course2md/wiki/GUI-Installation#macos-apple-silicon).

### Arch Linux / CachyOS: x86_64

If you have `yay`, run this to install the desktop app, video tools and required graphics libraries:

```sh
yay -S course2md-gui-bin
```

With `paru`, run `paru -S course2md-gui-bin`. Open **course2md** from your applications menu. Your graphics hardware needs a working Vulkan driver; see the [Linux installation guide](https://github.com/mizorewww/course2md/wiki/GUI-Installation#arch-linux--cachyos).

### Windows: x64

1. [Download the Windows portable package (ZIP)](https://github.com/mizorewww/course2md/releases/latest/download/course2md-desktop-windows-AMD64.zip) and **extract everything** to a permanent folder.
2. Install video tools in PowerShell:

   ```powershell
   winget install --id Gyan.FFmpeg -e
   winget install --id yt-dlp.yt-dlp -e
   ```

3. Open **course2md-desktop.exe** from the extracted folder. Keep `course2md.exe` beside it: this is the bundled engine. If the app was already open, quit and reopen it after installing tools.

### Ubuntu / other Linux distributions: x86_64

[Download the Linux desktop package (tar.gz)](https://github.com/mizorewww/course2md/releases/latest/download/course2md-desktop-linux-x86_64.tar.gz), install video tools and graphics libraries using the [Linux installation guide](https://github.com/mizorewww/course2md/wiki/GUI-Installation#other-linux-x64-distributions), then extract everything. Enter the extracted folder and run `./course2md-desktop`. Keep the `course2md` engine in the same folder.

The prebuilt package is based on Ubuntu 24.04. **Intel Mac and Linux ARM64 do not yet have prebuilt GUIs**; use the [CLI version](https://github.com/mizorewww/course2md/wiki/CLI-Guide). GitHub's **Source code** archives are developer sources, not app installers.

## First use

1. Open **course2md** and follow the setup guide to choose a notes folder and speech recognition method.
2. Check **Settings → Runtime environment**. `ffmpeg` (including `ffprobe`) processes video; `yt-dlp` handles online videos.
3. Add a video URL or local file and start conversion. Open the finished notes in your library.

Local recognition downloads models on first use. GPU / CPU recognition also needs `llama-server`; cloud recognition requires your own API configuration. The app provides setup controls; see the [first-launch guide](https://github.com/mizorewww/course2md/wiki/GUI-Installation#first-launch) for details.

## Install the CLI

The CLI and the desktop app share one configuration file and can be installed side by side.

**macOS / Linux (prebuilt binary):**

```sh
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

This installs `course2md` into `~/bin` (override with `COURSE2MD_BIN_DIR`) and checks dependencies. Also available: Homebrew `brew install mizorewww/tap/course2md`; Arch Linux `yay -S course2md-bin` (x86_64, aarch64); or from source with `cargo install --path .` (Rust stable).

`ffmpeg` (including `ffprobe`) is always required, `yt-dlp` for online links, and `llama-server` for GPU/CPU recognition — except on the macOS Apple-Silicon package, whose CoreML backend needs no extra runtime. Run `course2md doctor` to check everything.

## Quick start

```sh
course2md ./lecture.mp4                        # local file
course2md https://www.youtube.com/watch?v=…    # or a Bilibili link
course2md ./lecture.mp4 -o ./notes             # choose the output root
```

The first conversion in a terminal offers interactive setup; scripts should pass options explicitly. Each note is created under the output root as `platform/title/ID/`, containing `course.md`, `course.html` and the screenshots.

## Common commands

| Command | Purpose |
| --- | --- |
| `course2md <URL or file>` | Convert a video into notes |
| `course2md --login bilibili` / `--logout bilibili` | Scan a QR code to log in to Bilibili / remove the saved login |
| `course2md --login ollama` / `--login codex` | Use a local Ollama server / import the Codex CLI's ChatGPT login (browser authorization offered when absent) |
| `course2md doctor` | Check tools, speech backends and settings |
| `course2md config init` / `config show` | Create a configuration template / show the config path and settings |
| `course2md models list` / `models prepare` | Check downloaded models / pre-download a model |
| `course2md llm setup` / `llm status` / `llm disable` | Configure, inspect or turn off AI proofreading |
| `course2md summarize ./notes` | Add AI summaries to existing notes |
| `course2md run-task` | Execute a saved task from stdin JSON (used by the GUI engine) |

Use `<command> --help` for each command's options.

## Configuration

The configuration file is `~/.config/course2md/config.toml` (`%APPDATA%\course2md\config.toml` on Windows; `XDG_CONFIG_HOME` is respected). Precedence: command-line arguments > `config.toml` > built-in defaults. `course2md config init` writes a template to get started. AI proofreading is optional and off by default: `course2md llm setup` saves the endpoint, key and model, and `course2md remove` clears saved AI service settings.

## Frequently asked questions

**Conversion fails with a missing `ffmpeg`.** Install `ffmpeg` (it includes `ffprobe`) and `yt-dlp` for online links — for example `brew install ffmpeg yt-dlp` on macOS — then run `course2md doctor` to confirm.

**A Bilibili video fails or comes back in low quality.** Run `course2md --login bilibili` and scan the QR code; `--logout bilibili` removes the saved login.

**How large are the model downloads?** The first local recognition downloads the model automatically: about 1–2.3 GB for macOS CoreML models (cached in `~/Library/Caches/qwen3-speech/`) and about 2.4 GB for the llama.cpp GGUF (cached in `~/.cache/course2md/models/`). Pre-download with `course2md models prepare`, or inspect the cache with `course2md models inspect`.

## Documentation and help

[All documentation (GitHub Wiki)](https://github.com/mizorewww/course2md/wiki) · [CLI installation and usage](https://github.com/mizorewww/course2md/wiki/CLI-Guide) · [Upgrade and uninstall](https://github.com/mizorewww/course2md/wiki/GUI-Installation#upgrade-and-uninstall) · [Troubleshooting](https://github.com/mizorewww/course2md/wiki/Troubleshooting) · [Report a problem](https://github.com/mizorewww/course2md/issues)

For developers: [desktop build guide](desktop/README.md) · [development discipline](docs/DEVELOPMENT.md) · [architecture](docs/DESIGN.md) · [benchmarks](docs/BENCHMARKS.md) · [packaging and releases](docs/PACKAGING.md) · [Changelog](CHANGELOG.md) · [MIT license](LICENSE)
