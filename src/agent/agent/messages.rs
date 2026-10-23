use std::sync::Arc;

use async_openai::types::chat::{
    ChatCompletionRequestMessageContentPartImage, ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ImageUrl,
};

use crate::agent::media::Attachment;
use crate::agent::models::{CompatibleChatCompletionRequestMessage, SharedMessage};

/// 不支持视觉的模型收到图片时看到的占位文本
/// （让模型自行转告用户，避免直接触发 Provider 400）。
fn unsupported_image_text() -> String {
    "ERROR: Cannot read the attached image (this model does not support vision input). Inform the user.".to_string()
}

/// 空的/损坏的图片附件占位文本。
fn corrupted_image_text() -> String {
    "ERROR: Image file is empty or corrupted. Inform the user.".to_string()
}

/// 构造图片 content part（data URL）。
pub(super) fn image_content_part(att: &Attachment) -> ChatCompletionRequestUserMessageContentPart {
    ChatCompletionRequestUserMessageContentPart::ImageUrl(
        ChatCompletionRequestMessageContentPartImage {
            image_url: ImageUrl {
                url: att.data_url.clone(),
                detail: None,
            },
        },
    )
}

/// 兜底文本：附件全部无来源标识时使用。
///
/// 注意：合成 user 消息的提示文本仅供模型理解图片来源；展示层识别
/// 一律走 runtime_meta 的 `{"format":"tool-attachment"}` 标记，不做文本匹配。
pub(super) const TOOL_ATTACHMENT_NOTE_FALLBACK: &str = "[Attached image(s) from tool result]";

/// 生成合成 user 消息的提示文本，帮助模型把图片关联到来源工具：
/// - 单工具：`[Image(s) attached from tool "read": a.png, b.png]`
/// - 多工具：`[Image(s) attached from tool results: read -> a.png; web_fetch -> b.png]`
/// - 无 name 的项以计数占位；全部无名退回兜底常量。
pub(super) fn tool_attachment_note(items: &[(String, Attachment)]) -> String {
    if items.iter().all(|(_, a)| a.name.is_none()) {
        return TOOL_ATTACHMENT_NOTE_FALLBACK.to_string();
    }
    // 按工具分组（保持首次出现顺序）
    let mut groups: Vec<(&str, Vec<&Attachment>)> = Vec::new();
    for (tool, att) in items {
        match groups.iter_mut().find(|(t, _)| *t == tool.as_str()) {
            Some((_, atts)) => atts.push(att),
            None => groups.push((tool.as_str(), vec![att])),
        }
    }
    let describe = |atts: &[&Attachment]| -> String {
        let named: Vec<&str> = atts.iter().filter_map(|a| a.name.as_deref()).collect();
        let unnamed = atts.len() - named.len();
        let mut parts: Vec<String> = named.iter().map(|n| (*n).to_string()).collect();
        if unnamed > 0 {
            parts.push(format!("{unnamed} image(s)"));
        }
        parts.join(", ")
    };
    if groups.len() == 1 {
        let (tool, atts) = &groups[0];
        format!(
            "[Image(s) attached from tool \"{tool}\": {}]",
            describe(atts)
        )
    } else {
        let segments: Vec<String> = groups
            .iter()
            .map(|(tool, atts)| format!("{tool} -> {}", describe(atts)))
            .collect();
        format!(
            "[Image(s) attached from tool results: {}]",
            segments.join("; ")
        )
    }
}

/// 构造用户消息：无附件 → 纯文本；有附件 → Text + ImageUrl content parts
/// （文本为空时跳过 Text part——部分 OpenAI 兼容端点拒绝空 text part）。
pub fn build_user_message(
    text: &str,
    attachments: &[Attachment],
) -> CompatibleChatCompletionRequestMessage {
    if attachments.is_empty() {
        ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Text(text.to_string()),
            name: None,
        }
        .into()
    } else {
        let mut parts = Vec::new();
        if !text.is_empty() {
            parts.push(ChatCompletionRequestUserMessageContentPart::Text(
                ChatCompletionRequestMessageContentPartText {
                    text: text.to_string(),
                },
            ));
        }
        parts.extend(attachments.iter().map(image_content_part));
        ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Array(parts),
            name: None,
        }
        .into()
    }
}

/// 按模型视觉能力清洗消息列表中用户消息的图片附件。
/// - 支持视觉：仅校验空 base64，替换为损坏提示文本
/// - 不支持视觉：所有图片替换为「不支持」提示文本（在克隆上操作，不污染存储历史）
pub(super) fn sanitize_messages_for_request(
    messages: &[SharedMessage],
    supports_vision: bool,
) -> Vec<SharedMessage> {
    let needs_fix = messages
        .iter()
        .any(|msg| user_message_needs_vision_fix(msg, supports_vision));
    if !needs_fix {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|msg| {
            if !user_message_needs_vision_fix(msg, supports_vision) {
                return Arc::clone(msg);
            }
            // 按引用重建 parts：只克隆文本 part，避免深拷贝即将被丢弃的
            // base64 载荷（单图可达 ~13MB）
            let CompatibleChatCompletionRequestMessage::User(u) = msg.as_ref() else {
                return Arc::clone(msg);
            };
            let ChatCompletionRequestUserMessageContent::Array(parts) = &u.content else {
                return Arc::clone(msg);
            };
            let new_parts: Vec<_> = parts
                .iter()
                .map(|p| match p {
                    ChatCompletionRequestUserMessageContentPart::ImageUrl(img) => {
                        let text = if empty_image_payload(&img.image_url.url) {
                            corrupted_image_text()
                        } else {
                            unsupported_image_text()
                        };
                        ChatCompletionRequestUserMessageContentPart::Text(
                            ChatCompletionRequestMessageContentPartText { text },
                        )
                    }
                    other => other.clone(),
                })
                .collect();
            Arc::new(
                ChatCompletionRequestUserMessage {
                    content: ChatCompletionRequestUserMessageContent::Array(new_parts),
                    name: u.name.clone(),
                }
                .into(),
            )
        })
        .collect()
}

/// 用户消息是否含有需要降级的图片 part（模型不支持视觉，或 base64 载荷为空）。
fn user_message_needs_vision_fix(msg: &SharedMessage, supports_vision: bool) -> bool {
    matches!(
        msg.as_ref(),
        CompatibleChatCompletionRequestMessage::User(u)
            if matches!(&u.content, ChatCompletionRequestUserMessageContent::Array(parts)
                if parts.iter().any(|p| match p {
                    ChatCompletionRequestUserMessageContentPart::ImageUrl(img) =>
                        !supports_vision || empty_image_payload(&img.image_url.url),
                    _ => false,
                }))
    )
}

/// 判断 data URL 的 base64 载荷是否为空。
fn empty_image_payload(data_url: &str) -> bool {
    crate::agent::media::data_url_payload_is_empty(data_url)
}

/// 压缩场景的媒体降级：图片 part 替换为占位文本（摘要无需图片本体，
/// 且压缩请求不应携带大体量 base64）。
pub(super) fn strip_media_for_compaction(msg: &SharedMessage) -> SharedMessage {
    let CompatibleChatCompletionRequestMessage::User(u) = msg.as_ref() else {
        return Arc::clone(msg);
    };
    let ChatCompletionRequestUserMessageContent::Array(parts) = &u.content else {
        return Arc::clone(msg);
    };
    if !parts
        .iter()
        .any(|p| matches!(p, ChatCompletionRequestUserMessageContentPart::ImageUrl(_)))
    {
        return Arc::clone(msg);
    }
    // 按引用重建 parts：只克隆文本 part，不搬运即将被丢弃的 base64 载荷
    let new_parts: Vec<_> = parts
        .iter()
        .map(|p| match p {
            ChatCompletionRequestUserMessageContentPart::ImageUrl(_) => {
                ChatCompletionRequestUserMessageContentPart::Text(
                    ChatCompletionRequestMessageContentPartText {
                        text: "[Attached image: removed for context compaction]".to_string(),
                    },
                )
            }
            other => other.clone(),
        })
        .collect();
    Arc::new(
        ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Array(new_parts),
            name: u.name.clone(),
        }
        .into(),
    )
}
