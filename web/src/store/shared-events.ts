// chat / compact 流共享的事件处理与 items 操作工具
import type { StoreApi } from "zustand";
import type { ServerEvent, StoredMessage } from "../runtime/types";
import type { AppState, ChatItem } from "./types";

/** 工具图片合成消息（read 读取图片等，agent 层为注入模型上下文生成）：
 *  runtime_meta 带 {"format":"tool-attachment"} 标记，历史加载时跳过渲染。
 *  无标记的旧数据不识别（保持原样展示）。 */
export function isToolAttachmentMessage(msg: StoredMessage): boolean {
  if (!msg.runtime_meta) return false;
  try {
    const v = JSON.parse(msg.runtime_meta) as { format?: string };
    return v.format === "tool-attachment";
  } catch {
    return false;
  }
}

/** 从错误响应提取可读文本 */
export async function readError(resp: Response): Promise<string> {
  const text = await resp.text().catch(() => "");
  return text || `HTTP ${resp.status}`;
}

/** 压缩完成标记文本（对齐 TUI CompactMarker：条数 + 耗时；历史恢复无耗时仅条数） */
export function compactMarkerText(count: number, totalMs?: number): string {
  const secs =
    totalMs !== undefined && totalMs > 0
      ? ` · ${Math.max(1, Math.round(totalMs / 1000))}s`
      : "";
  return `已压缩 ${count} 条消息${secs}`;
}

/** 共享 items 操作：push 追加 / patchLast 原地替换末条 */
export function itemOps(
  get: StoreApi<AppState>["getState"],
  set: StoreApi<AppState>["setState"],
) {
  const items = get().items;
  const push = (item: ChatItem) => set((s) => ({ items: [...s.items, item] }));
  const patchLast = (patch: (item: ChatItem) => ChatItem) => {
    if (items.length === 0) return;
    set((s) => ({
      items: [...s.items.slice(0, -1), patch(s.items[s.items.length - 1]!)],
    }));
  };
  return { items, push, patchLast };
}

export type ItemOps = ReturnType<typeof itemOps>;

/** chat / compact 流共享事件分支（Notice / 压缩三事件）；返回 true 表示已消费 */
export function handleSharedEvent(
  event: ServerEvent,
  ops: ItemOps,
  set: StoreApi<AppState>["setState"],
): boolean {
  switch (event.type) {
    case "Notice":
      ops.push({ kind: "notice", text: event.text });
      return true;
    case "CompactStart":
      // 对齐 TUI：压缩中只保留一条状态行，摘要流不展示
      ops.push({ kind: "compacting", text: event.text });
      return true;
    case "CompactComplete":
      // 对齐 TUI：状态行整条替换为完成标记（条数 + 耗时）
      ops.patchLast(() => ({
        kind: "compact-marker",
        text: compactMarkerText(event.compacted_count, event.total_ms),
      }));
      // 对齐 TUI set_session_usage(estimated, 0)：立即刷新上下文计量
      // （仅上下文字段；顶栏累计口径不受压缩影响）
      if (event.context_tokens != null) {
        set({
          contextPromptTokens: event.context_tokens,
          contextCompletionTokens: 0,
        });
      }
      return true;
    case "Error": {
      // 压缩中状态行原地替换为错误（对齐 TUI CompactError 替换语义），其余场景照常追加
      const errItem: ChatItem = { kind: "error", text: event.message };
      if (ops.items[ops.items.length - 1]?.kind === "compacting") {
        ops.patchLast(() => errItem);
      } else {
        ops.push(errItem);
      }
      return true;
    }
    default:
      return false;
  }
}
