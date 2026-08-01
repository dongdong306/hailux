use std::ops::Range;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ElementKind {
    Paste,
    FileMention,
}

#[derive(Debug, Clone)]
struct TextElement {
    range: Range<usize>,
    kind: ElementKind,
}

pub(crate) struct InputHandler {
    text: String,
    cursor: usize,
    elements: Vec<TextElement>,
    input_history: Vec<String>,
    history_index: usize,
    input_draft: Option<String>,
    input_scroll_row: u16,
    last_area_width: u16,
    is_processing: bool,
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            elements: Vec::new(),
            input_history: Vec::new(),
            history_index: 0,
            input_draft: None,
            input_scroll_row: 0,
            last_area_width: 80,
            is_processing: false,
        }
    }

    // ===== Basic text accessors =====

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn text_before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.elements.clear();
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.elements.clear();
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor = self.clamp_to_boundary(pos.min(self.text.len()));
    }

    // ===== Text editing =====

    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let at = self.clamp_to_boundary(self.cursor);
        self.text.insert_str(at, s);
        self.shift_elements(at, 0, s.len());
        self.cursor = at + s.len();
    }

    pub fn insert_element(&mut self, payload: &str, kind: ElementKind) {
        if payload.is_empty() {
            return;
        }
        let at = self.clamp_to_boundary(self.cursor);
        let end = at + payload.len();
        self.text.insert_str(at, payload);
        self.shift_elements(at, 0, payload.len());
        self.elements.push(TextElement {
            range: at..end,
            kind,
        });
        self.elements.sort_by_key(|e| e.range.start);
        self.cursor = end;
    }

    pub fn delete_backward(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let target = self.prev_atomic_boundary(self.cursor);
        if target < self.cursor {
            self.replace_range(target..self.cursor, "");
            return true;
        }
        false
    }

    pub fn delete_forward(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }
        let target = self.next_atomic_boundary(self.cursor);
        if target > self.cursor {
            self.replace_range(self.cursor..target, "");
            return true;
        }
        false
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_atomic_boundary(self.cursor);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.next_atomic_boundary(self.cursor);
        }
    }

    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn drain_raw(&mut self, range: Range<usize>) {
        if range.start >= range.end || range.end > self.text.len() {
            return;
        }
        let removed = range.end - range.start;
        self.text.drain(range.clone());
        self.shift_elements(range.start, removed, 0);
        self.cursor = range.start;
    }

    // ===== Element accessors =====

    pub fn element_info(&self) -> Vec<(Range<usize>, ElementKind)> {
        self.elements
            .iter()
            .map(|e| (e.range.clone(), e.kind))
            .collect()
    }

    pub fn element_payloads(&self) -> Vec<String> {
        self.elements
            .iter()
            .filter_map(|e| self.text.get(e.range.clone()).map(str::to_string))
            .collect()
    }

    pub fn removed_elements(&self, before: &[String]) -> Vec<String> {
        let current: Vec<&str> = self
            .elements
            .iter()
            .filter_map(|e| self.text.get(e.range.clone()))
            .collect();
        before
            .iter()
            .filter(|p| !current.contains(&p.as_str()))
            .cloned()
            .collect()
    }

    // ===== State management =====

    pub fn set_processing(&mut self, processing: bool) {
        self.is_processing = processing;
    }

    pub fn update_area_width(&mut self, width: u16) {
        self.last_area_width = width;
    }

    pub fn input_scroll_row(&self) -> u16 {
        self.input_scroll_row
    }

    pub fn set_input_scroll_row(&mut self, row: u16) {
        self.input_scroll_row = row;
    }

    /// Push current input to history and reset state (called on submit).
    pub fn submit(&mut self, input: &str) {
        self.input_history.push(input.to_string());
        self.history_index = self.input_history.len();
        self.input_draft = None;
        self.clear();
    }

    /// Reset all input state (for session switch / new session).
    pub fn reset(&mut self) {
        self.clear();
        self.input_scroll_row = 0;
        self.input_history.clear();
        self.history_index = 0;
        self.input_draft = None;
    }

    // ===== History navigation =====

    pub fn history_prev(&mut self) {
        if self.history_index == self.input_history.len() && !self.is_empty() {
            self.input_draft = Some(self.text.clone());
        }
        if self.history_index > 0 {
            self.history_index -= 1;
            self.set_text(self.input_history[self.history_index].clone());
        }
    }

    pub fn history_next(&mut self) {
        if self.history_index < self.input_history.len() {
            self.history_index += 1;
            if self.history_index < self.input_history.len() {
                self.set_text(self.input_history[self.history_index].clone());
            } else {
                let draft = self.input_draft.take().unwrap_or_default();
                self.set_text(draft);
            }
        }
    }

    // ===== Visual row cursor movement =====

    pub fn cursor_on_first_visual_row(&self) -> bool {
        let before_cursor = &self.text[..self.cursor];
        if before_cursor.contains('\n') {
            return false;
        }
        let (vrow, _) =
            Self::visual_row_col_in_line(before_cursor, self.cursor, self.text_avail_width());
        vrow == 0
    }

    pub fn cursor_on_last_visual_row(&self) -> bool {
        let after_cursor = &self.text[self.cursor..];
        if after_cursor.contains('\n') {
            return false;
        }
        let total_vrows = Self::visual_rows_of_line(&self.text, self.text_avail_width());
        let before_cursor = &self.text[..self.cursor];
        let (vrow, _) =
            Self::visual_row_col_in_line(before_cursor, self.cursor, self.text_avail_width());
        vrow >= total_vrows.saturating_sub(1)
    }

    pub fn move_cursor_up(&mut self) {
        let text_avail = self.text_avail_width();
        let before_cursor = &self.text[..self.cursor];
        let current_line_start = before_cursor.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col_in_line = self.cursor - current_line_start;
        let current_line = &self.text[current_line_start..self.cursor];
        let (vrow, vcol) = Self::visual_row_col_in_line(current_line, col_in_line, text_avail);

        if vrow > 0 {
            let target_visual_row = vrow - 1;
            let new_pos =
                Self::byte_pos_for_visual_row(current_line, target_visual_row, vcol, text_avail);
            self.set_cursor(current_line_start + new_pos);
        } else if current_line_start > 0 {
            let prev_line_end = current_line_start - 1;
            let prev_line_start = self.text[..prev_line_end]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            let prev_line = &self.text[prev_line_start..prev_line_end];
            let prev_vrows = Self::visual_rows_of_line(prev_line, text_avail);
            let target_visual_row = prev_vrows.saturating_sub(1);
            let new_pos =
                Self::byte_pos_for_visual_row(prev_line, target_visual_row, vcol, text_avail);
            self.set_cursor(prev_line_start + new_pos);
        }
    }

    pub fn move_cursor_down(&mut self) {
        let text_avail = self.text_avail_width();
        let before_cursor = &self.text[..self.cursor];
        let current_line_start = before_cursor.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let after_cursor = &self.text[self.cursor..];
        let next_nl = after_cursor.find('\n');
        let line_end = self.cursor + next_nl.unwrap_or(after_cursor.len());
        let current_line = &self.text[current_line_start..line_end];
        let col_in_line = self.cursor - current_line_start;
        let (vrow, vcol) = Self::visual_row_col_in_line(current_line, col_in_line, text_avail);
        let total_vrows = Self::visual_rows_of_line(current_line, text_avail);

        if vrow + 1 < total_vrows {
            let target_visual_row = vrow + 1;
            let new_pos =
                Self::byte_pos_for_visual_row(current_line, target_visual_row, vcol, text_avail);
            self.set_cursor(current_line_start + new_pos);
        } else if line_end < self.text.len() {
            let next_line_start = line_end + 1;
            let next_after = &self.text[next_line_start..];
            let next_end = next_line_start + next_after.find('\n').unwrap_or(next_after.len());
            let next_line = &self.text[next_line_start..next_end];
            let new_pos = Self::byte_pos_for_visual_row(next_line, 0, vcol, text_avail);
            self.set_cursor(next_line_start + new_pos);
        }
    }

    // ===== Visual info for rendering =====

    pub fn compute_visual_info(&self) -> (u16, u16, u16) {
        let prefix_text = if self.is_processing { "⠋ " } else { "> " };
        let prefix_w: u16 = prefix_text.width() as u16;
        let before_cursor = &self.text[..self.cursor.min(self.text.len())];

        let logical_lines: Vec<&str> = self.text.split('\n').collect();
        let before_lines: Vec<&str> = before_cursor.split('\n').collect();
        let cursor_line_idx = before_lines.len() - 1;

        let mut total_rows: u16 = 0;
        let mut cursor_vrow: u16 = 0;
        let mut cursor_vcol: u16 = 0;

        let text_avail = self.text_avail_width();

        for (i, line) in logical_lines.iter().enumerate() {
            let mut row_count: u16 = 1;
            let mut width_acc: u16 = 0;

            for ch in line.chars() {
                let char_w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                if width_acc + char_w > text_avail && width_acc > 0 {
                    row_count += 1;
                    width_acc = 0;
                }
                width_acc += char_w;
            }

            if i == cursor_line_idx {
                let cursor_text = before_lines[i];
                let mut cv_row: u16 = 0;
                let mut cv_width: u16 = 0;

                for ch in cursor_text.chars() {
                    let char_w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                    if cv_width + char_w > text_avail && cv_width > 0 {
                        cv_row += 1;
                        cv_width = 0;
                    }
                    cv_width += char_w;
                }

                cursor_vrow = total_rows + cv_row;
                cursor_vcol = prefix_w + cv_width;
            }

            total_rows += row_count;
        }

        (total_rows, cursor_vrow, cursor_vcol)
    }

    // ===== Internal helpers =====

    fn text_avail_width(&self) -> u16 {
        let prefix = if self.is_processing { "⠋ " } else { "> " };
        let prefix_w = prefix.width() as u16;
        self.last_area_width.saturating_sub(prefix_w).max(1)
    }

    fn visual_row_col_in_line(line: &str, byte_pos: usize, text_avail: u16) -> (u16, u16) {
        let pos = byte_pos.min(line.len());
        let mut vrow: u16 = 0;
        let mut vcol: u16 = 0;
        for ch in line[..pos].chars() {
            let char_w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
            if vcol + char_w > text_avail && vcol > 0 {
                vrow += 1;
                vcol = 0;
            }
            vcol += char_w;
        }
        (vrow, vcol)
    }

    fn visual_rows_of_line(line: &str, text_avail: u16) -> u16 {
        let (vrow, vcol) = Self::visual_row_col_in_line(line, line.len(), text_avail);
        if vcol == 0 && vrow > 0 {
            vrow
        } else {
            vrow + 1
        }
    }

    fn byte_pos_for_visual_row(
        line: &str,
        target_vrow: u16,
        preferred_col: u16,
        text_avail: u16,
    ) -> usize {
        let mut vrow: u16 = 0;
        let mut vcol: u16 = 0;
        let mut last_pos_at_row_start: usize = 0;

        for (byte_idx, ch) in line.char_indices() {
            if vrow == target_vrow && vcol >= preferred_col {
                return byte_idx;
            }
            let char_w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
            if vcol + char_w > text_avail && vcol > 0 {
                vrow += 1;
                vcol = 0;
                last_pos_at_row_start = byte_idx;
                if vrow == target_vrow && vcol >= preferred_col {
                    return byte_idx;
                }
            }
            vcol += char_w;
        }
        if vrow == target_vrow {
            line.len()
        } else {
            last_pos_at_row_start
        }
    }

    // ===== TextElement internals =====

    fn find_element_containing(&self, pos: usize) -> Option<usize> {
        self.elements
            .iter()
            .position(|e| pos > e.range.start && pos < e.range.end)
    }

    fn find_element_at_start(&self, pos: usize) -> Option<usize> {
        self.elements.iter().position(|e| pos == e.range.start)
    }

    fn find_element_at_end(&self, pos: usize) -> Option<usize> {
        self.elements.iter().position(|e| pos == e.range.end)
    }

    fn clamp_to_boundary(&self, pos: usize) -> usize {
        let pos = pos.min(self.text.len());
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx].range;
            let dist_start = pos - e.start;
            let dist_end = e.end - pos;
            if dist_start <= dist_end {
                e.start
            } else {
                e.end
            }
        } else {
            pos
        }
    }

    fn prev_atomic_boundary(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        if let Some(idx) = self.find_element_at_end(pos) {
            return self.elements[idx].range.start;
        }
        if let Some(idx) = self.find_element_containing(pos) {
            return self.elements[idx].range.start;
        }
        if let Some((byte_idx, _)) = self.text[..pos].char_indices().next_back() {
            if let Some(idx) = self.find_element_containing(byte_idx) {
                return self.elements[idx].range.start;
            }
            byte_idx
        } else {
            0
        }
    }

    fn next_atomic_boundary(&self, pos: usize) -> usize {
        if pos >= self.text.len() {
            return self.text.len();
        }
        if let Some(idx) = self.find_element_at_start(pos) {
            return self.elements[idx].range.end;
        }
        if let Some(idx) = self.find_element_containing(pos) {
            return self.elements[idx].range.end;
        }
        if let Some((offset, _)) = self.text[pos..].char_indices().nth(1) {
            let target = pos + offset;
            if let Some(idx) = self.find_element_at_start(target) {
                return self.elements[idx].range.end;
            }
            target
        } else {
            self.text.len()
        }
    }

    fn replace_range(&mut self, range: Range<usize>, replacement: &str) {
        let range = self.expand_to_element_boundaries(range);
        let removed = range.end - range.start;
        let inserted = replacement.len();
        self.text.replace_range(range.clone(), replacement);
        self.shift_elements(range.start, removed, inserted);
        self.cursor = range.start + inserted;
    }

    fn expand_to_element_boundaries(&self, mut range: Range<usize>) -> Range<usize> {
        loop {
            let mut changed = false;
            for e in &self.elements {
                if e.range.start < range.end && e.range.end > range.start {
                    let new_start = range.start.min(e.range.start);
                    let new_end = range.end.max(e.range.end);
                    if new_start != range.start || new_end != range.end {
                        range.start = new_start;
                        range.end = new_end;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        range
    }

    fn shift_elements(&mut self, at: usize, removed: usize, inserted: usize) {
        let end = at + removed;
        let diff = inserted as isize - removed as isize;
        self.elements
            .retain(|e| !(e.range.start >= at && e.range.end <= end));
        for e in &mut self.elements {
            if e.range.end <= at {
            } else if e.range.start >= end {
                e.range.start = e.range.start.saturating_add_signed(diff);
                e.range.end = e.range.end.saturating_add_signed(diff);
            }
        }
    }
}
