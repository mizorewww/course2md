# course2md 原生桌面端

基于 GPUI 与 GPUI Component 的 macOS、Windows、Linux 客户端。转换使用同包 CLI，支持链接和本地视频、字幕优先、各 ASR 后端、进度/取消、课程搜索、图文笔记阅读，以及共享配置文件。

## 开发

需要 Rust stable、Python 3.11+、Git。macOS 需 Xcode Command Line Tools；Windows 需 Visual Studio C++ Build Tools 和 LLVM；Linux 系统依赖见 `.github/workflows/desktop.yml`。处理视频需要 ffmpeg/ffprobe，远程链接还需要 yt-dlp。GPU/CPU 识别需 llama-server；Apple 原生、Intel NPU 和 API 的要求与 CLI 相同。

```sh
python3 desktop/scripts/sources.py
cargo build
cargo build --manifest-path desktop/Cargo.toml
# 开发时指定刚构建的引擎；Windows PowerShell 使用 $env:COURSE2MD_BIN
COURSE2MD_BIN="$PWD/target/debug/course2md" cargo run --manifest-path desktop/Cargo.toml
```

`sources.py` 默认先 fast-forward pull `~/Developer/zed` 和 `~/Developer/gpui-component` 的 main，再建立 `desktop/.deps` 下的独立工作树。可通过 `--developer-dir` 指定仓库根目录；`--no-pull` 仅复用本次已更新的源码。开发不使用版本号或固定 commit。两个原始工作区必须干净，脚本不会替你丢弃修改。

组件主线现属于 GPUI Kit，使用重新发布的 `gpui-pre` 包名。准备脚本只在独立工作树中将依赖映射到 Zed GPUI，并让组件宏兼容原始 `gpui` 包名；不改动开发者原始工作区。上游布局变更时脚本会明确失败，要求检查兼容调整。

应用支持 `COURSE2MD_BIN`、同目录 CLI、PATH 三种引擎位置。macOS 从 Finder 启动也会补充 Homebrew 工具路径。配置位置与 CLI 相同，首次桌面使用默认保存到 `~/Documents/course2md`。

## 本机打包

```sh
python3 desktop/scripts/package.py --debug
```

产物位于 `desktop/target/packages/`。macOS 为包含 CLI 和 MLX Metal 库的 `.app`；Windows 为两份 `.exe`；Linux 为两份可执行文件以及桌面入口。Windows/Linux 解压后保留两份程序在同一目录。本机默认 ad-hoc 签名；发布 CI 使用已有 Developer ID 与 Apple API 凭据签名、公证并生成 DMG。macOS ZIP 使用 ditto 保留签名所需的符号链接。

## 发布

完成开发、测试和实际界面验收后冻结**当时使用的**源码：

```sh
python3 desktop/scripts/sources.py --freeze
# 一起提交 sources.lock.json 和 desktop/Cargo.lock
python3 desktop/scripts/sources.py --locked
python3 desktop/scripts/package.py
```

发布命令核对实际工作树与冻结记录，并使用 Cargo `--locked`。普通开发命令仍继续追踪 main。CI 为三个系统分别构建；发布使用冻结记录，PR 构建使用主线。可以手动运行 release 工作流并指定版本，全部构建成功后再创建对应 tag 和 GitHub Release。

## 验证

```sh
cargo test --features integration
cargo test --manifest-path desktop/Cargo.toml
```

实际操作记录与已知边界见 `../docs/REVIEW-2026-09.md`。开发验收使用独立 `XDG_CONFIG_HOME`，不修改个人 API 配置。


## 界面与操作

- 新建页只展示来源、字幕策略和 AI 开关；识别方式、导出格式、继续转换等选项可以展开。
- 开始、取消、保存操作固定在底部，缩小窗口或滚动页面也可访问。
- 任务在后台运行；完成时正在查看任务页会自动进入阅读，在其他页面时不会打断当前操作。
- 课程库显示缩略图与内容数量；阅读页提供文稿、截图、文件分区，复制后明确反馈。
- 设置按通用、语音识别、AI 整理、运行环境分组。切换页面保留未保存的输入，保存后供下一次任务使用。
