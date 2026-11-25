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

/// Escapes question-tool reply text: both `\` and `"` are escaped (`\` first,
/// then `"`, avoiding `\"` ambiguity), paired with history_cell's
/// parse_ask_user_pairs for lossless restoration.
pub(crate) fn ask_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Formats the question tool reply: `"question"="answer"` pairs comma-joined;
/// answers matching no option label are marked as user-typed free text
/// (mirrored by web `dialogs.tsx::formatAskReply`, parsed back by
/// `history_cell::parse_ask_user_pairs`).
pub(crate) fn format_ask_reply(
    questions: &[event::QuestionInfo],
    answers: &[Option<String>],
) -> String {
    questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let ans = answers
                .get(i)
                .and_then(|a| a.as_deref())
                .unwrap_or("Unanswered");
            let marked = if crate::agent::event::is_custom_answer(q, ans) {
                format!("{}{ans}", crate::agent::event::CUSTOM_ANSWER_PREFIX)
            } else {
                ans.to_string()
            };
            format!(
                "\"{}\"=\"{}\"",
                ask_escape(&q.question),
                ask_escape(&marked)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

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
            let formatted = format_ask_reply(&self.questions, &self.answers);
            let _ = tx.send(formatted);
            true
        } else {
            false
        }
    }

    pub fn try_select(&mut self, confirm_tab: usize) {
        let q = &self.questions[self.current_tab];
        let opts = &q.options;
        if self.selected < opts.len() {
            self.answers[self.current_tab] = Some(opts[self.selected].label.clone());
            // Align with Web: selecting an option does not auto-submit; it always
            // moves to the Confirm tab for confirmation
            self.current_tab += 1;
            if self.current_tab > confirm_tab {
                self.current_tab = confirm_tab;
            }
            self.selected = 0;
            self.editing_custom = false;
        } else {
            self.editing_custom = true;
        }
    }

    pub fn handle_event(&mut self, event: AppEvent, state: &mut AppState) -> Result<()> {
        let confirm_tab = self.questions.len();
        let tabs_total = confirm_tab + 1;
        let is_confirm = self.current_tab == confirm_tab;

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
                    Self::handle_editing_key(self, key);
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
                    KeyCode::Up => {
                        if is_confirm {
                            // Browse the answer review list; windowing keeps
                            // the cursor row visible
                            self.selected = self.selected.saturating_sub(1);
                        } else {
                            let q = &self.questions[self.current_tab];
                            let total = q.options.len() + 1;
                            if self.selected == 0 {
                                self.selected = total - 1;
                            } else {
                                self.selected -= 1;
                            }
                        }
                    }
                    KeyCode::Down => {
                        if is_confirm {
                            let last = self.questions.len().saturating_sub(1);
                            if self.selected < last {
                                self.selected += 1;
                            }
                        } else {
                            let q = &self.questions[self.current_tab];
                            let total = q.options.len() + 1;
                            self.selected = (self.selected + 1) % total;
                        }
                    }
                    KeyCode::Char(c) if !is_confirm && c.is_ascii_digit() => {
                        let digit = c.to_digit(10).unwrap_or(0);
                        if digit >= 1 {
                            let q = &self.questions[self.current_tab];
                            let total = q.options.len() + 1;
                            if (digit as usize) <= total.min(9) {
                                self.selected = (digit - 1) as usize;
                                self.try_select(confirm_tab);
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
                            self.try_select(confirm_tab);
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

    fn handle_editing_key(st: &mut AskUserState, key: KeyEvent) {
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
                // Align with Web: pressing Enter on a custom input moves to the
                // Confirm tab; it does not auto-submit
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

/// Borrowed view of the question dialog state, shared by the panel height
/// measure and the renderer so the two cannot drift apart.
pub(crate) struct AskUserView<'a> {
    pub questions: &'a [event::QuestionInfo],
    pub current_tab: usize,
    pub selected: usize,
    pub answers: &'a [Option<String>],
    pub custom_inputs: &'a [String],
    pub custom_cursor: usize,
    pub editing_custom: bool,
}

/// Panel lines plus the indices the renderer needs. The first `tab_rows` lines
/// are the tab bar; the selected block is body-relative (tab rows excluded).
struct AskUserLines {
    lines: Vec<Line<'static>>,
    tab_rows: usize,
    selected_block: Option<(usize, usize)>,
    editing_line_idx: Option<usize>,
    cur_col: usize,
}

/// Content-driven height of the question panel at the given width (tab bar +
/// current tab content + 2 border rows + 2 help rows). The caller clamps it to
/// the available space; the renderer keeps the selected row visible.
pub(crate) fn ask_user_panel_height(view: &AskUserView<'_>, width: u16) -> u16 {
    (build_ask_user_lines(view, width).lines.len() + 4) as u16
}

fn build_ask_user_lines(view: &AskUserView<'_>, width: u16) -> AskUserLines {
    use textwrap::wrap;

    let AskUserView {
        questions,
        current_tab,
        selected,
        answers,
        custom_inputs,
        custom_cursor,
        editing_custom,
    } = *view;

    let confirm_tab = questions.len();
    let is_confirm = current_tab == confirm_tab;
    let custom_input = custom_inputs
        .get(current_tab)
        .map(|s| s.as_str())
        .unwrap_or("");
    // Mirror the block geometry: inner = width - 2 border columns; text wraps
    // 2 columns narrower than the inner width
    let content_width = width.saturating_sub(2) as usize;
    let inner_w = width.saturating_sub(4) as usize;

    let mut lines: Vec<Line> = Vec::new();
    let mut selected_block = None;
    let mut editing_line_idx: Option<usize> = None;
    let mut cur_col = 0usize;

    // Tab bar: greedy-wrapped into as many rows as needed so the Confirm tab
    // always stays fully visible on narrow panels
    {
        let mut row_spans: Vec<Span> = Vec::new();
        let mut row_w = 0usize;
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
            let span = Span::styled(format!("{prefix} {} ", q.header), style);
            let w = span.content.width();
            if row_w > 0 && row_w + w > content_width {
                lines.push(Line::from(std::mem::take(&mut row_spans)));
                row_w = 0;
            }
            row_w += w;
            row_spans.push(span);
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
        let span = Span::styled(" Confirm ", style);
        let w = span.content.width();
        if row_w > 0 && row_w + w > content_width {
            lines.push(Line::from(std::mem::take(&mut row_spans)));
        }
        row_spans.push(span);
        lines.push(Line::from(row_spans));
    }
    let tab_rows = lines.len();

    if is_confirm {
        for (i, q) in questions.iter().enumerate() {
            let val = answers[i].as_deref().unwrap_or("(not answered)");
            // selected doubles as the confirm-page cursor: ↑↓ browse, windowing
            // keeps the cursor row in view
            let active = i == selected;
            let val_style = if answers[i].is_none() {
                Style::default().fg(Color::Red)
            } else if active {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} {}: ", if active { "▸" } else { " " }, q.header),
                    Style::default().fg(if active {
                        Color::White
                    } else {
                        Color::DarkGray
                    }),
                ),
                Span::styled(val.to_string(), val_style),
            ]));
            if active {
                selected_block = Some((lines.len() - 2, lines.len() - 1));
            }
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
            let block_start = lines.len();
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
            if active {
                selected_block = Some((block_start - 1, lines.len() - 1));
            }
        }

        let custom_idx = q.options.len();
        let active = selected == custom_idx;
        let block_start = lines.len();
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
            let (wrapped, cur_row, cur_col_at_cursor) = wrap_input(custom_input, custom_cursor, tw);
            cur_col = cur_col_at_cursor;

            for (li, wline) in wrapped.iter().enumerate() {
                if li == cur_row {
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
                lines.push(Line::from(spans));
            }
        }
        if active {
            selected_block = Some((block_start - 1, lines.len() - 1));
        }
    }

    AskUserLines {
        lines,
        tab_rows,
        selected_block,
        editing_line_idx,
        cur_col,
    }
}

/// Windows the body (tab rows excluded) to `visible` rows, keeping the selected
/// block in view. Returns the shown lines and the body offset they start at.
fn window_ask_user_lines(
    mut lines: Vec<Line<'static>>,
    visible: usize,
    tab_rows: usize,
    selected_block: Option<(usize, usize)>,
) -> (Vec<Line<'static>>, usize) {
    let tab_rows = tab_rows.min(lines.len()).max(1);
    let body: Vec<Line<'static>> = lines.drain(tab_rows..).collect();
    let body_len = body.len();
    let body_visible = visible.saturating_sub(tab_rows).max(1);
    if body_len <= body_visible {
        let mut shown = lines;
        shown.extend(body);
        return (shown, 0);
    }

    let (sel_start, sel_end) = selected_block.unwrap_or((0, 0));
    // A block taller than the window is shown from its start
    let win_start = sel_end
        .saturating_sub(body_visible)
        .min(sel_start)
        .min(body_len - body_visible);
    let win_end = (win_start + body_visible).min(body_len);
    let mut shown = lines;
    shown.extend_from_slice(&body[win_start..win_end]);
    (shown, win_start)
}

/// Renders the question panel into the input slot and returns the custom-input
/// cursor position, if editing
pub(crate) fn render_ask_user_panel(
    rect: Rect,
    buf: &mut Buffer,
    view: &AskUserView<'_>,
) -> Option<(u16, u16)> {
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    let built = build_ask_user_lines(view, rect.width);
    let is_confirm = view.current_tab == view.questions.len();

    Clear.render(rect, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray))
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            " ? ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    block.render(rect, buf);

    let [content_area, help_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);
    let visible = content_area.height as usize;

    let (lines, win_start) =
        window_ask_user_lines(built.lines, visible, built.tab_rows, built.selected_block);
    Paragraph::new(lines).render(content_area, buf);

    let help_text = if is_confirm {
        "↑↓=browse  enter=submit  esc=cancel"
    } else {
        "←→=tab  ↑↓=select  1-9=pick  enter=confirm  wheel/pgup=scroll  esc=cancel"
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

    // The tab rows stay; body rows shift by win_start
    built
        .editing_line_idx
        .map(|li| li.saturating_sub(win_start))
        .filter(|li| *li < visible)
        .map(|li| {
            let cx = (content_area.x + 5 + built.cur_col as u16)
                .min(content_area.x + content_area.width.saturating_sub(1));
            (cx, content_area.y + li as u16)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::event::{QuestionInfo, QuestionOption};

    fn question_with_options(labels: &[&str]) -> QuestionInfo {
        QuestionInfo {
            question: "Which framework?".to_string(),
            header: "Framework".to_string(),
            options: labels
                .iter()
                .map(|l| QuestionOption {
                    label: (*l).to_string(),
                    description: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn format_ask_reply_marks_custom_answers() {
        let q = question_with_options(&["React", "Vue"]);

        // Picked option: unmarked
        assert_eq!(
            format_ask_reply(&[q.clone()], &[Some("React".to_string())]),
            "\"Which framework?\"=\"React\""
        );

        // Free text typed by the user: marked
        assert_eq!(
            format_ask_reply(&[q.clone()], &[Some("Svelte with Vite".to_string())]),
            "\"Which framework?\"=\"[Custom input] Svelte with Vite\""
        );

        // Unanswered: unmarked
        assert_eq!(
            format_ask_reply(&[q.clone()], &[None]),
            "\"Which framework?\"=\"Unanswered\""
        );

        // Free text that happens to equal an option label: unmarked
        assert_eq!(
            format_ask_reply(&[q], &[Some("Vue".to_string())]),
            "\"Which framework?\"=\"Vue\""
        );
    }

    #[test]
    fn format_ask_reply_multiple_questions_mixed_answers() {
        let qs = [
            question_with_options(&["Fast", "Safe"]),
            QuestionInfo {
                question: "Any notes?".to_string(),
                header: "Notes".to_string(),
                options: Vec::new(),
            },
        ];
        let ans = [Some("Fast".to_string()), Some("keep it small".to_string())];
        assert_eq!(
            format_ask_reply(&qs, &ans),
            "\"Which framework?\"=\"Fast\", \"Any notes?\"=\"[Custom input] keep it small\""
        );
    }

    #[test]
    fn panel_height_matches_built_lines() {
        // The measure and the renderer share build_ask_user_lines, so an
        // unclamped panel always fits: tab row + body + 2 borders + 2 help rows
        let q = question_with_options(&["React", "Vue", "Svelte"]);
        let answers: Vec<Option<String>> = vec![None];
        let inputs = vec![String::new()];
        for (tab, sel, editing) in [(0, 0, false), (0, 3, true), (1, 0, false)] {
            let view = AskUserView {
                questions: std::slice::from_ref(&q),
                current_tab: tab,
                selected: sel,
                answers: &answers,
                custom_inputs: &inputs,
                custom_cursor: 0,
                editing_custom: editing,
            };
            let built = build_ask_user_lines(&view, 60);
            assert_eq!(
                ask_user_panel_height(&view, 60) as usize,
                built.lines.len() + 4
            );
        }
    }

    #[test]
    fn window_keeps_selected_block_visible() {
        // 1 tab row + 8 body rows
        let lines: Vec<Line<'static>> = (0..9).map(|i| Line::from(format!("row {i}"))).collect();
        // Fits: unchanged
        let (shown, start) = window_ask_user_lines(lines.clone(), 9, 1, Some((7, 8)));
        assert_eq!((shown.len(), start), (9, 0));
        // Clamped to 4 rows (3 body): block [7,8) → body 5..8
        let (shown, start) = window_ask_user_lines(lines.clone(), 4, 1, Some((7, 8)));
        assert_eq!((shown.len(), start), (4, 5));
        assert_eq!(shown[0].to_string(), "row 0");
        assert_eq!(shown[3].to_string(), "row 8");
        // Block taller than the window: shown from its start
        let (shown, start) = window_ask_user_lines(lines.clone(), 3, 1, Some((2, 7)));
        assert_eq!(start, 2);
        assert_eq!(shown[2].to_string(), "row 4");
        // No selection: top rows
        let (_, start) = window_ask_user_lines(lines, 4, 1, None);
        assert_eq!(start, 0);
    }

    #[test]
    fn tab_bar_wraps_when_narrow() {
        // Six headers do not fit a 40-column panel: the tab bar wraps onto
        // multiple rows and Confirm stays fully visible on the last row
        let qs: Vec<QuestionInfo> = (0..6)
            .map(|i| QuestionInfo {
                question: format!("question {i}"),
                header: format!("Q{i} header"),
                options: Vec::new(),
            })
            .collect();
        let answers: Vec<Option<String>> = vec![None; qs.len()];
        let view = AskUserView {
            questions: &qs,
            current_tab: 0,
            selected: 0,
            answers: &answers,
            custom_inputs: &[],
            custom_cursor: 0,
            editing_custom: false,
        };
        let built = build_ask_user_lines(&view, 40);
        assert!(built.tab_rows > 1);
        let last = &built.lines[built.tab_rows - 1];
        let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Confirm"));
        // Height accounting covers the extra tab rows
        assert_eq!(
            ask_user_panel_height(&view, 40) as usize,
            built.lines.len() + 4
        );
    }
}
