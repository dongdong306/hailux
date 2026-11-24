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
      /** Mid-run interjection entered the agent context at a loop
       * boundary. The submitting tab removes its queued item by client_id
       * (echo confirmation) and renders the user bubble. */
      type: "UserMessage";
      client_id: string;
      content: string;
      attachments?: ChatAttachment[];
      mentions?: ChatMention[];
    }
  | {
      /** An accepted interjection was discarded without entering the agent
       * context (run interrupted/errored before the injection boundary).
       * Transient like Error: never persisted. The frontend releases the
       * in-flight queued item (stays queued for a manual resend) and removes
       * the optimistic bubble. */
      type: "InterjectDropped";
      client_id: string;
    }
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
  | { type: "Error"; message: string }
  | {
      /** Subscribe first frame (first SSE frame only, not envelope-wrapped):
       * the attach-time replay snapshot */
      type: "Snapshot";
      /** Server start timestamp (secs); mismatch vs. the client = server
       * restarted, seq space invalidated */
      epoch: number;
      /** Whether a run is active at attach time */
      running: boolean;
      /** Buffered events with seq > since */
      events: Envelope[];
    }
  | {
      /** Session run-end sentinel (last event in an envelope; observer
       * streams close on it) */
      type: "RunComplete";
      /** The runner exited abnormally (observers should refresh and resync) */
      abnormal: boolean;
    };

/** SSE event envelope (protocol.rs::Envelope): every event carries a
 * per-session monotonic seq (continuous across runs); reconnecting clients
 * catch up with a `since` cursor and dedupe by seq */
export interface Envelope {
  seq: number;
  event: ServerEvent;
}

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
  /** Whether a run is active (sidebar indicator) */
  running?: boolean;
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
  /** Server start timestamp (secs): mismatch vs. the client = server
   * restarted, cursors invalidated */
  epoch: number;
  /** durable watermark: history already covers all events <= head_seq;
   * subscription replay starts after it */
  head_seq: number;
  /** Whether a run is active */
  running: boolean;
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
  /** Output token cap (max_completion_tokens); absent when the model can't be resolved */
  max_tokens?: number;
  /** Whether the model accepts image input (effective value) */
  supports_vision?: boolean;
  /** Whether the provider is predefined (predefined providers cannot be deleted wholesale) */
  provider_predefined: boolean;
}

/** Provider entry (GET /api/providers): configured ones first, unconfigured predefined after */
export interface ProviderOption {
  id: string;
  name: string;
  base_url: string;
  /** Predefined provider (no base_url needed when creating a model) */
  predefined: boolean;
  /** No API key configured (after selecting, only api_key is needed to enable) */
  needs_setup: boolean;
}

/** One model as reported by the provider's `/models` endpoint */
export interface DiscoveredModel {
  id: string;
  name?: string;
  context_window?: number;
  max_tokens?: number;
  supports_vision?: boolean;
  /** Already in the provider's model table (checkbox disabled) */
  already_added?: boolean;
}

/** Fetch a model list (POST /api/models/discover). With api_key the not-yet-saved credentials
 * of the add-model dialog apply; without one the saved credentials of provider_id are used.
 * `provider_id` is the cache key (a predefined id, or the custom provider's future name). */
export interface DiscoverModelsInput {
  provider_id?: string;
  base_url?: string;
  api_key?: string;
}

export interface DiscoverModelsResult {
  provider_id: string;
  models: DiscoveredModel[];
}

/** Result of adding the checked models to a provider's model table
 * (POST /api/models/apply-discovered) */
export interface ApplyDiscoveredResult {
  /** Ids that were actually added (already-present ones are skipped) */
  added: string[];
}

/** Add model input (POST /api/models/custom); align with the TUI setup flow */
export interface CreateModelInput {
  provider_id: string;
  base_url?: string;
  api_key?: string;
  model_id: string;
  /** Absent = take the cached `/models` metadata when present, otherwise 262144 (256K) */
  context_window?: number;
}

/** Edit an existing model (PUT /api/models/custom); omitted fields keep their effective values */
export interface UpdateModelInput {
  /** Existing selector provider/model */
  selector: string;
  /** New model id (rename) */
  model_id?: string;
  context_window?: number;
  max_tokens?: number;
  supports_vision?: boolean;
}

/** Edit an existing provider (PUT /api/providers); omitted fields keep their current values */
export interface UpdateProviderInput {
  provider_id: string;
  /** New provider id (rename); predefined providers cannot be renamed */
  new_id?: string;
  /** New API key (absent = keep current) */
  api_key?: string;
  /** New endpoint; empty string clears the override (predefined providers only) */
  base_url?: string;
}

/** Slash command entry (GET /api/commands) */
export interface CommandInfo {
  name: string;
  description: string;
  /** "prompt" (queued/sent, expanded into full prompt by backend)
   *  | "view" (pure UI action — open a panel/menu, immediate in any state)
   *  | "setting" (mode/compact toggle — immediate when idle, deferred to run end while running) */
  kind: "prompt" | "view" | "setting";
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

/** Mid-run interjection (POST /api/interject); requires an active chat run
 * for the session (409 otherwise). The message enters the agent context at
 * the next loop boundary and is echoed back as a `UserMessage` event. */
export interface InterjectRequest {
  session_id: string;
  /** Client-generated id; echoed on UserMessage for queue-item removal */
  client_id: string;
  message: string;
  attachments?: ChatAttachment[];
  mentions?: ChatMention[];
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
