use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
    ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, FunctionCall,
};

use super::messages::{
    TOOL_ATTACHMENT_NOTE_FALLBACK, build_user_message, sanitize_messages_for_request,
    tool_attachment_note,
};
use super::stream::AgentStreamState;
use crate::agent::media::Attachment;
use crate::agent::models::{CompatibleChatCompletionRequestMessage, SharedMessage};
use crate::agent::retry::RetryConfig;
use crate::agent::tools::{ToolExecuteError, ToolExecution, ToolRegistry};

use super::Agent;

fn make_test_agent() -> Agent {
    let config = OpenAIConfig::new().with_api_base("http://localhost:0");
    let pm =
        crate::permission::PermissionManager::new(crate::permission::PermissionMode::Yolo, vec![]);
    Agent::new(
        config,
        "test-model",
        "test/test-model",
        4096,
        false,
        pm,
        ".",
        RetryConfig::default(),
    )
}

#[test]
fn apply_compaction_keeps_system_adds_summary() {
    let mut agent = make_test_agent();
    agent.set_system_prompt("You are test");

    agent.messages.push(Arc::new(
        ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Text("hello".into()),
            name: None,
        }
        .into(),
    ));

    agent.apply_compaction("这是摘要");

    assert_eq!(agent.messages.len(), 2);
    assert!(matches!(
        agent.messages[0].as_ref(),
        CompatibleChatCompletionRequestMessage::System(_)
    ));
    match agent.messages[1].as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => {
            let text = match &u.content {
                ChatCompletionRequestUserMessageContent::Text(t) => t.clone(),
                _ => panic!("expected text content"),
            };
            assert!(text.contains("这是摘要"));
            assert!(text.contains("[Context Summary]"));
        }
        _ => panic!("expected User message"),
    }
}

#[test]
fn apply_compaction_without_system() {
    let mut agent = make_test_agent();
    agent.apply_compaction("摘要");
    assert_eq!(agent.messages.len(), 1);
    assert!(matches!(
        agent.messages[0].as_ref(),
        CompatibleChatCompletionRequestMessage::User(_)
    ));
}

#[test]
fn apply_compaction_then_append() {
    let mut agent = make_test_agent();
    agent.set_system_prompt("sys");
    agent.apply_compaction("摘要");

    agent.messages.push(Arc::new(
        ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Text("新问题".into()),
            name: None,
        }
        .into(),
    ));

    assert_eq!(agent.messages.len(), 3);
    assert!(matches!(
        agent.messages[0].as_ref(),
        CompatibleChatCompletionRequestMessage::System(_)
    ));
    assert!(matches!(
        agent.messages[1].as_ref(),
        CompatibleChatCompletionRequestMessage::User(_)
    ));
    assert!(matches!(
        agent.messages[2].as_ref(),
        CompatibleChatCompletionRequestMessage::User(_)
    ));
}

// ── 图片附件 ─────────────────────────────────────────

fn user_msg(text: &str) -> SharedMessage {
    Arc::new(build_user_message(text, &[]))
}

fn png_attachment() -> Attachment {
    let png_bytes: [u8; 16] = [
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4, 5, 6, 7, 8,
    ];
    Attachment::from_bytes("image/png", &png_bytes)
}

fn named_attachment(name: &str) -> Attachment {
    Attachment {
        name: Some(name.to_string()),
        ..png_attachment()
    }
}

// ── 工具附件合成消息的提示文本 ─────────────────────────

#[test]
fn tool_attachment_note_single_tool_lists_names() {
    let items = vec![
        ("read".to_string(), named_attachment("a.png")),
        ("read".to_string(), named_attachment("b.png")),
    ];
    assert_eq!(
        tool_attachment_note(&items),
        "[Image(s) attached from tool \"read\": a.png, b.png]"
    );
}

#[test]
fn tool_attachment_note_multiple_tools_grouped() {
    let items = vec![
        ("read".to_string(), named_attachment("a.png")),
        ("web_fetch".to_string(), named_attachment("b.jpg")),
        ("read".to_string(), named_attachment("c.png")),
    ];
    assert_eq!(
        tool_attachment_note(&items),
        "[Image(s) attached from tool results: read -> a.png, c.png; web_fetch -> b.jpg]"
    );
}

#[test]
fn tool_attachment_note_unnamed_counted_and_fallback() {
    let items = vec![
        ("read".to_string(), named_attachment("a.png")),
        ("read".to_string(), png_attachment()),
    ];
    assert_eq!(
        tool_attachment_note(&items),
        "[Image(s) attached from tool \"read\": a.png, 1 image(s)]"
    );

    let all_unnamed = vec![
        ("read".to_string(), png_attachment()),
        ("read".to_string(), png_attachment()),
    ];
    assert_eq!(
        tool_attachment_note(&all_unnamed),
        TOOL_ATTACHMENT_NOTE_FALLBACK
    );
}

#[test]
fn build_user_message_with_attachments_uses_array_content() {
    let msg = build_user_message("看这张图", &[png_attachment()]);
    match &msg {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Array(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(
                    parts[0],
                    ChatCompletionRequestUserMessageContentPart::Text(_)
                ));
                assert!(matches!(
                    parts[1],
                    ChatCompletionRequestUserMessageContentPart::ImageUrl(_)
                ));
            }
            _ => panic!("expected array content"),
        },
        _ => panic!("expected User message"),
    }
}

#[test]
fn sanitize_vision_unsupported_replaces_images_with_error_text() {
    let msgs = vec![
        user_msg("看这张图"),
        Arc::new(build_user_message("", &[png_attachment()])),
    ];
    let sanitized = sanitize_messages_for_request(&msgs, false);
    assert_eq!(sanitized.len(), 2);
    match sanitized[1].as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Array(parts) => {
                assert!(
                    parts
                        .iter()
                        .all(|p| matches!(p, ChatCompletionRequestUserMessageContentPart::Text(_)))
                );
                let joined: String = parts
                    .iter()
                    .map(|p| match p {
                        ChatCompletionRequestUserMessageContentPart::Text(t) => t.text.clone(),
                        _ => String::new(),
                    })
                    .collect();
                assert!(joined.contains("does not support vision"));
            }
            _ => panic!("expected array content"),
        },
        _ => panic!("expected User message"),
    }
}

#[test]
fn sanitize_vision_supported_keeps_images() {
    let msgs = vec![Arc::new(build_user_message("图", &[png_attachment()]))];
    let sanitized = sanitize_messages_for_request(&msgs, true);
    match sanitized[0].as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Array(parts) => {
                assert!(matches!(
                    parts[1],
                    ChatCompletionRequestUserMessageContentPart::ImageUrl(_)
                ));
            }
            _ => panic!("expected array content"),
        },
        _ => panic!("expected User message"),
    }
}

#[test]
fn sanitize_empty_payload_replaced_even_with_vision() {
    let corrupted = Attachment {
        mime: "image/png".to_string(),
        data_url: "data:image/png;base64,".to_string(),
        name: None,
    };
    let msgs = vec![Arc::new(build_user_message("空图", &[corrupted]))];
    let sanitized = sanitize_messages_for_request(&msgs, true);
    match sanitized[0].as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Array(parts) => {
                let joined: String = parts
                    .iter()
                    .map(|p| match p {
                        ChatCompletionRequestUserMessageContentPart::Text(t) => t.text.clone(),
                        _ => String::new(),
                    })
                    .collect();
                assert!(joined.contains("empty or corrupted"));
            }
            _ => panic!("expected array content"),
        },
        _ => panic!("expected User message"),
    }
}

#[test]
fn sanitize_plain_text_messages_untouched() {
    let msgs = vec![user_msg("纯文本"), user_msg("第二条")];
    let sanitized = sanitize_messages_for_request(&msgs, false);
    assert_eq!(sanitized.len(), 2);
    // Arc 未被克隆重建（同一指针）时无需修复 —— 仅校验内容不变
    match sanitized[0].as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => assert_eq!(t, "纯文本"),
            _ => panic!("expected text content"),
        },
        _ => panic!("expected User message"),
    }
}

// ── 并行工具调用的附件注入 ──────────────────────────────

/// 返回固定图片附件的测试工具（permission_category 为 None，跳过权限检查）
struct AttachmentTool;

impl crate::agent::tools::Tool for AttachmentTool {
    fn name(&self) -> &str {
        "attachment_tool"
    }
    fn description(&self) -> &str {
        "returns an image attachment"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn execute_async<'a>(
        &'a self,
        _arguments: &'a str,
        _work_dir: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, ToolExecuteError>> + Send + 'a>,
    > {
        Box::pin(async { Ok("image loaded".to_string()) })
    }
    fn execute_async_with_display<'a>(
        &'a self,
        _arguments: &'a str,
        _work_dir: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolExecution, ToolExecuteError>> + Send + 'a>,
    > {
        Box::pin(async move {
            Ok(ToolExecution {
                content: "image loaded".to_string(),
                display: None,
                attachments: vec![png_attachment()],
            })
        })
    }
}

fn tool_call(id: &str, name: &str) -> ChatCompletionMessageToolCalls {
    ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
        id: id.to_string(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: "{}".to_string(),
        },
    })
}

fn test_stream_state(supports_vision: bool) -> AgentStreamState {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(AttachmentTool));
    AgentStreamState {
        client: Client::with_config(OpenAIConfig::new().with_api_base("http://localhost:0")),
        tool_registry: registry,
        messages: Vec::new(),
        model: "test-model".to_string(),
        model_display: "test/test-model".to_string(),
        max_tokens: 4096,
        supports_vision,
        plan_mode: false,
        permission: crate::permission::PermissionManager::new(
            crate::permission::PermissionMode::Yolo,
            vec![],
        ),
        work_dir: ".".to_string(),
        retry: RetryConfig::default(),
    }
}

#[tokio::test]
async fn parallel_tool_attachments_go_after_all_tool_messages() {
    let (event_tx, _rx) = crate::agent::event::create_core_event_channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = test_stream_state(true);
    let calls = vec![
        tool_call("call_1", "attachment_tool"),
        tool_call("call_2", "attachment_tool"),
    ];

    super::dispatch::handle_tool_calls_stream(&mut state, &calls, &cancel, &event_tx)
        .await
        .unwrap();

    // 期望序列：tool → tool → user(合成)。合成 user 消息若插在两条
    // tool 消息之间，严格 Provider 会以 400 拒绝整轮请求。
    assert_eq!(state.messages.len(), 3);
    assert!(matches!(
        state.messages[0].as_ref(),
        CompatibleChatCompletionRequestMessage::Tool(_)
    ));
    assert!(matches!(
        state.messages[1].as_ref(),
        CompatibleChatCompletionRequestMessage::Tool(_)
    ));
    match state.messages[2].as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Array(parts) => {
                let image_count = parts
                    .iter()
                    .filter(|p| {
                        matches!(p, ChatCompletionRequestUserMessageContentPart::ImageUrl(_))
                    })
                    .count();
                assert_eq!(image_count, 2, "两张图片应合并进同一条合成消息");
            }
            _ => panic!("expected array content"),
        },
        _ => panic!("expected synthetic User message"),
    }
}

#[tokio::test]
async fn text_only_tool_calls_produce_no_synthetic_message() {
    let (event_tx, _rx) = crate::agent::event::create_core_event_channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut state = test_stream_state(true);
    let calls = vec![tool_call("call_1", "unknown_tool")];

    super::dispatch::handle_tool_calls_stream(&mut state, &calls, &cancel, &event_tx)
        .await
        .unwrap();

    assert_eq!(state.messages.len(), 1);
    assert!(matches!(
        state.messages[0].as_ref(),
        CompatibleChatCompletionRequestMessage::Tool(_)
    ));
}
