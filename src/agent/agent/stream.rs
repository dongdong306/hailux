use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_openai::{
    Client,
    config::OpenAIConfig,
    error::OpenAIError,
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
        ChatCompletionToolChoiceOption, FinishReason, FunctionCall, ToolChoiceOptions,
    },
};
use futures_util::StreamExt;

use super::messages::{ensure_tool_call_parity, sanitize_messages_for_request};
use crate::agent::event::{CoreEvent, CoreEventTx, Interjection, MessageUsage, TaskStatus};
use crate::agent::models::{
    CompatibleChatCompletionRequestAssistantMessage, CompatibleChatCompletionRequestMessage,
    CompatibleCreateChatCompletionRequestArgs, CompatibleCreateChatCompletionStreamResponse,
    ReasoningEffort, SharedMessage, ThinkingConfig,
};
use crate::agent::retry::{self, RetryConfig};
use crate::agent::tools::ToolRegistry;
use crate::permission::PermissionManager;

use super::CANCEL_POLL_INTERVAL;

const PLAN_MODE_PROMPT: &str = crate::prompts::PLAN_MODE;

#[derive(Default)]
pub(super) struct PartialToolCall {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) arguments: String,
}

pub(super) struct AgentStreamState {
    pub(super) client: Client<OpenAIConfig>,
    pub(super) tool_registry: ToolRegistry,
    pub(super) messages: Vec<SharedMessage>,
    pub(super) model: String,
    pub(super) model_display: String,
    pub(super) max_tokens: u32,
    pub(super) supports_vision: bool,
    pub(super) native_reasoning: bool,
    pub(super) reasoning_effort: ReasoningEffort,
    pub(super) plan_mode: bool,
    pub(super) permission: PermissionManager,
    pub(super) work_dir: String,
    pub(super) retry: RetryConfig,
    /// Swapped per run; drained at loop boundaries
    pub(super) interject_rx: tokio::sync::mpsc::Receiver<Interjection>,
    /// client_ids already injected this run: a repeated client_id is
    /// reported as dropped instead of being injected twice
    pub(super) delivered_interject_ids: std::collections::HashSet<String>,
}

impl AgentStreamState {
    pub(super) fn build_request(
        &self,
    ) -> Result<CompatibleCreateChatCompletionRequestArgs, Box<dyn Error + Send + Sync>> {
        let tools = self.tool_registry.to_chat_tools_filtered(self.plan_mode);
        let mut messages = self.messages.clone();
        if self.plan_mode {
            for msg in messages.iter_mut().rev() {
                let msg_mut = Arc::make_mut(msg);
                if let CompatibleChatCompletionRequestMessage::User(u) = msg_mut {
                    match &mut u.content {
                        async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(
                            t,
                        ) => {
                            t.push_str("\n\n");
                            t.push_str(PLAN_MODE_PROMPT);
                        }
                        // Separate text part for messages with image attachments
                        async_openai::types::chat::ChatCompletionRequestUserMessageContent::Array(
                            parts,
                        ) => {
                            parts.push(
                                async_openai::types::chat::ChatCompletionRequestUserMessageContentPart::Text(
                                    async_openai::types::chat::ChatCompletionRequestMessageContentPartText {
                                        text: format!("\n\n{PLAN_MODE_PROMPT}"),
                                    },
                                ),
                            );
                        }
                    }
                    break;
                }
            }
        }
        let messages = sanitize_messages_for_request(&messages, self.supports_vision);
        // Fill in tool results for dangling tool_calls left by interrupts /
        // disconnects / crashes so no provider ever sees the sequence
        let messages = ensure_tool_call_parity(&messages);
        let mut builder = CompatibleCreateChatCompletionRequestArgs::default();
        builder
            .max_completion_tokens(self.max_tokens)
            .model(&self.model)
            .stream(true)
            .tools(tools)
            .tool_choice(ChatCompletionToolChoiceOption::Mode(
                ToolChoiceOptions::Auto,
            ))
            .messages(messages)
            .extra("stream_options", serde_json::json!({"include_usage": true}));
        // Only `thinking` is DeepSeek/GLM-private and stays off the OpenAI
        // path so strictly validating servers never reject an unknown field;
        // reasoning_effort values are all within the OpenAI range.
        if self.native_reasoning {
            builder.thinking(ThinkingConfig::enabled());
        }
        builder.extra(
            "reasoning_effort",
            serde_json::json!(self.reasoning_effort.as_str()),
        );
        Ok(builder)
    }
}

pub(super) fn emit_persist_message(
    event_tx: &CoreEventTx,
    msg: &SharedMessage,
    usage: Option<MessageUsage>,
    model: &str,
    display: Option<String>,
) {
    let _ = event_tx.try_send(CoreEvent::PersistMessage {
        msg: Arc::clone(msg),
        usage,
        model: model.to_string(),
        display,
    });
}

/// Drain pending interjections into the context at a loop boundary; emits
/// `CoreEvent::UserMessage` per item (awaited: the echo is the delivery
/// confirmation — losing it would leave the submitter's item in-flight
/// forever). Non-zero return during the final answer stream means another
/// request round is needed.
pub(super) async fn drain_interjections(
    state: &mut AgentStreamState,
    event_tx: &CoreEventTx,
) -> usize {
    let mut injected = 0;
    while let Ok(interjection) = state.interject_rx.try_recv() {
        // Server-side idempotence: a repeated client_id is not injected
        // twice; report it dropped so no submitter's item hangs in-flight
        if !state
            .delivered_interject_ids
            .insert(interjection.client_id.clone())
        {
            let _ = event_tx
                .send(CoreEvent::InterjectDropped {
                    client_id: interjection.client_id,
                })
                .await;
            continue;
        }
        let msg: SharedMessage = Arc::new(super::build_user_message(
            &interjection.content,
            &interjection.attachments,
        ));
        state.messages.push(Arc::clone(&msg));
        let _ = event_tx.send(CoreEvent::UserMessage { interjection }).await;
        injected += 1;
    }
    injected
}

/// Report interjections that were accepted but never injected (run cancelled
/// / errored / completed before a boundary, or the narrow teardown window
/// after the last drain). Without this the submitter's queued item would
/// hang in-flight forever with no echo and no DB row. Never awaits in a way
/// that blocks teardown meaningfully — the event channel is 4096 deep and
/// the consumer keeps draining it until `RunComplete`.
pub(super) async fn drop_residual_interjections(
    state: &mut AgentStreamState,
    event_tx: &CoreEventTx,
) {
    while let Ok(interjection) = state.interject_rx.try_recv() {
        let _ = event_tx
            .send(CoreEvent::InterjectDropped {
                client_id: interjection.client_id,
            })
            .await;
    }
}

pub(super) fn finalize_messages(
    messages: &mut Vec<SharedMessage>,
    fallback_content: String,
    event_tx: &CoreEventTx,
    usages: Vec<MessageUsage>,
    model: &str,
    reasoning_effort: ReasoningEffort,
    status: TaskStatus,
) {
    let needs_assistant = !matches!(
        messages.last().map(|m| m.as_ref()),
        Some(CompatibleChatCompletionRequestMessage::Assistant(_))
    );
    if needs_assistant {
        let msg: SharedMessage = Arc::new(
            CompatibleChatCompletionRequestAssistantMessage {
                base: ChatCompletionRequestAssistantMessage {
                    content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                        fallback_content,
                    )),
                    ..Default::default()
                },
                reasoning_content: None,
            }
            .into(),
        );
        messages.push(Arc::clone(&msg));
        emit_persist_message(event_tx, &msg, None, model, None);
    }
    let final_messages = std::mem::take(messages);
    let _ = event_tx.try_send(CoreEvent::AgentComplete {
        messages: final_messages,
        usages,
        model: model.to_string(),
        reasoning_effort,
        status,
    });
}

#[derive(Default)]
pub(super) struct StreamOutput {
    pub(super) ai_message: String,
    pub(super) ai_reasoning: String,
    pub(super) tool_calls_map: BTreeMap<u32, PartialToolCall>,
    pub(super) is_tool_call: bool,
    pub(super) last_usage: Option<MessageUsage>,
}

enum AttemptOutcome {
    Done(StreamOutput),
    /// Partial output kept for the interrupt finalizer
    Cancelled(StreamOutput),
    /// Partial output is discarded and never enters context
    Retryable(OpenAIError),
    Fatal(OpenAIError),
}

fn classify_stream_error(e: OpenAIError) -> AttemptOutcome {
    if retry::is_retryable(&e) {
        AttemptOutcome::Retryable(e)
    } else {
        AttemptOutcome::Fatal(e)
    }
}

/// Failures never mutate `state.messages`: retry vs error is the caller's
/// decision, and partial output of a failed attempt lives only in this
/// return value.
async fn attempt_once(
    state: &AgentStreamState,
    cancel: &AtomicBool,
    event_tx: &CoreEventTx,
) -> Result<AttemptOutcome, Box<dyn Error + Send + Sync>> {
    let request = state.build_request()?.build()?;
    let client = state.client.clone();

    let mut stream = {
        let chat = client.chat();
        let fut =
            chat.create_stream_byot::<_, CompatibleCreateChatCompletionStreamResponse>(request);
        tokio::pin!(fut);
        let result_stream;
        loop {
            tokio::select! {
                result = &mut fut => {
                    result_stream = result;
                    break;
                }
                _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(AttemptOutcome::Cancelled(StreamOutput::default()));
                    }
                }
            }
        }
        match result_stream {
            Ok(s) => s,
            Err(e) => return Ok(classify_stream_error(e)),
        }
    };

    let mut out = StreamOutput::default();
    loop {
        tokio::select! {
            chunk = stream.next() => {
                match chunk {
                    None => break,
                    Some(Ok(chunk)) => {
                        if let Some(usage) = &chunk.usage {
                            out.last_usage = Some(MessageUsage {
                                prompt_tokens: usage.prompt_tokens,
                                completion_tokens: usage.completion_tokens,
                                cached_tokens: usage.cached_tokens(),
                            });
                        }
                        for choice in chunk.choices {
                            if let Some(content) = choice.delta.base.content {
                                let _ = event_tx.try_send(CoreEvent::AgentChunk(content.clone()));
                                out.ai_message.push_str(&content);
                            }

                            if let Some(reasoning) = choice.delta.reasoning_content {
                                let _ = event_tx.try_send(CoreEvent::AgentReasoningChunk(reasoning.clone()));
                                out.ai_reasoning.push_str(&reasoning);
                            }

                            if let Some(tool_calls) = choice.delta.base.tool_calls {
                                out.is_tool_call = true;
                                for tc in tool_calls {
                                    let entry = out.tool_calls_map
                                        .entry(tc.index)
                                        .or_default();
                                    if let Some(id) = tc.id {
                                        entry.id = id;
                                    }
                                    if let Some(func) = tc.function {
                                        if let Some(name) = func.name {
                                            entry.name = Some(name);
                                        }
                                        if let Some(args) = func.arguments {
                                            entry.arguments.push_str(&args);
                                        }
                                    }
                                }
                            }

                            if let Some(FinishReason::ToolCalls) = choice.finish_reason {
                                out.is_tool_call = true;
                            }
                        }
                    }
                    Some(Err(e)) => return Ok(classify_stream_error(e)),
                }
            }
            _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(AttemptOutcome::Cancelled(out));
                }
            }
        }
    }
    Ok(AttemptOutcome::Done(out))
}

pub(super) async fn run_stream_loop(
    state: &mut AgentStreamState,
    cancel: Arc<AtomicBool>,
    event_tx: &CoreEventTx,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Error exits (`?` from tool dispatch / fatal LLM errors / finalizers)
    // must still report accepted-but-uninjected interjections
    match run_stream_loop_inner(state, cancel, event_tx).await {
        Ok(v) => Ok(v),
        Err(e) => {
            drop_residual_interjections(state, event_tx).await;
            Err(e)
        }
    }
}

async fn run_stream_loop_inner(
    state: &mut AgentStreamState,
    cancel: Arc<AtomicBool>,
    event_tx: &CoreEventTx,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut all_usages: Vec<MessageUsage> = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop_residual_interjections(state, event_tx).await;
            super::dispatch::finalize_cancelled(state, event_tx, all_usages).await?;
            return Ok(());
        }
        // Interjection injection point: after tool results entered context,
        // before the next LLM request
        drain_interjections(state, event_tx).await;

        let mut retry_no: u32 = 0;
        let output = loop {
            match attempt_once(state, &cancel, event_tx).await? {
                AttemptOutcome::Done(out) => break out,
                AttemptOutcome::Cancelled(partial) => {
                    let _ = event_tx
                        .try_send(CoreEvent::AgentChunk("\n\n[Task interrupted]".to_string()));
                    push_partial_assistant(
                        state,
                        &partial.ai_message,
                        &partial.ai_reasoning,
                        &partial.tool_calls_map,
                        event_tx,
                    )
                    .await?;
                    drop_residual_interjections(state, event_tx).await;
                    super::dispatch::finalize_cancelled(state, event_tx, all_usages).await?;
                    return Ok(());
                }
                AttemptOutcome::Retryable(err) if retry_no < state.retry.max_retries => {
                    retry_no += 1;
                    let delay = retry::backoff_delay(&state.retry, retry_no);
                    let _ = event_tx.try_send(CoreEvent::AgentRetry {
                        attempt: retry_no,
                        max_retries: state.retry.max_retries,
                        delay_ms: delay.as_millis() as u64,
                        reason: err.to_string(),
                    });
                    if !retry::sleep_cancellable(delay, &cancel).await {
                        drop_residual_interjections(state, event_tx).await;
                        super::dispatch::finalize_cancelled(state, event_tx, all_usages).await?;
                        return Ok(());
                    }
                }
                AttemptOutcome::Retryable(err) | AttemptOutcome::Fatal(err) => {
                    return Err(Box::new(err));
                }
            }
        };

        let StreamOutput {
            ai_message,
            ai_reasoning,
            tool_calls_map,
            is_tool_call,
            last_usage,
        } = output;

        if is_tool_call && !tool_calls_map.is_empty() {
            let full_tool_calls: Vec<ChatCompletionMessageToolCalls> = tool_calls_map
                .into_values()
                .map(|partial| {
                    ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                        id: partial.id,
                        function: FunctionCall {
                            name: partial.name.unwrap_or_default(),
                            arguments: partial.arguments,
                        },
                    })
                })
                .collect();

            let pushed_msg: SharedMessage = {
                let msg: SharedMessage = Arc::new(
                    CompatibleChatCompletionRequestAssistantMessage {
                        base: ChatCompletionRequestAssistantMessage {
                            content: if ai_message.is_empty() {
                                None
                            } else {
                                Some(ChatCompletionRequestAssistantMessageContent::Text(
                                    ai_message,
                                ))
                            },
                            tool_calls: Some(full_tool_calls.clone()),
                            ..Default::default()
                        },
                        reasoning_content: if ai_reasoning.is_empty() {
                            None
                        } else {
                            Some(ai_reasoning)
                        },
                    }
                    .into(),
                );
                state.messages.push(Arc::clone(&msg));
                if let Some(u) = last_usage {
                    all_usages.push(u);
                    let _ = event_tx.try_send(CoreEvent::UsageUpdate {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                        cached_tokens: u.cached_tokens,
                    });
                }
                msg
            };

            emit_persist_message(
                event_tx,
                &pushed_msg,
                last_usage,
                &state.model_display,
                None,
            );

            super::dispatch::handle_tool_calls_stream(state, &full_tool_calls, &cancel, event_tx)
                .await?;

            if cancel.load(Ordering::Relaxed) {
                drop_residual_interjections(state, event_tx).await;
                super::dispatch::finalize_cancelled(state, event_tx, all_usages).await?;
                return Ok(());
            }
        } else {
            let pushed_msg: SharedMessage = {
                let msg: SharedMessage = Arc::new(
                    CompatibleChatCompletionRequestAssistantMessage {
                        base: ChatCompletionRequestAssistantMessage {
                            content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                                ai_message,
                            )),
                            ..Default::default()
                        },
                        reasoning_content: if ai_reasoning.is_empty() {
                            None
                        } else {
                            Some(ai_reasoning)
                        },
                    }
                    .into(),
                );
                state.messages.push(Arc::clone(&msg));
                if let Some(u) = last_usage {
                    all_usages.push(u);
                    let _ = event_tx.try_send(CoreEvent::UsageUpdate {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                        cached_tokens: u.cached_tokens,
                    });
                }
                msg
            };

            emit_persist_message(
                event_tx,
                &pushed_msg,
                last_usage,
                &state.model_display,
                None,
            );
            // A late interjection keeps the run alive for another round
            if drain_interjections(state, event_tx).await > 0 {
                continue;
            }
            break;
        }
    }

    // The run is over: interjections accepted in the narrow window since the
    // last drain (or while the final answer streamed) will never be injected
    drop_residual_interjections(state, event_tx).await;
    let final_messages = std::mem::take(&mut state.messages);
    let _ = event_tx.try_send(CoreEvent::AgentComplete {
        messages: final_messages,
        usages: all_usages,
        model: state.model_display.clone(),
        reasoning_effort: state.reasoning_effort,
        status: TaskStatus::Completed,
    });
    Ok(())
}

pub(super) async fn push_partial_assistant(
    state: &mut AgentStreamState,
    ai_message: &str,
    ai_reasoning: &str,
    tool_calls_map: &BTreeMap<u32, PartialToolCall>,
    event_tx: &CoreEventTx,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if ai_message.is_empty() && ai_reasoning.is_empty() && tool_calls_map.is_empty() {
        return Ok(());
    }

    // Empty-id entries are stream-interrupted half-deliveries no tool result
    // can answer — including them would create a dangling sequence
    let tool_calls: Vec<ChatCompletionMessageToolCalls> = tool_calls_map
        .values()
        .filter(|partial| !partial.id.is_empty())
        .map(|partial| {
            ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                id: partial.id.clone(),
                function: FunctionCall {
                    name: partial.name.clone().unwrap_or_default(),
                    arguments: partial.arguments.clone(),
                },
            })
        })
        .collect();

    let pushed_msgs: Vec<SharedMessage> = {
        let mut msgs = Vec::new();

        let assistant_msg: SharedMessage = Arc::new(
            CompatibleChatCompletionRequestAssistantMessage {
                base: ChatCompletionRequestAssistantMessage {
                    content: if ai_message.is_empty() && tool_calls.is_empty() {
                        Some(ChatCompletionRequestAssistantMessageContent::Text(
                            "[Task interrupted]".to_string(),
                        ))
                    } else if ai_message.is_empty() {
                        None
                    } else {
                        Some(ChatCompletionRequestAssistantMessageContent::Text(
                            ai_message.to_string(),
                        ))
                    },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls.clone())
                    },
                    ..Default::default()
                },
                reasoning_content: if ai_reasoning.is_empty() {
                    None
                } else {
                    Some(ai_reasoning.to_string())
                },
            }
            .into(),
        );
        state.messages.push(Arc::clone(&assistant_msg));
        msgs.push(assistant_msg);

        for tc in &tool_calls {
            if let ChatCompletionMessageToolCalls::Function(f) = tc {
                let tool_msg: SharedMessage = Arc::new(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(
                            "Tool execution aborted".to_string(),
                        ),
                        tool_call_id: f.id.clone(),
                    }
                    .into(),
                );
                state.messages.push(Arc::clone(&tool_msg));
                msgs.push(tool_msg);
            }
        }
        msgs
    };

    for msg in &pushed_msgs {
        emit_persist_message(event_tx, msg, None, &state.model_display, None);
    }

    Ok(())
}
