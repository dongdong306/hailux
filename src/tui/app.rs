use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::CellDiffOption;
use ratatui::prelude::*;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::ask_user::{self, AskUserState};
use super::chat_widget::{ChatWidget, RenderCache};
use super::command;
use super::event::{self, AppEvent, EventTx, TaskStatus};
use super::history_cell::{self, HistoryCell, SessionHeaderCell, TooltipCell};
use super::input::ElementKind;
use super::input::InputHandler;
use super::mcp_viewer::{self, render_mcp_viewer};
use super::model_picker::{AddModelForm, AddModelStep, render_add_model, render_model_picker};
use super::session_picker::SessionPicker;
use super::setup::{SetupForm, SetupStep, render_setup};
use super::skills_viewer::{self, render_skills_viewer};
use super::tasks_viewer::{self, TaskEntry, TaskRecord, TaskRunStatus, TasksViewer};
use super::terminal;
use crate::agent::Agent;
use crate::agent::CommandRegistry;
use crate::agent::Tool;
use crate::agent::command_def::INIT_COMMAND_NAME;
use crate::agent::skill::SkillInfo;
use crate::agent::subagent::{self, SharedConfig, SubagentConfig, TaskTool};
use crate::config::{self, ModelEntry};
use crate::mcp::{McpConnection, McpServerStatus, McpToolBackend, SharedMcpBackends};
use crate::storage::{ChatStorage, MessageRole, SessionSummary, StoredMessage};

const PASTE_BURST_MIN_CHARS: u16 = 3;
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);
#[cfg(not(windows))]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(8);
#[cfg(windows)]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(60);
const LARGE_PASTE_CHAR_THRESHOLD: usize = 200;

/// 批量消费积压事件的时间预算，超时后立即渲染，避免高速输出时 UI 卡顿
const BATCH_RENDER_BUDGET: Duration = Duration::from_millis(50);
/// 单次批量消费的事件上限，防止极端积压下长时间占用
const BATCH_MAX_EVENTS: usize = 128;
const DEFAULT_CONTEXT_WINDOW: u32 = 131072;
const DEFAULT_OUTPUT_TOKENS: u32 = 65536;

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

struct PasteBurst {
    last_plain_char_time: Option<Instant>,
    consecutive_plain_char_burst: u16,
    burst_window_until: Option<Instant>,
    buffer: String,
    active: bool,
    pending_first_char: Option<(char, Instant)>,
}

impl PasteBurst {
    fn new() -> Self {
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

    fn on_plain_char_no_hold(&mut self, now: Instant) -> Option<CharDecision> {
        self.note_plain_char(now);
        if self.active {
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return Some(CharDecision::BufferAppend);
        }
        if self.consecutive_plain_char_burst >= PASTE_BURST_MIN_CHARS {
            return Some(CharDecision::BeginBuffer {
                retro_chars: self.consecutive_plain_char_burst.saturating_sub(1),
            });
        }
        None
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

    fn try_append_char_if_active(&mut self, ch: char, now: Instant) -> bool {
        if self.active || !self.buffer.is_empty() {
            self.append_char_to_buffer(ch, now);
            true
        } else {
            false
        }
    }

    fn decide_begin_buffer(
        &mut self,
        now: Instant,
        before: &str,
        retro_chars: usize,
    ) -> Option<(usize, String)> {
        let start_byte = retro_start_index(before, retro_chars);
        let grabbed = before[start_byte..].to_string();
        let looks_pastey =
            grabbed.chars().any(char::is_whitespace) || grabbed.chars().count() >= 16;
        if looks_pastey {
            self.begin_with_retro_grabbed(grabbed.clone(), now);
            Some((start_byte, grabbed))
        } else {
            None
        }
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

    fn is_active(&self) -> bool {
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

    fn flush_timeout(&self) -> Option<Duration> {
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

enum PickerAction {
    None,
    Close,
    Switch(String),
    NewSession,
}

enum ModelPickerAction {
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

pub struct App {
    messages: Vec<Message>,
    input: InputHandler,
    scroll_offset: u16,
    should_auto_scroll: bool,
    is_processing: bool,
    plan_mode: bool,
    should_quit: bool,
    agent: Agent,
    event_tx: EventTx,
    event_rx: event::EventRx,
    model_name: String,
    state: AppState,
    storage: ChatStorage,
    current_session_id: Option<String>,
    work_dir: String,
    command_registry: CommandRegistry,
    command_entries: Vec<command::CommandEntry>,
    command_suggestions: Vec<command::CommandEntry>,
    selected_suggestion: usize,
    show_suggestions: bool,
    paste_burst: PasteBurst,
    last_esc_time: Option<Instant>,
    esc_hint_active: bool,
    pending_pastes: Vec<(String, String)>,
    config: config::Config,
    skills: Vec<SkillInfo>,
    home_dir: std::path::PathBuf,
    mcp_servers: Vec<McpServerStatus>,
    context_prompt_tokens: u32,
    context_completion_tokens: u32,
    max_context_tokens: u32,
    spinner_frame: usize,
    last_spinner_tick: Option<Instant>,
    file_picker_active: bool,
    file_picker_results: Vec<String>,
    file_picker_selected: usize,
    pending_file_mentions: Vec<(String, String)>,
    subagents: Vec<SubagentConfig>,
    task_records: Vec<TaskRecord>,
    task_call_counter: u64,
    active_task_call_id: Option<u64>,
    current_session_shared: Arc<Mutex<Option<String>>>,
    mcp_backends: SharedMcpBackends,
    shared_config: SharedConfig,
    resolved_config: async_openai::config::OpenAIConfig,
    resolved_model: String,
    resolved_max_tokens: u32,
    thinking_collapsed: bool,
    user_msg_sent_at: Option<Instant>,
    last_total_ms: Option<u64>,
    render_cache: RenderCache,
    cached_cells: Vec<Box<dyn HistoryCell>>,
    cells_dirty: bool,
    force_clear: bool,
    is_jediterm: bool,
}

#[allow(clippy::too_many_arguments)]
impl App {
    pub fn new(
        agent: Agent,
        model_name: String,
        max_context_tokens: u32,
        storage: ChatStorage,
        config: config::Config,
        skills: Vec<SkillInfo>,
        command_registry: CommandRegistry,
        mcp_servers: Vec<McpServerStatus>,
        event_tx: EventTx,
        event_rx: event::EventRx,
        subagents: Vec<SubagentConfig>,
        resolved_config: async_openai::config::OpenAIConfig,
        resolved_model: String,
        resolved_max_tokens: u32,
        current_session_shared: Arc<Mutex<Option<String>>>,
        mcp_backends: SharedMcpBackends,
        shared_config: SharedConfig,
    ) -> Self {
        let work_dir = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let home_dir = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let command_entries = command::build_all_entries(&command_registry);
        Self {
            messages: Vec::new(),
            input: InputHandler::new(),
            scroll_offset: 0,
            should_auto_scroll: true,
            is_processing: false,
            plan_mode: false,
            should_quit: false,
            agent,
            event_tx,
            event_rx,
            model_name,
            state: AppState::Chat,
            storage,
            current_session_id: None,
            work_dir,
            command_registry,
            command_entries,
            command_suggestions: Vec::new(),
            selected_suggestion: 0,
            show_suggestions: false,
            paste_burst: PasteBurst::new(),
            last_esc_time: None,
            esc_hint_active: false,
            pending_pastes: Vec::new(),
            config,
            skills,
            home_dir,
            mcp_servers,
            context_prompt_tokens: 0,
            context_completion_tokens: 0,
            max_context_tokens,
            spinner_frame: 0,
            last_spinner_tick: None,
            file_picker_active: false,
            file_picker_results: Vec::new(),
            file_picker_selected: 0,
            pending_file_mentions: Vec::new(),
            subagents,
            task_records: Vec::new(),
            task_call_counter: 0,
            active_task_call_id: None,
            current_session_shared,
            mcp_backends,
            shared_config,
            resolved_config,
            resolved_model,
            resolved_max_tokens,
            thinking_collapsed: true,
            user_msg_sent_at: None,
            last_total_ms: None,
            render_cache: RenderCache::new(),
            cached_cells: Vec::new(),
            cells_dirty: true,
            force_clear: false,
            is_jediterm: std::env::var("TERMINAL_EMULATOR")
                .map(|v| v.contains("JediTerm"))
                .unwrap_or(false),
        }
    }

    pub fn enter_setup(&mut self) {
        self.state = AppState::Setup(SetupForm::new());
    }

    pub async fn load_session_messages(&mut self) -> Result<()> {
        let Some(session_id) = &self.current_session_id else {
            return Ok(());
        };

        let compact_summary = self.storage.get_compact_summary(session_id).await?;
        let stored = self.storage.load_messages(session_id).await?;
        let active_stored = self.storage.load_active_messages(session_id).await?;

        let mut chat_messages = Vec::new();
        let mut display_messages = Vec::new();

        let mut tool_results: std::collections::HashMap<String, (String, Option<String>)> =
            std::collections::HashMap::new();
        for msg in &stored {
            if msg.role == MessageRole::Tool
                && let Some(id) = msg.tool_call_id.as_deref()
            {
                tool_results
                    .entry(id.to_string())
                    .or_insert((msg.content.clone(), msg.runtime_meta.clone()));
            }
        }

        let mut compact_marker_inserted = false;
        for (idx, msg) in stored.iter().enumerate() {
            if !compact_marker_inserted
                && compact_summary.is_some()
                && idx > 0
                && stored[idx - 1].compacted
                && !msg.compacted
            {
                display_messages.push(Message::CompactMarker {
                    summary: compact_summary.clone().unwrap(),
                    compacted_count: idx,
                });
                compact_marker_inserted = true;
            }

            match msg.role {
                MessageRole::User => {
                    let plan_mode = msg
                        .runtime_meta
                        .as_deref()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                        .and_then(|v| v.get("plan_mode").and_then(|v| v.as_bool()))
                        .unwrap_or(false);
                    display_messages.push(Message::User {
                        text: msg.content.clone(),
                        plan_mode,
                    });
                }
                MessageRole::Assistant => {
                    if let Some(reasoning) = msg.reasoning_content.as_ref()
                        && !reasoning.trim().is_empty()
                    {
                        display_messages.push(Message::AgentThinking {
                            text: reasoning.clone(),
                            think_ms: msg.think_ms.map(|v| v as u64),
                            thinking_started_at: None,
                        });
                    }
                    if !msg.content.is_empty() {
                        display_messages.push(Message::Agent(msg.content.clone()));
                    }
                    if let Some(tc_json) = msg.tool_calls.as_deref()
                        && let Ok(value) = serde_json::from_str::<serde_json::Value>(tc_json)
                        && let Some(arr) = value.as_array()
                    {
                        for tc in arr {
                            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let func = tc.get("function").or_else(|| tc.get("custom_tool"));
                            let name = func
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = func
                                .and_then(|f| f.get("arguments").or_else(|| f.get("input")))
                                .map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .unwrap_or_default();
                            if !name.is_empty() {
                                display_messages.push(Message::ToolCall {
                                    name: name.clone(),
                                    arguments,
                                });
                                if let Some((result, display)) = tool_results.remove(id) {
                                    display_messages.push(Message::ToolResult {
                                        name,
                                        result,
                                        display,
                                    });
                                }
                            }
                        }
                    }
                    if let Some(meta_str) = msg.runtime_meta.as_deref()
                        && let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str)
                        && let Some(total_ms) = meta.get("total_ms").and_then(|v| v.as_u64())
                    {
                        let model = meta
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let status = meta
                            .get("status")
                            .and_then(|v| v.as_str())
                            .map(|s| match s {
                                "interrupted" => TaskStatus::Interrupted,
                                "error" => TaskStatus::Error,
                                _ => TaskStatus::Completed,
                            })
                            .unwrap_or(TaskStatus::Completed);
                        display_messages.push(Message::AgentDone {
                            total_ms,
                            model,
                            status,
                        });
                    }
                }
                MessageRole::Tool => {}
                MessageRole::System => {}
            }
        }

        if !compact_marker_inserted
            && compact_summary.is_some()
            && !stored.is_empty()
            && stored.iter().all(|m| m.compacted)
        {
            display_messages.push(Message::CompactMarker {
                summary: compact_summary.clone().unwrap(),
                compacted_count: stored.len(),
            });
        }

        for (_id, (result, display)) in tool_results {
            display_messages.push(Message::ToolResult {
                name: String::new(),
                result,
                display,
            });
        }
        self.messages = display_messages;

        for msg in &active_stored {
            if let Some(chat_msg) = crate::storage::from_stored_message(msg) {
                chat_messages.push(chat_msg);
            }
        }

        if !chat_messages.is_empty() {
            let has_system = chat_messages
                .iter()
                .any(|m| crate::storage::compatible_message_role(m) == MessageRole::System);

            if let Some(ref summary) = compact_summary {
                let summary_msg: crate::agent::models::CompatibleChatCompletionRequestMessage =
                    async_openai::types::chat::ChatCompletionRequestUserMessage {
                        content: async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(
                            format!("[Context Summary]\n{}", summary),
                        ),
                        name: None,
                    }
                    .into();

                let preserved_system = if has_system {
                    None
                } else {
                    self.agent.take_system_prompt()
                };

                let non_system: Vec<_> = chat_messages
                    .into_iter()
                    .filter(|m| crate::storage::compatible_message_role(m) != MessageRole::System)
                    .collect();

                let mut final_messages = Vec::new();
                if let Some(prompt) = &preserved_system {
                    final_messages.push(
                        async_openai::types::chat::ChatCompletionRequestSystemMessage {
                            content: async_openai::types::chat::ChatCompletionRequestSystemMessageContent::Text(
                                prompt.clone(),
                            ),
                            name: None,
                        }
                        .into(),
                    );
                }
                final_messages.push(summary_msg);
                final_messages.extend(non_system);
                self.agent.sync_messages(final_messages);
            } else {
                let preserved_system = if has_system {
                    None
                } else {
                    self.agent.take_system_prompt()
                };
                self.agent.sync_messages(chat_messages);
                if let Some(prompt) = preserved_system {
                    self.agent.set_system_prompt(&prompt);
                }
            }
        }

        self.should_auto_scroll = true;
        self.scroll_offset = 0;
        Ok(())
    }

    pub async fn run(&mut self, terminal: &mut terminal::Tui) -> Result<()> {
        let tx = self.event_tx.clone();
        let event_collector = tokio::spawn(async move {
            event::collect_terminal_events(tx).await;
        });

        terminal.draw(|f| self.render(f))?;

        let mut pending_input_events: VecDeque<AppEvent> = VecDeque::new();

        loop {
            let timeout = if self.is_processing {
                Duration::from_millis(80)
            } else {
                self.paste_burst
                    .flush_timeout()
                    .map(|d| d + Duration::from_millis(5))
                    .unwrap_or(Duration::from_secs(3600))
            };

            if let Some(event) = pending_input_events.pop_front() {
                self.process_event(event).await?;
            } else {
                match tokio::time::timeout(timeout, self.event_rx.recv()).await {
                    Ok(Some(app_event)) => {
                        self.process_event(app_event).await?;
                    }
                    Ok(None) => break,
                    Err(_) => {
                        if !self.is_processing {
                            self.handle_paste_burst_flush(Instant::now());
                        }
                    }
                }
            }

            if self.should_quit {
                break;
            }

            // 批量消费积压的流式事件（chunk/tool 等），用户交互事件暂存后逐个处理
            if self.is_processing {
                let batch_start = Instant::now();
                let mut batch_count = 0usize;
                loop {
                    if self.should_quit {
                        break;
                    }
                    if batch_count >= BATCH_MAX_EVENTS
                        || batch_start.elapsed() >= BATCH_RENDER_BUDGET
                    {
                        break;
                    }
                    match self.event_rx.try_recv() {
                        Ok(event) if Self::is_batchable_event(&event) => {
                            self.process_event(event).await?;
                            batch_count += 1;
                            if !self.is_processing {
                                break;
                            }
                        }
                        Ok(event) => {
                            pending_input_events.push_back(event);
                            break;
                        }
                        Err(_) => break,
                    }
                }
            }

            if self.is_processing {
                let now = Instant::now();
                let should_tick = self
                    .last_spinner_tick
                    .map(|t| now.duration_since(t) >= Duration::from_millis(80))
                    .unwrap_or(true);
                if should_tick {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                    self.last_spinner_tick = Some(now);
                }
                // 5 秒后自动重置 esc 提示
                if self.esc_hint_active
                    && self
                        .last_esc_time
                        .is_some_and(|t| now.duration_since(t) >= Duration::from_secs(5))
                {
                    self.last_esc_time = None;
                    self.esc_hint_active = false;
                }
            }

            if !self.paste_burst.is_active() {
                if self.force_clear {
                    terminal.clear()?;
                    self.force_clear = false;
                }
                terminal.draw(|f| self.render(f))?;
            }
        }

        event_collector.abort();

        // 如果任务仍在进行中，执行清理：标记为中断并修复 orphaned tool calls
        if self.is_processing {
            self.cleanup_on_quit().await;
        }

        Ok(())
    }

    /// 退出时清理：中断 agent，尝试等待 AgentComplete（2s 超时），
    /// 超时则手动修复存储中的 orphaned tool calls 并写入 interrupted 状态。
    ///
    /// 竞态安全：stream task 通过事件通道通信，cleanup 退出后其事件无人读取；
    /// `repair_orphaned_tool_calls` 幂等（检查 existing_ids），不会产生重复记录。
    /// 仅处理 PersistMessage 和 AgentComplete，其余 UI 事件在退出路径无需处理。
    async fn cleanup_on_quit(&mut self) {
        self.agent.interrupt();

        // 尝试等待 AgentComplete 事件（最多 2 秒）
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(100), self.event_rx.recv()).await
            {
                match &event {
                    AppEvent::AgentComplete { .. } => {
                        if let Err(e) = self.handle_event(event).await {
                            eprintln!("[warn] cleanup handle_event error: {e}");
                        }
                        return;
                    }
                    AppEvent::PersistMessage { .. } => {
                        if let Err(e) = self.handle_event(event).await {
                            eprintln!("[warn] cleanup persist error: {e}");
                        }
                    }
                    _ => {}
                }
            }
        }

        // 超时 fallback：手动修复存储
        if let Some(session_id) = &self.current_session_id {
            if let Err(e) = self.storage.repair_orphaned_tool_calls(session_id).await {
                eprintln!("[warn] repair_orphaned_tool_calls: {e}");
            }
            // 写入 interrupted runtime_meta
            let total_ms = self
                .user_msg_sent_at
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            let meta = serde_json::json!({
                "total_ms": total_ms,
                "model": self.model_name,
                "status": "interrupted",
            })
            .to_string();
            if let Err(e) = self
                .storage
                .update_last_assistant_runtime_meta(session_id, &meta)
                .await
            {
                eprintln!("[warn] cleanup runtime_meta: {e}");
            }
        }
    }

    /// 包装 handle_event，自动检测消息变更并标记 cells 缓存失效
    async fn process_event(&mut self, event: AppEvent) -> Result<()> {
        let streaming = matches!(
            &event,
            AppEvent::AgentChunk(_)
                | AppEvent::AgentReasoningChunk(_)
                | AppEvent::AgentComplete { .. }
                | AppEvent::CompactChunk(_)
                | AppEvent::CompactComplete { .. }
        );
        let is_subagent_result = matches!(
            &event,
            AppEvent::ToolResult {
                subagent_name: Some(_),
                ..
            }
        );
        let len_before = self.messages.len();
        self.handle_event(event).await?;
        if self.messages.len() != len_before || streaming || is_subagent_result {
            self.cells_dirty = true;
        }
        Ok(())
    }

    /// 判断事件是否为可批量消费的高频流式事件
    fn is_batchable_event(event: &AppEvent) -> bool {
        matches!(
            event,
            AppEvent::AgentChunk(_)
                | AppEvent::AgentReasoningChunk(_)
                | AppEvent::UsageUpdate { .. }
                | AppEvent::PersistMessage { .. }
                | AppEvent::ToolCallStart { .. }
                | AppEvent::ToolResult { .. }
                | AppEvent::AgentComplete { .. }
                | AppEvent::CompactChunk(_)
        )
    }

    async fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        // MCP 后台连接完成事件，不受当前 state 限制
        if let AppEvent::McpReady(connections) = event {
            self.handle_mcp_ready(connections).await?;
            return Ok(());
        }

        // 非 Chat 覆盖层状态下，非键盘事件仍需转发给 Chat 处理器，
        // 确保 agent 响应、工具结果等不被丢弃。
        let is_overlay = !matches!(&self.state, AppState::Chat | AppState::AskUser { .. });
        let is_local_input = matches!(
            &event,
            AppEvent::InputKey(_)
                | AppEvent::InputPaste(_)
                | AppEvent::UserSubmit(_)
                | AppEvent::Resize
                | AppEvent::ScrollUp
                | AppEvent::ScrollDown
                | AppEvent::MouseClick
        );

        if is_overlay && !is_local_input {
            self.handle_chat_event(event).await?;
            return Ok(());
        }

        match &self.state {
            AppState::Chat => {
                self.handle_chat_event(event).await?;
            }
            AppState::SessionPicker { .. } => {
                self.handle_picker_event_inner(event).await?;
            }
            AppState::ModelPicker { .. } => {
                self.handle_model_picker_event(event)?;
            }
            AppState::AddModel(_) => {
                self.handle_add_model_event(event)?;
            }
            AppState::Skills { .. } => {
                self.handle_skills_event(event)?;
            }
            AppState::SkillDetail { .. } => {
                self.handle_skill_detail_event(event)?;
            }
            AppState::Mcp { .. } => {
                self.handle_mcp_event(event)?;
            }
            AppState::McpDetail { .. } => {
                self.handle_mcp_detail_event(event)?;
            }
            AppState::McpItemDetail { .. } => {
                self.handle_mcp_item_detail_event(event)?;
            }
            AppState::Tasks { .. } => {
                self.handle_tasks_event(event).await?;
            }
            AppState::TaskDetail { .. } => {
                self.handle_task_detail_event(event).await?;
            }
            AppState::Setup(_) => {
                self.handle_setup_event(event)?;
            }
            AppState::AskUser { .. } => {
                let mut st = match std::mem::replace(&mut self.state, AppState::Chat) {
                    AppState::AskUser {
                        questions,
                        response_tx,
                        current_tab,
                        selected,
                        answers,
                        custom_inputs,
                        custom_cursor,
                        editing_custom,
                        last_paste,
                    } => AskUserState {
                        questions,
                        response_tx: Some(response_tx),
                        current_tab,
                        selected,
                        answers,
                        custom_inputs,
                        custom_cursor,
                        editing_custom,
                        last_paste,
                    },
                    other => {
                        self.state = other;
                        return Ok(());
                    }
                };
                st.handle_event(event, &mut self.state)?;
            }
        }
        Ok(())
    }

    async fn handle_chat_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::InputKey(key) => self.handle_chat_key(key).await?,
            AppEvent::InputPaste(text) => {
                self.handle_paste(text);
            }
            AppEvent::UserSubmit(input) => {
                if let Some(matched) = command::match_command(&input, &self.command_registry) {
                    self.handle_command(matched).await?;
                } else {
                    self.handle_user_message(input).await?;
                }
            }
            AppEvent::AgentChunk(chunk) => {
                self.finalize_thinking_ms();
                match self.messages.last_mut() {
                    Some(Message::AgentStreaming(text)) => {
                        text.push_str(&chunk);
                    }
                    _ => {
                        self.messages.push(Message::AgentStreaming(chunk));
                    }
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::AgentReasoningChunk(chunk) => {
                match self.messages.last_mut() {
                    Some(Message::AgentThinking { text, think_ms, .. }) if think_ms.is_none() => {
                        text.push_str(&chunk);
                    }
                    _ => {
                        self.messages.push(Message::AgentThinking {
                            text: chunk,
                            think_ms: None,
                            thinking_started_at: Some(Instant::now()),
                        });
                    }
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::AgentComplete {
                messages: final_messages,
                usages,
                status,
            } => {
                // 计算最终耗时
                self.finalize_thinking_ms();
                if let Some(sent_at) = self.user_msg_sent_at {
                    self.last_total_ms = Some(sent_at.elapsed().as_millis() as u64);
                }
                // 将耗时和模型名写入 runtime_meta（含 status）
                if let Some(session_id) = &self.current_session_id
                    && let Some(total_ms) = self.last_total_ms
                {
                    let status_str = match status {
                        TaskStatus::Completed => "completed",
                        TaskStatus::Interrupted => "interrupted",
                        TaskStatus::Error => "error",
                    };
                    let meta = serde_json::json!({
                        "total_ms": total_ms,
                        "model": self.model_name,
                        "status": status_str,
                    })
                    .to_string();
                    if let Err(e) = self
                        .storage
                        .update_last_message_runtime_meta(session_id, &meta)
                        .await
                    {
                        eprintln!("[warn] failed to persist runtime_meta: {e}");
                    }
                }
                // 重置实时时间字段（保留 final 字段用于展示）
                self.user_msg_sent_at = None;

                if let Some(Message::AgentStreaming(text)) = self.messages.last() {
                    let text = text.clone();
                    *self.messages.last_mut().unwrap() = Message::Agent(text);
                }
                // 添加耗时消息
                if let Some(total_ms) = self.last_total_ms {
                    self.messages.push(Message::AgentDone {
                        total_ms,
                        model: self.model_name.clone(),
                        status,
                    });
                }
                if !final_messages.is_empty() {
                    if let Some(session_id) = &self.current_session_id
                        && let Some(last_usage) = usages.last()
                    {
                        self.storage
                            .update_session_usage(
                                session_id,
                                last_usage.prompt_tokens as i64,
                                last_usage.completion_tokens as i64,
                            )
                            .await?;
                    }
                    self.agent.sync_messages(final_messages);
                }
                self.is_processing = false;
                self.input.set_processing(false);
                self.last_esc_time = None;
                self.esc_hint_active = false;
                self.should_auto_scroll = true;
                self.scroll_offset = 0;
                if let Some(session_id) = &self.current_session_id {
                    self.storage.touch_session(session_id).await?;
                }

                let total_context = self.context_prompt_tokens + self.context_completion_tokens;
                let threshold =
                    (self.max_context_tokens as f32 * self.config.compact_threshold) as u32;
                if total_context > threshold {
                    let pct = (self.config.compact_threshold * 100.0) as u32;
                    self.compact_conversation(Some(&format!(
                        "上下文 token 已达 {}%，自动压缩",
                        pct
                    )))
                    .await?;
                }
            }
            AppEvent::UsageUpdate {
                prompt_tokens,
                completion_tokens,
            } => {
                self.set_session_usage(prompt_tokens, completion_tokens);
            }
            AppEvent::PersistMessage {
                msg,
                usage,
                display,
            } => {
                if let Some(session_id) = &self.current_session_id {
                    let mut stored = crate::storage::to_stored_message(&msg);
                    if let Some((pt, ct)) = usage {
                        stored.prompt_tokens = Some(pt as i64);
                        stored.completion_tokens = Some(ct as i64);
                    }
                    if let Some(ref d) = display {
                        stored.runtime_meta = Some(d.clone());
                    }
                    if stored.role == MessageRole::Assistant && stored.reasoning_content.is_some() {
                        let boundary_idx = self
                            .messages
                            .iter()
                            .rposition(|m| {
                                matches!(
                                    m,
                                    Message::User { .. }
                                        | Message::ToolCall { .. }
                                        | Message::ToolResult { .. }
                                        | Message::Agent(_)
                                        | Message::AgentDone { .. }
                                )
                            })
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        let think_ms: u64 = self.messages[boundary_idx..]
                            .iter()
                            .filter_map(|m| match m {
                                Message::AgentThinking { think_ms, .. } => *think_ms,
                                _ => None,
                            })
                            .sum();
                        if think_ms > 0 {
                            stored.think_ms = Some(think_ms as i64);
                        }
                    }
                    self.storage.append_message(session_id, &stored).await?;
                }
            }
            AppEvent::ToolCallStart {
                name,
                arguments,
                subagent_name,
            } => {
                self.finalize_thinking_ms();
                if let Some(san) = subagent_name {
                    self.messages.push(Message::SubagentStep {
                        name: san.clone(),
                        summary: crate::tui::history_cell::tool_call_summary(&name, &arguments),
                        is_done: false,
                        task_call_id: self.active_task_call_id,
                    });
                } else {
                    if name == "task"
                        && let Ok(args_val) = serde_json::from_str::<serde_json::Value>(&arguments)
                    {
                        let sub_name = args_val["subagent"].as_str().unwrap_or("").to_string();
                        let desc = args_val["description"].as_str().unwrap_or("").to_string();
                        if !sub_name.is_empty() {
                            self.task_call_counter += 1;
                            let call_id = self.task_call_counter;
                            self.active_task_call_id = Some(call_id);
                            self.task_records.push(TaskRecord {
                                call_id,
                                session_id: String::new(),
                                subagent_name: sub_name,
                                description: desc,
                                started_at: std::time::Instant::now(),
                                status: TaskRunStatus::Running,
                            });
                            if self.task_records.len() > 500 {
                                self.task_records.remove(0);
                            }
                        }
                    }
                    self.messages.push(Message::ToolCall { name, arguments });
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::ToolResult {
                name,
                result,
                display,
                subagent_name,
            } => {
                if subagent_name.is_some() {
                    let tool_name = name.as_str();
                    let result_summary =
                        crate::tui::history_cell::tool_result_summary(tool_name, &result);
                    if let Some(Message::SubagentStep {
                        is_done, summary, ..
                    }) = self.messages.last_mut()
                    {
                        *is_done = true;
                        if !result_summary.is_empty() {
                            summary.push_str(&format!(" — {}", result_summary));
                        }
                    }
                } else {
                    if name == "task" {
                        if let Some(sid) = parse_task_id_from_result(&result) {
                            if let Some(call_id) = self.active_task_call_id.take()
                                && let Some(record) =
                                    self.task_records.iter_mut().find(|r| r.call_id == call_id)
                            {
                                record.session_id = sid;
                                record.status = TaskRunStatus::Completed;
                            }
                        } else if let Some(call_id) = self.active_task_call_id.take()
                            && let Some(record) =
                                self.task_records.iter_mut().find(|r| r.call_id == call_id)
                        {
                            record.status = TaskRunStatus::Error;
                        }
                    }
                    self.messages.push(Message::ToolResult {
                        name,
                        result,
                        display,
                    });
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::Resize => {}
            AppEvent::ScrollUp => {
                self.should_auto_scroll = false;
                self.scroll_offset = self.scroll_offset.saturating_add(3);
            }
            AppEvent::ScrollDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(3);
                if self.scroll_offset == 0 {
                    self.should_auto_scroll = true;
                }
            }
            AppEvent::AskUser {
                questions,
                response_tx,
            } => {
                let count = questions.len();
                self.state = AppState::AskUser {
                    questions,
                    response_tx,
                    current_tab: 0,
                    selected: 0,
                    answers: vec![None; count],
                    custom_inputs: vec![String::new(); count],
                    custom_cursor: 0,
                    editing_custom: false,
                    last_paste: None,
                };
            }
            AppEvent::McpReady(_) => {}
            AppEvent::MouseClick => {}
            AppEvent::CompactChunk(chunk) => {
                if let Some(Message::CompactStreaming(text)) = self.messages.last_mut() {
                    text.push_str(&chunk);
                }
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            AppEvent::CompactComplete {
                summary,
                session_id,
            } => {
                let compact_session = self.current_session_id.as_deref() == Some(&session_id);

                let compacted_count =
                    self.storage.count_active_messages(&session_id).await? as usize;

                self.storage.mark_messages_compacted(&session_id).await?;
                self.storage
                    .set_compact_summary(&session_id, &summary)
                    .await?;

                if compact_session {
                    self.agent.apply_compaction(&summary);

                    if let Some(Message::CompactStreaming(_)) = self.messages.last_mut() {
                        *self.messages.last_mut().unwrap() = Message::CompactMarker {
                            summary: summary.clone(),
                            compacted_count,
                        };
                    }

                    self.should_auto_scroll = true;
                    self.scroll_offset = 0;
                    self.cells_dirty = true;
                }

                self.is_processing = false;
                self.input.set_processing(false);
            }
            AppEvent::CompactError(msg) => {
                if let Some(Message::CompactStreaming(_)) = self.messages.last_mut() {
                    *self.messages.last_mut().unwrap() =
                        Message::Agent(format!("[压缩失败: {}]", msg));
                }
                self.is_processing = false;
                self.input.set_processing(false);
                self.cells_dirty = true;
            }
        }
        Ok(())
    }

    async fn handle_chat_key(&mut self, key: KeyEvent) -> Result<()> {
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
                _ => {}
            }
        }

        if self.is_processing {
            match key.code {
                KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.thinking_collapsed = !self.thinking_collapsed;
                    self.cells_dirty = true;
                }
                KeyCode::Up => {
                    self.should_auto_scroll = false;
                    self.scroll_offset = self.scroll_offset.saturating_add(3);
                }
                KeyCode::Down => {
                    self.scroll_offset = self.scroll_offset.saturating_sub(3);
                }
                KeyCode::PageUp => {
                    self.should_auto_scroll = false;
                    self.scroll_offset = self.scroll_offset.saturating_add(20);
                }
                KeyCode::PageDown => {
                    self.scroll_offset = self.scroll_offset.saturating_sub(20);
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

        if self.file_picker_active {
            match key.code {
                KeyCode::Up => {
                    if self.file_picker_selected > 0 {
                        self.file_picker_selected -= 1;
                    }
                    return Ok(());
                }
                KeyCode::Down => {
                    if self.file_picker_selected + 1 < self.file_picker_results.len() {
                        self.file_picker_selected += 1;
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
                    self.file_picker_active = false;
                    return Ok(());
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => {
                if self.show_suggestions && self.paste_burst.is_active() {
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
                } else if self.show_suggestions && !self.command_suggestions.is_empty() {
                    self.apply_suggestion();
                } else {
                    let raw = self.input.text().to_string();
                    let input = self.expand_pending_pastes(&raw);
                    let input = self.expand_file_mentions(&input);
                    if !input.trim().is_empty() {
                        self.show_suggestions = false;
                        let _ = self.event_tx.try_send(AppEvent::UserSubmit(input));
                    }
                }
            }
            KeyCode::BackTab => {
                self.toggle_plan_mode();
                return Ok(());
            }
            KeyCode::Tab => {
                if self.show_suggestions && !self.command_suggestions.is_empty() {
                    self.apply_suggestion();
                }
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.thinking_collapsed = !self.thinking_collapsed;
                self.cells_dirty = true;
            }
            KeyCode::Char(c) => {
                let has_ctrl_or_alt = key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT);
                if !has_ctrl_or_alt {
                    if !c.is_ascii() {
                        self.handle_non_ascii_char(c, now);
                    } else {
                        self.handle_ascii_char(c, now);
                    }
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
                if self.show_suggestions {
                    self.show_suggestions = false;
                } else {
                    self.input.clear();
                    self.pending_pastes.clear();
                    self.pending_file_mentions.clear();
                    self.refresh_suggestions();
                    self.file_picker_active = false;
                }
            }
            KeyCode::Up => {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.handle_paste(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                if self.show_suggestions && !self.command_suggestions.is_empty() {
                    if self.selected_suggestion > 0 {
                        self.selected_suggestion -= 1;
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
                if self.show_suggestions && !self.command_suggestions.is_empty() {
                    if self.selected_suggestion + 1 < self.command_suggestions.len() {
                        self.selected_suggestion += 1;
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

    fn handle_ascii_char(&mut self, c: char, now: Instant) {
        match self.paste_burst.on_plain_char(c, now) {
            CharDecision::RetainFirstChar => {}
            CharDecision::BeginBufferFromPending => {
                self.paste_burst.append_char_to_buffer(c, now);
            }
            CharDecision::BeginBuffer { retro_chars } => {
                let before = self.input.text_before_cursor().to_string();
                if let Some((start_byte, _)) =
                    self.paste_burst
                        .decide_begin_buffer(now, &before, retro_chars as usize)
                {
                    self.input.drain_raw(start_byte..self.input.cursor());
                    self.paste_burst.append_char_to_buffer(c, now);
                } else {
                    self.input.insert_str(&c.to_string());
                    self.refresh_suggestions();
                }
            }
            CharDecision::BufferAppend => {
                self.paste_burst.append_char_to_buffer(c, now);
            }
        }
    }

    fn handle_non_ascii_char(&mut self, ch: char, now: Instant) {
        if self.paste_burst.try_append_char_if_active(ch, now) {
            return;
        }
        if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        if let Some(decision) = self.paste_burst.on_plain_char_no_hold(now) {
            match decision {
                CharDecision::BufferAppend => {
                    self.paste_burst.append_char_to_buffer(ch, now);
                    return;
                }
                CharDecision::BeginBuffer { retro_chars } => {
                    let before = self.input.text_before_cursor().to_string();
                    if let Some((start_byte, _)) =
                        self.paste_burst
                            .decide_begin_buffer(now, &before, retro_chars as usize)
                    {
                        self.input.drain_raw(start_byte..self.input.cursor());
                        self.paste_burst.append_char_to_buffer(ch, now);
                        return;
                    }
                }
                _ => {}
            }
        }
        if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        self.input.insert_str(&ch.to_string());
        self.refresh_suggestions();
    }

    fn handle_paste_burst_flush(&mut self, now: Instant) {
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

    async fn handle_picker_event_inner(&mut self, event: AppEvent) -> Result<()> {
        let action = {
            let AppState::SessionPicker {
                sessions,
                selected_index,
                search_query,
                filtered_indices,
            } = &mut self.state
            else {
                return Ok(());
            };

            match event {
                AppEvent::InputKey(key) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        match key.code {
                            KeyCode::Char('c') => return Ok(()),
                            KeyCode::Char('n') => PickerAction::NewSession,
                            KeyCode::Char('d') => {
                                if !filtered_indices.is_empty()
                                    && *selected_index < filtered_indices.len()
                                {
                                    let idx = filtered_indices[*selected_index];
                                    if idx < sessions.len() {
                                        let session_id = sessions[idx].id.clone();
                                        self.storage.delete_session(&session_id).await?;
                                        let work_dir = Self::current_work_dir()?;
                                        *sessions = self.storage.list_sessions(&work_dir).await?;
                                        *filtered_indices =
                                            Self::filter_sessions(sessions, search_query);
                                        if *selected_index >= filtered_indices.len()
                                            && *selected_index > 0
                                        {
                                            *selected_index -= 1;
                                        }
                                    }
                                }
                                return Ok(());
                            }
                            _ => return Ok(()),
                        }
                    } else {
                        match key.code {
                            KeyCode::Esc => PickerAction::Close,
                            KeyCode::Up => {
                                if *selected_index > 0 {
                                    *selected_index -= 1;
                                }
                                PickerAction::None
                            }
                            KeyCode::Down => {
                                if *selected_index + 1 < filtered_indices.len() {
                                    *selected_index += 1;
                                }
                                PickerAction::None
                            }
                            KeyCode::Enter => {
                                if !filtered_indices.is_empty()
                                    && *selected_index < filtered_indices.len()
                                {
                                    let idx = filtered_indices[*selected_index];
                                    if idx < sessions.len() {
                                        PickerAction::Switch(sessions[idx].id.clone())
                                    } else {
                                        PickerAction::None
                                    }
                                } else {
                                    PickerAction::None
                                }
                            }
                            KeyCode::Backspace => {
                                if !search_query.is_empty() {
                                    search_query.pop();
                                    *filtered_indices =
                                        Self::filter_sessions(sessions, search_query);
                                    *selected_index = 0;
                                }
                                PickerAction::None
                            }
                            KeyCode::Char(c) => {
                                search_query.push(c);
                                *filtered_indices = Self::filter_sessions(sessions, search_query);
                                *selected_index = 0;
                                PickerAction::None
                            }
                            _ => PickerAction::None,
                        }
                    }
                }
                _ => PickerAction::None,
            }
        };

        match action {
            PickerAction::Close => {
                self.state = AppState::Chat;
            }
            PickerAction::Switch(session_id) => {
                self.switch_to_session(&session_id).await?;
            }
            PickerAction::NewSession => {
                self.create_new_session().await?;
            }
            PickerAction::None => {}
        }
        Ok(())
    }

    fn filter_sessions(sessions: &[SessionSummary], query: &str) -> Vec<usize> {
        if query.is_empty() {
            (0..sessions.len()).collect()
        } else {
            let query_lower = query.to_lowercase();
            sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    s.title.to_lowercase().contains(&query_lower)
                        || s.model.to_lowercase().contains(&query_lower)
                })
                .map(|(i, _)| i)
                .collect()
        }
    }

    fn handle_model_picker_event(&mut self, event: AppEvent) -> Result<()> {
        let action = {
            let AppState::ModelPicker {
                models,
                selected_index,
            } = &mut self.state
            else {
                return Ok(());
            };

            // 总行数 = 模型数 + 1（"添加模型..." 项）
            let total_items = models.len() + 1;

            let AppEvent::InputKey(key) = event else {
                return Ok(());
            };

            match key.code {
                KeyCode::Esc => Some(ModelPickerAction::Close),
                KeyCode::Up => {
                    if *selected_index > 0 {
                        *selected_index -= 1;
                    }
                    None
                }
                KeyCode::Down => {
                    if *selected_index + 1 < total_items {
                        *selected_index += 1;
                    }
                    None
                }
                KeyCode::Enter => {
                    if *selected_index < models.len() {
                        Some(ModelPickerAction::Switch(models[*selected_index].clone()))
                    } else {
                        Some(ModelPickerAction::AddModel)
                    }
                }
                _ => None,
            }
        };

        match action {
            Some(ModelPickerAction::Close) => {
                self.state = AppState::Chat;
            }
            Some(ModelPickerAction::Switch(entry)) => {
                if entry.needs_setup {
                    // 未配置的预定义 provider，进入 API Key 设置（不提前修改 config）
                    let mut form = SetupForm::new();
                    form.step = SetupStep::PredefinedInputApiKey;
                    form.provider_index = config::PROVIDERS
                        .iter()
                        .position(|p| p.id == entry.provider_id)
                        .unwrap_or(0);
                    form.provider_id = entry.provider_id.clone();
                    form.append_only = true;
                    self.state = AppState::Setup(form);
                } else {
                    self.switch_model(&entry)?;
                }
            }
            Some(ModelPickerAction::AddModel) => {
                let providers = self.config.configured_providers();
                self.state = AppState::AddModel(AddModelForm::new(providers));
            }
            None => {}
        }
        Ok(())
    }

    fn handle_add_model_event(&mut self, event: AppEvent) -> Result<()> {
        let key = match event {
            AppEvent::InputKey(k) => k,
            AppEvent::InputPaste(text) => {
                if let AppState::AddModel(ref mut form) = self.state
                    && !matches!(form.step, AddModelStep::SelectProvider)
                {
                    let sanitized: String =
                        text.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
                    form.buffer.insert_str(form.cursor, &sanitized);
                    form.cursor += sanitized.len();
                    form.error_msg.clear();
                }
                return Ok(());
            }
            _ => return Ok(()),
        };

        if key.code == KeyCode::Esc {
            self.open_model_picker();
            return Ok(());
        }

        let mut form = match std::mem::replace(&mut self.state, AppState::Chat) {
            AppState::AddModel(f) => f,
            other => {
                self.state = other;
                return Ok(());
            }
        };

        form.error_msg.clear();

        match form.step {
            AddModelStep::SelectProvider => {
                let total = form.provider_options.len() + 1;
                match key.code {
                    KeyCode::Up => {
                        if form.selected_index > 0 {
                            form.selected_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if form.selected_index + 1 < total {
                            form.selected_index += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if form.selected_index < form.provider_options.len() {
                            let p = &form.provider_options[form.selected_index];
                            form.provider_id = p.id.clone();
                            form.base_url = p.base_url.clone();
                            form.api_key.clear();
                            form.step = AddModelStep::InputModelName;
                            form.buffer.clear();
                        } else {
                            form.step = AddModelStep::InputProviderName;
                            form.buffer.clear();
                        }
                    }
                    _ => {}
                }
                self.state = AppState::AddModel(form);
            }
            AddModelStep::InputContextWindow => {
                match key.code {
                    KeyCode::Left => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Right => {
                        if form.cursor < form.buffer.len() {
                            form.cursor = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                        }
                    }
                    KeyCode::Home => {
                        form.cursor = 0;
                    }
                    KeyCode::End => {
                        form.cursor = form.buffer.len();
                    }
                    KeyCode::Backspace => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.buffer.drain(prev..form.cursor);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        if form.cursor < form.buffer.len() {
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.buffer.drain(form.cursor..next);
                        }
                    }
                    KeyCode::Enter => {
                        let context_window = if form.buffer.is_empty() {
                            DEFAULT_CONTEXT_WINDOW
                        } else {
                            match form.buffer.parse::<u32>() {
                                Ok(t) => t,
                                Err(_) => {
                                    form.error_msg = "请输入有效的数字".to_string();
                                    self.state = AppState::AddModel(form);
                                    return Ok(());
                                }
                            }
                        };
                        let is_new_provider = !form.api_key.is_empty();
                        self.config.add_custom_model(
                            &form.provider_id,
                            if is_new_provider {
                                Some(form.base_url.as_str())
                            } else {
                                None
                            },
                            if is_new_provider {
                                Some(form.api_key.as_str())
                            } else {
                                None
                            },
                            &form.model_name,
                            DEFAULT_OUTPUT_TOKENS,
                            context_window,
                        );
                        if let Err(e) = self.config.save() {
                            form.error_msg = format!("保存失败: {}", e);
                            self.state = AppState::AddModel(form);
                            return Ok(());
                        }
                        let display = format!("{}/{}", form.provider_id, form.model_name);
                        let resolved = self.config.resolve(&display)?;
                        self.agent.switch_model(
                            resolved.config,
                            &resolved.model_id,
                            resolved.max_tokens,
                        );
                        self.model_name = display.clone();
                        self.max_context_tokens = resolved.context_window;
                        self.config.main_model = display;
                        if let Err(e) = self.config.save() {
                            form.error_msg = format!("保存失败: {}", e);
                            self.state = AppState::AddModel(form);
                            return Ok(());
                        }
                        // 同步到共享配置
                        if let Ok(mut shared) = self.shared_config.lock() {
                            *shared = self.config.clone();
                        }
                        // 不需要恢复 AddModel state，已经切到 Chat 了
                        return Ok(());
                    }
                    KeyCode::Char(c) => {
                        form.buffer.insert(form.cursor, c);
                        form.cursor += c.len_utf8();
                    }
                    _ => {}
                }
                self.state = AppState::AddModel(form);
            }
            _ => {
                // InputProviderName, InputBaseUrl, InputApiKey, InputModelName
                match key.code {
                    KeyCode::Left => {
                        if form.cursor > 0 {
                            // 向前找上一个 char 边界
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Right => {
                        if form.cursor < form.buffer.len() {
                            form.cursor = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                        }
                    }
                    KeyCode::Home => {
                        form.cursor = 0;
                    }
                    KeyCode::End => {
                        form.cursor = form.buffer.len();
                    }
                    KeyCode::Backspace => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.buffer.drain(prev..form.cursor);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        if form.cursor < form.buffer.len() {
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.buffer.drain(form.cursor..next);
                        }
                    }
                    KeyCode::Char(c) => {
                        form.buffer.insert(form.cursor, c);
                        form.cursor += c.len_utf8();
                    }
                    KeyCode::Enter => {
                        let valid = match form.step {
                            AddModelStep::InputProviderName if form.buffer.is_empty() => {
                                form.error_msg = "服务商名称不能为空".to_string();
                                false
                            }
                            AddModelStep::InputBaseUrl if form.buffer.is_empty() => {
                                form.error_msg = "API 地址不能为空".to_string();
                                false
                            }
                            AddModelStep::InputApiKey if form.buffer.is_empty() => {
                                form.error_msg = "API Key 不能为空".to_string();
                                false
                            }
                            AddModelStep::InputModelName if form.buffer.is_empty() => {
                                form.error_msg = "模型名称不能为空".to_string();
                                false
                            }
                            _ => true,
                        };
                        if valid {
                            match form.step {
                                AddModelStep::InputProviderName => {
                                    form.provider_id = form.buffer.clone();
                                    form.step = AddModelStep::InputBaseUrl;
                                    form.buffer.clear();
                                    form.cursor = 0;
                                }
                                AddModelStep::InputBaseUrl => {
                                    form.base_url = form.buffer.clone();
                                    form.step = AddModelStep::InputApiKey;
                                    form.buffer.clear();
                                    form.cursor = 0;
                                }
                                AddModelStep::InputApiKey => {
                                    form.api_key = form.buffer.clone();
                                    form.step = AddModelStep::InputModelName;
                                    form.buffer.clear();
                                    form.cursor = 0;
                                }
                                AddModelStep::InputModelName => {
                                    form.model_name = form.buffer.clone();
                                    form.step = AddModelStep::InputContextWindow;
                                    form.buffer = "131072".to_string();
                                    form.cursor = form.buffer.len();
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                self.state = AppState::AddModel(form);
            }
        }
        Ok(())
    }

    fn handle_setup_event(&mut self, event: AppEvent) -> Result<()> {
        let key = match event {
            AppEvent::InputKey(k) => k,
            AppEvent::InputPaste(text) => {
                if let AppState::Setup(ref mut form) = self.state
                    && matches!(
                        form.step,
                        SetupStep::PredefinedInputApiKey
                            | SetupStep::CustomInputProviderName
                            | SetupStep::CustomInputBaseUrl
                            | SetupStep::CustomInputApiKey
                            | SetupStep::CustomInputModelName
                            | SetupStep::CustomInputContextWindow
                    )
                {
                    let sanitized: String =
                        text.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
                    form.buffer.insert_str(form.cursor, &sanitized);
                    form.cursor += sanitized.len();
                    form.error_msg.clear();
                }
                return Ok(());
            }
            _ => return Ok(()),
        };

        if key.code == KeyCode::Esc {
            let mut form = match std::mem::replace(&mut self.state, AppState::Chat) {
                AppState::Setup(f) => f,
                other => {
                    self.state = other;
                    return Ok(());
                }
            };
            form.error_msg.clear();
            match form.step {
                SetupStep::Welcome => {
                    self.should_quit = true;
                }
                SetupStep::SelectProvider => {
                    form.step = SetupStep::Welcome;
                    form.buffer.clear();
                    form.cursor = 0;
                }
                SetupStep::PredefinedInputApiKey => {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = form.provider_index;
                    form.buffer.clear();
                    form.cursor = 0;
                }
                SetupStep::PredefinedSelectModel => {
                    form.step = SetupStep::PredefinedInputApiKey;
                    form.buffer = form.api_key.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::CustomInputProviderName => {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = config::PROVIDERS.len();
                    form.buffer.clear();
                    form.cursor = 0;
                }
                SetupStep::CustomInputBaseUrl => {
                    form.step = SetupStep::CustomInputProviderName;
                    form.buffer = form.provider_id.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::CustomInputApiKey => {
                    form.step = SetupStep::CustomInputBaseUrl;
                    form.buffer = form.base_url.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::CustomInputModelName => {
                    form.step = SetupStep::CustomInputApiKey;
                    form.buffer = form.api_key.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::CustomInputContextWindow => {
                    form.step = SetupStep::CustomInputModelName;
                    form.buffer = form.model_id.clone();
                    form.cursor = form.buffer.len();
                }
                SetupStep::Done => {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = if form.is_custom {
                        config::PROVIDERS.len()
                    } else {
                        form.provider_index
                    };
                }
            }
            self.state = AppState::Setup(form);
            return Ok(());
        }

        let mut form = match std::mem::replace(&mut self.state, AppState::Chat) {
            AppState::Setup(f) => f,
            other => {
                self.state = other;
                return Ok(());
            }
        };

        form.error_msg.clear();

        match form.step {
            SetupStep::Welcome => {
                if key.code == KeyCode::Enter {
                    form.step = SetupStep::SelectProvider;
                    form.selected_index = 0;
                }
                self.state = AppState::Setup(form);
            }
            SetupStep::SelectProvider => {
                let total = config::PROVIDERS.len() + 1;
                match key.code {
                    KeyCode::Up => {
                        if form.selected_index > 0 {
                            form.selected_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if form.selected_index + 1 < total {
                            form.selected_index += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if form.selected_index < config::PROVIDERS.len() {
                            form.provider_index = form.selected_index;
                            form.is_custom = false;
                            form.step = SetupStep::PredefinedInputApiKey;
                        } else {
                            form.is_custom = true;
                            form.step = SetupStep::CustomInputProviderName;
                        }
                        form.buffer.clear();
                        form.cursor = 0;
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
            }
            SetupStep::PredefinedSelectModel => {
                let provider = &config::PROVIDERS[form.provider_index];
                let total = provider.models.len();
                match key.code {
                    KeyCode::Up => {
                        if form.selected_index > 0 {
                            form.selected_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if form.selected_index + 1 < total {
                            form.selected_index += 1;
                        }
                    }
                    KeyCode::Enter => {
                        let m = &provider.models[form.selected_index];
                        form.model_id = m.id.to_string();
                        form.step = SetupStep::Done;
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
            }
            SetupStep::Done => {
                if key.code == KeyCode::Enter {
                    let cfg = match form.build_config() {
                        Ok(c) => c,
                        Err(e) => {
                            form.error_msg = format!("{}", e);
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                    };
                    if form.append_only {
                        // 从模型选择器进入：追加 provider 到现有 config
                        let provider_id = cfg
                            .main_model
                            .split_once('/')
                            .map(|(p, _)| p.to_string())
                            .unwrap_or_default();
                        let model_display = cfg.main_model.clone();
                        if let Some(entry) = cfg.providers.get(&provider_id) {
                            self.config
                                .add_predefined_provider(&provider_id, &entry.api_key);
                        }
                        self.config.main_model = model_display.clone();
                        if let Err(e) = self.config.save() {
                            form.error_msg = format!("保存失败: {}", e);
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                        // 同步到共享配置
                        if let Ok(mut shared) = self.shared_config.lock() {
                            *shared = self.config.clone();
                        }
                        match self.config.resolve(&model_display) {
                            Ok(resolved) => {
                                self.agent.switch_model(
                                    resolved.config,
                                    &resolved.model_id,
                                    resolved.max_tokens,
                                );
                                self.model_name = resolved.display;
                                self.max_context_tokens = resolved.context_window;
                                self.state = AppState::Chat;
                                return Ok(());
                            }
                            Err(e) => {
                                form.error_msg = format!("解析模型失败: {}", e);
                                self.state = AppState::Setup(form);
                                return Ok(());
                            }
                        }
                    }
                    if let Err(e) = cfg.save() {
                        form.error_msg = format!("保存失败: {}", e);
                        self.state = AppState::Setup(form);
                        return Ok(());
                    }
                    match cfg.resolve_default() {
                        Ok(resolved) => {
                            self.agent.switch_model(
                                resolved.config,
                                &resolved.model_id,
                                resolved.max_tokens,
                            );
                            self.model_name = resolved.display;
                            self.max_context_tokens = resolved.context_window;
                            self.config = cfg;
                            // 同步到共享配置
                            if let Ok(mut shared) = self.shared_config.lock() {
                                *shared = self.config.clone();
                            }
                            // state 已为 Chat（mem::replace 设置），保持
                            return Ok(());
                        }
                        Err(e) => {
                            form.error_msg = format!("解析模型失败: {}", e);
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                    }
                }
                self.state = AppState::Setup(form);
            }
            _ => {
                // 文本输入步骤
                let is_context_window = form.step == SetupStep::CustomInputContextWindow;
                match key.code {
                    KeyCode::Left => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Right => {
                        if form.cursor < form.buffer.len() {
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.cursor = next;
                        }
                    }
                    KeyCode::Home => {
                        form.cursor = 0;
                    }
                    KeyCode::End => {
                        form.cursor = form.buffer.len();
                    }
                    KeyCode::Backspace => {
                        if form.cursor > 0 {
                            let prev = form.buffer[..form.cursor]
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            form.buffer.drain(prev..form.cursor);
                            form.cursor = prev;
                        }
                    }
                    KeyCode::Delete => {
                        if form.cursor < form.buffer.len() {
                            let next = form.buffer[form.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| form.cursor + i)
                                .unwrap_or(form.buffer.len());
                            form.buffer.drain(form.cursor..next);
                        }
                    }
                    KeyCode::Char(c) => {
                        form.buffer.insert(form.cursor, c);
                        form.cursor += c.len_utf8();
                    }
                    KeyCode::Enter => {
                        if form.buffer.is_empty() && !is_context_window {
                            form.error_msg = match form.step {
                                SetupStep::PredefinedInputApiKey | SetupStep::CustomInputApiKey => {
                                    "API Key 不能为空".to_string()
                                }
                                SetupStep::CustomInputProviderName => {
                                    "服务商名称不能为空".to_string()
                                }
                                SetupStep::CustomInputBaseUrl => "API 地址不能为空".to_string(),
                                SetupStep::CustomInputModelName => "模型名称不能为空".to_string(),
                                _ => String::new(),
                            };
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                        if is_context_window
                            && !form.buffer.is_empty()
                            && form.buffer.parse::<u32>().is_err()
                        {
                            form.error_msg = "请输入有效的数字".to_string();
                            self.state = AppState::Setup(form);
                            return Ok(());
                        }
                        match form.step {
                            SetupStep::PredefinedInputApiKey => {
                                form.api_key = form.buffer.clone();
                                form.step = SetupStep::PredefinedSelectModel;
                                form.selected_index = 0;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::CustomInputProviderName => {
                                form.provider_id = form.buffer.clone();
                                form.step = SetupStep::CustomInputBaseUrl;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::CustomInputBaseUrl => {
                                form.base_url = form.buffer.clone();
                                form.step = SetupStep::CustomInputApiKey;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::CustomInputApiKey => {
                                form.api_key = form.buffer.clone();
                                form.step = SetupStep::CustomInputModelName;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            SetupStep::CustomInputModelName => {
                                form.model_id = form.buffer.clone();
                                form.step = SetupStep::CustomInputContextWindow;
                                form.buffer = "131072".to_string();
                                form.cursor = form.buffer.len();
                            }
                            SetupStep::CustomInputContextWindow => {
                                form.context_window = form.buffer.clone();
                                form.step = SetupStep::Done;
                                form.buffer.clear();
                                form.cursor = 0;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                self.state = AppState::Setup(form);
            }
        }
        Ok(())
    }

    fn current_work_dir() -> Result<String> {
        Ok(std::env::current_dir()?
            .canonicalize()?
            .display()
            .to_string())
    }

    fn handle_paste(&mut self, text: String) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let char_count = text.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            let element_text = format!(" {} ", placeholder);
            self.input.insert_element(&element_text, ElementKind::Paste);
            self.pending_pastes.push((element_text, text));
        } else {
            self.input.insert_str(&text);
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

    fn expand_pending_pastes(&mut self, text: &str) -> String {
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

    fn expand_file_mentions(&mut self, text: &str) -> String {
        if self.pending_file_mentions.is_empty() {
            return text.to_string();
        }
        let mut result = text.to_string();
        for (display, abs) in &self.pending_file_mentions {
            result = result.replace(&format!(" {} ", display), abs);
            result = result.replace(display, abs);
        }
        self.pending_file_mentions.clear();
        result
    }

    fn snapshot_elements(&self) -> Vec<String> {
        if self.pending_pastes.is_empty() && self.pending_file_mentions.is_empty() {
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
            let trimmed = payload.trim();
            self.pending_file_mentions
                .retain(|(display, _)| display != trimmed);
        }
    }

    fn refresh_suggestions(&mut self) {
        if self.input.text().starts_with('/') {
            let trimmed = self.input.text().trim_start();
            let prefix = trimmed[1..].trim_start();
            let cmd_prefix = prefix.split_whitespace().next().unwrap_or(prefix);
            let indices = command::filter_completions(&self.command_entries, cmd_prefix);
            self.command_suggestions = indices
                .iter()
                .map(|&i| self.command_entries[i].clone())
                .collect();
            if self.command_suggestions.is_empty() {
                self.show_suggestions = false;
            } else {
                self.show_suggestions = true;
                self.selected_suggestion = 0;
            }
        } else {
            self.show_suggestions = false;
            self.command_suggestions.clear();
        }
    }

    fn apply_suggestion(&mut self) {
        if let Some(cmd) = self
            .command_suggestions
            .get(self.selected_suggestion)
            .cloned()
        {
            let text = format!("/{} ", cmd.name);
            self.input.set_text(text);
            self.show_suggestions = false;
            self.command_suggestions.clear();
            if cmd.is_ui {
                let _ = self
                    .event_tx
                    .try_send(AppEvent::UserSubmit(format!("/{}", cmd.name)));
            }
        }
    }

    fn collect_files(&self) -> Vec<String> {
        use ignore::WalkBuilder;
        let work_path = std::path::Path::new(&self.work_dir);
        let mut files = Vec::new();
        let walker = WalkBuilder::new(work_path)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(|entry| {
                !crate::agent::IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
            })
            .build();
        for entry in walker.flatten() {
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                && let Some(abs) = entry.path().to_str()
            {
                files.push(abs.to_string());
            }
        }
        files.sort();
        files
    }

    fn refresh_file_picker(&mut self) {
        let before_cursor = self.input.text_before_cursor();
        if before_cursor.is_empty() {
            self.file_picker_active = false;
            return;
        }

        let at_pos = before_cursor.rfind('@');
        match at_pos {
            None => {
                self.file_picker_active = false;
            }
            Some(at_idx) => {
                let before_at = &before_cursor[..at_idx];
                let is_boundary =
                    before_at.is_empty() || before_at.ends_with(|c: char| c.is_whitespace());
                if !is_boundary {
                    self.file_picker_active = false;
                    return;
                }
                let query = &before_cursor[at_idx + 1..];
                if query.contains(|c: char| c.is_whitespace()) {
                    self.file_picker_active = false;
                    return;
                }
                let files = self.collect_files();
                let q_lower = query.to_lowercase();
                let mut results: Vec<String> = files
                    .into_iter()
                    .filter(|f| {
                        if query.is_empty() {
                            return true;
                        }
                        f.to_lowercase().contains(&q_lower)
                    })
                    .collect();
                if query.is_empty() {
                    results.truncate(20);
                } else {
                    results.truncate(15);
                }
                if results.is_empty() {
                    self.file_picker_active = false;
                } else {
                    self.file_picker_active = true;
                    self.file_picker_results = results;
                    self.file_picker_selected = 0;
                }
            }
        }
    }

    fn select_file(&mut self) {
        if let Some(abs_path) = self
            .file_picker_results
            .get(self.file_picker_selected)
            .cloned()
        {
            let rel_path = std::path::Path::new(&abs_path)
                .strip_prefix(&self.work_dir)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| abs_path.clone());
            let before_cursor = self.input.text_before_cursor();
            if let Some(at_idx) = before_cursor.rfind('@') {
                self.input.drain_raw(at_idx..self.input.cursor());
                let display = format!("@{}", rel_path);
                let element_text = format!(" {} ", display);
                self.input
                    .insert_element(&element_text, ElementKind::FileMention);
                self.pending_file_mentions
                    .push((display, format!("@{}", abs_path)));
            }
        }
        self.file_picker_active = false;
        self.refresh_suggestions();
    }

    async fn handle_command(&mut self, matched: command::MatchedCommand) -> Result<()> {
        match matched {
            command::MatchedCommand::Ui(cmd) => {
                self.input.clear();
                self.show_suggestions = false;
                self.command_suggestions.clear();
                match cmd {
                    command::Command::Session => {
                        self.open_session_picker().await?;
                    }
                    command::Command::New => {
                        self.create_new_session().await?;
                    }
                    command::Command::Models => {
                        self.open_model_picker();
                    }
                    command::Command::Skills => {
                        self.open_skills_viewer();
                    }
                    command::Command::Mcp => {
                        self.open_mcp_viewer();
                    }
                    command::Command::Tasks => {
                        self.open_tasks_viewer().await?;
                    }
                    command::Command::Plan => {
                        self.toggle_plan_mode();
                    }
                    command::Command::Compact => {
                        self.compact_conversation(None).await?;
                    }
                    command::Command::Exit => {
                        self.should_quit = true;
                    }
                }
            }
            command::MatchedCommand::Prompt { name, args } => {
                if let Some(cmd) = self.command_registry.find(&name) {
                    // /init 需要写入 AGENTS.md，但规划模式会过滤 write/edit 工具，
                    // 直接执行会导致 LLM 静默失败（只能输出文本，写不了文件）。
                    if name == INIT_COMMAND_NAME && self.plan_mode {
                        self.input.clear();
                        self.show_suggestions = false;
                        self.command_suggestions.clear();
                        self.messages.push(Message::Agent(
                            "当前为只读规划模式，无法创建 AGENTS.md。请先 /plan 退出规划模式，再运行 /init。".to_string(),
                        ));
                        self.cells_dirty = true;
                        return Ok(());
                    }
                    let rendered = cmd.render(&args);
                    self.input.clear();
                    self.show_suggestions = false;
                    self.command_suggestions.clear();
                    self.handle_user_message(rendered).await?;
                }
            }
        }
        Ok(())
    }

    /// 翻转规划模式：开关 plan_mode 并同步给 agent。
    fn toggle_plan_mode(&mut self) {
        self.plan_mode = !self.plan_mode;
        self.agent.set_plan_mode(self.plan_mode);
    }

    /// 从思考阶段过渡到输出阶段时，冻结当前思考块的耗时。
    fn finalize_thinking_ms(&mut self) {
        if let Some(Message::AgentThinking {
            think_ms,
            thinking_started_at,
            ..
        }) = self.messages.last_mut()
            && think_ms.is_none()
            && let Some(t) = thinking_started_at.take()
        {
            *think_ms = Some(t.elapsed().as_millis() as u64);
        }
    }

    async fn handle_user_message(&mut self, input: String) -> Result<()> {
        self.is_processing = true;
        self.input.set_processing(true);
        self.user_msg_sent_at = Some(Instant::now());
        self.last_total_ms = None;
        self.input.submit(&input);
        self.pending_pastes.clear();
        self.pending_file_mentions.clear();
        self.should_auto_scroll = true;
        self.scroll_offset = 0;
        self.messages.push(Message::User {
            text: input.clone(),
            plan_mode: self.plan_mode,
        });

        let new_session = self.current_session_id.is_none();
        if new_session {
            let work_dir = Self::current_work_dir()?;
            let session_id = self
                .storage
                .create_session(&self.model_name, &work_dir)
                .await?;
            self.current_session_id = Some(session_id.clone());
            if let Ok(mut guard) = self.current_session_shared.lock() {
                *guard = Some(session_id);
            }
        }

        let session_id = self.current_session_id.as_deref().unwrap().to_string();
        if new_session && let Some(prompt) = self.agent.take_system_prompt() {
            let sys_stored = StoredMessage {
                role: MessageRole::System,
                content: prompt,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                prompt_tokens: None,
                completion_tokens: None,
                runtime_meta: None,
                think_ms: None,
                compacted: false,
            };
            self.storage
                .append_message(&session_id, &sys_stored)
                .await?;
        }
        let stored = StoredMessage {
            role: MessageRole::User,
            content: input.clone(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            prompt_tokens: None,
            completion_tokens: None,
            runtime_meta: if self.plan_mode {
                Some(r#"{"plan_mode":true}"#.to_string())
            } else {
                None
            },
            think_ms: None,
            compacted: false,
        };
        self.storage.append_message(&session_id, &stored).await?;

        if new_session {
            let title: String = input.chars().take(50).collect();
            self.storage
                .update_session_title(&session_id, &title)
                .await?;
        }

        // 检查是否为 @subagent: 语法
        if let Some((subagent_name, subagent_prompt)) = subagent::parse_subagent_input(&input) {
            // 查找 subagent 配置
            if self.subagents.iter().any(|s| s.name == subagent_name) {
                let task_tool = TaskTool::new(
                    self.subagents.clone(),
                    self.skills.clone(),
                    self.storage.clone(),
                    self.resolved_config.clone(),
                    self.resolved_model.clone(),
                    self.resolved_max_tokens,
                    self.work_dir.clone(),
                    self.current_session_shared.clone(),
                    self.mcp_backends.clone(),
                    self.shared_config.clone(),
                    Some(self.event_tx.clone()),
                );

                let task_description = format!(
                    "@{}: {}",
                    subagent_name,
                    subagent_prompt.chars().take(60).collect::<String>()
                );

                let args = serde_json::json!({
                    "subagent": subagent_name,
                    "description": task_description.clone(),
                    "prompt": subagent_prompt,
                })
                .to_string();

                self.messages.push(Message::ToolCall {
                    name: "task".to_string(),
                    arguments: args.clone(),
                });
                if self.should_auto_scroll {
                    self.scroll_offset = 0;
                }

                self.task_call_counter += 1;
                let call_id = self.task_call_counter;
                self.active_task_call_id = Some(call_id);
                self.task_records.push(TaskRecord {
                    call_id,
                    session_id: String::new(),
                    subagent_name: subagent_name.clone(),
                    description: task_description.clone(),
                    started_at: std::time::Instant::now(),
                    status: TaskRunStatus::Running,
                });

                let result = task_tool.execute_async(&args).await;
                let task_status = if result.is_ok() {
                    TaskRunStatus::Completed
                } else {
                    TaskRunStatus::Error
                };
                let result_text = result.unwrap_or_else(|err| err.message);

                let sub_session_id = parse_task_id_from_result(&result_text);
                self.active_task_call_id = None;
                if let Some(record) = self.task_records.iter_mut().find(|r| r.call_id == call_id) {
                    record.session_id = sub_session_id.unwrap_or_default();
                    record.status = task_status;
                }

                self.messages.push(Message::ToolResult {
                    name: "task".to_string(),
                    result: result_text.clone(),
                    display: None,
                });

                // 将 subagent 结果作为 assistant 消息持久化到主 session
                let assistant_stored = StoredMessage {
                    role: MessageRole::Assistant,
                    content: result_text,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    runtime_meta: None,
                    think_ms: None,
                    compacted: false,
                };
                self.storage
                    .append_message(&session_id, &assistant_stored)
                    .await?;

                self.is_processing = false;
                self.input.set_processing(false);
                self.should_auto_scroll = true;
                self.scroll_offset = 0;
                return Ok(());
            }
        }

        let tx = self.event_tx.clone();
        if let Err(e) = self.agent.chat_stream(&input, tx) {
            self.messages.push(Message::Agent(format!("[错误: {}]", e)));
            self.is_processing = false;
            self.input.set_processing(false);
        }
        Ok(())
    }

    async fn compact_conversation(&mut self, auto_reason: Option<&str>) -> Result<()> {
        if self.is_processing {
            return Ok(());
        }

        let non_system_count = self.agent.messages_excluding_system_count();
        // 少于 4 条非 system 消息（不足 2 轮对话）时，压缩没有意义
        if non_system_count < 4 {
            self.messages
                .push(Message::Agent("消息太少，无需压缩".to_string()));
            self.cells_dirty = true;
            return Ok(());
        }

        let session_id = match &self.current_session_id {
            Some(id) => id.clone(),
            None => {
                self.messages
                    .push(Message::Agent("无活跃会话，无法压缩".to_string()));
                self.cells_dirty = true;
                return Ok(());
            }
        };

        self.is_processing = true;
        self.input.set_processing(true);
        self.should_auto_scroll = true;
        self.scroll_offset = 0;

        let label = if let Some(reason) = auto_reason {
            format!("正在压缩上下文（{}）...\n", reason)
        } else {
            "正在压缩上下文...\n".to_string()
        };
        self.messages.push(Message::CompactStreaming(label));
        self.cells_dirty = true;

        let tx = self.event_tx.clone();
        if let Err(e) = self.agent.request_compaction(tx, session_id) {
            if let Some(Message::CompactStreaming(_)) = self.messages.last_mut() {
                *self.messages.last_mut().unwrap() = Message::Agent(format!("[压缩失败: {}]", e));
            }
            self.is_processing = false;
            self.input.set_processing(false);
        }
        Ok(())
    }

    fn open_model_picker(&mut self) {
        let models = self.config.available_models();
        if models.is_empty() {
            self.enter_setup();
            return;
        }
        // 默认选中当前模型
        let selected_index = models
            .iter()
            .position(|m| m.display == self.model_name)
            .unwrap_or(0);
        self.state = AppState::ModelPicker {
            models,
            selected_index,
        };
    }

    /// 打开 skill 查看器。始终打开（即使为空，也会显示目录说明）。
    fn open_skills_viewer(&mut self) {
        self.state = AppState::Skills { selected_index: 0 };
    }

    /// 打开 MCP 服务器面板。
    fn open_mcp_viewer(&mut self) {
        self.state = AppState::Mcp { selected_index: 0 };
    }

    /// 打开子代理任务面板。
    async fn open_tasks_viewer(&mut self) -> Result<()> {
        let entries = self.build_task_entries().await?;
        self.state = AppState::Tasks {
            selected_index: 0,
            entries,
        };
        Ok(())
    }

    /// 合并内存 task_records 与数据库 subsession 记录，构建 TaskEntry 列表。
    async fn build_task_entries(&self) -> Result<Vec<TaskEntry>> {
        let session_id = match &self.current_session_id {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };

        let subsessions = self.storage.list_subsessions(session_id).await?;

        // 以 task_records 为主，匹配 subsession
        let mut entries: Vec<TaskEntry> = Vec::new();

        for record in &self.task_records {
            let subsession = subsessions
                .iter()
                .find(|s| s.id == record.session_id)
                .cloned();
            entries.push(TaskEntry {
                record: Some(record.clone()),
                subsession,
                subagent_name: record.subagent_name.clone(),
                description: record.description.clone(),
                status: record.status,
            });
        }

        // 补充存在于数据库但不在内存 task_records 中的 subsession（如历史会话恢复后）
        for sub in &subsessions {
            let already = entries
                .iter()
                .any(|e| e.record.as_ref().is_some_and(|r| r.session_id == sub.id));
            if !already {
                // 从 subsession 的 title 解析 subagent 名称和 description
                // title 格式: "subagent_name|description" 或仅 "subagent_name"
                let (name, desc) = if sub.title.contains('|') {
                    let mut parts = sub.title.splitn(2, '|');
                    let n = parts.next().unwrap_or("subagent").to_string();
                    let d = parts.next().unwrap_or("").to_string();
                    (n, d)
                } else if !sub.title.is_empty() {
                    (sub.title.clone(), String::new())
                } else {
                    ("subagent".to_string(), String::new())
                };

                entries.push(TaskEntry {
                    record: None,
                    subsession: Some(sub.clone()),
                    subagent_name: name,
                    description: desc,
                    status: TaskRunStatus::Completed,
                });
            }
        }

        Ok(entries)
    }

    /// Tasks 面板的事件处理：↑/↓ 移动选中，Enter 查看详情，Esc 返回聊天。
    async fn handle_tasks_event(&mut self, event: AppEvent) -> Result<()> {
        let (selected_index, entries_len) = {
            let AppState::Tasks {
                selected_index,
                entries,
            } = &self.state
            else {
                return Ok(());
            };
            (*selected_index, entries.len())
        };

        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Chat;
            }
            KeyCode::Up => {
                if selected_index > 0
                    && let AppState::Tasks { selected_index, .. } = &mut self.state
                {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if entries_len > 0
                    && selected_index + 1 < entries_len
                    && let AppState::Tasks { selected_index, .. } = &mut self.state
                {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if entries_len > 0 => {
                let (entry, idx, entries) = {
                    let AppState::Tasks {
                        entries,
                        selected_index,
                    } = &self.state
                    else {
                        return Ok(());
                    };
                    (
                        entries[*selected_index].clone(),
                        *selected_index,
                        entries.clone(),
                    )
                };

                let session_id = entry
                    .subsession
                    .as_ref()
                    .map(|s| s.id.clone())
                    .or_else(|| entry.record.as_ref().map(|r| r.session_id.clone()));

                let messages = if let Some(ref sid) = session_id {
                    self.storage.load_messages(sid).await.unwrap_or_default()
                } else {
                    Vec::new()
                };

                self.state = AppState::TaskDetail {
                    task_index: idx,
                    scroll_offset: usize::MAX,
                    messages,
                    entries,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// Task 详情面板的事件处理：↑/↓/PgUp/PgDn 滚动，Esc 返回列表。
    async fn handle_task_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let task_index = {
            let AppState::TaskDetail { task_index, .. } = &self.state else {
                return Ok(());
            };
            *task_index
        };

        // 鼠标滚轮事件
        match event {
            AppEvent::ScrollUp => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_sub(3);
                }
                return Ok(());
            }
            AppEvent::ScrollDown => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_add(3);
                }
                return Ok(());
            }
            _ => {}
        }

        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => {
                let entries = self.build_task_entries().await?;
                let safe_index = task_index.min(entries.len().saturating_sub(1));
                self.state = AppState::Tasks {
                    selected_index: safe_index,
                    entries,
                };
            }
            KeyCode::Up => {
                if task_index > 0 {
                    let new_idx = task_index - 1;
                    let entries = self.build_task_entries().await?;
                    let session_id = entries.get(new_idx).and_then(|e| {
                        e.subsession
                            .as_ref()
                            .map(|s| s.id.clone())
                            .or_else(|| e.record.as_ref().map(|r| r.session_id.clone()))
                    });
                    let messages = if let Some(ref sid) = session_id {
                        self.storage.load_messages(sid).await.unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    self.state = AppState::TaskDetail {
                        task_index: new_idx,
                        scroll_offset: 0,
                        messages,
                        entries,
                    };
                }
            }
            KeyCode::Down => {
                let entries = self.build_task_entries().await?;
                let total = entries.len();
                if total > 0 && task_index + 1 < total {
                    let new_idx = task_index + 1;
                    let session_id = entries.get(new_idx).and_then(|e| {
                        e.subsession
                            .as_ref()
                            .map(|s| s.id.clone())
                            .or_else(|| e.record.as_ref().map(|r| r.session_id.clone()))
                    });
                    let messages = if let Some(ref sid) = session_id {
                        self.storage.load_messages(sid).await.unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    self.state = AppState::TaskDetail {
                        task_index: new_idx,
                        scroll_offset: 0,
                        messages,
                        entries,
                    };
                }
            }
            KeyCode::PageUp => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if let AppState::TaskDetail { scroll_offset, .. } = &mut self.state {
                    *scroll_offset = scroll_offset.saturating_add(10);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// MCP 面板的事件处理：↑/↓ 移动选中，Enter 查看详情，Esc 返回聊天。
    fn handle_mcp_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::Mcp { selected_index } = &mut self.state else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };
        let total = self.mcp_servers.len();
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Chat;
            }
            KeyCode::Up => {
                if *selected_index > 0 {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if total > 0 && *selected_index + 1 < total {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if total > 0 && self.mcp_servers[*selected_index].connected => {
                let idx = *selected_index;
                self.state = AppState::McpDetail {
                    server_index: idx,
                    selected_index: 0,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// MCP 详情面板的事件处理：↑/↓ 移动选中，Esc 返回列表。
    fn handle_mcp_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::McpDetail {
            server_index,
            selected_index,
        } = &mut self.state
        else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        let server = match self.mcp_servers.get(*server_index) {
            Some(s) => s,
            None => {
                self.state = AppState::Mcp {
                    selected_index: *server_index,
                };
                return Ok(());
            }
        };

        let tools_len = server.tools.len();
        let resources_len = server.resources.len();
        let prompts_len = server.prompts.len();
        let total = tools_len + resources_len + prompts_len;

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Mcp {
                    selected_index: *server_index,
                };
            }
            KeyCode::Up => {
                if *selected_index > 0 {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if total > 0 && *selected_index + 1 < total {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if total > 0 => {
                let si = *server_index;
                let ii = *selected_index;
                self.state = AppState::McpItemDetail {
                    server_index: si,
                    item_index: ii,
                    scroll_offset: 0,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// MCP 单项详情面板的事件处理：Esc 返回列表。
    fn handle_mcp_item_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::McpItemDetail {
            server_index,
            item_index,
            scroll_offset,
        } = &mut self.state
        else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };

        let server = match self.mcp_servers.get(*server_index) {
            Some(s) => s,
            None => {
                self.state = AppState::McpDetail {
                    server_index: *server_index,
                    selected_index: *item_index,
                };
                return Ok(());
            }
        };

        let total = server.tools.len() + server.resources.len() + server.prompts.len();

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::McpDetail {
                    server_index: *server_index,
                    selected_index: *item_index,
                };
            }
            KeyCode::Up => {
                if *item_index > 0 {
                    *item_index -= 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Down => {
                if total > 0 && *item_index + 1 < total {
                    *item_index += 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::PageUp => {
                *scroll_offset = scroll_offset.saturating_sub(10);
            }
            KeyCode::PageDown => {
                *scroll_offset = scroll_offset.saturating_add(10);
            }
            _ => {}
        }
        Ok(())
    }

    /// 处理后台 MCP 连接完成事件：更新 UI 状态，注册工具给 agent。
    async fn handle_mcp_ready(&mut self, connections: Vec<McpConnection>) -> Result<()> {
        self.mcp_servers = connections.iter().map(|c| c.status.clone()).collect();
        let mut backends = self
            .mcp_backends
            .lock()
            .map_err(|e| color_eyre::eyre::eyre!("{e}"))?;
        backends.clear();
        for conn in &connections {
            if let Some(backend) = &conn.backend {
                let server_name = conn.status.name.clone();
                let backend_arc = backend.clone();
                let tools = conn.tools.clone();
                backends.push(McpToolBackend {
                    server_name,
                    backend: backend_arc,
                    tools,
                });
                for tool in &conn.tools {
                    self.agent.register_tool(Box::new(crate::mcp::McpTool::new(
                        &conn.status.name,
                        tool,
                        backend.clone(),
                    )));
                }
            }
        }
        Ok(())
    }

    /// skill 查看器的事件处理：↑/↓ 移动选中，Esc 返回聊天。
    fn handle_skills_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::Skills { selected_index } = &mut self.state else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };
        let total = self.skills.len();
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Chat;
            }
            KeyCode::Up => {
                if *selected_index > 0 {
                    *selected_index -= 1;
                }
            }
            KeyCode::Down => {
                if total > 0 && *selected_index + 1 < total {
                    *selected_index += 1;
                }
            }
            KeyCode::Enter if total > 0 => {
                self.state = AppState::SkillDetail {
                    skill_index: *selected_index,
                    scroll_offset: 0,
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// skill 详情面板的事件处理：Esc 返回列表。
    fn handle_skill_detail_event(&mut self, event: AppEvent) -> Result<()> {
        let AppState::SkillDetail {
            skill_index,
            scroll_offset,
        } = &mut self.state
        else {
            return Ok(());
        };
        let AppEvent::InputKey(key) = event else {
            return Ok(());
        };
        let total = self.skills.len();

        match key.code {
            KeyCode::Esc => {
                self.state = AppState::Skills {
                    selected_index: *skill_index,
                };
            }
            KeyCode::Up => {
                if *skill_index > 0 {
                    *skill_index -= 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::Down => {
                if total > 0 && *skill_index + 1 < total {
                    *skill_index += 1;
                    *scroll_offset = 0;
                }
            }
            KeyCode::PageUp => {
                *scroll_offset = scroll_offset.saturating_sub(10);
            }
            KeyCode::PageDown => {
                *scroll_offset = scroll_offset.saturating_add(10);
            }
            _ => {}
        }
        Ok(())
    }

    fn switch_model(&mut self, entry: &ModelEntry) -> Result<()> {
        // 如果 provider 的模型还未写入配置，同步写入
        self.config.ensure_provider_models(&entry.provider_id);

        let resolved = self.config.resolve(&entry.display)?;
        self.agent
            .switch_model(resolved.config, &resolved.model_id, resolved.max_tokens);
        self.model_name = entry.display.clone();
        self.max_context_tokens = entry.context_window;
        self.config.main_model = entry.display.clone();
        self.config.save()?;
        // 同步到共享配置（供 TaskTool / subagent 使用）
        if let Ok(mut shared) = self.shared_config.lock() {
            *shared = self.config.clone();
        }
        self.state = AppState::Chat;
        Ok(())
    }

    async fn open_session_picker(&mut self) -> Result<()> {
        let work_dir = Self::current_work_dir()?;
        let sessions = self.storage.list_sessions(&work_dir).await?;
        let filtered_indices = (0..sessions.len()).collect();
        self.state = AppState::SessionPicker {
            sessions,
            selected_index: 0,
            search_query: String::new(),
            filtered_indices,
        };
        Ok(())
    }

    fn set_session_usage(&mut self, prompt_tokens: u32, completion_tokens: u32) {
        self.context_prompt_tokens = prompt_tokens;
        self.context_completion_tokens = completion_tokens;
    }

    async fn switch_to_session(&mut self, session_id: &str) -> Result<()> {
        self.current_session_id = Some(session_id.to_string());
        if let Ok(mut guard) = self.current_session_shared.lock() {
            *guard = Some(session_id.to_string());
        }
        let (pt, ct) = self.storage.get_session_usage(session_id).await?;
        self.set_session_usage(pt as u32, ct as u32);
        self.messages.clear();
        self.task_records.clear();
        self.cells_dirty = true;
        self.force_clear = true;
        self.render_cache.clear();
        self.input.reset();
        self.pending_pastes.clear();
        self.pending_file_mentions.clear();
        self.scroll_offset = 0;
        self.should_auto_scroll = true;
        self.load_session_messages().await?;
        self.state = AppState::Chat;
        Ok(())
    }

    async fn create_new_session(&mut self) -> Result<()> {
        self.current_session_id = None;
        if let Ok(mut guard) = self.current_session_shared.lock() {
            *guard = None;
        }
        self.context_prompt_tokens = 0;
        self.context_completion_tokens = 0;
        self.messages.clear();
        self.task_records.clear();
        self.cells_dirty = true;
        self.force_clear = true;
        self.render_cache.clear();
        self.input.reset();
        self.pending_pastes.clear();
        self.pending_file_mentions.clear();
        self.scroll_offset = 0;
        self.should_auto_scroll = true;
        let system_prompt = self.agent.take_system_prompt();
        self.agent.clear_messages();
        if let Some(prompt) = system_prompt {
            self.agent.set_system_prompt(&prompt);
        }
        self.state = AppState::Chat;
        Ok(())
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let min_width: u16 = 30;

        if area.width < min_width || area.height < 5 {
            let msg = if area.width < min_width {
                "窗口太窄，请调整至更宽"
            } else {
                "窗口太矮，请调整至更高"
            };
            let col = area.x + area.width.saturating_sub(msg.len() as u16) / 2;
            let row = area.y + area.height / 2;
            if col < area.width && row < area.height {
                frame
                    .buffer_mut()
                    .set_string(col, row, msg, Style::default().fg(Color::Yellow));
            }
            return;
        }

        if let AppState::Setup(ref form) = self.state {
            let cursor_pos = render_setup(area, frame.buffer_mut(), form);
            if let Some(pos) = cursor_pos {
                frame.set_cursor_position(pos);
            }
            return;
        }

        let input_text = self.input.text().to_string();
        self.input.update_area_width(area.width);
        let (total_visual_rows, cursor_visual_row, cursor_visual_col) =
            self.input.compute_visual_info();

        let visible_input_rows: u16 = total_visual_rows.clamp(1, 6);
        let input_area_height: u16 = visible_input_rows + 2;

        let scroll_row = self.input.input_scroll_row();
        let new_scroll = if cursor_visual_row < scroll_row {
            cursor_visual_row
        } else if cursor_visual_row >= scroll_row + visible_input_rows {
            cursor_visual_row - visible_input_rows + 1
        } else {
            scroll_row
        };
        let max_input_scroll = total_visual_rows.saturating_sub(visible_input_rows);
        self.input
            .set_input_scroll_row(new_scroll.min(max_input_scroll));

        if self.cells_dirty {
            let mut cells: Vec<Box<dyn HistoryCell>> = Vec::new();
            cells.push(Box::new(SessionHeaderCell {
                model_name: self.model_name.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                directory: self.work_dir.clone(),
            }));
            cells.extend(history_cell::messages_to_cells(
                &self.messages,
                self.thinking_collapsed,
            ));
            if self.messages.is_empty() {
                cells.push(Box::new(TooltipCell {
                    text: "输入消息开始对话".to_string(),
                }));
            }
            let active_keys: HashSet<u64> = cells.iter().map(|c| c.cache_key()).collect();
            if self.render_cache.len() > active_keys.len() * 2 {
                self.render_cache.retain_keys(&active_keys);
            }
            self.cached_cells = cells;
            self.cells_dirty = false;
        }

        let widget = ChatWidget {
            messages: &self.messages,
            cells: &self.cached_cells,
            input_buffer: &input_text,
            input_elements: self.input.element_info(),
            scroll_offset: self.scroll_offset,
            is_processing: self.is_processing,
            model_name: &self.model_name,
            input_scroll_row: self.input.input_scroll_row(),
            input_area_height,
            directory: &self.work_dir,
            plan_mode: self.plan_mode,
            show_suggestions: self.show_suggestions,
            command_suggestions: &self.command_suggestions,
            selected_suggestion: self.selected_suggestion,
            esc_hint_active: self.esc_hint_active,
            context_tokens: self.context_prompt_tokens + self.context_completion_tokens,
            max_context_tokens: self.max_context_tokens,
            spinner_frame: self.spinner_frame,
            show_file_picker: self.file_picker_active,
            file_picker_results: &self.file_picker_results,
            file_picker_selected: self.file_picker_selected,
            user_msg_sent_at: self.user_msg_sent_at,
            render_cache: &mut self.render_cache,
        };
        let result = widget.render(area, frame.buffer_mut());
        if self.scroll_offset > result.max_hide {
            self.scroll_offset = result.max_hide;
        }

        let show_cursor = match &self.state {
            AppState::Chat => !self.is_processing,
            AppState::SessionPicker { .. }
            | AppState::ModelPicker { .. }
            | AppState::Skills { .. }
            | AppState::SkillDetail { .. }
            | AppState::Mcp { .. }
            | AppState::McpDetail { .. }
            | AppState::McpItemDetail { .. }
            | AppState::Tasks { .. }
            | AppState::TaskDetail { .. }
            | AppState::Setup(_) => false,
            AppState::AskUser { editing_custom, .. } => *editing_custom,
            AppState::AddModel(_) => true,
        };

        if show_cursor {
            let gap_height: u16 = 1;
            let status_height: u16 = 1;
            let input_content_y = area.y
                + area
                    .height
                    .saturating_sub(input_area_height + gap_height + status_height)
                + 1;

            let display_row = cursor_visual_row.saturating_sub(self.input.input_scroll_row());
            let display_col = cursor_visual_col.min(area.width.saturating_sub(1));

            frame.set_cursor_position((
                area.x + display_col,
                input_content_y + display_row.min(visible_input_rows - 1),
            ));
        }

        if let AppState::SessionPicker {
            sessions,
            selected_index,
            search_query,
            filtered_indices,
        } = &self.state
        {
            let picker = SessionPicker {
                sessions,
                filtered_indices,
                selected_index: *selected_index,
                search_query,
                current_session_id: self.current_session_id.as_deref(),
            };
            picker.render(area, frame.buffer_mut());
        }

        if let AppState::ModelPicker {
            models,
            selected_index,
        } = &self.state
        {
            render_model_picker(
                area,
                frame.buffer_mut(),
                models,
                *selected_index,
                &self.model_name,
            );
        }

        if let AppState::AddModel(ref form) = self.state {
            let cursor_pos = render_add_model(area, frame.buffer_mut(), form);
            frame.set_cursor_position(cursor_pos);
        }

        if let AppState::Skills { selected_index } = &self.state {
            render_skills_viewer(
                area,
                frame.buffer_mut(),
                &self.skills,
                *selected_index,
                &self.home_dir,
            );
        }

        if let AppState::SkillDetail {
            skill_index,
            scroll_offset,
        } = &self.state
        {
            skills_viewer::render_skill_detail(
                area,
                frame.buffer_mut(),
                &self.skills,
                *skill_index,
                *scroll_offset,
                &self.home_dir,
            );
        }

        if let AppState::Mcp { selected_index } = &self.state {
            render_mcp_viewer(area, frame.buffer_mut(), &self.mcp_servers, *selected_index);
        }

        if let AppState::McpDetail {
            server_index,
            selected_index,
        } = &self.state
        {
            mcp_viewer::render_mcp_detail(
                area,
                frame.buffer_mut(),
                &self.mcp_servers,
                *server_index,
                *selected_index,
                &self.mcp_backends,
            );
        }

        if let AppState::McpItemDetail {
            server_index,
            item_index,
            scroll_offset,
        } = &self.state
        {
            mcp_viewer::render_mcp_item_detail(
                area,
                frame.buffer_mut(),
                &self.mcp_servers,
                *server_index,
                *item_index,
                *scroll_offset,
                &self.mcp_backends,
            );
        }

        if let AppState::Tasks {
            selected_index,
            entries,
        } = &self.state
        {
            let viewer = TasksViewer {
                entries,
                selected_index: *selected_index,
            };
            viewer.render(area, frame.buffer_mut());
        }

        if let AppState::TaskDetail {
            task_index,
            scroll_offset,
            messages,
            entries,
        } = &mut self.state
        {
            tasks_viewer::render_task_detail(
                area,
                frame.buffer_mut(),
                entries,
                *task_index,
                messages,
                scroll_offset,
            );
        }

        if let AppState::AskUser {
            questions,
            current_tab,
            selected,
            answers,
            custom_inputs,
            custom_cursor,
            editing_custom,
            ..
        } = &self.state
        {
            let cursor_pos = ask_user::render_ask_user(
                area,
                frame.buffer_mut(),
                questions,
                *current_tab,
                *selected,
                answers,
                custom_inputs,
                *custom_cursor,
                *editing_custom,
            );
            if let Some(pos) = cursor_pos {
                frame.set_cursor_position(pos);
            }
        }

        // JediTerm 下 ratatui 的增量 diff 会导致渲染错位，强制全量重绘
        if self.is_jediterm {
            for cell in frame.buffer_mut().content.iter_mut() {
                cell.diff_option = CellDiffOption::AlwaysUpdate;
            }
        }
    }
}

/// 从 task 工具结果文本中解析 sub_session_id。
///
/// 格式: `<task subagent="..." description="..." task_id="UUID" state="completed">`
fn parse_task_id_from_result(result: &str) -> Option<String> {
    let marker = "task_id=\"";
    let start = result.find(marker)? + marker.len();
    let rest = &result[start..];
    let end = rest.find('"')?;
    let id = &rest[..end];
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}
