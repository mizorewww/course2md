# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 与
[语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

### 新增：外部依赖链自动安装（`course2md setup`）

- **按需自动安装外部工具**：首次运行缺 ffmpeg/ffprobe/yt-dlp/llama-server/uv 时，
  自动下载官方预编译二进制到私有工具目录 `~/.local/share/course2md/bin`
  （`COURSE2MD_TOOLS_DIR` 可重定向）：免 root、不改系统 PATH、删目录即卸载；
  全部资产固定版本 + sha256 校验（yt-dlp/uv 用官方校验和，llama.cpp/ffmpeg-static
  由 release 工程预计算固化）；`[deps] auto_install = false` / `--no-install` 可关闭
- **`course2md setup [--check|--yes|--all]`**：一键体检 + 安装缺失工具；
  `doctor` 的缺失提示同步指向 setup
- **需求驱动最小安装集**：本地文件 + `api` 后端只需 ffmpeg；URL 输入才要 yt-dlp；
  `gpu/cpu` 才要 llama-server；`npu` 才要 uv
- **llama-server 变体管理**：默认 Vulkan 构建（一张通吃 NVIDIA/AMD/Intel），
  `--provider cpu` 时自动切换 CPU 构建并清理旧变体；macOS 装 Metal 构建
  （coreml 回退 gpu 用）
- **带库工具的子目录布局**：llama-server 连同 ggml 动态库装入
  `tools_dir/llama-server/`（重装整体替换）；符号链接在解压平铺时原样重建
  （修复 .so 版本链断裂）；下载器泛化至 `net.rs` 并增加网络抖动重试（×3 退避）
  与纯 Rust tar.gz 解压（不再依赖系统 tar/gzip）
- **install.sh 简化为引导器**：只装 course2md 本体 + metallib，依赖交给
  `course2md setup`（旧脚本的缺依赖即退出闸门已移除）

### 修复（依赖自动安装自查）

- **模型 manifest 记账回归**：服务器无 Content-Length 时尺寸被记为 0，
  导致下次启动永远判"不完整"而无限重下；恢复记录实际字节数
- **重装不再破坏现有安装**：bundle 工具（llama-server）改为下载/解压全部
  成功后才替换旧目录，中途失败保留可用安装
- **manifest 升级检测**：stamp 记录 sha256 与版本，已装安装落后于清单时
  自动升级（`auto_install`）/提示（`setup --check` 显示 ↻ 标记）
- **sha256 完整性不再套用 1MB 启发式**：小体积工具也能通过完整性校验
  （1MB 下限仅保留在模型尺寸口径）
- **Windows 工具目录改用 LOCALAPPDATA**：避免二进制随 Roaming 同步到其他机器
- **下载重试跳过不可恢复的 4xx**（除 429），与 llm.rs 重试约定一致；
  附服务端返回体摘要
- **`setup --check` 核心依赖缺失时退出码 1**，可用于 CI/脚本判断
- **下载加读/连接超时**：读停顿超过 60s 判定连接僵死交给重试，
  不再永久挂起（总时长不限，2.4GB 模型不受影响）；manifest 改原子写入
- **bundle 工具重装改为同盘 staging + 原子换名**：平铺中途失败旧安装完好
- **tool_version 收拢至 runtime::probe_version**（消除 doctor/deps 重复）
- **readme 修正工具优先级描述**：私有目录优先于系统同名工具（与实现一致），
  并说明理由与回退方式
- **新增 docs/FORK-REVIEW.md**：fork 维护与功能审查 runbook（同步前/本地补丁/
  功能矩阵三场景）

### 改进

- **LLM 请求重试**：网络/TLS 错误、429、5xx 按指数退避重试（1s→2s，含抖动），
  最多 3 次尝试；4xx（鉴权/参数）快速失败并附服务端返回体
- **润色真并发**：波次式改为 worker 池抢占取活，消除队头阻塞；
  并发数可配置（`[llm] concurrency`，默认 4→8，范围 1~16）

## [1.2.0] — 2026-09-01

### 新增：视频总结与凭据清理（来自 #7，感谢 @1Cookie2gavh）

- **`course2md summarize`**：为已有输出生成 TL;DR / 核心要点 / 带时间戳
  大纲，就地写入 course.md/html（幂等，`--force` 覆盖，`-o` 导出独立文件）；
  `[llm] summarize = true` 转换后自动总结；超长视频 map-reduce；
  幻觉防护（仅字幕输入 / temperature 0 / JSON 结构化 / 时间戳可溯源）
- **`course2md remove [--asr]`**：清除 LLM/STT API 凭据（分享/提交前）
- **推理模型润色兼容**：json_object 响应格式（端点不支持时自动降级重试）、
  max_tokens 16384、解析三级容错（严格 → 尾逗号清理 → 逐对象扫描跳坏项）、
  失败批次拆半递归重试、Section 级 4 路并发润色、超时 300s
- 移植时修复：宽容扫描方向反了会漏掉首对象；严格解析成功但含坏项时
  早退跳过降级；json_object 无降级路径；测试初始化器缺字段（CI 编译失败）；
  contains_summary 误判（标题含「视频总结」）

### 段落组织（来自 #6，感谢 @QiuShunan）

- **段落组织**：同一截图下、短停顿（<3.5s）内的连续 ASR 片段合并为自然段
  （上限 420 字），文稿不再是 VAD 碎片流水账；LLM 校对的单位改为组织好的段落
- **独立语气词过滤**：单独成条的「嗯/啊/呃」等在无 LLM 时也会被过滤
  （嵌在句中的保留）；timeline.jsonl 仍保存完整原始事件供追溯
- **跨页切分语义收紧**：只在边界附近的句读/空格处拆分，找不到自然断点时
  整段保留——不再按字符比例从词中间截断
- HTML 输出去掉「……」对话式包裹，正文直接成段
- 默认校对提示词明确禁止概括、扩写、翻译或改变原意

## [1.1.0] — 2026-09-01

### 新增

- **LLM 视觉润色** `--llm-vision` / 配置 `vision = true`：按节附幻灯片截图，
  模型参照画面纠正技术词汇拼写；端点不支持图片时该批自动降级纯文本。
  `llm setup` 交互式询问模型视觉能力（脚本化调用不阻塞）。
  （Implements #5，感谢 @mizorewww）
- **纯语气词条目删除**：默认提示词允许对纯语气词/口头禅条目返回空文本，
  该条将被删除且原文保留在 `raw` 字段溯源。（Implements #5）
- `run.json` 记录 `llm_vision`。

### 修复

- **ASR 进度条不再被 llama-server 日志打穿**：llama-server stderr 此前直接
  继承终端，其每 chunk 的 slot timing 日志插在进度条重绘之间，导致进度条
  每次更新都新起一行。改为 piped + 后台 drain（尾部缓存进错误信息，debug
  可转发）；顺带修复 scene 采样 ffmpeg stderr 未 drain 的死锁隐患。
  （Fixes #4，感谢 @mizorewww）
- **`llm setup` / CoreML 模型选择支持方向键编辑**：裸 `read_line` 不处理
  方向键转义序列（←/→/Home/End 变字面字符）。改用 dialoguer。
  （Fixes #3，感谢 @mizorewww）

## [1.0.0] — 2026-09-01

首个正式版。本轮以「正确性审计 + 架构收敛」为主题：修复全部已知的
数据丢失 / 静默错误结果类缺陷，将字符串分发改为类型系统，把配置错误
提前到毫秒级暴露，并新增运行溯源与环境体检。

### 新增

- **平台字幕优先的转写来源** `--transcript-source auto|subtitle|asr`
  （默认 `auto`）：平台人工字幕 > 平台自动字幕 > 本地 ASR；命中字幕时
  完全不抽音频、不加载模型。本地视频支持同名 `.srt`/`.vtt` sidecar。
  解析器支持 SRT/VTT、多行 cue、行内标签清理、滚动字幕去重。
  （Implements #1，感谢 @kernerydel）
- **`course2md doctor`**：环境体检——ffmpeg/ffprobe/yt-dlp/llama-server/uv
  可用性、平台后端（CoreML/NPU）、配置文件（含权限告警）、模型缓存状态。
- **`run.json` 运行溯源**：每次运行在输出目录记录版本、转写来源、
  provider/模型、统计与耗时（原子写，不含凭据）。
- `--no-resume` / `--resume` 互斥参数与三态解析。
- `structured.json` 增加 `schema_version` 与 `generator{name,version}`；
  `Section` 增加 `end`（下一段起点 / 媒体时长）。

### 修复

- **`--no-resume` 此前完全无效**（声明了但从未读取），用户以为重跑全部，
  实际可能复用旧进度。
- **`--no-download` 不再删除用户已有视频**：只有「本次运行真正下载的」
  媒体文件才会在结束时清理（此前只要未开 `--keep-video` 就会误删）。
- **checkpoint 运行身份**：新增 `.asr_identity`（版本/provider/模型/
  max_speech）。此前换模型后断点续跑会静默混用两个模型的转写。
  1.0 前的无身份旧进度自动作废重算。
- **空语音 chunk 也计入 checkpoint**：静音段此前每次续跑都重复识别。
- **checkpoint 写盘失败不再标记完成**；`.asr_done` 原子写且错误不再吞掉；
  中间行损坏从静默跳过改为硬错误（末行半截仍按崩溃残留容忍）；
  `resume=false` 时清档，杜绝重复运行导致的 asr.jsonl 叠加双份文本。
- **NPU 后端三连修**：
  1. 【致命】内嵌 Python worker 存在语法错误（f-string 字面换行），
     `--provider npu` 自合入起无法启动；
  2. Whisper 管线硬编码 `<|zh|>`，英文课被强制按中文解码产生幻觉转写，
     改为语言自动检测；
  3. Qwen 下载/加载失败时静默回退 Whisper（模型族都变了），改为硬错误。
  （感谢 @little-q-exist 的报告催生了对 NPU 路径的完整审查）
- **GGUF 下载尊重 `HF_ENDPOINT` 镜像**（此前硬编码 huggingface.co，
  README 宣称的镜像方案仅 CoreML 路径生效）。（Fixes #2，感谢 @little-q-exist）
- **相似度阈值文案方向修正**：实际语义为「阈值越高越敏感、截图越多」，
  CLI 帮助 / 配置模板 / 双语 README 全部改正。
- **预检校验**：`max_speech=0` 会导致切分算法 `clamp(min>max)` panic、
  `--formats` 拼错要到渲染阶段才报错、provider×模型不兼容（如 gpu 上
  指定 whisper）被静默忽略、provider=api 缺 key 要切完音频才发现——
  全部提前到任何昂贵操作之前毫秒级失败。
- 配置文件路径展开 `~`（此前会真的创建 `./~/` 目录）；
  配置未知字段（如 `similairty`）直接报错而非静默忽略；
  TOML `provider="npu"` 此前被手写校验拒绝，现随类型系统天然支持。

### 变更 / 重构

- `provider` / `slide_mode` / `formats` 由裸字符串改为 typed enum
  （`AsrProvider` / `SlideMode` / `OutputFormat`），CLI、TOML、运行时
  共用同一套类型；非法值在解析期即失败。
- checkpoint 协议 v2：运行身份 + 空 chunk 记录 + 写失败不标记完成 +
  损坏策略 + 重复行去重。
- 新增 `runtime` 模块：`ManagedChild`（kill-on-drop，任何 `?` 早退不
  泄漏子进程——修复 NPU worker 泄漏）+ 健康轮询同时监视子进程秒退 +
  统一 `which`/`free_port`。
- 四个 ASR 后端的重复 chunk 循环收敛为 `asr::run_chunks`；云端 API
  并发路径删除 `Vec<Arc<AtomicBool>>` 与未使用的双层 Option 结果表。
- 时间线：跨截图边界的文本切点吸附到最近句读/空格（±6 字符窗口），
  不再词中切断；字符守恒保持不变。
- 首次运行模型选择器与 README 去掉「绝无漏字/识别极准/零漏句」等
  不可复现的营销话术，改为客观差异描述；readme.md 移除 3 份重复章节。
- CoreML 后端移除无依据的 `unsafe impl Send`（句柄从不跨线程移动）。

### 迁移提示

- 1.0 前生成的 ASR checkpoint（无身份标记）会在下次运行时自动作废重算
  一次，属预期行为（保证转写来源可追溯）。
- `--transcript-source auto` 成为默认：有平台字幕的课程将不再走本地
  ASR。如需旧行为请传 `--transcript-source asr`。

## [0.8.1]

- 全系统优先推荐 Qwen3-ASR 1.7B 并详细标注模型错漏分析。

## [0.8.0]

- Intel NPU 硬件加速识别（`--provider npu`）。

## [0.7.0]

- ASR checkpoint / 断点续跑。

## [0.6.0]

- 数据正确性大修（场景三状态机 / 能量感知切分 / 图文边界对齐）。

## [0.5.0]

- 云端 STT、Whisper CoreML、CLI 国际化、macOS 功耗基准。