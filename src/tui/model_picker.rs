use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::{self, ModelEntry};

/// 将输入文本按可用宽度折行，返回 (各行文本, 光标所在行, 光标所在列)。
/// 光标行列均以视觉宽度为单位，行从 0 开始。
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

        // 光标在此字符之前
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

/// 添加模型的多步骤表单
#[derive(Debug, Clone)]
pub(crate) struct AddModelForm {
    pub step: AddModelStep,
    pub buffer: String,
    pub cursor: usize,
    pub error_msg: String,
    pub selected_index: usize,
    // 已收集的数据
    pub provider_id: String,
    pub base_url: String,
    pub api_key: String,
    pub model_name: String,
    pub provider_options: Vec<config::ProviderInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AddModelStep {
    SelectProvider,
    InputProviderName,
    InputBaseUrl,
    InputApiKey,
    InputModelName,
    InputContextWindow,
}

impl AddModelForm {
    pub(crate) fn new(existing_providers: Vec<config::ProviderInfo>) -> Self {
        Self {
            step: AddModelStep::SelectProvider,
            buffer: String::new(),
            cursor: 0,
            error_msg: String::new(),
            selected_index: 0,
            provider_id: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            model_name: String::new(),
            provider_options: existing_providers,
        }
    }
}

pub fn render_model_picker(
    area: Rect,
    buf: &mut Buffer,
    models: &[ModelEntry],
    selected_index: usize,
    current_model: &str,
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
        .title(Span::styled(
            " 切换模型 ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center);

    let inner = block.inner(dialog_area);
    block.render(dialog_area, buf);

    let content_height = inner.height.saturating_sub(2) as usize;

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

        // provider 分隔线
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

        let id_suffix = if entry.model_name != entry.model_id {
            vec![Span::styled(
                format!("  ({})", entry.model_id),
                if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            )]
        } else {
            vec![]
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
        if entry.needs_setup {
            spans.push(Span::styled(
                " (需配置)",
                if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Yellow)
                },
            ));
        }
        spans.extend(id_suffix);
        // 尾部 span 填充背景色到行尾
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

    // "添加模型..." 选项（序号 = models.len()）
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

    lines.push(Line::from(""));
    let help_text = "↑↓选择 Enter确认 Esc关闭";
    let help_pad = (inner.width as usize).saturating_sub(UnicodeWidthStr::width(help_text)) / 2;
    lines.push(Line::from(Span::styled(
        format!("{}{}", " ".repeat(help_pad), help_text),
        Style::default().fg(Color::Gray),
    )));

    let paragraph = Paragraph::new(lines);
    paragraph.render(inner, buf);
}

/// 渲染添加模型表单，返回光标位置
pub fn render_add_model(area: Rect, buf: &mut Buffer, form: &AddModelForm) -> (u16, u16) {
    let dialog_width = (area.width as usize).min(58);
    let inner_width = dialog_width.saturating_sub(2);

    // 预计算输入折行（文本输入步骤）
    let input_width = inner_width.saturating_sub(3); // " > " prefix
    let (wrapped, cursor_row, cursor_col) = wrap_input(&form.buffer, form.cursor, input_width);
    let input_line_count = wrapped.len();

    // 计算对话框高度
    let dialog_height = if form.step == AddModelStep::SelectProvider {
        14u16
    } else {
        let hint_info = match form.step {
            AddModelStep::InputProviderName => ("用于标识，如 ollama、siliconflow", ""),
            AddModelStep::InputBaseUrl => ("", "例如: http://localhost:11434/v1"),
            AddModelStep::InputApiKey => ("", ""),
            AddModelStep::InputModelName => ("", "例如: qwen3-235b-a22b, llama3-70b"),
            AddModelStep::InputContextWindow => ("", "留空使用默认值 131072 (128K)"),
            _ => ("", ""),
        };
        let mut needed = 6 + input_line_count + 1; // base + footer
        if !hint_info.0.is_empty() {
            needed += 1;
        }
        if !hint_info.1.is_empty() {
            needed += 1;
        }
        if !form.error_msg.is_empty() {
            needed += 2;
        }
        needed.max(10).min(area.height as usize).max(10) as u16
    };

    let dialog_x = (area.width as usize).saturating_sub(dialog_width) / 2;
    let dialog_y = (area.height as usize).saturating_sub(dialog_height as usize) / 2;

    let dialog_area = Rect::new(
        dialog_x as u16,
        dialog_y as u16,
        dialog_width as u16,
        dialog_height,
    );

    Clear.render(dialog_area, buf);

    let title = " 添加模型 ";
    let (prompt, hint, hint2): (String, String, String) = match form.step {
        AddModelStep::SelectProvider => (String::new(), String::new(), String::new()),
        AddModelStep::InputProviderName => (
            "请输入服务商名称".into(),
            "用于标识，如 ollama、siliconflow".into(),
            String::new(),
        ),
        AddModelStep::InputBaseUrl => (
            "请输入 API 地址".into(),
            format!("服务商: {}", form.provider_id),
            "例如: http://localhost:11434/v1".into(),
        ),
        AddModelStep::InputApiKey => (
            "请输入 API Key".into(),
            format!("端点: {}", form.base_url),
            String::new(),
        ),
        AddModelStep::InputModelName => (
            "请输入模型名称 (Model ID)".into(),
            "例如: qwen3-235b-a22b, llama3-70b".into(),
            String::new(),
        ),
        AddModelStep::InputContextWindow => (
            "请输入上下文窗口大小".into(),
            "留空使用默认值 131072 (128K)".into(),
            String::new(),
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center);

    let inner = block.inner(dialog_area);
    block.render(dialog_area, buf);

    let mut lines: Vec<Line> = Vec::new();
    let mut input_row: usize = 0;

    match form.step {
        AddModelStep::SelectProvider => {
            // 步骤指示器
            lines.push(Line::from(Span::styled(
                "  选择目标服务商",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!(" ─{}", "─".repeat(inner.width.saturating_sub(3) as usize)),
                Style::default().fg(Color::DarkGray),
            )));

            for (i, p) in form.provider_options.iter().enumerate() {
                let is_sel = i == form.selected_index;
                let style = if is_sel {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let dim = if is_sel {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        "  ".to_string(),
                        Style::default().bg(if is_sel { Color::Cyan } else { Color::Reset }),
                    ),
                    Span::styled(p.name.clone(), style),
                    Span::styled(format!("  {}", p.base_url), dim),
                    Span::styled(
                        " ".repeat(inner.width as usize),
                        Style::default().bg(if is_sel { Color::Cyan } else { Color::Reset }),
                    ),
                ]));
            }

            // 分隔线
            lines.push(Line::from(Span::styled(
                format!(" ─{}", "─".repeat(inner.width.saturating_sub(3) as usize)),
                Style::default().fg(Color::DarkGray),
            )));

            let new_idx = form.provider_options.len();
            let is_sel = new_idx == form.selected_index;
            lines.push(Line::from(vec![
                Span::styled(
                    "  ".to_string(),
                    Style::default().bg(if is_sel { Color::Cyan } else { Color::Reset }),
                ),
                Span::styled(
                    "+ 新增自定义端点",
                    if is_sel {
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
                    Style::default().bg(if is_sel { Color::Cyan } else { Color::Reset }),
                ),
            ]));
        }
        _ => {
            // 通用文本输入表单布局
            // 步骤进度
            let step_num = match form.step {
                AddModelStep::InputProviderName => 1,
                AddModelStep::InputBaseUrl => 2,
                AddModelStep::InputApiKey => 3,
                AddModelStep::InputModelName => 4,
                AddModelStep::InputContextWindow => 5,
                _ => 0,
            };
            let total_steps = 5;
            let progress = format!(" 步骤 {}/{}", step_num, total_steps);

            lines.push(Line::from(Span::styled(
                progress,
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::from(Span::styled(
                format!(" ─{}", "─".repeat(inner.width.saturating_sub(3) as usize)),
                Style::default().fg(Color::DarkGray),
            )));

            // 主提示
            lines.push(Line::from(Span::styled(
                format!(" {}", prompt),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));

            // 提示信息
            if !hint.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(" {}", hint),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if !hint2.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(" {}", hint2),
                    Style::default().fg(Color::DarkGray),
                )));
            }

            lines.push(Line::from(""));

            input_row = lines.len();
            // 渲染折行输入：第一行带 " > " 前缀，后续行对齐
            for (wi, text) in wrapped.iter().enumerate() {
                if wi == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(" > ", Style::default().fg(Color::Cyan)),
                        Span::styled(text.clone(), Style::default().fg(Color::White)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled("   ", Style::default()),
                        Span::styled(text.clone(), Style::default().fg(Color::White)),
                    ]));
                }
            }
        }
    }

    // 错误信息
    if !form.error_msg.is_empty() {
        // 填充空行使错误信息在固定位置
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(" ⚠ {}", form.error_msg),
            Style::default().fg(Color::Red),
        )));
    }

    // 底部操作提示
    let footer_y = inner.height as usize - 1;
    while lines.len() < footer_y {
        lines.push(Line::from(""));
    }
    let help_text = "Enter 确认 · Esc 返回";
    let help_pad = (inner.width as usize).saturating_sub(UnicodeWidthStr::width(help_text)) / 2;
    lines.push(Line::from(Span::styled(
        format!("{}{}", " ".repeat(help_pad), help_text),
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines.clone());
    paragraph.render(inner, buf);

    // 光标定位
    if form.step == AddModelStep::SelectProvider {
        (0, 0)
    } else {
        let prefix_w: u16 = 3; // " > "
        (
            (inner.x + prefix_w + cursor_col as u16).min(inner.x + inner.width - 1),
            inner.y + input_row as u16 + cursor_row as u16,
        )
    }
}
