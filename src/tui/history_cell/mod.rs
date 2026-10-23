use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ratatui::prelude::*;

use super::app::types::Message;

mod ask_user_render;
mod diff;
mod message_cells;
mod task_cells;
mod todo_cells;
mod tool_cells;

#[cfg(test)]
mod tests;

pub(crate) use message_cells::{
    AgentMarkdownCell, DoneCell, ReasoningCell, SessionHeaderCell, TooltipCell, UserMessageCell,
};
pub(crate) use task_cells::{TaskStep, TaskToolCell};
pub(crate) use todo_cells::{TodoCell, TodoItem, TodoStatus};
pub(crate) use tool_cells::{ExploringCell, ToolCell};

pub(crate) trait HistoryCell: Send + Sync {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;
    /// 基于内容计算缓存键，用于跳过未变更 cell 的重复渲染
    fn cache_key(&self) -> u64;
}

fn finish_hasher(h: DefaultHasher) -> u64 {
    h.finish()
}

fn format_seconds(ms: u64) -> String {
    let secs = ms as f64 / 1000.0;
    format!("{:.1}", secs)
}

/// 工具分类，每种工具拥有独立的动词、颜色和显示标签
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ToolCategory {
    FileRead,
    FileWrite,
    FileEdit,
    Search,
    Glob,
    WebFetch,
    Command,
    Todo,
    Mcp,
    Skill,
    AskUser,
    Task,
    Other,
}

impl ToolCategory {
    pub fn from_name(name: &str) -> Self {
        match name {
            "read" => Self::FileRead,
            "write" => Self::FileWrite,
            "edit" => Self::FileEdit,
            "grep" => Self::Search,
            "glob" => Self::Glob,
            "web_fetch" => Self::WebFetch,
            "bash" => Self::Command,
            "todo_write" => Self::Todo,
            "ask_user" => Self::AskUser,
            "skill" => Self::Skill,
            "task" => Self::Task,
            n if n.starts_with("mcp_") => Self::Mcp,
            _ => Self::Other,
        }
    }

    /// 进行中动词（如 Codex 的 "Calling" / "Running" / "Exploring"）
    pub fn verb_active(&self) -> &'static str {
        match self {
            Self::FileRead => "Reading",
            Self::FileWrite => "Writing",
            Self::FileEdit => "Editing",
            Self::Search => "Searching",
            Self::Glob => "Listing",
            Self::WebFetch => "Fetching",
            Self::Command => "Running",
            Self::Todo => "Updating",
            Self::Mcp => "Calling",
            Self::Skill => "Load skill",
            Self::AskUser => "Asking",
            Self::Task => "Delegating",
            Self::Other => "Running",
        }
    }

    /// 完成动词（如 Codex 的 "Called" / "Ran" / "Explored"）
    pub fn verb_done(&self) -> &'static str {
        match self {
            Self::FileRead => "Read",
            Self::FileWrite => "Wrote",
            Self::FileEdit => "Edited",
            Self::Search => "Searched",
            Self::Glob => "Listed",
            Self::WebFetch => "Fetched",
            Self::Command => "Ran",
            Self::Todo => "Updated",
            Self::Mcp => "Called",
            Self::Skill => "Load skill",
            Self::AskUser => "Asked",
            Self::Task => "Delegated",
            Self::Other => "Ran",
        }
    }

    /// 主题色（活跃/成功状态）
    pub fn color(&self) -> Color {
        match self {
            Self::FileRead => Color::Cyan,
            Self::FileWrite | Self::FileEdit => Color::Green,
            Self::Search => Color::Yellow,
            Self::Glob => Color::Magenta,
            Self::WebFetch => Color::Blue,
            Self::Command => Color::Rgb(255, 160, 60),
            Self::Todo => Color::Cyan,
            Self::Mcp => Color::Rgb(180, 120, 255),
            Self::Skill => Color::Rgb(100, 200, 150),
            Self::AskUser => Color::Cyan,
            Self::Task => Color::Rgb(100, 180, 255),
            Self::Other => Color::Gray,
        }
    }
}

/// 聊天正文前景色
pub(crate) const CHAT_FG: Color = Color::Rgb(248, 250, 252); // #F8FAFC
/// 输入占位符（中性灰，不带蓝调）
pub(crate) const CHAT_PLACEHOLDER: Color = Color::Rgb(130, 130, 130); // #828282
/// 粘贴元素高亮
pub(crate) const CHAT_PASTE: Color = Color::Rgb(34, 211, 238); // #22D3EE
/// 路径引用 `@file`/`@folder` 高亮
pub(crate) const CHAT_FILE_MENTION: Color = Color::Rgb(251, 191, 36); // #FBBF24
pub(crate) const CHAT_IMAGE: Color = Color::Rgb(167, 139, 250); // #A78BFA
/// Plan 模式徽标
pub(crate) const PLAN_BADGE: Color = Color::Rgb(217, 159, 7); // #D99F07

/// 将 Message 列表转换为 HistoryCell 列表
/// 连续只读工具调用合并为 ExploringCell，其余各自独立展示
pub(crate) fn messages_to_cells(
    messages: &[Message],
    thinking_collapsed: bool,
) -> Vec<Box<dyn HistoryCell>> {
    use message_cells::DividerCell;
    use tool_cells::{PlainStepCell, explore_item_from, is_readonly_tool};

    let mut cells: Vec<Box<dyn HistoryCell>> = Vec::new();
    let mut i = 0;
    let mut last_plan_mode = false;

    while i < messages.len() {
        match &messages[i] {
            Message::User { text, plan_mode } => {
                last_plan_mode = *plan_mode;
                cells.push(Box::new(UserMessageCell {
                    text: text.clone(),
                    plan_mode: *plan_mode,
                }));
                i += 1;
            }
            Message::Agent(text) => {
                if !text.trim().is_empty() {
                    cells.push(Box::new(AgentMarkdownCell { text: text.clone() }));
                }
                i += 1;
            }
            Message::AgentStreaming(text) => {
                if !text.trim().is_empty() {
                    cells.push(Box::new(AgentMarkdownCell { text: text.clone() }));
                }
                i += 1;
            }
            Message::AgentThinking {
                text,
                think_ms,
                thinking_started_at,
            } => {
                if !text.trim().is_empty() {
                    cells.push(Box::new(ReasoningCell {
                        text: text.clone(),
                        collapsed: thinking_collapsed,
                        think_ms: *think_ms,
                        thinking_started_at: *thinking_started_at,
                    }));
                }
                i += 1;
            }
            Message::AgentDone {
                total_ms,
                model,
                status,
            } => {
                cells.push(Box::new(DoneCell {
                    total_ms: *total_ms,
                    model: model.clone(),
                    status: *status,
                    plan_mode: last_plan_mode,
                }));
                i += 1;
            }
            Message::ToolCall { name, arguments } => {
                if name == "task" {
                    // Task 工具：收集 SubagentStep 作为实时进度，取 ToolResult 作为终态
                    let mut task_result = None;
                    let mut steps = Vec::new();
                    let mut j = i + 1;
                    while j < messages.len() {
                        match &messages[j] {
                            Message::SubagentStep {
                                summary,
                                is_done,
                                agent,
                                index,
                            } => {
                                steps.push(TaskStep {
                                    agent: agent.clone(),
                                    summary: summary.clone(),
                                    is_done: *is_done,
                                    index: *index,
                                });
                                j += 1;
                            }
                            Message::ToolResult {
                                name: rn, result, ..
                            } if rn == "task" => {
                                task_result = Some(result.clone());
                                j += 1;
                                break;
                            }
                            _ => break,
                        }
                    }
                    cells.push(Box::new(TaskToolCell {
                        arguments: arguments.clone(),
                        result: task_result,
                        steps,
                    }));
                    i = j;
                } else if is_readonly_tool(name) {
                    let mut items = Vec::new();
                    let mut all_done = true;
                    let mut j = i;

                    while j < messages.len() {
                        match &messages[j] {
                            Message::ToolCall {
                                name: tn,
                                arguments: ta,
                            } => {
                                if !is_readonly_tool(tn) {
                                    break;
                                }
                                let result = if j + 1 < messages.len() {
                                    if let Message::ToolResult {
                                        name: rn, result, ..
                                    } = &messages[j + 1]
                                    {
                                        if rn == tn { Some(result.clone()) } else { None }
                                    } else {
                                        all_done = false;
                                        None
                                    }
                                } else {
                                    all_done = false;
                                    None
                                };
                                items.push(explore_item_from(tn, ta, result.as_deref()));
                                j += 1;
                                if result.is_some() {
                                    j += 1;
                                }
                            }
                            Message::ToolResult { .. } => {
                                j += 1;
                            }
                            _ => break,
                        }
                    }

                    if items.len() == 1 {
                        let (result, display) = if i + 1 < messages.len() {
                            if let Message::ToolResult {
                                name: rn,
                                result,
                                display,
                            } = &messages[i + 1]
                            {
                                if rn == name {
                                    (Some(result.clone()), display.clone())
                                } else {
                                    (None, None)
                                }
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        };
                        let has_result = result.is_some();
                        cells.push(Box::new(ToolCell {
                            name: name.clone(),
                            arguments: arguments.clone(),
                            result,
                            display,
                        }));
                        i += 1;
                        if has_result {
                            i += 1;
                        }
                    } else {
                        cells.push(Box::new(ExploringCell { items, all_done }));
                        i = j;
                    }
                } else if ToolCategory::from_name(name) == ToolCategory::Todo {
                    // todo_write：解析结果生成 TodoCell
                    let next_result = if i + 1 < messages.len() {
                        if let Message::ToolResult {
                            name: rn, result, ..
                        } = &messages[i + 1]
                        {
                            if rn == name {
                                Some(result.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(ref result) = next_result {
                        cells.push(Box::new(TodoCell::from_result(result)));
                        i += 2;
                    } else {
                        // 尚未有结果时，从参数中提取简略展示
                        let args: serde_json::Value =
                            serde_json::from_str(arguments).unwrap_or_default();
                        let mut items = Vec::new();
                        if let Some(todos) = args["todos"].as_array() {
                            for item in todos {
                                let content =
                                    item["content"].as_str().unwrap_or("未知任务").to_string();
                                let status = match item["status"].as_str().unwrap_or("pending") {
                                    "completed" => TodoStatus::Completed,
                                    "in_progress" => TodoStatus::InProgress,
                                    "cancelled" => TodoStatus::Cancelled,
                                    _ => TodoStatus::Pending,
                                };
                                items.push(TodoItem {
                                    content,
                                    status,
                                    priority: item["priority"].as_str().unwrap_or("中").to_string(),
                                });
                            }
                        }
                        cells.push(Box::new(TodoCell {
                            items,
                            all_done: false,
                            writing: true,
                        }));
                        i += 1;
                    }
                } else {
                    let (next_result, display) = if i + 1 < messages.len() {
                        if let Message::ToolResult {
                            name: rn,
                            result,
                            display,
                        } = &messages[i + 1]
                        {
                            if rn == name {
                                (Some(result.clone()), display.clone())
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        }
                    } else {
                        (None, None)
                    };

                    let has_result = next_result.is_some();
                    cells.push(Box::new(ToolCell {
                        name: name.clone(),
                        arguments: arguments.clone(),
                        result: next_result,
                        display,
                    }));

                    i += 1;
                    if has_result {
                        i += 1;
                    }
                }
            }
            Message::ToolResult {
                name,
                result,
                display,
            } => {
                cells.push(Box::new(ToolCell {
                    name: name.clone(),
                    arguments: String::new(),
                    result: Some(result.clone()),
                    display: display.clone(),
                }));
                i += 1;
            }
            Message::SubagentStep {
                summary,
                is_done,
                agent,
                index: _,
            } => {
                cells.push(Box::new(PlainStepCell {
                    summary: summary.clone(),
                    is_done: *is_done,
                    agent: agent.clone(),
                }));
                i += 1;
            }
            Message::CompactMarker {
                compacted_count,
                total_ms,
            } => {
                let label = if let Some(ms) = total_ms {
                    format!("─ Context compacted ({}s) ", format_seconds(*ms))
                } else {
                    "─ Context compacted ".to_string()
                };
                cells.push(Box::new(DividerCell {
                    label,
                    hash_seed: *compacted_count as u64,
                }));
                i += 1;
            }
            Message::CompactStreaming(text) => {
                cells.push(Box::new(DividerCell {
                    label: "─ Compacting context ".to_string(),
                    hash_seed: {
                        let mut h = DefaultHasher::new();
                        text.hash(&mut h);
                        h.finish()
                    },
                }));
                i += 1;
            }
        }
    }

    cells
}

/// 使用 textwrap 进行精确换行
fn wrap_text(text: &str, max_width: u16) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let options = textwrap::Options::new(max_width as usize)
        .break_words(true)
        .word_separator(textwrap::WordSeparator::AsciiSpace);
    textwrap::wrap(text, &options)
        .into_iter()
        .map(|cow| cow.into_owned())
        .collect()
}

/// 从工具调用的参数中提取摘要信息
/// subagent 步骤摘要：动词前缀 + 目标（如 "Read src/main.rs"、"Grep \"TODO\" in src"）
pub(crate) fn subagent_step_summary(name: &str, arguments: &str) -> String {
    let cat = ToolCategory::from_name(name);
    let detail = tool_call_summary(name, arguments);
    if detail.is_empty() {
        cat.verb_done().to_string()
    } else {
        format!("{} {}", cat.verb_done(), detail)
    }
}

pub(crate) fn tool_call_summary(name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    match name {
        "read" => {
            let path = args["file_path"].as_str().unwrap_or("");
            let offset = args["offset"].as_u64();
            let limit = args["limit"].as_u64();
            match (offset, limit) {
                (Some(o), Some(l)) => format!("{} (L{}-L{})", path, o, o + l - 1),
                (Some(o), None) => format!("{} (from L{})", path, o),
                _ => path.to_string(),
            }
        }
        "edit" => {
            let path = args["file_path"].as_str().unwrap_or("");
            let replace_all = args["replace_all"].as_bool().unwrap_or(false);
            if replace_all {
                format!("{} (replace all)", path)
            } else {
                path.to_string()
            }
        }
        "write" => args["file_path"].as_str().unwrap_or("").to_string(),
        "bash" => {
            let cmd = args["command_string"].as_str().unwrap_or("");
            truncate_str(cmd, 60)
        }
        "web_fetch" => {
            let url = args["url"].as_str().unwrap_or("");
            let format = args["format"].as_str().unwrap_or("markdown");
            if format == "markdown" {
                url.to_string()
            } else {
                format!("{} [{}]", url, format)
            }
        }
        "grep" => {
            let pattern = args["pattern"].as_str().unwrap_or("");
            let include = args["include"].as_str();
            let path = args["path"].as_str().unwrap_or(".");
            match include {
                Some(inc) => format!("\"{}\" in {} ({})", pattern, path, inc),
                None => format!("\"{}\" in {}", pattern, path),
            }
        }
        "glob" => {
            let pattern = args["pattern"].as_str().unwrap_or("");
            let path = args["path"].as_str().unwrap_or(".");
            format!("{} in {}", pattern, path)
        }
        "todo_write" => {
            let count = args["todos"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("{} items", count)
        }
        "ask_user" => {
            let count = args["questions"].as_array().map(|a| a.len()).unwrap_or(0);
            if count == 0 {
                String::new()
            } else {
                format!("{} question{}", count, if count > 1 { "s" } else { "" })
            }
        }
        "skill" => args["name"].as_str().unwrap_or("").to_string(),
        "task" => {
            let count = args["tasks"].as_array().map(|a| a.len()).unwrap_or(0);
            match count {
                0 => String::new(),
                1 => args["tasks"][0]["subagent"]
                    .as_str()
                    .unwrap_or("subagent")
                    .to_string(),
                n => format!("{n} agents"),
            }
        }
        _ => {
            let cat = ToolCategory::from_name(name);
            match cat {
                ToolCategory::Mcp => mcp_call_summary(name, &args),
                ToolCategory::Skill => skill_call_summary(name, &args),
                _ => truncate_str(arguments, 60),
            }
        }
    }
}

fn mcp_call_summary(name: &str, args: &serde_json::Value) -> String {
    let server = name.strip_prefix("mcp_").unwrap_or(name);
    let first_key = args
        .as_object()
        .and_then(|m| m.keys().next())
        .cloned()
        .unwrap_or_default();
    if first_key.is_empty() {
        server.to_string()
    } else {
        let val = args[&first_key].to_string();
        format!("{} {}", server, truncate_str(&val, 40))
    }
}

fn skill_call_summary(name: &str, args: &serde_json::Value) -> String {
    let skill_name = name.strip_prefix("skill_").unwrap_or(name);
    let prompt = args["prompt"].as_str().unwrap_or("");
    if prompt.is_empty() {
        skill_name.to_string()
    } else {
        format!("{} \"{}\"", skill_name, truncate_str(prompt, 40))
    }
}

pub(crate) fn tool_result_summary(name: &str, result: &str) -> String {
    if result.is_empty() {
        return String::new();
    }
    match name {
        "read" => {
            let kind = extract_xml_tag(result, "type");
            let content = extract_xml_tag(result, "content");
            let line_count = content.map(|c| c.lines().count()).unwrap_or(0);
            let entries = extract_xml_tag(result, "entries");
            let entry_count = entries
                .map(|e| e.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0);
            match (kind.as_deref(), line_count, entry_count) {
                (Some("file"), lc, _) if lc > 0 => format!("{} lines", lc),
                (Some("directory"), _, ec) if ec > 0 => format!("{} entries", ec),
                _ => String::new(),
            }
        }
        "edit" | "write" => {
            if result.contains("successful") {
                "done".to_string()
            } else {
                truncate_str(result.trim(), 40)
            }
        }
        "bash" => {
            let trimmed = result.trim();
            if trimmed.is_empty() {
                "no output".to_string()
            } else {
                let total_lines = trimmed.lines().count();
                let max_preview = 3;
                if total_lines <= max_preview {
                    truncate_str(trimmed, 120)
                } else {
                    let head: Vec<&str> = trimmed.lines().take(max_preview).collect();
                    format!(
                        "{}\n... +{} lines",
                        head.join("\n"),
                        total_lines - max_preview
                    )
                }
            }
        }
        "web_fetch" => format!("{} chars", result.chars().count()),
        "grep" => {
            if result.starts_with("No matches found") {
                "no matches".to_string()
            } else {
                let file_count = result
                    .lines()
                    .filter(|l| l.ends_with(':') && !l.starts_with(' '))
                    .count();
                let line_matches = result.lines().filter(|l| l.starts_with("  ")).count();
                if file_count > 0 {
                    format!("{} files, {} matches", file_count, line_matches)
                } else {
                    String::new()
                }
            }
        }
        "glob" => {
            if result.starts_with("No matching files found") {
                "no matches".to_string()
            } else {
                let count = result.lines().filter(|l| !l.trim().is_empty()).count();
                format!("{} files", count)
            }
        }
        "todo_write" => {
            let completed = result.matches("✓").count();
            let in_progress = result.matches("→").count();
            let total =
                result.matches('○').count() + completed + in_progress + result.matches("✗").count();
            format!("{}/{} done", completed, total)
        }
        "ask_user" => {
            if result.contains("[User Cancelled]") {
                "cancelled".to_string()
            } else {
                "answered".to_string()
            }
        }
        "task" => {
            // 提取 <task_result> 内容
            let inner =
                extract_xml_tag(result, "task_result").unwrap_or_else(|| result.to_string());
            let trimmed = inner.trim();
            if trimmed.is_empty() {
                "no output".to_string()
            } else {
                let total_lines = trimmed.lines().count();
                let max_preview = 5;
                if total_lines <= max_preview {
                    truncate_str(trimmed, 200)
                } else {
                    let head: Vec<&str> = trimmed.lines().take(max_preview).collect();
                    format!(
                        "{}\n... +{} lines",
                        head.join("\n"),
                        total_lines - max_preview
                    )
                }
            }
        }
        _ => truncate_str(result.trim(), 60),
    }
}

fn extract_xml_tag(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = content.find(&open)?;
    let content_after = &content[start + open.len()..];
    let end = content_after.find(&close)?;
    Some(content_after[..end].to_string())
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}
