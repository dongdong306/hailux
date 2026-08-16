//! ChatSession：绑定单个 work_dir 的会话编排单元。
//!
//! 持有 Agent（含该目录发现的 skills/subagents/commands/system prompt）、
//! 共享的 Storage / Config / MCP 后端，以及当前会话 ID。
//! TUI 与 Web 后端通过同一组方法操作会话（新建/切换/发送/中断/压缩），
//! 事件通过 [`EventHub`] + 每请求通道流出（见 `agent::event`）。

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use color_eyre::Result;

use crate::agent::event::{CoreEventTx, EventHub};
use crate::agent::subagent::{self, SubagentConfig, TaskTool};
use crate::agent::{Agent, AskTool, CommandRegistry, skill};
use crate::config;
use crate::mcp::SharedMcpBackends;
use crate::storage::{ChatStorage, StoredMessage};

/// 共享状态（ChatSession、TaskTool、subagent 之间传递）
pub struct SessionShared {
    pub current_session: Arc<Mutex<Option<String>>>,
    pub mcp_backends: SharedMcpBackends,
    pub config: Arc<Mutex<config::Config>>,
}

pub struct ChatSession {
    agent: Agent,
    pub work_dir: PathBuf,
    pub skills: Vec<skill::SkillInfo>,
    pub subagents: Vec<SubagentConfig>,
    pub command_registry: CommandRegistry,
    storage: ChatStorage,
    shared: Arc<SessionShared>,
    /// 当前绑定的工作目录（绝对路径，作为 SessionManager 的键）
    pub(crate) key: PathBuf,
}

impl ChatSession {
    /// 按 work_dir 完成全部发现工作（skills/agents/commands/AGENTS.md）并构建 Agent。
    ///
    /// - `initial_tx`：初始事件通道（TUI 传进程级通道；Web 传 `None`，每请求再绑定）
    /// - MCP 工具注册：`connect_mcp` 在外部完成后调用 [`ChatSession::register_mcp_tools`]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        resolved: &config::ResolvedModel,
        cfg: &config::Config,
        work_dir: &Path,
        storage: ChatStorage,
        mcp_backends: SharedMcpBackends,
        initial_tx: Option<CoreEventTx>,
    ) -> Result<Self> {
        let (mut agent, skills, command_registry, mut subagents) =
            crate::build_agent_base(resolved, cfg, work_dir)?;

        let hub = agent.event_hub().clone();
        if let Some(tx) = initial_tx {
            hub.set(tx);
        }
        agent.register_tool(Box::new(AskTool::new(hub.clone())));

        if !subagents.iter().any(|s| s.name == "general") {
            subagents.insert(0, subagent::builtin_general_subagent(&cfg.main_model));
        }

        let shared = Arc::new(SessionShared {
            current_session: Arc::new(Mutex::new(None)),
            mcp_backends,
            config: Arc::new(Mutex::new(cfg.clone())),
        });

        let session = Self {
            agent,
            work_dir: work_dir.to_path_buf(),
            skills,
            subagents,
            command_registry,
            storage,
            shared,
            key: normalize_key(work_dir),
        };
        Ok(session)
    }

    /// 会话 key（canonicalize 归一化，剥离 Windows verbatim 前缀）
    pub fn key_of(work_dir: &Path) -> PathBuf {
        normalize_key(work_dir)
    }

    pub fn key(&self) -> &Path {
        &self.key
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub fn agent_mut(&mut self) -> &mut Agent {
        &mut self.agent
    }

    pub fn storage(&self) -> &ChatStorage {
        &self.storage
    }

    pub fn shared(&self) -> &Arc<SessionShared> {
        &self.shared
    }

    pub fn event_hub(&self) -> EventHub {
        self.agent.event_hub().clone()
    }

    /// 注册 subagent TaskTool（模型配置完成后调用；setup 阶段不注册）
    pub fn register_task_tool(&mut self, resolved: &config::ResolvedModel) {
        let task_tool = TaskTool::new(
            self.subagents.clone(),
            self.skills.clone(),
            self.storage.clone(),
            resolved.config.clone(),
            resolved.display.clone(),
            resolved.max_tokens,
            self.work_dir.display().to_string(),
            self.shared.current_session.clone(),
            self.shared.mcp_backends.clone(),
            self.shared.config.clone(),
            self.agent.event_hub().clone(),
            self.agent.permission().clone(),
        );
        self.agent.register_tool(Box::new(task_tool));
    }

    /// 注册 MCP 工具（外部连接完成后调用）
    pub fn register_mcp_tools(&mut self, backends: &SharedMcpBackends) {
        let guard = match backends.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        for backend in guard.iter() {
            for tool in &backend.tools {
                self.agent.register_tool(Box::new(crate::mcp::McpTool::new(
                    &backend.server_name,
                    tool,
                    backend.backend.clone(),
                )));
            }
        }
    }

    // ── 消息 ─────────────────────────────────────────────

    /// 发送用户消息并启动流式处理（不等待完成，事件经 `tx` 流出）。
    /// 前置条件：已绑定事件通道（见 `bind_event_channel`）。
    pub fn send_message(
        &mut self,
        message: &str,
        tx: CoreEventTx,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.agent.chat_stream(message, tx)
    }

    /// 为本次请求绑定事件通道（ask_user / subagent 转发走 hub）
    pub fn bind_event_channel(&self, tx: CoreEventTx) {
        self.agent.event_hub().set(tx);
    }

    /// 请求结束解绑通道（Web 每请求调用；TUI 进程级通道无需）
    pub fn unbind_event_channel(&self) {
        self.agent.event_hub().clear();
    }

    pub fn interrupt(&self) {
        self.agent.interrupt();
    }

    /// 取消标志句柄（Web 中断端点直接置位，不经过会话锁）
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.agent.cancel_flag()
    }

    /// 应用压缩结果到 in-memory 上下文
    pub fn apply_compaction_result(&mut self, summary: &str) {
        self.agent.apply_compaction(summary);
    }

    pub fn sync_messages(&mut self, messages: Vec<crate::agent::models::SharedMessage>) {
        self.agent.sync_messages(messages);
    }

    // ── 会话管理 ─────────────────────────────────────────

    /// 当前会话 ID（None = 下次发消息时新建）
    pub fn current_session_id(&self) -> Option<String> {
        self.shared
            .current_session
            .lock()
            .ok()
            .and_then(|g| g.clone())
    }

    /// 新建数据库会话并设为当前会话
    pub async fn create_session(&self, model_display: &str) -> Result<String> {
        let id = self
            .storage
            .create_session(model_display, &self.work_dir.display().to_string())
            .await?;
        self.set_current_session(Some(id.clone()));
        Ok(id)
    }

    pub fn set_current_session(&self, id: Option<String>) {
        if let Ok(mut guard) = self.shared.current_session.lock() {
            *guard = id;
        }
    }

    /// 切换会话：加载权限规则 + 重建 agent 上下文（system prompt + 压缩摘要 + 活跃消息）
    pub async fn switch_session(&mut self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.set_current_session(Some(session_id.to_string()));
        self.agent
            .permission()
            .switch_session(session_id.to_string());

        let stored = self.storage.load_messages(session_id).await?;
        let active = self.storage.load_active_messages(session_id).await?;
        let compact_summary = self.storage.get_compact_summary(session_id).await?;

        let mut chat_messages = Vec::new();
        for msg in &active {
            if msg.role == crate::storage::MessageRole::System {
                continue;
            }
            if let Some(chat_msg) = crate::storage::from_stored_message(msg) {
                chat_messages.push(std::sync::Arc::new(chat_msg));
            }
        }

        if !chat_messages.is_empty() {
            let system_prompt = self.agent.take_system_prompt();
            let mut final_messages = Vec::new();
            if let Some(prompt) = &system_prompt {
                final_messages.push(std::sync::Arc::new(
                    async_openai::types::chat::ChatCompletionRequestSystemMessage {
                        content:
                            async_openai::types::chat::ChatCompletionRequestSystemMessageContent::Text(
                                prompt.clone(),
                            ),
                        name: None,
                    }
                    .into(),
                ));
            }
            if let Some(ref summary) = compact_summary {
                final_messages.push(std::sync::Arc::new(
                    async_openai::types::chat::ChatCompletionRequestUserMessage {
                        content:
                            async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(
                                format!("[Context Summary]\n{}", summary),
                            ),
                        name: None,
                    }
                    .into(),
                ));
            }
            final_messages.extend(chat_messages);
            self.agent.sync_messages(final_messages);
        }

        Ok(stored)
    }

    /// 重置为空会话（不新建 DB 记录，下次发消息时懒创建）
    pub fn reset_session(&mut self) {
        self.set_current_session(None);
        self.agent.permission().clear_session();
        let system_prompt = self.agent.take_system_prompt();
        self.agent.clear_messages();
        if let Some(prompt) = system_prompt {
            self.agent.set_system_prompt(&prompt);
        }
    }

    // ── 模式 ─────────────────────────────────────────────

    pub fn set_plan_mode(&mut self, on: bool) {
        self.agent.set_plan_mode(on);
    }

    pub fn plan_mode(&self) -> bool {
        self.agent.plan_mode()
    }

    pub fn toggle_yolo(&self) -> crate::permission::PermissionMode {
        self.agent.permission().toggle_yolo()
    }

    pub fn switch_model(&mut self, resolved: &config::ResolvedModel) {
        self.agent.switch_model(
            resolved.config.clone(),
            &resolved.model_id,
            resolved.max_tokens,
        );
    }

    // ── 压缩 ─────────────────────────────────────────────

    pub fn request_compaction(
        &self,
        tx: CoreEventTx,
        session_id: String,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.agent.request_compaction(tx, session_id)
    }

    pub fn messages_excluding_system_count(&self) -> usize {
        self.agent.messages_excluding_system_count()
    }
}

/// 归一化目录键：canonicalize + 剥离 Windows `\\?\` verbatim 前缀。
/// 同一目录的不同写法（`D:\a`、`d:/a`、`\\?\D:\a`）得到相同键。
pub(crate) fn normalize_key(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = canonical.to_string_lossy().to_string();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        canonical
    }
}
