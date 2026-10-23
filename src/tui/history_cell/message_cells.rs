use ratatui::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use unicode_width::UnicodeWidthStr;

use crate::tui::event::TaskStatus;
use crate::tui::markdown;

use super::{CHAT_FG, HistoryCell, PLAN_BADGE, finish_hasher, wrap_text};

/// 会话头部卡片：圆角边框显示模型名、工作目录
pub(crate) struct SessionHeaderCell {
    pub model_name: String,
    pub version: String,
    pub directory: String,
}

impl HistoryCell for SessionHeaderCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.model_name.hash(&mut h);
        self.version.hash(&mut h);
        self.directory.hash(&mut h);
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let max_inner = 56.min(width as usize).saturating_sub(4);
        let mut lines = Vec::new();

        let title = format!(" >_ hailux {}", self.version);
        let model_line = format!(" model:    {}", self.model_name);
        let dir_display = if self.directory.chars().count() > max_inner - 12 {
            let cut = max_inner - 15;
            let chars: Vec<char> = self.directory.chars().collect();
            let start = chars.len().saturating_sub(cut);
            let truncated: String = chars[start..].iter().collect();
            format!("...{}", truncated)
        } else {
            self.directory.clone()
        };
        let dir_line = format!(" directory: {}", dir_display);

        let title_w = UnicodeWidthStr::width(title.as_str());
        let model_w = UnicodeWidthStr::width(model_line.as_str());
        let dir_w = UnicodeWidthStr::width(dir_line.as_str());

        let inner_w = [title_w, model_w, dir_w]
            .into_iter()
            .max()
            .unwrap()
            .max(max_inner);

        let border_top = format!("╭{}╮", "─".repeat(inner_w + 2));
        let border_bot = format!("╰{}╯", "─".repeat(inner_w + 2));

        let dim = Style::default().fg(Color::Gray);
        let bold = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);

        lines.push(Line::from(Span::styled(border_top, dim)));
        lines.push(Line::from(vec![
            Span::styled("│ ", dim),
            Span::styled(title.clone(), bold),
            Span::styled(format!(" {}│", " ".repeat(inner_w - title_w)), dim),
        ]));
        lines.push(Line::from(vec![
            Span::styled("│ ", dim),
            Span::styled(model_line.clone(), Style::default().fg(Color::White)),
            Span::styled(format!(" {}│", " ".repeat(inner_w - model_w)), dim),
        ]));
        lines.push(Line::from(vec![
            Span::styled("│ ", dim),
            Span::styled(dir_line.clone(), Style::default().fg(Color::White)),
            Span::styled(format!(" {}│", " ".repeat(inner_w - dir_w)), dim),
        ]));
        lines.push(Line::from(Span::styled(border_bot, dim)));

        lines
    }
}

/// 用户消息：> 前缀 + 白色文本
pub(crate) struct UserMessageCell {
    pub text: String,
    pub plan_mode: bool,
}

impl HistoryCell for UserMessageCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.text.hash(&mut h);
        self.plan_mode.hash(&mut h);
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let prefix = "> ";
        let prefix_w = UnicodeWidthStr::width(prefix) as u16;
        let content_w = width.saturating_sub(prefix_w);

        let bar_color = if self.plan_mode { PLAN_BADGE } else { CHAT_FG };

        let mut lines = Vec::new();
        let wrapped = wrap_text(&self.text, content_w);

        for (i, line) in wrapped.iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(
                        prefix.to_string(),
                        Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        line.clone(),
                        Style::default().fg(CHAT_FG).add_modifier(Modifier::BOLD),
                    ),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        line.clone(),
                        Style::default().fg(CHAT_FG).add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
        }
        lines
    }
}

/// 助手消息（Markdown 渲染）
pub(crate) struct AgentMarkdownCell {
    pub text: String,
}

impl HistoryCell for AgentMarkdownCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.text.hash(&mut h);
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let content_w = width.saturating_sub(2);
        let md_lines = markdown::render_markdown(&self.text, content_w);

        let mut lines = Vec::new();
        for (i, md_line) in md_lines.iter().enumerate() {
            let mut spans = Vec::new();
            if i == 0 {
                spans.push(Span::styled("• ", Style::default().fg(Color::DarkGray)));
            } else {
                spans.push(Span::raw("  ".to_string()));
            }
            spans.extend(md_line.spans.iter().cloned());
            lines.push(Line::from(spans));
        }
        if lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines
    }
}

/// 帮助提示
pub(crate) struct TooltipCell {
    pub text: String,
}

impl HistoryCell for TooltipCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.text.hash(&mut h);
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let wrapped = wrap_text(&self.text, width.saturating_sub(2));
        let mut lines = Vec::new();
        for line in wrapped {
            lines.push(Line::from(Span::styled(
                format!("  {}", line),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines
    }
}

/// 完成耗时标记
pub(crate) struct DoneCell {
    pub total_ms: u64,
    pub model: String,
    pub status: TaskStatus,
    pub plan_mode: bool,
}

impl HistoryCell for DoneCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.total_ms.hash(&mut h);
        self.model.hash(&mut h);
        self.status.hash(&mut h);
        self.plan_mode.hash(&mut h);
        finish_hasher(h)
    }

    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let (icon, label, icon_color, text_color) = match self.status {
            TaskStatus::Completed => (
                "◆",
                "Done",
                if self.plan_mode {
                    PLAN_BADGE
                } else {
                    Color::Cyan
                },
                Color::DarkGray,
            ),
            TaskStatus::Interrupted => ("◇", "Interrupted", Color::Yellow, Color::DarkGray),
            TaskStatus::Error => ("◆", "Error", Color::Red, Color::DarkGray),
        };
        let icon_style = Style::default().fg(icon_color);
        let text_style = Style::default().fg(text_color).add_modifier(Modifier::DIM);
        vec![Line::from(vec![
            Span::styled(format!("{icon} "), icon_style),
            Span::styled(
                format!(
                    "{} · {} · {:.1}s",
                    label,
                    self.model,
                    self.total_ms as f64 / 1000.0
                ),
                text_style,
            ),
        ])]
    }
}

/// 思维链消息：dim + italic 样式
pub(crate) struct ReasoningCell {
    pub text: String,
    pub collapsed: bool,
    pub think_ms: Option<u64>,
    pub thinking_started_at: Option<std::time::Instant>,
}

const COLLAPSED_PREVIEW_LINES: usize = 6;

impl HistoryCell for ReasoningCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.text.hash(&mut h);
        self.collapsed.hash(&mut h);
        self.think_ms.hash(&mut h);
        if let Some(t) = self.thinking_started_at {
            t.elapsed().as_secs().saturating_div(3).hash(&mut h);
        }
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let content_w = width.saturating_sub(2);
        let style = Style::default().dim().italic();

        // 标题行：◈ Thinking (实时计时) / ◈ Thought (最终耗时)
        let title = if let Some(ms) = self.think_ms {
            format!("◈ Thought ({:.1}s)", ms as f64 / 1000.0)
        } else if let Some(t) = self.thinking_started_at {
            format!("◈ Thinking ({:.1}s)", t.elapsed().as_secs_f64())
        } else {
            "◈ Thinking".to_string()
        };
        let title_style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD);
        let mut lines = vec![Line::from(Span::styled(title, title_style))];

        if self.collapsed {
            let total_text_lines = self.text.lines().count();

            if total_text_lines <= COLLAPSED_PREVIEW_LINES {
                // 完整展示，无前缀（标题已占用 ◈）
                let md_lines = markdown::render_markdown(&self.text, content_w);
                for md_line in &md_lines {
                    let mut spans = vec![Span::styled("  ".to_string(), style)];
                    spans.extend(
                        md_line
                            .spans
                            .iter()
                            .map(|s| Span::styled(s.content.clone(), s.style.patch(style))),
                    );
                    lines.push(Line::from(spans));
                }
                return lines;
            }

            // 截断展示
            let preview_text: String = self.text.chars().take(1200).collect();
            let md_lines = markdown::render_markdown(&preview_text, content_w);
            for md_line in md_lines.iter().take(COLLAPSED_PREVIEW_LINES) {
                let mut spans = vec![Span::styled("  ".to_string(), style)];
                spans.extend(
                    md_line
                        .spans
                        .iter()
                        .map(|s| Span::styled(s.content.clone(), s.style.patch(style))),
                );
                lines.push(Line::from(spans));
            }
            let remaining = total_text_lines.saturating_sub(COLLAPSED_PREVIEW_LINES);
            let hint = format!("  ... 剩余约 {} 行 (Ctrl+O 展开)", remaining);
            lines.push(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            let total_text_lines = self.text.lines().count();
            let md_lines = markdown::render_markdown(&self.text, content_w);
            for md_line in &md_lines {
                let mut spans = vec![Span::styled("  ".to_string(), style)];
                spans.extend(
                    md_line
                        .spans
                        .iter()
                        .map(|s| Span::styled(s.content.clone(), s.style.patch(style))),
                );
                lines.push(Line::from(spans));
            }
            if total_text_lines > COLLAPSED_PREVIEW_LINES {
                lines.push(Line::from(Span::styled(
                    "  ... Ctrl+O 收起",
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        lines
    }
}

pub(super) struct DividerCell {
    pub(super) label: String,
    pub(super) hash_seed: u64,
}

impl HistoryCell for DividerCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.label.hash(&mut h);
        self.hash_seed.hash(&mut h);
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let dim = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM);

        let label_w = UnicodeWidthStr::width(self.label.as_str());
        let total_w = width as usize;

        if label_w >= total_w {
            vec![Line::from(Span::styled(self.label.to_string(), dim))]
        } else {
            vec![Line::from(vec![
                Span::styled(self.label.to_string(), dim),
                Span::styled("─".repeat(total_w - label_w), dim),
            ])]
        }
    }
}
