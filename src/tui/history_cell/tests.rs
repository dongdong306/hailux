use super::ask_user_render::parse_ask_user_pairs;
use super::diff::{MAX_DISPLAY_DIFF_LINES, render_diff_from_json};
use super::task_cells::{task_items_from_args, task_outcomes_from_result};
use super::{HistoryCell, TaskStep, TaskToolCell, format_seconds};

fn spans_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[test]
fn task_cell_running_single_agent_shows_recent_steps() {
    let cell = TaskToolCell {
        arguments: serde_json::json!({
            "tasks": [{"subagent": "general", "description": "d", "prompt": "p"}]
        })
        .to_string(),
        result: None,
        steps: vec![
            TaskStep {
                agent: Some("general".into()),
                summary: "Read a.rs".into(),
                is_done: true,
                index: Some(0),
            },
            TaskStep {
                agent: Some("general".into()),
                summary: "Grep x".into(),
                is_done: true,
                index: Some(0),
            },
            TaskStep {
                agent: Some("general".into()),
                summary: "Bash build".into(),
                is_done: false,
                index: Some(0),
            },
        ],
    };
    let texts: Vec<String> = cell.display_lines(100).iter().map(spans_text).collect();
    assert_eq!(texts.len(), 4);
    assert!(texts[0].contains("Delegating"));
    assert!(texts[0].contains("general"));
    assert!(texts[1].contains("├ ✓ Read a.rs"));
    assert!(texts[2].contains("├ ✓ Grep x"));
    assert!(texts[3].contains("└ ● Bash build"));
}

#[test]
fn task_cell_running_multi_agent_latest_action_per_agent() {
    let cell = TaskToolCell {
        arguments: serde_json::json!({
            "tasks": [
                {"subagent": "general", "description": "g", "prompt": "p"},
                {"subagent": "reviewer", "description": "r", "prompt": "p"}
            ]
        })
        .to_string(),
        result: None,
        steps: vec![
            TaskStep {
                agent: Some("general".into()),
                summary: "Read a.rs".into(),
                is_done: true,
                index: Some(0),
            },
            TaskStep {
                agent: Some("general".into()),
                summary: "Bash build".into(),
                is_done: false,
                index: Some(0),
            },
        ],
    };
    let texts: Vec<String> = cell.display_lines(100).iter().map(spans_text).collect();
    assert_eq!(texts.len(), 3);
    assert!(texts[1].contains("general"));
    assert!(texts[1].contains("Bash build"));
    // reviewer 无步骤 → 显示 description
    assert!(texts[2].contains("reviewer"));
    assert!(texts[2].contains("r"));
}

#[test]
fn subagent_step_summary_includes_verb_prefix() {
    let read_args = serde_json::json!({"file_path": "src/main.rs"}).to_string();
    assert_eq!(
        super::subagent_step_summary("read", &read_args),
        "Read src/main.rs"
    );
    let grep_args = serde_json::json!({"pattern": "TODO", "path": "src"}).to_string();
    assert_eq!(
        super::subagent_step_summary("grep", &grep_args),
        "Searched \"TODO\" in src"
    );
    let bash_args = serde_json::json!({"command_string": "cargo build"}).to_string();
    assert_eq!(
        super::subagent_step_summary("bash", &bash_args),
        "Ran cargo build"
    );
}

#[test]
fn task_items_prefers_description_over_prompt() {
    let args = serde_json::json!({
        "tasks": [
            {"subagent": "general", "description": "find todos", "prompt": "long prompt"},
            {"subagent": "reviewer", "prompt": "review this code please"}
        ]
    });
    let items = task_items_from_args(&args);
    assert_eq!(items[0], ("general".into(), "find todos".into()));
    assert_eq!(items[1].0, "reviewer");
    assert_eq!(items[1].1, "review this code please");
}

#[test]
fn task_items_empty_args_fallback() {
    let items = task_items_from_args(&serde_json::json!({}));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, "subagent");
}

#[test]
fn task_outcomes_wrapped_and_failed_blocks() {
    let result = "<tasks count=\"3\">\n\
            <task subagent=\"general\" task_id=\"id-1\" state=\"completed\">\n\
            <task_result>\nfirst line\nsecond line\n</task_result>\n</task>\n\
            <task subagent=\"reviewer\" state=\"failed\">\n\
            <error>\nboom\n</error>\n</task>\n\
            <task subagent=\"researcher\" task_id=\"id-3\" state=\"completed\">\n\
            <task_result>\n\n</task_result>\n</task>\n\
            </tasks>";
    let outcomes = task_outcomes_from_result(result);
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes[0].ok);
    assert_eq!(outcomes[0].first_line, "first line");
    assert!(!outcomes[1].ok);
    assert_eq!(outcomes[1].first_line, "boom");
    assert!(outcomes[2].ok);
    assert_eq!(outcomes[2].first_line, "(no output)");
}

#[test]
fn task_outcomes_single_unwrapped_block() {
    let result = "<task subagent=\"general\" task_id=\"id-1\" state=\"completed\">\n<task_result>\nhello\n</task_result>\n</task>";
    let outcomes = task_outcomes_from_result(result);
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].ok);
    assert_eq!(outcomes[0].first_line, "hello");
}

fn line_texts(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn renders_hunk_format_diff() {
    let json = serde_json::json!({
        "format": "diff",
        "path": "src/main.rs",
        "additions": 1,
        "deletions": 1,
        "old_lines": 10,
        "new_lines": 10,
        "is_new_file": false,
        "truncated": false,
        "hunks": [{
            "old_start": 2,
            "new_start": 2,
            "changes": [[" ", "a"], ["-", "b"], ["+", "B"], [" ", "c"]]
        }]
    })
    .to_string();
    let lines = render_diff_from_json(&json, 100);
    let texts = line_texts(&lines);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("main.rs") && t.contains("(+1 -1)"))
    );
    // 行号从 old_start/new_start=2 起步连续推进：
    // context(a)=2 → delete(b)=old 3 → insert(B)=new 3 → context(c)=new 4
    assert!(texts.iter().any(|t| t.contains("-b")));
    assert!(texts.iter().any(|t| t.contains("+B")));
    let del = texts.iter().find(|t| t.contains("-b")).unwrap();
    assert_eq!(del.trim_end(), "     3 -b");
    let ins = texts.iter().find(|t| t.contains("+B")).unwrap();
    assert_eq!(ins.trim_end(), "     3 +B");
}

#[test]
fn renders_hunk_format_new_file() {
    let json = serde_json::json!({
        "format": "diff",
        "path": "new.txt",
        "additions": 3,
        "deletions": 0,
        "old_lines": 0,
        "new_lines": 3,
        "is_new_file": true,
        "hunks": [{
            "old_start": null,
            "new_start": 1,
            "changes": [["+", "x"], ["+", "y"]]
        }]
    })
    .to_string();
    let lines = render_diff_from_json(&json, 100);
    let texts = line_texts(&lines);
    assert!(texts.iter().any(|t| t.contains("(+3 new file)")));
    // 新文件行：无旧行号，行号从 new_start=1 推进
    let first = texts.iter().find(|t| t.contains("+x")).unwrap();
    assert!(
        first.contains("1"),
        "new file line numbers start at 1: {first}"
    );
}

#[test]
fn renders_hunk_format_display_cap() {
    // 存储不限制，但渲染最多显示 MAX_DISPLAY_DIFF_LINES 行
    let changes: Vec<serde_json::Value> = (0..60)
        .map(|i| serde_json::json!(["+", format!("line{i}")]))
        .collect();
    let json = serde_json::json!({
        "format": "diff",
        "path": "big.txt",
        "additions": 60,
        "deletions": 0,
        "old_lines": 0,
        "new_lines": 60,
        "is_new_file": true,
        "hunks": [{"old_start": null, "new_start": 1, "changes": changes}]
    })
    .to_string();
    let lines = render_diff_from_json(&json, 100);
    let texts = line_texts(&lines);
    let shown = texts.iter().filter(|t| t.contains("+line")).count();
    assert_eq!(shown, MAX_DISPLAY_DIFF_LINES);
    assert!(texts.iter().any(|t| t.contains("truncated")));
}

#[test]
fn renders_legacy_format_diff() {
    // 旧格式（无 format 字段）：存量数据仍走 old/new 全文 + 现场 TextDiff
    let json = serde_json::json!({
        "old": "a\nb\nc\n",
        "new": "a\nB\nc\n",
        "path": "legacy.rs",
    })
    .to_string();
    let lines = render_diff_from_json(&json, 100);
    let texts = line_texts(&lines);
    assert!(texts.iter().any(|t| t.contains("legacy.rs")));
    assert!(texts.iter().any(|t| t.contains("-b")));
    assert!(texts.iter().any(|t| t.contains("+B")));
}

#[test]
fn format_seconds_zero() {
    assert_eq!(format_seconds(0), "0.0");
}

#[test]
fn format_seconds_exact_second() {
    assert_eq!(format_seconds(1000), "1.0");
}

#[test]
fn format_seconds_subsecond() {
    assert_eq!(format_seconds(550), "0.6");
}

#[test]
fn format_seconds_large() {
    assert_eq!(format_seconds(12_345), "12.3");
}

#[test]
fn parse_ask_user_pairs_escape_roundtrip() {
    // ask_escape 转义（`\` 与 `"`）→ parse_ask_user_pairs 无损还原
    let cases = [
        ("简单问题", "D:\\project\\hailux"),
        ("含引号 \" 和反斜杠 \\", "尾部反斜杠\\"),
        ("路径", "C:\\Users\\name\\"),
    ];
    for (q, a) in cases {
        let formatted = format!(
            "\"{}\"=\"{}\"",
            super::super::ask_user::ask_escape(q),
            super::super::ask_user::ask_escape(a)
        );
        let pairs = parse_ask_user_pairs(&formatted);
        assert_eq!(
            pairs,
            vec![(q.to_string(), a.to_string())],
            "input: {formatted}"
        );
    }
}

#[test]
fn parse_ask_user_pairs_multiple_and_legacy() {
    // 多对逗号连接 + 旧格式（仅 \" 转义）兼容
    let s = "\"q1\"=\"a1\", \"q2\"=\"Unanswered\", \"say \\\"hi\\\"\"=\"ok\"";
    assert_eq!(
        parse_ask_user_pairs(s),
        vec![
            ("q1".to_string(), "a1".to_string()),
            ("q2".to_string(), "Unanswered".to_string()),
            ("say \"hi\"".to_string(), "ok".to_string()),
        ]
    );
}
