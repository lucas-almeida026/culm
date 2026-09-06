//! One Claude Code session: a child process, its screen, and the thread that keeps
//! the screen current.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::pty::{Child, Pty, PtySpawner, SessionSpec};

const SCROLLBACK: usize = 2000;

pub struct Session {
    name: String,
    parser: Arc<Mutex<vt100::Parser>>,
    pty: Box<dyn Pty>,
    child: Box<dyn Child>,
    bytes: Arc<AtomicU64>,
    eof: Arc<AtomicBool>,
    rows: u16,
    cols: u16,
}

// vt100::Parser does not implement Debug, so Session implements it by hand.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("name", &self.name)
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("bytes_read", &self.bytes_read())
            .finish()
    }
}

impl Session {
    /// Starts a session and the thread that drains it.
    ///
    /// The drain thread runs for every session, visible or not. An undrained
    /// pseudoterminal fills its buffer and the child blocks. See FINDINGS.md.
    pub fn spawn(spawner: &dyn PtySpawner, spec: &SessionSpec) -> Result<Self> {
        let spawned = spawner.spawn(spec)?;
        let mut reader = spawned.reader;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            spec.rows, spec.cols, SCROLLBACK,
        )));
        let bytes = Arc::new(AtomicU64::new(0));
        let eof = Arc::new(AtomicBool::new(false));

        {
            let parser = Arc::clone(&parser);
            let bytes = Arc::clone(&bytes);
            let eof = Arc::clone(&eof);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    bytes.fetch_add(n as u64, Ordering::Relaxed);
                    if let Ok(mut p) = parser.lock() {
                        p.process(&buf[..n]);
                    }
                }
                eof.store(true, Ordering::Relaxed);
            });
        }

        Ok(Self {
            name: spec.name.clone(),
            parser,
            pty: spawned.pty,
            child: spawned.child,
            bytes,
            eof,
            rows: spec.rows,
            cols: spec.cols,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn bytes_read(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.pid()
    }

    #[must_use]
    pub fn parser(&self) -> &Arc<Mutex<vt100::Parser>> {
        &self.parser
    }

    /// True once the child ended, by request or on its own. End of file on the
    /// pseudoterminal is the first signal, and the child status confirms it.
    pub fn has_exited(&mut self) -> bool {
        self.eof.load(Ordering::Relaxed) || self.child.has_exited()
    }

    /// Asks the child to exit.
    pub fn terminate(&mut self) -> Result<()> {
        self.child.terminate()
    }

    /// Ends the child without asking.
    pub fn kill(&mut self) -> Result<()> {
        self.child.kill()
    }

    pub fn send(&mut self, data: &[u8]) -> Result<()> {
        self.pty.write(data)
    }

    /// Resizes the child to match its panel. The child then wraps at the panel width.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        if (rows, cols) == (self.rows, self.cols) || rows == 0 || cols == 0 {
            return Ok(());
        }
        self.rows = rows;
        self.cols = cols;
        self.pty.resize(rows, cols)?;
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
        Ok(())
    }

    /// The encoding the child asked mouse events to arrive in, or `None` when the
    /// child wants no mouse events.
    ///
    /// A child that takes the mouse owns its own scrolling, so culm forwards the
    /// wheel instead of moving its own scrollback. Claude Code is such a child.
    #[must_use]
    pub fn mouse_encoding(&self) -> Option<crate::keys::MouseEncoding> {
        let parser = match self.parser.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        if parser.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None {
            return None;
        }
        Some(match parser.screen().mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Sgr => crate::keys::MouseEncoding::Sgr,
            // The UTF-8 encoding is rare, and its coordinates agree with the default
            // encoding below column 96, so the default carries it.
            _ => crate::keys::MouseEncoding::Default,
        })
    }

    /// Moves the view back through the scrollback. A positive count goes up.
    ///
    /// `vt100` clamps to the real length of the scrollback, and it raises the offset
    /// when a new line arrives while the view is held back, so output from the child
    /// never drags the view.
    pub fn scroll_by(&self, lines: i32) {
        let mut parser = match self.parser.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        let current = parser.screen().scrollback();
        parser
            .screen_mut()
            .set_scrollback(current.saturating_add_signed(lines as isize));
    }

    /// Returns the view to the live output, as a terminal does when the user types.
    pub fn scroll_to_bottom(&self) {
        let mut parser = match self.parser.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        parser.screen_mut().set_scrollback(0);
    }

    /// How many lines the view sits above the live output. Zero means live.
    #[must_use]
    pub fn scrollback(&self) -> usize {
        match self.parser.lock() {
            Ok(p) => p.screen().scrollback(),
            Err(e) => e.into_inner().screen().scrollback(),
        }
    }

    /// The visible screen as plain text. Tests read this instead of a terminal.
    #[must_use]
    pub fn screen_text(&self) -> String {
        match self.parser.lock() {
            Ok(p) => p.screen().contents(),
            Err(e) => e.into_inner().screen().contents(),
        }
    }

    /// Waits until the screen contains `needle`, or the timeout expires.
    /// Tests use this instead of sleeping for a fixed period.
    pub fn wait_for_text(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.screen_text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// Waits until the child ends, or the timeout expires.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.has_exited() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.has_exited()
    }
}
