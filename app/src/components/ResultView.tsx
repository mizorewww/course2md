import { useEffect, useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import {
  ArrowLeft,
  ExternalLink,
  FileCode2,
  FileText,
  FolderOpen,
  Image as ImageIcon,
  Loader2,
} from "lucide-react";
import type { DoneStats, ResultData } from "../types";
import { openPath, revealPath } from "../lib/backend";
import { cx, errText, formatChars, formatDuration, numFrom } from "../lib/utils";
import { MarkdownImage } from "./MarkdownImage";
import { loadImageDataUrl } from "../lib/imageCache";
import { Lightbox } from "./Lightbox";
import { useToast } from "./Toast";

type Tab = "doc" | "frames" | "files";

interface ResultViewProps {
  outDir: string;
  data: ResultData | null; // null = 正在 read_result
  /** 来自 done 事件；历史页没有，则从 run.json 退化取数 */
  stats: DoneStats | null;
  onBack: () => void;
}

function FrameThumb({ path, root, onClick }: { path: string; root: string; onClick: () => void }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    loadImageDataUrl(root, path)
      .then((u) => {
        if (alive) setUrl(u);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [root, path]);
  return (
    <button
      onClick={onClick}
      className="group overflow-hidden rounded-lg border border-zinc-800 bg-zinc-900 transition-colors hover:border-zinc-600"
    >
      {url ? (
        <img
          src={url}
          alt={path.split("/").pop()}
          className="aspect-video w-full object-cover transition-opacity group-hover:opacity-85"
          loading="lazy"
        />
      ) : (
        <div className="aspect-video w-full animate-pulse bg-zinc-800/70" />
      )}
    </button>
  );
}

export function ResultView({ outDir, data, stats, onBack }: ResultViewProps) {
  const toast = useToast();
  const [tab, setTab] = useState<Tab>("doc");
  const [lightbox, setLightbox] = useState<number | null>(null);

  const title = stats?.title || data?.title || "转换结果";
  const slides = stats?.slides ?? numFrom(data?.run_json ?? null, "sections");
  const segments = stats?.segments ?? numFrom(data?.run_json ?? null, "speech_segments");
  const chars = stats?.chars ?? numFrom(data?.run_json ?? null, "chars");
  const elapsed = stats?.elapsedSecs ?? numFrom(data?.run_json ?? null, "elapsed_secs");

  // L3：只列目录里实际存在的文件（后端已过滤）；data 未回时先用 done 事件的 outputs
  const files = useMemo(() => {
    const names = data ? data.files : (stats?.outputs ?? []);
    return names.map((n) => ({ name: n, path: `${outDir}/${n}` }));
  }, [stats, data, outDir]);

  const act = (p: Promise<void>, okMsg: string) =>
    p.then(() => toast.success(okMsg)).catch((e) => toast.error(errText(e)));

  const TABS: Array<{ id: Tab; label: string; icon: typeof FileText }> = [
    { id: "doc", label: "文稿", icon: FileText },
    { id: "frames", label: `截图${data ? `（${data.frames.length}）` : ""}`, icon: ImageIcon },
    { id: "files", label: "文件", icon: FolderOpen },
  ];

  return (
    <div className="mx-auto flex h-full max-w-3xl flex-col px-8 py-7">
      {/* 头部 */}
      <div className="flex items-start gap-3">
        <button
          onClick={onBack}
          className="mt-1 rounded-lg border border-zinc-800 p-1.5 text-zinc-400 transition-colors hover:border-zinc-700 hover:text-zinc-200"
          title="返回"
        >
          <ArrowLeft size={15} />
        </button>
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-xl font-semibold text-zinc-100">{title}</h1>
          <div className="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-[12px] text-zinc-500">
            <span>{slides} 张截图</span>
            <span>{segments} 个语音段</span>
            <span>{formatChars(chars)}</span>
            <span>耗时 {formatDuration(elapsed)}</span>
          </div>
        </div>
        <div className="flex shrink-0 gap-2">
          <button
            onClick={() => act(revealPath(outDir), "已在访达中显示")}
            className="flex items-center gap-1.5 rounded-lg border border-zinc-800 px-3 py-1.5 text-[12px] text-zinc-300 transition-colors hover:border-zinc-700 hover:bg-zinc-900"
          >
            <FolderOpen size={13} />
            在访达中显示
          </button>
          {data?.has_html && (
            <button
              onClick={() => act(openPath(`${outDir}/course.html`), "已打开 HTML")}
              className="flex items-center gap-1.5 rounded-lg bg-emerald-500/15 px-3 py-1.5 text-[12px] font-medium text-emerald-300 transition-colors hover:bg-emerald-500/25"
            >
              <ExternalLink size={13} />
              打开 HTML
            </button>
          )}
        </div>
      </div>

      {/* Tabs */}
      <div className="mt-5 flex gap-1 border-b border-zinc-800">
        {TABS.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            onClick={() => setTab(id)}
            className={cx(
              "-mb-px flex items-center gap-1.5 border-b-2 px-3.5 pb-2.5 pt-1 text-[13px] transition-colors",
              tab === id
                ? "border-emerald-400 font-medium text-zinc-100"
                : "border-transparent text-zinc-500 hover:text-zinc-300",
            )}
          >
            <Icon size={14} className={tab === id ? "text-emerald-400" : undefined} />
            {label}
          </button>
        ))}
      </div>

      {/* 内容 */}
      <div className="min-h-0 flex-1 overflow-y-auto py-5">
        {!data ? (
          <div className="flex h-full items-center justify-center gap-2 text-zinc-500">
            <Loader2 size={16} className="animate-spin" />
            读取结果中…
          </div>
        ) : tab === "doc" ? (
          data.markdown ? (
            <article className="md-body">
              <ReactMarkdown
                components={{
                  img: ({ src, alt }) => (
                    <MarkdownImage
                      src={typeof src === "string" ? src : undefined}
                      alt={alt}
                      outDir={outDir}
                    />
                  ),
                }}
              >
                {data.markdown}
              </ReactMarkdown>
            </article>
          ) : (
            <EmptyNote text="没有 course.md（可能只输出了 html/json）" />
          )
        ) : tab === "frames" ? (
          data.frames.length > 0 ? (
            <div className="grid grid-cols-2 gap-3 lg:grid-cols-3 xl:grid-cols-4">
              {data.frames.map((f, i) => (
                <FrameThumb key={f} path={f} root={outDir} onClick={() => setLightbox(i)} />
              ))}
            </div>
          ) : (
            <EmptyNote text="没有截图" />
          )
        ) : (
          <div className="overflow-hidden rounded-xl border border-zinc-800">
            {files.map((f, i) => (
              <div
                key={f.name}
                className={cx(
                  "flex items-center gap-3 px-4 py-2.5 text-[13px]",
                  i > 0 && "border-t border-zinc-800/70",
                )}
              >
                <FileCode2 size={14} className="shrink-0 text-zinc-500" />
                <span className="font-mono text-zinc-300">{f.name}</span>
                <span className="min-w-0 flex-1 truncate text-right text-[11px] text-zinc-600">
                  {f.path}
                </span>
                <button
                  onClick={() => act(revealPath(f.path), "已在访达中显示")}
                  className="shrink-0 rounded-md p-1.5 text-zinc-500 transition-colors hover:bg-zinc-800 hover:text-zinc-200"
                  title="在访达中显示"
                >
                  <FolderOpen size={13} />
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      {lightbox !== null && data && data.frames.length > 0 && (
        <Lightbox
          frames={data.frames}
          index={lightbox}
          root={outDir}
          onClose={() => setLightbox(null)}
          onNavigate={setLightbox}
        />
      )}
    </div>
  );
}

function EmptyNote({ text }: { text: string }) {
  return (
    <div className="flex h-full items-center justify-center text-[13px] text-zinc-500">{text}</div>
  );
}
