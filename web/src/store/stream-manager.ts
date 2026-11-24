// Single-stream ownership manager.
//
// Structural invariant: **at most one event stream exists app-wide** — the
// subscription of the currently viewed session. Ownership transfers carry a
// generation token, so stale continuations (resubscribe timers, end-of-stream
// handlers) become inert by a single `current?.gen !== gen` check instead of
// identity heuristics spread across call sites. This is what makes double
// delivery structurally impossible: a replaced stream is aborted, its sink
// stops parsing, and the replacement attaches with the session's cursor so
// the server-side replay covers any gap.
//
// Chat/compact submission does NOT open streams: POST endpoints are
// submit-only (see sendMessage/runCompact in app-store); run events always
// arrive through the subscription, with the replay buffer covering anything
// published before it attaches.

import { HttpError, getJson, getSse } from "../runtime/sse-client";
import type { SessionDetail } from "../runtime/types";
import { createRunSink, type StoreGet, type StoreSet } from "./run-sink";

/** Backoff for transient failures of the detail fetch (network drop, server
 * restarting). Only a definitive 404/410 (session gone) stops resubscribing. */
const RETRY_DELAYS_MS = [1000, 2000, 4000, 8000, 15000, 30000];

interface StreamHandle {
  gen: number;
  controller: AbortController;
}

let nextGen = 1;
let current: StreamHandle | null = null;

/** Close the active stream, if any. The closed stream's continuation detects
 * the generation change and drops itself. */
export function closeStream(): void {
  current?.controller.abort();
  current = null;
}

/** Attach the event subscription of session `id`. Opening closes the previous
 * stream (if it belonged to another session). */
export function openStream(
  get: StoreGet,
  set: StoreSet,
  id: string,
  since: number,
  epoch: number,
): void {
  closeStream();
  const gen = nextGen++;
  const controller = new AbortController();
  current = { gen, controller };

  const sink = createRunSink(id, get, set);
  const done = getSse(
    `/api/sessions/${id}/events?since=${since}&epoch=${epoch}`,
    sink,
    controller.signal,
  );

  let attempt = 0;
  void (async () => {
    for (;;) {
      try {
        await done;
      } catch {
        // Network drop etc. — the resubscribe below handles it
      }
      // Inert unless we still own the stream: closeStream/openStream having
      // moved on means this end was intentional or the stream was replaced
      if (current?.gen !== gen) return;
      try {
        const detail = await getJson<SessionDetail>(`/api/sessions/${id}`);
        // The detail fetch is an async gap — ownership may have moved meanwhile
        if (current?.gen !== gen) return;
        set({
          epoch: detail.epoch,
          lastSeq: detail.head_seq,
          isRunning: detail.running,
          runStartedAt: detail.running ? Date.now() : null,
        });
        setTimeout(() => {
          if (current?.gen === gen && get().sessionId === id) {
            openStream(get, set, id, detail.head_seq, detail.epoch);
          }
        }, 1000);
        return;
      } catch (e) {
        if (e instanceof HttpError && (e.status === 404 || e.status === 410)) {
          // Session deleted: stop resubscribing
          if (current?.gen === gen) current = null;
          return;
        }
        // Transient: retry the detail fetch with backoff
        const delay = RETRY_DELAYS_MS[Math.min(attempt, RETRY_DELAYS_MS.length - 1)];
        await new Promise((r) => setTimeout(r, delay));
        attempt += 1;
        if (current?.gen !== gen) return;
      }
    }
  })();
}
