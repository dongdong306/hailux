use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

impl MessageRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

impl fmt::Display for MessageRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MessageRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "tool" => Ok(Self::Tool),
            other => Err(format!("unknown message role: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct SubsessionSummary {
    pub id: String,
    pub model: String,
    pub title: String,
    #[allow(dead_code)]
    pub created_at: String,
    #[allow(dead_code)]
    pub updated_at: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

/// Usage summary (assistant rows with usage in the messages table = one LLM request)
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct UsageSummary {
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// Usage aggregated by day
#[derive(Debug, Clone, serde::Serialize)]
pub struct DailyUsage {
    /// Date (YYYY-MM-DD, local timezone)
    pub date: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// Usage aggregated by model
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// Usage aggregated by project
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectUsage {
    pub work_dir: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// Known projects (distinct work_dir from the sessions table, most recently active first)
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkDirInfo {
    pub work_dir: String,
    pub sessions: i64,
    /// Last active time (max updated_at of sessions in the dir, ISO local time)
    pub last_active: String,
}

/// Usage details of recent requests
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageRecord {
    pub created_at: String,
    pub model: String,
    pub work_dir: String,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredMessage {
    pub role: MessageRole,
    pub content: String,
    pub tool_calls: Option<String>,
    pub tool_call_id: Option<String>,
    pub reasoning_content: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cached_tokens: Option<i64>,
    /// Model actually used for this LLM request (provider/model display);
    /// usage stats group by it
    pub model: Option<String>,
    pub runtime_meta: Option<String>,
    pub think_ms: Option<i64>,
    pub compacted: bool,
    /// Image attachments JSON ([{mime, data_url}]); used by user messages only
    pub attachments: Option<String>,
    /// @ mention metadata JSON ([{start, end, kind}], code-point offsets);
    /// user messages only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mentions: Option<String>,
}
