// App state: sessions/messages/permission requests/modes (Zustand)
import { create } from "zustand";
import type {
  ChatRequest,
  CommandInfo,
  ModelInfo,
  ProviderOption,
  ServerEvent,
  SessionInfo,
  StatsResponse,
} from "../runtime/types";
import { getJson, postJson, postSse } from "../runtime/sse-client";
import { rebuildSessionItems } from "./session-history";
import { handleSharedEvent, itemOps, readError } from "./shared-events";
import type {
  AppState,
  McpServerEntry,
  ModeStatus,
  SkillEntry,
} from "./types";

/** localStorage key: last visited project directory */
const LAST_WORKDIR_KEY = "hailux.lastWorkDir";

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
  sse: null,

  async initWorkDir() {
    // Prefer the last visited project directory, falling back to the server default (backend startup dir)
    let dir = "";
    try {
      dir = localStorage.getItem(LAST_WORKDIR_KEY) ?? "";
    } catch {
      // localStorage unavailable (private mode etc.) → use default directory
    }
    if (!dir) {
      const resp = await getJson<{ path: string }>("/api/default-workdir");
      dir = resp.path;
    }
    set({ workDir: dir });
    await get().loadSessions();
    get().loadCommands().catch(() => {});
    // Restore real modes from the server after page load/refresh (global modes are independent of work_dir)
    await get().syncMode();
  },

  async loadSessions() {
    // Fetch by current project directory (backend returns default dir when not initialized)
    const workDir = get().workDir;
    const query = workDir ? `?work_dir=${encodeURIComponent(workDir)}` : "";
    const sessions = await getJson<SessionInfo[]>(`/api/sessions${query}`);
    set({ sessions });
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
      // Command list load failure must not block the main flow (completion degrades to no hints)
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
    // name identifies the original server; a different input.name renames it (backend handles it as new_name)
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
      // Send unconditionally: days=0 (all time) is falsy, so conditional sending would be overridden by the backend default of 30 days
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
        workDir: session.work_dir,
        showWorkdirPicker: false,
      });
      await get().loadSessions();
    }
  },

  async switchSession(id) {
    try {
      const detail = await getJson<{
        messages: import("../runtime/types").StoredMessage[];
        compact_summary?: string;
      }>(`/api/sessions/${id}`);
      const rebuilt = rebuildSessionItems(
        detail.messages,
        Boolean(detail.compact_summary),
      );
      // Sessions carry their own working directory — switching a session switches the directory context
      const target = get().sessions.find((s) => s.id === id);
      set({
        sessionId: id,
        items: rebuilt.items,
        scrollBottomSignal: Date.now(),
        isRunning: false,
        runStartedAt: null,
        workDir: target?.work_dir ?? get().workDir,
        promptTokens: rebuilt.promptTokens,
        completionTokens: rebuilt.completionTokens,
        cachedTokens: rebuilt.cachedTokens,
        contextPromptTokens: rebuilt.contextPromptTokens,
        contextCompletionTokens: rebuilt.contextCompletionTokens,
      });
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
      set({
        sessionId: null,
        items: [],
        runStartedAt: null,
        promptTokens: 0,
        completionTokens: 0,
        cachedTokens: 0,
        contextPromptTokens: 0,
        contextCompletionTokens: 0,
      });
    }
    await get().loadSessions();
  },

  async sendMessage(message, attachments, mentions) {
    if (get().isRunning) return;
    const req: ChatRequest = {
      message,
      attachments: attachments?.length ? attachments : undefined,
      mentions: mentions?.length ? mentions : undefined,
      session_id: get().sessionId ?? undefined,
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

    // Snapshot of the latest UsageUpdate in this turn: attached to the done row at AgentComplete in the message footer action bar
    // (the closure variable is rebuilt per request, avoiding a stale snapshot left over from the previous turn)
    let turnCtx: { prompt: number; completion: number } | null = null;

    const handleEvent = (event: ServerEvent) => {
      const ops = itemOps(get, set);
      const { items, push, patchLast } = ops;
      if (handleSharedEvent(event, ops, set)) return;
      // Thinking → output/tool call transition: freeze the thinking timer (align with TUI finalize_thinking_ms)
      const freezeThinking = () => {
        const last = items[items.length - 1];
        if (last?.kind === "reasoning-streaming") {
          const startedAt = last.thinkStartedAt ?? Date.now();
          patchLast((i) => ({
            ...i,
            kind: "reasoning",
            thinkMs: (i.thinkMs ?? 0) + (Date.now() - startedAt),
            thinkStartedAt: undefined,
          }));
        }
      };

      switch (event.type) {
        case "AgentChunk": {
          freezeThinking();
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
          freezeThinking();
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
          freezeThinking();
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
      }
    };

    const sse = postSse("/api/chat", req, handleEvent);
    set({ sse });

    try {
      await sse.done;
    } catch (e) {
      if (!(e instanceof DOMException && e.name === "AbortError")) {
        set((s) => ({
          items: [...s.items, { kind: "error", text: String(e) }],
        }));
      }
    } finally {
      // Only clean up if this stream is still the active one; skip when taken over by setWorkDir (project switch),
      // so the session list is not re-fetched with the old directory and overwritten onto the new project
      if (get().sse === sse) {
        set({ isRunning: false, sse: null, runStartedAt: null });
        get().loadSessions().catch(() => {});
        // Low-cost reconcile after each turn ends (safety net for any mode drift during the run)
        get().syncMode();
      }
    }
  },

  async interrupt() {
    await postJson("/api/interrupt", {
      session_id: get().sessionId ?? undefined,
      work_dir: get().workDir || undefined,
    });
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

    const handleEvent = (event: ServerEvent) => {
      const ops = itemOps(get, set);
      if (handleSharedEvent(event, ops, set)) return;
      switch (event.type) {
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
            askUser: { requestId: event.request_id, questions: event.questions },
          });
          break;
      }
    };

    const sse = postSse(
      "/api/compact",
      { session_id: sessionId, work_dir: get().workDir || undefined },
      handleEvent,
    );
    set({ sse });

    try {
      await sse.done;
    } catch (e) {
      if (!(e instanceof DOMException && e.name === "AbortError")) {
        set((s) => ({
          items: [...s.items, { kind: "error", text: String(e) }],
        }));
      }
    } finally {
      if (get().sse === sse) {
        set({ isRunning: false, sse: null, runStartedAt: null });
      }
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
      // Roll back the optimistic value; the request may have reached the server, so syncMode reconciles the real state
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
      // Fail silently: retry at the next reconcile point (turn end / page visible / project switch)
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
    // Interrupt any in-flight request first, avoiding events leaking into the new project view.
    // Detach the sse reference in the store before aborting: sendMessage's finally detects
    // a non-active stream and skips cleanup and re-fetch (see sendMessage)
    const active = get().sse;
    if (active) {
      set({ isRunning: false, sse: null, runStartedAt: null });
      active.abort();
      await get()
        .interrupt()
        .catch(() => {});
    }
    set({
      workDir: dir,
      sessionId: null,
      items: [],
      isRunning: false,
      runStartedAt: null,
      sse: null,
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
