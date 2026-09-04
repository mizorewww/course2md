// 前端与后端的唯一边界：所有组件只 import 本文件，不直接碰 @tauri-apps/api。
// 纯浏览器环境（pnpm dev，无 Tauri 注入）时自动切换到 mock 实现，
// 让「新建 → 进度 → 结果」全流程可以在浏览器里走通。

import { invoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ConfigDto,
  ConfigResponse,
  EnvInfo,
  HistoryItem,
  JobEvent,
  JobOpts,
  ResultData,
} from "../types";

declare global {
  interface Window {
    __TAURI__?: unknown;
    __TAURI_INTERNALS__?: unknown;
  }
}

/** 是否运行在真实 Tauri 环境里（withGlobalTauri 未开时是 __TAURI_INTERNALS__） */
export const isTauri: boolean =
  typeof window !== "undefined" &&
  (window.__TAURI__ !== undefined || window.__TAURI_INTERNALS__ !== undefined);

// ---------------------------------------------------------------------------
// Mock 数据与模拟任务流
// ---------------------------------------------------------------------------

const MOCK_OUT_DIR = "/Users/demo/course2md/bilibili/零基础入门 Rust 编程/BV1xx411c7mD";
const MOCK_TITLE = "零基础入门 Rust 编程";

const MOCK_ENV: EnvInfo = {
  os: "macos",
  arch: "aarch64",
  apple_silicon: true,
  has_ffmpeg: true,
  has_ffprobe: true,
  has_ytdlp: true,
  has_llama_server: true,
  cli_path: "/Applications/course2md.app/Contents/MacOS/course2md",
  cli_version: "course2md 0.3.2",
  cli_source: "bundled",
  providers: [
    {
      id: "coreml",
      label: "CoreML（Apple 原生）",
      available: true,
      recommended: true,
      note: "Neural Engine 加速，零外部依赖，推荐",
    },
    {
      id: "gpu",
      label: "GPU（llama.cpp）",
      available: true,
      recommended: false,
      note: "Metal/CUDA/Vulkan 加速，速度快",
    },
    {
      id: "npu",
      label: "NPU（Intel）",
      available: false,
      recommended: false,
      note: "仅 Intel Linux/Windows 可用",
    },
    {
      id: "cpu",
      label: "CPU",
      available: true,
      recommended: false,
      note: "纯 CPU 运行，通用兜底，较慢",
    },
    {
      id: "api",
      label: "云端 API",
      available: true,
      recommended: false,
      note: "免本地模型下载，需配置 OpenAI 兼容端点与 API Key",
    },
  ],
};

const MOCK_MARKDOWN = `# 零基础入门 Rust 编程

> 来源：bilibili · BV1xx411c7mD · 时长 12:34 · 转写：CoreML / large-v3-turbo

## 00:00 为什么学 Rust

![00:00 为什么学 Rust](frames/slide_0001.jpg)

Rust 是一门系统级编程语言，主打**内存安全**与**零成本抽象**。它不需要垃圾回收，却能编译期杜绝悬垂指针与数据竞争，非常适合写对性能与可靠性都有要求的底层组件。

## 02:31 所有权与借用

![02:31 所有权与借用](frames/slide_0002.jpg)

所有权是 Rust 最独特的机制，规则只有三条：

1. 每个值都有一个唯一的**所有者**；
2. 同一时刻只能有一个所有者；
3. 所有者离开作用域时，值被自动释放。

\`\`\`rust
let s1 = String::from("hello");
let s2 = &s1; // 借用，不转移所有权
println!("{s1} {s2}");
\`\`\`

## 05:12 枚举与模式匹配

![05:12 枚举与模式匹配](frames/slide_0003.jpg)

\`Option<T>\` 和 \`Result<T, E>\` 取代了 null 与异常，配合 \`match\` 强制处理所有分支，把大量运行时错误提前到编译期。

## 08:40 并发与无畏并行

![08:40 并发与无畏并行](frames/slide_0004.jpg)

得益于所有权系统，编译器可以在编译期证明线程之间没有数据竞争——这就是 Rust 社区常说的**无畏并发**（fearless concurrency）。

## 11:05 小结

![11:05 小结](frames/slide_0005.jpg)

- 内存安全不靠 GC，靠所有权与借用检查；
- 枚举 + match 让错误处理显式且完备；
- 零成本抽象，性能可对标 C/C++。
`;

/** mock 配置的内存态：save 后再 get 应读到保存的值（端到端回环验证用） */
const mockConfigState: { current: ConfigDto; exists: boolean } = {
  exists: true,
  current: {
    defaults: {
      out: null,
      similarity: null,
      sample_interval: null,
      cooldown: null,
      slide_mode: null,
      stable_secs: null,
      max_height: null,
      roi: null,
      threads: null,
      provider: "coreml",
      asr_model: null,
      transcript_source: null,
      max_speech: null,
      formats: ["md", "html"],
      model_dir: null,
      keep_video: null,
      no_download: null,
      resume: null,
    },
    llm: {
      enabled: true,
      base_url: "https://api.deepseek.com/v1",
      api_key: "sk-demo-key",
      model: "deepseek-chat",
      prompt: null,
      disable_hint: false,
      vision: false,
      summarize: false,
      concurrency: null,
    },
    asr_api: {
      base_url: "https://openrouter.ai/api/v1",
      api_key: "",
      model: "qwen/qwen3-asr-flash-2026-02-10",
      mode: "",
    },
  },
};

function mockGetConfig(): Promise<ConfigResponse> {
  return Promise.resolve({
    path: "/Users/demo/.config/course2md/config.toml",
    exists: mockConfigState.exists,
    config: structuredClone(mockConfigState.current),
  });
}

function mockSaveConfig(cfg: ConfigDto): Promise<void> {
  // 与 Rust validate_config 对齐的最小校验
  const d = cfg.defaults;
  if (d.provider && !["coreml", "gpu", "npu", "cpu", "api"].includes(d.provider)) {
    return Promise.reject(`defaults.provider 只能是 coreml / gpu / npu / cpu / api，收到 "${d.provider}"`);
  }
  if (d.similarity !== null && !(d.similarity > 0 && d.similarity <= 1)) {
    return Promise.reject(`defaults.similarity 必须在 (0, 1] 区间，收到 ${d.similarity}`);
  }
  if (cfg.llm.enabled && !cfg.llm.base_url.trim()) {
    return Promise.reject("已开启 LLM 润色，但 llm.base_url 未配置");
  }
  mockConfigState.current = structuredClone(cfg);
  mockConfigState.exists = true;
  return Promise.resolve();
}

const MOCK_MODELS_LIST = `已安装模型
  whisper-large-v3-turbo   1.6 GB   ~/.cache/course2md/models/ggml-large-v3-turbo.bin
  qwen3-4b-instruct        2.4 GB   ~/.cache/course2md/models/qwen3-4b-q4_k_m.gguf

未安装
  whisper-medium           1.5 GB   （运行 models download 下载）
  whisper-small            0.5 GB   （运行 models download 下载）
`;

type MockJobEventPayload = { job_id: string; event: JobEvent };
type MockJobExitPayload = { job_id: string; code: number | null };

const mockEventListeners = new Set<(p: MockJobEventPayload) => void>();
const mockExitListeners = new Set<(p: MockJobExitPayload) => void>();

function mockEmit(jobId: string, event: JobEvent) {
  mockEventListeners.forEach((cb) => cb({ job_id: jobId, event }));
}
function mockEmitExit(jobId: string, code: number | null) {
  mockExitListeners.forEach((cb) => cb({ job_id: jobId, code }));
}

/** jobId → 未触发的定时器与类别，cancel 时统一清掉；同类任务拒绝并发（对齐后端 S1） */
const mockJobs = new Map<string, { timers: number[]; kind: "conversion" | "download" }>();
let mockJobSeq = 1;

function scheduleScript(
  jobId: string,
  script: Array<[number, JobEvent]>,
  exitCode: number,
  kind: "conversion" | "download",
) {
  const timers: number[] = [];
  let t = 0;
  for (const [delay, event] of script) {
    t += delay;
    timers.push(window.setTimeout(() => mockEmit(jobId, event), t));
  }
  timers.push(
    window.setTimeout(() => {
      mockJobs.delete(jobId);
      mockEmitExit(jobId, exitCode);
    }, t + 300),
  );
  mockJobs.set(jobId, { timers, kind });
  return jobId;
}

function mockStartJob(opts: JobOpts): Promise<string> {
  if ([...mockJobs.values()].some((j) => j.kind === "conversion")) {
    return Promise.reject("已有任务进行中");
  }
  const jobId = `mock-job-${Date.now()}-${mockJobSeq++}`;
  const outDir = opts.out && opts.out.trim() !== "" ? `${opts.out.trim()}/demo-run` : MOCK_OUT_DIR;
  const script: Array<[number, JobEvent]> = [
    [300, { type: "log", level: "info", message: `course2md 0.3.2 · provider=${opts.provider || "coreml"} · 输出目录 ${outDir}` }],
    [200, { type: "stage", stage: "fetch", status: "start" }],
    [900, { type: "log", level: "info", message: "解析来源：bilibili · BV1xx411c7mD" }],
    [800, { type: "log", level: "info", message: `视频标题：${MOCK_TITLE}（12:34 · 1080p）` }],
    [400, { type: "stage", stage: "fetch", status: "done" }],
    [200, { type: "stage", stage: "download", status: "start" }],
    [600, { type: "progress", stage: "download", current: 25, total: 100, message: "下载视频 25%" }],
    [500, { type: "progress", stage: "download", current: 62, total: 100, message: "下载视频 62%" }],
    [500, { type: "progress", stage: "download", current: 100, total: 100, message: "下载完成（48.2 MB）" }],
    [300, { type: "stage", stage: "download", status: "done" }],
    [200, { type: "stage", stage: "scenes", status: "start" }],
    [700, { type: "log", level: "info", message: `场景检测：similarity=${opts.similarity ?? 0.85} · sample-interval=${(opts.sampleInterval ?? 1.0).toFixed(1)}s` }],
    [700, { type: "progress", stage: "scenes", current: 44, total: 44, message: "去重后保留 8 张幻灯片" }],
    [300, { type: "stage", stage: "scenes", status: "done" }],
    [200, { type: "stage", stage: "audio", status: "start" }],
    [500, { type: "log", level: "info", message: "ffmpeg 抽取音轨 → 16kHz mono wav" }],
    [500, { type: "stage", stage: "audio", status: "done" }],
    [200, { type: "stage", stage: "transcribe", status: "start" }],
    [500, { type: "log", level: "info", message: "ASR 模型就绪：large-v3-turbo（CoreML）" }],
    [500, { type: "progress", stage: "transcribe", current: 10, total: 96, message: "转写中 10/96 段" }],
    [500, { type: "progress", stage: "transcribe", current: 24, total: 96, message: "转写中 24/96 段" }],
    [500, { type: "progress", stage: "transcribe", current: 39, total: 96, message: "转写中 39/96 段" }],
    [500, { type: "progress", stage: "transcribe", current: 55, total: 96, message: "转写中 55/96 段" }],
    [500, { type: "progress", stage: "transcribe", current: 71, total: 96, message: "转写中 71/96 段" }],
    [500, { type: "progress", stage: "transcribe", current: 86, total: 96, message: "转写中 86/96 段" }],
    [400, { type: "log", level: "warn", message: "第 3 段置信度偏低（0.61），已保留原文" }],
    [400, { type: "progress", stage: "transcribe", current: 96, total: 96, message: "转写完成" }],
    [300, { type: "stage", stage: "transcribe", status: "done" }],
    [200, { type: "stage", stage: "llm", status: "start" }],
    [800, { type: "log", level: "info", message: "LLM 润色：合并断句、修正标点（本地 qwen3-4b）" }],
    [800, { type: "stage", stage: "llm", status: "done" }],
    [200, { type: "stage", stage: "render", status: "start" }],
    [700, { type: "log", level: "info", message: "渲染 course.md / course.html" }],
    [300, { type: "log", level: "debug", message: "run.json 已写入（checkpoint 原子落盘）" }],
    [300, { type: "stage", stage: "render", status: "done" }],
    [200, { type: "log", level: "info", message: "全部完成，耗时 12.6 秒" }],
    [100, {
      type: "done",
      out_dir: outDir,
      title: MOCK_TITLE,
      slides: 8,
      segments: 96,
      chars: 12480,
      elapsed_secs: 12.6,
      outputs: ["course.md", "course.html"],
    }],
  ];
  return Promise.resolve(scheduleScript(jobId, script, 0, "conversion"));
}

function mockDownloadModels(): Promise<string> {
  if ([...mockJobs.values()].some((j) => j.kind === "download")) {
    return Promise.reject("已有模型下载进行中");
  }
  const jobId = `mock-job-${Date.now()}-${mockJobSeq++}`;
  const script: Array<[number, JobEvent]> = [
    [300, { type: "stage", stage: "download", status: "start" }],
    [400, { type: "log", level: "info", message: "下载 whisper-large-v3-turbo（1.6 GB）" }],
    [700, { type: "progress", stage: "download", current: 400, total: 1600, message: "whisper-large-v3-turbo 25%" }],
    [800, { type: "progress", stage: "download", current: 900, total: 1600, message: "whisper-large-v3-turbo 56%" }],
    [800, { type: "progress", stage: "download", current: 1400, total: 1600, message: "whisper-large-v3-turbo 88%" }],
    [700, { type: "progress", stage: "download", current: 1600, total: 1600, message: "whisper-large-v3-turbo 完成" }],
    [400, { type: "log", level: "info", message: "校验 SHA256 通过，模型可用" }],
    [200, { type: "stage", stage: "download", status: "done" }],
  ];
  return Promise.resolve(scheduleScript(jobId, script, 0, "download"));
}

function mockCancelJob(jobId: string): Promise<boolean> {
  const job = mockJobs.get(jobId);
  if (!job) return Promise.resolve(false);
  job.timers.forEach((t) => window.clearTimeout(t));
  mockJobs.delete(jobId);
  mockEmit(jobId, { type: "log", level: "warn", message: "收到取消信号，正在终止进程组…" });
  window.setTimeout(() => mockEmitExit(jobId, 137), 400);
  return Promise.resolve(true);
}

/** 用 canvas 生成一张占位幻灯片（渐变 + 序号），返回 base64（无前缀）。 */
function mockFrameImage(path: string): string {
  const m = /(\d+)\.\w+$/.exec(path);
  const idx = m ? parseInt(m[1], 10) : 1;
  try {
    const canvas = document.createElement("canvas");
    canvas.width = 960;
    canvas.height = 540;
    const ctx = canvas.getContext("2d");
    if (ctx) {
      const hue = (idx * 47) % 360;
      const g = ctx.createLinearGradient(0, 0, 960, 540);
      g.addColorStop(0, `hsl(${hue}, 45%, 16%)`);
      g.addColorStop(1, `hsl(${(hue + 60) % 360}, 55%, 8%)`);
      ctx.fillStyle = g;
      ctx.fillRect(0, 0, 960, 540);
      ctx.fillStyle = "rgba(255,255,255,0.08)";
      ctx.fillRect(60, 60, 840, 420);
      ctx.fillStyle = "rgba(255,255,255,0.85)";
      ctx.font = "600 64px -apple-system, sans-serif";
      ctx.textAlign = "center";
      ctx.fillText(`Slide ${String(idx).padStart(4, "0")}`, 480, 260);
      ctx.font = "32px -apple-system, sans-serif";
      ctx.fillStyle = "rgba(255,255,255,0.45)";
      ctx.fillText(MOCK_TITLE, 480, 330);
      const dataUrl = canvas.toDataURL("image/jpeg", 0.85);
      return dataUrl.slice(dataUrl.indexOf(",") + 1);
    }
  } catch {
    // canvas 不可用时落到下面的内嵌像素图
  }
  // 1x1 深灰 PNG
  return "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
}

function mockReadResult(outDir: string): Promise<ResultData> {
  const frames = Array.from({ length: 8 }, (_, i) => `${outDir}/frames/slide_${String(i + 1).padStart(4, "0")}.jpg`);
  return Promise.resolve({
    markdown: MOCK_MARKDOWN,
    has_html: true,
    frames,
    files: ["course.md", "course.html", "run.json", "timeline.jsonl"],
    title: MOCK_TITLE,
    run_json: {
      version: "0.3.2",
      provider: "coreml",
      asr_model: "large-v3-turbo",
      transcript_source: "asr",
      formats: ["md", "html"],
      llm_polish: true,
      sections: 8,
      speech_segments: 96,
      chars: 12480,
      elapsed_secs: 12.6,
      source: { platform: "bilibili", id: "BV1xx411c7mD" },
    },
  });
}

function mockListHistory(): Promise<HistoryItem[]> {
  const now = Date.now() / 1000;
  const root = "/Users/demo/course2md";
  return Promise.resolve([
    {
      dir: MOCK_OUT_DIR,
      title: MOCK_TITLE,
      platform: "bilibili",
      slides: 8,
      segments: 96,
      elapsed_secs: 12.6,
      modified: Math.floor(now - 3 * 3600),
    },
    {
      dir: `${root}/youtube/CS50 Lecture 0 Scratch/dQw4w9WgXcQ`,
      title: "CS50 Lecture 0 - Scratch",
      platform: "youtube",
      slides: 21,
      segments: 240,
      chars: 30200,
      elapsed_secs: 1830,
      modified: Math.floor(now - 26 * 3600),
    },
    {
      dir: `${root}/local/操作系统导论 第 3 讲/os-week3`,
      title: "操作系统导论 第 3 讲：进程与调度",
      platform: "local",
      slides: 15,
      segments: 180,
      chars: 21800,
      elapsed_secs: 1320,
      modified: Math.floor(now - 5 * 86400),
    },
  ]);
}

// ---------------------------------------------------------------------------
// 对外 API：真实环境走 invoke，浏览器走 mock
// ---------------------------------------------------------------------------

export function detectEnvironment(): Promise<EnvInfo> {
  if (isTauri) return invoke<EnvInfo>("detect_environment");
  return new Promise((r) => window.setTimeout(() => r(MOCK_ENV), 200));
}

export function startJob(opts: JobOpts): Promise<string> {
  if (isTauri) return invoke<string>("start_job", { opts });
  return mockStartJob(opts);
}

export function cancelJob(jobId: string): Promise<boolean> {
  if (isTauri) return invoke<boolean>("cancel_job", { jobId });
  return mockCancelJob(jobId);
}

export function downloadModels(): Promise<string> {
  if (isTauri) return invoke<string>("download_models");
  return mockDownloadModels();
}

export function modelsList(): Promise<string> {
  if (isTauri) return invoke<string>("models_list");
  return Promise.resolve(MOCK_MODELS_LIST);
}

export function getConfig(): Promise<ConfigResponse> {
  if (isTauri) return invoke<ConfigResponse>("get_config");
  return mockGetConfig();
}

export function saveConfig(cfg: ConfigDto): Promise<void> {
  if (isTauri) return invoke<void>("save_config", { cfg });
  return mockSaveConfig(cfg);
}

export function defaultOutRoot(): Promise<string> {
  if (isTauri) return invoke<string>("default_out_root");
  return Promise.resolve("/Users/demo/course2md");
}

/** 生效的输出根目录：配置文件 defaults.out → GUI 兜底（~/course2md） */
export async function configuredOutRoot(): Promise<string> {
  try {
    const r = await getConfig();
    const out = r.config.defaults.out?.trim();
    if (out) return out;
  } catch {
    // 配置读失败时用兜底目录
  }
  return defaultOutRoot();
}

export function listHistory(outRoot: string): Promise<HistoryItem[]> {
  if (isTauri) return invoke<HistoryItem[]>("list_history", { outRoot });
  void outRoot;
  return new Promise((r) => window.setTimeout(() => r(mockListHistory()), 0));
}

export function readResult(outDir: string): Promise<ResultData> {
  if (isTauri) return invoke<ResultData>("read_result", { outDir });
  return new Promise((r) => window.setTimeout(() => r(mockReadResult(outDir)), 300));
}

/** 返回 base64（无 MIME 前缀）；UI 按扩展名补前缀。root 为越界防护边界（S4） */
export function readImage(root: string, path: string): Promise<string> {
  if (isTauri) return invoke<string>("read_image", { root, path });
  void root;
  return new Promise((r) => window.setTimeout(() => r(mockFrameImage(path)), 250));
}

export function openPath(path: string): Promise<void> {
  if (isTauri) return invoke<void>("open_path", { path });
  void path;
  return Promise.resolve();
}

export function revealPath(path: string): Promise<void> {
  if (isTauri) return invoke<void>("reveal_path", { path });
  void path;
  return Promise.resolve();
}

export function pickVideoFile(): Promise<string | null> {
  if (isTauri) return invoke<string | null>("pick_video_file");
  return Promise.resolve("/Users/demo/Movies/操作系统导论-week3.mp4");
}

/** L5：选择输出目录（系统目录选择对话框） */
export function pickDirectory(): Promise<string | null> {
  if (isTauri) return invoke<string | null>("pick_directory");
  return Promise.resolve("/Users/demo/course2md/picked");
}

type JobEventPayload = { job_id: string; event: JobEvent };
type JobExitPayload = { job_id: string; code: number | null };

export function onJobEvent(cb: (jobId: string, event: JobEvent) => void): Promise<UnlistenFn> {
  if (isTauri) {
    return tauriListen<JobEventPayload>("job-event", (e) => cb(e.payload.job_id, e.payload.event));
  }
  const listener = (p: MockJobEventPayload) => cb(p.job_id, p.event);
  mockEventListeners.add(listener);
  return Promise.resolve(() => mockEventListeners.delete(listener));
}

export function onJobExit(cb: (jobId: string, code: number | null) => void): Promise<UnlistenFn> {
  if (isTauri) {
    return tauriListen<JobExitPayload>("job-exit", (e) => cb(e.payload.job_id, e.payload.code));
  }
  const listener = (p: MockJobExitPayload) => cb(p.job_id, p.code);
  mockExitListeners.add(listener);
  return Promise.resolve(() => mockExitListeners.delete(listener));
}
