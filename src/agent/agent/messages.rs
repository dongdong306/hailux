use std::collections::HashSet;
use std::sync::Arc;

use async_openai::types::chat::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestMessageContentPartImage, ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
    ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ImageUrl,
};

use crate::agent::media::Attachment;
use crate::agent::models::{
    CompatibleChatCompletionRequestAssistantMessage, CompatibleChatCompletionRequestMessage,
    SharedMessage,
};

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

/// 提取 tool call 的 id（Function / Custom 两种形态统一处理）。
fn tool_call_id(tc: &ChatCompletionMessageToolCalls) -> &str {
    match tc {
        ChatCompletionMessageToolCalls::Function(f) => &f.id,
        ChatCompletionMessageToolCalls::Custom(c) => &c.id,
    }
}

/// 请求前兜底：保证每个带 tool_calls 的 assistant 消息，其全部 tool_call_id
/// 都有对应的 tool result。悬挂的 tool_calls（会话中断/断连/崩溃遗留的脏数据，
/// 经 DB 重建进入上下文）会触发严格 Provider 400
///（"tool_calls must be followed by tool messages"）。
///
/// 仅在克隆列表上修补，不污染存储：
/// - 悬挂 id 在该 assistant 紧随的连续 tool results 之后补一条
///   "Tool execution aborted"（维持 tool 消息紧跟 assistant 且不打乱真实结果的顺序）
/// - 空 id 的残缺条目（流式中途取消的半成品，永远无法被应答）直接剔除
///
/// 干净上下文走快路径：一次反向判别扫描后原样返回（O(n) 枚举判别，零分配），
/// 相对每轮请求固有的序列化 + 网络 + 推理耗时，开销在噪声以下。
pub(super) fn ensure_tool_call_parity(messages: &[SharedMessage]) -> Vec<SharedMessage> {
    // ── 预检：反向扫描，位置 i 的 assistant 悬挂 id = 其后无任何 Tool 消息应答的 id ──
    // （"已应答"按全局 id 集合判：id 由 Provider 生成、跨 assistant 视为唯一，
    //   不做逐 assistant 区间配对——换取单次 O(n) 扫描的简单性）
    let mut answered_later: HashSet<String> = HashSet::new();
    let mut dangling: Vec<Vec<String>> = vec![Vec::new(); messages.len()];
    let mut has_empty_id: Vec<bool> = vec![false; messages.len()];
    let mut dirty = false;
    for (i, msg) in messages.iter().enumerate().rev() {
        match msg.as_ref() {
            CompatibleChatCompletionRequestMessage::Tool(t) => {
                answered_later.insert(t.tool_call_id.clone());
            }
            CompatibleChatCompletionRequestMessage::Assistant(a) => {
                if let Some(tcs) = &a.base.tool_calls {
                    for tc in tcs {
                        let id = tool_call_id(tc);
                        if id.is_empty() {
                            has_empty_id[i] = true;
                            dirty = true;
                        } else if !answered_later.contains(id) {
                            dangling[i].push(id.to_string());
                            dirty = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if !dirty {
        return messages.to_vec();
    }

    // ── 重建：剔除空 id 条目；在每个 assistant 的连续 tool results 之后插入 stub ──
    // （stub 不能直接追加到列表末尾：悬挂 assistant 之后可能还有后续轮次的消息，
    //   tool 消息必须紧跟带 tool_calls 的 assistant 才满足 OpenAI 交替约束）
    let mut out: Vec<SharedMessage> = Vec::with_capacity(messages.len() + 4);
    let mut i = 0usize;
    while i < messages.len() {
        if let CompatibleChatCompletionRequestMessage::Assistant(a) = messages[i].as_ref() {
            let assistant_pos = i;
            if has_empty_id[assistant_pos] {
                let mut rebuilt: ChatCompletionRequestAssistantMessage = a.base.clone();
                if let Some(tcs) = &mut rebuilt.tool_calls {
                    tcs.retain(|tc| !tool_call_id(tc).is_empty());
                    if tcs.is_empty() {
                        rebuilt.tool_calls = None;
                    }
                }
                out.push(Arc::new(
                    CompatibleChatCompletionRequestAssistantMessage {
                        base: rebuilt,
                        reasoning_content: a.reasoning_content.clone(),
                    }
                    .into(),
                ));
            } else {
                out.push(Arc::clone(&messages[i]));
            }
            i += 1;
            // 紧随的连续 tool results 原样保留，stub 插在其后
            while i < messages.len()
                && matches!(
                    messages[i].as_ref(),
                    CompatibleChatCompletionRequestMessage::Tool(_)
                )
            {
                out.push(Arc::clone(&messages[i]));
                i += 1;
            }
            for id in &dangling[assistant_pos] {
                out.push(Arc::new(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(
                            "Tool execution aborted".to_string(),
                        ),
                        tool_call_id: id.clone(),
                    }
                    .into(),
                ));
            }
        } else {
            out.push(Arc::clone(&messages[i]));
            i += 1;
        }
    }
    out
}
