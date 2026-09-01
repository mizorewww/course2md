# course2md 设计

course2md 是单一用途的命令行工具：把 YouTube、Bilibili 或本地网课视频转换为按时间组织的截图与文字稿。

## 管线

```text
URL ── yt-dlp ─┐
               ├─> 视频源 + meta.json
本地文件 ──────┘     (本地原地读取，在线下载为 media.mp4)
                         │
              ┌──────────┴──────────┐
              │                     │
              ▼                     ▼
  ffmpeg 定时灰度采样        ffmpeg 提取 16 kHz
  ROI + SSIM 比较            单声道 audio.wav
  cooldown 去抖                         │
  精确抽帧到 frames/         ┌──────────┴──────────┐
              │              ▼                     ▼
              │      [macOS Apple Silicon]    [通用 GPU / CPU]
              │      Silero VAD (CoreML)      silencedetect 分段
              │              │                     │
              │              ▼                     ▼
              │      Qwen3-ASR CoreML (ANE)   llama-server + Qwen3-ASR
              │      (speech-swift 运行时)    GGUF (300s 就绪超时)
              │              │                     │
              │              └──────────┬──────────┘
              │                         │
              │                         ▼
              │              [可选] LLM 字幕润色 (默认关闭)
              │              OpenAI 兼容接口 / 20段批量校对
              └──────────┬──────────────┘
                         ▼
              按时间合并截图与语音
                         │
              timeline.jsonl
                         │
          course.md / course.html / structured.json
```

截图与音频提取并行执行。画面检测是三状态机（last_emitted / candidate / 发射）：
采样永不休眠，`--cooldown` 只限制发射频率（不再造成检测盲区），发射时间戳取候选画面**首次出现**的时间；默认 `--slide-mode stable` 要求画面稳定 `--stable-secs`（默认 0.8s）后才发射，天然跳过 PPT 动画中间态。

语音分段（三种后端共享）：VAD 原始段先经能量感知后处理——超过 `--max-speech` 的段在目标切点 ±3s 窗口内选**能量最低点**切断（避开词中间）；切音频时向两侧静音各填充 0.25s（只进静音不进相邻语音，故无重复文本）。事件时间与切分范围分离（`Seg { start, end, cut_start, cut_end }`）。

时间线合并：每段语音按时间中点归属截图，跨越截图边界时仍完整保留该段文字。字符位置和语音时间并不一一对应；按字符比例拆分会破坏句子和后续校对。原始语音事件完整写入 `timeline.jsonl`；Markdown/HTML 渲染时会把同一画面下短停顿内的连续片段组织为自然段，并仅过滤独立出现的无语义填充词。LLM 润色按分段 id 上/下行（防重排错位），润色后保留 `raw` 原文作 provenance（timeline.jsonl 双字段）。

语音识别支持两种路径：
1. **CoreML 原生路径**（macOS Apple Silicon 预编译包默认）：通过静态链接的 `speech-swift` 运行 Silero VAD CoreML（ANE）与 Qwen3-ASR 0.6B CoreML（ANE + GPU），零外部运行时依赖。若 CoreML 初始化或运行失败，会自动回落至 `llama-server` 并发出警告。
2. **llama.cpp 路径**（Linux / Windows / 通用兜底）：由 ffmpeg `silencedetect` 分段，逐段提交给本地 `llama-server` 进程。模型为约 2.4GB 的 Qwen3-ASR GGUF，缺失时自动下载。

若开启 LLM 润色功能（默认关闭），系统会将识别事件按 20 段一批发送至 OpenAI 兼容端点进行口语修正与同音字纠错，单批失败时自动保留原文且不阻断转换流程。

配置优先级遵循：$$\text{命令行参数} > \text{config.toml} > \text{内置默认值}$$。配置文件位于 `~/.config/course2md/config.toml`（Windows 为 `%APPDATA%\course2md\config.toml`）。

时间线合并时，每段语音按时间中点归入当时最近的一张截图。默认生成 Markdown 和 HTML；JSON 通过 `--formats` 启用。对于本地文件输入，直接原地处理且不改动原文件；对于在线下载的 `media.mp4`，转换完成后默认删除。完成摘要会清晰打印各产物路径、统计数据、总耗时以及本进程与子进程（llama-server/ffmpeg）的峰值常驻内存（RSS）。

## 模块

```text
build.rs       macOS arm64 编译 Swift 模块、静态链接与 mlx.metallib 资源分发
src/
  main.rs      CLI 入口、子命令分发与参数/配置归一化
  cli.rs       命令行参数与子命令定义 (clap)
  config.rs    运行期配置结构体、ROI 与缓存路径工具
  settings.rs  配置文件 (config.toml) 加载、保存与 defaults 优先级合并
  apple.rs     macOS Apple Silicon 原生后端：Silero VAD + Qwen3-ASR (CoreML)
  asr.rs       ASR 调度、silencedetect 分段、llama-server 进程管理与转写
  fetch.rs     yt-dlp 元数据抓取与视频下载
  media.rs     ffprobe 视频/音频探测与 ffmpeg 音频抽取
  scene.rs     SSIM 采样、ROI 裁剪、冷却防抖与全分辨率抽帧
  llm.rs       LLM 字幕润色：OpenAI 兼容客户端、批处理与容错
  models.rs    Qwen3-ASR GGUF 下载与本地模型状态
  timeline.rs  截图/语音时间线合并算法与 timeline.jsonl 输出
  render.rs    Markdown、HTML、JSON 多格式渲染与写入
  pipeline.rs  全流程编排、清理与完成摘要输出
  error.rs     外部命令检查与错误格式化
native/
  apple-asr/   Swift Package Manager 静态库 (CAppleASR)，封装 speech-swift
```

Rust 负责数据契约和异步编排；外部工具（`yt-dlp`、`ffmpeg`、`ffprobe`、`llama-server`）或原生 Swift/CoreML 库在本地完成繁重计算，模型和识别数据完全保留在本地。

## 非目标

- 不做说话人分离、翻译、摘要或内容改写。
- 不做 GUI、服务端、队列或批量任务系统。
- 不接管 CUDA、Vulkan 等繁杂运行时配置；GPU 能力由用户安装的 llama.cpp 决定，`--provider cpu` 可强制 CPU。
- 不内建视频网站解析、音视频编解码；这些职责分别交给 yt-dlp 与 ffmpeg。
