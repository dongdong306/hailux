// Session history loading: StoredMessage[] → ChatItem[] render model + token
// stats
import type { ChatAttachment, ChatMention, StoredMessage } from "../runtime/types";
import { compactMarkerText, isToolAttachmentMessage } from "./shared-events";
import type { ChatItem } from "./types";

/** Rebuild result of switchSession: message render model + "this session
 *  cumulative" token accounting */
export interface RebuiltSession {
  items: ChatItem[];
  promptTokens: number;
  completionTokens: number;
  cachedTokens: number;
  contextPromptTokens: number;
  contextCompletionTokens: number;
}

/** Rebuild the render model from history messages (aligned with TUI
 *  load_session_messages):
 *  the compaction summary is not shown; a done divider is inserted only at
 *  the compacted/non-compacted boundary. */
export function rebuildSessionItems(
  messages: StoredMessage[],
  hasSummary: boolean,
): RebuiltSession {
  const items: ChatItem[] = [];
  let compactMarkerInserted = false;
  // Tool messages' runtime_meta only recognizes new-format diff display data
  // (format: "diff");
  // legacy {old,new} data falls back to the result text; timing JSON (no
  // format field) is excluded too
  const toolResults = new Map<string, { result: string; display?: string }>();
  // Usage snapshot of the turn's latest request (lands with the done row in
  // the message's bottom action bar)
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
          // Not JSON, not used as diff display
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
      // Synthetic tool-image messages (read fetching images etc.): runtime_meta
      // carries the {"format":"tool-attachment"} marker, context only for the
      // model, skip rendering.
      // Legacy data without the marker is displayed as-is (tolerated).
      if (isToolAttachmentMessage(msg)) {
        // The synthetic message sits mid-assistant-turn, do not reset turnCtx
        // (usage attribution unaffected)
        continue;
      }
      let images: ChatAttachment[] = [];
      if (msg.attachments) {
        try {
          images = JSON.parse(msg.attachments) as ChatAttachment[];
        } catch {
          // Ignore parse failures
        }
      }
      let mentions: ChatMention[] | undefined;
      if (msg.mentions) {
        try {
          const parsed = JSON.parse(msg.mentions) as ChatMention[];
          if (Array.isArray(parsed) && parsed.length > 0) mentions = parsed;
        } catch {
          // Ignore parse failures
        }
      }
      items.push({ kind: "user", text: msg.content, images, mentions });
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
          // Ignore parse failures
        }
      }
      // runtime_meta (this turn's duration/model/status) → done marker row
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
          // Ignore parse failures
        }
      }
    }
  }
  // All messages compacted (no chat after compaction): aligned with the TUI,
  // append the divider at the end
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
  // Restore the "this session cumulative" accounting: sum all assistant rows
  // carrying usage (top bar display);
  // also record the last usage-carrying row as the context usage estimate
  // (last request's input+output)
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
