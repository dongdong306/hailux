use super::models::{
    CompatibleChatCompletionRequestAssistantMessage, CompatibleChatCompletionRequestMessage,
    CompatibleCreateChatCompletionRequestArgs, CompatibleCreateChatCompletionStreamResponse,
    SharedMessage, ThinkingConfig,
};
use super::tools::ToolRegistry;
use crate::tui::AppEvent;
use crate::tui::event::{EventTx, MessageUsage, TaskStatus};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
        ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
        ChatCompletionToolChoiceOption, FinishReason, FunctionCall, ToolChoiceOptions,
    },
};
use futures_util::StreamExt;
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 规划模式提示词。开启 plan 模式时，会注入到本轮最后一条用户消息中，
/// 作为软约束提醒模型处于只读阶段。改编自 opencode 的 plan.txt。
const PLAN_MODE_PROMPT: &str = crate::prompts::PLAN_MODE;

/// 取消标志轮询间隔。通过定时 sleep 轮询 `AtomicBool` 实现取消，
/// 该间隔即为取消响应的最大延迟上限。
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 用于在流式响应中累加 tool_calls 的分片
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: Option<String>,
    arguments: String,
}

/// Agent，封装了与 LLM 交互的核心逻辑
pub struct Agent {
    client: Client<OpenAIConfig>,
    tool_registry: ToolRegistry,
    messages: Vec<SharedMessage>,
    model: String,
    max_tokens: u32,
    plan_mode: bool,
    cancel: Arc<AtomicBool>,
}

impl Agent {
    pub fn new(config: OpenAIConfig, model: &str, max_tokens: u32) -> Self {
        Self {
            client: Client::with_config(config),
            tool_registry: ToolRegistry::new(),
            messages: Vec::new(),
            model: model.to_string(),
            max_tokens,
            plan_mode: false,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn interrupt(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 开关规划模式。开启后，发给 LLM 的工具列表会过滤掉 edit/write，
    /// 并在用户消息中注入规划模式只读提示词。
    pub fn set_plan_mode(&mut self, on: bool) {
        self.plan_mode = on;
    }

    pub fn switch_model(&mut self, config: OpenAIConfig, model: &str, max_tokens: u32) {
        self.client = Client::with_config(config);
        self.model = model.to_string();
        self.max_tokens = max_tokens;
    }

    /// 注册一个工具
    pub fn register_tool(&mut self, tool: Box<dyn super::tools::Tool>) {
        self.tool_registry.register(tool);
    }

    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.messages.retain(|m| {
            !matches!(
                m.as_ref(),
                CompatibleChatCompletionRequestMessage::System(_)
            )
        });
        self.messages.insert(
            0,
            Arc::new(
                ChatCompletionRequestSystemMessage {
                    content: ChatCompletionRequestSystemMessageContent::Text(prompt.to_string()),
                    name: None,
                }
                .into(),
            ),
        );
    }

    /// 提取当前的 system prompt（用于切换会话后恢复）
    pub fn take_system_prompt(&self) -> Option<String> {
        self.messages.iter().find_map(|m| {
            if let CompatibleChatCompletionRequestMessage::System(sys) = m.as_ref() {
                match &sys.content {
                    ChatCompletionRequestSystemMessageContent::Text(t) => Some(t.clone()),
                    _ => None,
                }
            } else {
                None
            }
        })
    }

    /// 清空消息历史（保留 system prompt 的能力交给调用方）
    pub fn clear_messages(&mut self) {
        self.messages.clear();
    }

    /// 流式聊天，通过事件通道将输出发送给 TUI
    /// 返回一个 Arc<Mutex<Self>> 用于在独立 task 中运行
    pub fn chat_stream(
        &mut self,
        user_input: &str,
        event_tx: EventTx,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.messages.push(Arc::new(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(user_input.to_string()),
                name: None,
            }
            .into(),
        ));

        let client = self.client.clone();
        let tool_registry = self.tool_registry.clone_registry();
        let messages = self.messages.clone();

        let agent_handle = Arc::new(Mutex::new(AgentStreamState {
            client,
            tool_registry,
            messages,
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            plan_mode: self.plan_mode,
        }));

        let handle = agent_handle.clone();
        let tx_clone = event_tx.clone();
        let cancel = self.cancel.clone();
        self.cancel.store(false, Ordering::Relaxed);
        tokio::spawn(async move {
            if let Err(e) = run_stream_loop(handle.clone(), cancel, &tx_clone).await {
                let _ = tx_clone.try_send(AppEvent::AgentChunk(format!("\n[Error: {}]", e)));
                let mut messages = {
                    let state = handle.lock().map_err(|e| e.to_string());
                    match state {
                        Ok(mut state) => std::mem::take(&mut state.messages),
                        Err(_) => Vec::new(),
                    }
                };
                if !messages.is_empty() {
                    let needs_assistant = !matches!(
                        messages.last().map(|m| m.as_ref()),
                        Some(CompatibleChatCompletionRequestMessage::Assistant(_))
                    );
                    if needs_assistant {
                        let msg: SharedMessage = Arc::new(
                            CompatibleChatCompletionRequestAssistantMessage {
                                base: ChatCompletionRequestAssistantMessage {
                                    content: Some(
                                        ChatCompletionRequestAssistantMessageContent::Text(
                                            format!("[Error: {}]", e),
                                        ),
                                    ),
                                    ..Default::default()
                                },
                                reasoning_content: None,
                            }
                            .into(),
                        );
                        messages.push(Arc::clone(&msg));
                        let _ = tx_clone.try_send(AppEvent::PersistMessage {
                            msg,
                            usage: None,
                            display: None,
                        });
                    }
                }
                let _ = tx_clone.try_send(AppEvent::AgentComplete {
                    messages,
                    usages: Vec::new(),
                    status: TaskStatus::Error,
                });
            }
        });

        Ok(())
    }

    /// 流式聊天完成后，同步消息历史
    pub fn sync_messages(&mut self, messages: Vec<SharedMessage>) {
        self.messages = messages;
    }

    /// 返回非 System 消息的数量（用于判断是否可以压缩）
    pub fn messages_excluding_system_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| {
                !matches!(
                    m.as_ref(),
                    CompatibleChatCompletionRequestMessage::System(_)
                )
            })
            .count()
    }

    /// 应用压缩结果：保留 System 消息，替换其余为摘要 User 消息
    pub fn apply_compaction(&mut self, summary: &str) {
        self.messages.retain(|m| {
            matches!(
                m.as_ref(),
                CompatibleChatCompletionRequestMessage::System(_)
            )
        });
        self.messages.push(Arc::new(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(format!(
                    "[Context Summary]\n{summary}"
                )),
                name: None,
            }
            .into(),
        ));
    }

    /// 启动压缩 LLM 调用（无工具），流式输出摘要。
    /// 摘要 chunk 通过 `CompactChunk` 事件发送，完成后发送 `CompactComplete`。
    pub fn request_compaction(
        &self,
        event_tx: EventTx,
        session_id: String,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = self.client.clone();
        let model = self.model.clone();
        let max_tokens = self.max_tokens;

        let conversation: Vec<SharedMessage> = self
            .messages
            .iter()
            .filter(|m| {
                !matches!(
                    m.as_ref(),
                    CompatibleChatCompletionRequestMessage::System(_)
                )
            })
            .cloned()
            .collect();

        if conversation.len() < 2 {
            return Err("Not enough messages to compact".into());
        }

        let compact_system: SharedMessage = Arc::new(
            ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(
                    crate::prompts::COMPACT.to_string(),
                ),
                name: None,
            }
            .into(),
        );

        let mut compact_messages = vec![compact_system];
        compact_messages.extend(conversation);
        compact_messages.push(Arc::new(
            ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(
                    "请总结以上对话".to_string(),
                ),
                name: None,
            }
            .into(),
        ));

        let tx = event_tx.clone();
        self.cancel.store(false, Ordering::Relaxed);
        let cancel = self.cancel.clone();
        tokio::spawn(async move {
            let mut builder = {
                let mut b = CompatibleCreateChatCompletionRequestArgs::default();
                b.max_completion_tokens(max_tokens)
                    .model(&model)
                    .stream(true)
                    .messages(compact_messages)
                    .extra("stream_options", serde_json::json!({"include_usage": true}));
                b
            };

            let request = match builder.build() {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.try_send(AppEvent::CompactError(e.to_string()));
                    return;
                }
            };

            let mut stream = match client
                .chat()
                .create_stream_byot::<_, CompatibleCreateChatCompletionStreamResponse>(request)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.try_send(AppEvent::CompactError(e.to_string()));
                    return;
                }
            };

            let mut summary = String::new();
            while let Some(chunk_result) = stream.next().await {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.try_send(AppEvent::CompactError("压缩已取消".to_string()));
                    return;
                }
                match chunk_result {
                    Ok(chunk) => {
                        for choice in chunk.choices {
                            if let Some(content) = choice.delta.base.content {
                                let _ = tx.try_send(AppEvent::CompactChunk(content.clone()));
                                summary.push_str(&content);
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.try_send(AppEvent::CompactError(e.to_string()));
                        return;
                    }
                }
            }

            if summary.trim().is_empty() {
                let _ = tx.try_send(AppEvent::CompactError("压缩结果为空".to_string()));
            } else {
                let _ = tx.try_send(AppEvent::CompactComplete {
                    summary,
                    session_id,
                });
            }
        });

        Ok(())
    }
}

/// 流式处理过程中的 Agent 状态，用于在独立 tokio task 中运行
struct AgentStreamState {
    client: Client<OpenAIConfig>,
    tool_registry: ToolRegistry,
    messages: Vec<SharedMessage>,
    model: String,
    max_tokens: u32,
    plan_mode: bool,
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
                    if let ChatCompletionRequestUserMessageContent::Text(t) = &mut u.content {
                        t.push_str("\n\n");
                        t.push_str(PLAN_MODE_PROMPT);
                    }
                    break;
                }
            }
        }
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

fn emit_persist_message(
    event_tx: &EventTx,
    msg: &SharedMessage,
    usage: Option<(u32, u32)>,
    display: Option<String>,
) {
    let _ = event_tx.try_send(AppEvent::PersistMessage {
        msg: Arc::clone(msg),
        usage,
        display,
    });
}

async fn run_stream_loop(
    state: Arc<Mutex<AgentStreamState>>,
    cancel: Arc<AtomicBool>,
    event_tx: &EventTx,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut all_usages: Vec<MessageUsage> = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            finalize_cancelled(&state, event_tx, all_usages).await?;
            return Ok(());
        }

        let request = {
            let state = state.lock().map_err(|e| e.to_string())?;
            state.build_request()?.build()?
        };

        let client = {
            let state = state.lock().map_err(|e| e.to_string())?;
            state.client.clone()
        };

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
                            let _ = event_tx.try_send(AppEvent::AgentChunk("\n\n[Task interrupted]".to_string()));
                            finalize_cancelled(&state, event_tx, all_usages).await?;
                            return Ok(());
                        }
                    }
                }
            }
            result_stream?
        };

        let mut ai_message = String::new();
        let mut ai_reasoning = String::new();
        let mut tool_calls_map: BTreeMap<u32, PartialToolCall> = BTreeMap::new();
        let mut is_tool_call = false;
        let mut stream_cancelled = false;
        let mut last_usage: Option<(u32, u32)> = None;

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        None => break,
                        Some(Ok(chunk)) => {
                            if let Some(usage) = &chunk.usage {
                                last_usage = Some((usage.prompt_tokens, usage.completion_tokens));
                            }
                            for choice in chunk.choices {
                                if let Some(content) = choice.delta.base.content {
                                    let _ = event_tx.try_send(AppEvent::AgentChunk(content.clone()));
                                    ai_message.push_str(&content);
                                }

                                if let Some(reasoning) = choice.delta.reasoning_content {
                                    let _ = event_tx.try_send(AppEvent::AgentReasoningChunk(reasoning.clone()));
                                    ai_reasoning.push_str(&reasoning);
                                }

                                if let Some(tool_calls) = choice.delta.base.tool_calls {
                                    is_tool_call = true;
                                    for tc in tool_calls {
                                        let entry = tool_calls_map
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
                                    is_tool_call = true;
                                }
                            }
                        }
                        Some(Err(e)) => return Err(Box::new(e)),
                    }
                }
                _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {
                    if cancel.load(Ordering::Relaxed) {
                        stream_cancelled = true;
                        break;
                    }
                }
            }
        }

        if stream_cancelled {
            let _ = event_tx.try_send(AppEvent::AgentChunk("\n\n[Task interrupted]".to_string()));
            push_partial_assistant(
                &state,
                &ai_message,
                &ai_reasoning,
                &tool_calls_map,
                event_tx,
            )
            .await?;
            finalize_cancelled(&state, event_tx, all_usages).await?;
            return Ok(());
        }

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
                let mut state = state.lock().map_err(|e| e.to_string())?;
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
                if let Some((pt, ct)) = last_usage {
                    all_usages.push(MessageUsage {
                        prompt_tokens: pt,
                        completion_tokens: ct,
                    });
                    let _ = event_tx.try_send(AppEvent::UsageUpdate {
                        prompt_tokens: pt,
                        completion_tokens: ct,
                    });
                }
                msg
            };

            emit_persist_message(event_tx, &pushed_msg, last_usage, None);

            handle_tool_calls_stream(&state, &full_tool_calls, &cancel, event_tx).await?;

            if cancel.load(Ordering::Relaxed) {
                finalize_cancelled(&state, event_tx, all_usages).await?;
                return Ok(());
            }
        } else {
            let pushed_msg: SharedMessage = {
                let mut state = state.lock().map_err(|e| e.to_string())?;
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
                if let Some((pt, ct)) = last_usage {
                    all_usages.push(MessageUsage {
                        prompt_tokens: pt,
                        completion_tokens: ct,
                    });
                    let _ = event_tx.try_send(AppEvent::UsageUpdate {
                        prompt_tokens: pt,
                        completion_tokens: ct,
                    });
                }
                msg
            };

            emit_persist_message(event_tx, &pushed_msg, last_usage, None);
            break;
        }
    }

    let final_messages = {
        let mut state = state.lock().map_err(|e| e.to_string())?;
        std::mem::take(&mut state.messages)
    };
    let _ = event_tx.try_send(AppEvent::AgentComplete {
        messages: final_messages,
        usages: all_usages,
        status: TaskStatus::Completed,
    });
    Ok(())
}

async fn push_partial_assistant(
    state: &Arc<Mutex<AgentStreamState>>,
    ai_message: &str,
    ai_reasoning: &str,
    tool_calls_map: &BTreeMap<u32, PartialToolCall>,
    event_tx: &EventTx,
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
        let mut state = state.lock().map_err(|e| e.to_string())?;
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
        emit_persist_message(event_tx, msg, None, None);
    }

    Ok(())
}

async fn finalize_cancelled(
    state: &Arc<Mutex<AgentStreamState>>,
    event_tx: &EventTx,
    usages: Vec<MessageUsage>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (final_messages, pending_msg, orphaned_tool_msgs) = {
        let mut guard = state.lock().map_err(|e| e.to_string())?;

        // 检查最后一条 assistant 消息是否有 orphaned tool_calls（缺少对应的 tool result）
        let mut orphaned_tool_msgs = Vec::new();
        if let Some(msg) = guard.messages.last()
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
            guard.messages.push(Arc::clone(msg));
        }

        let needs_assistant = !matches!(
            guard.messages.last().map(|m| m.as_ref()),
            Some(CompatibleChatCompletionRequestMessage::Assistant(_))
        );
        let pending = if needs_assistant {
            let msg: SharedMessage = Arc::new(
                CompatibleChatCompletionRequestAssistantMessage {
                    base: ChatCompletionRequestAssistantMessage {
                        content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                            "[Task interrupted]".to_string(),
                        )),
                        ..Default::default()
                    },
                    reasoning_content: None,
                }
                .into(),
            );
            guard.messages.push(Arc::clone(&msg));
            Some(msg)
        } else {
            None
        };
        (
            std::mem::take(&mut guard.messages),
            pending,
            orphaned_tool_msgs,
        )
    };
    for msg in &orphaned_tool_msgs {
        emit_persist_message(event_tx, msg, None, None);
        if let CompatibleChatCompletionRequestMessage::Tool(t) = msg.as_ref() {
            let result_text = match &t.content {
                ChatCompletionRequestToolMessageContent::Text(t) => t.clone(),
                _ => String::new(),
            };
            let _ = event_tx.try_send(AppEvent::ToolResult {
                name: String::new(),
                result: result_text,
                display: None,
                subagent_name: None,
            });
        }
    }
    if let Some(msg) = &pending_msg {
        emit_persist_message(event_tx, msg, None, None);
    }
    let _ = event_tx.try_send(AppEvent::AgentComplete {
        messages: final_messages,
        usages,
        status: TaskStatus::Interrupted,
    });
    Ok(())
}

async fn handle_tool_calls_stream(
    state: &Arc<Mutex<AgentStreamState>>,
    tool_calls: &[ChatCompletionMessageToolCalls],
    cancel: &Arc<AtomicBool>,
    event_tx: &EventTx,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    for tool_call in tool_calls {
        if cancel.load(Ordering::Relaxed) {
            let _ = event_tx.try_send(AppEvent::AgentChunk("\n\n[Task interrupted]".to_string()));
            let (id, name) = match tool_call {
                ChatCompletionMessageToolCalls::Function(f) => (&f.id, &f.function.name),
                ChatCompletionMessageToolCalls::Custom(c) => (&c.id, &c.custom_tool.name),
            };
            let _ = event_tx.try_send(AppEvent::ToolResult {
                name: name.clone(),
                result: "Tool execution aborted".to_string(),
                display: None,
                subagent_name: None,
            });
            let pushed_msg: SharedMessage = {
                let mut state = state.lock().map_err(|e| e.to_string())?;
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
            emit_persist_message(event_tx, &pushed_msg, None, None);
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

        let _ = event_tx.try_send(AppEvent::ToolCallStart {
            name: name.clone(),
            arguments: arguments.clone(),
            subagent_name: None,
        });

        let tool_arc = {
            let state = state.lock().map_err(|e| e.to_string())?;
            state.tool_registry.get_arc(name)
        };
        let result_display: (String, Option<String>) = if let Some(tool) = tool_arc {
            if tool.cancellable() {
                let cancel_clone = cancel.clone();
                let execute_fut = tool.execute_async_with_display(arguments);
                tokio::pin!(execute_fut);
                tokio::select! {
                    r = &mut execute_fut => {
                        r.unwrap_or_else(|err| (err.message, None))
                    }
                    _ = async {
                        while !cancel_clone.load(Ordering::Relaxed) {
                            tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
                        }
                    } => {
                        ("Tool execution aborted".to_string(), None)
                    }
                }
            } else {
                tool.execute_async_with_display(arguments)
                    .await
                    .unwrap_or_else(|err| (err.message, None))
            }
        } else {
            ("Unknown function".to_string(), None)
        };
        let (result, display) = result_display;

        let _ = event_tx.try_send(AppEvent::ToolResult {
            name: name.clone(),
            result: result.clone(),
            display: display.clone(),
            subagent_name: None,
        });

        let pushed_msg: SharedMessage = {
            let mut state = state.lock().map_err(|e| e.to_string())?;
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
        emit_persist_message(event_tx, &pushed_msg, None, display);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_agent() -> Agent {
        let config = OpenAIConfig::new().with_api_base("http://localhost:0");
        Agent::new(config, "test-model", 4096)
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
}
