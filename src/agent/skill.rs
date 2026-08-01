use crate::agent::tools::{Tool, ToolExecuteError};
use crate::agent::utils;
use color_eyre::{Result, eyre::Context};
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

const CONFIG_DIR_NAME: &str = ".hailux";
const SKILL_DIR_NAME: &str = "skills";
const SKILL_FILE_NAME: &str = "SKILL.md";

/// 已发现的 skill 信息
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// SKILL.md 的绝对路径
    pub location: PathBuf,
    /// SKILL.md 去除 frontmatter 后的正文
    pub content: String,
}

impl SkillInfo {
    /// 该 skill 所在的目录（base directory），即 SKILL.md 的父目录
    pub fn base_dir(&self) -> PathBuf {
        self.location
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// 解析 SKILL.md 的 YAML frontmatter，提取 name 与 description。
///
/// 期望格式：
/// ```text
/// ---
/// name: my-skill
/// description: 描述何时使用此 skill
/// ---
/// [正文内容]
/// ```
///
/// 为避免引入 serde_yaml 依赖，这里采用轻量手写解析：
/// 仅识别首尾 `---` 分隔块，逐行匹配 `name:` 与 `description:`。
/// 返回 (name, description, content)；解析失败时返回 None。
fn parse_skill_md(raw: &str) -> Option<(String, String, String)> {
    let (frontmatter, content) = utils::split_frontmatter(raw)?;

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            if name.is_none() {
                name = Some(utils::strip_frontmatter_value(rest));
            }
        } else if let Some(rest) = line.strip_prefix("description:")
            && description.is_none()
        {
            description = Some(utils::strip_frontmatter_value(rest));
        }
    }

    let name = name.filter(|n| !n.is_empty())?;
    let description = description.unwrap_or_default();

    Some((name, description, content.to_string()))
}

/// 加载单个 SKILL.md 文件为 SkillInfo
fn load_skill(path: &Path) -> Result<Option<SkillInfo>> {
    let raw = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("Failed to read skill file: {}", path.display()))?;

    let (name, description, content) = match parse_skill_md(&raw) {
        Some(parsed) => parsed,
        None => {
            return Ok(None);
        }
    };

    let location = path
        .canonicalize()
        .wrap_err_with(|| format!("Cannot canonicalize path: {}", path.display()))?;

    Ok(Some(SkillInfo {
        name,
        description,
        location,
        content,
    }))
}

/// 在指定根目录下递归扫描 `**/SKILL.md`
fn scan_root(root: &Path, out: &mut Vec<SkillInfo>) -> Result<()> {
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
        let entry = entry.wrap_err("遍历 skill 目录失败")?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if entry.file_name() == SKILL_FILE_NAME {
            found_paths.push(entry.path().to_path_buf());
        }
    }

    for path in found_paths {
        match load_skill(&path) {
            Ok(Some(info)) => out.push(info),
            Ok(None) => {}
            Err(e) => {
                eprintln!("[warn] Skipping skill {}: {e}", path.display());
            }
        }
    }

    Ok(())
}

/// 发现所有可用 skill：全局 `~/.hailux/skills/` 与项目级 `<work_dir>/.hailux/skills/`。
/// 同名时项目级覆盖全局。
pub fn discover_skills(work_dir: &Path) -> Result<Vec<SkillInfo>> {
    let mut all: Vec<SkillInfo> = Vec::new();

    if let Some(home) = dirs::home_dir() {
        let global_root = home.join(CONFIG_DIR_NAME).join(SKILL_DIR_NAME);
        scan_root(&global_root, &mut all)?;
    }

    let project_root = work_dir.join(CONFIG_DIR_NAME).join(SKILL_DIR_NAME);
    if project_root.is_dir() {
        scan_root(&project_root, &mut all)?;
    }

    let mut by_name: HashMap<String, SkillInfo> = HashMap::new();
    for info in all {
        by_name.insert(info.name.clone(), info);
    }

    let mut result: Vec<SkillInfo> = by_name.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

/// 生成注入 system prompt 的 `<available_skills>` 块。
/// 仅包含 name + description（渐进式加载：不暴露正文）。
pub fn format_available_skills(skills: &[SkillInfo]) -> String {
    let described: Vec<&SkillInfo> = skills
        .iter()
        .filter(|s| !s.description.is_empty())
        .collect();

    if described.is_empty() {
        return String::new();
    }

    let mut out = String::from("<available_skills>\n");
    for skill in described {
        out.push_str("  <skill>\n");
        out.push_str(&format!("    <name>{}</name>\n", skill.name));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            skill.description
        ));
        out.push_str("  </skill>\n");
    }
    out.push_str("</available_skills>");
    out
}

/// 内置 help skill 的内容，首次初始化时写入 `~/.hailux/skills/help/SKILL.md`。
const DEFAULT_HELP_SKILL_MD: &str = crate::prompts::DEFAULT_HELP_SKILL_MD;

/// 首次初始化时将内置 help skill 写入 `~/.hailux/skills/help/SKILL.md`。
///
/// 仅在文件不存在时写入；已存在则跳过（不覆盖用户修改或删除）。
/// 失败时静默忽略，不阻断启动。
pub fn ensure_default_skills() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    let skill_dir = home.join(CONFIG_DIR_NAME).join(SKILL_DIR_NAME).join("help");
    let skill_file = skill_dir.join(SKILL_FILE_NAME);

    if skill_file.exists() {
        return;
    }

    if std::fs::create_dir_all(&skill_dir).is_err() {
        return;
    }

    let _ = std::fs::write(&skill_file, DEFAULT_HELP_SKILL_MD);
}

/// Skill 工具：渐进式加载命名的 skill 定义。
///
/// LLM 在 system prompt 中看到 `<available_skills>` 摘要后，调用本工具传入 `name`，
/// 即可获取该 skill 的完整指令正文与其所在目录的绝对路径；
/// skill 引用的同目录脚本/文件则由 LLM 随后用 `read`/`glob` 工具按需加载。
pub struct SkillTool {
    skills: Vec<SkillInfo>,
}

impl SkillTool {
    pub fn new(skills: Vec<SkillInfo>) -> Self {
        Self { skills }
    }

    fn find(&self, name: &str) -> Option<&SkillInfo> {
        self.skills.iter().find(|s| s.name == name)
    }
}

impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        crate::prompts::tools::SKILL
    }

    fn parameters(&self) -> Value {
        let names: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the skill to load; must match one listed in <available_skills>",
                    "enum": names
                }
            },
            "required": ["name"]
        })
    }

    fn execute_async<'a>(
        &'a self,
        arguments: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolExecuteError>> + Send + 'a>> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(arguments).unwrap_or_default();
            let name = match args["name"].as_str() {
                Some(n) => n,
                None => {
                    return Err(ToolExecuteError {
                        message: "name must not be empty".to_string(),
                    });
                }
            };

            let info = match self.find(name) {
                Some(info) => info,
                None => {
                    let available: Vec<&str> =
                        self.skills.iter().map(|s| s.name.as_str()).collect();
                    return Err(ToolExecuteError {
                        message: format!(
                            "Skill \"{}\" not found. Available skills: {}",
                            name,
                            if available.is_empty() {
                                "(none)".to_string()
                            } else {
                                available.join(", ")
                            }
                        ),
                    });
                }
            };

            let base = info.base_dir();
            let mut output = String::new();
            output.push_str(&format!("<skill_content name=\"{}\">\n", info.name));
            output.push_str(&format!("# Skill: {}\n\n", info.name));
            output.push_str(info.content.trim());
            output.push_str("\n\n");
            output.push_str(&format!(
                "Base directory for this skill: {}\n",
                base.display()
            ));
            output.push_str(
                "Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.\n",
            );
            output.push_str(
                "Use the `read` or `glob` tool to load any referenced scripts or files.\n",
            );
            output.push_str("</skill_content>");

            Ok(output)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_frontmatter() {
        let raw = "---\nname: my-skill\ndescription: a demo skill\n---\n# Body\nHello";
        let (name, desc, content) = parse_skill_md(raw).unwrap();
        assert_eq!(name, "my-skill");
        assert_eq!(desc, "a demo skill");
        assert_eq!(content, "# Body\nHello");
    }

    #[test]
    fn parses_quoted_description() {
        let raw = "---\nname: s\ndescription: \"with: colon\"\n---\nbody";
        let (name, desc, _content) = parse_skill_md(raw).unwrap();
        assert_eq!(name, "s");
        assert_eq!(desc, "with: colon");
    }

    #[test]
    fn rejects_missing_name() {
        let raw = "---\ndescription: no name here\n---\nbody";
        assert!(parse_skill_md(raw).is_none());
    }

    #[test]
    fn rejects_non_frontmatter() {
        assert!(parse_skill_md("# just a title\nbody").is_none());
    }

    #[test]
    fn discover_finds_project_level_skill() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!("hailux-skill-test-{}", uuid::Uuid::new_v4()));
        let skill_dir = tmp.join(CONFIG_DIR_NAME).join(SKILL_DIR_NAME).join("demo");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join(SKILL_FILE_NAME),
            "---\nname: demo\ndescription: a demo\n---\n# Demo\nbody",
        )
        .unwrap();

        let skills = discover_skills(&tmp).unwrap();
        let demo = skills.iter().find(|s| s.name == "demo").unwrap();
        assert_eq!(demo.description, "a demo");
        assert_eq!(demo.content, "# Demo\nbody");
        assert!(demo.base_dir().ends_with("demo"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn skill_tool_returns_content_and_base_dir() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!("hailux-skill-tool-{}", uuid::Uuid::new_v4()));
        let skill_dir = tmp.join(CONFIG_DIR_NAME).join(SKILL_DIR_NAME).join("wf");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join(SKILL_FILE_NAME),
            "---\nname: wf\ndescription: workflow\n---\nrun scripts/x.sh",
        )
        .unwrap();

        let skills = discover_skills(&tmp).unwrap();
        let tool = SkillTool::new(skills);
        let out = tool.execute_async(r#"{"name":"wf"}"#).await.unwrap();
        assert!(out.contains("Base directory for this skill:"));
        assert!(out.contains("run scripts/x.sh"));
        assert!(out.contains("<skill_content name=\"wf\">"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn default_help_skill_md_is_valid_frontmatter() {
        let (name, desc, content) = parse_skill_md(DEFAULT_HELP_SKILL_MD)
            .expect("DEFAULT_HELP_SKILL_MD must have valid frontmatter");
        assert_eq!(name, "help");
        assert!(!desc.is_empty());
        assert!(content.contains("hailux"));
        assert!(content.contains("MCP"));
        assert!(content.contains("SKILL.md"));
    }
}
