use crate::agent::CommandRegistry;

pub use crate::agent::parse_slash_input;

/// Static info of a UI-action command (for displaying built-in UI commands only).
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

/// UI-action command (triggers a UI action, never sends a message to the LLM).
pub enum Command {
    New,
    Session,
    Plan,
    Models,
    Compact,
    Effort,
    Skills,
    Mcp,
    Tasks,
    Stats,
    Share,
    Yolo,
    Exit,
}

/// Command entry for unified display (generated from both UI and prompt commands).
#[derive(Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub is_ui: bool,
}

/// Match result: UI-action command vs prompt command.
pub enum MatchedCommand {
    Ui { cmd: Command, args: String },
    Prompt { name: String, args: String },
}

static SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "new",
        description: "新建会话",
    },
    SlashCommand {
        name: "sessions",
        description: "打开会话选择器",
    },
    SlashCommand {
        name: "plan",
        description: "切换规划模式（只读）",
    },
    SlashCommand {
        name: "models",
        description: "切换模型",
    },
    SlashCommand {
        name: "compact",
        description: "压缩上下文（总结历史对话）",
    },
    SlashCommand {
        name: "effort",
        description: "设置思考强度（菜单选择；/effort low|high|max 直接设置）",
    },
    SlashCommand {
        name: "skills",
        description: "查看已加载的 skill",
    },
    SlashCommand {
        name: "mcp",
        description: "查看 MCP 服务器",
    },
    SlashCommand {
        name: "tasks",
        description: "查看子代理执行情况",
    },
    SlashCommand {
        name: "stats",
        description: "查看 token 用量统计",
    },
    SlashCommand {
        name: "share",
        description: "导出当前会话为 HTML 分享文件",
    },
    SlashCommand {
        name: "yolo",
        description: "切换 YOLO 模式（跳过权限确认）",
    },
    SlashCommand {
        name: "exit",
        description: "退出程序",
    },
];

fn match_ui_command(name: &str) -> Option<Command> {
    match name {
        "new" => Some(Command::New),
        "sessions" => Some(Command::Session),
        "plan" => Some(Command::Plan),
        "models" => Some(Command::Models),
        "compact" => Some(Command::Compact),
        "effort" => Some(Command::Effort),
        "skills" => Some(Command::Skills),
        "mcp" => Some(Command::Mcp),
        "tasks" => Some(Command::Tasks),
        "stats" => Some(Command::Stats),
        "share" => Some(Command::Share),
        "yolo" => Some(Command::Yolo),
        "exit" | "quit" | "q" => Some(Command::Exit),
        _ => None,
    }
}

/// Try to match user input as a command (checks UI-action commands first, then prompt commands).
pub fn match_command(input: &str, registry: &CommandRegistry) -> Option<MatchedCommand> {
    let (name, args) = parse_slash_input(input)?;

    if let Some(cmd) = match_ui_command(&name) {
        return Some(MatchedCommand::Ui { cmd, args });
    }

    if registry.find(&name).is_some() {
        return Some(MatchedCommand::Prompt { name, args });
    }

    None
}

/// Unified display priority for UI commands and built-in prompt commands.
/// Custom commands not in this list are appended at the end in alphabetical order.
const COMMAND_PRIORITY: &[&str] = &[
    "new", "sessions", "init", "plan", "models", "compact", "effort", "skills", "mcp", "tasks",
    "stats", "share", "yolo", "exit",
];

/// Build the display list of all commands (UI + prompt commands, mixed, sorted by priority).
pub fn build_all_entries(registry: &CommandRegistry) -> Vec<CommandEntry> {
    let mut entries: Vec<CommandEntry> = SLASH_COMMANDS
        .iter()
        .map(|cmd| CommandEntry {
            name: cmd.name.to_string(),
            description: cmd.description.to_string(),
            is_ui: true,
        })
        .collect();

    for (name, desc) in registry.list() {
        if !SLASH_COMMANDS.iter().any(|c| c.name == name) {
            entries.push(CommandEntry {
                name: name.to_string(),
                description: desc.to_string(),
                is_ui: false,
            });
        }
    }

    entries.sort_by_key(|e| {
        COMMAND_PRIORITY
            .iter()
            .position(|p| *p == e.name)
            .unwrap_or(usize::MAX)
    });

    entries
}

/// Filter the command list by prefix, returning the matched indices.
pub fn filter_completions(entries: &[CommandEntry], prefix: &str) -> Vec<usize> {
    if prefix.is_empty() {
        return (0..entries.len()).collect();
    }
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.name.starts_with(prefix))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_registry() -> CommandRegistry {
        CommandRegistry::default()
    }

    #[test]
    fn parse_slash_no_args() {
        let (name, args) = parse_slash_input("/exit").unwrap();
        assert_eq!(name, "exit");
        assert_eq!(args, "");
    }

    #[test]
    fn parse_slash_with_args() {
        let (name, args) = parse_slash_input("/review fix bug").unwrap();
        assert_eq!(name, "review");
        assert_eq!(args, "fix bug");
    }

    #[test]
    fn parse_slash_not_command() {
        assert!(parse_slash_input("hello").is_none());
    }

    #[test]
    fn match_ui_command_exit() {
        let reg = empty_registry();
        match match_command("/exit", &reg).unwrap() {
            MatchedCommand::Ui {
                cmd: Command::Exit, ..
            } => {}
            _ => panic!("expected Exit"),
        }
    }

    #[test]
    fn match_compact_command() {
        let reg = empty_registry();
        match match_command("/compact", &reg).unwrap() {
            MatchedCommand::Ui {
                cmd: Command::Compact,
                ..
            } => {}
            _ => panic!("expected Compact"),
        }
    }

    #[test]
    fn effort_command_carries_args() {
        let reg = empty_registry();
        match match_command("/effort low", &reg).unwrap() {
            MatchedCommand::Ui {
                cmd: Command::Effort,
                args,
            } => assert_eq!(args, "low"),
            _ => panic!("expected Effort"),
        }
    }

    #[test]
    fn match_share_command() {
        let reg = empty_registry();
        match match_command("/share", &reg).unwrap() {
            MatchedCommand::Ui {
                cmd: Command::Share,
                ..
            } => {}
            _ => panic!("expected Share"),
        }
    }

    #[test]
    fn share_in_command_entries() {
        let reg = empty_registry();
        let entries = build_all_entries(&reg);
        assert!(entries.iter().any(|e| e.name == "share"));
    }

    #[test]
    fn compact_in_command_entries() {
        let reg = empty_registry();
        let entries = build_all_entries(&reg);
        assert!(entries.iter().any(|e| e.name == "compact"));
    }

    #[test]
    fn match_ui_command_aliases() {
        let reg = empty_registry();
        assert!(matches!(
            match_command("/quit", &reg),
            Some(MatchedCommand::Ui {
                cmd: Command::Exit,
                ..
            })
        ));
        assert!(matches!(
            match_command("/q", &reg),
            Some(MatchedCommand::Ui {
                cmd: Command::Exit,
                ..
            })
        ));
    }

    #[test]
    fn match_unknown_returns_none() {
        let reg = empty_registry();
        assert!(match_command("/nonexistent", &reg).is_none());
    }

    #[test]
    fn build_entries_includes_ui_commands() {
        let reg = empty_registry();
        let entries = build_all_entries(&reg);
        assert!(entries.iter().any(|e| e.name == "sessions"));
        assert!(entries.iter().any(|e| e.name == "exit"));
    }

    #[test]
    fn filter_completions_empty_prefix() {
        let reg = empty_registry();
        let entries = build_all_entries(&reg);
        let result = filter_completions(&entries, "");
        assert_eq!(result.len(), entries.len());
    }

    #[test]
    fn filter_completions_partial() {
        let reg = empty_registry();
        let entries = build_all_entries(&reg);
        let result = filter_completions(&entries, "s");
        assert!(result.iter().any(|&i| entries[i].name == "sessions"));
        assert!(result.iter().any(|&i| entries[i].name == "skills"));
    }
}
