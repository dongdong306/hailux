use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_width::UnicodeWidthStr;

use crate::storage::{MessageRole, StoredMessage, SubsessionSummary};
use crate::tui::app::Message;
use crate::tui::history_cell::{self, HistoryCell};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRunStatus {
    Running,
    Completed,
    Error,
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub call_id: u64,
    pub session_id: String,
    pub subagent_name: String,
    pub description: String,
    #[allow(dead_code)]
    pub started_at: std::time::Instant,
    pub status: TaskRunStatus,
}

#[derive(Debug, Clone)]
pub struct TaskEntry {
    pub record: Option<TaskRecord>,
    pub subsession: Option<SubsessionSummary>,
    pub subagent_name: String,
    pub description: String,
    pub status: TaskRunStatus,
}

pub struct TasksViewer<'a> {
    pub entries: &'a [TaskEntry],
    pub selected_index: usize,
}

impl<'a> TasksViewer<'a> {
    pub fn render(self, area: Rect, buf: &mut Buffer) {
        let dialog_width = (area.width as usize).min(72);
        let dialog_height = ((area.height as usize).saturating_sub(4) * 8 / 10).clamp(20, 44);
        let dialog_x = (area.width as usize).saturating_sub(dialog_width) / 2;
        let dialog_y = (area.height as usize).saturating_sub(dialog_height) / 2;

        let dialog_area = Rect::new(
            dialog_x as u16,
            dialog_y as u16,
            dialog_width as u16,
            dialog_height as u16,
        );

        Clear.render(dialog_area, buf);

        let running_count = self
            .entries
            .iter()
            .filter(|e| e.status == TaskRunStatus::Running)
            .count();
        let total = self.entries.len();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Gray))
            .title(Span::styled(
                format!(" Subagent Tasks ({}/{}) ", running_count, total),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center);

        let inner = block.inner(dialog_area);
        block.render(dialog_area, buf);

        let [content_area, help_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);

        let content_height = content_area.height as usize;
        let content_width = content_area.width as usize;

        let mut lines: Vec<Line> = Vec::new();

        if self.entries.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  当前会话还没有子代理执行记录",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  子代理（subagent）在独立会话中运行，",
                Style::default().fg(Color::Gray),
            )));
            lines.push(Line::from(Span::styled(
                "  可通过 @subagent: 语法或 task 工具调用。",
                Style::default().fg(Color::Gray),
            )));
            Paragraph::new(lines).render(content_area, buf);
            render_help(help_area, buf);
            return;
        }

        const ITEM_LINES: usize = 3;
        let visible_items = content_height / ITEM_LINES;
        let first_visible = if self.selected_index >= visible_items {
            self.selected_index - visible_items / 2
        } else {
            0
        };

        for (i, entry) in self.entries.iter().enumerate() {
            if i < first_visible {
                continue;
            }
            if lines.len() >= content_height.saturating_sub(ITEM_LINES) + ITEM_LINES {
                break;
            }

            let is_selected = i == self.selected_index;
            let fill_bg = if is_selected {
                Color::Cyan
            } else {
                Color::Reset
            };

            let sel_fg = if is_selected {
                Color::Black
            } else {
                Color::Reset
            };
            let name_style = Style::default()
                .fg(sel_fg)
                .bg(fill_bg)
                .add_modifier(Modifier::BOLD);
            let dim_style = Style::default()
                .fg(if is_selected {
                    Color::Black
                } else {
                    Color::DarkGray
                })
                .bg(fill_bg);

            let selector = if is_selected { "▸ " } else { "  " };

            let (icon, icon_color) = match entry.status {
                TaskRunStatus::Running => ("●", Color::Yellow),
                TaskRunStatus::Completed => ("✓", Color::Green),
                TaskRunStatus::Error => ("✗", Color::Red),
            };

            let desc_display = if entry.description.is_empty() {
                "(no description)".to_string()
            } else {
                truncate_str(&entry.description, content_width.saturating_sub(10))
            };

            lines.push(Line::from(vec![
                Span::styled(selector, dim_style),
                Span::styled(
                    icon,
                    Style::default()
                        .fg(icon_color)
                        .bg(fill_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {} ", entry.subagent_name), name_style),
                Span::styled(desc_display, dim_style),
                Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
            ]));

            let detail = if let Some(ref sub) = entry.subsession {
                let pt = format_tokens(sub.prompt_tokens);
                let ct = format_tokens(sub.completion_tokens);
                format!("{} → {}", pt, ct)
            } else if entry.status == TaskRunStatus::Running {
                "running...".to_string()
            } else {
                String::new()
            };

            let model_detail = entry
                .subsession
                .as_ref()
                .map(|s| s.model.clone())
                .unwrap_or_default();

            let meta_parts: Vec<String> = [detail, model_detail]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect();

            lines.push(Line::from(vec![
                Span::styled("  ", dim_style),
                Span::styled(meta_parts.join("  ·  "), dim_style),
                Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
            ]));

            lines.push(Line::from(""));
        }

        Paragraph::new(lines).render(content_area, buf);
        render_help(help_area, buf);
    }
}

/// 将 subagent 的 StoredMessage 列表转换为主聊天区的 Message 列表，
/// 以复用 history_cell 渲染管线。
fn stored_messages_to_chat(messages: &[StoredMessage]) -> Vec<Message> {
    use std::collections::HashMap;
    let mut result = Vec::new();
    let mut tool_call_names: HashMap<String, String> = HashMap::new();
    for msg in messages {
        match msg.role {
            MessageRole::System => {}
            MessageRole::User => {
                if !msg.content.is_empty() {
                    result.push(Message::User(msg.content.clone()));
                }
            }
            MessageRole::Assistant => {
                // 推理内容（如果有）先于正文输出
                if let Some(ref reasoning) = msg.reasoning_content
                    && !reasoning.is_empty()
                {
                    result.push(Message::AgentThinking {
                        text: reasoning.clone(),
                        think_ms: msg.think_ms.map(|ms| ms as u64),
                        thinking_started_at: None,
                    });
                }

                let has_tool_calls = msg
                    .tool_calls
                    .as_ref()
                    .and_then(|tc| serde_json::from_str::<serde_json::Value>(tc).ok())
                    .and_then(|v| v.as_array().map(|a| !a.is_empty()))
                    .unwrap_or(false);

                if !msg.content.is_empty() {
                    result.push(Message::Agent(msg.content.clone()));
                }

                if has_tool_calls
                    && let Some(tc_str) = &msg.tool_calls
                    && let Ok(tc_arr) = serde_json::from_str::<Vec<serde_json::Value>>(tc_str)
                {
                    for tc in &tc_arr {
                        let name = tc["function"]["name"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string();
                        let args = tc["function"]["arguments"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        let tc_id = tc["id"].as_str().unwrap_or("").to_string();
                        result.push(Message::ToolCall {
                            name: name.clone(),
                            arguments: args,
                        });
                        if !tc_id.is_empty() {
                            tool_call_names.insert(tc_id, name);
                        }
                    }
                }
            }
            MessageRole::Tool => {
                let name = msg
                    .tool_call_id
                    .as_deref()
                    .and_then(|id| tool_call_names.get(id))
                    .cloned()
                    .or_else(|| {
                        result.iter().rev().find_map(|m| match m {
                            Message::ToolCall { name, .. } => Some(name.clone()),
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| "tool".to_string());
                result.push(Message::ToolResult {
                    name,
                    result: msg.content.clone(),
                    display: None,
                });
            }
        }
    }
    result
}

pub fn render_task_detail(
    area: Rect,
    buf: &mut Buffer,
    entries: &[TaskEntry],
    task_index: usize,
    messages: &[StoredMessage],
    scroll_offset: &mut usize,
) {
    let entry = match entries.get(task_index) {
        Some(e) => e,
        None => return,
    };

    Clear.render(area, buf);

    let (status_icon, status_color) = match entry.status {
        TaskRunStatus::Running => ("●", Color::Yellow),
        TaskRunStatus::Completed => ("✓", Color::Green),
        TaskRunStatus::Error => ("✗", Color::Red),
    };

    // 布局：状态行(1) + 任务ID/描述行(1) + 分割线(1) + 消息区(flex) + 底部提示(1)
    let [header_top, header_bot, divider_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // 第一行：状态图标 + subagent_name + 状态/tokens/model
    let mut top_spans = vec![
        Span::styled(
            format!(" {} ", status_icon),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            entry.subagent_name.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let (status_text, status_fg) = match entry.status {
        TaskRunStatus::Running => ("Running", Color::Yellow),
        TaskRunStatus::Completed => ("Completed", Color::Green),
        TaskRunStatus::Error => ("Error", Color::Red),
    };

    if let Some(ref sub) = entry.subsession {
        let pt = format_tokens(sub.prompt_tokens);
        let ct = format_tokens(sub.completion_tokens);
        top_spans.push(Span::styled(
            format!("  {}  ·  {} → {}  ·  {}", status_text, pt, ct, sub.model),
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        top_spans.push(Span::styled(
            format!("  {}", status_text),
            Style::default().fg(status_fg),
        ));
    }

    Paragraph::new(Line::from(top_spans)).render(header_top, buf);

    // 第二行：task_id + description
    let task_id = entry
        .subsession
        .as_ref()
        .map(|s| s.id.as_str())
        .or_else(|| entry.record.as_ref().map(|r| r.session_id.as_str()))
        .unwrap_or("");
    let bot_width = header_bot.width as usize;
    let id_label = format!("ID: {}", task_id);
    let id_w = UnicodeWidthStr::width(id_label.as_str());
    let desc_w = bot_width.saturating_sub(id_w + 2);
    let desc_display: String = entry.description.chars().take(desc_w).collect();

    let mut bot_spans: Vec<Span> = Vec::new();
    bot_spans.push(Span::styled(id_label, Style::default().fg(Color::DarkGray)));
    if !desc_display.is_empty() && desc_w > 3 {
        bot_spans.push(Span::styled(
            format!("  {}", desc_display),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Paragraph::new(Line::from(bot_spans)).render(header_bot, buf);

    // 分割线
    let divider_line = Line::from(vec![Span::styled(
        "─".repeat(divider_area.width as usize),
        Style::default().fg(Color::DarkGray),
    )]);
    Paragraph::new(divider_line).render(divider_area, buf);

    // 消息列表（复用 history_cell 管线）
    let scrollbar_width: u16 = 2;
    let text_width = body_area.width.saturating_sub(scrollbar_width);
    let visible_height = body_area.height;

    let chat_messages = stored_messages_to_chat(messages);
    let cells: Vec<Box<dyn HistoryCell>> = if chat_messages.is_empty() {
        vec![]
    } else {
        history_cell::messages_to_cells(&chat_messages, true)
    };

    // 构建带分隔空行的行列表（每个 cell 后加一个空行，同主聊天区逻辑）
    let mut all_lines: Vec<Line> = Vec::new();
    for cell in &cells {
        let lines = cell.display_lines(text_width);
        all_lines.extend(lines);
        all_lines.push(Line::from("")); // 消息间分隔
    }

    if all_lines.is_empty() {
        all_lines.push(Line::from(""));
        all_lines.push(Line::from(Span::styled(
            "  (无消息记录)",
            Style::default().fg(Color::Gray),
        )));
    }

    // 背景色填充
    for line in all_lines.iter_mut() {
        if let Some(bg) = line.spans.iter().find_map(|span| match span.style.bg {
            Some(color) if color != Color::Reset => Some(color),
            _ => None,
        }) {
            line.style = line.style.bg(bg);
        }
    }

    let total_lines = all_lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height as usize);
    let actual_scroll = (*scroll_offset).min(max_scroll);
    // 写回 clamp 后的值，防止 PgDown 累积超过上限
    *scroll_offset = actual_scroll;

    let visible_lines: Vec<Line> = all_lines
        .into_iter()
        .skip(actual_scroll)
        .take(visible_height as usize)
        .collect();

    let text_area = Rect::new(body_area.x, body_area.y, text_width, visible_height);
    Paragraph::new(visible_lines).render(text_area, buf);

    // 滚动条
    if total_lines > visible_height as usize {
        let scrollbar_area = Rect::new(
            body_area.x + text_width,
            body_area.y,
            scrollbar_width,
            visible_height,
        );
        Clear.render(scrollbar_area, buf);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"))
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .style(Style::default().fg(Color::DarkGray))
            .thumb_style(Style::default().fg(Color::White));

        let scroll_position = if max_scroll > 0 {
            let ratio = actual_scroll as f64 / max_scroll as f64;
            ((total_lines - 1) as f64 * ratio).round() as usize
        } else {
            0
        };

        let mut scrollbar_state = ScrollbarState::new(total_lines)
            .viewport_content_length(visible_height as usize)
            .position(scroll_position);

        scrollbar.render(scrollbar_area, buf, &mut scrollbar_state);
    }

    // 底部提示栏
    let help_text = if total_lines > visible_height as usize {
        "↑↓ 切换  PgUp/PgDn 翻页  Esc 返回列表"
    } else {
        "↑↓ 切换  Esc 返回列表"
    };
    let help = format!(" {} ", help_text);
    let help_w = UnicodeWidthStr::width(help.as_str()) as u16;
    let help_line = Line::from(vec![
        Span::styled(help, Style::default().fg(Color::DarkGray)),
        Span::styled(
            " ".repeat(footer_area.width.saturating_sub(help_w) as usize),
            Style::default(),
        ),
    ]);
    Paragraph::new(help_line).render(footer_area, buf);
}

fn render_help(area: Rect, buf: &mut Buffer) {
    let help_text = "↑↓ 移动  Enter 详情  Esc 关闭";
    let help_pad = (area.width as usize).saturating_sub(UnicodeWidthStr::width(help_text)) / 2;
    let help_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("{}{}", " ".repeat(help_pad), help_text),
            Style::default().fg(Color::Gray),
        )),
    ];
    Paragraph::new(help_lines).render(area, buf);
}

fn format_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}
