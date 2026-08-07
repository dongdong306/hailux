use std::path::{Path, PathBuf};

const CONFIG_DIR_NAME: &str = ".hailux";
const AGENT_MD_FILE_NAME: &str = "AGENTS.md";

/// 从全局及工作目录（含向上遍历的祖先目录）查找 AGENTS.md，
/// 返回所有找到的 (绝对路径, 文件内容)，按优先级从低到高排列。
///
/// 返回顺序：
/// 1. `~/.hailux/AGENTS.md`（全局，优先级最低）
/// 2. 最远祖先目录的 `AGENTS.md`
/// 3. … 逐级向下
/// 4. `<work_dir>/AGENTS.md`（最高优先级）
///
/// 注：调用方将返回结果按原顺序拼接注入 system prompt，
/// 越靠后的内容在 prompt 中位置越后，对 LLM 的约束力越强，
/// 因此工作目录的 AGENTS.md 具有最高实际优先级。
pub fn discover_agent_md(work_dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(PathBuf, String)> = Vec::new();

    // 1. 全局 ~/.hailux/AGENTS.md（忽略大小写）
    if let Some(home) = dirs::home_dir() {
        let global_dir = home.join(CONFIG_DIR_NAME);
        if let Some(path) = find_agent_md_in_dir(&global_dir)
            && let Some(content) = try_read_md(&path)
        {
            found.push((path, content));
        }
    }

    // 2. 工作目录及祖先目录的 AGENTS.md
    found.extend(discover_local_agent_md(work_dir));

    found
}

/// 从 work_dir 向上遍历（最多3级），收集祖先目录的 AGENTS.md。
/// 返回结果按优先级从低到高排列（最远祖先在前，work_dir 在最后）。
fn discover_local_agent_md(work_dir: &Path) -> Vec<(PathBuf, String)> {
    let mut ancestors: Vec<(PathBuf, String)> = Vec::new();
    let mut current = Some(work_dir);
    let mut depth = 0;
    while let Some(dir) = current {
        if depth >= 3 {
            break;
        }
        if let Some(path) = find_agent_md_in_dir(dir)
            && let Some(content) = try_read_md(&path)
        {
            ancestors.push((path, content));
        }
        current = dir.parent();
        depth += 1;
    }
    // ancestors 的顺序是从近到远（work_dir 在前），需要反转
    ancestors.reverse();
    ancestors
}

/// 将 AGENTS.md 列表格式化为注入 system prompt 的文本（包含路径与内容）。
/// 返回 `None` 表示没有找到任何文件。
pub fn format_agent_md_prompt(entries: &[(PathBuf, String)]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }

    let mut out = String::new();
    for (path, content) in entries {
        out.push_str(&format!("## {} \n\n{}\n\n", path.display(), content.trim()));
    }

    Some(out)
}

/// 在指定目录下查找 AGENTS.md（忽略大小写），返回找到的第一个匹配路径。
fn find_agent_md_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        if name_str.eq_ignore_ascii_case(AGENT_MD_FILE_NAME) {
            let path = entry.path();
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn try_read_md(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        Ok(_) => None,
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_empty_when_no_file() {
        let tmp =
            std::env::temp_dir().join(format!("hailux-agent-md-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let result = discover_local_agent_md(&tmp);
        assert!(result.is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn finds_work_dir_agent_md() {
        let tmp =
            std::env::temp_dir().join(format!("hailux-agent-md-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(AGENT_MD_FILE_NAME), "# Rules\nAlways use tabs").unwrap();

        let result = discover_local_agent_md(&tmp);
        assert_eq!(result.len(), 1);
        assert!(result[0].0.ends_with(AGENT_MD_FILE_NAME));
        assert!(result[0].1.contains("Always use tabs"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn finds_multiple_ancestor_files() {
        let root =
            std::env::temp_dir().join(format!("hailux-agent-md-test-{}", uuid::Uuid::new_v4()));
        let child = root.join("sub").join("deep");
        std::fs::create_dir_all(&child).unwrap();

        let root_md = root.join(AGENT_MD_FILE_NAME);
        let child_md = child.join(AGENT_MD_FILE_NAME);
        std::fs::write(&root_md, "# Root\nRoot rule").unwrap();
        std::fs::write(&child_md, "# Child\nChild rule").unwrap();

        let result = discover_local_agent_md(&child);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, root_md);
        assert_eq!(result[1].0, child_md);
        assert!(result[0].1.contains("Root rule"));
        assert!(result[1].1.contains("Child rule"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn format_prompt_includes_path_and_content() {
        let entries = vec![
            (
                PathBuf::from("/home/user/.hailux/AGENTS.md"),
                "Global rule".to_string(),
            ),
            (
                PathBuf::from("/project/AGENTS.md"),
                "Project rule".to_string(),
            ),
        ];
        let prompt = format_agent_md_prompt(&entries).unwrap();
        assert!(prompt.contains("/home/user/.hailux/AGENTS.md"));
        assert!(prompt.contains("Global rule"));
        assert!(prompt.contains("/project/AGENTS.md"));
        assert!(prompt.contains("Project rule"));
    }

    #[test]
    fn format_prompt_returns_none_for_empty() {
        assert!(format_agent_md_prompt(&[]).is_none());
    }

    #[test]
    fn finds_lowercase_agent_md() {
        let tmp =
            std::env::temp_dir().join(format!("hailux-agent-md-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("agents.md"), "# Lowercase\nlower rule").unwrap();

        let result = discover_local_agent_md(&tmp);
        assert_eq!(result.len(), 1);
        assert!(result[0].1.contains("lower rule"));

        std::fs::remove_dir_all(&tmp).ok();
    }
}
