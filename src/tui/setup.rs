use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use std::collections::BTreeSet;
use unicode_width::UnicodeWidthStr;

use crate::config::model_discovery::RemoteModel;
use crate::config::{self, Config};
use crate::tui::model_picker::wrap_input;

/// One entry in the provider selection list
#[derive(Debug, Clone)]
pub(crate) struct ProviderOption {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub kind: ProviderKind,
}

/// What extra info the selected provider needs
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ProviderKind {
    /// Already configured (has a key): reuse credentials, fetch the model list directly
    Configured,
    /// Predefined provider not yet configured: only needs an API key
    Predefined,
    /// Custom endpoint: name + API base URL + API key
    Custom,
}

/// Multi-step form shared by first-time setup / add-model flows.
/// Flow: pick provider → (name → base URL →) API key → fetch /models → check models (or manual input) → done
#[derive(Debug, Clone)]
pub(crate) struct SetupForm {
    pub step: SetupStep,
    pub buffer: String,
    pub cursor: usize,
    pub error_msg: String,
    pub selected_index: usize,
    /// Entered from the model picker: on completion, append provider/model to the existing config.
    /// During first-time setup the config is empty and takes the same append path, just with an extra welcome page.
    pub append_only: bool,
    pub provider_options: Vec<ProviderOption>,
    pub provider_id: String,
    pub provider_kind: ProviderKind,
    /// Base URL entered by the user for custom endpoints; built-in address for predefined providers
    pub base_url: String,
    pub api_key: String,
    pub model_id: String,
    pub context_window: String,
    /// /models fetch result (data source for the SelectModel multi-select list); empty = fetch failed or not attempted, fall back to manual input
    pub remote_models: Vec<RemoteModel>,
    /// Model ids checked for addition (SelectModel step, all unchecked by default)
    pub checked: BTreeSet<String>,
    /// Model ids already present in the provider config table (marked "already in list" in the UI; checking them adds no new row)
    pub existing_ids: BTreeSet<String>,
    /// Number of models actually added after confirming the multi-select (shown on the Done page)
    pub added_count: usize,
    /// Selector to set as the default model after confirming the multi-select (first-time setup only; append mode doesn't switch the default model)
    pub default_model: String,
}

/// The model multi-select (checkbox) step; a "manual input" entry is kept at the end
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SetupStep {
    Welcome,
    SelectProvider,
    InputProviderName,
    InputBaseUrl,
    InputApiKey,
    /// Waiting for the background /models request (AppEvent::ModelsRefreshed)
    FetchingModels,
    SelectModel,
    InputModelName,
    InputContextWindow,
    Done,
}

impl SetupForm {
    /// Provider options: configured ones first (reuse credentials), then unconfigured predefined providers; the custom endpoint is always the last entry
    pub(crate) fn new(cfg: &Config, append_only: bool) -> Self {
        let mut provider_options: Vec<ProviderOption> = cfg
            .configured_providers()
            .into_iter()
            .map(|p| ProviderOption {
                id: p.id,
                name: p.name,
                base_url: p.base_url,
                kind: ProviderKind::Configured,
            })
            .collect();
        for def in config::PROVIDERS {
            let configured = cfg
                .providers
                .get(def.id)
                .is_some_and(|e| !e.api_key.is_empty());
            if !configured {
                provider_options.push(ProviderOption {
                    id: def.id.to_string(),
                    name: def.name.to_string(),
                    base_url: def.base_url.to_string(),
                    kind: ProviderKind::Predefined,
                });
            }
        }

        Self {
            step: if append_only {
                SetupStep::SelectProvider
            } else {
                SetupStep::Welcome
            },
            buffer: String::new(),
            cursor: 0,
            error_msg: String::new(),
            selected_index: 0,
            append_only,
            provider_options,
            provider_id: String::new(),
            provider_kind: ProviderKind::Custom,
            base_url: String::new(),
            api_key: String::new(),
            model_id: String::new(),
            context_window: String::new(),
            remote_models: Vec::new(),
            checked: BTreeSet::new(),
            existing_ids: BTreeSet::new(),
            added_count: 0,
            default_model: String::new(),
        }
    }

    /// Row count of the SelectModel list: fetched models + the trailing "manual input" entry
    pub(crate) fn model_choice_count(&self) -> usize {
        self.remote_models.len() + 1
    }

    /// Return checked model ids in remote_models order
    pub(crate) fn picked_ids(&self) -> Vec<String> {
        self.remote_models
            .iter()
            .filter(|m| self.checked.contains(&m.id))
            .map(|m| m.id.clone())
            .collect()
    }

    /// Whether the model is already in the provider's config table
    pub(crate) fn is_existing(&self, model_id: &str) -> bool {
        self.existing_ids
            .iter()
            .any(|k| k.eq_ignore_ascii_case(model_id))
    }

    pub(crate) fn needs_api_key(&self) -> bool {
        self.provider_kind != ProviderKind::Configured
    }

    /// Whether this is a text input step; used for wrapping/cursor render info on the manual-input path
    pub(crate) fn is_text_step(&self) -> bool {
        matches!(
            self.step,
            SetupStep::InputProviderName
                | SetupStep::InputBaseUrl
                | SetupStep::InputApiKey
                | SetupStep::InputModelName
                | SetupStep::InputContextWindow
        )
    }

    /// (step number, total steps) for text input steps; the total varies with provider kind
    fn step_progress(&self) -> Option<(usize, usize)> {
        let total = match self.provider_kind {
            ProviderKind::Custom => 5,
            ProviderKind::Predefined => 3,
            ProviderKind::Configured => 2,
        };
        let n = match (&self.step, self.provider_kind) {
            (SetupStep::InputProviderName, _) => 1,
            (SetupStep::InputBaseUrl, _) => 2,
            (SetupStep::InputApiKey, ProviderKind::Custom) => 3,
            (SetupStep::InputApiKey, ProviderKind::Predefined) => 1,
            (SetupStep::InputModelName, ProviderKind::Custom) => 4,
            (SetupStep::InputModelName, ProviderKind::Predefined) => 2,
            (SetupStep::InputModelName, ProviderKind::Configured) => 1,
            (SetupStep::InputContextWindow, _) => total,
            _ => return None,
        };
        Some((n, total))
    }

    /// (prompt, hint, hint2) for text input steps
    fn text_step_info(&self) -> Option<(&'static str, String, String)> {
        match self.step {
            SetupStep::InputProviderName => Some((
                "请输入服务商名称",
                "用于标识，如 ollama、siliconflow".into(),
                String::new(),
            )),
            SetupStep::InputBaseUrl => Some((
                "请输入 API 地址",
                format!("服务商: {}", self.provider_id),
                "例如: http://localhost:11434/v1".into(),
            )),
            SetupStep::InputApiKey => Some((
                "请输入 API Key",
                format!("服务商: {}", self.provider_id),
                match self.provider_kind {
                    ProviderKind::Predefined => format!("可从 {} 获取", self.base_url),
                    _ => format!("端点: {}", self.base_url),
                },
            )),
            SetupStep::InputModelName => Some((
                "请输入模型名称 (Model ID)",
                "例如: qwen3-235b-a22b, llama3-70b".into(),
                String::new(),
            )),
            SetupStep::InputContextWindow => Some((
                "请输入上下文窗口大小",
                format!(
                    "留空使用默认值 {} ({}K)",
                    config::DEFAULT_CONTEXT_WINDOW,
                    config::DEFAULT_CONTEXT_WINDOW / 1024
                ),
                String::new(),
            )),
            _ => None,
        }
    }
}

fn dialog_height(step: &SetupStep, form: &SetupForm, area_h: usize, input_lines: usize) -> usize {
    let needed = match step {
        SetupStep::Welcome => 9,
        // title + separator + options + separator + custom entry + footer
        SetupStep::SelectProvider => 6 + form.provider_options.len() + 3,
        SetupStep::SelectModel => 6 + form.model_choice_count() + 2,
        SetupStep::FetchingModels => 8,
        SetupStep::Done => 12,
        _ => {
            // Text input step: border(2) + progress(1) + separator(1) + prompt(1)
            // + hint(0..2) + blank(1) + input_lines + error(0..2) + footer(1)
            let hint_count = form
                .text_step_info()
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

/// Render the setup wizard; returns the cursor position (text input steps), None means no cursor
pub(crate) fn render_setup(area: Rect, buf: &mut Buffer, form: &SetupForm) -> Option<(u16, u16)> {
    let dialog_width = (area.width as usize).min(62);
    let inner_width = dialog_width.saturating_sub(2);

    // Precompute input line wrapping (text input steps)
    let input_width = inner_width.saturating_sub(3);
    let (wrapped, cursor_row, cursor_col) = if form.is_text_step() {
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

    let title = if form.append_only {
        " 添加模型 "
    } else {
        " 欢迎使用 hailux · 首次配置 "
    };
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
            lines.push(separator(" 请选择 AI 服务商"));

            let scroll_offset = scroll_offset(form.selected_index, content_height);
            for (i, p) in form.provider_options.iter().enumerate() {
                if !visible(i, scroll_offset, content_height) {
                    continue;
                }
                let tag = match p.kind {
                    ProviderKind::Configured => "  · 已配置",
                    ProviderKind::Predefined => "  · 需 API Key",
                    ProviderKind::Custom => "",
                };
                lines.push(option_row(
                    inner,
                    i == form.selected_index,
                    &p.name,
                    &format!("  {}{}", p.base_url, tag),
                ));
            }

            lines.push(separator(""));
            let custom_idx = form.provider_options.len();
            if visible(custom_idx, scroll_offset, content_height) {
                lines.push(option_row(
                    inner,
                    custom_idx == form.selected_index,
                    "+ 自定义 OpenAI 兼容端点",
                    "",
                ));
            }
        }
        SetupStep::SelectModel => {
            let provider_name = form
                .provider_options
                .iter()
                .find(|p| p.id == form.provider_id)
                .map(|p| p.name.as_str())
                .unwrap_or(form.provider_id.as_str());
            lines.push(separator(&format!(" 勾选要添加的 {provider_name} 模型")));
            lines.push(Line::from(""));

            let scroll_offset = scroll_offset(form.selected_index, content_height);
            for (i, m) in form.remote_models.iter().enumerate() {
                if !visible(i, scroll_offset, content_height) {
                    continue;
                }
                let is_existing = form.is_existing(&m.id);
                let mut detail = String::new();
                if let Some(name) = &m.name
                    && name != &m.id
                {
                    detail.push_str(&format!("  {name}"));
                }
                if let Some(w) = m.context_window {
                    detail.push_str(&format!("  · {}K", w / 1024));
                }
                if is_existing {
                    detail.push_str("  · 已在列表");
                }
                if !detail.is_empty() {
                    detail = format!("  ({})", detail.trim_start_matches("  "));
                }
                lines.push(checkbox_row(
                    inner,
                    i == form.selected_index,
                    form.checked.contains(&m.id),
                    &m.id,
                    &detail,
                ));
            }
            let manual_idx = form.remote_models.len();
            if visible(manual_idx, scroll_offset, content_height) {
                lines.push(option_row(
                    inner,
                    manual_idx == form.selected_index,
                    "+ 手动输入模型名…",
                    "",
                ));
            }
        }
        SetupStep::FetchingModels => {
            lines.push(separator(" 正在获取模型列表"));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  ⏳ 请求 /models 中…",
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::from(Span::styled(
                format!("  服务商: {}", form.provider_id),
                Style::default().fg(Color::DarkGray),
            )));
        }
        SetupStep::Done => {
            lines.push(Line::from(Span::styled(
                "  ✓ 配置已就绪",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  服务商:   {}", form.provider_id),
                Style::default().fg(Color::White),
            )));
            if form.added_count > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  新增模型: {} 个", form.added_count),
                    Style::default().fg(Color::White),
                )));
            }
            if !form.model_id.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("  手动添加: {}", form.model_id),
                    Style::default().fg(Color::White),
                )));
            }
            if !form.default_model.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("  当前模型: {}", form.default_model),
                    Style::default().fg(Color::White),
                )));
            }
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
        _ if form.is_text_step() => {
            if let Some((prompt, hint, hint2)) = form.text_step_info() {
                if let Some((n, total)) = form.step_progress() {
                    lines.push(Line::from(Span::styled(
                        format!(" 步骤 {n}/{total}"),
                        Style::default().fg(Color::Cyan),
                    )));
                    lines.push(separator(""));
                }
                lines.push(Line::from(Span::styled(
                    format!(" {prompt}"),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )));
                if !hint.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!(" {hint}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                if !hint2.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!(" {hint2}"),
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

    let footer = match form.step {
        SetupStep::Welcome => "Enter 开始 · Esc 退出",
        SetupStep::SelectProvider => "↑↓选择 Enter确认 · Esc返回",
        SetupStep::FetchingModels => "正在获取… · Esc 返回",
        SetupStep::SelectModel => "Space勾选 A全选 Enter确认 · Esc返回",
        SetupStep::Done => "Enter 完成 · Esc 重新配置",
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

/// Separator line: ` ──── title ────`
fn separator(title: &str) -> Line<'static> {
    // Title width is decided by the caller; use a single sufficiently long line here (clipped to the area when rendered)
    Line::from(Span::styled(
        format!(" {title} {}", "─".repeat(80)),
        Style::default().fg(Color::DarkGray),
    ))
}

fn scroll_offset(selected: usize, content_height: usize) -> usize {
    let visible_items = content_height.saturating_sub(2);
    if selected >= visible_items {
        selected - visible_items / 2
    } else {
        0
    }
}

fn visible(index: usize, scroll_offset: usize, content_height: usize) -> bool {
    index >= scroll_offset && index < scroll_offset + content_height.saturating_sub(2)
}

/// A list row: the selected item gets a full-row Cyan background; `text` is the main label and `dim_text` is the gray suffix after it
fn option_row(inner: Rect, is_sel: bool, text: &str, dim_text: &str) -> Line<'static> {
    let bg = if is_sel { Color::Cyan } else { Color::Reset };
    let text_style = if is_sel {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let dim_style = if is_sel {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Line::from(vec![
        Span::styled("  ".to_string(), Style::default().bg(bg)),
        Span::styled(text.to_string(), text_style),
        Span::styled(dim_text.to_string(), dim_style),
        Span::styled(" ".repeat(inner.width as usize), Style::default().bg(bg)),
    ])
}

/// Multi-select checkbox row: `[x]`/`[ ]` prefix + model id + gray supplementary info
fn checkbox_row(
    inner: Rect,
    is_sel: bool,
    is_checked: bool,
    text: &str,
    dim_text: &str,
) -> Line<'static> {
    let bg = if is_sel { Color::Cyan } else { Color::Reset };
    let text_style = if is_sel {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let box_style = if is_sel {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if is_checked {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let dim_style = if is_sel {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let checkbox = if is_checked { "[x]" } else { "[ ]" };
    Line::from(vec![
        Span::styled("  ".to_string(), Style::default().bg(bg)),
        Span::styled(checkbox.to_string(), box_style),
        Span::styled(" ".to_string(), Style::default().bg(bg)),
        Span::styled(text.to_string(), text_style),
        Span::styled(dim_text.to_string(), dim_style),
        Span::styled(" ".repeat(inner.width as usize), Style::default().bg(bg)),
    ])
}
