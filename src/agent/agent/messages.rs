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

/// Placeholder text shown to models without vision when they receive an
/// image (the model relays it to the user, avoiding a direct Provider 400).
fn unsupported_image_text() -> String {
    "ERROR: Cannot read the attached image (this model does not support vision input). Inform the user.".to_string()
}

/// Placeholder text for empty/corrupted image attachments.
fn corrupted_image_text() -> String {
    "ERROR: Image file is empty or corrupted. Inform the user.".to_string()
}

/// Builds an image content part (data URL).
pub(super) fn image_content_part(att: &Attachment) -> ChatCompletionRequestUserMessageContentPart {
    ChatCompletionRequestUserMessageContentPart::ImageUrl(
        ChatCompletionRequestMessageContentPartImage {
            image_url: ImageUrl {
                url: att.data_url.clone(),
                detail: None,
            },
            prompt_cache_breakpoint: None,
        },
    )
}

/// Fallback text: used when no attachment carries a source identifier.
///
/// Note: the synthetic user message's hint text only helps the model
/// understand where images come from; the display layer identifies
/// synthesized messages solely via the runtime_meta
/// `{"format":"tool-attachment"}` marker, never by text matching.
pub(super) const TOOL_ATTACHMENT_NOTE_FALLBACK: &str = "[Attached image(s) from tool result]";

/// Builds the hint text of the synthetic user message, helping the model
/// associate images with their source tools:
/// - Single tool: `[Image(s) attached from tool "read": a.png, b.png]`
/// - Multiple tools: `[Image(s) attached from tool results: read -> a.png; web_fetch -> b.png]`
/// - Unnamed items are counted as placeholders; when all are unnamed, falls
///   back to the fallback constant.
pub(super) fn tool_attachment_note(items: &[(String, Attachment)]) -> String {
    if items.iter().all(|(_, a)| a.name.is_none()) {
        return TOOL_ATTACHMENT_NOTE_FALLBACK.to_string();
    }
    // Group by tool (preserving first-occurrence order)
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

/// Builds a user message: no attachments → plain text; with attachments →
/// Text + ImageUrl content parts (the Text part is skipped when the text
/// is empty — some OpenAI-compatible endpoints reject empty text parts).
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
                    prompt_cache_breakpoint: None,
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

/// Append transient text to the last User message (shared injection point
/// for the plan-mode prompt and the previous-run failure system-reminder).
/// Clone-on-write via `Arc::make_mut`: callers either operate on a cloned
/// list (plan mode) or on a just-pushed user message with refcount 1 (error
/// reminder), so stored history and shared in-memory references are never
/// polluted. No-op when the list has no User message.
pub(super) fn append_text_to_last_user_message(messages: &mut [SharedMessage], text: &str) {
    let Some(idx) = messages
        .iter()
        .rposition(|m| matches!(m.as_ref(), CompatibleChatCompletionRequestMessage::User(_)))
    else {
        return;
    };
    let msg_mut = Arc::make_mut(&mut messages[idx]);
    let CompatibleChatCompletionRequestMessage::User(u) = msg_mut else {
        return;
    };
    match &mut u.content {
        ChatCompletionRequestUserMessageContent::Text(t) => {
            t.push_str("\n\n");
            t.push_str(text);
        }
        // Array form (image attachments): push a separate text part
        ChatCompletionRequestUserMessageContent::Array(parts) => {
            parts.push(ChatCompletionRequestUserMessageContentPart::Text(
                ChatCompletionRequestMessageContentPartText {
                    text: format!("\n\n{text}"),
                    prompt_cache_breakpoint: None,
                },
            ));
        }
    }
}

/// Sanitizes image attachments in user messages according to the model's
/// vision capability.
/// - Vision supported: only checks for empty base64, replaced with the
///   corrupted-image text
/// - Vision unsupported: all images replaced with the "unsupported" text
///   (operates on a clone, never polluting the stored history)
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
            // Rebuild parts by reference: clone only text parts, avoiding a
            // deep copy of the soon-to-be-discarded base64 payload (a single
            // image can reach ~13MB)
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
                            ChatCompletionRequestMessageContentPartText {
                                text,
                                prompt_cache_breakpoint: None,
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
        })
        .collect()
}

/// Whether a user message contains an image part that needs downgrading
/// (model without vision, or an empty base64 payload).
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

/// Whether the base64 payload of a data URL is empty.
fn empty_image_payload(data_url: &str) -> bool {
    crate::agent::media::data_url_payload_is_empty(data_url)
}

/// Media downgrade for compaction: image parts are replaced with placeholder
/// text (the summary needs no image data, and the compaction request should
/// not carry bulky base64 payloads).
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
    // Rebuild parts by reference: clone only text parts, without carrying
    // over the soon-to-be-discarded base64 payload
    let new_parts: Vec<_> = parts
        .iter()
        .map(|p| match p {
            ChatCompletionRequestUserMessageContentPart::ImageUrl(_) => {
                ChatCompletionRequestUserMessageContentPart::Text(
                    ChatCompletionRequestMessageContentPartText {
                        text: "[Attached image: removed for context compaction]".to_string(),
                        prompt_cache_breakpoint: None,
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

/// Extracts a tool call's id (handles Function and Custom forms alike).
fn tool_call_id(tc: &ChatCompletionMessageToolCalls) -> &str {
    match tc {
        ChatCompletionMessageToolCalls::Function(f) => &f.id,
        ChatCompletionMessageToolCalls::Custom(c) => &c.id,
    }
}

/// Pre-request fallback: guarantees that every tool_call_id of each
/// assistant message carrying tool_calls has a matching tool result.
/// Dangling tool_calls (dirty data left behind by interrupted sessions,
/// disconnects, or crashes, re-entering the context via DB rebuild) trigger
/// a strict Provider 400
/// ("tool_calls must be followed by tool messages").
///
/// Patched only on a cloned list, never polluting storage:
/// - Dangling ids get a "Tool execution aborted" stub inserted after the
///   consecutive tool results immediately following that assistant
///   (tool messages stay adjacent to the assistant and real results keep
///   their order)
/// - Entries with empty ids (half-written artifacts cancelled mid-stream,
///   which can never be answered) are dropped outright
///
/// Clean contexts take the fast path: a single reverse scan returns the
/// list as-is (O(n) discriminant checks, zero allocations); the overhead is
/// noise compared to the serialization + network + inference time inherent
/// to every request.
pub(super) fn ensure_tool_call_parity(messages: &[SharedMessage]) -> Vec<SharedMessage> {
    // ── Pre-check: reverse scan; a dangling id at assistant position i =
    // one never answered by any Tool message after it ──
    // ("answered" is judged against the global id set: ids are generated by
    // the Provider and unique across assistants, skipping per-assistant
    // interval pairing in exchange for the simplicity of a single O(n) scan)
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

    // ── Rebuild: drop empty-id entries; insert stubs after each assistant's
    // consecutive tool results ──
    // (stubs cannot simply be appended to the end of the list: later turns
    // may follow the dangling assistant, and tool messages must come right
    // after the assistant carrying tool_calls to satisfy OpenAI's
    // alternation constraint)
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
            // Consecutive tool results right after it stay as-is; stubs are
            // inserted after them
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
