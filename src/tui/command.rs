use crate::agent::CommandRegistry;

pub use crate::agent::parse_slash_input;

/// UI-action 命令的静态信息（仅用于内建 UI 命令的展示）。
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

/// UI-action 命令（触发界面操作，不发送消息给 LLM）。
pub enum Command {
    New,
    Session,
    Plan,
    Models,
    Compact,
    Skills,
    Mcp,
    Tasks,
    Yolo,
    Exit,
}

/// 统一展示用的命令条目（UI 命令 + prompt 命令均可生成）。
#[derive(Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub is_ui: bool,
}

/// 匹配结果：UI-action 命令 vs prompt 命令。
pub enum MatchedCommand {
    Ui(Command),
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
        "skills" => Some(Command::Skills),
        "mcp" => Some(Command::Mcp),
        "tasks" => Some(Command::Tasks),
        "yolo" => Some(Command::Yolo),
        "exit" | "quit" | "q" => Some(Command::Exit),
        _ => None,
    }
}

/// 尝试将用户输入匹配为命令（先查 UI-action 命令，再查 prompt 命令）。
pub fn match_command(input: &str, registry: &CommandRegistry) -> Option<MatchedCommand> {
    let (name, args) = parse_slash_input(input)?;

    if let Some(cmd) = match_ui_command(&name) {
        return Some(MatchedCommand::Ui(cmd));
    }

    if registry.find(&name).is_some() {
        return Some(MatchedCommand::Prompt { name, args });
    }

    None
}

/// UI 命令与内建 prompt 命令的统一展示优先级。
/// 不在此列表中的自定义命令按字母排序追加在末尾。
const COMMAND_PRIORITY: &[&str] = &[
    "new", "sessions", "init", "plan", "models", "compact", "skills", "mcp", "tasks", "yolo",
    "exit",
];

/// 构建所有可用命令的展示列表（UI 命令 + prompt 命令按优先级混合排序）。
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

/// 根据前缀过滤命令列表，返回匹配的索引列表。
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
            MatchedCommand::Ui(Command::Exit) => {}
            _ => panic!("expected Exit"),
        }
    }

    #[test]
    fn match_compact_command() {
        let reg = empty_registry();
        match match_command("/compact", &reg).unwrap() {
            MatchedCommand::Ui(Command::Compact) => {}
            _ => panic!("expected Compact"),
        }
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
            Some(MatchedCommand::Ui(Command::Exit))
        ));
        assert!(matches!(
            match_command("/q", &reg),
            Some(MatchedCommand::Ui(Command::Exit))
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
