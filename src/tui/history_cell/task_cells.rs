use ratatui::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;

use super::{HistoryCell, ToolCategory, extract_xml_tag, finish_hasher, truncate_str, wrap_text};

/// Task tool call (tasks array; single/multi subagent concurrent batch)
pub(crate) struct TaskToolCell {
    pub arguments: String,
    pub result: Option<String>,
    /// Live steps emitted by subagents while running (event order; includes source agent)
    pub steps: Vec<TaskStep>,
}

/// A single subagent step
pub(crate) struct TaskStep {
    pub agent: Option<String>,
    pub summary: String,
    pub is_done: bool,
    /// Index of the source task in the tasks array (disambiguates instances with duplicate names)
    pub index: Option<usize>,
}

/// Parse the task list from the task arguments into (subagent name, display summary)
/// Summary prefers description, falling back to a truncated prompt
pub(super) fn task_items_from_args(args: &serde_json::Value) -> Vec<(String, String)> {
    let Some(arr) = args["tasks"].as_array() else {
        return vec![("subagent".to_string(), String::new())];
    };
    arr.iter()
        .map(|t| {
            let name = t["subagent"].as_str().unwrap_or("subagent").to_string();
            let preview = t["description"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    truncate_str(t["prompt"].as_str().unwrap_or(""), 48).to_string()
                });
            (name, preview)
        })
        .collect()
}

/// Output of a single task within the task tool result
pub(super) struct TaskOutcome {
    pub(super) ok: bool,
    pub(super) first_line: String,
}

fn extract_attr(tag: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=\"");
    let start = tag.find(&marker)? + marker.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Parse the `<task subagent=".." state="..">` blocks in the result line by line,
/// tolerating results with or without a `<tasks>` wrapper; order matches the tasks array
pub(super) fn task_outcomes_from_result(result: &str) -> Vec<TaskOutcome> {
    let mut out: Vec<TaskOutcome> = Vec::new();
    let mut current: Option<TaskOutcome> = None;
    for line in result.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("<task ") {
            if let Some(mut c) = current.take() {
                if c.first_line.is_empty() {
                    c.first_line = "(no output)".to_string();
                }
                out.push(c);
            }
            let ok = extract_attr(rest, "state")
                .map(|s| s != "failed")
                .unwrap_or(true);
            current = Some(TaskOutcome {
                ok,
                first_line: String::new(),
            });
        } else if let Some(c) = current.as_mut() {
            if trimmed.starts_with("</task") {
                if c.first_line.is_empty() {
                    c.first_line = "(no output)".to_string();
                }
                out.push(current.take().expect("checked above"));
            } else if !trimmed.is_empty()
                && !trimmed.starts_with("<task_result")
                && !trimmed.starts_with("<error")
                && !trimmed.starts_with("</")
                && c.first_line.is_empty()
            {
                c.first_line = truncate_str(trimmed, 60).to_string();
            }
        }
    }
    if let Some(mut c) = current.take() {
        if c.first_line.is_empty() {
            c.first_line = "(no output)".to_string();
        }
        out.push(c);
    }
    out
}

impl HistoryCell for TaskToolCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.arguments.hash(&mut h);
        self.result.hash(&mut h);
        for s in &self.steps {
            s.agent.hash(&mut h);
            s.summary.hash(&mut h);
            s.is_done.hash(&mut h);
            s.index.hash(&mut h);
        }
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let args: serde_json::Value = serde_json::from_str(&self.arguments).unwrap_or_default();
        let items = task_items_from_args(&args);
        let has_result = self.result.is_some();
        let accent = ToolCategory::Task.color();
        let mut lines = Vec::new();

        let bullet = if has_result {
            Span::styled(
                "•",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "●",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        };
        let verb = if has_result {
            "Delegated"
        } else {
            "Delegating"
        };

        if items.len() <= 1 {
            // Single task: verb + agent name; show recent steps live while running,
            // result preview once done
            let (name, preview) = items
                .into_iter()
                .next()
                .unwrap_or_else(|| ("subagent".to_string(), String::new()));
            lines.push(Line::from(vec![
                bullet,
                Span::raw(" "),
                Span::styled(
                    verb.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(name, Style::default().fg(accent)),
            ]));
            if !has_result {
                // Running: last 3 steps (✓ done / ● in progress)
                let recent_count = self.steps.len().min(3);
                if recent_count == 0 {
                    if !preview.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("  └ {preview}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                } else {
                    let start = self.steps.len() - recent_count;
                    for (k, step) in self.steps[start..].iter().enumerate() {
                        let branch = if k + 1 == recent_count { "└" } else { "├" };
                        let (icon, icon_color) = if step.is_done {
                            ("✓", Color::DarkGray)
                        } else {
                            ("●", Color::Yellow)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {branch} "),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(icon.to_string(), Style::default().fg(icon_color)),
                            Span::styled(
                                format!(" {}", step.summary),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                }
            }
        } else {
            // Multiple tasks: title count + per-task live action lines
            lines.push(Line::from(vec![
                bullet,
                Span::raw(" "),
                Span::styled(
                    verb.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{} agents", items.len()),
                    Style::default().fg(accent),
                ),
            ]));
            let outcomes = self.result.as_deref().map(task_outcomes_from_result);
            let last = items.len().saturating_sub(1);
            for (idx, (name, preview)) in items.iter().enumerate() {
                let branch = if idx == last { "└" } else { "├" };
                let mut spans = vec![Span::styled(
                    format!("  {branch} "),
                    Style::default().fg(Color::DarkGray),
                )];
                let detail = match outcomes.as_ref().and_then(|os| os.get(idx)) {
                    Some(o) => {
                        let status_style = if o.ok {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default().fg(Color::Red)
                        };
                        let icon = if o.ok { "✓" } else { "✗" };
                        spans.push(Span::styled(format!("{icon} "), status_style));
                        o.first_line.clone()
                    }
                    None => {
                        // Running: latest action for this task (match by index first;
                        // names cannot disambiguate instances when same-name
                        // subagents run concurrently)
                        let action = self
                            .steps
                            .iter()
                            .rev()
                            .find(|s| s.index == Some(idx))
                            .or_else(|| {
                                self.steps
                                    .iter()
                                    .rev()
                                    .find(|s| s.agent.as_deref() == Some(name.as_str()))
                            })
                            .map(|s| s.summary.clone())
                            .unwrap_or_else(|| {
                                if preview.is_empty() {
                                    "(starting)".to_string()
                                } else {
                                    preview.clone()
                                }
                            });
                        spans.push(Span::raw("  "));
                        action
                    }
                };
                spans.push(Span::styled(name.clone(), Style::default().fg(accent)));
                if !detail.is_empty() {
                    spans.push(Span::styled(
                        format!(" · {detail}"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ));
                }
                lines.push(Line::from(spans));
            }
            return lines;
        }

        // Single-task result summary (3-line preview + collapsed count)
        if let Some(ref result) = self.result {
            let inner =
                extract_xml_tag(result, "task_result").unwrap_or_else(|| result.to_string());
            let trimmed = inner.trim();
            if !trimmed.is_empty() {
                let content_lines: Vec<&str> = trimmed.lines().collect();
                let total = content_lines.len();
                let max_preview = 3;
                let mut result_lines: Vec<String> = Vec::new();
                for (idx, l) in content_lines.iter().enumerate().take(max_preview) {
                    if idx == 0 {
                        result_lines.push(format!("  └ {}", l));
                    } else {
                        result_lines.push(format!("    {}", l));
                    }
                }
                if total > max_preview {
                    result_lines.push(format!("    ... +{} lines", total - max_preview));
                }
                let result_w = width.saturating_sub(4) as usize;
                for rline in &result_lines {
                    let wrapped = wrap_text(rline, result_w.max(1) as u16);
                    for wline in &wrapped {
                        lines.push(Line::from(Span::styled(
                            wline.clone(),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM),
                        )));
                    }
                }
            }
        }

        lines
    }
}
