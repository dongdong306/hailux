use color_eyre::Result;
use std::time::Instant;

use super::{App, AppState, Message};
use crate::agent::Tool;
use crate::agent::command_def::INIT_COMMAND_NAME;
use crate::agent::event::CoreEvent;
use crate::agent::subagent::{self, TaskTool};
use crate::storage::{MessageRole, StoredMessage, plan_mode_runtime_meta};
use crate::tui::command;
use crate::tui::event::{AppEvent, CompactUsage, TaskStatus};
use crate::tui::tasks_viewer::{TaskRecord, TaskRunStatus};

/// Estimates the post-compaction prompt token count.
/// Formula: old_prompt - compact_prompt + compact_completion
/// (the compact request's prompt includes system-prompt overhead, so this skews low; UI display only)
fn estimate_compact_prompt(old_prompt: u32, usage: CompactUsage) -> u32 {
    old_prompt
        .saturating_sub(usage.prompt_tokens)
        .saturating_add(usage.completion_tokens)
}

impl App {
    pub(super) async fn handle_chat_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::Core(core) => self.handle_core_event(core).await?,
            AppEvent::InputKey(key) => self.handle_chat_key(key).await?,
            AppEvent::InputPaste(text) => {
                // Empty Event::Paste (Unix image clipboard): probe for an image and intercept on a hit;
                // non-empty text (including empty bracketed paste) follows the default flow
                if text.is_empty() {
                    self.spawn_clipboard_image_probe(false);
                    return Ok(());
                }
                self.handle_paste(text);
            }
            AppEvent::PasteImageProbe => {
                // Windows: crossterm's WinAPI input produces no Event::Paste, so WT's empty
                // bracketed paste (image-clipboard Ctrl+V) arrives as character keys and is caught
                // by handle_paste's marker pairing → trigger the clipboard image probe here
                self.spawn_clipboard_image_probe(false);
            }
            AppEvent::PasteImageResult { image, interactive } => {
                self.handle_clipboard_image_result(image, interactive);
            }
            AppEvent::UserSubmit {
                text,
                attachments,
                mentions,
            } => {
                self.submit_text(text, attachments, mentions).await?;
            }
            AppEvent::Resize => {}
            AppEvent::ScrollUp => self.scroll_chat(3),
            AppEvent::ScrollDown => self.scroll_chat(-3),
            AppEvent::McpReady(_) => {}
            // Fetch results belong to the model picker / setup form (handle_event routes
            // them there); ignored here to keep the match exhaustive
            AppEvent::ModelsRefreshed { .. } => {}
            AppEvent::MouseClick => {}
        }
        Ok(())
    }

    /// Domain event handling (agent / subagent output; UI-agnostic CoreEvents land here as TUI state)
    pub(super) async fn handle_core_event(&mut self, event: CoreEvent) -> Result<()> {
        match event {
            CoreEvent::AgentChunk(chunk) => {
                self.finalize_thinking_ms();
                self.drop_trailing_retry_notice();
                match self.last_real_message_mut() {
                    Some(Message::AgentStreaming(text)) => {
                        text.push_str(&chunk);
                    }
                    _ => {
                        self.push_turn_output(Message::AgentStreaming(chunk));
                    }
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            CoreEvent::AgentReasoningChunk(chunk) => {
                self.drop_trailing_retry_notice();
                match self.last_real_message_mut() {
                    Some(Message::AgentThinking { text, think_ms, .. }) if think_ms.is_none() => {
                        text.push_str(&chunk);
                    }
                    _ => {
                        self.push_turn_output(Message::AgentThinking {
                            text: chunk,
                            think_ms: None,
                            thinking_started_at: Some(Instant::now()),
                        });
                    }
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            CoreEvent::AgentRetry {
                attempt,
                max_retries,
                delay_ms,
                reason,
            } => {
                // Partial output from a failed attempt never enters context: trailing streaming renders are dropped in lockstep
                let cut = self.turn_output_insert_index();
                while cut > 0
                    && matches!(
                        self.messages[cut - 1],
                        Message::AgentStreaming(_) | Message::AgentThinking { .. }
                    )
                {
                    self.messages.remove(cut - 1);
                }
                let secs = delay_ms as f64 / 1000.0;
                let notice = format!(
                    "[请求失败，{secs:.1}s 后进行第 {attempt}/{max_retries} 次重试：{reason}]"
                );
                // Replace a trailing retry notice in place; only the latest
                // attempt stays visible instead of one line per attempt
                upsert_retry_notice(&mut self.messages, notice);
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            CoreEvent::AgentComplete {
                messages: final_messages,
                usages,
                model: turn_model,
                reasoning_effort,
                status,
            } => {
                self.finalize_thinking_ms();
                self.timing.finish(Instant::now());
                // Write elapsed time and model name into runtime_meta (plus status and reasoning effort, for later analysis);
                // model / reasoning_effort are streaming snapshot values, unaffected by mid-turn switches
                if let Some(session_id) = &self.current_session_id
                    && let Some(total_ms) = self.timing.last_total_ms
                {
                    let status_str = match status {
                        TaskStatus::Completed => "completed",
                        TaskStatus::Interrupted => "interrupted",
                        TaskStatus::Error => "error",
                    };
                    let meta = serde_json::json!({
                        "total_ms": total_ms,
                        "model": turn_model,
                        "status": status_str,
                        "reasoning_effort": reasoning_effort.as_str(),
                    })
                    .to_string();
                    if let Err(e) = self
                        .storage
                        .update_last_assistant_runtime_meta(session_id, &meta)
                        .await
                    {
                        eprintln!("[warn] failed to persist runtime_meta: {e}");
                    }
                }

                let last_idx = self.turn_output_insert_index().checked_sub(1);
                let is_streaming = last_idx
                    .and_then(|i| self.messages.get(i))
                    .is_some_and(|m| matches!(m, Message::AgentStreaming(_)));
                if is_streaming {
                    let i = last_idx.unwrap();
                    if let Message::AgentStreaming(text) = &self.messages[i] {
                        let text = text.clone();
                        self.messages[i] = Message::Agent(text);
                    }
                }
                if let Some(total_ms) = self.timing.last_total_ms {
                    self.push_turn_output(Message::AgentDone {
                        total_ms,
                        model: turn_model.clone(),
                        status,
                    });
                }
                if !final_messages.is_empty() {
                    if let Some(session_id) = &self.current_session_id
                        && let Some(last_usage) = usages.last()
                    {
                        self.storage
                            .update_session_usage(
                                session_id,
                                last_usage.prompt_tokens as i64,
                                last_usage.completion_tokens as i64,
                            )
                            .await?;
                    }
                    self.agent.sync_messages(final_messages);
                }
                self.is_processing = false;
                self.input.set_processing(false);
                self.last_esc_time = None;
                self.esc_hint_active = false;
                self.should_auto_scroll = true;
                self.scroll_offset = 0;
                // Safety net: an interrupt before the injection boundary
                // leaves optimistic bubbles behind and in-flight queue items
                // stuck — strip the markers so later output appends normally,
                // and release the items (they stayed queued; the Completed
                // drain below resends them)
                let in_flight_ids: Vec<String> = self
                    .messages
                    .iter()
                    .filter_map(|m| match m {
                        Message::User {
                            pending_interject: Some(id),
                            ..
                        } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                for m in &mut self.messages {
                    if let Message::User {
                        pending_interject, ..
                    } = m
                    {
                        *pending_interject = None;
                    }
                }
                for id in in_flight_ids {
                    if let Some(item) = self.queue.iter_mut().find(|q| q.client_id == id) {
                        item.in_flight = false;
                    }
                }
                if let Some(session_id) = &self.current_session_id {
                    self.storage.touch_session(session_id).await?;
                }

                let total_context = self.context_prompt_tokens + self.context_completion_tokens;
                let threshold =
                    (self.resolved.context_window as f32 * self.config.compact_threshold) as u32;
                if total_context > threshold {
                    self.drain_after_compact = true;
                    let pct = (self.config.compact_threshold * 100.0) as u32;
                    self.compact_conversation(Some(&format!(
                        "上下文 token 已达 {}%，自动压缩",
                        pct
                    )))
                    .await?;
                } else if status == TaskStatus::Completed {
                    self.drain_queue().await?;
                }
            }
            CoreEvent::UsageUpdate {
                prompt_tokens,
                completion_tokens,
                ..
            } => {
                self.set_session_usage(prompt_tokens, completion_tokens);
            }
            CoreEvent::PersistMessage {
                msg,
                usage,
                model,
                display,
            } => {
                if let Some(session_id) = &self.current_session_id {
                    let mut stored = crate::storage::to_stored_message(&msg);
                    if let Some(u) = usage {
                        stored.prompt_tokens = Some(u.prompt_tokens as i64);
                        stored.completion_tokens = Some(u.completion_tokens as i64);
                        stored.cached_tokens = Some(u.cached_tokens as i64);
                    }
                    // Usage stats are attributed per message to the model carried by the event (streaming snapshot value)
                    stored.model = Some(model);
                    if let Some(ref d) = display {
                        stored.runtime_meta = Some(d.clone());
                    }
                    if stored.role == MessageRole::Assistant && stored.reasoning_content.is_some() {
                        let boundary_idx = self
                            .messages
                            .iter()
                            .rposition(|m| match m {
                                Message::User {
                                    pending_interject: Some(_),
                                    ..
                                } => false,
                                Message::User { .. }
                                | Message::ToolCall { .. }
                                | Message::ToolResult { .. }
                                | Message::Agent(_)
                                | Message::Retry(_)
                                | Message::AgentDone { .. } => true,
                                _ => false,
                            })
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        let think_ms: u64 = self.messages[boundary_idx..]
                            .iter()
                            .filter_map(|m| match m {
                                Message::AgentThinking { think_ms, .. } => *think_ms,
                                _ => None,
                            })
                            .sum();
                        if think_ms > 0 {
                            stored.think_ms = Some(think_ms as i64);
                        }
                    }
                    self.storage.append_message(session_id, &stored).await?;
                }
            }
            CoreEvent::PersistUserReminder {
                session_id,
                reminder,
            } => {
                // The in-memory user message already carries the reminder;
                // land it in the row's runtime_meta so a rebuild reproduces
                // the exact request bytes (prompt-cache stable). Use the
                // session anchored at injection time, not the ambient
                // current session (a mid-queue switch would misfile the row).
                if let Err(e) = self
                    .storage
                    .append_user_message_reminder(&session_id, &reminder)
                    .await
                {
                    eprintln!("[warn] failed to persist user reminder: {e}");
                }
            }
            CoreEvent::ToolCallStart {
                name,
                arguments,
                subagent_name,
                subagent_index,
            } => {
                self.finalize_thinking_ms();
                self.drop_trailing_retry_notice();
                if subagent_name.is_some() {
                    self.push_turn_output(Message::SubagentStep {
                        summary: crate::tui::history_cell::subagent_step_summary(&name, &arguments),
                        is_done: false,
                        agent: subagent_name.clone(),
                        index: subagent_index,
                    });
                } else {
                    if name == "task"
                        && let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&arguments)
                    {
                        let items: Vec<(String, String)> = args_val["tasks"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .map(|t| {
                                        (
                                            t["subagent"].as_str().unwrap_or("").to_string(),
                                            t["description"].as_str().unwrap_or("").to_string(),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        for (sub_name, desc) in items {
                            if sub_name.is_empty() {
                                continue;
                            }
                            self.tasks.call_counter += 1;
                            let call_id = self.tasks.call_counter;
                            self.tasks.active_call_ids.push(call_id);
                            self.tasks.records.push(TaskRecord {
                                call_id,
                                session_id: String::new(),
                                subagent_name: sub_name.clone(),
                                description: desc,
                                started_at: std::time::Instant::now(),
                                status: TaskRunStatus::Running,
                            });
                            if self.tasks.records.len() > 500 {
                                self.tasks.records.remove(0);
                            }
                        }
                    }
                    self.push_turn_output(Message::ToolCall { name, arguments });
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            CoreEvent::ToolResult {
                name,
                result,
                display,
                subagent_name,
                subagent_index,
            } => {
                // GUI programs in the shell subprocess tree may bypass pipes and write straight to the
                // console screen (see the CREATE_NO_WINDOW comment in agent/tools/shell.rs); once the run
                // finishes, force a full repaint to clear any leftover dirty screen regions
                if name == "shell" {
                    self.render.force_clear = true;
                }
                if subagent_name.is_some() {
                    let tool_name = name.as_str();
                    let result_summary =
                        crate::tui::history_cell::tool_result_summary(tool_name, &result);
                    // Steps interleave within a concurrent batch: match by task index first (concurrent same-name
                    // subagents are indistinguishable by name); fall back to source-agent matching for index-less historical data
                    let target_idx = self
                        .messages
                        .iter()
                        .rev()
                        .position(
                            |m| matches!(m, Message::SubagentStep { is_done: false, index: Some(i), .. } if Some(*i) == subagent_index),
                        )
                        .or_else(|| {
                            self.messages.iter().rev().position(
                                |m| matches!(m, Message::SubagentStep { is_done: false, agent, .. } if agent == &subagent_name),
                            )
                        })
                        .or_else(|| {
                            // Single-task batch (agent=None) or historical data: take the last unfinished step
                            self.messages.iter().rev().position(
                                |m| matches!(m, Message::SubagentStep { is_done: false, agent: None, .. }),
                            )
                        })
                        .map(|pos| self.messages.len() - 1 - pos);
                    if let Some(idx) = target_idx
                        && let Some(Message::SubagentStep {
                            is_done, summary, ..
                        }) = self.messages.get_mut(idx)
                    {
                        *is_done = true;
                        if !result_summary.is_empty() {
                            summary.push_str(&format!(" — {}", result_summary));
                        }
                    }
                } else {
                    if name == "task" {
                        let blocks = parse_task_outcomes(&result);
                        let call_ids = std::mem::take(&mut self.tasks.active_call_ids);
                        // join_all preserves order: result block order = tasks array order = record
                        // creation order, paired positionally; concurrent same-name subagents
                        // (even with mixed success/failure) can't be mispaired
                        for (i, call_id) in call_ids.iter().enumerate() {
                            let Some(record) = self
                                .tasks
                                .records
                                .iter_mut()
                                .find(|r| r.call_id == *call_id)
                            else {
                                continue;
                            };
                            match blocks.get(i) {
                                Some(b) if b.ok => {
                                    record.session_id = b.task_id.clone().unwrap_or_default();
                                    record.status = TaskRunStatus::Completed;
                                }
                                _ => record.status = TaskRunStatus::Error,
                            }
                        }
                    }
                    self.push_turn_output(Message::ToolResult {
                        name,
                        result,
                        display,
                    });
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            CoreEvent::AskUser {
                questions,
                response_tx,
            } => {
                let count = questions.len();
                self.state = AppState::AskUser {
                    questions,
                    response_tx,
                    current_tab: 0,
                    selected: 0,
                    answers: vec![None; count],
                    custom_inputs: vec![String::new(); count],
                    custom_cursor: 0,
                    editing_custom: false,
                    last_paste: None,
                };
                self.timing.pause();
            }
            CoreEvent::CompactChunk(chunk) => {
                if let Some(Message::CompactStreaming(text)) = self.last_real_message_mut() {
                    text.push_str(&chunk);
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            CoreEvent::CompactComplete {
                summary,
                session_id,
                usage,
            } => {
                let compact_session = self.current_session_id.as_deref() == Some(&session_id);

                let compacted_count =
                    self.storage.count_active_messages(&session_id).await? as usize;

                self.storage.mark_messages_compacted(&session_id).await?;
                self.storage
                    .set_compact_summary(&session_id, &summary)
                    .await?;

                if compact_session {
                    self.agent.apply_compaction(&summary);

                    self.timing.finish(Instant::now());
                    let total_ms = self.timing.last_total_ms;
                    self.last_esc_time = None;
                    self.esc_hint_active = false;

                    if let Some(usage) = usage {
                        let estimated_prompt =
                            estimate_compact_prompt(self.context_prompt_tokens, usage);
                        self.set_session_usage(estimated_prompt, 0);
                        if let Some(sid) = &self.current_session_id {
                            self.storage
                                .update_session_usage(sid, estimated_prompt as i64, 0)
                                .await?;
                        }
                    }

                    self.replace_trailing_compact_streaming(Message::CompactMarker {
                        compacted_count,
                        total_ms,
                    });

                    self.should_auto_scroll = true;
                    self.scroll_offset = 0;
                    self.render.dirty = true;
                }

                self.is_processing = false;
                self.input.set_processing(false);
                if self.drain_after_compact {
                    self.drain_after_compact = false;
                    self.drain_queue().await?;
                }
            }
            CoreEvent::CompactError(msg) => {
                self.replace_trailing_compact_streaming(Message::Agent(format!(
                    "[压缩失败: {}]",
                    msg
                )));
                self.timing.abort();
                self.last_esc_time = None;
                self.esc_hint_active = false;
                self.is_processing = false;
                self.input.set_processing(false);
                self.render.dirty = true;
            }
            // Permission requests are enqueued at the top of App::handle_event and never reach here
            CoreEvent::PermissionRequest { .. } => {}
            CoreEvent::UserMessage { interjection } => {
                self.handle_interjected_user_message(interjection).await?;
            }
            CoreEvent::InterjectDropped { client_id } => {
                self.handle_interject_dropped(&client_id);
            }
        }
        Ok(())
    }

    pub(super) async fn handle_command(&mut self, matched: command::MatchedCommand) -> Result<()> {
        match matched {
            command::MatchedCommand::Ui { cmd, args } => {
                self.input.clear();
                // Clear pending state along with the input box
                self.pending_images.clear();
                self.pending_pastes.clear();
                self.file_picker.pending_mentions.clear();
                self.cmd_suggestion.show = false;
                self.cmd_suggestion.items.clear();
                match cmd {
                    command::Command::Session => {
                        self.open_session_picker().await?;
                    }
                    command::Command::New => {
                        self.create_new_session().await?;
                    }
                    command::Command::Models => {
                        self.open_model_picker();
                    }
                    command::Command::Skills => {
                        self.open_skills_viewer();
                    }
                    command::Command::Mcp => {
                        self.open_mcp_viewer();
                    }
                    command::Command::Tasks => {
                        self.open_tasks_viewer().await?;
                    }
                    command::Command::Stats => {
                        self.open_stats_viewer().await?;
                    }
                    command::Command::Share => {
                        self.share_session_html().await?;
                    }
                    command::Command::Plan => {
                        self.toggle_plan_mode();
                    }
                    command::Command::Yolo => {
                        let mode = self.agent.permission().toggle_yolo();
                        let yolo_on = mode == crate::permission::PermissionMode::Yolo;
                        let label = if yolo_on { "YOLO" } else { "ASK" };
                        self.messages
                            .push(Message::Agent(format!("Permission mode: {label}")));
                        // Persist to config.toml so the mode survives restarts / new sessions
                        // (empty string = Normal; save() omits the field to keep the file minimal)
                        self.config.permission.mode = if yolo_on {
                            "yolo".to_string()
                        } else {
                            String::new()
                        };
                        if let Err(e) = self.config.save() {
                            self.messages
                                .push(Message::Agent(format!("保存 YOLO 配置失败: {e}")));
                        }
                        if let Ok(mut shared) = self.shared.config.lock() {
                            *shared = self.config.clone();
                        }
                    }
                    command::Command::Compact => {
                        self.compact_conversation(None).await?;
                    }
                    command::Command::Effort => {
                        self.handle_effort_command(&args);
                    }
                    command::Command::Exit => {
                        self.should_quit = true;
                    }
                }
            }
            command::MatchedCommand::Prompt { name, args } => {
                if let Some(cmd) = self.command_registry.find(&name) {
                    // /init needs to write AGENTS.md, but plan mode filters out write/edit tools, so
                    // running it directly fails silently (the LLM can only emit text, not write files).
                    if name == INIT_COMMAND_NAME && self.plan_mode {
                        self.input.clear();
                        self.cmd_suggestion.show = false;
                        self.cmd_suggestion.items.clear();
                        self.messages.push(Message::Agent(
                            "当前为只读规划模式，无法创建 AGENTS.md。请先 /plan 退出规划模式，再运行 /init。".to_string(),
                        ));
                        self.render.dirty = true;
                        return Ok(());
                    }
                    let rendered = cmd.render(&args);
                    self.input.clear();
                    self.cmd_suggestion.show = false;
                    self.cmd_suggestion.items.clear();
                    self.handle_user_message(rendered, Vec::new(), Vec::new())
                        .await?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn toggle_plan_mode(&mut self) {
        self.plan_mode = !self.plan_mode;
        self.agent.set_plan_mode(self.plan_mode);
    }

    /// /effort command: no argument opens the submenu; `low|high|max` sets it directly (case-insensitive).
    /// Scope mirrors YOLO: applies immediately to the current Agent + persisted to config.toml + synced
    /// to the shared config (new subagents follow), taking effect on the next LLM request.
    pub(super) fn handle_effort_command(&mut self, args: &str) {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            self.open_effort_picker();
            return;
        }

        match crate::agent::models::ReasoningEffort::parse(trimmed) {
            Some(effort) => self.apply_effort(effort),
            None => {
                self.messages.push(Message::Agent(format!(
                    "无效的思考强度: {trimmed}，可选值 low / high / max"
                )));
                self.render.dirty = true;
            }
        }
    }

    pub(super) fn open_effort_picker(&mut self) {
        let selected_index = match self.agent.reasoning_effort() {
            crate::agent::models::ReasoningEffort::Low => 0,
            crate::agent::models::ReasoningEffort::High => 1,
            crate::agent::models::ReasoningEffort::Max => 2,
        };
        self.state = AppState::EffortPicker { selected_index };
    }

    pub(super) fn apply_effort(&mut self, effort: crate::agent::models::ReasoningEffort) {
        self.agent.set_reasoning_effort(effort);
        self.config.reasoning_effort = effort;
        if let Err(e) = self.config.save() {
            self.messages
                .push(Message::Agent(format!("保存思考强度配置失败: {e}")));
        }
        if let Ok(mut shared) = self.shared.config.lock() {
            shared.reasoning_effort = effort;
        }
        self.messages.push(Message::Agent(format!(
            "Thinking effort: {}",
            effort.as_str().to_uppercase()
        )));
        self.render.dirty = true;
    }

    pub(super) fn finalize_thinking_ms(&mut self) {
        if let Some(Message::AgentThinking {
            think_ms,
            thinking_started_at,
            ..
        }) = self.last_real_message_mut()
            && think_ms.is_none()
            && let Some(t) = thinking_started_at.take()
        {
            *think_ms = Some(t.elapsed().as_millis() as u64);
        }
    }

    fn replace_trailing_compact_streaming(&mut self, msg: Message) {
        let i = self.turn_output_insert_index().checked_sub(1);
        if let Some(i) = i
            && matches!(&self.messages[i], Message::CompactStreaming(_))
        {
            self.messages[i] = msg;
        }
    }

    pub(super) async fn handle_user_message(
        &mut self,
        input: String,
        attachments: Vec<crate::agent::media::Attachment>,
        mention_display: Vec<(String, String)>,
    ) -> Result<()> {
        self.is_processing = true;
        self.input.set_processing(true);
        self.timing.start_request();
        self.input.submit(&input);
        self.pending_pastes.clear();
        self.pending_images.clear();
        self.file_picker.reset();
        self.should_auto_scroll = true;
        self.scroll_offset = 0;
        let (display_text, mention_spans) =
            super::types::rewrite_mentions(&input, &mention_display);
        self.messages.push(Message::User {
            text: display_text,
            plan_mode: self.plan_mode,
            mentions: mention_spans,
            pending_interject: None,
        });

        let new_session = self.current_session_id.is_none();
        if new_session {
            let work_dir = Self::current_work_dir()?;
            let session_id = self
                .storage
                .create_session(&self.resolved.display, &work_dir)
                .await?;
            self.current_session_id = Some(session_id.clone());
            if let Ok(mut guard) = self.shared.current_session.lock() {
                *guard = Some(session_id);
            }
        }

        let session_id = self.current_session_id.as_deref().unwrap().to_string();
        let attachments_json = if attachments.is_empty() {
            None
        } else {
            serde_json::to_string(&attachments).ok()
        };
        let mentions_json = mentions_json_for(&input, &mention_display);
        let stored = StoredMessage {
            role: MessageRole::User,
            content: input.clone(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            prompt_tokens: None,
            completion_tokens: None,
            cached_tokens: None,
            model: None,
            runtime_meta: plan_mode_runtime_meta(self.plan_mode),
            think_ms: None,
            compacted: false,
            attachments: attachments_json,
            mentions: mentions_json,
        };
        self.storage.append_message(&session_id, &stored).await?;

        if new_session {
            let title: String = if input.chars().count() > 50 {
                format!("{}...", input.chars().take(50).collect::<String>())
            } else {
                input.chars().take(50).collect()
            };
            self.storage
                .update_session_title(&session_id, &title)
                .await?;
        }

        if let Some((subagent_name, subagent_prompt)) = subagent::parse_subagent_input(&input)
            && self.subagents.iter().any(|s| s.name == subagent_name)
        {
            let task_tool = TaskTool::new(
                self.subagents.clone(),
                self.skills.clone(),
                self.storage.clone(),
                self.resolved.display.clone(),
                self.work_dir.clone(),
                self.shared.current_session.clone(),
                self.shared.mcp_backends.clone(),
                self.shared.config.clone(),
                self.agent.event_hub().clone(),
                self.agent.permission().clone(),
            );

            let task_description = format!(
                "@{}: {}",
                subagent_name,
                subagent_prompt.chars().take(60).collect::<String>()
            );

            let args = serde_json::json!({
                "tasks": [{
                    "subagent": subagent_name,
                    "description": task_description.clone(),
                    "prompt": subagent_prompt,
                }]
            })
            .to_string();

            self.push_turn_output(Message::ToolCall {
                name: "task".to_string(),
                arguments: args.clone(),
            });
            if self.should_auto_scroll {
                self.scroll_offset = 0;
            }

            self.tasks.call_counter += 1;
            let call_id = self.tasks.call_counter;
            self.tasks.active_call_ids = vec![call_id];
            self.tasks.records.push(TaskRecord {
                call_id,
                session_id: String::new(),
                subagent_name: subagent_name.clone(),
                description: task_description.clone(),
                started_at: std::time::Instant::now(),
                status: TaskRunStatus::Running,
            });

            let result = task_tool.execute_async(&args, &self.work_dir).await;
            let task_status = if result.is_ok() {
                TaskRunStatus::Completed
            } else {
                TaskRunStatus::Error
            };
            let result_text = result.unwrap_or_else(|err| err.message);

            let sub_session_id = parse_task_id_from_result(&result_text);
            self.tasks.active_call_ids.clear();
            if let Some(record) = self.tasks.records.iter_mut().find(|r| r.call_id == call_id) {
                record.session_id = sub_session_id.unwrap_or_default();
                record.status = task_status;
            }

            self.push_turn_output(Message::ToolResult {
                name: "task".to_string(),
                result: result_text.clone(),
                display: None,
            });

            let assistant_stored = StoredMessage {
                role: MessageRole::Assistant,
                content: result_text,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
                model: None,
                runtime_meta: None,
                think_ms: None,
                compacted: false,
                attachments: None,
                mentions: None,
            };
            self.storage
                .append_message(&session_id, &assistant_stored)
                .await?;

            self.is_processing = false;
            self.input.set_processing(false);
            self.should_auto_scroll = true;
            self.scroll_offset = 0;
            return Ok(());
        }

        let tx = self.core_tx.clone();
        if let Err(e) = self.agent.chat_stream(&session_id, &input, attachments, tx) {
            self.push_turn_output(Message::Agent(format!("[错误: {}]", e)));
            self.is_processing = false;
            self.input.set_processing(false);
        }
        Ok(())
    }

    pub(super) async fn compact_conversation(&mut self, auto_reason: Option<&str>) -> Result<()> {
        if self.is_processing {
            return Ok(());
        }

        let non_system_count = self.agent.messages_excluding_system_count();
        // Compaction is pointless with fewer than 4 non-system messages (under 2 conversation rounds)
        if non_system_count < 4 {
            self.messages
                .push(Message::Agent("消息太少，无需压缩".to_string()));
            self.render.dirty = true;
            return Ok(());
        }

        let session_id = match &self.current_session_id {
            Some(id) => id.clone(),
            None => {
                self.messages
                    .push(Message::Agent("无活跃会话，无法压缩".to_string()));
                self.render.dirty = true;
                return Ok(());
            }
        };

        self.is_processing = true;
        self.input.set_processing(true);
        self.should_auto_scroll = true;
        self.scroll_offset = 0;
        self.timing.start_request();

        let label = if let Some(reason) = auto_reason {
            format!("正在压缩上下文（{}）...\n", reason)
        } else {
            "正在压缩上下文...\n".to_string()
        };
        self.push_turn_output(Message::CompactStreaming(label));
        self.render.dirty = true;

        let tx = self.core_tx.clone();
        if let Err(e) = self.agent.request_compaction(tx, session_id) {
            self.replace_trailing_compact_streaming(Message::Agent(format!("[压缩失败: {}]", e)));
            self.timing.abort();
            self.is_processing = false;
            self.input.set_processing(false);
        }
        Ok(())
    }

    /// Optimistic interjection bubbles at the tail are transparent: new
    /// output goes before them so the timeline matches a rebuilt view
    pub(super) fn turn_output_insert_index(&self) -> usize {
        turn_output_insert_index(&self.messages)
    }

    pub(super) fn push_turn_output(&mut self, msg: Message) {
        push_turn_output(&mut self.messages, msg);
    }

    /// A retry notice is transient: the next attempt's first streamed output
    /// (content / reasoning / tool call) removes it. Exhausted retries emit
    /// their final failure as an `AgentChunk("\n[Error: ...]")`, which drops
    /// the trailing notice the same way — the error line replaces the notice
    /// as the error trail.
    pub(super) fn drop_trailing_retry_notice(&mut self) {
        drop_trailing_retry_notice(&mut self.messages);
    }

    pub(super) fn last_real_message_mut(&mut self) -> Option<&mut Message> {
        let idx = self.turn_output_insert_index();
        if idx == 0 {
            None
        } else {
            self.messages.get_mut(idx - 1)
        }
    }

    pub(super) async fn submit_text(
        &mut self,
        text: String,
        attachments: Vec<crate::agent::media::Attachment>,
        mention_display: Vec<(String, String)>,
    ) -> Result<()> {
        if let Some(matched) = command::match_command(&text, &self.command_registry) {
            self.handle_command(matched).await
        } else {
            self.handle_user_message(text, attachments, mention_display)
                .await
        }
    }

    pub(super) async fn drain_queue(&mut self) -> Result<()> {
        if self.queue.is_empty() {
            return Ok(());
        }
        let item = self.queue.remove(0);
        self.clamp_queue_selection();
        self.render.dirty = true;
        let attachments = item.all_attachments();
        self.submit_text_preserving_draft(item.text, attachments, item.mention_display)
            .await
    }

    /// After any queue removal, keep the QueueManager selection in bounds
    /// (the manager stays open while Core events drain the queue)
    fn clamp_queue_selection(&mut self) {
        if let AppState::QueueManager { selected_index } = &mut self.state {
            if self.queue.is_empty() {
                self.state = AppState::Chat;
            } else {
                *selected_index = (*selected_index).min(self.queue.len() - 1);
            }
        }
    }

    /// Programmatic submits must not wipe a draft the user is typing:
    /// `input.submit()` and UI-command handling clear the box, so snapshot
    /// and restore around the actual submit.
    async fn submit_text_preserving_draft(
        &mut self,
        text: String,
        attachments: Vec<crate::agent::media::Attachment>,
        mention_display: Vec<(String, String)>,
    ) -> Result<()> {
        let draft = self.input.text().to_string();
        self.submit_text(text, attachments, mention_display).await?;
        if !draft.is_empty() && self.input.text().is_empty() {
            self.input.set_text(draft);
            self.refresh_suggestions();
            self.render.dirty = true;
        }
        Ok(())
    }

    pub(super) async fn interject_one(&mut self, client_id: &str) -> Result<()> {
        let Some(pos) = self.queue.iter().position(|q| q.client_id == client_id) else {
            return Ok(());
        };
        if self.queue[pos].in_flight {
            // Delivery already in progress (waiting for echo / drop)
            return Ok(());
        }
        if self.is_processing {
            let item = &self.queue[pos];
            let rendered = self.command_registry.render_prompt_command(&item.text);
            // Mention offsets are computed against the raw text; a rendered
            // command template shifts them — drop mentions there instead of
            // emitting wrong ranges
            let mentions = if rendered.is_some() {
                Vec::new()
            } else {
                mentions_for(&item.text, &item.mention_display)
            };
            let ok = self.agent.interject(crate::agent::event::Interjection {
                client_id: client_id.to_string(),
                content: rendered.unwrap_or_else(|| item.text.clone()),
                attachments: item.all_attachments(),
                mentions,
            });
            if ok {
                // The item stays queued (in_flight) until the UserMessage echo
                // removes it, so an InterjectDropped finds it intact
                self.queue[pos].in_flight = true;
                let item = &self.queue[pos];
                let (display_text, mention_spans) =
                    super::types::rewrite_mentions(&item.text, &item.mention_display);
                self.push_turn_output(Message::User {
                    text: display_text,
                    plan_mode: self.plan_mode,
                    mentions: mention_spans,
                    pending_interject: Some(client_id.to_string()),
                });
                self.should_auto_scroll = true;
                self.scroll_offset = 0;
                self.render.dirty = true;
                return Ok(());
            }
            // Run just ended / cancelling / bounded queue full: keep the item
            // queued for the AgentComplete auto-drain — never fall through to
            // submit_text here (it would race the finishing run into a
            // double-run with context pollution)
            return Ok(());
        }
        let item = self.queue.remove(pos);
        self.clamp_queue_selection();
        let attachments = item.all_attachments();
        self.submit_text_preserving_draft(item.text, attachments, item.mention_display)
            .await
    }

    /// An accepted interjection never entered the context (run interrupted /
    /// errored / teardown race): release the in-flight item (stays queued for
    /// a manual resend) and remove the optimistic bubble.
    fn handle_interject_dropped(&mut self, client_id: &str) {
        if let Some(item) = self.queue.iter_mut().find(|q| q.client_id == client_id) {
            item.in_flight = false;
        }
        self.messages.retain(|m| {
            !matches!(
                m,
                Message::User {
                    pending_interject: Some(id),
                    ..
                } if id == client_id
            )
        });
        // Consecutive drops (a burst on interrupt) collapse into one notice
        let is_dup = matches!(
            self.messages.last(),
            Some(Message::Agent(t)) if t == INTERJECT_DROPPED_NOTICE
        );
        if !is_dup {
            self.messages
                .push(Message::Agent(INTERJECT_DROPPED_NOTICE.to_string()));
        }
        self.clamp_queue_selection();
        self.should_auto_scroll = true;
        self.scroll_offset = 0;
        self.render.dirty = true;
    }

    async fn handle_interjected_user_message(
        &mut self,
        interjection: crate::agent::event::Interjection,
    ) -> Result<()> {
        let crate::agent::event::Interjection {
            client_id,
            content,
            attachments,
            mentions,
        } = interjection;
        // Echo = actually injected: remove the in-flight queue item now
        if let Some(pos) = self.queue.iter().position(|q| q.client_id == client_id) {
            self.queue.remove(pos);
            self.clamp_queue_selection();
            self.render.dirty = true;
        }
        let optimistic_idx = self.messages.iter().rposition(|m| {
            matches!(
                m,
                Message::User {
                    pending_interject: Some(id),
                    ..
                } if id == &client_id
            )
        });
        if let Some(i) = optimistic_idx {
            if let Message::User {
                pending_interject, ..
            } = &mut self.messages[i]
            {
                *pending_interject = None;
            }
        } else {
            let mention_spans = mentions
                .iter()
                .filter_map(|m| {
                    crate::tui::app::types::char_range_to_byte_range(&content, m.start, m.end)
                })
                .collect::<Vec<_>>();
            self.push_turn_output(Message::User {
                text: content.clone(),
                plan_mode: self.plan_mode,
                mentions: mention_spans,
                pending_interject: None,
            });
        }
        self.should_auto_scroll = true;
        self.scroll_offset = 0;
        self.render.dirty = true;

        if let Some(session_id) = &self.current_session_id {
            let attachments_json = if attachments.is_empty() {
                None
            } else {
                serde_json::to_string(&attachments).ok()
            };
            let mentions_json = if mentions.is_empty() {
                None
            } else {
                serde_json::to_string(&mentions).ok()
            };
            let stored = StoredMessage {
                role: MessageRole::User,
                content,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
                model: None,
                runtime_meta: plan_mode_runtime_meta(self.plan_mode),
                think_ms: None,
                compacted: false,
                attachments: attachments_json,
                mentions: mentions_json,
            };
            self.storage.append_message(session_id, &stored).await?;
        }

        Ok(())
    }
}

/// Shown when an accepted interjection was discarded without entering the
/// context (drop event burst collapses into one notice)
const INTERJECT_DROPPED_NOTICE: &str = "插话未送达（任务已中断或结束），消息已放回排队列表";

/// Mention metadata for a message body, as core-level `Mention` values
/// (offsets in Unicode code points against `text`)
pub(super) fn mentions_for(
    text: &str,
    mention_display: &[(String, String)],
) -> Vec<crate::agent::event::Mention> {
    if mention_display.is_empty() {
        return Vec::new();
    }
    let values: Vec<String> = mention_display.iter().map(|(abs, _)| abs.clone()).collect();
    crate::tui::app::types::locate_mentions(text, &values)
        .iter()
        .map(|r| crate::tui::app::types::byte_range_to_char_range(text, r))
        .map(|r| crate::agent::event::Mention {
            start: r.start,
            end: r.end,
            kind: "file".to_string(),
        })
        .collect()
}

pub(super) fn mentions_json_for(
    text: &str,
    mention_display: &[(String, String)],
) -> Option<String> {
    let list = mentions_for(text, mention_display);
    if list.is_empty() {
        None
    } else {
        serde_json::to_string(&list).ok()
    }
}

fn parse_task_id_from_result(result: &str) -> Option<String> {
    parse_task_outcomes(result)
        .into_iter()
        .find(|b| b.ok)
        .and_then(|b| b.task_id)
}

struct TaskOutcome {
    task_id: Option<String>,
    ok: bool,
}

fn parse_task_outcomes(result: &str) -> Vec<TaskOutcome> {
    let mut blocks = Vec::new();
    for line in result.lines() {
        let Some(rest) = line.trim().strip_prefix("<task ") else {
            continue;
        };
        let Some(tag) = rest.strip_suffix('>') else {
            continue;
        };
        blocks.push(TaskOutcome {
            task_id: extract_task_attr(tag, "task_id").filter(|id| !id.is_empty()),
            ok: extract_task_attr(tag, "state")
                .map(|s| s != "failed")
                .unwrap_or(true),
        });
    }
    blocks
}

fn extract_task_attr(tag: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=\"");
    let start = tag.find(&marker)? + marker.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Optimistic interjection bubbles at the tail are transparent: new output
/// goes before them so the timeline matches a rebuilt view
fn turn_output_insert_index(messages: &[Message]) -> usize {
    let mut idx = messages.len();
    while idx > 0
        && matches!(
            &messages[idx - 1],
            Message::User {
                pending_interject: Some(_),
                ..
            }
        )
    {
        idx -= 1;
    }
    idx
}

fn push_turn_output(messages: &mut Vec<Message>, msg: Message) {
    let at = turn_output_insert_index(messages);
    if at == messages.len() {
        messages.push(msg);
    } else {
        messages.insert(at, msg);
    }
}

/// Returns whether a trailing retry notice was removed.
fn drop_trailing_retry_notice(messages: &mut Vec<Message>) -> bool {
    let idx = turn_output_insert_index(messages);
    if idx > 0 && matches!(&messages[idx - 1], Message::Retry(_)) {
        messages.remove(idx - 1);
        true
    } else {
        false
    }
}

/// Replace a trailing retry notice in place (one line per retry sequence,
/// not one line per attempt); push as a fresh notice otherwise.
fn upsert_retry_notice(messages: &mut Vec<Message>, notice: String) {
    let at = turn_output_insert_index(messages);
    if at > 0 && matches!(&messages[at - 1], Message::Retry(_)) {
        messages[at - 1] = Message::Retry(notice);
    } else {
        push_turn_output(messages, Message::Retry(notice));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_notice_upsert_replaces_trailing_notice_in_place() {
        let mut messages: Vec<Message> = vec![Message::Agent("answer".into())];
        upsert_retry_notice(&mut messages, "[retry 1/3]".into());
        upsert_retry_notice(&mut messages, "[retry 2/3]".into());
        assert_eq!(messages.len(), 2, "one line per retry sequence");
        assert!(matches!(&messages[1], Message::Retry(t) if t == "[retry 2/3]"));
    }

    #[test]
    fn retry_notice_upsert_pushes_when_notice_not_trailing() {
        let mut messages: Vec<Message> = vec![
            Message::Retry("[retry 1/3]".into()),
            Message::AgentStreaming("partial".into()),
        ];
        upsert_retry_notice(&mut messages, "[retry 2/3]".into());
        assert_eq!(messages.len(), 3);
        assert!(matches!(&messages[2], Message::Retry(_)));
    }

    #[test]
    fn retry_notice_dropped_by_first_output_of_next_attempt() {
        let mut messages: Vec<Message> = vec![
            Message::Agent("answer".into()),
            Message::Retry("[retry 1/3]".into()),
        ];
        assert!(drop_trailing_retry_notice(&mut messages));
        assert_eq!(messages.len(), 1);
        push_turn_output(&mut messages, Message::AgentStreaming("new".into()));
        assert!(matches!(&messages[1], Message::AgentStreaming(_)));
    }

    #[test]
    fn retry_notice_drop_noop_when_not_trailing() {
        let mut messages: Vec<Message> = vec![
            Message::Agent("answer".into()),
            Message::Retry("[retry 1/3]".into()),
            Message::AgentStreaming("partial".into()),
        ];
        assert!(!drop_trailing_retry_notice(&mut messages));
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn retry_notice_drop_noop_on_empty_list() {
        let mut messages: Vec<Message> = Vec::new();
        assert!(!drop_trailing_retry_notice(&mut messages));
    }

    #[test]
    fn estimate_basic() {
        let usage = CompactUsage {
            prompt_tokens: 5000,
            completion_tokens: 800,
        };
        assert_eq!(estimate_compact_prompt(10_000, usage), 5_800);
    }

    #[test]
    fn estimate_underflow_clamped() {
        let usage = CompactUsage {
            prompt_tokens: 15_000,
            completion_tokens: 800,
        };
        assert_eq!(estimate_compact_prompt(10_000, usage), 800);
    }

    #[test]
    fn estimate_zero_completion() {
        let usage = CompactUsage {
            prompt_tokens: 3_000,
            completion_tokens: 0,
        };
        assert_eq!(estimate_compact_prompt(10_000, usage), 7_000);
    }

    #[test]
    fn parse_task_outcomes_keeps_failed_blocks_in_order() {
        let result = "<tasks count=\"3\">\n\
            <task subagent=\"general\" task_id=\"id-1\" state=\"completed\">\n<task_result>\nok\n</task_result>\n</task>\n\
            <task subagent=\"reviewer\" state=\"failed\">\n<error>\nboom\n</error>\n</task>\n\
            <task subagent=\"researcher\" task_id=\"id-3\" state=\"completed\">\n<task_result>\nok\n</task_result>\n</task>\n\
            </tasks>";
        let blocks = parse_task_outcomes(result);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].task_id.as_deref(), Some("id-1"));
        assert!(blocks[0].ok);
        assert_eq!(blocks[1].task_id, None);
        assert!(!blocks[1].ok);
        assert_eq!(blocks[2].task_id.as_deref(), Some("id-3"));
        assert!(blocks[2].ok);
        assert_eq!(parse_task_id_from_result(result), Some("id-1".to_string()));
    }

    #[test]
    fn parse_task_id_skips_leading_failed_block() {
        let result = "<tasks count=\"2\">\n\
            <task subagent=\"general\" state=\"failed\">\n<error>\nboom\n</error>\n</task>\n\
            <task subagent=\"general\" task_id=\"id-2\" state=\"completed\">\n<task_result>\nok\n</task_result>\n</task>\n\
            </tasks>";
        assert_eq!(parse_task_id_from_result(result), Some("id-2".to_string()));
    }

    #[test]
    fn parse_task_id_from_single_result() {
        let result = "<task subagent=\"general\" task_id=\"uuid-x\" state=\"completed\">\n<task_result>\ndone\n</task_result>\n</task>";
        assert_eq!(parse_task_id_from_result(result), Some("uuid-x".into()));
        assert_eq!(parse_task_id_from_result("no tags here"), None);
    }

    #[test]
    fn mentions_json_for_empty_mentions_is_none() {
        assert!(mentions_json_for("hello", &[]).is_none());
    }

    #[test]
    fn mentions_json_for_builds_char_ranges() {
        let pairs = vec![("D:/p/src/main.rs".to_string(), "src/main.rs".to_string())];
        let json = mentions_json_for("看下 @D:/p/src/main.rs 谢谢", &pairs).unwrap();
        let list: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(list.len(), 1);
        let start = list[0]["start"].as_u64().unwrap() as usize;
        let end = list[0]["end"].as_u64().unwrap() as usize;
        assert_eq!(list[0]["kind"], "file");
        let text = "看下 @D:/p/src/main.rs 谢谢";
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(chars[start], '@');
        assert_eq!(chars[end - 1], 's');
    }
}
