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

/// 共享消息类型：Arc 包裹的 Compatible 消息，使得 Vec 克隆变为 O(n) 指针拷贝。
pub type SharedMessage = Arc<CompatibleChatCompletionRequestMessage>;

/// 思考模式配置
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub type_: String,
}

#[allow(dead_code)]
impl ThinkingConfig {
    pub fn enabled() -> Self {
        Self {
            type_: "enabled".to_string(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            type_: "disabled".to_string(),
        }
    }
}

/// 扩展的消息枚举，兼容标准 ChatCompletionRequestMessage 并支持 reasoning_content
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

/// 标准消息 → Compatible 消息（加载历史记录时使用）
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

/// Compatible 消息 → 标准消息（持久化存储时使用，reasoning_content 会被丢弃）
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

/// 各标准消息类型 → Compatible 消息（用于 .into() 链式调用）
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

/// 扩展的 Assistant 消息，在标准消息基础上增加 reasoning_content 字段
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct CompatibleChatCompletionRequestAssistantMessage {
    #[serde(flatten)]
    pub base: ChatCompletionRequestAssistantMessage,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// 自定义聊天完成请求，序列化时替换 messages 字段以支持扩展消息类型
pub struct CompatibleCreateChatCompletionRequest {
    base: CreateChatCompletionRequest,
    custom_messages: Vec<SharedMessage>,
    thinking: Option<ThinkingConfig>,
    extra: Map<String, Value>,
}

/// 自定义序列化：将 base 序列化后用 custom_messages 替换 messages 字段，再附加 thinking 和 extra
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

/// 请求构建器，复用 CreateChatCompletionRequestArgs 并扩展特有字段
#[allow(dead_code)]
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

#[allow(dead_code)]
impl CompatibleCreateChatCompletionRequestArgs {
    // === 委托标准字段 ===

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

    pub fn temperature(&mut self, temp: f32) -> &mut Self {
        self.base.temperature(temp);
        self
    }

    pub fn top_p(&mut self, top_p: f32) -> &mut Self {
        self.base.top_p(top_p);
        self
    }

    pub fn frequency_penalty(&mut self, penalty: f32) -> &mut Self {
        self.base.frequency_penalty(penalty);
        self
    }

    pub fn presence_penalty(&mut self, penalty: f32) -> &mut Self {
        self.base.presence_penalty(penalty);
        self
    }

    // === 特有字段 ===

    pub fn thinking(&mut self, thinking: ThinkingConfig) -> &mut Self {
        self.thinking = Some(thinking);
        self
    }

    pub fn extra(&mut self, key: impl Into<String>, value: Value) -> &mut Self {
        self.extra.insert(key.into(), value);
        self
    }

    /// 构建最终的请求对象
    pub fn build(&mut self) -> Result<CompatibleCreateChatCompletionRequest, OpenAIError> {
        // base 中 messages 设为空，序列化时用 custom_messages 替代
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
    pub usage: Option<async_openai::types::chat::CompletionUsage>,
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

#[derive(Serialize, Deserialize, Debug)]
pub struct CompatibleChatCompletionStreamResponseDelta {
    #[serde(flatten)]
    pub base: ChatCompletionStreamResponseDelta,

    pub reasoning_content: Option<String>,
}
