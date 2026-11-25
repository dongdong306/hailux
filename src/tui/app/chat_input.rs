use color_eyre::Result;
#[cfg(windows)]
use crossterm::event::KeyEventKind;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

use super::attachments::read_image_attachment;
use super::paste::{CharDecision, FlushResult};
use super::types::Message;
use super::{App, EscClearedSnapshot};
use crate::tui::command;
use crate::tui::event::AppEvent;
use crate::tui::input::ElementKind;

const LARGE_PASTE_CHAR_THRESHOLD: usize = 200;

impl App {
    pub(super) async fn handle_chat_key(&mut self, key: KeyEvent) -> Result<()> {
        // Ctrl+V Release leak (Windows Terminal only): with an image
        // clipboard, conhost drops the empty bracketed-paste sequence, which
        // is the only reachable signal — probe silently, deduped by the
        // 300ms window against paired Press+Release arrivals
        #[cfg(windows)]
        if key.kind == KeyEventKind::Release
            && matches!(
                key.code,
                KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('\x16')
            )
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            let recent_press = self
                .last_ctrl_v_press
                .is_some_and(|t| t.elapsed() < Duration::from_millis(300));
            if !recent_press {
                self.spawn_clipboard_image_probe(false);
            }
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('d') => {
                    self.should_quit = true;
                    return Ok(());
                }
                KeyCode::Char('x') => {
                    self.open_session_picker().await?;
                    return Ok(());
                }
                KeyCode::Char('n') => {
                    self.create_new_session().await?;
                    return Ok(());
                }
                KeyCode::Char('m') => {
                    self.open_model_picker();
                    return Ok(());
                }
                KeyCode::Char('q') => {
                    self.open_queue_manager();
                    return Ok(());
                }
                KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('\x16') => {
                    self.spawn_clipboard_image_probe(true);
                    self.last_ctrl_v_press = Some(Instant::now());
                    return Ok(());
                }
                _ => {}
            }
        }

        if key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('\x16'))
        {
            self.spawn_clipboard_image_probe(true);
            return Ok(());
        }

        if self.is_processing {
            let now = Instant::now();
            let input_empty = self.input.text().is_empty();
            match key.code {
                KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.thinking_collapsed = !self.thinking_collapsed;
                    self.render.dirty = true;
                    return Ok(());
                }
                KeyCode::Esc => {
                    if self
                        .last_esc_time
                        .is_some_and(|t| now.duration_since(t) < Duration::from_secs(5))
                    {
                        self.last_esc_time = None;
                        self.esc_hint_active = false;
                        self.agent.interrupt();
                    } else {
                        self.last_esc_time = Some(now);
                        self.esc_hint_active = true;
                    }
                    return Ok(());
                }
                KeyCode::Enter => {
                    if self.paste_burst.append_newline_if_active(now) {
                        return Ok(());
                    }
                    let want_newline = key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT)
                        || self.paste_burst.newline_should_insert(now);
                    if want_newline {
                        self.input.insert_str("\n");
                        if self.paste_burst.newline_should_insert(now) {
                            self.paste_burst.extend_window(now);
                        }
                    } else {
                        self.enqueue_current();
                    }
                    return Ok(());
                }
                KeyCode::Up if input_empty => {
                    self.scroll_chat(3);
                    return Ok(());
                }
                KeyCode::Down if input_empty => {
                    self.scroll_chat(-3);
                    return Ok(());
                }
                KeyCode::PageUp if input_empty => {
                    self.scroll_chat(20);
                    return Ok(());
                }
                KeyCode::PageDown if input_empty => {
                    self.scroll_chat(-20);
                    return Ok(());
                }
                _ => {}
            }
        }

        let now = Instant::now();
        self.handle_paste_burst_flush(now);
        if self.file_picker.active {
            match key.code {
                KeyCode::Up => {
                    if self.file_picker.selected > 0 {
                        self.file_picker.selected -= 1;
                    }
                    return Ok(());
                }
                KeyCode::Down => {
                    if self.file_picker.selected + 1 < self.file_picker.results.len() {
                        self.file_picker.selected += 1;
                    }
                    return Ok(());
                }
                KeyCode::Tab => {
                    self.select_file();
                    return Ok(());
                }
                KeyCode::Enter => {
                    if self.paste_burst.append_newline_if_active(now) {
                        return Ok(());
                    }
                    let want_newline = key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT)
                        || self.paste_burst.newline_should_insert(now);
                    if !want_newline {
                        self.select_file();
                        return Ok(());
                    }
                }
                KeyCode::Esc => {
                    self.file_picker.active = false;
                    return Ok(());
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => {
                if self.cmd_suggestion.show && self.paste_burst.is_active() {
                    if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                        self.handle_paste(pasted);
                    }
                    self.refresh_suggestions();
                }
                if self.paste_burst.append_newline_if_active(now) {
                    return Ok(());
                }
                let want_newline = key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT)
                    || self.paste_burst.newline_should_insert(now);
                if want_newline {
                    self.input.insert_str("\n");
                    if self.paste_burst.newline_should_insert(now) {
                        self.paste_burst.extend_window(now);
                    }
                } else if self.cmd_suggestion.show && !self.cmd_suggestion.items.is_empty() {
                    self.apply_suggestion();
                } else {
                    let raw = self.input.text().to_string();
                    let input = self.expand_pending_pastes(&raw);
                    let mut attachments = self.take_mention_images(&input);
                    let (input, mention_display) = self.expand_file_mentions(&input);
                    attachments.extend(self.take_pending_images());
                    if !input.trim().is_empty() || !attachments.is_empty() {
                        self.cmd_suggestion.show = false;
                        let _ = self.events.0.try_send(AppEvent::UserSubmit {
                            text: input,
                            attachments,
                            mentions: mention_display,
                        });
                    }
                }
            }
            KeyCode::BackTab => {
                self.toggle_plan_mode();
                return Ok(());
            }
            KeyCode::Tab => {
                if self.cmd_suggestion.show && !self.cmd_suggestion.items.is_empty() {
                    self.apply_suggestion();
                }
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.thinking_collapsed = !self.thinking_collapsed;
                self.render.dirty = true;
            }
            KeyCode::Char(c) => {
                let has_ctrl_or_alt = key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT);
                if !has_ctrl_or_alt {
                    self.handle_plain_char(c, now);
                    self.refresh_suggestions();
                    self.refresh_file_picker();
                    return Ok(());
                }
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.input.insert_str(&c.to_string());
                self.refresh_suggestions();
                self.refresh_file_picker();
            }
            KeyCode::Backspace => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                let elements_before = self.snapshot_elements();
                self.input.delete_backward();
                self.reconcile_deleted_elements(&elements_before);
                self.refresh_suggestions();
                self.refresh_file_picker();
            }
            KeyCode::Delete => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                let elements_before = self.snapshot_elements();
                self.input.delete_forward();
                self.reconcile_deleted_elements(&elements_before);
                self.refresh_suggestions();
                self.refresh_file_picker();
            }
            KeyCode::Left => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                self.input.move_left();
            }
            KeyCode::Right => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                self.input.move_right();
            }
            KeyCode::Home => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                self.input.move_to_start();
            }
            KeyCode::End => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                self.input.move_to_end();
            }
            KeyCode::Esc => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                if self.cmd_suggestion.show {
                    self.cmd_suggestion.show = false;
                } else {
                    // An Esc byte inside `\e[200~`/`\e[201~` lands here: snapshot
                    // and restore wholesale if a paste marker follows
                    let snapshot = EscClearedSnapshot::take_from(self);
                    self.esc_cleared = Some((snapshot, Instant::now()));
                    self.input.clear();
                    self.file_picker.reset();
                    self.refresh_suggestions();
                }
            }
            KeyCode::Up => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                if self.cmd_suggestion.show && !self.cmd_suggestion.items.is_empty() {
                    if self.cmd_suggestion.selected > 0 {
                        self.cmd_suggestion.selected -= 1;
                    }
                } else if self.input.cursor_on_first_visual_row() {
                    self.input.history_prev();
                } else {
                    self.input.move_cursor_up();
                }
            }
            KeyCode::Down => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                if self.cmd_suggestion.show && !self.cmd_suggestion.items.is_empty() {
                    if self.cmd_suggestion.selected + 1 < self.cmd_suggestion.items.len() {
                        self.cmd_suggestion.selected += 1;
                    }
                } else if self.input.cursor_on_last_visual_row() {
                    self.input.history_next();
                } else {
                    self.input.move_cursor_down();
                }
            }
            KeyCode::PageUp => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                self.scroll_chat(20);
            }
            KeyCode::PageDown => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                self.scroll_chat(-20);
            }
            _ => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
            }
        }
        Ok(())
    }

    pub(super) fn handle_plain_char(&mut self, ch: char, now: Instant) {
        match self.paste_burst.on_plain_char(ch, now) {
            CharDecision::RetainFirstChar => {}
            CharDecision::BeginBufferFromPending => {
                self.paste_burst.append_char_to_buffer(ch, now);
            }
            CharDecision::BeginBuffer { retro_chars } => {
                let before = self.input.text_before_cursor().to_string();
                let (start_byte, _) =
                    self.paste_burst
                        .decide_begin_buffer(now, &before, retro_chars as usize);
                self.input.drain_raw(start_byte..self.input.cursor());
                self.paste_burst.append_char_to_buffer(ch, now);
            }
            CharDecision::BufferAppend => {
                self.paste_burst.append_char_to_buffer(ch, now);
            }
        }
    }

    pub(super) fn handle_paste_burst_flush(&mut self, now: Instant) {
        match self.paste_burst.flush_if_due(now) {
            FlushResult::Paste(pasted) => {
                self.handle_paste(pasted);
                self.refresh_file_picker();
            }
            FlushResult::Typed(ch) => {
                self.input.insert_str(&ch.to_string());
                self.refresh_suggestions();
                self.refresh_file_picker();
            }
            FlushResult::None => {}
        }
    }

    pub(super) fn handle_paste(&mut self, text: String) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (clean, saw_start, saw_end) = Self::bracket_marker_info(&text);
        let now = Instant::now();
        if self
            .bracket_paste
            .at
            .is_some_and(|t| now.duration_since(t) > Duration::from_secs(1))
        {
            self.bracket_paste = Default::default();
        }
        if saw_start || saw_end {
            // The sequence's own Esc byte already wiped the input box —
            // restore wholesale within the short window
            if let Some((snapshot, at)) = self.esc_cleared.take()
                && now.duration_since(at) < Duration::from_millis(500)
            {
                self.input.insert_str(&snapshot.text);
                self.pending_images = snapshot.pending_images;
                self.pending_pastes = snapshot.pending_pastes;
                self.file_picker.pending_mentions = snapshot.mentions;
                self.refresh_suggestions();
            }
        }
        if saw_start {
            self.bracket_paste.pending_start = true;
            self.bracket_paste.at = Some(now);
        }
        if saw_end && self.bracket_paste.pending_start {
            self.bracket_paste = Default::default();
            if clean.is_empty() {
                // Image-only clipboard (WT convention)
                let _ = self.events.0.try_send(AppEvent::PasteImageProbe);
                self.paste_burst.clear_after_explicit_paste();
                return;
            }
        }

        let char_count = clean.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            let element_text = format!(" {} ", placeholder);
            self.input.insert_element(&element_text, ElementKind::Paste);
            self.pending_pastes.push((element_text, clean));
        } else if !clean.is_empty() {
            if let Some(path) = Self::image_path_from_paste(&clean)
                && let Some(attachment) = read_image_attachment(&path)
            {
                self.attach_image(attachment);
                // Dedupes against the leak-probe result (WeChat/QQ double-channel copies)
                self.path_image_attached_at = Some(Instant::now());
                self.render.dirty = true;
            } else {
                self.input.insert_str(&clean);
            }
        }
        self.paste_burst.clear_after_explicit_paste();
        self.refresh_suggestions();
    }

    pub(super) fn enqueue_current(&mut self) {
        let raw = self.input.text().to_string();
        // UI commands cannot be queued: bail before any take/expand consumes
        // pending state the input box still displays (placeholders would
        // survive while the data behind them is gone)
        let first_word = raw.split_whitespace().next().unwrap_or("");
        if let Some(name) = first_word.strip_prefix('/')
            && let Some(entry) = self.command_entries.iter().find(|e| e.name == name)
            && entry.is_ui
        {
            self.messages.push(Message::Agent(
                "UI 命令无法排队，请等待当前任务结束后直接执行".to_string(),
            ));
            self.render.dirty = true;
            return;
        }
        let input = self.expand_pending_pastes(&raw);
        let mention_images = self.take_mention_images(&input);
        let (text, mention_display) = self.expand_file_mentions(&input);
        let images = std::mem::take(&mut self.pending_images);
        if text.trim().is_empty() && images.is_empty() && mention_images.is_empty() {
            return;
        }
        self.queue.push(crate::tui::app::types::QueuedItem {
            client_id: uuid::Uuid::new_v4().to_string(),
            text,
            images,
            mention_images,
            mention_display,
            in_flight: false,
        });
        self.input.clear();
        self.pending_pastes.clear();
        self.file_picker.reset();
        self.cmd_suggestion.show = false;
        self.refresh_suggestions();
        self.render.dirty = true;
    }

    fn next_large_paste_placeholder(&self, char_count: usize) -> String {
        let base = format!("[Pasted Content {} chars]", char_count);
        let duplicate_count = self
            .pending_pastes
            .iter()
            .filter(|(p, _)| p.contains(&base))
            .count();
        if duplicate_count == 0 {
            base
        } else {
            format!("{} #{}", base, duplicate_count + 1)
        }
    }

    fn bracket_marker_info(text: &str) -> (String, bool, bool) {
        let saw_start = text.contains("[200~");
        let saw_end = text.contains("[201~");
        if !saw_start && !saw_end {
            return (text.to_string(), false, false);
        }
        (
            text.replace("[200~", "").replace("[201~", ""),
            saw_start,
            saw_end,
        )
    }

    fn image_path_from_paste(text: &str) -> Option<std::path::PathBuf> {
        let trimmed = text.trim().trim_matches('"').trim();
        if trimmed.is_empty() || trimmed.len() > 1024 {
            return None;
        }
        if trimmed.contains('"') || trimmed.contains('\n') {
            return None;
        }
        let path = std::path::Path::new(trimmed);
        if !path.is_absolute() {
            return None;
        }
        crate::agent::media::mime_from_extension(path)?;
        Some(path.to_path_buf())
    }

    pub(super) fn expand_pending_pastes(&mut self, text: &str) -> String {
        if self.pending_pastes.is_empty() {
            return text.to_string();
        }
        let mut result = text.to_string();
        for (element_text, content) in &self.pending_pastes {
            result = result.replace(element_text, content);
        }
        self.pending_pastes.clear();
        result
    }

    pub(super) fn expand_file_mentions(&mut self, text: &str) -> (String, Vec<(String, String)>) {
        let pending = std::mem::take(&mut self.file_picker.pending_mentions);
        if pending.is_empty() {
            return (text.to_string(), Vec::new());
        }
        let mut result = text.to_string();
        let mut display_map = Vec::with_capacity(pending.len());
        for (display, abs) in &pending {
            let abs_spaced = format!("{abs} ");
            result = result.replace(&format!("{display} "), &abs_spaced);
            result = result.replace(display, &abs_spaced);
            let value = abs.strip_prefix('@').unwrap_or(abs);
            display_map.push((value.to_string(), display[1..].to_string()));
        }
        (result, display_map)
    }

    fn snapshot_elements(&self) -> Vec<String> {
        if self.pending_pastes.is_empty()
            && self.file_picker.pending_mentions.is_empty()
            && self.pending_images.is_empty()
        {
            Vec::new()
        } else {
            self.input.element_payloads()
        }
    }

    fn reconcile_deleted_elements(&mut self, before: &[String]) {
        if before.is_empty() {
            return;
        }
        let removed = self.input.removed_elements(before);
        for payload in &removed {
            self.pending_pastes.retain(|(ph, _)| ph != payload);
            self.pending_images.retain(|(ph, _)| ph != payload);
            let trimmed = payload.trim();
            self.file_picker
                .pending_mentions
                .retain(|(display, _)| display != trimmed);
        }
    }

    pub(super) fn refresh_suggestions(&mut self) {
        if self.input.text().starts_with('/') {
            let trimmed = self.input.text().trim_start();
            let prefix = trimmed[1..].trim_start();
            let cmd_prefix = prefix.split_whitespace().next().unwrap_or(prefix);
            let indices = command::filter_completions(&self.command_entries, cmd_prefix);
            self.cmd_suggestion.items = indices
                .iter()
                .map(|&i| self.command_entries[i].clone())
                .collect();
            if self.cmd_suggestion.items.is_empty() {
                self.cmd_suggestion.show = false;
            } else {
                self.cmd_suggestion.show = true;
                self.cmd_suggestion.selected = 0;
            }
        } else {
            self.cmd_suggestion.show = false;
            self.cmd_suggestion.items.clear();
        }
    }

    fn apply_suggestion(&mut self) {
        if let Some(cmd) = self
            .cmd_suggestion
            .items
            .get(self.cmd_suggestion.selected)
            .cloned()
        {
            let text = format!("/{} ", cmd.name);
            self.input.set_text(text);
            self.pending_images.clear();
            self.pending_pastes.clear();
            self.file_picker.pending_mentions.clear();
            self.cmd_suggestion.show = false;
            self.cmd_suggestion.items.clear();
            if cmd.is_ui {
                let _ = self.events.0.try_send(AppEvent::UserSubmit {
                    text: format!("/{}", cmd.name),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                });
            }
        }
    }

    fn collect_paths(&self) -> Vec<(String, String)> {
        use ignore::WalkBuilder;
        let work_path = std::path::Path::new(&self.work_dir);
        let mut paths = Vec::new();
        let walker = WalkBuilder::new(work_path)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(|entry| {
                !crate::agent::IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
            })
            .build();
        for entry in walker.flatten() {
            if entry.path() == work_path {
                continue;
            }
            let Some(abs) = entry.path().to_str() else {
                continue;
            };
            let ft = entry.file_type();
            if ft.is_some_and(|ft| ft.is_file()) {
                let lower = abs.to_lowercase();
                paths.push((abs.to_string(), lower));
            } else if ft.is_some_and(|ft| ft.is_dir()) {
                let mut s = abs.to_string();
                s.push(std::path::MAIN_SEPARATOR);
                let lower = s.to_lowercase();
                paths.push((s, lower));
            }
        }
        paths.sort_by(|a, b| a.0.cmp(&b.0));
        paths
    }

    pub(super) fn refresh_file_picker(&mut self) {
        let before_cursor = self.input.text_before_cursor();
        let Some(query) = Self::file_picker_query(before_cursor) else {
            self.file_picker.active = false;
            return;
        };
        if self.file_picker.cached.is_none() {
            self.file_picker.cached = Some(self.collect_paths());
        }
        let cached = self.file_picker.cached.as_ref().unwrap();
        let q_lower = query.to_lowercase();
        let limit = if query.is_empty() { 20 } else { 15 };
        let mut results = Vec::with_capacity(limit);
        for (path, lower) in cached {
            if query.is_empty() || lower.contains(&q_lower) {
                results.push(path.clone());
                if results.len() >= limit {
                    break;
                }
            }
        }
        if results.is_empty() {
            self.file_picker.active = false;
        } else {
            self.file_picker.active = true;
            self.file_picker.results = results;
            self.file_picker.selected = 0;
        }
    }

    fn file_picker_query(before_cursor: &str) -> Option<&str> {
        let at_idx = before_cursor.rfind('@')?;
        let before_at = &before_cursor[..at_idx];
        if !before_at.is_empty() && !before_at.ends_with(|c: char| c.is_whitespace()) {
            return None;
        }
        let query = &before_cursor[at_idx + 1..];
        if query.contains(|c: char| c.is_whitespace()) {
            return None;
        }
        Some(query)
    }

    fn select_file(&mut self) {
        if let Some(abs_path) = self
            .file_picker
            .results
            .get(self.file_picker.selected)
            .cloned()
        {
            let is_dir = abs_path.ends_with(std::path::MAIN_SEPARATOR);
            let mut rel_path = std::path::Path::new(&abs_path)
                .strip_prefix(&self.work_dir)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| abs_path.clone());
            if is_dir && !rel_path.ends_with(std::path::MAIN_SEPARATOR) {
                rel_path.push(std::path::MAIN_SEPARATOR);
            }
            let before_cursor = self.input.text_before_cursor();
            if let Some(at_idx) = before_cursor.rfind('@') {
                self.input.drain_raw(at_idx..self.input.cursor());
                let display = format!("@{}", rel_path);
                let element_text = format!("{} ", display);
                self.input
                    .insert_element(&element_text, ElementKind::FileMention);
                self.file_picker
                    .pending_mentions
                    .push((display, format!("@{}", abs_path)));
            }
        }
        self.file_picker.active = false;
        self.refresh_suggestions();
    }
}

#[cfg(test)]
mod tests {
    use super::App;

    #[test]
    fn bracket_markers_start_and_end_fragments() {
        let (clean, start, end) = App::bracket_marker_info("[200~");
        assert_eq!((clean.as_str(), start, end), ("", true, false));
        let (clean, start, end) = App::bracket_marker_info("[201~");
        assert_eq!((clean.as_str(), start, end), ("", false, true));
    }

    #[test]
    fn bracket_markers_empty_paste_single_flush() {
        let (clean, start, end) = App::bracket_marker_info("[200~[201~");
        assert_eq!((clean.as_str(), start, end), ("", true, true));
    }

    #[test]
    fn bracket_markers_text_paste_stripped() {
        let (clean, start, end) = App::bracket_marker_info("[200~hello world[201~");
        assert_eq!((clean.as_str(), start, end), ("hello world", true, true));
        let (clean, start, end) = App::bracket_marker_info("[200~multi\nline[201~");
        assert_eq!((clean.as_str(), start, end), ("multi\nline", true, true));
    }

    #[test]
    fn bracket_markers_plain_text_untouched() {
        let (clean, start, end) = App::bracket_marker_info("just some pasted text");
        assert_eq!(
            (clean.as_str(), start, end),
            ("just some pasted text", false, false)
        );
        let (clean, _, _) = App::bracket_marker_info("");
        assert_eq!(clean, "");
    }

    #[test]
    fn image_path_from_paste_accepts_single_image_file() {
        #[cfg(windows)]
        let (raw, quoted) = (
            r#"C:\Users\a b\Pictures\shot.png"#,
            r#""C:\Users\a b\Pictures\shot.png""#,
        );
        #[cfg(not(windows))]
        let (raw, quoted) = (
            "/home/a b/Pictures/shot.png",
            "\"/home/a b/Pictures/shot.png\"",
        );
        assert!(App::image_path_from_paste(raw).is_some());
        assert!(App::image_path_from_paste(quoted).is_some());
    }

    #[test]
    fn image_path_from_paste_rejects_non_single_image() {
        assert!(App::image_path_from_paste(r#""C:\a.png" "C:\b.png""#).is_none());
        assert!(App::image_path_from_paste(r"pictures\shot.png").is_none());
        assert!(App::image_path_from_paste(r"C:\docs\readme.txt").is_none());
        assert!(App::image_path_from_paste("hello world").is_none());
        assert!(App::image_path_from_paste("").is_none());
    }
}
