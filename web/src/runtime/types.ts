// Frontend type definitions corresponding to src/web/protocol.rs

export type ServerEvent =
  | { type: "SessionStarted"; session_id: string }
  | { type: "AgentChunk"; text: string }
  | { type: "AgentReasoningChunk"; text: string }
  | { type: "AgentComplete"; status: string; total_ms: number; model: string }
  | {
      type: "UsageUpdate";
      prompt_tokens: number;
      completion_tokens: number;
      cached_tokens: number;
      context_window: number;
    }
  | {
      type: "ToolCallStart";
      name: string;
      arguments: string;
      subagent?: string;
      /** Index of source task in tasks array (distinguishes same-name concurrent subagents) */
      subagent_index?: number;
    }
  | {
      type: "ToolResult";
      name: string;
      result: string;
      display?: string;
      subagent?: string;
      subagent_index?: number;
    }
  | {
      type: "PermissionRequest";
      request_id: string;
      description: string;
      patterns: string[];
      subagent?: string;
    }
  | {
      type: "AskUser";
      request_id: string;
      questions: QuestionInfo[];
    }
  | { type: "Notice"; text: string }
  | {
      /** An LLM request failed and will auto-retry (frontend drops trailing streamed content and notifies) */
      type: "AgentRetry";
      /** Sequence number of the upcoming retry (1-based) */
      attempt: number;
      /** Max retry count */
      max_retries: number;
      /** Backoff wait before retry (ms) */
      delay_ms: number;
      /** Failure reason summary */
      reason: string;
    }
  | { type: "CompactStart"; text: string }
  | {
      type: "CompactComplete";
      compacted_count: number;
      /** Compaction duration (ms) */
      total_ms: number;
      /** Estimated context tokens after compaction (omitted without usage data; frontend skips context meter update) */
      context_tokens?: number;
    }
  | { type: "Error"; message: string };

export interface QuestionOption {
  label: string;
  description: string;
}

export interface QuestionInfo {
  question: string;
  header: string;
  options: QuestionOption[];
}

export interface SessionInfo {
  id: string;
  title: string;
  model: string;
  updated_at: string;
  work_dir: string;
}

export interface StoredMessage {
  role: "System" | "User" | "Assistant" | "Tool";
  content: string;
  tool_calls: string | null;
  tool_call_id: string | null;
  reasoning_content: string | null;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  cached_tokens: number | null;
  model: string | null;
  runtime_meta: string | null;
  think_ms: number | null;
  compacted: boolean;
  /** Image attachment JSON ([{mime, data_url}]); used by user messages only */
  attachments: string | null;
  /** @ mention metadata JSON ([{start, end, kind}], code point offsets); used by user messages only */
  mentions?: string | null;
}

export interface SessionDetail {
  messages: StoredMessage[];
  compact_summary?: string;
}

export interface WorkdirInfo {
  path: string;
}

export interface ModelInfo {
  provider_id: string;
  provider_name: string;
  model_id: string;
  display: string;
  active: boolean;
  context_window?: number;
  /** Deletable (user-added custom; predefined models cannot be deleted) */
  deletable: boolean;
  /** Whether the provider is predefined (predefined providers cannot be deleted wholesale) */
  provider_predefined: boolean;
  /** Provider has no API key configured (must be set up before use) */
  needs_setup: boolean;
}

/** Configured provider entry (GET /api/providers) */
export interface ProviderOption {
  id: string;
  name: string;
  base_url: string;
  /** Predefined provider (no base_url needed when creating a model) */
  predefined: boolean;
  /** No API key configured (after selecting, only api_key is needed to enable) */
  needs_setup: boolean;
}

/** Add custom model input (POST /api/models/custom); align with TUI AddModelForm */
export interface CreateModelInput {
  provider_id: string;
  base_url?: string;
  api_key?: string;
  model_id: string;
  /** Defaults to 131072 (128K) */
  context_window?: number;
}

/** Slash command entry (GET /api/commands) */
export interface CommandInfo {
  name: string;
  description: string;
  /** "prompt" (expanded into full prompt by backend) | "ui" (handled locally by frontend) */
  kind: "prompt" | "ui";
}

export interface SkillInfoDto {
  name: string;
  description: string;
}

export interface McpServerInfo {
  name: string;
  connected: boolean;
  tools: number;
}

/** Image attachments carried by user messages (uploaded as data URLs; backend validates type/size/empty payload) */
export interface ChatAttachment {
  mime: string;
  data_url: string;
}

/** @ mention metadata: start/end are Unicode code point offsets in content (backend contract `ChatMention` in protocol.rs) */
export interface ChatMention {
  start: number;
  end: number;
  kind: string;
}

export interface ChatRequest {
  message: string;
  attachments?: ChatAttachment[];
  mentions?: ChatMention[];
  session_id?: string;
  work_dir?: string;
}

// ── Usage stats (GET /api/stats)────────────────────────────────

/** Usage summary (assistant rows with usage in the messages table = one LLM request each) */
export interface UsageSummary {
  requests: number;
  prompt_tokens: number;
  cached_tokens: number;
  completion_tokens: number;
}

/** Usage aggregated by day */
export interface DailyUsage {
  /** YYYY-MM-DD (local timezone) */
  date: string;
  requests: number;
  prompt_tokens: number;
  cached_tokens: number;
  completion_tokens: number;
}

/** Usage aggregated by model */
export interface ModelUsage {
  model: string;
  requests: number;
  prompt_tokens: number;
  cached_tokens: number;
  completion_tokens: number;
}

/** Usage details of the most recent request */
export interface UsageRecord {
  created_at: string;
  model: string;
  work_dir: string;
  prompt_tokens: number;
  cached_tokens: number;
  completion_tokens: number;
}

export interface StatsResponse {
  days: number;
  summary: UsageSummary;
  total: UsageSummary;
  daily: DailyUsage[];
  by_model: ModelUsage[];
  recent: UsageRecord[];
}
