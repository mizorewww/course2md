import { useEffect, useRef, useState } from "react";
import { ArrowDownToLine, Pause, Play, Terminal } from "lucide-react";
import type { LogLine } from "../types";
import { cx } from "../lib/utils";

const LEVEL_CLASS: Record<LogLine["level"], string> = {
  info: "text-zinc-300",
  warn: "text-amber-300",
  error: "text-red-400",
  debug: "text-zinc-500",
};

const LEVEL_BADGE: Record<LogLine["level"], string> = {
  info: "text-zinc-500",
  warn: "text-amber-500",
  error: "text-red-500",
  debug: "text-zinc-600",
};

interface LogConsoleProps {
  lines: LogLine[];
  className?: string;
}

export function LogConsole({ lines, className }: LogConsoleProps) {
  const [paused, setPaused] = useState(false);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!paused && boxRef.current) {
      boxRef.current.scrollTop = boxRef.current.scrollHeight;
    }
  }, [lines, paused]);

  return (
    <div className={cx("flex flex-col overflow-hidden rounded-xl border border-zinc-800 bg-black", className)}>
      <div className="flex items-center justify-between border-b border-zinc-800/80 px-3.5 py-2">
        <div className="flex items-center gap-2 text-[12px] text-zinc-500">
          <Terminal size={13} />
          日志
          <span className="text-zinc-600">{lines.length} 行</span>
        </div>
        <button
          onClick={() => setPaused((p) => !p)}
          className={cx(
            "flex items-center gap-1.5 rounded-md border px-2 py-1 text-[11px] transition-colors",
            paused
              ? "border-amber-500/40 bg-amber-500/10 text-amber-300"
              : "border-zinc-800 text-zinc-500 hover:text-zinc-300",
          )}
        >
          {paused ? <Play size={11} /> : <Pause size={11} />}
          {paused ? "已暂停滚动" : "暂停滚动"}
        </button>
      </div>
      <div ref={boxRef} className="log-scroll min-h-0 flex-1 overflow-y-auto px-3.5 py-2.5 font-mono text-[12px] leading-relaxed">
        {lines.length === 0 ? (
          <div className="flex h-full items-center justify-center gap-2 text-zinc-600">
            <ArrowDownToLine size={13} />
            等待输出…
          </div>
        ) : (
          lines.map((l) => (
            <div key={l.id} className="flex gap-2 whitespace-pre-wrap break-all">
              <span className="shrink-0 select-none text-zinc-600">{l.time}</span>
              <span className={cx("shrink-0 select-none uppercase", LEVEL_BADGE[l.level])}>
                {l.level.padEnd(5)}
              </span>
              <span className={LEVEL_CLASS[l.level]}>{l.message}</span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
