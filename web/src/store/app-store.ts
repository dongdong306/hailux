// 应用状态：会话/消息/权限请求/模式（Zustand）
import { create } from "zustand";
import type {
  ChatRequest,
  CommandInfo,
  ModelInfo,
  QuestionInfo,
  ServerEvent,
  SessionInfo,
} from "../runtime/types";
import { getJson, postJson, postSse, type SseSession } from "../runtime/sse-client";

export interface ChatItem {
  kind:
    | "user"
    | "assistant"
    | "assistant-streaming"
    | "reasoning"
    | "reasoning-streaming"
    | "tool-call"
    | "tool-result"
    | "notice"
    | "compact-marker"
    | "done"
    | "error";
  text?: string;
  name?: string; // tool 名称
  arguments?: string;
  result?: string;
  display?: string; // 工具展示数据（如 diff）
  subagent?: string;
  status?: string; // done 状态
  totalMs?: number;
  model?: string;
  thinkMs?: number; // 思考耗时（最终）
  thinkStartedAt?: number; // 思考计时起点（进行中，epoch ms）
}

/** localStorage：上次访问的项目目录 key */
const LAST_WORKDIR_KEY = "hailux.lastWorkDir";

/** 技能目录内文件条目 */
export interface SkillFileEntry {
  /** 相对技能目录的路径（`/` 分隔） */
  path: string;
  /** 字节大小 */
  size: number;
}

/** 技能条目（GET /api/skills 返回结构） */
export interface SkillEntry {
  name: string;
  description: string;
  /** SKILL.md 绝对路径 */
  location: string;
  /** "global"（~/.hailux/skills）| "project"（<work_dir>/.hailux/skills） */
  scope: "global" | "project";
  /** SKILL.md 正文（frontmatter 之后） */
  content: string;
  /** 技能目录内全部文件（含 SKILL.md） */
  files: SkillFileEntry[];
}

/** 主区域视图：聊天 / 技能管理 / MCP 管理 */
export type ActiveView = "chat" | "skills" | "mcp";

/** JSON Schema 片段（工具参数 schema / 子属性） */
export interface JsonSchemaNode {
  type?: string;
  description?: string;
  properties?: Record<string, JsonSchemaNode>;
  required?: string[];
  items?: JsonSchemaNode;
  enum?: unknown[];
  [key: string]: unknown;
}

/** MCP 工具条目（名称 + 描述 + 参数 schema） */
export interface McpToolEntry {
  name: string;
  description: string;
  schema?: JsonSchemaNode;
}

/** MCP 服务器条目（GET /api/mcp 返回结构）：配置 + 连接状态 */
export interface McpServerEntry {
  name: string;
  connected: boolean;
  tools: number;
  /** "stdio" | "http" */
  transport: string;
  command?: string;
  args: string[];
  env: Record<string, string>;
  url?: string;
  headers: Record<string, string>;
  error?: string;
  tool_details: McpToolEntry[];
}

/** MCP 服务器表单字段（创建/更新共用） */
export interface McpServerInput {
  name: string;
  transport: "stdio" | "http";
  command?: string;
  args: string[];
  env: Record<string, string>;
  url?: string;
  headers: Record<string, string>;
}

/** 从错误响应提取可读文本 */
async function readError(resp: Response): Promise<string> {
  const text = await resp.text().catch(() => "");
  return text || `HTTP ${resp.status}`;
}

interface AppState {
  // 会话
  sessions: SessionInfo[];
  sessionId: string | null;
  workDir: string;
  // 项目目录列表（workdir picker 用）
  workdirs: string[];
  // 消息流
  items: ChatItem[];
  isRunning: boolean;
  /** 本轮请求计时起点（epoch ms），完成后由 done 行展示后端耗时 */
  runStartedAt: number | null;
  promptTokens: number;
  completionTokens: number;
  /** 模型上下文窗口大小（token）；0 = 未知 */
  contextWindow: number;
  // 模式
  planMode: boolean;
  yolo: boolean;
  // 输入历史（光标在首行 ↑ 上一条）
  inputHistory: string[];
  // 弹窗
  permission: {
    requestId: string;
    description: string;
    patterns: string[];
    subagent?: string;
  } | null;
  askUser: { requestId: string; questions: QuestionInfo[] } | null;
  // 选择器
  models: ModelInfo[];
  /** 斜杠命令列表（随 workDir 变化刷新） */
  commands: CommandInfo[];
  showWorkdirPicker: boolean;
  showModelPicker: boolean;
  /** 主区域视图（技能/MCP 为内联面板，替换聊天区） */
  activeView: ActiveView;
  skills: SkillEntry[];
  /** 技能列表加载失败信息（面板内展示 + 重试） */
  skillsError: string | null;
  mcpServers: McpServerEntry[];
  /** MCP 列表加载失败信息（面板内展示 + 重试） */
  mcpError: string | null;
  // 中断提示（双击 Esc 语义）
  escHint: boolean;

  sse: SseSession | null;

  // actions
  /** 启动初始化：确定初始项目目录（上次访问 > 服务器默认目录）并拉取会话 */
  initWorkDir: () => Promise<void>;
  loadSessions: () => Promise<void>;
  loadModels: () => Promise<void>;
  loadWorkdirs: () => Promise<void>;
  loadCommands: () => Promise<void>;
  /** 切换主区域视图；skills/mcp 进入时刷新列表 */
  setView: (view: ActiveView) => void;
  reloadSkills: () => Promise<void>;
  createSkill: (input: {
    name: string;
    description: string;
    content: string;
    scope: "global" | "project";
  }) => Promise<void>;
  updateSkill: (input: {
    location: string;
    name: string;
    description: string;
    content: string;
  }) => Promise<void>;
  deleteSkill: (location: string) => Promise<void>;
  reloadMcp: () => Promise<void>;
  createMcpServer: (input: McpServerInput) => Promise<void>;
  updateMcpServer: (name: string, input: McpServerInput) => Promise<void>;
  deleteMcpServer: (name: string) => Promise<void>;
  newSession: (workDir?: string) => Promise<void>;
  switchSession: (id: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  sendMessage: (message: string) => Promise<void>;
  /** 手动压缩当前会话上下文（POST /api/compact，SSE 响应） */
  runCompact: () => Promise<void>;
  interrupt: () => Promise<void>;
  replyPermission: (allow: boolean, always: boolean) => Promise<void>;
  replyAsk: (answer: string) => Promise<void>;
  setPlanMode: (on: boolean) => Promise<void>;
  setYolo: (on: boolean) => Promise<void>;
  switchModel: (selector: string) => Promise<void>;
  /** 切换项目目录：中断运行中的请求，重置会话上下文并拉取该项目会话列表 */
  setWorkDir: (dir: string) => Promise<void>;
  setWorkdirPicker: (show: boolean) => void;
  setModelPicker: (show: boolean) => void;
  setEscHint: (on: boolean) => void;
}

export const useApp = create<AppState>((set, get) => ({
  sessions: [],
  sessionId: null,
  workDir: "",
  items: [],
  isRunning: false,
  runStartedAt: null,
  promptTokens: 0,
  completionTokens: 0,
  contextWindow: 0,
  planMode: false,
  yolo: false,
  inputHistory: [],
  permission: null,
  askUser: null,
  models: [],
  commands: [],
  workdirs: [],
  showWorkdirPicker: false,
  showModelPicker: false,
  activeView: "chat",
  skills: [],
  skillsError: null,
  mcpServers: [],
  mcpError: null,
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

  /** 切换主区域视图；进入 skills/mcp 时刷新对应列表（错误写入 xxxError，不阻止切换） */
  setView(view) {
    set({ activeView: view });
    if (view === "skills") get().reloadSkills().catch(() => {});
    if (view === "mcp") get().reloadMcp().catch(() => {});
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
    const items: ChatItem[] = [];
    if (detail.compact_summary) {
      items.push({ kind: "compact-marker", text: detail.compact_summary });
    }
    // Tool 消息的 runtime_meta 仅识别新格式 diff 展示数据（format: "diff"）；
    // 旧格式 {old,new} 存量数据显示 result 文本，计时 JSON（无 format 字段）同样排除
    const toolResults = new Map<string, { result: string; display?: string }>();
    for (const msg of detail.messages) {
      if (msg.role === "Tool" && msg.tool_call_id) {
        let display: string | undefined;
        if (msg.runtime_meta) {
          try {
            const v = JSON.parse(msg.runtime_meta) as { format?: string };
            if (v.format === "diff") {
              display = msg.runtime_meta;
            }
          } catch {
            // 非 JSON，不作为 diff 展示
          }
        }
        toolResults.set(msg.tool_call_id, { result: msg.content, display });
      }
    }
    for (const msg of detail.messages) {
      if (msg.role === "User") {
        items.push({ kind: "user", text: msg.content });
      } else if (msg.role === "Assistant") {
        if (msg.reasoning_content) {
          items.push({
            kind: "reasoning",
            text: msg.reasoning_content,
            thinkMs: msg.think_ms ?? undefined,
          });
        }
        if (msg.content) items.push({ kind: "assistant", text: msg.content });
        if (msg.tool_calls) {
          try {
            const calls = JSON.parse(msg.tool_calls) as {
              id: string;
              function?: { name: string; arguments?: string };
            }[];
            for (const call of calls) {
              items.push({
                kind: "tool-call",
                name: call.function?.name ?? "",
                arguments: call.function?.arguments ?? "",
              });
              const tr = toolResults.get(call.id);
              if (tr !== undefined) {
                items.push({
                  kind: "tool-result",
                  name: call.function?.name ?? "",
                  result: tr.result,
                  display: tr.display,
                });
              }
            }
          } catch {
            // 忽略解析失败
          }
        }
        // runtime_meta（含本轮耗时/模型/状态）→ 完成标记行
        if (msg.runtime_meta) {
          try {
            const meta = JSON.parse(msg.runtime_meta) as {
              total_ms?: number;
              model?: string;
              status?: string;
            };
            if (typeof meta.total_ms === "number") {
              items.push({
                kind: "done",
                status: meta.status ?? "completed",
                totalMs: meta.total_ms,
                model: meta.model ?? "",
              });
            }
          } catch {
            // 忽略解析失败
          }
        }
      }
    }
    // 会话携带其所属工作目录 —— 切换会话即切换目录上下文
    const target = get().sessions.find((s) => s.id === id);
    // 从历史 usage 恢复上下文用量（最后一条带 usage 的消息）
    let promptTokens = 0;
    let completionTokens = 0;
    for (let i = detail.messages.length - 1; i >= 0; i--) {
      const m = detail.messages[i]!;
      if (m.prompt_tokens !== null) {
        promptTokens = m.prompt_tokens;
        completionTokens = m.completion_tokens ?? 0;
        break;
      }
    }
    set({
      sessionId: id,
      items,
      isRunning: false,
      runStartedAt: null,
      workDir: target?.work_dir ?? get().workDir,
      promptTokens,
      completionTokens,
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
      });
    }
    await get().loadSessions();
  },

  async sendMessage(message) {
    if (get().isRunning) return;
    const req: ChatRequest = {
      message,
      session_id: get().sessionId ?? undefined,
      work_dir: get().workDir || undefined,
    };

    // 本地立刻显示用户消息 + 记录输入历史 + 启动本地请求计时
    // （完成后由 done 行展示后端返回的 total_ms）
    set((s) => ({
      isRunning: true,
      runStartedAt: Date.now(),
      inputHistory: [...s.inputHistory.filter((h) => h !== message), message].slice(-100),
      items: [...s.items, { kind: "user", text: message }],
    }));

    const handleEvent = (event: ServerEvent) => {
      const items = get().items;
      const push = (item: ChatItem) => set((s) => ({ items: [...s.items, item] }));
      const patchLast = (patch: (item: ChatItem) => ChatItem) => {
        if (items.length === 0) return;
        set((s) => ({
          items: [
            ...s.items.slice(0, -1),
            patch(s.items[s.items.length - 1]!),
          ],
        }));
      };
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
        case "ToolCallStart":
          freezeThinking();
          push({
            kind: "tool-call",
            name: event.name,
            arguments: event.arguments,
            subagent: event.subagent,
          });
          break;
        case "ToolResult":
          push({
            kind: "tool-result",
            name: event.name,
            result: event.result,
            display: event.display,
            subagent: event.subagent,
          });
          break;
        case "UsageUpdate":
          set({
            promptTokens: event.prompt_tokens,
            completionTokens: event.completion_tokens,
            contextWindow: event.context_window || get().contextWindow,
          });
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
        case "Notice":
          push({ kind: "notice", text: event.text });
          break;
        case "CompactChunk":
          push({ kind: "notice", text: event.text });
          break;
        case "CompactComplete":
          push({
            kind: "compact-marker",
            text: `已压缩 ${event.compacted_count} 条消息`,
          });
          break;
        case "AgentComplete": {
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
          });
          break;
        }
        case "Error":
          push({ kind: "error", text: event.message });
          break;
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
      switch (event.type) {
        case "Notice":
        case "CompactChunk":
          set((s) => ({
            items: [...s.items, { kind: "notice", text: event.text }],
          }));
          break;
        case "CompactComplete":
          set((s) => ({
            items: [
              ...s.items,
              { kind: "compact-marker", text: `已压缩 ${event.compacted_count} 条消息` },
            ],
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
            askUser: { requestId: event.request_id, questions: event.questions },
          });
          break;
        case "Error":
          set((s) => ({
            items: [...s.items, { kind: "error", text: event.message }],
          }));
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
    await postJson("/api/plan-mode", {
      enabled: on,
      work_dir: get().workDir || undefined,
    });
  },

  async setYolo(on) {
    set({ yolo: on });
    await postJson("/api/yolo", {
      enabled: on,
      work_dir: get().workDir || undefined,
    });
  },

  async switchModel(selector) {
    await postJson("/api/models", { selector });
    set({ showModelPicker: false });
    await get().loadModels();
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
      showWorkdirPicker: false,
    });
    try {
      localStorage.setItem(LAST_WORKDIR_KEY, dir);
    } catch {
      // 忽略持久化失败
    }
    await get().loadSessions();
    get().loadCommands().catch(() => {});
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
