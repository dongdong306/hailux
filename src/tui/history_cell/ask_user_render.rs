use ratatui::prelude::*;

use super::{truncate_str, wrap_text};

/// 从 ask_user 工具结果中解析 "question"="answer" 对
pub(super) fn parse_ask_user_pairs(result: &str) -> Vec<(String, String)> {
    /// 还原 ask_escape 的转义：`\\`→`\`、`\"`→`"`；孤立反斜杠（异常输入/旧数据）原样保留
    fn unescape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    let mut pairs = Vec::new();
    let bytes = result.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // 解析 key
            let key_start = i + 1;
            let mut j = key_start;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'"' {
                    break;
                }
                j += 1;
            }
            if j >= bytes.len() {
                break;
            }
            let key = unescape(&result[key_start..j]);

            // 寻找 =
            let mut k = j + 1;
            while k < bytes.len() && bytes[k] == b' ' {
                k += 1;
            }
            if k >= bytes.len() || bytes[k] != b'=' {
                i = j + 1;
                continue;
            }
            k += 1;
            while k < bytes.len() && bytes[k] == b' ' {
                k += 1;
            }
            if k >= bytes.len() || bytes[k] != b'"' {
                i = j + 1;
                continue;
            }

            // 解析 value
            let val_start = k + 1;
            let mut m = val_start;
            while m < bytes.len() {
                if bytes[m] == b'\\' {
                    m += 2;
                    continue;
                }
                if bytes[m] == b'"' {
                    break;
                }
                m += 1;
            }
            if m >= bytes.len() {
                break;
            }
            let val = unescape(&result[val_start..m]);

            pairs.push((key, val));
            i = m + 1;
        } else {
            i += 1;
        }
    }
    pairs
}

/// 渲染 ask_user 工具调用的 Q&A 结果
pub(super) fn render_ask_user_result(
    arguments: &str,
    result: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    if result.contains("[User Cancelled]") {
        lines.push(Line::from(Span::styled(
            "  └ cancelled",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
        return lines;
    }

    let pairs = parse_ask_user_pairs(result);
    let detail_w = (width as usize).saturating_sub(4).max(10);

    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let questions = args["questions"].as_array();

    if let Some(qs) = questions {
        let total = qs.len();
        for (qi, q) in qs.iter().enumerate() {
            let question = q["question"].as_str().unwrap_or("");
            let answer = pairs
                .iter()
                .find(|(k, _)| k == question)
                .map(|(_, v)| v.as_str())
                .unwrap_or("Unanswered");

            let connector = if qi == total - 1 { "└" } else { "├" };

            // Q 行
            let q_label = "Q: ";
            let q_w = detail_w.saturating_sub(q_label.len());
            let q_wrapped = wrap_text(question, q_w.max(1) as u16);
            for (li, wline) in q_wrapped.iter().enumerate() {
                let prefix = if li == 0 {
                    format!("  {} {}", connector, q_label)
                } else {
                    format!("    {}", " ".repeat(q_label.len()))
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                    Span::styled(wline.clone(), Style::default().fg(Color::Gray)),
                ]));
            }

            // A 行
            let a_label = "A: ";
            let a_w = detail_w.saturating_sub(a_label.len());
            let a_wrapped = wrap_text(answer, a_w.max(1) as u16);
            let a_prefix_align = " ".repeat(a_label.len());
            for (li, wline) in a_wrapped.iter().enumerate() {
                let prefix = if li == 0 {
                    format!("    {}", a_label)
                } else {
                    format!("    {}", a_prefix_align)
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                    Span::styled(wline.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
    } else {
        // 回退：无参数时直接显示原始键值对
        let total = pairs.len();
        for (qi, (k, v)) in pairs.iter().enumerate() {
            let connector = if qi == total - 1 { "└" } else { "├" };
            let qa_text = format!("{}: {}", truncate_str(k, 20), v);
            let wrapped = wrap_text(&qa_text, detail_w as u16);
            for (li, wline) in wrapped.iter().enumerate() {
                let prefix = if li == 0 {
                    format!("  {} ", connector)
                } else {
                    "    ".to_string()
                };
                lines.push(Line::from(Span::styled(
                    format!("{}{}", prefix, wline),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    lines
}
