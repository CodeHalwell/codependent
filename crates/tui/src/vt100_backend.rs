//! VT100 terminal emulator backend for screen-state snapshot testing (Adoption 12 A1).
//!
//! Wraps a `CrosstermBackend<Vt100Writer>`: every ratatui draw is serialized
//! to actual ANSI bytes by the crossterm backend, which processes bytes through
//! an in-memory `vt100::Parser`; `Display` renders `parser.screen().contents()`
//! for `insta::assert_snapshot!`.
//!
//! Deliberately avoids any crossterm call that touches the real stdout (size
//! and cursor position come from the vt100 screen).

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::prelude::CrosstermBackend;

/// Shared writer wrapping a `vt100::Parser`.
#[derive(Clone)]
pub struct Vt100Writer {
    parser: Arc<Mutex<vt100::Parser>>,
}

impl Write for Vt100Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut p = self
            .parser
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        p.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Terminal backend that renders via Crossterm into an in-memory `vt100::Parser`.
pub struct VT100Backend {
    parser: Arc<Mutex<vt100::Parser>>,
    crossterm_backend: CrosstermBackend<Vt100Writer>,
}

impl VT100Backend {
    /// Creates a new `VT100Backend` with the specified width and height.
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self::with_scrollback(width, height, 0)
    }

    /// Creates a new `VT100Backend` with custom scrollback history length.
    #[must_use]
    pub fn with_scrollback(width: u16, height: u16, scrollback_len: usize) -> Self {
        crossterm::style::force_color_output(true);
        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            height,
            width,
            scrollback_len,
        )));
        let writer = Vt100Writer {
            parser: Arc::clone(&parser),
        };
        Self {
            parser,
            crossterm_backend: CrosstermBackend::new(writer),
        }
    }

    /// Returns a clone of the underlying `vt100::Parser` arc mutex.
    #[must_use]
    pub fn parser(&self) -> Arc<Mutex<vt100::Parser>> {
        Arc::clone(&self.parser)
    }
}

impl Write for VT100Backend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut p = self
            .parser
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        p.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Display for VT100Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.parser.lock().map_err(|_| fmt::Error)?;
        write!(f, "{}", p.screen().contents())
    }
}

impl Backend for VT100Backend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.crossterm_backend.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.crossterm_backend.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        let p = self
            .parser
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let (row, col) = p.screen().cursor_position();
        Ok(Position::new(col, row))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.crossterm_backend.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.crossterm_backend.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.crossterm_backend.clear_region(clear_type)
    }

    fn append_lines(&mut self, line_count: u16) -> io::Result<()> {
        self.crossterm_backend.append_lines(line_count)
    }

    fn size(&self) -> io::Result<Size> {
        let p = self
            .parser
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let (rows, cols) = p.screen().size();
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let p = self
            .parser
            .lock()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let (rows, cols) = p.screen().size();
        Ok(WindowSize {
            columns_rows: Size::new(cols, rows),
            pixels: Size {
                width: 640,
                height: 480,
            },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.crossterm_backend)
    }
}
