mod app_render;
mod attachments;
mod chat_events;
mod chat_input;
mod overlay;
mod paste;
mod session_ops;
pub(crate) mod types;

pub(crate) use types::AppState;
pub use types::{AppSharedState, Message};

use paste::PasteBurst;

use color_eyre::Result;
#[allow(unused_imports)]
use crossterm::event::{KeyCode, KeyModifiers};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::ask_user::AskUserState;
use super::command;
use super::event::{self, AppEvent, EventTx};
#[allow(unused_imports)]
use super::history_cell::{self, HistoryCell, SessionHeaderCell, TooltipCell};
use super::input::InputHandler;
use super::setup::SetupForm;
#[allow(unused_imports)]
use super::tasks_viewer::{TaskEntry, TaskRecord, TaskRunStatus};
use super::terminal;
use crate::agent::Agent;
use crate::agent::CommandRegistry;
use crate::agent::skill::SkillInfo;
use crate::agent::subagent::SubagentConfig;
use crate::config::{self, ResolvedModel};
use crate::mcp::McpServerStatus;
use crate::storage::ChatStorage;

use super::chat_widget::RenderCache;

/// Time budget for batch-draining backlogged events; render immediately once exceeded to avoid UI stutter during fast output
pub(super) const BATCH_RENDER_BUDGET: Duration = Duration::from_millis(50);
/// Cap on events consumed per batch, preventing long occupation under extreme backlogs
pub(super) const BATCH_MAX_EVENTS: usize = 128;

/// File picker state
#[derive(Default)]
pub(super) struct FilePickerState {
    pub(super) active: bool,
    pub(super) results: Vec<String>,
    pub(super) selected: usize,
    pub(super) pending_mentions: Vec<(String, String)>,
    /// Lazily cached path list (path, lowercase); the filesystem is walked once per picker session.
    /// None = not built yet, Some(vec) = built (possibly empty)
    pub(super) cached: Option<Vec<(String, String)>>,
}

impl FilePickerState {
    /// Called when input content is consumed/reset: closes the picker and clears the cache; the next `@` rescans
    pub(super) fn reset(&mut self) {
        self.active = false;
        self.results.clear();
        self.selected = 0;
        self.pending_mentions.clear();
        self.cached = None;
    }
}

/// Command completion suggestion
#[derive(Default)]
pub(super) struct CommandSuggestion {
    pub(super) show: bool,
    pub(super) items: Vec<command::CommandEntry>,
    pub(super) selected: usize,
}

/// State machine for pairing Windows bracketed paste markers.
///
/// crossterm's Windows input source goes through Win32 `ReadConsoleInput` (INPUT_RECORD key events)
/// with no VT input parsing — it can **never produce `Event::Paste`**. Per convention (aligned
/// with the empty-paste protocol used by Claude Code and others), WT sends `\e[200~\e[201~` (an
/// empty bracketed paste) when the clipboard holds only an image; conhost drops that sequence in
/// WinAPI input mode, so what reaches the app is either a "Ctrl+V Release leak" or a key-event
/// stream (verified with paste_probe; see event.rs / chat_input.rs). There conhost has already split
/// `\e[200~` into `Esc`, `[`, `2`, `0`, `0`, `~` keys, which PasteBurst reassembles into text for [`super::App::handle_paste`].
///
/// Marker stripping and pairing therefore happen at the handle_paste layer:
/// - start+end markers with no content → empty paste (image-clipboard Ctrl+V) → trigger clipboard probing
/// - markers wrapping text → strip the markers and treat it as a normal paste
#[derive(Default)]
pub(super) struct BracketPasteState {
    /// Saw `[200~` (start marker), waiting for `[201~`
    pub(super) pending_start: bool,
    /// When the state was established; expires automatically on timeout (orphan marker)
    pub(super) at: Option<Instant>,
}

/// Snapshot for recovering from accidental Esc clears. On Windows the first byte of a `\e[200~` sequence
/// is Esc, wiping the input box along with attachment/paste/mention elements; restored wholesale when paste markers follow.
#[derive(Default)]
pub(super) struct EscClearedSnapshot {
    pub(super) text: String,
    pub(super) pending_images: Vec<(String, crate::agent::media::Attachment)>,
    pub(super) pending_pastes: Vec<(String, String)>,
    pub(super) mentions: Vec<(String, String)>,
}

impl EscClearedSnapshot {
    /// Grabs a snapshot of the current input state (take semantics: the original state is cleared)
    pub(super) fn take_from(app: &mut App) -> Self {
        Self {
            text: app.input.text().to_string(),
            pending_images: std::mem::take(&mut app.pending_images),
            pending_pastes: std::mem::take(&mut app.pending_pastes),
            mentions: std::mem::take(&mut app.file_picker.pending_mentions),
        }
    }
}

/// Task tracking
#[derive(Default)]
pub(super) struct TaskTracker {
    pub(super) records: Vec<TaskRecord>,
    pub(super) call_counter: u64,
    /// Records for in-flight task calls (a concurrent batch holds several)
    pub(super) active_call_ids: Vec<u64>,
}

/// Spinner animation
#[derive(Default)]
pub(super) struct Spinner {
    pub(super) frame: usize,
    pub(super) last_tick: Option<Instant>,
}

/// Request timing stats
#[derive(Default)]
pub(super) struct TimingStats {
    pub(super) user_msg_sent_at: Option<Instant>,
    pub(super) last_total_ms: Option<u64>,
    /// Accumulated paused time: dialogs waiting for user input (permission / ask) don't count toward elapsed time
    pub(super) paused_total: Duration,
    /// Start of the current pause; Some = currently blocked on a dialog waiting for user input
    pub(super) paused_at: Option<Instant>,
}

impl TimingStats {
    /// Called when entering a wait-for-user-input state (Permission / AskUser dialog).
    /// No-op if already paused or no request is in flight.
    pub(super) fn pause(&mut self) {
        if self.user_msg_sent_at.is_some() && self.paused_at.is_none() {
            self.paused_at = Some(Instant::now());
        }
    }

    /// Called after the dialog closes (reply done / canceled / Esc); resumes timing.
    pub(super) fn resume(&mut self) {
        if let Some(paused_at) = self.paused_at.take() {
            self.paused_total += paused_at.elapsed();
        }
    }

    /// Effective send instant with paused time deducted.
    /// While paused this value shifts forward in lockstep with the clock, so `elapsed()` appears frozen.
    pub(super) fn effective_sent_at(&self, now: Instant) -> Option<Instant> {
        let sent_at = self.user_msg_sent_at?;
        let mut adjust = self.paused_total;
        if let Some(paused_at) = self.paused_at {
            adjust += now - paused_at;
        }
        Some(sent_at + adjust)
    }

    /// Actual elapsed time in milliseconds with paused time deducted.
    pub(super) fn effective_elapsed_ms(&self, now: Instant) -> Option<u64> {
        self.effective_sent_at(now)
            .map(|t| now.duration_since(t).as_millis() as u64)
    }

    /// Clears pause state (a new request starts / a request round ends).
    pub(super) fn clear_pause(&mut self) {
        self.paused_total = Duration::ZERO;
        self.paused_at = None;
    }

    /// Starts timing a new request round: records the send instant and resets the previous round's elapsed time and pause state.
    pub(super) fn start_request(&mut self) {
        self.user_msg_sent_at = Some(Instant::now());
        self.last_total_ms = None;
        self.clear_pause();
    }

    /// Finishes timing: records the final elapsed time and clears in-flight state.
    pub(super) fn finish(&mut self, now: Instant) {
        self.last_total_ms = self.effective_elapsed_ms(now);
        self.user_msg_sent_at = None;
        self.clear_pause();
    }

    /// Aborts timing: clears in-flight state without recording elapsed time (error / cancel paths).
    pub(super) fn abort(&mut self) {
        self.user_msg_sent_at = None;
        self.clear_pause();
    }
}

/// Render cache and dirty flag
pub(super) struct RenderState {
    pub(super) cache: RenderCache,
    pub(super) cells: Vec<Box<dyn HistoryCell>>,
    pub(super) dirty: bool,
    pub(super) force_clear: bool,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            cache: RenderCache::new(),
            cells: Vec::new(),
            dirty: true,
            force_clear: false,
        }
    }
}

pub struct App {
    pub(super) messages: Vec<Message>,
    pub(super) input: InputHandler,
    pub(super) scroll_offset: u16,
    pub(super) should_auto_scroll: bool,
    pub(super) last_total_lines: u16,
    pub(super) is_processing: bool,
    pub(super) plan_mode: bool,
    pub(super) should_quit: bool,
    pub(super) agent: Agent,
    pub(super) events: (EventTx, event::EventRx),
    /// Agent domain event sender (wrapped by the forwarder into `AppEvent::Core` to enter the event loop)
    pub(super) core_tx: crate::agent::event::CoreEventTx,
    pub(super) resolved: ResolvedModel,
    pub(super) state: AppState,
    pub(super) storage: ChatStorage,
    pub(super) current_session_id: Option<String>,
    pub(super) work_dir: String,
    pub(super) command_registry: CommandRegistry,
    pub(super) command_entries: Vec<command::CommandEntry>,
    pub(super) cmd_suggestion: CommandSuggestion,
    pub(super) paste_burst: PasteBurst,
    pub(super) last_esc_time: Option<Instant>,
    pub(super) esc_hint_active: bool,
    pub(super) pending_pastes: Vec<(String, String)>,
    /// Pending image attachments (placeholder text → attachment), passed to the agent on message submit
    pub(super) pending_images: Vec<(String, crate::agent::media::Attachment)>,
    /// Windows bracketed paste marker pairing state (see the `BracketPasteState` docs)
    pub(super) bracket_paste: BracketPasteState,
    /// Full snapshot of the input state most recently cleared by Esc (text + attachments/pastes/mentions).
    /// On Windows the first byte of `\e[200~` / `\e[201~` sequences is Esc, which wipes the input
    /// box; restored wholesale when paste markers follow immediately (see handle_paste).
    pub(super) esc_cleared: Option<(EscClearedSnapshot, Instant)>,
    /// Time of the most recent Ctrl+V Press (dedupe window for Release-leak probing):
    /// terminals that don't intercept Ctrl+V deliver Press+Release in pairs; avoids duplicate probes
    pub(super) last_ctrl_v_press: Option<Instant>,
    /// Whether a clipboard image probe is in flight (the probe runs as a detached async child
    /// process; new requests are ignored while in flight, so holding Ctrl+V doesn't queue a flood)
    pub(super) image_probe_in_flight: bool,
    /// Start time of the most recent silent clipboard probe (Release leak / empty bracketed paste).
    /// WeChat/QQ "copy image" carries both a bitmap and a FileDrop, so one Ctrl+V arrives via two
    /// channels: path text injection (original file bytes) + probe GetImage (re-encoded bitmap
    /// bytes) — the data_urls differ, so attach_image dedupe can't match them; drop copies by pairing window
    pub(super) leak_probe_started_at: Option<Instant>,
    /// When an image was last attached via the path-injection channel (handle_paste's FileDrop → path recognition)
    pub(super) path_image_attached_at: Option<Instant>,
    pub(super) config: config::Config,
    pub(super) skills: Vec<SkillInfo>,
    pub(super) home_dir: std::path::PathBuf,
    pub(super) mcp_servers: Vec<McpServerStatus>,
    pub(super) context_prompt_tokens: u32,
    pub(super) context_completion_tokens: u32,
    pub(super) spinner: Spinner,
    pub(super) file_picker: FilePickerState,
    pub(super) subagents: Vec<SubagentConfig>,
    pub(super) tasks: TaskTracker,
    pub(super) shared: AppSharedState,
    pub(super) thinking_collapsed: bool,
    pub(super) timing: TimingStats,
    pub(super) render: RenderState,
    pub(super) is_jediterm: bool,
}

#[allow(clippy::too_many_arguments)]
impl App {
    pub fn new(
        agent: Agent,
        resolved: ResolvedModel,
        storage: ChatStorage,
        config: config::Config,
        skills: Vec<SkillInfo>,
        command_registry: CommandRegistry,
        mcp_servers: Vec<McpServerStatus>,
        subagents: Vec<SubagentConfig>,
        shared: AppSharedState,
        events: (EventTx, event::EventRx),
        core_tx: crate::agent::event::CoreEventTx,
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
            last_total_lines: 0,
            is_processing: false,
            plan_mode: false,
            should_quit: false,
            agent,
            events,
            core_tx,
            resolved,
            state: AppState::Chat,
            storage,
            current_session_id: None,
            work_dir,
            command_registry,
            command_entries,
            cmd_suggestion: CommandSuggestion::default(),
            paste_burst: PasteBurst::new(),
            last_esc_time: None,
            esc_hint_active: false,
            pending_pastes: Vec::new(),
            pending_images: Vec::new(),
            bracket_paste: BracketPasteState::default(),
            esc_cleared: None,
            last_ctrl_v_press: None,
            image_probe_in_flight: false,
            leak_probe_started_at: None,
            path_image_attached_at: None,
            config,
            skills,
            home_dir,
            mcp_servers,
            context_prompt_tokens: 0,
            context_completion_tokens: 0,
            spinner: Spinner::default(),
            file_picker: FilePickerState::default(),
            subagents,
            tasks: TaskTracker::default(),
            shared,
            thinking_collapsed: true,
            timing: TimingStats::default(),
            render: RenderState::default(),
            is_jediterm: std::env::var("TERMINAL_EMULATOR")
                .map(|v| v.contains("JediTerm"))
                .unwrap_or(false),
        }
    }

    pub fn enter_setup(&mut self) {
        self.state = AppState::Setup(SetupForm::new(&self.config, false));
    }

    pub async fn run(&mut self, terminal: &mut terminal::Tui) -> Result<()> {
        let tx = self.events.0.clone();
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
                match tokio::time::timeout(timeout, self.events.1.recv()).await {
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

            // Batch-drain backlogged InputKey events while not processing so pasting doesn't stutter
            // with per-character renders. Pastes (especially in terminals without bracketed paste)
            // send each character as a separate key event; without batching, each triggers a full render cycle.
            if !self.is_processing && !self.should_quit && matches!(self.state, AppState::Chat) {
                let batch_start = Instant::now();
                let mut batch_count = 0usize;
                loop {
                    if batch_count >= BATCH_MAX_EVENTS
                        || batch_start.elapsed() >= BATCH_RENDER_BUDGET
                    {
                        break;
                    }
                    match self.events.1.try_recv() {
                        Ok(AppEvent::InputKey(key)) => {
                            self.handle_chat_key(key).await?;
                            batch_count += 1;
                        }
                        Ok(event) => {
                            pending_input_events.push_back(event);
                            break;
                        }
                        Err(_) => break,
                    }
                }
            }

            // Batch-drain backlogged streaming events (chunks/tools etc.); user interaction events are parked and handled one by one
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
                    match self.events.1.try_recv() {
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
                    .spinner
                    .last_tick
                    .map(|t| now.duration_since(t) >= Duration::from_millis(80))
                    .unwrap_or(true);
                if should_tick {
                    self.spinner.frame = self.spinner.frame.wrapping_add(1);
                    self.spinner.last_tick = Some(now);
                }
                // Auto-reset the Esc hint after 5 seconds
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
                if self.render.force_clear {
                    // Can't use Terminal::clear(): ratatui 0.30's clear first sends a CPR query
                    // (ESC[6n) to read the cursor position; on Unix the reply can be swallowed by
                    // EventStream's stdin reads, and the 2s timeout kills the whole app (Windows
                    // goes through the Win32 API and is unaffected). Resizing to the current size
                    // also clears the screen, resets the back buffer (full redraw next frame), and issues no input queries on any platform.
                    terminal.resize(terminal.size()?.into())?;
                    self.render.force_clear = false;
                }
                terminal.draw(|f| self.render(f))?;
            }
        }

        event_collector.abort();

        // If work is still in flight, clean up: mark it interrupted and repair orphaned tool calls
        if self.is_processing {
            self.cleanup_on_quit().await;
        }

        Ok(())
    }

    /// Cleanup on quit: interrupt the agent and try to wait for AgentComplete (2s timeout);
    /// on timeout, manually repair orphaned tool calls in storage and write the interrupted status.
    ///
    /// Race-safe: the stream task communicates via the event channel and nobody reads its events
    /// after cleanup returns; `repair_orphaned_tool_calls` is idempotent (checks existing_ids) so
    /// no duplicates. Only PersistMessage and AgentComplete are handled; other UI events don't matter on the exit path.
    async fn cleanup_on_quit(&mut self) {
        self.agent.interrupt();

        // Try to wait for the AgentComplete event (up to 2 seconds)
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(Some(event)) =
                tokio::time::timeout(Duration::from_millis(100), self.events.1.recv()).await
            {
                match &event {
                    AppEvent::Core(crate::agent::event::CoreEvent::AgentComplete { .. }) => {
                        if let Err(e) = self.handle_event(event).await {
                            eprintln!("[warn] cleanup handle_event error: {e}");
                        }
                        return;
                    }
                    AppEvent::Core(crate::agent::event::CoreEvent::PersistMessage { .. }) => {
                        if let Err(e) = self.handle_event(event).await {
                            eprintln!("[warn] cleanup persist error: {e}");
                        }
                    }
                    _ => {}
                }
            }
        }

        // Timeout fallback: repair storage manually
        if let Some(session_id) = &self.current_session_id {
            if let Err(e) = self.storage.repair_orphaned_tool_calls(session_id).await {
                eprintln!("[warn] repair_orphaned_tool_calls: {e}");
            }
            // Write interrupted runtime_meta (reasoning effort taken from the current Agent value:
            // the streaming snapshot is unavailable at interrupt time; normal rounds write it via the AgentComplete path)
            let total_ms = self
                .timing
                .effective_elapsed_ms(Instant::now())
                .unwrap_or(0);
            let meta = serde_json::json!({
                "total_ms": total_ms,
                "model": self.resolved.display,
                "status": "interrupted",
                "reasoning_effort": self.agent.reasoning_effort().as_str(),
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

    /// Wraps handle_event, auto-detecting message changes and marking the cells cache dirty
    async fn process_event(&mut self, event: AppEvent) -> Result<()> {
        let streaming = matches!(
            &event,
            AppEvent::Core(
                crate::agent::event::CoreEvent::AgentChunk(_)
                    | crate::agent::event::CoreEvent::AgentReasoningChunk(_)
                    | crate::agent::event::CoreEvent::AgentComplete { .. }
                    | crate::agent::event::CoreEvent::CompactChunk(_)
                    | crate::agent::event::CoreEvent::CompactComplete { .. },
            )
        );
        let is_subagent_result = matches!(
            &event,
            AppEvent::Core(crate::agent::event::CoreEvent::ToolResult {
                subagent_name: Some(_),
                ..
            })
        );
        let len_before = self.messages.len();
        self.handle_event(event).await?;
        if self.messages.len() != len_before || streaming || is_subagent_result {
            self.render.dirty = true;
        }
        Ok(())
    }

    /// Whether an event is a high-frequency streaming event eligible for batch draining
    fn is_batchable_event(event: &AppEvent) -> bool {
        match event {
            AppEvent::Core(e) => matches!(
                e,
                crate::agent::event::CoreEvent::AgentChunk(_)
                    | crate::agent::event::CoreEvent::AgentReasoningChunk(_)
                    | crate::agent::event::CoreEvent::UsageUpdate { .. }
                    | crate::agent::event::CoreEvent::PersistMessage { .. }
                    | crate::agent::event::CoreEvent::ToolCallStart { .. }
                    | crate::agent::event::CoreEvent::ToolResult { .. }
                    | crate::agent::event::CoreEvent::AgentComplete { .. }
                    | crate::agent::event::CoreEvent::CompactChunk(_)
            ),
            _ => false,
        }
    }

    async fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        // MCP background connection completion event, not gated by the current state
        if let AppEvent::McpReady(connections) = event {
            self.handle_mcp_ready(connections).await?;
            return Ok(());
        }

        // Model list fetch results: the model picker (R key) or the setup form (post-key fetch)
        if let AppEvent::ModelsRefreshed {
            provider_id,
            result,
        } = event
        {
            match &self.state {
                AppState::ModelPicker { .. } => {
                    self.handle_model_picker_event(AppEvent::ModelsRefreshed {
                        provider_id,
                        result,
                    })?;
                }
                AppState::Setup(_) => {
                    self.handle_setup_models_refreshed(&provider_id, result)?;
                }
                _ => {}
            }
            return Ok(());
        }

        // Permission requests are enqueued here regardless of the current state.
        // Must be handled first: if another dialog (e.g. AskUser) is open and the request were
        // routed to that handler, the event would be swallowed and the agent-side oneshot would never resolve, deadlocking.
        if let AppEvent::Core(crate::agent::event::CoreEvent::PermissionRequest {
            request,
            response_tx,
            subagent_name,
        }) = event
        {
            let pending = crate::tui::app::types::PermissionPending {
                request,
                response_tx,
                subagent_name,
            };
            match &mut self.state {
                // Dialog already open: enqueue the new request instead of overwriting the current one
                AppState::Permission { pending: queue, .. } => queue.push(pending),
                _ => {
                    self.state = AppState::Permission {
                        pending: vec![pending],
                        selected: 0,
                    };
                }
            }
            self.timing.pause();
            return Ok(());
        }

        // In non-Chat overlay states, non-keyboard events must still be forwarded to the Chat handler
        // so agent responses, tool results, etc. aren't dropped.
        let is_overlay = !matches!(
            &self.state,
            AppState::Chat | AppState::AskUser { .. } | AppState::Permission { .. }
        );
        let is_local_input = matches!(
            &event,
            AppEvent::InputKey(_)
                | AppEvent::InputPaste(_)
                | AppEvent::UserSubmit { .. }
                | AppEvent::PasteImageProbe
                | AppEvent::PasteImageResult { .. }
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
            AppState::EffortPicker { .. } => {
                self.handle_effort_picker_event(event)?;
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
            AppState::Stats { .. } => {
                self.handle_stats_event(event).await?;
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
                if matches!(self.state, AppState::Chat) {
                    self.timing.resume();
                }
            }
            AppState::Permission { .. } => {
                if let AppEvent::InputKey(key) = event {
                    let (mut pending, mut selected) =
                        match std::mem::replace(&mut self.state, AppState::Chat) {
                            AppState::Permission { pending, selected } => (pending, selected),
                            other => {
                                self.state = other;
                                return Ok(());
                            }
                        };
                    let done = crate::tui::permission_dialog::handle_key(key, &mut selected);
                    if done {
                        self.reply_permission(&mut pending, selected);
                    } else {
                        self.state = AppState::Permission { pending, selected };
                    }
                }
                if matches!(self.state, AppState::Chat) {
                    self.timing.resume();
                }
            }
        }
        Ok(())
    }

    /// Replies to the current permission request (queue head) and batch-handles the remaining requests in the same group:
    /// - Always: after session rules are written, same-group requests covered by the new rules are auto-approved
    /// - Deny: the other pending requests in the group are denied as well
    /// - Once: only the current request is answered
    fn reply_permission(
        &mut self,
        pending: &mut Vec<crate::tui::app::types::PermissionPending>,
        selected: usize,
    ) {
        let reply = crate::tui::permission_dialog::reply_from_selected(selected);
        let Some(current) = pending.first() else {
            self.state = AppState::Chat;
            return;
        };
        let current_group = current.subagent_name.clone();
        let current_permission = current.request.permission.clone();
        let current_always = current.request.always_patterns.clone();
        let current = pending.remove(0);

        if reply == crate::permission::PermissionReply::Always {
            self.agent
                .permission()
                .add_session_rules(&current_permission, &current_always);
        }
        let _ = current.response_tx.send(reply);

        if reply == crate::permission::PermissionReply::Always
            || reply == crate::permission::PermissionReply::Deny
        {
            let pm = self.agent.permission();
            let mut i = 0;
            while i < pending.len() {
                let same_group = pending[i].subagent_name == current_group;
                let auto_reply = if same_group {
                    match reply {
                        crate::permission::PermissionReply::Always => pm.rules_allow(
                            &pending[i].request.permission,
                            &pending[i].request.patterns,
                        ),
                        _ => true,
                    }
                } else {
                    false
                };
                if auto_reply {
                    let p = pending.remove(i);
                    let _ = p.response_tx.send(reply);
                } else {
                    i += 1;
                }
            }
        }

        if pending.is_empty() {
            self.state = AppState::Chat;
        } else {
            self.state = AppState::Permission {
                pending: std::mem::take(pending),
                selected: 0,
            };
        }
    }
}
