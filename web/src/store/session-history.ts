// 会话历史加载：StoredMessage[] → ChatItem[] 渲染模型 + token 统计
import type { ChatAttachment, StoredMessage } from "../runtime/types";
import { compactMarkerText, isToolAttachmentMessage } from "./shared-events";
import type { ChatItem } from "./types";

/** switchSession 的重建结果：消息渲染模型 + “本会话累计”token 口径 */
export interface RebuiltSession {
  items: ChatItem[];
  promptTokens: number;
  completionTokens: number;
  cachedTokens: number;
  contextPromptTokens: number;
  contextCompletionTokens: number;
}

/** 由历史消息重建渲染模型（对齐 TUI load_session_messages）：
 *  压缩摘要不展示，仅在 compacted/未 compacted 边界插入完成分隔线。 */
export function rebuildSessionItems(
  messages: StoredMessage[],
  hasSummary: boolean,
): RebuiltSession {
  const items: ChatItem[] = [];
  let compactMarkerInserted = false;
  // Tool 消息的 runtime_meta 仅识别新格式 diff 展示数据（format: "diff"）；
  // 旧格式 {old,new} 存量数据显示 result 文本，计时 JSON（无 format 字段）同样排除
  const toolResults = new Map<string, { result: string; display?: string }>();
  // 本轮最近一次请求的 usage 快照（随 done 行落进消息底部操作栏）
  let turnCtx: { prompt: number; completion: number } | null = null;
  for (const msg of messages) {
    if (msg.role === "Tool" && msg.tool_call_id) {
      let display: string | undefined;
      if (msg.runtime_meta) {
        try {
          const v = JSON.parse(msg.runtime_meta) as { format?: string };
          if (v.format === "diff") {
            display = msg.runtime_meta;
          }
        } catch {
          // 非 JSON，不作为 diff 展示
        }
      }
      toolResults.set(msg.tool_call_id, { result: msg.content, display });
    }
  }
  for (let idx = 0; idx < messages.length; idx++) {
    const msg = messages[idx]!;
    if (
      !compactMarkerInserted &&
      hasSummary &&
      idx > 0 &&
      messages[idx - 1]!.compacted &&
      !msg.compacted
    ) {
      items.push({ kind: "compact-marker", text: compactMarkerText(idx) });
      compactMarkerInserted = true;
    }
    if (msg.role === "User") {
      // 工具图片合成消息（read 读取图片等）：runtime_meta 带
      // {"format":"tool-attachment"} 标记，仅供模型上下文，跳过渲染。
      // 无标记的旧数据保持原样展示（容忍）。
      if (isToolAttachmentMessage(msg)) {
        // 合成消息位于 assistant 回合中间，不重置 turnCtx（usage 归属不受影响）
        continue;
      }
      let images: ChatAttachment[] = [];
      if (msg.attachments) {
        try {
          images = JSON.parse(msg.attachments) as ChatAttachment[];
        } catch {
          // 忽略解析失败
        }
      }
      items.push({ kind: "user", text: msg.content, images });
      turnCtx = null;
    } else if (msg.role === "Assistant") {
      if (msg.prompt_tokens !== null) {
        turnCtx = {
          prompt: msg.prompt_tokens,
          completion: msg.completion_tokens ?? 0,
        };
      }
      if (msg.reasoning_content) {
        items.push({
          kind: "reasoning",
          text: msg.reasoning_content,
          thinkMs: msg.think_ms ?? undefined,
        });
      }
      if (msg.content) items.push({ kind: "assistant", text: msg.content });
      if (msg.tool_calls) {
        try {
          const calls = JSON.parse(msg.tool_calls) as {
            id: string;
            function?: { name: string; arguments?: string };
          }[];
          for (const call of calls) {
            items.push({
              kind: "tool-call",
              name: call.function?.name ?? "",
              arguments: call.function?.arguments ?? "",
            });
            const tr = toolResults.get(call.id);
            if (tr !== undefined) {
              items.push({
                kind: "tool-result",
                name: call.function?.name ?? "",
                result: tr.result,
                display: tr.display,
              });
            }
          }
        } catch {
          // 忽略解析失败
        }
      }
      // runtime_meta（含本轮耗时/模型/状态）→ 完成标记行
      if (msg.runtime_meta) {
        try {
          const meta = JSON.parse(msg.runtime_meta) as {
            total_ms?: number;
            model?: string;
            status?: string;
          };
          if (typeof meta.total_ms === "number") {
            items.push({
              kind: "done",
              status: meta.status ?? "completed",
              totalMs: meta.total_ms,
              model: meta.model ?? "",
              ctxPromptTokens: turnCtx?.prompt,
              ctxCompletionTokens: turnCtx?.completion,
            });
          }
        } catch {
          // 忽略解析失败
        }
      }
    }
  }
  // 全部消息已压缩（压缩后未再对话）：对齐 TUI，末尾补插分隔线
  if (
    !compactMarkerInserted &&
    hasSummary &&
    messages.length > 0 &&
    messages.every((m) => m.compacted)
  ) {
    items.push({
      kind: "compact-marker",
      text: compactMarkerText(messages.length),
    });
  }
  // 恢复“本会话累计”口径：全部带 usage 的 assistant 行求和（顶栏展示）
  // 同时记录最后一条带 usage 的行，作为上下文占用的近似（最后请求的输入+输出）
  let promptTokens = 0;
  let completionTokens = 0;
  let cachedTokens = 0;
  let contextPromptTokens = 0;
  let contextCompletionTokens = 0;
  for (const m of messages) {
    if (m.prompt_tokens !== null) {
      promptTokens += m.prompt_tokens;
      completionTokens += m.completion_tokens ?? 0;
      cachedTokens += m.cached_tokens ?? 0;
      contextPromptTokens = m.prompt_tokens;
      contextCompletionTokens = m.completion_tokens ?? 0;
    }
  }
  return {
    items,
    promptTokens,
    completionTokens,
    cachedTokens,
    contextPromptTokens,
    contextCompletionTokens,
  };
}
