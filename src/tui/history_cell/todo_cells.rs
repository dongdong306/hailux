use ratatui::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;

use super::{HistoryCell, finish_hasher, wrap_text};

/// 待办列表项的状态
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub(crate) enum TodoStatus {
    Completed,
    InProgress,
    Pending,
    Cancelled,
}

/// 待办列表项
#[derive(Debug, Clone)]
pub(crate) struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: String,
}

/// 待办列表卡片：解析 todo_write 的结果，以 checkbox 风格渲染
///   • Updated plan
///     ✔ 完成任务 (strikethrough dim)
///     → 进行中任务 (cyan bold)
///     ○ 待处理任务 (dim)
///     ✗ 已取消任务 (dim strikethrough)
pub(crate) struct TodoCell {
    pub items: Vec<TodoItem>,
    pub all_done: bool,
    /// 写入是否仍在进行（尚无结果、仅从参数提取时为 true）
    pub writing: bool,
}

impl TodoCell {
    /// 从 todo_write 工具的原始结果文本解析出结构化项
    pub fn from_result(result: &str) -> Self {
        let mut items = Vec::new();
        for line in result.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("任务列表已更新") {
                continue;
            }
            // 格式: "1. ✓ 任务内容 [中] completed"
            // 去掉前导编号 "N. "
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
            // 提取优先级标签 [高]/[中]/[低]
            let priority = if let Some(start) = after_icon.find('[') {
                if let Some(end) = after_icon[start..].find(']') {
                    after_icon[start + 1..start + end].to_string()
                } else {
                    String::from("中")
                }
            } else {
                String::from("中")
            };
            // 内容 = 去掉末尾的 " [X] status"
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
            Span::styled(" plan", Style::default().fg(Color::Cyan)),
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
