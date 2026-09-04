# course2md

Turn YouTube, Bilibili, or local course/meeting recordings into slide-illustrated Markdown and HTML lecture notes.

[![Rust](https://img.shields.io/badge/Language-Rust-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)](#installation)
[![AUR](https://img.shields.io/aur/version/course2md-bin?color=blue)](https://aur.archlinux.org/packages/course2md-bin)

**English** · [中文](readme.zh.md)

---

## Quick Start

> Make sure you completed the [Installation](#installation) section first.

Simply provide an online video URL or a path to a local video file. When the run finishes, the illustrated notes (`course.md` / `course.html`) appear under `./out/<platform>/<title>/<id>/`:

```bash
# Process a Bilibili video
course2md https://www.bilibili.com/video/BV1pb8o6yE8f

# Process a YouTube video
course2md https://youtu.be/dQw4w9WgXcQ

# Process a local lecture or meeting recording
course2md ./lecture.mp4
```

> **First Run Note**: The very first run (`course2md <URL or file>` with no config file yet and no `--provider` flag, in an interactive terminal) launches a setup wizard for speech recognition:
> - **Local or cloud first**: **Local recognition** (recommended — offline, free, private; model download required) or **Cloud API** (no model download; needs an OpenAI-compatible endpoint API key, pay-as-you-go).
> - **Local backends, matched to your machine**: the recommended option is listed first — `coreml` (Apple native) on macOS Apple Silicon, `gpu` when `llama-server` is installed, `npu` on Intel NPU machines, and `cpu` as the universal fallback.
> - **`gpu` / `cpu` chosen**: confirms whether to download the ~2.4 GB model now, switch to the cloud API instead, or exit and download later via `course2md models download`.
> - **`coreml` chosen (macOS)**: the model is downloaded on first transcription (~1–2.3 GB to `~/Library/Caches/qwen3-speech/`); an interactive prompt lets you pick **qwen3-1.7b** (default — Qwen3-ASR 1.7B MLX, most accurate) / **qwen3-0.6b** (CoreML on ANE, power-sipping, ~1 GB) / **whisper** (large-v3-turbo, multilingual).
> - **Cloud API chosen**: prompts for base URL (defaults to OpenRouter), API key (may be left empty and supplied later via the `COURSE2MD_ASR_API_KEY` environment variable), and model name.
> - The choice is saved to `~/.config/course2md/config.toml` — override it any time with `--provider` or by editing the file directly. Non-interactive environments (CI, pipes) skip the wizard and use the platform defaults.
> - **Slow / blocked network?** Set a HuggingFace mirror first: `export HF_ENDPOINT=https://hf-mirror.com` (download errors print this hint too) — or skip local models entirely with `course2md <URL> --provider api`.
> - Tip: pre-download the offline model any time with `course2md models download`.

---

## Installation

`course2md` relies on the following multimedia tools:
- `ffmpeg` & `ffprobe` (Audio/video extraction and slide sampling)
- `yt-dlp` (Online video parsing and downloading; only needed for online URLs)
- `llama-server` (Provided by `llama.cpp`; only needed for local `gpu` / `cpu` backends, not required for macOS `coreml` or cloud `api` mode)

Once installed, just run `course2md <URL or file>` — the first run interactively guides you through picking a speech recognition backend (see [First Run Note](#quick-start)).

---

### macOS

> Requires **macOS 15 (Sequoia) or later** on Apple Silicon (the CoreML backend depends on the ANE runtime shipped with macOS 15+; Intel Macs fall back to the `gpu`/`cpu` backends).

**Homebrew (recommended)** — dependencies, the Developer-ID-signed binary and the CoreML `mlx.metallib` are all handled for you:

```bash
brew install mizorewww/tap/course2md
```

<details>
<summary>Alternative: install.sh</summary>

```bash
brew install ffmpeg yt-dlp   # llama.cpp only needed for the gpu/cpu fallback backend
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```
</details>

---

### Arch Linux / CachyOS

Available on the **AUR** with automated dependency resolution:

```bash
# Install via AUR helper (first-class citizen)
yay -S course2md-bin
# or using paru:
# paru -S course2md-bin
```

<details>
<summary>Manual installation</summary>

```bash
# 1. Install dependencies
sudo pacman -S ffmpeg yt-dlp llama-cpp

# 2. Install course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```
</details>

---

### Debian / Ubuntu

```bash
# 1. Install base dependencies and build tools
sudo apt update
sudo apt install -y ffmpeg yt-dlp git cmake build-essential

# 2. Build and install llama-server
git clone https://github.com/ggml-org/llama.cpp.git
cmake -S llama.cpp -B llama.cpp/build -DLLAMA_CURL=OFF
cmake --build llama.cpp/build --config Release -j
sudo install -m755 llama.cpp/build/bin/llama-server /usr/local/bin/llama-server

# 3. Install course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

---

### Windows

Install dependencies via `winget` in **PowerShell**:

```powershell
winget install --id Gyan.FFmpeg -e
winget install --id yt-dlp.yt-dlp -e
winget install --id ggml.llamacpp -e
```

> Alternatively, install via Scoop: `scoop install ffmpeg yt-dlp` (for local `gpu`/`cpu` ASR you additionally need `llama-server.exe` from llama.cpp releases on your `PATH`).

**Install course2md**:
1. Download `course2md-windows-x86_64.exe` from [Releases](https://github.com/mizorewww/course2md/releases).
2. Rename to `course2md.exe` and place it in a directory listed in your `PATH`.

---

### Building from Source

Requires the stable Rust toolchain:

```bash
git clone https://github.com/mizorewww/course2md.git
cd course2md

# Standard install
cargo install --path .

# Or build release binary only
cargo build --release
```

- **macOS Apple Silicon Note**: Building native CoreML support requires Xcode 16+ (Swift 6 toolchain). `build.rs` compiles the Swift package and copies `mlx.metallib` to the target directory. If you do not need native CoreML support, skip it via: `COURSE2MD_NO_APPLE=1 cargo build --release`.
- **Other Platforms**: Linux, Windows, and x86_64 macOS builds automatically skip Apple-native components.

---

## ASR Backends

`course2md` provides multiple speech recognition backends via `--provider <backend>` or configuration:

| Backend (`--provider`) | Target & Default Policy | Architecture & Models | External Dependencies | Model Download & Cache Path | Highlights |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **`coreml`** | **macOS Apple Silicon**<br>(Default for prebuilt arm64) | **Silero VAD v6.2.1 CoreML** (ANE)<br>+ **Qwen3-ASR 1.7B MLX 8bit** (default, GPU) / **Qwen3-ASR 0.6B** (CoreML on ANE) / **Whisper large-v3-turbo** ([speech-swift](https://github.com/soniqo/speech-swift)) | **Zero external dependencies**<br>(requires co-located `mlx.metallib`) | ~1–2.3 GB<br>`~/Library/Caches/qwen3-speech/`<br>*(supports `HF_ENDPOINT` mirror)* | Zero external deps and no daemon process; default 1.7B MLX model is the most accurate local option; `qwen3-0.6b` runs on the Neural Engine with the lowest power consumption (~375 J per 3 min) |
| **`gpu`** | **Linux / Windows / Intel Mac**<br>(Default on non-Apple-Silicon) | **ffmpeg silencedetect**<br>+ **Qwen3-ASR 1.7B GGUF Q8** | Requires `llama-server`<br>(from `llama.cpp`) | ~2.4 GB<br>`~/.cache/course2md/models/` | High-precision 1.7B Q8 quantized model; fastest throughput via Metal / CUDA / Vulkan |
| **`cpu`** | **Universal Fallback** | Same as `gpu`, with `-ngl 0` | Requires `llama-server` | ~2.4 GB<br>`~/.cache/course2md/models/` | Pure CPU execution; maximum hardware compatibility |
| **`api`** | **Cloud STT (Any platform)** | **ffmpeg silencedetect**<br>+ OpenAI-compatible `/audio/transcriptions` (e.g. OpenRouter) | **Zero local model dependencies**<br>(requires network & API key) | **None** (Cloud-hosted) | Zero disk consumption, offloads computation to cloud. *Privacy note: audio chunks are uploaded.* |
| **`npu`** | **Linux / Windows**<br>(Intel Core Ultra / AI Boost) | **ffmpeg silencedetect**<br>+ **OpenVINO Whisper Large-v3 Turbo** (default) / Base / Tiny | Requires `uv` or `python` with `openvino-genai` & NPU driver | Downloaded on demand via HuggingFace | **>6x faster than CPU**, low power, low memory (550MB vs 3.5GB CPU), high accuracy on Intel NPU |

> **CoreML Model Selection**: When using `--provider coreml`, switch models via `--asr-model qwen3-1.7b` (default, MLX on GPU), `--asr-model qwen3-0.6b` (CoreML on ANE, power-sipping), or `--asr-model whisper` (large-v3-turbo). On first use in an interactive terminal, `course2md` asks and remembers your choice in `defaults.asr_model` of `~/.config/course2md/config.toml` (a legacy `~/.config/course2md/asr_model` marker file is migrated automatically and removed).
>
> **Automatic Fallback**: On macOS, if the `coreml` backend fails during initialization or runtime, `course2md` automatically logs a warning and falls back to the `gpu` / `llama-server` pipeline to ensure task completion.

### GPU Offload Controls (`gpu` / `cpu` backends)

By default the `gpu` backend starts `llama-server` with `-ngl 99` (all layers on GPU, mmproj included). On machines where an iGPU also drives the desktop — notably **AMD/ROCm (e.g. Radeon 780M)** — full offload can hang or reset the GPU (see [ROCm#6512](https://github.com/ROCm/ROCm/issues/6512)) and take the desktop session down with it. Two controls are available:

- `--gpu-layers <0-99>` (config: `gpu_layers = 8`): cap how many layers are offloaded to GPU (`-ngl`).
- `--no-mmproj-offload` (config: `mmproj_offload = false`): keep the multimodal projector on CPU. The inverse flag `--mmproj-offload` re-enables it.

> **AMD/ROCm note**: there is no evidence that any specific non-zero layer count or disabling mmproj is stable on gfx iGPUs, so `course2md` deliberately does **not** apply a "safe" default. On Linux with an AMD GPU it prints a warning when the `gpu` backend is used, pointing at these options and the CPU/API fallbacks. If you hit hangs, the proven-reliable path is `--provider cpu` (or `--provider api`).

The `cpu` backend goes further than `-ngl 0`: it also passes `--device none --no-op-offload --no-mmproj-offload` to keep every operator on CPU (a bare `-ngl 0` can still offload the projector or host operators on newer llama.cpp). These extra flags are only appended when `llama-server --help` shows them, so older distro builds of llama.cpp keep working unchanged. Every run — successful or failed — writes a `run.json` into the output directory recording the version, provider, error (on failure), and the actual `llama-server` arguments; attach it to issue reports.

```bash
# Things to try on AMD/ROCm iGPUs if the GPU path hangs
course2md lecture.mp4 --provider gpu --gpu-layers 8 --no-mmproj-offload
# Fully CPU, immune to GPU hangs (slower but proven on Radeon 780M)
course2md lecture.mp4 --provider cpu --transcript-source asr
```

---

---

## Model Selection & Accuracy Guide

To ensure high-quality illustrated notes from lectures and technical talks, `course2md` was benchmarked thoroughly across models. **We strongly recommend Qwen3-ASR 1.7B across all platforms.**

### 1. Real-World Transcription Error & Omission Analysis (Same 3-min CS Lecture)

| Evaluation Metric | Qwen3-ASR 1.7B (Strongly Recommended) | Whisper Large-v3 Turbo | Whisper Tiny / Base |
| :--- | :--- | :--- | :--- |
| **Technical Jargon & Code-Switching** | **Flawless**: Accurately transcribes `NeoVim`, `Altair 8800`, `Computer Science`, `ICQ`, `OICQ`, `QQ`, `native speaker`, `ChatGPT`, `Web Coding`, `Codex` | **Partial mishearings**: Captures `NeoWim`, but misrecognizes `Altair 8800` as `"PCG RTIR 8800"` and `Web Coding` as `"vipcoding"` | **Severe phonetic hallucinations**: `NeoVim` misheard as "cow smell" / "pinching tail" in Chinese; most technical terms mangled |
| **Sentence Completeness** | **100% Complete**: Zero dropped clauses or truncated segment endings | **Occasional Truncation**: Fast speech at segment ends occasionally gets dropped (e.g. omitted an entire sentence on PC-to-Internet transition) | **Fragmented**: Choppy fragments |
| **Punctuation & Formatting** | **Standard & Clean**: Outputs natural commas, periods, and quotation marks (e.g. quotes around phrases and proper nouns) | **Sparse punctuation**: Mostly misses periods and quotes; runs sentences together | **Barely any valid punctuation** |

### 2. Model Trade-offs & Recommendations

| Model | Recommendation | Recommended Backend | Memory Footprint | Key Strengths | Limitations |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Qwen3-ASR 1.7B** | **★★★★★<br>(Default & Recommended)** | • macOS: `--provider coreml` (default `qwen3-1.7b`, MLX on GPU) or `--provider gpu` (Metal accelerated in 13s)<br>• Linux: `--provider gpu` (CUDA) or `--provider npu`<br>• Universal: `--provider cpu` or `--provider api` | ~1.7–2.7 GB | Gold standard for Chinese & mixed-language technical lectures; flawless technical vocabulary; full punctuation; no truncated clauses | Larger download than 0.6B; MLX path uses GPU instead of the low-power ANE |
| **Qwen3-ASR 0.6B** | **★★★★☆<br>(Lightweight)** | • macOS: `--provider coreml --asr-model qwen3-0.6b` (Native Apple Neural Engine)<br>• NPU: `--provider npu --asr-model 0.6b` | ~600 MB–1.4 GB | Compact, lowest power draw on laptops on battery; zero external dependencies | Slightly lower comprehension on rare technical jargon compared to 1.7B |
| **Whisper Large-v3 Turbo** | **★★★☆☆<br>(Multilingual)** | • NPU: `--provider npu --asr-model whisper`<br>• macOS: `--provider coreml --asr-model whisper` | ~800 MB–1.5 GB | Strong for pure English or non-Chinese multilingual lectures; 12x real-time on Intel NPU | Sparse Chinese punctuation; occasional dropped clauses at segment boundaries; higher phonetic confusion on tech terms |
| **Whisper Tiny / Base** | **★☆☆☆☆<br>(Fast Pipeline Test Only)** | • NPU: `--provider npu --asr-model tiny` | <200 MB | Ultra-fast (~39x real-time, 3 min in 4s), minimal RAM | High error rate and phonetic hallucinations; not recommended for production notes |

## Configuration

To avoid passing repetitive command-line arguments, `course2md` provides a global TOML configuration file.

## Configuration Path
- **macOS / Linux**: `~/.config/course2md/config.toml` (follows `$XDG_CONFIG_HOME`)
- **Windows**: `%APPDATA%\course2md\config.toml`

### Priority Hierarchy
**CLI Flags > Configuration File (config.toml) > Built-in Defaults**

## Configuration Management Commands

```bash
# 1. Generate an annotated configuration template (use --force to overwrite existing)
course2md config init

# 2. Display the configuration path and effective default settings
course2md config show
```

## Configuration File Structure

```toml
# ~/.config/course2md/config.toml

[defaults]
# Output root directory (structured as <out>/<platform>/<title>/<id>/)
out = "out"

# Frame similarity SSIM threshold (0.0 to 1.0; lower value = more slides captured)
similarity = 0.85

# Frame sampling check interval in seconds
sample_interval = 1.0

# Cooldown time (seconds) after a new slide is captured before capturing again
cooldown = 10.0

# Region of Interest (ROI), e.g. "40%,0%-100%,100%"; empty compares full frame
# roi = "40%,0%-100%,100%"

# ASR transcription thread count (for local llama.cpp)
threads = 4

# Inference backend: coreml (macOS Apple Silicon) | gpu | cpu | api
# provider = "coreml"

# CoreML model variant: qwen3-1.7b (default, MLX on GPU) | qwen3-0.6b (CoreML on ANE, low power) | whisper (large-v3-turbo)
# asr_model = "qwen3-1.7b"

# Maximum speech segment duration in seconds before splitting
max_speech = 20.0

# Output document formats: md, html, json
formats = ["md", "html"]

# llama.cpp GGUF model directory (leave commented for default cache)
# model_dir = "~/.cache/course2md/models"

# Keep downloaded media.mp4 video file after processing
keep_video = false

[asr_api]
# Cloud STT settings (used when --provider api)
# base_url can point to any OpenAI-compatible endpoint (self-hosted gateway, DeepInfra, Groq, ...)
#mode = "transcriptions"   # transcriptions = POST {base_url}/audio/transcriptions (default, dedicated STT endpoint)
                           # chat = POST {base_url}/chat/completions (audio-capable multimodal LLMs,
                           #        e.g. gpt-4o-audio-preview, google/gemini-2.5-flash, qwen2-audio)
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-v1-xxxxxxxx"
model = "qwen/qwen3-asr-flash-2026-02-10"
# Other popular models on OpenRouter: openai/whisper-large-v3-turbo, qwen/qwen3-asr-1.7b

[llm]
# Enable LLM subtitle polishing by default (default: false; run `course2md llm setup` to configure)
enabled = false

# OpenAI-compatible API endpoint (auto-prefixes https:// if omitted)
base_url = "https://api.deepseek.com/v1"

# API Key (file permissions automatically restricted to 0600 on Unix)
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"

# Model identifier
model = "deepseek-chat"

# Custom prompt (leave empty to use high-quality built-in proofreading prompt)
prompt = ""

# Permanently suppress the post-run LLM suggestion hint (default: false)
disable_hint = false

# Vision polishing: attach the slide screenshot to each request to correct
# terminology spelling (requires a vision-capable model; default: false)
#vision = false

# Polishing concurrency (sections are independent; raise for self-hosted gateways)
#concurrency = 8
```

---

## Cloud STT (`--provider api`)

`course2md` can transcribe via any OpenAI-compatible endpoint — no local GPU or ASR model download required. `base_url` accepts any custom endpoint (self-hosted gateway, DeepInfra, Groq, ...). Two request modes:

- **`transcriptions` (default)**: POST `{base_url}/audio/transcriptions`, dedicated transcription endpoint.
- **`chat`**: POST `{base_url}/chat/completions` with `input_audio` payloads — let audio-capable multimodal LLMs transcribe directly, e.g. `gpt-4o-audio-preview`, `google/gemini-2.5-flash`, `qwen2-audio`.

- **Default Provider**: OpenRouter with `qwen/qwen3-asr-flash-2026-02-10` (~$0.000035/second of audio).
- **Other Models**: Supports `openai/whisper-large-v3-turbo`, `qwen/qwen3-asr-1.7b`, etc.
- **API Key Resolution**: Reads `--asr-api-key`, config file `[asr_api].api_key`, or the `COURSE2MD_ASR_API_KEY` environment variable (`OPENROUTER_API_KEY` still accepted).

```bash
# Transcribe using OpenRouter with an environment variable
export COURSE2MD_ASR_API_KEY=sk-or-v1-xxxx
course2md https://... --provider api

# Override model or endpoint via CLI
course2md https://... --provider api --asr-api-model openai/whisper-large-v3-turbo

# Custom endpoint: point base_url at any OpenAI-compatible service
course2md https://... --provider api \
  --asr-api-base-url https://your-gateway.example.com/v1 \
  --asr-api-model whisper-large-v3

# Audio-capable multimodal LLM (chat mode)
course2md https://... --provider api --asr-api-mode chat \
  --asr-api-model google/gemini-2.5-flash
```

> **Privacy Note**: With `--provider api`, speech audio chunks are uploaded to the specified cloud endpoint for transcription. Video frames, OCR/SSIM, and VAD segmentation remain strictly local.

---

## LLM Subtitle Polishing (Optional)

`course2md` can automatically invoke a Large Language Model (LLM) after ASR transcription to proofread and refine the generated transcript.

- **Polishing Scope**: Corrects verbal tics and filler words (e.g., "um", "uh", "you know"), stuttering/repetitions, homophone typos, and technical terminology spelling. **Preserves original meaning, does not summarize, add, or translate content**.
- **Compatible Endpoints**: Any OpenAI-compatible `/chat/completions` API (e.g., DeepSeek, GLM, OpenAI, Ollama, vLLM).
- **Fault Tolerance**: Batches requests in 20-segment chunks (`temperature=0`). If a batch fails or returns invalid JSON, it automatically falls back to raw ASR text and logs a warning without halting the conversion.

> **Privacy Note**: Enabling LLM polishing uploads the transcript text to the configured LLM endpoint; with vision polishing (`vision = true`), the corresponding slide screenshots are uploaded as well. Data retention for uploaded text and screenshots is governed by that service provider. This is an independent data path from `--provider api` audio uploads — local ASR does not imply LLM text and screenshots stay local.

### Management Commands

```bash
# Interactive setup and enablement (press Enter to keep existing values; tests connectivity upon save)
course2md llm setup

# Non-interactive configuration via flags
course2md llm setup --base-url https://api.deepseek.com/v1 --api-key sk-xxxx --model deepseek-chat

# View current LLM status (API Key masked)
course2md llm status

# Disable LLM polishing while preserving configured credentials
course2md llm disable
```

### CLI Overrides at Runtime

```bash
# Force enable / disable LLM polishing for a single run
course2md https://... --llm
course2md https://... --no-llm

# Temporarily override endpoint, key, or model
course2md https://... --llm --llm-base-url https://api.deepseek.com/v1 --llm-api-key sk-xxxx --llm-model deepseek-chat

# Suppress post-run LLM suggestion hint for a single run
course2md https://... --no-llm-hint
```

---

## Language

CLI help (`--help`) is in English; runtime logs, completion summaries, and prompts are in Chinese.

---

## Output Structure

Generated assets are organized into `out/<platform>/<title>/<id>/`:

```text
out/<platform>/<title>/<id>/
├── course.md          # Illustrated Markdown document (default)
├── course.html        # Self-contained styled HTML document (default)
├── structured.json    # Full structured data (when formats includes json)
├── frames/            # Extracted slide keyframe images
│   ├── slide_0001.jpg
│   └── ...
├── audio.wav          # Extracted audio (16kHz mono WAV)
├── timeline.jsonl     # Timestamp-aligned event stream
├── meta.json          # Video title, author, duration metadata
├── run.json           # Run provenance: version, transcript source, provider/model, stats
└── media.mp4          # Downloaded video (local input is read in-place; cleaned up by default)
```

### Completion Summary Example

Upon completion, `course2md` outputs a comprehensive summary detailing paths, metrics, elapsed time, and resident memory usage (RSS):

```text
──────── course2md done ────────
Title: Introduction to Computer Science - Lecture 01
Output dir: out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f

Documents:
  out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/course.md
  out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/course.html
Screenshots: out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/frames/ (24 images)
Audio: out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/audio.wav
Video: deleted (--keep-video)
Timeline: out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/timeline.jsonl

Stats: 24 screenshots / 142 speech segments / 8930 chars
Elapsed: 47s
Peak memory: 1406 MB (course2md) + largest child 59 MB (llama-server/ffmpeg)
Model dir: /Users/username/.cache/course2md/models
──────────────────────────────
```

---

## CLI Options

| Option | Description | Default |
| :--- | :--- | :--- |
| `-o, --out <DIR>` | Output root directory | `out` |
| `--transcript-source <auto/subtitle/asr>` | Transcript source: `auto` = platform subtitles first (manual > auto-caption), fall back to local ASR; `subtitle` = fail if none; `asr` = skip subtitles | `auto` |
| `--provider <coreml/gpu/cpu/api/npu>` | ASR backend: `coreml` (macOS arm64), `gpu` (non-Mac), `cpu`, or `api` (cloud STT) | Platform default |
| `--gpu-layers <0-99>` | GPU offload layers for `llama-server` (`-ngl`); try lowering it if AMD/ROCm iGPUs hang | `99` |
| `--mmproj-offload` / `--no-mmproj-offload` | Offload the multimodal projector to GPU or keep it on CPU | Offload |
| `--asr-model <qwen3-1.7b/qwen3-0.6b/whisper>` | CoreML ASR model variant: `qwen3-1.7b` (default, MLX on GPU), `qwen3-0.6b` (CoreML on ANE, low power), or `whisper` (large-v3-turbo) | `qwen3-1.7b` |
| `--asr-api-base-url <URL>` | Cloud STT base URL (OpenAI-compatible) | `https://openrouter.ai/api/v1` |
| `--asr-api-key <KEY>` | Cloud STT API Key (or set `COURSE2MD_ASR_API_KEY` env) | Config / Env |
| `--asr-api-model <MODEL>` | Cloud STT model slug (e.g. `qwen/qwen3-asr-flash-2026-02-10`) | `qwen/qwen3-asr-flash-2026-02-10` |
| `--similarity <0~1>` | SSIM similarity threshold; **higher = more sensitive = more slides captured** | `0.85` |
| `--sample-interval <SEC>` | Frame sampling check interval in seconds | `1.0` |
| `--cooldown <SEC>` | Minimum seconds between two consecutive slide captures | `10.0` |
| `--roi <x1,y1-x2,y2>` | Region of interest for slide comparison (e.g. `40%,0%-100%,100%`) | Full frame |
| `--formats <FORMATS>` | Comma-separated output formats: `md,html,json` | `md,html` |
| `--threads <N>` | Number of ASR worker threads (for local `gpu`/`cpu`) | `4` |
| `--max-speech <SEC>` | Maximum speech segment duration in seconds | `20.0` |
| `--keep-video` | Preserve downloaded/extracted `media.mp4` | Disabled |
| `--no-download` | Skip downloading (when `media.mp4` exists in directory) | Disabled |
| `--llm` | Force enable LLM subtitle polishing for this run | Disabled |
| `--no-llm` | Force disable LLM subtitle polishing for this run | Disabled |
| `--llm-vision` | Vision-assisted polish: attach the section slide to correct technical terms (multimodal model required) | Disabled |
| `--no-llm-vision` | Disable vision-assisted polish for this run | Disabled |
| `--no-llm-hint` | Suppress post-run LLM suggestion hint | Disabled |
| `--resume` | Resume unfinished ASR chunks from the output dir | Disabled |
| `--no-resume` | Discard existing checkpoints and redo everything | Disabled |
| `-v, --verbose` | Increase logging verbosity (use `-vv` for debug; default logs omit timestamps, `-vv` restores the full RFC3339 format) | `info` |
| `-q, --quiet` | Quiet mode (errors only) | Disabled |

Display full help:

```bash
course2md --help
```

---

## Benchmarks & Power Metrics

Measured on Apple Silicon (arm64) running a **3-minute** 1080p recorded lecture clip with `powermetrics` hardware sampling (idle baseline ≈ 1.9 W):

| Backend (`--provider`) | Wall Time | Avg Power (CPU / GPU / ANE) | Peak Memory | Notes |
| :--- | :--- | :--- | :--- | :--- |
| **`coreml` + qwen3-0.6b** | 47 s | 6.7 W / 0.2 W / **3.5 W** | 1.41 GB in-proc | **Lowest power** — Neural Engine does the heavy lifting; best on battery; zero external dependencies |
| **`coreml` + whisper-turbo** | 87 s | 15.3 W / 0.3 W / 0.4 W | 1.51 GB in-proc | Whisper large-v3-turbo on CoreML; decoder mostly on CPU for short segments |
| **`gpu` (llama.cpp Metal)** | **13 s** | 4.7 W / **16.0 W** / — | 26 MB + 3.3 GB child | **Fastest**; GPU bursts; needs `llama-server` (Qwen3-ASR 1.7B Q8) |
| **`cpu` (llama.cpp)** | 26 s | **21.2 W** / 0.6 W / — | 26 MB + 4.8 GB child | Universal fallback; high CPU power |
| **`api` (cloud STT)** | ~10 s | < 1 W | negligible | Audio uploaded to provider; speed depends on network |
| **`npu` (Intel Core Ultra)** | **16 s** | NPU hardware acceleration | 18 MB + 557 MB child | **>6x faster than CPU** on Intel Core Ultra laptops; Whisper Large-v3 Turbo |

> **Note on the `coreml` default model**: the measurements above were taken with the former 0.6B CoreML default. The current default **`qwen3-1.7b`** (Qwen3-ASR 1.7B MLX 8bit, running on GPU) is both more accurate and faster per upstream benchmarks (WER 1.52% vs 3.02%, RTF 0.033 vs 0.098), at the cost of roughly 2× peak memory (RSS ~2.7 GB vs ~1.4 GB) and giving up the ANE low-power path. Pick `--asr-model qwen3-0.6b` when battery life matters most.

👉 See the comprehensive [macOS Benchmark Report](docs/BENCHMARKS.md) for full methodology, energy breakdowns, and reproduction scripts.

---

## Video Summary (LLM, optional)

With `[llm] summarize = true` (or `course2md summarize` after setup), course2md
generates a **TL;DR / key points / timestamped outline** and inserts it at the
top of `course.md` / `course.html`:

```bash
course2md summarize out/          # summarize existing outputs (idempotent, --force to overwrite)
course2md summarize out/ -o dir/  # also export standalone <title>.summary.md files
```

- Hallucination guards: timestamped-subtitles-only input, temperature=0, structured JSON output
- Map-reduce for long videos (chunked summaries → merge)
- Reasoning-model friendly: json_object response format (auto-fallback when the
  endpoint rejects it), 16384 max_tokens, split-half retry on failures,
  4-way concurrent polishing

## Remove Credentials

Before sharing your config or committing code:

```bash
course2md remove          # clear LLM API config
course2md remove --asr    # also clear the cloud STT API key
```

## Troubleshooting

Run the environment check first:

```bash
course2md doctor
```

It reports ffmpeg / ffprobe / yt-dlp / llama-server / uv availability, platform
backends (CoreML / NPU), config-file validity (including permission warnings),
and the local model cache state.

When opening an issue, please attach:

1. The full output of `course2md doctor`
2. The `run.json` file from the output directory (records provider, model,
   transcript source, and stats — no credentials)
3. The command line you used (redact URLs if needed)

Common fixes:

| Symptom | Fix |
| :--- | :--- |
| Download fails on restricted networks | `export HF_ENDPOINT=https://hf-mirror.com` (honored by both GGUF and CoreML downloads; download errors print this hint too) |
| Transcripts look mixed/inconsistent after switching models | Pre-1.0 checkpoints are discarded automatically; rerun with `--no-resume` to force a clean pass |
| `--no-download` deleted my video | Fixed in 1.0 — files not downloaded by the current run are never removed |
| English course transcribed as Chinese on NPU | Fixed in 1.0 — language is auto-detected; force nothing |
| Prefer platform subtitles over local ASR | Default behavior in 1.0 (`--transcript-source auto`); force with `--transcript-source subtitle` |

## License

This project is licensed under the [MIT License](LICENSE).
