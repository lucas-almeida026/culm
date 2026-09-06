//! Application state. Holds the sessions and decides which one is visible.
//! Every function here is testable without a terminal.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEventKind};

use crate::git::Git;
use crate::hooks::{Attention, HookEvent};
use crate::keys::{HostAction, host_action};
use crate::project::{
    Project, SessionRecord, SessionRepo, SessionState, slugify, system_prompt, unique_slug,
};
use crate::pty::{PtySpawner, SessionSpec};
use crate::session::Session;
use crate::stats::MemoryProbe;
use crate::store::Store;

/// Nine sessions is the limit, because one digit addresses a session.
pub const MAX_ACTIVE: usize = 9;
pub const SIDEBAR_MIN: u16 = 20;
pub const SIDEBAR_MAX: u16 = 60;
pub const SIDEBAR_DEFAULT: u16 = 30;

/// How long a pause waits for a child to leave before it stops asking.
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

/// The side effects the application needs. Passed at the call, never stored, so a
/// test hands over fakes.
pub struct Deps<'a> {
    pub spawner: &'a dyn PtySpawner,
    pub git: &'a dyn Git,
    pub store: &'a dyn Store,
}

impl std::fmt::Debug for Deps<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Deps")
    }
}

/// One session as the interface holds it: what is saved, what is running, and what
/// the sidebar shows.
#[derive(Debug)]
pub struct Entry {
    pub record: SessionRecord,
    pub live: Option<Session>,
    pub attention: Attention,
    pub rss: u64,
}

impl Entry {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.record.is_active()
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.record.name
    }
}

/// What the interface is showing. The shell holds position 0 and owns no record, so
/// it cannot be an index into `entries`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Shell,
    Entry(usize),
}

/// Which field of the new-session form holds the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Field {
    #[default]
    Name,
    Repos,
}

/// The new-session form. While a form is open, no key reaches a child.
#[derive(Debug, Clone, Default)]
pub struct NewSession {
    pub name: String,
    pub chosen: Vec<bool>,
    pub field: Field,
}

#[derive(Debug)]
pub struct App {
    project: Project,
    entries: Vec<Entry>,
    /// The shell at position 0. Not a Claude Code session: no record, no marker, no
    /// transcript, and never saved.
    shell: Option<Session>,
    shell_rss: u64,
    shell_focused: bool,
    entry_focus: usize,
    quit: bool,
    sidebar_width: u16,
    dragging: bool,
    modal: Option<NewSession>,
    status: String,
    hooks_installed: bool,
    nerd_mode: bool,
    fps: f64,
    rows: u16,
    cols: u16,
}

impl App {
    #[must_use]
    pub fn new(project: Project) -> Self {
        Self {
            project,
            entries: Vec::new(),
            shell: None,
            shell_rss: 0,
            shell_focused: false,
            entry_focus: 0,
            quit: false,
            sidebar_width: SIDEBAR_DEFAULT,
            dragging: false,
            modal: None,
            status: String::new(),
            hooks_installed: true,
            nerd_mode: false,
            fps: 0.0,
            rows: 24,
            cols: 80,
        }
    }

    /// Builds the interface from saved state and starts every active session.
    ///
    /// A paused session gets no process. A session that fails to start is reported
    /// and left paused, so one broken session never stops the rest.
    pub fn open(project: Project, deps: &Deps<'_>, rows: u16, cols: u16) -> Self {
        let mut app = Self::new(project);
        app.rows = rows;
        app.cols = cols;
        let records = std::mem::take(&mut app.project.sessions);
        for mut record in records {
            let mut entry = Entry {
                live: None,
                attention: Attention::None,
                rss: 0,
                record: record.clone(),
            };
            if record.is_active() {
                match app.start(&record, deps) {
                    Ok(session) => {
                        record.started = true;
                        entry.record = record;
                        entry.live = Some(session);
                    }
                    Err(e) => {
                        app.status = format!("{}: {e}", record.name);
                        entry.record.state = SessionState::Paused;
                    }
                }
            }
            app.entries.push(entry);
        }
        app.reorder();
        app.spawn_shell(deps);
        app.shell_focused = app.shell.is_some();
        app.save(deps).ok();
        app
    }

    /// Starts the shell at the project root. The shell carries no `CULM_SOCKET`, so a
    /// `claude` the user starts by hand there posts no marker culm cannot attribute.
    fn spawn_shell(&mut self, deps: &Deps<'_>) {
        let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let spec =
            SessionSpec::new("shell", program, &self.project.root).with_size(self.rows, self.cols);
        match Session::spawn(deps.spawner, &spec) {
            Ok(session) => self.shell = Some(session),
            Err(e) => self.status = format!("shell did not start: {e}"),
        }
    }

    fn start(&self, record: &SessionRecord, deps: &Deps<'_>) -> Result<Session> {
        Session::spawn(deps.spawner, &self.spec_for(record))
    }

    /// The command line for one session. A session that ran before resumes its
    /// transcript. A new session takes the id culm generated for it.
    #[must_use]
    pub fn spec_for(&self, record: &SessionRecord) -> SessionSpec {
        let mut args: Vec<String> = if record.started {
            vec!["--resume".into(), record.id.clone()]
        } else {
            vec!["--session-id".into(), record.id.clone()]
        };
        // The session reaches every repository of the project, not only its worktree.
        args.push("--add-dir".into());
        args.push(self.project.root.display().to_string());
        if !record.repos.is_empty() {
            args.push("--append-system-prompt".into());
            args.push(system_prompt(&record.repos));
        }
        SessionSpec::new(&record.name, "claude", &record.cwd)
            .with_args(args)
            .with_env([(
                "CULM_SOCKET",
                crate::hooks::socket_path(&self.project.slug)
                    .display()
                    .to_string(),
            )])
            .with_size(self.rows, self.cols)
    }

    #[must_use]
    pub fn project(&self) -> &Project {
        &self.project
    }

    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_active()).count()
    }

    #[must_use]
    pub fn focus(&self) -> Focus {
        if self.shell_focused {
            Focus::Shell
        } else {
            Focus::Entry(self.entry_focus)
        }
    }

    /// The focused session, or `None` while the shell holds the focus.
    #[must_use]
    pub fn focused_entry(&self) -> Option<usize> {
        (!self.shell_focused).then_some(self.entry_focus)
    }

    #[must_use]
    pub fn shell(&self) -> Option<&Session> {
        self.shell.as_ref()
    }

    #[must_use]
    pub fn shell_rss(&self) -> u64 {
        self.shell_rss
    }

    /// Moves the focus to the shell. Position 0 never swaps and never pauses.
    pub fn focus_shell(&mut self) {
        self.shell_focused = true;
    }

    #[must_use]
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    #[must_use]
    pub fn sidebar_width(&self) -> u16 {
        self.sidebar_width
    }

    #[must_use]
    pub fn modal(&self) -> Option<&NewSession> {
        self.modal.as_ref()
    }

    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn set_hooks_installed(&mut self, installed: bool) {
        self.hooks_installed = installed;
    }

    #[must_use]
    pub fn hooks_installed(&self) -> bool {
        self.hooks_installed
    }

    #[must_use]
    pub fn nerd_mode(&self) -> bool {
        self.nerd_mode
    }

    #[must_use]
    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// The event loop measures the frame rate. Nerd mode renders it.
    pub fn set_fps(&mut self, fps: f64) {
        self.fps = fps;
    }

    /// Focuses a session. An index past the end leaves the focus unchanged.
    pub fn set_focus(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entry_focus = index;
            self.shell_focused = false;
        }
    }

    /// The focused session, or `None` while the shell holds the focus.
    #[must_use]
    pub fn visible(&self) -> Option<&Entry> {
        if self.shell_focused {
            return None;
        }
        self.entries.get(self.entry_focus)
    }

    /// Applies a host action, feeds the form, or forwards the bytes to the visible
    /// session.
    pub fn on_key(&mut self, key: &KeyEvent, deps: &Deps<'_>) -> Result<()> {
        if self.modal.is_some() {
            return self.modal_key(key, deps);
        }
        match host_action(key) {
            Some(HostAction::Quit) => self.shutdown(deps)?,
            Some(HostAction::Focus(i)) => {
                if i < self.active_count() {
                    self.set_focus(i);
                }
            }
            Some(HostAction::NewSession) => self.open_form(),
            Some(HostAction::TogglePause) => self.toggle_pause(deps)?,
            Some(HostAction::NerdMode) => self.nerd_mode = !self.nerd_mode,
            Some(HostAction::FocusShell) => self.focus_shell(),
            None => {
                if let Some(bytes) = crate::keys::encode(key) {
                    self.send_to_focus(&bytes)?;
                }
            }
        }
        Ok(())
    }

    /// Forwards a paste to the visible session as one bracketed block.
    pub fn on_paste(&mut self, text: &str) -> Result<()> {
        if self.modal.is_some() {
            if let Some(form) = self.modal.as_mut()
                && form.field == Field::Name
            {
                form.name.push_str(text);
            }
            return Ok(());
        }
        let bytes = crate::keys::encode_paste(text);
        self.send_to_focus(&bytes)
    }

    /// Forwards bytes to the visible session, and clears a permission marker.
    ///
    /// No hook fires when the user answers a permission prompt, so the keystroke that
    /// answers it is the only signal culm receives. A focus change sends no bytes and
    /// therefore still clears nothing.
    fn send_to_focus(&mut self, bytes: &[u8]) -> Result<()> {
        if self.shell_focused {
            if let Some(shell) = self.shell.as_mut() {
                shell.send(bytes)?;
            }
            return Ok(());
        }
        let Some(entry) = self.entries.get_mut(self.entry_focus) else {
            return Ok(());
        };
        if entry.attention == Attention::NeedsPermission {
            entry.attention = Attention::None;
        }
        if let Some(session) = entry.live.as_mut() {
            session.send(bytes)?;
        }
        Ok(())
    }

    fn open_form(&mut self) {
        if self.active_count() >= MAX_ACTIVE {
            self.status = format!("{MAX_ACTIVE} active sessions is the limit. pause one first.");
            return;
        }
        self.status.clear();
        self.modal = Some(NewSession {
            name: String::new(),
            chosen: vec![false; self.project.repos.len()],
            field: Field::Name,
        });
    }

    fn modal_key(&mut self, key: &KeyEvent, deps: &Deps<'_>) -> Result<()> {
        use ratatui::crossterm::event::KeyCode;
        let Some(form) = self.modal.as_mut() else {
            return Ok(());
        };
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Tab | KeyCode::BackTab => {
                form.field = match form.field {
                    Field::Name => Field::Repos,
                    Field::Repos => Field::Name,
                };
            }
            KeyCode::Enter => {
                let form = form.clone();
                self.modal = None;
                self.create_session(&form, deps)?;
            }
            KeyCode::Backspace if form.field == Field::Name => {
                form.name.pop();
            }
            KeyCode::Char(c @ '1'..='9') if form.field == Field::Repos => {
                let i = c as usize - '1' as usize;
                if let Some(slot) = form.chosen.get_mut(i) {
                    *slot = !*slot;
                }
            }
            KeyCode::Char(c) if form.field == Field::Name => form.name.push(c),
            _ => {}
        }
        Ok(())
    }

    /// Creates the worktrees, then the session. A git failure stops before the spawn,
    /// so a half-made session never reaches the list.
    pub fn create_session(&mut self, form: &NewSession, deps: &Deps<'_>) -> Result<()> {
        if self.active_count() >= MAX_ACTIVE {
            self.status = format!("{MAX_ACTIVE} active sessions is the limit. pause one first.");
            return Ok(());
        }
        let name = form.name.trim();
        if name.is_empty() {
            self.status = "a session needs a name".into();
            return Ok(());
        }
        let taken: Vec<String> = self.entries.iter().map(|e| e.record.slug.clone()).collect();
        let slug = unique_slug(
            &slugify(name),
            &taken.iter().map(String::as_str).collect::<Vec<_>>(),
        );

        let mut repos = Vec::new();
        for (i, chosen) in form.chosen.iter().enumerate() {
            if !chosen {
                continue;
            }
            let Some(repo) = self.project.repos.get(i) else {
                continue;
            };
            let worktree = self.project.worktree_dir(&repo.name, &slug);
            if let Err(e) = deps.git.worktree_add(&repo.path, &worktree, &slug) {
                self.status = format!("git worktree failed for {}: {e}", repo.name);
                return Ok(());
            }
            repos.push(SessionRepo {
                name: repo.name.clone(),
                worktree,
                branch: slug.clone(),
            });
        }

        let cwd = repos
            .first()
            .map_or_else(|| self.project.root.clone(), |r| r.worktree.clone());
        let mut record = SessionRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            slug,
            state: SessionState::Active,
            cwd,
            repos,
            started: false,
        };

        match self.start(&record, deps) {
            Ok(session) => {
                record.started = true;
                self.entries.push(Entry {
                    record,
                    live: Some(session),
                    attention: Attention::None,
                    rss: 0,
                });
                self.reorder();
                self.focus_slug_of_last_active();
                self.status.clear();
                self.save(deps)?;
            }
            Err(e) => self.status = format!("{name} did not start: {e}"),
        }
        Ok(())
    }

    fn focus_slug_of_last_active(&mut self) {
        if let Some(i) = self.entries.iter().rposition(Entry::is_active) {
            self.entry_focus = i;
        }
    }

    /// Pauses the focused session, or resumes it when it is already paused.
    pub fn toggle_pause(&mut self, deps: &Deps<'_>) -> Result<()> {
        if self.shell_focused {
            return Ok(());
        }
        let Some(entry) = self.entries.get(self.entry_focus) else {
            return Ok(());
        };
        let record = entry.record.clone();
        let running = entry.live.is_some();

        if running {
            let Some(entry) = self.entries.get_mut(self.entry_focus) else {
                return Ok(());
            };
            if let Some(mut session) = entry.live.take() {
                session.terminate().ok();
                session.wait_for_exit(TERMINATE_GRACE);
            }
            entry.record.state = SessionState::Paused;
            entry.attention = Attention::None;
            entry.rss = 0;
            self.status = format!("{} paused", record.name);
        } else {
            if self.active_count() >= MAX_ACTIVE {
                self.status =
                    format!("{MAX_ACTIVE} active sessions is the limit. pause one first.");
                return Ok(());
            }
            match self.start(&record, deps) {
                Ok(session) => {
                    let Some(entry) = self.entries.get_mut(self.entry_focus) else {
                        return Ok(());
                    };
                    entry.record.started = true;
                    entry.record.state = SessionState::Active;
                    entry.live = Some(session);
                    self.status = format!("{} resumed", record.name);
                }
                Err(e) => {
                    self.status = format!("{} did not resume: {e}", record.name);
                    return Ok(());
                }
            }
        }

        self.reorder();
        self.focus_id(&record.id);
        self.save(deps)
    }

    /// Once a second: reap a child that ended on its own, and sample memory.
    pub fn on_tick(&mut self, probe: &dyn MemoryProbe, deps: &Deps<'_>) -> Result<()> {
        let mut reaped = false;
        for entry in &mut self.entries {
            let Some(session) = entry.live.as_mut() else {
                continue;
            };
            if session.has_exited() {
                entry.live = None;
                entry.record.state = SessionState::Paused;
                entry.rss = 0;
                reaped = true;
                continue;
            }
            entry.rss = session.pid().map_or(0, |pid| probe.rss_tree(pid));
        }

        // Position 0 always holds a live shell, so an exited one is replaced here.
        let restart = match self.shell.as_mut() {
            Some(shell) => {
                if shell.has_exited() {
                    true
                } else {
                    self.shell_rss = shell.pid().map_or(0, |pid| probe.rss_tree(pid));
                    false
                }
            }
            None => true,
        };
        if restart {
            self.shell = None;
            self.shell_rss = 0;
            self.spawn_shell(deps);
        }
        if reaped {
            let id = self
                .entries
                .get(self.entry_focus)
                .map(|e| e.record.id.clone());
            self.reorder();
            if let Some(id) = id {
                self.focus_id(&id);
            }
            self.save(deps)?;
        }
        Ok(())
    }

    /// Marks the session the hook belongs to. A payload for an unknown session is
    /// dropped, because every Claude Code session on the machine runs the hook.
    pub fn on_hook(&mut self, event: &HookEvent) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.record.id == event.session_id)
        {
            entry.attention = entry.attention.apply(event);
        }
    }

    /// Ends every child and saves. Active sessions stay active, so the next open
    /// resumes them.
    pub fn shutdown(&mut self, deps: &Deps<'_>) -> Result<()> {
        for entry in &mut self.entries {
            if let Some(session) = entry.live.as_mut() {
                session.terminate().ok();
            }
        }
        let deadline = Instant::now() + TERMINATE_GRACE;
        for entry in &mut self.entries {
            let Some(session) = entry.live.as_mut() else {
                continue;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            if !session.wait_for_exit(left) {
                session.kill().ok();
            }
        }
        for entry in &mut self.entries {
            entry.live = None;
        }
        if let Some(shell) = self.shell.as_mut() {
            shell.terminate().ok();
            if !shell.wait_for_exit(TERMINATE_GRACE) {
                shell.kill().ok();
            }
        }
        self.shell = None;
        self.quit = true;
        self.save(deps)
    }

    /// Maps a click or a drag onto the sidebar.
    pub fn on_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        hit: &crate::ui::HitBox,
    ) {
        match kind {
            MouseEventKind::Down(MouseButton::Left) if column == hit.separator_col => {
                self.dragging = true;
            }
            MouseEventKind::Down(MouseButton::Left) if column < hit.sidebar_width => {
                match hit.rows.iter().find(|(r, _)| *r == row) {
                    Some((_, Focus::Shell)) => self.focus_shell(),
                    Some((_, Focus::Entry(index))) => self.set_focus(*index),
                    None => {}
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                self.sidebar_width = column.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = false,
            _ => {}
        }
    }

    /// Resizes every running session to the panel. Sessions that are not visible are
    /// resized as well, so a switch never shows a stale layout.
    pub fn resize_all(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.rows = rows;
        self.cols = cols;
        for entry in &mut self.entries {
            if let Some(session) = entry.live.as_mut() {
                session.resize(rows, cols)?;
            }
        }
        if let Some(shell) = self.shell.as_mut() {
            shell.resize(rows, cols)?;
        }
        Ok(())
    }

    /// Active sessions first, paused sessions after, each half in its own order.
    fn reorder(&mut self) {
        let focused = self
            .entries
            .get(self.entry_focus)
            .map(|e| e.record.id.clone());
        let mut active: Vec<Entry> = Vec::new();
        let mut paused: Vec<Entry> = Vec::new();
        for entry in self.entries.drain(..) {
            if entry.is_active() {
                active.push(entry);
            } else {
                paused.push(entry);
            }
        }
        active.append(&mut paused);
        self.entries = active;
        if let Some(id) = focused {
            self.focus_id(&id);
        }
        if self.entry_focus >= self.entries.len() {
            self.entry_focus = self.entries.len().saturating_sub(1);
        }
    }

    fn focus_id(&mut self, id: &str) {
        if let Some(i) = self.entries.iter().position(|e| e.record.id == id) {
            self.entry_focus = i;
        }
    }

    /// Writes the session list after every change, so an unexpected exit costs
    /// nothing.
    fn save(&mut self, deps: &Deps<'_>) -> Result<()> {
        self.project.sessions = self.entries.iter().map(|e| e.record.clone()).collect();
        deps.store.save_project(&self.project)
    }
}
