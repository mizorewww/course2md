import { useEffect, useState, type ReactNode } from "react";
import { Eye, EyeOff } from "lucide-react";
import { cx } from "../lib/utils";

export const inputCls =
  "w-full rounded-lg border border-zinc-800 bg-zinc-900/70 px-3 py-2 text-[13px] text-zinc-200 placeholder-zinc-600 outline-none transition-colors focus:border-emerald-500/60";

/** 分组卡片 */
export function Section({
  icon: Icon,
  title,
  desc,
  children,
  actions,
}: {
  icon?: typeof Eye;
  title: string;
  desc?: string;
  children: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <section className="rounded-2xl border border-zinc-800 bg-zinc-900/40 p-5">
      <div className="mb-3 flex items-center justify-between">
        <div className="flex items-center gap-2 text-[13px] font-medium text-zinc-300">
          {Icon && <Icon size={14} className="text-zinc-500" />}
          {title}
        </div>
        {actions}
      </div>
      {desc && <p className="-mt-1.5 mb-3 text-[11px] leading-relaxed text-zinc-600">{desc}</p>}
      {children}
    </section>
  );
}

/** 字段：label + 控件 + 一行说明 */
export function Field({
  label,
  hint,
  children,
  className,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <label className={cx("block", className)}>
      <span className="mb-1.5 block text-[12px] text-zinc-400">{label}</span>
      {children}
      {hint && <span className="mt-1 block text-[11px] leading-relaxed text-zinc-600">{hint}</span>}
    </label>
  );
}

export function TextInput({
  value,
  onChange,
  placeholder,
  mono,
  type = "text",
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  mono?: boolean;
  type?: string;
}) {
  return (
    <input
      className={cx(inputCls, mono && "font-mono")}
      type={type}
      value={value}
      placeholder={placeholder}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}

/** 数字输入：内部用文本框承载，空串 = null（未设置，回落内置默认）。
 *  M2：Number.isFinite + min/max 范围校验，非法时红字提示并经 onValidate 上报（父组件禁用提交）。 */
export function NumberInput({
  value,
  onChange,
  placeholder,
  integer,
  min,
  max,
  minExclusive,
  onValidate,
}: {
  value: number | null;
  onChange: (v: number | null) => void;
  placeholder?: string;
  integer?: boolean;
  min?: number;
  max?: number;
  /** true 时 min 为开区间（要求 v > min） */
  minExclusive?: boolean;
  onValidate?: (error: string | null) => void;
}) {
  const [text, setText] = useState(value === null ? "" : String(value));
  const [error, setError] = useState<string | null>(null);

  const rangeCheck = (n: number): string | null => {
    if (!Number.isFinite(n)) return "必须是有效数字";
    if (min !== undefined && (minExclusive ? n <= min : n < min)) {
      return minExclusive ? `必须大于 ${min}` : `不能小于 ${min}`;
    }
    if (max !== undefined && n > max) return `不能大于 ${max}`;
    return null;
  };

  // 外部 value 变化（如配置加载完成）时同步文本；与当前输入解析值一致则不打扰
  useEffect(() => {
    setText((t) => {
      const parsed = t.trim() === "" ? null : Number(t);
      if (parsed === value) return t;
      return value === null ? "" : String(value);
    });
  }, [value]);

  // 挂载时校验一次初始值（配置文件可能被手改成非法值）
  useEffect(() => {
    if (value !== null) {
      const err = rangeCheck(value);
      setError(err);
      onValidate?.(err);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div>
      <input
        className={cx(inputCls, error && "border-red-500/60 focus:border-red-500/80")}
        type="text"
        inputMode="decimal"
        value={text}
        placeholder={placeholder}
        onChange={(e) => {
          const t = e.target.value;
          setText(t);
          const trimmed = t.trim();
          let err: string | null = null;
          if (trimmed === "") {
            onChange(null);
          } else {
            const n = Number(trimmed);
            err = rangeCheck(n);
            if (!err) onChange(integer ? Math.trunc(n) : n);
          }
          setError(err);
          onValidate?.(err);
        }}
      />
      {error && <span className="mt-1 block text-[11px] text-red-400">{error}</span>}
    </div>
  );
}

export function SelectInput({
  value,
  onChange,
  options,
}: {
  value: string;
  onChange: (v: string) => void;
  options: Array<{ value: string; label: string }>;
}) {
  return (
    <select className={inputCls} value={value} onChange={(e) => onChange(e.target.value)}>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

export function PasswordInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  const [show, setShow] = useState(false);
  return (
    <div className="relative">
      <input
        className={cx(inputCls, "pr-9 font-mono")}
        type={show ? "text" : "password"}
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
      />
      <button
        type="button"
        onClick={() => setShow((s) => !s)}
        className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 text-zinc-500 transition-colors hover:text-zinc-300"
        title={show ? "隐藏" : "显示"}
      >
        {show ? <EyeOff size={14} /> : <Eye size={14} />}
      </button>
    </div>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  hint?: string;
}) {
  return (
    <button type="button" onClick={() => onChange(!checked)} className="flex items-center gap-2.5 rounded-lg py-1 text-left">
      <span
        className={cx(
          "flex h-4.5 w-8 items-center rounded-full px-0.5 transition-colors",
          checked ? "justify-end bg-emerald-500" : "justify-start bg-zinc-700",
        )}
      >
        <span className="h-3.5 w-3.5 rounded-full bg-white shadow" />
      </span>
      <span>
        <span className="block text-[13px] text-zinc-300">{label}</span>
        {hint && <span className="block text-[11px] text-zinc-600">{hint}</span>}
      </span>
    </button>
  );
}
