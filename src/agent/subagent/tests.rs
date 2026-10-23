use super::discovery::{
    AGENT_DIR_NAME, AGENT_FILE_NAME, CONFIG_DIR_NAME, builtin_general_subagent, discover_subagents,
    format_available_subagents, parse_agent_md,
};
use super::parse_subagent_input;
use super::task_tool::{MAX_PARALLEL_TASKS, TaskSpec, parse_task_specs, resolve_task_specs};

#[test]
fn parses_simple_frontmatter() {
    let raw = "---\nname: reviewer\ndescription: code review\n---\n# Body\nYou are a reviewer.";
    let info = parse_agent_md(raw).unwrap();
    assert_eq!(info.name, "reviewer");
    assert_eq!(info.description, "code review");
    assert!(info.tools.is_none());
    assert!(info.skills.is_none());
    assert!(info.mcp.is_none());
    assert!(info.model.is_none());
    assert_eq!(info.system_prompt, "# Body\nYou are a reviewer.");
}

#[test]
fn parses_tools_and_model() {
    let raw = "---\nname: explorer\ndescription: explore code\ntools: [bash, read, grep, glob]\nmodel: deepseek/deepseek-chat\n---\nExplore.";
    let info = parse_agent_md(raw).unwrap();
    assert_eq!(info.name, "explorer");
    assert_eq!(info.description, "explore code");
    assert_eq!(
        info.tools,
        Some(vec![
            "bash".into(),
            "read".into(),
            "grep".into(),
            "glob".into()
        ])
    );
    assert_eq!(info.model, Some("deepseek/deepseek-chat".into()));
    assert_eq!(info.system_prompt, "Explore.");
}

#[test]
fn parses_skills_and_mcp() {
    let raw = "---\nname: researcher\ndescription: research\nskills: [code-review, help]\nmcp: [context7, zread]\n---\nResearch.";
    let info = parse_agent_md(raw).unwrap();
    assert_eq!(info.name, "researcher");
    assert_eq!(info.skills, Some(vec!["code-review".into(), "help".into()]));
    assert_eq!(info.mcp, Some(vec!["context7".into(), "zread".into()]));
    assert_eq!(info.system_prompt, "Research.");
}

#[test]
fn rejects_missing_name() {
    let raw = "---\ndescription: no name\n---\nbody";
    assert!(parse_agent_md(raw).is_none());
}

#[test]
fn parses_without_frontmatter() {
    let raw = "Just a prompt";
    let info = parse_agent_md(raw).unwrap();
    assert_eq!(info.name, "");
    assert_eq!(info.description, "");
    assert!(info.tools.is_none());
    assert!(info.model.is_none());
    assert_eq!(info.system_prompt, "Just a prompt");
}

#[test]
fn parse_subagent_input_basic() {
    let (name, prompt) = parse_subagent_input("@subagent: general 搜索所有 TODO 注释").unwrap();
    assert_eq!(name, "general");
    assert_eq!(prompt, "搜索所有 TODO 注释");
}

#[test]
fn parse_subagent_input_no_match() {
    assert!(parse_subagent_input("hello world").is_none());
    assert!(parse_subagent_input("@subagent:").is_none());
    assert!(parse_subagent_input("@subagent: general").is_none());
}

#[test]
fn discover_finds_project_level_subagent() {
    let tmp = std::env::temp_dir().join(format!("hailux-subagent-test-{}", uuid::Uuid::new_v4()));
    let agent_dir = tmp
        .join(CONFIG_DIR_NAME)
        .join(AGENT_DIR_NAME)
        .join("reviewer");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join(AGENT_FILE_NAME),
        "---\nname: reviewer\ndescription: review code\ntools: [read, grep]\n---\n# Reviewer\nYou review code.",
    )
    .unwrap();

    let subagents = discover_subagents(&tmp).unwrap();
    let reviewer = subagents.iter().find(|s| s.name == "reviewer").unwrap();
    assert_eq!(reviewer.description, "review code");
    assert_eq!(
        reviewer.allowed_tools,
        Some(vec!["read".into(), "grep".into()])
    );
    assert_eq!(reviewer.system_prompt, "# Reviewer\nYou review code.");

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn format_available_subagents_not_empty() {
    let configs = vec![builtin_general_subagent("test/model")];
    let formatted = format_available_subagents(&configs);
    assert!(formatted.contains("<available_subagents>"));
    assert!(formatted.contains("general"));
}

#[test]
fn parse_task_specs_valid_array() {
    let args = serde_json::json!({
        "tasks": [
            {
                "subagent": "general",
                "description": "search todos",
                "prompt": "find all TODO comments"
            },
            {
                "subagent": "reviewer",
                "prompt": "review src/lib.rs",
                "task_id": "abc"
            }
        ]
    });
    let specs = parse_task_specs(&args).unwrap();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].subagent, "general");
    assert_eq!(specs[0].description, "search todos");
    assert_eq!(specs[0].task_id, None);
    assert_eq!(specs[1].subagent, "reviewer");
    assert_eq!(specs[1].description, "");
    assert_eq!(specs[1].task_id.as_deref(), Some("abc"));
}

#[test]
fn parse_task_specs_missing_array() {
    let args = serde_json::json!({"subagent": "general", "prompt": "p"});
    let err = parse_task_specs(&args).unwrap_err();
    assert!(err.contains("\"tasks\""));
}

#[test]
fn parse_task_specs_empty_array() {
    let args = serde_json::json!({"tasks": []});
    assert!(parse_task_specs(&args).is_err());
}

#[test]
fn parse_task_specs_item_missing_prompt() {
    let args = serde_json::json!({"tasks": [{"subagent": "general"}]});
    let err = parse_task_specs(&args).unwrap_err();
    assert!(err.contains("tasks[0]"));
}

#[test]
fn parse_task_specs_blank_prompt() {
    let args = serde_json::json!({"tasks": [{"subagent": "general", "prompt": "   "}]});
    assert!(parse_task_specs(&args).is_err());
}

fn spec(subagent: &str, task_id: Option<&str>) -> TaskSpec {
    TaskSpec {
        subagent: subagent.to_string(),
        description: "d".to_string(),
        prompt: "p".to_string(),
        task_id: task_id.map(|s| s.to_string()),
    }
}

#[test]
fn resolve_task_specs_ok() {
    let subs = vec![builtin_general_subagent("m")];
    let specs = vec![spec("general", None), spec("general", Some("id1"))];
    let configs = resolve_task_specs(&specs, &subs).unwrap();
    assert_eq!(configs.len(), 2);
    assert!(configs.iter().all(|c| c.name == "general"));
}

#[test]
fn resolve_task_specs_unknown_subagent() {
    let subs = vec![builtin_general_subagent("m")];
    let specs = vec![spec("ghost", None)];
    let err = resolve_task_specs(&specs, &subs).unwrap_err();
    assert!(err.contains("not found"));
    assert!(err.contains("general"));
}

#[test]
fn resolve_task_specs_duplicate_task_id() {
    let subs = vec![builtin_general_subagent("m")];
    let specs = vec![spec("general", Some("same")), spec("general", Some("same"))];
    let err = resolve_task_specs(&specs, &subs).unwrap_err();
    assert!(err.contains("duplicate task_id"));
}

#[test]
fn resolve_task_specs_too_many() {
    let subs = vec![builtin_general_subagent("m")];
    let specs: Vec<TaskSpec> = (0..=MAX_PARALLEL_TASKS)
        .map(|_| spec("general", None))
        .collect();
    let err = resolve_task_specs(&specs, &subs).unwrap_err();
    assert!(err.contains("too many tasks"));
}
