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

use super::event::{
    CompactUsage, CoreEvent, CoreEventTx, InterjectSlot, Interjection, TaskStatus,
    deliver_interjection,
};
use super::media::Attachment;
use super::models::{
    CompatibleChatCompletionRequestMessage, CompatibleCreateChatCompletionRequestArgs,
    CompatibleCreateChatCompletionStreamResponse, ReasoningEffort, SharedMessage,
};
use super::retry::{self, RetryConfig};
use super::tools::ToolRegistry;

mod dispatch;
mod messages;
mod stream;

#[cfg(test)]
mod tests;

pub use messages::build_user_message;
use messages::{ensure_tool_call_parity, strip_media_for_compaction};
use stream::{AgentStreamState, finalize_messages, run_stream_loop};

/// Bounds the worst-case cancel latency (cancel is a polled `AtomicBool`)
pub(super) const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct Agent {
    client: Client<OpenAIConfig>,
    tool_registry: ToolRegistry,
    messages: Vec<SharedMessage>,
    model: String,
    /// Usage stats are keyed by this, not `model`
    model_display: String,
    max_tokens: u32,
    supports_vision: bool,
    native_reasoning: bool,
    reasoning_effort: ReasoningEffort,
    plan_mode: bool,
    cancel: Arc<AtomicBool>,
    permission: crate::permission::PermissionManager,
    work_dir: String,
    event_hub: super::event::EventHub,
    retry: RetryConfig,
    /// Swapped per chat_stream; a stale sender fails once its run task drops
    /// the receiver. Shared with SessionManager so delivery skips the session
    /// lock the runner holds for the whole run.
    interject_slot: InterjectSlot,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: OpenAIConfig,
        model: &str,
        model_display: &str,
        max_tokens: u32,
        supports_vision: bool,
        native_reasoning: bool,
        reasoning_effort: ReasoningEffort,
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
            native_reasoning,
            reasoning_effort,
            plan_mode: false,
            cancel: Arc::new(AtomicBool::new(false)),
            permission,
            work_dir: work_dir.to_string(),
            event_hub: super::event::EventHub::new(),
            retry,
            interject_slot: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn event_hub(&self) -> &super::event::EventHub {
        &self.event_hub
    }

    pub fn interrupt(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Web interrupt endpoint sets this without the session lock
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub(crate) fn interject_slot(&self) -> InterjectSlot {
        self.interject_slot.clone()
    }

    /// False = no active chat run (channel unset, receiver dropped, run
    /// cancelling, or the bounded queue is full). Delivery is best-effort:
    /// `true` only means the interjection was buffered; it is confirmed by
    /// the `CoreEvent::UserMessage` echo once actually injected, and reported
    /// via `CoreEvent::InterjectDropped` if the run ends before that.
    pub fn interject(&self, interjection: Interjection) -> bool {
        // A cancelling run never drains at a boundary again; rejecting here
        // lets the caller fall back (TUI keeps the item queued, web turns the
        // request into a normal send) instead of silently dropping it
        if self.cancel.load(Ordering::Relaxed) {
            return false;
        }
        deliver_interjection(&self.interject_slot, interjection)
    }

    pub fn set_plan_mode(&mut self, on: bool) {
        self.plan_mode = on;
    }

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
        native_reasoning: bool,
    ) {
        self.client = Client::with_config(config);
        self.model = model.to_string();
        self.model_display = model_display.to_string();
        self.max_tokens = max_tokens;
        self.supports_vision = supports_vision;
        self.native_reasoning = native_reasoning;
    }

    pub fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.reasoning_effort = effort;
    }

    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    pub fn permission(&self) -> &crate::permission::PermissionManager {
        &self.permission
    }

    /// Denied by a `*` config rule = never registered: invisible to the
    /// model and not re-enableable by session-level "always" grants
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

    pub fn clear_messages(&mut self) {
        self.messages.clear();
    }

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

        // Fresh bounded channel per run: unconsumed interjections from a
        // previous run are dropped with its receiver (and reported as
        // `InterjectDropped` by the run's exit paths); the bound keeps a
        // runaway client from buffering unbounded memory mid-run
        let (interject_tx, interject_rx) =
            tokio::sync::mpsc::channel::<Interjection>(super::event::INTERJECT_QUEUE_LIMIT);
        if let Ok(mut slot) = self.interject_slot.lock() {
            *slot = Some(interject_tx);
        }

        let mut state = AgentStreamState {
            client,
            tool_registry,
            messages,
            model: self.model.clone(),
            model_display: self.model_display.clone(),
            max_tokens: self.max_tokens,
            supports_vision: self.supports_vision,
            native_reasoning: self.native_reasoning,
            reasoning_effort: self.reasoning_effort,
            plan_mode: self.plan_mode,
            permission,
            work_dir,
            retry: self.retry.clone(),
            interject_rx,
            delivered_interject_ids: std::collections::HashSet::new(),
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
                    state.reasoning_effort,
                    TaskStatus::Error,
                );
            }
        });

        Ok(())
    }

    pub fn sync_messages(&mut self, messages: Vec<SharedMessage>) {
        self.messages = messages;
    }

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
        // No tools on the compaction request, but strict providers still
        // validate tool_calls/tool pairing — a dangling sequence would 400
        // the whole /compact. The DB repair sweep only covers the tail, so
        // mid-sequence danglers must be fixed here.
        let conversation = ensure_tool_call_parity(&conversation);

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
        // Safety contract: chat_stream and request_compaction share one
        // cancel flag and never run concurrently — compact_conversation()
        // checks is_processing in the UI layer so neither can start while
        // the other is active.
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

            // Silent retry for the compaction request (no UI reset semantics):
            // partial summaries of failed attempts are dropped; CompactError
            // is reported only when all attempts are exhausted
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

/// Retry a transient compaction error: back off and return `None` (caller
/// re-issues the request) while retryable and under the limit; otherwise
/// return the error message (non-retryable / exhausted / cancelled).
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
