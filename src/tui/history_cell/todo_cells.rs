use ratatui::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;

use super::{HistoryCell, finish_hasher, wrap_text};

/// Status of a todo list item
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub(crate) enum TodoStatus {
    Completed,
    InProgress,
    Pending,
    Cancelled,
}

/// Todo list item
#[derive(Debug, Clone)]
pub(crate) struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: String,
}

/// Todo list card: renders a todo_write result as a checkbox-style list.
///   • Updated todo
///     ✔ completed item (strikethrough dim)
///     → in-progress item (cyan bold)
///     ○ pending item (dim)
///     ✗ cancelled item (dim strikethrough)
pub(crate) struct TodoCell {
    pub items: Vec<TodoItem>,
    pub all_done: bool,
    /// Whether the write is still in progress (true when there is no result yet
    /// and items were extracted from the arguments only)
    pub writing: bool,
}

impl TodoCell {
    /// Parse structured items from the raw result text of the todo_write tool
    pub fn from_result(result: &str) -> Self {
        let mut items = Vec::new();
        for line in result.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("任务列表已更新") {
                continue;
            }
            // Format: "1. ✓ task content [中] completed"
            // Strip the leading numbering "N. "
            let rest = trimmed
                .find(". ")
                .map(|pos| &trimmed[pos + 2..])
                .unwrap_or(trimmed);

            let (status, icon_len) = if rest.starts_with("✓") {
                (TodoStatus::Completed, "✓".len())
            } else if rest.starts_with("→") {
                (TodoStatus::InProgress, "→".len())
            } else if rest.starts_with("✗") {
                (TodoStatus::Cancelled, "✗".len())
            } else if rest.starts_with("○") {
                (TodoStatus::Pending, "○".len())
            } else {
                continue;
            };

            let after_icon = &rest[icon_len..].trim_start();
            // Extract the priority tag [高]/[中]/[低]
            let priority = if let Some(start) = after_icon.find('[') {
                if let Some(end) = after_icon[start..].find(']') {
                    after_icon[start + 1..start + end].to_string()
                } else {
                    String::from("中")
                }
            } else {
                String::from("中")
            };
            // Content = strip the trailing " [X] status"
            let content = if let Some(bracket_pos) = after_icon.rfind(" [") {
                after_icon[..bracket_pos].to_string()
            } else {
                after_icon.to_string()
            };

            items.push(TodoItem {
                content,
                status,
                priority,
            });
        }

        let all_done = !items.is_empty()
            && items
                .iter()
                .all(|it| it.status == TodoStatus::Completed || it.status == TodoStatus::Cancelled);
        Self {
            items,
            all_done,
            writing: false,
        }
    }
}

impl HistoryCell for TodoCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.all_done.hash(&mut h);
        self.writing.hash(&mut h);
        self.items.len().hash(&mut h);
        for item in &self.items {
            item.content.hash(&mut h);
            item.status.hash(&mut h);
            item.priority.hash(&mut h);
        }
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let verb = if self.writing { "Updating" } else { "Updated" };
        let bullet = if self.all_done {
            Span::styled(
                "•".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("•".to_string(), Style::default().fg(Color::Cyan))
        };

        let mut lines = vec![Line::from(vec![
            bullet,
            Span::raw(" "),
            Span::styled(
                verb.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(" todo", Style::default().fg(Color::Cyan)),
        ])];

        let detail_w = width.saturating_sub(6) as usize;
        for item in &self.items {
            let (icon, icon_style) = match item.status {
                TodoStatus::Completed => ("✓", Style::default().fg(Color::Green)),
                TodoStatus::InProgress => (
                    "→",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                TodoStatus::Pending => ("○", Style::default().fg(Color::DarkGray)),
                TodoStatus::Cancelled => ("✗", Style::default().fg(Color::DarkGray)),
            };

            let content_style = match item.status {
                TodoStatus::Completed => Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::CROSSED_OUT),
                TodoStatus::InProgress => Style::default().fg(Color::White),
                TodoStatus::Pending => Style::default().fg(Color::DarkGray),
                TodoStatus::Cancelled => Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::CROSSED_OUT),
            };

            let text = item.content.clone();
            let wrapped = wrap_text(&text, detail_w.max(1) as u16);
            for (i, line) in wrapped.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled("  └ ", Style::default()),
                        Span::styled(icon.to_string(), icon_style),
                        Span::styled(" ", Style::default()),
                        Span::styled(line.clone(), content_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("      ", Style::default()),
                        Span::styled(line.clone(), content_style),
                    ]));
                }
            }
        }

        lines
    }
}
