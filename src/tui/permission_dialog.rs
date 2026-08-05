use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use textwrap::wrap;
use unicode_width::UnicodeWidthStr;

use crate::permission::PermissionRequest;

const OPTIONS: &[&str] = &["Allow once", "Allow always (this session)", "Deny"];

/// 处理权限对话框键盘事件，返回是否完成（true = 已回复）
pub(crate) fn handle_key(key: crossterm::event::KeyEvent, selected: &mut usize) -> bool {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Up => {
            if *selected == 0 {
                *selected = OPTIONS.len() - 1;
            } else {
                *selected -= 1;
            }
            false
        }
        KeyCode::Down => {
            *selected = (*selected + 1) % OPTIONS.len();
            false
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            let digit = c.to_digit(10).unwrap_or(0);
            if digit >= 1 && (digit as usize) <= OPTIONS.len() {
                *selected = (digit - 1) as usize;
                true
            } else {
                false
            }
        }
        KeyCode::Enter => true,
        KeyCode::Esc => {
            *selected = 2; // Deny
            true
        }
        _ => false,
    }
}

/// 根据当前选中项构造权限回复
pub(crate) fn reply_from_selected(selected: usize) -> crate::permission::PermissionReply {
    match selected {
        0 => crate::permission::PermissionReply::Once,
        1 => crate::permission::PermissionReply::Always,
        _ => crate::permission::PermissionReply::Deny,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_permission_dialog(
    area: Rect,
    buf: &mut Buffer,
    request: &PermissionRequest,
    selected: usize,
    subagent_name: Option<&str>,
) {
    let dialog_width = (area.width as usize).clamp(50, 80);
    let inner_w = dialog_width.saturating_sub(4);

    // 标题：subagent 请求时带上名称
    let title = if let Some(name) = subagent_name {
        format!(" Permission Required [{name}] ")
    } else {
        " Permission Required ".to_string()
    };

    // 计算高度
    let perm_line = format!("[{}] {}", request.permission, request.description);
    let desc_lines = wrap(&perm_line, inner_w);
    let mut total_lines = desc_lines.len() + 2; // desc + blank + options

    // 如果 description 很长，可能需要换行
    for opt in OPTIONS {
        total_lines += 1 + wrap(opt, inner_w.saturating_sub(5)).len().saturating_sub(1);
    }

    let dialog_height = (total_lines + 4) as u16;
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
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center);

    let inner = block.inner(dialog_area);
    block.render(dialog_area, buf);

    let [content_area, help_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);

    let mut lines: Vec<Line> = Vec::new();

    // 描述行
    for line in &desc_lines {
        lines.push(Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::from(""));

    // 选项
    for (i, opt) in OPTIONS.iter().enumerate() {
        let active = i == selected;
        let selector = if active { "▸ " } else { "  " };
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let num_style = if active {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        lines.push(Line::from(vec![
            Span::styled(selector, style),
            Span::styled(format!("{}.", i + 1), num_style),
            Span::styled(format!(" {opt}"), style),
            Span::styled(
                " ".repeat(content_area.width as usize),
                Style::default().bg(if active { Color::Yellow } else { Color::Reset }),
            ),
        ]));
    }

    Paragraph::new(lines).render(content_area, buf);

    let help_text = "up/down=select  1-3=quick pick  enter=confirm  esc=deny";
    let help_pad = (help_area.width as usize).saturating_sub(UnicodeWidthStr::width(help_text)) / 2;
    let help_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("{}{}", " ".repeat(help_pad), help_text),
            Style::default().fg(Color::Gray),
        )),
    ];
    Paragraph::new(help_lines).render(help_area, buf);
}
