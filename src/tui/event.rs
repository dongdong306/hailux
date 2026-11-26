use crate::agent::event::CoreEvent;
pub use crate::agent::event::{
    CompactUsage, MessageUsage, QuestionInfo, QuestionOption, TaskStatus,
};
use crate::mcp::McpConnection;
use crossterm::event::{
    Event as CrosstermEvent, KeyEvent, MouseButton, MouseEvent, MouseEventKind,
};
use tokio::sync::mpsc;

/// TUI events = domain events (wrapped) + terminal-specific events.
///
/// The agent only produces `CoreEvent`; the forwarder task in this module wraps it as `AppEvent::Core`
/// and feeds it into the terminal event loop, while terminal input (keyboard/paste/resize/mouse) directly produces the specific variants.
#[derive(Debug)]
pub enum AppEvent {
    /// Domain event (produced by the agent / subagent)
    Core(CoreEvent),
    InputKey(KeyEvent),
    InputPaste(String),
    /// User submits a message, optionally carrying image attachments and (absolute path, display relative path) mention pairs
    UserSubmit {
        text: String,
        attachments: Vec<crate::agent::media::Attachment>,
        mentions: Vec<(String, String)>,
    },
    /// An empty bracketed paste was detected (the key-stream shape of image-clipboard Ctrl+V on Windows WT),
    /// triggering one clipboard image probe (internal event, emitted via the marker pairing in handle_paste)
    PasteImageProbe,
    /// Clipboard image probe result (the probe runs in a separate task to avoid blocking the event loop).
    /// `interactive` distinguishes an explicit keypress (show a hint on miss) from a silent probe (no hint on miss)
    PasteImageResult {
        image: Option<crate::tui::clipboard::ClipboardImage>,
        interactive: bool,
    },
    Resize,
    ScrollUp,
    ScrollDown,
    MouseClick,
    /// MCP background connections finished, carrying all connection results for UI updates and tool registration
    McpReady(Vec<McpConnection>),
    /// `/models` fetch result from a background task (the model picker's fetch key or the
    /// setup form's post-key fetch); only meaningful while the originating overlay is open
    ModelsRefreshed {
        provider_id: String,
        result: Result<Vec<crate::config::model_discovery::RemoteModel>, String>,
    },
    /// LLM-generated session title persisted by the background title task
    /// (see `session::title`). Only the session-picker refreshes live;
    /// other states ignore it — the picker re-reads the DB when opened.
    SessionTitleUpdated {
        session_id: String,
        title: String,
    },
}

pub type EventTx = mpsc::Sender<AppEvent>;
pub type EventRx = mpsc::Receiver<AppEvent>;

pub fn create_event_channel() -> (EventTx, EventRx) {
    mpsc::channel(4096)
}

/// Forwards the domain event stream into the TUI event loop (`CoreEvent` → `AppEvent::Core`).
/// Exits automatically when the TUI event channel is closed.
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
                    // When Windows WT hands Ctrl+V to the paste action, Press is consumed by the terminal
                    // and only Release leaks to the app — the only reachable event signal in the image-clipboard scenario
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

/// When WT (and other terminals that bind Ctrl+V to the paste action) hands the keypress to paste,
/// `Press` is consumed by the terminal and only `Release` leaks to the app. In the image-clipboard
/// scenario, conhost drops the empty bracketed paste sequence in WinAPI input mode (`Event::Paste`
/// is never produced), so this "Release without Press" signature event is the only reachable signal;
/// it triggers the clipboard image probe (see handle_chat_key in chat_input).
///
/// This leak shape exists only on Windows: on Unix, once the kitty keyboard event-type protocol
/// (REPORT_EVENT_TYPES) is enabled the terminal reports Release too, and letting it through would
/// cause double Ctrl+V probes and hint noise, so non-Windows never matches (Release keeps its original drop behavior).
#[cfg(windows)]
fn is_ctrl_v_paste_leak(key: &KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

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
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

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

    /// WT Ctrl+V paste leak shape: Release only + Ctrl + v (including the Shift variant and uppercase)
    #[cfg(windows)]
    #[test]
    fn detects_ctrl_v_release_leak() {
        assert!(is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Char('v'),
            true
        )));
        // WT binds Ctrl+Shift+V to paste by default too; crossterm reports Char('V')
        assert!(is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Char('V'),
            true
        )));
        // legacy-terminal \x16 control-character form
        assert!(is_ctrl_v_paste_leak(&key(
            KeyEventKind::Release,
            KeyCode::Char('\x16'),
            true
        )));
    }

    /// Press / missing Ctrl / Release of other keys all fail to match
    /// (on non-Windows the stub always returns false, so this test holds there as well)
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
