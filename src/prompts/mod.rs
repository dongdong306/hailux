use std::path::{Path, PathBuf};

use crate::agent::agents_md;
use crate::agent::skill::{self, SkillInfo};
use crate::agent::subagent::{self, SubagentConfig};

pub const SYSTEM: &str = include_str!("system.txt");
pub const PLAN_MODE: &str = include_str!("plan_mode.txt");
pub const GENERAL_SUBAGENT: &str = include_str!("general_subagent.txt");
pub const TASK_TOOL_TEMPLATE: &str = include_str!("task_tool.txt");
pub const DEFAULT_HELP_SKILL_MD: &str = include_str!("default_help_skill.md");
pub const COMPACT: &str = include_str!("compact.txt");
pub const INIT: &str = include_str!("init.txt");

pub mod tools {
    pub const ASK_USER: &str = include_str!("tools/ask_user.txt");
    #[cfg(windows)]
    pub const BASH: &str = include_str!("tools/bash_windows.txt");
    #[cfg(not(windows))]
    pub const BASH: &str = include_str!("tools/bash_unix.txt");
    pub const READ: &str = include_str!("tools/read.txt");
    pub const EDIT: &str = include_str!("tools/edit.txt");
    pub const WRITE: &str = include_str!("tools/write.txt");
    pub const WEB_FETCH: &str = include_str!("tools/web_fetch.txt");
    pub const GREP: &str = include_str!("tools/grep.txt");
    pub const GLOB: &str = include_str!("tools/glob.txt");
    pub const TODO_WRITE: &str = include_str!("tools/todo_write.txt");
    pub const SKILL: &str = include_str!("tools/skill.txt");
}

pub fn build_system_prompt(
    work_dir: &Path,
    skills: &[SkillInfo],
    agent_md_entries: &[(PathBuf, String)],
    subagents: &[SubagentConfig],
) -> String {
    let mut prompt = SYSTEM.to_string();

    prompt.push_str(&format!(
        "\nCurrent working directory: {}",
        work_dir.display()
    ));

    if !skills.is_empty() {
        let available = skill::format_available_skills(skills);
        if !available.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&available);
            prompt.push_str(
                "\n\nLoad a specialized skill when the task at hand matches one of the skills listed above. \
                 Use the `skill` tool (passing the skill `name`) to load its full instructions and base directory, \
                 then use `read`/`glob` to load any referenced scripts or files.",
            );
        }
    }

    if let Some(md_prompt) = agents_md::format_agent_md_prompt(agent_md_entries) {
        prompt.push_str("\n\n# AGENTS.md Instructions\n\n");
        prompt.push_str(
            "The following are instructions defined in project or global AGENTS.md files (with file paths). Please follow them strictly:\n\n",
        );
        prompt.push_str(&md_prompt);
    }

    if !subagents.is_empty() {
        let available = subagent::format_available_subagents(subagents);
        if !available.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&available);
            prompt.push_str(
                "\n\nUse the `task` tool to delegate complex, multi-step work to a subagent listed above. \
                 Pass the subagent `name`, a short `description`, and a detailed `prompt` with all necessary context. \
                 The subagent runs autonomously in its own session and returns only the final result. \
                 You can also manually invoke a subagent by typing `@subagent: <name> <task>` in the input.",
            );
        }
    }

    prompt
}
