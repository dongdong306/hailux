use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::BorderType;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

use super::app::types::AppState;
use super::event::{self, AppEvent};
use super::model_picker::wrap_input;

const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);

pub(crate) struct AskUserState {
    pub questions: Vec<event::QuestionInfo>,
    pub response_tx: Option<tokio::sync::oneshot::Sender<String>>,
    pub current_tab: usize,
    pub selected: usize,
    pub answers: Vec<Option<String>>,
    pub custom_inputs: Vec<String>,
    pub custom_cursor: usize,
    pub editing_custom: bool,
    pub last_paste: Option<Instant>,
}

impl AskUserState {
    pub fn save(self, state: &mut AppState) {
        let response_tx = match self.response_tx {
            Some(tx) => tx,
            None => {
                *state = AppState::Chat;
                return;
            }
        };
        *state = AppState::AskUser {
            questions: self.questions,
            response_tx,
            current_tab: self.current_tab,
            selected: self.selected,
            answers: self.answers,
            custom_inputs: self.custom_inputs,
            custom_cursor: self.custom_cursor,
            editing_custom: self.editing_custom,
            last_paste: self.last_paste,
        };
    }

    pub fn submit_and_finish(&mut self) -> bool {
        if let Some(tx) = self.response_tx.take() {
            let formatted = self
                .questions
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let ans = self.answers[i].as_deref().unwrap_or("Unanswered");
                    let esc_q = q.question.replace('"', "\\\"");
                    let esc_a = ans.replace('"', "\\\"");
                    format!("\"{esc_q}\"=\"{esc_a}\"")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let _ = tx.send(formatted);
            true
        } else {
            false
        }
    }

    pub fn try_select(&mut self, single: bool, confirm_tab: usize) {
        let q = &self.questions[self.current_tab];
        let opts = &q.options;
        if self.selected < opts.len() {
            self.answers[self.current_tab] = Some(opts[self.selected].label.clone());
            if single {
                self.submit_and_finish();
            } else {
                self.current_tab += 1;
                if self.current_tab > confirm_tab {
                    self.current_tab = confirm_tab;
                }
                self.selected = 0;
                self.editing_custom = false;
            }
        } else {
            self.editing_custom = true;
        }
    }

    pub fn handle_event(&mut self, event: AppEvent, state: &mut AppState) -> Result<()> {
        let single = self.questions.len() == 1;
        let tabs_total = if single { 1 } else { self.questions.len() + 1 };
        let confirm_tab = self.questions.len();
        let is_confirm = !single && self.current_tab == confirm_tab;

        match event {
            AppEvent::InputPaste(text) => {
                let sanitized: String =
                    text.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
                if self.editing_custom && !is_confirm {
                    let ci = &mut self.custom_inputs[self.current_tab];
                    ci.insert_str(self.custom_cursor, &sanitized);
                    self.custom_cursor += sanitized.len();
                }
                self.last_paste = Some(Instant::now());
                if self.response_tx.is_none() {
                    *state = AppState::Chat;
                } else {
                    let st = std::mem::replace(self, AskUserState::placeholder());
                    st.save(state);
                }
                return Ok(());
            }
            AppEvent::InputKey(key) => {
                if self.editing_custom && !is_confirm {
                    Self::handle_editing_key(self, key, single);
                    if self.response_tx.is_none() {
                        *state = AppState::Chat;
                    } else {
                        let st = std::mem::replace(self, AskUserState::placeholder());
                        st.save(state);
                    }
                    return Ok(());
                }

                let now = Instant::now();
                let paste_suppressed = self
                    .last_paste
                    .is_some_and(|t| now.duration_since(t) < PASTE_ENTER_SUPPRESS_WINDOW);

                match key.code {
                    KeyCode::Esc => {
                        if let Some(tx) = self.response_tx.take() {
                            let _ = tx.send("[User Cancelled]".to_string());
                        }
                        *state = AppState::Chat;
                        return Ok(());
                    }
                    KeyCode::Left => {
                        if self.current_tab == 0 {
                            self.current_tab = tabs_total - 1;
                        } else {
                            self.current_tab -= 1;
                        }
                        self.selected = 0;
                        self.editing_custom = false;
                        self.custom_cursor = 0;
                    }
                    KeyCode::Right => {
                        self.current_tab = (self.current_tab + 1) % tabs_total;
                        self.selected = 0;
                        self.editing_custom = false;
                        self.custom_cursor = 0;
                    }
                    KeyCode::Tab => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            if self.current_tab == 0 {
                                self.current_tab = tabs_total - 1;
                            } else {
                                self.current_tab -= 1;
                            }
                        } else {
                            self.current_tab = (self.current_tab + 1) % tabs_total;
                        }
                        self.selected = 0;
                        self.editing_custom = false;
                        self.custom_cursor = 0;
                    }
                    KeyCode::Up if !is_confirm => {
                        let q = &self.questions[self.current_tab];
                        let total = q.options.len() + 1;
                        if self.selected == 0 {
                            self.selected = total - 1;
                        } else {
                            self.selected -= 1;
                        }
                    }
                    KeyCode::Down if !is_confirm => {
                        let q = &self.questions[self.current_tab];
                        let total = q.options.len() + 1;
                        self.selected = (self.selected + 1) % total;
                    }
                    KeyCode::Char(c) if !is_confirm && c.is_ascii_digit() => {
                        let digit = c.to_digit(10).unwrap_or(0);
                        if digit >= 1 {
                            let q = &self.questions[self.current_tab];
                            let total = q.options.len() + 1;
                            if (digit as usize) <= total.min(9) {
                                self.selected = (digit - 1) as usize;
                                self.try_select(single, confirm_tab);
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if paste_suppressed {
                            self.last_paste = None;
                        } else if is_confirm {
                            self.submit_and_finish();
                            *state = AppState::Chat;
                            return Ok(());
                        } else {
                            self.try_select(single, confirm_tab);
                        }
                    }
                    _ => {}
                }
                if self.response_tx.is_none() {
                    *state = AppState::Chat;
                } else {
                    let st = std::mem::replace(self, AskUserState::placeholder());
                    st.save(state);
                }
            }
            _ => {
                let st = std::mem::replace(self, AskUserState::placeholder());
                st.save(state);
            }
        }
        Ok(())
    }

    fn handle_editing_key(st: &mut AskUserState, key: KeyEvent, single: bool) {
        let tab = st.current_tab;
        let ci = &mut st.custom_inputs[tab];
        match key.code {
            KeyCode::Esc => {
                st.editing_custom = false;
            }
            KeyCode::Enter => {
                let text = ci.trim().to_string();
                if text.is_empty() {
                    st.editing_custom = false;
                    return;
                }
                *ci = text.clone();
                st.answers[st.current_tab] = Some(text.clone());
                if single {
                    st.submit_and_finish();
                    return;
                }
                let confirm_tab = st.questions.len();
                st.current_tab += 1;
                if st.current_tab > confirm_tab {
                    st.current_tab = confirm_tab;
                }
                st.selected = 0;
                st.editing_custom = false;
                st.custom_cursor = 0;
            }
            KeyCode::Backspace => {
                if st.custom_cursor > 0 {
                    let prev = ci[..st.custom_cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    ci.drain(prev..st.custom_cursor);
                    st.custom_cursor = prev;
                }
            }
            KeyCode::Delete => {
                if st.custom_cursor < ci.len() {
                    let next = ci[st.custom_cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| st.custom_cursor + i)
                        .unwrap_or(ci.len());
                    ci.drain(st.custom_cursor..next);
                }
            }
            KeyCode::Left => {
                if st.custom_cursor > 0 {
                    let prev = ci[..st.custom_cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    st.custom_cursor = prev;
                }
            }
            KeyCode::Right => {
                if st.custom_cursor < ci.len() {
                    let next = ci[st.custom_cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| st.custom_cursor + i)
                        .unwrap_or(ci.len());
                    st.custom_cursor = next;
                }
            }
            KeyCode::Home => {
                st.custom_cursor = 0;
            }
            KeyCode::End => {
                st.custom_cursor = ci.len();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                ci.insert(st.custom_cursor, c);
                st.custom_cursor += c.len_utf8();
            }
            _ => {}
        }
    }

    fn placeholder() -> Self {
        Self {
            questions: Vec::new(),
            response_tx: None,
            current_tab: 0,
            selected: 0,
            answers: Vec::new(),
            custom_inputs: Vec::new(),
            custom_cursor: 0,
            editing_custom: false,
            last_paste: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_ask_user(
    area: Rect,
    buf: &mut Buffer,
    questions: &[event::QuestionInfo],
    current_tab: usize,
    selected: usize,
    answers: &[Option<String>],
    custom_inputs: &[String],
    custom_cursor: usize,
    editing_custom: bool,
) -> Option<(u16, u16)> {
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};
    use textwrap::wrap;

    let single = questions.len() == 1;
    let confirm_tab = questions.len();
    let is_confirm = !single && current_tab == confirm_tab;
    let custom_input = custom_inputs
        .get(current_tab)
        .map(|s| s.as_str())
        .unwrap_or("");

    let dialog_width = (area.width as usize).clamp(50, 80);
    let inner_w = dialog_width.saturating_sub(4);

    let tab_lines = if single { 0 } else { 1 };

    let question_lines = if is_confirm {
        let mut total = 0;
        for (i, q) in questions.iter().enumerate() {
            let val = answers[i].as_deref().unwrap_or("(not answered)");
            let line_text = format!(" {}: {}", q.header, val);
            total += wrap(&line_text, inner_w).len().max(1);
        }
        total
    } else {
        let q = &questions[current_tab];
        let qw = wrap(&q.question, inner_w);
        let mut lines = qw.len();
        for opt in &q.options {
            lines += 1 + wrap(&format!("  {}", opt.description), inner_w.saturating_sub(5)).len();
        }
        let edit_lines = if editing_custom {
            let tw = inner_w.saturating_sub(5).max(1);
            wrap_input(custom_input, 0, tw).0.len()
        } else {
            0
        };
        lines + 1 + edit_lines
    };

    let dialog_height = (tab_lines + question_lines + 4) as u16;
    let dialog_x = (area.width as usize).saturating_sub(dialog_width) / 2;
    let dialog_y = (area.height as usize).saturating_sub(dialog_height as usize) / 2;

    let dialog_area = Rect::new(
        dialog_x as u16,
        dialog_y as u16,
        dialog_width as u16,
        dialog_height,
    );

    Clear.render(dialog_area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray))
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            " ? ",
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

    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_pos = None;
    let mut editing_line_idx: Option<usize> = None;

    if !single {
        let mut tab_spans = Vec::new();
        for (i, q) in questions.iter().enumerate() {
            let active = i == current_tab;
            let answered = answers[i].is_some();
            let style = if active {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if answered {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let prefix = if i > 0 { " " } else { "" };
            tab_spans.push(Span::styled(format!("{prefix} {} ", q.header), style));
        }
        let active = current_tab == confirm_tab;
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        tab_spans.push(Span::styled(" Confirm ", style));
        lines.push(Line::from(tab_spans));
    }

    if is_confirm {
        for (i, q) in questions.iter().enumerate() {
            let val = answers[i].as_deref().unwrap_or("(not answered)");
            let val_style = if answers[i].is_some() {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Red)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {}: ", q.header),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(val.to_string(), val_style),
            ]));
        }
    } else {
        let q = &questions[current_tab];

        let q_wrapped = wrap(&q.question, inner_w);
        for line in &q_wrapped {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::White),
            )));
        }

        for (i, opt) in q.options.iter().enumerate() {
            let active = i == selected;
            let picked = answers[current_tab].as_deref() == Some(&opt.label);

            let (num_style, label_style, fill_bg, dim_style) = if active {
                (
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    Color::Cyan,
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                )
            } else {
                (
                    Style::default().fg(Color::DarkGray),
                    if picked {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::White)
                    },
                    Color::Reset,
                    Style::default().fg(Color::DarkGray),
                )
            };

            let selector = if active { "▸ " } else { "  " };

            lines.push(Line::from(vec![
                Span::styled(selector, dim_style),
                Span::styled(format!("{}.", i + 1), num_style),
                Span::styled(format!(" {}", opt.label), label_style),
                Span::styled(
                    if picked { " ✓" } else { "" },
                    Style::default()
                        .fg(if active { Color::Black } else { Color::Green })
                        .bg(fill_bg),
                ),
                Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
            ]));

            for dline in wrap(&opt.description, inner_w.saturating_sub(5)) {
                lines.push(Line::from(vec![
                    Span::styled("     ", dim_style),
                    Span::styled(dline.to_string(), dim_style),
                    Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
                ]));
            }
        }

        let custom_idx = q.options.len();
        let active = selected == custom_idx;
        let custom_picked = answers[current_tab]
            .as_deref()
            .is_some_and(|v| v == custom_input.trim() && !custom_input.trim().is_empty());

        let (num_style, label_style, fill_bg, dim_style) = if active {
            (
                Style::default().fg(Color::Black).bg(Color::Cyan),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                Color::Cyan,
                Style::default().fg(Color::Black).bg(Color::Cyan),
            )
        } else {
            (
                Style::default().fg(Color::DarkGray),
                if custom_picked {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::White)
                },
                Color::Reset,
                Style::default().fg(Color::DarkGray),
            )
        };

        let selector = if active { "▸ " } else { "  " };

        lines.push(Line::from(vec![
            Span::styled(selector, dim_style),
            Span::styled(format!("{}.", custom_idx + 1), num_style),
            Span::styled(" Type your own answer", label_style),
            Span::styled(
                if custom_picked { " ✓" } else { "" },
                Style::default()
                    .fg(if active { Color::Black } else { Color::Green })
                    .bg(fill_bg),
            ),
            Span::styled(" ".repeat(content_width), Style::default().bg(fill_bg)),
        ]));

        if editing_custom {
            let tw = content_width.saturating_sub(5).max(1);
            let (wrapped, cur_row, cur_col) = wrap_input(custom_input, custom_cursor, tw);

            for (li, wline) in wrapped.iter().enumerate() {
                let is_cur_line = li == cur_row;
                if is_cur_line {
                    editing_line_idx = Some(lines.len());
                }

                let mut spans = if li == 0 {
                    vec![
                        Span::styled("   ", Style::default()),
                        Span::styled("> ", Style::default().fg(Color::Yellow)),
                    ]
                } else {
                    vec![Span::styled("     ", Style::default())]
                };
                spans.push(Span::styled(
                    wline.clone(),
                    Style::default().fg(Color::White),
                ));
                if is_cur_line {
                    spans.push(Span::styled("|", Style::default().fg(Color::Gray)));
                }
                lines.push(Line::from(spans));
            }

            cursor_pos = editing_line_idx.map(|li| {
                let cx = (content_area.x + 5 + cur_col as u16)
                    .min(content_area.x + content_area.width.saturating_sub(1));
                let cy = content_area.y + li as u16;
                (cx, cy)
            });
        }
    }

    Paragraph::new(lines).render(content_area, buf);

    let help_text = if is_confirm {
        "enter=submit  esc=cancel"
    } else if single {
        "↑↓=select  1-9=quick pick  enter=confirm  esc=cancel"
    } else {
        "←→=tab  ↑↓=select  1-9=quick pick  enter=confirm  esc=cancel"
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

    cursor_pos
}
