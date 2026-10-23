// 应用状态：会话/消息/权限请求/模式（Zustand）
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

/** localStorage：上次访问的项目目录 key */
const LAST_WORKDIR_KEY = "hailux.lastWorkDir";

// 组件沿用 `../store/app-store` 导入路径；类型实际定义在 ./types
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
  showModelPicker: false,
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
    // 优先上次访问的项目目录，fallback 到服务器默认目录（后端启动目录）
    let dir = "";
    try {
      dir = localStorage.getItem(LAST_WORKDIR_KEY) ?? "";
    } catch {
      // localStorage 不可用（隐私模式等）→ 用默认目录
    }
    if (!dir) {
      const resp = await getJson<{ path: string }>("/api/default-workdir");
      dir = resp.path;
    }
    set({ workDir: dir });
    await get().loadSessions();
    get().loadCommands().catch(() => {});
    // 页面加载/刷新后从服务器恢复真实模式（全局模式与 work_dir 无关）
    await get().syncMode();
  },

  async loadSessions() {
    // 按当前项目目录拉取（未初始化时后端按默认目录返回）
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
      // 命令列表加载失败不阻塞主流程（补全降级为无提示）
    }
  },

  /** 切换主区域视图；进入 skills/mcp/stats/settings 时刷新对应列表（错误写入 xxxError，不阻止切换） */
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
    // name 定位原服务器；input.name 不同即改名（后端按 new_name 处理）
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
      // 无条件发送：days=0（全部时间）是 falsy，条件发送会被后端默认 30 天覆盖
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
    const detail = await getJson<{
      messages: import("../runtime/types").StoredMessage[];
      compact_summary?: string;
    }>(`/api/sessions/${id}`);
    const rebuilt = rebuildSessionItems(
      detail.messages,
      Boolean(detail.compact_summary),
    );
    // 会话携带其所属工作目录 —— 切换会话即切换目录上下文
    const target = get().sessions.find((s) => s.id === id);
    set({
      sessionId: id,
      items: rebuilt.items,
      isRunning: false,
      runStartedAt: null,
      workDir: target?.work_dir ?? get().workDir,
      promptTokens: rebuilt.promptTokens,
      completionTokens: rebuilt.completionTokens,
      cachedTokens: rebuilt.cachedTokens,
      contextPromptTokens: rebuilt.contextPromptTokens,
      contextCompletionTokens: rebuilt.contextCompletionTokens,
    });
  },

  async deleteSession(id) {
    const resp = await fetch(`/api/sessions/${id}`, { method: "DELETE" });
    if (!resp.ok) return;
    if (get().sessionId === id) {
      // 删除的是当前会话 → 回到空状态
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

  async sendMessage(message, attachments) {
    if (get().isRunning) return;
    const req: ChatRequest = {
      message,
      attachments: attachments?.length ? attachments : undefined,
      session_id: get().sessionId ?? undefined,
      work_dir: get().workDir || undefined,
    };

    // 本地立刻显示用户消息 + 记录输入历史 + 启动本地请求计时
    // （完成后由 done 行展示后端返回的 total_ms）
    // 顶栏计量 = 本会话累计：跨对话轮持续累加；context 字段保留旧值直到首个 UsageUpdate
    set((s) => ({
      isRunning: true,
      runStartedAt: Date.now(),
      inputHistory: [...s.inputHistory.filter((h) => h !== message), message].slice(-100),
      items: [...s.items, { kind: "user", text: message, images: attachments }],
    }));

    // 本轮最近一次 UsageUpdate 快照：AgentComplete 时随 done 行落进消息底部操作栏
    // （闭包变量随每次请求重建，避免快照到上一轮的旧值）
    let turnCtx: { prompt: number; completion: number } | null = null;

    const handleEvent = (event: ServerEvent) => {
      const ops = itemOps(get, set);
      const { items, push, patchLast } = ops;
      if (handleSharedEvent(event, ops, set)) return;
      // 思考→输出/工具调用切换：冻结思考计时（对齐 TUI finalize_thinking_ms）
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
          // 失败尝试的部分输出不进入上下文：末尾流式 items 同步清除
          //（对齐 TUI pop 语义），notice 结束当前 acc，重试内容另起新消息
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
          // 会话累计（顶栏）+ 记录最后请求（消息底部上下文占用近似）
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
          // ctx 依赖事件顺序：后端保证 UsageUpdate 先于 AgentComplete；
          // 本轮无任何 usage（如首个请求即失败）时 ctx 为 null → 不展示上下文占用
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
      // 仅当该流仍是活跃流时清理；被 setWorkDir 接管（切项目）时跳过，
      // 避免用旧目录重拉会话列表覆盖新项目
      if (get().sse === sse) {
        set({ isRunning: false, sse: null, runStartedAt: null });
        get().loadSessions().catch(() => {});
        // 每轮对话结束后低成本对账（兜底运行期间的任何模式漂移）
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
      // 以后端返回的最终状态为准（不依赖本地猜测）
      const mode = (await resp.json()) as ModeStatus;
      set({ yolo: mode.yolo, planMode: mode.plan_mode });
    } catch {
      // 回滚乐观值；请求可能已到达服务器，syncMode 对账真实状态
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
      set({ yolo: mode.yolo, planMode: mode.plan_mode });
    } catch {
      // 静默失败：下次对账时机（对话结束 / 页面可见 / 切项目）重试
    }
  },

  async switchModel(selector) {
    await postJson("/api/models", { selector });
    set({ showModelPicker: false });
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
    // 运行中的请求先中断，避免事件串台到新项目视图。
    // 先断开 store 里的 sse 引用再 abort：sendMessage 的 finally 检测到
    // 非活跃流即跳过清理与重拉（见 sendMessage）
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
      // 忽略持久化失败
    }
    await get().loadSessions();
    get().loadCommands().catch(() => {});
    // 切项目后对账全局模式（模式跨项目保持，由服务器保证一致）
    get().syncMode();
  },

  setWorkdirPicker(show) {
    set({ showWorkdirPicker: show });
    if (show) get().loadWorkdirs().catch(() => {});
  },

  setModelPicker(show) {
    set({ showModelPicker: show });
    if (show) get().loadModels().catch(() => {});
  },

  setEscHint(on) {
    set({ escHint: on });
  },
}));

/** 是否有全局弹窗打开（权限确认 / 提问 / 模型选择 / 添加模型 / 项目选择）。
 *  视图级 Esc 处理需让位于弹窗关闭，避免一次 Esc 同时关弹窗和切换视图 */
export function anyDialogOpen(): boolean {
  const s = useApp.getState();
  return (
    s.permission !== null ||
    s.askUser !== null ||
    s.showAddModel ||
    s.showModelPicker ||
    s.showWorkdirPicker
  );
}
