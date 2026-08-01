use crate::agent::models::SharedMessage;
use crate::mcp::McpConnection;
use crossterm::event::{
    Event as CrosstermEvent, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct MessageUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone)]
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

#[derive(Debug)]
pub enum AppEvent {
    InputKey(KeyEvent),
    InputPaste(String),
    UserSubmit(String),
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
    Resize,
    ScrollUp,
    ScrollDown,
    MouseClick,
    /// MCP 后台连接完成，携带所有连接结果供 UI 更新与工具注册
    McpReady(Vec<McpConnection>),
    CompactChunk(String),
    CompactComplete {
        summary: String,
        session_id: String,
    },
    CompactError(String),
}

pub type EventTx = mpsc::Sender<AppEvent>;
pub type EventRx = mpsc::Receiver<AppEvent>;

pub fn create_event_channel() -> (EventTx, EventRx) {
    mpsc::channel(4096)
}

pub async fn collect_terminal_events(tx: EventTx) {
    use crossterm::event::{EventStream, KeyEventKind};
    use futures_util::StreamExt;

    let mut reader = EventStream::new();

    while let Some(result) = reader.next().await {
        let Ok(event) = result else {
            continue;
        };
        match event {
            CrosstermEvent::Key(key) => {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    let _ = tx.try_send(AppEvent::InputKey(key));
                }
            }
            CrosstermEvent::Paste(text) => {
                let _ = tx.try_send(AppEvent::InputPaste(text));
            }
            CrosstermEvent::Resize(_, _) => {
                let _ = tx.try_send(AppEvent::Resize);
            }
            CrosstermEvent::Mouse(mouse) => {
                handle_mouse_event(mouse, &tx);
            }
            _ => {}
        }
    }
}

fn handle_mouse_event(mouse: MouseEvent, tx: &EventTx) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            let _ = tx.try_send(AppEvent::ScrollUp);
        }
        MouseEventKind::ScrollDown => {
            let _ = tx.try_send(AppEvent::ScrollDown);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let _ = tx.try_send(AppEvent::MouseClick);
        }
        _ => {}
    }
}
