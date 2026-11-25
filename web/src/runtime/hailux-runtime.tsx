// assistant-ui runtime adapter: Zustand ChatItem[] → ThreadMessageLike[]
import { useMemo, type ReactNode } from "react";
import {
  AssistantRuntimeProvider,
  useExternalStoreRuntime,
  type AppendMessage,
  type ThreadMessageLike,
} from "@assistant-ui/react";
import { useApp, type ChatItem } from "../store/app-store";
import {
  subagentStepResultSummary,
  subagentStepSummary,
  type TaskStepData,
} from "./subagent-steps";

/** System notice row (notice/error/compact marker), attached at
 *  message.metadata.custom.row */
export interface SystemRow {
  kind: "notice" | "error" | "compact";
  text: string;
  tone?: "info" | "warn" | "danger" | "success";
  detail?: string;
  /** Compaction in progress (spinner status row, replaced by the done marker
   *  once finished) */
  spinning?: boolean;
}

/** Assistant message custom metadata (reasoning segment timing / end-of-turn
 *  info) */
export interface AssistantMeta {
  running?: boolean;
  /** Reasoning segment timing: each reasoning segment is timed independently
   *  (mirrors the TUI where each AgentThinking block has its own think_ms) */
  thinkSegments?: ThinkSegment[];
  model?: string;
  totalMs?: number;
  status?: string;
  /** Context usage of this turn (input/output tokens of the last request) */
  ctxPromptTokens?: number;
  ctxCompletionTokens?: number;
}

/** Timing of a single reasoning segment (keyed by part index within the
 *  message) */
export interface ThinkSegment {
  /** Index of the owning reasoning part within the message parts */
  partIndex: number;
  /** Frozen segment duration (ms) */
  ms?: number;
  /** Start time of an in-progress segment (epoch ms) */
  startedAt?: number;
}

type AssistantPart =
  | { type: "text"; text: string }
  | { type: "reasoning"; text: string }
  | {
      type: "tool-call";
      toolCallId: string;
      toolName: string;
      // No eslint config; `any` is used to match ReadonlyJSONObject
      args: Record<string, any>;
      argsText: string;
      result?: { output: string; display?: string };
    };

interface Accum {
  id: string;
  parts: AssistantPart[];
  running: boolean;
  thinkSegments: ThinkSegment[];
  /** End-of-turn info (AgentComplete), shown in the bottom action bar */
  model?: string;
  totalMs?: number;
  status?: string;
  ctxPromptTokens?: number;
  ctxCompletionTokens?: number;
}

function parseArgs(raw: string | undefined): Record<string, any> {
  if (!raw) return {};
  try {
    const v = JSON.parse(raw);
    return typeof v === "object" && v !== null
      ? (v as Record<string, unknown>)
      : { value: v };
  } catch {
    return { _raw: raw };
  }
}

/** Fold categories (single-source rules shared by toThreadMessages /
 *  lastAssistantMessageId):
 *  part (create/reuse the assistant acc, push a new part) · backfill (fill an
 *  existing acc, no new part)
 *  · boundary (end the current acc, emit one non-assistant message).
 *  switch exhaustively covers ChatItem["kind"] — an unclassified new kind
 *  fails to compile, forcing both sides to stay in sync */
function foldKind(kind: ChatItem["kind"]): "part" | "backfill" | "boundary" {
  switch (kind) {
    case "assistant":
    case "assistant-streaming":
    case "reasoning":
    case "reasoning-streaming":
    case "tool-call":
      return "part";
    case "tool-result":
    case "done":
      return "backfill";
    case "user":
    case "notice":
    case "error":
    case "compact-marker":
    case "compacting":
      return "boundary";
  }
}

/** Fold the flat ChatItem stream into an assistant-ui message sequence */
export function toThreadMessages(items: ChatItem[]): ThreadMessageLike[] {
  const messages: ThreadMessageLike[] = [];
  let acc: Accum | null = null;
  let toolSeq = 0;

  const flush = () => {
    if (!acc || acc.parts.length === 0) return;
    // Mirror fromThreadMessageLike's empty-part filtering rule (empty
    // text/reasoning parts get dropped);
    // filter first, then renumber thinkSegments' partIndex so indices stay
    // consistent with the runtime parts
    const kept: { part: AssistantPart; oldIndex: number }[] = [];
    acc.parts.forEach((part, oldIndex) => {
      if (
        (part.type === "text" || part.type === "reasoning") &&
        !(part.text ?? "").trim()
      ) {
        return;
      }
      kept.push({ part, oldIndex });
    });
    const indexMap = new Map(kept.map((k, i) => [k.oldIndex, i]));
    const thinkSegments = acc.thinkSegments
      .map((seg) => {
        const partIndex = indexMap.get(seg.partIndex);
        return partIndex === undefined ? null : { ...seg, partIndex };
      })
      .filter((s): s is ThinkSegment => s !== null);
    const {
      model,
      totalMs,
      status,
      ctxPromptTokens,
      ctxCompletionTokens,
      id: _id,
      parts: _parts,
      ...rest
    } = acc;
    const custom: Record<string, unknown> = { ...rest };
    if (thinkSegments.length > 0) custom.thinkSegments = thinkSegments;
    if (model !== undefined) custom.model = model;
    if (totalMs !== undefined) custom.totalMs = totalMs;
    if (status !== undefined) custom.status = status;
    if (ctxPromptTokens !== undefined)
      custom.ctxPromptTokens = ctxPromptTokens;
    if (ctxCompletionTokens !== undefined)
      custom.ctxCompletionTokens = ctxCompletionTokens;
    messages.push({
      role: "assistant",
      id: acc.id,
      content: kept.map(({ part }) =>
        part.type === "tool-call"
          ? {
              type: "tool-call" as const,
              toolCallId: part.toolCallId,
              toolName: part.toolName,
              args: part.args,
              argsText: part.argsText,
              result: part.result,
            }
          : { type: part.type, text: part.text },
      ),
      metadata: { custom },
    });
    acc = null;
  };

  const ensureAcc = (streaming: boolean): Accum => {
    if (!acc) {
      acc = {
        id: `a-${messages.length}`,
        parts: [],
        running: streaming,
        thinkSegments: [],
      };
    } else if (!streaming) {
      acc.running = false;
    }
    return acc;
  };

  for (const item of items) {
    // subagent steps (tool-call/tool-result carrying a subagent marker):
    // merged into the nearest unfinished task part as live progress
    // (args._steps), not rendered as a separate card
    if (
      (item.kind === "tool-call" || item.kind === "tool-result") &&
      item.subagent
    ) {
      const agent = item.subagent;
      const a = acc as Accum | null;
      let taskPart: Extract<AssistantPart, { type: "tool-call" }> | undefined;
      if (a) {
        for (let j = a.parts.length - 1; j >= 0; j--) {
          const p = a.parts[j]!;
          if (
            p.type === "tool-call" &&
            p.toolName === "task" &&
            p.result === undefined
          ) {
            taskPart = p;
            break;
          }
        }
      }
      if (taskPart) {
        const steps = (taskPart.args._steps ??= []) as TaskStepData[];
        // Instance identity: index into the tasks array (concurrent subagents
        // with the same name cannot be told apart by name);
        // historical/anomalous data without an index uses -1 as a placeholder,
        // degrading to agent-name matching
        const idx = item.subagentIndex ?? -1;
        if (item.kind === "tool-call") {
          steps.push({
            index: idx,
            agent,
            text: subagentStepSummary(item.name ?? "", item.arguments),
            done: false,
          });
        } else {
          for (let k = steps.length - 1; k >= 0; k--) {
            const s = steps[k]!;
            const matched =
              idx >= 0
                ? s.index === idx && !s.done
                : s.agent === agent && !s.done;
            if (matched) {
              s.done = true;
              const rs = subagentStepResultSummary(item.name ?? "", item.result);
              if (rs) s.text += ` — ${rs}`;
              break;
            }
          }
        }
        continue;
      }
      // No owning task part found (out-of-order/history data): fall back to
      // plain tool-card rendering
    }
    switch (foldKind(item.kind)) {
      case "part": {
        // Assistant text / reasoning / tool call: create or reuse the acc and
        // push a part
        if (item.kind === "tool-call") {
          const a = ensureAcc(false);
          const args = parseArgs(item.arguments);
          if (item.subagent) args["_subagent"] = item.subagent;
          a.parts.push({
            type: "tool-call",
            toolCallId: `tc-${toolSeq++}`,
            toolName: item.name ?? "",
            args,
            argsText: item.arguments ?? "",
          });
        } else if (
          item.kind === "reasoning" ||
          item.kind === "reasoning-streaming"
        ) {
          const a = ensureAcc(item.kind === "reasoning-streaming");
          // Each reasoning item becomes its own part (the store already patches
          // same-segment text into one item; a new segment after the streaming
          // one freezes is a brand-new item, never merged across items)
          a.parts.push({ type: "reasoning", text: item.text ?? "" });
          const partIndex = a.parts.length - 1;
          if (item.kind === "reasoning-streaming") {
            a.thinkSegments.push({
              partIndex,
              startedAt: item.thinkStartedAt ?? Date.now(),
            });
          } else if (item.thinkMs !== undefined && item.thinkMs > 0) {
            // Frozen (duration already computed by the store) or history
            // restore (DB think_ms), each records its own
            a.thinkSegments.push({ partIndex, ms: item.thinkMs });
          }
        } else {
          const a = ensureAcc(item.kind === "assistant-streaming");
          const last = a.parts[a.parts.length - 1];
          if (item.kind === "assistant-streaming" && last?.type === "text") {
            last.text += item.text ?? "";
          } else {
            a.parts.push({ type: "text", text: item.text ?? "" });
          }
        }
        break;
      }
      case "backfill": {
        // tool-result / done: fill the existing acc, no new part
        // (acc is assigned inside the closure, so TS flow analysis narrows it
        // to null; an explicit assertion is needed here)
        const a = acc as Accum | null;
        if (!a) break;
        if (item.kind === "tool-result") {
          // The protocol carries no tool_call_id: fill the nearest tool-call
          // with the "same name and no result yet"
          for (let j = a.parts.length - 1; j >= 0; j--) {
            const p = a.parts[j]!;
            if (
              p.type === "tool-call" &&
              p.toolName === item.name &&
              p.result === undefined
            ) {
              p.result = { output: item.result ?? "", display: item.display };
              break;
            }
          }
        } else {
          // done: merge completion info (model/duration/status/context usage)
          // into the current assistant message, shown in the bottom action bar
          a.running = false;
          a.model = item.model;
          a.totalMs = item.totalMs;
          a.status = item.status;
          a.ctxPromptTokens = item.ctxPromptTokens;
          a.ctxCompletionTokens = item.ctxCompletionTokens;
        }
        break;
      }
      case "boundary": {
        // user / notice / error / compact-marker / compacting: end the current
        // acc, each emits one non-assistant message
        flush();
        if (item.kind === "user") {
          const content: Array<
            { type: "text"; text: string } | { type: "image"; image: string }
          > = [{ type: "text", text: item.text ?? "" }];
          for (const img of item.images ?? []) {
            content.push({ type: "image", image: img.data_url });
          }
          messages.push({
            role: "user",
            id: `u-${messages.length}`,
            content,
            metadata: item.mentions?.length
              ? { custom: { mentions: item.mentions } }
              : undefined,
          });
        } else {
          const row: SystemRow =
            item.kind === "error"
              ? { kind: "error", text: item.text ?? "", tone: "danger" }
              : item.kind === "notice"
                ? { kind: "notice", text: item.text ?? "", tone: item.retry ? "warn" : "info" }
                : item.kind === "compacting"
                  ? {
                      kind: "compact",
                      text: item.text ?? "",
                      tone: "info",
                      spinning: true,
                    }
                  : { kind: "compact", text: item.text ?? "", tone: "info" };
          messages.push({
            role: "assistant",
            id: `s-${messages.length}`,
            content: [{ type: "text", text: "" }],
            metadata: { custom: { row } },
          });
        }
        break;
      }
    }
  }
  flush();
  return messages;
}

/** Lightweight computation of the last assistant message (non-system-row) id
 *  — numbering matches toThreadMessages exactly:
 *  categories share foldKind (single source); no other message is produced
 *  between acc creation and flush,
 *  so the number at flush time equals messages.length when the acc was
 *  created.
 *  Avoids callers rebuilding the whole message sequence just to fetch the id
 *  (items change on every chunk while streaming) */
export function lastAssistantMessageId(items: ChatItem[]): string | null {
  let count = 0; // mirrors toThreadMessages' messages.length
  let accHasParts = false;
  let lastId: string | null = null;

  // Mirrors flush(): an assistant message is emitted only when the acc has parts
  const flush = () => {
    if (!accHasParts) return;
    lastId = `a-${count}`;
    count++;
    accHasParts = false;
  };

  for (const item of items) {
    const cls = foldKind(item.kind);
    if (cls === "part") {
      accHasParts = true; // parts always create/reuse the acc
    } else if (cls === "boundary") {
      flush();
      count++; // user message / system row (s-*), non-assistant message
    }
    // backfill (tool-result/done): fills the existing acc, does not affect numbering
  }
  flush();
  return lastId;
}

function HailuxRuntime({ children }: { children: ReactNode }) {
  const items = useApp((s) => s.items);
  const isRunning = useApp((s) => s.isRunning);
  const sendMessage = useApp((s) => s.sendMessage);
  const interrupt = useApp((s) => s.interrupt);

  const messages = useMemo(() => toThreadMessages(items), [items]);

  const runtime = useExternalStoreRuntime({
    messages,
    convertMessage: (m: ThreadMessageLike) => m,
    isRunning,
    onNew: async (m: AppendMessage) => {
      const text = m.content
        .filter(
          (p): p is Extract<typeof p, { type: "text" }> => p.type === "text",
        )
        .map((p) => p.text)
        .join("\n");
      if (!text) throw new Error("仅支持文本消息");
      await sendMessage(text);
    },
    onCancel: async () => {
      await interrupt();
    },
  });

  return (
    <AssistantRuntimeProvider runtime={runtime}>
      {children}
    </AssistantRuntimeProvider>
  );
}

export function HailuxRuntimeProvider({ children }: { children: ReactNode }) {
  return <HailuxRuntime>{children}</HailuxRuntime>;
}
