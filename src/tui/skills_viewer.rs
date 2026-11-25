use std::path::Path;

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::agent::skill::SkillInfo;
use crate::tui::model_picker::wrap_input;

/// Renders the skill viewer secondary page (read-only list).
///
/// - `skills`: loaded skill list
/// - `selected_index`: currently selected entry
/// - `home_dir`: user home directory, used to tell each skill's origin (global/project)
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

    // Each skill: name(1) + description(up to 2 lines) + blank(1) = up to 4 lines
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

    // Scroll offset: keep the selected entry roughly centered (in 4-line blocks)
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

        // Line 1: name
        lines.push(Line::from(vec![
            Span::styled(selector, desc_style),
            Span::styled(skill.name.clone(), name_style),
            Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
        ]));

        // Lines 2-3: description (wrapped, at most 2 lines; truncated with an ellipsis when over)
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

        // blank spacing line
        lines.push(Line::from(""));
    }

    Paragraph::new(lines).render(content_area, buf);

    render_help(help_area, buf);
}

/// Renders the detail page of a single skill.
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

    // name
    lines.push(Line::from(vec![
        Span::styled("名称  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            skill.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // source
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

    // description
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

    // body content
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

    // scroll clipping
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

    // scroll indicator
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

    // help text
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

/// Whether the skill comes from the global directory (~/.hailux/skills/)
fn is_global(skill: &SkillInfo, home_dir: &Path) -> bool {
    let global_root = home_dir.join(".hailux").join("skills");
    // skill.location is already canonicalized; canonicalize here too for a correct comparison
    let global_root = match global_root.canonicalize() {
        Ok(p) => p,
        Err(_) => global_root,
    };
    skill.location.starts_with(&global_root)
}

/// Display the home directory, falling back to "~" on failure
fn display_home(home_dir: &Path) -> String {
    home_dir.display().to_string()
}
