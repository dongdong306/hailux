use color_eyre::Result;
#[cfg(windows)]
use crossterm::event::KeyEventKind;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

use super::{App, EscClearedSnapshot, Message};
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

    fn decide_begin_buffer(
        &mut self,
        now: Instant,
        before: &str,
        retro_chars: usize,
    ) -> (usize, String) {
        let start_byte = retro_start_index(before, retro_chars);
        let grabbed = before[start_byte..].to_string();
        self.begin_with_retro_grabbed(grabbed.clone(), now);
        (start_byte, grabbed)
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
        // Ctrl+V 的 Release 泄漏（WT 把按键交给 paste 动作，Press 被终端消费）：
        // 图片剪贴板场景下 conhost 丢弃空 bracketed paste 序列，这是唯一可达信号。
        // 静默探测——文本粘贴场景（剪贴板有文本）探测无图时不产生提示噪音；
        // 不拦截 Ctrl+V 的终端 Press+Release 成对到达，凭 300ms 窗口去重。
        // 泄漏形态仅存在于 Windows（event.rs 已在非 Windows 拦下 Release），
        // 此处再门控一层，Unix kitty 协议的 Release 到达时直接走常规按键路径。
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
                // Ctrl+V（含 legacy 终端报为 \x16 控制字符、Ctrl+Shift+V 报为
                // 'V' 的形态）：终端不拦截时 Press 直达应用，作为粘贴图片的入口之一
                KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('\x16') => {
                    self.spawn_clipboard_image_probe(true);
                    // 探测发起后记时间戳：Release 泄漏在 Press 处理返回之后
                    // 到达（事件循环串行），300ms 去重窗口据此抑制重复探测
                    self.last_ctrl_v_press = Some(Instant::now());
                    return Ok(());
                }
                _ => {}
            }
        }

        // Alt+V：粘贴剪贴板图片的可靠入口（多数终端不拦截 Alt 组合键；
        // 现代 Windows Terminal 对图片剪贴板的 Ctrl+V 不产生任何事件）
        if key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('\x16'))
        {
            self.spawn_clipboard_image_probe(true);
            return Ok(());
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
                    if self.scroll_offset == 0 {
                        self.should_auto_scroll = true;
                    }
                }
                KeyCode::PageUp => {
                    self.should_auto_scroll = false;
                    self.scroll_offset = self.scroll_offset.saturating_add(20);
                }
                KeyCode::PageDown => {
                    self.scroll_offset = self.scroll_offset.saturating_sub(20);
                    if self.scroll_offset == 0 {
                        self.should_auto_scroll = true;
                    }
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
                    // @图片提及在 expand 前基于 display 文本检测，读文件内联为附件
                    let mut attachments = self.take_mention_images(&input);
                    let input = self.expand_file_mentions(&input);
                    attachments.extend(self.take_pending_images());
                    if !input.trim().is_empty() || !attachments.is_empty() {
                        self.cmd_suggestion.show = false;
                        let _ = self.events.0.try_send(AppEvent::UserSubmit {
                            text: input,
                            attachments,
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
                    // Windows 下 `\e[200~` / `\e[201~` 序列的 Esc 字节也会走到这里：
                    // 记录完整状态快照，若紧接着出现 paste 标记则整体恢复（见 handle_paste）
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
                self.should_auto_scroll = false;
                self.scroll_offset = self.scroll_offset.saturating_add(20);
            }
            KeyCode::PageDown => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
                if self.scroll_offset == 0 {
                    self.should_auto_scroll = true;
                }
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
        // Windows bracketed paste 标记配对（见 BracketPasteState 注释）
        let (clean, saw_start, saw_end) = Self::bracket_marker_info(&text);
        let now = Instant::now();
        // 孤立 start 标记超时失效
        if self
            .bracket_paste
            .at
            .is_some_and(|t| now.duration_since(t) > Duration::from_secs(1))
        {
            self.bracket_paste = Default::default();
        }
        if saw_start || saw_end {
            // `\e[200~` / `\e[201~` 的首字节是 Esc：序列中途的 Esc 已把输入框
            // 连同附件/粘贴/提及误清空 → 短窗内整体恢复（start/end 两个标记
            // 场景都可能触发，end-only 时 pending_start 尚未建立）
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
                // 空 bracketed paste = 剪贴板只有图片（WT Ctrl+V 约定）→ 探测剪贴板
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
            // 资源管理器复制文件 → WT paste 把 FileDrop 转成路径文本注入：
            // 单个图片文件路径转为附件而非插入文本
            if let Some(path) = Self::image_path_from_paste(&clean)
                && let Some(attachment) = read_image_attachment(&path)
            {
                self.attach_image(attachment);
                // 供泄漏探测结果配对去重（微信/QQ 复制图片双通道，见
                // handle_clipboard_image_result）
                self.path_image_attached_at = Some(Instant::now());
                self.render.dirty = true;
            } else {
                self.input.insert_str(&clean);
            }
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

    /// 剥离 Windows 按键流形态的 bracketed paste 标记（`\e[200~` / `\e[201~`
    /// 的 Esc 字节被 conhost 消化后剩余的 `[200~` / `[201~` 字符）。
    /// 返回 (净文本, 是否含 start 标记, 是否含 end 标记)。
    /// 正常粘贴文本不含这两个子串；手打字符不经过 handle_paste（PasteBurst
    /// 仅将快速连续字符判定为粘贴），无误伤路径。
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

    /// 粘贴文本是否为单个图片文件的绝对路径（资源管理器复制文件后 Ctrl+V，
    /// WT 的 paste 把 FileDrop 转成路径文本注入）。仅形态判定，不做 IO；
    /// 多文件列表（引号夹空格）/相对路径/非图片扩展名均不匹配。
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

    // ===== 图片附件 =====

    /// 生成下一个图片占位符基础文本（`[Image N]`）。
    /// 同名占位符已存在时追加 ` #k` 去重（仿大文本粘贴）。
    fn next_image_placeholder(&self) -> String {
        let base = format!("[Image {}]", self.pending_images.len() + 1);
        let duplicates = self
            .pending_images
            .iter()
            .filter(|(ph, _)| ph.contains(&base))
            .count();
        if duplicates == 0 {
            base
        } else {
            format!("{} #{}", base, duplicates + 1)
        }
    }

    /// 把一张图片挂到输入框：插入占位符元素并记录附件。
    /// 占位符字面量会随文本一起提交（占位符进入 user text part）。
    fn attach_image(&mut self, attachment: crate::agent::media::Attachment) {
        // 去重：路径文本转附件与剪贴板探测可能对同一张图双通道命中
        if self
            .pending_images
            .iter()
            .any(|(_, a)| a.data_url == attachment.data_url)
        {
            return;
        }
        let placeholder = self.next_image_placeholder();
        // 尾随空格与其他元素（大文本粘贴/@提及）惯例一致，避免后续输入紧贴 ]
        let element_text = format!("{} ", placeholder);
        self.input.insert_element(&element_text, ElementKind::Image);
        self.pending_images.push((element_text, attachment));
        self.refresh_suggestions();
    }

    /// 发起剪贴板图片探测（异步分离执行）：子进程探测可能耗时 0.5~2s
    /// （超时 10s），不能在事件循环内同步 await，否则每次 Ctrl+V 冻结 UI、
    /// 长按会排队大量探测。结果经 `AppEvent::PasteImageResult` 回传挂载；
    /// in-flight 期间忽略新请求（配合 300ms Release 去重抑制重复探测）。
    pub(super) fn spawn_clipboard_image_probe(&mut self, interactive: bool) {
        if self.image_probe_in_flight || self.is_processing {
            return;
        }
        self.image_probe_in_flight = true;
        if !interactive {
            self.leak_probe_started_at = Some(Instant::now());
        }
        let events = self.events.0.clone();
        tokio::spawn(async move {
            let image = crate::tui::clipboard::read_clipboard_image().await;
            let image = image.filter(|img| img.bytes.len() <= crate::agent::media::MAX_IMAGE_BYTES);
            let _ = events.try_send(AppEvent::PasteImageResult { image, interactive });
        });
    }

    /// 处理剪贴板探测结果：命中挂载图片占位符；交互模式下未命中给轻量提示，
    /// 静默模式（Release 泄漏/空 bracketed paste）未命中无感。
    pub(super) fn handle_clipboard_image_result(
        &mut self,
        image: Option<crate::tui::clipboard::ClipboardImage>,
        interactive: bool,
    ) {
        self.image_probe_in_flight = false;
        let leak_probe_at = self.leak_probe_started_at.take();
        match image {
            Some(img) => {
                // 双通道去重：微信/QQ「复制图片」同时携带位图与 FileDrop，同一
                // 次 Ctrl+V 的路径注入通道（handle_paste 的文件路径识别）已按
                // 原文件字节挂图；探测走 GetImage 拿到的是位图重编码副本，字节
                // 不同、data_url 去重无法识别 → 按配对时间窗丢弃
                if suppress_probe_result(leak_probe_at, self.path_image_attached_at, Instant::now())
                {
                    return;
                }
                let attachment = crate::agent::media::Attachment::from_bytes(&img.mime, &img.bytes);
                self.attach_image(attachment);
                self.render.dirty = true;
            }
            None if interactive => {
                self.messages.push(Message::Agent(
                    "剪贴板中没有可粘贴的图片（支持 PNG/JPEG/GIF/WebP，≤10MB）".to_string(),
                ));
                self.render.dirty = true;
            }
            None => {}
        }
    }

    /// 取走全部待发送图片附件（提交时调用）
    fn take_pending_images(&mut self) -> Vec<crate::agent::media::Attachment> {
        self.pending_images.drain(..).map(|(_, att)| att).collect()
    }

    /// 扫描文本中实际存在的 @提及，把其中的图片文件读为附件（提交时内联）。
    /// @文本被用户删除 → 不附；非图片/超大/
    /// 内容与扩展名不符 → 不附（只发路径，模型 read 会得到相应提示，自洽）。
    fn take_mention_images(&self, text: &str) -> Vec<crate::agent::media::Attachment> {
        let mut images = Vec::new();
        for (display, abs) in &self.file_picker.pending_mentions {
            if !text.contains(display.as_str()) {
                continue;
            }
            let Some(path_str) = abs.strip_prefix('@') else {
                continue;
            };
            let path = std::path::Path::new(path_str);
            if crate::agent::media::mime_from_extension(path).is_none() {
                continue;
            }
            if let Some(att) = read_image_attachment(path) {
                images.push(att);
            }
        }
        images
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
            // set_text 替换整个输入框（元素一并清除）：同步清空 pending 状态，
            // 避免残留附件附着到后续无关消息
            self.pending_images.clear();
            self.pending_pastes.clear();
            self.file_picker.pending_mentions.clear();
            self.cmd_suggestion.show = false;
            self.cmd_suggestion.items.clear();
            if cmd.is_ui {
                let _ = self.events.0.try_send(AppEvent::UserSubmit {
                    text: format!("/{}", cmd.name),
                    attachments: Vec::new(),
                });
            }
        }
    }

    /// 遍历工作目录，返回 (绝对路径, 小写形式) 列表，目录路径带尾部分隔符。
    /// 仅在 picker 会话首次激活时调用一次，后续按键复用缓存。
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
        // 懒构建缓存：picker 会话期间仅首次遍历文件系统，其余按键只做内存过滤
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

    /// 解析光标前输入中的 `@query` 片段；无有效 `@` 触发时返回 None。
    /// 语义：`@` 前必须是行首或空白，query 内不含空白。
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
                // @文件（含图片）一律展示为 @相对路径；
                // 图片内容在提交时由 take_mention_images 内联为附件
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

/// 读取图片文件为附件（≤10MB + 魔数校验：扩展名是图片但内容不是则拒绝）。
/// 供粘贴路径转附件与 @提及内联共用。
fn read_image_attachment(path: &std::path::Path) -> Option<crate::agent::media::Attachment> {
    use crate::agent::media::{self, Attachment};
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() as usize > media::MAX_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let mime = media::sniff_image_mime(&bytes)?;
    Some(Attachment::from_bytes(mime, &bytes))
}

/// 泄漏探测结果是否应被丢弃（与路径注入通道配对去重）：
/// 微信/QQ「复制图片」同时携带位图与 FileDrop，同一次粘贴双通道都会挂图，
/// 两份数据字节不同（位图重编码 vs 原文件），data_url 去重无法识别。
/// 两种时序都视为同一次手势：
/// - 探测先发起（Release 泄漏）、路径后挂图：结果在探测超时窗口（10s+余量）内
/// - 路径先挂图、探测后发起（`\e[200~path\e[201~` 按键流被 Esc 拆片，尾部
///   `[201~` 触发空 paste 探测）：间隔仅在 PasteBurst flush 级别（<500ms）
fn suppress_probe_result(
    leak_probe_at: Option<Instant>,
    path_attached_at: Option<Instant>,
    now: Instant,
) -> bool {
    let (Some(leak), Some(path)) = (leak_probe_at, path_attached_at) else {
        return false;
    };
    if path >= leak {
        now.duration_since(leak) < Duration::from_secs(11)
    } else {
        leak.duration_since(path) < Duration::from_millis(500)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows WT 空 bracketed paste：Esc 被 conhost 消化后，
    /// "[200~" 与 "[201~" 分两次 flush 到达（被序列中的 Esc 拆开）
    #[test]
    fn bracket_markers_start_and_end_fragments() {
        let (clean, start, end) = App::bracket_marker_info("[200~");
        assert_eq!((clean.as_str(), start, end), ("", true, false));
        let (clean, start, end) = App::bracket_marker_info("[201~");
        assert_eq!((clean.as_str(), start, end), ("", false, true));
    }

    /// 整个空序列一次 flush 到达
    #[test]
    fn bracket_markers_empty_paste_single_flush() {
        let (clean, start, end) = App::bracket_marker_info("[200~[201~");
        assert_eq!((clean.as_str(), start, end), ("", true, true));
    }

    /// 文本粘贴：标记夹内容（一次 flush 或拆片）→ 剥标记保留正文
    #[test]
    fn bracket_markers_text_paste_stripped() {
        let (clean, start, end) = App::bracket_marker_info("[200~hello world[201~");
        assert_eq!((clean.as_str(), start, end), ("hello world", true, true));
        let (clean, start, end) = App::bracket_marker_info("[200~multi\nline[201~");
        assert_eq!((clean.as_str(), start, end), ("multi\nline", true, true));
    }

    /// 普通粘贴文本不含标记 → 原样返回
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

    /// 资源管理器复制文件 → WT paste 注入的路径文本（带/不带引号、含空格）
    #[test]
    fn image_path_from_paste_accepts_single_image_file() {
        let p = App::image_path_from_paste(r#"C:\Users\a b\Pictures\shot.png"#);
        assert!(p.is_some());
        let p = App::image_path_from_paste(r#""C:\Users\a b\Pictures\shot.png""#);
        assert!(p.is_some());
    }

    /// 多文件列表 / 相对路径 / 非图片扩展名 / 纯文本 → 不识别
    #[test]
    fn image_path_from_paste_rejects_non_single_image() {
        assert!(App::image_path_from_paste(r#""C:\a.png" "C:\b.png""#).is_none());
        assert!(App::image_path_from_paste(r"pictures\shot.png").is_none());
        assert!(App::image_path_from_paste(r"C:\docs\readme.txt").is_none());
        assert!(App::image_path_from_paste("hello world").is_none());
        assert!(App::image_path_from_paste("").is_none());
    }

    /// read_image_attachment：魔数校验兜底（扩展名是图片但内容不是 → None）
    #[test]
    fn read_image_attachment_sniffs_content() {
        let dir = std::env::temp_dir().join(format!("hailux_paste_img_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake.png");
        std::fs::write(&fake, b"plain text not an image").unwrap();
        assert!(read_image_attachment(&fake).is_none());

        let real = dir.join("real.png");
        let mut bytes = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(b"payload");
        std::fs::write(&real, &bytes).unwrap();
        let att = read_image_attachment(&real).unwrap();
        assert_eq!(att.mime, "image/png");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 双通道去重：探测发起后路径通道挂图（微信/QQ 复制图片同时携带
    /// 位图与 FileDrop）→ 探测结果丢弃
    #[test]
    fn suppress_probe_result_pairs_with_path_attach() {
        let now = Instant::now();
        let leak = now - Duration::from_millis(1500);
        let path = now - Duration::from_millis(1400); // 路径挂图晚于探测发起
        assert!(suppress_probe_result(Some(leak), Some(path), now));
    }

    /// 反向时序：路径挂图在先、探测后发起（`\e[200~path\e[201~` 被 Esc
    /// 拆片，尾部空 paste 触发探测）→ 同样抑制
    #[test]
    fn suppress_probe_result_pairs_with_probe_after_path() {
        let now = Instant::now();
        let path = now - Duration::from_millis(300);
        let probe = now - Duration::from_millis(250); // 探测发起晚于路径挂图
        assert!(suppress_probe_result(Some(probe), Some(path), now));
    }

    /// 路径挂图早于本次探测（两次独立手势）→ 不抑制
    #[test]
    fn suppress_probe_result_ignores_older_path_attach() {
        let now = Instant::now();
        let path = now - Duration::from_millis(2000);
        let leak = now - Duration::from_millis(1500); // 新探测晚于上次路径挂图
        assert!(!suppress_probe_result(Some(leak), Some(path), now));
    }

    /// 探测已超出超时窗口（11s）→ 不抑制（视为独立手势）
    #[test]
    fn suppress_probe_result_expires_after_window() {
        let now = Instant::now();
        let leak = now - Duration::from_secs(12);
        let path = now - Duration::from_secs(11);
        assert!(!suppress_probe_result(Some(leak), Some(path), now));
    }

    /// 缺少任一时间戳 → 不抑制
    #[test]
    fn suppress_probe_result_requires_both_timestamps() {
        let now = Instant::now();
        let t = now - Duration::from_millis(100);
        assert!(!suppress_probe_result(None, Some(t), now));
        assert!(!suppress_probe_result(Some(t), None, now));
        assert!(!suppress_probe_result(None, None, now));
    }
}
