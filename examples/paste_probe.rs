//! Paste event stream diagnostics (locating the Windows Ctrl+V image paste issue).
//!
//! Run: `cargo run --example paste_probe`
//!
//! Steps:
//! 1. Copy a screenshot (Win+Shift+S)
//! 2. Press Ctrl+V in this program and watch the printed event stream
//! 3. Then paste some plain text for comparison
//! 4. Ctrl+C to exit
//!
//! Interpreting the output:
//! - `Paste(len=0)` appears → crossterm received an empty paste (normal
//!   protocol path)
//! - A run of `Esc` / `[` / `2` / `0` / `0` / `~` key presses → WT sent the
//!   marker but conhost split it into char key presses (hailux's marker
//!   pairing should catch it; if it still fails, it is a timing issue)
//! - Nothing printed at all after Ctrl+V → WT/ConPTY delivered no events
//!   (ConPTY may have swallowed ?2004h; WT did not enable bracketed paste)

use std::time::Instant;

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers,
};
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;

    let start = Instant::now();
    println!("[paste_probe] 事件流诊断开始（Ctrl+C 退出）\r");
    println!("[paste_probe] 请先复制一张截图，然后按 Ctrl+V\r");

    let mut events = EventStream::new();
    while let Some(Ok(ev)) = events.next().await {
        let t = start.elapsed().as_millis();
        match ev {
            Event::Key(k) => {
                let kind = match k.kind {
                    KeyEventKind::Press => "Press",
                    KeyEventKind::Repeat => "Repeat",
                    KeyEventKind::Release => "Release",
                };
                println!(
                    "[{t}ms] Key {kind} code={:?} mods={:?}\r",
                    k.code, k.modifiers
                );
                if matches!(k.code, KeyCode::Char('c'))
                    && k.modifiers.contains(KeyModifiers::CONTROL)
                    && k.kind == KeyEventKind::Press
                {
                    break;
                }
            }
            Event::Paste(p) => {
                println!(
                    "[{t}ms] Paste(len={}) head={:?}\r",
                    p.len(),
                    &p.chars().take(40).collect::<String>()
                );
            }
            other => println!("[{t}ms] {other:?}\r"),
        }
    }

    crossterm::execute!(std::io::stdout(), DisableBracketedPaste)?;
    crossterm::terminal::disable_raw_mode()?;
    println!("[paste_probe] 结束");
    Ok(())
}
