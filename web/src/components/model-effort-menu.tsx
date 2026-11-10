// 输入区组合控件：模型 + 推理等级切换 —— 发送按钮左侧的胶囊按钮（当前模型名 + 档位 + 开合箭头），
// 点击弹出锚定菜单：根视图两行（模型 / 推理等级，行右展示当前值 + 箭头），
// 点击行在弹窗内下钻到对应列表（← 返回），选择后关闭。
// 开合状态走全局 store：Ctrl+M 可远程开合，anyDialogOpen 的 Esc 让位语义保持一致。
import { useEffect, useState } from "react";
import {
  ArrowLeft,
  Check,
  ChevronDown,
  ChevronRight,
  ChevronUp,
} from "lucide-react";
import { useApp } from "../store/app-store";
import type { ReasoningEffort } from "../store/types";
import { cn } from "../lib/utils";

/** 弹窗视图：根（两行入口）/ 模型列表 / 推理等级列表 */
type MenuView = "root" | "model" | "effort";

const EFFORT_LEVELS: ReasoningEffort[] = ["low", "high", "max"];

const EFFORT_HINTS: Record<ReasoningEffort, string> = {
  low: "轻量推理，更快响应",
  high: "均衡模式（默认）",
  max: "深度推理，更强但更慢",
};

/** 档位展示名（Low / High / Max） */
const effortLabel = (e: ReasoningEffort) =>
  e[0]!.toUpperCase() + e.slice(1);

/** 根视图行：左标签 + 右当前值 + 右箭头 */
function RootRow({
  label,
  value,
  onClick,
}: {
  label: string;
  value: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex w-full cursor-pointer items-center justify-between gap-3 px-3 py-2.5 text-left text-sm text-foreground transition-colors hover:bg-muted/60"
    >
      <span className="shrink-0">{label}</span>
      <span className="flex min-w-0 items-center gap-1 text-muted-foreground">
        <span className="truncate">{value}</span>
        <ChevronRight className="size-4 shrink-0 text-muted-foreground/50" />
      </span>
    </button>
  );
}

/** 下钻视图头：← 返回 + 标题 */
function ViewHeader({
  title,
  onBack,
}: {
  title: string;
  onBack: () => void;
}) {
  return (
    <div className="flex items-center gap-1.5 border-b border-border/60 px-2 py-1.5">
      <button
        type="button"
        onClick={onBack}
        aria-label="返回"
        className="flex size-6 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
      >
        <ArrowLeft className="size-3.5" />
      </button>
      <span className="text-xs font-semibold text-foreground">{title}</span>
    </div>
  );
}

export function ModelEffortMenu() {
  const open = useApp((s) => s.showModelMenu);
  const setModelMenu = useApp((s) => s.setModelMenu);
  const model = useApp((s) => s.models.find((m) => m.active)?.display ?? "");
  const models = useApp((s) => s.models);
  const effort = useApp((s) => s.effort);
  const switchModel = useApp((s) => s.switchModel);
  const setEffort = useApp((s) => s.setEffort);
  /** 下钻视图；关闭时重置回根视图 */
  const [view, setView] = useState<MenuView>("root");

  useEffect(() => {
    if (!open) setView("root");
  }, [open]);

  // Esc 关闭（语义对齐 Overlay 弹窗）
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setModelMenu(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, setModelMenu]);

  const close = () => setModelMenu(false);

  return (
    <div className="relative">
      {/* 点击胶囊外关闭（透明遮罩，位于弹窗之下） */}
      {open && <div className="fixed inset-0 z-20 cursor-default" onMouseDown={close} />}

      {/* 锚定弹窗（右对齐胶囊）：根视图 / 模型列表 / 推理等级列表 */}
      {open && (
        <div className="absolute bottom-full right-0 z-30 mb-2 w-72 overflow-hidden rounded-xl border border-border bg-background shadow-lg">
          {view === "root" && (
            <>
              <RootRow
                label="模型"
                value={model || "未配置模型"}
                onClick={() => setView("model")}
              />
              <div className="border-t border-border/60" />
              <RootRow
                label="推理等级"
                value={effortLabel(effort)}
                onClick={() => setView("effort")}
              />
            </>
          )}

          {view === "model" && (
            <>
              <ViewHeader title="模型" onBack={() => setView("root")} />
              <div className="max-h-72 overflow-y-auto overscroll-contain py-1">
                {models.length === 0 && (
                  <p className="px-3 py-2 text-xs text-muted-foreground/60">
                    未配置模型
                  </p>
                )}
                {models.map((m) => (
                  <button
                    key={`${m.provider_id}:${m.model_id}`}
                    type="button"
                    onClick={() => {
                      // 失败静默：接口报错时菜单不关，用户可直接重试
                      switchModel(m.display).catch(() => {});
                    }}
                    className={cn(
                      "flex w-full cursor-pointer items-center gap-2 px-3 py-1.5 text-left transition-colors",
                      m.active
                        ? "text-foreground"
                        : "text-muted-foreground/80 hover:bg-muted/60 hover:text-foreground",
                    )}
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-sm">{m.display}</span>
                      <span className="block truncate text-xs text-muted-foreground/60">
                        {m.provider_name}
                      </span>
                    </span>
                    {m.active && <Check className="size-4 shrink-0" />}
                  </button>
                ))}
              </div>
            </>
          )}

          {view === "effort" && (
            <div className="py-1">
              <ViewHeader title="推理等级" onBack={() => setView("root")} />
              {EFFORT_LEVELS.map((level) => (
                <button
                  key={level}
                  type="button"
                  onClick={() => {
                    void setEffort(level);
                    close();
                  }}
                  className={cn(
                    "flex w-full cursor-pointer items-center gap-2 px-3 py-1.5 text-left transition-colors",
                    level === effort
                      ? "text-foreground"
                      : "text-muted-foreground/80 hover:bg-muted/60 hover:text-foreground",
                  )}
                >
                  <span className="min-w-0 flex-1">
                    <span className="block text-sm">{effortLabel(level)}</span>
                    <span className="block text-xs text-muted-foreground/60">
                      {EFFORT_HINTS[level]}
                    </span>
                  </span>
                  {level === effort && <Check className="size-4 shrink-0" />}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {/* 胶囊锚点：模型名 + 推理等级 + 开合箭头 */}
      <button
        type="button"
        onClick={() => setModelMenu(!open)}
        aria-label="模型与推理等级"
        aria-expanded={open}
        title={open ? "收起" : "模型与推理等级（Ctrl+M）"}
        className="flex h-8 min-w-0 shrink cursor-pointer items-center gap-1.5 rounded-lg px-2 text-xs transition-colors hover:bg-muted/60"
      >
        <span className="max-w-40 truncate text-foreground/80">
          {model || "未配置模型"}
        </span>
        <span className="shrink-0 text-muted-foreground/60">
          {effortLabel(effort)}
        </span>
        {open ? (
          <ChevronUp className="size-3.5 shrink-0 text-muted-foreground/60" />
        ) : (
          <ChevronDown className="size-3.5 shrink-0 text-muted-foreground/60" />
        )}
      </button>
    </div>
  );
}
