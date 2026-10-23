use ratatui::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;

use super::ask_user_render::render_ask_user_result;
use super::diff::render_diff_from_json;
use super::{
    HistoryCell, ToolCategory, finish_hasher, tool_call_summary, tool_result_summary, wrap_text,
};

/// 工具调用：模仿 Codex 风格
///   • Reading /path/to/file.rs
///     └ 42 lines
///   • Ran echo done
///     └ output line 1
///       ... +3 lines
pub(crate) struct ToolCell {
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
    pub display: Option<String>,
}

impl ToolCell {
    fn category(&self) -> ToolCategory {
        ToolCategory::from_name(&self.name)
    }
}

impl HistoryCell for ToolCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.name.hash(&mut h);
        self.arguments.hash(&mut h);
        self.result.hash(&mut h);
        self.display.hash(&mut h);
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let cat = self.category();
        let color = cat.color();
        let has_result = self.result.is_some();

        // 第一行：• + 动词 + 摘要
        let verb = if has_result {
            cat.verb_done()
        } else {
            cat.verb_active()
        };
        let summary = tool_call_summary(&self.name, &self.arguments);

        let bullet = if has_result {
            Span::styled(
                "•".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "•".to_string(),
                Style::default().fg(color).add_modifier(Modifier::DIM),
            )
        };

        let verb_span = Span::styled(
            verb.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        );

        let header_line = if summary.is_empty() {
            Line::from(vec![bullet, Span::raw(" "), verb_span])
        } else {
            Line::from(vec![
                bullet,
                Span::raw(" "),
                verb_span,
                Span::raw(" "),
                Span::styled(summary, Style::default().fg(color)),
            ])
        };

        let mut lines = vec![header_line];

        // 如果有 display（diff JSON），优先渲染
        if let Some(ref display) = self.display {
            let diff_lines = render_diff_from_json(display, width);
            lines.extend(diff_lines);
        } else if let Some(ref result) = self.result {
            if cat == ToolCategory::AskUser {
                lines.extend(render_ask_user_result(&self.arguments, result, width));
            } else {
                // 结果行：  └ summary（dim）
                let result_summary = tool_result_summary(&self.name, result);
                if !result_summary.is_empty() {
                    let detail_w = width.saturating_sub(4) as usize;
                    let wrapped = wrap_text(&result_summary, detail_w.max(1) as u16);
                    for (i, line) in wrapped.iter().enumerate() {
                        if i == 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  └ {}", line),
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::DIM),
                            )));
                        } else {
                            lines.push(Line::from(Span::styled(
                                format!("    {}", line),
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::DIM),
                            )));
                        }
                    }
                }
            }
        }

        lines
    }
}

/// 独立的 SubagentStep（无父 task call）
pub(super) struct PlainStepCell {
    pub(super) summary: String,
    pub(super) is_done: bool,
    /// 多 subagent 并发时显示来源名称，如 "[reviewer]"
    pub(super) agent: Option<String>,
}

impl HistoryCell for PlainStepCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.summary.hash(&mut h);
        self.is_done.hash(&mut h);
        self.agent.hash(&mut h);
        finish_hasher(h)
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let icon = if self.is_done { "✓" } else { "●" };
        let icon_color = if self.is_done {
            Color::DarkGray
        } else {
            Color::Yellow
        };
        let prefix_w = self.agent.as_ref().map_or(0, |a| a.chars().count() + 4);
        let step_w = width.saturating_sub(4 + prefix_w as u16) as usize;
        let wrapped = wrap_text(&self.summary, step_w.max(1) as u16);
        let mut lines = Vec::new();
        for (li, wline) in wrapped.iter().enumerate() {
            if li == 0 {
                let mut spans = vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(icon, Style::default().fg(icon_color)),
                ];
                if let Some(ref agent) = self.agent {
                    spans.push(Span::styled(
                        format!(" [{agent}]"),
                        Style::default().fg(ToolCategory::Task.color()),
                    ));
                }
                spans.push(Span::styled(
                    format!(" {wline}"),
                    Style::default().fg(Color::DarkGray),
                ));
                lines.push(Line::from(spans));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("    {}", wline),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        lines
    }
}

/// 连续只读工具调用合并展示（模仿 Codex Exploring/Explored）
pub(crate) struct ExploringCell {
    pub items: Vec<ExploreItem>,
    pub all_done: bool,
}

pub(crate) struct ExploreItem {
    pub label: &'static str,
    pub detail: String,
}

impl HistoryCell for ExploringCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.all_done.hash(&mut h);
        self.items.len().hash(&mut h);
        for item in &self.items {
            item.label.hash(&mut h);
            item.detail.hash(&mut h);
        }
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let verb = if self.all_done {
            "Explored"
        } else {
            "Exploring"
        };
        let bullet = if self.all_done {
            Span::styled(
                "•".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "•".to_string(),
                Style::default().add_modifier(Modifier::DIM),
            )
        };

        let mut lines = vec![Line::from(vec![
            bullet,
            Span::raw(" "),
            Span::styled(
                verb.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ])];

        let detail_w = width.saturating_sub(4) as usize;
        for item in &self.items {
            let display = if item.detail.is_empty() {
                item.label.to_string()
            } else {
                format!("{} {}", item.label, item.detail)
            };
            let wrapped = wrap_text(&display, detail_w.max(1) as u16);
            for (i, line) in wrapped.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(Span::styled(
                        format!("  └ {}", line),
                        Style::default().fg(Color::Cyan),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        format!("    {}", line),
                        Style::default().fg(Color::Cyan),
                    )));
                }
            }
        }

        lines
    }
}

/// 判断是否为只读工具（可合并为 Exploring）
pub(super) fn is_readonly_tool(name: &str) -> bool {
    matches!(
        ToolCategory::from_name(name),
        ToolCategory::FileRead | ToolCategory::Search | ToolCategory::Glob | ToolCategory::WebFetch
    )
}

/// 从只读工具参数中提取 Exploring 子项标签和摘要
pub(super) fn explore_item_from(name: &str, arguments: &str, result: Option<&str>) -> ExploreItem {
    let cat = ToolCategory::from_name(name);
    let label = cat.verb_done();
    let mut detail = tool_call_summary(name, arguments);
    if let Some(res) = result {
        let rs = tool_result_summary(name, res);
        if !rs.is_empty() {
            if !detail.is_empty() {
                detail = format!("{} — {}", detail, rs);
            } else {
                detail = rs;
            }
        }
    }
    ExploreItem { label, detail }
}
