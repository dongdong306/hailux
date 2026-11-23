//! Web frontend/backend JSON protocol (frontend contract).
//!
//! `ServerEvent` = SSE push events (backend → frontend), with `#[serde(tag = "type")]`;
//! Request body structs correspond to the REST endpoints.

use serde::{Deserialize, Serialize};

use crate::agent::event::{QuestionInfo, TaskStatus};

// ── SSE push events (backend → frontend) ───────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    /// Effective session ID of this turn. Emitted when the request omits
    /// session_id and the backend creates/reuses one, so the frontend can
    /// sync its session state.
    SessionStarted {
        session_id: String,
    },
    AgentChunk {
        text: String,
    },
    AgentReasoningChunk {
        text: String,
    },
    AgentComplete {
        status: String,
        total_ms: u64,
        model: String,
    },
    UsageUpdate {
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: u32,
        /// Model context window size (lets the frontend display context usage ratio)
        context_window: u32,
    },
    ToolCallStart {
        name: String,
        arguments: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        subagent: Option<String>,
        /// Index of the source task in the tasks array (distinguishes instances when same-named subagents run concurrently)
        #[serde(skip_serializing_if = "Option::is_none")]
        subagent_index: Option<usize>,
    },
    ToolResult {
        name: String,
        result: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subagent: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subagent_index: Option<usize>,
    },
    PermissionRequest {
        request_id: String,
        description: String,
        patterns: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subagent: Option<String>,
    },
    AskUser {
        request_id: String,
        questions: Vec<QuestionInfo>,
    },
    /// Informational notice within the session
    Notice {
        text: String,
    },
    /// An LLM request failed and is about to be retried automatically.
    /// Frontend semantics: discard the trailing streamed content (partial output from the failed attempt
    /// does not enter the context) and show a retry notice; after a successful retry, new content streams from scratch.
    AgentRetry {
        /// 1-based sequence number of the upcoming retry
        attempt: u32,
        /// Maximum retry count
        max_retries: u32,
        /// Backoff wait before retry (ms)
        delay_ms: u64,
        /// Summary of the failure reason
        reason: String,
    },
    /// Compaction started (maps to the TUI CompactStreaming status line)
    CompactStart {
        text: String,
    },
    CompactComplete {
        compacted_count: usize,
        /// Compaction duration (ms)
        total_ms: u64,
        /// Estimated context tokens after compaction (align with TUI set_session_usage(estimated, 0);
        /// None when no usage data, in which case the frontend skips the context usage update)
        #[serde(skip_serializing_if = "Option::is_none")]
        context_tokens: Option<u32>,
    },
    Error {
        message: String,
    },
}

pub fn status_str(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Completed => "completed",
        TaskStatus::Interrupted => "interrupted",
        TaskStatus::Error => "error",
    }
}

// ── Usage stats (GET /api/stats) ────────────────────────────────

/// Usage stats response: in-window summary + all-time summary + per-dimension aggregation.
/// Reuses the storage layer's Serialize structs (UsageSummary/DailyUsage/ModelUsage/UsageRecord).
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    /// Stats window (days; 0 = all)
    pub days: u32,
    /// In-window summary
    pub summary: crate::storage::UsageSummary,
    /// All-time summary (not limited by days)
    pub total: crate::storage::UsageSummary,
    pub daily: Vec<crate::storage::DailyUsage>,
    pub by_model: Vec<crate::storage::ModelUsage>,
    pub recent: Vec<crate::storage::UsageRecord>,
}

// ── Request bodies (frontend → backend) ──────────────────────────

/// Image attachments carried by the user message (uploaded by the frontend as data URLs).
#[derive(Debug, Deserialize)]
pub struct ChatAttachment {
    pub mime: String,
    /// Full data URL in the form `data:<mime>;base64,<data>`
    pub data_url: String,
}

/// @ mention metadata: `start`/`end` are Unicode code point offsets into content
#[derive(Debug, Deserialize, Serialize)]
pub struct ChatMention {
    /// Start code point offset (token includes the `@` prefix)
    pub start: usize,
    /// End code point offset (exclusive)
    pub end: usize,
    pub kind: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    #[serde(default)]
    pub mentions: Vec<ChatMention>,
    pub session_id: Option<String>,
    pub work_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub work_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PermissionReplyBody {
    pub allow: bool,
    #[serde(default)]
    pub always: bool,
}

#[derive(Debug, Deserialize)]
pub struct AskReplyBody {
    pub answer: String,
}

#[derive(Debug, Deserialize)]
pub struct InterruptRequest {
    pub session_id: Option<String>,
    pub work_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CompactRequest {
    pub session_id: String,
    pub work_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PlanModeRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct YoloRequest {
    pub enabled: bool,
}

/// Global mode status (response body of `GET /api/mode` and of the yolo/plan-mode/reasoning-effort setters).
/// Frontend contract: the `ModeStatus` interface in `web/src/runtime/types.ts`.
#[derive(Debug, Serialize)]
pub struct ModeStatus {
    pub yolo: bool,
    pub plan_mode: bool,
    pub reasoning_effort: crate::agent::ReasoningEffort,
}

/// Set reasoning effort (POST /api/reasoning-effort)
#[derive(Debug, Deserialize)]
pub struct EffortRequest {
    pub effort: crate::agent::ReasoningEffort,
}

#[derive(Debug, Deserialize)]
pub struct ValidateWorkdirRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct SwitchModelRequest {
    /// Model selector, format provider/model (global switch, written back to config.toml)
    pub selector: String,
}

/// Add a model (POST /api/models/custom). Align with the TUI setup flow:
/// an existing provider reuses its credentials (base_url/api_key optional); a new provider requires both.
#[derive(Debug, Deserialize)]
pub struct CreateModelRequest {
    /// Target provider id; reuses its credentials if it exists, creates it otherwise (needs base_url + api_key)
    pub provider_id: String,
    /// Only needed when creating a new provider; optional for predefined providers (their built-in base_url is used)
    #[serde(default)]
    pub base_url: Option<String>,
    /// Only needed when creating a new provider
    #[serde(default)]
    pub api_key: Option<String>,
    /// Model ID (e.g. qwen3-235b-a22b)
    pub model_id: String,
    /// Context window size. Absent = take the `/models` metadata when cached (see /api/models/discover),
    /// otherwise the default 262144 (256K)
    #[serde(default)]
    pub context_window: Option<u32>,
}

/// Fetch a provider's model list (POST /api/models/discover). With an api_key the (not yet
/// saved) credentials of the add-model dialog are used; without one the saved credentials of
/// `provider_id` apply. The result is cached under `provider_id`, so a following
/// apply-discovered / create_model for that provider picks the metadata up.
#[derive(Debug, Deserialize)]
pub struct DiscoverModelsRequest {
    /// Cache key: the provider the list belongs to (a predefined id, or the name of the custom
    /// provider about to be created). With no base_url it must be a predefined id.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// Custom endpoint; defaults to the built-in endpoint of a predefined provider_id
    #[serde(default)]
    pub base_url: Option<String>,
    /// Absent/empty = use the saved key of provider_id (the fetch dialog of a configured provider)
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DiscoveredModelDto {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    /// Already in the provider's model table (the checkbox is pre-checked/disabled)
    #[serde(default)]
    pub already_added: bool,
}

#[derive(Debug, Serialize)]
pub struct DiscoverModelsResponse {
    /// The provider id the list was cached under (empty for a custom endpoint)
    pub provider_id: String,
    pub models: Vec<DiscoveredModelDto>,
}

/// Delete a custom model (DELETE /api/models/custom)
#[derive(Debug, Deserialize)]
pub struct DeleteModelRequest {
    /// Model selector, format provider/model
    pub selector: String,
}

/// Delete a custom provider (DELETE /api/providers, the whole entry including credentials and all models)
#[derive(Debug, Deserialize)]
pub struct DeleteProviderRequest {
    pub provider_id: String,
}

/// Edit an existing model (PUT /api/models/custom). Omitted fields keep their effective values.
#[derive(Debug, Deserialize)]
pub struct UpdateModelRequest {
    /// Existing selector, format provider/model
    pub selector: String,
    /// New model id (rename)
    #[serde(default)]
    pub model_id: Option<String>,
    /// Context window size (must be positive)
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Output token cap (must be positive)
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Whether the model accepts image input
    #[serde(default)]
    pub supports_vision: Option<bool>,
}

/// Edit an existing provider (PUT /api/providers). Omitted fields keep their current values.
#[derive(Debug, Deserialize)]
pub struct UpdateProviderRequest {
    pub provider_id: String,
    /// New provider id (rename); predefined providers cannot be renamed
    #[serde(default)]
    pub new_id: Option<String>,
    /// New API key (absent = keep current)
    #[serde(default)]
    pub api_key: Option<String>,
    /// New endpoint; empty string clears the override and falls back to the predefined endpoint
    /// (rejected for custom providers, which have no fallback)
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Add the picked models of a fetched list to the provider's model table
/// (POST /api/models/apply-discovered)
#[derive(Debug, Deserialize)]
pub struct ApplyDiscoveredRequest {
    pub provider_id: String,
    /// Model ids to add; ids already in the table are skipped
    pub model_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ApplyDiscoveredResponse {
    /// Ids that were actually added (skipped ones are not included)
    pub added: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSkillRequest {
    pub work_dir: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// SKILL.md body (after frontmatter)
    #[serde(default)]
    pub content: String,
    /// "global" (default, ~/.hailux/skills) | "project" (<work_dir>/.hailux/skills)
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSkillRequest {
    pub work_dir: Option<String>,
    /// Absolute path of the target SKILL.md (the location returned by GET /api/skills)
    pub location: String,
    /// New name (written into frontmatter; the discovery mechanism indexes by frontmatter name)
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct DeleteSkillRequest {
    pub work_dir: Option<String>,
    /// Absolute path of the target SKILL.md (its skill directory gets deleted)
    pub location: String,
}

// ── Response bodies (backend → frontend) ─────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub model: String,
    pub updated_at: String,
    pub work_dir: String,
}

#[derive(Debug, Serialize)]
pub struct WorkdirInfo {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ModelInfo {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub display: String,
    pub active: bool,
    /// Context window size (absent when parsing fails, e.g. provider not configured)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Output token cap (max_completion_tokens); same absence rule as context_window
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Whether the model accepts image input (effective value after the fallback chain)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    /// Whether the provider is predefined (predefined providers cannot be deleted as a whole)
    pub provider_predefined: bool,
}
/// Provider entry (GET /api/providers; used to pick a target provider when adding a model):
/// configured ones first, unconfigured predefined providers after
#[derive(Debug, Serialize)]
pub struct ProviderInfoDto {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// Predefined provider (base_url comes from the built-in table; no need to fill it in when adding a model)
    pub predefined: bool,
    /// No API Key configured (after selecting, just fill in api_key to enable)
    pub needs_setup: bool,
}

/// Slash command entry (GET /api/commands)
#[derive(Debug, Serialize)]
pub struct CommandInfoDto {
    pub name: String,
    pub description: String,
    /// "prompt" (expanded into the full prompt by the backend) | "ui" (handled locally by the frontend)
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct SkillInfoDto {
    pub name: String,
    pub description: String,
    /// Absolute path of SKILL.md (Windows verbatim prefix stripped)
    pub location: String,
    /// "global" (~/.hailux/skills) | "project" (<work_dir>/.hailux/skills)
    pub scope: String,
    /// SKILL.md body (after frontmatter)
    pub content: String,
    /// All files in the skill directory (including SKILL.md; relative path + byte size)
    #[serde(default)]
    pub files: Vec<SkillFileDto>,
}

/// A file entry inside the skill directory
#[derive(Debug, Clone, Serialize)]
pub struct SkillFileDto {
    /// Path relative to the skill directory (`/`-separated)
    pub path: String,
    /// Size in bytes
    pub size: u64,
}

/// Response body of GET /api/skills/file
#[derive(Debug, Serialize)]
pub struct SkillFileContentDto {
    /// Path relative to the skill directory
    pub path: String,
    /// File text content (lossy conversion for non-UTF-8 files)
    pub content: String,
}

/// MCP tool summary (name + description + parameter JSON Schema)
#[derive(Debug, Clone, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Parameter JSON Schema (object type with properties / required); may be missing
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub schema: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct McpServerInfo {
    pub name: String,
    pub connected: bool,
    pub tools: usize,
    /// "stdio" | "http"
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Error message when not connected (connecting / failure reason)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Tool list of the connected server (name + description)
    #[serde(default)]
    pub tool_details: Vec<McpToolInfo>,
}

/// Create an MCP server (POST /api/mcp).
/// command is required when transport = "stdio"; url is required when "http".
#[derive(Debug, Deserialize)]
pub struct CreateMcpServerRequest {
    pub name: String,
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

/// Update an MCP server (PUT /api/mcp). Located by name; new_name optionally renames it.
#[derive(Debug, Deserialize)]
pub struct UpdateMcpServerRequest {
    pub name: String,
    #[serde(default)]
    pub new_name: Option<String>,
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

/// Delete an MCP server (DELETE /api/mcp)
#[derive(Debug, Deserialize)]
pub struct DeleteMcpServerRequest {
    pub name: String,
}
