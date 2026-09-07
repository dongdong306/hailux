/// 遍历目录时始终排除的目录名（不依赖 .gitignore 机制）
pub const IGNORED_DIRS: &[&str] = &[".git"];

#[allow(clippy::module_inception)]
mod agent;
pub mod agents_md;
pub mod command_def;
pub mod event;
pub mod media;
pub mod models;
pub mod skill;
pub mod subagent;
mod tools;
mod utils;

pub use agent::Agent;
pub use agent::build_user_message;
pub use command_def::{CommandRegistry, parse_slash_input};
pub use event::{CoreEvent, CoreEventRx, CoreEventTx, create_core_event_channel};
pub use media::Attachment;
pub use skill::SkillTool;
#[allow(unused_imports)]
pub use subagent::{SubagentConfig, TaskTool};
#[cfg(windows)]
pub use tools::prewarm_shell_description;
pub use tools::{
    AskTool, BashTool, EditTool, GlobTool, GrepTool, ReadTool, TodoWriteTool, Tool,
    ToolExecuteError, WebFetchTool, WriteTool,
};
