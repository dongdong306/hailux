use std::borrow::Cow;

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

/// 弹窗固定开销（行数）：desc 下空行 + 选项 + 上下边框 + 帮助区 2 行。
/// 假设每个选项渲染只占 1 行：最长选项 27 字符 < 最小内宽 46（宽下限 50 - 边框 4）
const FIXED_LINES: usize = 1 + OPTIONS.len() + 4;

/// 按终端高度预算描述行：超出可显示行数时保留前 N-1 行，
/// 末尾追加一行省略提示（计数为实际隐藏的内容行数）
fn budget_desc_lines<'a>(all: Vec<Cow<'a, str>>, area_height: u16) -> Vec<Cow<'a, str>> {
    let max_desc_lines = (area_height as usize).saturating_sub(FIXED_LINES);
    let shown_count = max_desc_lines.min(all.len());
    if shown_count == all.len() {
        return all;
    }
    let keep = shown_count.saturating_sub(1);
    let mut shown = all[..keep].to_vec();
    shown.push(Cow::Owned(format!("… (+{} more lines)", all.len() - keep)));
    shown
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

    // 计算高度：命令可能非常长，换行后行数无上限，
    // 弹窗高度必须以终端高度为上限，否则底部选项会被切出屏幕
    let perm_line = format!("[{}] {}", request.permission, request.description);
    let all_desc_lines = wrap(&perm_line, inner_w);
    let desc_lines = budget_desc_lines(all_desc_lines, area.height);

    let dialog_height = (desc_lines.len() + FIXED_LINES).min(area.height as usize) as u16;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<Cow<'static, str>> {
        (0..n).map(|i| Cow::Owned(format!("line {i}"))).collect()
    }

    #[test]
    fn no_truncation_when_fits() {
        let out = budget_desc_lines(lines(5), (5 + FIXED_LINES) as u16);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0], "line 0");
        assert_eq!(out[4], "line 4");
    }

    #[test]
    fn truncates_and_counts_hidden_lines() {
        // 终端 30 行 → 预算 22 行：显示前 21 行 + 1 行省略提示
        let out = budget_desc_lines(lines(60), (22 + FIXED_LINES) as u16);
        assert_eq!(out.len(), 22);
        assert_eq!(out[0], "line 0");
        assert_eq!(out[20], "line 20");
        assert_eq!(out[21], "… (+39 more lines)");
    }

    #[test]
    fn very_short_terminal_keeps_only_notice() {
        // 终端高度等于固定开销，描述预算为 0：只剩省略提示行
        let out = budget_desc_lines(lines(30), FIXED_LINES as u16);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], "… (+30 more lines)");
    }

    #[test]
    fn empty_description_stays_empty() {
        let out = budget_desc_lines(Vec::new(), 30);
        assert!(out.is_empty());
    }
}
