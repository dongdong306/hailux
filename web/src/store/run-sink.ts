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
import type { AppState, ChatItem } from "./types";

export type StoreGet = StoreApi<AppState>["getState"];
export type StoreSet = StoreApi<AppState>["setState"];

interface TurnCtx {
  prompt: number;
  completion: number;
}

export function createRunSink(
  initialSession: string,
  get: StoreGet,
  set: StoreSet,
): (frame: unknown) => void {
  let bound = initialSession;
  let lastSeq = 0;
  let turnCtx: TurnCtx | null = null;
  /** Whether the last AgentComplete was clean (RunComplete drains the queue
   *  only then; interrupted/errored turns keep it for manual handling) */
  let lastCompleted = false;

  /** Index of the last item that is not a trailing pending-interject
   *  bubble: those bubbles are not part of the previous turn's output, so
   *  stream append/freeze must skip them (TUI's turn_output_insert_index) */
  const lastRealIndex = (list: ChatItem[]) => {
    let i = list.length - 1;
    while (i >= 0 && list[i]!.kind === "user" && list[i]!.pendingInterject) {
      i--;
    }
    return i;
  };

  /** Freeze the thinking timer (thinking -> output/tool-call transition;
   * aligns with the TUI's finalize_thinking_ms) */
  const freezeThinking = (ops: ReturnType<typeof itemOps>) => {
    const i = lastRealIndex(ops.items);
    const last = i >= 0 ? ops.items[i] : undefined;
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
      lastCompleted = false;
      return;
    }

    // UserMessage echo bookkeeping (view-independent):
    // - dequeue the queue item (covers the in-flight marker design: the item
    //   stays queued from interject until the echo arrives)
    // - consume the optimistic marker: a hit means this tab already rendered
    //   the bubble, only the marker cleanup remains
    let interjectEchoedLocally = false;
    if (event.type === "UserMessage") {
      get().removeQueuedByClientId(bound, event.client_id);
      const pending = get().pendingInterjectEcho;
      if (pending[event.client_id]) {
        const { [event.client_id]: _drop, ...rest } = pending;
        set({ pendingInterjectEcho: rest });
        interjectEchoedLocally = true;
      }
    }
    // Accepted-but-dropped bookkeeping (view-independent): release the
    // in-flight item so it stays queued for a manual resend, and drop the
    // optimistic marker (the bubble itself is removed below, current view
    // only — other tabs never rendered it)
    let interjectDroppedLocally = false;
    if (event.type === "InterjectDropped") {
      get().setQueuedInFlight(bound, event.client_id, false);
      const pending = get().pendingInterjectEcho;
      if (pending[event.client_id]) {
        const { [event.client_id]: _drop, ...rest } = pending;
        set({ pendingInterjectEcho: rest });
        interjectDroppedLocally = true;
      }
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

    // While an optimistic interjection bubble (pendingInterject, echo not
    // yet arrived) waits at the tail, the previous turn's later output
    // appends before it — the timeline matches a rebuilt view. The marker is
    // stripped on echo, restoring append-at-end for the new turn. Stream
    // lookups also skip trailing bubbles so thinking/answers never get split
    // into two cards by a bubble.
    const push = (item: ChatItem) => {
      const cur = get().items;
      const at = lastRealIndex(cur) + 1;
      if (at === cur.length) {
        set((s) => ({ items: [...s.items, item] }));
      } else {
        set((s) => ({
          items: [...s.items.slice(0, at), item, ...s.items.slice(at)],
        }));
      }
    };
    const patchLast = (patch: (item: ChatItem) => ChatItem) => {
      const i = lastRealIndex(get().items);
      if (i < 0) return;
      set((s) => ({
        items: [...s.items.slice(0, i), patch(s.items[i]!), ...s.items.slice(i + 1)],
      }));
    };
    const items = get().items;
    const ops = { items, push, patchLast };
    if (handleSharedEvent(event, ops, set)) return;

    switch (event.type) {
      case "AgentChunk": {
        freezeThinking(ops);
        const lastIdx = lastRealIndex(items);
        const last = lastIdx >= 0 ? items[lastIdx] : undefined;
        if (last?.kind === "assistant-streaming") {
          patchLast((i) => ({ ...i, text: (i.text ?? "") + event.text }));
        } else {
          push({ kind: "assistant-streaming", text: event.text });
        }
        break;
      }
      case "AgentReasoningChunk": {
        const lastIdx = lastRealIndex(items);
        const last = lastIdx >= 0 ? items[lastIdx] : undefined;
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
        let i = lastRealIndex(cur);
        while (
          i >= 0 &&
          (cur[i]!.kind === "assistant-streaming" ||
            cur[i]!.kind === "reasoning-streaming")
        ) {
          cur.splice(i, 1);
          i = lastRealIndex(cur);
        }
        const secs = (event.delay_ms / 1000).toFixed(1);
        set({ items: cur });
        push({
          kind: "notice",
          text: `请求失败，${secs}s 后进行第 ${event.attempt}/${event.max_retries} 次重试：${event.reason}`,
        });
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
        lastCompleted = event.status === "completed";
        const lastIdx = lastRealIndex(items);
        const last = lastIdx >= 0 ? items[lastIdx] : undefined;
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
      case "UserMessage": {
        // Locally rendered interjection: the echo only strips the bubble's
        // pendingInterject marker (the bubble is already in the right
        // timeline spot; later turns resume append-at-end), never re-renders.
        // Miss (other tab's interjection / replay after reconnect) → render
        // normally.
        if (interjectEchoedLocally) {
          const cid = event.client_id;
          set((s) => ({
            items: s.items.map((it) =>
              it.pendingInterject === cid
                ? { ...it, pendingInterject: undefined }
                : it,
            ),
          }));
        } else {
          push({
            kind: "user",
            text: event.content,
            images: event.attachments?.length ? event.attachments : undefined,
            mentions: event.mentions?.length ? event.mentions : undefined,
          });
        }
        break;
      }
      case "InterjectDropped": {
        // Never entered the context: remove the optimistic bubble and tell
        // the user the message is back in the queue
        if (interjectDroppedLocally) {
          const cid = event.client_id;
          set((s) => ({
            items: s.items.filter((it) => it.pendingInterject !== cid),
          }));
          get().pushTurnItem({
            kind: "notice",
            text: "插话未送达（任务已中断或结束），消息已放回排队列表",
          });
        }
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
        // Leftover in-flight interjections: echoes arrive at the injection
        // boundary (before AgentComplete), so anything still pending never
        // entered the context (interrupted before the boundary / teardown
        // race). Remove the optimistic bubbles — the message is NOT in the
        // conversation — and release the queue items; the auto-drain below
        // resends them as a normal message.
        const leftoverIds = Object.keys(get().pendingInterjectEcho);
        if (leftoverIds.length > 0) {
          set({ pendingInterjectEcho: {} });
          set((s) => ({
            items: s.items.filter((it) => !it.pendingInterject),
          }));
          set((s) => ({
            queuedMessages: {
              ...s.queuedMessages,
              [bound]: (s.queuedMessages[bound] ?? []).map((q) =>
                leftoverIds.includes(q.id)
                  ? { ...q, inFlight: undefined }
                  : q,
              ),
            },
          }));
        }
        // Queue auto-drain (this view's session only; interrupted/errored/
        // compaction turns keep the queue). Chained: this RunComplete pops
        // one, its RunComplete pops the next. sendMessage's optimistic items
        // push + isRunning flip are synchronous before its first await, so
        // ordering stays deterministic.
        if (!event.abnormal && lastCompleted) {
          void get().drainQueuedHead(bound);
        }
        // Deferred setting commands (/plan /yolo /effort /compact) apply
        // after a clean turn; compact waits for an empty queue (mutually
        // exclusive with the drain chain)
        if (!event.abnormal && lastCompleted) {
          get().drainDeferredSettings(bound, !get().queuedMessages[bound]?.length);
        }
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
    apply(f as unknown as ServerEvent);
  };
}
