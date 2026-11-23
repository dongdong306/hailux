use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::ModelEntry;

/// Wrap the input text at the available width, returning (line texts, cursor row, cursor column).
/// Cursor row/column are measured in visual width units; rows start at 0.
pub(crate) fn wrap_input(buffer: &str, cursor: usize, width: usize) -> (Vec<String>, usize, usize) {
    if width == 0 {
        return (vec![String::new()], 0, 0);
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    let mut byte_pos = 0usize;

    for ch in buffer.chars() {
        let ch_w = UnicodeWidthChar::width(ch).unwrap_or(1);

        // Cursor is at the position before this character
        if byte_pos == cursor {
            cursor_row = lines.len();
            cursor_col = current_w;
        }

        if current_w + ch_w > width {
            lines.push(std::mem::take(&mut current));
            current_w = 0;
            if byte_pos == cursor {
                cursor_row = lines.len();
                cursor_col = 0;
            }
        }

        current.push(ch);
        current_w += ch_w;
        byte_pos += ch.len_utf8();
    }

    if byte_pos == cursor {
        cursor_row = lines.len();
        cursor_col = current_w;
    }

    lines.push(current);

    (lines, cursor_row, cursor_col)
}

pub fn render_model_picker(
    area: Rect,
    buf: &mut Buffer,
    models: &[ModelEntry],
    selected_index: usize,
    current_model: &str,
    refreshing: Option<&str>,
    refresh_note: Option<&(bool, String)>,
) {
    let dialog_width = (area.width as usize).min(50);
    let dialog_height = ((area.height as usize).saturating_sub(4) * 8 / 10).clamp(20, 40);
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
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            " 切换模型 ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center);

    let inner = block.inner(dialog_area);
    block.render(dialog_area, buf);

    // The status row (refreshing / last result) takes one line; the rest goes to the list
    let status_rows = u16::from(refreshing.is_some() || refresh_note.is_some());
    let content_height = inner.height.saturating_sub(2 + status_rows) as usize;

    let scroll_offset = if selected_index >= content_height {
        selected_index - content_height / 2
    } else {
        0
    };

    let mut lines: Vec<Line> = Vec::new();
    let mut last_provider = String::new();

    for (i, entry) in models.iter().enumerate() {
        if i < scroll_offset || i >= scroll_offset + content_height {
            continue;
        }

        if entry.provider_name != last_provider {
            last_provider = entry.provider_name.clone();
            let sep_text = format!("  ── {} ", entry.provider_name);
            let sep_remaining =
                (inner.width as usize).saturating_sub(UnicodeWidthStr::width(sep_text.as_str()));
            lines.push(Line::from(vec![
                Span::styled(sep_text, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "─".repeat(sep_remaining),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        let is_current = entry.display == current_model;
        let is_selected = i == selected_index;

        let prefix = if is_current { "● " } else { "  " };

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

        let lead_bg = if is_selected {
            Color::Cyan
        } else {
            Color::Reset
        };
        let mut spans = vec![
            Span::styled("  ".to_string(), Style::default().bg(lead_bg)),
            Span::styled(
                prefix,
                if is_selected {
                    Style::default().fg(Color::DarkGray).bg(Color::Cyan)
                } else if is_current {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::styled(entry.model_name.clone(), style),
        ];
        // Trailing span fills the background color to end of line
        let fill_bg = if is_selected {
            Color::Cyan
        } else {
            Color::Reset
        };
        spans.push(Span::styled(
            " ".repeat(inner.width as usize),
            Style::default().bg(fill_bg),
        ));
        lines.push(Line::from(spans));
    }

    // "Add model..." entry (index = models.len())
    let add_index = models.len();
    if add_index >= scroll_offset && add_index < scroll_offset + content_height {
        let is_selected = add_index == selected_index;
        let fill_bg = if is_selected {
            Color::Cyan
        } else {
            Color::Reset
        };
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  ".to_string(), Style::default().bg(fill_bg)),
            Span::styled(
                "+ 添加自定义模型...",
                if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Cyan)
                },
            ),
            Span::styled(
                " ".repeat(inner.width as usize),
                Style::default().bg(fill_bg),
            ),
        ]));
    }

    if let Some(pid) = refreshing {
        lines.push(Line::from(Span::styled(
            format!("  正在获取 {pid} 的模型列表…"),
            Style::default().fg(Color::Cyan),
        )));
    } else if let Some((ok, text)) = refresh_note {
        lines.push(Line::from(Span::styled(
            format!("  {text}"),
            if *ok {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Red)
            },
        )));
    }

    lines.push(Line::from(""));
    let help_text = "↑↓选择 R获取模型 Enter确认 Esc关闭";
    let help_pad = (inner.width as usize).saturating_sub(UnicodeWidthStr::width(help_text)) / 2;
    lines.push(Line::from(Span::styled(
        format!("{}{}", " ".repeat(help_pad), help_text),
        Style::default().fg(Color::Gray),
    )));

    let paragraph = Paragraph::new(lines);
    paragraph.render(inner, buf);
}

/// Render the reasoning-effort submenu (/effort): three levels + current-level marker + descriptions, styled consistently with the model picker
pub fn render_effort_picker(
    area: Rect,
    buf: &mut Buffer,
    selected_index: usize,
    current: crate::agent::ReasoningEffort,
) {
    let dialog_width = (area.width as usize).min(44);
    let dialog_height = 10u16;
    let dialog_x = (area.width as usize).saturating_sub(dialog_width) / 2;
    let dialog_y = (area.height as usize).saturating_sub(dialog_height as usize) / 2;

    let dialog_area = Rect::new(
        dialog_x as u16,
        dialog_y as u16,
        dialog_width as u16,
        dialog_height,
    );

    Clear.render(dialog_area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray))
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            " 思考强度 ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center);

    let inner = block.inner(dialog_area);
    block.render(dialog_area, buf);

    const LEVELS: [(crate::agent::ReasoningEffort, &str, &str); 3] = [
        (
            crate::agent::ReasoningEffort::Low,
            "LOW",
            "轻量推理，更快响应",
        ),
        (
            crate::agent::ReasoningEffort::High,
            "HIGH",
            "均衡模式（默认）",
        ),
        (
            crate::agent::ReasoningEffort::Max,
            "MAX",
            "深度推理，更强但更慢",
        ),
    ];

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    for (i, (effort, label, desc)) in LEVELS.iter().enumerate() {
        let is_current = *effort == current;
        let is_selected = i == selected_index;
        let fill_bg = if is_selected {
            Color::Cyan
        } else {
            Color::Reset
        };

        let spans = vec![
            Span::styled("  ".to_string(), Style::default().bg(fill_bg)),
            Span::styled(
                if is_current { "● " } else { "  " },
                if is_selected {
                    Style::default().fg(Color::Green).bg(Color::Cyan)
                } else if is_current {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::styled(
                format!("{label:<5}"),
                if is_selected {
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
                },
            ),
            Span::styled(
                format!("  {desc}"),
                if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::styled(
                " ".repeat(inner.width as usize),
                Style::default().bg(fill_bg),
            ),
        ];
        // Characters beyond the trailing fill span get clipped when the row exceeds the line width (fallback for narrow terminals)
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));
    let help_text = "↑↓选择 Enter确认 Esc关闭";
    let help_pad = (inner.width as usize).saturating_sub(UnicodeWidthStr::width(help_text)) / 2;
    lines.push(Line::from(Span::styled(
        format!("{}{}", " ".repeat(help_pad), help_text),
        Style::default().fg(Color::Gray),
    )));

    Paragraph::new(lines).render(inner, buf);
}
