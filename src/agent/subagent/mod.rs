use std::path::PathBuf;
use std::sync::{Arc, Mutex};

mod discovery;
mod runner;
mod task_tool;

#[cfg(test)]
mod tests;

pub use discovery::{builtin_general_subagent, discover_subagents, format_available_subagents};
pub use task_tool::TaskTool;

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

/// Task 工具：并发启动多个 subagent 执行委派任务。
///
/// subagent 在独立 session 中运行，执行过程对主 TUI 不可见。
/// 仅返回最终结果文本作为工具结果。
pub type SharedConfig = Arc<Mutex<crate::config::Config>>;

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
