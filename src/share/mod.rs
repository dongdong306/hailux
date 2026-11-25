//! Chat history export: renders a SQLite-backed session as a self-contained
//! single-file HTML document.
//!
//! Shared by the TUI (`/share` command) and Web (`GET /api/sessions/{id}/export`);
//! no dependency on any UI feature. Design notes:
//!
//! - **Visually aligned with the Web chat UI**: tokens/layout/component styles
//!   mirror `web/src/styles/globals.css` and `web/src/components/assistant-ui/*`
//!   (right-aligned muted bubble for the user, bubble-less body for the
//!   assistant, rounded-border cards for tool calls/reasoning, plain-text done
//!   row `model · 4.3s · status`, thin rules on both sides of the compact
//!   divider, markdown component styles and code block language header). Light
//!   is the default; dark follows the system preference.
//! - **Full history**: compacted messages render as usual, with a divider
//!   inserted at the compaction boundary (mirrors the rebuild algorithm of
//!   `session_ops::load_session_messages`, but without active-range filtering).
//! - **Dangling tool_call tolerance**: `repair_orphaned_tool_calls` only covers
//!   the active range (dangling calls in compacted ranges are never repaired by
//!   design), so when the pairing map misses, the tool call block still
//!   renders, just without its result section.
//! - **XSS protection**: after pulldown-cmark parses the raw markdown,
//!   `Html`/`InlineHtml` events are downgraded to plain text (push_html escapes
//!   them on output); all other text goes through [`escape_html`] before
//!   templating; images only accept `image/*` data URLs.
//! - **Single file**: CSS inlined, images base64-inlined, icons as inline SVG;
//!   no external resources.

use color_eyre::Result;
use pulldown_cmark::{Event, Options, Parser};
use serde_json::Value;

use crate::storage::{ChatStorage, MessageRole, StoredMessage};

static TEMPLATE: &str = include_str!("template.html");

/// Export result: raw title, filename-safe slug and the full HTML document.
pub struct ShareExport {
    /// Raw session title (empty sessions become "未命名会话")
    pub title: String,
    /// Filename-safe title slug (no extension), used for local file naming
    pub slug: String,
    /// Full HTML document
    pub html: String,
}

/// Exports an entire session (including compacted history) as single-file HTML.
pub async fn export_session_html(storage: &ChatStorage, session_id: &str) -> Result<ShareExport> {
    let info = storage
        .get_session_info(session_id)
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("会话不存在: {session_id}"))?;
    let messages = storage.load_messages(session_id).await?;
    let compact_summary = storage.get_compact_summary(session_id).await?;
    let work_dir = storage
        .get_session_work_dir(session_id)
        .await?
        .map(|d| strip_verbatim(&d));

    let blocks = build_blocks(&messages, compact_summary.is_some(), work_dir.as_deref());
    let body = render_body(&blocks);

    let (prompt, completion, cached) =
        messages.iter().fold((0_i64, 0_i64, 0_i64), |(p, c, k), m| {
            match m.prompt_tokens {
                Some(pt) => (
                    p + pt,
                    c + m.completion_tokens.unwrap_or(0),
                    k + m.cached_tokens.unwrap_or(0),
                ),
                None => (p, c, k),
            }
        });

    let title = if info.title.trim().is_empty() {
        "未命名会话".to_string()
    } else {
        info.title.clone()
    };
    let slug = slugify_title(&info.title);
    let rounds = blocks
        .iter()
        .filter(|b| matches!(b, ShareBlock::User { .. }))
        .count();

    let mut meta = vec![
        escape_html(&info.model),
        escape_html(&info.updated_at.replace('T', " ")),
    ];
    meta.push(format!("{rounds} 轮对话"));
    let tokens_line = fmt_tokens_line(prompt, completion, cached);
    if !tokens_line.is_empty() {
        meta.push(tokens_line);
    }

    let html = TEMPLATE
        .replace("{{TITLE}}", &neutralize_placeholders(escape_html(&title)))
        .replace(
            "{{META}}",
            &neutralize_placeholders(meta.join(" <span>·</span> ")),
        )
        .replace("{{BODY}}", &neutralize_placeholders(body))
        .replace(
            "{{EXPORTED}}",
            &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        );

    Ok(ShareExport { title, slug, html })
}

// ── Intermediate representation ──────────────────────────────

/// Display data of a tool call (mirrors web ToolFallback title layout: small verb + object body)
#[derive(Debug, Clone)]
struct ToolCallItem {
    verb: String,
    object: String,
    /// Secondary info such as search scope (grep/glob path/include)
    suffix: Option<String>,
    /// Raw arguments JSON (shown expanded, mirrors web argsText pre)
    args: String,
    /// None = dangling call (active-range repair deliberately skips compacted ranges)
    result: Option<String>,
}

#[derive(Debug, Clone)]
enum ShareBlock {
    /// User message (text + Plan badge + image data URLs)
    User {
        text: String,
        plan_mode: bool,
        images: Vec<String>,
    },
    /// Assistant body (rendered markdown HTML fragment)
    Assistant { html: String },
    /// Reasoning process (collapsible card)
    Reasoning { text: String, think_ms: Option<i64> },
    /// Single tool call (collapsible card)
    ToolCall(ToolCallItem),
    /// Merged card for consecutive tool calls (mirrors web ToolGroup: merged
    /// at ≥2 calls, collapsible rows inside)
    ToolGroup(Vec<ToolCallItem>),
    /// todo_write card (mirrors web TodoView; raw args/result not shown)
    Todo { items: Vec<TodoItem> },
    /// question Q&A card (mirrors web AskUserView)
    AskUser {
        cancelled: bool,
        pairs: Vec<(String, String)>,
    },
    /// Done marker row for one LLM request (model · 4.3s · status)
    Done {
        model: String,
        total_ms: Option<u64>,
        status: &'static str,
    },
    /// Context compaction divider
    CompactDivider { count: usize },
}

#[derive(Debug, Clone)]
struct TodoItem {
    content: String,
    status: String,
}

/// Builds display blocks from the full message list. The algorithm mirrors
/// `session_ops::load_session_messages` and
/// `session-history.ts::rebuildSessionItems`, but without active-range
/// filtering: compacted messages render as usual, with a divider inserted only
/// at the compaction boundary.
fn build_blocks(
    messages: &[StoredMessage],
    has_summary: bool,
    work_dir: Option<&str>,
) -> Vec<ShareBlock> {
    let mut blocks = Vec::new();

    // Pre-scan: tool_call_id -> result, first-wins (mirrors TUI or_insert)
    let mut tool_results: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for msg in messages {
        if msg.role == MessageRole::Tool
            && let Some(id) = msg.tool_call_id.as_deref()
        {
            tool_results
                .entry(id.to_string())
                .or_insert_with(|| msg.content.clone());
        }
    }

    let mut compact_divider_inserted = false;
    for (idx, msg) in messages.iter().enumerate() {
        if !compact_divider_inserted
            && has_summary
            && idx > 0
            && messages[idx - 1].compacted
            && !msg.compacted
        {
            blocks.push(ShareBlock::CompactDivider { count: idx });
            compact_divider_inserted = true;
        }

        match msg.role {
            MessageRole::User => {
                // Synthetic tool-image message (read on images etc., generated
                // to inject model context): runtime_meta carries the
                // {"format":"tool-attachment"} marker, skip rendering
                if is_tool_attachment(msg) {
                    continue;
                }
                let plan_mode = runtime_meta_value(msg)
                    .as_ref()
                    .and_then(|v| v.get("plan_mode"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let images = parse_image_attachments(msg);
                blocks.push(ShareBlock::User {
                    text: msg.content.clone(),
                    plan_mode,
                    images,
                });
            }
            MessageRole::Assistant => {
                if let Some(reasoning) = msg.reasoning_content.as_ref()
                    && !reasoning.trim().is_empty()
                {
                    blocks.push(ShareBlock::Reasoning {
                        text: reasoning.clone(),
                        think_ms: msg.think_ms,
                    });
                }
                if !msg.content.trim().is_empty() {
                    blocks.push(ShareBlock::Assistant {
                        html: markdown_to_html(&msg.content),
                    });
                }
                if let Some(tc_json) = msg.tool_calls.as_deref()
                    && let Ok(calls) = serde_json::from_str::<Value>(tc_json)
                    && let Some(arr) = calls.as_array()
                {
                    for call in arr {
                        let func = call.get("function").or_else(|| call.get("custom_tool"));
                        let name = func
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if name.is_empty() {
                            continue;
                        }
                        let arguments = func
                            .and_then(|f| f.get("arguments").or_else(|| f.get("input")))
                            .map(|v| match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default();
                        let call_id = call.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        // Malformed entries with an empty id (half-built calls
                        // cancelled mid-stream, never answerable) are skipped
                        // entirely, mirroring the drop semantics of
                        // ensure_tool_call_parity
                        if call_id.is_empty() {
                            continue;
                        }
                        let result = tool_results.remove(call_id);
                        blocks.push(build_tool_block(name, &arguments, result, work_dir));
                    }
                }
                if let Some(meta) = runtime_meta_value(msg)
                    .as_ref()
                    .and_then(|v| v.get("total_ms"))
                    .and_then(|v| v.as_u64())
                {
                    let status = runtime_meta_value(msg)
                        .as_ref()
                        .and_then(|v| v.get("status"))
                        .and_then(|v| v.as_str())
                        .map(|s| match s {
                            "interrupted" => "interrupted",
                            "error" => "error",
                            _ => "completed",
                        })
                        .unwrap_or("completed");
                    blocks.push(ShareBlock::Done {
                        model: runtime_meta_value(msg)
                            .as_ref()
                            .and_then(|v| v.get("model"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        total_ms: Some(meta),
                        status,
                    });
                }
            }
            // Tool already consumed via the pairing map; System is an internal prompt, not exported
            MessageRole::Tool | MessageRole::System => {}
        }
    }

    // All messages compacted (no conversation after compaction): append a tail divider
    if !compact_divider_inserted
        && has_summary
        && !messages.is_empty()
        && messages.iter().all(|m| m.compacted)
    {
        blocks.push(ShareBlock::CompactDivider {
            count: messages.len(),
        });
    }

    merge_tool_groups(blocks)
}

/// Merges ≥2 consecutive tool calls into a ToolGroup card (mirrors the web
/// groupBy + ToolGroup: a single call degrades to a standalone card)
fn merge_tool_groups(blocks: Vec<ShareBlock>) -> Vec<ShareBlock> {
    let mut merged: Vec<ShareBlock> = Vec::with_capacity(blocks.len());
    let mut pending: Vec<ToolCallItem> = Vec::new();
    for block in blocks {
        match block {
            ShareBlock::ToolCall(item) => pending.push(item),
            other => {
                flush_tool_group(&mut pending, &mut merged);
                merged.push(other);
            }
        }
    }
    flush_tool_group(&mut pending, &mut merged);
    merged
}

fn flush_tool_group(pending: &mut Vec<ToolCallItem>, merged: &mut Vec<ShareBlock>) {
    if pending.len() >= 2 {
        merged.push(ShareBlock::ToolGroup(std::mem::take(pending)));
    } else {
        for item in pending.drain(..) {
            merged.push(ShareBlock::ToolCall(item));
        }
    }
}

// ── Rendering ────────────────────────────────────────────────

/// lucide icons (v1.31.0, ISC) — paths taken from lucide-react, identical to the Web UI
const ICON_CHECK: &str = "M20 6 9 17l-5-5";
const ICON_CHEVRON: &str = "m6 9 6 6 6-6";
const ICON_BRAIN: &[&str] = &[
    "M12 18V5",
    "M15 13a4.17 4.17 0 0 1-3-4 4.17 4.17 0 0 1-3 4",
    "M17.598 6.5A3 3 0 1 0 12 5a3 3 0 1 0-5.598 1.5",
    "M17.997 5.125a4 4 0 0 1 2.526 5.77",
    "M18 18a4 4 0 0 0 2-7.464",
    "M19.967 17.483A4 4 0 1 1 12 18a4 4 0 1 1-7.967-.517",
    "M6 18a4 4 0 0 1-2-7.464",
    "M6.003 5.125a4 4 0 0 0-2.526 5.77",
];

fn icon_svg(paths: &[&str], cls: &str) -> String {
    let body: String = paths
        .iter()
        .map(|p| format!(r#"<path d="{p}"/>"#))
        .collect();
    format!(
        r#"<svg class="{cls}" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{body}</svg>"#
    )
}

fn render_body(blocks: &[ShareBlock]) -> String {
    if blocks.is_empty() {
        return "<div class=\"empty\">（会话没有可展示的消息）</div>".to_string();
    }
    let mut out = String::with_capacity(8192);
    for block in blocks {
        match block {
            ShareBlock::User {
                text,
                plan_mode,
                images,
            } => {
                if *plan_mode {
                    out.push_str("<div class=\"plan-tag\">Plan 模式</div>");
                }
                out.push_str("<div class=\"msg-user-row\"><div class=\"msg-user\">");
                out.push_str("<div class=\"user-text\">");
                out.push_str(&escape_html(text));
                out.push_str("</div>");
                if !images.is_empty() {
                    out.push_str("<div class=\"attachments\">");
                    for url in images {
                        out.push_str(&format!(
                            "<img src=\"{}\" alt=\"附件图片\" loading=\"lazy\">",
                            escape_attr(url)
                        ));
                    }
                    out.push_str("</div>");
                }
                out.push_str("</div></div>");
            }
            ShareBlock::Assistant { html } => {
                out.push_str("<div class=\"msg-assistant\"><div class=\"markdown\">");
                out.push_str(html);
                out.push_str("</div></div>");
            }
            ShareBlock::Reasoning { text, think_ms } => {
                let timer = think_ms
                    .map(|v| {
                        format!(
                            "<span class=\"card-timer\">耗时 {:.1}s</span>",
                            v as f64 / 1000.0
                        )
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "<details class=\"card reasoning\"><summary>{}<span class=\"card-title\">思维过程</span>{}{}</summary>\
                     <div class=\"tool-body reasoning-body\">{}</div></details>",
                    icon_svg(ICON_BRAIN, ""),
                    timer,
                    icon_svg(&[ICON_CHEVRON], "chev"),
                    escape_html(text)
                ));
            }
            ShareBlock::ToolCall(item) => {
                out.push_str(&render_tool_card(item, false));
            }
            ShareBlock::ToolGroup(items) => {
                out.push_str(&format!(
                    "<details class=\"card toolgroup\"><summary>{}<span class=\"card-title\">执行了 {} 次工具调用</span>{}</summary>\
                     <div class=\"tool-body group-body\">",
                    icon_svg(&[ICON_CHECK], ""),
                    items.len(),
                    icon_svg(&[ICON_CHEVRON], "chev"),
                ));
                for item in items {
                    out.push_str(&render_tool_card(item, true));
                }
                out.push_str("</div></details>");
            }
            ShareBlock::Todo { items } => {
                out.push_str(&render_todo_card(items));
            }
            ShareBlock::AskUser { cancelled, pairs } => {
                out.push_str(&render_ask_card(*cancelled, pairs));
            }
            ShareBlock::Done {
                model,
                total_ms,
                status,
            } => {
                let mut segments = vec![escape_html(model)];
                if let Some(ms) = total_ms {
                    segments.push(format!("{:.1}s", *ms as f64 / 1000.0));
                }
                match *status {
                    "interrupted" => {
                        segments.push("<span class=\"st-interrupted\">已中断</span>".to_string())
                    }
                    "error" => segments.push("<span class=\"st-error\">出错</span>".to_string()),
                    _ => {}
                }
                out.push_str(&format!(
                    "<div class=\"done-row\">{}</div>",
                    segments.join(" · ")
                ));
            }
            ShareBlock::CompactDivider { count } => {
                out.push_str(&format!(
                    "<div class=\"compact-divider\"><span class=\"cline\"></span><span>已压缩 {} 条消息</span><span class=\"cline\"></span></div>",
                    count
                ));
            }
        }
    }
    out
}

/// Renders a single tool call: standalone card, or bordered row inside a merged card (in_group)
fn render_tool_card(item: &ToolCallItem, in_group: bool) -> String {
    let mut summary = String::new();
    summary.push_str(&icon_svg(&[ICON_CHECK], ""));
    summary.push_str(&format!(
        "<span class=\"tool-verb\">{}</span>",
        escape_html(&item.verb)
    ));
    if !item.object.is_empty() {
        summary.push_str(&format!(
            "<span class=\"tool-object\">{}</span>",
            escape_html(&item.object)
        ));
    }
    if let Some(suffix) = &item.suffix {
        summary.push_str(&format!(
            "<span class=\"tool-suffix\">{}</span>",
            escape_html(suffix)
        ));
    }
    summary.push_str(&icon_svg(&[ICON_CHEVRON], "chev"));

    let mut body = String::new();
    if !item.args.is_empty() {
        body.push_str(&format!(
            "<pre class=\"tool-pre\">{}</pre>",
            escape_html(&item.args)
        ));
    }
    match &item.result {
        Some(result) => {
            body.push_str("<p class=\"result-label\">结果：</p>");
            body.push_str(&format!(
                "<pre class=\"tool-pre\">{}</pre>",
                escape_html(result)
            ));
        }
        None => {
            body.push_str("<p class=\"result-label\">结果：</p>");
            body.push_str("<p class=\"tool-noresult\">（无结果记录：调用被中断或数据悬挂）</p>");
        }
    }

    if in_group {
        format!(
            "<details class=\"tool-row\"><summary>{summary}</summary><div class=\"tool-body\">{body}</div></details>"
        )
    } else {
        format!(
            "<details class=\"card toolcall\"><summary>{summary}</summary><div class=\"tool-body\">{body}</div></details>"
        )
    }
}

/// Todo card (mirrors web TodoView: • Updated todo N/M + checkbox list).
/// The export is a static history snapshot; todos have no runtime state and
/// the header is always "Updated todo".
fn render_todo_card(items: &[TodoItem]) -> String {
    let done = items
        .iter()
        .filter(|t| t.status == "completed" || t.status == "cancelled")
        .count();
    let all_done = !items.is_empty() && done == items.len();
    let mut out = format!(
        "<div class=\"card\"><div class=\"card-head\"><span class=\"todo-icon {}\">•</span>\
         <span class=\"card-title\">Updated todo</span><span class=\"todo-count\">{done}/{}</span></div>\
         <div class=\"todo-items\">",
        if all_done { "done" } else { "doing" },
        items.len(),
    );
    for item in items {
        let (icon, icon_cls, text_cls) = match item.status.as_str() {
            "completed" => ("✓", "done", "done"),
            "in_progress" => ("→", "doing", "doing"),
            "cancelled" => ("✗", "rest", "cancelled"),
            _ => ("○", "rest", "pending"),
        };
        out.push_str(&format!(
            "<div class=\"todo-row\"><span class=\"todo-icon {icon_cls}\">{icon}</span>\
             <span class=\"todo-text {text_cls}\">{}</span></div>",
            escape_html(&item.content)
        ));
    }
    out.push_str("</div></div>");
    out
}

/// question Q&A card (mirrors web AskUserView: cancelled greyed out / Q&A list)
fn render_ask_card(cancelled: bool, pairs: &[(String, String)]) -> String {
    let mut out = format!(
        "<div class=\"card\"><div class=\"card-head\">{}<span class=\"card-title\">已提问</span></div>",
        icon_svg(&[ICON_CHECK], "")
    );
    if cancelled {
        out.push_str("<div class=\"ask-cancelled\">cancelled</div>");
    } else if pairs.is_empty() {
        out.push_str("<div class=\"ask-cancelled\">Unanswered</div>");
    } else {
        out.push_str("<div class=\"ask-body\">");
        for (q, a) in pairs {
            out.push_str(&format!(
                "<div class=\"ask-row\"><span class=\"ask-q\">{}</span><span class=\"ask-sep\"> → </span><span class=\"ask-a\">{}</span></div>",
                escape_html(q),
                escape_html(a)
            ));
        }
        out.push_str("</div>");
    }
    out.push_str("</div>");
    out
}

// ── Markdown → HTML (XSS safe) ──────────────────────────────

/// Renders markdown into an HTML fragment. Raw HTML events
/// (`Event::Html`/`InlineHtml`) are downgraded to plain text: push_html escapes
/// them when emitting Text, so injected content only shows up as source.
/// Code block content is not entity-preprocessed and displays verbatim.
fn markdown_to_html(text: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(text, options).map(|ev| match ev {
        Event::Html(h) => Event::Text(h),
        Event::InlineHtml(h) => Event::Text(h),
        other => other,
    });
    let mut out = String::with_capacity(text.len() * 2);
    pulldown_cmark::html::push_html(&mut out, parser);
    decorate_code_blocks(&out)
}

/// Adds a language header to tagged code blocks (mirrors web CodeHeader:
/// rounded-t-xl language bar + rounded-b-xl code body). pulldown output looks
/// like `<pre><code class="language-ts">…</code></pre>` and is wrapped into
/// the codeblock structure here.
fn decorate_code_blocks(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + 256);
    let mut rest = html;
    const OPEN_LANG: &str = "<pre><code class=\"language-";
    const OPEN_PLAIN: &str = "<pre><code>";
    const CLOSE: &str = "</code></pre>";
    loop {
        let lang_pos = rest.find(OPEN_LANG);
        let plain_pos = rest.find(OPEN_PLAIN);
        let (pos, is_lang) = match (lang_pos, plain_pos) {
            (None, None) => {
                out.push_str(rest);
                return out;
            }
            (Some(a), Some(b)) => {
                if a <= b {
                    (a, true)
                } else {
                    (b, false)
                }
            }
            (Some(a), None) => (a, true),
            (None, Some(b)) => (b, false),
        };
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        let body_start = if is_lang {
            let prefix_end = after.find("\">").map(|p| p + 2);
            match prefix_end {
                Some(p) if after.starts_with(OPEN_LANG) => p,
                _ => {
                    out.push_str(after);
                    return out;
                }
            }
        } else {
            OPEN_PLAIN.len()
        };
        let Some(close_rel) = after.find(CLOSE) else {
            out.push_str(after);
            return out;
        };
        let close_end = close_rel + CLOSE.len();
        if is_lang {
            // Language name = escaped text between `language-` and the
            // closing quote of the class attribute
            let lang = &after[OPEN_LANG.len()..body_start - 2];
            out.push_str(&format!(
                        "<div class=\"codeblock\"><div class=\"code-header\"><span class=\"code-lang\">{lang}</span></div>{}&nbsp;</div>",
                        &after[..close_end]
                    ));
        } else {
            out.push_str(&format!(
                "<div class=\"codeblock plain\">{}</div>",
                &after[..close_end]
            ));
        }
        rest = &after[close_end..];
    }
}

/// Escapes HTML text nodes.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Placeholder neutralization: `{{`/`}}` inside inserted values are
/// entity-encoded into literal braces (lossless rendering), preventing
/// content like `{{BODY}}` from being re-expanded as template placeholders
/// by later replace calls.
fn neutralize_placeholders(s: String) -> String {
    s.replace("{{", "&#123;&#123;")
        .replace("}}", "&#125;&#125;")
}

/// Escapes HTML attribute values (currently same as escape_html; named separately for intent).
fn escape_attr(s: &str) -> String {
    escape_html(s)
}

// ── Message parsing helpers ──────────────────────────────────

fn runtime_meta_value(msg: &StoredMessage) -> Option<Value> {
    msg.runtime_meta
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
}

fn is_tool_attachment(msg: &StoredMessage) -> bool {
    runtime_meta_value(msg)
        .as_ref()
        .and_then(|v| v.get("format"))
        .and_then(|f| f.as_str())
        == Some("tool-attachment")
}

/// Parses image attachments of a user message: only image/* data URLs are
/// accepted (prevents non-image MIME from emitting odd content via img tags);
/// parse failures are silently ignored.
fn parse_image_attachments(msg: &StoredMessage) -> Vec<String> {
    let Some(json) = msg.attachments.as_deref() else {
        return Vec::new();
    };
    let Ok(arr) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    arr.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let mime = item.get("mime").and_then(|v| v.as_str())?;
                    let url = item.get("data_url").and_then(|v| v.as_str())?;
                    (mime.starts_with("image/") && url.starts_with("data:image/"))
                        .then(|| url.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Strips the Windows verbatim prefix (same semantics as web handlers::strip_verbatim)
fn strip_verbatim(path: &str) -> String {
    path.strip_prefix(r"\\?\")
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Display name for a file: inside the work dir → relative path; outside → full path.
/// Mirrors web tool-fallback.tsx::displayPath (`\`→`/` normalization +
/// case-insensitive prefix comparison)
fn display_path(path: &str, work_dir: Option<&str>) -> String {
    let Some(wd) = work_dir else {
        return path.to_string();
    };
    let norm = |p: &str| p.replace('\\', "/");
    let wd = norm(wd);
    let wd = wd.trim_end_matches('/');
    if wd.is_empty() {
        return path.to_string();
    }
    let p = norm(path);
    // The slice boundary must land exactly on the separator after wd: case
    // conversion of some characters changes the UTF-8 byte length (e.g. U+212A
    // KELVIN SIGN), so slicing at byte length based only on a to_lowercase
    // prefix hit may cut mid-character (panic) or misalign (wrong relative path).
    if let Some(tail) = p.get(wd.len()..)
        && tail.starts_with('/')
        && p.to_lowercase()
            .starts_with(&format!("{}/", wd.to_lowercase()))
    {
        let rel = tail.trim_start_matches('/');
        if rel.is_empty() {
            "./".to_string()
        } else {
            rel.to_string()
        }
    } else {
        path.to_string()
    }
}

// ── Tool call parsing (mirrors web ToolFallback title field priority) ──

/// Builds the display block by tool name: todo_write/question get dedicated cards, everything else uses the generic tool card
fn build_tool_block(
    name: &str,
    arguments: &str,
    result: Option<String>,
    work_dir: Option<&str>,
) -> ShareBlock {
    let args: Value = serde_json::from_str(arguments).unwrap_or_default();

    // todo_write: checkbox-style list (raw args/result not shown, mirrors web TodoView)
    if name == "todo_write" {
        let items = args["todos"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|t| TodoItem {
                        content: t["content"].as_str().unwrap_or("—").to_string(),
                        status: t["status"].as_str().unwrap_or("pending").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        return ShareBlock::Todo { items };
    }

    // question: Q&A pairing (mirrors web AskUserView + TUI parse_ask_user_pairs)
    if name == "question" {
        let cancelled = result
            .as_deref()
            .is_some_and(|r| r.contains("[User Cancelled]"));
        let pairs = result
            .as_deref()
            .map(parse_ask_user_pairs)
            .unwrap_or_default();
        return ShareBlock::AskUser { cancelled, pairs };
    }

    let (verb, object, suffix) = tool_call_title(name, &args, work_dir);
    ShareBlock::ToolCall(ToolCallItem {
        verb,
        object,
        suffix,
        args: arguments.to_string(),
        result,
    })
}

/// Title triple (verb/object/scope), field priority mirrors web ToolFallback:
/// file tools show the relative path; grep/glob show pattern + scope; skill
/// shows "Load skill {name}"; shell/web_fetch/task take an explicit-field
/// summary; unknown tools (incl. MCP) show "使用工具 {tool_name}"
fn tool_call_title(
    name: &str,
    args: &Value,
    work_dir: Option<&str>,
) -> (String, String, Option<String>) {
    match name {
        "edit" | "write" | "read" => {
            let path = args["file_path"].as_str().unwrap_or("");
            let mut object = if path.is_empty() {
                String::new()
            } else {
                display_path(path, work_dir)
            };
            if name == "read" {
                match (args["offset"].as_u64(), args["limit"].as_u64()) {
                    (Some(o), Some(l)) => object = format!("{object} (L{}-L{})", o, o + l - 1),
                    (Some(o), None) => object = format!("{object} (from L{o})"),
                    _ => {}
                }
            }
            (name.to_string(), object, None)
        }
        "skill" => (
            "Load skill".to_string(),
            args["name"].as_str().unwrap_or("").to_string(),
            None,
        ),
        "grep" | "glob" => {
            let pattern = args["pattern"].as_str().unwrap_or("").to_string();
            let scope = match args["path"].as_str() {
                Some(p) if !p.is_empty() && p != "." => Some(p.to_string()),
                _ => args["include"].as_str().map(|s| s.to_string()),
            };
            (name.to_string(), pattern, scope)
        }
        "shell" => (
            name.to_string(),
            truncate_str(args["command_string"].as_str().unwrap_or(""), 80),
            None,
        ),
        "web_fetch" => (
            name.to_string(),
            args["url"].as_str().unwrap_or("").to_string(),
            None,
        ),
        "task" => {
            let count = args["tasks"].as_array().map(|a| a.len()).unwrap_or(0);
            let object = match count {
                0 => String::new(),
                1 => args["tasks"][0]["subagent"]
                    .as_str()
                    .unwrap_or("subagent")
                    .to_string(),
                n => format!("{n} agents"),
            };
            (name.to_string(), object, None)
        }
        _ => ("使用工具".to_string(), name.to_string(), None),
    }
}

/// question result parsing: "question"="answer" pairs (quote scanning with
/// `\\`/`\"` unescaping), mirroring TUI parse_ask_user_pairs and web
/// parseAskUserPairs
fn parse_ask_user_pairs(result: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = result.chars().collect();
    let mut pairs = Vec::new();
    let mut i = 0usize;

    // Reads one quoted string; returns (value, next index after consumption)
    fn read_quoted(chars: &[char], start: usize) -> Option<(String, usize)> {
        if chars.get(start) != Some(&'"') {
            return None;
        }
        let mut val = String::new();
        let mut j = start + 1;
        while j < chars.len() {
            match chars[j] {
                '\\' => {
                    let next = chars.get(j + 1);
                    if next == Some(&'\\') || next == Some(&'"') {
                        val.push(*next.unwrap());
                        j += 2;
                        continue;
                    }
                    val.push('\\');
                    j += 1;
                }
                '"' => {
                    return Some((val, j + 1));
                }
                c => {
                    val.push(c);
                    j += 1;
                }
            }
        }
        None
    }

    while i < chars.len() {
        if chars[i] == '"' {
            let Some((key, next)) = read_quoted(&chars, i) else {
                break;
            };
            i = next;
            // Skip whitespace looking for =
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            if chars.get(i) != Some(&'=') {
                continue;
            }
            i += 1;
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            if let Some((val, next)) = read_quoted(&chars, i) {
                pairs.push((key, val));
                i = next;
            }
        } else {
            i += 1;
        }
    }
    pairs
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

// ── Display formatting ───────────────────────────────────────

fn fmt_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn fmt_tokens_line(prompt: i64, completion: i64, cached: i64) -> String {
    if prompt == 0 && completion == 0 {
        return String::new();
    }
    let total = fmt_tokens(prompt + completion);
    if cached > 0 {
        format!("{} tokens（含缓存 {}）", total, fmt_tokens(cached))
    } else {
        format!("{total} tokens")
    }
}

/// Session title → filename-safe slug: Windows-illegal and control characters
/// are replaced with `-`, whitespace collapses, truncated to 30 chars
/// (character-level, CJK friendly), leading/trailing `-`/`.`/whitespace trimmed.
pub fn slugify_title(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut prev_dash = true; // suppress a leading hyphen
    for ch in title.chars() {
        let c = if matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            || ch.is_control()
            || ch.is_whitespace()
        {
            '-'
        } else {
            ch
        };
        if c == '-' {
            if !prev_dash {
                slug.push('-');
            }
            prev_dash = true;
        } else {
            slug.push(c);
            prev_dash = false;
        }
        if slug.chars().count() >= 30 {
            break;
        }
    }
    let slug = slug.trim_end_matches(['-', '.', ' ']).to_string();
    if slug.is_empty() {
        "session".to_string()
    } else {
        slug
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: MessageRole, content: &str) -> StoredMessage {
        StoredMessage {
            role,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            prompt_tokens: None,
            completion_tokens: None,
            cached_tokens: None,
            model: None,
            runtime_meta: None,
            think_ms: None,
            compacted: false,
            attachments: None,
            mentions: None,
        }
    }

    fn assistant_with_calls(calls_json: &str) -> StoredMessage {
        let mut m = msg(MessageRole::Assistant, "");
        m.tool_calls = Some(calls_json.to_string());
        m
    }

    fn tool_result(id: &str, content: &str) -> StoredMessage {
        let mut m = msg(MessageRole::Tool, content);
        m.tool_call_id = Some(id.to_string());
        m
    }

    // ── XSS protection ──────────────────────────────────────────

    #[test]
    fn markdown_raw_html_is_neutralized() {
        let html = markdown_to_html("<script>alert(1)</script>");
        assert!(!html.contains("<script>"), "raw script tag leaked: {html}");
        assert!(html.contains("&lt;script&gt;"), "expected escaped: {html}");
    }

    #[test]
    fn markdown_inline_html_is_neutralized() {
        let html = markdown_to_html("hello <img src=x onerror=alert(1)> world");
        assert!(!html.contains("<img"), "raw img leaked: {html}");
        assert!(html.contains("&lt;img"), "expected escaped: {html}");
    }

    #[test]
    fn markdown_structure_survives_sanitizing() {
        let html = markdown_to_html("# 标题\n\n- a\n- b\n\n`code`");
        assert!(html.contains("<h1>标题</h1>"), "{html}");
        assert!(html.contains("<li>a</li>"), "{html}");
        assert!(html.contains("<code>code</code>"), "{html}");
    }

    #[test]
    fn code_block_html_shown_verbatim() {
        let html = markdown_to_html("```\n<div>hi</div>\n```");
        assert!(html.contains("&lt;div&gt;"), "{html}");
        assert!(!html.contains("<div>"), "{html}");
    }

    #[test]
    fn escape_html_covers_all_metachars() {
        assert_eq!(
            escape_html(r#"a & <b> "c" 'd'"#),
            "a &amp; &lt;b&gt; &quot;c&quot; &#39;d&#39;"
        );
    }

    // ── Code block language header ──────────────────────────────

    #[test]
    fn fenced_code_block_gets_language_header() {
        let html = markdown_to_html("```ts\nlet a = 1;\n```");
        assert!(html.contains(r#"code-lang">ts</span>"#), "{html}");
        assert!(
            html.contains(r#"codeblock"><div class="code-header""#),
            "{html}"
        );
        assert!(html.contains("let a = 1;"), "{html}");
        assert!(html.trim_end().ends_with("</div>"), "{html}");
    }

    #[test]
    fn plain_code_block_wrapped_without_header() {
        let html = markdown_to_html("```\nraw\n```");
        assert!(html.contains(r#"codeblock plain"><pre><code>"#), "{html}");
        assert!(!html.contains("code-header"), "{html}");
    }

    #[test]
    fn inline_code_not_wrapped_as_block() {
        let html = markdown_to_html("use `ls -la` here");
        assert!(!html.contains("codeblock"), "{html}");
        assert!(html.contains("<code>ls -la</code>"), "{html}");
    }

    // ── Message traversal and pairing ───────────────────────────

    #[test]
    fn tool_calls_pair_with_results() {
        let calls = r#"[{"id":"call_1","function":{"name":"read","arguments":"{\"file_path\":\"src/main.rs\"}"}}]"#;
        let messages = vec![
            msg(MessageRole::User, "看看这个文件"),
            assistant_with_calls(calls),
            tool_result("call_1", "1: fn main() {}"),
        ];
        let blocks = build_blocks(&messages, false, None);
        let Some(ShareBlock::ToolCall(item)) =
            blocks.iter().find(|b| matches!(b, ShareBlock::ToolCall(_)))
        else {
            panic!("expected tool call block, got {blocks:?}");
        };
        assert_eq!(item.verb, "read");
        assert_eq!(item.object, "src/main.rs");
        assert_eq!(item.result.as_deref(), Some("1: fn main() {}"));
    }

    /// Dangling tool_calls in compacted ranges are never repaired (a
    /// deliberate narrowing of active-range repair); the export must tolerate
    /// failed pairing: the block renders as usual with result = None.
    #[test]
    fn dangling_tool_call_renders_without_result() {
        let calls = r#"[{"id":"call_x","function":{"name":"shell","arguments":"{\"command_string\":\"cargo build\"}"}}]"#;
        let messages = vec![
            msg(MessageRole::User, "build"),
            assistant_with_calls(calls), // no matching tool result
            msg(MessageRole::User, "继续"),
        ];
        let blocks = build_blocks(&messages, false, None);
        let Some(ShareBlock::ToolCall(item)) =
            blocks.iter().find(|b| matches!(b, ShareBlock::ToolCall(_)))
        else {
            panic!("expected tool call block");
        };
        assert!(
            item.result.is_none(),
            "dangling call must not fake a result"
        );
    }

    #[test]
    fn empty_id_tool_call_is_skipped() {
        let calls = r#"[{"id":"","function":{"name":"shell","arguments":"{}"}}]"#;
        let messages = vec![assistant_with_calls(calls)];
        let blocks = build_blocks(&messages, false, None);
        assert!(
            !blocks.iter().any(|b| matches!(b, ShareBlock::ToolCall(_))),
            "empty-id call should be skipped: {blocks:?}"
        );
    }

    #[test]
    fn tool_attachment_user_message_skipped() {
        let mut m = msg(MessageRole::User, "[截图]");
        m.runtime_meta = Some(r#"{"format":"tool-attachment"}"#.to_string());
        let messages = vec![msg(MessageRole::User, "你好"), m];
        let blocks = build_blocks(&messages, false, None);
        assert_eq!(
            blocks.len(),
            1,
            "synthetic message must be skipped: {blocks:?}"
        );
    }

    #[test]
    fn plan_mode_badge_parsed() {
        let mut m = msg(MessageRole::User, "分析一下");
        m.runtime_meta = Some(r#"{"plan_mode":true}"#.to_string());
        let blocks = build_blocks(&[m], false, None);
        let Some(ShareBlock::User { plan_mode, .. }) = blocks.first() else {
            panic!("expected user block");
        };
        assert!(*plan_mode);
    }

    #[test]
    fn image_attachments_embedded_with_mime_gate() {
        let mut m = msg(MessageRole::User, "看图");
        m.attachments = Some(
            r#"[
            {"mime":"image/png","data_url":"data:image/png;base64,AAAA"},
            {"mime":"text/html","data_url":"data:text/html;base64,PHNjcmlwdD4="}
        ]"#
            .to_string(),
        );
        let blocks = build_blocks(&[m], false, None);
        let Some(ShareBlock::User { images, .. }) = blocks.first() else {
            panic!("expected user block");
        };
        assert_eq!(images.len(), 1, "non-image mime must be filtered");
        assert_eq!(images[0], "data:image/png;base64,AAAA");
    }

    #[test]
    fn compact_divider_inserted_at_boundary() {
        let mut a = msg(MessageRole::User, "旧问题");
        a.compacted = true;
        let mut b = msg(MessageRole::Assistant, "新回答");
        b.compacted = false;
        let messages = vec![a, b];
        let blocks = build_blocks(&messages, true, None);
        assert!(
            matches!(blocks[1], ShareBlock::CompactDivider { count: 1 }),
            "divider expected between compacted/active boundary: {blocks:?}"
        );
    }

    #[test]
    fn compact_divider_appended_when_all_compacted() {
        let mut a = msg(MessageRole::User, "旧问题");
        a.compacted = true;
        let blocks = build_blocks(&[a], true, None);
        let Some(ShareBlock::CompactDivider { count: 1 }) = blocks.last() else {
            panic!("tail divider expected: {blocks:?}");
        };
    }

    #[test]
    fn reasoning_and_done_rendered() {
        let mut m = msg(MessageRole::Assistant, "答案");
        m.reasoning_content = Some("思考中...".to_string());
        m.think_ms = Some(3200);
        m.prompt_tokens = Some(1200);
        m.completion_tokens = Some(300);
        m.runtime_meta = Some(
            r#"{"total_ms":4300,"model":"deepseek/deepseek-chat","status":"completed"}"#
                .to_string(),
        );
        let blocks = build_blocks(&[m], false, None);
        assert!(matches!(blocks[0], ShareBlock::Reasoning { .. }));
        assert!(matches!(blocks[1], ShareBlock::Assistant { .. }));
        let Some(ShareBlock::Done { status, .. }) = blocks.get(2) else {
            panic!("expected done block: {blocks:?}");
        };
        assert_eq!(*status, "completed");
    }

    // ── Tool-specific cards ─────────────────────────────────────

    #[test]
    fn todo_write_renders_todo_card() {
        let calls = r#"[{"id":"c1","function":{"name":"todo_write","arguments":"{\"todos\":[{\"content\":\"修 bug\",\"status\":\"completed\"},{\"content\":\"跑测试\",\"status\":\"in_progress\"}]}"}}]"#;
        let messages = vec![assistant_with_calls(calls), tool_result("c1", "ok")];
        let blocks = build_blocks(&messages, false, None);
        let Some(ShareBlock::Todo { items }) =
            blocks.iter().find(|b| matches!(b, ShareBlock::Todo { .. }))
        else {
            panic!("expected todo block: {blocks:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].status, "completed");
        assert!(render_body(&blocks).contains("Updated todo"));
    }

    #[test]
    fn question_renders_pairs_and_cancelled() {
        // Mirrors the escaping form of TUI ask_escape: "question"="answer"
        let calls = r#"[{"id":"c1","function":{"name":"question","arguments":"{\"questions\":[{\"question\":\"继续吗\"}]}"}}]"#;
        let messages = vec![
            assistant_with_calls(calls),
            tool_result("c1", r#""继续吗"="继续""#),
        ];
        let blocks = build_blocks(&messages, false, None);
        let Some(ShareBlock::AskUser { cancelled, pairs }) = blocks
            .iter()
            .find(|b| matches!(b, ShareBlock::AskUser { .. }))
        else {
            panic!("expected ask block: {blocks:?}");
        };
        assert!(!*cancelled);
        assert_eq!(pairs, &[("继续吗".to_string(), "继续".to_string())]);

        let cancelled_blocks = build_blocks(
            &[
                assistant_with_calls(calls),
                tool_result("c1", "[User Cancelled]"),
            ],
            false,
            None,
        );
        let Some(ShareBlock::AskUser { cancelled, .. }) = cancelled_blocks
            .iter()
            .find(|b| matches!(b, ShareBlock::AskUser { .. }))
        else {
            panic!("expected ask block");
        };
        assert!(*cancelled);
    }

    #[test]
    fn consecutive_tool_calls_merge_into_group() {
        let calls = r#"[
            {"id":"c1","function":{"name":"read","arguments":"{\"file_path\":\"a.rs\"}"}},
            {"id":"c2","function":{"name":"grep","arguments":"{\"pattern\":\"TODO\",\"path\":\"src\"}"}},
            {"id":"c3","function":{"name":"read","arguments":"{\"file_path\":\"b.rs\"}"}}
        ]"#;
        let messages = vec![
            assistant_with_calls(calls),
            tool_result("c1", "ok"),
            tool_result("c2", "match"),
            tool_result("c3", "ok"),
        ];
        let blocks = build_blocks(&messages, false, Some("D:/proj"));
        let group = blocks.iter().find_map(|b| match b {
            ShareBlock::ToolGroup(items) => Some(items),
            _ => None,
        });
        let items = group.expect("expected merged group");
        assert_eq!(items.len(), 3);
        // grep scope goes into the suffix (mirrors web)
        assert_eq!(items[1].suffix.as_deref(), Some("src"));

        // A single call is not merged
        let single = build_blocks(
            &[assistant_with_calls(
                r#"[{"id":"c1","function":{"name":"read","arguments":"{}"}}]"#,
            )],
            false,
            None,
        );
        assert!(matches!(single[0], ShareBlock::ToolCall(_)));
    }

    // ── Path relativization and escaping ────────────────────────

    #[test]
    fn display_path_relativizes_workdir() {
        assert_eq!(
            display_path(r"D:\proj\src\a.rs", Some(r"D:\proj")),
            "src/a.rs"
        );
        assert_eq!(
            display_path(r"E:\else\b.rs", Some(r"D:\proj")),
            r"E:\else\b.rs"
        );
        assert_eq!(display_path("d:/proj/c.rs", Some(r"D:\proj")), "c.rs");
        assert_eq!(display_path("x.rs", None), "x.rs");
    }

    /// U+212A (KELVIN SIGN, 3 bytes) lowercases to the 1-byte 'k': when the
    /// lowercase forms of wd and the path match but byte lengths differ,
    /// byte-offset slicing must neither panic nor cut at the wrong offset.
    #[test]
    fn display_path_unicode_case_length_mismatch_is_safe() {
        let wd = "D:/2\u{212A}";
        let p = "d:/2k/é.rs";
        assert_eq!(display_path(p, Some(wd)), p);
    }

    #[test]
    fn tool_title_unknown_tool_uses_prefix() {
        let args = serde_json::from_str::<Value>("{}").unwrap();
        let (verb, object, _) = tool_call_title("mcp_foo_bar", &args, None);
        assert_eq!(verb, "使用工具");
        assert_eq!(object, "mcp_foo_bar");
    }

    // ── End-to-end: full HTML document ──────────────────────────

    #[tokio::test]
    async fn export_session_html_end_to_end() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage
            .create_session("deepseek/deepseek-chat", "D:\\tmp")
            .await
            .unwrap();
        storage
            .append_message(&sid, &msg(MessageRole::User, "<b>注入测试</b> & 修复"))
            .await
            .unwrap();
        storage
            .append_message(
                &sid,
                &msg(MessageRole::Assistant, "# 分析\n\n**结论**：可行 `x < y`"),
            )
            .await
            .unwrap();
        storage
            .update_session_title(&sid, "修复/报告:最终")
            .await
            .unwrap();

        let export = export_session_html(&storage, &sid).await.unwrap();
        assert_eq!(export.slug, "修复-报告-最终");
        assert!(
            export
                .html
                .contains("&lt;b&gt;注入测试&lt;/b&gt; &amp; 修复")
        );
        assert!(export.html.contains("<h1>分析</h1>"));
        assert!(
            export.html.contains("修复/报告:最终"),
            "title should appear in header"
        );
        assert!(
            !export.html.contains("{{TITLE}}") && !export.html.contains("{{META}}"),
            "placeholders must all be filled"
        );
    }

    #[tokio::test]
    async fn export_missing_session_errors() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        assert!(export_session_html(&storage, "nope").await.is_err());
    }

    /// Literal placeholders such as `{{BODY}}`/`{{EXPORTED}}` in titles/bodies
    /// must not be re-expanded by template replacement; they are entity-encoded
    /// and rendered as literal braces.
    #[tokio::test]
    async fn placeholder_text_in_content_not_expanded() {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let sid = storage.create_session("m", "D:\\t").await.unwrap();
        storage
            .append_message(&sid, &msg(MessageRole::User, "正文 {{BODY}} 占位"))
            .await
            .unwrap();
        storage
            .update_session_title(&sid, "标题{{EXPORTED}}测试")
            .await
            .unwrap();

        let export = export_session_html(&storage, &sid).await.unwrap();
        assert!(
            export.html.contains("&#123;&#123;BODY&#125;&#125;"),
            "body placeholder must be entity-escaped: {}",
            export.html
        );
        assert!(
            export.html.contains("&#123;&#123;EXPORTED&#125;&#125;"),
            "title placeholder must be entity-escaped"
        );
        assert!(!export.html.contains("{{BODY}}"));
        assert!(!export.html.contains("{{EXPORTED}}"));
    }

    #[test]
    fn slugify_title_edge_cases() {
        assert_eq!(slugify_title(""), "session");
        assert_eq!(slugify_title("///"), "session");
        assert_eq!(slugify_title("a  b"), "a-b");
        assert_eq!(slugify_title("trailing..."), "trailing");
        let long: String = "字".repeat(50);
        assert_eq!(slugify_title(&long).chars().count(), 30);
    }

    #[test]
    fn fmt_tokens_units() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(12_400), "12.4k");
        assert_eq!(fmt_tokens(3_200_000), "3.2M");
    }
}
