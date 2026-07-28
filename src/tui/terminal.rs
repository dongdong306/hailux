use color_eyre::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::io::{self, Stdout, Write};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn init() -> Result<Tui> {
    crossterm::terminal::enable_raw_mode()?;
    enable_mouse_input()?;
    crossterm::execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        SetCursorStyle::BlinkingBar
    )?;
    // Enable kitty keyboard protocol if the terminal supports it.
    // This allows proper detection of modifier keys (e.g. Shift+Enter vs Enter).
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = crossterm::execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS,
            )
        );
    }
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// 启用鼠标输入（滚轮事件），保留文本选择能力。
///
/// Windows: 通过 Win32 API 设置 `ENABLE_MOUSE_INPUT`，
/// 但**不清除** `ENABLE_QUICK_EDIT_MODE`，以便用户仍可选择和复制文本。
/// （QuickEdit 在传统 conhost 中会拦截滚轮事件，但在 Windows Terminal 中无影响。）
///
/// 所有平台: 写入 ANSI 转义序列启用鼠标报告（仅按钮+滚轮，不含鼠标移动）。
/// 在支持 Shift+拖拽的终端中，用户可通过 Shift 临时绕过鼠标追踪进行文本选择。
fn enable_mouse_input() -> Result<()> {
    #[cfg(windows)]
    {
        use crossterm_winapi::{ConsoleMode, Handle};
        let mode = ConsoleMode::from(Handle::current_in_handle()?);
        let current = mode.mode()?;
        // ENABLE_MOUSE_INPUT=0x10, ENABLE_EXTENDED_FLAGS=0x80, ENABLE_WINDOW_INPUT=0x08
        // 不清除 ENABLE_QUICK_EDIT_MODE，让用户可以选择和复制文本
        let new_mode = current | 0x0010 | 0x0080 | 0x0008;
        mode.set_mode(new_mode)?;
    }

    let mut stdout = io::stdout();
    stdout.write_all(b"\x1b[?1000h\x1b[?1006h")?;
    stdout.flush()?;

    Ok(())
}

fn disable_mouse_input() -> Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(b"\x1b[?1000l\x1b[?1006l")?;
    stdout.flush()?;
    Ok(())
}

pub fn restore(terminal: &mut Tui) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = crossterm::execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_mouse_input();
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}

pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
        let _ = disable_mouse_input();
        let _ = crossterm::execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            crossterm::terminal::DisableLineWrap,
        );
        original_hook(panic_info);
    }));
}
