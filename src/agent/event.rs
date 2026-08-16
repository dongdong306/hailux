//! Agent 领域事件出口 —— agent 模块对上层（TUI / Web）的唯一事件契约。
//!
//! agent 不感知任何 UI；上层自行消费 `CoreEvent`：
//! - TUI：`tui::event::AppEvent::Core(CoreEvent)` 包装后进入终端事件循环
//! - Web：`web::protocol` 将其序列化为 SSE JSON 推送
//!
//! 注意：`AskUser` / `PermissionRequest` 携带 `oneshot::Sender` 回复通道，
//! 是同步阻塞语义（agent task 等待用户答复）。无 UI 的消费者（非交互模式、
//! Web 断连兜底）必须保证最终回复，否则 agent task 永久挂起。

use crate::agent::models::SharedMessage;
use crate::permission::{PermissionReply, PermissionRequest};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct MessageUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, serde::Serialize)]
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

/// 领域事件（UI 无关）
#[derive(Debug)]
pub enum CoreEvent {
    AgentChunk(String),
    AgentReasoningChunk(String),
    AgentComplete {
        messages: Vec<SharedMessage>,
        usages: Vec<MessageUsage>,
        status: TaskStatus,
    },
    UsageUpdate {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    PersistMessage {
        msg: SharedMessage,
        usage: Option<(u32, u32)>,
        display: Option<String>,
    },
    ToolCallStart {
        name: String,
        arguments: String,
        /// 若来自 subagent 转发，携带 subagent 名称
        subagent_name: Option<String>,
    },
    ToolResult {
        name: String,
        result: String,
        display: Option<String>,
        /// 若来自 subagent 转发，携带 subagent 名称
        subagent_name: Option<String>,
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

/// 可替换的事件通道句柄。
///
/// 部分工具（ask_user、TaskTool 的 subagent 转发）在构造时无法预知
/// 事件该发往哪个消费者，通过 hub 间接持有通道：
/// - TUI：启动时 `set` 一次，通道与进程同生命周期
/// - Web：每个 `/api/chat` 请求 `set` 为该请求的通道，请求结束 `clear`
///
/// `send` 在通道缺失或已关闭时返回 `false`，调用方自行兜底
/// （ask_user 直接报错返回，转发类静默丢弃）。
#[derive(Clone, Default)]
pub struct EventHub {
    tx: std::sync::Arc<std::sync::RwLock<Option<CoreEventTx>>>,
}

impl EventHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// 绑定通道（覆盖旧通道）
    pub fn set(&self, tx: CoreEventTx) {
        if let Ok(mut guard) = self.tx.write() {
            *guard = Some(tx);
        }
    }

    /// 解绑通道（请求结束后调用）
    pub fn clear(&self) {
        if let Ok(mut guard) = self.tx.write() {
            *guard = None;
        }
    }

    /// 尝试投递事件。返回 `false` = 未绑定通道或通道已关闭（未投递）。
    pub fn send(&self, event: CoreEvent) -> bool {
        if let Ok(guard) = self.tx.read()
            && let Some(tx) = guard.as_ref()
        {
            return tx.try_send(event).is_ok();
        }
        false
    }

    /// 是否绑定了通道
    pub fn is_bound(&self) -> bool {
        self.tx.read().map(|g| g.is_some()).unwrap_or(false)
    }
}
