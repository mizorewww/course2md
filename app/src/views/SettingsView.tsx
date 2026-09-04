import { useEffect, useRef, useState } from "react";
import { Cloud, Download, ExternalLink, MessageSquareText, Package, Save, SlidersHorizontal, Wand2 } from "lucide-react";
import type { ConfigDto, DefaultsDto, LogLine, StageId } from "../types";
import {
  downloadModels,
  getConfig,
  modelsList,
  onJobEvent,
  onJobExit,
  openPath,
  saveConfig,
} from "../lib/backend";
import { cx, errText, nowTime } from "../lib/utils";
import { useToast } from "../components/Toast";
import {
  Field,
  NumberInput,
  PasswordInput,
  Section,
  SelectInput,
  TextInput,
  Toggle,
  inputCls,
} from "../components/controls";

/** CLI 内置默认值（src/config.rs 顶部常量 / settings.rs 模板），只用于 placeholder 展示 */
const BUILTIN = {
  out: "./out",
  similarity: "0.85",
  sample_interval: "1.0",
  cooldown: "10.0",
  stable_secs: "0.8",
  max_height: "1080",
  threads: "4",
  max_speech: "20.0",
  asr_model: "qwen3-1.7b",
  concurrency: "8",
};

const PROVIDER_OPTIONS = [
  { value: "", label: "自动（按平台推荐）" },
  { value: "coreml", label: "coreml — Apple 原生" },
  { value: "gpu", label: "gpu — llama.cpp（Metal/CUDA/Vulkan）" },
  { value: "npu", label: "npu — Intel NPU" },
  { value: "cpu", label: "cpu — 纯 CPU 兜底" },
  { value: "api", label: "api — 云端 STT" },
];

const TRANSCRIPT_OPTIONS = [
  { value: "", label: "auto — 字幕优先，无字幕走本地 ASR" },
  { value: "subtitle", label: "subtitle — 强制字幕（无字幕报错）" },
  { value: "asr", label: "asr — 跳过字幕直接识别" },
];

const FORMATS = ["md", "html", "json"] as const;

type DKey = keyof DefaultsDto;

export function SettingsView() {
  const toast = useToast();

  // 配置文件（结构化表单）
  const [cfg, setCfg] = useState<ConfigDto | null>(null);
  const [cfgPath, setCfgPath] = useState("");
  const [cfgExists, setCfgExists] = useState(false);
  const [saving, setSaving] = useState(false);

  // M2：数值字段非法时禁用保存
  const [invalid, setInvalid] = useState<Record<string, boolean>>({});
  const reportValid = (key: string) => (err: string | null) =>
    setInvalid((m) => {
      const v = err !== null;
      if (m[key] === v) return m;
      return { ...m, [key]: v };
    });
  const hasInvalid = Object.values(invalid).some(Boolean);

  useEffect(() => {
    getConfig()
      .then((r) => {
        setCfg(r.config);
        setCfgPath(r.path);
        setCfgExists(r.exists);
      })
      .catch((e) => toast.error(errText(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const setD = <K extends DKey>(key: K, val: DefaultsDto[K]) =>
    setCfg((c) => (c ? { ...c, defaults: { ...c.defaults, [key]: val } } : c));
  const setLlm = (patch: Partial<ConfigDto["llm"]>) =>
    setCfg((c) => (c ? { ...c, llm: { ...c.llm, ...patch } } : c));
  const setApi = (patch: Partial<ConfigDto["asr_api"]>) =>
    setCfg((c) => (c ? { ...c, asr_api: { ...c.asr_api, ...patch } } : c));

  const toggleFormat = (f: (typeof FORMATS)[number]) =>
    setCfg((c) => {
      if (!c) return c;
      const cur = c.defaults.formats ?? ["md", "html"];
      const next = cur.includes(f) ? cur.filter((x) => x !== f) : [...cur, f];
      return { ...c, defaults: { ...c.defaults, formats: next.length ? next : null } };
    });

  const save = async () => {
    if (!cfg) return;
    setSaving(true);
    try {
      await saveConfig(cfg);
      setCfgExists(true);
      toast.success("配置已保存");
    } catch (e) {
      toast.error(errText(e));
    } finally {
      setSaving(false);
    }
  };

  // 模型管理（保持原有行为）
  const [models, setModels] = useState<string | null>(null);
  const [dlJobId, setDlJobId] = useState<string | null>(null);
  const [dlPct, setDlPct] = useState<number | null>(null);
  const [dlLog, setDlLog] = useState<string>("");
  const logSeq = useRef(0);
  const dlLines = useRef<LogLine[]>([]);

  const refreshModels = () => {
    modelsList()
      .then(setModels)
      .catch((e) => setModels(`models list 失败：${errText(e)}`));
  };
  useEffect(refreshModels, []);

  useEffect(() => {
    if (!dlJobId) return;
    let unEvent: (() => void) | undefined;
    let unExit: (() => void) | undefined;
    onJobEvent((id, event) => {
      if (id !== dlJobId) return;
      // M9：兼容批量形态
      const events = event.type === "logs" ? event.logs : [event];
      for (const ev of events) {
        if (ev.type === "progress" && ev.total > 0) {
          setDlPct(Math.min(100, Math.round((ev.current / ev.total) * 100)));
          if (ev.message) setDlLog(ev.message);
        } else if (ev.type === "log") {
          setDlLog(ev.message);
          dlLines.current.push({ id: ++logSeq.current, level: ev.level, message: ev.message, time: nowTime() });
        } else if (ev.type === "stage") {
          void (ev.stage as StageId);
        }
      }
    }).then((u) => (unEvent = u));
    onJobExit((id, code) => {
      if (id !== dlJobId) return;
      setDlJobId(null);
      setDlPct(null);
      if (code === 0) {
        toast.success("模型下载完成");
        refreshModels();
      } else {
        toast.error(`模型下载失败（退出码 ${code ?? "?"}）`);
      }
    }).then((u) => (unExit = u));
    return () => {
      unEvent?.();
      unExit?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dlJobId]);

  const startDownload = async () => {
    try {
      dlLines.current = [];
      setDlLog("");
      setDlPct(0);
      setDlJobId(await downloadModels());
    } catch (e) {
      toast.error(errText(e));
      setDlPct(null);
    }
  };

  const formats = cfg?.defaults.formats ?? ["md", "html"];

  return (
    <div className="mx-auto h-full max-w-3xl overflow-y-auto px-8 py-7">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-zinc-100">设置</h1>
          <p className="mt-1 text-[12px] text-zinc-500">
            留空 = 未设置，回落内置默认值；placeholder 中显示内置默认。
          </p>
        </div>
        <button
          onClick={save}
          disabled={!cfg || saving || hasInvalid}
          className={cx(
            "flex items-center gap-1.5 rounded-lg px-4 py-2 text-[13px] font-medium transition-colors",
            saving || !cfg || hasInvalid
              ? "cursor-not-allowed bg-emerald-500/30 text-emerald-100/60"
              : "bg-emerald-500 text-zinc-950 hover:bg-emerald-400",
          )}
        >
          <Save size={14} />
          {saving ? "保存中…" : "保存配置"}
        </button>
      </div>

      {!cfg ? (
        <div className="mt-6 space-y-4">
          {[0, 1, 2].map((i) => (
            <div key={i} className="h-40 animate-pulse rounded-2xl border border-zinc-800 bg-zinc-900/40" />
          ))}
        </div>
      ) : (
        <div className="mt-5 space-y-4">
          {/* 输出 */}
          <Section icon={Save} title="输出" desc="每个任务在输出根目录下按 平台/标题/编号 归类。">
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label="输出根目录" hint={`留空 = 内置默认 ${BUILTIN.out}`} className="sm:col-span-2">
                <TextInput mono value={cfg.defaults.out ?? ""} onChange={(v) => setD("out", v)} placeholder={BUILTIN.out} />
              </Field>
              <div className="sm:col-span-2">
                <span className="mb-1.5 block text-[12px] text-zinc-400">输出格式</span>
                <div className="flex gap-2">
                  {FORMATS.map((f) => (
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
                <span className="mt-1 block text-[11px] text-zinc-600">全不选 = 内置默认 md + html</span>
              </div>
            </div>
          </Section>

          {/* 转写 */}
          <Section icon={SlidersHorizontal} title="转写" desc="识别后端与转写策略；命令行参数仍可临时覆盖这里的默认值。">
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label="识别后端 provider" hint="gpu 推荐（Qwen3-ASR 1.7B，3 分钟音频约 13 秒）；coreml 零依赖；api 免本地模型">
                <SelectInput
                  value={cfg.defaults.provider ?? ""}
                  onChange={(v) => setD("provider", (v || null) as DefaultsDto["provider"])}
                  options={PROVIDER_OPTIONS}
                />
              </Field>
              <Field label="识别模型 asr_model" hint="qwen3-1.7b 中文课程更好；whisper 适合纯英文/多语种。可直接输入自定义模型名">
                <TextInput
                  mono
                  value={cfg.defaults.asr_model ?? ""}
                  onChange={(v) => setD("asr_model", v)}
                  placeholder={BUILTIN.asr_model}
                />
              </Field>
              <Field label="转写来源 transcript_source">
                <SelectInput
                  value={cfg.defaults.transcript_source ?? ""}
                  onChange={(v) => setD("transcript_source", (v || null) as DefaultsDto["transcript_source"])}
                  options={TRANSCRIPT_OPTIONS}
                />
              </Field>
              <div className="grid grid-cols-2 gap-3">
                <Field label="单段语音最长（秒）" hint="过长自动在静音点切分">
                  <NumberInput value={cfg.defaults.max_speech} onChange={(v) => setD("max_speech", v)} onValidate={reportValid("max_speech")} min={0} minExclusive placeholder={BUILTIN.max_speech} />
                </Field>
                <Field label="识别线程数">
                  <NumberInput integer value={cfg.defaults.threads} onChange={(v) => setD("threads", v)} onValidate={reportValid("threads")} min={1} placeholder={BUILTIN.threads} />
                </Field>
              </div>
            </div>
          </Section>

          {/* 云端 STT */}
          <Section
            icon={Cloud}
            title="云端 STT（asr_api）"
            desc="provider = api 时生效。base_url 可指向任何 OpenAI 兼容端点；API Key 留空保存即清除。"
          >
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <Field label="Base URL" className="sm:col-span-2">
                <TextInput mono value={cfg.asr_api.base_url} onChange={(v) => setApi({ base_url: v })} placeholder="https://openrouter.ai/api/v1" />
              </Field>
              <Field label="API Key" hint="留空保存即清除已保存的 Key">
                <PasswordInput value={cfg.asr_api.api_key} onChange={(v) => setApi({ api_key: v })} placeholder="sk-or-…" />
              </Field>
              <Field label="模型" hint="如 qwen/qwen3-asr-flash、openai/whisper-large-v3-turbo">
                <TextInput mono value={cfg.asr_api.model} onChange={(v) => setApi({ model: v })} placeholder="qwen/qwen3-asr-flash-2026-02-10" />
              </Field>
              <Field
                label="接口模式 mode"
                hint="chat 模式用于 gpt-4o-audio / Gemini / Qwen2-Audio 等支持音频输入的多模态 LLM（POST /chat/completions）"
                className="sm:col-span-2"
              >
                <SelectInput
                  value={cfg.asr_api.mode}
                  onChange={(v) => setApi({ mode: v as ConfigDto["asr_api"]["mode"] })}
                  options={[
                    { value: "", label: "transcriptions — POST /audio/transcriptions（默认，专用转录端点）" },
                    { value: "chat", label: "chat — POST /chat/completions（多模态 LLM）" },
                  ]}
                />
              </Field>
            </div>
          </Section>

          {/* LLM 润色 */}
          <Section
            icon={Wand2}
            title="LLM 润色（llm）"
            desc="对转录文本做断句合并、标点修正；开启后 base_url 与 model 必填。"
          >
            <div className="grid grid-cols-1 gap-3">
              <Toggle checked={cfg.llm.enabled} onChange={(v) => setLlm({ enabled: v })} label="启用 LLM 润色" hint="默认关闭；开启后转写完成自动润色" />
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <Field label="Base URL">
                  <TextInput mono value={cfg.llm.base_url} onChange={(v) => setLlm({ base_url: v })} placeholder="https://api.deepseek.com/v1" />
                </Field>
                <Field label="API Key" hint="留空保存即清除已保存的 Key">
                  <PasswordInput value={cfg.llm.api_key} onChange={(v) => setLlm({ api_key: v })} placeholder="sk-…" />
                </Field>
                <Field label="模型">
                  <TextInput mono value={cfg.llm.model} onChange={(v) => setLlm({ model: v })} placeholder="deepseek-chat" />
                </Field>
                <Field label="并发数 concurrency" hint="Section 间相互独立；自建网关可调高">
                  <NumberInput integer value={cfg.llm.concurrency} onChange={(v) => setLlm({ concurrency: v })} onValidate={reportValid("concurrency")} min={1} placeholder={BUILTIN.concurrency} />
                </Field>
              </div>
              <Field label="自定义校对指令 prompt" hint="输出格式约束由系统自动追加；留空用内置">
                <textarea
                  className={cx(inputCls, "h-24 resize-y font-mono leading-relaxed")}
                  value={cfg.llm.prompt ?? ""}
                  onChange={(e) => setLlm({ prompt: e.target.value || null })}
                  placeholder="留空 = 内置校对指令"
                  spellCheck={false}
                />
              </Field>
              <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-3">
                <Toggle checked={cfg.llm.vision} onChange={(v) => setLlm({ vision: v })} label="视觉润色" hint="附幻灯片截图辅助纠正术语（模型须支持图片输入）" />
                <Toggle checked={cfg.llm.summarize} onChange={(v) => setLlm({ summarize: v })} label="自动生成总结" hint="TL;DR/要点/大纲写入 md/html 开头（需启用润色）" />
                <Toggle checked={cfg.llm.disable_hint} onChange={(v) => setLlm({ disable_hint: v })} label="关闭开启提示" hint="不再在任务结束时提示可开启 LLM" />
              </div>
            </div>
          </Section>

          {/* 高级 */}
          <Section icon={SlidersHorizontal} title="高级（defaults）" desc="截图检测与任务行为参数。">
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
              <Field label="相似度阈值" hint="SSIM，越高越敏感、截图越多">
                <NumberInput value={cfg.defaults.similarity} onChange={(v) => setD("similarity", v)} onValidate={reportValid("similarity")} min={0} max={1} minExclusive placeholder={BUILTIN.similarity} />
              </Field>
              <Field label="采样间隔（秒）" hint="每隔几秒检查一次画面">
                <NumberInput value={cfg.defaults.sample_interval} onChange={(v) => setD("sample_interval", v)} onValidate={reportValid("sample_interval")} min={0} minExclusive placeholder={BUILTIN.sample_interval} />
              </Field>
              <Field label="冷却（秒）" hint="新截图之后至少间隔">
                <NumberInput value={cfg.defaults.cooldown} onChange={(v) => setD("cooldown", v)} onValidate={reportValid("cooldown")} min={0} placeholder={BUILTIN.cooldown} />
              </Field>
              <Field label="截图最大高度">
                <NumberInput integer value={cfg.defaults.max_height} onChange={(v) => setD("max_height", v)} onValidate={reportValid("max_height")} min={240} max={2160} placeholder={BUILTIN.max_height} />
              </Field>
              <Field label="选帧策略 slide_mode">
                <SelectInput
                  value={cfg.defaults.slide_mode ?? ""}
                  onChange={(v) => setD("slide_mode", (v || null) as DefaultsDto["slide_mode"])}
                  options={[
                    { value: "", label: "默认（stable）" },
                    { value: "first", label: "first（首个不同帧）" },
                    { value: "stable", label: "stable（等画面稳定）" },
                  ]}
                />
              </Field>
              <Field label="稳定时长（秒）" hint="stable 模式判定画面稳定的时长">
                <NumberInput value={cfg.defaults.stable_secs} onChange={(v) => setD("stable_secs", v)} onValidate={reportValid("stable_secs")} min={0} minExclusive placeholder={BUILTIN.stable_secs} />
              </Field>
              <Field label="比较区域 roi" hint='如 "40%,0%-100%,100%"' className="col-span-2">
                <TextInput mono value={cfg.defaults.roi ?? ""} onChange={(v) => setD("roi", v)} placeholder="整幅画面" />
              </Field>
              <Field label="模型目录 model_dir" hint="llama.cpp GGUF；CoreML 模型缓存在 ~/Library/Caches/qwen3-speech/" className="col-span-2">
                <TextInput mono value={cfg.defaults.model_dir ?? ""} onChange={(v) => setD("model_dir", v)} placeholder="~/.cache/course2md/models" />
              </Field>
            </div>
            <div className="mt-4 grid grid-cols-1 gap-2.5 border-t border-zinc-800/80 pt-4 sm:grid-cols-3">
              <Toggle
                checked={cfg.defaults.keep_video ?? false}
                onChange={(v) => setD("keep_video", v ? true : null)}
                label="保留视频"
                hint="保留下载的 media.mp4"
              />
              <Toggle
                checked={cfg.defaults.no_download ?? false}
                onChange={(v) => setD("no_download", v ? true : null)}
                label="不下载"
                hint="跳过视频下载（配合本地文件/续跑）"
              />
              <Toggle
                checked={cfg.defaults.resume ?? false}
                onChange={(v) => setD("resume", v ? true : null)}
                label="断点续跑"
                hint="复用上次 checkpoint"
              />
            </div>
          </Section>

          {/* 模型管理（保留） */}
          <Section
            icon={Package}
            title="模型管理"
            actions={
              <button
                onClick={startDownload}
                disabled={dlJobId !== null}
                className={cx(
                  "flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-[12px] font-medium transition-colors",
                  dlJobId !== null
                    ? "cursor-wait bg-emerald-500/30 text-emerald-100/60"
                    : "bg-emerald-500/15 text-emerald-300 hover:bg-emerald-500/25",
                )}
              >
                <Download size={13} />
                {dlJobId !== null ? "下载中…" : "下载模型"}
              </button>
            }
          >
            {dlPct !== null && (
              <div className="mb-3">
                <div className="h-1.5 overflow-hidden rounded-full bg-zinc-800">
                  <div className="h-full rounded-full bg-emerald-500 transition-all duration-500" style={{ width: `${dlPct}%` }} />
                </div>
                <div className="mt-1.5 truncate font-mono text-[11px] text-zinc-500">{dlLog}</div>
              </div>
            )}
            <pre className="max-h-48 overflow-auto rounded-lg bg-black/60 p-3.5 font-mono text-[11px] leading-relaxed text-zinc-400">
              {models ?? "加载中…"}
            </pre>
          </Section>

          {/* 配置文件路径 + 底部保存 */}
          <div className="flex items-center justify-between rounded-2xl border border-zinc-800 bg-zinc-900/40 px-5 py-4">
            <div className="min-w-0">
              <div className="truncate font-mono text-[11px] text-zinc-500">{cfgPath}</div>
              <div className="mt-0.5 text-[11px] text-zinc-600">
                {cfgExists ? "配置文件已存在" : "配置文件尚不存在，保存后创建"}
              </div>
            </div>
            <div className="flex shrink-0 gap-2">
              <button
                onClick={() => openPath(cfgPath).catch((e) => toast.error(errText(e)))}
                className="flex items-center gap-1.5 rounded-lg border border-zinc-800 px-3 py-1.5 text-[12px] text-zinc-300 transition-colors hover:border-zinc-700 hover:bg-zinc-900"
              >
                <ExternalLink size={13} />
                在编辑器中打开
              </button>
              <button
                onClick={save}
                disabled={saving || hasInvalid}
                className="flex items-center gap-1.5 rounded-lg bg-emerald-500 px-3 py-1.5 text-[12px] font-medium text-zinc-950 transition-colors hover:bg-emerald-400 disabled:opacity-50"
              >
                <Save size={13} />
                保存配置
              </button>
            </div>
          </div>

          <div className="flex items-center gap-1.5 pb-2 text-[11px] text-zinc-600">
            <MessageSquareText size={11} />
            保存时会自动备份旧配置为 config.toml.bak。
          </div>
        </div>
      )}
    </div>
  );
}
