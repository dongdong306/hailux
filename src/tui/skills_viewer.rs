use std::path::Path;

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::agent::skill::SkillInfo;
use crate::tui::model_picker::wrap_input;

/// 渲染 skill 查看器二级页面（只读列表）。
///
/// - `skills`: 已加载的 skill 列表
/// - `selected_index`: 当前选中项
/// - `home_dir`: 用户主目录，用于判断每个 skill 的来源（全局/项目）
pub fn render_skills_viewer(
    area: Rect,
    buf: &mut Buffer,
    skills: &[SkillInfo],
    selected_index: usize,
    home_dir: &Path,
) {
    let dialog_width = (area.width as usize).min(70);
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
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" 已加载的 Skills ({}) ", skills.len()),
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

    // 每个 skill：名称(1) + 描述(最多2行) + 空行(1) = 最多 4 行
    const ITEM_LINES: usize = 4;
    const MAX_DESC_LINES: usize = 2;

    let mut lines: Vec<Line> = Vec::new();

    if skills.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  未发现任何 skill",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  将 skill 放置在以下目录下（含 SKILL.md）：",
            Style::default().fg(Color::Gray),
        )));
        lines.push(Line::from(Span::styled(
            format!("    全局: {}/.hailux/skills/", display_home(home_dir)),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "    项目: ./.hailux/skills/",
            Style::default().fg(Color::DarkGray),
        )));
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
        render_help(help_area, buf);
        return;
    }

    // 滚动偏移：选中项尽量居中（以 4 行块为单位）
    let visible_items = content_height / ITEM_LINES;
    let first_visible = if selected_index >= visible_items {
        selected_index - visible_items / 2
    } else {
        0
    };

    let desc_width = content_width.saturating_sub(2); // "  " prefix

    for (i, skill) in skills.iter().enumerate() {
        if i < first_visible {
            continue;
        }
        if lines.len() >= content_height.saturating_sub(ITEM_LINES) + ITEM_LINES {
            break;
        }

        let is_selected = i == selected_index;

        let name_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        };
        let desc_style = if is_selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        let fill_bg = if is_selected {
            Color::Cyan
        } else {
            Color::Reset
        };

        let selector = if is_selected { "▸ " } else { "  " };

        // 第 1 行：名称
        lines.push(Line::from(vec![
            Span::styled(selector, desc_style),
            Span::styled(skill.name.clone(), name_style),
            Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
        ]));

        // 第 2-3 行：描述（折行，最多 2 行，超出截断加省略号）
        let desc_text = if skill.description.is_empty() {
            "(无描述)".to_string()
        } else {
            skill.description.clone()
        };
        let (wrapped, _, _) = wrap_input(&desc_text, 0, desc_width);
        let display_lines: Vec<String> = if wrapped.len() > MAX_DESC_LINES {
            let mut result: Vec<String> = wrapped.into_iter().take(MAX_DESC_LINES).collect();
            if let Some(last) = result.last_mut()
                && !last.is_empty()
            {
                last.pop();
                last.push('…');
            }
            result
        } else {
            wrapped
        };

        for line in &display_lines {
            lines.push(Line::from(vec![
                Span::styled("  ", desc_style),
                Span::styled(line.clone(), desc_style),
                Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
            ]));
        }

        // 空行间距
        lines.push(Line::from(""));
    }

    Paragraph::new(lines).render(content_area, buf);

    render_help(help_area, buf);
}

/// 渲染单个 skill 的详情页面。
pub fn render_skill_detail(
    area: Rect,
    buf: &mut Buffer,
    skills: &[SkillInfo],
    skill_index: usize,
    scroll_offset: usize,
    home_dir: &Path,
) {
    let skill = match skills.get(skill_index) {
        Some(s) => s,
        None => return,
    };

    let dialog_width = (area.width as usize).min(76);
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
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" {} ", skill.name),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center);

    let inner = block.inner(dialog_area);
    block.render(dialog_area, buf);

    let [content_area, help_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);

    let content_width = content_area.width as usize;
    let desc_width = content_width.saturating_sub(2); // " " prefix

    let mut lines: Vec<Line> = Vec::new();

    // 名称
    lines.push(Line::from(vec![
        Span::styled("名称  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            skill.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // 来源
    let source_tag = if is_global(skill, home_dir) {
        "全局"
    } else {
        "项目"
    };
    lines.push(Line::from(vec![
        Span::styled("来源  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("[{}] ", source_tag),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            skill.location.display().to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    lines.push(Line::from(""));

    // 描述
    lines.push(Line::from(Span::styled(
        "描述",
        Style::default().fg(Color::DarkGray),
    )));

    let desc_text = if skill.description.is_empty() {
        "(无描述)".to_string()
    } else {
        skill.description.clone()
    };
    let (wrapped, _, _) = wrap_input(&desc_text, 0, desc_width);
    for line in &wrapped {
        lines.push(Line::from(Span::styled(
            format!(" {}", line),
            Style::default().fg(Color::White),
        )));
    }

    // 正文内容
    if !skill.content.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "正文",
            Style::default().fg(Color::DarkGray),
        )));
        for raw_line in skill.content.lines() {
            if raw_line.is_empty() {
                lines.push(Line::from(""));
            } else {
                let (content_wrapped, _, _) = wrap_input(raw_line, 0, desc_width);
                for line in &content_wrapped {
                    lines.push(Line::from(Span::styled(
                        format!(" {}", line),
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
        }
    }

    // 滚动裁剪
    let total_lines = lines.len();
    let content_height = content_area.height as usize;
    let max_scroll = total_lines.saturating_sub(content_height);
    let actual_scroll = scroll_offset.min(max_scroll);

    let visible_lines: Vec<Line> = lines
        .into_iter()
        .skip(actual_scroll)
        .take(content_height)
        .collect();

    Paragraph::new(visible_lines).render(content_area, buf);

    // 滚动指示器
    if max_scroll > 0 {
        let indicator = if actual_scroll > 0 && actual_scroll < max_scroll {
            " ↑↓ "
        } else if actual_scroll > 0 {
            " ↑ "
        } else {
            " ↓ "
        };
        let ind_x = content_area.x + content_area.width.saturating_sub(indicator.len() as u16);
        let ind_y = content_area.y;
        buf.set_string(
            ind_x,
            ind_y,
            indicator,
            Style::default().fg(Color::DarkGray),
        );
    }

    // 帮助文本
    let help_text = if max_scroll > 0 {
        "↑↓ 切换  PgUp/PgDn 翻页  Esc 返回"
    } else {
        "↑↓ 切换  Esc 返回"
    };
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

/// 判断 skill 是否来自全局目录（~/.hailux/skills/）
fn is_global(skill: &SkillInfo, home_dir: &Path) -> bool {
    let global_root = home_dir.join(".hailux").join("skills");
    // skill.location 已经过 canonicalize，这里也需要 canonicalize 才能正确比较
    let global_root = match global_root.canonicalize() {
        Ok(p) => p,
        Err(_) => global_root,
    };
    skill.location.starts_with(&global_root)
}

/// 显示主目录，失败时回退到 "~"
fn display_home(home_dir: &Path) -> String {
    home_dir.display().to_string()
}
