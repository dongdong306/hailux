use color_eyre::Result;

use super::{App, AppState, Message};
use crate::storage::MessageRole;
use crate::tui::event::TaskStatus;

impl App {
    pub(super) async fn load_session_messages(&mut self) -> Result<()> {
        let Some(session_id) = &self.current_session_id else {
            return Ok(());
        };

        let compact_summary = self.storage.get_compact_summary(session_id).await?;
        let stored = self.storage.load_messages(session_id).await?;
        let active_stored = self.storage.load_active_messages(session_id).await?;

        let mut chat_messages = Vec::new();
        let mut display_messages = Vec::new();

        let mut tool_results: std::collections::HashMap<String, (String, Option<String>)> =
            std::collections::HashMap::new();
        for msg in &stored {
            if msg.role == MessageRole::Tool
                && let Some(id) = msg.tool_call_id.as_deref()
            {
                tool_results
                    .entry(id.to_string())
                    .or_insert((msg.content.clone(), msg.runtime_meta.clone()));
            }
        }

        let mut compact_marker_inserted = false;
        for (idx, msg) in stored.iter().enumerate() {
            if !compact_marker_inserted
                && compact_summary.is_some()
                && idx > 0
                && stored[idx - 1].compacted
                && !msg.compacted
            {
                display_messages.push(Message::CompactMarker {
                    summary: compact_summary.clone().unwrap(),
                    compacted_count: idx,
                    total_ms: None,
                });
                compact_marker_inserted = true;
            }

            match msg.role {
                MessageRole::User => {
                    let plan_mode = msg
                        .runtime_meta
                        .as_deref()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                        .and_then(|v| v.get("plan_mode").and_then(|v| v.as_bool()))
                        .unwrap_or(false);
                    display_messages.push(Message::User {
                        text: msg.content.clone(),
                        plan_mode,
                    });
                }
                MessageRole::Assistant => {
                    if let Some(reasoning) = msg.reasoning_content.as_ref()
                        && !reasoning.trim().is_empty()
                    {
                        display_messages.push(Message::AgentThinking {
                            text: reasoning.clone(),
                            think_ms: msg.think_ms.map(|v| v as u64),
                            thinking_started_at: None,
                        });
                    }
                    if !msg.content.is_empty() {
                        display_messages.push(Message::Agent(msg.content.clone()));
                    }
                    if let Some(tc_json) = msg.tool_calls.as_deref()
                        && let Ok(value) = serde_json::from_str::<serde_json::Value>(tc_json)
                        && let Some(arr) = value.as_array()
                    {
                        for tc in arr {
                            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let func = tc.get("function").or_else(|| tc.get("custom_tool"));
                            let name = func
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = func
                                .and_then(|f| f.get("arguments").or_else(|| f.get("input")))
                                .map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .unwrap_or_default();
                            if !name.is_empty() {
                                display_messages.push(Message::ToolCall {
                                    name: name.clone(),
                                    arguments,
                                });
                                if let Some((result, display)) = tool_results.remove(id) {
                                    display_messages.push(Message::ToolResult {
                                        name,
                                        result,
                                        display,
                                    });
                                }
                            }
                        }
                    }
                    if let Some(meta_str) = msg.runtime_meta.as_deref()
                        && let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str)
                        && let Some(total_ms) = meta.get("total_ms").and_then(|v| v.as_u64())
                    {
                        let model = meta
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let status = meta
                            .get("status")
                            .and_then(|v| v.as_str())
                            .map(|s| match s {
                                "interrupted" => TaskStatus::Interrupted,
                                "error" => TaskStatus::Error,
                                _ => TaskStatus::Completed,
                            })
                            .unwrap_or(TaskStatus::Completed);
                        display_messages.push(Message::AgentDone {
                            total_ms,
                            model,
                            status,
                        });
                    }
                }
                MessageRole::Tool => {}
                MessageRole::System => {}
            }
        }

        if !compact_marker_inserted
            && compact_summary.is_some()
            && !stored.is_empty()
            && stored.iter().all(|m| m.compacted)
        {
            display_messages.push(Message::CompactMarker {
                summary: compact_summary.clone().unwrap(),
                compacted_count: stored.len(),
                total_ms: None,
            });
        }

        for (_id, (result, display)) in tool_results {
            display_messages.push(Message::ToolResult {
                name: String::new(),
                result,
                display,
            });
        }
        self.messages = display_messages;

        for msg in &active_stored {
            if let Some(chat_msg) = crate::storage::from_stored_message(msg) {
                chat_messages.push(std::sync::Arc::new(chat_msg));
            }
        }

        if !chat_messages.is_empty() {
            let has_system = chat_messages
                .iter()
                .any(|m| crate::storage::compatible_message_role(m) == MessageRole::System);

            if let Some(ref summary) = compact_summary {
                let summary_msg: crate::agent::models::SharedMessage = std::sync::Arc::new(
                    async_openai::types::chat::ChatCompletionRequestUserMessage {
                        content: async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(
                            format!("[Context Summary]\n{}", summary),
                        ),
                        name: None,
                    }
                    .into(),
                );

                let preserved_system = if has_system {
                    None
                } else {
                    self.agent.take_system_prompt()
                };

                let non_system: Vec<_> = chat_messages
                    .into_iter()
                    .filter(|m| crate::storage::compatible_message_role(m) != MessageRole::System)
                    .collect();

                let mut final_messages = Vec::new();
                if let Some(prompt) = &preserved_system {
                    final_messages.push(std::sync::Arc::new(
                        async_openai::types::chat::ChatCompletionRequestSystemMessage {
                            content: async_openai::types::chat::ChatCompletionRequestSystemMessageContent::Text(
                                prompt.clone(),
                            ),
                            name: None,
                        }
                        .into(),
                    ));
                }
                final_messages.push(summary_msg);
                final_messages.extend(non_system);
                self.agent.sync_messages(final_messages);
            } else {
                let preserved_system = if has_system {
                    None
                } else {
                    self.agent.take_system_prompt()
                };
                self.agent.sync_messages(chat_messages);
                if let Some(prompt) = preserved_system {
                    self.agent.set_system_prompt(&prompt);
                }
            }
        }

        self.should_auto_scroll = true;
        self.scroll_offset = 0;
        Ok(())
    }

    pub(super) fn current_work_dir() -> Result<String> {
        Ok(std::env::current_dir()?
            .canonicalize()?
            .display()
            .to_string())
    }

    pub(super) async fn open_session_picker(&mut self) -> Result<()> {
        let work_dir = Self::current_work_dir()?;
        let sessions = self.storage.list_sessions(&work_dir).await?;
        let filtered_indices = (0..sessions.len()).collect();
        self.state = AppState::SessionPicker {
            sessions,
            selected_index: 0,
            search_query: String::new(),
            filtered_indices,
        };
        Ok(())
    }

    pub(super) fn set_session_usage(&mut self, prompt_tokens: u32, completion_tokens: u32) {
        self.context_prompt_tokens = prompt_tokens;
        self.context_completion_tokens = completion_tokens;
    }

    pub(super) async fn switch_to_session(&mut self, session_id: &str) -> Result<()> {
        self.current_session_id = Some(session_id.to_string());
        if let Ok(mut guard) = self.shared.current_session.lock() {
            *guard = Some(session_id.to_string());
        }
        let (pt, ct) = self.storage.get_session_usage(session_id).await?;
        self.set_session_usage(pt as u32, ct as u32);
        self.messages.clear();
        self.tasks.records.clear();
        self.render.dirty = true;
        self.render.force_clear = true;
        self.render.cache.clear();
        self.input.reset();
        self.pending_pastes.clear();
        self.file_picker.reset();
        self.scroll_offset = 0;
        self.should_auto_scroll = true;
        self.agent
            .permission()
            .switch_session(session_id.to_string());
        self.load_session_messages().await?;
        self.state = AppState::Chat;
        Ok(())
    }

    pub(super) async fn create_new_session(&mut self) -> Result<()> {
        self.current_session_id = None;
        if let Ok(mut guard) = self.shared.current_session.lock() {
            *guard = None;
        }
        self.context_prompt_tokens = 0;
        self.context_completion_tokens = 0;
        self.messages.clear();
        self.tasks.records.clear();
        self.render.dirty = true;
        self.render.force_clear = true;
        self.render.cache.clear();
        self.input.reset();
        self.pending_pastes.clear();
        self.file_picker.reset();
        self.scroll_offset = 0;
        self.should_auto_scroll = true;
        self.agent.permission().clear_session();
        let system_prompt = self.agent.take_system_prompt();
        self.agent.clear_messages();
        if let Some(prompt) = system_prompt {
            self.agent.set_system_prompt(&prompt);
        }
        self.state = AppState::Chat;
        Ok(())
    }
}
