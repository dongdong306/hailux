use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::agent::subagent::SharedConfig;
use crate::config::ModelEntry;
use crate::mcp::SharedMcpBackends;
use crate::storage::SessionSummary;
use crate::tui::event::{self, TaskStatus};
use crate::tui::setup::SetupForm;
use crate::tui::tasks_viewer::TaskEntry;

#[derive(Debug, Clone)]
pub enum Message {
    User {
        text: String,
        plan_mode: bool,
        mentions: Vec<std::ops::Range<usize>>,
        /// Optimistic interjection bubble: rendered on delivery, carries the
        /// client_id until the `CoreEvent::UserMessage` echo. While set, the
        /// previous turn's output inserts before this bubble (see
        /// `turn_output_insert_index`); stripped on echo / turn end
        pending_interject: Option<String>,
    },
    Agent(String),
    AgentStreaming(String),
    AgentThinking {
        text: String,
        think_ms: Option<u64>,
        thinking_started_at: Option<Instant>,
    },
    AgentDone {
        total_ms: u64,
        model: String,
        status: TaskStatus,
    },
    ToolCall {
        name: String,
        arguments: String,
    },
    ToolResult {
        name: String,
        result: String,
        display: Option<String>,
    },
    SubagentStep {
        summary: String,
        is_done: bool,
        agent: Option<String>,
        index: Option<usize>,
    },
    CompactMarker {
        compacted_count: usize,
        total_ms: Option<u64>,
    },
    CompactStreaming(String),
}

pub(super) enum PickerAction {
    None,
    Close,
    Switch(String),
    NewSession,
}

pub(super) enum ModelPickerAction {
    Close,
    Switch(ModelEntry),
    AddModel,
    Refresh(String),
}

pub(crate) enum AppState {
    Chat,
    SessionPicker {
        sessions: Vec<SessionSummary>,
        selected_index: usize,
        search_query: String,
        filtered_indices: Vec<usize>,
    },
    ModelPicker {
        models: Vec<ModelEntry>,
        selected_index: usize,
        refreshing: Option<String>,
        refresh_note: Option<(bool, String)>,
    },
    EffortPicker {
        selected_index: usize,
    },
    Setup(SetupForm),
    Skills {
        selected_index: usize,
    },
    SkillDetail {
        skill_index: usize,
        scroll_offset: usize,
    },
    Mcp {
        selected_index: usize,
    },
    McpDetail {
        server_index: usize,
        selected_index: usize,
    },
    McpItemDetail {
        server_index: usize,
        item_index: usize,
        scroll_offset: usize,
    },
    Tasks {
        selected_index: usize,
        entries: Vec<TaskEntry>,
    },
    QueueManager {
        selected_index: usize,
    },
    Stats {
        data: crate::tui::stats_viewer::StatsData,
        selected_index: usize,
        work_dir: Option<String>,
        projects: Vec<crate::storage::WorkDirInfo>,
        picker_index: Option<usize>,
    },
    TaskDetail {
        task_index: usize,
        scroll_offset: usize,
        messages: Vec<crate::storage::StoredMessage>,
        entries: Vec<TaskEntry>,
    },
    AskUser {
        questions: Vec<event::QuestionInfo>,
        response_tx: tokio::sync::oneshot::Sender<String>,
        current_tab: usize,
        selected: usize,
        answers: Vec<Option<String>>,
        custom_inputs: Vec<String>,
        custom_cursor: usize,
        editing_custom: bool,
        last_paste: Option<Instant>,
    },
    Permission {
        pending: Vec<PermissionPending>,
        selected: usize,
    },
}

/// Permission requests queued for the user's reply.
/// Requests from the main agent and subagents converge into one queue, grouped by
/// `subagent_name` (None = main agent, Some(name) = the corresponding subagent).
pub(crate) struct PermissionPending {
    pub request: crate::permission::PermissionRequest,
    pub response_tx: tokio::sync::oneshot::Sender<crate::permission::PermissionReply>,
    pub subagent_name: Option<String>,
}

pub(crate) struct QueuedItem {
    pub client_id: String,
    pub text: String,
    pub images: Vec<(String, crate::agent::media::Attachment)>,
    pub mention_images: Vec<crate::agent::media::Attachment>,
    pub mention_display: Vec<(String, String)>,
    /// Delivery in progress (accepted by the agent, waiting for the
    /// `CoreEvent::UserMessage` echo / `InterjectDropped` release); blocks
    /// double-interject, edit and delete
    pub in_flight: bool,
}

impl QueuedItem {
    pub(crate) fn all_attachments(&self) -> Vec<crate::agent::media::Attachment> {
        self.images
            .iter()
            .map(|(_, a)| a.clone())
            .chain(self.mention_images.iter().cloned())
            .collect()
    }
}

pub(crate) fn locate_mentions(text: &str, values: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut ordered: Vec<&String> = values.iter().collect();
    ordered.sort_by_key(|v| std::cmp::Reverse(v.len()));
    let mut hits: Vec<std::ops::Range<usize>> = Vec::new();
    for value in ordered {
        if value.is_empty() {
            continue;
        }
        let token = format!("@{value}");
        let mut from = 0;
        while let Some(pos) = text[from..].find(&token) {
            let start = from + pos;
            let end = start + token.len();
            from = end;
            if hits.iter().any(|h| start < h.end && end > h.start) {
                continue;
            }
            hits.push(start..end);
        }
    }
    hits.sort_by_key(|h| h.start);
    hits
}

pub(crate) fn rewrite_mentions(
    text: &str,
    mentions: &[(String, String)],
) -> (String, Vec<std::ops::Range<usize>>) {
    let values: Vec<String> = mentions.iter().map(|(v, _)| v.clone()).collect();
    let displays: Vec<&str> = mentions.iter().map(|(_, d)| d.as_str()).collect();
    let hits = locate_mentions(text, &values);
    let mut out = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut last = 0;
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&text[last..h.start]);
        let span_start = out.len();
        out.push('@');
        out.push_str(displays[i]);
        spans.push(span_start..out.len());
        last = h.end;
    }
    out.push_str(&text[last..]);
    (out, spans)
}

pub(crate) fn byte_range_to_char_range(
    text: &str,
    range: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    let start = text[..range.start].chars().count();
    let len = text[range.clone()].chars().count();
    start..start + len
}

pub(crate) fn char_range_to_byte_range(
    text: &str,
    start_cp: usize,
    end_cp: usize,
) -> Option<std::ops::Range<usize>> {
    let mut boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    boundaries.push(text.len());
    let start = boundaries.get(start_cp)?;
    let end = boundaries.get(end_cp)?;
    if start <= end {
        Some(*start..*end)
    } else {
        None
    }
}

pub struct AppSharedState {
    pub current_session: Arc<Mutex<Option<String>>>,
    pub mcp_backends: SharedMcpBackends,
    pub config: SharedConfig,
}

#[cfg(test)]
mod tests {
    use super::rewrite_mentions;

    #[test]
    fn queued_item_all_attachments_merges_placeholder_and_inline() {
        use super::QueuedItem;
        use crate::agent::media::Attachment;
        let placeholder_img = Attachment::from_bytes("image/png", &[0x89, b'P']);
        let inline_img = Attachment::from_bytes("image/png", b"NG");
        let item = QueuedItem {
            client_id: "c1".to_string(),
            text: "看图 [Image 1] 与 @D:/p/a.png".to_string(),
            images: vec![("[Image 1] ".to_string(), placeholder_img)],
            mention_images: vec![inline_img],
            mention_display: Vec::new(),
            in_flight: false,
        };
        let all = item.all_attachments();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].data_url, item.images[0].1.data_url);
        assert_eq!(all[1].data_url, item.mention_images[0].data_url);
    }

    #[test]
    fn rewrite_mentions_expanded_to_display() {
        let input = "看下 @D:\\proj\\hailux/src/main.rs 和 @D:\\proj\\hailux/README.md 谢谢";
        let pairs = vec![
            (
                "D:\\proj\\hailux/src/main.rs".to_string(),
                "src/main.rs".to_string(),
            ),
            (
                "D:\\proj\\hailux/README.md".to_string(),
                "README.md".to_string(),
            ),
        ];
        let (out, spans) = rewrite_mentions(input, &pairs);
        assert_eq!(out, "看下 @src/main.rs 和 @README.md 谢谢");
        assert_eq!(spans.len(), 2);
        assert_eq!(&out[spans[0].clone()], "@src/main.rs");
        assert_eq!(&out[spans[1].clone()], "@README.md");
    }

    #[test]
    fn rewrite_mentions_prefix_overlap_and_missing() {
        let input = "@D:/p/src/a/b/c.rs 与 @D:/p/src/a.rs 以及手动输入的 @别的";
        let pairs = vec![
            ("D:/p/src/a/b/c.rs".to_string(), "a/b/c.rs".to_string()),
            ("D:/p/src/a.rs".to_string(), "a.rs".to_string()),
            ("D:/p/src/a".to_string(), "a".to_string()),
        ];
        let (out, spans) = rewrite_mentions(input, &pairs);
        assert_eq!(out, "@a/b/c.rs 与 @a.rs 以及手动输入的 @别的");
        assert_eq!(spans.len(), 2);
        assert_eq!(&out[spans[0].clone()], "@a/b/c.rs");
        assert_eq!(&out[spans[1].clone()], "@a.rs");
    }

    #[test]
    fn rewrite_mentions_empty() {
        let (out, spans) = rewrite_mentions("@D:/x/a.rs", &[]);
        assert_eq!(out, "@D:/x/a.rs");
        assert!(spans.is_empty());
    }
}
