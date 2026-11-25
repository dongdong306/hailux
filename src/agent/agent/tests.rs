use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
    ChatCompletionRequestAssistantMessage, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart,
    FunctionCall,
};

use super::messages::{
    TOOL_ATTACHMENT_NOTE_FALLBACK, build_user_message, ensure_tool_call_parity,
    sanitize_messages_for_request, tool_attachment_note,
};
use super::stream::{AgentStreamState, PartialToolCall};
use crate::agent::media::Attachment;
use crate::agent::models::{
    CompatibleChatCompletionRequestAssistantMessage, CompatibleChatCompletionRequestMessage,
    SharedMessage,
};
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
        true,
        crate::agent::models::ReasoningEffort::default(),
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
    match sanitized[0].as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => assert_eq!(t, "纯文本"),
            _ => panic!("expected text content"),
        },
        _ => panic!("expected User message"),
    }
}

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
    test_stream_state_with(supports_vision, true)
}

fn test_stream_state_with(supports_vision: bool, native_reasoning: bool) -> AgentStreamState {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(AttachmentTool));
    AgentStreamState {
        client: Client::with_config(OpenAIConfig::new().with_api_base("http://localhost:0")),
        tool_registry: registry,
        messages: Vec::new(),
        session_id: "test-session".to_string(),
        model: "test-model".to_string(),
        model_display: "test/test-model".to_string(),
        max_tokens: 4096,
        supports_vision,
        native_reasoning,
        reasoning_effort: crate::agent::models::ReasoningEffort::default(),
        plan_mode: false,
        permission: crate::permission::PermissionManager::new(
            crate::permission::PermissionMode::Yolo,
            vec![],
        ),
        work_dir: ".".to_string(),
        retry: RetryConfig::default(),
        interject_rx: tokio::sync::mpsc::channel(1).1,
        delivered_interject_ids: std::collections::HashSet::new(),
    }
}

#[test]
fn build_request_native_sends_thinking_and_raw_effort() {
    let mut state = test_stream_state_with(false, true);
    state.reasoning_effort = crate::agent::models::ReasoningEffort::Max;
    let req = state.build_request().unwrap().build().unwrap();
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["thinking"]["type"], "enabled");
    assert_eq!(json["reasoning_effort"], "max");
}

#[test]
fn build_request_openai_passthrough_effort_and_drops_thinking() {
    let mut state = test_stream_state_with(false, false);
    state.reasoning_effort = crate::agent::models::ReasoningEffort::Max;
    let req = state.build_request().unwrap().build().unwrap();
    let json = serde_json::to_value(&req).unwrap();
    assert!(json.get("thinking").is_none());
    assert_eq!(json["reasoning_effort"], "max");
    assert_eq!(json["stream_options"]["include_usage"], true);
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

    // Expected order: tool -> tool -> user(synthesized). Inserting the
    // synthesized user message between the two tool messages would 400 the whole request on strict providers.
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

fn assistant_with_tool_calls(ids: &[&str]) -> SharedMessage {
    Arc::new(
        CompatibleChatCompletionRequestAssistantMessage {
            base: ChatCompletionRequestAssistantMessage {
                tool_calls: Some(ids.iter().map(|id| tool_call(id, "read")).collect()),
                ..Default::default()
            },
            reasoning_content: None,
        }
        .into(),
    )
}

fn tool_result_msg(id: &str) -> SharedMessage {
    Arc::new(
        ChatCompletionRequestToolMessage {
            content: ChatCompletionRequestToolMessageContent::Text("ok".to_string()),
            tool_call_id: id.to_string(),
        }
        .into(),
    )
}

fn tool_call_ids(msg: &SharedMessage) -> Vec<String> {
    match msg.as_ref() {
        CompatibleChatCompletionRequestMessage::Assistant(a) => a
            .base
            .tool_calls
            .as_ref()
            .map(|tcs| {
                tcs.iter()
                    .map(|tc| match tc {
                        ChatCompletionMessageToolCalls::Function(f) => f.id.clone(),
                        ChatCompletionMessageToolCalls::Custom(c) => c.id.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => panic!("expected Assistant message"),
    }
}

#[test]
fn tool_parity_clean_context_untouched() {
    let msgs = vec![
        user_msg("hi"),
        assistant_with_tool_calls(&["call_1", "call_2"]),
        tool_result_msg("call_1"),
        tool_result_msg("call_2"),
        user_msg("next"),
    ];
    let out = ensure_tool_call_parity(&msgs);
    assert_eq!(out.len(), msgs.len());
    assert_eq!(tool_call_ids(&out[1]), vec!["call_1", "call_2"]);
}

#[test]
fn tool_parity_fills_missing_results_in_place() {
    let msgs = vec![
        user_msg("hi"),
        assistant_with_tool_calls(&["call_1", "call_2", "call_3"]),
        tool_result_msg("call_1"),
        user_msg("next"),
    ];
    let out = ensure_tool_call_parity(&msgs);
    assert_eq!(out.len(), 6);
    for (i, id) in ["call_2", "call_3"].iter().enumerate() {
        match out[3 + i].as_ref() {
            CompatibleChatCompletionRequestMessage::Tool(t) => {
                assert_eq!(t.tool_call_id, *id);
                assert!(matches!(
                    &t.content,
                    ChatCompletionRequestToolMessageContent::Text(s)
                        if s == "Tool execution aborted"
                ));
            }
            _ => panic!("expected Tool stub"),
        }
    }
    assert!(matches!(
        out[5].as_ref(),
        CompatibleChatCompletionRequestMessage::User(_)
    ));
}

#[test]
fn tool_parity_drops_empty_id_entries() {
    let msgs = vec![
        user_msg("hi"),
        assistant_with_tool_calls(&["", "call_1"]),
        tool_result_msg("call_1"),
    ];
    let out = ensure_tool_call_parity(&msgs);
    assert_eq!(out.len(), msgs.len());
    assert_eq!(tool_call_ids(&out[1]), vec!["call_1".to_string()]);
}

#[test]
fn tool_parity_pure_empty_id_assistant_drops_tool_calls() {
    let msgs = vec![user_msg("hi"), assistant_with_tool_calls(&[""])];
    let out = ensure_tool_call_parity(&msgs);
    assert_eq!(out.len(), msgs.len());
    assert_eq!(tool_call_ids(&out[1]), Vec::<String>::new());
}

#[tokio::test]
async fn cancelled_tool_calls_all_get_stub_results() {
    let (event_tx, _rx) = crate::agent::event::create_core_event_channel();
    let cancel = Arc::new(AtomicBool::new(true));
    let mut state = test_stream_state(true);
    let calls = vec![
        tool_call("call_1", "attachment_tool"),
        tool_call("call_2", "attachment_tool"),
        tool_call("call_3", "attachment_tool"),
    ];

    super::dispatch::handle_tool_calls_stream(&mut state, &calls, &cancel, &event_tx)
        .await
        .unwrap();

    assert_eq!(state.messages.len(), 3);
    for (i, id) in ["call_1", "call_2", "call_3"].iter().enumerate() {
        match state.messages[i].as_ref() {
            CompatibleChatCompletionRequestMessage::Tool(t) => {
                assert_eq!(t.tool_call_id, *id);
                assert!(matches!(
                    &t.content,
                    ChatCompletionRequestToolMessageContent::Text(s)
                        if s == "Tool execution aborted"
                ));
            }
            _ => panic!("expected Tool stub"),
        }
    }
}

#[tokio::test]
async fn push_partial_assistant_skips_idless_tool_calls() {
    let (event_tx, _rx) = crate::agent::event::create_core_event_channel();
    let mut state = test_stream_state(true);
    let mut map = BTreeMap::new();
    map.insert(
        0u32,
        PartialToolCall {
            id: "call_1".to_string(),
            name: Some("read".to_string()),
            arguments: "{}".to_string(),
        },
    );
    map.insert(
        1u32,
        PartialToolCall {
            id: String::new(),
            name: Some("read".to_string()),
            arguments: "{}".to_string(),
        },
    );

    super::stream::push_partial_assistant(&mut state, "", "", &map, &event_tx)
        .await
        .unwrap();

    assert_eq!(state.messages.len(), 2);
    assert_eq!(
        tool_call_ids(&state.messages[0]),
        vec!["call_1".to_string()]
    );
    assert!(matches!(
        state.messages[1].as_ref(),
        CompatibleChatCompletionRequestMessage::Tool(_)
    ));
}

#[test]
fn agent_interject_fails_without_active_run() {
    let agent = make_test_agent();
    assert!(!agent.interject(crate::agent::event::Interjection {
        client_id: "c1".to_string(),
        content: "hello".to_string(),
        attachments: vec![],
        mentions: Vec::new(),
    }));
}

#[tokio::test]
async fn drain_interjections_appends_user_messages_and_emits_events() {
    let mut state = test_stream_state(false);
    let (interject_tx, interject_rx) = tokio::sync::mpsc::channel(8);
    state.interject_rx = interject_rx;
    let (event_tx, mut event_rx) = crate::agent::event::create_core_event_channel();

    interject_tx
        .send(crate::agent::event::Interjection {
            client_id: "c1".to_string(),
            content: "插话一".to_string(),
            attachments: vec![],
            mentions: Vec::new(),
        })
        .await
        .unwrap();
    interject_tx
        .send(crate::agent::event::Interjection {
            client_id: "c2".to_string(),
            content: "插话二".to_string(),
            attachments: vec![Attachment::from_bytes(
                "image/png",
                &[0x89, b'P', b'N', b'G'],
            )],
            mentions: vec![crate::agent::event::Mention {
                start: 0,
                end: 2,
                kind: "file".to_string(),
            }],
        })
        .await
        .unwrap();

    let injected = super::stream::drain_interjections(&mut state, &event_tx).await;
    assert_eq!(injected, 2);
    assert_eq!(state.messages.len(), 2);
    assert!(matches!(
        state.messages[0].as_ref(),
        CompatibleChatCompletionRequestMessage::User(_)
    ));
    assert!(matches!(
        state.messages[1].as_ref(),
        CompatibleChatCompletionRequestMessage::User(_)
    ));

    for expected_id in ["c1", "c2"] {
        match event_rx.recv().await.unwrap() {
            crate::agent::event::CoreEvent::UserMessage { interjection } => {
                assert_eq!(interjection.client_id, expected_id);
                assert!(interjection.content.starts_with("插话"));
                if expected_id == "c2" {
                    assert_eq!(interjection.attachments.len(), 1);
                    assert_eq!(interjection.mentions.len(), 1);
                } else {
                    assert!(interjection.attachments.is_empty());
                    assert!(interjection.mentions.is_empty());
                }
            }
            other => panic!("expected UserMessage event, got {other:?}"),
        }
    }

    assert_eq!(
        super::stream::drain_interjections(&mut state, &event_tx).await,
        0
    );
}

#[tokio::test]
async fn drain_interjections_dedupes_by_client_id() {
    let mut state = test_stream_state(false);
    let (interject_tx, interject_rx) = tokio::sync::mpsc::channel(8);
    state.interject_rx = interject_rx;
    let (event_tx, mut event_rx) = crate::agent::event::create_core_event_channel();

    for _ in 0..2 {
        interject_tx
            .send(crate::agent::event::Interjection {
                client_id: "c1".to_string(),
                content: "同一条插话".to_string(),
                attachments: vec![],
                mentions: Vec::new(),
            })
            .await
            .unwrap();
    }

    let injected = super::stream::drain_interjections(&mut state, &event_tx).await;
    // Only the first occurrence enters the context
    assert_eq!(injected, 1);
    assert_eq!(state.messages.len(), 1);

    // The duplicate is reported dropped, not silently swallowed
    match event_rx.recv().await.unwrap() {
        crate::agent::event::CoreEvent::UserMessage { interjection } => {
            assert_eq!(interjection.client_id, "c1");
        }
        other => panic!("expected UserMessage event, got {other:?}"),
    }
    match event_rx.recv().await.unwrap() {
        crate::agent::event::CoreEvent::InterjectDropped { client_id } => {
            assert_eq!(client_id, "c1");
        }
        other => panic!("expected InterjectDropped event, got {other:?}"),
    }
}

#[tokio::test]
async fn drop_residual_interjections_emits_dropped_events_without_injecting() {
    let mut state = test_stream_state(false);
    let (interject_tx, interject_rx) = tokio::sync::mpsc::channel(8);
    state.interject_rx = interject_rx;
    let (event_tx, mut event_rx) = crate::agent::event::create_core_event_channel();

    for id in ["c1", "c2"] {
        interject_tx
            .send(crate::agent::event::Interjection {
                client_id: id.to_string(),
                content: "插话".to_string(),
                attachments: vec![],
                mentions: Vec::new(),
            })
            .await
            .unwrap();
    }

    super::stream::drop_residual_interjections(&mut state, &event_tx).await;

    // Never injected: the context must stay untouched
    assert!(state.messages.is_empty());
    // Both client_ids are reported so consumers can release/re-queue
    for expected_id in ["c1", "c2"] {
        match event_rx.recv().await.unwrap() {
            crate::agent::event::CoreEvent::InterjectDropped { client_id } => {
                assert_eq!(client_id, expected_id);
            }
            other => panic!("expected InterjectDropped event, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn chat_stream_plan_mode_appends_and_persists_plan_reminder() {
    let mut agent = make_test_agent();
    agent.set_plan_mode(true);
    let (event_tx, mut event_rx) = crate::agent::event::create_core_event_channel();

    agent
        .chat_stream("s1", "规划一下重构", vec![], event_tx)
        .unwrap();

    // The reminder is part of the real context now, not a request clone
    match agent.messages.last().unwrap().as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => {
                assert!(t.starts_with("规划一下重构"));
                assert!(t.contains(crate::prompts::PLAN_MODE));
            }
            _ => panic!("expected text content"),
        },
        other => panic!("expected user message, got {other:?}"),
    }
    assert!(agent.plan_context);

    // First event (synchronous, before the run task spawns) is the
    // persistence request anchored to the session
    match event_rx.recv().await.unwrap() {
        crate::agent::event::CoreEvent::PersistUserReminder {
            session_id,
            reminder,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(reminder, crate::prompts::PLAN_MODE);
        }
        other => panic!("expected PersistUserReminder, got {other:?}"),
    }
}

#[tokio::test]
async fn chat_stream_exit_reminder_on_first_chat_message_after_plan() {
    let mut agent = make_test_agent();
    agent.set_plan_mode(true);
    let (tx1, _rx1) = crate::agent::event::create_core_event_channel();
    agent.chat_stream("s1", "plan turn", vec![], tx1).unwrap();

    agent.set_plan_mode(false);
    let (tx2, mut rx2) = crate::agent::event::create_core_event_channel();
    agent.chat_stream("s1", "chat turn", vec![], tx2).unwrap();

    match agent.messages.last().unwrap().as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => {
                assert!(t.starts_with("chat turn"));
                assert!(
                    t.contains(crate::prompts::PLAN_MODE_EXIT),
                    "the transition message must carry the exit reminder"
                );
            }
            _ => panic!("expected text content"),
        },
        other => panic!("expected user message, got {other:?}"),
    }
    assert!(!agent.plan_context);

    match rx2.recv().await.unwrap() {
        crate::agent::event::CoreEvent::PersistUserReminder { reminder, .. } => {
            assert_eq!(reminder, crate::prompts::PLAN_MODE_EXIT);
        }
        other => panic!("expected PersistUserReminder, got {other:?}"),
    }
}

#[tokio::test]
async fn chat_stream_chat_mode_without_plan_context_skips_reminders() {
    let mut agent = make_test_agent();
    let (event_tx, _event_rx) = crate::agent::event::create_core_event_channel();

    agent
        .chat_stream("s1", "plain message", vec![], event_tx)
        .unwrap();

    match agent.messages.last().unwrap().as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => {
                assert_eq!(t, "plain message");
                assert!(!t.contains(crate::prompts::PLAN_MODE));
            }
            _ => panic!("expected text content"),
        },
        other => panic!("expected user message, got {other:?}"),
    }
    assert!(!agent.plan_context);
}

#[tokio::test]
async fn chat_stream_plan_mode_with_pending_error_persists_both_reminders() {
    let mut agent = make_test_agent();
    agent.set_plan_mode(true);
    let (tx1, _rx1) = crate::agent::event::create_core_event_channel();
    agent.chat_stream("s1", "plan turn", vec![], tx1).unwrap();

    // Simulate the previous run stashing a failure (the spawned task would
    // do this against the unreachable test API base)
    if let Ok(mut slot) = agent.last_error.lock() {
        *slot = Some("boom".to_string());
    }

    let (tx2, mut rx2) = crate::agent::event::create_core_event_channel();
    agent.chat_stream("s1", "retry turn", vec![], tx2).unwrap();

    // Both reminders on the same message: plan first, then the failure
    match agent.messages.last().unwrap().as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => {
                let plan_idx = t.find(crate::prompts::PLAN_MODE).unwrap();
                let err_idx = t.find("The previous request failed").unwrap();
                assert!(
                    plan_idx < err_idx,
                    "plan reminder precedes failure reminder"
                );
            }
            _ => panic!("expected text content"),
        },
        other => panic!("expected user message, got {other:?}"),
    }

    // Persisted in the same order so rebuild splices byte-identically
    match rx2.recv().await.unwrap() {
        crate::agent::event::CoreEvent::PersistUserReminder { reminder, .. } => {
            assert_eq!(reminder, crate::prompts::PLAN_MODE);
        }
        other => panic!("expected plan PersistUserReminder, got {other:?}"),
    }
    match rx2.recv().await.unwrap() {
        crate::agent::event::CoreEvent::PersistUserReminder { reminder, .. } => {
            assert!(reminder.contains("boom"));
        }
        other => panic!("expected failure PersistUserReminder, got {other:?}"),
    }
}

#[tokio::test]
async fn chat_stream_exit_reminder_with_pending_error_persists_both() {
    let mut agent = make_test_agent();
    agent.set_plan_mode(true);
    let (tx1, _rx1) = crate::agent::event::create_core_event_channel();
    agent.chat_stream("s1", "plan turn", vec![], tx1).unwrap();

    // Leave plan mode while the previous run's failure is stashed: the exit
    // reminder precedes the failure reminder, both persisted in order
    agent.set_plan_mode(false);
    if let Ok(mut slot) = agent.last_error.lock() {
        *slot = Some("boom".to_string());
    }

    let (tx2, mut rx2) = crate::agent::event::create_core_event_channel();
    agent.chat_stream("s1", "exit turn", vec![], tx2).unwrap();

    match agent.messages.last().unwrap().as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => {
                let exit_idx = t.find(crate::prompts::PLAN_MODE_EXIT).unwrap();
                let err_idx = t.find("The previous request failed").unwrap();
                assert!(
                    exit_idx < err_idx,
                    "exit reminder precedes failure reminder"
                );
            }
            _ => panic!("expected text content"),
        },
        other => panic!("expected user message, got {other:?}"),
    }
    assert!(!agent.plan_context());

    match rx2.recv().await.unwrap() {
        crate::agent::event::CoreEvent::PersistUserReminder { reminder, .. } => {
            assert_eq!(reminder, crate::prompts::PLAN_MODE_EXIT);
        }
        other => panic!("expected exit PersistUserReminder, got {other:?}"),
    }
    match rx2.recv().await.unwrap() {
        crate::agent::event::CoreEvent::PersistUserReminder { reminder, .. } => {
            assert!(reminder.contains("boom"));
        }
        other => panic!("expected failure PersistUserReminder, got {other:?}"),
    }
}

#[tokio::test]
async fn drain_interjections_plan_mode_persists_reminder() {
    let mut state = test_stream_state(false);
    state.plan_mode = true;
    let (interject_tx, interject_rx) = tokio::sync::mpsc::channel(8);
    state.interject_rx = interject_rx;
    let (event_tx, mut event_rx) = crate::agent::event::create_core_event_channel();

    interject_tx
        .send(crate::agent::event::Interjection {
            client_id: "c1".to_string(),
            content: "补充一点".to_string(),
            attachments: vec![],
            mentions: Vec::new(),
        })
        .await
        .unwrap();

    let injected = super::stream::drain_interjections(&mut state, &event_tx).await;
    assert_eq!(injected, 1);

    // Echo first, then the persistence event (consumer anchors the row the
    // echo's persistence just wrote)
    match event_rx.recv().await.unwrap() {
        crate::agent::event::CoreEvent::UserMessage { interjection } => {
            assert_eq!(interjection.client_id, "c1");
        }
        other => panic!("expected UserMessage, got {other:?}"),
    }
    match event_rx.recv().await.unwrap() {
        crate::agent::event::CoreEvent::PersistUserReminder {
            session_id,
            reminder,
        } => {
            assert_eq!(session_id, "test-session");
            assert_eq!(reminder, crate::prompts::PLAN_MODE);
        }
        other => panic!("expected PersistUserReminder, got {other:?}"),
    }

    match state.messages.last().unwrap().as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => {
                assert!(t.starts_with("补充一点"));
                assert!(t.contains(crate::prompts::PLAN_MODE));
            }
            _ => panic!("expected text content"),
        },
        other => panic!("expected user message, got {other:?}"),
    }
}

#[test]
fn interject_rejects_when_bounded_queue_is_full() {
    let agent = make_test_agent();
    // Simulate an active run: bind a channel at the interjection queue limit
    let (tx, _rx) = tokio::sync::mpsc::channel::<crate::agent::event::Interjection>(
        crate::agent::event::INTERJECT_QUEUE_LIMIT,
    );
    *agent.interject_slot().lock().unwrap() = Some(tx);
    let make = |id: &str| crate::agent::event::Interjection {
        client_id: id.to_string(),
        content: "hello".to_string(),
        attachments: vec![],
        mentions: Vec::new(),
    };
    for i in 0..crate::agent::event::INTERJECT_QUEUE_LIMIT {
        assert!(
            agent.interject(make(&format!("c{i}"))),
            "slot below the limit must accept"
        );
    }
    assert!(
        !agent.interject(make("overflow")),
        "the {LIMIT}-item limit must reject further interjections",
        LIMIT = crate::agent::event::INTERJECT_QUEUE_LIMIT
    );
}

#[test]
fn interject_rejects_while_cancelling() {
    let agent = make_test_agent();
    let (tx, _rx) = tokio::sync::mpsc::channel::<crate::agent::event::Interjection>(4);
    *agent.interject_slot().lock().unwrap() = Some(tx);
    agent.interrupt();
    assert!(
        !agent.interject(crate::agent::event::Interjection {
            client_id: "c1".to_string(),
            content: "hello".to_string(),
            attachments: vec![],
            mentions: Vec::new(),
        }),
        "a cancelling run must reject interjections so callers fall back"
    );
}

// ── Retry-error context policy ─────────────────────────────────────
// Errors leave no assistant trace in the stored context; the failure reaches
// the next turn as a transient system-reminder injected on the new user
// message (append_text_to_last_user_message).

fn user_text(msg: &SharedMessage) -> String {
    match msg.as_ref() {
        CompatibleChatCompletionRequestMessage::User(u) => match &u.content {
            ChatCompletionRequestUserMessageContent::Text(t) => t.clone(),
            ChatCompletionRequestUserMessageContent::Array(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ChatCompletionRequestUserMessageContentPart::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        },
        _ => panic!("expected a User message"),
    }
}

#[test]
fn append_reminder_text_form_appends_to_last_user() {
    use super::messages::append_text_to_last_user_message;
    let mut messages = vec![user_msg("older"), user_msg("hello")];
    append_text_to_last_user_message(&mut messages, "<note/>");
    assert_eq!(user_text(&messages[1]), "hello\n\n<note/>");
    assert_eq!(user_text(&messages[0]), "older");
}

#[test]
fn append_reminder_hits_user_before_trailing_tool_results() {
    use super::messages::append_text_to_last_user_message;
    let mut messages = vec![
        user_msg("hello"),
        assistant_with_tool_calls(&["c1"]),
        tool_result_msg("c1"),
    ];
    append_text_to_last_user_message(&mut messages, "<note/>");
    assert!(user_text(&messages[0]).ends_with("<note/>"));
}

#[test]
fn append_reminder_array_form_pushes_text_part() {
    use super::messages::append_text_to_last_user_message;
    let img = png_attachment();
    let mut messages = vec![Arc::new(build_user_message("看图", &[img]))];
    append_text_to_last_user_message(&mut messages, "<note/>");
    let CompatibleChatCompletionRequestMessage::User(u) = messages[0].as_ref() else {
        panic!("expected User");
    };
    let ChatCompletionRequestUserMessageContent::Array(parts) = &u.content else {
        panic!("expected Array content");
    };
    assert_eq!(parts.len(), 3, "text part + image part + appended note");
    let ChatCompletionRequestUserMessageContentPart::Text(t) = &parts[2] else {
        panic!("expected trailing Text part");
    };
    assert!(t.text.ends_with("<note/>"));
}

#[test]
fn append_reminder_noop_without_user_message() {
    use super::messages::append_text_to_last_user_message;
    let assistant: SharedMessage = Arc::new(
        ChatCompletionRequestAssistantMessage {
            content: Some(
                async_openai::types::chat::ChatCompletionRequestAssistantMessageContent::Text(
                    "hi".to_string(),
                ),
            ),
            ..Default::default()
        }
        .into(),
    );
    let mut messages: Vec<SharedMessage> = vec![assistant];
    append_text_to_last_user_message(&mut messages, "<note/>");
    assert!(matches!(
        messages[0].as_ref(),
        CompatibleChatCompletionRequestMessage::Assistant(_)
    ));
}

#[tokio::test]
async fn finalize_error_status_leaves_no_assistant_trace() {
    let (event_tx, mut rx) = crate::agent::event::create_core_event_channel();
    let mut messages = vec![user_msg("hello")];
    super::stream::finalize_messages(
        &mut messages,
        "[Error: 429]".to_string(),
        &event_tx,
        Vec::new(),
        "test/test-model",
        crate::agent::models::ReasoningEffort::default(),
        crate::agent::event::TaskStatus::Error,
    );
    // The final list is handed to AgentComplete (input drained by design)
    let crate::agent::event::CoreEvent::AgentComplete {
        messages: final_msgs,
        ..
    } = rx.recv().await.expect("AgentComplete emitted")
    else {
        panic!("expected AgentComplete");
    };
    assert_eq!(
        final_msgs.len(),
        1,
        "error finalize must not synthesize an assistant message"
    );
    assert!(matches!(
        final_msgs[0].as_ref(),
        CompatibleChatCompletionRequestMessage::User(_)
    ));
}

#[tokio::test]
async fn finalize_completed_status_keeps_assistant_fallback() {
    let (event_tx, mut rx) = crate::agent::event::create_core_event_channel();
    let mut messages = vec![user_msg("hello")];
    super::stream::finalize_messages(
        &mut messages,
        "[fallback]".to_string(),
        &event_tx,
        Vec::new(),
        "test/test-model",
        crate::agent::models::ReasoningEffort::default(),
        crate::agent::event::TaskStatus::Completed,
    );
    // PersistMessage (fallback synthesis) precedes the sentinel
    let mut final_msgs = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let crate::agent::event::CoreEvent::AgentComplete { messages, .. } = ev {
            final_msgs = messages;
            break;
        }
    }
    assert_eq!(final_msgs.len(), 2, "non-error fallback still applies");
    let CompatibleChatCompletionRequestMessage::Assistant(a) = final_msgs[1].as_ref() else {
        panic!("expected synthesized Assistant");
    };
    let Some(async_openai::types::chat::ChatCompletionRequestAssistantMessageContent::Text(t)) =
        &a.base.content
    else {
        panic!("expected text content");
    };
    assert_eq!(t, "[fallback]");
}
