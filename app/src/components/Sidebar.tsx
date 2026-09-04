import { Clapperboard, Clock, Loader2, PlusCircle, RefreshCw, Settings } from "lucide-react";
import type { EnvInfo } from "../types";
import { cx } from "../lib/utils";

export type NavId = "new-job" | "running" | "history" | "settings";

interface SidebarProps {
  active: NavId;
  onNavigate: (id: NavId) => void;
  env: EnvInfo | null;
  /** M3：env 加载失败时的错误文案（区别于加载中的 null） */
  envError: string | null;
  /** M3：重新检测环境 */
  onRefreshEnv: () => void;
  /** S1：有转换任务进行中时显示「进行中」导航项 */
  running: boolean;
}

const NAV: Array<{ id: NavId; label: string; icon: typeof PlusCircle }> = [
  { id: "new-job", label: "新建任务", icon: PlusCircle },
  { id: "history", label: "历史", icon: Clock },
  { id: "settings", label: "设置", icon: Settings },
];

function Dot({ ok }: { ok: boolean }) {
  return (
    <span
      className={cx(
        "inline-block h-1.5 w-1.5 rounded-full",
        ok ? "bg-emerald-400" : "bg-zinc-600",
      )}
    />
  );
}

const SOURCE_LABEL: Record<string, string> = {
  bundled: "内置",
  path: "PATH",
  env: "环境变量",
};

export function Sidebar({ active, onNavigate, env, envError, onRefreshEnv, running }: SidebarProps) {
  const detecting = env === null && envError === null;
  return (
    <aside className="flex w-56 shrink-0 flex-col border-r border-zinc-800/80 bg-zinc-950">
      {/* 应用名 + 版本 */}
      <div className="flex items-center gap-2.5 px-5 pb-5 pt-6">
        <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-emerald-500/15 text-emerald-400">
          <Clapperboard size={17} />
        </div>
        <div className="min-w-0">
          <div className="text-sm font-semibold tracking-wide text-zinc-100">course2md</div>
          <div className="truncate text-[11px] text-zinc-500">
            {env?.cli_version || "桌面客户端"}
          </div>
        </div>
      </div>

      {/* 导航 */}
      <nav className="flex flex-col gap-1 px-3">
        {running && (
          <button
            onClick={() => onNavigate("running")}
            className={cx(
              "flex items-center gap-2.5 rounded-lg px-3 py-2 text-left text-[13px] transition-colors",
              active === "running"
                ? "bg-zinc-800/80 font-medium text-zinc-100"
                : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200",
            )}
          >
            <Loader2 size={15} className="animate-spin text-emerald-400" />
            进行中
          </button>
        )}
        {NAV.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            onClick={() => onNavigate(id)}
            className={cx(
              "flex items-center gap-2.5 rounded-lg px-3 py-2 text-left text-[13px] transition-colors",
              active === id
                ? "bg-zinc-800/80 font-medium text-zinc-100"
                : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200",
            )}
          >
            <Icon size={15} className={active === id ? "text-emerald-400" : undefined} />
            {label}
          </button>
        ))}
      </nav>

      <div className="flex-1" />

      {/* 环境状态 */}
      <div className="mx-3 mb-4 rounded-xl border border-zinc-800/80 bg-zinc-900/50 px-3.5 py-3">
        <div className="mb-2 flex items-center justify-between">
          <span className="text-[11px] font-medium uppercase tracking-wider text-zinc-500">
            环境
          </span>
          <button
            onClick={onRefreshEnv}
            title="重新检测"
            className="rounded p-1 text-zinc-600 transition-colors hover:bg-zinc-800 hover:text-zinc-300"
          >
            <RefreshCw size={11} className={detecting ? "animate-spin" : undefined} />
          </button>
        </div>
        {envError ? (
          <div className="space-y-2">
            <p className="text-[11px] leading-relaxed text-red-400">环境检测失败：{envError}</p>
            <button
              onClick={onRefreshEnv}
              className="flex items-center gap-1 rounded-md border border-zinc-800 px-2 py-1 text-[11px] text-zinc-400 transition-colors hover:border-zinc-700 hover:text-zinc-200"
            >
              <RefreshCw size={11} />
              重试
            </button>
          </div>
        ) : (
          <div className="space-y-1.5 text-[12px] text-zinc-400">
            <div className="flex items-center justify-between">
              <span>CLI</span>
              <span className="text-zinc-300">
                {env ? (SOURCE_LABEL[env.cli_source] ?? env.cli_source) : "…"}
              </span>
            </div>
            <div className="flex items-center justify-between">
              <span>ffmpeg</span>
              <Dot ok={env?.has_ffmpeg ?? false} />
            </div>
            <div className="flex items-center justify-between">
              <span>yt-dlp</span>
              <Dot ok={env?.has_ytdlp ?? false} />
            </div>
            <div className="flex items-center justify-between">
              <span>llama-server</span>
              <Dot ok={env?.has_llama_server ?? false} />
            </div>
          </div>
        )}
      </div>
    </aside>
  );
}
