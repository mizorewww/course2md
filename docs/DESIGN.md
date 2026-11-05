# course2md 架构：转换引擎

桌面界面见 [设计 skill](../.agents/skills/course2md-design/SKILL.md)。

course2md 是单一用途的工具：把 YouTube、Bilibili 或本地网课视频转换为按时间组织的截图与文字稿。有平台字幕时默认直接使用字幕（`--transcript-source subtitle|asr` 可强制指定），下图的语音识别分支只在没有字幕时运行。

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
              │      Qwen3-ASR (MLX/CoreML)   llama-server + Qwen3-ASR
              │      (speech-swift 运行时)    GGUF
              │              │                     │
              │              └──────────┬──────────┘
              │                         │
              │                         ▼
              │              按截图与自然停顿组织段落
              │                         │
              │                         ▼
              │              [可选] LLM 段落校对 (默认关闭)
              │              OpenAI 兼容接口 / 批量校对
              └──────────┬──────────────┘
                         ▼
              按时间合并截图与语音
                         │
              timeline.jsonl
                         │
          course.md / course.html / structured.json
```

参数与默认值以 `crates/c2m-engine/src/options.rs`、`crates/c2m-config/src/config.rs` 为准；模块职责见 [CRATES.md](CRATES.md)、各 crate 的 `Cargo.toml` 描述与各文件开头的文档注释。下面只记录从代码不易看出的决定。

- **画面检测**是三状态机（last_emitted / candidate / 发射）：采样不休眠，`--cooldown` 只限制发射频率，发射时间戳取候选画面首次出现的时间；`--slide-mode stable` 要求画面稳定后才发射，从而跳过 PPT 动画的中间帧。
- **语音分段**在能量最低点切断，避免切在词中间；事件时间与切分范围分离（`Seg { start, end, cut_start, cut_end }`），切音频时向静音填充但不进入相邻语音，因此不会产生重复文本。
- **时间线合并**：每段语音按时间中点归属截图；跨截图边界时只在句读或空格处拆分，找不到自然断点则整段保留，避免从词中间截断。原始事件先完整写入 `timeline.jsonl`；输出与 LLM 校对都以组织后的自然段为单位，校对后以 `raw` 保留原文，单批失败时保留原文，不阻断转换。
- **失败不换后端**：CoreML 路径失败时报错并保留任务参数与进度，不自动换用其他后端。
- **发送给服务的文本固定为简体中文**，不随界面语言变化：任务的请求身份包含提示词，这样换语言恢复任务不会重发已完成的请求；2.0.1 及更早版本在其他界面语言下保存的校对回执，按当时的措辞作为旧身份继续使用（见 `crates/course2md/tests/prompt_identity.rs`，CI 运行）。
- **HTTP** 用 ureq 3，有意不读取环境代理，否则会误伤本地回环服务；不确定是否已发送的请求由用户决定是否重发。
- **模型下载源**不查询公网 IP：系统地区或时区为中国大陆时优先国内镜像，其他地区以镜像作备用。`HF_ENDPOINT` 优先于一切；只有 `COURSE2MD_IP_LOOKUP=1` 才会启用 IP 查询。

语音识别后端（`--provider`）：

1. **CoreML 原生路径**（macOS Apple Silicon）：`crates/c2m-asr/build.rs` 把 `native/apple-asr`（封装 speech-swift 的 Swift 静态库）链入，不需要外部运行时。
2. **llama.cpp 路径**（`gpu` / `cpu`）：逐段提交给本地 `llama-server` 进程。
3. **Intel NPU**（`npu`）：由 uv 运行依赖版本固定的 worker（`crates/c2m-asr/src/npu.rs`），通过 OpenVINO GenAI 运行 Whisper 导出模型。
4. **云端**（`api`）：音频分段上传到 OpenAI 兼容的语音识别接口。

每个视频在保存位置下只有一份笔记：`<平台>/<课程 ID>/`（桌面应用为 `course-<ID>/`），文件夹里是 schema 2 的 `manifest.json`、文档、`course.md`、截图和导出。旧版本的 `versions/` 与 `current.json` 在打开时整理为一份笔记，其余版本删除。处理过程不写进笔记：保存位置下的 `.course2md/` 放 `locks/`（每份笔记一把锁）、`work/<任务>/`（任务的进度与回执）和 `swap/`（替换中的笔记）。

重新生成、补做和导出都原地覆盖这份笔记：新笔记先在 `swap/<笔记>/new-<任务>` 完整写好并落盘，再把旧笔记改名为 `old-<任务>`、新笔记改名到位，最后删除旧笔记；中途崩溃时，下次打开会放回旧笔记或清理残留。旧笔记中不属于它清单的文件（手动添加或修改过的）会带到新笔记里，与新文件同名的以 `.edited` 名保留并报告。转换失败或什么都没补完时，原笔记保持不变。

除非选择云端识别或 AI 服务，模型与识别数据都保留在本地。

## 非目标

- 不做说话人分离或翻译；校对与摘要是可选的 LLM 功能，默认关闭。
- 转换内核不做 GUI 或服务端；任务队列与批量转换由 `desktop/` 的桌面应用负责，它通过 `course2md run-task` 把本 CLI 当作转换引擎。
- 不接管 CUDA、Vulkan 等繁杂运行时配置；GPU 能力由随包或用户安装的 llama.cpp 决定，`--provider cpu` 可强制 CPU。
- 不内建视频网站解析、音视频编解码；这些职责分别交给 yt-dlp 与 ffmpeg。
