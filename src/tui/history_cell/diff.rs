use ratatui::prelude::*;

use super::wrap_text;

pub(super) const MAX_DISPLAY_DIFF_LINES: usize = 40;

/// Background color constants (dark terminal palette)
const ADD_LINE_BG: Color = Color::Rgb(33, 58, 43); // #213A2B
const DEL_LINE_BG: Color = Color::Rgb(74, 34, 29); // #4A221D

/// A single hunk in the precomputed format (see `serialize_diff_data` in the tools module)
struct DiffHunk {
    old_start: Option<usize>,
    new_start: Option<usize>,
    changes: Vec<(char, String)>,
}

enum DiffPayload {
    /// New format: hunk data precomputed at tool execution time (`format: "diff"`)
    Hunks {
        additions: usize,
        deletions: usize,
        old_lines: usize,
        new_lines: usize,
        is_new_file: bool,
        hunks: Vec<DiffHunk>,
    },
    /// Legacy format: full old/new text; diff computed on the fly at render time.
    /// TODO(transition compat): only used to render legacy data persisted before
    /// this upgrade. Remove this branch after the transition period (e.g. 0.10.0);
    /// legacy data will no longer render a diff.
    Legacy { old: Option<String>, new: String },
}

struct DiffData {
    path: String,
    payload: DiffPayload,
}

fn parse_diff_data(json: &str) -> Option<DiffData> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let path = v["path"].as_str()?.to_string();

    if v["format"].as_str() == Some("diff") {
        let hunks = v["hunks"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| {
                        let changes = h["changes"]
                            .as_array()?
                            .iter()
                            .filter_map(|c| {
                                Some((c[0].as_str()?.chars().next()?, c[1].as_str()?.to_string()))
                            })
                            .collect::<Vec<_>>();
                        Some(DiffHunk {
                            old_start: h["old_start"].as_u64().map(|n| n as usize),
                            new_start: h["new_start"].as_u64().map(|n| n as usize),
                            changes,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return Some(DiffData {
            path,
            payload: DiffPayload::Hunks {
                additions: v["additions"].as_u64()? as usize,
                deletions: v["deletions"].as_u64()? as usize,
                old_lines: v["old_lines"].as_u64().unwrap_or(0) as usize,
                new_lines: v["new_lines"].as_u64().unwrap_or(0) as usize,
                is_new_file: v["is_new_file"].as_bool().unwrap_or(false),
                hunks,
            },
        });
    }

    Some(DiffData {
        path,
        payload: DiffPayload::Legacy {
            old: v["old"].as_str().map(|s| s.to_string()),
            new: v["new"].as_str()?.to_string(),
        },
    })
}

/// Intermediate line used for rendering
struct DiffLine {
    sign: char,
    ln: Option<usize>,
    text: String,
}

fn advance_line_counter(counter: &mut Option<usize>) -> Option<usize> {
    let cur = *counter;
    if let Some(c) = counter {
        *c += 1;
    }
    cur
}

pub(super) fn render_diff_from_json(json: &str, width: u16) -> Vec<Line<'static>> {
    let data = match parse_diff_data(json) {
        Some(d) => d,
        None => return Vec::new(),
    };

    // Expand into intermediate lines + hunk boundaries + stats line
    let (mut diff_lines, hunk_starts, stats_text, ln_width) = match &data.payload {
        DiffPayload::Hunks {
            additions,
            deletions,
            old_lines,
            new_lines,
            is_new_file,
            hunks,
        } => {
            let mut diff_lines = Vec::new();
            let mut hunk_starts = Vec::new();
            for hunk in hunks {
                hunk_starts.push(diff_lines.len());
                let mut old_counter = hunk.old_start;
                let mut new_counter = hunk.new_start;
                for (sign, text) in &hunk.changes {
                    let ln = match sign {
                        '-' => advance_line_counter(&mut old_counter),
                        '+' => advance_line_counter(&mut new_counter),
                        _ => {
                            let n = new_counter;
                            advance_line_counter(&mut new_counter);
                            advance_line_counter(&mut old_counter);
                            n
                        }
                    };
                    diff_lines.push(DiffLine {
                        sign: *sign,
                        ln,
                        text: text.clone(),
                    });
                }
            }
            let stats_text = if *is_new_file {
                format!("(+{} new file)", additions)
            } else {
                format!("(+{} -{})", additions, deletions)
            };
            (
                diff_lines,
                hunk_starts,
                stats_text,
                (*old_lines).max(*new_lines).to_string().len(),
            )
        }
        DiffPayload::Legacy { old, new } => {
            use similar::{ChangeTag, TextDiff};

            let old_lines: Vec<&str> = old
                .as_deref()
                .map(|s| s.lines().collect())
                .unwrap_or_default();
            let new_lines: Vec<&str> = new.lines().collect();
            let diff = TextDiff::from_slices(&old_lines, &new_lines);

            let mut additions = 0usize;
            let mut deletions = 0usize;
            for change in diff.iter_all_changes() {
                match change.tag() {
                    ChangeTag::Insert => additions += 1,
                    ChangeTag::Delete => deletions += 1,
                    _ => {}
                }
            }

            let stats_text = if old.is_none() {
                format!("(+{} new file)", additions)
            } else {
                format!("(+{} -{})", additions, deletions)
            };

            let mut diff_lines = Vec::new();
            let mut hunk_starts = Vec::new();
            for group in diff.grouped_ops(3) {
                hunk_starts.push(diff_lines.len());
                for op in &group {
                    for change in diff.iter_changes(op) {
                        let sign = match change.tag() {
                            ChangeTag::Insert => '+',
                            ChangeTag::Delete => '-',
                            ChangeTag::Equal => ' ',
                        };
                        let ln = match change.tag() {
                            ChangeTag::Insert => change.new_index().map(|i| i + 1),
                            ChangeTag::Delete => change.old_index().map(|i| i + 1),
                            ChangeTag::Equal => change.new_index().map(|i| i + 1),
                        };
                        diff_lines.push(DiffLine {
                            sign,
                            ln,
                            text: change.value().to_string(),
                        });
                    }
                }
            }
            (
                diff_lines,
                hunk_starts,
                stats_text,
                new_lines.len().to_string().len(),
            )
        }
    };

    // Render cap: at most 40 lines whether data comes from precomputed hunks or legacy full text
    let truncated = diff_lines.len() > MAX_DISPLAY_DIFF_LINES;
    if truncated {
        diff_lines.truncate(MAX_DISPLAY_DIFF_LINES);
    }

    // Stats line: path (+N -M) or path (new file, +N lines)
    let short_path = data.path.rsplit(['/', '\\']).next().unwrap_or(&data.path);

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "  └ ".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
        Span::styled(short_path.to_string(), Style::default().fg(Color::Gray)),
        Span::raw(" "),
        Span::styled(
            stats_text,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]));

    // diff body — with line-number gutter and background colors
    let indent = "    ";

    // Prefix width: 4(indent) + ln_width + 1(gutter space) + 1(sign)
    let prefix_w = 4 + ln_width + 2;
    let content_w = width.saturating_sub(prefix_w as u16) as usize;

    for (idx, dl) in diff_lines.iter().enumerate() {
        // Hunk separator ⋮ (except for the first group)
        if idx > 0 && hunk_starts.contains(&idx) {
            let spacer = format!("{:width$} ", "", width = ln_width.max(1));
            lines.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(spacer, Style::default().add_modifier(Modifier::DIM)),
                Span::styled(
                    "⋮".to_string(),
                    Style::default().add_modifier(Modifier::DIM),
                ),
            ]));
        }

        let (sign_color, content_color, bg_color) = match dl.sign {
            '+' => (Color::Green, Color::Green, Some(ADD_LINE_BG)),
            '-' => (Color::Red, Color::Red, Some(DEL_LINE_BG)),
            _ => (Color::DarkGray, Color::DarkGray, None),
        };

        // Line numbers (insert/new use new line numbers, delete uses old, context uses new)
        let gutter_text = match dl.ln {
            Some(n) => format!("{:>width$} ", n, width = ln_width),
            None => format!("{:width$} ", "", width = ln_width),
        };

        let wrapped = if content_w > 0 {
            wrap_text(&dl.text, content_w as u16)
        } else {
            vec![dl.text.clone()]
        };

        for (i, wl) in wrapped.iter().enumerate() {
            let mut spans = vec![Span::raw(indent)];

            if i == 0 {
                spans.push(Span::styled(
                    gutter_text.clone(),
                    Style::default().add_modifier(Modifier::DIM),
                ));
                spans.push(Span::styled(
                    dl.sign.to_string(),
                    Style::default().fg(sign_color),
                ));
            } else {
                let pad = format!("{:width$}  ", "", width = ln_width);
                spans.push(Span::raw(pad));
            }

            spans.push(Span::styled(wl.clone(), Style::default().fg(content_color)));

            let mut line = Line::from(spans);
            if let Some(bg) = bg_color {
                line = line.style(Style::default().bg(bg));
            }
            lines.push(line);
        }
    }

    if truncated {
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(
                format!("... diff truncated (max {} lines)", MAX_DISPLAY_DIFF_LINES),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
        ]));
    }

    lines
}
