import type {
  ChatAttachment,
  ChatMention,
  CommandInfo,
  CreateModelInput,
  DiscoverModelsInput,
  DiscoverModelsResult,
  ModelInfo,
  ProviderOption,
  QuestionInfo,
  SessionInfo,
  StatsResponse,
  UpdateModelInput,
  UpdateProviderInput,
} from "../runtime/types";

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
  images?: ChatAttachment[];
  mentions?: ChatMention[];
  /** Optimistic interjection bubble: carries the client_id until the
   *  UserMessage echo. While set, the previous turn's streaming output
   *  inserts before this bubble (timeline matches a rebuilt view); removed
   *  on echo or when an InterjectDropped / RunComplete cleanup finds it
   *  never entered the context. */
  pendingInterject?: string;
}

export interface QueuedMessage {
  id: string;
  text: string;
  images?: ChatAttachment[];
  mentions?: ChatMention[];
  createdAt: number;
  /** Delivery in progress (accepted by /api/interject, waiting for the
   *  UserMessage echo or InterjectDropped release). Blocks double-interject,
   *  edit and delete; cleared by the echo (item removed), a drop (item
   *  stays queued) or the RunComplete leftover cleanup. */
  inFlight?: boolean;
}

/** A setting-type slash command deferred while running (only /compact is
 *  staged — it needs a new run; plan/yolo/effort act immediately).
 *  Applied after RunComplete once the queue drains, so it never races the
 *  auto-drain run. Toggles read state at apply time, not enqueue time. */
export interface DeferredSettingAction {
  kind: "plan" | "yolo" | "effort" | "compact";
  value?: ReasoningEffort;
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

export interface SkillFileEntry {
  path: string;
  size: number;
}

/** Skill entry (GET /api/skills response structure) */
export interface SkillEntry {
  name: string;
  description: string;
  location: string;
  /** "global" (~/.hailux/skills) | "project" (<work_dir>/.hailux/skills) */
  scope: "global" | "project";
  content: string;
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
  sessions: SessionInfo[];
  sessionId: string | null;
  workDir: string;
  workdirs: string[];
  items: ChatItem[];
  /** Monotonic timestamp bumped on session switch; the chat viewport watches it to
   *  scroll to bottom (assistant-ui's own auto-scroll hooks don't fire on
   *  external-store switches — items are swapped in place, hasMessages stays true) */
  scrollBottomSignal: number;
  isRunning: boolean;
  /** Current run start timestamp (epoch ms); done line shows backend duration on completion */
  runStartedAt: number | null;
  promptTokens: number;
  completionTokens: number;
  cachedTokens: number;
  contextPromptTokens: number;
  contextCompletionTokens: number;
  contextWindow: number;
  planMode: boolean;
  yolo: boolean;
  /** Reasoning effort (server-level global, reconciled by syncMode) */
  effort: ReasoningEffort;
  inputHistory: string[];
  permission: {
    requestId: string;
    description: string;
    patterns: string[];
    subagent?: string;
  } | null;
  askUser: { requestId: string; questions: QuestionInfo[] } | null;
  models: ModelInfo[];
  providers: ProviderOption[];
  /** Add-model dialog; may open with a preset (provider card / model id prefill) */
  showAddModel: boolean;
  addModelPreset: { provider: string; modelId?: string } | null;
  commands: CommandInfo[];
  showWorkdirPicker: boolean;
  /** Open state of the model + reasoning-effort combo menu (input area pill control) */
  showModelMenu: boolean;
  /** Main area view (skills/MCP are inline panels replacing the chat area) */
  activeView: ActiveView;
  skills: SkillEntry[];
  skillsError: string | null;
  mcpServers: McpServerEntry[];
  mcpError: string | null;
  stats: StatsResponse | null;
  statsFilter: string;
  statsDays: number;
  statsLoading: boolean;
  statsError: string | null;
  escHint: boolean;

  /** Server epoch of the current session's bus (restart detection; null =
   * never subscribed). Owned by stream-manager.ts */
  epoch: number | null;
  /** Consumed event seq cursor of the current session (subscription since
   * baseline) */
  lastSeq: number;
  /** Running state per session (sidebar indicator; visibility of parallel
   * cross-session runs) */
  runningSessions: Record<string, boolean>;
  /** Queued messages per session (running-mode staging; kept across tab
   * switches; the first item auto-drains FIFO after a clean RunComplete) */
  queuedMessages: Record<string, QueuedMessage[]>;
  /** Deferred setting commands per session (only /compact today; applied
   *  after RunComplete, queue drained first to avoid racing isRunning) */
  deferredSettings: Record<string, DeferredSettingAction[]>;
  /** Interjection client_ids with an optimistic bubble awaiting the
   *  UserMessage echo: an echo hit consumes the marker only (bubble already
   *  rendered); RunComplete clears leftovers. */
  pendingInterjectEcho: Record<string, boolean>;

  /** Startup init: determine initial project dir (last visited > server default) and fetch sessions */
  initWorkDir: () => Promise<void>;
  loadSessions: () => Promise<void>;
  loadModels: () => Promise<void>;
  loadProviders: () => Promise<void>;
  addModel: (input: CreateModelInput) => Promise<void>;
  updateModel: (input: UpdateModelInput) => Promise<void>;
  deleteModel: (selector: string) => Promise<void>;
  updateProvider: (input: UpdateProviderInput) => Promise<void>;
  deleteProvider: (providerId: string) => Promise<void>;
  /** Add the checked models of a fetched list to a provider's model table
   * (returns the ids that were actually added) */
  applyDiscoveredModels: (providerId: string, modelIds: string[]) => Promise<string[]>;
  /** Fetch a model list: with unsaved credentials (add-model dialog) or through the saved
   * credentials of a configured provider (fetch dialog); cached server-side */
  discoverModels: (input: DiscoverModelsInput) => Promise<DiscoverModelsResult>;
  loadWorkdirs: () => Promise<void>;
  loadCommands: () => Promise<void>;
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
  loadStats: () => Promise<void>;
  setStatsFilter: (dir: string) => void;
  setStatsDays: (days: number) => void;
  newSession: (workDir?: string) => Promise<void>;
  switchSession: (id: string) => Promise<void>;
  deleteSession: (id: string) => Promise<void>;
  /** Send a message and start a run; false = submit failed (409/429/network,
   *  error item already in items). Queue drain relies on this to re-enqueue. */
  sendMessage: (
    message: string,
    attachments?: ChatAttachment[],
    mentions?: ChatMention[],
  ) => Promise<boolean>;
  runCompact: () => Promise<void>;
  interrupt: () => Promise<void>;
  enqueueMessage: (text: string, images?: ChatAttachment[], mentions?: ChatMention[]) => void;
  removeQueuedMessage: (sessionId: string, id: string) => void;
  removeQueuedByClientId: (sessionId: string, clientId: string) => void;
  /** Set/clear the in-flight marker on a queued item (view-independent
   *  bookkeeping; the run-sink calls it for InterjectDropped) */
  setQueuedInFlight: (sessionId: string, id: string, inFlight: boolean) => void;
  /** Interject one queued message: POST /api/interject, optimistic bubble +
   *  in-flight marker on success (the item stays queued until the echo);
   *  409 (run just ended) falls back to a normal send with the auto-drain
   *  chain. The optimistic bubble is only appended while the target session
   *  is still the current view. */
  interjectQueued: (sessionId: string, item: QueuedMessage) => Promise<void>;
  interjectAllQueued: (sessionId: string) => Promise<void>;
  /** Pop the first queued item and send it as a normal message (RunComplete
   *  auto-drain; also the reconciliation when switching back to a session
   *  that finished while unobserved). Submit failure requeues the head. */
  drainQueuedHead: (sessionId: string) => Promise<void>;
  /** Append a notice/error into the timeline at the turn boundary (before
   *  trailing pending-interject bubbles, same rule as the run-sink push) */
  pushTurnItem: (item: ChatItem) => void;
  deferSetting: (sessionId: string, action: DeferredSettingAction) => void;
  applySettingAction: (action: DeferredSettingAction) => void;
  /** Drain deferred settings after RunComplete: instant ones apply now;
   *  compact only when the queue is empty */
  drainDeferredSettings: (sessionId: string, queueEmpty: boolean) => void;
  replyPermission: (allow: boolean, always: boolean) => Promise<void>;
  replyAsk: (answer: string) => Promise<void>;
  setPlanMode: (on: boolean) => Promise<void>;
  setYolo: (on: boolean) => Promise<void>;
  /** Set reasoning effort (POST /api/reasoning-effort, rollback on failure + syncMode reconciliation) */
  setEffort: (effort: ReasoningEffort) => Promise<void>;
  /** Reconcile global modes from server (yolo / planMode / effort); fail silently (retry next chance) */
  syncMode: () => Promise<void>;
  switchModel: (selector: string) => Promise<void>;
  setAddModel: (show: boolean, preset?: { provider: string; modelId?: string } | null) => void;
  /** Switch project dir: keep running tasks alive (session-routed events), reset view state, fetch that project's sessions */
  setWorkDir: (dir: string) => Promise<void>;
  setWorkdirPicker: (show: boolean) => void;
  setModelMenu: (show: boolean) => void;
  setEscHint: (on: boolean) => void;
}
