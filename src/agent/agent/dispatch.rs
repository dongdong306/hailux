use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_openai::types::chat::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
    ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart,
};
use tokio::sync::oneshot;

use super::CANCEL_POLL_INTERVAL;
use super::messages::{image_content_part, tool_attachment_note};
use super::stream::{AgentStreamState, emit_persist_message, finalize_messages};
use crate::agent::event::{CoreEvent, CoreEventTx, MessageUsage, TaskStatus};
use crate::agent::media::Attachment;
use crate::agent::models::{CompatibleChatCompletionRequestMessage, SharedMessage};
use crate::agent::tools::ToolExecution;
use crate::permission::{PermissionManager, PermissionReply, PermissionRequest, PermissionResult};

/// Evaluates all permission requests of one tool call in order: any Deny
/// short-circuits with a rejection; the first Ask opens a dialog and waits
/// for the user's reply; all Allow approves (returns None).
///
/// The request list is ordered (e.g. shell: command-rule request first,
/// out-of-workdir request second), so deny/ask rules of the higher-priority
/// category are not short-circuited by lower-priority requests. A user's
/// Once choice only approves that single request; later requests keep being
/// evaluated (Deny still applies).
async fn evaluate_permission_requests(
    requests: &[PermissionRequest],
    manager: &PermissionManager,
    plan_mode: bool,
    event_tx: &CoreEventTx,
    cancel: &AtomicBool,
) -> Option<String> {
    for request in requests {
        match manager.check(request, plan_mode) {
            PermissionResult::Allowed => continue,
            PermissionResult::Denied(reason) => return Some(reason),
            PermissionResult::NeedsAsk(request) => {
                let always_patterns = request.always_patterns.clone();
                let (tx, rx) = oneshot::channel::<PermissionReply>();
                if event_tx
                    .try_send(CoreEvent::PermissionRequest {
                        request: request.clone(),
                        response_tx: tx,
                        subagent_name: None,
                    })
                    .is_err()
                {
                    // Event channel full or closed: cannot ask the user via
                    // a dialog. Fall back to denial instead of awaiting an
                    // rx that never resolves (deadlock).
                    return Some("Permission request channel unavailable".to_string());
                }
                tokio::select! {
                    reply = rx => match reply {
                        Ok(PermissionReply::Once) => continue,
                        Ok(PermissionReply::Always) => {
                            // Persist under the request's own permission category
                            // (not the tool's), so "always" also covers
                            // same-category requests (e.g. external_directory)
                            manager
                                .add_session_rules_async(&request.permission, &always_patterns)
                                .await;
                            continue;
                        }
                        Ok(PermissionReply::Deny) => {
                            return Some("Permission denied by user".to_string());
                        }
                        Err(_) => {
                            return Some("Permission request channel closed".to_string());
                        }
                    },
                    // User cancelled while waiting (e.g. Ctrl+C): abort this
                    // ask instead of getting stuck on the dialog
                    _ = async {
                        while !cancel.load(Ordering::Relaxed) {
                            tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
                        }
                    } => return Some("Permission request cancelled".to_string()),
                }
            }
        }
    }
    None
}

pub(super) async fn finalize_cancelled(
    state: &mut AgentStreamState,
    event_tx: &CoreEventTx,
    usages: Vec<MessageUsage>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Check whether the last assistant message has orphaned tool_calls
    // (missing their corresponding tool results)
    let mut orphaned_tool_msgs = Vec::new();
    if let Some(msg) = state.messages.last()
        && let CompatibleChatCompletionRequestMessage::Assistant(a) = msg.as_ref()
        && let Some(tool_calls) = &a.base.tool_calls
        && !tool_calls.is_empty()
    {
        for tc in tool_calls {
            let id = match tc {
                ChatCompletionMessageToolCalls::Function(f) => f.id.clone(),
                ChatCompletionMessageToolCalls::Custom(c) => c.id.clone(),
            };
            let tool_msg: SharedMessage = Arc::new(
                ChatCompletionRequestToolMessage {
                    content: ChatCompletionRequestToolMessageContent::Text(
                        "Tool execution aborted".to_string(),
                    ),
                    tool_call_id: id,
                }
                .into(),
            );
            orphaned_tool_msgs.push(tool_msg);
        }
    }
    for msg in &orphaned_tool_msgs {
        state.messages.push(Arc::clone(msg));
    }

    // Emit persistence + ToolResult events for the orphaned tool messages
    for msg in &orphaned_tool_msgs {
        emit_persist_message(event_tx, msg, None, &state.model_display, None);
        if let CompatibleChatCompletionRequestMessage::Tool(t) = msg.as_ref() {
            let result_text = match &t.content {
                ChatCompletionRequestToolMessageContent::Text(t) => t.clone(),
                _ => String::new(),
            };
            let _ = event_tx.try_send(CoreEvent::ToolResult {
                name: String::new(),
                result: result_text,
                display: None,
                subagent_name: None,
                subagent_index: None,
            });
        }
    }

    // Note: when push_partial_assistant already pushed
    // [Assistant(partial), Tool(aborted)...], finalize_messages detects that
    // the last message is not an Assistant (but a Tool) and appends a
    // "[Task interrupted]" Assistant message responding to the tool results.
    // This satisfies the OpenAI API message alternation constraint:
    // Assistant → Tool → Assistant.
    finalize_messages(
        &mut state.messages,
        "[Task interrupted]".to_string(),
        event_tx,
        usages,
        &state.model_display,
        state.reasoning_effort,
        TaskStatus::Interrupted,
    );
    Ok(())
}

pub(super) async fn handle_tool_calls_stream(
    state: &mut AgentStreamState,
    tool_calls: &[ChatCompletionMessageToolCalls],
    cancel: &Arc<AtomicBool>,
    event_tx: &CoreEventTx,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Attachments are injected in one batch after the loop: inserting a
    // synthetic user message between consecutive tool messages would violate
    // the "tool messages must immediately follow the assistant carrying
    // tool_calls" constraint (strict providers 400 outright), and the
    // persisted interleaved sequence would pollute every later request.
    // Elements are (source tool name, attachment), used to build the
    // source-annotated hint text.
    let mut pending_attachments: Vec<(String, Attachment)> = Vec::new();
    for tool_call in tool_calls {
        if cancel.load(Ordering::Relaxed) {
            let _ = event_tx.try_send(CoreEvent::AgentChunk("\n\n[Task interrupted]".to_string()));
            let (id, name) = match tool_call {
                ChatCompletionMessageToolCalls::Function(f) => (&f.id, &f.function.name),
                ChatCompletionMessageToolCalls::Custom(c) => (&c.id, &c.custom_tool.name),
            };
            let _ = event_tx.try_send(CoreEvent::ToolResult {
                name: name.clone(),
                result: "Tool execution aborted".to_string(),
                display: None,
                subagent_name: None,
                subagent_index: None,
            });
            let pushed_msg: SharedMessage = {
                let msg: SharedMessage = Arc::new(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(
                            "Tool execution aborted".to_string(),
                        ),
                        tool_call_id: id.clone(),
                    }
                    .into(),
                );
                state.messages.push(Arc::clone(&msg));
                msg
            };
            emit_persist_message(event_tx, &pushed_msg, None, &state.model_display, None);
            continue;
        }
        let (id, name, arguments) = match tool_call {
            ChatCompletionMessageToolCalls::Function(f) => {
                (&f.id, &f.function.name, &f.function.arguments)
            }
            ChatCompletionMessageToolCalls::Custom(c) => {
                (&c.id, &c.custom_tool.name, &c.custom_tool.input)
            }
        };

        let tool_arc = state.tool_registry.get_arc(name);

        // Permission check
        let permission_denied: Option<String> = if let Some(ref tool) = tool_arc {
            let needs_check = tool.permission_category().is_some();
            if needs_check {
                let (perm_reqs, perm_mgr, plan_mode) = {
                    let reqs = tool.extract_permission(arguments, &state.work_dir);
                    let pm = state.permission.clone();
                    let pm2 = state.plan_mode;
                    (reqs, pm, pm2)
                };
                // Plan mode: reject non-read-only shell commands outright
                // (fail-closed, no dialog), consistent with plan mode hiding
                // the edit/write tools.
                let plan_denial = if plan_mode && tool.name() == "shell" {
                    crate::agent::tools::plan_mode_shell_denial(arguments)
                } else {
                    None
                };
                if let Some(reason) = plan_denial {
                    Some(reason)
                } else {
                    evaluate_permission_requests(&perm_reqs, &perm_mgr, plan_mode, event_tx, cancel)
                        .await
                }
            } else {
                None
            }
        } else {
            None
        };

        // Report "tool started" only after permission passes; denied tools
        // are not surfaced
        if permission_denied.is_none() {
            let _ = event_tx.try_send(CoreEvent::ToolCallStart {
                name: name.clone(),
                arguments: arguments.clone(),
                subagent_name: None,
                subagent_index: None,
            });
        }

        let execution: ToolExecution = if let Some(deny_reason) = permission_denied {
            ToolExecution::text(deny_reason)
        } else if let Some(tool) = tool_arc {
            // Clone work_dir locally so the execution future does not hold a
            // borrow of state
            let work_dir = state.work_dir.clone();
            if tool.cancellable() {
                let cancel_clone = cancel.clone();
                let execute_fut = tool.execute_async_with_display(arguments, &work_dir);
                tokio::pin!(execute_fut);
                tokio::select! {
                    r = &mut execute_fut => {
                        r.unwrap_or_else(|err| ToolExecution::text(err.message))
                    }
                    _ = async {
                        while !cancel_clone.load(Ordering::Relaxed) {
                            tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
                        }
                    } => {
                        ToolExecution::text("Tool execution aborted")
                    }
                }
            } else {
                tool.execute_async_with_display(arguments, &work_dir)
                    .await
                    .unwrap_or_else(|err| ToolExecution::text(err.message))
            }
        } else {
            ToolExecution::text("Unknown function")
        };
        let ToolExecution {
            content: mut result,
            display,
            attachments,
        } = execution;

        // When the model lacks vision, no synthetic message is injected;
        // instead a note is appended to the tool output so the model can
        // inform the user
        let omit_images = !attachments.is_empty() && !state.supports_vision;
        if omit_images {
            result.push_str(
                "\n[Image attachment omitted: this model does not support vision input. Inform the user.]",
            );
        }

        let _ = event_tx.try_send(CoreEvent::ToolResult {
            name: name.clone(),
            result: result.clone(),
            display: display.clone(),
            subagent_name: None,
            subagent_index: None,
        });

        let pushed_msg: SharedMessage = {
            let msg: SharedMessage = Arc::new(
                ChatCompletionRequestToolMessage {
                    content: ChatCompletionRequestToolMessageContent::Text(result),
                    tool_call_id: id.clone(),
                }
                .into(),
            );
            state.messages.push(Arc::clone(&msg));
            msg
        };
        emit_persist_message(event_tx, &pushed_msg, None, &state.model_display, display);

        // The tool returned media attachments (e.g. read hit an image):
        // OpenAI Chat Completions tool results allow text only, so media is
        // collected and stripped into a single synthetic user message after
        // all tool messages (keeping them contiguous to satisfy Provider
        // validation).
        if !attachments.is_empty() && !omit_images {
            pending_attachments.extend(attachments.into_iter().map(|a| (name.clone(), a)));
        }
    }
    if !pending_attachments.is_empty() {
        let mut parts = vec![ChatCompletionRequestUserMessageContentPart::Text(
            ChatCompletionRequestMessageContentPartText {
                text: tool_attachment_note(&pending_attachments),
                prompt_cache_breakpoint: None,
            },
        )];
        parts.extend(
            pending_attachments
                .iter()
                .map(|(_, att)| image_content_part(att)),
        );
        let synth_msg: SharedMessage = Arc::new(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Array(parts),
                name: None,
            }
            .into(),
        );
        state.messages.push(Arc::clone(&synth_msg));
        // Display-layer identification marker: written to runtime_meta via
        // the PersistMessage display field (aligned with the
        // {"format":"diff"} convention for diff display). Web/TUI history
        // loading skips this synthetic message based on it instead of
        // rendering it as a user message; identification never relies on the
        // hint text.
        emit_persist_message(
            event_tx,
            &synth_msg,
            None,
            &state.model_display,
            Some(r#"{"format":"tool-attachment"}"#.to_string()),
        );
    }
    Ok(())
}
