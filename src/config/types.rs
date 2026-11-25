// ── Runtime config structs ──────────────────────────────────

use serde::Deserialize;
use std::collections::BTreeMap;

use super::DEFAULT_CONTEXT_WINDOW;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PermissionConfig {
    /// "ask" (Normal) or "yolo" (Yolo)
    #[serde(default)]
    pub mode: String,
    /// shell permission rules: pattern -> action ("allow"/"deny"/"ask")
    #[serde(default)]
    pub shell: BTreeMap<String, String>,
    /// read permission rules
    #[serde(default)]
    pub read: BTreeMap<String, String>,
    /// edit permission rules
    #[serde(default)]
    pub edit: BTreeMap<String, String>,
    /// write permission rules
    #[serde(default)]
    pub write: BTreeMap<String, String>,
    /// mcp permission rules
    #[serde(default)]
    pub mcp: BTreeMap<String, String>,
    /// external_directory rules (ask by default; specific external dirs can be allowed)
    #[serde(default)]
    pub external_directory: BTreeMap<String, String>,
}

/// The `[retry]` section of config.toml: automatic retry policy for transient LLM request errors.
/// Absent fields take defaults from [`crate::agent::retry::RetryConfig`].
#[derive(Debug, Clone, Deserialize)]
pub struct RetryConfigToml {
    /// Max extra retries (total attempts = 1 + max_retries; 0 disables retries)
    #[serde(default = "default_retry_max_retries")]
    pub max_retries: u32,
    /// Backoff before the first retry (ms)
    #[serde(default = "default_retry_initial_delay_ms")]
    pub initial_delay_ms: u64,
    /// Backoff cap (ms)
    #[serde(default = "default_retry_max_delay_ms")]
    pub max_delay_ms: u64,
    /// Exponential backoff multiplier
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
    /// Whether every field equals its default (decides whether save writes the `[retry]` section)
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

/// The `[session]` section of config.toml: web multi-session concurrency guardrails.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionConfigToml {
    /// Max concurrent top-level agent runs in the web server (0 = unlimited).
    /// Excess chat/compact requests are rejected with 429 instead of queueing.
    #[serde(default = "default_max_concurrent_runs")]
    pub max_concurrent_runs: u32,
}

impl Default for SessionConfigToml {
    fn default() -> Self {
        Self {
            max_concurrent_runs: default_max_concurrent_runs(),
        }
    }
}

fn default_max_concurrent_runs() -> u32 {
    4
}

impl SessionConfigToml {
    /// Whether every field equals its default (decides whether save writes the `[session]` section)
    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProviderEntry {
    pub api_key: String,
    /// Endpoint override; absent for predefined providers (their built-in URL applies)
    #[serde(default)]
    pub base_url: Option<String>,
    /// Models the user picked (setup / model fetch / settings). The visible model list is this
    /// table and nothing else — models only reported by the provider's `/models` endpoint are
    /// suggestions until explicitly added.
    #[serde(default)]
    pub models: BTreeMap<String, CustomModelEntry>,
}

impl ProviderEntry {
    /// The table's canonical key for a model id (case-insensitive match). Model lookups,
    /// duplicate checks and removals all go through this so a differently-cased selector
    /// still resolves to the one row; inserts keep the spelling the user provided.
    pub fn find_model_key(&self, model_id: &str) -> Option<String> {
        self.models
            .keys()
            .find(|k| k.eq_ignore_ascii_case(model_id))
            .cloned()
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct CustomModelEntry {
    /// API output cap
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Context window size
    #[serde(default = "default_context_window")]
    pub context_window: u32,
    /// Whether the model accepts image input. When absent (None) the value is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
}

pub(super) fn default_max_tokens() -> u32 {
    131072 // Default 128K (written to the config)
}

pub(super) fn default_context_window() -> u32 {
    DEFAULT_CONTEXT_WINDOW
}

// ── Selectable model entries (for the UI) ───────────────────

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub display: String,
}

/// Why a model/provider update was rejected (the web layer maps these to HTTP statuses)
#[derive(Debug)]
pub enum UpdateError {
    /// Selector is not in provider/model form
    InvalidSelector(String),
    /// A field value is unusable (empty name, non-http endpoint, zero or negative size)
    Invalid(String),
    /// Provider or model not found
    NotFound(String),
    /// Conflicts with a config invariant (predefined provider renamed, name taken, endpoint cleared)
    Conflict(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::InvalidSelector(msg)
            | UpdateError::Invalid(msg)
            | UpdateError::NotFound(msg)
            | UpdateError::Conflict(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for UpdateError {}

/// Why deleting a custom model/provider failed (the web layer maps these to HTTP status codes)
#[derive(Debug)]
pub enum RemoveError {
    /// Malformed selector, expected provider/model
    InvalidSelector(String),
    /// Predefined provider cannot be deleted
    Predefined(String),
    /// Provider or model not found
    NotFound(String),
    /// No usable model would remain, deletion refused
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
