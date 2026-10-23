// 状态与视图模型类型：ChatItem 渲染模型、技能/MCP 条目、AppState 契约
import type {
  ChatAttachment,
  CommandInfo,
  CreateModelInput,
  ModelInfo,
  ProviderOption,
  QuestionInfo,
  SessionInfo,
  StatsResponse,
} from "../runtime/types";
import type { SseSession } from "../runtime/sse-client";

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
    | "compacting"
    | "done"
    | "error";
  text?: string;
  name?: string; // tool 名称
  arguments?: string;
  result?: string;
  display?: string; // 工具展示数据（如 diff）
  subagent?: string;
  subagentIndex?: number; // 来源任务在 tasks 数组中的下标（同名并发区分实例）
  status?: string; // done 状态
  totalMs?: number;
  model?: string;
  ctxPromptTokens?: number; // 本轮上下文占用快照（最后一次请求输入 token；done 行携带）
  ctxCompletionTokens?: number; // 本轮上下文占用快照（最后一次请求输出 token）
  thinkMs?: number; // 思考耗时（最终）
  thinkStartedAt?: number; // 思考计时起点（进行中，epoch ms）
  /** 用户消息的图片附件（data URL，用于气泡内缩略图） */
  images?: ChatAttachment[];
}

/** 全局模式状态（GET /api/mode 及 yolo/plan-mode 设置响应体）。
 *  后端契约：src/web/protocol.rs 的 `ModeStatus` */
export interface ModeStatus {
  yolo: boolean;
  plan_mode: boolean;
}

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

/** 主区域视图：聊天 / 技能管理 / MCP 管理 / 用量统计 / 设置 */
export type ActiveView = "chat" | "skills" | "mcp" | "stats" | "settings";

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

export interface AppState {
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
  /** 本会话累计输入 token（当前 session 全部 LLM 请求之和；顶栏展示） */
  promptTokens: number;
  /** 本会话累计输出 token */
  completionTokens: number;
  /** 本会话累计缓存命中 token（顶栏展示） */
  cachedTokens: number;
  /** 最后一次请求的输入 token（≈ 当前上下文大小；消息底部上下文计量用） */
  contextPromptTokens: number;
  /** 最后一次请求的输出 token */
  contextCompletionTokens: number;
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
  /** provider 列表（含未配置的预定义 provider；添加模型表单用） */
  providers: ProviderOption[];
  /** 添加模型弹窗；打开时可带预设（provider 卡片 / 需配置模型入口） */
  showAddModel: boolean;
  addModelPreset: { provider: string; modelId?: string } | null;
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
  // 用量统计
  stats: StatsResponse | null;
  /** 统计的项目筛选（"" = 全部项目） */
  statsFilter: string;
  /** 统计窗口天数 */
  statsDays: number;
  statsLoading: boolean;
  statsError: string | null;
  // 中断提示（双击 Esc 语义）
  escHint: boolean;

  sse: SseSession | null;

  // actions
  /** 启动初始化：确定初始项目目录（上次访问 > 服务器默认目录）并拉取会话 */
  initWorkDir: () => Promise<void>;
  loadSessions: () => Promise<void>;
  loadModels: () => Promise<void>;
  loadProviders: () => Promise<void>;
  /** 添加自定义模型并自动切换到新模型 */
  addModel: (input: CreateModelInput) => Promise<void>;
  /** 删除自定义模型（删除当前模型时后端自动迁移） */
  deleteModel: (selector: string) => Promise<void>;
  /** 删除自定义 provider（整条含凭据与全部模型） */
  deleteProvider: (providerId: string) => Promise<void>;
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
  /** 拉取用量统计（statsFilter/statsDays 为当前筛选） */
  loadStats: () => Promise<void>;
  setStatsFilter: (dir: string) => void;
  setStatsDays: (days: number) => void;
  newSession: (workDir?: string) => Promise<void>;
  switchSession: (id: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  sendMessage: (message: string, attachments?: ChatAttachment[]) => Promise<void>;
  /** 手动压缩当前会话上下文（POST /api/compact，SSE 响应） */
  runCompact: () => Promise<void>;
  interrupt: () => Promise<void>;
  replyPermission: (allow: boolean, always: boolean) => Promise<void>;
  replyAsk: (answer: string) => Promise<void>;
  setPlanMode: (on: boolean) => Promise<void>;
  setYolo: (on: boolean) => Promise<void>;
  /** 从服务器对账全局模式（yolo / planMode），失败静默（下次时机重试） */
  syncMode: () => Promise<void>;
  switchModel: (selector: string) => Promise<void>;
  /** show=true 打开添加模型弹窗；preset 为预填项（provider / 模型 id） */
  setAddModel: (show: boolean, preset?: { provider: string; modelId?: string } | null) => void;
  /** 切换项目目录：中断运行中的请求，重置会话上下文并拉取该项目会话列表 */
  setWorkDir: (dir: string) => Promise<void>;
  setWorkdirPicker: (show: boolean) => void;
  setModelPicker: (show: boolean) => void;
  setEscHint: (on: boolean) => void;
}
