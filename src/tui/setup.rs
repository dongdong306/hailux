use color_eyre::{Result, eyre::eyre};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::config::{self, Config, CustomModelEntry, ProviderEntry};
use crate::tui::model_picker::wrap_input;
use std::collections::BTreeMap;

const DEFAULT_CONTEXT_WINDOW: u32 = 131072;
const DEFAULT_OUTPUT_TOKENS: u32 = 65536;

/// 首次运行引导的多步骤表单
#[derive(Debug, Clone)]
pub(crate) struct SetupForm {
    pub step: SetupStep,
    pub buffer: String,
    pub cursor: usize,
    pub error_msg: String,
    pub selected_index: usize,
    pub is_custom: bool,
    /// 从模型选择器进入：setup 完成后只追加 provider 而非替换整个 config
    pub append_only: bool,
    pub provider_index: usize,
    pub provider_id: String,
    pub base_url: String,
    pub api_key: String,
    pub model_id: String,
    pub context_window: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SetupStep {
    Welcome,
    SelectProvider,
    PredefinedInputApiKey,
    PredefinedSelectModel,
    CustomInputProviderName,
    CustomInputBaseUrl,
    CustomInputApiKey,
    CustomInputModelName,
    CustomInputContextWindow,
    Done,
}

impl SetupForm {
    pub(crate) fn new() -> Self {
        Self {
            step: SetupStep::Welcome,
            buffer: String::new(),
            cursor: 0,
            error_msg: String::new(),
            selected_index: 0,
            is_custom: false,
            append_only: false,
            provider_index: 0,
            provider_id: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            model_id: String::new(),
            context_window: String::new(),
        }
    }

    /// 根据已收集的数据构造 Config
    pub(crate) fn build_config(&self) -> Result<Config> {
        if self.is_custom {
            let context_window = if self.context_window.is_empty() {
                DEFAULT_CONTEXT_WINDOW
            } else {
                self.context_window
                    .parse::<u32>()
                    .map_err(|_| eyre!("context_window 不是有效数字: {}", self.context_window))?
            };
            let mut custom_models = BTreeMap::new();
            custom_models.insert(
                self.model_id.clone(),
                CustomModelEntry {
                    max_tokens: DEFAULT_OUTPUT_TOKENS,
                    context_window,
                },
            );
            let mut providers = BTreeMap::new();
            providers.insert(
                self.provider_id.clone(),
                ProviderEntry {
                    api_key: self.api_key.clone(),
                    base_url: Some(self.base_url.clone()),
                    models: Some(custom_models),
                },
            );
            Ok(Config {
                main_model: format!("{}/{}", self.provider_id, self.model_id),
                providers,
                ..Default::default()
            })
        } else {
            let provider = &config::PROVIDERS[self.provider_index];
            let mut models = BTreeMap::new();
            for m in provider.models {
                models.insert(
                    m.id.to_string(),
                    CustomModelEntry {
                        max_tokens: m.max_tokens,
                        context_window: m.context_window,
                    },
                );
            }
            let mut providers = BTreeMap::new();
            providers.insert(
                provider.id.to_string(),
                ProviderEntry {
                    api_key: self.api_key.clone(),
                    base_url: None,
                    models: Some(models),
                },
            );
            Ok(Config {
                main_model: format!("{}/{}", provider.id, self.model_id),
                providers,
                ..Default::default()
            })
        }
    }
}

fn is_text_step(step: &SetupStep) -> bool {
    matches!(
        step,
        SetupStep::PredefinedInputApiKey
            | SetupStep::CustomInputProviderName
            | SetupStep::CustomInputBaseUrl
            | SetupStep::CustomInputApiKey
            | SetupStep::CustomInputModelName
            | SetupStep::CustomInputContextWindow
    )
}

/// 文本输入步骤的 (步骤号, 总步数)
fn step_progress(step: &SetupStep) -> Option<(usize, usize)> {
    match step {
        SetupStep::PredefinedInputApiKey => Some((2, 3)),
        SetupStep::PredefinedSelectModel => Some((3, 3)),
        SetupStep::CustomInputProviderName => Some((2, 6)),
        SetupStep::CustomInputBaseUrl => Some((3, 6)),
        SetupStep::CustomInputApiKey => Some((4, 6)),
        SetupStep::CustomInputModelName => Some((5, 6)),
        SetupStep::CustomInputContextWindow => Some((6, 6)),
        _ => None,
    }
}

/// 文本输入步骤的 (prompt, hint, hint2)
fn text_step_info(form: &SetupForm) -> Option<(&'static str, String, String)> {
    match form.step {
        SetupStep::PredefinedInputApiKey => {
            let p = &config::PROVIDERS[form.provider_index];
            Some((
                "请输入 API Key",
                format!("服务商: {}", p.name),
                format!("可从 {} 获取", p.base_url),
            ))
        }
        SetupStep::CustomInputProviderName => Some((
            "请输入服务商名称",
            "用于标识，如 ollama、siliconflow".into(),
            String::new(),
        )),
        SetupStep::CustomInputBaseUrl => Some((
            "请输入 API 地址",
            format!("服务商: {}", form.provider_id),
            "例如: http://localhost:11434/v1".into(),
        )),
        SetupStep::CustomInputApiKey => Some((
            "请输入 API Key",
            format!("端点: {}", form.base_url),
            String::new(),
        )),
        SetupStep::CustomInputModelName => Some((
            "请输入模型名称 (Model ID)",
            "例如: qwen3-235b-a22b, llama3-70b".into(),
            String::new(),
        )),
        SetupStep::CustomInputContextWindow => Some((
            "请输入上下文窗口大小",
            "留空使用默认值 131072 (128K)".into(),
            String::new(),
        )),
        _ => None,
    }
}

fn dialog_height(step: &SetupStep, form: &SetupForm, area_h: usize, input_lines: usize) -> usize {
    let needed = match step {
        SetupStep::Welcome => 9,
        SetupStep::SelectProvider => 6 + config::PROVIDERS.len() + 3,
        SetupStep::PredefinedSelectModel => {
            6 + config::PROVIDERS[form.provider_index].models.len() + 2
        }
        SetupStep::Done => 12,
        _ => {
            // Text input step: border(2) + progress(1) + separator(1) + prompt(1)
            // + hint(0..2) + blank(1) + input_lines + error(0..2) + footer(1)
            let hint_count = text_step_info(form)
                .map(|(_, h1, h2)| (!h1.is_empty() as usize) + (!h2.is_empty() as usize))
                .unwrap_or(0);
            let mut h = 7 + hint_count + input_lines;
            if !form.error_msg.is_empty() {
                h += 2;
            }
            h
        }
    };
    needed.min(area_h.saturating_sub(2).max(8))
}

/// 渲染引导界面，返回光标位置（文本输入步骤），None 表示不显示光标
pub(crate) fn render_setup(area: Rect, buf: &mut Buffer, form: &SetupForm) -> Option<(u16, u16)> {
    let dialog_width = (area.width as usize).min(62);
    let inner_width = dialog_width.saturating_sub(2);

    // 预计算输入折行（文本输入步骤）
    let input_width = inner_width.saturating_sub(3);
    let (wrapped, cursor_row, cursor_col) = if is_text_step(&form.step) {
        wrap_input(&form.buffer, form.cursor, input_width)
    } else {
        (vec![String::new()], 0, 0)
    };

    let dialog_height = dialog_height(&form.step, form, area.height as usize, wrapped.len());
    let dialog_x = (area.width as usize).saturating_sub(dialog_width) / 2;
    let dialog_y = (area.height as usize).saturating_sub(dialog_height) / 2;

    let dialog_area = Rect::new(
        dialog_x as u16,
        dialog_y as u16,
        dialog_width as u16,
        dialog_height as u16,
    );

    Clear.render(dialog_area, buf);

    let title = " 欢迎使用 hailux · 首次配置 ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
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

    let content_height = inner.height as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_pos: Option<(u16, u16)> = None;

    match form.step {
        SetupStep::Welcome => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  欢迎使用 hailux",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                "  首次运行，需要进行一些配置",
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  将引导你配置 AI 服务商与模型，",
                Style::default().fg(Color::Gray),
            )));
            lines.push(Line::from(Span::styled(
                "  配置完成后即可开始对话。",
                Style::default().fg(Color::Gray),
            )));
        }
        SetupStep::SelectProvider => {
            lines.push(Line::from(Span::styled(
                "  请选择 AI 服务商",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!(" ─{}", "─".repeat(inner.width.saturating_sub(3) as usize)),
                Style::default().fg(Color::DarkGray),
            )));

            let scroll_offset = {
                let visible_items = content_height.saturating_sub(2);
                if form.selected_index >= visible_items {
                    form.selected_index - visible_items / 2
                } else {
                    0
                }
            };

            for (i, p) in config::PROVIDERS.iter().enumerate() {
                if i < scroll_offset || i >= scroll_offset + content_height.saturating_sub(2) {
                    continue;
                }
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
                    Span::styled(p.name.to_string(), style),
                    Span::styled(format!("  {}", p.base_url), dim),
                    Span::styled(
                        " ".repeat(inner.width as usize),
                        Style::default().bg(if is_sel { Color::Cyan } else { Color::Reset }),
                    ),
                ]));
            }

            let custom_idx = config::PROVIDERS.len();
            if custom_idx >= scroll_offset
                && custom_idx < scroll_offset + content_height.saturating_sub(2)
            {
                let is_sel = custom_idx == form.selected_index;
                lines.push(Line::from(vec![
                    Span::styled(
                        "  ".to_string(),
                        Style::default().bg(if is_sel { Color::Cyan } else { Color::Reset }),
                    ),
                    Span::styled(
                        "+ 自定义 OpenAI 兼容端点",
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
        }
        SetupStep::PredefinedSelectModel => {
            let provider = &config::PROVIDERS[form.provider_index];
            lines.push(Line::from(Span::styled(
                format!(" 步骤 {}/{}", 3, 3),
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::from(Span::styled(
                format!(" ─{}", "─".repeat(inner.width.saturating_sub(3) as usize)),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                format!(" 请选择 {} 的默认模型", provider.name),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));

            let scroll_offset = {
                let visible_items = content_height.saturating_sub(2);
                if form.selected_index >= visible_items {
                    form.selected_index - visible_items / 2
                } else {
                    0
                }
            };

            for (i, m) in provider.models.iter().enumerate() {
                if i < scroll_offset || i >= scroll_offset + content_height.saturating_sub(2) {
                    continue;
                }
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
                    Span::styled(m.name.to_string(), style),
                    Span::styled(format!("  (context_window: {})", m.context_window), dim),
                    Span::styled(
                        " ".repeat(inner.width as usize),
                        Style::default().bg(if is_sel { Color::Cyan } else { Color::Reset }),
                    ),
                ]));
            }
        }
        SetupStep::Done => {
            let (provider_label, model_label) = if form.is_custom {
                (form.provider_id.clone(), form.model_id.clone())
            } else {
                let p = &config::PROVIDERS[form.provider_index];
                (p.name.to_string(), form.model_id.clone())
            };
            lines.push(Line::from(Span::styled(
                "  ✓ 配置已就绪",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  服务商:   {}", provider_label),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(Span::styled(
                format!("  当前模型: {}", model_label),
                Style::default().fg(Color::White),
            )));
            lines.push(Line::from(Span::styled(
                "  保存到:   ~/.hailux/config.toml",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  启动后可按 Ctrl+M 切换模型",
                Style::default().fg(Color::Gray),
            )));
        }
        _ if is_text_step(&form.step) => {
            if let Some((prompt, hint, hint2)) = text_step_info(form) {
                if let Some((n, total)) = step_progress(&form.step) {
                    lines.push(Line::from(Span::styled(
                        format!(" 步骤 {}/{}", n, total),
                        Style::default().fg(Color::Cyan),
                    )));
                    lines.push(Line::from(Span::styled(
                        format!(" ─{}", "─".repeat(inner.width.saturating_sub(3) as usize)),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::from(Span::styled(
                    format!(" {}", prompt),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )));
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

                let input_row = lines.len();
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

                if !form.error_msg.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        format!(" ⚠ {}", form.error_msg),
                        Style::default().fg(Color::Red),
                    )));
                }

                let prefix_w: u16 = 3;
                cursor_pos = Some((
                    (inner.x + prefix_w + cursor_col as u16).min(inner.x + inner.width - 1),
                    inner.y + input_row as u16 + cursor_row as u16,
                ));
            }
        }
        _ => {}
    }

    // 底部操作提示
    let footer = match form.step {
        SetupStep::Welcome => "Enter 开始 · Esc 退出",
        SetupStep::SelectProvider => "↑↓选择 Enter确认",
        SetupStep::PredefinedSelectModel => "↑↓选择 Enter确认 · Esc 返回",
        SetupStep::Done => "Enter 启动 hailux · Esc 重新配置",
        _ => "Enter 确认 · Esc 返回",
    };
    let footer_y = inner.height as usize - 1;
    while lines.len() < footer_y {
        lines.push(Line::from(""));
    }
    let footer_pad = (inner.width as usize).saturating_sub(UnicodeWidthStr::width(footer)) / 2;
    lines.push(Line::from(Span::styled(
        format!("{}{}", " ".repeat(footer_pad), footer),
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines);
    paragraph.render(inner, buf);

    cursor_pos
}
