// 对应 src/web/protocol.rs 的前端类型定义

export type ServerEvent =
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
      /** 来源任务在 tasks 数组中的下标（同名 subagent 并发时区分实例） */
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
  | { type: "CompactChunk"; text: string }
  | { type: "CompactComplete"; summary_chars: number; compacted_count: number }
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
  runtime_meta: string | null;
  think_ms: number | null;
  compacted: boolean;
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
  /** 可删除（用户自定义添加；预定义模型不可删） */
  deletable: boolean;
  /** provider 是否为预定义（预定义 provider 不可整条删除） */
  provider_predefined: boolean;
  /** provider 未配置 API Key（需 setup 后才可使用） */
  needs_setup: boolean;
}

/** 已配置 provider 条目（GET /api/providers） */
export interface ProviderOption {
  id: string;
  name: string;
  base_url: string;
  /** 预定义 provider（新建模型时无需填 base_url） */
  predefined: boolean;
  /** 未配置 API Key（选择后仅需填写 api_key 即可启用） */
  needs_setup: boolean;
}

/** 添加自定义模型入参（POST /api/models/custom）；对齐 TUI AddModelForm */
export interface CreateModelInput {
  provider_id: string;
  base_url?: string;
  api_key?: string;
  model_id: string;
  /** 缺省 131072 (128K) */
  context_window?: number;
}

/** 斜杠命令条目（GET /api/commands） */
export interface CommandInfo {
  name: string;
  description: string;
  /** "prompt"（后端展开为完整提示词）| "ui"（前端本地处理） */
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

export interface ChatRequest {
  message: string;
  session_id?: string;
  work_dir?: string;
}

// ── 用量统计（GET /api/stats）────────────────────────────────

/** 用量汇总（基于 messages 表中带 usage 的 assistant 行 = 一次 LLM 请求） */
export interface UsageSummary {
  requests: number;
  prompt_tokens: number;
  cached_tokens: number;
  completion_tokens: number;
}

/** 按日聚合的用量 */
export interface DailyUsage {
  /** YYYY-MM-DD（本地时区） */
  date: string;
  requests: number;
  prompt_tokens: number;
  cached_tokens: number;
  completion_tokens: number;
}

/** 按模型聚合的用量 */
export interface ModelUsage {
  model: string;
  requests: number;
  prompt_tokens: number;
  cached_tokens: number;
  completion_tokens: number;
}

/** 最近一次请求的用量明细 */
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
