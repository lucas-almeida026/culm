//! Application state. Holds the sessions and decides which one is visible.
//! Every function here is testable without a terminal.

use anyhow::Result;

use crate::keys::{HostAction, host_action};
use crate::pty::{PtySpawner, SessionSpec};
use crate::session::Session;

#[derive(Debug)]
pub struct App {
    sessions: Vec<Session>,
    focus: usize,
    quit: bool,
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            focus: 0,
            quit: false,
        }
    }

    /// Adds a session. The spawner is passed in, so tests supply a fake.
    pub fn add_session(&mut self, spawner: &dyn PtySpawner, spec: &SessionSpec) -> Result<()> {
        self.sessions.push(Session::spawn(spawner, spec)?);
        Ok(())
    }

    #[must_use]
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    #[must_use]
    pub fn focus(&self) -> usize {
        self.focus
    }

    #[must_use]
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// Focuses a session. An index past the end leaves the focus unchanged.
    pub fn set_focus(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.focus = index;
        }
    }

    #[must_use]
    pub fn visible(&self) -> Option<&Session> {
        self.sessions.get(self.focus)
    }

    /// Applies a host action, or forwards the bytes to the visible session.
    pub fn on_key(&mut self, key: &ratatui::crossterm::event::KeyEvent) -> Result<()> {
        match host_action(key) {
            Some(HostAction::Quit) => self.quit = true,
            Some(HostAction::Focus(i)) => self.set_focus(i),
            None => {
                if let Some(bytes) = crate::keys::encode(key)
                    && let Some(s) = self.sessions.get_mut(self.focus)
                {
                    s.send(&bytes)?;
                }
            }
        }
        Ok(())
    }

    /// Maps a click in the sidebar to a session. `first_row` is the screen row of
    /// the first session entry, refreshed on every draw.
    pub fn on_click(&mut self, column: u16, row: u16, sidebar_width: u16, first_row: u16) {
        if column < sidebar_width && row >= first_row {
            self.set_focus((row - first_row) as usize);
        }
    }

    /// Resizes every session to the panel. Sessions that are not visible are resized
    /// as well, so a switch never shows a stale layout.
    pub fn resize_all(&mut self, rows: u16, cols: u16) -> Result<()> {
        for s in &mut self.sessions {
            s.resize(rows, cols)?;
        }
        Ok(())
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
