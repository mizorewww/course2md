import { useEffect, useMemo, useState } from "react";
import { ChevronLeft, ChevronRight, X } from "lucide-react";
import { loadImageDataUrl } from "../lib/imageCache";

interface LightboxProps {
  frames: string[];
  index: number;
  /** read_image 的越界防护根目录（S4），即输出目录 */
  root: string;
  onClose: () => void;
  onNavigate: (index: number) => void;
}

/** 大图遮罩层：Esc / 点击空白关闭，← → 切换 */
export function Lightbox({ frames, index, root, onClose, onNavigate }: LightboxProps) {
  const path = frames[index];
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setUrl(null);
    loadImageDataUrl(root, path)
      .then((u) => {
        if (alive) setUrl(u);
      })
      .catch(() => {
        if (alive) setUrl(null);
      });
    return () => {
      alive = false;
    };
  }, [root, path]);

  const prev = useMemo(() => (index - 1 + frames.length) % frames.length, [index, frames.length]);
  const next = useMemo(() => (index + 1) % frames.length, [index, frames.length]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (e.key === "ArrowLeft") onNavigate(prev);
      if (e.key === "ArrowRight") onNavigate(next);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, onNavigate, prev, next]);

  const name = path.split("/").pop() ?? path;

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/80 backdrop-blur-sm"
      onClick={onClose}
    >
      <button
        className="absolute right-5 top-5 rounded-full border border-zinc-700 bg-zinc-900/80 p-2 text-zinc-400 transition-colors hover:text-zinc-100"
        onClick={onClose}
      >
        <X size={18} />
      </button>

      <button
        className="absolute left-5 rounded-full border border-zinc-700 bg-zinc-900/80 p-2.5 text-zinc-400 transition-colors hover:text-zinc-100"
        onClick={(e) => {
          e.stopPropagation();
          onNavigate(prev);
        }}
      >
        <ChevronLeft size={20} />
      </button>

      <div
        className="flex max-h-[85vh] max-w-[80vw] flex-col items-center gap-3"
        onClick={(e) => e.stopPropagation()}
      >
        {url ? (
          <img
            src={url}
            alt={name}
            className="max-h-[78vh] max-w-full rounded-lg border border-zinc-700 object-contain shadow-2xl"
          />
        ) : (
          <div className="aspect-video w-[60vw] animate-pulse rounded-lg bg-zinc-800" />
        )}
        <div className="text-[12px] text-zinc-400">
          {name} · {index + 1} / {frames.length}
        </div>
      </div>

      <button
        className="absolute right-5 rounded-full border border-zinc-700 bg-zinc-900/80 p-2.5 text-zinc-400 transition-colors hover:text-zinc-100"
        onClick={(e) => {
          e.stopPropagation();
          onNavigate(next);
        }}
      >
        <ChevronRight size={20} />
      </button>
    </div>
  );
}
