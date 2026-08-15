//! Interactive TUI session driver running over a pseudoterminal.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
use thiserror::Error;

/// Errors produced by the TUI end-to-end harness.
#[derive(Debug, Error)]
pub enum E2eError {
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("Timed out waiting for pattern '{0}'")]
    Timeout(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Process exited unexpectedly: {0}")]
    ProcessExited(String),
}

/// A live TUI session driven via pseudoterminal.
pub struct TuiSession {
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    _reader_handle: std::thread::JoinHandle<()>,
}

impl TuiSession {
    /// Launches a binary under a new pseudo-terminal with the specified screen dimensions.
    pub fn launch(
        binary: impl AsRef<Path>,
        args: &[&str],
        cols: u16,
        rows: u16,
    ) -> Result<Self, E2eError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| E2eError::Pty(e.to_string()))?;

        let mut cmd = CommandBuilder::new(binary.as_ref());
        for arg in args {
            cmd.arg(*arg);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| E2eError::Pty(e.to_string()))?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| E2eError::Pty(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| E2eError::Pty(e.to_string()))?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 1000)));
        let parser_clone = Arc::clone(&parser);

        let reader_handle = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut p) = parser_clone.lock() {
                    p.process(&buf[..n]);
                }
            }
        });

        Ok(Self {
            child,
            writer,
            parser,
            _reader_handle: reader_handle,
        })
    }

    /// Blocks until the terminal screen contains the target text or timeout expires.
    pub fn wait_for(&self, text: &str, timeout: Duration) -> Result<(), E2eError> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if let Ok(p) = self.parser.lock() {
                let content = p.screen().contents();
                if content.contains(text) {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(E2eError::Timeout(text.to_string()))
    }

    /// Asserts that the specified text is not present on the current terminal screen.
    pub fn assert_absent(&self, text: &str) {
        if let Ok(p) = self.parser.lock() {
            let content = p.screen().contents();
            assert!(
                !content.contains(text),
                "expected '{text}' to be absent from screen, but screen contains:\n{content}"
            );
        }
    }

    /// Types an ASCII or UTF-8 string into the session.
    pub fn type_str(&mut self, text: &str) -> Result<(), E2eError> {
        self.writer.write_all(text.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    /// Sends a crossterm KeyCode (translated to ANSI escape bytes) to the terminal.
    pub fn press(&mut self, key: crossterm::event::KeyCode) -> Result<(), E2eError> {
        use crossterm::event::KeyCode;
        let bytes: &[u8] = match key {
            KeyCode::Enter => b"\r",
            KeyCode::Esc => b"\x1b",
            KeyCode::Backspace => b"\x7f",
            KeyCode::Tab => b"\t",
            KeyCode::Up => b"\x1b[A",
            KeyCode::Down => b"\x1b[B",
            KeyCode::Right => b"\x1b[C",
            KeyCode::Left => b"\x1b[D",
            KeyCode::Char(c) => {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                return self.type_str(s);
            }
            _ => b"",
        };
        if !bytes.is_empty() {
            self.writer.write_all(bytes)?;
            self.writer.flush()?;
        }
        Ok(())
    }

    /// Returns the complete current screen contents as rendered plain text.
    #[must_use]
    pub fn snapshot(&self) -> String {
        self.parser
            .lock()
            .map(|p| p.screen().contents())
            .unwrap_or_default()
    }

    /// Terminates the child process and closes the session.
    pub fn shutdown(&mut self) -> Result<(), E2eError> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
