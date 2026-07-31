use crate::agent::utils;
use color_eyre::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CONFIG_DIR_NAME: &str = ".hailux";
const COMMAND_DIR_NAME: &str = "commands";

/// 内建 `/init` 命令名（app.rs 中 plan 模式守卫会引用）。
pub const INIT_COMMAND_NAME: &str = "init";

/// 公共 trait：所有提示词命令（内建与自定义）的统一抽象。
pub trait PromptCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn render(&self, args: &str) -> String;
}

/// 内建提示词命令：模板存储在代码中。
pub struct BuiltinPromptCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub template: String,
}

impl PromptCommand for BuiltinPromptCommand {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn render(&self, args: &str) -> String {
        self.template.replace("$ARGUMENTS", args)
    }
}

/// 自定义命令：从 `.md` 文件加载。
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub template: String,
    #[allow(dead_code)]
    pub source_path: PathBuf,
}

impl PromptCommand for CustomCommand {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn render(&self, args: &str) -> String {
        self.template.replace("$ARGUMENTS", args)
    }
}

/// 解析命令 `.md` 文件的 YAML frontmatter，提取 description。
/// 复用 `utils::split_frontmatter` / `utils::strip_frontmatter_value`，
/// 避免与 skill.rs、subagent.rs 中的解析逻辑重复。
///
/// 期望格式：
/// ```text
/// ---
/// description: 描述此命令的作用
/// ---
/// [模板正文，可包含 $ARGUMENTS]
/// ```
fn parse_command_md(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);

    if !trimmed.starts_with("---") {
        let content = trimmed.trim();
        if content.is_empty() {
            return None;
        }
        return Some((String::new(), content.to_string()));
    }

    let (frontmatter, content) = utils::split_frontmatter(raw)?;

    let mut description: Option<String> = None;
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("description:")
            && description.is_none()
        {
            description = Some(utils::strip_frontmatter_value(rest));
        }
    }

    Some((description.unwrap_or_default(), content.to_string()))
}

/// 从指定目录扫描 `*.md` 文件，返回 CustomCommand 列表。
fn scan_dir(root: &Path) -> Result<Vec<CustomCommand>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(root)? {
        let entries = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entries.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[warn] 跳过 command 文件 {}: {e}", path.display());
                continue;
            }
        };
        let (description, template) = match parse_command_md(&raw) {
            Some(parsed) => parsed,
            None => continue,
        };
        out.push(CustomCommand {
            name,
            description,
            template,
            source_path: path,
        });
    }
    Ok(out)
}

/// 统一命令注册表：管理所有提示词命令（内建 + 自定义）。
#[derive(Default)]
pub struct CommandRegistry {
    commands: Vec<Box<dyn PromptCommand>>,
}

impl CommandRegistry {
    /// 从内建 prompt 命令和文件系统（全局 + 项目级）发现所有命令。
    /// 优先级：项目级 > 全局 > 内建。
    pub fn discover(work_dir: &Path) -> Result<Self> {
        let home = dirs::home_dir();
        Self::discover_from(work_dir, home.as_deref())
    }

    /// `discover_from` 接受显式的 home 路径，便于测试注入。
    fn discover_from(work_dir: &Path, home: Option<&Path>) -> Result<Self> {
        let builtins: Vec<BuiltinPromptCommand> = builtin_prompt_commands(work_dir);
        let mut by_name: HashMap<String, Box<dyn PromptCommand>> = HashMap::new();

        for cmd in builtins {
            by_name.insert(cmd.name.to_string(), Box::new(cmd));
        }

        if let Some(home) = home {
            let global_root = home.join(CONFIG_DIR_NAME).join(COMMAND_DIR_NAME);
            for cmd in scan_dir(&global_root)? {
                by_name.insert(cmd.name.clone(), Box::new(cmd));
            }
        }

        let project_root = work_dir.join(CONFIG_DIR_NAME).join(COMMAND_DIR_NAME);
        for cmd in scan_dir(&project_root)? {
            by_name.insert(cmd.name.clone(), Box::new(cmd));
        }

        let mut commands: Vec<Box<dyn PromptCommand>> = by_name.into_values().collect();
        commands.sort_by(|a, b| a.name().cmp(b.name()));

        Ok(Self { commands })
    }

    pub fn find(&self, name: &str) -> Option<&dyn PromptCommand> {
        self.commands
            .iter()
            .find(|c| c.name() == name)
            .map(|c| c.as_ref())
    }

    pub fn list(&self) -> Vec<(&str, &str)> {
        self.commands
            .iter()
            .map(|c| (c.name(), c.description()))
            .collect()
    }
}

/// 内建 prompt 命令列表。`init` 模板中会注入工作目录路径，
/// 让 LLM 知道 AGENTS.md 应写入/更新的位置。
fn builtin_prompt_commands(work_dir: &Path) -> Vec<BuiltinPromptCommand> {
    let init_template = crate::prompts::INIT.replace("{path}", &work_dir.display().to_string());
    vec![BuiltinPromptCommand {
        name: INIT_COMMAND_NAME,
        description: "生成 AGENTS.md 总结当前项目",
        template: init_template,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let raw = "---\ndescription: review code\n---\nPlease review:\n$ARGUMENTS";
        let (desc, body) = parse_command_md(raw).unwrap();
        assert_eq!(desc, "review code");
        assert_eq!(body, "Please review:\n$ARGUMENTS");
    }

    #[test]
    fn parses_without_frontmatter() {
        let raw = "Just a template with $ARGUMENTS";
        let (desc, body) = parse_command_md(raw).unwrap();
        assert_eq!(desc, "");
        assert_eq!(body, "Just a template with $ARGUMENTS");
    }

    #[test]
    fn builtin_renders_arguments() {
        let cmd = BuiltinPromptCommand {
            name: "test",
            description: "test desc",
            template: "Do: $ARGUMENTS".to_string(),
        };
        assert_eq!(cmd.render("hello world"), "Do: hello world");
    }

    #[test]
    fn builtin_renders_no_placeholder() {
        let cmd = BuiltinPromptCommand {
            name: "test",
            description: "test desc",
            template: "Static content".to_string(),
        };
        assert_eq!(cmd.render("ignored"), "Static content");
    }

    #[test]
    fn discover_includes_builtin_init() {
        let tmp =
            std::env::temp_dir().join(format!("hailux-command-init-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let registry = CommandRegistry::discover(&tmp).unwrap();
        let cmd = registry.find(INIT_COMMAND_NAME).expect("init command");
        assert_eq!(cmd.description(), "生成 AGENTS.md 总结当前项目");
        assert!(cmd.render("").contains(&tmp.display().to_string()));
        assert!(cmd.render("只生成精简版").contains("只生成精简版"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn project_command_overrides_builtin_init() {
        let tmp =
            std::env::temp_dir().join(format!("hailux-command-init-test-{}", uuid::Uuid::new_v4()));
        let cmd_dir = tmp.join(CONFIG_DIR_NAME).join(COMMAND_DIR_NAME);
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(
            cmd_dir.join("init.md"),
            "---\ndescription: project init\n---\nProject: $ARGUMENTS",
        )
        .unwrap();

        let registry = CommandRegistry::discover(&tmp).unwrap();
        let cmd = registry.find(INIT_COMMAND_NAME).unwrap();
        assert_eq!(cmd.description(), "project init");
        assert_eq!(cmd.render("x"), "Project: x");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn custom_command_render() {
        let cmd = CustomCommand {
            name: "review".to_string(),
            description: "review".to_string(),
            template: "Review this:\n$ARGUMENTS".to_string(),
            source_path: PathBuf::from("/tmp/review.md"),
        };
        assert_eq!(cmd.render("src/main.rs"), "Review this:\nsrc/main.rs");
    }

    #[test]
    fn discover_finds_project_level_command() {
        let tmp =
            std::env::temp_dir().join(format!("hailux-command-test-{}", uuid::Uuid::new_v4()));
        let cmd_dir = tmp.join(CONFIG_DIR_NAME).join(COMMAND_DIR_NAME);
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(
            cmd_dir.join("review.md"),
            "---\ndescription: code review\n---\nReview:\n$ARGUMENTS",
        )
        .unwrap();

        let registry = CommandRegistry::discover(&tmp).unwrap();
        assert!(registry.find("review").is_some());
        assert_eq!(
            registry.find("review").unwrap().description(),
            "code review"
        );
        assert_eq!(
            registry.find("review").unwrap().render("main.rs"),
            "Review:\nmain.rs"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn project_overrides_global() {
        let tmp = std::env::temp_dir().join(format!(
            "hailux-command-override-test-{}",
            uuid::Uuid::new_v4()
        ));
        let global_dir = tmp
            .join("global")
            .join(CONFIG_DIR_NAME)
            .join(COMMAND_DIR_NAME);
        let project_dir = tmp
            .join("project")
            .join(CONFIG_DIR_NAME)
            .join(COMMAND_DIR_NAME);
        std::fs::create_dir_all(&global_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        std::fs::write(
            global_dir.join("test.md"),
            "---\ndescription: global\n---\nGlobal: $ARGUMENTS",
        )
        .unwrap();
        std::fs::write(
            project_dir.join("test.md"),
            "---\ndescription: project\n---\nProject: $ARGUMENTS",
        )
        .unwrap();

        let registry =
            CommandRegistry::discover_from(&tmp.join("project"), Some(&tmp.join("global")))
                .unwrap();
        let cmd = registry.find("test").unwrap();
        assert_eq!(cmd.description(), "project");
        assert_eq!(cmd.render("x"), "Project: x");

        std::fs::remove_dir_all(&tmp).ok();
    }
}
