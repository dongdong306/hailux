use color_eyre::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::prelude::*;
use std::io::{self, Stdout, Write};
use std::sync::{Arc, Mutex};

pub type Tui = Terminal<SynchronizedBackend<Stdout>>;

/// Writer wrapper sharing one underlying writer instance between
/// [`SynchronizedBackend`] and its inner [`CrosstermBackend`], so Begin/End
/// markers and frame bytes stay interleaved in a single ordered stream
/// (CrosstermBackend has no writer() accessor, hence the clone-by-arc).
struct SharedWriter<W> {
    inner: Arc<Mutex<W>>,
}

// Manual impl: derive(Clone) would add a `W: Clone` bound, but cloning the Arc
// never touches W.
impl<W> Clone for SharedWriter<W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<W> SharedWriter<W> {
    fn new(writer: W) -> Self {
        Self {
            inner: Arc::new(Mutex::new(writer)),
        }
    }

    #[cfg(test)]
    fn into_inner(self) -> io::Result<W> {
        match Arc::try_unwrap(self.inner) {
            Ok(mutex) => Ok(mutex.into_inner().unwrap_or_else(|e| e.into_inner())),
            Err(_) => Err(io::Error::other("shared writer is still borrowed")),
        }
    }
}

impl<W: Write> Write for SharedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }
}

/// Backend wrapper that brackets each frame in DEC 2026 synchronized output.
///
/// CrosstermBackend flushes mid-frame (cursor commands use `execute!`), so terminals
/// that render while parsing see the hardware cursor travel through every diffed cell
/// before ratatui moves it back to the input box — visible as cursor jumping during
/// streaming output. Wrapping the frame in Begin/End Synchronized Update makes capable
/// terminals (Windows Terminal 1.18+, WezTerm, kitty, Alacritty, Ghostty, iTerm2)
/// present the whole frame, final cursor position included, atomically. Terminals
/// without DEC 2026 ignore the unknown private modes, so this is a safe best-effort
/// optimization.
///
/// The `sync_open` flag keeps Begin/End strictly paired: `clear()` opens a region so a
/// resize repaint never flashes an empty screen between the clear and the next frame
/// (ratatui resizes via clear-then-redraw), a failed `draw()` closes the region again,
/// and a duplicate Begin is never queued. `restore()` and the panic hook still send a
/// final End as a guard against aborts that bypass these methods.
// `W: Write` is required transitively: CrosstermBackend's own struct definition
// carries the bound.
pub struct SynchronizedBackend<W: Write> {
    inner: CrosstermBackend<SharedWriter<W>>,
    writer: SharedWriter<W>,
    sync_open: bool,
}

impl<W: Write> SynchronizedBackend<W> {
    pub fn new(writer: W) -> Self {
        let writer = SharedWriter::new(writer);
        Self {
            inner: CrosstermBackend::new(writer.clone()),
            writer,
            sync_open: false,
        }
    }

    #[cfg(test)]
    pub fn into_inner(self) -> io::Result<W> {
        drop(self.inner);
        self.writer.into_inner()
    }

    fn begin_sync(&mut self) -> io::Result<()> {
        if !self.sync_open {
            crossterm::queue!(self.writer, BeginSynchronizedUpdate)?;
            self.sync_open = true;
        }
        Ok(())
    }

    fn end_sync(&mut self) -> io::Result<()> {
        if self.sync_open {
            crossterm::queue!(self.writer, EndSynchronizedUpdate)?;
            self.sync_open = false;
        }
        Ok(())
    }
}

impl<W: Write> Backend for SynchronizedBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.begin_sync()?;
        match self.inner.draw(content) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Close the region so the terminal never stays frozen on a half frame.
                let _ = self.end_sync();
                Err(e)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Queued before the inner flush so the End marker leaves as the final bytes
        // of the frame, after the cursor-position commands.
        self.end_sync()?;
        // Fully qualified: CrosstermBackend implements both Backend::flush and
        // Write::flush.
        Backend::flush(&mut self.inner)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.begin_sync()?;
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.begin_sync()?;
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
}

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
    let backend = SynchronizedBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Enable mouse input (wheel events) while preserving text selection.
///
/// Windows: sets `ENABLE_MOUSE_INPUT` via the Win32 API but does NOT clear
/// `ENABLE_QUICK_EDIT_MODE`, so users can still select and copy text.
/// (QuickEdit intercepts wheel events in legacy conhost but has no effect in Windows Terminal.)
///
/// All platforms: writes ANSI escape sequences to enable mouse reporting (buttons + wheel
/// only, no mouse motion). On terminals supporting Shift+drag, users can hold Shift to
/// temporarily bypass mouse tracking for text selection.
fn enable_mouse_input() -> Result<()> {
    #[cfg(windows)]
    {
        use crossterm_winapi::{ConsoleMode, Handle};
        let mode = ConsoleMode::from(Handle::current_in_handle()?);
        let current = mode.mode()?;
        // ENABLE_MOUSE_INPUT=0x10, ENABLE_EXTENDED_FLAGS=0x80, ENABLE_WINDOW_INPUT=0x08
        // don't clear ENABLE_QUICK_EDIT_MODE so users can select and copy text
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
        let _ = crossterm::execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_mouse_input();
    // Close a dangling synchronized-update region (e.g. interrupted mid-frame) so
    // the terminal resumes presenting output.
    let _ = crossterm::execute!(io::stdout(), EndSynchronizedUpdate);
    crossterm::execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste)?;
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
            EndSynchronizedUpdate,
            LeaveAlternateScreen,
            DisableBracketedPaste,
            crossterm::terminal::DisableLineWrap,
        );
        original_hook(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEGIN: &[u8] = b"\x1b[?2026h";
    const END: &[u8] = b"\x1b[?2026l";
    const CLEAR_ALL: &[u8] = b"\x1b[2J";

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// Begin/End markers must appear strictly alternating in the stream.
    fn assert_paired(out: &[u8]) {
        let mut open = false;
        for (i, w) in out.windows(BEGIN.len()).enumerate() {
            if w == BEGIN {
                assert!(!open, "nested Begin near byte {i}");
                open = true;
            } else if w == END {
                assert!(open, "End without Begin near byte {i}");
                open = false;
            }
        }
        assert!(!open, "dangling Begin at end of stream");
    }

    fn draw_one_cell(backend: &mut SynchronizedBackend<Vec<u8>>) -> io::Result<()> {
        let cell = Cell::new("x");
        Backend::draw(backend, [(0, 0, &cell)].into_iter())?;
        Backend::flush(backend)
    }

    #[test]
    fn frame_is_bracketed_by_synchronized_markers() {
        let mut backend = SynchronizedBackend::new(Vec::new());
        draw_one_cell(&mut backend).unwrap();
        let out = backend.into_inner().unwrap();
        assert_paired(&out);
        assert!(out.starts_with(BEGIN), "frame must start with Begin");
        assert!(out.ends_with(END), "frame must end with End");
        assert!(out.len() > BEGIN.len() + END.len(), "diff bytes missing");
    }

    #[test]
    fn consecutive_frames_pair_every_marker() {
        let mut backend = SynchronizedBackend::new(Vec::new());
        draw_one_cell(&mut backend).unwrap();
        draw_one_cell(&mut backend).unwrap();
        let out = backend.into_inner().unwrap();
        assert_paired(&out);
    }

    #[test]
    fn clear_opens_a_region_the_next_frame_closes() {
        let mut backend = SynchronizedBackend::new(Vec::new());
        draw_one_cell(&mut backend).unwrap();
        Backend::clear(&mut backend).unwrap();
        draw_one_cell(&mut backend).unwrap();
        let out = backend.into_inner().unwrap();
        assert_paired(&out);
        // The clear bytes must sit inside the reopened region, atomically with the
        // following frame (no visible empty-screen flash between clear and repaint).
        let clear_inside_region = [BEGIN, CLEAR_ALL].concat();
        assert!(
            contains(&out, &clear_inside_region),
            "clear must be bracketed together with the following frame"
        );
    }

    #[test]
    fn clear_region_also_opens_the_region() {
        let mut backend = SynchronizedBackend::new(Vec::new());
        Backend::clear_region(&mut backend, ClearType::All).unwrap();
        draw_one_cell(&mut backend).unwrap();
        let out = backend.into_inner().unwrap();
        assert_paired(&out);
    }

    /// Writer that fails on the Nth write call and records the successful bytes.
    struct FailOnNthWriter {
        buf: Vec<u8>,
        writes: usize,
        fail_on: usize,
    }

    impl Write for FailOnNthWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if self.writes == self.fail_on {
                return Err(io::Error::other("simulated write failure"));
            }
            self.buf.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_draw_closes_the_region() {
        // Write #1 queues the Begin, write #2 (the first frame byte) fails, so the
        // recovery End must be write #3.
        let mut backend = SynchronizedBackend::new(FailOnNthWriter {
            buf: Vec::new(),
            writes: 0,
            fail_on: 2,
        });
        let cell = Cell::new("x");
        let result = Backend::draw(&mut backend, [(0, 0, &cell)].into_iter());
        assert!(result.is_err(), "draw must propagate the write failure");
        Backend::flush(&mut backend).unwrap();
        let writer = backend.into_inner().unwrap();
        // The Begin opened before the failure is closed again immediately; the
        // trailing flush must not emit a second End for the closed region.
        assert_eq!(writer.buf, [BEGIN, END].concat());
    }
}
