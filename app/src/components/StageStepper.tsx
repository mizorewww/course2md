import { Check, Loader2 } from "lucide-react";
import type { StageId } from "../types";
import { cx } from "../lib/utils";

const STAGE_ORDER: StageId[] = ["fetch", "download", "scenes", "audio", "transcribe", "llm", "render"];

const STAGE_LABEL: Record<StageId, string> = {
  fetch: "解析来源",
  download: "下载",
  scenes: "场景检测",
  audio: "提取音频",
  transcribe: "语音转写",
  llm: "LLM 润色",
  render: "渲染输出",
};

interface StageStepperProps {
  /** 各阶段状态：start=进行中，done=完成；未出现的键表示还没走到 */
  states: Partial<Record<StageId, "start" | "done">>;
  failed: boolean;
}

export function StageStepper({ states, failed }: StageStepperProps) {
  // 当前阶段 = 最后一个 status=start 的；audio/llm 可能整段跳过，跳过的不渲染
  const entries = STAGE_ORDER.map((id) => ({ id, status: states[id] })).filter(
    (e) => e.status !== undefined,
  );
  let currentIdx = -1;
  entries.forEach((e, i) => {
    if (e.status === "start") currentIdx = i;
  });
  // 从未 start 过的后续已知阶段，以「待处理」灰显接在当前之后
  const upcoming = STAGE_ORDER.filter((id) => states[id] === undefined);

  return (
    <div className="flex flex-wrap items-center gap-y-2">
      {entries.map((e, i) => {
        const done = e.status === "done";
        const current = i === currentIdx && !failed;
        return (
          <div key={e.id} className="flex items-center">
            {i > 0 && <div className="mx-2 h-px w-5 bg-zinc-800" />}
            <div
              className={cx(
                "flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[12px] transition-colors",
                done && "border-emerald-500/30 bg-emerald-500/10 text-emerald-300",
                current && "border-emerald-400/50 bg-emerald-500/15 text-emerald-200",
                !done && !current && "border-zinc-800 bg-zinc-900 text-zinc-500",
              )}
            >
              {done ? (
                <Check size={13} className="text-emerald-400" />
              ) : current ? (
                <Loader2 size={13} className="animate-spin text-emerald-400" />
              ) : (
                <span className="h-1.5 w-1.5 rounded-full bg-zinc-700" />
              )}
              {STAGE_LABEL[e.id]}
            </div>
          </div>
        );
      })}
      {/* L8：运行中才显示尾部「…」；失败/取消后未出现的阶段（如字幕路径跳过的 audio/transcribe）不再吊灰 */}
      {!failed && upcoming.length > 0 && entries.length > 0 && (
        <div className="flex items-center">
          <div className="mx-2 h-px w-5 bg-zinc-800" />
          <span className="text-[12px] text-zinc-600">…</span>
        </div>
      )}
    </div>
  );
}

export { STAGE_LABEL, STAGE_ORDER };
