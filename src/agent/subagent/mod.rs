use std::path::PathBuf;
use std::sync::{Arc, Mutex};

mod discovery;
mod runner;
mod task_tool;

#[cfg(test)]
mod tests;

pub use discovery::{builtin_general_subagent, discover_subagents, format_available_subagents};
pub use task_tool::TaskTool;

/// A discovered subagent configuration
#[derive(Debug, Clone)]
pub struct SubagentConfig {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    /// None = all builtin tools (except task and question); Some = only the whitelisted tools
    pub allowed_tools: Option<Vec<String>>,
    /// None = no skills loaded; Some = only the named skills are loaded
    pub allowed_skills: Option<Vec<String>>,
    /// None = no MCP tools loaded; Some = only tools of the named MCP servers
    pub allowed_mcp_servers: Option<Vec<String>>,
    pub model: Option<String>,
    #[allow(dead_code)]
    pub source_path: PathBuf,
}

/// Task tool: launches multiple subagents concurrently to run delegated
/// tasks.
///
/// Subagents run in isolated sessions; their execution is invisible to the
/// main TUI. Only the final result text is returned as the tool result.
pub type SharedConfig = Arc<Mutex<crate::config::Config>>;

/// Parses input of the form "@subagent: name work_content"
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
