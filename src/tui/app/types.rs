use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::agent::subagent::SharedConfig;
use crate::config::ModelEntry;
use crate::mcp::SharedMcpBackends;
use crate::storage::SessionSummary;
use crate::tui::event::{self, TaskStatus};
use crate::tui::model_picker::AddModelForm;
use crate::tui::setup::SetupForm;
use crate::tui::tasks_viewer::TaskEntry;

#[derive(Debug, Clone)]
pub enum Message {
    User {
        text: String,
        plan_mode: bool,
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
    /// subagent 执行过程中的一步（工具调用+结果）
    SubagentStep {
        #[allow(dead_code)]
        name: String,
        summary: String,
        is_done: bool,
        /// 关联的 task 工具调用 ID（用于 messages_to_cells 分组）
        #[allow(dead_code)]
        task_call_id: Option<u64>,
    },
    /// 上下文压缩分隔标记（UI 可见，不进入 LLM 上下文）
    #[allow(dead_code)]
    CompactMarker {
        summary: String,
        compacted_count: usize,
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
    },
    AddModel(AddModelForm),
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
}

/// 共享状态（在 App、TaskTool、subagent 之间传递）
pub struct AppSharedState {
    pub current_session: Arc<Mutex<Option<String>>>,
    pub mcp_backends: SharedMcpBackends,
    pub config: SharedConfig,
}
