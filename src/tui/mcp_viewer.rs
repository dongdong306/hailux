use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::mcp::{McpServerStatus, SharedMcpBackends, config::config_path_display};
use crate::tui::model_picker::wrap_input;

/// 渲染 MCP 服务器面板（二级页面，只读列表）。
///
/// 显示所有已配置的服务器及其连接状态、工具/资源/提示数量，失败的列出错误原因。
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

    // 每个服务器条目的高度：名称(1) + 状态行(1) + 工具/错误(1) + 空行(1) = 4
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

        // 状态图标
        let (icon, icon_color) = if server.connected {
            ("●", Color::Green)
        } else {
            ("✗", Color::Red)
        };

        // 第 1 行：图标 + 名称 + 传输
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

        // 第 2 行：服务器自报名称 / 版本
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

        // 第 3 行：工具/资源/提示统计 或 错误信息
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

        // 第 4 行：空行间距
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

/// 渲染单个 MCP 服务器的详情面板。
///
/// 展示该服务器的工具、资源、提示词列表。
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

    // 从 mcp_backends 获取工具描述
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

    // 统一构建条目列表：每个条目有 icon, name, 折行后的描述列表, 颜色
    // 列表视图中每个条目最多显示 2 行描述（截断）
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
            // 在最后一行末尾加省略号
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

    // 分段：tools / resources / prompts
    let tools_end = server.tools.len();
    let resources_end = tools_end + server.resources.len();

    // 每段信息
    let sections: &[(usize, usize, &str, Color)] = &[
        (0, tools_end, "工具", Color::Yellow),
        (tools_end, resources_end, "资源", Color::Blue),
        (resources_end, total, "提示词", Color::Magenta),
    ];

    // 计算每个条目占用的行数（1 name + N desc lines）
    let entry_height = |e: &DetailEntry| 1 + e.desc_lines.len();
    let section_header_height = 2; // 标题行 + 空行

    // 计算选中项的虚拟行偏移
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

    // 统一渲染所有段
    let mut current_row = 0usize;

    for &(start, end, label, accent) in sections {
        if start == end {
            continue;
        }

        // Section 标题行
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

        // 空行
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

            // 名称行
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

            // 描述行（可能多行）
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

    // 裁剪 lines 到 content_height
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

/// 渲染单个 MCP 工具/资源/提示词的详情页面。
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

    // 确定条目类型和内容
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

    // 类型标签行
    lines.push(Line::from(vec![
        Span::styled(format!("{} ", icon), Style::default().fg(accent)),
        Span::styled(
            format!("[{}]", type_label),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
    ]));

    // 名称行
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

    // 描述标题
    lines.push(Line::from(Span::styled(
        "描述",
        Style::default().fg(Color::DarkGray),
    )));

    // 描述内容（完整折行）
    for line in &wrapped_desc {
        lines.push(Line::from(Span::styled(
            format!(" {}", line),
            Style::default().fg(Color::White),
        )));
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
