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
    // Image attachments of user messages are extracted from Array content
    // and stored as JSON in the attachments column
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
                                // name only feeds prompt text in agent memory; not persisted
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
        mentions: None,
    }
}

/// Splice persisted `<system-reminder>` blocks back into the rebuilt user
/// message so request bytes stay identical across restarts (prompt-cache
/// stable). Blocks live in the row's `runtime_meta.system_reminder` array in
/// injection order; `content` itself is never polluted — UI/export read it
/// verbatim.
fn stored_user_reminders(msg: &StoredMessage) -> Vec<String> {
    msg.runtime_meta
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("system_reminder")
                .and_then(|b| b.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| b.as_str().map(String::from))
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// Serialized `runtime_meta` marking a user turn that entered in plan mode.
/// Written by TUI and Web persistence; read back by the plan badge parsers
/// (resume replay, share export) and the plan-context restore on rebuild
/// (see `stored_plan_mode`).
pub fn plan_mode_runtime_meta(on: bool) -> Option<String> {
    on.then(|| r#"{"plan_mode":true}"#.to_string())
}

/// Whether a stored user row entered in plan mode (`runtime_meta.plan_mode`).
/// The plan-context restore intentionally reads the last *active*
/// (non-compacted) user row: compaction wipes the rows carrying the plan
/// reminders from the rebuilt context, so their reminders need no exit
/// countermand.
pub fn stored_plan_mode(msg: &StoredMessage) -> bool {
    msg.runtime_meta
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("plan_mode").and_then(|v| v.as_bool()))
        .unwrap_or(false)
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
            // With attachments → rebuild Array content (Text + ImageUrl
            // parts) so images remain usable on history replay
            let attachments = msg
                .attachments
                .as_deref()
                .and_then(|json| {
                    serde_json::from_str::<Vec<crate::agent::media::Attachment>>(json).ok()
                })
                .unwrap_or_default();
            // Persisted reminders splice back exactly as injected in memory
            // ("\n\n"-joined into the Text content, separate trailing parts
            // in the Array form) so rebuilt requests are byte-identical
            let reminders = stored_user_reminders(msg);
            if attachments.is_empty() {
                let mut text = msg.content.clone();
                for r in &reminders {
                    text.push_str("\n\n");
                    text.push_str(r);
                }
                Some(
                    ChatCompletionRequestUserMessage {
                        content: ChatCompletionRequestUserMessageContent::Text(text),
                        name: None,
                    }
                    .into(),
                )
            } else {
                // Skip the Text part when text is empty (symmetric with
                // build_user_message; some compatible endpoints reject
                // empty text parts)
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
                for r in &reminders {
                    parts.push(ChatCompletionRequestUserMessageContentPart::Text(
                        ChatCompletionRequestMessageContentPartText {
                            text: format!("\n\n{r}"),
                        },
                    ));
                }
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
