// Chat message stream: assistant-ui base styling (GroupedParts, two-level grouping,
// Reasoning/ToolFallback/ToolGroup) + hand-rolled scroll-to-bottom button
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import {
  ActionBarPrimitive,
  MessagePrimitive,
  ThreadPrimitive,
  type PartState,
} from "@assistant-ui/react";
import { ArrowDown, Check, Copy, Gauge, Loader2, Terminal } from "lucide-react";
import { cn, fmtTokens } from "../../lib/utils";
import { splitByMentions, mentionDisplayPath } from "../../lib/mentions";
import { MentionIcon } from "../mention-icon";
import type { ChatMention } from "../../runtime/types";
import { useApp } from "../../store/app-store";
import { MarkdownText } from "./markdown-text";
import { ReasoningGroup } from "./reasoning";
import { ToolFallback, ToolGroup } from "./tool-fallback";
import { TaskToolCard } from "./task-card";
import {
  lastAssistantMessageId,
  type SystemRow,
} from "../../runtime/hailux-runtime";
import { ThreadNav, USER_MSG_ATTR } from "./thread-nav";

/** todo_write/ask_user/task stay ungrouped — they render dedicated cards.
 *  Module-level so the reference stays stable and the GroupedParts tree memo holds */
const groupBy = (part: PartState): readonly ("group-reasoning" | "group-tools")[] => {
  if (part.type === "reasoning") return ["group-reasoning"];
  if (
    part.type === "tool-call" &&
    part.toolName !== "todo_write" &&
    part.toolName !== "ask_user" &&
    part.toolName !== "task"
  )
    return ["group-tools"];
  return [];
};

/* ── User message: official base (bg-muted bubble, image thumbnails) ──── */
function UserMessage({ mentions }: { mentions?: ChatMention[] }) {
  const workDir = useApp((s) => s.workDir);
  return (
    <MessagePrimitive.Root className="flex justify-end">
      <div className="bg-muted text-foreground max-w-[80%] rounded-xl px-4 py-2">
        <MessagePrimitive.Parts
          components={{
            Text: ({ text }) => (
              <p className="whitespace-pre-wrap leading-relaxed">
                {splitByMentions(text, mentions).map((seg, i) => {
                  if (!seg.kind) {
                    return <span key={i}>{seg.text}</span>;
                  }
                  const rel = mentionDisplayPath((seg.value ?? "").slice(1), workDir);
                  return (
                    <span
                      key={i}
                      title={seg.value}
                      className="mx-0.5 rounded-md px-1 py-0.5 text-blue-600 hover:bg-blue-500/10 dark:text-blue-400 dark:hover:bg-blue-400/10"
                    >
                      <MentionIcon
                        rel={rel}
                        mono
                        className="mr-1 inline-block size-3.5 align-[-1px]"
                      />
                      {rel}
                    </span>
                  );
                })}
              </p>
            ),
            Image: ({ image }) => (
              <img
                src={image}
                alt="attachment"
                loading="lazy"
                decoding="async"
                onError={(ev) => {
                  ev.currentTarget.style.opacity = "0.3";
                }}
                className="mt-1 max-h-48 rounded-lg border border-border object-contain"
              />
            ),
          }}
        />
      </div>
    </MessagePrimitive.Root>
  );
}

/* ── System rows: notices / errors / compaction ──────────────── */
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
        {row.spinning && <Loader2 className="size-3 animate-spin" />}
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

/* ── Assistant message: official base (no bubble + GroupedParts) ── */

/** Per-turn end metadata (attached to the last assistant message of each turn) */
interface TurnMeta {
  model?: string;
  totalMs?: number;
  status?: string;
  ctxPromptTokens?: number;
  ctxCompletionTokens?: number;
}

/** Context usage: last request's input+output (≈ current context size) over the context window */
function ContextUsage({
  meta,
  show,
}: {
  meta: TurnMeta;
  show: boolean;
}) {
  const contextWindow = useApp((s) => s.contextWindow);
  const isRunning = useApp((s) => s.isRunning);

  if (!show || isRunning) return null;
  if (meta.ctxPromptTokens === undefined) return null;
  const used = meta.ctxPromptTokens + (meta.ctxCompletionTokens ?? 0);
  if (used === 0) return null;
  const pct = contextWindow > 0 ? Math.min(used / contextWindow, 1) : null;

  return (
    <span
      className={cn(
        "flex items-center gap-1 rounded-md px-1.5 py-0.5 tabular-nums",
        pct !== null && pct >= 0.8 && "text-warning",
      )}
      title={`上下文占用 ${fmtTokens(used)}${contextWindow > 0 ? ` / ${fmtTokens(contextWindow)} · ${Math.round((pct ?? 0) * 100)}%` : ""}`}
    >
      <Gauge className="size-3.5" />
      {contextWindow > 0
        ? `${fmtTokens(used)} / ${fmtTokens(contextWindow)}${pct !== null ? ` · ${Math.round(pct * 100)}%` : ""}`
        : fmtTokens(used)}
    </span>
  );
}

/** Per-turn action bar: copy + model/duration (+ context usage on the last turn) */
function AssistantActions({
  meta,
  showContext,
}: {
  meta: TurnMeta;
  showContext: boolean;
}) {
  const segments: string[] = [];
  if (meta.model) segments.push(meta.model);
  if (meta.totalMs !== undefined)
    segments.push(`${(meta.totalMs / 1000).toFixed(1)}s`);
  if (meta.status === "interrupted") segments.push("已中断");
  if (meta.status === "error") segments.push("出错");

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
            meta.status === "error" && "text-destructive",
            meta.status === "interrupted" && "text-warning",
          )}
          title="模型 · 本轮耗时"
        >
          {segments.join(" · ")}
        </span>
      )}
      <div className="ml-auto">
        <ContextUsage meta={meta} show={showContext} />
      </div>
    </div>
  );
}

/** Running timer: mirrors the TUI status banner (braille spinner + "Working"
 *  shimmer + live duration), pinned below the message list */
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

/* ── Scroll-to-bottom button: hand-rolled instead of ThreadPrimitive.ScrollToBottom
 * because the library's isAtBottom state misses several cases: it never updates
 * while scrolling down without reaching the bottom (its debounce against
 * auto-scroll flicker), so a stale true from a session switch keeps the button
 * hidden. Positioned against ThreadPrimitive.Root (outside the scrolling
 * content) — inside the viewport it would scroll away together with the messages. */
function ScrollToBottomButton({
  viewportRef,
  contentRef,
}: {
  viewportRef: RefObject<HTMLDivElement | null>;
  contentRef: RefObject<HTMLDivElement | null>;
}) {
  const [visible, setVisible] = useState(false);

  const measure = useCallback(() => {
    const vp = viewportRef.current;
    if (!vp) return;
    const distance = vp.scrollHeight - vp.scrollTop - vp.clientHeight;
    setVisible(vp.scrollHeight > vp.clientHeight && distance > 8);
  }, [viewportRef]);

  // Content height can grow without a scroll event (lazy images, collapsed
  // cards), so pair the scroll listener with a ResizeObserver on the content
  useEffect(() => {
    const vp = viewportRef.current;
    if (!vp) return;
    let raf = 0;
    const schedule = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        measure();
      });
    };
    vp.addEventListener("scroll", schedule, { passive: true });
    const content = contentRef.current;
    const ro = content ? new ResizeObserver(schedule) : null;
    if (content) ro?.observe(content);
    return () => {
      vp.removeEventListener("scroll", schedule);
      ro?.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [viewportRef, contentRef, measure]);

  return (
    <button
      type="button"
      aria-label="滚动到底部"
      title="滚动到底部"
      onClick={() => {
        const vp = viewportRef.current;
        if (!vp) return;
        vp.scrollTo({ top: vp.scrollHeight, behavior: "smooth" });
      }}
      className={cn(
        "absolute bottom-4 left-1/2 z-10 flex size-8 -translate-x-1/2 cursor-pointer items-center justify-center rounded-full border border-border bg-background text-muted-foreground shadow-lg transition-all hover:text-foreground",
        visible ? "opacity-100" : "pointer-events-none opacity-0",
      )}
    >
      <ArrowDown className="size-4" />
    </button>
  );
}

function AssistantMessage({
  meta,
  isLast = false,
}: {
  meta?: TurnMeta;
  isLast?: boolean;
}) {
  return (
    <MessagePrimitive.Root className="relative">
      <div className="text-foreground px-2 leading-relaxed break-words">
        <MessagePrimitive.GroupedParts groupBy={groupBy}>
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
              case "group-tools": {
                // A single call has nothing to merge — degrade to a standalone card
                if (part.indices.length <= 1) return <>{children}</>;
                return (
                  <ToolGroup
                    count={part.indices.length}
                    running={part.status.type === "running"}
                  >
                    {children}
                  </ToolGroup>
                );
              }
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
                return part.toolName === "task" ? (
                  <TaskToolCard {...part} />
                ) : (part.toolUI ?? <ToolFallback {...part} />);
              case "indicator":
                // Running indicator is unified into RunningTimer below the message list
                return null;
              default:
                return null;
            }
          }}
        </MessagePrimitive.GroupedParts>
      </div>
      {/* Action bar renders once the turn's done metadata arrives; hidden while streaming */}
      {meta && (meta.model || meta.totalMs !== undefined) && (
        <AssistantActions meta={meta} showContext={isLast} />
      )}
    </MessagePrimitive.Root>
  );
}

/* ── Welcome screen: decorative background + logo ────────────── */
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
              把想法交给我，把时间留给自己
            </p>
          </div>
        </div>

      </div>
    </div>
  );
}

/* ── Thread body ─────────────────────────────────────────────── */
export function Thread() {
  // Context usage hangs on this message alone
  const items = useApp((s) => s.items);
  const lastId = useMemo(() => lastAssistantMessageId(items), [items]);

  // Turn-nav (right rail) needs both containers to measure turn positions / observe height
  const viewportRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);

  // Session switch jumps to the latest message (scrollBottomSignal set in app-store.switchSession;
  // assistant-ui only auto-scrolls on its own initialize/run events, which don't fire here)
  const scrollBottomSignal = useApp((s) => s.scrollBottomSignal);
  useEffect(() => {
    if (!scrollBottomSignal) return;
    const vp = viewportRef.current;
    if (!vp) return;
    vp.scrollTo({ top: vp.scrollHeight });
  }, [scrollBottomSignal]);

  return (
    <ThreadPrimitive.Root className="relative flex min-h-0 flex-1 flex-col overflow-hidden">
      <ThreadPrimitive.Viewport
        ref={viewportRef}
        className="flex flex-1 flex-col overflow-y-auto scroll-smooth px-4 py-6"
      >
        <ThreadPrimitive.Empty>
          <Welcome />
        </ThreadPrimitive.Empty>

        <div ref={contentRef} className="mx-auto w-full max-w-3xl">
          <div className="space-y-5">
            <ThreadPrimitive.Messages>
              {({ message }) => {
                // System rows and per-turn metadata (model/duration/status/context) both ride on metadata.custom
                const custom = message.metadata?.custom as
                  | ({ row?: SystemRow; mentions?: ChatMention[] } & TurnMeta)
                  | undefined;
                if (custom?.row) {
                  return <SystemRowView key={message.id} row={custom.row} />;
                }
                if (message.role === "user") {
                  return (
                    <div key={message.id} {...{ [USER_MSG_ATTR]: "" }}>
                      <UserMessage mentions={custom?.mentions} />
                    </div>
                  );
                }
                const { row: _metaRow, ...turnMeta } = custom ?? {};
                return (
                  <AssistantMessage
                    key={message.id}
                    meta={turnMeta}
                    isLast={message.id === lastId}
                  />
                );
              }}
            </ThreadPrimitive.Messages>

            <RunningTimer />
          </div>
        </div>
      </ThreadPrimitive.Viewport>

      <ScrollToBottomButton viewportRef={viewportRef} contentRef={contentRef} />

      <ThreadNav viewportRef={viewportRef} contentRef={contentRef} />
    </ThreadPrimitive.Root>
  );
}
