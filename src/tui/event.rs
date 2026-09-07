use crate::agent::event::CoreEvent;
pub use crate::agent::event::{
    CompactUsage, MessageUsage, QuestionInfo, QuestionOption, TaskStatus,
};
use crate::mcp::McpConnection;
use crossterm::event::{
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
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
    /// 用户提交一条消息，可携带图片附件
    UserSubmit {
        text: String,
        attachments: Vec<crate::agent::media::Attachment>,
    },
    /// 检测到空 bracketed paste（Windows WT 图片剪贴板 Ctrl+V 的按键流形态），
    /// 触发一次剪贴板图片探测（内部事件，由 handle_paste 的标记配对发出）
    PasteImageProbe,
    /// 剪贴板图片探测结果（探测在独立任务中执行，避免阻塞事件循环）。
    /// `interactive` 区分显式按键（未命中给提示）与静默探测（未命中无感）
    PasteImageResult {
        image: Option<crate::tui::clipboard::ClipboardImage>,
        interactive: bool,
    },
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
                } else if is_ctrl_v_paste_leak(&key) {
                    // Windows WT 把 Ctrl+V 交给 paste 动作时 Press 被终端消费、
                    // 仅 Release 泄漏给应用——图片剪贴板场景唯一可达的事件信号
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

/// WT（及其他把 Ctrl+V 绑定为 paste 的终端）将按键交给 paste 动作时，
/// `Press` 被终端消费、仅 `Release` 泄漏给应用。图片剪贴板场景下，
/// conhost 在 WinAPI 输入模式丢弃空 bracketed paste 序列（`Event::Paste`
/// 永不产生），这个「只有 Release 没有 Press」的特征事件是唯一可达信号，
/// 据此触发剪贴板图片探测（见 chat_input 的 handle_chat_key）。
///
/// 仅 Windows 存在该泄漏形态：Unix 开启 kitty 事件类型协议（REPORT_EVENT_TYPES）
/// 后终端同样上报 Release，放行会导致 Ctrl+V 双探测与提示噪音，故非 Windows
/// 一律不匹配（Release 维持原有的丢弃行为）。
#[cfg(windows)]
fn is_ctrl_v_paste_leak(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Release
        && matches!(
            key.code,
            KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('\x16')
        )
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(not(windows))]
fn is_ctrl_v_paste_leak(_key: &KeyEvent) -> bool {
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(kind: KeyEventKind, code: KeyCode, ctrl: bool) -> KeyEvent {
        KeyEvent::new_with_kind(
            code,
            if ctrl {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            },
            kind,
        )
    }

    /// WT Ctrl+V paste 泄漏形态：仅 Release + Ctrl + v（含 Shift 变体与大写）
    #[cfg(windows)]
    #[test]
    fn detects_ctrl_v_release_leak() {
        assert!(is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Char('v'),
            true
        )));
        // WT 默认把 Ctrl+Shift+V 也绑定为 paste，crossterm 报 Char('V')
        assert!(is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Char('V'),
            true
        )));
        // legacy 终端 \x16 控制字符形态
        assert!(is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Char('\x16'),
            true
        )));
    }

    /// Press / 无 Ctrl / 其他键的 Release 均不匹配
    /// （非 Windows 下 stub 恒 false，本测试同样成立）
    #[test]
    fn rejects_non_leak_keys() {
        assert!(!is_ctrl_v_paste_leak(&key(
            KeyEventKind::Press,
            KeyCode::Char('v'),
            true
        )));
        assert!(!is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Char('v'),
            false
        )));
        assert!(!is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Char('c'),
            true
        )));
        assert!(!is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Enter,
            true
        )));
    }
}
