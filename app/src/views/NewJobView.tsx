import { useEffect, useState } from "react";
import { AlertTriangle, ChevronDown, FolderOpen, Play, Rocket } from "lucide-react";
import type { EnvInfo, JobOpts } from "../types";
import { configuredOutRoot, pickDirectory, pickVideoFile, startJob } from "../lib/backend";
import { cx, errText } from "../lib/utils";
import { ProviderPicker } from "../components/ProviderPicker";
import { useToast } from "../components/Toast";
import { Field, NumberInput, Toggle, inputCls } from "../components/controls";

interface NewJobViewProps {
  env: EnvInfo | null;
  /** S1：有转换任务进行中时禁用「开始转换」并提示 */
  jobRunning: boolean;
  onStarted: (jobId: string) => void;
}

/** M6：LLM 润色三态——跟随配置（不传 flag）/ 强制开（--llm）/ 强制关（--no-llm） */
type LlmMode = "follow" | "on" | "off";

export function NewJobView({ env, jobRunning, onStarted }: NewJobViewProps) {
  const toast = useToast();
  const [source, setSource] = useState("");
  const [provider, setProvider] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [starting, setStarting] = useState(false);

  // api provider 表单
  const [apiBaseUrl, setApiBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [apiModel, setApiModel] = useState("");
  const [apiMode, setApiMode] = useState("transcriptions");

  // 高级选项（数值为 number|null，null = 不传 flag，跟随配置/内置默认）
  const [similarity, setSimilarity] = useState<number | null>(null);
  const [sampleInterval, setSampleInterval] = useState<number | null>(null);
  const [cooldown, setCooldown] = useState<number | null>(null);
  const [maxHeight, setMaxHeight] = useState<number | null>(null);
  const [threads, setThreads] = useState<number | null>(null);
  const [slideMode, setSlideMode] = useState("");
  // M6：全不勾 = 不传 --formats，跟随配置文件
  const [formats, setFormats] = useState<string[]>([]);
  const [llmMode, setLlmMode] = useState<LlmMode>("follow");
  const [resume, setResume] = useState(false);
  const [keepVideo, setKeepVideo] = useState(false);

  // M2：数值字段非法时禁用开始按钮
  const [invalid, setInvalid] = useState<Record<string, boolean>>({});
  const reportValid = (key: string) => (err: string | null) =>
    setInvalid((m) => {
      const v = err !== null;
      if (m[key] === v) return m;
      return { ...m, [key]: v };
    });
  const hasInvalid = Object.values(invalid).some(Boolean);

  const [outRoot, setOutRoot] = useState("");

  // S3：输出根目录永远落成绝对路径——配置文件 defaults.out，兜底 ~/course2md
  useEffect(() => {
    configuredOutRoot()
      .then(setOutRoot)
      .catch(() => undefined);
  }, []);

  const missingFfmpeg = env && !env.has_ffmpeg;
  // L1：yt-dlp 横幅只在输入看起来像 URL 时显示（本地文件用不到 yt-dlp）
  const looksLikeUrl = /^https?:\/\//i.test(source.trim());
  const missingYtdlp = env && !env.has_ytdlp && looksLikeUrl;

  const pickFile = async () => {
    try {
      const p = await pickVideoFile();
      if (p) setSource(p);
    } catch (e) {
      toast.error(errText(e));
    }
  };

  const pickOutDir = async () => {
    try {
      const d = await pickDirectory();
      if (d) setOutRoot(d);
    } catch (e) {
      toast.error(errText(e));
    }
  };

  const toggleFormat = (f: string) =>
    setFormats((prev) => (prev.includes(f) ? prev.filter((x) => x !== f) : [...prev, f]));

  const start = async () => {
    const src = source.trim();
    if (!src) {
      toast.error("请先粘贴视频链接或选择本地文件");
      return;
    }
    setStarting(true);
    try {
      const opts: JobOpts = { source: src };
      // S3：用户清空时也回落到绝对路径兜底，绝不传相对/省略 --out
      opts.out = outRoot.trim() || (await configuredOutRoot());
      // M6：provider「自动」= 不传，让配置文件/CLI 自动选择生效
      if (provider) opts.provider = provider;
      if (provider === "api") {
        if (apiBaseUrl.trim()) opts.asrApiBaseUrl = apiBaseUrl.trim();
        if (apiKey.trim()) opts.asrApiKey = apiKey.trim();
        if (apiModel.trim()) opts.asrApiModel = apiModel.trim();
        opts.asrApiMode = apiMode;
      }
      if (similarity !== null) opts.similarity = similarity;
      if (sampleInterval !== null) opts.sampleInterval = sampleInterval;
      if (cooldown !== null) opts.cooldown = cooldown;
      if (maxHeight !== null) opts.maxHeight = maxHeight;
      if (slideMode) opts.slideMode = slideMode;
      if (formats.length > 0) opts.formats = formats;
      if (llmMode === "on") opts.llm = true;
      if (llmMode === "off") opts.noLlm = true;
      if (resume) opts.resume = true;
      if (keepVideo) opts.keepVideo = true;
      if (threads !== null) opts.threads = threads;
      const jobId = await startJob(opts);
      onStarted(jobId);
    } catch (e) {
      toast.error(errText(e));
      setStarting(false);
    }
  };

  const startDisabled = starting || jobRunning || hasInvalid;

  return (
    <div className="mx-auto h-full max-w-3xl overflow-y-auto px-8 py-7">
      <h1 className="text-xl font-semibold text-zinc-100">新建任务</h1>
      <p className="mt-1 text-[13px] text-zinc-500">
        把课程视频转成「截图 + 转录」的 Markdown 笔记。
      </p>

      {/* 依赖缺失横幅 */}
      {(missingFfmpeg || missingYtdlp) && (
        <div className="mt-4 flex items-start gap-2.5 rounded-xl border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-[12px] leading-relaxed text-amber-200">
          <AlertTriangle size={15} className="mt-0.5 shrink-0 text-amber-400" />
          <div>
            <div className="font-medium">
              缺少依赖：{[missingFfmpeg && "ffmpeg", missingYtdlp && "yt-dlp"].filter(Boolean).join("、")}
            </div>
            <div className="mt-0.5 text-amber-200/80">
              macOS：<code className="rounded bg-amber-500/10 px-1 font-mono">brew install ffmpeg yt-dlp</code>
              　Ubuntu：<code className="rounded bg-amber-500/10 px-1 font-mono">sudo apt install ffmpeg && pip install yt-dlp</code>
            </div>
          </div>
        </div>
      )}

      {/* 视频来源 */}
      <section className="mt-6 rounded-2xl border border-zinc-800 bg-zinc-900/40 p-5">
        <h2 className="mb-3 text-[13px] font-medium text-zinc-300">视频来源</h2>
        <div className="flex gap-2.5">
          <input
            className={cx(inputCls, "flex-1 py-2.5")}
            placeholder="粘贴 YouTube / Bilibili 链接，或本地视频路径…"
            value={source}
            onChange={(e) => setSource(e.target.value)}
          />
          <button
            onClick={pickFile}
            className="flex shrink-0 items-center gap-1.5 rounded-lg border border-zinc-800 px-3.5 text-[13px] text-zinc-300 transition-colors hover:border-zinc-700 hover:bg-zinc-900"
          >
            <FolderOpen size={14} />
            选择文件
          </button>
        </div>
      </section>

      {/* 转写方式 */}
      <section className="mt-4 rounded-2xl border border-zinc-800 bg-zinc-900/40 p-5">
        <h2 className="mb-3 text-[13px] font-medium text-zinc-300">转写方式</h2>
        {env ? (
          <ProviderPicker providers={env.providers} value={provider} onChange={setProvider} allowAuto />
        ) : (
          <div className="text-[12px] text-zinc-500">检测环境中…</div>
        )}
        {provider === "api" && (
          <div className="mt-4 grid grid-cols-1 gap-3 border-t border-zinc-800/80 pt-4 sm:grid-cols-2">
            <Field label="Base URL">
              <input className={inputCls} placeholder="https://api.openai.com/v1" value={apiBaseUrl} onChange={(e) => setApiBaseUrl(e.target.value)} />
            </Field>
            <Field label="API Key">
              <input className={inputCls} type="password" placeholder="sk-…" value={apiKey} onChange={(e) => setApiKey(e.target.value)} />
            </Field>
            <Field label="模型">
              <input className={inputCls} placeholder="whisper-1" value={apiModel} onChange={(e) => setApiModel(e.target.value)} />
            </Field>
            <Field label="接口模式">
              <select className={inputCls} value={apiMode} onChange={(e) => setApiMode(e.target.value)}>
                <option value="transcriptions">transcriptions</option>
                <option value="chat">chat</option>
              </select>
            </Field>
          </div>
        )}
      </section>

      {/* 高级选项 */}
      <section className="mt-4 rounded-2xl border border-zinc-800 bg-zinc-900/40">
        <button
          onClick={() => setShowAdvanced((v) => !v)}
          className="flex w-full items-center justify-between px-5 py-4 text-[13px] font-medium text-zinc-300 transition-colors hover:text-zinc-100"
        >
          高级选项
          <ChevronDown size={15} className={cx("text-zinc-500 transition-transform", showAdvanced && "rotate-180")} />
        </button>
        {showAdvanced && (
          <div className="border-t border-zinc-800/80 px-5 py-4">
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
              <Field label="相似度阈值" hint="0~1，默认 0.85">
                <NumberInput value={similarity} onChange={setSimilarity} onValidate={reportValid("similarity")} min={0} max={1} minExclusive placeholder="0.85" />
              </Field>
              <Field label="采样间隔（秒）">
                <NumberInput value={sampleInterval} onChange={setSampleInterval} onValidate={reportValid("sampleInterval")} min={0} minExclusive placeholder="1.0" />
              </Field>
              <Field label="冷却（秒）">
                <NumberInput value={cooldown} onChange={setCooldown} onValidate={reportValid("cooldown")} min={0} placeholder="默认" />
              </Field>
              <Field label="截图最大高度" hint="240 ~ 2160">
                <NumberInput integer value={maxHeight} onChange={setMaxHeight} onValidate={reportValid("maxHeight")} min={240} max={2160} placeholder="默认" />
              </Field>
              <Field label="选帧策略">
                <select className={inputCls} value={slideMode} onChange={(e) => setSlideMode(e.target.value)}>
                  <option value="">默认</option>
                  <option value="first">first</option>
                  <option value="stable">stable</option>
                </select>
              </Field>
              <Field label="线程数">
                <NumberInput integer value={threads} onChange={setThreads} onValidate={reportValid("threads")} min={1} placeholder="自动" />
              </Field>
              <div className="col-span-2">
                <span className="mb-1.5 block text-[12px] text-zinc-400">输出格式</span>
                <div className="flex gap-2">
                  {["md", "html", "json"].map((f) => (
                    <button
                      key={f}
                      onClick={() => toggleFormat(f)}
                      className={cx(
                        "rounded-lg border px-3 py-1.5 font-mono text-[12px] transition-colors",
                        formats.includes(f)
                          ? "border-emerald-500/60 bg-emerald-500/10 text-emerald-300"
                          : "border-zinc-800 text-zinc-500 hover:border-zinc-700",
                      )}
                    >
                      {f}
                    </button>
                  ))}
                </div>
                <span className="mt-1 block text-[11px] text-zinc-600">全不选 = 跟随配置文件（未配置则 md + html）</span>
              </div>
            </div>
            <div className="mt-4 grid grid-cols-1 gap-2.5 border-t border-zinc-800/80 pt-4 sm:grid-cols-3">
              <div>
                <span className="mb-1.5 block text-[12px] text-zinc-400">LLM 润色</span>
                <div className="flex overflow-hidden rounded-lg border border-zinc-800">
                  {(
                    [
                      ["follow", "跟随配置"],
                      ["on", "强制开"],
                      ["off", "强制关"],
                    ] as Array<[LlmMode, string]>
                  ).map(([mode, label]) => (
                    <button
                      key={mode}
                      onClick={() => setLlmMode(mode)}
                      className={cx(
                        "flex-1 px-2 py-1.5 text-[12px] transition-colors",
                        llmMode === mode
                          ? "bg-emerald-500/15 text-emerald-300"
                          : "text-zinc-500 hover:bg-zinc-900 hover:text-zinc-300",
                      )}
                    >
                      {label}
                    </button>
                  ))}
                </div>
                <span className="mt-1 block text-[11px] text-zinc-600">跟随配置 = 不传 flag，配置文件的 llm.enabled 生效</span>
              </div>
              <Toggle checked={resume} onChange={setResume} label="断点续跑" hint="复用上次 checkpoint" />
              <Toggle checked={keepVideo} onChange={setKeepVideo} label="保留视频" hint="不删除下载的源文件" />
            </div>
          </div>
        )}
      </section>

      {/* 输出目录 */}
      <section className="mt-4 rounded-2xl border border-zinc-800 bg-zinc-900/40 p-5">
        <Field label="输出根目录（本次任务）" hint="默认取配置文件 defaults.out，未配置则 ~/course2md；每个任务在其下按 平台/标题/ID 建子目录">
          <div className="flex gap-2.5">
            <input className={cx(inputCls, "flex-1 font-mono")} value={outRoot} onChange={(e) => setOutRoot(e.target.value)} placeholder="~/course2md" />
            <button
              onClick={pickOutDir}
              className="flex shrink-0 items-center gap-1.5 rounded-lg border border-zinc-800 px-3.5 text-[13px] text-zinc-300 transition-colors hover:border-zinc-700 hover:bg-zinc-900"
            >
              <FolderOpen size={14} />
              选择目录
            </button>
          </div>
        </Field>
      </section>

      {/* 开始按钮 */}
      <button
        onClick={start}
        disabled={startDisabled}
        className={cx(
          "mt-6 flex w-full items-center justify-center gap-2 rounded-xl py-3.5 text-[15px] font-semibold transition-colors",
          startDisabled
            ? "cursor-not-allowed bg-emerald-500/40 text-emerald-100/70"
            : "bg-emerald-500 text-zinc-950 hover:bg-emerald-400",
        )}
      >
        {starting ? <Play size={17} className="animate-pulse" /> : <Rocket size={17} />}
        {starting ? "正在启动…" : "开始转换"}
      </button>
      {jobRunning && (
        <p className="mt-2 text-center text-[12px] text-amber-300/90">
          已有任务进行中，完成后才能开始新任务（可从侧栏「进行中」查看）
        </p>
      )}
      {hasInvalid && !jobRunning && (
        <p className="mt-2 text-center text-[12px] text-red-400/90">
          高级选项中有非法数值，请修正后再开始
        </p>
      )}
    </div>
  );
}
