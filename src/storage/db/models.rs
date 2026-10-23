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

/// 用量汇总（基于 messages 表中带 usage 的 assistant 行 = 一次 LLM 请求）
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct UsageSummary {
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// 按日聚合的用量
#[derive(Debug, Clone, serde::Serialize)]
pub struct DailyUsage {
    /// 日期（YYYY-MM-DD，本地时区）
    pub date: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// 按模型聚合的用量
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// 按项目聚合的用量
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectUsage {
    pub work_dir: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub cached_tokens: i64,
    pub completion_tokens: i64,
}

/// 已知项目（sessions 表 distinct work_dir，最近活跃降序）
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkDirInfo {
    pub work_dir: String,
    pub sessions: i64,
    /// 最近活跃时间（该目录下会话的最大 updated_at，ISO 本地时间）
    pub last_active: String,
}

/// 最近一次请求的用量明细
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
    /// 该次 LLM 请求实际使用的模型（provider/model display）；用量统计按此分组
    pub model: Option<String>,
    pub runtime_meta: Option<String>,
    pub think_ms: Option<i64>,
    pub compacted: bool,
    /// 图片附件 JSON（[{mime, data_url}]）；仅用户消息使用
    pub attachments: Option<String>,
}
