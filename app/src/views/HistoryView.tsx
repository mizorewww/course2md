import { useCallback, useEffect, useState } from "react";
import { Clock, FileVideo, History, Image as ImageIcon, RefreshCw } from "lucide-react";
import type { HistoryItem } from "../types";
import { listHistory, readResult, configuredOutRoot } from "../lib/backend";
import { errText, formatDuration, relativeTime } from "../lib/utils";
import { useToast } from "../components/Toast";

const PLATFORM_STYLE: Record<string, string> = {
  bilibili: "bg-pink-500/15 text-pink-300",
  youtube: "bg-red-500/15 text-red-300",
  local: "bg-sky-500/15 text-sky-300",
};

interface HistoryViewProps {
  onOpen: (outDir: string) => void;
}

export function HistoryView({ onOpen }: HistoryViewProps) {
  const toast = useToast();
  const [items, setItems] = useState<HistoryItem[] | null>(null);
  const [outRoot, setOutRoot] = useState<string>("");

  const refresh = useCallback(async () => {
    try {
      const root = await configuredOutRoot();
      setOutRoot(root);
      setItems(await listHistory(root));
    } catch (e) {
      toast.error(errText(e));
      setItems([]);
    }
  }, [toast]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const open = async (dir: string) => {
    try {
      await readResult(dir); // 先确认可读，再切结果视图
      onOpen(dir);
    } catch (e) {
      toast.error(errText(e));
    }
  };

  return (
    <div className="mx-auto h-full max-w-3xl overflow-y-auto px-8 py-7">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-zinc-100">历史</h1>
          <p className="mt-1 truncate font-mono text-[11px] text-zinc-600">{outRoot}</p>
        </div>
        <button
          onClick={refresh}
          className="flex items-center gap-1.5 rounded-lg border border-zinc-800 px-3 py-1.5 text-[12px] text-zinc-300 transition-colors hover:border-zinc-700 hover:bg-zinc-900"
        >
          <RefreshCw size={13} />
          刷新
        </button>
      </div>

      {items === null ? (
        <div className="mt-6 space-y-3">
          {[0, 1, 2].map((i) => (
            <div key={i} className="h-20 animate-pulse rounded-2xl border border-zinc-800 bg-zinc-900/40" />
          ))}
        </div>
      ) : items.length === 0 ? (
        <div className="mt-24 flex flex-col items-center gap-3 text-zinc-500">
          <History size={36} className="text-zinc-700" />
          <p className="text-[13px]">还没有转换记录</p>
          <p className="text-[12px] text-zinc-600">去「新建任务」转换第一个课程视频吧</p>
        </div>
      ) : (
        <div className="mt-5 space-y-3">
          {items.map((it) => (
            <button
              key={it.dir}
              onClick={() => open(it.dir)}
              className="w-full rounded-2xl border border-zinc-800 bg-zinc-900/40 p-4 text-left transition-colors hover:border-zinc-700 hover:bg-zinc-900"
            >
              <div className="flex items-center gap-2.5">
                <span className="min-w-0 flex-1 truncate text-[14px] font-medium text-zinc-100">
                  {it.title}
                </span>
                {it.platform && (
                  <span
                    className={`shrink-0 rounded-full px-2 py-0.5 text-[10px] font-medium ${PLATFORM_STYLE[it.platform] ?? "bg-zinc-800 text-zinc-400"}`}
                  >
                    {it.platform}
                  </span>
                )}
              </div>
              <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-[12px] text-zinc-500">
                <span className="flex items-center gap-1">
                  <ImageIcon size={12} />
                  {it.slides} 截图
                </span>
                <span className="flex items-center gap-1">
                  <FileVideo size={12} />
                  {it.segments} 段
                </span>
                <span>耗时 {formatDuration(it.elapsed_secs)}</span>
                <span className="ml-auto flex items-center gap-1 text-zinc-600">
                  <Clock size={12} />
                  {relativeTime(it.modified)}
                </span>
              </div>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
