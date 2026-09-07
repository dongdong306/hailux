//! 粘贴事件流诊断（Windows Ctrl+V 图片粘贴问题定位）。
//!
//! 运行：`cargo run --example paste_probe`
//!
//! 步骤：
//! 1. 复制一张截图（Win+Shift+S）
//! 2. 在本程序中按 Ctrl+V，观察打印的事件流
//! 3. 再粘贴一段纯文本对照
//! 4. Ctrl+C 退出
//!
//! 判读：
//! - 出现 `Paste(len=0)` → crossterm 收到空 paste（正常协议路径）
//! - 出现一串 `Esc` / `[` / `2` / `0` / `0` / `~` 按键 → WT 发了标记但被
//!   conhost 拆成字符按键（hailux 的标记配对应能接住，若仍不行是时序问题）
//! - 按下 Ctrl+V 后毫无输出 → WT/ConPTY 未投递任何事件
//!   （ConPTY 可能吞掉了 ?2004h，WT 没有启用 bracketed paste 包装）

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
