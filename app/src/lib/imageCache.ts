// M5：模块级图片缓存。ResultView 缩略图 / MarkdownImage / Lightbox 共享，
// 同一绝对路径只读一次盘；缓存 Promise，并发请求自动合并。失败不缓存（可重试）。

import { readImage } from "./backend";

/** 按扩展名推断 data URL 的 MIME（后端 read_image 只给裸 base64） */
export function mimeFor(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "png") return "image/png";
  if (ext === "svg") return "image/svg+xml";
  if (ext === "webp") return "image/webp";
  if (ext === "gif") return "image/gif";
  return "image/jpeg";
}

const cache = new Map<string, Promise<string>>();

/** 读取图片为 data URL（带缓存）。root 是后端 read_image 的越界防护边界（S4） */
export function loadImageDataUrl(root: string, path: string): Promise<string> {
  let p = cache.get(path);
  if (!p) {
    p = readImage(root, path).then((b64) => `data:${mimeFor(path)};base64,${b64}`);
    p.catch(() => cache.delete(path));
    cache.set(path, p);
  }
  return p;
}
