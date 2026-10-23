use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use color_eyre::Result;
use futures_util::future::join_all;
use serde_json::{Value, json};

use super::SharedConfig;
use super::SubagentConfig;
use super::runner::{SharedTaskCtx, run_subagent_task};
use crate::agent::skill::SkillInfo;
use crate::agent::tools::{Tool, ToolExecuteError};
use crate::mcp::SharedMcpBackends;
use crate::storage::ChatStorage;

/// 单次 task 调用允许并发启动的 subagent 数量上限
pub(super) const MAX_PARALLEL_TASKS: usize = 8;

/// tasks 数组中单个任务的解析结果
#[derive(Debug, Clone)]
pub(super) struct TaskSpec {
    pub(super) subagent: String,
    pub(super) description: String,
    pub(super) prompt: String,
    pub(super) task_id: Option<String>,
}

/// 解析 tool 参数中的 `tasks` 数组
pub(super) fn parse_task_specs(args: &Value) -> Result<Vec<TaskSpec>, String> {
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
pub(super) fn resolve_task_specs<'a>(
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
        _work_dir: &'a str,
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
