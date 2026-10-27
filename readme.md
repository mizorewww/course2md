# course2md

Turn **YouTube, Bilibili, or local videos** into illustrated Markdown / HTML notes. Add a video in the desktop app, generate notes, then organize and read them in your notes library.

**English** · [中文](readme.zh.md) · [GitHub Wiki](https://github.com/mizorewww/course2md/wiki)

## Features

- Slide-style notes: a screenshot whenever the picture changes, with the transcript in paragraphs beneath it, saved as `course.md` and `course.html`.
- YouTube, Bilibili and local files. Existing subtitles are used when available; otherwise speech is transcribed.
- Speech recognition on your own computer: Apple Silicon works out of the box, and other systems can use a graphics card, the CPU, or an Intel Core Ultra NPU. A cloud service is optional; audio leaves your computer only if you choose it.
- Optional AI proofreading and summaries using any OpenAI-compatible service, a local Ollama server, or a ChatGPT subscription. Off by default. With AI turned on, notes can also be bilingual (a translation under each paragraph) and spoken formulas are written and rendered as math.
- The desktop app adds background tasks you can pause and reorder, a library with nested folders and search, a reader with outline, find and a screenshot view, and 10 themes.
- Speech models download with resume and checksum verification, from mirrors when Hugging Face is slow to reach (for example in mainland China).


## Interface languages

The desktop app and CLI are available in **English, 简体中文, 繁體中文 and 日本語**. The desktop follows the system language by default; change it in **Settings → Appearance → Interface language**. The CLI follows the system locale, and `--lang en|zh-Hans|zh-Hant|ja` (or `COURSE2MD_LANG`) overrides it. Machine-readable `--json` output stays in Simplified Chinese so scripts keep stable message text.

## Install the desktop app (GUI)

The desktop app includes the conversion engine, so you do not need to install the CLI. It does need `ffmpeg`, which the steps below install.

### macOS: Apple Silicon, macOS 15 or later

**If you have Homebrew:** run this in Terminal to install the app and the tools `ffmpeg`, `yt-dlp` and `aria2`:

```sh
brew install --cask mizorewww/tap/course2md-gui
```

After installation, open **course2md** from **Finder → Applications**.

**Without Homebrew:** [download the DMG](https://github.com/mizorewww/course2md/releases/latest/download/course2md-gui-macos-arm64.dmg), open it, and drag **course2md** to Applications. The app includes `yt-dlp`; you also need `ffmpeg`, see the [macOS guide](https://github.com/mizorewww/course2md/wiki/GUI-Installation#macos-apple-silicon).

### Arch Linux / CachyOS: x86_64

If you have `yay`, run this to install the desktop app, video tools and required graphics libraries:

```sh
yay -S course2md-gui-bin
```

With `paru`, run `paru -S course2md-gui-bin`. Open **course2md** from your applications menu. Your graphics hardware needs a working Vulkan driver; see the [Linux installation guide](https://github.com/mizorewww/course2md/wiki/GUI-Installation#arch-linux--cachyos).

### Windows: x64

1. [Download the Windows portable package (ZIP)](https://github.com/mizorewww/course2md/releases/latest/download/course2md-desktop-windows-AMD64.zip) and **extract everything** to a permanent folder.
2. The package includes `yt-dlp` and `aria2`. Install `ffmpeg` in PowerShell:

   ```powershell
   winget install --id Gyan.FFmpeg -e
   ```

3. Open **course2md-desktop.exe** from the extracted folder. Keep the other files next to it.

### Ubuntu / other Linux distributions: x86_64

[Download the Linux package (tar.gz)](https://github.com/mizorewww/course2md/releases/latest/download/course2md-desktop-linux-x86_64.tar.gz), install `ffmpeg` and the graphics libraries from the [Linux guide](https://github.com/mizorewww/course2md/wiki/GUI-Installation#other-linux-x64-distributions), extract it, and run `./course2md-desktop`. The package includes `yt-dlp`. Built on Ubuntu 24.04.

Intel Macs and Linux ARM64 have no desktop app yet; use the [CLI](https://github.com/mizorewww/course2md/wiki/CLI-Guide).

## First use

1. Open **course2md** and follow the setup steps: choose how speech is recognized, optionally connect an AI service and Bilibili, and prepare the speech model. Notes go to a default library; add other locations in **Settings → Storage**.
2. Open **Settings → Application** and look at **Run checks** to confirm the video tools are detected.
3. On **Home**, paste a video link or drag in a local video, review the preview, and click **Start conversion**. Open the finished notes from **Library**.

The first local transcription downloads a speech model (1–2.4 GB). See the [first-launch guide](https://github.com/mizorewww/course2md/wiki/GUI-Installation#first-launch).

## Install the CLI

The CLI and the desktop app can be installed side by side. They share the Bilibili login and downloaded speech models; each keeps its own settings.

**macOS / Linux (prebuilt binary):**

```sh
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

This installs `course2md` into `~/bin` (change with `COURSE2MD_BIN_DIR`). Alternatives: `brew install mizorewww/tap/course2md`, `yay -S course2md-bin`, or `cargo install --path .` from source.

Requires `ffmpeg`. Online links also need `yt-dlp` (installed automatically if missing); with `aria2` installed, models download over several connections. Local recognition on non-Apple systems needs `llama-server`. Run `course2md doctor` to check.

## Quick start

```sh
course2md ./lecture.mp4                        # local file
course2md https://www.youtube.com/watch?v=…    # or a Bilibili link
course2md ./lecture.mp4 -o ./notes             # choose the output root
```

The first run asks a few setup questions. Notes are saved under the output folder as `platform/title/ID/`.

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

Use `<command> --help` for each command's options.

## Configuration

CLI settings live in `~/.config/course2md/config.toml` (`%APPDATA%\course2md\config.toml` on Windows); command-line options override them. The desktop app keeps its own settings, changed in **Settings**. `course2md remove` clears saved AI service settings.

## Frequently asked questions

**Conversion fails with a missing `ffmpeg`.** Install `ffmpeg` (it includes `ffprobe`) and `yt-dlp` for online links — for example `brew install ffmpeg yt-dlp` on macOS — then run `course2md doctor` to confirm.

**A Bilibili video fails or comes back in low quality.** Run `course2md --login bilibili` and scan the QR code; `--logout bilibili` removes the saved login.

**How large are the model downloads?** About 1–2.3 GB on Apple Silicon (stored in `~/Library/Caches/qwen3-speech/`) and 2.4 GB elsewhere (`~/.cache/course2md/models/`). Download ahead of time with `course2md models prepare`.

**I upgraded from 1.7. Where are my notes?** Notes made by 1.x stay where they were as `course.md` / `course.html` and still open in any editor or browser, but the 2.0 library only lists notes made by 2.0. Downloaded models, the Bilibili login and the CLI's `config.toml` keep working; set up the desktop app's recognition and AI services again in the setup steps.

## Documentation and help

[All documentation (GitHub Wiki)](https://github.com/mizorewww/course2md/wiki) · [CLI installation and usage](https://github.com/mizorewww/course2md/wiki/CLI-Guide) · [Upgrade and uninstall](https://github.com/mizorewww/course2md/wiki/GUI-Installation#upgrade-and-uninstall) · [Troubleshooting](https://github.com/mizorewww/course2md/wiki/Troubleshooting) · [Report a problem](https://github.com/mizorewww/course2md/issues)

For developers: [contributing](AGENTS.md) · [desktop development](desktop/README.md) · [engine architecture](docs/DESIGN.md) · [benchmarks](docs/BENCHMARKS.md) · [releases](docs/PACKAGING.md) · [Changelog](CHANGELOG.md) · [MIT license](LICENSE)
