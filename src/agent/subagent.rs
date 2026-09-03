use super::Agent;
use super::models::CompatibleChatCompletionRequestMessage;
use super::tools::{Tool, ToolExecuteError};
use crate::agent::event::{CoreEvent, create_core_event_channel};
use crate::agent::skill::{SkillInfo, format_available_skills};
use crate::agent::utils;
use crate::agent::{
    BashTool, EditTool, GlobTool, GrepTool, ReadTool, SkillTool, TodoWriteTool, WebFetchTool,
    WriteTool,
};
use crate::mcp::{McpTool, SharedMcpBackends};
use crate::storage::{ChatStorage, MessageRole};
use color_eyre::Result;
use futures_util::future::join_all;
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

const CONFIG_DIR_NAME: &str = ".hailux";
const AGENT_DIR_NAME: &str = "agents";
const AGENT_FILE_NAME: &str = "AGENTS.md";
const TOOL_RESULT_MAX_CHARS: usize = 2000;
/// 单次 task 调用允许并发启动的 subagent 数量上限
const MAX_PARALLEL_TASKS: usize = 8;

/// 已发现的 subagent 配置
#[derive(Debug, Clone)]
pub struct SubagentConfig {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    /// None = 所有内建工具（除 task 和 ask_user）；Some = 仅白名单中的工具
    pub allowed_tools: Option<Vec<String>>,
    /// None = 不加载任何 skill；Some = 仅加载指定名称的 skill
    pub allowed_skills: Option<Vec<String>>,
    /// None = 不加载任何 MCP 工具；Some = 仅加载指定 MCP 服务器的工具
    pub allowed_mcp_servers: Option<Vec<String>>,
    pub model: Option<String>,
    #[allow(dead_code)]
    pub source_path: PathBuf,
}

/// AGENTS.md 解析结果
struct AgentMdInfo {
    name: String,
    description: String,
    tools: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    mcp: Option<Vec<String>>,
    model: Option<String>,
    system_prompt: String,
}

/// 解析 AGENTS.md 的 YAML frontmatter，提取 name、description、tools、model、skills、mcp。
fn parse_agent_md(raw: &str) -> Option<AgentMdInfo> {
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);

    if !trimmed.starts_with("---") {
        let content = trimmed.trim();
        if content.is_empty() {
            return None;
        }
        return Some(AgentMdInfo {
            name: String::new(),
            description: String::new(),
            tools: None,
            skills: None,
            mcp: None,
            model: None,
            system_prompt: content.to_string(),
        });
    }

    let (frontmatter, content) = utils::split_frontmatter(trimmed)?;

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut tools: Option<Vec<String>> = None;
    let mut skills: Option<Vec<String>> = None;
    let mut mcp: Option<Vec<String>> = None;
    let mut model: Option<String> = None;

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            if name.is_none() {
                name = Some(utils::strip_frontmatter_value(rest));
            }
        } else if let Some(rest) = line.strip_prefix("description:") {
            if description.is_none() {
                description = Some(utils::strip_frontmatter_value(rest));
            }
        } else if let Some(rest) = line.strip_prefix("tools:") {
            if tools.is_none() {
                tools = parse_tools_list(rest);
            }
        } else if let Some(rest) = line.strip_prefix("skills:") {
            if skills.is_none() {
                skills = parse_tools_list(rest);
            }
        } else if let Some(rest) = line.strip_prefix("mcp:") {
            if mcp.is_none() {
                mcp = parse_tools_list(rest);
            }
        } else if let Some(rest) = line.strip_prefix("model:")
            && model.is_none()
        {
            model = Some(utils::strip_frontmatter_value(rest));
        }
    }

    let name = name.filter(|n| !n.is_empty())?;
    let description = description.unwrap_or_default();

    Some(AgentMdInfo {
        name,
        description,
        tools,
        skills,
        mcp,
        model,
        system_prompt: content.to_string(),
    })
}

fn parse_tools_list(raw: &str) -> Option<Vec<String>> {
    let v = raw.trim();
    if v.is_empty() {
        return None;
    }
    // 支持 [tool1, tool2] 格式
    let inner = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(v);
    let tools: Vec<String> = inner
        .split(',')
        .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if tools.is_empty() { None } else { Some(tools) }
}

fn load_subagent(path: &Path) -> Result<Option<SubagentConfig>> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        color_eyre::eyre::eyre!("Failed to read agent file {}: {e}", path.display())
    })?;

    let info = match parse_agent_md(&raw) {
        Some(parsed) => parsed,
        None => return Ok(None),
    };

    let location = path.canonicalize().map_err(|e| {
        color_eyre::eyre::eyre!("Cannot canonicalize path: {}: {e}", path.display())
    })?;

    Ok(Some(SubagentConfig {
        name: info.name,
        description: info.description,
        system_prompt: info.system_prompt,
        allowed_tools: info.tools,
        allowed_skills: info.skills,
        allowed_mcp_servers: info.mcp,
        model: info.model,
        source_path: location,
    }))
}

fn scan_root(root: &Path, out: &mut Vec<SubagentConfig>) -> Result<()> {
    if !root.is_dir() {
        return Ok(());
    }

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .build();

    let mut found_paths: Vec<PathBuf> = Vec::new();
    for entry in walker {
        let entry = entry
            .map_err(|e| color_eyre::eyre::eyre!("Failed to traverse agent directory: {e}"))?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if entry.file_name() == AGENT_FILE_NAME {
            found_paths.push(entry.path().to_path_buf());
        }
    }

    for path in found_paths {
        match load_subagent(&path) {
            Ok(Some(info)) => out.push(info),
            Ok(None) => {}
            Err(e) => {
                eprintln!("[warn] Skipping agent {}: {e}", path.display());
            }
        }
    }

    Ok(())
}

/// 发现所有可用 subagent：全局 `~/.hailux/agents/` 与项目级 `<work_dir>/.hailux/agents/`。
/// 同名时项目级覆盖全局。
pub fn discover_subagents(work_dir: &Path) -> Result<Vec<SubagentConfig>> {
    let mut all: Vec<SubagentConfig> = Vec::new();

    if let Some(home) = dirs::home_dir() {
        let global_root = home.join(CONFIG_DIR_NAME).join(AGENT_DIR_NAME);
        scan_root(&global_root, &mut all)?;
    }

    let project_root = work_dir.join(CONFIG_DIR_NAME).join(AGENT_DIR_NAME);
    if project_root.is_dir() {
        scan_root(&project_root, &mut all)?;
    }

    let mut by_name: HashMap<String, SubagentConfig> = HashMap::new();
    for info in all {
        by_name.insert(info.name.clone(), info);
    }

    let mut result: Vec<SubagentConfig> = by_name.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

/// 生成注入 system prompt 的 `<available_subagents>` 块。
pub fn format_available_subagents(subagents: &[SubagentConfig]) -> String {
    let described: Vec<&SubagentConfig> = subagents
        .iter()
        .filter(|s| !s.description.is_empty())
        .collect();

    if described.is_empty() {
        return String::new();
    }

    let mut out = String::from("<available_subagents>\n");
    for sa in described {
        out.push_str("  <subagent>\n");
        out.push_str(&format!("    <name>{}</name>\n", sa.name));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            sa.description
        ));
        out.push_str("  </subagent>\n");
    }
    out.push_str("</available_subagents>");
    out
}

/// 内置 general subagent 的 system prompt
const GENERAL_SUBAGENT_PROMPT: &str = crate::prompts::GENERAL_SUBAGENT;

/// 创建内置的 general subagent 配置
pub fn builtin_general_subagent(main_model: &str) -> SubagentConfig {
    let model = if main_model.is_empty() {
        "deepseek/deepseek-chat"
    } else {
        main_model
    };
    SubagentConfig {
        name: "general".to_string(),
        description: "General-purpose subagent for multi-step research and coding tasks. Suitable for complex work requiring multiple tool calls."
            .to_string(),
        system_prompt: GENERAL_SUBAGENT_PROMPT.to_string(),
        allowed_tools: None,
        allowed_skills: None,
        allowed_mcp_servers: None,
        model: Some(model.to_string()),
        source_path: PathBuf::new(),
    }
}

/// Task 工具：并发启动多个 subagent 执行委派任务。
///
/// subagent 在独立 session 中运行，执行过程对主 TUI 不可见。
/// 仅返回最终结果文本作为工具结果。
pub type SharedConfig = Arc<Mutex<crate::config::Config>>;

/// tasks 数组中单个任务的解析结果
#[derive(Debug, Clone)]
struct TaskSpec {
    subagent: String,
    description: String,
    prompt: String,
    task_id: Option<String>,
}

/// 解析 tool 参数中的 `tasks` 数组
fn parse_task_specs(args: &Value) -> Result<Vec<TaskSpec>, String> {
    let Some(tasks) = args["tasks"].as_array() else {
        return Err(
            "missing required parameter \"tasks\": pass an array of task objects, e.g. \
             {\"tasks\": [{\"subagent\": \"general\", \"description\": \"...\", \"prompt\": \"...\"}]}"
                .to_string(),
        );
    };
    if tasks.is_empty() {
        return Err("\"tasks\" must contain at least one task".to_string());
    }
    let mut specs = Vec::with_capacity(tasks.len());
    for (i, task) in tasks.iter().enumerate() {
        let subagent = task["subagent"]
            .as_str()
            .ok_or_else(|| format!("tasks[{i}]: \"subagent\" is required"))?
            .to_string();
        let prompt = task["prompt"]
            .as_str()
            .ok_or_else(|| format!("tasks[{i}]: \"prompt\" is required"))?
            .to_string();
        if prompt.trim().is_empty() {
            return Err(format!("tasks[{i}]: \"prompt\" must not be empty"));
        }
        let description = task["description"].as_str().unwrap_or("").to_string();
        let task_id = task
            .get("task_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        specs.push(TaskSpec {
            subagent,
            description,
            prompt,
            task_id,
        });
    }
    Ok(specs)
}

/// 启动前的预校验：数量上限、subagent 名称存在、task_id 不重复；
/// 校验通过则返回每个任务对应的 subagent 配置（顺序与 specs 一致）
fn resolve_task_specs<'a>(
    specs: &[TaskSpec],
    subagents: &'a [SubagentConfig],
) -> Result<Vec<&'a SubagentConfig>, String> {
    if specs.len() > MAX_PARALLEL_TASKS {
        return Err(format!(
            "too many tasks in one call: {} (max {}); split into multiple calls",
            specs.len(),
            MAX_PARALLEL_TASKS
        ));
    }
    let mut configs = Vec::with_capacity(specs.len());
    for spec in specs {
        let Some(config) = subagents.iter().find(|s| s.name == spec.subagent) else {
            let available: Vec<&str> = subagents.iter().map(|s| s.name.as_str()).collect();
            return Err(format!(
                "Subagent \"{}\" not found. Available subagents: {}",
                spec.subagent,
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            ));
        };
        configs.push(config);
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for spec in specs {
        if let Some(id) = spec.task_id.as_deref()
            && !seen.insert(id)
        {
            return Err(format!(
                "duplicate task_id \"{id}\" in tasks array; each task must reference a distinct session"
            ));
        }
    }
    Ok(configs)
}

/// TaskTool 的可克隆依赖集合，供并发运行的多个 subagent 任务共享
#[derive(Clone)]
struct SharedTaskCtx {
    skills: Vec<SkillInfo>,
    storage: ChatStorage,
    model: String,
    work_dir: String,
    current_session_id: Arc<Mutex<Option<String>>>,
    mcp_backends: SharedMcpBackends,
    config: SharedConfig,
    /// 主会话的事件通道句柄，用于将 subagent 的工具调用过程实时转发到聊天区
    main_event_hub: crate::agent::event::EventHub,
    /// 权限管理器（与主 Agent 共享）
    permission: crate::permission::PermissionManager,
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
        let (agent_config, agent_model, agent_display, agent_max_tokens) = app_config
            .resolve(model_selector)
            .map(|resolved| {
                (
                    resolved.config,
                    resolved.model_id,
                    resolved.display,
                    resolved.max_tokens,
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
            self.permission.clone(),
            &self.work_dir,
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

pub struct TaskTool {
    subagents: Vec<SubagentConfig>,
    ctx: SharedTaskCtx,
}

impl TaskTool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        subagents: Vec<SubagentConfig>,
        skills: Vec<SkillInfo>,
        storage: ChatStorage,
        model: String,
        work_dir: String,
        current_session_id: Arc<Mutex<Option<String>>>,
        mcp_backends: SharedMcpBackends,
        config: SharedConfig,
        main_event_hub: crate::agent::event::EventHub,
        permission: crate::permission::PermissionManager,
    ) -> Self {
        Self {
            subagents,
            ctx: SharedTaskCtx {
                skills,
                storage,
                model,
                work_dir,
                current_session_id,
                mcp_backends,
                config,
                main_event_hub,
                permission,
            },
        }
    }
}

impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        crate::prompts::TASK_TOOL_TEMPLATE
    }

    fn parameters(&self) -> Value {
        let names: Vec<&str> = self.subagents.iter().map(|s| s.name.as_str()).collect();
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "Tasks to launch; each item starts one subagent and all items run concurrently",
                    "items": {
                        "type": "object",
                        "properties": {
                            "subagent": {
                                "type": "string",
                                "description": "Name of the subagent to use",
                                "enum": names
                            },
                            "description": {
                                "type": "string",
                                "description": "Short description of the task (3-5 words)"
                            },
                            "prompt": {
                                "type": "string",
                                "description": "Detailed task instructions for the subagent to execute"
                            },
                            "task_id": {
                                "type": "string",
                                "description": "Optional. Pass the session ID from a previous subagent invocation to resume the same session context, continuing the previous conversation instead of creating a new session"
                            }
                        },
                        "required": ["subagent", "description", "prompt"]
                    },
                    "minItems": 1,
                    "maxItems": MAX_PARALLEL_TASKS
                }
            },
            "required": ["tasks"]
        })
    }

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        let args: Value = serde_json::from_str(arguments).unwrap_or_default();

        let specs = match parse_task_specs(&args) {
            Ok(s) => s,
            Err(message) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError { message })));
            }
        };

        let ctx = self.ctx.clone();
        let configs: Vec<SubagentConfig> = match resolve_task_specs(&specs, &self.subagents) {
            Ok(configs) => configs.into_iter().cloned().collect(),
            Err(message) => {
                return Box::pin(std::future::ready(Err(ToolExecuteError { message })));
            }
        };

        Box::pin(async move {
            let futures = specs
                .into_iter()
                .zip(configs)
                .enumerate()
                .map(|(index, (spec, config))| run_subagent_task(ctx.clone(), config, spec, index));
            let blocks = join_all(futures).await;

            if blocks.len() == 1 {
                return Ok(blocks.into_iter().next().unwrap_or_default());
            }
            Ok(format!(
                "<tasks count=\"{}\">\n{}\n</tasks>",
                blocks.len(),
                blocks.join("\n")
            ))
        })
    }

    fn allowed_in_plan_mode(&self) -> bool {
        false
    }

    fn cancellable(&self) -> bool {
        true
    }
}

/// 运行单个 subagent 任务，始终返回格式化的 `<task>` 结果块；
/// 内部失败以 `state="failed"` + `<error>` 呈现，不影响其他并发任务
async fn run_subagent_task(
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
        .chat_stream(&prompt, sub_tx)
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

/// 解析 "@subagent: name work_content" 格式的输入
pub fn parse_subagent_input(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix("@subagent:")?.trim_start();
    let (name, prompt) = rest.split_once(char::is_whitespace)?;
    let name = name.trim();
    let prompt = prompt.trim();
    if name.is_empty() || prompt.is_empty() {
        return None;
    }
    Some((name.to_string(), prompt.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_frontmatter() {
        let raw = "---\nname: reviewer\ndescription: code review\n---\n# Body\nYou are a reviewer.";
        let info = parse_agent_md(raw).unwrap();
        assert_eq!(info.name, "reviewer");
        assert_eq!(info.description, "code review");
        assert!(info.tools.is_none());
        assert!(info.skills.is_none());
        assert!(info.mcp.is_none());
        assert!(info.model.is_none());
        assert_eq!(info.system_prompt, "# Body\nYou are a reviewer.");
    }

    #[test]
    fn parses_tools_and_model() {
        let raw = "---\nname: explorer\ndescription: explore code\ntools: [bash, read, grep, glob]\nmodel: deepseek/deepseek-chat\n---\nExplore.";
        let info = parse_agent_md(raw).unwrap();
        assert_eq!(info.name, "explorer");
        assert_eq!(info.description, "explore code");
        assert_eq!(
            info.tools,
            Some(vec![
                "bash".into(),
                "read".into(),
                "grep".into(),
                "glob".into()
            ])
        );
        assert_eq!(info.model, Some("deepseek/deepseek-chat".into()));
        assert_eq!(info.system_prompt, "Explore.");
    }

    #[test]
    fn parses_skills_and_mcp() {
        let raw = "---\nname: researcher\ndescription: research\nskills: [code-review, help]\nmcp: [context7, zread]\n---\nResearch.";
        let info = parse_agent_md(raw).unwrap();
        assert_eq!(info.name, "researcher");
        assert_eq!(info.skills, Some(vec!["code-review".into(), "help".into()]));
        assert_eq!(info.mcp, Some(vec!["context7".into(), "zread".into()]));
        assert_eq!(info.system_prompt, "Research.");
    }

    #[test]
    fn rejects_missing_name() {
        let raw = "---\ndescription: no name\n---\nbody";
        assert!(parse_agent_md(raw).is_none());
    }

    #[test]
    fn parses_without_frontmatter() {
        let raw = "Just a prompt";
        let info = parse_agent_md(raw).unwrap();
        assert_eq!(info.name, "");
        assert_eq!(info.description, "");
        assert!(info.tools.is_none());
        assert!(info.model.is_none());
        assert_eq!(info.system_prompt, "Just a prompt");
    }

    #[test]
    fn parse_subagent_input_basic() {
        let (name, prompt) = parse_subagent_input("@subagent: general 搜索所有 TODO 注释").unwrap();
        assert_eq!(name, "general");
        assert_eq!(prompt, "搜索所有 TODO 注释");
    }

    #[test]
    fn parse_subagent_input_no_match() {
        assert!(parse_subagent_input("hello world").is_none());
        assert!(parse_subagent_input("@subagent:").is_none());
        assert!(parse_subagent_input("@subagent: general").is_none());
    }

    #[test]
    fn discover_finds_project_level_subagent() {
        let tmp =
            std::env::temp_dir().join(format!("hailux-subagent-test-{}", uuid::Uuid::new_v4()));
        let agent_dir = tmp
            .join(CONFIG_DIR_NAME)
            .join(AGENT_DIR_NAME)
            .join("reviewer");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join(AGENT_FILE_NAME),
            "---\nname: reviewer\ndescription: review code\ntools: [read, grep]\n---\n# Reviewer\nYou review code.",
        )
        .unwrap();

        let subagents = discover_subagents(&tmp).unwrap();
        let reviewer = subagents.iter().find(|s| s.name == "reviewer").unwrap();
        assert_eq!(reviewer.description, "review code");
        assert_eq!(
            reviewer.allowed_tools,
            Some(vec!["read".into(), "grep".into()])
        );
        assert_eq!(reviewer.system_prompt, "# Reviewer\nYou review code.");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn format_available_subagents_not_empty() {
        let configs = vec![builtin_general_subagent("test/model")];
        let formatted = format_available_subagents(&configs);
        assert!(formatted.contains("<available_subagents>"));
        assert!(formatted.contains("general"));
    }

    #[test]
    fn parse_task_specs_valid_array() {
        let args = serde_json::json!({
            "tasks": [
                {
                    "subagent": "general",
                    "description": "search todos",
                    "prompt": "find all TODO comments"
                },
                {
                    "subagent": "reviewer",
                    "prompt": "review src/lib.rs",
                    "task_id": "abc"
                }
            ]
        });
        let specs = parse_task_specs(&args).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].subagent, "general");
        assert_eq!(specs[0].description, "search todos");
        assert_eq!(specs[0].task_id, None);
        assert_eq!(specs[1].subagent, "reviewer");
        assert_eq!(specs[1].description, "");
        assert_eq!(specs[1].task_id.as_deref(), Some("abc"));
    }

    #[test]
    fn parse_task_specs_missing_array() {
        let args = serde_json::json!({"subagent": "general", "prompt": "p"});
        let err = parse_task_specs(&args).unwrap_err();
        assert!(err.contains("\"tasks\""));
    }

    #[test]
    fn parse_task_specs_empty_array() {
        let args = serde_json::json!({"tasks": []});
        assert!(parse_task_specs(&args).is_err());
    }

    #[test]
    fn parse_task_specs_item_missing_prompt() {
        let args = serde_json::json!({"tasks": [{"subagent": "general"}]});
        let err = parse_task_specs(&args).unwrap_err();
        assert!(err.contains("tasks[0]"));
    }

    #[test]
    fn parse_task_specs_blank_prompt() {
        let args = serde_json::json!({"tasks": [{"subagent": "general", "prompt": "   "}]});
        assert!(parse_task_specs(&args).is_err());
    }

    fn spec(subagent: &str, task_id: Option<&str>) -> TaskSpec {
        TaskSpec {
            subagent: subagent.to_string(),
            description: "d".to_string(),
            prompt: "p".to_string(),
            task_id: task_id.map(|s| s.to_string()),
        }
    }

    #[test]
    fn resolve_task_specs_ok() {
        let subs = vec![builtin_general_subagent("m")];
        let specs = vec![spec("general", None), spec("general", Some("id1"))];
        let configs = resolve_task_specs(&specs, &subs).unwrap();
        assert_eq!(configs.len(), 2);
        assert!(configs.iter().all(|c| c.name == "general"));
    }

    #[test]
    fn resolve_task_specs_unknown_subagent() {
        let subs = vec![builtin_general_subagent("m")];
        let specs = vec![spec("ghost", None)];
        let err = resolve_task_specs(&specs, &subs).unwrap_err();
        assert!(err.contains("not found"));
        assert!(err.contains("general"));
    }

    #[test]
    fn resolve_task_specs_duplicate_task_id() {
        let subs = vec![builtin_general_subagent("m")];
        let specs = vec![spec("general", Some("same")), spec("general", Some("same"))];
        let err = resolve_task_specs(&specs, &subs).unwrap_err();
        assert!(err.contains("duplicate task_id"));
    }

    #[test]
    fn resolve_task_specs_too_many() {
        let subs = vec![builtin_general_subagent("m")];
        let specs: Vec<TaskSpec> = (0..=MAX_PARALLEL_TASKS)
            .map(|_| spec("general", None))
            .collect();
        let err = resolve_task_specs(&specs, &subs).unwrap_err();
        assert!(err.contains("too many tasks"));
    }
}
