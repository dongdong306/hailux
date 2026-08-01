use color_eyre::Result;
use std::time::Instant;

use super::{App, AppState, Message};
use crate::agent::Tool;
use crate::agent::command_def::INIT_COMMAND_NAME;
use crate::agent::subagent::{self, TaskTool};
use crate::storage::{MessageRole, StoredMessage};
use crate::tui::command;
use crate::tui::event::{AppEvent, TaskStatus};
use crate::tui::tasks_viewer::{TaskRecord, TaskRunStatus};

impl App {
    pub(super) async fn handle_chat_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::InputKey(key) => self.handle_chat_key(key).await?,
            AppEvent::InputPaste(text) => {
                self.handle_paste(text);
            }
            AppEvent::UserSubmit(input) => {
                if let Some(matched) = command::match_command(&input, &self.command_registry) {
                    self.handle_command(matched).await?;
                } else {
                    self.handle_user_message(input).await?;
                }
            }
            AppEvent::AgentChunk(chunk) => {
                self.finalize_thinking_ms();
                match self.messages.last_mut() {
                    Some(Message::AgentStreaming(text)) => {
                        text.push_str(&chunk);
                    }
                    _ => {
                        self.messages.push(Message::AgentStreaming(chunk));
                    }
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::AgentReasoningChunk(chunk) => {
                match self.messages.last_mut() {
                    Some(Message::AgentThinking { text, think_ms, .. }) if think_ms.is_none() => {
                        text.push_str(&chunk);
                    }
                    _ => {
                        self.messages.push(Message::AgentThinking {
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
            AppEvent::AgentComplete {
                messages: final_messages,
                usages,
                status,
            } => {
                // 计算最终耗时
                self.finalize_thinking_ms();
                if let Some(sent_at) = self.timing.user_msg_sent_at {
                    self.timing.last_total_ms = Some(sent_at.elapsed().as_millis() as u64);
                }
                // 将耗时和模型名写入 runtime_meta（含 status）
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
                        "model": self.resolved.display,
                        "status": status_str,
                    })
                    .to_string();
                    if let Err(e) = self
                        .storage
                        .update_last_message_runtime_meta(session_id, &meta)
                        .await
                    {
                        eprintln!("[warn] failed to persist runtime_meta: {e}");
                    }
                }
                // 重置实时时间字段（保留 final 字段用于展示）
                self.timing.user_msg_sent_at = None;

                if let Some(Message::AgentStreaming(text)) = self.messages.last() {
                    let text = text.clone();
                    *self.messages.last_mut().unwrap() = Message::Agent(text);
                }
                // 添加耗时消息
                if let Some(total_ms) = self.timing.last_total_ms {
                    self.messages.push(Message::AgentDone {
                        total_ms,
                        model: self.resolved.display.clone(),
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
                if let Some(session_id) = &self.current_session_id {
                    self.storage.touch_session(session_id).await?;
                }

                let total_context = self.context_prompt_tokens + self.context_completion_tokens;
                let threshold =
                    (self.resolved.context_window as f32 * self.config.compact_threshold) as u32;
                if total_context > threshold {
                    let pct = (self.config.compact_threshold * 100.0) as u32;
                    self.compact_conversation(Some(&format!(
                        "上下文 token 已达 {}%，自动压缩",
                        pct
                    )))
                    .await?;
                }
            }
            AppEvent::UsageUpdate {
                prompt_tokens,
                completion_tokens,
            } => {
                self.set_session_usage(prompt_tokens, completion_tokens);
            }
            AppEvent::PersistMessage {
                msg,
                usage,
                display,
            } => {
                if let Some(session_id) = &self.current_session_id {
                    let mut stored = crate::storage::to_stored_message(&msg);
                    if let Some((pt, ct)) = usage {
                        stored.prompt_tokens = Some(pt as i64);
                        stored.completion_tokens = Some(ct as i64);
                    }
                    if let Some(ref d) = display {
                        stored.runtime_meta = Some(d.clone());
                    }
                    if stored.role == MessageRole::Assistant && stored.reasoning_content.is_some() {
                        let boundary_idx = self
                            .messages
                            .iter()
                            .rposition(|m| {
                                matches!(
                                    m,
                                    Message::User { .. }
                                        | Message::ToolCall { .. }
                                        | Message::ToolResult { .. }
                                        | Message::Agent(_)
                                        | Message::AgentDone { .. }
                                )
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
            AppEvent::ToolCallStart {
                name,
                arguments,
                subagent_name,
            } => {
                self.finalize_thinking_ms();
                if let Some(san) = subagent_name {
                    self.messages.push(Message::SubagentStep {
                        name: san.clone(),
                        summary: crate::tui::history_cell::tool_call_summary(&name, &arguments),
                        is_done: false,
                        task_call_id: self.tasks.active_call_id,
                    });
                } else {
                    if name == "task"
                        && let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&arguments)
                    {
                        let sub_name = args_val["subagent"].as_str().unwrap_or("").to_string();
                        let desc = args_val["description"].as_str().unwrap_or("").to_string();
                        if !sub_name.is_empty() {
                            self.tasks.call_counter += 1;
                            let call_id = self.tasks.call_counter;
                            self.tasks.active_call_id = Some(call_id);
                            self.tasks.records.push(TaskRecord {
                                call_id,
                                session_id: String::new(),
                                subagent_name: sub_name,
                                description: desc,
                                started_at: std::time::Instant::now(),
                                status: TaskRunStatus::Running,
                            });
                            if self.tasks.records.len() > 500 {
                                self.tasks.records.remove(0);
                            }
                        }
                    }
                    self.messages.push(Message::ToolCall { name, arguments });
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::ToolResult {
                name,
                result,
                display,
                subagent_name,
            } => {
                if subagent_name.is_some() {
                    let tool_name = name.as_str();
                    let result_summary =
                        crate::tui::history_cell::tool_result_summary(tool_name, &result);
                    if let Some(Message::SubagentStep {
                        is_done, summary, ..
                    }) = self.messages.last_mut()
                    {
                        *is_done = true;
                        if !result_summary.is_empty() {
                            summary.push_str(&format!(" — {}", result_summary));
                        }
                    }
                } else {
                    if name == "task" {
                        if let Some(sid) = parse_task_id_from_result(&result) {
                            if let Some(call_id) = self.tasks.active_call_id.take()
                                && let Some(record) =
                                    self.tasks.records.iter_mut().find(|r| r.call_id == call_id)
                            {
                                record.session_id = sid;
                                record.status = TaskRunStatus::Completed;
                            }
                        } else if let Some(call_id) = self.tasks.active_call_id.take()
                            && let Some(record) =
                                self.tasks.records.iter_mut().find(|r| r.call_id == call_id)
                        {
                            record.status = TaskRunStatus::Error;
                        }
                    }
                    self.messages.push(Message::ToolResult {
                        name,
                        result,
                        display,
                    });
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::Resize => {}
            AppEvent::ScrollUp => {
                self.should_auto_scroll = false;
                self.scroll_offset = self.scroll_offset.saturating_add(3);
            }
            AppEvent::ScrollDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(3);
                if self.scroll_offset == 0 {
                    self.should_auto_scroll = true;
                }
            }
            AppEvent::AskUser {
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
            }
            AppEvent::McpReady(_) => {}
            AppEvent::MouseClick => {}
            AppEvent::CompactChunk(chunk) => {
                if let Some(Message::CompactStreaming(text)) = self.messages.last_mut() {
                    text.push_str(&chunk);
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::CompactComplete {
                summary,
                session_id,
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

                    if let Some(Message::CompactStreaming(_)) = self.messages.last_mut() {
                        *self.messages.last_mut().unwrap() = Message::CompactMarker {
                            summary: summary.clone(),
                            compacted_count,
                        };
                    }

                    self.should_auto_scroll = true;
                    self.scroll_offset = 0;
                    self.render.dirty = true;
                }

                self.is_processing = false;
                self.input.set_processing(false);
            }
            AppEvent::CompactError(msg) => {
                if let Some(Message::CompactStreaming(_)) = self.messages.last_mut() {
                    *self.messages.last_mut().unwrap() =
                        Message::Agent(format!("[压缩失败: {}]", msg));
                }
                self.is_processing = false;
                self.input.set_processing(false);
                self.render.dirty = true;
            }
        }
        Ok(())
    }

    pub(super) async fn handle_command(&mut self, matched: command::MatchedCommand) -> Result<()> {
        match matched {
            command::MatchedCommand::Ui(cmd) => {
                self.input.clear();
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
                    command::Command::Plan => {
                        self.toggle_plan_mode();
                    }
                    command::Command::Compact => {
                        self.compact_conversation(None).await?;
                    }
                    command::Command::Exit => {
                        self.should_quit = true;
                    }
                }
            }
            command::MatchedCommand::Prompt { name, args } => {
                if let Some(cmd) = self.command_registry.find(&name) {
                    // /init 需要写入 AGENTS.md，但规划模式会过滤 write/edit 工具，
                    // 直接执行会导致 LLM 静默失败（只能输出文本，写不了文件）。
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
                    self.handle_user_message(rendered).await?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn toggle_plan_mode(&mut self) {
        self.plan_mode = !self.plan_mode;
        self.agent.set_plan_mode(self.plan_mode);
    }

    /// 从思考阶段过渡到输出阶段时，冻结当前思考块的耗时。
    pub(super) fn finalize_thinking_ms(&mut self) {
        if let Some(Message::AgentThinking {
            think_ms,
            thinking_started_at,
            ..
        }) = self.messages.last_mut()
            && think_ms.is_none()
            && let Some(t) = thinking_started_at.take()
        {
            *think_ms = Some(t.elapsed().as_millis() as u64);
        }
    }

    pub(super) async fn handle_user_message(&mut self, input: String) -> Result<()> {
        self.is_processing = true;
        self.input.set_processing(true);
        self.timing.user_msg_sent_at = Some(Instant::now());
        self.timing.last_total_ms = None;
        self.input.submit(&input);
        self.pending_pastes.clear();
        self.file_picker.pending_mentions.clear();
        self.should_auto_scroll = true;
        self.scroll_offset = 0;
        self.messages.push(Message::User {
            text: input.clone(),
            plan_mode: self.plan_mode,
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
        if new_session && let Some(prompt) = self.agent.take_system_prompt() {
            let sys_stored = StoredMessage {
                role: MessageRole::System,
                content: prompt,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                prompt_tokens: None,
                completion_tokens: None,
                runtime_meta: None,
                think_ms: None,
                compacted: false,
            };
            self.storage
                .append_message(&session_id, &sys_stored)
                .await?;
        }
        let stored = StoredMessage {
            role: MessageRole::User,
            content: input.clone(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            prompt_tokens: None,
            completion_tokens: None,
            runtime_meta: if self.plan_mode {
                Some(r#"{"plan_mode":true}"#.to_string())
            } else {
                None
            },
            think_ms: None,
            compacted: false,
        };
        self.storage.append_message(&session_id, &stored).await?;

        if new_session {
            let title: String = input.chars().take(50).collect();
            self.storage
                .update_session_title(&session_id, &title)
                .await?;
        }

        // 检查是否为 @subagent: 语法
        if let Some((subagent_name, subagent_prompt)) = subagent::parse_subagent_input(&input) {
            // 查找 subagent 配置
            if self.subagents.iter().any(|s| s.name == subagent_name) {
                let task_tool = TaskTool::new(
                    self.subagents.clone(),
                    self.skills.clone(),
                    self.storage.clone(),
                    self.resolved.config.clone(),
                    self.resolved.display.clone(),
                    self.resolved.max_tokens,
                    self.work_dir.clone(),
                    self.shared.current_session.clone(),
                    self.shared.mcp_backends.clone(),
                    self.shared.config.clone(),
                    Some(self.events.0.clone()),
                );

                let task_description = format!(
                    "@{}: {}",
                    subagent_name,
                    subagent_prompt.chars().take(60).collect::<String>()
                );

                let args = serde_json::json!({
                    "subagent": subagent_name,
                    "description": task_description.clone(),
                    "prompt": subagent_prompt,
                })
                .to_string();

                self.messages.push(Message::ToolCall {
                    name: "task".to_string(),
                    arguments: args.clone(),
                });
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }

                self.tasks.call_counter += 1;
                let call_id = self.tasks.call_counter;
                self.tasks.active_call_id = Some(call_id);
                self.tasks.records.push(TaskRecord {
                    call_id,
                    session_id: String::new(),
                    subagent_name: subagent_name.clone(),
                    description: task_description.clone(),
                    started_at: std::time::Instant::now(),
                    status: TaskRunStatus::Running,
                });

                let result = task_tool.execute_async(&args).await;
                let task_status = if result.is_ok() {
                    TaskRunStatus::Completed
                } else {
                    TaskRunStatus::Error
                };
                let result_text = result.unwrap_or_else(|err| err.message);

                let sub_session_id = parse_task_id_from_result(&result_text);
                self.tasks.active_call_id = None;
                if let Some(record) = self.tasks.records.iter_mut().find(|r| r.call_id == call_id) {
                    record.session_id = sub_session_id.unwrap_or_default();
                    record.status = task_status;
                }

                self.messages.push(Message::ToolResult {
                    name: "task".to_string(),
                    result: result_text.clone(),
                    display: None,
                });

                // 将 subagent 结果作为 assistant 消息持久化到主 session
                let assistant_stored = StoredMessage {
                    role: MessageRole::Assistant,
                    content: result_text,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    runtime_meta: None,
                    think_ms: None,
                    compacted: false,
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
        }

        let tx = self.events.0.clone();
        if let Err(e) = self.agent.chat_stream(&input, tx) {
            self.messages.push(Message::Agent(format!("[错误: {}]", e)));
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
        // 少于 4 条非 system 消息（不足 2 轮对话）时，压缩没有意义
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

        let label = if let Some(reason) = auto_reason {
            format!("正在压缩上下文（{}）...\n", reason)
        } else {
            "正在压缩上下文...\n".to_string()
        };
        self.messages.push(Message::CompactStreaming(label));
        self.render.dirty = true;

        let tx = self.events.0.clone();
        if let Err(e) = self.agent.request_compaction(tx, session_id) {
            if let Some(Message::CompactStreaming(_)) = self.messages.last_mut() {
                *self.messages.last_mut().unwrap() = Message::Agent(format!("[压缩失败: {}]", e));
            }
            self.is_processing = false;
            self.input.set_processing(false);
        }
        Ok(())
    }
}

/// 从 task 工具结果文本中解析 sub_session_id。
///
/// 格式: `<task subagent="..." description="..." task_id="UUID" state="completed">`
fn parse_task_id_from_result(result: &str) -> Option<String> {
    let marker = "task_id=\"";
    let start = result.find(marker)? + marker.len();
    let rest = &result[start..];
    let end = rest.find('"')?;
    let id = &rest[..end];
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}
