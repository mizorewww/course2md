import { BadgeCheck, Cloud, Cpu, Gpu, Layers, Zap } from "lucide-react";
import type { ProviderInfo } from "../types";
import { cx } from "../lib/utils";

const PROVIDER_ICON: Record<string, typeof Cpu> = {
  coreml: Zap,
  gpu: Gpu,
  npu: Layers,
  cpu: Cpu,
  api: Cloud,
};

interface ProviderPickerProps {
  providers: ProviderInfo[];
  value: string;
  onChange: (id: string) => void;
  /** M6：为 true 时顶部多一个「自动」卡片（value=""），选中=不传 --provider，让配置文件生效 */
  allowAuto?: boolean;
}

/** 卡片式单选：推荐项置顶并带 emerald 徽章，不可用的置灰并显示原因 */
export function ProviderPicker({ providers, value, onChange, allowAuto }: ProviderPickerProps) {
  const sorted = [...providers].sort((a, b) => Number(b.recommended) - Number(a.recommended));
  const renderCard = (p: ProviderInfo) => {
        const Icon = PROVIDER_ICON[p.id] ?? Cpu;
        const selected = value === p.id;
        const disabled = !p.available;
        return (
          <button
            key={p.id || "auto"}
            disabled={disabled}
            onClick={() => onChange(p.id)}
            className={cx(
              "relative rounded-xl border p-3.5 text-left transition-colors",
              selected
                ? "border-emerald-500/60 bg-emerald-500/10"
                : "border-zinc-800 bg-zinc-900/60 hover:border-zinc-700 hover:bg-zinc-900",
              disabled && "cursor-not-allowed opacity-45 hover:border-zinc-800 hover:bg-zinc-900/60",
            )}
          >
            <div className="flex items-center gap-2.5">
              <Icon size={16} className={selected ? "text-emerald-400" : "text-zinc-500"} />
              <span className={cx("text-[13px] font-medium", selected ? "text-zinc-100" : "text-zinc-300")}>
                {p.label}
              </span>
              {p.recommended && (
                <span className="ml-auto flex items-center gap-1 rounded-full bg-emerald-500/15 px-2 py-0.5 text-[10px] font-medium text-emerald-300">
                  <BadgeCheck size={11} />
                  推荐
                </span>
              )}
              {selected && !p.recommended && (
                <span className="ml-auto h-2 w-2 rounded-full bg-emerald-400" />
              )}
            </div>
            <p className="mt-1.5 text-[11px] leading-relaxed text-zinc-500">{p.note}</p>
          </button>
        );
      };
  return (
    <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
      {allowAuto &&
        renderCard({
          id: "",
          label: "自动",
          available: true,
          recommended: false,
          note: "跟随配置文件 defaults.provider；未配置则由 CLI 按平台自动选择",
        })}
      {sorted.map(renderCard)}
    </div>
  );
}
