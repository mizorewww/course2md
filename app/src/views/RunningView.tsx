import { useState } from "react";
import { ArrowLeft, XCircle } from "lucide-react";
import type { JobState, StageId } from "../types";
import { cancelJob } from "../lib/backend";
import { cx, errText } from "../lib/utils";
import { StageStepper, STAGE_LABEL } from "../components/StageStepper";
import { LogConsole } from "../components/LogConsole";
import { useToast } from "../components/Toast";

interface RunningViewProps {
  job: JobState;
  onCancel: () => void;
  onBack: () => void;
}

export function RunningView({ job, onCancel, onBack }: RunningViewProps) {
  const toast = useToast();
  const [cancelling, setCancelling] = useState(false);

  const failed = job.error !== null || job.cancelled;
  const pct =
    job.progress && job.progress.total > 0
      ? Math.min(100, Math.round((job.progress.current / job.progress.total) * 100))
      : null;
  const stageLabel = job.progress
    ? (STAGE_LABEL[job.progress.stage as StageId] ?? job.progress.stage)
    : null;

  const cancel = async () => {
    setCancelling(true);
    try {
      await cancelJob(job.jobId);
      onCancel();
    } catch (e) {
      toast.error(errText(e));
      setCancelling(false);
    }
  };

  return (
    <div className="mx-auto flex h-full max-w-3xl flex-col px-8 py-7">
      {/* 头部 */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-zinc-100">
            {failed ? (job.cancelled ? "已取消" : "任务失败") : "正在转换"}
          </h1>
          <p className="mt-0.5 font-mono text-[11px] text-zinc-600">{job.jobId}</p>
        </div>
        {failed ? (
          <button
            onClick={onBack}
            className="flex items-center gap-1.5 rounded-lg border border-zinc-800 px-3.5 py-2 text-[13px] text-zinc-300 transition-colors hover:border-zinc-700 hover:bg-zinc-900"
          >
            <ArrowLeft size={14} />
            返回
          </button>
        ) : (
          <button
            onClick={cancel}
            disabled={cancelling}
            className={cx(
              "flex items-center gap-1.5 rounded-lg border border-red-500/50 px-3.5 py-2 text-[13px] text-red-300 transition-colors",
              cancelling ? "opacity-50" : "hover:bg-red-500/10",
            )}
          >
            <XCircle size={14} />
            {cancelling ? "取消中…" : "取消"}
          </button>
        )}
      </div>

      {/* 错误横幅（M1：取消文案提示断点续跑） */}
      {failed && (
        <div className="mt-4 rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-[13px] leading-relaxed text-red-200">
          {job.cancelled
            ? "任务已被取消。中间产物已保留，下次新建任务时勾选「断点续跑」可从中断处继续。"
            : job.error}
        </div>
      )}

      {/* 阶段步进器 */}
      <div className="mt-5 rounded-2xl border border-zinc-800 bg-zinc-900/40 px-5 py-4">
        <StageStepper states={job.stages} failed={failed} />
      </div>

      {/* 当前进度条 */}
      {!failed && job.progress && pct !== null && (
        <div className="mt-4 rounded-2xl border border-zinc-800 bg-zinc-900/40 px-5 py-4">
          <div className="mb-2 flex items-center justify-between text-[12px]">
            <span className="text-zinc-400">
              {stageLabel} · {job.progress.message ?? `${job.progress.current}/${job.progress.total}`}
            </span>
            <span className="font-mono text-emerald-300">{pct}%</span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-zinc-800">
            <div
              className="h-full rounded-full bg-emerald-500 transition-all duration-500"
              style={{ width: `${pct}%` }}
            />
          </div>
        </div>
      )}

      {/* 日志 */}
      <LogConsole lines={job.logs} className="mt-4 flex-1" />
    </div>
  );
}
