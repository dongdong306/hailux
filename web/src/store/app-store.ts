// App state: sessions/messages/permission requests/modes (Zustand)
import { create } from "zustand";
import type {
  ChatRequest,
  CommandInfo,
  DiscoverModelsResult,
  ModelInfo,
  ProviderOption,
  SessionDetail,
  SessionInfo,
  StatsResponse,
} from "../runtime/types";
import { getJson, postJson } from "../runtime/sse-client";
import { rebuildSessionItems } from "./session-history";
import { closeStream, openStream } from "./stream-manager";
import { readError } from "./shared-events";
import type {
  AppState,
  McpServerEntry,
  ModeStatus,
  SkillEntry,
} from "./types";

/** localStorage key: last visited project directory */
const LAST_WORKDIR_KEY = "hailux.lastWorkDir";

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
    // Sidebar live indicator: the list response carries per-session running
    // state, merged into runningSessions
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
      // Sessions carry their own working directory — switching a session switches the directory context
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
    await get().loadSessions();
  },

  async sendMessage(message, attachments, mentions) {
    if (get().isRunning) return;
    // Ensure a session exists before submitting: events are delivered via the
    // session's single subscription, which newSession attaches. The server
    // reuses the latest blank session, so this does not stack empty sessions.
    let sessionId = get().sessionId;
    if (!sessionId) {
      await get().newSession();
      sessionId = get().sessionId;
      if (!sessionId) return;
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
    } catch (e) {
      // 409 (session busy) / 429 (global cap) / network errors. If the run
      // actually started server-side, the subscription re-asserts isRunning at
      // SessionStarted — the state self-corrects event-driven.
      set((s) => ({
        items: [...s.items, { kind: "error", text: String(e) }],
        isRunning: false,
        runStartedAt: null,
      }));
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
