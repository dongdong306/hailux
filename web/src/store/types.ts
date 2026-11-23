// State and view-model types: ChatItem render model, skill/MCP entries, AppState contract
import type {
  ChatAttachment,
  ChatMention,
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
  name?: string; // tool name
  arguments?: string;
  result?: string;
  display?: string; // Tool display data (e.g. diff)
  subagent?: string;
  subagentIndex?: number; // Index of source task in tasks array (distinguishes same-name concurrent instances)
  status?: string; // done status
  totalMs?: number;
  model?: string;
  ctxPromptTokens?: number; // Context usage snapshot this run (last request input tokens; carried by done line)
  ctxCompletionTokens?: number; // Context usage snapshot this run (last request output tokens)
  thinkMs?: number; // Thinking duration (final)
  thinkStartedAt?: number; // Thinking timer start (in progress, epoch ms)
  /** Image attachments of the user message (data URLs for inline bubble thumbnails) */
  images?: ChatAttachment[];
  /** @ mention metadata of the user message */
  mentions?: ChatMention[];
}

/** Global mode state (GET /api/mode and yolo/plan-mode/reasoning-effort set response body).
 *  Backend contract: `ModeStatus` in src/web/protocol.rs */
export interface ModeStatus {
  yolo: boolean;
  plan_mode: boolean;
  reasoning_effort: ReasoningEffort;
}

/** Reasoning effort level (top-level reasoning_effort in request body; backend contract ReasoningEffort enum) */
export type ReasoningEffort = "low" | "high" | "max";

/** File entry inside a skill directory */
export interface SkillFileEntry {
  /** Path relative to the skill directory (`/` separated) */
  path: string;
  /** Size in bytes */
  size: number;
}

/** Skill entry (GET /api/skills response structure) */
export interface SkillEntry {
  name: string;
  description: string;
  /** Absolute path of SKILL.md */
  location: string;
  /** "global" (~/.hailux/skills) | "project" (<work_dir>/.hailux/skills) */
  scope: "global" | "project";
  /** SKILL.md body (after frontmatter) */
  content: string;
  /** All files inside the skill directory (including SKILL.md) */
  files: SkillFileEntry[];
}

/** Main area view: chat / skills / MCP / usage stats / settings */
export type ActiveView = "chat" | "skills" | "mcp" | "stats" | "settings";

/** JSON Schema fragment (tool parameter schema / sub-properties) */
export interface JsonSchemaNode {
  type?: string;
  description?: string;
  properties?: Record<string, JsonSchemaNode>;
  required?: string[];
  items?: JsonSchemaNode;
  enum?: unknown[];
  [key: string]: unknown;
}

/** MCP tool entry (name + description + parameter schema) */
export interface McpToolEntry {
  name: string;
  description: string;
  schema?: JsonSchemaNode;
}

/** MCP server entry (GET /api/mcp response structure): config + connection status */
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

/** MCP server form fields (shared by create/update) */
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
  // Sessions
  sessions: SessionInfo[];
  sessionId: string | null;
  workDir: string;
  // Project directory list (for workdir picker)
  workdirs: string[];
  // Message stream
  items: ChatItem[];
  isRunning: boolean;
  /** Current run start timestamp (epoch ms); done line shows backend duration on completion */
  runStartedAt: number | null;
  /** Cumulative input tokens for this session (sum of all LLM requests; shown in top bar) */
  promptTokens: number;
  /** Cumulative output tokens for this session */
  completionTokens: number;
  /** Cumulative cache-hit tokens for this session (shown in top bar) */
  cachedTokens: number;
  /** Input tokens of the last request (≈ current context size; for the context meter below messages) */
  contextPromptTokens: number;
  /** Output tokens of the last request */
  contextCompletionTokens: number;
  /** Model context window size (tokens); 0 = unknown */
  contextWindow: number;
  // Modes
  planMode: boolean;
  yolo: boolean;
  /** Reasoning effort (server-level global, reconciled by syncMode) */
  effort: ReasoningEffort;
  // Input history (↑ at first line goes to previous entry)
  inputHistory: string[];
  // Dialogs
  permission: {
    requestId: string;
    description: string;
    patterns: string[];
    subagent?: string;
  } | null;
  askUser: { requestId: string; questions: QuestionInfo[] } | null;
  // Pickers
  models: ModelInfo[];
  /** Provider list (incl. unconfigured predefined providers; for the add-model form) */
  providers: ProviderOption[];
  /** Add-model dialog; may open with a preset (provider card / needs-setup model entry) */
  showAddModel: boolean;
  addModelPreset: { provider: string; modelId?: string } | null;
  /** Slash command list (refreshed when workDir changes) */
  commands: CommandInfo[];
  showWorkdirPicker: boolean;
  /** Open state of the model + reasoning-effort combo menu (input area pill control) */
  showModelMenu: boolean;
  /** Main area view (skills/MCP are inline panels replacing the chat area) */
  activeView: ActiveView;
  skills: SkillEntry[];
  /** Skill list load failure message (shown in panel + retry) */
  skillsError: string | null;
  mcpServers: McpServerEntry[];
  /** MCP list load failure message (shown in panel + retry) */
  mcpError: string | null;
  // Usage stats
  stats: StatsResponse | null;
  /** Project filter for stats ("" = all projects) */
  statsFilter: string;
  /** Stats window in days */
  statsDays: number;
  statsLoading: boolean;
  statsError: string | null;
  // Interrupt hint (double-Esc semantics)
  escHint: boolean;

  sse: SseSession | null;

  // actions
  /** Startup init: determine initial project dir (last visited > server default) and fetch sessions */
  initWorkDir: () => Promise<void>;
  loadSessions: () => Promise<void>;
  loadModels: () => Promise<void>;
  loadProviders: () => Promise<void>;
  /** Add a custom model and auto-switch to it */
  addModel: (input: CreateModelInput) => Promise<void>;
  /** Delete a custom model (backend auto-migrates when the current model is deleted) */
  deleteModel: (selector: string) => Promise<void>;
  /** Delete a custom provider (entire entry incl. credentials and all its models) */
  deleteProvider: (providerId: string) => Promise<void>;
  loadWorkdirs: () => Promise<void>;
  loadCommands: () => Promise<void>;
  /** Switch main area view; refresh list when entering skills/mcp */
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
  /** Fetch usage stats (statsFilter/statsDays as the current filters) */
  loadStats: () => Promise<void>;
  setStatsFilter: (dir: string) => void;
  setStatsDays: (days: number) => void;
  newSession: (workDir?: string) => Promise<void>;
  switchSession: (id: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  sendMessage: (
    message: string,
    attachments?: ChatAttachment[],
    mentions?: ChatMention[],
  ) => Promise<void>;
  /** Manually compact the current session context (POST /api/compact, SSE response) */
  runCompact: () => Promise<void>;
  interrupt: () => Promise<void>;
  replyPermission: (allow: boolean, always: boolean) => Promise<void>;
  replyAsk: (answer: string) => Promise<void>;
  setPlanMode: (on: boolean) => Promise<void>;
  setYolo: (on: boolean) => Promise<void>;
  /** Set reasoning effort (POST /api/reasoning-effort, rollback on failure + syncMode reconciliation) */
  setEffort: (effort: ReasoningEffort) => Promise<void>;
  /** Reconcile global modes from server (yolo / planMode / effort); fail silently (retry next chance) */
  syncMode: () => Promise<void>;
  switchModel: (selector: string) => Promise<void>;
  /** show=true opens the add-model dialog; preset is the prefill (provider / model id) */
  setAddModel: (show: boolean, preset?: { provider: string; modelId?: string } | null) => void;
  /** Switch project dir: interrupt running request, reset session context, fetch that project's sessions */
  setWorkDir: (dir: string) => Promise<void>;
  setWorkdirPicker: (show: boolean) => void;
  setModelMenu: (show: boolean) => void;
  setEscHint: (on: boolean) => void;
}
