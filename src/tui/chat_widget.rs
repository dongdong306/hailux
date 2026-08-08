use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::types::Message;
use super::command;
use super::history_cell::{
    CHAT_FG, CHAT_FILE_MENTION, CHAT_PASTE, CHAT_PLACEHOLDER, HistoryCell, PLAN_BADGE,
};
use super::input::ElementKind;

pub struct RenderCache {
    entries: HashMap<u64, Vec<Line<'static>>>,
    last_width: u16,
}

impl RenderCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            last_width: 0,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.last_width = 0;
    }

    fn touch_width(&mut self, width: u16) {
        if width != self.last_width {
            self.entries.clear();
            self.last_width = width;
        }
    }

    /// 确保 key 对应的 lines 已缓存，返回行数（不 clone）
    fn ensure_cached(
        &mut self,
        key: u64,
        width: u16,
        compute: impl FnOnce() -> Vec<Line<'static>>,
    ) -> usize {
        self.touch_width(width);
        self.entries.entry(key).or_insert_with(compute).len()
    }

    /// 按 key 查已缓存的 lines 引用
    fn get_lines(&self, key: u64) -> Option<&Vec<Line<'static>>> {
        self.entries.get(&key)
    }

    /// 仅保留 active_keys 中存在的条目，淘汰其余陈旧缓存
    pub fn retain_keys(&mut self, active_keys: &HashSet<u64>) {
        self.entries.retain(|k, _| active_keys.contains(k));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

pub struct RenderResult {
    pub max_hide: u16,
}

pub struct ChatWidget<'a> {
    pub messages: &'a [Message],
    pub cells: &'a [Box<dyn HistoryCell>],
    pub input_buffer: &'a str,
    pub input_elements: Vec<(std::ops::Range<usize>, ElementKind)>,
    pub scroll_offset: u16,
    pub is_processing: bool,
    pub model_name: &'a str,
    pub input_scroll_row: u16,
    pub input_area_height: u16,
    pub directory: &'a str,
    pub plan_mode: bool,
    pub yolo_mode: bool,
    pub show_suggestions: bool,
    pub command_suggestions: &'a [command::CommandEntry],
    pub selected_suggestion: usize,
    pub esc_hint_active: bool,
    pub context_tokens: u32,
    pub max_context_tokens: u32,
    pub spinner_frame: usize,
    pub show_file_picker: bool,
    pub file_picker_results: &'a [String],
    pub file_picker_selected: usize,
    pub user_msg_sent_at: Option<Instant>,
    pub render_cache: &'a mut RenderCache,
}

const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn shimmer_spans(text: &str) -> Vec<Span<'static>> {
    use std::sync::OnceLock;
    static PROCESS_START: OnceLock<Instant> = OnceLock::new();
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let start = PROCESS_START.get_or_init(Instant::now);
    let padding = 10usize;
    let period = chars.len() + padding * 2;
    let sweep_seconds = 2.5f32;
    let pos_f = (start.elapsed().as_secs_f32() % sweep_seconds) / sweep_seconds * (period as f32);
    let pos = pos_f as usize;
    let band_half_width = 6.0f32;

    let base_color = (100, 100, 100);
    let highlight_color = (255, 255, 255);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(chars.len());
    let mut chunk = String::with_capacity(chars.len());
    let mut chunk_style: Option<Style> = None;

    for (i, ch) in chars.iter().enumerate() {
        let i_pos = i as isize + padding as isize;
        let dist = (i_pos - pos as isize).abs() as f32;
        let t = if dist <= band_half_width {
            let x = std::f32::consts::PI * (dist / band_half_width);
            0.5 * (1.0 + x.cos())
        } else {
            0.0
        };
        let highlight = t.clamp(0.0, 1.0);
        let r = (base_color.0 as f32 + (highlight_color.0 - base_color.0) as f32 * highlight) as u8;
        let g = (base_color.1 as f32 + (highlight_color.1 - base_color.1) as f32 * highlight) as u8;
        let b = (base_color.2 as f32 + (highlight_color.2 - base_color.2) as f32 * highlight) as u8;
        let style = if highlight > 0.3 {
            Style::default()
                .fg(Color::Rgb(r, g, b))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(r, g, b))
        };

        if chunk_style == Some(style) {
            chunk.push(*ch);
        } else {
            if !chunk.is_empty() {
                spans.push(Span::styled(chunk, chunk_style.unwrap()));
                chunk = String::with_capacity(chars.len());
            }
            chunk.push(*ch);
            chunk_style = Some(style);
        }
    }
    if !chunk.is_empty() {
        spans.push(Span::styled(chunk, chunk_style.unwrap()));
    }
    spans
}

impl<'a> ChatWidget<'a> {
    pub fn render(mut self, area: Rect, buf: &mut Buffer) -> RenderResult {
        let [messages_area, timing_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(self.input_area_height),
            Constraint::Length(1),
        ])
        .areas(area);

        let indent = 1u16;
        let messages_area = Rect::new(
            messages_area.x + indent,
            messages_area.y,
            messages_area.width.saturating_sub(indent),
            messages_area.height,
        );
        let timing_area = Rect::new(
            timing_area.x + indent,
            timing_area.y,
            timing_area.width.saturating_sub(indent),
            timing_area.height,
        );

        let max_hide = self.render_messages(messages_area, buf, indent);
        self.render_timing(timing_area, buf);
        self.render_input(input_area, buf);
        self.render_status(status_area, buf);

        if self.show_suggestions && !self.command_suggestions.is_empty() {
            self.render_suggestions(area, input_area, buf);
        }

        if self.show_file_picker && !self.file_picker_results.is_empty() {
            self.render_file_picker(area, input_area, buf);
        }

        RenderResult { max_hide }
    }

    fn render_messages(&mut self, messages_area: Rect, buf: &mut Buffer, indent: u16) -> u16 {
        let scrollbar_width: u16 = 2;
        let text_width = messages_area.width.saturating_sub(scrollbar_width);
        let visible_height = messages_area.height;

        let cells = self.cells;

        // Phase 1: ensure all cells' lines are cached, collect (key, line_count)
        let segments: Vec<(u64, usize)> = (0..cells.len())
            .map(|i| {
                let key = cells[i].cache_key();
                let count = self
                    .render_cache
                    .ensure_cached(key, text_width, || cells[i].display_lines(text_width));
                (key, count)
            })
            .collect();

        // total = sum of (cell lines + 1 separator) per cell
        let total_lines: usize = segments.iter().map(|&(_, n)| n + 1).sum();
        let total_lines_u16 = total_lines as u16;
        let max_hide = total_lines_u16.saturating_sub(visible_height);
        let hide_from_bottom = self.scroll_offset.min(max_hide);
        let ratatui_scroll = max_hide.saturating_sub(hide_from_bottom);

        // Phase 2: collect only lines within the visible window
        let win_start = ratatui_scroll as usize;
        let win_end = (win_start + visible_height as usize).min(total_lines);
        let mut all_lines: Vec<Line<'static>> =
            Vec::with_capacity(win_end.saturating_sub(win_start));

        let header_total = segments.first().map(|&(_, n)| n + 1).unwrap_or(0);

        let mut pos = 0usize;
        for &(key, cell_count) in &segments {
            // cell lines
            let cell_end = pos + cell_count;
            if cell_end > win_start
                && pos < win_end
                && let Some(cached) = self.render_cache.get_lines(key)
            {
                let s = win_start.saturating_sub(pos);
                let e = win_end.min(cell_end) - pos;
                all_lines.extend(cached[s..e].iter().cloned());
            }
            // separator
            let sep_pos = cell_end;
            if sep_pos >= win_start && sep_pos < win_end {
                all_lines.push(Line::from(""));
            }
            pos = cell_end + 1;
        }

        // Lines belonging to the header cell (no indent); rest are indented
        let header_visible = if win_start < header_total {
            header_total.min(win_end) - win_start
        } else {
            0
        }
        .min(all_lines.len());

        let text_area = Rect::new(messages_area.x, messages_area.y, text_width, visible_height);

        for line in all_lines.iter_mut() {
            if let Some(bg) = line.spans.iter().find_map(|span| match span.style.bg {
                Some(color) if color != Color::Reset => Some(color),
                _ => None,
            }) {
                line.style = line.style.bg(bg);
            }
        }

        let messages_paragraph = Paragraph::new(all_lines.clone());
        messages_paragraph.render(text_area, buf);

        // Render header lines without indent (align with input box border)
        if header_visible > 0 {
            let header_area = Rect::new(
                text_area.x.saturating_sub(indent),
                text_area.y,
                text_area.width + indent,
                text_area.height,
            );
            let header_lines = &all_lines[..header_visible.min(all_lines.len())];
            if !header_lines.is_empty() {
                Clear.render(
                    Rect::new(
                        header_area.x,
                        header_area.y,
                        header_area.width,
                        header_lines.len() as u16,
                    ),
                    buf,
                );
                Paragraph::new(header_lines.to_vec()).render(header_area, buf);
            }
        }

        // 将 diff 行的背景色延伸到行尾，避免 ratatui 宽字符 trailing cell
        // diff 不更新背景色（PR#2308）导致滚动残留
        for (row, line) in all_lines.iter().enumerate() {
            if row as u16 >= text_area.height {
                break;
            }
            if let Some(bg) = line.style.bg.filter(|&c| c != Color::Reset) {
                let y = text_area.y + row as u16;
                for x in text_area.left()..text_area.right() {
                    buf[(x, y)].set_bg(bg);
                }
            }
        }

        if total_lines_u16 > visible_height {
            let scrollbar_area = Rect::new(
                messages_area.x + text_width,
                messages_area.y,
                scrollbar_width,
                visible_height,
            );
            Clear.render(scrollbar_area, buf);

            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .thumb_symbol("█")
                .style(Style::default().fg(Color::DarkGray))
                .thumb_style(Style::default().fg(Color::White));

            let scroll_position = if max_hide > 0 {
                let ratio = ratatui_scroll as f64 / max_hide as f64;
                ((total_lines_u16 - 1) as f64 * ratio).round() as usize
            } else {
                0
            };

            let mut scrollbar_state = ScrollbarState::new(total_lines)
                .viewport_content_length(visible_height as usize)
                .position(scroll_position);

            scrollbar.render(scrollbar_area, buf, &mut scrollbar_state);
        } else {
            let scrollbar_area = Rect::new(
                messages_area.x + text_width,
                messages_area.y,
                scrollbar_width,
                visible_height,
            );
            Clear.render(scrollbar_area, buf);
        }

        max_hide
    }

    fn render_input(&self, input_area: Rect, buf: &mut Buffer) {
        let border_color = if self.is_processing {
            Color::DarkGray
        } else if self.plan_mode {
            PLAN_BADGE
        } else {
            Color::Rgb(100, 100, 100)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color));
        let input_render_area = block.inner(input_area);
        block.render(input_area, buf);

        let prefix_text = "> ".to_string();
        let prefix_w: u16 = prefix_text.width() as u16;
        let input_prefix = Span::styled(
            prefix_text,
            if self.is_processing {
                Style::default().fg(Color::DarkGray)
            } else if self.plan_mode {
                Style::default().fg(PLAN_BADGE)
            } else {
                Style::default().fg(Color::Rgb(120, 120, 120))
            },
        );
        let area_w = input_render_area.width;

        let text_style = if self.is_processing {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(CHAT_FG)
        };
        let element_style = if self.is_processing {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(CHAT_PASTE)
        };
        let file_mention_style = if self.is_processing {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(CHAT_FILE_MENTION)
        };

        if self.input_buffer.is_empty() && !self.is_processing {
            let input_paragraph = Paragraph::new(Line::from(vec![
                input_prefix,
                Span::styled(
                    "输入消息... (Shift+Enter 换行)",
                    Style::default().fg(CHAT_PLACEHOLDER),
                ),
            ]));
            input_paragraph.render(input_render_area, buf);
            return;
        }

        let mut visual_lines: Vec<Line<'static>> = Vec::new();
        let logical_lines: Vec<&str> = self.input_buffer.split('\n').collect();
        let text_avail = area_w.saturating_sub(prefix_w).max(1);

        let mut global_byte: usize = 0;

        for (line_idx, logical_line) in logical_lines.iter().enumerate() {
            let line_byte_offset = global_byte;

            if logical_line.is_empty() {
                global_byte += if line_idx < logical_lines.len() - 1 {
                    1
                } else {
                    0
                };
                if line_idx == 0 {
                    visual_lines.push(Line::from(vec![input_prefix.clone()]));
                } else {
                    visual_lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(String::new(), text_style),
                    ]));
                }
                continue;
            }

            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut current_chunk = String::new();
            let mut current_width = 0u16;
            let mut current_style = text_style;
            let mut is_first_chunk = line_idx == 0;

            for ch in logical_line.chars() {
                let in_element = self.input_elements.iter().find_map(|(r, kind)| {
                    let local_start = r.start.saturating_sub(line_byte_offset);
                    let local_end = r.end.saturating_sub(line_byte_offset);
                    let ch_local_start = global_byte - line_byte_offset;
                    let ch_local_end = ch_local_start + ch.len_utf8();
                    if ch_local_start >= local_start && ch_local_end <= local_end {
                        Some(*kind)
                    } else {
                        None
                    }
                });
                let ch_style = match in_element {
                    Some(ElementKind::Paste) => element_style,
                    Some(ElementKind::FileMention) => file_mention_style,
                    None => text_style,
                };

                if !current_chunk.is_empty() && ch_style != current_style {
                    spans.push(Span::styled(current_chunk.clone(), current_style));
                    current_chunk.clear();
                }
                current_style = ch_style;

                let char_w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                if current_width + char_w > text_avail && !current_chunk.is_empty() {
                    if !current_chunk.is_empty() {
                        spans.push(Span::styled(current_chunk.clone(), current_style));
                        current_chunk.clear();
                    }
                    let mut line_spans = if is_first_chunk {
                        vec![input_prefix.clone()]
                    } else {
                        vec![Span::raw("  ")]
                    };
                    line_spans.append(&mut spans);
                    visual_lines.push(Line::from(line_spans));
                    is_first_chunk = false;
                    current_width = 0;
                }

                current_chunk.push(ch);
                current_width += char_w;
                global_byte += ch.len_utf8();
            }

            if !current_chunk.is_empty() {
                spans.push(Span::styled(current_chunk, current_style));
            }
            if !spans.is_empty() {
                let mut line_spans = if is_first_chunk {
                    vec![input_prefix.clone()]
                } else {
                    vec![Span::raw("  ")]
                };
                line_spans.append(&mut spans);
                visual_lines.push(Line::from(line_spans));
            }

            if line_idx < logical_lines.len() - 1 {
                global_byte += 1; // \n
            }
        }

        let input_paragraph = Paragraph::new(visual_lines).scroll((self.input_scroll_row, 0));
        input_paragraph.render(input_render_area, buf);
    }

    /// 渲染状态横幅（输入框上方）
    /// 处理中: spinner + shimmer("Thinking"/"Working") + 耗时 + Esc 提示
    /// 完成后: 不显示（耗时渲染在最后一条消息后）
    fn render_timing(&self, area: Rect, buf: &mut Buffer) {
        if !self.is_processing {
            return;
        }
        let Some(sent_at) = self.user_msg_sent_at else {
            return;
        };
        let elapsed = sent_at.elapsed().as_secs_f64();
        let ch = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];

        let header = match self.messages.last() {
            Some(Message::AgentThinking { think_ms: None, .. }) => "Thinking",
            Some(Message::CompactStreaming(_)) => "Compacting context",
            _ => "Working",
        };

        let esc_hint = if self.esc_hint_active {
            "再按一次 Esc 中断"
        } else {
            "按 Esc 中断"
        };

        let mut spans = Vec::new();
        spans.push(Span::styled(
            format!("{} ", ch),
            Style::default().fg(Color::DarkGray),
        ));
        spans.extend(shimmer_spans(header));
        let elapsed_text = format!("  {:.1}s · {}", elapsed, esc_hint);
        let esc_style = Style::default().fg(Color::DarkGray);
        spans.push(Span::styled(elapsed_text, esc_style));
        let paragraph = Paragraph::new(Line::from(spans));
        paragraph.render(area, buf);
    }

    fn render_status(&self, status_area: Rect, buf: &mut Buffer) {
        let gray = Style::default().fg(Color::Rgb(160, 160, 160));

        let ratio = if self.max_context_tokens > 0 {
            (self.context_tokens as f64 / self.max_context_tokens as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };

        const BAR_WIDTH: usize = 10;
        let bar_spans = build_progress_bar(ratio, BAR_WIDTH);

        let ctx_text = format!(
            " {}/{} ({:.0}%)",
            format_tokens(self.context_tokens as i64),
            format_tokens(self.max_context_tokens as i64),
            ratio * 100.0,
        );

        let mut left_spans = Vec::new();

        if self.plan_mode {
            left_spans.push(Span::styled(
                " PLAN ",
                Style::default().fg(PLAN_BADGE).add_modifier(Modifier::BOLD),
            ));
        }

        if self.yolo_mode {
            left_spans.push(Span::styled(
                " YOLO ",
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        left_spans.push(Span::styled(format!(" {} ", self.directory), gray));
        left_spans.push(Span::styled("· ", gray));
        left_spans.push(Span::styled(format!("{} ", self.model_name), gray));

        if self.scroll_offset > 0 {
            left_spans.push(Span::styled(format!("↑{} ", self.scroll_offset), gray));
        }

        let mut right_spans = vec![Span::styled(" ", gray)];
        right_spans.extend(bar_spans);
        right_spans.push(Span::styled(" ", gray));
        right_spans.push(Span::styled(ctx_text, gray));

        let left_line = Line::from(left_spans);
        let right_line = Line::from(right_spans);
        let left_width = left_line.width() as u16;
        let right_width = right_line.width() as u16;

        left_line.render(Rect::new(status_area.x, status_area.y, left_width, 1), buf);

        let right_x = status_area.x + status_area.width.saturating_sub(right_width);
        right_line.render(Rect::new(right_x, status_area.y, right_width, 1), buf);
    }

    fn render_suggestions(&self, full_area: Rect, input_area: Rect, buf: &mut Buffer) {
        let suggestion_count = self.command_suggestions.len() as u16;
        let max_name_width = self
            .command_suggestions
            .iter()
            .map(|cmd| cmd.name.width())
            .max()
            .unwrap_or(0);
        // 列宽："/" + name + "  " + description + " " + 边框
        let content_width = 1
            + max_name_width
            + 2
            + self
                .command_suggestions
                .iter()
                .map(|cmd| cmd.description.width())
                .max()
                .unwrap_or(0);
        let popup_width = (content_width as u16 + 5).min(full_area.width);
        let popup_height = suggestion_count + 2; // +2 为上下边框

        let popup_x = full_area.x;
        let popup_y = input_area.y.saturating_sub(popup_height);

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        Clear.render(popup_area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .border_type(BorderType::Rounded);

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let lines: Vec<Line> = self
            .command_suggestions
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let is_selected = i == self.selected_suggestion;
                let style = if is_selected {
                    Style::default().fg(Color::Black).bg(Color::White)
                } else {
                    Style::default().fg(Color::White)
                };
                let name_style = if is_selected {
                    Style::default().fg(Color::Black).bg(Color::White).bold()
                } else {
                    Style::default().fg(Color::Cyan)
                };
                let mut cmd_text = format!("/{}", cmd.name);
                let pad = max_name_width.saturating_sub(cmd.name.width());
                if pad > 0 {
                    cmd_text.push_str(&" ".repeat(pad));
                }
                let desc_text = format!("  {}", cmd.description);
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(cmd_text, name_style),
                    Span::styled(desc_text, style),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines);
        paragraph.render(inner, buf);
    }

    fn render_file_picker(&self, full_area: Rect, input_area: Rect, buf: &mut Buffer) {
        let count = self.file_picker_results.len() as u16;
        let display_paths: Vec<String> = self
            .file_picker_results
            .iter()
            .map(|abs| {
                std::path::Path::new(abs)
                    .strip_prefix(self.directory)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| abs.clone())
            })
            .collect();
        let max_path_width = display_paths.iter().map(|p| p.width()).max().unwrap_or(0);
        let content_width = max_path_width + 2; // prefix "@ " + path
        let popup_width = (content_width as u16 + 4).min(full_area.width);
        let popup_height = count + 2;

        let popup_x = full_area.x;
        let popup_y = input_area.y.saturating_sub(popup_height);

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        Clear.render(popup_area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .border_type(BorderType::Rounded);

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let lines: Vec<Line> = display_paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let is_selected = i == self.file_picker_selected;
                let path_style = if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                let at_style = if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Yellow).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(vec![
                    Span::styled("@ ", at_style),
                    Span::styled(path.clone(), path_style),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(lines);
        paragraph.render(inner, buf);
    }
}

fn format_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

const BLOCK_PARTIALS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

fn build_progress_bar(ratio: f64, width: usize) -> Vec<Span<'static>> {
    let ratio = ratio.clamp(0.0, 1.0);
    let total_steps = width * 8;
    let steps = (ratio * total_steps as f64).round() as usize;
    let full_cells = steps / 8;
    let remainder = steps % 8;

    let color = if ratio < 0.5 {
        Color::Rgb(98, 190, 68)
    } else if ratio < 0.75 {
        Color::Rgb(220, 180, 50)
    } else if ratio < 0.9 {
        Color::Rgb(240, 140, 40)
    } else {
        Color::Rgb(230, 80, 70)
    };

    let track_bg = Color::Rgb(60, 60, 60);
    let full_style = Style::default().fg(color).bg(track_bg);
    let empty_style = Style::default().bg(track_bg);

    let mut spans = Vec::with_capacity(3);

    // Filled full cells with track background
    if full_cells > 0 {
        spans.push(Span::styled("█".repeat(full_cells), full_style));
    }

    // Partial cell using foreground color on track background
    if remainder > 0 && full_cells < width {
        spans.push(Span::styled(BLOCK_PARTIALS[remainder], full_style));
    }

    // Empty cells with track background only
    let filled_count = full_cells + (if remainder > 0 { 1 } else { 0 });
    if filled_count < width {
        spans.push(Span::styled(" ".repeat(width - filled_count), empty_style));
    }

    spans
}
