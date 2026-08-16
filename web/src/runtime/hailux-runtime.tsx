// assistant-ui 运行时适配器：Zustand ChatItem[] → ThreadMessageLike[]
import { useMemo, type ReactNode } from "react";
import {
  AssistantRuntimeProvider,
  useExternalStoreRuntime,
  type AppendMessage,
  type ThreadMessageLike,
} from "@assistant-ui/react";
import { useApp, type ChatItem } from "../store/app-store";

/** 系统提示行（通知/错误/压缩标记），挂在 message.metadata.custom.row */
export interface SystemRow {
  kind: "notice" | "error" | "compact";
  text: string;
  tone?: "info" | "warn" | "danger" | "success";
  detail?: string;
}

/** 助手消息自定义元数据（思考分段计时 / 本轮结束信息） */
export interface AssistantMeta {
  running?: boolean;
  /** 思考分段计时：每个 reasoning 分段独立计时（对齐 TUI 每块 AgentThinking 自己的 think_ms） */
  thinkSegments?: ThinkSegment[];
  model?: string;
  totalMs?: number;
  status?: string;
}

/** 单个思考分段计时（按消息内 part 索引关联） */
export interface ThinkSegment {
  /** 所属 reasoning part 在消息 parts 中的索引 */
  partIndex: number;
  /** 已冻结的分段耗时（ms） */
  ms?: number;
  /** 进行中分段的起点（epoch ms） */
  startedAt?: number;
}

type AssistantPart =
  | { type: "text"; text: string }
  | { type: "reasoning"; text: string }
  | {
      type: "tool-call";
      toolCallId: string;
      toolName: string;
      // eslint 无配置，any 用于匹配 ReadonlyJSONObject
      args: Record<string, any>;
      argsText: string;
      result?: { output: string; display?: string };
    };

interface Accum {
  id: string;
  parts: AssistantPart[];
  running: boolean;
  thinkSegments: ThinkSegment[];
  /** 本轮结束信息（AgentComplete），底部操作栏展示 */
  model?: string;
  totalMs?: number;
  status?: string;
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

/** 把扁平 ChatItem 流折叠为 assistant-ui 消息序列 */
export function toThreadMessages(items: ChatItem[]): ThreadMessageLike[] {
  const messages: ThreadMessageLike[] = [];
  let acc: Accum | null = null;
  let toolSeq = 0;

  const flush = () => {
    if (!acc || acc.parts.length === 0) return;
    // 镜像 fromThreadMessageLike 的空 part 过滤规则（空 text/reasoning 会被丢弃），
    // 先过滤再重编 thinkSegments 的 partIndex，保证与运行时 parts 索引一致
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
    const { model, totalMs, status, id: _id, parts: _parts, ...rest } = acc;
    const custom: Record<string, unknown> = { ...rest };
    if (thinkSegments.length > 0) custom.thinkSegments = thinkSegments;
    if (model !== undefined) custom.model = model;
    if (totalMs !== undefined) custom.totalMs = totalMs;
    if (status !== undefined) custom.status = status;
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
    switch (item.kind) {
      case "user": {
        flush();
        messages.push({
          role: "user",
          id: `u-${messages.length}`,
          content: [{ type: "text", text: item.text ?? "" }],
        });
        break;
      }
      case "assistant":
      case "assistant-streaming": {
        const a = ensureAcc(item.kind === "assistant-streaming");
        const last = a.parts[a.parts.length - 1];
        if (item.kind === "assistant-streaming" && last?.type === "text") {
          last.text += item.text ?? "";
        } else {
          a.parts.push({ type: "text", text: item.text ?? "" });
        }
        break;
      }
      case "reasoning":
      case "reasoning-streaming": {
        const a = ensureAcc(item.kind === "reasoning-streaming");
        // 每个思考 item 独立成 part（store 层已把同段文本 patch 进同一 item，
        // 流式→冻结后新分段是全新 item，不跨 item 合并）
        a.parts.push({ type: "reasoning", text: item.text ?? "" });
        const partIndex = a.parts.length - 1;
        if (item.kind === "reasoning-streaming") {
          a.thinkSegments.push({
            partIndex,
            startedAt: item.thinkStartedAt ?? Date.now(),
          });
        } else if (item.thinkMs !== undefined && item.thinkMs > 0) {
          // 冻结（store 已算好耗时）或历史恢复（DB think_ms），各记各的
          a.thinkSegments.push({ partIndex, ms: item.thinkMs });
        }
        break;
      }
      case "tool-call": {
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
        break;
      }
      case "tool-result": {
        // 协议不带 tool_call_id：回填到最近的“同名且无 result”的 tool-call
        // （acc 在闭包内赋值，TS 流分析收窄为 null，这里显式断言）
        const a = acc as Accum | null;
        if (a) {
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
        }
        break;
      }
      case "notice":
      case "error":
      case "done":
      case "compact-marker": {
        if (item.kind === "done") {
          // 完成信息（模型/耗时/状态）合并进当前助手消息，由底部操作栏展示
          const a = acc as Accum | null;
          if (a) {
            a.running = false;
            a.model = item.model;
            a.totalMs = item.totalMs;
            a.status = item.status;
          }
          break;
        }
        flush();
        const row: SystemRow =
          item.kind === "error"
            ? { kind: "error", text: item.text ?? "", tone: "danger" }
            : item.kind === "notice"
              ? { kind: "notice", text: item.text ?? "", tone: "info" }
              : { kind: "compact", text: item.text ?? "", tone: "info" };
        messages.push({
          role: "assistant",
          id: `s-${messages.length}`,
          content: [{ type: "text", text: "" }],
          metadata: { custom: { row } },
        });
        break;
      }
    }
  }
  flush();
  return messages;
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
