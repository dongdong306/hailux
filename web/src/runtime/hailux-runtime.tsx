// assistant-ui 运行时适配器：Zustand ChatItem[] → ThreadMessageLike[]
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

/** 系统提示行（通知/错误/压缩标记），挂在 message.metadata.custom.row */
export interface SystemRow {
  kind: "notice" | "error" | "compact";
  text: string;
  tone?: "info" | "warn" | "danger" | "success";
  detail?: string;
  /** 压缩进行中（spinner 状态行，完成后替换为完成标记） */
  spinning?: boolean;
}

/** 助手消息自定义元数据（思考分段计时 / 本轮结束信息） */
export interface AssistantMeta {
  running?: boolean;
  /** 思考分段计时：每个 reasoning 分段独立计时（对齐 TUI 每块 AgentThinking 自己的 think_ms） */
  thinkSegments?: ThinkSegment[];
  model?: string;
  totalMs?: number;
  status?: string;
  /** 本轮上下文占用（最后一次请求输入/输出 token） */
  ctxPromptTokens?: number;
  ctxCompletionTokens?: number;
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

/** 折叠分类（toThreadMessages / lastAssistantMessageId 共用的单源规则）：
 *  part（建立/复用助手 acc，push 新 part）· backfill（回填既有 acc，不新建 part）
 *  · boundary（结束当前 acc，产出一条非助手消息）。
 *  switch 穷尽 ChatItem["kind"] —— 新增 kind 未分类会编译报错，强制两侧同步 */
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
    // subagent 步骤（带 subagent 标记的 tool-call/tool-result）：
    // 并入最近一个未完成的 task part 作为实时进度（args._steps），不单独成卡
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
        // 实例标识：tasks 数组下标（同名 subagent 并发时名称无法区分）；
        // 无下标的历史/异常数据用 -1 占位，退化为按 agent 名匹配
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
      // 找不到归属 task part（异常顺序/历史数据）：回退为普通工具卡渲染
    }
    switch (foldKind(item.kind)) {
      case "part": {
        // 助手文本 / 思考 / 工具调用：建立或复用 acc 并 push part
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
        // tool-result / done：回填既有 acc，不新建 part
        // （acc 在闭包内赋值，TS 流分析收窄为 null，这里显式断言）
        const a = acc as Accum | null;
        if (!a) break;
        if (item.kind === "tool-result") {
          // 协议不带 tool_call_id：回填到最近的“同名且无 result”的 tool-call
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
          // done：完成信息（模型/耗时/状态/上下文占用）合并进当前助手消息，
          // 由底部操作栏展示
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
        // user / notice / error / compact-marker / compacting：结束当前 acc，各产出一条非助手消息
        flush();
        if (item.kind === "user") {
          messages.push({
            role: "user",
            id: `u-${messages.length}`,
            content: [{ type: "text", text: item.text ?? "" }],
          });
        } else {
          const row: SystemRow =
            item.kind === "error"
              ? { kind: "error", text: item.text ?? "", tone: "danger" }
              : item.kind === "notice"
                ? { kind: "notice", text: item.text ?? "", tone: "info" }
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

/** 轻量计算最后一条助手消息（非系统行）的 id —— 编号规则与 toThreadMessages 一致：
 *  分类共用 foldKind（单源）；acc 建立到 flush 之间不会产出其他消息，
 *  故 flush 时的编号等于 acc 创建时的 messages.length。
 *  避免调用方为取 id 而全量重建消息序列（流式期间 items 每个 chunk 都会变化） */
export function lastAssistantMessageId(items: ChatItem[]): string | null {
  let count = 0; // 对齐 toThreadMessages 的 messages.length
  let accHasParts = false;
  let lastId: string | null = null;

  // 与 flush() 对应：acc 有 part 才产出一条助手消息
  const flush = () => {
    if (!accHasParts) return;
    lastId = `a-${count}`;
    count++;
    accHasParts = false;
  };

  for (const item of items) {
    const cls = foldKind(item.kind);
    if (cls === "part") {
      accHasParts = true; // part 均会建立/复用 acc
    } else if (cls === "boundary") {
      flush();
      count++; // user 消息 / 系统行（s-*），非助手消息
    }
    // backfill（tool-result/done）：回填既有 acc，不影响编号
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
