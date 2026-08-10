mod app_render;
mod chat_events;
mod chat_input;
mod overlay;
mod session_ops;
pub(crate) mod types;

pub(crate) use types::AppState;
pub use types::{AppSharedState, Message};

use chat_input::PasteBurst;

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

/// 批量消费积压事件的时间预算，超时后立即渲染，避免高速输出时 UI 卡顿
pub(super) const BATCH_RENDER_BUDGET: Duration = Duration::from_millis(50);
/// 单次批量消费的事件上限，防止极端积压下长时间占用
pub(super) const BATCH_MAX_EVENTS: usize = 128;
pub(super) const DEFAULT_CONTEXT_WINDOW: u32 = 131072;
pub(super) const DEFAULT_OUTPUT_TOKENS: u32 = 65536;

/// 文件选择器状态
#[derive(Default)]
pub(super) struct FilePickerState {
    pub(super) active: bool,
    pub(super) results: Vec<String>,
    pub(super) selected: usize,
    pub(super) pending_mentions: Vec<(String, String)>,
    /// 懒缓存的路径列表（path, lowercase），picker 会话期间只遍历一次文件系统。
    /// None = 尚未构建，Some(vec) = 已构建（可能为空）
    pub(super) cached: Option<Vec<(String, String)>>,
}

impl FilePickerState {
    /// 输入内容被消费/重置时调用：关闭 picker 并清空缓存，下次 `@` 重新扫描
    pub(super) fn reset(&mut self) {
        self.active = false;
        self.results.clear();
        self.selected = 0;
        self.pending_mentions.clear();
        self.cached = None;
    }
}

/// 命令补全建议
#[derive(Default)]
pub(super) struct CommandSuggestion {
    pub(super) show: bool,
    pub(super) items: Vec<command::CommandEntry>,
    pub(super) selected: usize,
}

/// Task 跟踪
#[derive(Default)]
pub(super) struct TaskTracker {
    pub(super) records: Vec<TaskRecord>,
    pub(super) call_counter: u64,
    pub(super) active_call_id: Option<u64>,
}

/// Spinner 动画
#[derive(Default)]
pub(super) struct Spinner {
    pub(super) frame: usize,
    pub(super) last_tick: Option<Instant>,
}

/// 请求计时统计
#[derive(Default)]
pub(super) struct TimingStats {
    pub(super) user_msg_sent_at: Option<Instant>,
    pub(super) last_total_ms: Option<u64>,
    /// 累计暂停时长：等待用户输入的弹窗（权限确认 / ask）期间不计入耗时
    pub(super) paused_total: Duration,
    /// 当前暂停起点；Some = 正处于弹窗等待用户输入
    pub(super) paused_at: Option<Instant>,
}

impl TimingStats {
    /// 进入等待用户输入的状态（Permission / AskUser 弹窗）时调用。
    /// 已在暂停中或没有进行中的请求时为无操作。
    pub(super) fn pause(&mut self) {
        if self.user_msg_sent_at.is_some() && self.paused_at.is_none() {
            self.paused_at = Some(Instant::now());
        }
    }

    /// 弹窗关闭（回复完成 / 取消 / Esc）后调用，恢复计时。
    pub(super) fn resume(&mut self) {
        if let Some(paused_at) = self.paused_at.take() {
            self.paused_total += paused_at.elapsed();
        }
    }

    /// 扣除暂停时间后的等效发送时刻。
    /// 暂停中该值随时间同步后移，使 `elapsed()` 显示为冻结值。
    pub(super) fn effective_sent_at(&self, now: Instant) -> Option<Instant> {
        let sent_at = self.user_msg_sent_at?;
        let mut adjust = self.paused_total;
        if let Some(paused_at) = self.paused_at {
            adjust += now - paused_at;
        }
        Some(sent_at + adjust)
    }

    /// 扣除暂停时间后的实际耗时（毫秒）。
    pub(super) fn effective_elapsed_ms(&self, now: Instant) -> Option<u64> {
        self.effective_sent_at(now)
            .map(|t| now.duration_since(t).as_millis() as u64)
    }

    /// 清空暂停状态（新请求开始 / 一轮请求结束时）。
    pub(super) fn clear_pause(&mut self) {
        self.paused_total = Duration::ZERO;
        self.paused_at = None;
    }

    /// 开始一轮新的请求计时：记录发送时刻、清空上一轮耗时与暂停状态。
    pub(super) fn start_request(&mut self) {
        self.user_msg_sent_at = Some(Instant::now());
        self.last_total_ms = None;
        self.clear_pause();
    }

    /// 结束计时：记录最终耗时并清空进行中状态。
    pub(super) fn finish(&mut self, now: Instant) {
        self.last_total_ms = self.effective_elapsed_ms(now);
        self.user_msg_sent_at = None;
        self.clear_pause();
    }

    /// 中止计时：不记录耗时，直接清空进行中状态（用于错误 / 取消路径）。
    pub(super) fn abort(&mut self) {
        self.user_msg_sent_at = None;
        self.clear_pause();
    }
}

/// 渲染缓存与脏标记
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
        self.state = AppState::Setup(SetupForm::new());
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

            // 在非处理状态下批量消费积压的 InputKey 事件，避免粘贴时逐字符渲染导致卡顿。
            // 粘贴（尤其无 bracketed paste 的终端）会以独立按键事件发送每个字符，
            // 若不批量处理，每个字符都会触发一次完整渲染周期。
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
                if self.render.force_clear {
                    terminal.clear()?;
                    self.render.force_clear = false;
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
                tokio::time::timeout(Duration::from_millis(100), self.events.1.recv()).await
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
                .timing
                .effective_elapsed_ms(Instant::now())
                .unwrap_or(0);
            let meta = serde_json::json!({
                "total_ms": total_ms,
                "model": self.resolved.display,
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
            self.render.dirty = true;
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

        // 权限请求统一在此入队，不受当前 state 限制。
        // 必须优先处理：若此时处于其他弹窗（如 AskUser）而把请求
        // 交给对应 handler，事件会被吞掉，agent 端 oneshot 永不 resolve 而死锁。
        if let AppEvent::PermissionRequest {
            request,
            response_tx,
            subagent_name,
        } = event
        {
            let pending = crate::tui::app::types::PermissionPending {
                request,
                response_tx,
                subagent_name,
            };
            match &mut self.state {
                // 弹窗已打开：新请求入队，不覆盖当前请求
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

        // 非 Chat 覆盖层状态下，非键盘事件仍需转发给 Chat 处理器，
        // 确保 agent 响应、工具结果等不被丢弃。
        let is_overlay = !matches!(
            &self.state,
            AppState::Chat | AppState::AskUser { .. } | AppState::Permission { .. }
        );
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

    /// 回复当前权限请求（队列队首），并批量处理同组其余请求：
    /// - Always：写入会话规则后，同组中已被新规则覆盖的请求自动放行
    /// - Deny：同组其余 pending 请求一并拒绝
    /// - Once：仅回复当前请求
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
