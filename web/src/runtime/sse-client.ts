// SSE 客户端：POST + fetch ReadableStream（EventSource 只支持 GET，不适用）
import type { ServerEvent } from "./types";

export interface SseSession {
  abort: () => void;
  /** 流正常结束 resolve；异常/中止 reject */
  done: Promise<void>;
}

export function postSse(
  url: string,
  body: unknown,
  onEvent: (event: ServerEvent) => void,
): SseSession {
  const controller = new AbortController();

  const done = (async () => {
    const response = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: controller.signal,
    });

    if (!response.ok || !response.body) {
      const text = await response.text().catch(() => "");
      throw new Error(text || `HTTP ${response.status}`);
    }

    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    for (;;) {
      const { done: finished, value } = await reader.read();
      if (finished) break;

      buffer += decoder.decode(value, { stream: true });
      // SSE 事件以空行分隔
      const blocks = buffer.split("\n\n");
      buffer = blocks.pop() ?? "";

      for (const block of blocks) {
        for (const line of block.split("\n")) {
          if (!line.startsWith("data: ")) continue;
          const raw = line.slice(6);
          try {
            onEvent(JSON.parse(raw) as ServerEvent);
          } catch {
            // 忽略无法解析的行（如 keep-alive 注释）
          }
        }
      }
    }
  })();

  return {
    abort: () => controller.abort(),
    done,
  };
}

export async function postJson(url: string, body?: unknown): Promise<Response> {
  return fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}

export async function getJson<T>(url: string): Promise<T> {
  const resp = await fetch(url);
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  return resp.json() as Promise<T>;
}
