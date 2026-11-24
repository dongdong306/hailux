// SSE client: fetch ReadableStream (EventSource is GET-only, not suitable).
// All event delivery goes through the single subscription stream owned by
// stream-manager.ts; POST endpoints are submit-only (see app-store).

/** HTTP error carrying the status code, so callers can distinguish
 * permanent conditions (404/410) from transient ones (network drop). */
export class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

function readSseResponse(
  response: Response,
  onEvent: (event: unknown) => void,
): Promise<void> {
  if (!response.ok || !response.body) {
    const textPromise = response.text().catch(() => "");
    return textPromise.then((text) => {
      throw new HttpError(
        response.status,
        text ||
          (response.status === 413
            ? "请求体过大（附件过多或过大）"
            : `HTTP ${response.status}`),
      );
    });
  }

  const body = response.body;
  return (async () => {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    for (;;) {
      const { done: finished, value } = await reader.read();
      if (finished) break;

      buffer += decoder.decode(value, { stream: true });
      const blocks = buffer.split("\n\n");
      buffer = blocks.pop() ?? "";

      for (const block of blocks) {
        for (const line of block.split("\n")) {
          if (!line.startsWith("data: ")) continue;
          const raw = line.slice(6);
          try {
            onEvent(JSON.parse(raw));
          } catch {
            // Ignore unparseable lines (e.g. keep-alive comments)
          }
        }
      }
    }
  })();
}

/** GET + SSE (the session event subscription stream; onEvent receives raw
 * pre-decode frames). Rejects on error and when `signal` aborts. */
export async function getSse(
  url: string,
  onEvent: (event: unknown) => void,
  signal?: AbortSignal,
): Promise<void> {
  const response = await fetch(url, { signal });
  await readSseResponse(response, onEvent);
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
  if (!resp.ok) throw new HttpError(resp.status, `HTTP ${resp.status}`);
  return resp.json() as Promise<T>;
}
