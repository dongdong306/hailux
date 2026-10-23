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

/// 依序评估一次工具调用的全部权限请求：任一 Deny → 短路拒绝；第一个
/// Ask → 弹窗等待用户应答；全部 Allow → 放行（返回 None）。
///
/// 请求列表有序（如 bash：命令规则请求在前、越界路径请求在后），因此
/// 高优先级类别的 deny/ask 规则不会被低优先级请求短路。用户选 Once 只
/// 放行当前这一个请求，后续请求继续评估（Deny 依然生效）。
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
                    // 事件通道满或已关闭：无法向用户弹窗询问。
                    // 回退为拒绝而非等待一个永不 resolve 的 rx（死锁）。
                    return Some("Permission request channel unavailable".to_string());
                }
                tokio::select! {
                    reply = rx => match reply {
                        Ok(PermissionReply::Once) => continue,
                        Ok(PermissionReply::Always) => {
                            // 以请求自身的 permission 类别持久化（而非工具类别），
                            // 保证 always 对同类别请求生效（如 external_directory）
                            manager.add_session_rules(&request.permission, &always_patterns);
                            continue;
                        }
                        Ok(PermissionReply::Deny) => {
                            return Some("Permission denied by user".to_string());
                        }
                        Err(_) => {
                            return Some("Permission request channel closed".to_string());
                        }
                    },
                    // 等待期间用户取消（如 Ctrl+C）：中止本次询问，避免卡死在弹窗上
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
    // 检查最后一条 assistant 消息是否有 orphaned tool_calls（缺少对应的 tool result）
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

    // 为 orphaned tool 消息发送持久化 + ToolResult 事件
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

    // 注意：当 push_partial_assistant 已推入 [Assistant(partial), Tool(aborted)...] 时，
    // finalize_messages 会检测到最后一条不是 Assistant（而是 Tool），
    // 从而追加一条 "[Task interrupted]" Assistant 消息作为对 tool results 的回应。
    // 这符合 OpenAI API 消息交替约束：Assistant → Tool → Assistant。
    finalize_messages(
        &mut state.messages,
        "[Task interrupted]".to_string(),
        event_tx,
        usages,
        &state.model_display,
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
    // 附件统一在循环结束后注入：合成 user 消息若插在连续 tool 消息中间，
    // 会破坏「tool 消息必须紧跟带 tool_calls 的 assistant 消息」的约束
    // （严格 Provider 直接 400），且已持久化的交错序列会污染后续每轮请求。
    // 元素为 (来源工具名, 附件)，用于生成带来源的提示文本。
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

        // 权限检查
        let permission_denied: Option<String> = if let Some(ref tool) = tool_arc {
            let needs_check = tool.permission_category().is_some();
            if needs_check {
                let (perm_reqs, perm_mgr, plan_mode) = {
                    let reqs = tool.extract_permission(arguments, &state.work_dir);
                    let pm = state.permission.clone();
                    let pm2 = state.plan_mode;
                    (reqs, pm, pm2)
                };
                // plan 模式：bash 非只读命令直接拒绝（fail-closed，不弹窗），
                // 与 plan 模式隐藏 edit/write 工具保持一致。
                let plan_denial = if plan_mode && tool.name() == "bash" {
                    crate::agent::tools::plan_mode_bash_denial(arguments)
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

        // 权限通过后才上报「工具已开始」，被拒的工具不显示
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
            // work_dir 克隆到局部，避免执行 future 持有对 state 的借用
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

        // 模型不支持视觉时不注入合成消息，改为在工具输出尾部附说明，让模型转告用户
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

        // 工具返回了媒体附件（如 read 读到图片）：OpenAI Chat Completions 的 tool result
        // 只允许文本，媒体收集后在全部 tool 消息之后统一剥离为一条合成 user 消息
        //（保持 tool 消息连续以满足 Provider 校验）。
        if !attachments.is_empty() && !omit_images {
            pending_attachments.extend(attachments.into_iter().map(|a| (name.clone(), a)));
        }
    }
    if !pending_attachments.is_empty() {
        let mut parts = vec![ChatCompletionRequestUserMessageContentPart::Text(
            ChatCompletionRequestMessageContentPartText {
                text: tool_attachment_note(&pending_attachments),
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
        // 展示层识别标记：经 PersistMessage 的 display 写入 runtime_meta
        // （与 diff 展示的 {"format":"diff"} 惯例对齐）。Web/TUI 历史加载
        // 据此跳过该合成消息，不渲染为用户消息；识别不依赖提示文本。
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
