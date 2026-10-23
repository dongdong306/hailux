// ── 运行时配置结构 ───────────────────────────────────────────

use serde::Deserialize;
use std::collections::BTreeMap;

use super::DEFAULT_CONTEXT_WINDOW;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PermissionConfig {
    /// "ask" (Normal) 或 "yolo" (Yolo)
    #[serde(default)]
    pub mode: String,
    /// bash 权限规则: pattern -> action ("allow"/"deny"/"ask")
    #[serde(default)]
    pub bash: BTreeMap<String, String>,
    /// read 权限规则
    #[serde(default)]
    pub read: BTreeMap<String, String>,
    /// edit 权限规则
    #[serde(default)]
    pub edit: BTreeMap<String, String>,
    /// write 权限规则
    #[serde(default)]
    pub write: BTreeMap<String, String>,
    /// mcp 权限规则
    #[serde(default)]
    pub mcp: BTreeMap<String, String>,
    /// external_directory 权限规则（默认询问；可配置放行特定外部目录）
    #[serde(default)]
    pub external_directory: BTreeMap<String, String>,
}

/// config.toml `[retry]` 段：LLM 请求瞬态错误的自动重试策略。
/// 缺省字段取 [`crate::agent::retry::RetryConfig`] 的默认值。
#[derive(Debug, Clone, Deserialize)]
pub struct RetryConfigToml {
    /// 额外重试次数上限（总尝试次数 = 1 + max_retries；0 = 禁用重试）
    #[serde(default = "default_retry_max_retries")]
    pub max_retries: u32,
    /// 首次重试前的退避等待（ms）
    #[serde(default = "default_retry_initial_delay_ms")]
    pub initial_delay_ms: u64,
    /// 退避等待上限（ms）
    #[serde(default = "default_retry_max_delay_ms")]
    pub max_delay_ms: u64,
    /// 指数退避倍率
    #[serde(default = "default_retry_multiplier")]
    pub multiplier: f64,
}

impl Default for RetryConfigToml {
    fn default() -> Self {
        Self {
            max_retries: default_retry_max_retries(),
            initial_delay_ms: default_retry_initial_delay_ms(),
            max_delay_ms: default_retry_max_delay_ms(),
            multiplier: default_retry_multiplier(),
        }
    }
}

fn default_retry_max_retries() -> u32 {
    3
}

fn default_retry_initial_delay_ms() -> u64 {
    1000
}

fn default_retry_max_delay_ms() -> u64 {
    20_000
}

fn default_retry_multiplier() -> f64 {
    2.0
}

impl RetryConfigToml {
    /// 是否全字段等于默认值（save 时决定是否写入 `[retry]` 段）
    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl PartialEq for RetryConfigToml {
    fn eq(&self, other: &Self) -> bool {
        self.max_retries == other.max_retries
            && self.initial_delay_ms == other.initial_delay_ms
            && self.max_delay_ms == other.max_delay_ms
            && self.multiplier == other.multiplier
    }
}

impl From<&RetryConfigToml> for crate::agent::retry::RetryConfig {
    fn from(t: &RetryConfigToml) -> Self {
        Self {
            max_retries: t.max_retries,
            initial_delay: std::time::Duration::from_millis(t.initial_delay_ms),
            max_delay: std::time::Duration::from_millis(t.max_delay_ms),
            multiplier: t.multiplier,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProviderEntry {
    pub api_key: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub models: Option<BTreeMap<String, CustomModelEntry>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CustomModelEntry {
    /// API 输出上限
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// 上下文窗口大小
    #[serde(default = "default_context_window")]
    pub context_window: u32,
    /// 是否支持视觉（图片）输入。缺省（None）时按回退链取值：
    /// 同 provider 预定义模型声明 > false。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
}

pub(super) fn default_max_tokens() -> u32 {
    131072 // 默认 128K（写入配置）
}

pub(super) fn default_context_window() -> u32 {
    DEFAULT_CONTEXT_WINDOW
}

// ── 可选模型条目（供 UI 使用）─────────────────────────────────

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub display: String,
    pub needs_setup: bool,
    /// 可删除（用户自定义添加的模型；预定义 provider 的预定义模型不可删，会被自动合并回来）
    pub deletable: bool,
}

/// 删除自定义模型/provider 的失败原因（Web 层据此映射 HTTP 状态码）
#[derive(Debug)]
pub enum RemoveError {
    /// selector 格式错误，应为 provider/model
    InvalidSelector(String),
    /// 预定义模型/provider 不可删除
    Predefined(String),
    /// 未找到 provider 或模型
    NotFound(String),
    /// 删除后没有任何可用模型，拒绝删除
    NoModelsLeft(String),
}

impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoveError::InvalidSelector(msg)
            | RemoveError::Predefined(msg)
            | RemoveError::NotFound(msg)
            | RemoveError::NoModelsLeft(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for RemoveError {}
