use crate::agent::event::CoreEvent;
pub use crate::agent::event::{
    CompactUsage, MessageUsage, QuestionInfo, QuestionOption, TaskStatus,
};
use crate::mcp::McpConnection;
use crossterm::event::{
    Event as CrosstermEvent, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
};
use tokio::sync::mpsc;

/// TUI 事件 = 领域事件（包装）+ 终端专有事件。
///
/// agent 只产出 `CoreEvent`；本模块的转发任务将其包装为 `AppEvent::Core`
/// 送入终端事件循环，终端输入（键盘/粘贴/缩放/鼠标）则直接产生专有变体。
#[derive(Debug)]
pub enum AppEvent {
    /// 领域事件（agent / subagent 产出）
    Core(CoreEvent),
    InputKey(KeyEvent),
    InputPaste(String),
    UserSubmit(String),
    Resize,
    ScrollUp,
    ScrollDown,
    MouseClick,
    /// MCP 后台连接完成，携带所有连接结果供 UI 更新与工具注册
    McpReady(Vec<McpConnection>),
}

pub type EventTx = mpsc::Sender<AppEvent>;
pub type EventRx = mpsc::Receiver<AppEvent>;

pub fn create_event_channel() -> (EventTx, EventRx) {
    mpsc::channel(4096)
}

/// 将领域事件流转发进 TUI 事件循环（`CoreEvent` → `AppEvent::Core`）。
/// TUI 事件通道关闭时自动退出。
pub fn spawn_core_forwarder(mut core_rx: crate::agent::event::CoreEventRx, tx: EventTx) {
    tokio::spawn(async move {
        while let Some(event) = core_rx.recv().await {
            if tx.send(AppEvent::Core(event)).await.is_err() {
                break;
            }
        }
    });
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
