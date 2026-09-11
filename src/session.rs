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
    /// Returns how far the view really moved, which is short of `lines` at either end
    /// of the buffer and zero for a child that keeps no scrollback here at all. A
    /// selection is carried by that distance, so the caller needs the real figure.
    ///
    /// `vt100` clamps to the real length of the scrollback, and it raises the offset
    /// when a new line arrives while the view is held back, so output from the child
    /// never drags the view.
    pub fn scroll_by(&self, lines: i32) -> i32 {
        let mut parser = match self.parser.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        let before = parser.screen().scrollback();
        parser
            .screen_mut()
            .set_scrollback(before.saturating_add_signed(lines as isize));
        let after = parser.screen().scrollback();
        offset_delta(before, after)
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

    /// One hash per visible row, with a blank row hashing to zero.
    ///
    /// A child that owns the mouse scrolls by repainting its own panel, and it tells
    /// culm nothing about how far the text went. Comparing a fingerprint taken before
    /// the notch against one taken after is the only way culm can find out.
    #[must_use]
    pub fn row_fingerprint(&self) -> Vec<u64> {
        let parser = match self.parser.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        let (_, cols) = parser.screen().size();
        parser
            .screen()
            .rows(0, cols)
            .map(|row| hash_row(&row))
            .collect()
    }

    /// The text of a linear selection, both ends included.
    ///
    /// Each end is `(row, column)`, counted inside the panel border at the offset the
    /// view holds now. A negative row, or a row past the last one, names a line the
    /// user scrolled away from after marking it, so the reader walks the scrollback
    /// to reach it. A line older than the buffer still holds reads as nothing.
    #[must_use]
    pub fn text_between(&self, start: (i32, u16), end: (i32, u16)) -> String {
        let mut parser = match self.parser.lock() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        let (rows, cols) = parser.screen().size();
        let height = i32::from(rows);

        // The everyday selection sits wholly on the panel. vt100 reads it in one pass,
        // which is the only path that joins a wrapped line to the row beneath it.
        if start.0 >= 0 && end.0 < height {
            return read_rows(parser.screen(), start, end, cols);
        }

        // Otherwise the selection reaches off the panel. Hold the view so that each
        // chunk in turn starts at the top row, read it, and put the view back.
        let original = parser.screen().scrollback();
        let mut out = String::new();
        let mut row = start.0;
        while row <= end.0 {
            let wanted = i64::try_from(original).unwrap_or(i64::MAX) - i64::from(row);
            parser
                .screen_mut()
                .set_scrollback(usize::try_from(wanted.max(0)).unwrap_or(0));
            // The buffer clamps, so the row may not have landed where it was asked to.
            let top = i64::from(row) + offset_delta_wide(original, parser.screen().scrollback());
            if top >= i64::from(height) {
                break;
            }
            if top < 0 {
                // Older than the scrollback still holds. Skip to the oldest line it has.
                row = row.saturating_add(i32::try_from(-top).unwrap_or(i32::MAX));
                continue;
            }
            let top = i32::try_from(top).unwrap_or(0);
            let last = end.0.min(row + (height - 1 - top));
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if last == end.0 {
                end.1
            } else {
                cols.saturating_sub(1)
            };
            if !out.is_empty() {
                out.push('\n');
            }
            let foot = top + (last - row);
            out.push_str(&read_rows(parser.screen(), (top, from), (foot, to), cols));
            row = last.saturating_add(1);
        }
        parser.screen_mut().set_scrollback(original);
        out
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

/// Reads one run of rows out of the visible screen, both ends included.
///
/// Rows are panel rows, so the caller has already put the view where the text is.
fn read_rows(screen: &vt100::Screen, start: (i32, u16), end: (i32, u16), cols: u16) -> String {
    let first = u16::try_from(start.0).unwrap_or(0);
    let last = u16::try_from(end.0).unwrap_or(0);
    // vt100 counts the end column as exclusive, so the last cell needs one more.
    let after = end.1.saturating_add(1).min(cols);
    screen.contents_between(first, start.1, last, after)
}

/// Hashes one row of the panel. A blank row hashes to zero, because a panel holds
/// many of them and they would otherwise line up at any offset at all.
fn hash_row(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    if text.trim().is_empty() {
        return 0;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    // Zero is the blank marker, so a real row must never take it.
    match hasher.finish() {
        0 => 1,
        h => h,
    }
}

/// How far the view moved between two scrollback offsets. Positive means the text
/// moved down the panel, which is what scrolling up does to it.
fn offset_delta(before: usize, after: usize) -> i32 {
    i32::try_from(offset_delta_wide(before, after)).unwrap_or(0)
}

fn offset_delta_wide(before: usize, after: usize) -> i64 {
    let before = i64::try_from(before).unwrap_or(i64::MAX);
    let after = i64::try_from(after).unwrap_or(i64::MAX);
    after - before
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn offset_delta_is_positive_when_the_view_moves_back() {
        assert_eq!(offset_delta(0, 3), 3);
    }

    #[test]
    fn offset_delta_is_negative_when_the_view_returns_to_the_live_output() {
        assert_eq!(offset_delta(3, 0), -3);
    }

    #[test]
    fn offset_delta_is_zero_when_the_buffer_refused_to_move() {
        assert_eq!(offset_delta(7, 7), 0);
    }
}
