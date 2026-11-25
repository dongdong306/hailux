use crate::agent::tools::{Tool, ToolExecuteError};
use crate::agent::utils;
use color_eyre::{Result, eyre::Context};
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

const CONFIG_DIR_NAME: &str = ".hailux";
const SKILL_DIR_NAME: &str = "skills";
const SKILL_FILE_NAME: &str = "SKILL.md";

/// Information about a discovered skill
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// Absolute path to SKILL.md
    pub location: PathBuf,
    /// SKILL.md body content with the frontmatter removed
    pub content: String,
}

impl SkillInfo {
    /// The directory containing this skill (base directory), i.e. the parent of SKILL.md
    pub fn base_dir(&self) -> PathBuf {
        self.location
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Parses the YAML frontmatter of SKILL.md, extracting name and description.
///
/// Expected format:
/// ```text
/// ---
/// name: my-skill
/// description: when to use this skill
/// ---
/// [body content]
/// ```
///
/// To avoid a serde_yaml dependency, a lightweight hand-written parser is used:
/// only the leading/trailing `---` delimiter block is recognized, matching `name:`
/// and `description:` line by line.
/// Returns (name, description, content); returns None when parsing fails.
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

/// Loads a single SKILL.md file into a SkillInfo
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

/// Recursively scans for `**/SKILL.md` under the given root directory
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

/// Discovers all available skills: global `~/.hailux/skills/` and project-level
/// `<work_dir>/.hailux/skills/`.
/// Project-level entries override globals on name collisions.
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

/// Generates the `<available_skills>` block for injection into the system prompt.
/// Contains only name + description (progressive loading: the body is not exposed).
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

/// Content of the builtin help skill, written to `~/.hailux/skills/help/SKILL.md`
/// on first initialization.
const DEFAULT_HELP_SKILL_MD: &str = crate::prompts::DEFAULT_HELP_SKILL_MD;

/// Writes the builtin help skill to `~/.hailux/skills/help/SKILL.md` on first initialization.
///
/// Writes only when the file does not exist; skips when it does
/// (never overwrites user edits or deletions).
/// Failures are silently ignored and do not block startup.
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

/// Maximum number of files shown in the file tree
const MAX_SKILL_FILE_LIST: usize = 15;

/// Recursively collects all files under `dir` (paths relative to `root`), skipping the
/// root-level SKILL.md (its body is already injected).
fn collect_relative_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_relative_files(root, &path, out);
        } else if file_type.is_file()
            && let Ok(rel) = path.strip_prefix(root)
            && rel != Path::new(SKILL_FILE_NAME)
        {
            out.push(rel.to_path_buf());
        }
    }
}

/// Selects at most `max` files from an ordered file list, first ensuring every subdirectory
/// (at any depth) shows at least one file (directories sorted by depth then path, shallower first);
/// remaining slots are filled in by (depth, path).
fn select_skill_files(files: &[PathBuf], max: usize) -> Vec<PathBuf> {
    if files.len() <= max {
        return files.to_vec();
    }
    let mut by_dir: HashMap<PathBuf, Vec<&PathBuf>> = HashMap::new();
    for f in files {
        let parent = f.parent().unwrap_or(Path::new("")).to_path_buf();
        by_dir.entry(parent).or_default().push(f);
    }
    let mut dirs: Vec<(PathBuf, Vec<&PathBuf>)> = by_dir.into_iter().collect();
    dirs.sort_by_key(|(d, _)| (d.components().count(), d.clone()));

    let mut selected: Vec<PathBuf> = Vec::new();
    for (_, group) in &dirs {
        if selected.len() >= max {
            break;
        }
        selected.push((*group[0]).clone());
    }
    let mut remaining: Vec<&PathBuf> = files.iter().collect();
    remaining.sort_by_key(|p| (p.components().count(), (*p).clone()));
    for f in remaining {
        if selected.len() >= max {
            break;
        }
        if !selected.contains(f) {
            selected.push(f.clone());
        }
    }
    selected.sort();
    selected
}

#[derive(Default)]
struct FileTreeNode {
    dirs: BTreeMap<String, FileTreeNode>,
    files: Vec<String>,
}

fn render_file_tree(node: &FileTreeNode, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for (name, child) in &node.dirs {
        out.push_str(&format!("{pad}{name}/\n"));
        render_file_tree(child, indent + 1, out);
    }
    let mut files = node.files.clone();
    files.sort();
    for name in &files {
        out.push_str(&format!("{pad}{name}\n"));
    }
}

/// Builds a tree listing of the other files in the skill directory (SKILL.md excluded).
/// Returns (tree text, number of hidden files); returns empty text when the directory is empty.
fn format_skill_file_tree(base_dir: &Path) -> (String, usize) {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_relative_files(base_dir, base_dir, &mut files);
    files.sort();
    if files.is_empty() {
        return (String::new(), 0);
    }
    let total = files.len();
    let selected = select_skill_files(&files, MAX_SKILL_FILE_LIST);

    let mut root = FileTreeNode::default();
    for p in &selected {
        let components: Vec<String> = p.iter().map(|c| c.to_string_lossy().into_owned()).collect();
        let mut node = &mut root;
        for (i, name) in components.iter().enumerate() {
            if i + 1 == components.len() {
                node.files.push(name.clone());
            } else {
                node = node.dirs.entry(name.clone()).or_default();
            }
        }
    }

    let mut tree = String::new();
    render_file_tree(&root, 0, &mut tree);
    (tree, total - selected.len())
}

/// Skill tool: progressively loads a named skill definition.
///
/// After seeing the `<available_skills>` summary in the system prompt, the LLM calls this
/// tool with a `name` to obtain the skill's full instruction body, the absolute path of its
/// directory, and a tree listing of the other files in that directory (at most 15 files,
/// with at least one shown per subdirectory); scripts/files referenced by the skill in the
/// same directory are then loaded on demand by the LLM via the `read`/`glob` tools.
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

    fn permission_category(&self) -> Option<&str> {
        Some("skill")
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
        _work_dir: &'a str,
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
            output.push_str("\n</skill_content>\n\n");
            output.push_str("<skill_context>\n");
            output.push_str(&format!(
                "Base directory for this skill: {}\n",
                base.display()
            ));
            output.push_str(
                "Relative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.\n",
            );
            let (tree, hidden) = format_skill_file_tree(&base);
            if !tree.is_empty() {
                output.push_str("\nFiles in this skill's directory:\n");
                output.push_str(&tree);
                if hidden > 0 {
                    output.push_str(&format!(
                        "... and {hidden} more files not shown; use `glob` on the base directory for the full list\n"
                    ));
                }
            }
            output.push_str(
                "Use the `read` or `glob` tool to load any referenced scripts or files.\n",
            );
            output.push_str("</skill_context>");

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
        let out = tool.execute_async(r#"{"name":"wf"}"#, ".").await.unwrap();
        assert!(out.contains("Base directory for this skill:"));
        assert!(out.contains("run scripts/x.sh"));
        assert!(out.contains("<skill_content name=\"wf\">"));
        assert!(out.contains("<skill_context>"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn skill_tool_lists_directory_tree() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!("hailux-skill-tree-{}", uuid::Uuid::new_v4()));
        let skill_dir = tmp.join(CONFIG_DIR_NAME).join(SKILL_DIR_NAME).join("tree");
        fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        fs::create_dir_all(skill_dir.join("reference").join("api")).unwrap();
        fs::write(
            skill_dir.join(SKILL_FILE_NAME),
            "---\nname: tree\ndescription: demo\n---\nbody",
        )
        .unwrap();
        fs::write(skill_dir.join("scripts").join("deploy.ps1"), "echo").unwrap();
        fs::write(
            skill_dir.join("reference").join("api").join("endpoints.md"),
            "# API",
        )
        .unwrap();
        fs::write(skill_dir.join("README.md"), "readme").unwrap();

        let skills = discover_skills(&tmp).unwrap();
        let tool = SkillTool::new(skills);
        let out = tool.execute_async(r#"{"name":"tree"}"#, ".").await.unwrap();
        assert!(out.contains("Files in this skill's directory:"));
        assert!(out.contains("scripts/\n  deploy.ps1"));
        assert!(out.contains("reference/\n  api/\n    endpoints.md"));
        assert!(out.contains("README.md"));
        // SKILL.md itself is not listed again
        assert!(!out.contains("SKILL.md\n"));
        // Auxiliary info sits inside <skill_context>: the body ends before the context block
        let content_end = out.find("</skill_content>").unwrap();
        let ctx_start = out.find("<skill_context>").unwrap();
        assert!(content_end < ctx_start);
        assert!(out.trim_end().ends_with("</skill_context>"));

        fs::remove_dir_all(&tmp).ok();
    }

    fn pb(parts: &[&str]) -> PathBuf {
        parts.iter().collect::<PathBuf>()
    }

    #[test]
    fn select_skill_files_caps_and_covers_each_dir() {
        // 13 root-level files + 3 subdirectory files = 16 > 15: when truncated, each
        // subdirectory still shows at least one
        let files: Vec<PathBuf> = vec![
            pb(&["a.md"]),
            pb(&["b.md"]),
            pb(&["c.md"]),
            pb(&["d.md"]),
            pb(&["e.md"]),
            pb(&["f.md"]),
            pb(&["g.md"]),
            pb(&["h.md"]),
            pb(&["i.md"]),
            pb(&["j.md"]),
            pb(&["k.md"]),
            pb(&["l.md"]),
            pb(&["m.md"]),
            pb(&["docs", "x.md"]),
            pb(&["docs", "y.md"]),
            pb(&["scripts", "run.sh"]),
        ];
        let selected = select_skill_files(&files, MAX_SKILL_FILE_LIST);
        assert_eq!(selected.len(), MAX_SKILL_FILE_LIST);
        assert!(selected.contains(&pb(&["docs", "x.md"])));
        assert!(selected.contains(&pb(&["scripts", "run.sh"])));
        // Result stays sorted
        let mut sorted = selected.clone();
        sorted.sort();
        assert_eq!(selected, sorted);
    }

    #[test]
    fn select_skill_files_small_list_passthrough() {
        let files = vec![pb(&["a.md"]), pb(&["scripts", "x.sh"])];
        assert_eq!(select_skill_files(&files, MAX_SKILL_FILE_LIST), files);
    }

    #[test]
    fn format_skill_file_tree_reports_hidden_count() {
        use std::fs;
        let tmp =
            std::env::temp_dir().join(format!("hailux-skill-hidden-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(tmp.join("docs")).unwrap();
        for i in 0..17 {
            fs::write(tmp.join(format!("f{i}.md")), "x").unwrap();
        }
        fs::write(tmp.join("docs").join("d.md"), "x").unwrap();

        let (tree, hidden) = format_skill_file_tree(&tmp);
        let lines: Vec<&str> = tree.lines().collect();
        assert_eq!(lines.len(), MAX_SKILL_FILE_LIST + 1); // 15 files + docs/ directory line
        assert!(lines.contains(&"docs/"));
        assert_eq!(hidden, 3); // 18 files - 15 shown

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
