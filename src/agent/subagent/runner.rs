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
    BashTool, EditTool, GlobTool, GrepTool, ReadTool, SkillTool, TodoWriteTool, WebFetchTool,
    WriteTool,
};
use crate::mcp::{McpTool, SharedMcpBackends};
use crate::storage::{ChatStorage, MessageRole};

const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// TaskTool 的可克隆依赖集合，供并发运行的多个 subagent 任务共享
#[derive(Clone)]
pub(super) struct SharedTaskCtx {
    pub(super) skills: Vec<SkillInfo>,
    pub(super) storage: ChatStorage,
    pub(super) model: String,
    pub(super) work_dir: String,
    pub(super) current_session_id: Arc<Mutex<Option<String>>>,
    pub(super) mcp_backends: SharedMcpBackends,
    pub(super) config: SharedConfig,
    /// 主会话的事件通道句柄，用于将 subagent 的工具调用过程实时转发到聊天区
    pub(super) main_event_hub: crate::agent::event::EventHub,
    /// 权限管理器（与主 Agent 共享）
    pub(super) permission: crate::permission::PermissionManager,
}

impl SharedTaskCtx {
    /// 构建 subagent 的 Agent 并注册工具
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
        let (agent_config, agent_model, agent_display, agent_max_tokens, agent_supports_vision) =
            app_config
                .resolve(model_selector)
                .map(|resolved| {
                    (
                        resolved.config,
                        resolved.model_id,
                        resolved.display,
                        resolved.max_tokens,
                        resolved.supports_vision,
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
            self.permission.clone(),
            &self.work_dir,
            app_config.retry_config(),
        );

        // 注册内建工具（不包含 task 和 ask_user）
        let allowed = config.allowed_tools.as_ref();

        let register_if =
            |tool: Box<dyn Tool>, agent: &mut Agent, allowed: Option<&Vec<String>>| {
                let name = tool.name().to_string();
                let is_allowed = allowed.is_none_or(|list| list.iter().any(|t| t == &name));
                if is_allowed {
                    agent.register_tool(tool);
                }
            };

        register_if(Box::new(BashTool), &mut agent, allowed);
        register_if(Box::new(ReadTool), &mut agent, allowed);
        register_if(Box::new(EditTool), &mut agent, allowed);
        register_if(Box::new(WriteTool), &mut agent, allowed);
        register_if(Box::new(GrepTool), &mut agent, allowed);
        register_if(Box::new(GlobTool), &mut agent, allowed);
        register_if(Box::<WebFetchTool>::default(), &mut agent, allowed);
        register_if(Box::new(TodoWriteTool), &mut agent, allowed);
        // ask_user 不注册给 subagent

        // 注册 skill 工具（仅注册 allowed_skills 中指定的 skill）
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
                // 将 skill 摘要注入 system prompt
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

        // 注册 MCP 工具（仅注册 allowed_mcp_servers 中指定的服务器）
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

        // 设置 system prompt
        let mut system_prompt = config.system_prompt.clone();
        system_prompt.push_str(&skills_available);
        system_prompt.push_str(&format!("\n\nCurrent working directory: {}", self.work_dir));
        agent.set_system_prompt(&system_prompt);

        Ok(agent)
    }
}

/// 运行单个 subagent 任务，始终返回格式化的 `<task>` 结果块；
/// 内部失败以 `state="failed"` + `<error>` 呈现，不影响其他并发任务
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

    // 获取 parent session_id
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

    // 判断是恢复已有会话还是创建新会话
    let (sub_session_id, agent) = if let Some(existing_id) = task_id {
        // 恢复已有会话：加载历史消息
        let stored_messages =
            ctx.storage
                .load_messages(&existing_id)
                .await
                .map_err(|e| ToolExecuteError {
                    message: e.to_string(),
                })?;

        // 构建 subagent 并加载历史消息
        let mut restore_agent = ctx.build_subagent(&config)?;

        // 将历史消息加载到 agent 中
        let mut chat_messages = Vec::new();
        for msg in &stored_messages {
            if let Some(chat_msg) = crate::storage::from_stored_message(msg) {
                chat_messages.push(std::sync::Arc::new(chat_msg));
            }
        }
        if !chat_messages.is_empty() {
            // 保留已有的 system prompt（build_subagent 已设置），仅加载非 system 消息
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

        // 持久化新的 user prompt
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
        };
        if let Err(e) = ctx.storage.append_message(&existing_id, &user_stored).await {
            eprintln!("[warn] Failed to persist subagent user message: {e}");
        }

        (existing_id, restore_agent)
    } else {
        // 创建新会话
        // subagent 实际使用的模型（与 subsession 的 sessions.model 同源，用于用量统计归属）
        let sub_model = config.model.clone().unwrap_or_else(|| ctx.model.clone());
        let sub_session_id = ctx
            .storage
            .create_subsession(&parent_id, &sub_model, &ctx.work_dir)
            .await
            .map_err(|e| ToolExecuteError {
                message: e.to_string(),
            })?;

        // 将 subagent 名称和任务描述写入 title，格式: "name|description"
        let title = format!("{}|{}", config.name, description);
        if let Err(e) = ctx
            .storage
            .update_session_title(&sub_session_id, &title)
            .await
        {
            eprintln!("[warn] Failed to set subagent session title: {e}");
        }

        // 持久化 user prompt
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
        };
        if let Err(e) = ctx
            .storage
            .append_message(&sub_session_id, &user_stored)
            .await
        {
            eprintln!("[warn] Failed to persist subagent user message: {e}");
        }

        // 构建 subagent
        let agent = ctx.build_subagent(&config)?;

        (sub_session_id, agent)
    };

    let mut agent = agent;

    // 创建局部 event channel
    let (sub_tx, mut sub_rx) = create_core_event_channel();

    // 启动 subagent 流式对话
    agent
        .chat_stream(&prompt, Vec::new(), sub_tx)
        .map_err(|e| ToolExecuteError {
            message: e.to_string(),
        })?;

    // 消费 subagent 事件，提取最终结果并转发工具调用过程到主 TUI
    let mut final_text = String::new();
    let mut completed = false;
    while let Some(event) = sub_rx.recv().await {
        match event {
            CoreEvent::AgentChunk(chunk) => {
                final_text.push_str(&chunk);
            }
            CoreEvent::AgentRetry { .. } => {
                // 失败尝试的部分输出作废：清空累积，防止「通道提前关闭」
                // 场景把两次生成的文本拼在一起返回
                final_text.clear();
            }
            CoreEvent::AgentComplete { messages, .. } => {
                // 从最终消息中提取最后一条 assistant 消息的文本
                for msg in messages.iter().rev() {
                    if let CompatibleChatCompletionRequestMessage::Assistant(assistant) = msg.as_ref()
                        && let Some(ref content) = assistant.base.content
                            && let async_openai::types::chat::ChatCompletionRequestAssistantMessageContent::Text(t) = content
                                && !t.is_empty() {
                                    final_text = t.clone();
                                    break;
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
                // 持久化到 subagent session
                let mut stored = crate::storage::to_stored_message(&msg);
                if let Some(u) = usage {
                    stored.prompt_tokens = Some(u.prompt_tokens as i64);
                    stored.completion_tokens = Some(u.completion_tokens as i64);
                    stored.cached_tokens = Some(u.cached_tokens as i64);
                }
                // 用量统计按消息级模型归属（事件携带的流式快照值）
                stored.model = Some(model);
                if let Some(ref d) = display {
                    stored.runtime_meta = Some(d.clone());
                }
                if let Err(e) = ctx.storage.append_message(&sub_session_id, &stored).await {
                    eprintln!("[warn] Failed to persist subagent message: {e}");
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
            // 其他事件全部忽略
            _ => {}
        }
    }

    // 信道提前关闭（chat_stream 内部任务异常退出）：已收到的只是截断输出，
    // 不能当作成功结果返回
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

/// 按字符数截断，超长时追加省略号
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max_chars).collect();
        format!("{cut}...")
    }
}
