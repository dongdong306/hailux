use std::path::{Path, PathBuf};

const CONFIG_DIR_NAME: &str = ".hailux";
const AGENT_MD_FILE_NAME: &str = "AGENTS.md";

/// Finds AGENTS.md in the global directory, the working directory, and ancestor
/// directories (walking upward), returning all found (absolute path, content) pairs
/// ordered from lowest to highest priority.
///
/// Return order:
/// 1. `~/.hailux/AGENTS.md` (global, lowest priority)
/// 2. The `AGENTS.md` in the farthest ancestor directory
/// 3. … down level by level
/// 4. `<work_dir>/AGENTS.md` (highest priority)
///
/// Note: callers concatenate the returned results in their original order when
/// injecting into the system prompt; the later an entry appears in the prompt,
/// the stronger it constrains the LLM, so the working directory's AGENTS.md
/// carries the highest effective priority.
pub fn discover_agent_md(work_dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found: Vec<(PathBuf, String)> = Vec::new();

    // 1. Global ~/.hailux/AGENTS.md (case-insensitive)
    if let Some(home) = dirs::home_dir() {
        let global_dir = home.join(CONFIG_DIR_NAME);
        if let Some(path) = find_agent_md_in_dir(&global_dir)
            && let Some(content) = try_read_md(&path)
        {
            found.push((path, content));
        }
    }

    // 2. AGENTS.md in the working directory and ancestor directories
    found.extend(discover_local_agent_md(work_dir));

    found
}

/// Walks upward from work_dir (at most 3 levels), collecting ancestor directories' AGENTS.md.
/// Results are ordered from lowest to highest priority (farthest ancestor first, work_dir last).
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
    // ancestors is ordered nearest-to-farthest (work_dir first), so it needs reversing
    ancestors.reverse();
    ancestors
}

/// Formats the AGENTS.md list into text for injection into the system prompt (paths and contents).
/// Returns `None` when no files were found.
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

/// Finds AGENTS.md in the given directory (case-insensitive), returning the first
/// matching path found.
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
