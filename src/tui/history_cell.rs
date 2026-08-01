use ratatui::prelude::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use unicode_width::UnicodeWidthStr;

use super::app::types::Message;
use super::event::TaskStatus;
use super::markdown;

pub(crate) trait HistoryCell: Send + Sync {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;
    /// 基于内容计算缓存键，用于跳过未变更 cell 的重复渲染
    fn cache_key(&self) -> u64;
}

fn finish_hasher(h: DefaultHasher) -> u64 {
    h.finish()
}

/// 工具分类，每种工具拥有独立的动词、颜色和显示标签
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ToolCategory {
    FileRead,
    FileWrite,
    FileEdit,
    Search,
    Glob,
    WebFetch,
    Command,
    Todo,
    Mcp,
    Skill,
    AskUser,
    Task,
    Other,
}

impl ToolCategory {
    pub fn from_name(name: &str) -> Self {
        match name {
            "read" => Self::FileRead,
            "write" => Self::FileWrite,
            "edit" => Self::FileEdit,
            "grep" => Self::Search,
            "glob" => Self::Glob,
            "web_fetch" => Self::WebFetch,
            "bash" => Self::Command,
            "todo_write" => Self::Todo,
            "ask_user" => Self::AskUser,
            "skill" => Self::Skill,
            "task" => Self::Task,
            n if n.starts_with("mcp_") => Self::Mcp,
            _ => Self::Other,
        }
    }

    /// 进行中动词（如 Codex 的 "Calling" / "Running" / "Exploring"）
    pub fn verb_active(&self) -> &'static str {
        match self {
            Self::FileRead => "Reading",
            Self::FileWrite => "Writing",
            Self::FileEdit => "Editing",
            Self::Search => "Searching",
            Self::Glob => "Listing",
            Self::WebFetch => "Fetching",
            Self::Command => "Running",
            Self::Todo => "Updating",
            Self::Mcp => "Calling",
            Self::Skill => "Load skill",
            Self::AskUser => "Asking",
            Self::Task => "Delegating",
            Self::Other => "Running",
        }
    }

    /// 完成动词（如 Codex 的 "Called" / "Ran" / "Explored"）
    pub fn verb_done(&self) -> &'static str {
        match self {
            Self::FileRead => "Read",
            Self::FileWrite => "Wrote",
            Self::FileEdit => "Edited",
            Self::Search => "Searched",
            Self::Glob => "Listed",
            Self::WebFetch => "Fetched",
            Self::Command => "Ran",
            Self::Todo => "Updated",
            Self::Mcp => "Called",
            Self::Skill => "Load skill",
            Self::AskUser => "Asked",
            Self::Task => "Delegated",
            Self::Other => "Ran",
        }
    }

    /// 主题色（活跃/成功状态）
    pub fn color(&self) -> Color {
        match self {
            Self::FileRead => Color::Cyan,
            Self::FileWrite | Self::FileEdit => Color::Green,
            Self::Search => Color::Yellow,
            Self::Glob => Color::Magenta,
            Self::WebFetch => Color::Blue,
            Self::Command => Color::Rgb(255, 160, 60),
            Self::Todo => Color::Cyan,
            Self::Mcp => Color::Rgb(180, 120, 255),
            Self::Skill => Color::Rgb(100, 200, 150),
            Self::AskUser => Color::Cyan,
            Self::Task => Color::Rgb(100, 180, 255),
            Self::Other => Color::Gray,
        }
    }
}

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

        let bar_color = if self.plan_mode {
            PLAN_BADGE
        } else {
            CHAT_FG
        };

        let mut lines = Vec::new();
        let wrapped = wrap_text(&self.text, content_w);

        for (i, line) in wrapped.iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(prefix.to_string(), Style::default().fg(bar_color).add_modifier(Modifier::BOLD)),
                    Span::styled(line.clone(), Style::default().fg(CHAT_FG).add_modifier(Modifier::BOLD)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(line.clone(), Style::default().fg(CHAT_FG).add_modifier(Modifier::BOLD)),
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
struct PlainStepCell {
    summary: String,
    is_done: bool,
}

impl HistoryCell for PlainStepCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.summary.hash(&mut h);
        self.is_done.hash(&mut h);
        finish_hasher(h)
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let icon = if self.is_done { "✓" } else { "●" };
        let icon_color = if self.is_done {
            Color::DarkGray
        } else {
            Color::Yellow
        };
        let step_w = width.saturating_sub(4) as usize;
        let wrapped = wrap_text(&self.summary, step_w.max(1) as u16);
        let mut lines = Vec::new();
        for (li, wline) in wrapped.iter().enumerate() {
            if li == 0 {
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(icon, Style::default().fg(icon_color)),
                    Span::styled(format!(" {}", wline), Style::default().fg(Color::DarkGray)),
                ]));
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

/// Task 工具调用
pub(crate) struct TaskToolCell {
    pub arguments: String,
    pub result: Option<String>,
}

impl HistoryCell for TaskToolCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.arguments.hash(&mut h);
        self.result.hash(&mut h);
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let args: serde_json::Value = serde_json::from_str(&self.arguments).unwrap_or_default();
        let sub_name = args["subagent"].as_str().unwrap_or("subagent");
        let desc = args["description"].as_str().unwrap_or("");

        let has_result = self.result.is_some();
        let accent = if has_result {
            Color::Green
        } else {
            Color::Magenta
        };

        let mut lines = Vec::new();

        // 标题行
        let bullet = if has_result {
            Span::styled(
                "•",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "●",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        };

        let verb = if has_result {
            "Delegated"
        } else {
            "Delegating"
        };

        let mut title_spans = vec![
            bullet,
            Span::raw(" "),
            Span::styled(
                verb.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" to {}", sub_name), Style::default().fg(accent)),
        ];
        if !desc.is_empty() {
            title_spans.push(Span::styled(
                format!("  {}", desc),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(title_spans));

        // 结果摘要（紧跟标题下方第一行，与其他 ToolCell 一致）
        if let Some(ref result) = self.result {
            let inner =
                extract_xml_tag(result, "task_result").unwrap_or_else(|| result.to_string());
            let trimmed = inner.trim();
            if !trimmed.is_empty() {
                let content_lines: Vec<&str> = trimmed.lines().collect();
                let total = content_lines.len();
                let max_preview = 3;
                let mut result_lines: Vec<String> = Vec::new();
                for (idx, l) in content_lines.iter().enumerate().take(max_preview) {
                    if idx == 0 {
                        result_lines.push(format!("  └ {}", l));
                    } else {
                        result_lines.push(format!("    {}", l));
                    }
                }
                if total > max_preview {
                    result_lines.push(format!("    ... +{} lines", total - max_preview));
                }
                let result_w = width.saturating_sub(4) as usize;
                for rline in &result_lines {
                    let wrapped = wrap_text(rline, result_w.max(1) as u16);
                    for wline in &wrapped {
                        lines.push(Line::from(Span::styled(
                            wline.clone(),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM),
                        )));
                    }
                }
            }
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

/// 待办列表项的状态
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub(crate) enum TodoStatus {
    Completed,
    InProgress,
    Pending,
    Cancelled,
}

/// 待办列表项
#[derive(Debug, Clone)]
pub(crate) struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub priority: String,
}

/// 待办列表卡片：解析 todo_write 的结果，以 checkbox 风格渲染
///   • Updated plan
///     ✔ 完成任务 (strikethrough dim)
///     → 进行中任务 (cyan bold)
///     ○ 待处理任务 (dim)
///     ✗ 已取消任务 (dim strikethrough)
pub(crate) struct TodoCell {
    pub items: Vec<TodoItem>,
    pub all_done: bool,
}

impl TodoCell {
    /// 从 todo_write 工具的原始结果文本解析出结构化项
    pub fn from_result(result: &str) -> Self {
        let mut items = Vec::new();
        for line in result.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("任务列表已更新") {
                continue;
            }
            // 格式: "1. ✓ 任务内容 [中] completed"
            // 去掉前导编号 "N. "
            let rest = trimmed
                .find(". ")
                .map(|pos| &trimmed[pos + 2..])
                .unwrap_or(trimmed);

            let (status, icon_len) = if rest.starts_with("✓") {
                (TodoStatus::Completed, "✓".len())
            } else if rest.starts_with("→") {
                (TodoStatus::InProgress, "→".len())
            } else if rest.starts_with("✗") {
                (TodoStatus::Cancelled, "✗".len())
            } else if rest.starts_with("○") {
                (TodoStatus::Pending, "○".len())
            } else {
                continue;
            };

            let after_icon = &rest[icon_len..].trim_start();
            // 提取优先级标签 [高]/[中]/[低]
            let priority = if let Some(start) = after_icon.find('[') {
                if let Some(end) = after_icon[start..].find(']') {
                    after_icon[start + 1..start + end].to_string()
                } else {
                    String::from("中")
                }
            } else {
                String::from("中")
            };
            // 内容 = 去掉末尾的 " [X] status"
            let content = if let Some(bracket_pos) = after_icon.rfind(" [") {
                after_icon[..bracket_pos].to_string()
            } else {
                after_icon.to_string()
            };

            items.push(TodoItem {
                content,
                status,
                priority,
            });
        }

        let all_done = !items.is_empty()
            && items
                .iter()
                .all(|it| it.status == TodoStatus::Completed || it.status == TodoStatus::Cancelled);
        Self { items, all_done }
    }
}

impl HistoryCell for TodoCell {
    fn cache_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.all_done.hash(&mut h);
        self.items.len().hash(&mut h);
        for item in &self.items {
            item.content.hash(&mut h);
            item.status.hash(&mut h);
            item.priority.hash(&mut h);
        }
        finish_hasher(h)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let verb = if self.all_done { "Updated" } else { "Updating" };
        let bullet = if self.all_done {
            Span::styled(
                "•".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("•".to_string(), Style::default().fg(Color::Cyan))
        };

        let mut lines = vec![Line::from(vec![
            bullet,
            Span::raw(" "),
            Span::styled(
                verb.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(" plan", Style::default().fg(Color::Cyan)),
        ])];

        let detail_w = width.saturating_sub(6) as usize;
        for item in &self.items {
            let (icon, icon_style) = match item.status {
                TodoStatus::Completed => ("✓", Style::default().fg(Color::Green)),
                TodoStatus::InProgress => (
                    "→",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                TodoStatus::Pending => ("○", Style::default().fg(Color::DarkGray)),
                TodoStatus::Cancelled => ("✗", Style::default().fg(Color::DarkGray)),
            };

            let content_style = match item.status {
                TodoStatus::Completed => Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::CROSSED_OUT),
                TodoStatus::InProgress => Style::default().fg(Color::White),
                TodoStatus::Pending => Style::default().fg(Color::DarkGray),
                TodoStatus::Cancelled => Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::CROSSED_OUT),
            };

            let text = item.content.clone();
            let wrapped = wrap_text(&text, detail_w.max(1) as u16);
            for (i, line) in wrapped.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled("  └ ", Style::default()),
                        Span::styled(icon.to_string(), icon_style),
                        Span::styled(" ", Style::default()),
                        Span::styled(line.clone(), content_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("      ", Style::default()),
                        Span::styled(line.clone(), content_style),
                    ]));
                }
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
fn is_readonly_tool(name: &str) -> bool {
    matches!(
        ToolCategory::from_name(name),
        ToolCategory::FileRead | ToolCategory::Search | ToolCategory::Glob | ToolCategory::WebFetch
    )
}

/// 从只读工具参数中提取 Exploring 子项标签和摘要
fn explore_item_from(name: &str, arguments: &str, result: Option<&str>) -> ExploreItem {
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

struct DividerCell {
    label: &'static str,
    hash_seed: u64,
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

        let label_w = UnicodeWidthStr::width(self.label);
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

/// 将 Message 列表转换为 HistoryCell 列表
/// 连续只读工具调用合并为 ExploringCell，其余各自独立展示
pub(crate) fn messages_to_cells(
    messages: &[Message],
    thinking_collapsed: bool,
) -> Vec<Box<dyn HistoryCell>> {
    let mut cells: Vec<Box<dyn HistoryCell>> = Vec::new();
    let mut i = 0;
    let mut last_plan_mode = false;

    while i < messages.len() {
        match &messages[i] {
            Message::User { text, plan_mode } => {
                last_plan_mode = *plan_mode;
                cells.push(Box::new(UserMessageCell {
                    text: text.clone(),
                    plan_mode: *plan_mode,
                }));
                i += 1;
            }
            Message::Agent(text) => {
                if !text.trim().is_empty() {
                    cells.push(Box::new(AgentMarkdownCell { text: text.clone() }));
                }
                i += 1;
            }
            Message::AgentStreaming(text) => {
                if !text.trim().is_empty() {
                    cells.push(Box::new(AgentMarkdownCell { text: text.clone() }));
                }
                i += 1;
            }
            Message::AgentThinking {
                text,
                think_ms,
                thinking_started_at,
            } => {
                if !text.trim().is_empty() {
                    cells.push(Box::new(ReasoningCell {
                        text: text.clone(),
                        collapsed: thinking_collapsed,
                        think_ms: *think_ms,
                        thinking_started_at: *thinking_started_at,
                    }));
                }
                i += 1;
            }
            Message::AgentDone {
                total_ms,
                model,
                status,
            } => {
                cells.push(Box::new(DoneCell {
                    total_ms: *total_ms,
                    model: model.clone(),
                    status: *status,
                    plan_mode: last_plan_mode,
                }));
                i += 1;
            }
            Message::ToolCall { name, arguments } => {
                if name == "task" {
                    // Task 工具：跳过后续 SubagentStep，只取 ToolResult
                    let mut task_result = None;
                    let mut j = i + 1;
                    while j < messages.len() {
                        match &messages[j] {
                            Message::SubagentStep { .. } => {
                                j += 1;
                            }
                            Message::ToolResult {
                                name: rn, result, ..
                            } if rn == "task" => {
                                task_result = Some(result.clone());
                                j += 1;
                                break;
                            }
                            _ => break,
                        }
                    }
                    cells.push(Box::new(TaskToolCell {
                        arguments: arguments.clone(),
                        result: task_result,
                    }));
                    i = j;
                } else if is_readonly_tool(name) {
                    let mut items = Vec::new();
                    let mut all_done = true;
                    let mut j = i;

                    while j < messages.len() {
                        match &messages[j] {
                            Message::ToolCall {
                                name: tn,
                                arguments: ta,
                            } => {
                                if !is_readonly_tool(tn) {
                                    break;
                                }
                                let result = if j + 1 < messages.len() {
                                    if let Message::ToolResult {
                                        name: rn, result, ..
                                    } = &messages[j + 1]
                                    {
                                        if rn == tn { Some(result.clone()) } else { None }
                                    } else {
                                        all_done = false;
                                        None
                                    }
                                } else {
                                    all_done = false;
                                    None
                                };
                                items.push(explore_item_from(tn, ta, result.as_deref()));
                                j += 1;
                                if result.is_some() {
                                    j += 1;
                                }
                            }
                            Message::ToolResult { .. } => {
                                j += 1;
                            }
                            _ => break,
                        }
                    }

                    if items.len() == 1 {
                        let (result, display) = if i + 1 < messages.len() {
                            if let Message::ToolResult {
                                name: rn,
                                result,
                                display,
                            } = &messages[i + 1]
                            {
                                if rn == name {
                                    (Some(result.clone()), display.clone())
                                } else {
                                    (None, None)
                                }
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        };
                        let has_result = result.is_some();
                        cells.push(Box::new(ToolCell {
                            name: name.clone(),
                            arguments: arguments.clone(),
                            result,
                            display,
                        }));
                        i += 1;
                        if has_result {
                            i += 1;
                        }
                    } else {
                        cells.push(Box::new(ExploringCell { items, all_done }));
                        i = j;
                    }
                } else if ToolCategory::from_name(name) == ToolCategory::Todo {
                    // todo_write：解析结果生成 TodoCell
                    let next_result = if i + 1 < messages.len() {
                        if let Message::ToolResult {
                            name: rn, result, ..
                        } = &messages[i + 1]
                        {
                            if rn == name {
                                Some(result.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(ref result) = next_result {
                        cells.push(Box::new(TodoCell::from_result(result)));
                        i += 2;
                    } else {
                        // 尚未有结果时，从参数中提取简略展示
                        let args: serde_json::Value =
                            serde_json::from_str(arguments).unwrap_or_default();
                        let mut items = Vec::new();
                        if let Some(todos) = args["todos"].as_array() {
                            for item in todos {
                                let content =
                                    item["content"].as_str().unwrap_or("未知任务").to_string();
                                let status = match item["status"].as_str().unwrap_or("pending") {
                                    "completed" => TodoStatus::Completed,
                                    "in_progress" => TodoStatus::InProgress,
                                    "cancelled" => TodoStatus::Cancelled,
                                    _ => TodoStatus::Pending,
                                };
                                items.push(TodoItem {
                                    content,
                                    status,
                                    priority: item["priority"].as_str().unwrap_or("中").to_string(),
                                });
                            }
                        }
                        cells.push(Box::new(TodoCell {
                            items,
                            all_done: false,
                        }));
                        i += 1;
                    }
                } else {
                    let (next_result, display) = if i + 1 < messages.len() {
                        if let Message::ToolResult {
                            name: rn,
                            result,
                            display,
                        } = &messages[i + 1]
                        {
                            if rn == name {
                                (Some(result.clone()), display.clone())
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        }
                    } else {
                        (None, None)
                    };

                    let has_result = next_result.is_some();
                    cells.push(Box::new(ToolCell {
                        name: name.clone(),
                        arguments: arguments.clone(),
                        result: next_result,
                        display,
                    }));

                    i += 1;
                    if has_result {
                        i += 1;
                    }
                }
            }
            Message::ToolResult {
                name,
                result,
                display,
            } => {
                cells.push(Box::new(ToolCell {
                    name: name.clone(),
                    arguments: String::new(),
                    result: Some(result.clone()),
                    display: display.clone(),
                }));
                i += 1;
            }
            Message::SubagentStep {
                summary, is_done, ..
            } => {
                cells.push(Box::new(PlainStepCell {
                    summary: summary.clone(),
                    is_done: *is_done,
                }));
                i += 1;
            }
            Message::CompactMarker {
                summary: _,
                compacted_count,
            } => {
                cells.push(Box::new(DividerCell {
                    label: "─ Context compacted ",
                    hash_seed: *compacted_count as u64,
                }));
                i += 1;
            }
            Message::CompactStreaming(text) => {
                cells.push(Box::new(DividerCell {
                    label: "─ Compacting context ",
                    hash_seed: {
                        let mut h = DefaultHasher::new();
                        text.hash(&mut h);
                        h.finish()
                    },
                }));
                i += 1;
            }
        }
    }

    cells
}

/// 使用 textwrap 进行精确换行
fn wrap_text(text: &str, max_width: u16) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let options = textwrap::Options::new(max_width as usize)
        .break_words(true)
        .word_separator(textwrap::WordSeparator::AsciiSpace);
    textwrap::wrap(text, &options)
        .into_iter()
        .map(|cow| cow.into_owned())
        .collect()
}

const MAX_DISPLAY_DIFF_LINES: usize = 40;

/// 背景色常量（暗色终端调色板）
const ADD_LINE_BG: Color = Color::Rgb(33, 58, 43); // #213A2B
const DEL_LINE_BG: Color = Color::Rgb(74, 34, 29); // #4A221D

/// 聊天正文前景色
pub(crate) const CHAT_FG: Color = Color::Rgb(248, 250, 252); // #F8FAFC
/// 输入占位符（中性灰，不带蓝调）
pub(crate) const CHAT_PLACEHOLDER: Color = Color::Rgb(130, 130, 130); // #828282
/// 粘贴元素高亮
pub(crate) const CHAT_PASTE: Color = Color::Rgb(34, 211, 238); // #22D3EE
/// 文件引用 `@file` 高亮
pub(crate) const CHAT_FILE_MENTION: Color = Color::Rgb(251, 191, 36); // #FBBF24
/// Plan 模式徽标
pub(crate) const PLAN_BADGE: Color = Color::Rgb(217, 159, 7); // #D99F07

struct DiffData {
    old: Option<String>,
    new: String,
    path: String,
}

fn parse_diff_data(json: &str) -> Option<DiffData> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(DiffData {
        old: v["old"].as_str().map(|s| s.to_string()),
        new: v["new"].as_str()?.to_string(),
        path: v["path"].as_str()?.to_string(),
    })
}

fn render_diff_from_json(json: &str, width: u16) -> Vec<Line<'static>> {
    let data = match parse_diff_data(json) {
        Some(d) => d,
        None => return Vec::new(),
    };

    use similar::{ChangeTag, TextDiff};

    let old_lines: Vec<&str> = data
        .old
        .as_deref()
        .map(|s| s.lines().collect())
        .unwrap_or_default();
    let new_lines: Vec<&str> = data.new.lines().collect();
    let diff = TextDiff::from_slices(&old_lines, &new_lines);

    // 统计
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            _ => {}
        }
    }

    // 简短路径
    let short_path = data.path.rsplit(['/', '\\']).next().unwrap_or(&data.path);

    // 统计行：path (+N -M) 或 path (new file, +N lines)
    let stats_text = if data.old.is_none() {
        format!("(+{} new file)", additions)
    } else {
        format!("(+{} -{})", additions, deletions)
    };

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "  └ ".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
        Span::styled(short_path.to_string(), Style::default().fg(Color::Gray)),
        Span::raw(" "),
        Span::styled(
            stats_text,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]));

    // diff body — 带行号 gutter 和背景色
    let indent = "    ";

    // 计算行号宽度
    let max_ln = new_lines.len();
    let ln_width = max_ln.to_string().len();

    // 前缀宽度: 4(indent) + ln_width + 1(gutter空格) + 1(sign)
    let prefix_w = 4 + ln_width + 2;
    let content_w = width.saturating_sub(prefix_w as u16) as usize;

    let mut shown = 0u32;
    let mut is_first_group = true;

    for group in diff.grouped_ops(3) {
        if shown >= MAX_DISPLAY_DIFF_LINES as u32 {
            break;
        }

        if !is_first_group {
            // hunk 分隔符 ⋮
            let spacer = format!("{:width$} ", "", width = ln_width.max(1));
            lines.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(spacer, Style::default().add_modifier(Modifier::DIM)),
                Span::styled(
                    "⋮".to_string(),
                    Style::default().add_modifier(Modifier::DIM),
                ),
            ]));
        }
        is_first_group = false;

        for op in &group {
            if shown >= MAX_DISPLAY_DIFF_LINES as u32 {
                break;
            }
            for change in diff.iter_changes(op) {
                if shown >= MAX_DISPLAY_DIFF_LINES as u32 {
                    break;
                }
                shown += 1;

                let (tag, value) = (change.tag(), change.value());
                let old_ln = change.old_index().map(|i| i + 1);
                let new_ln = change.new_index().map(|i| i + 1);

                let (sign, sign_color, content_color, bg_color) = match tag {
                    ChangeTag::Insert => ('+', Color::Green, Color::Green, Some(ADD_LINE_BG)),
                    ChangeTag::Delete => ('-', Color::Red, Color::Red, Some(DEL_LINE_BG)),
                    ChangeTag::Equal => (' ', Color::DarkGray, Color::DarkGray, None),
                };

                // 行号（insert/new 用新行号，delete 用旧行号，context 用新行号）
                let ln = match tag {
                    ChangeTag::Insert => new_ln,
                    ChangeTag::Delete => old_ln,
                    ChangeTag::Equal => new_ln,
                };

                let gutter_text = match ln {
                    Some(n) => format!("{:>width$} ", n, width = ln_width),
                    None => format!("{:width$} ", "", width = ln_width),
                };

                let wrapped = if content_w > 0 {
                    wrap_text(value, content_w as u16)
                } else {
                    vec![value.to_string()]
                };

                for (i, wl) in wrapped.iter().enumerate() {
                    let mut spans = vec![Span::raw(indent)];

                    if i == 0 {
                        spans.push(Span::styled(
                            gutter_text.clone(),
                            Style::default().add_modifier(Modifier::DIM),
                        ));
                        spans.push(Span::styled(
                            sign.to_string(),
                            Style::default().fg(sign_color),
                        ));
                    } else {
                        let pad = format!("{:width$}  ", "", width = ln_width);
                        spans.push(Span::raw(pad));
                    }

                    spans.push(Span::styled(wl.clone(), Style::default().fg(content_color)));

                    let mut line = Line::from(spans);
                    if let Some(bg) = bg_color {
                        line = line.style(Style::default().bg(bg));
                    }
                    lines.push(line);
                }
            }
        }
    }

    if shown >= MAX_DISPLAY_DIFF_LINES as u32 {
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(
                format!("... diff truncated (max {} lines)", MAX_DISPLAY_DIFF_LINES),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ]));
    }

    lines
}

/// 从工具调用的参数中提取摘要信息
pub(crate) fn tool_call_summary(name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    match name {
        "read" => {
            let path = args["file_path"].as_str().unwrap_or("");
            let offset = args["offset"].as_u64();
            let limit = args["limit"].as_u64();
            match (offset, limit) {
                (Some(o), Some(l)) => format!("{} (L{}-L{})", path, o, o + l - 1),
                (Some(o), None) => format!("{} (from L{})", path, o),
                _ => path.to_string(),
            }
        }
        "edit" => {
            let path = args["file_path"].as_str().unwrap_or("");
            let replace_all = args["replace_all"].as_bool().unwrap_or(false);
            if replace_all {
                format!("{} (replace all)", path)
            } else {
                path.to_string()
            }
        }
        "write" => args["file_path"].as_str().unwrap_or("").to_string(),
        "bash" => {
            let cmd = args["command_string"].as_str().unwrap_or("");
            truncate_str(cmd, 60)
        }
        "web_fetch" => {
            let url = args["url"].as_str().unwrap_or("");
            let format = args["format"].as_str().unwrap_or("markdown");
            if format == "markdown" {
                url.to_string()
            } else {
                format!("{} [{}]", url, format)
            }
        }
        "grep" => {
            let pattern = args["pattern"].as_str().unwrap_or("");
            let include = args["include"].as_str();
            let path = args["path"].as_str().unwrap_or(".");
            match include {
                Some(inc) => format!("\"{}\" in {} ({})", pattern, path, inc),
                None => format!("\"{}\" in {}", pattern, path),
            }
        }
        "glob" => {
            let pattern = args["pattern"].as_str().unwrap_or("");
            let path = args["path"].as_str().unwrap_or(".");
            format!("{} in {}", pattern, path)
        }
        "todo_write" => {
            let count = args["todos"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("{} items", count)
        }
        "ask_user" => {
            let count = args["questions"].as_array().map(|a| a.len()).unwrap_or(0);
            if count == 0 {
                String::new()
            } else {
                format!("{} question{}", count, if count > 1 { "s" } else { "" })
            }
        }
        "skill" => args["name"].as_str().unwrap_or("").to_string(),
        "task" => {
            let subagent = args["subagent"].as_str().unwrap_or("");
            let desc = args["description"].as_str().unwrap_or("");
            if desc.is_empty() {
                subagent.to_string()
            } else {
                format!("[{}] {}", subagent, desc)
            }
        }
        _ => {
            let cat = ToolCategory::from_name(name);
            match cat {
                ToolCategory::Mcp => mcp_call_summary(name, &args),
                ToolCategory::Skill => skill_call_summary(name, &args),
                _ => truncate_str(arguments, 60),
            }
        }
    }
}

fn mcp_call_summary(name: &str, args: &serde_json::Value) -> String {
    let server = name.strip_prefix("mcp_").unwrap_or(name);
    let first_key = args
        .as_object()
        .and_then(|m| m.keys().next())
        .cloned()
        .unwrap_or_default();
    if first_key.is_empty() {
        server.to_string()
    } else {
        let val = args[&first_key].to_string();
        format!("{} {}", server, truncate_str(&val, 40))
    }
}

fn skill_call_summary(name: &str, args: &serde_json::Value) -> String {
    let skill_name = name.strip_prefix("skill_").unwrap_or(name);
    let prompt = args["prompt"].as_str().unwrap_or("");
    if prompt.is_empty() {
        skill_name.to_string()
    } else {
        format!("{} \"{}\"", skill_name, truncate_str(prompt, 40))
    }
}

/// 从 ask_user 工具结果中解析 "question"="answer" 对
fn parse_ask_user_pairs(result: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let bytes = result.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // 解析 key
            let key_start = i + 1;
            let mut j = key_start;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'"' {
                    break;
                }
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            let key = result[key_start..j].replace("\\\"", "\"");

            // 寻找 =
            let mut k = j + 1;
            while k < bytes.len() && bytes[k] == b' ' {
                k += 1;
            }
            if k >= bytes.len() || bytes[k] != b'=' {
                i = j + 1;
                continue;
            }
            k += 1;
            while k < bytes.len() && bytes[k] == b' ' {
                k += 1;
            }
            if k >= bytes.len() || bytes[k] != b'"' {
                i = j + 1;
                continue;
            }

            // 解析 value
            let val_start = k + 1;
            let mut m = val_start;
            while m < bytes.len() {
                if bytes[m] == b'\\' {
                    m += 2;
                    continue;
                }
                if bytes[m] == b'"' {
                    break;
                }
                m += 1;
            }
            if m >= bytes.len() {
                break;
            }
            let val = result[val_start..m].replace("\\\"", "\"");

            pairs.push((key, val));
            i = m + 1;
        } else {
            i += 1;
        }
    }
    pairs
}

/// 渲染 ask_user 工具调用的 Q&A 结果
fn render_ask_user_result(arguments: &str, result: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if result.contains("[User Cancelled]") {
        lines.push(Line::from(Span::styled(
            "  └ cancelled",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
        return lines;
    }

    let pairs = parse_ask_user_pairs(result);
    let detail_w = (width as usize).saturating_sub(4).max(10);

    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let questions = args["questions"].as_array();

    if let Some(qs) = questions {
        let total = qs.len();
        for (qi, q) in qs.iter().enumerate() {
            let question = q["question"].as_str().unwrap_or("");
            let answer = pairs
                .iter()
                .find(|(k, _)| k == question)
                .map(|(_, v)| v.as_str())
                .unwrap_or("Unanswered");

            let connector = if qi == total - 1 { "└" } else { "├" };

            // Q 行
            let q_label = "Q: ";
            let q_w = detail_w.saturating_sub(q_label.len());
            let q_wrapped = wrap_text(question, q_w.max(1) as u16);
            for (li, wline) in q_wrapped.iter().enumerate() {
                let prefix = if li == 0 {
                    format!("  {} {}", connector, q_label)
                } else {
                    format!("    {}", " ".repeat(q_label.len()))
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                    Span::styled(wline.clone(), Style::default().fg(Color::Gray)),
                ]));
            }

            // A 行
            let a_label = "A: ";
            let a_w = detail_w.saturating_sub(a_label.len());
            let a_wrapped = wrap_text(answer, a_w.max(1) as u16);
            let a_prefix_align = " ".repeat(a_label.len());
            for (li, wline) in a_wrapped.iter().enumerate() {
                let prefix = if li == 0 {
                    format!("    {}", a_label)
                } else {
                    format!("    {}", a_prefix_align)
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                    Span::styled(wline.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
    } else {
        // 回退：无参数时直接显示原始键值对
        let total = pairs.len();
        for (qi, (k, v)) in pairs.iter().enumerate() {
            let connector = if qi == total - 1 { "└" } else { "├" };
            let qa_text = format!("{}: {}", truncate_str(k, 20), v);
            let wrapped = wrap_text(&qa_text, detail_w as u16);
            for (li, wline) in wrapped.iter().enumerate() {
                let prefix = if li == 0 {
                    format!("  {} ", connector)
                } else {
                    "    ".to_string()
                };
                lines.push(Line::from(Span::styled(
                    format!("{}{}", prefix, wline),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    lines
}

pub(crate) fn tool_result_summary(name: &str, result: &str) -> String {
    if result.is_empty() {
        return String::new();
    }
    match name {
        "read" => {
            let kind = extract_xml_tag(result, "type");
            let content = extract_xml_tag(result, "content");
            let line_count = content.map(|c| c.lines().count()).unwrap_or(0);
            let entries = extract_xml_tag(result, "entries");
            let entry_count = entries
                .map(|e| e.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0);
            match (kind.as_deref(), line_count, entry_count) {
                (Some("file"), lc, _) if lc > 0 => format!("{} lines", lc),
                (Some("directory"), _, ec) if ec > 0 => format!("{} entries", ec),
                _ => String::new(),
            }
        }
        "edit" | "write" => {
            if result.contains("成功") {
                "done".to_string()
            } else {
                truncate_str(result.trim(), 40)
            }
        }
        "bash" => {
            let trimmed = result.trim();
            if trimmed.is_empty() {
                "no output".to_string()
            } else {
                let total_lines = trimmed.lines().count();
                let max_preview = 3;
                if total_lines <= max_preview {
                    truncate_str(trimmed, 120)
                } else {
                    let head: Vec<&str> = trimmed.lines().take(max_preview).collect();
                    format!(
                        "{}\n... +{} lines",
                        head.join("\n"),
                        total_lines - max_preview
                    )
                }
            }
        }
        "web_fetch" => format!("{} chars", result.chars().count()),
        "grep" => {
            if result.starts_with("未找到") {
                "no matches".to_string()
            } else {
                let file_count = result
                    .lines()
                    .filter(|l| l.ends_with(':') && !l.starts_with(' '))
                    .count();
                let line_matches = result.lines().filter(|l| l.starts_with("  Line ")).count();
                if file_count > 0 {
                    format!("{} files, {} matches", file_count, line_matches)
                } else {
                    String::new()
                }
            }
        }
        "glob" => {
            if result.starts_with("未找到") {
                "no matches".to_string()
            } else {
                let count = result.lines().filter(|l| !l.trim().is_empty()).count();
                format!("{} files", count)
            }
        }
        "todo_write" => {
            let completed = result.matches("✓").count();
            let in_progress = result.matches("→").count();
            let total =
                result.matches('○').count() + completed + in_progress + result.matches("✗").count();
            format!("{}/{} done", completed, total)
        }
        "ask_user" => {
            if result.contains("[User Cancelled]") {
                "cancelled".to_string()
            } else {
                "answered".to_string()
            }
        }
        "task" => {
            // 提取 <task_result> 内容
            let inner =
                extract_xml_tag(result, "task_result").unwrap_or_else(|| result.to_string());
            let trimmed = inner.trim();
            if trimmed.is_empty() {
                "no output".to_string()
            } else {
                let total_lines = trimmed.lines().count();
                let max_preview = 5;
                if total_lines <= max_preview {
                    truncate_str(trimmed, 200)
                } else {
                    let head: Vec<&str> = trimmed.lines().take(max_preview).collect();
                    format!(
                        "{}\n... +{} lines",
                        head.join("\n"),
                        total_lines - max_preview
                    )
                }
            }
        }
        _ => truncate_str(result.trim(), 60),
    }
}

fn extract_xml_tag(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = content.find(&open)?;
    let content_after = &content[start + open.len()..];
    let end = content_after.find(&close)?;
    Some(content_after[..end].to_string())
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}
