import { useEffect, useState } from "react";
import { loadImageDataUrl } from "../lib/imageCache";

/** markdown 里的相对图片（frames/slide_0001.jpg）→ 拼绝对路径 → read_image → data URL */
export function useResolvedImage(src: string, outDir: string) {
  const [dataUrl, setDataUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let alive = true;
    setDataUrl(null);
    setFailed(false);
    if (/^(https?:|data:)/.test(src)) {
      setDataUrl(src);
      return;
    }
    const abs = `${outDir}/${src}`;
    loadImageDataUrl(outDir, abs)
      .then((url) => {
        if (alive) setDataUrl(url);
      })
      .catch(() => {
        if (alive) setFailed(true);
      });
    return () => {
      alive = false;
    };
  }, [src, outDir]);

  return { dataUrl, failed };
}

interface MarkdownImageProps {
  src?: string;
  alt?: string;
  outDir: string;
}

export function MarkdownImage({ src, alt, outDir }: MarkdownImageProps) {
  const { dataUrl, failed } = useResolvedImage(src ?? "", outDir);
  if (!src) return null;
  if (failed) {
    return (
      <span className="block rounded-lg border border-zinc-800 bg-zinc-900 px-4 py-6 text-center text-[12px] text-zinc-500">
        图片加载失败：{src}
      </span>
    );
  }
  if (!dataUrl) {
    // 骨架块：按 16:9 占位，避免布局跳动
    return <span className="block aspect-video animate-pulse rounded-lg bg-zinc-800/70" />;
  }
  return (
    <img
      src={dataUrl}
      alt={alt ?? ""}
      className="rounded-lg border border-zinc-800"
      loading="lazy"
    />
  );
}
