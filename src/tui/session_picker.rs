use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::storage::SessionSummary;

pub struct SessionPicker<'a> {
    pub sessions: &'a [SessionSummary],
    pub filtered_indices: &'a [usize],
    pub selected_index: usize,
    pub search_query: &'a str,
    pub current_session_id: Option<&'a str>,
}

impl<'a> SessionPicker<'a> {
    pub fn render(self, area: Rect, buf: &mut Buffer) {
        let dialog_width = (area.width as usize).min(60);
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

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Gray))
            .title(Span::styled(
                " Sessions ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Center);

        let inner = block.inner(dialog_area);
        block.render(dialog_area, buf);

        let [search_area, content_area, help_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(inner);

        let search_line = Line::from(vec![
            Span::styled(" 搜索: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                if self.search_query.is_empty() {
                    "输入关键词过滤...".to_string()
                } else {
                    self.search_query.to_string()
                },
                Style::default().fg(if self.search_query.is_empty() {
                    Color::Gray
                } else {
                    Color::White
                }),
            ),
            if !self.search_query.is_empty() {
                Span::styled("█", Style::default().fg(Color::White))
            } else {
                Span::raw("")
            },
        ]);
        Paragraph::new(vec![search_line, Line::from("")]).render(search_area, buf);

        let mut lines: Vec<Line> = Vec::new();

        let content_height = content_area.height as usize;
        let visible_items = self.filtered_indices.len();

        let scroll_offset = if self.selected_index >= content_height {
            self.selected_index - content_height / 2
        } else {
            0
        };

        let mut last_date = String::new();
        for (i, &idx) in self.filtered_indices.iter().enumerate() {
            if i < scroll_offset || i >= scroll_offset + content_height {
                continue;
            }
            if idx >= self.sessions.len() {
                continue;
            }
            let session = &self.sessions[idx];

            let date_label = format_date_category(&session.updated_at);
            if date_label != last_date {
                last_date = date_label.clone();
                lines.push(Line::from(Span::styled(
                    format!("  {}", date_label),
                    Style::default().fg(Color::Gray),
                )));
            }

            let is_current = self.current_session_id == Some(session.id.as_str());
            let is_selected = i == self.selected_index;

            let title = truncate_str(&session.title, dialog_width - 16);
            let time = format_time_short(&session.updated_at);

            let prefix = if is_current { " ● " } else { "   " };
            let selector = if is_selected { "▸ " } else { "  " };

            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let dim_style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            };
            let fill_bg = if is_selected {
                Color::Cyan
            } else {
                Color::Reset
            };

            let display_title = if title.is_empty() {
                "(新会话)".to_string()
            } else {
                title
            };

            lines.push(Line::from(vec![
                Span::styled(format!("{}{}", prefix, selector), dim_style),
                Span::styled(display_title, style),
                Span::styled(format!("  {}", time), dim_style),
                Span::styled(
                    " ".repeat(content_area.width as usize),
                    Style::default().bg(fill_bg),
                ),
            ]));
        }

        if visible_items == 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  没有找到匹配的会话",
                Style::default().fg(Color::Gray),
            )));
        }

        Paragraph::new(lines).render(content_area, buf);

        let help_text = "↑↓选择 Enter切换 Ctrl+D删除 Ctrl+N新建 Esc关闭";
        let help_pad =
            (help_area.width as usize).saturating_sub(UnicodeWidthStr::width(help_text)) / 2;
        let help_lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("{}{}", " ".repeat(help_pad), help_text),
                Style::default().fg(Color::Gray),
            )),
        ];
        Paragraph::new(help_lines).render(help_area, buf);
    }
}

fn format_date_category(iso: &str) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let date_part = iso.get(..10).unwrap_or(&today).to_string();
    if date_part == today {
        "Today".to_string()
    } else {
        date_part
    }
}

fn format_time_short(iso: &str) -> String {
    iso.get(11..16).unwrap_or("--:--").to_string()
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}
