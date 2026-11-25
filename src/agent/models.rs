use async_openai::types::chat::ChatCompletionStreamResponseDelta;
use async_openai::{
    error::OpenAIError,
    types::chat::{
        ChatChoiceLogprobs, ChatCompletionRequestAssistantMessage,
        ChatCompletionRequestDeveloperMessage, ChatCompletionRequestFunctionMessage,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestToolMessage, ChatCompletionRequestUserMessage,
        ChatCompletionToolChoiceOption, ChatCompletionTools, CreateChatCompletionRequest,
        CreateChatCompletionRequestArgs, FinishReason,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;

/// Shared message type: an Arc-wrapped Compatible message, making Vec clones an O(n) pointer copy.
pub type SharedMessage = Arc<CompatibleChatCompletionRequestMessage>;

/// Reasoning-effort levels (top-level `reasoning_effort` field in the request body).
/// DeepSeek V4 / GLM-5.3 officially support the low/high/max levels, defaulting to high;
/// the OpenAI-side value domain evolves with models
/// (none/minimal/low/medium/high/xhigh/max, model-dependent), but all three values
/// low/high/max fall within it and can be passed through directly.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    #[default]
    High,
    Max,
}

impl ReasoningEffort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// Case-insensitive parsing (used by the TUI /effort argument)
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "high" => Some(Self::High),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    /// Cycles through: low → high → max → low
    pub fn next(&self) -> Self {
        match self {
            Self::Low => Self::High,
            Self::High => Self::Max,
            Self::Max => Self::Low,
        }
    }
}

/// Thinking-mode configuration
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub type_: String,
}

impl ThinkingConfig {
    pub fn enabled() -> Self {
        Self {
            type_: "enabled".to_string(),
        }
    }
}

/// Extended message enum: standard ChatCompletionRequestMessage plus reasoning_content support
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum CompatibleChatCompletionRequestMessage {
    Developer(ChatCompletionRequestDeveloperMessage),
    System(ChatCompletionRequestSystemMessage),
    User(ChatCompletionRequestUserMessage),
    Assistant(CompatibleChatCompletionRequestAssistantMessage),
    Tool(ChatCompletionRequestToolMessage),
    Function(ChatCompletionRequestFunctionMessage),
}

/// Standard message → Compatible message (used when loading history)
impl From<ChatCompletionRequestMessage> for CompatibleChatCompletionRequestMessage {
    fn from(msg: ChatCompletionRequestMessage) -> Self {
        match msg {
            ChatCompletionRequestMessage::Developer(m) => Self::Developer(m),
            ChatCompletionRequestMessage::System(m) => Self::System(m),
            ChatCompletionRequestMessage::User(m) => Self::User(m),
            ChatCompletionRequestMessage::Assistant(m) => {
                Self::Assistant(CompatibleChatCompletionRequestAssistantMessage {
                    base: m,
                    reasoning_content: None,
                })
            }
            ChatCompletionRequestMessage::Tool(m) => Self::Tool(m),
            ChatCompletionRequestMessage::Function(m) => Self::Function(m),
        }
    }
}

/// Compatible message → standard message (used when persisting; reasoning_content is dropped)
impl From<CompatibleChatCompletionRequestMessage> for ChatCompletionRequestMessage {
    fn from(msg: CompatibleChatCompletionRequestMessage) -> Self {
        match msg {
            CompatibleChatCompletionRequestMessage::Developer(m) => Self::Developer(m),
            CompatibleChatCompletionRequestMessage::System(m) => Self::System(m),
            CompatibleChatCompletionRequestMessage::User(m) => Self::User(m),
            CompatibleChatCompletionRequestMessage::Assistant(m) => Self::Assistant(m.base),
            CompatibleChatCompletionRequestMessage::Tool(m) => Self::Tool(m),
            CompatibleChatCompletionRequestMessage::Function(m) => Self::Function(m),
        }
    }
}

/// Each standard message type → Compatible message (for .into() call chains)
impl From<ChatCompletionRequestSystemMessage> for CompatibleChatCompletionRequestMessage {
    fn from(m: ChatCompletionRequestSystemMessage) -> Self {
        Self::System(m)
    }
}

impl From<ChatCompletionRequestUserMessage> for CompatibleChatCompletionRequestMessage {
    fn from(m: ChatCompletionRequestUserMessage) -> Self {
        Self::User(m)
    }
}

impl From<ChatCompletionRequestAssistantMessage> for CompatibleChatCompletionRequestMessage {
    fn from(m: ChatCompletionRequestAssistantMessage) -> Self {
        Self::Assistant(CompatibleChatCompletionRequestAssistantMessage {
            base: m,
            reasoning_content: None,
        })
    }
}

impl From<ChatCompletionRequestToolMessage> for CompatibleChatCompletionRequestMessage {
    fn from(m: ChatCompletionRequestToolMessage) -> Self {
        Self::Tool(m)
    }
}

impl From<CompatibleChatCompletionRequestAssistantMessage>
    for CompatibleChatCompletionRequestMessage
{
    fn from(m: CompatibleChatCompletionRequestAssistantMessage) -> Self {
        Self::Assistant(m)
    }
}

/// Extended Assistant message that adds a reasoning_content field on top of the standard one
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct CompatibleChatCompletionRequestAssistantMessage {
    #[serde(flatten)]
    pub base: ChatCompletionRequestAssistantMessage,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// Custom chat completion request: replaces the messages field during serialization
/// to support the extended message types.
pub struct CompatibleCreateChatCompletionRequest {
    base: CreateChatCompletionRequest,
    custom_messages: Vec<SharedMessage>,
    thinking: Option<ThinkingConfig>,
    extra: Map<String, Value>,
}

/// Custom serialization: serializes base, replaces the messages field with custom_messages,
/// then appends thinking and extra.
impl Serialize for CompatibleCreateChatCompletionRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut value = serde_json::to_value(&self.base).map_err(serde::ser::Error::custom)?;

        if let Value::Object(map) = &mut value {
            map.insert(
                "messages".to_string(),
                serde_json::to_value(&self.custom_messages).map_err(serde::ser::Error::custom)?,
            );
            if let Some(thinking) = &self.thinking {
                map.insert(
                    "thinking".to_string(),
                    serde_json::to_value(thinking).map_err(serde::ser::Error::custom)?,
                );
            }
            for (k, v) in &self.extra {
                map.insert(k.clone(), v.clone());
            }
        }

        value.serialize(serializer)
    }
}

/// Request builder that reuses CreateChatCompletionRequestArgs and adds extension-specific fields
pub struct CompatibleCreateChatCompletionRequestArgs {
    base: CreateChatCompletionRequestArgs,
    custom_messages: Vec<SharedMessage>,
    thinking: Option<ThinkingConfig>,
    extra: Map<String, Value>,
}

impl Default for CompatibleCreateChatCompletionRequestArgs {
    fn default() -> Self {
        Self {
            base: CreateChatCompletionRequestArgs::default(),
            custom_messages: Vec::new(),
            thinking: None,
            extra: Map::new(),
        }
    }
}

impl CompatibleCreateChatCompletionRequestArgs {
    // === Delegated standard fields ===

    pub fn model(&mut self, model: impl Into<String>) -> &mut Self {
        self.base.model(model);
        self
    }

    pub fn max_completion_tokens(&mut self, tokens: u32) -> &mut Self {
        self.base.max_completion_tokens(tokens);
        self
    }

    pub fn stream(&mut self, stream: bool) -> &mut Self {
        self.base.stream(stream);
        self
    }

    pub fn messages(&mut self, messages: Vec<SharedMessage>) -> &mut Self {
        self.custom_messages = messages;
        self
    }

    pub fn tools(&mut self, tools: Vec<ChatCompletionTools>) -> &mut Self {
        self.base.tools(tools);
        self
    }

    pub fn tool_choice(&mut self, choice: ChatCompletionToolChoiceOption) -> &mut Self {
        self.base.tool_choice(choice);
        self
    }

    // === Extension-specific fields ===

    pub fn thinking(&mut self, thinking: ThinkingConfig) -> &mut Self {
        self.thinking = Some(thinking);
        self
    }

    pub fn extra(&mut self, key: impl Into<String>, value: Value) -> &mut Self {
        self.extra.insert(key.into(), value);
        self
    }

    /// Builds the final request object
    pub fn build(&mut self) -> Result<CompatibleCreateChatCompletionRequest, OpenAIError> {
        // messages in base is set empty; custom_messages replaces it at serialization time
        self.base.messages(vec![]);
        let base = self.base.build()?;
        Ok(CompatibleCreateChatCompletionRequest {
            base,
            custom_messages: std::mem::take(&mut self.custom_messages),
            thinking: self.thinking.clone(),
            extra: self.extra.clone(),
        })
    }
}

/// DeepSeek-compatible usage stats: standard OpenAI fields plus flatten-captured
/// extension fields such as `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct CompatibleCompletionUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<async_openai::types::chat::PromptTokensDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<async_openai::types::chat::CompletionTokensDetails>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CompatibleCompletionUsage {
    /// Number of input tokens that hit the cache.
    /// Prefers the OpenAI-style `prompt_tokens_details.cached_tokens`,
    /// falling back to the DeepSeek-style `prompt_cache_hit_tokens`.
    pub fn cached_tokens(&self) -> u32 {
        if let Some(details) = &self.prompt_tokens_details
            && let Some(cached) = details.cached_tokens
        {
            return cached;
        }
        self.extra
            .get("prompt_cache_hit_tokens")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0)
    }

    /// Input tokens that missed the cache (DeepSeek `prompt_cache_miss_tokens`, inferable).
    pub fn cache_miss_tokens(&self) -> u32 {
        self.extra
            .get("prompt_cache_miss_tokens")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0)
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CompatibleCreateChatCompletionStreamResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub choices: Vec<CompatibleChatChoiceStream>,
    #[serde(default)]
    pub created: u32,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub object: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<async_openai::types::chat::ServiceTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<CompatibleCompletionUsage>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CompatibleChatChoiceStream {
    pub index: u32,
    pub delta: CompatibleChatCompletionStreamResponseDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<ChatChoiceLogprobs>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_effort_serde_roundtrip() {
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"low\"").unwrap(),
            ReasoningEffort::Low
        );
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"max\"").unwrap(),
            ReasoningEffort::Max
        );
        assert_eq!(
            serde_json::to_value(ReasoningEffort::High).unwrap(),
            serde_json::json!("high")
        );
    }

    #[test]
    fn reasoning_effort_parse_case_insensitive() {
        assert_eq!(ReasoningEffort::parse("LOW"), Some(ReasoningEffort::Low));
        assert_eq!(
            ReasoningEffort::parse(" High "),
            Some(ReasoningEffort::High)
        );
        assert_eq!(ReasoningEffort::parse("Max"), Some(ReasoningEffort::Max));
        assert_eq!(ReasoningEffort::parse("medium"), None);
        assert_eq!(ReasoningEffort::parse(""), None);
    }

    #[test]
    fn reasoning_effort_next_cycles() {
        assert_eq!(ReasoningEffort::Low.next(), ReasoningEffort::High);
        assert_eq!(ReasoningEffort::High.next(), ReasoningEffort::Max);
        assert_eq!(ReasoningEffort::Max.next(), ReasoningEffort::Low);
    }

    #[test]
    fn reasoning_effort_values_in_openai_domain() {
        // low/high/max are all valid OpenAI reasoning_effort values (fully supported since
        // GPT-5.6+); pass through directly, no mapping needed
        assert_eq!(ReasoningEffort::Low.as_str(), "low");
        assert_eq!(ReasoningEffort::High.as_str(), "high");
        assert_eq!(ReasoningEffort::Max.as_str(), "max");
    }

    #[test]
    fn request_serializes_reasoning_effort() {
        // End to end: builder extra field → custom serialization → request JSON
        let mut builder = CompatibleCreateChatCompletionRequestArgs::default();
        builder
            .model("test-model")
            .stream(true)
            .thinking(ThinkingConfig::enabled())
            .extra(
                "reasoning_effort",
                serde_json::json!(ReasoningEffort::Max.as_str()),
            );
        let req = builder.build().unwrap();
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json.get("reasoning_effort").and_then(Value::as_str),
            Some("max")
        );
        assert_eq!(json["thinking"]["type"], "enabled");
    }

    #[test]
    fn usage_deepseek_cache_fields() {
        let usage: CompatibleCompletionUsage = serde_json::from_str(
            r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,
                "prompt_cache_hit_tokens":80,"prompt_cache_miss_tokens":20}"#,
        )
        .unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.cached_tokens(), 80);
        assert_eq!(usage.cache_miss_tokens(), 20);
    }

    #[test]
    fn usage_openai_style_details() {
        let usage: CompatibleCompletionUsage = serde_json::from_str(
            r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,
                "prompt_tokens_details":{"cached_tokens":64}}"#,
        )
        .unwrap();
        assert_eq!(usage.cached_tokens(), 64);
        assert_eq!(usage.cache_miss_tokens(), 0);
    }

    #[test]
    fn usage_missing_cache_fields_defaults_zero() {
        let usage: CompatibleCompletionUsage =
            serde_json::from_str(r#"{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}"#)
                .unwrap();
        assert_eq!(usage.cached_tokens(), 0);
        assert_eq!(usage.cache_miss_tokens(), 0);
    }

    #[test]
    fn stream_response_usage_parsed() {
        let chunk: CompatibleCreateChatCompletionStreamResponse = serde_json::from_str(
            r#"{"id":"1","choices":[],"created":0,"model":"m","object":"chat.completion.chunk",
                "usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,
                "prompt_cache_hit_tokens":8,"prompt_cache_miss_tokens":2}}"#,
        )
        .unwrap();
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.cached_tokens(), 8);
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CompatibleChatCompletionStreamResponseDelta {
    #[serde(flatten)]
    pub base: ChatCompletionStreamResponseDelta,

    pub reasoning_content: Option<String>,
}
