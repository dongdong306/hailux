use crate::agent::models::{
    CompatibleChatCompletionRequestAssistantMessage, CompatibleChatCompletionRequestMessage,
};
use async_openai::types::chat::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestAssistantMessageContent, ChatCompletionRequestAssistantMessageContentPart,
    ChatCompletionRequestDeveloperMessageContent, ChatCompletionRequestDeveloperMessageContentPart,
    ChatCompletionRequestMessageContentPartImage, ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
    ChatCompletionRequestSystemMessageContentPart, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent, ChatCompletionRequestToolMessageContentPart,
    ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ImageUrl,
};

use super::models::{MessageRole, StoredMessage};

fn extract_user_content(content: &ChatCompletionRequestUserMessageContent) -> String {
    match content {
        ChatCompletionRequestUserMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestUserMessageContent::Array(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ChatCompletionRequestUserMessageContentPart::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn extract_assistant_content(
    content: &Option<ChatCompletionRequestAssistantMessageContent>,
) -> String {
    match content {
        Some(ChatCompletionRequestAssistantMessageContent::Text(t)) => t.clone(),
        Some(ChatCompletionRequestAssistantMessageContent::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                ChatCompletionRequestAssistantMessageContentPart::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

fn extract_tool_content(content: &ChatCompletionRequestToolMessageContent) -> String {
    match content {
        ChatCompletionRequestToolMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestToolMessageContent::Array(parts) => parts
            .iter()
            .map(|p| match p {
                ChatCompletionRequestToolMessageContentPart::Text(t) => t.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn extract_system_content(content: &ChatCompletionRequestSystemMessageContent) -> String {
    match content {
        ChatCompletionRequestSystemMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestSystemMessageContent::Array(parts) => parts
            .iter()
            .map(|p| match p {
                ChatCompletionRequestSystemMessageContentPart::Text(t) => t.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn extract_developer_content(content: &ChatCompletionRequestDeveloperMessageContent) -> String {
    match content {
        ChatCompletionRequestDeveloperMessageContent::Text(t) => t.clone(),
        ChatCompletionRequestDeveloperMessageContent::Array(parts) => parts
            .iter()
            .map(|p| match p {
                ChatCompletionRequestDeveloperMessageContentPart::Text(t) => t.text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn compatible_reasoning_content(msg: &CompatibleChatCompletionRequestMessage) -> Option<String> {
    match msg {
        CompatibleChatCompletionRequestMessage::Assistant(m) => m.reasoning_content.clone(),
        _ => None,
    }
}

fn compatible_message_role(msg: &CompatibleChatCompletionRequestMessage) -> MessageRole {
    match msg {
        CompatibleChatCompletionRequestMessage::System(_) => MessageRole::System,
        CompatibleChatCompletionRequestMessage::Developer(_) => MessageRole::System,
        CompatibleChatCompletionRequestMessage::User(_) => MessageRole::User,
        CompatibleChatCompletionRequestMessage::Assistant(_) => MessageRole::Assistant,
        CompatibleChatCompletionRequestMessage::Tool(_) => MessageRole::Tool,
        CompatibleChatCompletionRequestMessage::Function(_) => MessageRole::Tool,
    }
}

fn compatible_message_content_text(msg: &CompatibleChatCompletionRequestMessage) -> String {
    match msg {
        CompatibleChatCompletionRequestMessage::User(m) => extract_user_content(&m.content),
        CompatibleChatCompletionRequestMessage::Assistant(m) => {
            extract_assistant_content(&m.base.content)
        }
        CompatibleChatCompletionRequestMessage::Tool(m) => extract_tool_content(&m.content),
        CompatibleChatCompletionRequestMessage::System(m) => extract_system_content(&m.content),
        CompatibleChatCompletionRequestMessage::Developer(m) => {
            extract_developer_content(&m.content)
        }
        CompatibleChatCompletionRequestMessage::Function(m) => {
            m.content.clone().unwrap_or_default()
        }
    }
}

fn compatible_message_tool_calls_json(
    msg: &CompatibleChatCompletionRequestMessage,
) -> Option<String> {
    match msg {
        CompatibleChatCompletionRequestMessage::Assistant(m) => m
            .base
            .tool_calls
            .as_ref()
            .map(|tc| serde_json::to_string(tc).unwrap_or_default()),
        _ => None,
    }
}

fn compatible_message_tool_call_id(msg: &CompatibleChatCompletionRequestMessage) -> Option<String> {
    match msg {
        CompatibleChatCompletionRequestMessage::Tool(m) => Some(m.tool_call_id.clone()),
        _ => None,
    }
}

pub fn to_stored_message(msg: &CompatibleChatCompletionRequestMessage) -> StoredMessage {
    // 用户消息的图片附件从 Array content 中抽出，以 JSON 存入 attachments 列
    let attachments = match msg {
        CompatibleChatCompletionRequestMessage::User(m) => match &m.content {
            ChatCompletionRequestUserMessageContent::Array(parts) => {
                let images: Vec<crate::agent::media::Attachment> = parts
                    .iter()
                    .filter_map(|p| match p {
                        ChatCompletionRequestUserMessageContentPart::ImageUrl(img) => {
                            let mime = img
                                .image_url
                                .url
                                .strip_prefix("data:")
                                .and_then(|rest| rest.split(';').next())
                                .unwrap_or("image/png")
                                .to_string();
                            Some(crate::agent::media::Attachment {
                                mime,
                                data_url: img.image_url.url.clone(),
                                // name 仅在 agent 内存态用于生成提示文本，不落库
                                name: None,
                            })
                        }
                        _ => None,
                    })
                    .collect();
                if images.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&images).unwrap_or_default())
                }
            }
            _ => None,
        },
        _ => None,
    };
    StoredMessage {
        role: compatible_message_role(msg),
        content: compatible_message_content_text(msg),
        tool_calls: compatible_message_tool_calls_json(msg),
        tool_call_id: compatible_message_tool_call_id(msg),
        reasoning_content: compatible_reasoning_content(msg),
        prompt_tokens: None,
        completion_tokens: None,
        cached_tokens: None,
        model: None,
        runtime_meta: None,
        think_ms: None,
        compacted: false,
        attachments,
    }
}

pub fn from_stored_message(msg: &StoredMessage) -> Option<CompatibleChatCompletionRequestMessage> {
    match msg.role {
        MessageRole::System => Some(
            ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(msg.content.clone()),
                name: None,
            }
            .into(),
        ),
        MessageRole::User => {
            // 有附件 → 重建 Array content（Text + ImageUrl parts），历史重放时图片仍可用
            let attachments = msg
                .attachments
                .as_deref()
                .and_then(|json| {
                    serde_json::from_str::<Vec<crate::agent::media::Attachment>>(json).ok()
                })
                .unwrap_or_default();
            if attachments.is_empty() {
                Some(
                    ChatCompletionRequestUserMessage {
                        content: ChatCompletionRequestUserMessageContent::Text(msg.content.clone()),
                        name: None,
                    }
                    .into(),
                )
            } else {
                // 文本为空时跳过 Text part（与 build_user_message 对称，部分
                // 兼容端点拒绝空 text part）
                let mut parts = Vec::new();
                if !msg.content.is_empty() {
                    parts.push(ChatCompletionRequestUserMessageContentPart::Text(
                        ChatCompletionRequestMessageContentPartText {
                            text: msg.content.clone(),
                        },
                    ));
                }
                parts.extend(attachments.iter().map(|att| {
                    ChatCompletionRequestUserMessageContentPart::ImageUrl(
                        ChatCompletionRequestMessageContentPartImage {
                            image_url: ImageUrl {
                                url: att.data_url.clone(),
                                detail: None,
                            },
                        },
                    )
                }));
                Some(
                    ChatCompletionRequestUserMessage {
                        content: ChatCompletionRequestUserMessageContent::Array(parts),
                        name: None,
                    }
                    .into(),
                )
            }
        }
        MessageRole::Assistant => {
            let tool_calls: Option<Vec<ChatCompletionMessageToolCalls>> = msg
                .tool_calls
                .as_ref()
                .and_then(|tc| serde_json::from_str(tc).ok());
            Some(
                CompatibleChatCompletionRequestAssistantMessage {
                    base: ChatCompletionRequestAssistantMessage {
                        content: if msg.content.is_empty() {
                            None
                        } else {
                            Some(ChatCompletionRequestAssistantMessageContent::Text(
                                msg.content.clone(),
                            ))
                        },
                        tool_calls,
                        ..Default::default()
                    },
                    reasoning_content: msg.reasoning_content.clone(),
                }
                .into(),
            )
        }
        MessageRole::Tool => Some(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(msg.content.clone()),
                tool_call_id: msg.tool_call_id.clone().unwrap_or_default(),
            }
            .into(),
        ),
    }
}
