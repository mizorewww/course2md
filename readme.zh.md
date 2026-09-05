# course2md

把 YouTube、Bilibili 或本地网课/录屏视频转换为带截图的 Markdown / HTML 笔记。

[![Rust](https://img.shields.io/badge/Language-Rust-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)](#安装指南)
[![AUR](https://img.shields.io/aur/version/course2md-bin?color=blue)](https://aur.archlinux.org/packages/course2md-bin)

[English](readme.md) · **中文**

---

## 快速上手

> 请先完成[安装指南](#安装指南)。

传入在线视频 URL 或本地视频文件路径即可开始转换。完成后，图文笔记（`course.md` / `course.html`）保存在 `./out/<平台>/<标题>/<编号>/` 目录：

```bash
# 解析 B 站视频
course2md https://www.bilibili.com/video/BV1pb8o6yE8f

# 解析 YouTube 视频
course2md https://youtu.be/dQw4w9WgXcQ

# 解析本地课件/会议录屏
course2md ./lecture.mp4
```

> **首次运行说明**：首次运行（配置文件尚不存在、未传 `--provider`、且处于交互式终端）会进入配置向导，引导设置语音转写方式：
> - **先选本地还是云端**：**本地识别**（推荐——离线、免费、隐私；首次需下载模型）或**云端 API**（免下载模型；需 OpenAI 兼容端点 API key，按量计费）。
> - **本地后端按本机能力列出**：推荐项置顶——macOS Apple Silicon 上为 `coreml`（Apple 原生），装有 `llama-server` 时可选 `gpu`，Intel NPU 机器可选 `npu`，`cpu` 通用兜底。
> - **选 `gpu` / `cpu`**：会确认是否现在下载约 2.4GB 模型，也可改为云端 API，或退出后稍后运行 `course2md models download` 手动下载。
> - **选 `coreml`（macOS）**：模型在首次识别时才下载（约 1~2.3GB，保存到 `~/Library/Caches/qwen3-speech/`），届时可交互选择 **qwen3-1.7b**（默认——Qwen3-ASR 1.7B MLX，中文/中英混合最准）/ **qwen3-0.6b**（CoreML 走 ANE，省电低功耗，约 1GB）/ **whisper**（large-v3-turbo，多语种）。
> - **选云端 API**：依次引导填写 base URL（默认 OpenRouter）、API Key（可留空，稍后用 `COURSE2MD_ASR_API_KEY` 环境变量提供）与模型名。
> - 选择会写入 `~/.config/course2md/config.toml`——以后可用 `--provider` 临时切换，或直接编辑配置文件。非交互环境（CI、管道）不触发向导，走平台默认。
> - **网络受限？** 先设 HuggingFace 镜像：`export HF_ENDPOINT=https://hf-mirror.com`（下载失败时错误信息也会提示）；或直接 `course2md <URL> --provider api` 免本地模型试用。
> - 提示：随时可用 `course2md models download` 预先下载离线识别模型。

---

## 桌面客户端（GUI）

原生客户端位于 [`desktop/`](desktop/README.md)，基于 GPUI 与 GPUI Component，已替代 Tauri。包含简洁的新建流程、进度与取消、课程搜索、文稿 / 截图 / 文件视图，以及与 CLI 共享的配置。

从 [GitHub Releases](https://github.com/mizorewww/course2md/releases) 下载 macOS DMG、Windows 便携 ZIP 或 Linux tar.gz。包内包含匹配的 CLI；需要另行安装 ffmpeg/ffprobe，在线视频还需要 yt-dlp。macOS 发布流程在仓库签名凭据可用时执行 Developer ID 签名与公证。

开发和打包方法见[原生客户端指南](desktop/README.md)。开发跟随两个上游 main，发布冻结实际测试的源码提交。

**脚本集成**：CLI 的 `--json` 输出逐行 JSON 事件，包含 `stage`、`progress`、`log`、`done` 和 `error`。

---

## 安装指南

运行 `course2md` 依赖以下基础多媒体工具：
- `ffmpeg` & `ffprobe`（音视频抽取与画面采样）
- `yt-dlp`（在线视频解析与下载；仅处理在线链接时需要）
- `llama-server`（由 `llama.cpp` 提供；仅在本地 `gpu` / `cpu` 识别后端下需要，macOS `coreml` 与云端 `api` 模式无需安装）

装完直接运行 `course2md <URL或文件>` 即可——首次运行会引导配置语音识别后端（见上文[首次运行说明](#快速上手)）。

---

### macOS

> 要求 **macOS 15 (Sequoia) 及以上**（Apple Silicon 的 CoreML 后端依赖 macOS 15+ 的 ANE 运行时；Intel Mac 自动回落 `gpu`/`cpu` 后端）。

**Homebrew（推荐）**——依赖、Developer ID 签名的二进制和 CoreML 所需的 `mlx.metallib` 一次装齐：

```bash
brew install mizorewww/tap/course2md
```

<details>
<summary>备选：install.sh 脚本</summary>

```bash
brew install ffmpeg yt-dlp   # llama.cpp 仅在 gpu/cpu 兜底后端时需要
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```
</details>

**桌面客户端（GUI）**：从 [Releases](https://github.com/mizorewww/course2md/releases) 下载 `course2md-gui-macos-arm64.dmg` 拖入「应用程序」即可——Developer ID 签名 + 公证，已内置 CLI 与 `mlx.metallib`。

---

### Arch Linux / CachyOS

推荐直接通过 **AUR** 安装，自动配置所有依赖与软链接：

```bash
# 通过 AUR 助手安装（一等公民支持）
yay -S course2md-bin
# 或使用 paru:
# paru -S course2md-bin
```

<details>
<summary>手动安装方式</summary>

```bash
# 1. 安装系统依赖
sudo pacman -S ffmpeg yt-dlp llama-cpp

# 2. 安装 course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```
</details>

---

### Debian / Ubuntu

```bash
# 1. 安装基础依赖与编译工具
sudo apt update
sudo apt install -y ffmpeg yt-dlp git cmake build-essential

# 2. 编译并安装 llama-server
git clone https://github.com/ggml-org/llama.cpp.git
cmake -S llama.cpp -B llama.cpp/build -DLLAMA_CURL=OFF
cmake --build llama.cpp/build --config Release -j
sudo install -m755 llama.cpp/build/bin/llama-server /usr/local/bin/llama-server

# 3. 安装 course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

**桌面客户端（GUI）**：从 [Releases](https://github.com/mizorewww/course2md/releases) 下载 `course2md-desktop-linux-x86_64.tar.gz`，解压后运行其中的 `course2md-desktop`。

---

### Windows

在 **PowerShell** 中使用 `winget` 一键安装依赖：

```powershell
winget install --id Gyan.FFmpeg -e
winget install --id yt-dlp.yt-dlp -e
winget install --id ggml.llamacpp -e
```

> 也可以通过 Scoop (`scoop install ffmpeg yt-dlp`) 或 Chocolatey 安装。请确保 `ffmpeg`、`ffprobe`、`yt-dlp`、`llama-server.exe` 均已加入系统 `PATH`。

**安装 course2md**：
1. 前往 [Releases](https://github.com/mizorewww/course2md/releases) 下载 `course2md-windows-x86_64.exe`。
2. 重命名为 `course2md.exe` 并将其移动至已加入系统 `PATH` 的目录中。

**桌面客户端（GUI）**：从 [Releases](https://github.com/mizorewww/course2md/releases) 下载 `course2md-desktop-windows-AMD64.zip`，解压后运行 `course2md-desktop.exe`。

---

### 从源码构建

需要安装 Rust 稳定版工具链：

```bash
git clone https://github.com/mizorewww/course2md.git
cd course2md

# 标准构建与安装
cargo install --path .

# 或仅编译 Release 二进制文件
cargo build --release
```

- **macOS Apple Silicon 说明**：构建原生 CoreML 支持需要系统安装 Xcode 16+（包含 Swift 6 工具链）。`build.rs` 会自动编译 Swift 模块并将 `mlx.metallib` 复制到 target 目录。如果不需要 CoreML 模块，可通过环境变量跳过：`COURSE2MD_NO_APPLE=1 cargo build --release`。
- **其他平台**：Linux、Windows 以及 x86_64 macOS 构建时会自动跳过 Apple 原生模块。

---

## 识别后端（ASR Backends）

`course2md` 提供多种识别后端，可通过 `--provider <后端>` 或在配置文件中指定：

| 后端 (`--provider`) | 适用平台与默认策略 | 核心架构与模型 | 外部依赖 | 首次下载与缓存路径 | 特点 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **`coreml`** | **macOS Apple Silicon**<br>(预编译包默认) | **Silero VAD v6.2.1 CoreML** (ANE)<br>+ **Qwen3-ASR 1.7B MLX 8bit**（默认，走 GPU）/ **Qwen3-ASR 0.6B**（CoreML 走 ANE）/ **Whisper large-v3-turbo** ([speech-swift](https://github.com/soniqo/speech-swift)) | **零外部依赖**<br>(仅需同目录 `mlx.metallib`) | 约 1~2.3GB<br>`~/Library/Caches/qwen3-speech/`<br>*(支持 `HF_ENDPOINT` 镜像)* | 零外部依赖、无子进程；默认 1.7B MLX 模型最准；`qwen3-0.6b` 走神经网络引擎 (ANE)，功耗极低（3 分钟约 375 J） |
| **`gpu`** | **Linux / Windows / Intel Mac**<br>(非 Apple Silicon 默认) | **ffmpeg silencedetect**<br>+ **Qwen3-ASR 1.7B GGUF Q8** | 需要 `llama-server`<br>(由 `llama.cpp` 提供) | 约 2.4GB<br>`~/.cache/course2md/models/` | 1.7B 高精度量化模型，支持 Metal / CUDA / Vulkan 等显卡加速，吞吐极高 |
| **`cpu`** | **通用兜底** | 同 `gpu`，禁用 GPU 卸载 (`-ngl 0`) | 需要 `llama-server` | 约 2.4GB<br>`~/.cache/course2md/models/` | 纯 CPU 计算，兼容性最高 |
| **`api`** | **云端 STT（跨平台通用）** | **ffmpeg silencedetect**<br>+ OpenAI 兼容 `/audio/transcriptions` 端点（如 OpenRouter） | **零本地模型依赖**<br>(需网络与 API Key) | **无**（云端托管） | 零磁盘模型占用，低配置设备友好。*隐私提示：音频切片将上传云端。* |
| **`npu`** | **Linux / Windows**<br>(Intel Core Ultra / AI Boost) | **ffmpeg silencedetect**<br>+ **OpenVINO Whisper Large-v3 Turbo** (默认) / Base / Tiny | 需要 `uv` 或 `python` 带 `openvino-genai` 与 NPU 驱动 | 按需自 HuggingFace 下载 | **比纯 CPU 快 6 倍以上**，极低功耗，显存/内存节省 84%（550MB vs 3.5GB） |

> **CoreML 模型切换**：使用 `--provider coreml` 时，可通过 `--asr-model qwen3-1.7b`（默认，MLX 走 GPU）、`--asr-model qwen3-0.6b`（CoreML 走 ANE，省电）或 `--asr-model whisper`（large-v3-turbo）切换。首次在交互式终端使用且未配置时，程序会提示选择并记忆至 `~/.config/course2md/config.toml` 的 `defaults.asr_model`（旧的 `~/.config/course2md/asr_model` marker 文件会自动迁移后删除）。
>
> **自动回落机制**：在 macOS 上如果 `coreml` 后端初始化或运行失败，系统会自动给出警告并无缝回退至 `gpu` / `llama-server` 模式，确保转换任务顺利完成。

---

---

## 模型选型与错漏分析指南（Model Selection & Accuracy Guide）

为了保证网课与学术视频转换的高质量，`course2md` 在多款模型间进行了严格的实测对照。**所有平台均首推采用 Qwen3-ASR 1.7B**。

### 1. 真实评测错漏分析（以同一段 3 分钟大学计算机课程为例）

| 维度 | Qwen3-ASR 1.7B（全平台首选推荐） | Whisper Large-v3 Turbo | Whisper Tiny / Base |
| :--- | :--- | :--- | :--- |
| **中英混合专业词汇** | **极准**：精准识别 `NeoVim`、`Altair 8800`、`Computer Science`、`ICQ`、`OICQ`、`QQ`、`native speaker`、`ChatGPT`、`Web Coding`、`Codex` | **存在误判**：识别出 `NeoWim`，但将 `Altair 8800` 误为 `"PCG RTIR 8800"`，`Web Coding` 误为 `"vipcoding"` | **严重幻觉**：`NeoVim` 严重错认为“牛味”、“捏尾巴”；专业术语大部分无法辨识 |
| **句子完整度** | **100% 完整**：无漏句、无截断，说话人语速较快时依然完整留存 | **偶发截断**：长分段末尾偶发丢失整句（例如漏掉“啊，整理一次，从PC到互联网...”） | **分段碎裂**：多处短句残缺 |
| **标点符号规范** | **规范完整**：全自动输出符合中文语法的逗号、句号、双引号（如“AI替我上大学”、“hello”） | **标点缺失**：句号大面积缺失，长难句连成一片 | **基本无有效标点** |

### 2. 几个主要模型的优劣与适用场景

| 模型 | 推荐级别 | 推荐运行方式 | 显存/内存占用 | 核心优势 | 劣势与注意事项 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Qwen3-ASR 1.7B** | **★★★★★<br>(强烈推荐)** | • macOS: `--provider coreml`（默认 `qwen3-1.7b`，MLX 走 GPU）或 `--provider gpu` (Metal 加速，仅需 13 秒)<br>• Linux: `--provider gpu` (CUDA) 或 `--provider npu`<br>• 通用: `--provider cpu` 或 `--provider api` | ~1.7GB~2.7GB | 中文及技术课程整体表现更好；标点较完整，专有名词更稳 | 模型体积略大于 0.6B；MLX 路径走 GPU，放弃 ANE 低功耗 |
| **Qwen3-ASR 0.6B** | **★★★★☆<br>(极致高能效)** | • macOS: `--provider coreml --asr-model qwen3-0.6b` (Apple Neural Engine 原生)<br>• NPU: `--provider npu --asr-model 0.6b` | ~600MB~1.4GB | 体积小、在轻薄本和电池模式下能效极高；纯本地零外部依赖 | 生僻复杂技术词理解略逊于 1.7B 满血版 |
| **Whisper Large-v3 Turbo** | **★★★☆☆<br>(纯英文/小语种)** | • NPU: `--provider npu --asr-model whisper`<br>• macOS: `--provider coreml --asr-model whisper` | ~800MB~1.5GB | 纯英文或非中文多语种识别能力优秀；OpenVINO NPU 上达 12x 实时加速 | 中文标点欠缺；语速快时偶发句尾吞词；技术词音近误判率高 |
| **Whisper Tiny / Base** | **★☆☆☆☆<br>(仅供测试)** | • NPU: `--provider npu --asr-model tiny` | <200MB | 极速（39x 实时，3分钟仅需4秒），极低显存 | 严重音近幻觉，不建议用于正式讲义 |

## 配置文件（Configuration）

为了避免每次输入冗长的命令行参数，`course2md` 提供了完善的全局配置文件支持。

### 文件路径
- **macOS / Linux**：`~/.config/course2md/config.toml`（遵循 `$XDG_CONFIG_HOME` 规范）
- **Windows**：`%APPDATA%\course2md\config.toml`

### 优先级规则
**命令行参数 (CLI Flags) > 配置文件 (config.toml) > 内置默认值 (Built-in Defaults)**

### 便捷配置命令

```bash
# 1. 初始化生成带完整注释的配置模板（文件已存在时加 --force 可覆盖）
course2md config init

# 2. 查看当前配置文件路径及生效的默认设置
course2md config show
```

### 配置项参考

```toml
# ~/.config/course2md/config.toml

[defaults]
# 输出根目录（其下按 平台/标题/编号 自动归类）
out = "out"

# 画面变化 SSIM 相似度阈值（0.0 ~ 1.0），数值越高越敏感、截图越多
similarity = 0.85

# 画面采样检查间隔（秒）
sample_interval = 1.0

# 新截图触发后的冷却防抖间隔（秒）
cooldown = 10.0

# 感兴趣区域（ROI），格式如 "40%,0%-100%,100%"，留空则比较全屏
# roi = "40%,0%-100%,100%"

# ASR 识别线程数（供本地 llama.cpp 使用）
threads = 4

# 识别后端：coreml（macOS Apple Silicon 推荐）| gpu | cpu | api
# provider = "coreml"

# CoreML 识别模型选择：qwen3-1.7b（默认，MLX 走 GPU）| qwen3-0.6b（CoreML 走 ANE，省电）| whisper（large-v3-turbo）
# asr_model = "qwen3-1.7b"

# 单段语音最长切分秒数
max_speech = 20.0

# 默认生成的文稿格式，支持 md, html, json
formats = ["md", "html"]

# llama.cpp GGUF 模型目录（留空使用默认缓存）
# model_dir = "~/.cache/course2md/models"

# 是否保留下载的原始 media.mp4 文件
keep_video = false

[asr_api]
# 云端 STT 配置（--provider api 时使用）
# base_url 可指向任何 OpenAI 兼容端点（自建网关、DeepInfra、Groq 等均可）
#mode = "transcriptions"   # transcriptions = POST {base_url}/audio/transcriptions（默认，专用转录端点）
                           # chat = POST {base_url}/chat/completions（支持音频输入的多模态 LLM，
                           #        如 gpt-4o-audio-preview、google/gemini-2.5-flash、qwen2-audio）
base_url = "https://openrouter.ai/api/v1"
api_key = "sk-or-v1-xxxxxxxx"
model = "qwen/qwen3-asr-flash-2026-02-10"
# OpenRouter 上其他常用模型：openai/whisper-large-v3-turbo, qwen/qwen3-asr-1.7b

[llm]
# 是否默认开启 LLM 字幕润色（默认 false，运行 course2md llm setup 可交互式开启）
enabled = false

# OpenAI 兼容 API 地址（如未包含协议头会自动补全 https://）
base_url = "https://api.deepseek.com/v1"

# API 密钥（文件权限在 Unix 上自动设置为 0600）
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"

# 使用的模型名称
model = "deepseek-chat"

# 自定义校对提示词（留空则使用内置的高质量校对 Prompt）
prompt = ""

# 是否永久关闭任务结束时的 LLM 开启提示（默认 false）
disable_hint = false

# 视觉润色：润色请求附对应幻灯片截图，辅助纠正术语拼写（需模型支持图片输入，默认 false）
#vision = false

# 润色并发数（Section 间相互独立；自建网关/代理可调高）
#concurrency = 8
```

---

## 云端 STT 支持 (`--provider api`)

`course2md` 支持接入任意 OpenAI 兼容端点，无需本地显卡与大模型下载。`base_url` 可指向任何自定义端点（自建网关、DeepInfra、Groq 等）。两种请求模式：

- **`transcriptions`（默认）**：POST `{base_url}/audio/transcriptions`，专用转录端点。
- **`chat`**：POST `{base_url}/chat/completions`（`input_audio` 音频输入），让支持音频的多模态 LLM 直接转录，如 `gpt-4o-audio-preview`、`google/gemini-2.5-flash`、`qwen2-audio`。

- **推荐服务**：OpenRouter 托管的 `qwen/qwen3-asr-flash-2026-02-10`（约 $0.000035 / 秒音频）。
- **兼容模型**：支持 `openai/whisper-large-v3-turbo`、`qwen/qwen3-asr-1.7b` 等。
- **密钥获取与配置**：可通过 `--asr-api-key`、配置文件 `[asr_api].api_key` 或 `COURSE2MD_ASR_API_KEY` 环境变量传入（兼容旧名 `OPENROUTER_API_KEY`）。

```bash
# 通过环境变量使用 OpenRouter 转写
export COURSE2MD_ASR_API_KEY=sk-or-v1-xxxx
course2md https://... --provider api

# 命令行即时覆盖模型
course2md https://... --provider api --asr-api-model openai/whisper-large-v3-turbo

# 自定义端点：base_url 指向任何 OpenAI 兼容服务
course2md https://... --provider api \
  --asr-api-base-url https://your-gateway.example.com/v1 \
  --asr-api-model whisper-large-v3

# 音频多模态 LLM（chat 模式）
course2md https://... --provider api --asr-api-mode chat \
  --asr-api-model google/gemini-2.5-flash
```

> **隐私提示**：使用 `--provider api` 时，语音切片将上传至所配置的云端服务完成转写；视频截图、SSIM 画面分析与 VAD 静音切分仍全部在本地执行。

---

## LLM 字幕润色（可选）

`course2md` 支持在 ASR 转写完成后，调用大语言模型（LLM）对字幕文本进行自动化校对与润色。

- **润色目标**：修正语气词/口头禅（如「呃」、「嗯」、「这个那个」等）、重复字词、明显的同音错别字与专有名词拼写；**不增删实质内容、不翻译、不改变原意**。
  - *示例*：`我我干了什么呢？我在，我这是我的Neo Vim` → `我干了什么呢？我在，这是我的Neo Vim`
- **兼容接口**：支持任意 OpenAI 兼容的 `/chat/completions` 端点（如 DeepSeek、GLM、OpenAI、Ollama、vLLM 等）。
- **容错保证**：按 20 段语音合并批次并发起请求（`temperature=0`）。若某批次请求失败或响应解析异常，将自动回退保留 ASR 原始文本并给出警告，**绝不阻断整体转换流程**。

> **隐私提示**：开启 LLM 润色会将转写文本上传至所配置的 LLM 服务；开启视觉润色（`vision = true`）还会随请求上传对应的幻灯片截图。文本与截图的数据保留政策由该服务商决定。这与 `--provider api` 的语音上传是相互独立的数据路径——「ASR 在本地运行」不代表 LLM 的文本和截图也留在本地。

### 快捷管理命令

```bash
# 交互式配置并开启（提示输入，按回车保留已配置项，保存后自动测试连通性）
course2md llm setup

# 也可以直接通过命令行参数配置
course2md llm setup --base-url https://api.deepseek.com/v1 --api-key sk-xxxx --model deepseek-chat

# 查看当前 LLM 配置状态（API Key 自动脱敏打码）
course2md llm status

# 暂时关闭 LLM 润色功能（保留已配置的凭据与端点）
course2md llm disable
```

### 运行时命令行覆盖

```bash
# 单次运行强制开启 / 关闭 LLM 润色
course2md https://... --llm
course2md https://... --no-llm

# 临时指定其他模型或端点
course2md https://... --llm --llm-base-url https://api.deepseek.com/v1 --llm-api-key sk-xxxx --llm-model deepseek-chat

# 单次运行关闭结束时的 LLM 开启提示
course2md https://... --no-llm-hint
```

---

## 语言

命令行帮助（`--help`）为英文；运行日志、完成摘要与提示信息为中文。

---

## 输出目录结构

转换产物按 `out/<平台>/<标题>/<编号>/` 格式自动归档：

```text
out/<平台>/<标题>/<编号>/
├── course.md          # 图文混排 Markdown 文档（默认生成）
├── course.html        # 独立排版 HTML 页面（默认生成）
├── structured.json    # 结构化数据（指定 --formats 包含 json 时生成）
├── frames/            # 文稿中引用的幻灯片/关键帧截图
│   ├── slide_0001.jpg
│   └── ...
├── audio.wav          # 提取的音频（16kHz 单声道 WAV）
├── timeline.jsonl     # 带时间戳对齐的原始识别序列
├── meta.json          # 视频标题、作者、时长等元数据
├── run.json           # 本次运行溯源：版本、转写来源、provider/模型、统计
└── media.mp4          # 下载的视频（本地文件输入时不重复复制；默认转换完成后自动清理）
```

### 完成摘要输出示例

任务完成后，终端会详细打印生成文稿、截图、音频、视频及时间线路径，汇总统计数据、总耗时以及进程常驻内存（RSS），清晰透明：

```text
──────── course2md 完成 ────────
标题: 计算机科学导论-第01讲
输出目录: out/bilibili/计算机科学导论-第01讲/BV1pb8o6yE8f

文稿:
  out/bilibili/计算机科学导论-第01讲/BV1pb8o6yE8f/course.md
  out/bilibili/计算机科学导论-第01讲/BV1pb8o6yE8f/course.html
截图: out/bilibili/计算机科学导论-第01讲/BV1pb8o6yE8f/frames/ (24 张)
音频: out/bilibili/计算机科学导论-第01讲/BV1pb8o6yE8f/audio.wav
视频: 已删除 (--keep-video 可保留)
时间线: out/bilibili/计算机科学导论-第01讲/BV1pb8o6yE8f/timeline.jsonl

统计: 24 张截图 / 142 段语音 / 8930 字
耗时: 47s
峰值内存: 1406 MB (course2md) + 最大子进程 59 MB (llama-server/ffmpeg)
模型目录: /Users/username/.cache/course2md/models
──────────────────────────────
```

---

## 常用参数

| 参数 | 说明 | 默认值 |
| :--- | :--- | :--- |
| `-o, --out <目录>` | 指定输出根目录 | `out` |
| `--transcript-source <auto/subtitle/asr>` | 转写来源：`auto` = 平台字幕优先（人工>自动），无字幕再走本地 ASR；`subtitle` = 强制字幕（无则报错）；`asr` = 跳过字幕直接识别 | `auto` |
| `--provider <coreml/gpu/cpu/api/npu>` | 识别后端：`coreml`（macOS 默认）、`gpu`（非 Mac 默认）、`cpu`、`api`（云端 STT） | 视平台而定 |
| `--asr-model <qwen3-1.7b/qwen3-0.6b/whisper>` | CoreML 识别模型变体：`qwen3-1.7b`（默认，MLX 走 GPU）、`qwen3-0.6b`（CoreML 走 ANE，省电）或 `whisper`（large-v3-turbo） | `qwen3-1.7b` |
| `--asr-api-base-url <URL>` | 云端 STT base URL（OpenAI 兼容） | `https://openrouter.ai/api/v1` |
| `--asr-api-key <KEY>` | 云端 STT API Key（亦可设置 `COURSE2MD_ASR_API_KEY` 环境变量） | 配置文件 / 环境变量 |
| `--asr-api-model <模型名>` | 云端 STT 模型名称（如 `qwen/qwen3-asr-flash-2026-02-10`） | `qwen/qwen3-asr-flash-2026-02-10` |
| `--similarity <0~1>` | SSIM 画面相似度阈值；**数值越高越敏感、截图越多** | `0.85` |
| `--sample-interval <秒>` | 画面采样检查间隔（秒） | `1.0` |
| `--cooldown <秒>` | 连续两张截图之间的最短间隔时间（秒） | `10.0` |
| `--roi <x1,y1-x2,y2>` | 只比较画面指定区域（如 `40%,0%-100%,100%`） | 全屏 |
| `--formats <格式>` | 输出格式，逗号分隔，可选 `md,html,json` | `md,html` |
| `--threads <数量>` | ASR 识别线程数（供本地 `gpu`/`cpu` 后端使用） | `4` |
| `--max-speech <秒>` | 单段语音最长切分秒数 | `20.0` |
| `--keep-video` | 保留下载或提取的原始 `media.mp4` 文件 | 关闭 |
| `--no-download` | 跳过下载（目录中已有 `media.mp4` 时） | 关闭 |
| `--llm` | 本次运行强制启用 LLM 字幕润色 | 关闭 |
| `--no-llm` | 本次运行强制禁用 LLM 字幕润色 | 关闭 |
| `--llm-vision` | 视觉润色：请求附对应幻灯片截图，辅助纠正技术词汇（需多模态模型） | 关闭 |
| `--no-llm-vision` | 本次运行关闭视觉润色 | 关闭 |
| `--no-llm-hint` | 本次运行关闭任务结束时的 LLM 开启提示 | 关闭 |
| `--resume` | 从输出目录续跑未完成的 ASR chunk | 关闭 |
| `--no-resume` | 丢弃既有进度，全部重算 | 关闭 |
| `-v, --verbose` | 输出更详细的执行日志（可叠加 `-vv` 进入 debug；默认日志不带时间戳，`-vv` 恢复完整 RFC3339 格式） | 默认 info |
| `-q, --quiet` | 静默模式，只显示错误 | 关闭 |

查看完整参数与子命令列表：

```bash
course2md --help
```

---

## 性能与功耗实测（Benchmarks）

在 Apple Silicon (arm64) 上运行 **3 分钟** 1080p 教学课件视频实测，通过 `powermetrics` 采集硬件功率（整机空闲基线 ≈ 1.9 W）：

| 识别后端 (`--provider`) | 总耗时 | 平均功率 (CPU / GPU / ANE) | 峰值内存 | 说明 |
| :--- | :--- | :--- | :--- | :--- |
| **`coreml` + qwen3-0.6b** | 47 s | 6.7 W / 0.2 W / **3.5 W** | 1.41 GB 进程内 | **功耗最低**：神经网络引擎扛主力，电池场景首选；零外部依赖 |
| **`coreml` + whisper-turbo** | 87 s | 15.3 W / 0.3 W / 0.4 W | 1.51 GB 进程内 | Whisper large-v3-turbo CoreML；短分段下解码器主要在 CPU |
| **`gpu`**（llama.cpp Metal） | **13 s** | 4.7 W / **16.0 W** / — | 26 MB + 3.3 GB 子进程 | **最快**：GPU 峰值高；需 `llama-server`（Qwen3-ASR 1.7B Q8） |
| **`cpu`**（llama.cpp） | 26 s | **21.2 W** / 0.6 W / — | 26 MB + 4.8 GB 子进程 | 通用兜底；CPU 功耗高 |
| **`api`**（云端 STT） | ~10 s | < 1 W | 可忽略 | 音频会上传；速度取决于网络 |
| **`npu`**（Intel Core Ultra） | **16 s** | NPU 硬件加速 | 18 MB + 557 MB 子进程 | **比纯 CPU 快 6 倍**（3 分钟音频 15 秒识别），低功耗，Whisper Large-v3 Turbo |

> **关于 `coreml` 默认模型的说明**：上表数据是在旧默认 0.6B CoreML 模型下实测的。当前默认 **`qwen3-1.7b`**（Qwen3-ASR 1.7B MLX 8bit，走 GPU）按上游 benchmark 更准也更快（WER 1.52% vs 3.02%，RTF 0.033 vs 0.098），代价是峰值内存约 2 倍（RSS 约 2.7GB vs 1.4GB），且不再走 ANE 低功耗路径。电池优先场景可显式选择 `--asr-model qwen3-0.6b`。

👉 详见完整的 [macOS 性能与功耗基准报告](docs/BENCHMARKS.md)（含测试方法论、详细能耗拆解与复现脚本）。

---

## 视频总结（LLM，可选）

开启 `[llm] summarize = true`（或配置后用 `course2md summarize`）后，
course2md 会为文稿生成 **TL;DR / 核心要点 / 带时间戳大纲**，插入
`course.md` / `course.html` 开头：

```bash
course2md summarize out/          # 为已有输出生成总结（幂等，--force 覆盖）
course2md summarize out/ -o dir/  # 另导出独立 <标题>.summary.md
```

- 幻觉防护：仅以带时间戳字幕为输入、temperature=0、JSON 结构化输出
- 超长视频自动 map-reduce（分段总结 → 合并）
- 推理模型（DeepSeek V4 Flash 等）兼容：json_object 响应格式（端点不支持时自动降级）、
  16384 max_tokens、失败批次拆半递归重试、润色 4 路并发

## 清除凭据

分享配置或提交代码前：

```bash
course2md remove          # 清除 LLM API 配置
course2md remove --asr    # 同时清除云端 STT 的 API Key
```

## 故障排查

先跑环境体检：

```bash
course2md doctor
```

一次性报告 ffmpeg / ffprobe / yt-dlp / llama-server / uv 可用性、平台后端
（CoreML / NPU）、配置文件（含权限告警）与本地模型缓存状态。

提 issue 时请附上：

1. `course2md doctor` 完整输出
2. 输出目录中的 `run.json`（记录 provider/模型/转写来源/统计，不含凭据）
3. 所用命令行（涉密 URL 可打码）

常见问题：

| 症状 | 处理 |
| :--- | :--- |
| 网络受限下载失败 | `export HF_ENDPOINT=https://hf-mirror.com`（GGUF 与 CoreML 下载均生效；下载失败的报错也会提示该镜像） |
| 换模型后转写混杂 | 1.0 起旧 checkpoint 自动作废；也可 `--no-resume` 强制重算 |
| `--no-download` 删了我的视频 | 1.0 已修复——非本次运行下载的文件永不删除 |
| NPU 上英文课被转成中文 | 1.0 已修复——语言改为自动检测，不再强制中文 |
| 想直接用平台字幕 | 1.0 默认行为（`--transcript-source auto`）；强制字幕用 `--transcript-source subtitle` |

## 开源协议

本项目基于 [MIT License](LICENSE) 开源。
