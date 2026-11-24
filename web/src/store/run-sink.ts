// Run event sink: SSE frames (Snapshot replay / Envelope envelopes / bare
// events) -> state updates.
//
// Exactly one sink is attached at any time — the single subscription owned by
// stream-manager.ts — so the local seq cursor is the only dedup needed (the
// server filters replay by `since`; broadcast delivers once per receiver).
//
// Per-session routing: the sink is bound to its owning session at construction
// and only mutates `items` while that session is the current view — switching
// away mid-run keeps the task running without polluting the new view.
//
// Cross-tab run state: SessionStarted / RunComplete drive isRunning and
// runningSessions, so observer tabs follow a run started elsewhere live.

import type { StoreApi } from "zustand";
import type { Envelope, ServerEvent } from "../runtime/types";
import { handleSharedEvent, itemOps } from "./shared-events";
import type { AppState } from "./types";

export type StoreGet = StoreApi<AppState>["getState"];
export type StoreSet = StoreApi<AppState>["setState"];

/** Latest request's usage snapshot of this turn (attached to the done row in
 * the message footer action bar) */
interface TurnCtx {
  prompt: number;
  completion: number;
}

/**
 * Create a run event sink (fed as onEvent into getSse).
 *
 * `initialSession` is the owning session id. The sink is stateful (local seq
 * cursor + turnCtx); stream-manager.ts guarantees a single live instance.
 */
export function createRunSink(
  initialSession: string,
  get: StoreGet,
  set: StoreSet,
): (frame: unknown) => void {
  let bound = initialSession;
  let lastSeq = 0;
  let turnCtx: TurnCtx | null = null;

  /** Freeze the thinking timer (thinking -> output/tool-call transition;
   * aligns with the TUI's finalize_thinking_ms) */
  const freezeThinking = (ops: ReturnType<typeof itemOps>) => {
    const last = ops.items[ops.items.length - 1];
    if (last?.kind === "reasoning-streaming") {
      const startedAt = last.thinkStartedAt ?? Date.now();
      ops.patchLast((i) => ({
        ...i,
        kind: "reasoning",
        thinkMs: (i.thinkMs ?? 0) + (Date.now() - startedAt),
        thinkStartedAt: undefined,
      }));
    }
  };

  const apply = (event: ServerEvent) => {
    // SessionStarted: enter the running state before any items mutation so
    // later events route correctly
    if (event.type === "SessionStarted") {
      // Take over sessionId only when the current view is (or has) no session;
      // if the user already switched away, bookkeeping only — never steal the view
      const cur = get().sessionId;
      if (cur === null || cur === event.session_id) {
        set((s) => ({
          sessionId: event.session_id,
          isRunning: true,
          runStartedAt: Date.now(),
          lastSeq,
          runningSessions: {
            ...s.runningSessions,
            [event.session_id]: true,
          },
        }));
      }
      turnCtx = null;
      return;
    }

    // Cursor bookkeeping: write the global slot only while the owning session
    // is the current view (late events after a switch must not clobber the
    // new session's cursor)
    if (get().sessionId === bound) {
      set({ lastSeq });
    }
    // Not the current view: bookkeeping only, no rendering. The task keeps
    // running server-side; switching back replays via head_seq + subscription.
    if (get().sessionId !== bound) return;

    const ops = itemOps(get, set);
    const { items, push, patchLast } = ops;
    if (handleSharedEvent(event, ops, set)) return;

    switch (event.type) {
      case "AgentChunk": {
        freezeThinking(ops);
        const last = items[items.length - 1];
        if (last?.kind === "assistant-streaming") {
          patchLast((i) => ({ ...i, text: (i.text ?? "") + event.text }));
        } else {
          push({ kind: "assistant-streaming", text: event.text });
        }
        break;
      }
      case "AgentReasoningChunk": {
        const last = items[items.length - 1];
        if (last?.kind === "reasoning-streaming") {
          patchLast((i) => ({ ...i, text: (i.text ?? "") + event.text }));
        } else {
          push({
            kind: "reasoning-streaming",
            text: event.text,
            thinkStartedAt: Date.now(),
          });
        }
        break;
      }
      case "AgentRetry": {
        // Partial output of failed attempts must not enter context: clear the trailing streaming items in sync
        // (align with TUI pop semantics); the notice ends the current acc, retried content starts a new message
        const cur = [...items];
        while (
          cur.length > 0 &&
          (cur[cur.length - 1]?.kind === "assistant-streaming" ||
            cur[cur.length - 1]?.kind === "reasoning-streaming")
        ) {
          cur.pop();
        }
        const secs = (event.delay_ms / 1000).toFixed(1);
        cur.push({
          kind: "notice",
          text: `请求失败，${secs}s 后进行第 ${event.attempt}/${event.max_retries} 次重试：${event.reason}`,
        });
        set({ items: cur });
        break;
      }
      case "ToolCallStart":
        freezeThinking(ops);
        push({
          kind: "tool-call",
          name: event.name,
          arguments: event.arguments,
          subagent: event.subagent,
          subagentIndex: event.subagent_index,
        });
        break;
      case "ToolResult":
        push({
          kind: "tool-result",
          name: event.name,
          result: event.result,
          display: event.display,
          subagent: event.subagent,
          subagentIndex: event.subagent_index,
        });
        break;
      case "UsageUpdate":
        // Session cumulative (top bar) + record the last request (approximate context usage in the message footer)
        turnCtx = {
          prompt: event.prompt_tokens,
          completion: event.completion_tokens,
        };
        set((s) => ({
          promptTokens: s.promptTokens + event.prompt_tokens,
          completionTokens: s.completionTokens + event.completion_tokens,
          cachedTokens: s.cachedTokens + event.cached_tokens,
          contextPromptTokens: event.prompt_tokens,
          contextCompletionTokens: event.completion_tokens,
          contextWindow: event.context_window || s.contextWindow,
        }));
        break;
      case "PermissionRequest":
        set({
          permission: {
            requestId: event.request_id,
            description: event.description,
            patterns: event.patterns,
            subagent: event.subagent,
          },
        });
        break;
      case "AskUser":
        set({
          askUser: {
            requestId: event.request_id,
            questions: event.questions,
          },
        });
        break;
      case "AgentComplete": {
        // ctx depends on event order: the backend guarantees UsageUpdate precedes AgentComplete;
        // when the turn has no usage at all (e.g. the first request fails), ctx is null → no context usage shown
        freezeThinking(ops);
        const last = items[items.length - 1];
        if (last?.kind === "assistant-streaming") {
          patchLast((i) => ({ ...i, kind: "assistant" }));
        }
        push({
          kind: "done",
          status: event.status,
          totalMs: event.total_ms,
          model: event.model,
          ctxPromptTokens: turnCtx?.prompt,
          ctxCompletionTokens: turnCtx?.completion,
        });
        break;
      }
      case "RunComplete": {
        // Sentinel: wrap up the running state. Turn-end side effects live here
        // (event-driven, also fires for observer tabs that didn't submit the run)
        set((s) => ({
          isRunning: false,
          runStartedAt: null,
          runningSessions: { ...s.runningSessions, [bound]: false },
        }));
        if (event.abnormal) {
          push({ kind: "error", text: "任务异常终止，请刷新页面重新同步" });
        }
        // Refresh the session list (title / updated_at); reconcile global modes
        get().loadSessions().catch(() => {});
        void get().syncMode();
        break;
      }
    }
  };

  return (frame: unknown) => {
    const f = frame as Record<string, unknown> | null;
    if (f && f.type === "Snapshot") {
      const snap = f as unknown as Extract<ServerEvent, { type: "Snapshot" }>;
      // Epoch change = server restart: the seq space is invalidated, cursor
      // resets to 0 (the server already did a full replay from 0)
      if (get().epoch !== null && get().epoch !== snap.epoch) {
        set({ epoch: snap.epoch, lastSeq: 0 });
        lastSeq = 0;
      } else {
        set({ epoch: snap.epoch });
      }
      for (const env of snap.events) {
        if (env.seq <= lastSeq) continue;
        lastSeq = env.seq;
        apply(env.event);
      }
      return;
    }
    if (f && typeof f.seq === "number" && f.event) {
      const env = f as unknown as Envelope;
      if (env.seq <= lastSeq) return;
      lastSeq = env.seq;
      apply(env.event);
      return;
    }
    // Bare event (unusual frames like the Lagged notice)
    apply(f as unknown as ServerEvent);
  };
}
