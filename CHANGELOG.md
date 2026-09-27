# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 与
[语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

- 修复分 P 视频（如 Bilibili 多集课程）候选列表只显示"视频 n"的问题：现在自动补全真实分集标题、首帧预览图与时长（每个 BV 一次公开接口请求，失败时保留原候选不阻断任务）。
- 视频下载重新显示实时进度：下载阶段显示已下字节、实测速度、预计剩余与确定性进度条；AI 校对与摘要阶段实时显示累计上传/下载 token 计数，不再只有"等待服务返回结果"。
- 设置页侧栏不再出现装饰性大圆圈（共享选择轨道的圆角发丝边在竖向方形轨道上被画成整圆）；设置 → 服务与账号新增「AI 账号」分组：Codex 订阅登录（导入/浏览器授权/退出）与 Ollama 本地服务（自动检测地址与模型列表、一键预填添加，已添加时直达服务条目）平级可见。

## [2.0.0-rc.5] — 2026-09-23

- 新增插件化登录框架：登录方法实现统一 trait 后一行注册即可接入 `--login/--logout`；新增 `--login ollama`（本地 Ollama 服务发现与模型选择）与 `--login codex`（导入 codex CLI 的 ChatGPT 登录态，token 自动刷新；无 codex CLI 时可浏览器 PKCE 授权）。桌面端 AI 服务同样支持 Ollama 本地与 Codex 订阅登录。
- 修复语音端点持续返回空文本时的静默失败：连续空分片现在提前熔断并明确报错（此前约 200 次请求全部记为"静音"且不产出笔记）；全空转写缓存不再毒化后续重跑；润色失败的结果消息带失败计数与首个错误原因。
- 修复纯文本 LLM 校对按 Section 切片导致的请求放大缺陷：跨 Section 统一按 20 条语音合并批次，避免因画面频繁切换导致请求数被放大十几倍。
- CLI 增加 `--summarize` 与 `--no-summarize` 参数，支持直接在命令行开启或禁用 AI 总结，且支持单独使用 `--summarize` 快速生成视频总结（跳过逐句校对）。
- 发布范围、安装和已知验证边界见 [RC5 发布说明](docs/releases/v2.0.0-rc.5.md)。

## [2.0.0-rc.4] — 2026-09-15

- 修复 macOS 上 GPU 不被正确识别的三层根因：`doctor` 不再把 BLAS/Accelerate 等 CPU 后端误报为 GPU 设备，桌面端环境探测与引擎共享设备枚举超时预算，设置中的本机引擎选择可靠持久化。
- 修复两个可冻结整个桌面界面的高危缺陷：任务凭据不再在主线程同步解析；所有阻塞调用（钥匙串、受限 HTTP、子进程探测、模型文件校验、课程库迁移扫描等）统一移至专用 OS 线程。
- 任务可靠性：作业监视器不再因子进程输出管道未关闭而永久卡住；取消覆盖补齐到 ASR 服务启动、视频下载、AI 校对与截图/转写联合中止；确定性错误不再无意义重试。
- 桌面端排版、同级选择控件、状态徽章、信息图标与动效时长统一进共享设计原语；阅读器章节标题、目录截断、行内截图限高与滚动条偏好一致兑现；模型下载统一显示速度与预计剩余，可暂停可恢复。
- 发布范围、安装和已知验证边界见 [RC4 发布说明](docs/releases/v2.0.0-rc.4.md)。

## [2.0.0-rc.3] — 2026-09-10

- 修复 Linux/Windows 桌面端无法保存 AI 服务 API Key 的问题：优先使用系统钥匙串（Linux Secret Service / Windows 凭据管理器），不可用时回退到文件存储（0600 权限）。
- 保存的 AI 服务从「停用」改为真正的「删除」：一并移除版本、草稿、凭据与默认绑定，恢复旧备份也不能复活派发；已发出的请求结果仍会保存。
- 设置页统一为共享卡片与紧凑状态行：应用页的运行检查、默认识别模型与缓存并入同一版式；生成笔记页移除突兀的说明句。
- 首次引导的引擎选择平铺展示，llama.cpp 在本机存在即可用（不再依赖 --list-devices 成功）；已保存的 API Key 以掩码占位而不是空白框。
- 课程库的全部笔记/文件夹筛选与列表/卡片布局改为一层选项；任务页标题与摘要获得统一页边距。
- 模型下载在设置、引导与任务进度中统一显示带标签的速度与预计剩余时间。
- 内部：桌面端项目代码告警清零，渲染期不再进行文件系统探测，移除死代码与一次性源码守卫测试；新增 `just install` 本机测试安装。
- 发布范围、安装和已知验证边界见 [RC3 发布说明](docs/releases/v2.0.0-rc.3.md)。

## [2.0.0-rc.2] — 2026-09-10

- 桌面端虚拟化阅读器、任务列表与课程库，只渲染可见区域；合并工作区写入与重绘，阅读位置保存限流，长内容滚动与切换更顺滑。
- 引擎 I/O 提速：复用 LLM 连接、音频切分与识别双缓冲、长音频内存占用大幅降低、产物复制与恢复校验并行、在线视频单次探测；输出与行为不变。
- CLI：场景扫描显示预计总帧数，字幕阶段与转写剩余时间可见；已有配置但未设置服务时也触发首次运行向导；补齐中英文提示，`models inspect` / `models prepare` 输出摘要，`--quiet` 生效。
- 移除 1.x 笔记导入、旧 `config.toml` 迁移界面和旧工作区状态迁移；旧格式文件改为保留并重置的恢复路径，不再静默迁移。同时移除 `OPENROUTER_API_KEY` 环境变量回退和 `@alpha` Homebrew 别名。
- 合并冗余测试并让 CI 在三个平台运行集成套件；精简一次性评审记录，重写中英文 README 与开发、打包文档。
- 发布范围、安装和已知验证边界见 [RC2 发布说明](docs/releases/v2.0.0-rc.2.md)。

## [2.0.0-rc.1] — 2026-09-09

- 简化转换流程：添加视频后直接开始转换，完成后进入正文；后台任务完成时保留用户当前页面，移除草稿管理、计划确认和独立完成页。
- 重做四步首次使用引导：选择默认识别方式与模型、配置和测试 AI 服务、连接 Bilibili、准备识别模型；设置中可重新开始。
- 统一设置分组、控件对齐、图标、主题与状态动效，支持十种配色、界面缩放和减少动态效果。
- 完善任务暂停、重启恢复和部分结果补做；补做保留原任务配置，导出失败可以单独重试，完成后按用户当前阅读意图接续新版。
- 改进阅读搜索、目录、截图网格、窄窗工具栏和图片查看；保留阅读位置，导出 HTML 与图片资源可直接定位。
- 改进服务模型发现、配置检查失效处理、模型准备进度、课程库离线恢复与存储迁移反馈。
- 修复 macOS 同步重绘循环、Metal drawable 生命周期与页面渲染中的磁盘检查；性能采样仅在显式启用时运行。
- 修复 Linux/Windows 引导模型名称解析的构建依赖，以及 Windows 课程库重新关联时对同路径替换文件夹的识别。
- 发布范围、安装和已知验证边界见 [RC1 发布说明](docs/releases/v2.0.0-rc.1.md)。

## [2.0.0-alpha.1] — 2026-09-08

- 重做桌面工作台、笔记库、阅读页与设置，在线与本地草稿分别保存，单次生成选项与默认设置独立。
- 引入持久任务、暂停/继续/退出恢复、后台结果提示和可补做的部分结果。
- 生成笔记按版本保存，保留旧文件与手工修改，阅读位置按版本记忆。
- 恢复旧配置导入与损坏修复入口，验证存储迁移、发布中断与冷启动恢复。
- Apple 原生识别从本地缓存离线加载，修复拉丁语言字幕合并时的句间空格。
- 修复 Windows 笔记发布时的文件同步权限及字幕导入时的文件占用，补齐内置服务样本与 Windows 源码准备。
- 增加独立 Homebrew alpha GUI/CLI 通道，预发布不更改稳定包和 GitHub Latest。
- 安装步骤、测试范围与升级说明见 [发布说明](docs/releases/v2.0.0-alpha.1.md)。

## [1.7.0] — 2026-09-06

### 新增

- 桌面首次设置向导、分组设置与自动保存、运行环境安装帮助、版本与构建信息。

- 桌面课程库支持列表、卡片和按文件夹分组，记住展示偏好；文件夹移动与删除不移动或删除课程输出。
- 桌面设置与 Bilibili 来源表单提供扫码登录、账号状态检查和过期恢复，无需先使用终端登录。
- Bilibili 扫码登录与退出：`--login bilibili` / `--logout bilibili`；CLI 和桌面预览共享登录状态，凭据与课程输出分开保存。
- GPU 卸载控制（#12）：`--gpu-layers <0-99>`、`--mmproj-offload` / `--no-mmproj-offload`，以及共享配置中的 `gpu_layers`、`mmproj_offload`。
- 输出目录确定后的失败也写入诊断 `run.json`；成功和失败记录可包含实际 llama-server 启动参数。

### 修复

- 统一桌面控件高度、分组与卡片对齐；导出格式使用标准多选框，展开与标签指示条采用可中断非线性动效。
- 保存课程库展示偏好不再触发整库扫描，切换输出根时清除旧库折叠状态。
- Unix 文件锁在作用域结束时显式解锁，避免并行启动进程继承描述符时延长锁的寿命。
- Bilibili 412 有限重试；视频解析、字幕或下载失败时提示登录，首次设置推荐登录以获取账号有权访问的更高清晰度。
- GPU 模式检查 llama-server 可用设备，缺少 GPU 后端时明确报错；`doctor` 显示设备，Arch 安装文档及 AUR 可选依赖补齐 Vulkan 后端。
- CPU 模式显式禁用主模型、算子和 mmproj 的 GPU 卸载（#12）。旧版参数不足且无法确认没有 GPU 时停止运行，提示升级或使用 CPU-only 构建。
- llama-server 帮助探测改用临时文件，避免长帮助输出填满管道，导致探测误超时而漏加 CPU 保护参数。
- 合并 GPU 控制时保留字幕优先、并行场景检测/转写及桌面进度事件；CLI 与桌面端统一应用新增配置。

## [1.6.0] — 2026-09-05

- GPUI 原生桌面端替代 Tauri，提供三平台打包、转换进度、课程库与笔记阅读。
- 修复断点、并发写入、字幕时序与子进程清理；详见 [1.6.0 发布说明](docs/releases/v1.6.0.md)。

## [1.5.0] — 2026-09-05

### 新增

- **桌面客户端（GUI）**：`app/` 目录新增 Tauri v2 客户端——转写后端按本机
  能力推荐、阶段步进器 + 实时进度条 + 日志控制台、应用内结果预览
  （文稿/截图网格/文件）、历史记录、设置页（模型下载、config.toml 编辑）；
  sidecar 内置 CLI 二进制，取消任务连带终止子进程。`packaging/build-app.sh`
  一键构建 macOS .app/.dmg
- **发版附三平台 GUI 安装包**：macOS dmg（Developer ID 签名 + 公证 +
  staple）、Windows nsis 安装包、Linux AppImage，随 Release 一同产出
- **`--json` 机器可读模式**：stdout 输出 NDJSON 事件流
  （stage/progress/log/done/error），供 GUI 与脚本消费；`models download`
  同样支持。人类模式输出与行为不变

### 修复

- macOS 签名公证适配：`mlx.metallib` 移入 `.app` 的 Resources 密封区
  （codesign 把 Contents/MacOS/ 下文件都当代码，数据文件在其中无法签名），
  GUI 启动 sidecar 时以该目录为 CWD 命中 MLX 的兜底搜索路径

## [1.4.1] — 2026-09-03

### 修复

- **LLM 输出契约统一**（#11）：润色结果改为顶层对象 `{"segments":[...]}`，
  与 `response_format: json_object` 一致（此前提示词要求顶层数组，契约
  互相矛盾）；解析器仍兼容模型直接返回顶层数组的情况
- **视觉降级收敛**（#11）：仅当端点返回参数类 4xx（疑似不支持图片输入）
  才降级纯文本；401/403/404/429/5xx、网络与解析失败不再误触发降级
- **请求放大收敛**（#11）：4xx 确定性错误与限流失败后不再递归拆分批次；
  `response_format` 兼容性降级剔除 403/404；响应缺失 message.content
  直接报错而非静默空串；LLM 配置缺失在进入润色前一次性拦截跳过
- `llm setup` 连接测试在 `vision=true` 时附带测试图片，修复
  「文本通了但图片请求不可用」的假阳性（#11）

### 改进

- `config init` 模板与 `config show` / `llm status` 补齐 `vision`、
  `concurrency` 等非敏感字段展示（#11）
- 中英文 README 补充 LLM 文本与截图上传的隐私说明（#11）

## [1.4.0] — 2026-09-03

### 新增

- **首次使用向导**：首次运行（无配置文件 + 未传 --provider + 交互终端）
  进入交互式配置——先选本地/云端，本地按本机能力推荐后端
  （coreml > gpu > npu > cpu，推荐项置顶），gpu/cpu 可确认预下载
  2.4GB 模型或改用云端；云端引导填写端点/Key/模型。
  选择写入 config.toml 并提示后续 `--provider` 覆盖方式；
  非交互环境不触发，走自动默认
- **Apple 原生后端新增 qwen3-1.7b 默认模型**：Qwen3-ASR 1.7B MLX 8bit
  （GPU 路径，WER 约为 0.6B 一半、速度约 3 倍）；原 0.6B CoreML/ANE
  保留为 `--asr-model qwen3-0.6b`（省电低功耗）。
  首次模型选择写入 config.toml（旧 asr_model 标记文件自动迁移）

### 改进

- **模型下载错误分类**：4xx（除 429）确定性错误不再退避重试，直接失败；
  错误信息携带响应体尾部，镜像 404/鉴权失败一眼可见（思路来自 #9，
  感谢 @sleepinlava）
- 模型 manifest 改原子写入，防止半截 JSON
- **日志更清爽**：默认输出不再带 RFC3339 时间戳（`-vv` 恢复完整格式）；
  总结块排版优化；字幕路径不再误打「音频」行、api/coreml 不再打无关的
  「模型目录」行；缺依赖报错附平台对应的安装命令
- 模型下载失败的错误信息提示 HF_ENDPOINT 镜像

## [1.3.0] — 2026-09-02

### 新增

- **云端 STT 支持 chat 模式**：`[asr_api] mode = "chat"`（或 `--asr-api-mode chat`）
  走 OpenAI 兼容 `/chat/completions`（`input_audio` 音频输入），可直接用
  gpt-4o-audio-preview、Gemini、Qwen2-Audio 等支持音频的多模态 LLM 转录；
  `base_url` 自定义端点用法补充进用户文档

### 修复（全面代码审查）

- **checkpoint 崩溃恢复自我污染**：resume 打开 append 前先把 asr.jsonl 截断到
  最后一个完整行，避免半截末行与新记录拼成中间损坏行导致整档作废
- **Swift 改动不生效**：build.rs 不再用 stamp 文件跳过 `swift build`
  （SPM 自己做增量），改 shim.swift 后一定重链
- **api_key 掩码 panic**：非 ASCII key 按字节切片越界，改为按字符截取
- **字幕 HTML 实体双重反转义**：`&amp;` 移到最后替换，`&amp;lt;` 不再变成 `<`
- 多处 `partial_cmp().unwrap()` 对 NaN panic 改为 `total_cmp`；
  字幕解析出口过滤非有限时间戳
- `unsafe libc::isatty(2)` 改为 `std::io::IsTerminal`（检查 stdin；
  顺带修复 Windows 编译）
- 空转写（静音课件）不再发 LLM 总结请求（纯幻觉输出）
- NPU worker 关闭流程：去掉零等待的 SIGTERM+SIGKILL 叠加，
  改为 shutdown → 轮询等待 → 进程组 SIGTERM → Drop 兜底
- **response_format 降级重试只在 400 时触发**（401 不再双发、429 不再放大限流）
- **总结块改用显式哨兵注释** `<!-- course2md:summary -->`：插入/幂等/删除统一
  走哨兵；`--force` 不再误删用户在总结后手工追加的内容
- `fetch_subtitle` 区分「命令失败」（warn 带 stderr）与「平台无字幕」（None）
- ffprobe 输出改 `-of json` 解析，不再按逗号+空白切文本
- `display()` 拼路径改为 `OsString` 拼接，非 UTF-8 路径不再损坏
- CoreML 未知 `--asr-model` 不再静默归为 qwen3，直接报错（与 NPU 后端对齐）

### 改进

- **ASR 层重构**：四个 provider 分支抽 `run_with_cp` 统一 checkpoint 骨架；
  临时目录改 RAII guard（`TempWorkDir`）；云端 STT 与 llama-server 转写
  加指数退避重试；`run_api` 手写线程池改 `std::thread::scope`
  （11 个 Arc clone 全删，错误链保留）
- **截图抽帧 4 路并发**（JoinSet 限流），长课件抽帧阶段显著提速
- **checkpoint 身份改用独立 schema 版本**（不再随 course2md patch 版本
  作废全部 ASR 进度；本次升级会使旧 checkpoint 失效一次）
- **llama 模型身份单一真相源**：`models::llama_gguf_identity()`，
  checkpoint identity 与下载逻辑同源
- **默认值单一来源**：内置默认值收敛为 `config.rs` 顶部常量，
  main/settings 模板/`config show` 三处共用；`config show` 补齐
  no_download/resume/stable_secs/max_height/transcript_source 展示
- **云端 STT 环境变量改名 `COURSE2MD_ASR_API_KEY`**（`OPENROUTER_API_KEY`
  兼容保留），与可配置 base_url 解耦
- `settings::save`：Unix 下一步建成 0600（消除明文窗口），覆盖前备份
  `config.toml.bak`
- LLM 润色同一 Section 的截图只读盘+base64 一次；重试不再克隆数 MB
  请求体；summarize map-reduce 4 路并发；总结 prompt 注入视频标题/UP主
- 2.4GB 模型下载加超时与重试，失败保留 `.part` 并提示
- `wait_ready` 校验 `/health` 响应体，端口被无关服务抢占不再误判就绪
- NPU worker 脚本挪出输出目录（进临时目录、原子写入）；
  worker stderr 不再 inherit 污染进度条
- 删除死代码：`parse_text_array`、`AsrInput`、NPU base64 分支、
  Python 默认端口、vendored `mlx.metallib`（3.8MB，由 SPM 构建产物回退）
- **移除半途而废的 i18n 模块**（help 平行表已漂移且静默失败；
  界面输出统一为中文，CLI help 保持英文 derive）

### 改进（此前已列入）

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
