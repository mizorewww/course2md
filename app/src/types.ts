// 与 Rust 后端（src-tauri/src/lib.rs）对齐的类型定义。
// invoke 参数全部 camelCase；事件通过 "job-event" / "job-exit" 广播。

export type LogLevel = "info" | "warn" | "error" | "debug";

export type StageId =
  | "fetch"
  | "download"
  | "scenes"
  | "audio"
  | "transcribe"
  | "llm"
  | "render";

export type JobEvent =
  | { type: "log"; level: LogLevel; message: string }
  | { type: "stage"; stage: StageId; status: "start" | "done" }
  | { type: "progress"; stage: string; current: number; total: number; message?: string }
  | {
      type: "done";
      out_dir: string;
      title: string;
      slides: number;
      segments: number;
      chars: number;
      elapsed_secs: number;
      outputs: string[];
    }
  | { type: "error"; message: string }
  /** M9：后端 50ms 窗口合并多条时发批量形态；内层不会再嵌套 logs */
  | { type: "logs"; logs: JobEvent[] };

export interface ProviderInfo {
  id: string;
  label: string;
  available: boolean;
  recommended: boolean;
  note: string;
}

export interface EnvInfo {
  os: string;
  arch: string;
  apple_silicon: boolean;
  has_ffmpeg: boolean;
  has_ffprobe: boolean;
  has_ytdlp: boolean;
  has_llama_server: boolean;
  cli_path: string;
  cli_version: string;
  cli_source: "bundled" | "path" | "env" | string;
  providers: ProviderInfo[];
}

/** start_job 的 opts：除 source 外全部可选，None 就不传对应 flag。 */
export interface JobOpts {
  source: string;
  out?: string;
  provider?: string;
  asrModel?: string;
  asrApiBaseUrl?: string;
  asrApiKey?: string;
  asrApiModel?: string;
  asrApiMode?: string;
  similarity?: number;
  sampleInterval?: number;
  cooldown?: number;
  maxHeight?: number;
  slideMode?: string;
  formats?: string[];
  llm?: boolean;
  noLlm?: boolean;
  keepVideo?: boolean;
  resume?: boolean;
  transcriptSource?: string;
  threads?: number;
}

export interface HistoryItem {
  dir: string;
  title: string;
  platform: string;
  slides: number;
  segments: number;
  elapsed_secs: number;
  /** unix 秒 */
  modified: number;
}

export interface ResultData {
  markdown: string | null;
  has_html: boolean;
  frames: string[];
  /** L3：目录里实际存在的产物文件（后端已过滤），文件 tab 只列这些 */
  files: string[];
  run_json: Record<string, unknown> | null;
  title: string;
}

export interface SettingsFile {
  path: string;
  toml: string;
  exists: boolean;
}

/** 配置 DTO（与 src-tauri ConfigDto / CLI src/settings.rs 对齐，snake_case） */
export interface DefaultsDto {
  out: string | null;
  similarity: number | null;
  sample_interval: number | null;
  cooldown: number | null;
  slide_mode: "first" | "stable" | null;
  stable_secs: number | null;
  max_height: number | null;
  roi: string | null;
  threads: number | null;
  provider: "coreml" | "gpu" | "npu" | "cpu" | "api" | null;
  asr_model: string | null;
  transcript_source: "auto" | "subtitle" | "asr" | null;
  max_speech: number | null;
  formats: Array<"md" | "html" | "json"> | null;
  model_dir: string | null;
  keep_video: boolean | null;
  no_download: boolean | null;
  resume: boolean | null;
}

export interface LlmDto {
  enabled: boolean;
  base_url: string;
  api_key: string;
  model: string;
  prompt: string | null;
  disable_hint: boolean;
  vision: boolean;
  summarize: boolean;
  /** null = 内置默认 8 */
  concurrency: number | null;
}

export interface AsrApiDto {
  base_url: string;
  api_key: string;
  model: string;
  /** "" = 未设置 = transcriptions */
  mode: "transcriptions" | "chat" | "";
}

export interface ConfigDto {
  defaults: DefaultsDto;
  llm: LlmDto;
  asr_api: AsrApiDto;
}

export interface ConfigResponse {
  path: string;
  exists: boolean;
  config: ConfigDto;
}

/** done 事件的统计部分，结果视图复用。 */
export interface DoneStats {
  outDir: string;
  title: string;
  slides: number;
  segments: number;
  chars: number;
  elapsedSecs: number;
  outputs: string[];
}

/** 进度视图聚合的任务运行状态 */
export interface JobState {
  jobId: string;
  stages: Partial<Record<StageId, "start" | "done">>;
  progress: { stage: string; current: number; total: number; message?: string } | null;
  logs: LogLine[];
  error: string | null;
  cancelled: boolean;
}

export interface LogLine {
  id: number;
  level: LogLevel;
  message: string;
  time: string;
}
