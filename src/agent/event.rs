//! Core event contract from the agent to its consumers (TUI / Web).
//!
//! `AskUser` / `PermissionRequest` carry a `oneshot::Sender` with blocking
//! semantics: UI-less consumers (non-interactive mode, web disconnect
//! fallback) must eventually reply, or the agent task hangs forever.

use crate::agent::models::{ReasoningEffort, SharedMessage};
use crate::permission::{PermissionReply, PermissionRequest};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy)]
pub struct MessageUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cached_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuestionInfo {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    Completed,
    Interrupted,
    Error,
}

#[derive(Debug, Clone, Copy)]
pub struct CompactUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Mention metadata attached to a user message: `start`/`end` are Unicode
/// code point offsets into the content (token includes the `@` prefix).
/// Core-level mirror of the web protocol's `ChatMention`, keeping agent
/// consumers free of web protocol types.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mention {
    pub start: usize,
    pub end: usize,
    pub kind: String,
}

/// A mid-run user message (web "interjection"): appended at an agent loop
/// boundary without interrupting the current stream; one arriving during the
/// final answer stream opens another request round within the same run.
#[derive(Debug, Clone)]
pub struct Interjection {
    /// Echoed back so the frontend can drop the matching queued item
    pub client_id: String,
    pub content: String,
    pub attachments: Vec<crate::agent::media::Attachment>,
    pub mentions: Vec<Mention>,
}

/// Max pending interjections per run. The channel is bounded so a runaway
/// client cannot buffer unbounded memory mid-run; `deliver_interjection`
/// rejects beyond this and the caller falls back (409 / keep queued).
pub const INTERJECT_QUEUE_LIMIT: usize = 10;

/// Shared handle to the active run's interjection channel. The channel is
/// swapped per run; a stale sender fails once its run task drops the
/// receiver. Shared so delivery skips the session lock the runner holds for
/// the whole run.
pub type InterjectSlot =
    std::sync::Arc<std::sync::Mutex<Option<tokio::sync::mpsc::Sender<Interjection>>>>;

/// Deliver through an `InterjectSlot` without holding any session lock.
/// False = no instance / no active run / run cancelling / channel full.
pub fn deliver_interjection(slot: &InterjectSlot, interjection: Interjection) -> bool {
    let Ok(guard) = slot.lock() else {
        return false;
    };
    match guard.as_ref() {
        Some(tx) => tx.try_send(interjection).is_ok(),
        None => false,
    }
}

#[derive(Debug)]
pub enum CoreEvent {
    AgentChunk(String),
    AgentReasoningChunk(String),
    AgentComplete {
        messages: Vec<SharedMessage>,
        usages: Vec<MessageUsage>,
        /// Snapshot at chat_stream start, not the live config
        model: String,
        reasoning_effort: ReasoningEffort,
        status: TaskStatus,
    },
    UsageUpdate {
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: u32,
    },
    /// An interjected user message entered the context at a loop boundary.
    /// Consumers: persist + echo (Web runner); TUI settles the optimistic
    /// bubble and persists via `handle_interjected_user_message`.
    UserMessage {
        /// The interjected message as accepted; `client_id` is the echo key
        /// the frontend resolves against its queue / optimistic bubble
        interjection: Interjection,
    },
    /// An accepted interjection was discarded without entering the context:
    /// the run was interrupted / errored / completed before the injection
    /// boundary (or in the narrow teardown window after the last drain).
    /// Never persisted. Consumers re-queue the item so the user can resend.
    InterjectDropped {
        client_id: String,
    },
    PersistMessage {
        msg: SharedMessage,
        usage: Option<MessageUsage>,
        model: String,
        display: Option<String>,
    },
    /// A transient `<system-reminder>` was appended to the current turn's
    /// user message in memory (previous-run failure notice). Consumers:
    /// persist into that session's last user row's
    /// `runtime_meta.system_reminder` array so a rebuild reproduces the exact
    /// request bytes (prompt-cache stable). `session_id` anchors the target
    /// row at injection time — consumers must use it, not their ambient
    /// "current session" (a mid-queue session switch would otherwise misfile
    /// the row). Content itself is never polluted — UI reads `content` only.
    PersistUserReminder {
        session_id: String,
        reminder: String,
    },
    ToolCallStart {
        name: String,
        arguments: String,
        subagent_name: Option<String>,
        /// Index into the tasks array; distinguishes same-named concurrent subagents
        subagent_index: Option<usize>,
    },
    ToolResult {
        name: String,
        result: String,
        display: Option<String>,
        subagent_name: Option<String>,
        subagent_index: Option<usize>,
    },
    AskUser {
        questions: Vec<QuestionInfo>,
        response_tx: oneshot::Sender<String>,
    },
    PermissionRequest {
        request: PermissionRequest,
        response_tx: oneshot::Sender<PermissionReply>,
        subagent_name: Option<String>,
    },
    /// UI semantics: drop the trailing streamed content (partial output of a
    /// failed attempt never enters context) and show a retry notice.
    AgentRetry {
        attempt: u32,
        max_retries: u32,
        delay_ms: u64,
        reason: String,
    },
    CompactChunk(String),
    CompactComplete {
        summary: String,
        session_id: String,
        usage: Option<CompactUsage>,
    },
    CompactError(String),
}

pub type CoreEventTx = mpsc::Sender<CoreEvent>;
pub type CoreEventRx = mpsc::Receiver<CoreEvent>;

pub fn create_core_event_channel() -> (CoreEventTx, CoreEventRx) {
    mpsc::channel(4096)
}

/// Swappable event-channel handle for tools that cannot know their consumer
/// at construction time (question, subagent forwarding). TUI binds once per
/// process; the web binds per `/api/chat` request. `send` returns false when
/// unbound/closed — callers handle the fallback (question errors out,
/// forwarding drops silently).
#[derive(Clone, Default)]
pub struct EventHub {
    tx: std::sync::Arc<std::sync::RwLock<Option<CoreEventTx>>>,
}

impl EventHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, tx: CoreEventTx) {
        if let Ok(mut guard) = self.tx.write() {
            *guard = Some(tx);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.tx.write() {
            *guard = None;
        }
    }

    pub fn send(&self, event: CoreEvent) -> bool {
        if let Ok(guard) = self.tx.read()
            && let Some(tx) = guard.as_ref()
        {
            return tx.try_send(event).is_ok();
        }
        false
    }

    pub fn is_bound(&self) -> bool {
        self.tx.read().map(|g| g.is_some()).unwrap_or(false)
    }
}
