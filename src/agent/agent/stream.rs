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

use super::messages::sanitize_messages_for_request;
use crate::agent::event::{CoreEvent, CoreEventTx, MessageUsage, TaskStatus};
use crate::agent::models::{
    CompatibleChatCompletionRequestAssistantMessage, CompatibleChatCompletionRequestMessage,
    CompatibleCreateChatCompletionRequestArgs, CompatibleCreateChatCompletionStreamResponse,
    SharedMessage, ThinkingConfig,
};
use crate::agent::retry::{self, RetryConfig};
use crate::agent::tools::ToolRegistry;
use crate::permission::PermissionManager;

use super::CANCEL_POLL_INTERVAL;

/// 规划模式提示词。开启 plan 模式时，会注入到本轮最后一条用户消息中，
/// 作为软约束提醒模型处于只读阶段。
const PLAN_MODE_PROMPT: &str = crate::prompts::PLAN_MODE;

/// 用于在流式响应中累加 tool_calls 的分片
#[derive(Default)]
pub(super) struct PartialToolCall {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) arguments: String,
}

/// 流式处理过程中的 Agent 状态，用于在独立 tokio task 中运行
pub(super) struct AgentStreamState {
    pub(super) client: Client<OpenAIConfig>,
    pub(super) tool_registry: ToolRegistry,
    pub(super) messages: Vec<SharedMessage>,
    pub(super) model: String,
    /// 快照的展示模型名（事件携带，用量统计归属依据）
    pub(super) model_display: String,
    pub(super) max_tokens: u32,
    /// 当前模型是否支持视觉输入（决定图片附件是否降级）
    pub(super) supports_vision: bool,
    pub(super) plan_mode: bool,
    pub(super) permission: PermissionManager,
    pub(super) work_dir: String,
    /// 瞬态错误自动重试策略（chat_stream 启动时的快照）
    pub(super) retry: RetryConfig,
}

impl AgentStreamState {
    fn build_request(
        &self,
    ) -> Result<CompatibleCreateChatCompletionRequestArgs, Box<dyn Error + Send + Sync>> {
        let tools = self.tool_registry.to_chat_tools_filtered(self.plan_mode);
        let mut messages = self.messages.clone();
        // 规划模式下，把只读提示词注入到最后一条用户消息（在克隆上操作，不污染存储）。
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
                        // 带图片附件的消息：提示词追加为独立的 text part
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
        // 视觉能力兜底校验：
        // 不支持视觉的模型收到图片时替换为错误文本，由模型转告用户
        let messages = sanitize_messages_for_request(&messages, self.supports_vision);
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
            .thinking(ThinkingConfig::enabled())
            .extra("stream_options", serde_json::json!({"include_usage": true}));
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

/// 确保消息列表以 Assistant 消息结尾，然后发送 AgentComplete 事件。
/// 若最后一条不是 Assistant，则追加一条内容为 `fallback_content` 的 Assistant 消息。
/// 被 `chat_stream` 错误路径和 `finalize_cancelled` 共用，避免逻辑重复。
pub(super) fn finalize_messages(
    messages: &mut Vec<SharedMessage>,
    fallback_content: String,
    event_tx: &CoreEventTx,
    usages: Vec<MessageUsage>,
    model: &str,
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
        status,
    });
}

/// 单次 LLM 请求（连接 + 流消费）的累积输出
#[derive(Default)]
pub(super) struct StreamOutput {
    pub(super) ai_message: String,
    pub(super) ai_reasoning: String,
    pub(super) tool_calls_map: BTreeMap<u32, PartialToolCall>,
    pub(super) is_tool_call: bool,
    pub(super) last_usage: Option<MessageUsage>,
}

/// 单次 LLM 请求尝试的结果
enum AttemptOutcome {
    /// 流正常结束（完整输出）
    Done(StreamOutput),
    /// 用户取消（携带已收到的部分输出，供中断收尾保留）
    Cancelled(StreamOutput),
    /// 瞬态错误（部分输出直接丢弃，不进入上下文）
    Retryable(OpenAIError),
    /// 不可重试错误
    Fatal(OpenAIError),
}

/// 按可重试性归类请求错误
fn classify_stream_error(e: OpenAIError) -> AttemptOutcome {
    if retry::is_retryable(&e) {
        AttemptOutcome::Retryable(e)
    } else {
        AttemptOutcome::Fatal(e)
    }
}

/// 执行一次 LLM 请求：建立流连接并消费完毕，累积输出。
/// 任何失败都不修改 `state.messages` —— 重试或报错由调用方决定；
/// 失败尝试的部分输出只存在于本函数返回值与 UI 流式缓冲中。
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
                        // 连接阶段取消：尚无任何输出
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
    let mut all_usages: Vec<MessageUsage> = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            super::dispatch::finalize_cancelled(state, event_tx, all_usages).await?;
            return Ok(());
        }

        // ── 单次 LLM 请求（连接 + 流消费），瞬态错误自动重试 ──
        // 失败尝试的部分输出不进入消息上下文、不落库；仅通过 AgentRetry
        // 事件通知 UI 丢弃已流式渲染的内容后重新生成。
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
                    // 退避等待期间用户取消：中止重试，走中断收尾
                    if !retry::sleep_cancellable(delay, &cancel).await {
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
            break;
        }
    }

    let final_messages = std::mem::take(&mut state.messages);
    let _ = event_tx.try_send(CoreEvent::AgentComplete {
        messages: final_messages,
        usages: all_usages,
        model: state.model_display.clone(),
        status: TaskStatus::Completed,
    });
    Ok(())
}

async fn push_partial_assistant(
    state: &mut AgentStreamState,
    ai_message: &str,
    ai_reasoning: &str,
    tool_calls_map: &BTreeMap<u32, PartialToolCall>,
    event_tx: &CoreEventTx,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if ai_message.is_empty() && ai_reasoning.is_empty() && tool_calls_map.is_empty() {
        return Ok(());
    }

    let tool_calls: Vec<ChatCompletionMessageToolCalls> = tool_calls_map
        .values()
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
            if let ChatCompletionMessageToolCalls::Function(f) = tc
                && !f.id.is_empty()
            {
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
