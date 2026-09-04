/** 简单 className 拼接 */
export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}

/** 秒 → "1 分 23 秒" / "45 秒" */
export function formatDuration(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return "-";
  const s = Math.round(secs);
  if (s < 60) return `${s} 秒`;
  const m = Math.floor(s / 60);
  const rest = s % 60;
  if (m < 60) return rest > 0 ? `${m} 分 ${rest} 秒` : `${m} 分钟`;
  const h = Math.floor(m / 60);
  return `${h} 小时 ${m % 60} 分`;
}

/** 字数 → "1.2 万字" */
export function formatChars(n: number): string {
  if (n >= 10000) return `${(n / 10000).toFixed(1)} 万字`;
  return `${n} 字`;
}

/** unix 秒 → "3 小时前" */
export function relativeTime(unixSecs: number): string {
  const diff = Date.now() / 1000 - unixSecs;
  if (diff < 60) return "刚刚";
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  if (diff < 86400 * 30) return `${Math.floor(diff / 86400)} 天前`;
  const d = new Date(unixSecs * 1000);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(
    d.getDate(),
  ).padStart(2, "0")}`;
}

/** 当前时间 HH:MM:SS（日志行前缀） */
export function nowTime(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** 从 run.json 里安全取数字 */
export function numFrom(obj: Record<string, unknown> | null, key: string): number {
  const v = obj?.[key];
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

/** 错误对象 → 用户可读文案（Tauri invoke reject 是纯字符串，mock 可能是 Error） */
export function errText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String(e);
}
