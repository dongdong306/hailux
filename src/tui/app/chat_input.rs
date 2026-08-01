use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

use super::App;
use crate::tui::command;
use crate::tui::event::AppEvent;
use crate::tui::input::ElementKind;

// ===== PasteBurst state machine =====

const PASTE_BURST_MIN_CHARS: u16 = 3;
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);
#[cfg(not(windows))]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(8);
#[cfg(windows)]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(60);
const LARGE_PASTE_CHAR_THRESHOLD: usize = 200;

enum CharDecision {
    BeginBuffer { retro_chars: u16 },
    BufferAppend,
    RetainFirstChar,
    BeginBufferFromPending,
}

enum FlushResult {
    Paste(String),
    Typed(char),
    None,
}

pub(in crate::tui) struct PasteBurst {
    last_plain_char_time: Option<Instant>,
    consecutive_plain_char_burst: u16,
    burst_window_until: Option<Instant>,
    buffer: String,
    active: bool,
    pending_first_char: Option<(char, Instant)>,
}

impl PasteBurst {
    pub(in crate::tui) fn new() -> Self {
        Self {
            last_plain_char_time: None,
            consecutive_plain_char_burst: 0,
            burst_window_until: None,
            buffer: String::new(),
            active: false,
            pending_first_char: None,
        }
    }

    fn on_plain_char(&mut self, ch: char, now: Instant) -> CharDecision {
        self.note_plain_char(now);
        if self.active {
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return CharDecision::BufferAppend;
        }
        if let Some((held, held_at)) = self.pending_first_char
            && now.duration_since(held_at) <= PASTE_BURST_CHAR_INTERVAL
        {
            self.active = true;
            let _ = self.pending_first_char.take();
            self.buffer.push(held);
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return CharDecision::BeginBufferFromPending;
        }
        if self.consecutive_plain_char_burst >= PASTE_BURST_MIN_CHARS {
            return CharDecision::BeginBuffer {
                retro_chars: self.consecutive_plain_char_burst.saturating_sub(1),
            };
        }
        self.pending_first_char = Some((ch, now));
        CharDecision::RetainFirstChar
    }

    fn on_plain_char_no_hold(&mut self, now: Instant) -> Option<CharDecision> {
        self.note_plain_char(now);
        if self.active {
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return Some(CharDecision::BufferAppend);
        }
        if self.consecutive_plain_char_burst >= PASTE_BURST_MIN_CHARS {
            return Some(CharDecision::BeginBuffer {
                retro_chars: self.consecutive_plain_char_burst.saturating_sub(1),
            });
        }
        None
    }

    fn note_plain_char(&mut self, now: Instant) {
        match self.last_plain_char_time {
            Some(prev) if now.duration_since(prev) <= PASTE_BURST_CHAR_INTERVAL => {
                self.consecutive_plain_char_burst =
                    self.consecutive_plain_char_burst.saturating_add(1)
            }
            _ => self.consecutive_plain_char_burst = 1,
        }
        self.last_plain_char_time = Some(now);
    }

    fn flush_if_due(&mut self, now: Instant) -> FlushResult {
        let timeout = if self.is_active_internal() {
            PASTE_BURST_ACTIVE_IDLE_TIMEOUT
        } else {
            PASTE_BURST_CHAR_INTERVAL
        };
        let timed_out = self
            .last_plain_char_time
            .is_some_and(|t| now.duration_since(t) > timeout);
        if timed_out && self.is_active_internal() {
            self.active = false;
            FlushResult::Paste(std::mem::take(&mut self.buffer))
        } else if timed_out {
            if let Some((ch, _)) = self.pending_first_char.take() {
                FlushResult::Typed(ch)
            } else {
                FlushResult::None
            }
        } else {
            FlushResult::None
        }
    }

    fn append_newline_if_active(&mut self, now: Instant) -> bool {
        if self.is_active() {
            self.buffer.push('\n');
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            true
        } else {
            false
        }
    }

    fn newline_should_insert(&self, now: Instant) -> bool {
        let in_window = self.burst_window_until.is_some_and(|until| now <= until);
        self.is_active() || in_window
    }

    fn extend_window(&mut self, now: Instant) {
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    fn begin_with_retro_grabbed(&mut self, grabbed: String, now: Instant) {
        if !grabbed.is_empty() {
            self.buffer.push_str(&grabbed);
        }
        self.active = true;
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    fn append_char_to_buffer(&mut self, ch: char, now: Instant) {
        self.buffer.push(ch);
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    fn try_append_char_if_active(&mut self, ch: char, now: Instant) -> bool {
        if self.active || !self.buffer.is_empty() {
            self.append_char_to_buffer(ch, now);
            true
        } else {
            false
        }
    }

    fn decide_begin_buffer(
        &mut self,
        now: Instant,
        before: &str,
        retro_chars: usize,
    ) -> Option<(usize, String)> {
        let start_byte = retro_start_index(before, retro_chars);
        let grabbed = before[start_byte..].to_string();
        let looks_pastey =
            grabbed.chars().any(char::is_whitespace) || grabbed.chars().count() >= 16;
        if looks_pastey {
            self.begin_with_retro_grabbed(grabbed.clone(), now);
            Some((start_byte, grabbed))
        } else {
            None
        }
    }

    fn flush_before_modified_input(&mut self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        self.active = false;
        let mut out = std::mem::take(&mut self.buffer);
        if let Some((ch, _)) = self.pending_first_char.take() {
            out.push(ch);
        }
        Some(out)
    }

    fn clear_window_after_non_char(&mut self) {
        self.consecutive_plain_char_burst = 0;
        self.last_plain_char_time = None;
        self.burst_window_until = None;
        self.active = false;
        self.pending_first_char = None;
    }

    pub(in crate::tui) fn is_active(&self) -> bool {
        self.is_active_internal() || self.pending_first_char.is_some()
    }

    fn is_active_internal(&self) -> bool {
        self.active || !self.buffer.is_empty()
    }

    fn clear_after_explicit_paste(&mut self) {
        self.last_plain_char_time = None;
        self.consecutive_plain_char_burst = 0;
        self.burst_window_until = None;
        self.active = false;
        self.buffer.clear();
        self.pending_first_char = None;
    }

    pub(in crate::tui) fn flush_timeout(&self) -> Option<Duration> {
        if self.is_active_internal() {
            Some(PASTE_BURST_ACTIVE_IDLE_TIMEOUT)
        } else if self.pending_first_char.is_some() {
            Some(PASTE_BURST_CHAR_INTERVAL)
        } else {
            None
        }
    }
}

fn retro_start_index(before: &str, retro_chars: usize) -> usize {
    if retro_chars == 0 {
        return before.len();
    }
    before
        .char_indices()
        .rev()
        .nth(retro_chars.saturating_sub(1))
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

// ===== App keyboard input handling =====

impl App {
    pub(super) async fn handle_chat_key(&mut self, key: KeyEvent) -> Result<()> {
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
                _ => {}
            }
        }

        if self.is_processing {
            match key.code {
                KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.thinking_collapsed = !self.thinking_collapsed;
                    self.render.dirty = true;
                }
                KeyCode::Up => {
                    self.should_auto_scroll = false;
                    self.scroll_offset = self.scroll_offset.saturating_add(3);
                }
                KeyCode::Down => {
                    self.scroll_offset = self.scroll_offset.saturating_sub(3);
                }
                KeyCode::PageUp => {
                    self.should_auto_scroll = false;
                    self.scroll_offset = self.scroll_offset.saturating_add(20);
                }
                KeyCode::PageDown => {
                    self.scroll_offset = self.scroll_offset.saturating_sub(20);
                }
                KeyCode::Esc => {
                    let now = Instant::now();
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
                }
                _ => {}
            }
            return Ok(());
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
                    let input = self.expand_file_mentions(&input);
                    if !input.trim().is_empty() {
                        self.cmd_suggestion.show = false;
                        let _ = self.events.0.try_send(AppEvent::UserSubmit(input));
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
                    if !c.is_ascii() {
                        self.handle_non_ascii_char(c, now);
                    } else {
                        self.handle_ascii_char(c, now);
                    }
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
                    self.input.clear();
                    self.pending_pastes.clear();
                    self.file_picker.pending_mentions.clear();
                    self.refresh_suggestions();
                    self.file_picker.active = false;
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
                self.should_auto_scroll = false;
                self.scroll_offset = self.scroll_offset.saturating_add(20);
            }
            KeyCode::PageDown => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
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

    pub(super) fn handle_ascii_char(&mut self, c: char, now: Instant) {
        match self.paste_burst.on_plain_char(c, now) {
            CharDecision::RetainFirstChar => {}
            CharDecision::BeginBufferFromPending => {
                self.paste_burst.append_char_to_buffer(c, now);
            }
            CharDecision::BeginBuffer { retro_chars } => {
                let before = self.input.text_before_cursor().to_string();
                if let Some((start_byte, _)) =
                    self.paste_burst
                        .decide_begin_buffer(now, &before, retro_chars as usize)
                {
                    self.input.drain_raw(start_byte..self.input.cursor());
                    self.paste_burst.append_char_to_buffer(c, now);
                } else {
                    self.input.insert_str(&c.to_string());
                    self.refresh_suggestions();
                }
            }
            CharDecision::BufferAppend => {
                self.paste_burst.append_char_to_buffer(c, now);
            }
        }
    }

    pub(super) fn handle_non_ascii_char(&mut self, ch: char, now: Instant) {
        if self.paste_burst.try_append_char_if_active(ch, now) {
            return;
        }
        if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        if let Some(decision) = self.paste_burst.on_plain_char_no_hold(now) {
            match decision {
                CharDecision::BufferAppend => {
                    self.paste_burst.append_char_to_buffer(ch, now);
                    return;
                }
                CharDecision::BeginBuffer { retro_chars } => {
                    let before = self.input.text_before_cursor().to_string();
                    if let Some((start_byte, _)) =
                        self.paste_burst
                            .decide_begin_buffer(now, &before, retro_chars as usize)
                    {
                        self.input.drain_raw(start_byte..self.input.cursor());
                        self.paste_burst.append_char_to_buffer(ch, now);
                        return;
                    }
                }
                _ => {}
            }
        }
        if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        self.input.insert_str(&ch.to_string());
        self.refresh_suggestions();
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
        let char_count = text.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            let element_text = format!(" {} ", placeholder);
            self.input.insert_element(&element_text, ElementKind::Paste);
            self.pending_pastes.push((element_text, text));
        } else {
            self.input.insert_str(&text);
        }
        self.paste_burst.clear_after_explicit_paste();
        self.refresh_suggestions();
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

    pub(super) fn expand_file_mentions(&mut self, text: &str) -> String {
        if self.file_picker.pending_mentions.is_empty() {
            return text.to_string();
        }
        let mut result = text.to_string();
        for (display, abs) in &self.file_picker.pending_mentions {
            let abs_spaced = format!("{} ", abs);
            result = result.replace(&format!("{} ", display), &abs_spaced);
            result = result.replace(display, &abs_spaced);
        }
        self.file_picker.pending_mentions.clear();
        result
    }

    fn snapshot_elements(&self) -> Vec<String> {
        if self.pending_pastes.is_empty() && self.file_picker.pending_mentions.is_empty() {
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
            self.cmd_suggestion.show = false;
            self.cmd_suggestion.items.clear();
            if cmd.is_ui {
                let _ = self
                    .events
                    .0
                    .try_send(AppEvent::UserSubmit(format!("/{}", cmd.name)));
            }
        }
    }

    fn collect_files(&self) -> Vec<String> {
        use ignore::WalkBuilder;
        let work_path = std::path::Path::new(&self.work_dir);
        let mut files = Vec::new();
        let walker = WalkBuilder::new(work_path)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(|entry| {
                !crate::agent::IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
            })
            .build();
        for entry in walker.flatten() {
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                && let Some(abs) = entry.path().to_str()
            {
                files.push(abs.to_string());
            }
        }
        files.sort();
        files
    }

    pub(super) fn refresh_file_picker(&mut self) {
        let before_cursor = self.input.text_before_cursor();
        if before_cursor.is_empty() {
            self.file_picker.active = false;
            return;
        }

        let at_pos = before_cursor.rfind('@');
        match at_pos {
            None => {
                self.file_picker.active = false;
            }
            Some(at_idx) => {
                let before_at = &before_cursor[..at_idx];
                let is_boundary =
                    before_at.is_empty() || before_at.ends_with(|c: char| c.is_whitespace());
                if !is_boundary {
                    self.file_picker.active = false;
                    return;
                }
                let query = &before_cursor[at_idx + 1..];
                if query.contains(|c: char| c.is_whitespace()) {
                    self.file_picker.active = false;
                    return;
                }
                let files = self.collect_files();
                let q_lower = query.to_lowercase();
                let mut results: Vec<String> = files
                    .into_iter()
                    .filter(|f| {
                        if query.is_empty() {
                            return true;
                        }
                        f.to_lowercase().contains(&q_lower)
                    })
                    .collect();
                if query.is_empty() {
                    results.truncate(20);
                } else {
                    results.truncate(15);
                }
                if results.is_empty() {
                    self.file_picker.active = false;
                } else {
                    self.file_picker.active = true;
                    self.file_picker.results = results;
                    self.file_picker.selected = 0;
                }
            }
        }
    }

    fn select_file(&mut self) {
        if let Some(abs_path) = self
            .file_picker
            .results
            .get(self.file_picker.selected)
            .cloned()
        {
            let rel_path = std::path::Path::new(&abs_path)
                .strip_prefix(&self.work_dir)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| abs_path.clone());
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
