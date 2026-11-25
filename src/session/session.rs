//! ChatSession: session orchestration bound to one work_dir. Owns the Agent
//! (with this dir's discovered skills/subagents/commands/system prompt),
//! shared Storage / Config / MCP backends, and the current session id.

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
    /// Absolute path; the SessionManager key
    pub(crate) key: PathBuf,
    /// The chat entry reconciles this against the global config: broadcast
    /// switches skip locked instances, drift at entry re-applies the switch
    current_model_display: String,
}

impl ChatSession {
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
            current_model_display: resolved.display.clone(),
        };
        Ok(session)
    }

    /// Canonicalized; Windows verbatim prefix stripped
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

    /// Not registered during setup
    pub fn register_task_tool(&mut self, resolved: &config::ResolvedModel) {
        let task_tool = TaskTool::new(
            self.subagents.clone(),
            self.skills.clone(),
            self.storage.clone(),
            resolved.display.clone(),
            self.work_dir.display().to_string(),
            self.shared.current_session.clone(),
            self.shared.mcp_backends.clone(),
            self.shared.config.clone(),
            self.agent.event_hub().clone(),
            self.agent.permission().clone(),
        );
        self.agent.register_tool(Box::new(task_tool));
    }

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

    pub fn send_message(
        &mut self,
        session_id: &str,
        message: &str,
        attachments: Vec<crate::agent::media::Attachment>,
        tx: CoreEventTx,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.agent.chat_stream(session_id, message, attachments, tx)
    }

    pub fn bind_event_channel(&self, tx: CoreEventTx) {
        self.agent.event_hub().set(tx);
    }

    pub fn unbind_event_channel(&self) {
        self.agent.event_hub().clear();
    }

    pub fn interrupt(&self) {
        self.agent.interrupt();
    }

    /// Web interrupt endpoint sets this without the session lock
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.agent.cancel_flag()
    }

    /// `&self` on purpose: the runner holds the session lock for the whole
    /// run, so delivery must not take it
    pub fn interject(&self, interjection: crate::agent::event::Interjection) -> bool {
        self.agent.interject(interjection)
    }

    pub(crate) fn interject_slot(&self) -> crate::agent::event::InterjectSlot {
        self.agent.interject_slot()
    }

    pub fn apply_compaction_result(&mut self, summary: &str) {
        self.agent.apply_compaction(summary);
    }

    pub fn sync_messages(&mut self, messages: Vec<crate::agent::models::SharedMessage>) {
        self.agent.sync_messages(messages);
    }

    pub fn current_session_id(&self) -> Option<String> {
        self.shared
            .current_session
            .lock()
            .ok()
            .and_then(|g| g.clone())
    }

    /// Clears the in-memory context: the Web reuses one Agent instance
    /// across sessions, leftover messages must never leak
    pub async fn create_session(&mut self, model_display: &str) -> Result<String> {
        self.reset_context();
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

    /// Switch sessions: load permission rules + rebuild the agent context
    /// (system prompt + compaction summary + active messages)
    pub async fn switch_session(&mut self, session_id: &str) -> Result<Vec<StoredMessage>> {
        self.set_current_session(Some(session_id.to_string()));
        self.agent
            .permission()
            .switch_session_async(session_id.to_string())
            .await;

        // Repair dangling tool_calls left by historical interrupts so DB
        // dirty data never reaches the rebuilt context
        if let Err(e) = self.storage.repair_orphaned_tool_calls(session_id).await {
            eprintln!("[warn] repair_orphaned_tool_calls: {e}");
        }

        // One query; filter compacted rows in memory — a second query would
        // re-parse the large base64 attachment fields
        let stored = self.storage.load_messages(session_id).await?;
        let active: Vec<&crate::storage::StoredMessage> =
            stored.iter().filter(|m| !m.compacted).collect();
        // Restore the plan-context flag from the last active user row: if
        // that turn ran in plan mode, the first message sent after resuming
        // in chat mode must carry the exit reminder. Active rows only —
        // compacted rows left the context, so their reminders need no
        // countermand (see stored_plan_mode).
        let plan_context = active
            .iter()
            .rev()
            .find(|m| m.role == crate::storage::MessageRole::User)
            .map(|m| crate::storage::stored_plan_mode(m))
            .unwrap_or(false);
        let compact_summary = self.storage.get_compact_summary(session_id).await?;

        let mut chat_messages = Vec::new();
        for msg in active {
            if msg.role == crate::storage::MessageRole::System {
                continue;
            }
            if let Some(chat_msg) = crate::storage::from_stored_message(msg) {
                chat_messages.push(std::sync::Arc::new(chat_msg));
            }
        }

        // Always rebuild wholesale: switching to an empty session must clear
        // the previous session's leftovers
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
        self.agent.set_plan_context(plan_context);

        Ok(stored)
    }

    pub fn reset_session(&mut self) {
        self.set_current_session(None);
        self.reset_context();
    }

    pub(crate) fn reset_context(&mut self) {
        self.agent.permission().clear_session();
        let system_prompt = self.agent.take_system_prompt();
        self.agent.clear_messages();
        self.agent.set_plan_context(false);
        if let Some(prompt) = system_prompt {
            self.agent.set_system_prompt(&prompt);
        }
    }

    pub fn set_plan_mode(&mut self, on: bool) {
        self.agent.set_plan_mode(on);
    }

    pub fn plan_mode(&self) -> bool {
        self.agent.plan_mode()
    }

    pub fn toggle_yolo(&self) -> crate::permission::PermissionMode {
        self.agent.permission().toggle_yolo()
    }

    /// Web global-mode broadcast entry, not a toggle
    pub fn set_yolo(&self, on: bool) {
        let mode = if on {
            crate::permission::PermissionMode::Yolo
        } else {
            crate::permission::PermissionMode::Normal
        };
        self.agent.permission().set_mode(mode);
    }

    /// SessionManager broadcasts through this handle; TaskTool etc. already
    /// share the instance, so broadcasts reach subagents immediately
    pub fn permission_manager(&self) -> crate::permission::PermissionManager {
        self.agent.permission().clone()
    }

    pub fn switch_model(&mut self, resolved: &config::ResolvedModel) {
        self.current_model_display = resolved.display.clone();
        self.agent.switch_model(
            resolved.config.clone(),
            &resolved.model_id,
            &resolved.display,
            resolved.max_tokens,
            resolved.supports_vision,
            resolved.native_reasoning,
        );
    }

    pub fn current_model_display(&self) -> &str {
        &self.current_model_display
    }

    pub fn set_reasoning_effort(&mut self, effort: crate::agent::ReasoningEffort) {
        self.agent.set_reasoning_effort(effort);
    }

    pub fn reasoning_effort(&self) -> crate::agent::ReasoningEffort {
        self.agent.reasoning_effort()
    }

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

/// Canonicalize + strip the Windows `\\?\` verbatim prefix so different
/// spellings of the same directory map to one key
pub(crate) fn normalize_key(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = canonical.to_string_lossy().to_string();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        canonical
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::models::SharedMessage;
    use crate::storage::{MessageRole, plan_mode_runtime_meta};

    async fn make_session() -> (ChatSession, PathBuf) {
        let storage = ChatStorage::new_in_memory().await.unwrap();
        let work_dir =
            std::env::temp_dir().join(format!("hailux-session-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&work_dir).unwrap();
        let resolved = config::ResolvedModel {
            config: async_openai::config::OpenAIConfig::new(),
            model_id: "test-model".to_string(),
            max_tokens: 1024,
            context_window: 8192,
            display: "test/test-model".to_string(),
            supports_vision: false,
            native_reasoning: false,
        };
        let session = ChatSession::new(
            &resolved,
            &config::Config::default(),
            &work_dir,
            storage,
            Arc::new(Mutex::new(Vec::new())),
            None,
        )
        .unwrap();
        (session, work_dir)
    }

    fn user_message(text: &str) -> SharedMessage {
        Arc::new(
            async_openai::types::chat::ChatCompletionRequestUserMessage {
                content: async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(
                    text.to_string(),
                ),
                name: None,
            }
            .into(),
        )
    }

    fn system_message(text: &str) -> SharedMessage {
        Arc::new(
            async_openai::types::chat::ChatCompletionRequestSystemMessage {
                content: async_openai::types::chat::ChatCompletionRequestSystemMessageContent::Text(
                    text.to_string(),
                ),
                name: None,
            }
            .into(),
        )
    }

    fn stored_user_message(content: &str) -> StoredMessage {
        StoredMessage {
            role: MessageRole::User,
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

    #[tokio::test]
    async fn create_session_clears_stale_agent_context() {
        let (mut session, work_dir) = make_session().await;
        session.sync_messages(vec![system_message("sys"), user_message("旧会话消息")]);
        assert_eq!(session.messages_excluding_system_count(), 1);

        let id = session.create_session("test-model").await.unwrap();
        assert_eq!(session.current_session_id().as_deref(), Some(id.as_str()));
        assert_eq!(session.messages_excluding_system_count(), 0);
        assert_eq!(session.agent().take_system_prompt().as_deref(), Some("sys"));

        std::fs::remove_dir_all(&work_dir).ok();
    }

    #[tokio::test]
    async fn switch_session_to_empty_clears_stale_agent_context() {
        let (mut session, work_dir) = make_session().await;
        let dir = work_dir.display().to_string();
        let a = session.storage().create_session("m", &dir).await.unwrap();
        session
            .storage()
            .append_message(&a, &stored_user_message("历史消息"))
            .await
            .unwrap();
        let b = session.storage().create_session("m", &dir).await.unwrap();

        session.switch_session(&a).await.unwrap();
        assert_eq!(session.messages_excluding_system_count(), 1);

        session.switch_session(&b).await.unwrap();
        assert_eq!(session.messages_excluding_system_count(), 0);
        assert!(session.agent().take_system_prompt().is_some());

        std::fs::remove_dir_all(&work_dir).ok();
    }

    #[tokio::test]
    async fn switch_session_restores_plan_context_from_last_active_user_row() {
        let (mut session, work_dir) = make_session().await;
        let dir = work_dir.display().to_string();
        let a = session.storage().create_session("m", &dir).await.unwrap();

        // Chat-mode turn only: no plan context
        session
            .storage()
            .append_message(&a, &stored_user_message("chat turn"))
            .await
            .unwrap();
        session.switch_session(&a).await.unwrap();
        assert!(!session.agent().plan_context());

        // Plan-mode turn (runtime_meta.plan_mode) sets the flag
        let mut row = stored_user_message("plan turn");
        row.runtime_meta = plan_mode_runtime_meta(true);
        session.storage().append_message(&a, &row).await.unwrap();
        session.switch_session(&a).await.unwrap();
        assert!(session.agent().plan_context());

        // Compaction wipes every row carrying reminders from the rebuilt
        // context — nothing left to countermand, so the flag must clear
        session.storage().mark_messages_compacted(&a).await.unwrap();
        session.switch_session(&a).await.unwrap();
        assert!(!session.agent().plan_context());

        std::fs::remove_dir_all(&work_dir).ok();
    }

    fn stored_assistant_with_tool_calls() -> StoredMessage {
        StoredMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: Some(
                r#"[{"id":"call_1","type":"function","function":{"name":"read","arguments":"{}"}},{"id":"call_2","type":"function","function":{"name":"read","arguments":"{}"}}]"#
                    .to_string(),
            ),
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

    fn stored_tool_result(id: &str) -> StoredMessage {
        StoredMessage {
            role: MessageRole::Tool,
            content: "ok".to_string(),
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
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

    #[tokio::test]
    async fn switch_session_repairs_orphaned_tool_calls() {
        let (mut session, work_dir) = make_session().await;
        let dir = work_dir.display().to_string();
        let id = session.storage().create_session("m", &dir).await.unwrap();
        session
            .storage()
            .append_message(&id, &stored_user_message("hi"))
            .await
            .unwrap();
        session
            .storage()
            .append_message(&id, &stored_assistant_with_tool_calls())
            .await
            .unwrap();
        session
            .storage()
            .append_message(&id, &stored_tool_result("call_1"))
            .await
            .unwrap();

        session.switch_session(&id).await.unwrap();

        let stored = session.storage().load_messages(&id).await.unwrap();
        let stubs: Vec<_> = stored
            .iter()
            .filter(|m| m.role == MessageRole::Tool && m.tool_call_id.as_deref() == Some("call_2"))
            .collect();
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].content, "Tool execution aborted");

        assert_eq!(session.messages_excluding_system_count(), 4);

        // Idempotent: switching again produces no duplicate stubs
        session.switch_session(&id).await.unwrap();
        let stored = session.storage().load_messages(&id).await.unwrap();
        let stubs: Vec<_> = stored
            .iter()
            .filter(|m| m.role == MessageRole::Tool && m.tool_call_id.as_deref() == Some("call_2"))
            .collect();
        assert_eq!(stubs.len(), 1);

        std::fs::remove_dir_all(&work_dir).ok();
    }
}
