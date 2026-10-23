use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_openai::{
    Client,
    config::OpenAIConfig,
    error::OpenAIError,
    types::chat::{
        ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
    },
};
use futures_util::StreamExt;

use super::event::{CompactUsage, CoreEvent, CoreEventTx, TaskStatus};
use super::media::Attachment;
use super::models::{
    CompatibleChatCompletionRequestMessage, CompatibleCreateChatCompletionRequestArgs,
    CompatibleCreateChatCompletionStreamResponse, SharedMessage,
};
use super::retry::{self, RetryConfig};
use super::tools::ToolRegistry;

mod dispatch;
mod messages;
mod stream;

#[cfg(test)]
mod tests;

pub use messages::build_user_message;
use messages::strip_media_for_compaction;
use stream::{AgentStreamState, finalize_messages, run_stream_loop};

/// 取消标志轮询间隔。通过定时 sleep 轮询 `AtomicBool` 实现取消，
/// 该间隔即为取消响应的最大延迟上限。
pub(super) const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Agent，封装了与 LLM 交互的核心逻辑
pub struct Agent {
    client: Client<OpenAIConfig>,
    tool_registry: ToolRegistry,
    messages: Vec<SharedMessage>,
    /// API 请求使用的模型 ID（如 `deepseek-chat`）
    model: String,
    /// 展示用模型名（`provider/model`，与用量统计口径一致）
    model_display: String,
    max_tokens: u32,
    /// 当前模型是否支持视觉（图片）输入；不支持时附件在请求组装层降级为提示文本
    supports_vision: bool,
    plan_mode: bool,
    cancel: Arc<AtomicBool>,
    permission: crate::permission::PermissionManager,
    work_dir: String,
    event_hub: super::event::EventHub,
    /// 瞬态错误（网络/429/5xx/流中断）自动重试策略
    retry: RetryConfig,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: OpenAIConfig,
        model: &str,
        model_display: &str,
        max_tokens: u32,
        supports_vision: bool,
        permission: crate::permission::PermissionManager,
        work_dir: &str,
        retry: RetryConfig,
    ) -> Self {
        Self {
            client: Client::with_config(config),
            tool_registry: ToolRegistry::new(),
            messages: Vec::new(),
            model: model.to_string(),
            model_display: model_display.to_string(),
            max_tokens,
            supports_vision,
            plan_mode: false,
            cancel: Arc::new(AtomicBool::new(false)),
            permission,
            work_dir: work_dir.to_string(),
            event_hub: super::event::EventHub::new(),
            retry,
        }
    }

    /// 事件通道句柄（ask_user / subagent 转发等无法在构造期拿到通道的场景）
    pub fn event_hub(&self) -> &super::event::EventHub {
        &self.event_hub
    }

    pub fn interrupt(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 取消标志句柄（Web 中断端点不经过会话锁，直接置位）
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// 开关规划模式。开启后，发给 LLM 的工具列表会过滤掉 edit/write，
    /// 并在用户消息中注入规划模式只读提示词。
    pub fn set_plan_mode(&mut self, on: bool) {
        self.plan_mode = on;
    }

    /// 当前是否处于规划模式
    pub fn plan_mode(&self) -> bool {
        self.plan_mode
    }

    pub fn switch_model(
        &mut self,
        config: OpenAIConfig,
        model: &str,
        model_display: &str,
        max_tokens: u32,
        supports_vision: bool,
    ) {
        self.client = Client::with_config(config);
        self.model = model.to_string();
        self.model_display = model_display.to_string();
        self.max_tokens = max_tokens;
        self.supports_vision = supports_vision;
    }

    pub fn permission(&self) -> &crate::permission::PermissionManager {
        &self.permission
    }

    /// 注册一个工具。
    /// config 中该工具权限类别被 `*` + deny 禁用时直接跳过注册，
    /// 使工具对模型完全不可见（不可被会话级 always 绕过）。
    pub fn register_tool(&mut self, tool: Box<dyn super::tools::Tool>) {
        if self.permission.is_disabled(tool.permission_category()) {
            return;
        }
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
    /// 在独立 tokio task 中运行，完成后通过 AgentComplete 事件回传最终消息
    pub fn chat_stream(
        &mut self,
        user_input: &str,
        attachments: Vec<Attachment>,
        event_tx: CoreEventTx,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.messages
            .push(Arc::new(build_user_message(user_input, &attachments)));

        let client = self.client.clone();
        let tool_registry = self.tool_registry.clone_registry();
        let messages = self.messages.clone();
        let permission = self.permission.clone();
        let work_dir = self.work_dir.clone();

        let mut state = AgentStreamState {
            client,
            tool_registry,
            messages,
            model: self.model.clone(),
            model_display: self.model_display.clone(),
            max_tokens: self.max_tokens,
            supports_vision: self.supports_vision,
            plan_mode: self.plan_mode,
            permission,
            work_dir,
            retry: self.retry.clone(),
        };

        let tx_clone = event_tx.clone();
        let cancel = self.cancel.clone();
        self.cancel.store(false, Ordering::Relaxed);
        tokio::spawn(async move {
            if let Err(e) = run_stream_loop(&mut state, cancel, &tx_clone).await {
                let _ = tx_clone.try_send(CoreEvent::AgentChunk(format!("\n[Error: {}]", e)));
                finalize_messages(
                    &mut state.messages,
                    format!("[Error: {}]", e),
                    &tx_clone,
                    Vec::new(),
                    &state.model_display,
                    TaskStatus::Error,
                );
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
        event_tx: CoreEventTx,
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
            .map(strip_media_for_compaction)
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
        // 安全约定：chat_stream 与 request_compaction 共享同一个 cancel 标志。
        // 不会并发运行：TUI 层 compact_conversation() 会检查 is_processing，
        // 确保流式对话进行中不会触发压缩，反之亦然。
        self.cancel.store(false, Ordering::Relaxed);
        let cancel = self.cancel.clone();
        let retry_cfg = self.retry.clone();
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

            // 压缩请求静默重试（无 UI 重置语义）：失败尝试的部分摘要丢弃，
            // 全部用尽才上报 CompactError
            let mut retry_no: u32 = 0;
            let outcome: Result<(String, Option<CompactUsage>), String> = loop {
                let request = match builder.build() {
                    Ok(r) => r,
                    Err(e) => break Err(e.to_string()),
                };

                let mut stream = match client
                    .chat()
                    .create_stream_byot::<_, CompatibleCreateChatCompletionStreamResponse>(request)
                    .await
                {
                    Ok(s) => s,
                    Err(e) => match compact_retry(&retry_cfg, &mut retry_no, e, &cancel).await {
                        Some(msg) => break Err(msg),
                        None => continue,
                    },
                };

                let mut summary = String::new();
                let mut last_usage: Option<CompactUsage> = None;
                let mut stream_err: Option<OpenAIError> = None;
                let mut cancelled = false;
                while let Some(chunk_result) = stream.next().await {
                    if cancel.load(Ordering::Relaxed) {
                        cancelled = true;
                        break;
                    }
                    match chunk_result {
                        Ok(chunk) => {
                            if let Some(usage) = &chunk.usage {
                                last_usage = Some(CompactUsage {
                                    prompt_tokens: usage.prompt_tokens,
                                    completion_tokens: usage.completion_tokens,
                                });
                            }
                            for choice in chunk.choices {
                                if let Some(content) = choice.delta.base.content {
                                    let _ = tx.try_send(CoreEvent::CompactChunk(content.clone()));
                                    summary.push_str(&content);
                                }
                            }
                        }
                        Err(e) => {
                            stream_err = Some(e);
                            break;
                        }
                    }
                }
                if cancelled {
                    break Err("压缩已取消".to_string());
                }
                if let Some(e) = stream_err {
                    match compact_retry(&retry_cfg, &mut retry_no, e, &cancel).await {
                        Some(msg) => break Err(msg),
                        None => continue,
                    }
                }
                break Ok((summary, last_usage));
            };

            match outcome {
                Ok((summary, usage)) => {
                    if summary.trim().is_empty() {
                        let _ = tx.try_send(CoreEvent::CompactError("压缩结果为空".to_string()));
                    } else {
                        let _ = tx.try_send(CoreEvent::CompactComplete {
                            summary,
                            session_id,
                            usage,
                        });
                    }
                }
                Err(msg) => {
                    let _ = tx.try_send(CoreEvent::CompactError(msg));
                }
            }
        });

        Ok(())
    }
}

/// 压缩请求的瞬态错误重试：可重试且未超限时退避等待后返回 `None`
/// （调用方 `continue` 重发）；否则返回错误消息（不可重试 / 次数用尽 / 取消）。
async fn compact_retry(
    cfg: &RetryConfig,
    retry_no: &mut u32,
    err: OpenAIError,
    cancel: &AtomicBool,
) -> Option<String> {
    if !retry::is_retryable(&err) || *retry_no >= cfg.max_retries {
        return Some(err.to_string());
    }
    *retry_no += 1;
    let delay = retry::backoff_delay(cfg, *retry_no);
    if !retry::sleep_cancellable(delay, cancel).await {
        return Some("压缩已取消".to_string());
    }
    None
}
