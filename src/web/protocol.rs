//! Web 前后端 JSON 协议（前端契约）。
//!
//! `ServerEvent` = SSE 推送（后端 → 前端），`#[serde(tag = "type")]`；
//! 请求体结构对应各 REST 端点。

use serde::{Deserialize, Serialize};

use crate::agent::event::{QuestionInfo, TaskStatus};

// ── SSE 推送事件（后端 → 前端）───────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
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
        /// 模型上下文窗口大小（供前端展示上下文占用比例）
        context_window: u32,
    },
    ToolCallStart {
        name: String,
        arguments: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        subagent: Option<String>,
        /// 来源任务在 tasks 数组中的下标（同名 subagent 并发时区分实例）
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
    /// 会话内信息性提示（如压缩开始）
    Notice {
        text: String,
    },
    CompactChunk {
        text: String,
    },
    CompactComplete {
        summary_chars: usize,
        compacted_count: usize,
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

// ── 用量统计（GET /api/stats）────────────────────────────────

/// 用量统计响应：窗口内汇总 + 全量汇总 + 分维度聚合。
/// 复用 storage 层的 Serialize 结构（UsageSummary/DailyUsage/ModelUsage/UsageRecord）。
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    /// 统计窗口（天；0 = 全部）
    pub days: u32,
    /// 窗口内汇总
    pub summary: crate::storage::UsageSummary,
    /// 全量汇总（不受 days 限制）
    pub total: crate::storage::UsageSummary,
    pub daily: Vec<crate::storage::DailyUsage>,
    pub by_model: Vec<crate::storage::ModelUsage>,
    pub recent: Vec<crate::storage::UsageRecord>,
}

// ── 请求体（前端 → 后端）─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
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
    pub work_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct YoloRequest {
    pub enabled: bool,
    pub work_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ValidateWorkdirRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct SwitchModelRequest {
    /// 模型选择器，格式 provider/model（全局切换，写回 config.toml）
    pub selector: String,
}

/// 添加自定义模型（POST /api/models/custom）。对齐 TUI AddModelForm：
/// 已配置 provider 复用凭据（base_url/api_key 可省），新建 provider 两者必填。
#[derive(Debug, Deserialize)]
pub struct CreateModelRequest {
    /// 目标 provider id；已存在时复用其凭据，不存在时新建（需 base_url + api_key）
    pub provider_id: String,
    /// 仅新建 provider 时需要；预定义 provider 可省（用预定义 base_url）
    #[serde(default)]
    pub base_url: Option<String>,
    /// 仅新建 provider 时需要
    #[serde(default)]
    pub api_key: Option<String>,
    /// 模型 ID（如 qwen3-235b-a22b）
    pub model_id: String,
    /// 上下文窗口大小，缺省 131072 (128K)
    #[serde(default)]
    pub context_window: Option<u32>,
}

/// 删除自定义模型（DELETE /api/models/custom）
#[derive(Debug, Deserialize)]
pub struct DeleteModelRequest {
    /// 模型选择器，格式 provider/model
    pub selector: String,
}

/// 删除自定义 provider（DELETE /api/providers，整条含凭据与全部模型）
#[derive(Debug, Deserialize)]
pub struct DeleteProviderRequest {
    pub provider_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateSkillRequest {
    pub work_dir: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// SKILL.md 正文（frontmatter 之后）
    #[serde(default)]
    pub content: String,
    /// "global"（默认，~/.hailux/skills）| "project"（<work_dir>/.hailux/skills）
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSkillRequest {
    pub work_dir: Option<String>,
    /// 目标 SKILL.md 绝对路径（GET /api/skills 返回的 location）
    pub location: String,
    /// 新名称（写入 frontmatter；发现机制按 frontmatter name 索引）
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct DeleteSkillRequest {
    pub work_dir: Option<String>,
    /// 目标 SKILL.md 绝对路径（删除其所在技能目录）
    pub location: String,
}

// ── 响应体（后端 → 前端）─────────────────────────────────────

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
    /// 上下文窗口大小（解析失败如未配置 provider 时缺省）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// 可删除（用户自定义添加；预定义模型不可删，会被自动合并回来）
    pub deletable: bool,
    /// provider 是否为预定义（预定义 provider 不可整条删除）
    pub provider_predefined: bool,
    /// provider 未配置 API Key（需 setup 后才可使用）
    pub needs_setup: bool,
}

/// 已配置 provider 条目（GET /api/providers；添加模型时选择目标 provider 用）
#[derive(Debug, Serialize)]
pub struct ProviderInfoDto {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// 预定义 provider（base_url 来自预定义表，新建模型时无需填写）
    pub predefined: bool,
    /// 未配置 API Key（选择后仅需填写 api_key 即可启用）
    pub needs_setup: bool,
}

/// 斜杠命令条目（GET /api/commands）
#[derive(Debug, Serialize)]
pub struct CommandInfoDto {
    pub name: String,
    pub description: String,
    /// "prompt"（后端展开为完整提示词）| "ui"（前端本地处理）
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct SkillInfoDto {
    pub name: String,
    pub description: String,
    /// SKILL.md 绝对路径（已剥离 Windows verbatim 前缀）
    pub location: String,
    /// "global"（~/.hailux/skills）| "project"（<work_dir>/.hailux/skills）
    pub scope: String,
    /// SKILL.md 正文（frontmatter 之后）
    pub content: String,
    /// 技能目录内全部文件（含 SKILL.md，相对路径 + 字节大小）
    #[serde(default)]
    pub files: Vec<SkillFileDto>,
}

/// 技能目录内的文件条目
#[derive(Debug, Clone, Serialize)]
pub struct SkillFileDto {
    /// 相对于技能目录的路径（`/` 分隔）
    pub path: String,
    /// 字节大小
    pub size: u64,
}

/// GET /api/skills/file 响应体
#[derive(Debug, Serialize)]
pub struct SkillFileContentDto {
    /// 相对于技能目录的路径
    pub path: String,
    /// 文件文本内容（非 UTF-8 文件为有损转换）
    pub content: String,
}

/// MCP 工具摘要（名称 + 描述 + 参数 JSON Schema）
#[derive(Debug, Clone, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 参数 JSON Schema（object 类型，含 properties / required）；可能缺失
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
    /// 未连接时的错误信息（连接中 / 失败原因）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 已连接服务器的工具列表（名称 + 描述）
    #[serde(default)]
    pub tool_details: Vec<McpToolInfo>,
}

/// 创建 MCP 服务器（POST /api/mcp）。
/// transport = "stdio" 时须提供 command；"http" 时须提供 url。
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

/// 更新 MCP 服务器（PUT /api/mcp）。按 name 定位；new_name 可选改名。
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

/// 删除 MCP 服务器（DELETE /api/mcp）
#[derive(Debug, Deserialize)]
pub struct DeleteMcpServerRequest {
    pub name: String,
}
