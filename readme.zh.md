# course2md

把 **YouTube、Bilibili 或本地视频**转换成带截图的 Markdown / HTML 笔记。在桌面应用里添加视频、生成笔记，再用课程库整理和阅读。

[English](readme.md) · **中文** · [GitHub Wiki](https://github.com/mizorewww/course2md/wiki)

**2.0 RC6 试用：**[2.0.0-rc.6 发布说明与安装](https://github.com/mizorewww/course2md/releases/tag/v2.0.0-rc.6)。Homebrew 使用 `course2md-gui@rc`；以下默认安装入口继续提供稳定版。

## 功能特性

- 幻灯片式笔记：画面变化时自动截图，文字稿按截图组织成段落，导出 `course.md` 与 `course.html`（`--formats` 可加 JSON）。
- 支持 YouTube、Bilibili 与本地文件。默认优先使用平台字幕，无字幕时自动语音识别（`--transcript-source subtitle|asr` 可强制指定）。
- 本地语音识别可选后端：Apple Silicon CoreML（零额外运行时）、Intel NPU、llama.cpp GPU/CPU 或云端 API；除云端后端外，识别数据不出本机。
- 可选 AI 润色与总结：兼容任意 OpenAI 接口，也可用本地 Ollama（`--login ollama`）或 ChatGPT 订阅（`--login codex` 导入 codex CLI 登录态），默认关闭；`course2md llm setup` 配置自定义端点。
- 转换可从检查点恢复（`--resume`），对脚本友好：NDJSON 进度（`--json`）、静默模式、「中文 / English」双语 CLI 帮助。
- 桌面应用提供可暂停、取消的后台任务，支持文件夹与搜索的课程库，带目录、查找、版本与导出的阅读器，以及 10 套内置主题。

## 安装桌面应用（GUI）

按你的系统选择下面的一种安装方式。**GUI 已自带转换引擎，无需另装 CLI。**

### macOS：M 系列芯片，macOS 15 及以上

**已安装 Homebrew：**在终端运行以下命令，它会安装应用及视频工具 `ffmpeg`、`yt-dlp`：

```sh
brew install --cask mizorewww/tap/course2md-gui
```

装完后，从「访达 → 应用程序」打开 **course2md**。

**手动安装：**[下载 macOS 安装包（DMG）](https://github.com/mizorewww/course2md/releases/latest/download/course2md-gui-macos-arm64.dmg)，打开后将 **course2md.app** 拖入「应用程序」。这种方式只安装应用，视频工具需按 [macOS 安装指南](https://github.com/mizorewww/course2md/wiki/%E5%AE%89%E8%A3%85%E6%A1%8C%E9%9D%A2%E5%BA%94%E7%94%A8#macos-apple-silicon)另行安装。

### Arch Linux / CachyOS：x86_64

已安装 `yay` 的用户运行以下命令；它会安装桌面应用、视频工具和所需图形库：

```sh
yay -S course2md-gui-bin
```

使用 `paru` 时运行 `paru -S course2md-gui-bin`。装完后从应用菜单打开 **course2md**。显卡需有可用的 Vulkan 驱动，详见 [Linux 安装指南](https://github.com/mizorewww/course2md/wiki/%E5%AE%89%E8%A3%85%E6%A1%8C%E9%9D%A2%E5%BA%94%E7%94%A8#arch-linux--cachyos)。

### Windows：x64

1. [下载 Windows 便携包（ZIP）](https://github.com/mizorewww/course2md/releases/latest/download/course2md-desktop-windows-AMD64.zip)，**完整解压**到固定文件夹。
2. 在 PowerShell 中安装视频工具：

   ```powershell
   winget install --id Gyan.FFmpeg -e
   winget install --id yt-dlp.yt-dlp -e
   ```

3. 打开解压目录中的 **course2md-desktop.exe**。旁边的 `course2md.exe` 是配套引擎，请保留在同一目录。如果应用已打开，安装工具后退出并重新打开。

### Ubuntu / 其他 Linux：x86_64

[下载 Linux 桌面包（tar.gz）](https://github.com/mizorewww/course2md/releases/latest/download/course2md-desktop-linux-x86_64.tar.gz)，按 [Linux 安装指南](https://github.com/mizorewww/course2md/wiki/%E5%AE%89%E8%A3%85%E6%A1%8C%E9%9D%A2%E5%BA%94%E7%94%A8#其他-linux-x64)安装视频工具和图形运行库，再完整解压。进入解压目录，运行 `./course2md-desktop`，保留同目录的 `course2md` 引擎。

预编译包基于 Ubuntu 24.04。**Intel Mac 和 Linux ARM64 暂无预编译 GUI**，可使用 [CLI 版本](https://github.com/mizorewww/course2md/wiki/CLI-%E6%8C%87%E5%8D%97)。GitHub 的 **Source code** 压缩包是开发源码，不是应用安装包。

## 第一次使用

1. 打开 **course2md**，按向导选择笔记保存目录和语音识别方式。
2. 在「设置 → 运行环境」确认工具可用。`ffmpeg`（包含 `ffprobe`）用于处理视频；`yt-dlp` 用于在线视频。
3. 添加视频链接或本地文件，开始生成，完成后在课程库打开笔记。

本地识别首次使用需要下载模型；选择 GPU / CPU 识别还需安装 `llama-server`；选择云端识别需要填写自己的 API 配置。应用提供设置入口，详细步骤见 [首次使用指南](https://github.com/mizorewww/course2md/wiki/%E5%AE%89%E8%A3%85%E6%A1%8C%E9%9D%A2%E5%BA%94%E7%94%A8#首次启动)。

## 安装 CLI

CLI 与桌面应用共享同一份配置文件，可以同时安装。

**macOS / Linux（预编译二进制）：**

```sh
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

脚本把 `course2md` 装进 `~/bin`（可用 `COURSE2MD_BIN_DIR` 覆盖）并检查依赖。也可以选择：Homebrew `brew install mizorewww/tap/course2md`；Arch Linux `yay -S course2md-bin`（x86_64、aarch64）；或用 Rust stable 执行 `cargo install --path .` 从源码安装。

`ffmpeg`（含 `ffprobe`）始终必需；在线链接需要 `yt-dlp`；GPU/CPU 识别需要 `llama-server`——macOS Apple Silicon 预编译包含 CoreML 后端，无需额外运行时。运行 `course2md doctor` 可一键检查。

## 快速上手

```sh
course2md ./lecture.mp4                        # 本地文件
course2md https://www.youtube.com/watch?v=…    # 或 Bilibili 链接
course2md ./lecture.mp4 -o ./notes             # 指定输出根目录
```

交互终端首次转换会引导配置；脚本请显式传入所需参数。每篇笔记创建在输出根目录的 `平台/标题/ID/` 子目录中，包含 `course.md`、`course.html` 与截图。

## 常用命令

| 命令 | 用途 |
| --- | --- |
| `course2md <链接或文件>` | 把视频转换成笔记 |
| `course2md --login bilibili` / `--logout bilibili` | 扫码登录 Bilibili / 清除保存的登录 |
| `course2md --login ollama` / `--login codex` | 使用本地 Ollama 服务 / 导入 codex CLI 的 ChatGPT 登录态（未安装时可用浏览器授权） |
| `course2md doctor` | 检查依赖、识别后端和配置 |
| `course2md config init` / `config show` | 生成配置模板 / 显示配置路径和文件设置 |
| `course2md models list` / `models prepare` | 检查已下载模型 / 预先下载模型 |
| `course2md llm setup` / `llm status` / `llm disable` | 配置、查看或关闭 AI 润色 |
| `course2md summarize ./notes` | 为已有笔记生成 AI 总结 |
| `course2md run-task` | 从标准输入读取 JSON 参数执行已保存任务（GUI 引擎使用） |

使用 `<命令> --help` 查看每个子命令的参数。

## 配置

配置文件位于 `~/.config/course2md/config.toml`（Windows 为 `%APPDATA%\course2md\config.toml`，遵循 `XDG_CONFIG_HOME`）。优先级：命令行参数 > `config.toml` > 内置默认值。`course2md config init` 可生成配置模板。AI 润色是可选功能且默认关闭：`course2md llm setup` 保存服务地址、密钥和模型，`course2md remove` 清除已保存的 AI 服务配置。

## 常见问题

**转换时报缺少 `ffmpeg`。**安装 `ffmpeg`（含 `ffprobe`）；处理在线链接还需 `yt-dlp`，例如 macOS 上 `brew install ffmpeg yt-dlp`，然后运行 `course2md doctor` 确认。

**Bilibili 视频失败或清晰度低。**运行 `course2md --login bilibili` 扫码登录；`--logout bilibili` 清除保存的登录。

**模型下载有多大？**首次本地识别会自动下载模型：macOS CoreML 模型约 1–2.3 GB（缓存在 `~/Library/Caches/qwen3-speech/`），llama.cpp GGUF 约 2.4 GB（缓存在 `~/.cache/course2md/models/`）。可用 `course2md models prepare` 预先下载，或用 `course2md models inspect` 查看缓存。

## 文档与帮助

[全部文档（GitHub Wiki）](https://github.com/mizorewww/course2md/wiki) · [CLI 安装与使用](https://github.com/mizorewww/course2md/wiki/CLI-%E6%8C%87%E5%8D%97) · [升级与卸载](https://github.com/mizorewww/course2md/wiki/%E5%AE%89%E8%A3%85%E6%A1%8C%E9%9D%A2%E5%BA%94%E7%94%A8#升级与卸载) · [故障排查](https://github.com/mizorewww/course2md/wiki/%E6%95%85%E9%9A%9C%E6%8E%92%E6%9F%A5) · [反馈问题](https://github.com/mizorewww/course2md/issues)

面向开发者：[桌面端构建指南](desktop/README.md) · [开发纪律](docs/DEVELOPMENT.md) · [架构](docs/DESIGN.md) · [性能基准](docs/BENCHMARKS.md) · [打包与发布](docs/PACKAGING.md) · [更新记录](CHANGELOG.md) · [MIT 许可证](LICENSE)
