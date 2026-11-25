use std::sync::{Arc, Mutex};

use super::SharedConfig;
use super::SubagentConfig;
use super::task_tool::TaskSpec;
use crate::agent::Agent;
use crate::agent::event::{CoreEvent, create_core_event_channel};
use crate::agent::models::CompatibleChatCompletionRequestMessage;
use crate::agent::skill::{SkillInfo, format_available_skills};
use crate::agent::tools::{Tool, ToolExecuteError};
use crate::agent::{
    EditTool, GlobTool, GrepTool, ReadTool, ShellTool, SkillTool, TodoWriteTool, WebFetchTool,
    WriteTool,
};
use crate::mcp::{McpTool, SharedMcpBackends};
use crate::storage::{ChatStorage, MessageRole};

const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Cloneable dependency set of TaskTool, shared by concurrently running
/// subagent tasks
#[derive(Clone)]
pub(super) struct SharedTaskCtx {
    pub(super) skills: Vec<SkillInfo>,
    pub(super) storage: ChatStorage,
    pub(super) model: String,
    pub(super) work_dir: String,
    pub(super) current_session_id: Arc<Mutex<Option<String>>>,
    pub(super) mcp_backends: SharedMcpBackends,
    pub(super) config: SharedConfig,
    /// Handle to the main session's event channel, forwarding subagent tool
    /// activity to the chat area in real time
    pub(super) main_event_hub: crate::agent::event::EventHub,
    /// Permission manager (shared with the main Agent)
    pub(super) permission: crate::permission::PermissionManager,
}

impl SharedTaskCtx {
    /// Builds the subagent's Agent and registers its tools
    fn build_subagent(&self, config: &SubagentConfig) -> Result<Agent, ToolExecuteError> {
        let model_selector = config.model.as_ref().ok_or_else(|| ToolExecuteError {
            message: format!("subagent \"{}\" has no model configured; please specify the model field in AGENTS.md", config.name),
        })?;
        let app_config = {
            let guard = self.config.lock().map_err(|e| ToolExecuteError {
                message: format!("Failed to lock config: {e}"),
            })?;
            guard.clone()
        };
        let (
            agent_config,
            agent_model,
            agent_display,
            agent_max_tokens,
            agent_supports_vision,
            agent_native_reasoning,
        ) = app_config
            .resolve(model_selector)
            .map(|resolved| {
                (
                    resolved.config,
                    resolved.model_id,
                    resolved.display,
                    resolved.max_tokens,
                    resolved.supports_vision,
                    resolved.native_reasoning,
                )
            })
            .map_err(|e| ToolExecuteError {
                message: format!(
                    "Failed to resolve model \"{}\" for subagent \"{}\": {e}",
                    model_selector, config.name
                ),
            })?;

        let mut agent = Agent::new(
            agent_config,
            &agent_model,
            &agent_display,
            agent_max_tokens,
            agent_supports_vision,
            agent_native_reasoning,
            app_config.reasoning_effort,
            self.permission.clone(),
            &self.work_dir,
            app_config.retry_config(),
        );

        let allowed = config.allowed_tools.as_ref();

        let register_if =
            |tool: Box<dyn Tool>, agent: &mut Agent, allowed: Option<&Vec<String>>| {
                let name = tool.name().to_string();
                let is_allowed = allowed.is_none_or(|list| list.iter().any(|t| t == &name));
                if is_allowed {
                    agent.register_tool(tool);
                }
            };

        register_if(Box::new(ShellTool), &mut agent, allowed);
        register_if(Box::new(ReadTool), &mut agent, allowed);
        register_if(Box::new(EditTool), &mut agent, allowed);
        register_if(Box::new(WriteTool), &mut agent, allowed);
        register_if(Box::new(GrepTool), &mut agent, allowed);
        register_if(Box::new(GlobTool), &mut agent, allowed);
        register_if(Box::<WebFetchTool>::default(), &mut agent, allowed);
        register_if(Box::new(TodoWriteTool), &mut agent, allowed);
        // question is intentionally omitted: subagents run non-interactively
        // (isolated event channel, only the final text is returned).

        // Register the skill tool (only skills listed in allowed_skills)
        let mut skills_available = String::new();
        if let Some(ref allowed_skill_names) = config.allowed_skills {
            let filtered_skills: Vec<SkillInfo> = self
                .skills
                .iter()
                .filter(|s| allowed_skill_names.iter().any(|name| name == &s.name))
                .cloned()
                .collect();
            if !filtered_skills.is_empty() {
                agent.register_tool(Box::new(SkillTool::new(filtered_skills.clone())));
                // Inject the skill summary into the system prompt
                let available = format_available_skills(&filtered_skills);
                if !available.is_empty() {
                    skills_available = format!(
                        "\n\n{available}\n\n\
                         Load a specialized skill when the task at hand matches one of the skills listed above. \
                         Use the `skill` tool (passing the skill `name`) to load its full instructions and base directory, \
                         then use `read`/`glob` to load any referenced scripts or files."
                    );
                }
            }
        }

        // Register MCP tools (only servers listed in allowed_mcp_servers)
        if let Some(ref allowed_servers) = config.allowed_mcp_servers
            && let Ok(backends) = self.mcp_backends.lock()
        {
            for backend in backends.iter() {
                if !allowed_servers.iter().any(|s| s == &backend.server_name) {
                    continue;
                }
                for tool_def in &backend.tools {
                    let tool_name = format!("mcp__{}__{}", backend.server_name, tool_def.name);
                    let is_allowed = allowed.is_none_or(|list| {
                        list.iter().any(|t| t == &tool_name || t == &tool_def.name)
                    });
                    if is_allowed {
                        agent.register_tool(Box::new(McpTool::new(
                            &backend.server_name,
                            tool_def,
                            backend.backend.clone(),
                        )));
                    }
                }
            }
        }

        // Set the system prompt
        let mut system_prompt = config.system_prompt.clone();
        system_prompt.push_str(&skills_available);
        system_prompt.push_str(&format!("\n\nCurrent working directory: {}", self.work_dir));
        agent.set_system_prompt(&system_prompt);

        Ok(agent)
    }
}

/// Runs a single subagent task, always returning a formatted `<task>` result
/// block; internal failures surface as `state="failed"` + `<error>` without
/// affecting other concurrent tasks
pub(super) async fn run_subagent_task(
    ctx: SharedTaskCtx,
    config: SubagentConfig,
    spec: TaskSpec,
    index: usize,
) -> String {
    let esc_name = xml_escape(&config.name);
    let esc_desc = xml_escape(&spec.description);
    match run_subagent_task_inner(ctx, config, spec, index).await {
        Ok((sub_session_id, final_text)) => format!(
            "<task subagent=\"{}\" description=\"{}\" task_id=\"{}\" state=\"completed\">\n<task_result>\n{}\n</task_result>\n</task>",
            esc_name,
            esc_desc,
            sub_session_id,
            xml_escape(&final_text)
        ),
        Err(e) => format!(
            "<task subagent=\"{}\" description=\"{}\" state=\"failed\">\n<error>\n{}\n</error>\n</task>",
            esc_name,
            esc_desc,
            xml_escape(&e.message)
        ),
    }
}

async fn run_subagent_task_inner(
    ctx: SharedTaskCtx,
    config: SubagentConfig,
    spec: TaskSpec,
    index: usize,
) -> Result<(String, String), ToolExecuteError> {
    let TaskSpec {
        description,
        prompt,
        task_id,
        ..
    } = spec;
    let subagent_name = config.name.clone();

    // Get the parent session_id
    let parent_id = {
        let guard = ctx
            .current_session_id
            .lock()
            .map_err(|e| ToolExecuteError {
                message: e.to_string(),
            })?;
        guard.clone()
    };

    let parent_id = parent_id.ok_or_else(|| ToolExecuteError {
        message: "No active session; cannot create subagent session".to_string(),
    })?;

    // Decide whether to resume an existing session or create a new one
    let (sub_session_id, agent) = if let Some(existing_id) = task_id {
        // Resume an existing session: load history messages
        // Resume-path fallback: repair historical dangling tool_calls
        // (idempotent). When the main session was interrupted, the detached
        // task's event channel is already closed, so persisting stubs can
        // only happen here
        if let Err(e) = ctx.storage.repair_orphaned_tool_calls(&existing_id).await {
            eprintln!("[warn] repair_orphaned_tool_calls: {e}");
        }
        let stored_messages =
            ctx.storage
                .load_messages(&existing_id)
                .await
                .map_err(|e| ToolExecuteError {
                    message: e.to_string(),
                })?;

        // Build the subagent and load history messages
        let mut restore_agent = ctx.build_subagent(&config)?;

        // Load the history messages into the agent
        let mut chat_messages = Vec::new();
        for msg in &stored_messages {
            if let Some(chat_msg) = crate::storage::from_stored_message(msg) {
                chat_messages.push(std::sync::Arc::new(chat_msg));
            }
        }
        if !chat_messages.is_empty() {
            // Keep the existing system prompt (set by build_subagent); load
            // non-system messages only
            let non_system: Vec<_> = chat_messages
                .into_iter()
                .filter(|m| {
                    !matches!(
                        m.as_ref(),
                        CompatibleChatCompletionRequestMessage::System(_)
                    )
                })
                .collect();
            restore_agent.sync_messages(non_system);
        }

        // Persist the new user prompt
        let user_stored = crate::storage::StoredMessage {
            role: MessageRole::User,
            content: prompt.clone(),
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
        if let Err(e) = ctx.storage.append_message(&existing_id, &user_stored).await {
            eprintln!("[warn] Failed to persist subagent user message: {e}");
        }

        (existing_id, restore_agent)
    } else {
        // Create a new session
        // Model actually used by the subagent (same source as the subsession's
        // sessions.model, used for usage attribution)
        let sub_model = config.model.clone().unwrap_or_else(|| ctx.model.clone());
        let sub_session_id = ctx
            .storage
            .create_subsession(&parent_id, &sub_model, &ctx.work_dir)
            .await
            .map_err(|e| ToolExecuteError {
                message: e.to_string(),
            })?;

        // Write the subagent name and task description into the title,
        // format: "name|description"
        let title = format!("{}|{}", config.name, description);
        if let Err(e) = ctx
            .storage
            .update_session_title(&sub_session_id, &title)
            .await
        {
            eprintln!("[warn] Failed to set subagent session title: {e}");
        }

        // Persist the user prompt
        let user_stored = crate::storage::StoredMessage {
            role: MessageRole::User,
            content: prompt.clone(),
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
        if let Err(e) = ctx
            .storage
            .append_message(&sub_session_id, &user_stored)
            .await
        {
            eprintln!("[warn] Failed to persist subagent user message: {e}");
        }

        // Build the subagent
        let agent = ctx.build_subagent(&config)?;

        (sub_session_id, agent)
    };

    let mut agent = agent;

    // Create a local event channel
    let (sub_tx, mut sub_rx) = create_core_event_channel();

    // Start the subagent's streaming conversation
    agent
        .chat_stream(&sub_session_id, &prompt, Vec::new(), sub_tx)
        .map_err(|e| ToolExecuteError {
            message: e.to_string(),
        })?;

    // Consume subagent events: extract the final result and forward tool
    // activity to the main TUI
    let mut final_text = String::new();
    let mut completed = false;
    while let Some(event) = sub_rx.recv().await {
        match event {
            CoreEvent::AgentChunk(chunk) => {
                final_text.push_str(&chunk);
            }
            CoreEvent::AgentRetry { .. } => {
                // Discard partial output of the failed attempt: clear the
                // accumulation so the "channel closed early" case never
                // concatenates text from two separate runs
                final_text.clear();
            }
            CoreEvent::AgentComplete {
                messages, status, ..
            } => {
                // Extract the final assistant text only on non-error completion.
                // On error the context no longer carries a synthesized
                // [Error: ...] assistant message, and a reverse scan would
                // wrongly surface an *earlier* turn's text (masking the
                // failure) — keep the chunk-accumulated text instead.
                // Deliberately still a success-shaped return: the accumulated
                // text ends with the run's "\n[Error: ...]" chunk, so the
                // caller sees the failure reason inline; converting to
                // ToolExecuteError here would also discard any partial output.
                if status != crate::agent::event::TaskStatus::Error {
                    for msg in messages.iter().rev() {
                        if let CompatibleChatCompletionRequestMessage::Assistant(assistant) = msg.as_ref()
                            && let Some(ref content) = assistant.base.content
                                && let async_openai::types::chat::ChatCompletionRequestAssistantMessageContent::Text(t) = content
                                    && !t.is_empty() {
                                        final_text = t.clone();
                                        break;
                                    }
                    }
                }
                completed = true;
                break;
            }
            CoreEvent::PersistMessage {
                msg,
                usage,
                model,
                display,
            } => {
                // Persist to the subagent session
                let mut stored = crate::storage::to_stored_message(&msg);
                if let Some(u) = usage {
                    stored.prompt_tokens = Some(u.prompt_tokens as i64);
                    stored.completion_tokens = Some(u.completion_tokens as i64);
                    stored.cached_tokens = Some(u.cached_tokens as i64);
                }
                // Usage stats are attributed by the message-level model
                // (streaming snapshot value carried by the event)
                stored.model = Some(model);
                if let Some(ref d) = display {
                    stored.runtime_meta = Some(d.clone());
                }
                if let Err(e) = ctx.storage.append_message(&sub_session_id, &stored).await {
                    eprintln!("[warn] Failed to persist subagent message: {e}");
                }
            }
            CoreEvent::PersistUserReminder {
                session_id: reminder_session,
                reminder,
            } => {
                // Land the in-memory reminder into the subagent row's
                // runtime_meta (resume rebuilds splice it back in order)
                if let Err(e) = ctx
                    .storage
                    .append_user_message_reminder(&reminder_session, &reminder)
                    .await
                {
                    eprintln!("[warn] Failed to persist subagent user reminder: {e}");
                }
            }
            CoreEvent::UsageUpdate {
                prompt_tokens,
                completion_tokens,
                ..
            } => {
                if let Err(e) = ctx
                    .storage
                    .update_session_usage(
                        &sub_session_id,
                        prompt_tokens as i64,
                        completion_tokens as i64,
                    )
                    .await
                {
                    eprintln!("[warn] Failed to update subagent session usage: {e}");
                }
            }
            CoreEvent::ToolCallStart {
                name, arguments, ..
            } => {
                let _ = ctx.main_event_hub.send(CoreEvent::ToolCallStart {
                    name,
                    arguments,
                    subagent_name: Some(subagent_name.clone()),
                    subagent_index: Some(index),
                });
            }
            CoreEvent::ToolResult {
                name,
                result,
                display,
                ..
            } => {
                let truncated = if result.chars().count() > TOOL_RESULT_MAX_CHARS {
                    let safe: String = result.chars().take(TOOL_RESULT_MAX_CHARS).collect();
                    format!("{safe}...(truncated)")
                } else {
                    result
                };
                let _ = ctx.main_event_hub.send(CoreEvent::ToolResult {
                    name,
                    result: truncated,
                    display,
                    subagent_name: Some(subagent_name.clone()),
                    subagent_index: Some(index),
                });
            }
            CoreEvent::PermissionRequest {
                request,
                response_tx,
                ..
            } => {
                let _ = ctx.main_event_hub.send(CoreEvent::PermissionRequest {
                    request,
                    response_tx,
                    subagent_name: Some(subagent_name.clone()),
                });
            }
            // All other events are ignored
            _ => {}
        }
    }

    // Channel closed early (the task inside chat_stream exited abnormally):
    // what was received is truncated output only and must not be returned as
    // a successful result
    if !completed {
        return Err(ToolExecuteError {
            message: format!(
                "subagent event channel closed before completion; partial output: {}",
                truncate_chars(&final_text, 200)
            ),
        });
    }

    if final_text.trim().is_empty() {
        final_text = "(Subagent returned no text result)".to_string();
    }

    Ok((sub_session_id, final_text))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Truncates by character count, appending an ellipsis when over the limit
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max_chars).collect();
        format!("{cut}...")
    }
}
