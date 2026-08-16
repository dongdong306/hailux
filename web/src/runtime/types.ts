// 对应 src/web/protocol.rs 的前端类型定义

export type ServerEvent =
  | { type: "AgentChunk"; text: string }
  | { type: "AgentReasoningChunk"; text: string }
  | { type: "AgentComplete"; status: string; total_ms: number; model: string }
  | {
      type: "UsageUpdate";
      prompt_tokens: number;
      completion_tokens: number;
      context_window: number;
    }
  | {
      type: "ToolCallStart";
      name: string;
      arguments: string;
      subagent?: string;
    }
  | {
      type: "ToolResult";
      name: string;
      result: string;
      display?: string;
      subagent?: string;
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

export interface FsEntry {
  name: string;
  path: string;
  is_dir: boolean;
}

export interface ModelInfo {
  provider_id: string;
  provider_name: string;
  model_id: string;
  display: string;
  active: boolean;
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
