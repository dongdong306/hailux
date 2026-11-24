use color_eyre::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::super::types::QueuedItem;
use super::super::{App, AppState};
use crate::tui::event::AppEvent;

impl App {
    pub(in crate::tui::app) fn open_queue_manager(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.state = AppState::QueueManager { selected_index: 0 };
    }

    pub(in crate::tui::app) async fn handle_queue_manager_event(
        &mut self,
        event: AppEvent,
    ) -> Result<()> {
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };
        let AppState::QueueManager { selected_index } = &self.state else {
            return Ok(());
        };
        let selected = *selected_index;
        let queue_len = self.queue.len();

        enum Action {
            None,
            Close,
            Delete(usize),
            Edit(usize),
            InterjectOne(String),
        }
        let action = match key.code {
            KeyCode::Esc => Action::Close,
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Close,
            KeyCode::Up if selected > 0 => {
                if let AppState::QueueManager { selected_index } = &mut self.state {
                    *selected_index -= 1;
                }
                Action::None
            }
            KeyCode::Down if queue_len > 0 && selected + 1 < queue_len => {
                if let AppState::QueueManager { selected_index } = &mut self.state {
                    *selected_index += 1;
                }
                Action::None
            }
            // In-flight items are being delivered; Enter would double-send
            KeyCode::Enter if queue_len > 0 && selected < queue_len => {
                let item = &self.queue[selected];
                if item.in_flight {
                    Action::None
                } else {
                    Action::InterjectOne(item.client_id.clone())
                }
            }
            // Bare keys only: Ctrl+D is the global quit chord (muscle memory
            // from the chat state) and must not delete a queued item
            KeyCode::Char('d') | KeyCode::Char('D')
                if key.modifiers.is_empty() && queue_len > 0 =>
            {
                let item = &self.queue[selected];
                if item.in_flight {
                    Action::None
                } else {
                    Action::Delete(selected)
                }
            }
            KeyCode::Char('e') | KeyCode::Char('E')
                if key.modifiers.is_empty() && queue_len > 0 =>
            {
                let item = &self.queue[selected];
                if item.in_flight {
                    Action::None
                } else {
                    Action::Edit(selected)
                }
            }
            _ => Action::None,
        };

        match action {
            Action::None => {}
            Action::Close => self.state = AppState::Chat,
            Action::Delete(idx) => {
                if idx < self.queue.len() {
                    self.queue.remove(idx);
                    self.render.dirty = true;
                    if self.queue.is_empty() {
                        self.state = AppState::Chat;
                    } else if let AppState::QueueManager { selected_index } = &mut self.state {
                        *selected_index = (*selected_index).min(self.queue.len() - 1);
                    }
                }
            }
            Action::Edit(idx) => {
                if idx < self.queue.len() {
                    let item = self.queue.remove(idx);
                    self.input.clear();
                    self.input.insert_str(&item.text);
                    self.pending_images = item.images;
                    // Restore mention state as pending mentions (display, abs),
                    // both with the '@' prefix — re-submitting re-expands them
                    // inline, never as appended "[Image N]" placeholders
                    self.file_picker.pending_mentions = item
                        .mention_display
                        .iter()
                        .map(|(abs, display)| (format!("@{display}"), format!("@{abs}")))
                        .collect();
                    self.state = AppState::Chat;
                    self.refresh_suggestions();
                    self.render.dirty = true;
                }
            }
            Action::InterjectOne(client_id) => {
                self.state = AppState::Chat;
                self.interject_one(&client_id).await?;
            }
        }
        Ok(())
    }
}

pub(in crate::tui::app) fn render_queue_manager(
    area: Rect,
    buf: &mut Buffer,
    queue: &[QueuedItem],
    selected_index: usize,
) {
    const MAX_DIALOG_HEIGHT: u16 = 20;

    let dialog_width = (area.width as usize).min(72) as u16;
    let max_height = (area.height as usize)
        .saturating_sub(4)
        .min(MAX_DIALOG_HEIGHT as usize)
        .max(5) as u16;
    let dialog_height = (queue.len() as u16 + 3).clamp(5, max_height);
    let dialog_x = (area.width.saturating_sub(dialog_width)) / 2;
    let dialog_y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(dialog_x, dialog_y, dialog_width, dialog_height);

    Clear.render(dialog_area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Gray))
        .border_type(BorderType::Rounded)
        .title(
            Line::from(Span::styled(
                format!(" 排队消息 ({}) ", queue.len()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .centered(),
        )
        .title_bottom(
            Line::from(Span::styled(
                "↑↓ 选择 · Enter 插话 · E 编辑 · D 删除 · Esc 关闭",
                Style::default().fg(Color::DarkGray),
            ))
            .centered(),
        );
    let inner = block.inner(dialog_area);
    block.render(dialog_area, buf);

    let text_width = inner.width.saturating_sub(4) as usize;
    let padded_visible = inner.height.saturating_sub(1) as usize;
    let scroll = selected_index.saturating_sub(padded_visible.saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();
    if scroll == 0 {
        lines.push(Line::from(""));
    }

    for (i, item) in queue.iter().enumerate().skip(scroll) {
        let images = item.images.len() + item.mention_images.len();
        let mut text = item.text.replace('\n', " ");
        if images > 0 {
            text.push_str(&format!("  [图片 ×{images}]"));
        }
        if item.in_flight {
            text.push_str("  [发送中]");
        }
        let mut w = 0usize;
        let mut cut = String::new();
        for ch in text.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > text_width.saturating_sub(4) {
                cut.push('…');
                break;
            }
            w += cw;
            cut.push(ch);
        }
        let style = if item.in_flight {
            Style::default().fg(Color::DarkGray)
        } else if i == selected_index {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let marker = if i == selected_index { "> " } else { "  " };
        lines.push(Line::from(Span::styled(format!("{marker}{}", cut), style)));
    }

    Paragraph::new(lines).render(inner, buf);
}
