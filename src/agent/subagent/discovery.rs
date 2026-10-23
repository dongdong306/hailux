use std::collections::HashMap;
use std::path::{Path, PathBuf};

use color_eyre::Result;
use ignore::WalkBuilder;

use super::SubagentConfig;
use crate::agent::utils;

pub(super) const CONFIG_DIR_NAME: &str = ".hailux";
pub(super) const AGENT_DIR_NAME: &str = "agents";
pub(super) const AGENT_FILE_NAME: &str = "AGENTS.md";

/// AGENTS.md 解析结果
pub(super) struct AgentMdInfo {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) tools: Option<Vec<String>>,
    pub(super) skills: Option<Vec<String>>,
    pub(super) mcp: Option<Vec<String>>,
    pub(super) model: Option<String>,
    pub(super) system_prompt: String,
}

/// 解析 AGENTS.md 的 YAML frontmatter，提取 name、description、tools、model、skills、mcp。
pub(super) fn parse_agent_md(raw: &str) -> Option<AgentMdInfo> {
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);

    if !trimmed.starts_with("---") {
        let content = trimmed.trim();
        if content.is_empty() {
            return None;
        }
        return Some(AgentMdInfo {
            name: String::new(),
            description: String::new(),
            tools: None,
            skills: None,
            mcp: None,
            model: None,
            system_prompt: content.to_string(),
        });
    }

    let (frontmatter, content) = utils::split_frontmatter(trimmed)?;

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut tools: Option<Vec<String>> = None;
    let mut skills: Option<Vec<String>> = None;
    let mut mcp: Option<Vec<String>> = None;
    let mut model: Option<String> = None;

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            if name.is_none() {
                name = Some(utils::strip_frontmatter_value(rest));
            }
        } else if let Some(rest) = line.strip_prefix("description:") {
            if description.is_none() {
                description = Some(utils::strip_frontmatter_value(rest));
            }
        } else if let Some(rest) = line.strip_prefix("tools:") {
            if tools.is_none() {
                tools = parse_tools_list(rest);
            }
        } else if let Some(rest) = line.strip_prefix("skills:") {
            if skills.is_none() {
                skills = parse_tools_list(rest);
            }
        } else if let Some(rest) = line.strip_prefix("mcp:") {
            if mcp.is_none() {
                mcp = parse_tools_list(rest);
            }
        } else if let Some(rest) = line.strip_prefix("model:")
            && model.is_none()
        {
            model = Some(utils::strip_frontmatter_value(rest));
        }
    }

    let name = name.filter(|n| !n.is_empty())?;
    let description = description.unwrap_or_default();

    Some(AgentMdInfo {
        name,
        description,
        tools,
        skills,
        mcp,
        model,
        system_prompt: content.to_string(),
    })
}

fn parse_tools_list(raw: &str) -> Option<Vec<String>> {
    let v = raw.trim();
    if v.is_empty() {
        return None;
    }
    // 支持 [tool1, tool2] 格式
    let inner = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(v);
    let tools: Vec<String> = inner
        .split(',')
        .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if tools.is_empty() { None } else { Some(tools) }
}

fn load_subagent(path: &Path) -> Result<Option<SubagentConfig>> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        color_eyre::eyre::eyre!("Failed to read agent file {}: {e}", path.display())
    })?;

    let info = match parse_agent_md(&raw) {
        Some(parsed) => parsed,
        None => return Ok(None),
    };

    let location = path.canonicalize().map_err(|e| {
        color_eyre::eyre::eyre!("Cannot canonicalize path: {}: {e}", path.display())
    })?;

    Ok(Some(SubagentConfig {
        name: info.name,
        description: info.description,
        system_prompt: info.system_prompt,
        allowed_tools: info.tools,
        allowed_skills: info.skills,
        allowed_mcp_servers: info.mcp,
        model: info.model,
        source_path: location,
    }))
}

fn scan_root(root: &Path, out: &mut Vec<SubagentConfig>) -> Result<()> {
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
        let entry = entry
            .map_err(|e| color_eyre::eyre::eyre!("Failed to traverse agent directory: {e}"))?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if entry.file_name() == AGENT_FILE_NAME {
            found_paths.push(entry.path().to_path_buf());
        }
    }

    for path in found_paths {
        match load_subagent(&path) {
            Ok(Some(info)) => out.push(info),
            Ok(None) => {}
            Err(e) => {
                eprintln!("[warn] Skipping agent {}: {e}", path.display());
            }
        }
    }

    Ok(())
}

/// 发现所有可用 subagent：全局 `~/.hailux/agents/` 与项目级 `<work_dir>/.hailux/agents/`。
/// 同名时项目级覆盖全局。
pub fn discover_subagents(work_dir: &Path) -> Result<Vec<SubagentConfig>> {
    let mut all: Vec<SubagentConfig> = Vec::new();

    if let Some(home) = dirs::home_dir() {
        let global_root = home.join(CONFIG_DIR_NAME).join(AGENT_DIR_NAME);
        scan_root(&global_root, &mut all)?;
    }

    let project_root = work_dir.join(CONFIG_DIR_NAME).join(AGENT_DIR_NAME);
    if project_root.is_dir() {
        scan_root(&project_root, &mut all)?;
    }

    let mut by_name: HashMap<String, SubagentConfig> = HashMap::new();
    for info in all {
        by_name.insert(info.name.clone(), info);
    }

    let mut result: Vec<SubagentConfig> = by_name.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

/// 生成注入 system prompt 的 `<available_subagents>` 块。
pub fn format_available_subagents(subagents: &[SubagentConfig]) -> String {
    let described: Vec<&SubagentConfig> = subagents
        .iter()
        .filter(|s| !s.description.is_empty())
        .collect();

    if described.is_empty() {
        return String::new();
    }

    let mut out = String::from("<available_subagents>\n");
    for sa in described {
        out.push_str("  <subagent>\n");
        out.push_str(&format!("    <name>{}</name>\n", sa.name));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            sa.description
        ));
        out.push_str("  </subagent>\n");
    }
    out.push_str("</available_subagents>");
    out
}

/// 内置 general subagent 的 system prompt
const GENERAL_SUBAGENT_PROMPT: &str = crate::prompts::GENERAL_SUBAGENT;

/// 创建内置的 general subagent 配置
pub fn builtin_general_subagent(main_model: &str) -> SubagentConfig {
    let model = if main_model.is_empty() {
        "deepseek/deepseek-chat"
    } else {
        main_model
    };
    SubagentConfig {
        name: "general".to_string(),
        description: "General-purpose subagent for multi-step research and coding tasks. Suitable for complex work requiring multiple tool calls."
            .to_string(),
        system_prompt: GENERAL_SUBAGENT_PROMPT.to_string(),
        allowed_tools: None,
        allowed_skills: None,
        allowed_mcp_servers: None,
        model: Some(model.to_string()),
        source_path: PathBuf::new(),
    }
}
