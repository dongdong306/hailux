/// 遍历目录时始终排除的目录名（不依赖 .gitignore 机制）
pub const IGNORED_DIRS: &[&str] = &[".git"];

#[allow(clippy::module_inception)]
mod agent;
pub mod agents_md;
pub mod command_def;
pub mod models;
pub mod skill;
pub mod subagent;
mod tools;
mod utils;

pub use agent::Agent;
pub use command_def::CommandRegistry;
pub use skill::SkillTool;
#[allow(unused_imports)]
pub use subagent::{SubagentConfig, TaskTool};
pub use tools::{
    AskTool, BashTool, EditTool, GlobTool, GrepTool, ReadTool, TodoWriteTool, Tool,
    ToolExecuteError, WebFetchTool, WriteTool,
};
