use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::mcp::{McpServerStatus, SharedMcpBackends, config::config_path_display};
use crate::tui::model_picker::wrap_input;

/// Renders the MCP servers panel (secondary page, read-only list).
///
/// Shows all configured servers with their connection status and tool/resource/prompt
/// counts; failures list the error reason.
pub fn render_mcp_viewer(
    area: Rect,
    buf: &mut Buffer,
    servers: &[McpServerStatus],
    selected_index: usize,
) {
    let dialog_width = (area.width as usize).min(76);
    let dialog_height = ((area.height as usize).saturating_sub(4) * 8 / 10).clamp(20, 48);
    let dialog_x = (area.width as usize).saturating_sub(dialog_width) / 2;
    let dialog_y = (area.height as usize).saturating_sub(dialog_height) / 2;

    let dialog_area = Rect::new(
        dialog_x as u16,
        dialog_y as u16,
        dialog_width as u16,
        dialog_height as u16,
    );

    Clear.render(dialog_area, buf);

    let connected_count = servers.iter().filter(|s| s.connected).count();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray))
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" MCP 服务器 ({}/{}) ", connected_count, servers.len()),
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

    let mut lines: Vec<Line> = Vec::new();

    if servers.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  未配置任何 MCP 服务器",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  在以下文件中配置 MCP 服务器：",
            Style::default().fg(Color::Gray),
        )));
        lines.push(Line::from(Span::styled(
            format!("    {}", config_path_display()),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  示例：",
            Style::default().fg(Color::Gray),
        )));
        lines.push(Line::from(Span::styled(
            "  [mcp_servers.context7]",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  command = \"npx\"",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  args = [\"-y\", \"@upstash/context7-mcp\"]",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  [mcp_servers.remote]",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "  url = \"https://example.com/mcp\"",
            Style::default().fg(Color::DarkGray),
        )));
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
        render_help(help_area, buf);
        return;
    }

    // Height of each server entry: name(1) + status line(1) + tools/error(1) + blank(1) = 4
    const ITEM_LINES: usize = 4;
    let visible_items = content_height / ITEM_LINES;

    let first_visible = if selected_index >= visible_items {
        selected_index - visible_items / 2
    } else {
        0
    };

    for (i, server) in servers.iter().enumerate() {
        if i < first_visible {
            continue;
        }
        if lines.len() >= content_height {
            break;
        }

        let is_selected = i == selected_index;

        let sel_bg = if is_selected {
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
            .bg(sel_bg)
            .add_modifier(Modifier::BOLD);
        let dim_style = Style::default()
            .fg(if is_selected {
                Color::Black
            } else {
                Color::DarkGray
            })
            .bg(sel_bg);

        let selector = if is_selected { "▸ " } else { "  " };
        let fill_bg = if is_selected {
            Color::Cyan
        } else {
            Color::Reset
        };

        // status icon
        let (icon, icon_color) = if server.connected {
            ("●", Color::Green)
        } else {
            ("✗", Color::Red)
        };

        // Line 1: icon + name + transport
        let mut line1 = vec![
            Span::styled(selector, dim_style),
            Span::styled(
                icon,
                Style::default()
                    .fg(icon_color)
                    .bg(sel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {} ", server.name), name_style),
        ];
        line1.push(Span::styled(format!("[{}]", server.transport), dim_style));
        line1.push(Span::styled(
            " ".repeat(content_area.width as usize),
            Style::default().bg(fill_bg),
        ));
        lines.push(Line::from(line1));

        // Line 2: server-reported name / version
        let detail = if server.connected {
            if server.server_name.is_empty() {
                "已连接".to_string()
            } else {
                format!("{} {}", server.server_name, server.server_version)
            }
        } else {
            "未连接".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled("  ", dim_style),
            Span::styled(detail, dim_style),
            Span::styled(
                " ".repeat(content_area.width as usize),
                Style::default().bg(fill_bg),
            ),
        ]));

        // Line 3: tool/resource/prompt counts or error message
        if server.connected {
            let mut parts = vec![format!("工具: {}", server.tools.len())];
            if server.resource_count > 0 {
                parts.push(format!("资源: {}", server.resource_count));
            }
            if server.prompt_count > 0 {
                parts.push(format!("提示: {}", server.prompt_count));
            }
            let tools_line = if server.tools.is_empty() {
                parts.join("  ·  ")
            } else {
                format!("{}  ·  {}", parts.join("  ·  "), server.tools.join(", "))
            };
            lines.push(Line::from(vec![
                Span::styled("  ", dim_style),
                Span::styled(tools_line, dim_style),
                Span::styled(
                    " ".repeat(content_area.width as usize),
                    Style::default().bg(fill_bg),
                ),
            ]));
        } else if let Some(err) = &server.error {
            let is_connecting = err == "连接中...";
            let err_fg = if is_connecting {
                Color::Yellow
            } else {
                Color::Red
            };
            lines.push(Line::from(vec![
                Span::styled("  ", dim_style),
                Span::styled(err, Style::default().fg(err_fg).bg(sel_bg)),
                Span::styled(
                    " ".repeat(content_area.width as usize),
                    Style::default().bg(fill_bg),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                " ".repeat(content_area.width as usize),
                Style::default().bg(fill_bg),
            )]));
        }

        // Line 4: blank spacing line
        lines.push(Line::from(""));
    }

    Paragraph::new(lines).render(content_area, buf);

    render_help(help_area, buf);
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

/// Renders the detail panel of a single MCP server.
///
/// Shows the server's tool, resource, and prompt lists.
#[allow(clippy::too_many_arguments)]
pub fn render_mcp_detail(
    area: Rect,
    buf: &mut Buffer,
    servers: &[McpServerStatus],
    server_index: usize,
    selected_index: usize,
    mcp_backends: &SharedMcpBackends,
) {
    let server = match servers.get(server_index) {
        Some(s) => s,
        None => return,
    };

    let dialog_width = (area.width as usize).min(76);
    let dialog_height = ((area.height as usize).saturating_sub(4) * 8 / 10).clamp(20, 48);
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
            format!(" {} ", server.name),
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

    // Fetch tool descriptions from mcp_backends
    let tool_descs: std::collections::HashMap<String, String> = {
        let mut map = std::collections::HashMap::new();
        if let Ok(backends) = mcp_backends.lock()
            && let Some(backend) = backends.iter().find(|b| b.server_name == server.name)
        {
            for tool in &backend.tools {
                let desc = tool.description.as_deref().unwrap_or("").to_string();
                map.insert(tool.name.to_string(), desc);
            }
        }
        map
    };

    // Build the entry list uniformly: each entry has icon, name, wrapped description lines, color
    // In the list view each entry shows at most 2 description lines (truncated)
    const MAX_DESC_LINES: usize = 2;
    struct DetailEntry {
        icon: char,
        name: String,
        desc_lines: Vec<String>,
    }

    let desc_width = content_width.saturating_sub(3); // "  " prefix + content

    let truncate_desc = |desc: &str| -> Vec<String> {
        if desc.is_empty() {
            return vec!["(无描述)".to_string()];
        }
        let (wrapped, _, _) = wrap_input(desc, 0, desc_width);
        if wrapped.len() > MAX_DESC_LINES {
            let mut result: Vec<String> = wrapped.into_iter().take(MAX_DESC_LINES).collect();
            // append an ellipsis at the end of the last line
            let last = result.last_mut().unwrap();
            if !last.is_empty() {
                last.pop();
                last.push('…');
            }
            result
        } else {
            wrapped
        }
    };

    let mut entries: Vec<DetailEntry> = Vec::new();

    for tool_name in &server.tools {
        let desc = tool_descs.get(tool_name).cloned().unwrap_or_default();
        entries.push(DetailEntry {
            icon: '🔧',
            name: tool_name.clone(),
            desc_lines: truncate_desc(&desc),
        });
    }
    for (name, desc) in &server.resources {
        entries.push(DetailEntry {
            icon: '📄',
            name: name.clone(),
            desc_lines: truncate_desc(desc),
        });
    }
    for (name, desc) in &server.prompts {
        entries.push(DetailEntry {
            icon: '💬',
            name: name.clone(),
            desc_lines: truncate_desc(desc),
        });
    }

    let total = entries.len();

    let mut lines: Vec<Line> = Vec::new();

    if total == 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  此服务器没有暴露任何工具、资源或提示词",
            Style::default().fg(Color::Gray),
        )));
        Paragraph::new(lines).render(content_area, buf);
        render_detail_help(help_area, buf);
        return;
    }

    // Sections: tools / resources / prompts
    let tools_end = server.tools.len();
    let resources_end = tools_end + server.resources.len();

    // Info per section
    let sections: &[(usize, usize, &str, Color)] = &[
        (0, tools_end, "工具", Color::Yellow),
        (tools_end, resources_end, "资源", Color::Blue),
        (resources_end, total, "提示词", Color::Magenta),
    ];

    // Compute the number of rows each entry occupies (1 name + N desc lines)
    let entry_height = |e: &DetailEntry| 1 + e.desc_lines.len();
    let section_header_height = 2; // title line + blank line

    // Compute the virtual row offset of the selected entry
    let selected_virtual_row = {
        let mut row = 0usize;
        for &(start, end, _, _) in sections {
            if start == end {
                continue;
            }
            row += section_header_height;
            for (i, entry) in entries.iter().enumerate().take(end).skip(start) {
                if i == selected_index {
                    break;
                }
                row += entry_height(entry);
            }
            if selected_index >= start && selected_index < end {
                break;
            }
        }
        row
    };

    let scroll_offset = if selected_virtual_row >= content_height {
        selected_virtual_row - content_height / 2
    } else {
        0
    };

    // Render all sections uniformly
    let mut current_row = 0usize;

    for &(start, end, label, accent) in sections {
        if start == end {
            continue;
        }

        // Section title line
        let title = format!(" {} ({})", label, end - start);
        if current_row >= scroll_offset && current_row < scroll_offset + content_height {
            let title_w = UnicodeWidthStr::width(title.as_str());
            lines.push(Line::from(vec![
                Span::styled(
                    title,
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "─".repeat(content_width.saturating_sub(title_w)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        } else {
            lines.push(Line::from(""));
        }
        current_row += 1;

        // blank line
        lines.push(Line::from(""));
        current_row += 1;

        for (i, entry) in entries.iter().enumerate().take(end).skip(start) {
            let is_selected = i == selected_index;

            let (name_style, dim_style, fill_bg) = if is_selected {
                (
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                    Color::Cyan,
                )
            } else {
                (
                    Style::default().fg(Color::White),
                    Style::default().fg(Color::Gray),
                    Color::Reset,
                )
            };

            let selector = if is_selected { "▸ " } else { "  " };

            // name line
            if current_row >= scroll_offset && current_row < scroll_offset + content_height {
                lines.push(Line::from(vec![
                    Span::styled(selector, dim_style),
                    Span::styled(format!("{} ", entry.icon), dim_style),
                    Span::styled(entry.name.clone(), name_style),
                    Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
                ]));
            } else {
                lines.push(Line::from(""));
            }
            current_row += 1;

            // description lines (possibly multiple)
            for desc_line in &entry.desc_lines {
                if current_row >= scroll_offset && current_row < scroll_offset + content_height {
                    lines.push(Line::from(vec![
                        Span::styled("  ", dim_style),
                        Span::styled(desc_line.clone(), dim_style),
                        Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
                    ]));
                } else {
                    lines.push(Line::from(""));
                }
                current_row += 1;
            }
        }
    }

    // Clip lines to content_height
    let visible_lines: Vec<Line> = lines
        .into_iter()
        .skip(scroll_offset)
        .take(content_height)
        .collect();

    Paragraph::new(visible_lines).render(content_area, buf);

    render_detail_help(help_area, buf);
}

fn render_detail_help(area: Rect, buf: &mut Buffer) {
    let help_text = "↑↓ 移动  Enter 详情  Esc 返回";
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

/// Renders the detail page of a single MCP tool/resource/prompt.
#[allow(clippy::too_many_arguments)]
pub fn render_mcp_item_detail(
    area: Rect,
    buf: &mut Buffer,
    servers: &[McpServerStatus],
    server_index: usize,
    item_index: usize,
    scroll_offset: usize,
    mcp_backends: &SharedMcpBackends,
) {
    let server = match servers.get(server_index) {
        Some(s) => s,
        None => return,
    };

    let tools_end = server.tools.len();
    let resources_end = tools_end + server.resources.len();

    // Determine the entry type and content
    let (icon, name, desc, accent, type_label) = if item_index < tools_end {
        let tool_name = &server.tools[item_index];
        let tool_desc = {
            let mut map = std::collections::HashMap::new();
            if let Ok(backends) = mcp_backends.lock()
                && let Some(backend) = backends.iter().find(|b| b.server_name == server.name)
            {
                for tool in &backend.tools {
                    let d = tool.description.as_deref().unwrap_or("").to_string();
                    map.insert(tool.name.to_string(), d);
                }
            }
            map.get(tool_name).cloned().unwrap_or_default()
        };
        ('🔧', tool_name.clone(), tool_desc, Color::Yellow, "工具")
    } else if item_index < resources_end {
        let idx = item_index - tools_end;
        let (n, d) = &server.resources[idx];
        ('📄', n.clone(), d.clone(), Color::Blue, "资源")
    } else {
        let idx = item_index - resources_end;
        let (n, d) = &server.prompts[idx];
        ('💬', n.clone(), d.clone(), Color::Magenta, "提示词")
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

    let title = format!(" {} / {} ", server.name, name);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray))
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            title,
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

    let (wrapped_desc, _, _) = wrap_input(
        if desc.is_empty() {
            "(无描述)"
        } else {
            &desc
        },
        0,
        desc_width,
    );

    let mut lines: Vec<Line> = Vec::new();

    // type label line
    lines.push(Line::from(vec![
        Span::styled(format!("{} ", icon), Style::default().fg(accent)),
        Span::styled(
            format!("[{}]", type_label),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
    ]));

    // name line
    lines.push(Line::from(vec![
        Span::styled("名称  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    lines.push(Line::from(""));

    // description title
    lines.push(Line::from(Span::styled(
        "描述",
        Style::default().fg(Color::DarkGray),
    )));

    // description content (fully wrapped)
    for line in &wrapped_desc {
        lines.push(Line::from(Span::styled(
            format!(" {}", line),
            Style::default().fg(Color::White),
        )));
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
