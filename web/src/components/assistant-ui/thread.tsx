// 聊天消息流：assistant-ui 官方 base 样式（GroupedParts + 两层分组 + Reasoning/ToolFallback/ToolGroup）
import { useEffect, useMemo, useState } from "react";
import {
  ActionBarPrimitive,
  groupPartByType,
  MessagePrimitive,
  ThreadPrimitive,
} from "@assistant-ui/react";
import { ArrowDown, Check, Copy, Gauge, Terminal } from "lucide-react";
import { cn } from "../../lib/utils";
import { useApp } from "../../store/app-store";
import { MarkdownText } from "./markdown-text";
import { ReasoningGroup } from "./reasoning";
import { ToolFallback } from "./tool-fallback";
import { toThreadMessages, type SystemRow } from "../../runtime/hailux-runtime";

/* ── 用户消息：官方 base（bg-muted 气泡）────────────────────── */
function UserMessage() {
  return (
    <MessagePrimitive.Root className="flex justify-end">
      <div className="bg-muted text-foreground max-w-[80%] rounded-xl px-4 py-2">
        <MessagePrimitive.Parts
          components={{
            Text: ({ text }) => (
              <p className="whitespace-pre-wrap leading-relaxed">{text}</p>
            ),
          }}
        />
      </div>
    </MessagePrimitive.Root>
  );
}

/* ── 系统提示行：通知/错误/压缩 ──────────────────────────────── */
const TONE_CLASS: Record<string, string> = {
  info: "text-muted-foreground/60",
  success: "text-foreground/70",
  warn: "text-warning",
  danger: "text-destructive",
};

function SystemRowView({ row }: { row: SystemRow }) {
  if (row.kind === "compact") {
    return (
      <div className="my-4 flex items-center gap-3 text-xs text-muted-foreground/50">
        <span className="h-px flex-1 bg-border" />
        <span className="whitespace-nowrap">{row.text}</span>
        <span className="h-px flex-1 bg-border" />
      </div>
    );
  }
  return (
    <div className={cn("my-2 text-center text-xs", TONE_CLASS[row.tone ?? "info"])}>
      {row.detail ? `${row.text} · ${row.detail}` : row.text}
    </div>
  );
}

/* ── 助手消息：官方 base（无气泡 + GroupedParts 两层分组）────── */

/** 本轮结束元信息（挂在最后一条助手消息上） */
interface TurnMeta {
  model?: string;
  totalMs?: number;
  status?: string;
}

/** token 数量格式化：1.2M / 12.3k / 128 */
function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/** 上下文使用情况：当前上下文 token / 模型上下文窗口 */
function ContextUsage() {
  const promptTokens = useApp((s) => s.promptTokens);
  const completionTokens = useApp((s) => s.completionTokens);
  const contextWindow = useApp((s) => s.contextWindow);

  const used = promptTokens + completionTokens;
  if (used === 0) return null;
  const pct =
    contextWindow > 0 ? Math.min(used / contextWindow, 1) : null;

  return (
    <span
      className={cn(
        "flex items-center gap-1 rounded-md px-1.5 py-0.5 tabular-nums",
        pct !== null && pct >= 0.8 && "text-warning",
      )}
      title="上下文使用情况（最近一轮输入 + 输出 / 上下文窗口）"
    >
      <Gauge className="size-3.5" />
      {contextWindow > 0
        ? `${fmtTokens(used)} / ${fmtTokens(contextWindow)}${pct !== null ? ` · ${Math.round(pct * 100)}%` : ""}`
        : fmtTokens(used)}
    </span>
  );
}

/** 最后一条助手消息底部操作栏：复制 + 模型/耗时 + 上下文用量 */
function AssistantActions({ meta }: { meta?: TurnMeta }) {
  const segments: string[] = [];
  if (meta?.model) segments.push(meta.model);
  if (meta?.totalMs !== undefined)
    segments.push(`${(meta.totalMs / 1000).toFixed(1)}s`);
  if (meta?.status === "interrupted") segments.push("已中断");
  if (meta?.status === "error") segments.push("出错");

  return (
    <div className="flex items-center gap-0.5 px-2 pt-1 font-sans text-xs text-muted-foreground/60">
      <ActionBarPrimitive.Copy
        copiedDuration={2000}
        className="group/copy flex cursor-pointer items-center gap-1 rounded-md px-1.5 py-0.5 transition-colors hover:bg-muted hover:text-foreground disabled:hidden"
        title="复制消息"
      >
        <Copy className="size-3.5 group-data-[copied=true]/copy:hidden" />
        <Check className="hidden size-3.5 group-data-[copied=true]/copy:block" />
        <span className="group-data-[copied=true]/copy:hidden">复制</span>
        <span className="hidden group-data-[copied=true]/copy:inline">
          已复制
        </span>
      </ActionBarPrimitive.Copy>
      {segments.length > 0 && (
        <span
          className={cn(
            "flex items-center gap-1 rounded-md px-1.5 py-0.5 tabular-nums",
            meta?.status === "error" && "text-destructive",
            meta?.status === "interrupted" && "text-warning",
          )}
          title="模型 · 本轮耗时"
        >
          {segments.join(" · ")}
        </span>
      )}
      <div className="ml-auto">
        <ContextUsage />
      </div>
    </div>
  );
}

/** 运行中的计时指示器：对齐 TUI 状态横幅
 *  （braille spinner + "Working" shimmer 扫光 + 实时耗时），挂在消息区底部 */
function RunningTimer() {
  const runStartedAt = useApp((s) => s.runStartedAt);
  const isCompacting = useApp((s) =>
    s.items.some((i) => i.kind === "notice" && i.text?.includes("压缩")),
  );
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (runStartedAt === null) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 100);
    return () => clearInterval(t);
  }, [runStartedAt]);

  if (runStartedAt === null) return null;
  return (
    <div
      className="flex items-center gap-1.5 px-2 font-sans text-sm tabular-nums"
      aria-label="助手正在处理"
    >
      <span className="hlx-spinner text-muted-foreground/70" aria-hidden />
      <span className="hlx-shimmer font-medium">
        {isCompacting ? "Compacting context" : "Working"}
      </span>
      <span className="text-muted-foreground/60">
        {((now - runStartedAt) / 1000).toFixed(1)}s
      </span>
    </div>
  );
}

function AssistantMessage({
  isLast = false,
  meta,
}: {
  isLast?: boolean;
  meta?: TurnMeta;
}) {
  return (
    <MessagePrimitive.Root className="relative">
      <div className="text-foreground px-2 leading-relaxed break-words">
        <MessagePrimitive.GroupedParts
          groupBy={groupPartByType({
            reasoning: ["group-reasoning"],
          })}
        >
          {({ part, children }) => {
            switch (part.type) {
              case "group-reasoning":
                return (
                  <ReasoningGroup
                    startIndex={part.indices[0] ?? 0}
                    endIndex={part.indices[part.indices.length - 1] ?? 0}
                  >
                    {children}
                  </ReasoningGroup>
                );
              case "text":
                return (
                  <div className="my-2">
                    <MarkdownText />
                  </div>
                );
              case "reasoning":
                return (
                  <div className="text-muted-foreground whitespace-pre-wrap text-sm leading-relaxed">
                    {part.text}
                  </div>
                );
              case "tool-call":
                return part.toolUI ?? <ToolFallback {...part} />;
              case "indicator":
                // 运行指示统一由消息区底部的 RunningTimer 展示
                return null;
              default:
                return null;
            }
          }}
        </MessagePrimitive.GroupedParts>
      </div>
      {isLast && <AssistantActions meta={meta} />}
    </MessagePrimitive.Root>
  );
}

/* ── 欢迎屏：装饰背景 + Logo ────────────────────────────────── */
function Welcome() {
  return (
    <div className="relative flex h-full flex-col items-center justify-center px-6 py-12">
      <div className="pointer-events-none absolute inset-0 overflow-hidden">
        <div className="absolute left-1/2 top-1/3 size-[500px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-primary/[0.03] blur-3xl" />
        <div className="absolute right-1/4 top-2/3 size-[300px] rounded-full bg-blue-500/[0.02] blur-3xl" />
      </div>

      <div className="relative flex flex-col items-center gap-8">
        <div className="flex flex-col items-center gap-3">
          <div className="flex size-14 items-center justify-center rounded-2xl bg-primary/[0.08] shadow-sm ring-1 ring-primary/10">
            <Terminal className="size-7 text-primary/70" />
          </div>
          <div className="text-center">
            <h1 className="text-2xl font-bold tracking-tight text-foreground">
              hailux
            </h1>
            <p className="mt-1 text-sm text-muted-foreground">
              终端级能力，浏览器直达
            </p>
          </div>
        </div>

      </div>
    </div>
  );
}

/* ── Thread 主体 ────────────────────────────────────────────── */
export function Thread() {
  // 最后一条助手消息（非系统行）的 id 及其元信息 —— 底部操作栏只挂在该消息上
  const items = useApp((s) => s.items);
  const last = useMemo(() => {
    const msgs = toThreadMessages(items);
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i]!;
      const custom = m.metadata?.custom as
        | ({ row?: SystemRow } & TurnMeta)
        | undefined;
      if (m.role === "assistant" && !custom?.row) {
        const { row: _row, ...meta } = custom ?? {};
        return { id: m.id, meta };
      }
    }
    return null;
  }, [items]);

  return (
    <ThreadPrimitive.Root className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <ThreadPrimitive.Viewport className="flex flex-1 flex-col overflow-y-auto scroll-smooth px-4 py-6">
        <ThreadPrimitive.Empty>
          <Welcome />
        </ThreadPrimitive.Empty>

        <div className="mx-auto w-full max-w-3xl">
          <div className="space-y-5">
            <ThreadPrimitive.Messages>
              {({ message }) => {
                const custom = message.metadata?.custom as
                  | { row?: SystemRow; running?: boolean }
                  | undefined;
                if (custom?.row) {
                  return <SystemRowView key={message.id} row={custom.row} />;
                }
                if (message.role === "user") {
                  return <UserMessage key={message.id} />;
                }
                return (
                  <AssistantMessage
                    key={message.id}
                    isLast={message.id === last?.id}
                    meta={last?.meta}
                  />
                );
              }}
            </ThreadPrimitive.Messages>

            <RunningTimer />
          </div>
        </div>

        <div className="relative h-0 w-full max-w-3xl">
          <ThreadPrimitive.ScrollToBottom className="absolute -top-12 right-2 mx-auto flex size-8 cursor-pointer items-center justify-center rounded-full border border-border bg-background text-muted-foreground shadow-lg transition-all hover:text-foreground disabled:invisible disabled:opacity-0">
            <ArrowDown className="size-4" />
          </ThreadPrimitive.ScrollToBottom>
        </div>
      </ThreadPrimitive.Viewport>
    </ThreadPrimitive.Root>
  );
}
