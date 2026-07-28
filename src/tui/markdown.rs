use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd, TextMergeStream};
use ratatui::prelude::*;
use ratatui::text::Line;
use std::sync::LazyLock;
use syntect::highlighting::{Style as SynStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// Markdown 元素样式（对齐 Codex 风格）
struct MarkdownStyles {
    h1: Style,
    h2: Style,
    h3: Style,
    h4: Style,
    h5: Style,
    h6: Style,
    code: Style,
    emphasis: Style,
    strong: Style,
    strikethrough: Style,
    ordered_list_marker: Style,
    unordered_list_marker: Style,
    link: Style,
    blockquote: Style,
    code_block_border: Style,
    table_border: Style,
    table_header: Style,
}

impl Default for MarkdownStyles {
    fn default() -> Self {
        Self {
            h1: Style::new()
                .fg(Color::Rgb(255, 200, 100))
                .bold()
                .underlined(),
            h2: Style::new().fg(Color::Rgb(255, 175, 95)).bold(),
            h3: Style::new().fg(Color::Rgb(120, 210, 220)).bold(),
            h4: Style::new().fg(Color::Rgb(130, 170, 230)).bold(),
            h5: Style::new().fg(Color::Rgb(180, 190, 210)).bold(),
            h6: Style::new().fg(Color::Rgb(150, 150, 160)).bold(),
            code: Style::new().cyan(),
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().crossed_out(),
            ordered_list_marker: Style::new().light_blue(),
            unordered_list_marker: Style::new(),
            link: Style::new().cyan().underlined(),
            blockquote: Style::new().green(),
            code_block_border: Style::new().fg(Color::Rgb(120, 120, 140)),
            table_border: Style::new().fg(Color::Rgb(100, 100, 120)),
            table_header: Style::new().bold(),
        }
    }
}

static STYLES: LazyLock<MarkdownStyles> = LazyLock::new(MarkdownStyles::default);

/// 将 Markdown 文本渲染为 ratatui 的 Line 列表
pub fn render_markdown(text: &str, max_width: u16) -> Vec<Line<'static>> {
    let max_w = max_width as usize;
    let mut renderer = MarkdownRenderer::new(max_w);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(text, options);
    let merged = TextMergeStream::new(parser);

    for event in merged {
        renderer.handle_event(event);
    }
    renderer.finish()
}

struct MarkdownRenderer {
    max_width: usize,
    lines: Vec<Line<'static>>,
    current_spans: Vec<Span<'static>>,
    current_line_len: usize,
    in_code_block: bool,
    code_block_lang: String,
    code_block_content: String,
    in_heading: bool,
    heading_level: u8,
    style_stack: Vec<Style>,
    list_depth: usize,
    list_counters: Vec<u64>,
    in_blockquote: bool,
    table: Option<TableState>,
}

struct TableState {
    alignments: Vec<Alignment>,
    header_row: Vec<String>,
    body_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    is_header: bool,
}

impl MarkdownRenderer {
    fn new(max_width: usize) -> Self {
        Self {
            max_width,
            lines: Vec::new(),
            current_spans: Vec::new(),
            current_line_len: 0,
            in_code_block: false,
            code_block_lang: String::new(),
            code_block_content: String::new(),
            in_heading: false,
            heading_level: 0,
            style_stack: Vec::new(),
            list_depth: 0,
            list_counters: Vec::new(),
            in_blockquote: false,
            table: None,
        }
    }

    fn current_style(&self) -> Style {
        let mut style = Style::default();
        for s in &self.style_stack {
            style = style.patch(*s);
        }
        if self.in_heading {
            let heading_style = match self.heading_level {
                1 => STYLES.h1,
                2 => STYLES.h2,
                3 => STYLES.h3,
                4 => STYLES.h4,
                5 => STYLES.h5,
                _ => STYLES.h6,
            };
            style = style.patch(heading_style);
        }
        if self.in_blockquote {
            style = style.patch(STYLES.blockquote);
        }
        style
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.handle_start(tag),
            Event::End(tag_end) => self.handle_end(tag_end),
            Event::Text(text) => self.handle_text(&text),
            Event::Code(code) => self.handle_inline_code(&code),
            Event::SoftBreak | Event::HardBreak => {
                self.flush_line();
            }
            Event::Html(html) => {
                self.push_styled_text(&html);
            }
            _ => {}
        }
    }

    fn handle_start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                if !self.current_spans.is_empty() {
                    self.flush_line();
                }
                if self.lines.last().is_some_and(|l| !l.spans.is_empty()) {
                    self.lines.push(Line::from(""));
                }
                self.in_heading = true;
                self.heading_level = level as u8;
            }
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.code_block_content.clear();
                self.code_block_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                    _ => String::new(),
                };
            }
            Tag::Emphasis => {
                self.style_stack.push(STYLES.emphasis);
            }
            Tag::Strong => {
                self.style_stack.push(STYLES.strong);
            }
            Tag::Strikethrough => {
                self.style_stack.push(STYLES.strikethrough);
            }
            Tag::List(start_num) => {
                self.list_depth += 1;
                if let Some(n) = start_num {
                    self.list_counters.push(n);
                } else {
                    self.list_counters.push(1);
                }
            }
            Tag::Item => {
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                if let Some(counter) = self.list_counters.last_mut() {
                    let marker = format!("{}{}. ", indent, *counter);
                    *counter += 1;
                    let w = UnicodeWidthStr::width(marker.trim());
                    self.current_spans
                        .push(Span::styled(marker, STYLES.ordered_list_marker));
                    self.current_line_len += w;
                } else {
                    let marker = format!("{}- ", indent);
                    let w = UnicodeWidthStr::width(marker.trim());
                    self.current_spans
                        .push(Span::styled(marker, STYLES.unordered_list_marker));
                    self.current_line_len += w;
                }
            }
            Tag::BlockQuote(_) => {
                self.in_blockquote = true;
                let span = Span::styled("> ".to_string(), STYLES.blockquote);
                self.current_spans.push(span);
                self.current_line_len += 2;
            }
            Tag::Link { .. } => {
                self.style_stack.push(STYLES.link);
            }
            Tag::Table(alignments) => {
                self.table = Some(TableState {
                    alignments: alignments.to_vec(),
                    header_row: Vec::new(),
                    body_rows: Vec::new(),
                    current_row: Vec::new(),
                    current_cell: String::new(),
                    is_header: false,
                });
            }
            Tag::TableHead => {
                if let Some(t) = &mut self.table {
                    t.is_header = true;
                    t.current_row.clear();
                }
            }
            Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.current_row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(t) = &mut self.table {
                    t.current_cell.clear();
                }
            }
            _ => {}
        }
    }

    fn handle_end(&mut self, tag_end: TagEnd) {
        match tag_end {
            TagEnd::Paragraph => {
                self.flush_line();
            }
            TagEnd::Heading(_) => {
                self.in_heading = false;
                self.flush_line();
                self.lines.push(Line::from(""));
            }
            TagEnd::CodeBlock => {
                self.flush_code_block();
                self.in_code_block = false;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.style_stack.pop();
            }
            TagEnd::List(_) => {
                self.list_depth = self.list_depth.saturating_sub(1);
                self.list_counters.pop();
                if self.list_depth == 0 {
                    self.flush_line();
                }
            }
            TagEnd::Item => {
                self.flush_line();
            }
            TagEnd::BlockQuote(_) => {
                self.in_blockquote = false;
                self.flush_line();
            }
            TagEnd::Link => {
                self.style_stack.pop();
            }
            TagEnd::TableCell => {
                if let Some(t) = &mut self.table {
                    t.current_row.push(std::mem::take(&mut t.current_cell));
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = &mut self.table {
                    let row = std::mem::take(&mut t.current_row);
                    if !row.is_empty() {
                        t.body_rows.push(row);
                    }
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    t.header_row = std::mem::take(&mut t.current_row);
                    t.is_header = false;
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.render_table(&t);
                }
            }
            _ => {}
        }
    }

    fn handle_text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_block_content.push_str(text);
            return;
        }
        if let Some(t) = &mut self.table {
            t.current_cell.push_str(text);
            return;
        }
        self.push_styled_text(text);
    }

    fn handle_inline_code(&mut self, code: &str) {
        if let Some(t) = &mut self.table {
            t.current_cell.push_str(code);
            return;
        }
        let span = Span::styled(code.to_string(), STYLES.code);
        let w = UnicodeWidthStr::width(code);
        self.current_spans.push(span);
        self.current_line_len += w;
    }

    fn push_styled_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let style = self.current_style();

        for (i, line_text) in text.split('\n').enumerate() {
            if i > 0 {
                self.flush_line();
            }

            if line_text.is_empty() {
                continue;
            }

            let w = UnicodeWidthStr::width(line_text);
            let available = self.max_width.saturating_sub(self.current_line_len);

            if w <= available {
                self.current_spans
                    .push(Span::styled(line_text.to_string(), style));
                self.current_line_len += w;
            } else {
                let mut line_start = 0;
                let mut current_width = self.current_line_len;

                for (byte_pos, ch) in line_text.char_indices() {
                    let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if current_width + cw > self.max_width {
                        let slice = &line_text[line_start..byte_pos];
                        if !slice.is_empty() {
                            self.current_spans
                                .push(Span::styled(slice.to_string(), style));
                            self.current_line_len += UnicodeWidthStr::width(slice);
                        }
                        self.flush_line();
                        line_start = byte_pos;
                        current_width = 0;
                    }
                    current_width += cw;
                }

                let remaining = &line_text[line_start..];
                if !remaining.is_empty() {
                    let rw = UnicodeWidthStr::width(remaining);
                    self.current_spans
                        .push(Span::styled(remaining.to_string(), style));
                    self.current_line_len += rw;
                }
            }
        }
    }

    fn flush_line(&mut self) {
        if !self.current_spans.is_empty() {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current_spans)));
        } else {
            self.lines.push(Line::from(""));
        }
        self.current_line_len = 0;
    }

    fn render_table(&mut self, state: &TableState) {
        let col_count = if !state.header_row.is_empty() {
            state.header_row.len()
        } else if !state.body_rows.is_empty() {
            state.body_rows[0].len()
        } else {
            return;
        };

        let all_rows: Vec<&Vec<String>> = std::iter::once(&state.header_row)
            .chain(state.body_rows.iter())
            .filter(|r| !r.is_empty())
            .collect();

        if all_rows.is_empty() {
            return;
        }

        let mut col_widths = vec![0usize; col_count];
        for row in &all_rows {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    col_widths[i] = col_widths[i].max(UnicodeWidthStr::width(cell.as_str()));
                }
            }
        }

        let border_overhead = col_count * 3 + 1;
        let total_content_width: usize = col_widths.iter().sum();
        let max_content = self.max_width.saturating_sub(border_overhead);
        if total_content_width > max_content {
            let scale = max_content as f64 / total_content_width as f64;
            let mut scaled: Vec<usize> = col_widths
                .iter()
                .map(|&w| ((w as f64) * scale).floor() as usize)
                .collect();
            let sum: usize = scaled.iter().sum();
            let mut diff = max_content.saturating_sub(sum);
            let mut i = 0;
            while diff > 0 {
                scaled[i % col_count] += 1;
                diff -= 1;
                i += 1;
            }
            col_widths = scaled;
        }

        let border_ch = "─";
        let border_style = STYLES.table_border;

        let mut separator = String::from("├");
        let mut top_border = String::from("┌");
        let mut bottom_border = String::from("└");
        for (i, &w) in col_widths.iter().enumerate() {
            let line = border_ch.repeat(w + 2);
            top_border.push_str(&line);
            separator.push_str(&line);
            bottom_border.push_str(&line);
            if i < col_count - 1 {
                top_border.push('┬');
                separator.push('┼');
                bottom_border.push('┴');
            }
        }
        top_border.push('┐');
        separator.push('┤');
        bottom_border.push('┘');

        self.lines
            .push(Line::from(Span::styled(top_border, border_style)));

        if !state.header_row.is_empty() {
            let spans = self.format_row(&state.header_row, &col_widths, &state.alignments, true);
            self.lines.push(spans);
            let mut header_sep = String::from("╞");
            for (i, &w) in col_widths.iter().enumerate() {
                let line = "═".repeat(w + 2);
                header_sep.push_str(&line);
                if i < col_count - 1 {
                    header_sep.push('╪');
                }
            }
            header_sep.push('╡');
            self.lines
                .push(Line::from(Span::styled(header_sep, border_style)));
        }

        for (row_idx, row) in state.body_rows.iter().enumerate() {
            let spans = self.format_row(row, &col_widths, &state.alignments, false);
            self.lines.push(spans);
            if row_idx < state.body_rows.len() - 1 {
                self.lines
                    .push(Line::from(Span::styled(separator.clone(), border_style)));
            }
        }

        self.lines
            .push(Line::from(Span::styled(bottom_border, border_style)));
    }

    fn format_row(
        &self,
        row: &[String],
        col_widths: &[usize],
        alignments: &[Alignment],
        is_header: bool,
    ) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = vec![Span::styled("│ ", STYLES.table_border)];

        for (i, width) in col_widths.iter().enumerate() {
            let cell_text = row.get(i).map(|s| s.as_str()).unwrap_or("");
            let cell_width = UnicodeWidthStr::width(cell_text);
            let padding = width.saturating_sub(cell_width);

            let align = alignments.get(i).copied().unwrap_or(Alignment::None);
            let (left_pad, right_pad) = match align {
                Alignment::Center => (padding / 2, padding.saturating_sub(padding / 2)),
                Alignment::Right => (padding, 0),
                _ => (0, padding),
            };

            if left_pad > 0 {
                spans.push(Span::from(" ".repeat(left_pad)));
            }

            let cell_style = if is_header {
                STYLES.table_header
            } else {
                Style::default()
            };
            spans.push(Span::styled(cell_text.to_string(), cell_style));

            if right_pad > 0 {
                spans.push(Span::from(" ".repeat(right_pad)));
            }

            if i < col_widths.len() - 1 {
                spans.push(Span::styled(" │ ", STYLES.table_border));
            }
        }

        spans.push(Span::styled(" │", STYLES.table_border));
        Line::from(spans)
    }

    fn flush_code_block(&mut self) {
        let lang_display = if self.code_block_lang.is_empty() {
            String::new()
        } else {
            format!(" {}", self.code_block_lang)
        };
        self.lines.push(Line::from(Span::styled(
            format!(" ┌─{}", lang_display),
            STYLES.code_block_border,
        )));

        let highlighted = self.highlight_code(&self.code_block_content, &self.code_block_lang);
        let gutter_width = 3;

        for line_spans in highlighted {
            let mut spans: Vec<Span<'static>> = vec![Span::styled(" │ ", STYLES.code_block_border)];

            let mut line_width = gutter_width;
            for span in line_spans {
                let span_text: &str = &span.content;
                let span_style = span.style;
                let span_width = UnicodeWidthStr::width(span_text);
                if line_width + span_width <= self.max_width {
                    spans.push(span);
                    line_width += span_width;
                } else {
                    let remaining = self.max_width.saturating_sub(line_width);
                    if remaining > 0 {
                        let mut truncated = String::new();
                        let mut w = 0;
                        for ch in span_text.chars() {
                            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                            if w + cw > remaining {
                                break;
                            }
                            truncated.push(ch);
                            w += cw;
                        }
                        if !truncated.is_empty() {
                            spans.push(Span::styled(truncated, span_style));
                        }
                    }
                    break;
                }
            }
            self.lines.push(Line::from(spans));
        }

        self.lines
            .push(Line::from(Span::styled(" └─", STYLES.code_block_border)));
    }

    fn highlight_code(&self, code: &str, lang: &str) -> Vec<Vec<Span<'static>>> {
        let syntax = if !lang.is_empty() {
            SYNTAX_SET
                .find_syntax_by_token(lang)
                .or_else(|| SYNTAX_SET.find_syntax_by_extension(lang))
        } else {
            None
        };

        let syntax = syntax.unwrap_or_else(|| {
            SYNTAX_SET
                .find_syntax_by_first_line(code)
                .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text())
        });

        let theme = &THEME_SET.themes["base16-eighties.dark"];
        let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
        let mut result = Vec::new();

        for line in syntect::util::LinesWithEndings::from(code) {
            match highlighter.highlight_line(line, &SYNTAX_SET) {
                Ok(ranges) => {
                    let mut spans = Vec::new();
                    for (syn_style, text) in ranges {
                        let text = text.trim_end_matches('\n').trim_end_matches('\r');
                        if text.is_empty() {
                            continue;
                        }
                        let style = syn_style_to_ratatui(syn_style);
                        spans.push(Span::styled(text.to_string(), style));
                    }
                    result.push(spans);
                }
                Err(_) => {
                    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                    result.push(vec![Span::from(trimmed.to_string())]);
                }
            }
        }

        result
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.current_spans.is_empty() {
            self.flush_line();
        }
        while let Some(last) = self.lines.last() {
            if last.spans.is_empty() {
                self.lines.pop();
            } else {
                break;
            }
        }
        self.lines
    }
}

fn syn_style_to_ratatui(style: SynStyle) -> Style {
    let fg = color_to_ratatui(style.foreground);
    let mut rat_style = Style::default().fg(fg);

    if style
        .font_style
        .contains(syntect::highlighting::FontStyle::BOLD)
    {
        rat_style = rat_style.add_modifier(Modifier::BOLD);
    }
    if style
        .font_style
        .contains(syntect::highlighting::FontStyle::ITALIC)
    {
        rat_style = rat_style.add_modifier(Modifier::ITALIC);
    }
    if style
        .font_style
        .contains(syntect::highlighting::FontStyle::UNDERLINE)
    {
        rat_style = rat_style.add_modifier(Modifier::UNDERLINED);
    }

    rat_style
}

fn color_to_ratatui(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}
