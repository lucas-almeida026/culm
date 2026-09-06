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
