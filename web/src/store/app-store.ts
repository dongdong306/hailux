import { create } from "zustand";
import type {
  ChatRequest,
  CommandInfo,
  DiscoverModelsResult,
  InterjectRequest,
  ModelInfo,
  ProviderOption,
  SessionDetail,
  SessionInfo,
  StatsResponse,
} from "../runtime/types";
import { HttpError, getJson, postJson } from "../runtime/sse-client";
import { rebuildSessionItems } from "./session-history";
import { closeStream, openStream } from "./stream-manager";
import { readError } from "./shared-events";
import type {
  AppState,
  McpServerEntry,
  ModeStatus,
  QueuedMessage,
  SkillEntry,
} from "./types";

const LAST_WORKDIR_KEY = "hailux.lastWorkDir";

/** crypto.randomUUID requires a secure context (HTTPS / localhost) — plain
 *  HTTP on a LAN IP leaves it undefined and would break enqueueing entirely */
function genClientId(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  return `q-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

/** Submit a chat/compact request. Submit-only: the response body (an SSE
 * stream nobody consumes) is cancelled immediately — run events arrive via
 * the session's single subscription, with server-side replay covering
 * anything published before it attaches. Throws on HTTP errors (409/429/...). */
async function submitRun(path: string, body: unknown): Promise<void> {
  const resp = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!resp.ok) {
    throw new Error(await readError(resp));
  }
  await resp.body?.cancel();
}

// Components keep importing from `../store/app-store`; types are actually defined in ./types
export type {
  ActiveView,
  ChatItem,
  JsonSchemaNode,
  McpServerEntry,
  McpServerInput,
  McpToolEntry,
  ModeStatus,
  SkillEntry,
  SkillFileEntry,
} from "./types";

export const useApp = create<AppState>((set, get) => ({
  sessions: [],
  sessionId: null,
  workDir: "",
  items: [],
  scrollBottomSignal: 0,
  isRunning: false,
  runStartedAt: null,
  promptTokens: 0,
  completionTokens: 0,
  cachedTokens: 0,
  contextPromptTokens: 0,
  contextCompletionTokens: 0,
  contextWindow: 0,
  planMode: false,
  yolo: false,
  effort: "high",
  inputHistory: [],
  permission: null,
  askUser: null,
  models: [],
  providers: [],
  showAddModel: false,
  addModelPreset: null,
  commands: [],
  workdirs: [],
  showWorkdirPicker: false,
  showModelMenu: false,
  activeView: "chat",
  skills: [],
  skillsError: null,
  mcpServers: [],
  mcpError: null,
  stats: null,
  statsFilter: "",
  statsDays: 30,
  statsLoading: false,
  statsError: null,
  escHint: false,
  epoch: null,
  lastSeq: 0,
  runningSessions: {},
  queuedMessages: {},
  deferredSettings: {},
  pendingInterjectEcho: {},

  async initWorkDir() {
    let dir = "";
    try {
      dir = localStorage.getItem(LAST_WORKDIR_KEY) ?? "";
    } catch {
      // localStorage unavailable (private mode etc.) — fall through
    }
    if (!dir) {
      const resp = await getJson<{ path: string }>("/api/default-workdir");
      dir = resp.path;
    }
    set({ workDir: dir });
    await get().loadSessions();
    get().loadCommands().catch(() => {});
    await get().syncMode();
  },

  async loadSessions() {
    const workDir = get().workDir;
    const query = workDir ? `?work_dir=${encodeURIComponent(workDir)}` : "";
    const sessions = await getJson<SessionInfo[]>(`/api/sessions${query}`);
    set({
      sessions,
      runningSessions: {
        ...get().runningSessions,
        ...Object.fromEntries(
          sessions.map((s) => [s.id, Boolean(s.running)] as const),
        ),
      },
    });
  },

  async loadModels() {
    const models = await getJson<ModelInfo[]>("/api/models");
    set({
      models,
      contextWindow:
        models.find((m) => m.active)?.context_window ?? get().contextWindow,
    });
  },

  async loadProviders() {
    const providers = await getJson<ProviderOption[]>("/api/providers");
    set({ providers });
  },

  async addModel(input) {
    const resp = await postJson("/api/models/custom", input);
    if (!resp.ok) throw new Error(await readError(resp));
    set({ showAddModel: false });
    await get().loadModels();
    get().loadProviders().catch(() => {});
  },

  async updateModel(input) {
    const resp = await fetch("/api/models/custom", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
    });
    if (!resp.ok) throw new Error(await readError(resp));
    await get().loadModels();
    get().loadProviders().catch(() => {});
  },

  async deleteModel(selector) {
    const resp = await fetch("/api/models/custom", {
      method: "DELETE",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ selector }),
    });
    if (!resp.ok) throw new Error(await readError(resp));
    await get().loadModels();
    get().loadProviders().catch(() => {});
  },

  async updateProvider(input) {
    const resp = await fetch("/api/providers", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input),
    });
    if (!resp.ok) throw new Error(await readError(resp));
    await get().loadModels();
    get().loadProviders().catch(() => {});
  },

  async deleteProvider(providerId) {
    const resp = await fetch("/api/providers", {
      method: "DELETE",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ provider_id: providerId }),
    });
    if (!resp.ok) throw new Error(await readError(resp));
    await get().loadModels();
    get().loadProviders().catch(() => {});
  },

  async applyDiscoveredModels(providerId, modelIds) {
    const resp = await postJson("/api/models/apply-discovered", {
      provider_id: providerId,
      model_ids: modelIds,
    });
    if (!resp.ok) throw new Error(await readError(resp));
    const result = (await resp.json()) as { added: string[] };
    await get().loadModels();
    return result.added;
  },

  async discoverModels(input) {
    const resp = await postJson("/api/models/discover", input);
    if (!resp.ok) throw new Error(await readError(resp));
    return (await resp.json()) as DiscoverModelsResult;
  },

  async loadWorkdirs() {
    const dirs = await getJson<{ path: string }[]>("/api/workdirs");
    set({ workdirs: dirs.map((d) => d.path) });
  },

  async loadCommands() {
    const workDir = get().workDir;
    const query = workDir ? `?work_dir=${encodeURIComponent(workDir)}` : "";
    try {
      const commands = await getJson<CommandInfo[]>(`/api/commands${query}`);
      set({ commands });
    } catch {
      // Command discovery is best-effort; the slash menu just stays empty
    }
  },

  /** Switch the main area view; refresh the matching list when entering skills/mcp/stats/settings (errors go into xxxError, without blocking the switch) */
  setView(view) {
    set({ activeView: view });
    if (view === "skills") get().reloadSkills().catch(() => {});
    if (view === "mcp") get().reloadMcp().catch(() => {});
    if (view === "stats") {
      get().loadWorkdirs().catch(() => {});
      get().loadStats().catch(() => {});
    }
    if (view === "settings") {
      get().loadModels().catch(() => {});
      get().loadProviders().catch(() => {});
    }
  },

  async reloadSkills() {
    const workDir = get().workDir;
    try {
      const skills = await getJson<SkillEntry[]>(
        `/api/skills${workDir ? `?work_dir=${encodeURIComponent(workDir)}` : ""}`,
      );
      set({ skills, skillsError: null });
    } catch (e) {
      set({
        skills: [],
        skillsError: e instanceof Error ? e.message : String(e),
      });
    }
  },

  async createSkill(input: {
    name: string;
    description: string;
    content: string;
    scope: "global" | "project";
  }) {
    const resp = await postJson("/api/skills", {
      ...input,
      work_dir: get().workDir || undefined,
    });
    if (!resp.ok) throw new Error(await readError(resp));
    await get().reloadSkills();
  },

  async updateSkill(input: {
    location: string;
    name: string;
    description: string;
    content: string;
  }) {
    const resp = await fetch("/api/skills", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        ...input,
        work_dir: get().workDir || undefined,
      }),
    });
    if (!resp.ok) throw new Error(await readError(resp));
    await get().reloadSkills();
  },

  async deleteSkill(location: string) {
    const resp = await fetch("/api/skills", {
      method: "DELETE",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        location,
        work_dir: get().workDir || undefined,
      }),
    });
    if (!resp.ok && resp.status !== 204) throw new Error(await readError(resp));
    await get().reloadSkills();
  },

  async reloadMcp() {
    try {
      const mcpServers = await getJson<McpServerEntry[]>("/api/mcp");
      set({ mcpServers, mcpError: null });
    } catch (e) {
      set({
        mcpServers: [],
        mcpError: e instanceof Error ? e.message : String(e),
      });
    }
  },

  async createMcpServer(input) {
    const resp = await postJson("/api/mcp", input);
    if (!resp.ok) throw new Error(await readError(resp));
    await get().reloadMcp();
  },

  async updateMcpServer(name, input) {
    const { name: newName, ...rest } = input;
    const resp = await fetch("/api/mcp", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        name,
        ...rest,
        new_name: newName !== name ? newName : undefined,
      }),
    });
    if (!resp.ok) throw new Error(await readError(resp));
    await get().reloadMcp();
  },

  async deleteMcpServer(name) {
    const resp = await fetch("/api/mcp", {
      method: "DELETE",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    });
    if (!resp.ok && resp.status !== 204) throw new Error(await readError(resp));
    await get().reloadMcp();
  },

  async loadStats() {
    const { statsFilter, statsDays } = get();
    set({ statsLoading: true, statsError: null });
    try {
      const params = new URLSearchParams();
      if (statsFilter) params.set("work_dir", statsFilter);
      params.set("days", String(statsDays));
      const qs = params.toString();
      const stats = await getJson<StatsResponse>(
        `/api/stats${qs ? `?${qs}` : ""}`,
      );
      set({ stats, statsLoading: false });
    } catch (e) {
      set({
        stats: null,
        statsLoading: false,
        statsError: e instanceof Error ? e.message : String(e),
      });
    }
  },

  setStatsFilter(dir) {
    set({ statsFilter: dir });
    get().loadStats().catch(() => {});
  },

  setStatsDays(days) {
    set({ statsDays: days });
    get().loadStats().catch(() => {});
  },

  async newSession(workDir) {
    const dir = workDir ?? get().workDir;
    const resp = await postJson("/api/sessions", { work_dir: dir || undefined });
    if (resp.ok) {
      const session = (await resp.json()) as SessionInfo;
      set({
        sessionId: session.id,
        items: [],
        isRunning: false,
        runStartedAt: null,
        promptTokens: 0,
        completionTokens: 0,
        cachedTokens: 0,
        contextPromptTokens: 0,
        contextCompletionTokens: 0,
        epoch: null,
        lastSeq: 0,
        workDir: session.work_dir,
        showWorkdirPicker: false,
      });
      // The new session is observable across tabs immediately (idle long
      // connection)
      openStream(get, set, session.id, 0, get().epoch ?? 0);
      await get().loadSessions();
    }
  },

  async switchSession(id) {
    // Close the previous session's stream first; the new one attaches after
    // the history load. Anything published during the gap stays in the
    // server-side replay buffer and is covered by since=head_seq.
    closeStream();
    try {
      const detail = await getJson<SessionDetail>(`/api/sessions/${id}`);
      const rebuilt = rebuildSessionItems(
        detail.messages,
        Boolean(detail.compact_summary),
      );
      const target = get().sessions.find((s) => s.id === id);
      set({
        sessionId: id,
        items: rebuilt.items,
        scrollBottomSignal: Date.now(),
        isRunning: detail.running,
        runStartedAt: detail.running ? Date.now() : null,
        workDir: target?.work_dir ?? get().workDir,
        epoch: detail.epoch,
        lastSeq: detail.head_seq,
        runningSessions: { ...get().runningSessions, [id]: detail.running },
        promptTokens: rebuilt.promptTokens,
        completionTokens: rebuilt.completionTokens,
        cachedTokens: rebuilt.cachedTokens,
        contextPromptTokens: rebuilt.contextPromptTokens,
        contextCompletionTokens: rebuilt.contextCompletionTokens,
      });
      // Long-lived subscription: running -> replay/resume after head_seq;
      // idle -> keep waiting
      openStream(get, set, id, detail.head_seq, detail.epoch);
      // Reconcile a session that finished while unobserved: its RunComplete
      // fired elsewhere (or in a closed tab) and never drained here. Trade-off
      // noted: SessionDetail does not carry the last run status, so an
      // interrupted run's queue also drains — matching the clean-completion
      // path is the lesser surprise vs. items stuck forever.
      if (!detail.running) {
        // drainQueuedHead removes the head synchronously before its first
        // await, so the deferred drain below sees the post-removal length
        void get().drainQueuedHead(id);
        get().drainDeferredSettings(id, !get().queuedMessages[id]?.length);
      }
    } catch (e) {
      // No silent failure: an unset sessionId makes /compact report "no
      // active session"; surface the error so the user can retry.
      set((s) => ({
        items: [
          ...s.items,
          { kind: "error", text: `切换会话失败: ${String(e)}` },
        ],
      }));
    }
  },

  async deleteSession(id) {
    const resp = await fetch(`/api/sessions/${id}`, { method: "DELETE" });
    if (!resp.ok) return;
    if (get().sessionId === id) {
      // Close the deleted session's stream (the backend already evicted the
      // instance and interrupted the run)
      closeStream();
      set({
        sessionId: null,
        items: [],
        isRunning: false,
        runStartedAt: null,
        epoch: null,
        lastSeq: 0,
        promptTokens: 0,
        completionTokens: 0,
        cachedTokens: 0,
        contextPromptTokens: 0,
        contextCompletionTokens: 0,
      });
    }
    // Queues are per-session: drop them along with the session
    set((s) => {
      const dropQueue = id in s.queuedMessages;
      const dropDeferred = id in s.deferredSettings;
      if (!dropQueue && !dropDeferred) return {};
      return {
        queuedMessages: dropQueue
          ? Object.fromEntries(Object.entries(s.queuedMessages).filter(([k]) => k !== id))
          : s.queuedMessages,
        deferredSettings: dropDeferred
          ? Object.fromEntries(Object.entries(s.deferredSettings).filter(([k]) => k !== id))
          : s.deferredSettings,
      };
    });
    await get().loadSessions();
  },

  async sendMessage(message, attachments, mentions) {
    if (get().isRunning) return false;
    // Ensure a session exists before submitting: events are delivered via the
    // session's single subscription, which newSession attaches. The server
    // reuses the latest blank session, so this does not stack empty sessions.
    let sessionId = get().sessionId;
    if (!sessionId) {
      await get().newSession();
      sessionId = get().sessionId;
      if (!sessionId) return false;
    }
    const req: ChatRequest = {
      message,
      attachments: attachments?.length ? attachments : undefined,
      mentions: mentions?.length ? mentions : undefined,
      session_id: sessionId,
      work_dir: get().workDir || undefined,
    };

    // Top bar counters = this session's cumulative totals: keep accumulating across turns; context fields keep old values until the first UsageUpdate
    set((s) => ({
      isRunning: true,
      runStartedAt: Date.now(),
      inputHistory: [...s.inputHistory.filter((h) => h !== message), message].slice(-100),
      items: [
        ...s.items,
        { kind: "user", text: message, images: attachments, mentions },
      ],
    }));

    try {
      await submitRun("/api/chat", req);
      return true;
    } catch (e) {
      // 409 (session busy) / 429 (global cap) / network errors. If the run
      // actually started server-side, the subscription re-asserts isRunning at
      // SessionStarted — the state self-corrects event-driven.
      set((s) => ({
        items: [...s.items, { kind: "error", text: String(e) }],
        isRunning: false,
        runStartedAt: null,
      }));
      return false;
    }
    // Turn-end side effects (session list refresh, mode reconcile) live in the
    // sink's RunComplete branch — fired event-driven, even for observer tabs.
  },

  async interrupt() {
    // Precisely interrupt the current session (the backend locates the
    // instance's cancel flag by session_id)
    const sessionId = get().sessionId;
    if (!sessionId) return;
    await postJson("/api/interrupt", { session_id: sessionId }).catch(
      () => {},
    );
  },

  enqueueMessage(text, images, mentions) {
    const sessionId = get().sessionId;
    if (!sessionId) return;
    const item: QueuedMessage = {
      id: genClientId(),
      text,
      images: images?.length ? images : undefined,
      mentions: mentions?.length ? mentions : undefined,
      createdAt: Date.now(),
    };
    set((s) => ({
      queuedMessages: {
        ...s.queuedMessages,
        [sessionId]: [...(s.queuedMessages[sessionId] ?? []), item],
      },
    }));
  },

  removeQueuedMessage(sessionId, id) {
    set((s) => {
      const queue = s.queuedMessages[sessionId];
      if (!queue) return {};
      return {
        queuedMessages: {
          ...s.queuedMessages,
          [sessionId]: queue.filter((q) => q.id !== id),
        },
      };
    });
  },

  removeQueuedByClientId(sessionId, clientId) {
    // client_id == queue item id (UserMessage echo / cross-tab sync)
    get().removeQueuedMessage(sessionId, clientId);
  },

  setQueuedInFlight(sessionId, id, inFlight) {
    set((s) => ({
      queuedMessages: {
        ...s.queuedMessages,
        [sessionId]: (s.queuedMessages[sessionId] ?? []).map((q) =>
          q.id === id ? { ...q, inFlight: inFlight || undefined } : q,
        ),
      },
    }));
  },

  pushTurnItem(item) {
    // Insert before trailing pending-interject bubbles (same rule as the
    // run-sink push) so notices/errors never land after an optimistic bubble
    set((s) => {
      let at = s.items.length;
      while (
        at > 0 &&
        s.items[at - 1]!.kind === "user" &&
        s.items[at - 1]!.pendingInterject
      ) {
        at--;
      }
      if (at === s.items.length) {
        return { items: [...s.items, item] };
      }
      return {
        items: [...s.items.slice(0, at), item, ...s.items.slice(at)],
      };
    });
  },

  async interjectQueued(sessionId, item) {
    const queue = get().queuedMessages[sessionId] ?? [];
    const originalIdx = queue.findIndex((q) => q.id === item.id);
    if (originalIdx < 0) return;
    if (queue[originalIdx]!.inFlight) return;
    // In-flight marker replaces the old synchronous dequeue: the item stays
    // in the queue until the UserMessage echo removes it (so an
    // InterjectDropped finds it intact), while the marker still guarantees
    // double-trigger protection (a re-entry snapshots an in-flight item and
    // skips)
    get().setQueuedInFlight(sessionId, item.id, true);
    const readd = () =>
      set((s) => {
        const q = (s.queuedMessages[sessionId] ?? []).filter(
          (x) => x.id !== item.id,
        );
        const at = Math.min(originalIdx, q.length);
        const next = [...q];
        next.splice(at, 0, { ...item, inFlight: undefined });
        return { queuedMessages: { ...s.queuedMessages, [sessionId]: next } };
      });

    const runningTarget = get().isRunning && get().sessionId === sessionId;
    if (runningTarget) {
      let delivered = false;
      try {
        const resp = await postJson("/api/interject", {
          session_id: sessionId,
          client_id: item.id,
          message: item.text,
          attachments: item.images,
          mentions: item.mentions,
        } satisfies InterjectRequest);
        if (!resp.ok) throw new HttpError(resp.status, await readError(resp));
        delivered = true;
      } catch (e) {
        get().setQueuedInFlight(sessionId, item.id, false);
        if (e instanceof HttpError && e.status === 409) {
          // fall through to the normal send below
        } else {
          get().pushTurnItem({
            kind: "error",
            text: `插话发送失败: ${String(e)}`,
          });
          return;
        }
      }
      if (delivered) {
        // Optimistic bubble: rendered now, injected at the next loop boundary.
        // The pendingInterject marker keeps pre-echo output inserting before
        // the bubble (timeline matches a rebuilt view); the UserMessage echo
        // consumes the marker and removes the queue item. The bubble is only
        // appended while this session is still the current view — the POST
        // may outlive a session switch, and the sink's invariant is that
        // non-current views never get items mutations (the echoed message is
        // covered by the DB rebuild on switching back).
        if (get().sessionId === sessionId) {
          set((s) => ({
            pendingInterjectEcho: { ...s.pendingInterjectEcho, [item.id]: true },
            items: [
              ...s.items,
              {
                kind: "user",
                text: item.text,
                images: item.images,
                mentions: item.mentions,
                pendingInterject: item.id,
              },
            ],
          }));
        }
        return;
      }
    }
    // Idle / 409 fallback: normal send — no echo will arrive for a plain
    // message, so consume the queue item now (in-flight cleared first)
    get().setQueuedInFlight(sessionId, item.id, false);
    get().removeQueuedMessage(sessionId, item.id);
    const ok = await get().sendMessage(item.text, item.images, item.mentions);
    if (!ok) {
      readd();
    }
  },

  async interjectAllQueued(sessionId) {
    const queue = [...(get().queuedMessages[sessionId] ?? [])];
    // Deliver in queue order; in-flight items are skipped by interjectQueued.
    // A 409 fallback mid-way opens a new run and the remaining items
    // interject into it — all messages still land in order
    for (const item of queue) {
      await get().interjectQueued(sessionId, item);
    }
  },

  async drainQueuedHead(sessionId) {
    const queue = get().queuedMessages[sessionId];
    if (!queue?.length) return;
    const first = queue[0]!;
    if (first.inFlight) {
      // Stuck in-flight (echo lost, e.g. across a server restart): release it
      get().setQueuedInFlight(sessionId, first.id, false);
      return;
    }
    get().removeQueuedMessage(sessionId, first.id);
    const ok = await get().sendMessage(first.text, first.images, first.mentions);
    if (!ok) {
      // Submit failed (error item already in items): back to the front for
      // a manual retry
      set((s) => ({
        queuedMessages: {
          ...s.queuedMessages,
          [sessionId]: [first, ...(s.queuedMessages[sessionId] ?? [])],
        },
      }));
    }
  },

  deferSetting(sessionId, action) {
    set((s) => ({
      deferredSettings: {
        ...s.deferredSettings,
        [sessionId]: [...(s.deferredSettings[sessionId] ?? []), action],
      },
    }));
  },

  applySettingAction(action) {
    if (action.kind === "plan") {
      void get().setPlanMode(!get().planMode);
    } else if (action.kind === "yolo") {
      void get().setYolo(!get().yolo);
    } else if (action.kind === "effort" && action.value) {
      void get().setEffort(action.value);
    } else if (action.kind === "compact") {
      void get().runCompact();
    }
  },

  drainDeferredSettings(sessionId, queueEmpty) {
    const actions = get().deferredSettings[sessionId];
    if (!actions?.length) return;
    // Only /compact opens a run: apply it only after the queue drains so it
    // never races the auto-drain sendMessage; everything else is instant
    const instant = actions.filter((a) => a.kind !== "compact");
    const compacts = queueEmpty ? actions.filter((a) => a.kind === "compact") : [];
    const rest = queueEmpty ? [] : actions.filter((a) => a.kind === "compact");
    set((s) => ({
      deferredSettings: { ...s.deferredSettings, [sessionId]: rest },
    }));
    for (const action of [...instant, ...compacts]) {
      get().applySettingAction(action);
    }
  },

  async runCompact() {
    if (get().isRunning) return;
    const sessionId = get().sessionId;
    if (!sessionId) {
      set((s) => ({
        items: [...s.items, { kind: "notice", text: "当前没有活动会话，无需压缩" }],
      }));
      return;
    }

    set({ isRunning: true, runStartedAt: Date.now() });
    try {
      await submitRun("/api/compact", {
        session_id: sessionId,
        work_dir: get().workDir || undefined,
      });
    } catch (e) {
      set((s) => ({
        items: [...s.items, { kind: "error", text: String(e) }],
        isRunning: false,
        runStartedAt: null,
      }));
    }
  },

  async replyPermission(allow, always) {
    const p = get().permission;
    if (!p) return;
    set({ permission: null });
    await postJson(`/api/permission/${p.requestId}/reply`, {
      allow,
      always,
    });
  },

  async replyAsk(answer) {
    const a = get().askUser;
    if (!a) return;
    set({ askUser: null });
    await postJson(`/api/ask/${a.requestId}/reply`, { answer });
  },

  async setPlanMode(on) {
    set({ planMode: on });
    try {
      const resp = await postJson("/api/plan-mode", { enabled: on });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      // Trust the final state returned by the backend (no local guesswork)
      const mode = (await resp.json()) as ModeStatus;
      set({ yolo: mode.yolo, planMode: mode.plan_mode });
    } catch {
      set({ planMode: !on });
      get().syncMode();
    }
  },

  async setYolo(on) {
    set({ yolo: on });
    try {
      const resp = await postJson("/api/yolo", { enabled: on });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const mode = (await resp.json()) as ModeStatus;
      set({ yolo: mode.yolo, planMode: mode.plan_mode });
    } catch {
      set({ yolo: !on });
      get().syncMode();
    }
  },

  async syncMode() {
    try {
      const mode = await getJson<ModeStatus>("/api/mode");
      set({
        yolo: mode.yolo,
        planMode: mode.plan_mode,
        effort: mode.reasoning_effort,
      });
    } catch {
      // Best-effort reconciliation; retried at the next chat/switch entry
    }
  },

  async setEffort(effort) {
    const prev = get().effort;
    set({ effort });
    try {
      const resp = await postJson("/api/reasoning-effort", { effort });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const mode = (await resp.json()) as ModeStatus;
      set({ yolo: mode.yolo, planMode: mode.plan_mode, effort: mode.reasoning_effort });
    } catch {
      set({ effort: prev });
      get().syncMode();
    }
  },

  async switchModel(selector) {
    await postJson("/api/models", { selector });
    set({ showModelMenu: false });
    await get().loadModels();
  },

  setAddModel(show, preset) {
    set({
      showAddModel: show,
      addModelPreset: show ? (preset ?? null) : null,
    });
    if (show) get().loadProviders().catch(() => {});
  },

  async setWorkDir(dir) {
    if (!dir || dir === get().workDir) {
      set({ showWorkdirPicker: false });
      return;
    }
    // Switching projects does not interrupt running tasks (tasks are
    // view-independent post-decoupling; switching back to the original
    // session resumes live). Only close the current session's stream and
    // reset the view state.
    closeStream();
    set({
      workDir: dir,
      sessionId: null,
      items: [],
      isRunning: false,
      runStartedAt: null,
      epoch: null,
      lastSeq: 0,
      promptTokens: 0,
      completionTokens: 0,
      cachedTokens: 0,
      contextPromptTokens: 0,
      contextCompletionTokens: 0,
      showWorkdirPicker: false,
    });
    try {
      localStorage.setItem(LAST_WORKDIR_KEY, dir);
    } catch {
      // Ignore persistence failure
    }
    await get().loadSessions();
    get().loadCommands().catch(() => {});
    // Reconcile global modes after switching projects (modes persist across projects, consistency is guaranteed by the server)
    get().syncMode();
  },

  setWorkdirPicker(show) {
    set({ showWorkdirPicker: show });
    if (show) get().loadWorkdirs().catch(() => {});
  },

  setModelMenu(show) {
    set({ showModelMenu: show });
    if (show) get().loadModels().catch(() => {});
  },

  setEscHint(on) {
    set({ escHint: on });
  },
}));

/** Whether any global dialog is open (permission confirm / ask user / model & reasoning effort menu / add model / project picker).
 *  View-level Esc handling must yield to closing dialogs, so a single Esc never both closes a dialog and switches views */
export function anyDialogOpen(): boolean {
  const s = useApp.getState();
  return (
    s.permission !== null ||
    s.askUser !== null ||
    s.showAddModel ||
    s.showModelMenu ||
    s.showWorkdirPicker
  );
}
