//! Application state. Holds the sessions and decides which one is visible.
//! Every function here is testable without a terminal.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::clock::Clock;
use crate::config::Config;
use crate::git::Git;
use crate::gitview::{GitKey, GitSplit, GitView, Owner, RepoTab};
use crate::hooks::{Attention, HookEvent};
use crate::keys::{HostAction, host_action};
use crate::project::{Project, SessionRecord, SessionState, slugify, system_prompt, unique_slug};
use crate::pty::{PtySpawner, SessionSpec};
use crate::recap::{RECAP_UNAVAILABLE, RecapJob, Recapper};
use crate::session::Session;
use crate::stats::MemoryProbe;
use crate::store::Store;

/// Eight active sessions per project is the limit.
pub const MAX_ACTIVE: usize = 8;

// The sidebar limits live with the project, because a project now saves its width.
pub use crate::project::{SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN};

/// Lines one notch of the wheel moves the view.
const WHEEL_LINES: i32 = 3;

/// Passes a forwarded wheel notch waits for the child to repaint. The loop runs
/// far faster than a child draws, so this is a fraction of a second.
const SCROLL_SETTLE_TRIES: u32 = 40;

/// The share of a panel's written rows that must line up for a repaint to count as
/// a scroll rather than as a fresh screen.
const SCROLL_MATCH_SHARE: usize = 4;

/// How long a pause waits for a child to leave before it stops asking.
const TERMINATE_GRACE: Duration = Duration::from_secs(3);

/// Seconds a quit gives every child, the shell included, to leave on its own. One
/// grace for all of them rather than one each, so a quit never takes twice as long.
const CLOSE_GRACE_SECS: u64 = 3;

/// Passes between spinner frames. The loop runs every few milliseconds, so this
/// lands near a tenth of a second, which is readable.
const SPINNER_EVERY: u64 = 20;

/// Seconds a recap may take before culm gives up on it. The model reads a whole
/// transcript, and a long one is slow.
const RECAP_LIMIT_SECS: u64 = 45;

/// The switches the settings form shows, one per row.
pub const SETTINGS_ROWS: usize = 4;

/// How long the report of a forced quit stays on the bar, in seconds.
const FORCED_QUIT_NOTICE_SECS: u64 = 5;

/// Rows above a copy that break a tie when its words appear twice in the transcript.
const COPY_CONTEXT_ROWS: i32 = 6;

/// What the status bar shows after culm kept `Ctrl+z` from a session.
pub const CTRL_Z_BLOCKED: &str =
    "ctrl+z would suspend claude, so culm kept it. culm config set block-ctrl-z false sends it.";

/// How long that notice stays, in seconds.
const CTRL_Z_NOTICE_SECS: u64 = 5;

/// True for `Ctrl+z`, with or without shift.
fn is_ctrl_z(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(
            key.code,
            ratatui::crossterm::event::KeyCode::Char('z' | 'Z')
        )
}

/// True for `Ctrl+v` with no other modifier.
fn is_ctrl_v(key: &KeyEvent) -> bool {
    key.modifiers == KeyModifiers::CONTROL
        && key.code == ratatui::crossterm::event::KeyCode::Char('v')
}

/// What the status bar shows when `Alt+Shift+Z` finds no paste to take back.
pub const UNDO_NO_PASTE: &str = "the last edit is not a paste, so nothing was undone.";
/// What it shows when `Alt+Shift+Z` reaches the shell.
pub const UNDO_NO_SHELL: &str = "alt+shift+z undoes a paste in a claude code session only.";
/// How long either notice stays, in seconds.
const UNDO_NOTICE_SECS: u64 = 5;

/// What the status bar shows while the last session to start has no wanted microphone.
pub const NO_MIC: &str = "no microphone matched. voice dictation uses the system default.";

/// The side effects the application needs. Passed at the call, never stored, so a
/// test hands over fakes.
pub struct Deps<'a> {
    pub spawner: &'a dyn PtySpawner,
    pub git: &'a dyn Git,
    pub store: &'a dyn Store,
    pub clock: &'a dyn Clock,
    pub recapper: &'a dyn Recapper,
    pub hooks: &'a dyn crate::hooks::HookListener,
    pub mic: &'a dyn crate::mic::Microphones,
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

/// Which half of the screen holds the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    /// The session list. Keys move a highlight and never reach a child.
    Sidebar,
    /// The visible session. Keys reach its child.
    #[default]
    Panel,
    /// The changes panel. Keys move through its lists and its diff.
    Git,
}

/// The sidebar row under the keyboard highlight. A session is held by id, because
/// the list reorders as sessions pause and resume.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarRow {
    Shell,
    Session(String),
}

/// The new-session form. While a form is open, no key reaches a child.
///
/// It asks for a name only. A session starts at the project root, and registers its
/// worktrees later, when Claude Code has made them.
#[derive(Debug, Clone, Default)]
pub struct NewSession {
    pub name: String,
}

/// Which button of a confirmation holds the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Answer {
    Yes,
    /// The default. Deletion has no undo, so a stray `Enter` cancels.
    #[default]
    No,
}

/// The delete confirmation. Two buttons, and the cursor starts on `No`.
#[derive(Debug, Clone)]
pub struct ConfirmDelete {
    pub index: usize,
    pub name: String,
    pub answer: Answer,
    /// The archive list the question came from. It returns after the answer.
    pub archive: Option<ArchiveList>,
}

impl ConfirmDelete {
    #[must_use]
    pub fn confirmed(&self) -> bool {
        self.answer == Answer::Yes
    }
}

/// The rename form. The slug, the branch, and the worktree keep the names they were
/// created with, because a path that moved would break a running session.
#[derive(Debug, Clone)]
pub struct Rename {
    pub index: usize,
    pub name: String,
}

/// A drag over the panel. Both ends are `(row, column)`, counted inside the panel
/// border, so a resize of the sidebar never moves a selection that is already made.
///
/// The row is signed because culm carries a selection when it scrolls the panel, and
/// text the user marked then scrolled past sits above the first row or below the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (i32, u16),
    pub head: (i32, u16),
}

/// A quit in progress. The loop keeps drawing while the children leave, so the
/// screen shows what is happening instead of freezing.
#[derive(Debug, Clone)]
struct Closing {
    /// Unix seconds past which a child that has not left is killed.
    deadline: u64,
    /// Passes since the quit. This moves the spinner, so it needs no clock.
    polls: u64,
    /// True when the user asked twice and the wait was cut short.
    forced: bool,
}

/// A recap being fetched for one paused session.
#[derive(Debug)]
struct RecapWork {
    /// The session the answer belongs to. An index would not survive a reorder.
    id: String,
    name: String,
    job: Box<dyn RecapJob>,
    /// Unix seconds past which the work is abandoned.
    deadline: u64,
}

/// A wheel notch culm handed to a child, waiting to be measured.
///
/// The child repaints when it pleases, so the distance its text moved is not known
/// at the moment the notch goes out. The rows as they were are kept until the panel
/// changes, and the two are then lined up against each other.
#[derive(Debug, Clone)]
struct PendingScroll {
    before: Vec<u64>,
    /// The same rows as text, so the rows the child moved can be kept.
    before_text: Vec<String>,
    /// Passes spent waiting. A child that ignores the wheel must not hold a
    /// selection for ever.
    tries: u32,
}

/// One wheel notch waiting to go to a self-scrolling child.
#[derive(Debug, Clone, Copy)]
struct HeldNotch {
    up: bool,
    column: u16,
    row: u16,
    panel: Rect,
}

/// The most notches culm holds. A flick past this many is cut short.
const HELD_NOTCHES: usize = 64;

/// The archive list. It holds its own search, apart from the paused list's, so the
/// two never filter each other.
#[derive(Debug, Clone, Default)]
pub struct ArchiveList {
    pub filter: String,
    /// The highlighted row, counted among the matches of `filter`.
    pub cursor: usize,
    /// Why the last restore failed. Shown inside the list, where the user is
    /// looking, and cleared by the next key.
    pub notice: Option<String>,
}

/// The form on top of the panel, if any.
#[derive(Debug, Clone)]
pub enum Modal {
    NewSession(NewSession),
    ConfirmDelete(ConfirmDelete),
    Rename(Rename),
    /// The shortcut table. It reads state and changes none, so it holds no data.
    Help,
    /// The settings that apply to every project. One switch so far, so the cursor
    /// has nowhere else to be and the form holds no data either.
    Settings,
    Archive(ArchiveList),
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
    modal: Option<Modal>,
    status_text: String,
    hooks_installed: bool,
    /// True when the host terminal reports the keyboard enhancement. The bar shows
    /// which mode is live, because the two paths encode a modified key differently.
    keyboard_enhanced: bool,
    nerd_mode: bool,
    fps: f64,
    rows: u16,
    cols: u16,
    /// The clock reading of the most recent key or tick. A keystroke stamps the
    /// session it reaches with this, so the paused list orders by last use without
    /// reading the clock on every byte.
    now: u64,
    /// True when a record changed but the change has not reached the store. The tick
    /// writes it, so typing never costs a file write.
    dirty: bool,
    selection: Option<Selection>,
    /// A forwarded wheel notch that has not been measured yet.
    pending_scroll: Option<PendingScroll>,
    /// Wheel notches for a self-scrolling child that wait for the last one to be
    /// measured. Notches sent together move the text further than the panel is
    /// tall, and two panels that far apart share no row to line up.
    held_wheel: std::collections::VecDeque<HeldNotch>,
    /// The rows a self-scrolling child moved off the panel while a selection was
    /// held, so a copy reaches them.
    seen: crate::seen::SeenRows,
    /// The quit in progress, if any.
    closing: Option<Closing>,
    /// Settings shared by every project.
    config: Config,
    /// The recap being fetched, if any. One at a time.
    recap_work: Option<RecapWork>,
    /// Unix seconds after which the status message stops showing. A message with no
    /// deadline stays until something replaces it.
    status_until: Option<u64>,
    /// The last session to start found no wanted microphone.
    mic_missing: bool,
    /// The input of each Claude Code session, by id, for the undo of a paste.
    pastes: std::collections::HashMap<String, crate::paste::PasteTrack>,
    /// The last copy. A middle click pastes it, and it survives a focus change so a
    /// copy in one session pastes into another.
    clipboard: String,
    /// Text the event loop has yet to hand to the terminal through OSC 52.
    copied: Option<String>,
    /// What the paused list is filtered by. Empty means every paused session.
    filter: String,
    /// True while keys go to the search box rather than to a child.
    filter_focused: bool,
    /// Which half of the screen the keys go to while the search box is not focused.
    pane: Pane,
    /// The highlighted sidebar row. `None` follows whatever the panel shows.
    sidebar_cursor: Option<SidebarRow>,
    /// The changes panel, which follows the visible session.
    git: GitView,
    /// True while the left border of the changes panel is dragged.
    dragging_git: bool,
    /// True while the rule right of the changes panel's list is dragged.
    dragging_git_list: bool,
    /// The settings row under the cursor.
    settings_cursor: usize,
}

impl App {
    #[must_use]
    pub fn new(project: Project) -> Self {
        Self {
            // Read before the project moves into the struct.
            sidebar_width: project.sidebar_width(),
            git: {
                let mut git = GitView::default();
                git.restore(project.view.changes_width, project.view.changes_list);
                git
            },
            project,
            entries: Vec::new(),
            shell: None,
            shell_rss: 0,
            shell_focused: false,
            entry_focus: 0,
            quit: false,
            dragging: false,
            modal: None,
            status_text: String::new(),
            hooks_installed: true,
            keyboard_enhanced: false,
            nerd_mode: false,
            fps: 0.0,
            rows: 24,
            cols: 80,
            now: 0,
            dirty: false,
            selection: None,
            pending_scroll: None,
            held_wheel: std::collections::VecDeque::new(),
            seen: crate::seen::SeenRows::default(),
            closing: None,
            config: Config::default(),
            recap_work: None,
            status_until: None,
            mic_missing: false,
            pastes: std::collections::HashMap::new(),
            clipboard: String::new(),
            copied: None,
            filter: String::new(),
            filter_focused: false,
            pane: Pane::Panel,
            sidebar_cursor: None,
            dragging_git: false,
            dragging_git_list: false,
            settings_cursor: 0,
        }
    }

    /// Builds the interface from saved state and starts every active session.
    ///
    /// A paused session gets no process. A session that fails to start is reported
    /// and left paused, so one broken session never stops the rest. A project saved
    /// with more active sessions than the limit, by an older culm, starts the first
    /// `MAX_ACTIVE` and leaves the rest paused.
    pub fn open(project: Project, deps: &Deps<'_>, rows: u16, cols: u16) -> Self {
        let mut app = Self::new(project);
        app.rows = rows;
        app.cols = cols;
        app.now = deps.clock.now_secs();
        let records = std::mem::take(&mut app.project.sessions);
        let mut failed = Vec::new();
        let mut over_limit = 0usize;
        let mut started = 0usize;
        for mut record in records {
            let mut entry = Entry {
                live: None,
                attention: Attention::None,
                rss: 0,
                record: record.clone(),
            };
            if record.is_active() && started >= MAX_ACTIVE {
                entry.record.state = SessionState::Paused;
                failed.push(record.id.clone());
                over_limit += 1;
            } else if record.is_active() {
                match app.start(&record, deps) {
                    Ok(session) => {
                        started += 1;
                        record.started = true;
                        entry.record = record;
                        entry.live = Some(session);
                    }
                    Err(e) => {
                        app.status_text = format!("{}: {e}", record.name);
                        entry.record.state = SessionState::Paused;
                        failed.push(record.id.clone());
                    }
                }
            }
            app.entries.push(entry);
        }
        for id in &failed {
            app.lift(id);
        }
        if over_limit > 0 {
            app.status_text = format!(
                "{over_limit} sessions opened paused: {MAX_ACTIVE} active sessions is the limit"
            );
        }
        app.reorder();
        app.spawn_shell(deps);
        app.shell_focused = app.shell.is_some();
        if app.project.forced_quit {
            app.project.forced_quit = false;
            app.set_status_for("recovered from forced quit", FORCED_QUIT_NOTICE_SECS);
        }
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
            Err(e) => self.status_text = format!("shell did not start: {e}"),
        }
    }

    /// Starts one session on the wanted microphone. The sound server is asked at each
    /// start, so a microphone plugged in later reaches the next session to start.
    fn start(&mut self, record: &SessionRecord, deps: &Deps<'_>) -> Result<Session> {
        // A new process starts with an empty prompt, so no paste is left to undo.
        self.pastes.remove(&record.id);
        let mut spec = self.spec_for(record);
        let sources = deps.mic.sources().unwrap_or_default();
        self.mic_missing = match crate::mic::pick(&sources) {
            Some(name) => {
                spec.env
                    .push((crate::mic::PULSE_SOURCE.into(), name.into()));
                false
            }
            None => true,
        };
        Session::spawn(deps.spawner, &spec)
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
        let prompt = system_prompt(record);
        if !prompt.is_empty() {
            args.push("--append-system-prompt".into());
            args.push(prompt);
        }
        // `culm session` reads the last two, so a command run inside the session
        // needs no flag to name it.
        SessionSpec::new(&record.name, "claude", &record.cwd)
            .with_args(args)
            .with_env([
                (
                    "CULM_SOCKET",
                    crate::hooks::socket_path(&self.project.slug)
                        .display()
                        .to_string(),
                ),
                (crate::cli::SESSION_VAR, record.id.clone()),
                (crate::cli::PROJECT_VAR, self.project.slug.clone()),
            ])
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

    /// Moves the focus to the shell. Position 0 never pauses and never closes.
    pub fn focus_shell(&mut self) {
        self.shell_focused = true;
        self.selection = None;
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
    pub fn modal(&self) -> Option<&Modal> {
        self.modal.as_ref()
    }

    /// The new-session form, when that is the form on screen.
    #[must_use]
    pub fn new_session_form(&self) -> Option<&NewSession> {
        match self.modal.as_ref() {
            Some(Modal::NewSession(form)) => Some(form),
            _ => None,
        }
    }

    /// The delete confirmation, when that is the form on screen.
    #[must_use]
    pub fn confirm_delete(&self) -> Option<&ConfirmDelete> {
        match self.modal.as_ref() {
            Some(Modal::ConfirmDelete(confirm)) => Some(confirm),
            _ => None,
        }
    }

    /// The selection, ordered so that the first end comes first on screen.
    #[must_use]
    pub fn selection(&self) -> Option<((i32, u16), (i32, u16))> {
        let s = self.selection?;
        Some(if s.anchor <= s.head {
            (s.anchor, s.head)
        } else {
            (s.head, s.anchor)
        })
    }

    /// Takes the text waiting to reach the terminal's clipboard. The event loop calls
    /// this after every mouse event, because only the loop owns the output stream.
    pub fn take_copy(&mut self) -> Option<String> {
        self.copied.take()
    }

    /// What the paused list is filtered by.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// True while the search box holds the keys.
    #[must_use]
    pub fn filter_focused(&self) -> bool {
        self.filter_focused
    }

    /// Which half of the screen holds the keys. The search box sits in the sidebar,
    /// so the sidebar holds them while the box does.
    #[must_use]
    pub fn pane(&self) -> Pane {
        if self.filter_focused {
            Pane::Sidebar
        } else {
            self.pane
        }
    }

    /// Moves the keys to one half of the screen. Entering the sidebar puts the
    /// highlight on the row the panel shows, so the keyboard starts from there.
    pub fn set_pane(&mut self, pane: Pane) {
        if pane == Pane::Sidebar && self.pane() != Pane::Sidebar {
            self.sidebar_cursor = Some(self.row_of(self.focus()));
        }
        if pane != Pane::Sidebar {
            // The search box is part of the sidebar, so it lets go of the keys too.
            self.filter_focused = false;
        }
        self.pane = pane;
    }

    /// Every row the sidebar lists, in screen order: the shell, the active sessions,
    /// and the paused sessions the search admits.
    #[must_use]
    pub fn sidebar_rows(&self) -> Vec<Focus> {
        let mut rows = vec![Focus::Shell];
        rows.extend(
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.is_active())
                .map(|(i, _)| Focus::Entry(i)),
        );
        rows.extend(self.paused_matches().into_iter().map(Focus::Entry));
        rows
    }

    /// The row under the keyboard highlight, while the sidebar holds the keys and the
    /// search box does not.
    #[must_use]
    pub fn sidebar_cursor(&self) -> Option<Focus> {
        if self.pane != Pane::Sidebar || self.filter_focused {
            return None;
        }
        let rows = self.sidebar_rows();
        let wanted = match self.sidebar_cursor.as_ref() {
            Some(SidebarRow::Shell) => Some(Focus::Shell),
            Some(SidebarRow::Session(id)) => self
                .entries
                .iter()
                .position(|e| &e.record.id == id)
                .map(Focus::Entry),
            None => None,
        };
        // A row that left the list, by an archive or a search, hands the highlight
        // back to the row the panel shows.
        match wanted {
            Some(row) if rows.contains(&row) => Some(row),
            _ => Some(self.focus()),
        }
    }

    fn row_of(&self, focus: Focus) -> SidebarRow {
        match focus {
            Focus::Shell => SidebarRow::Shell,
            Focus::Entry(i) => self.entries.get(i).map_or(SidebarRow::Shell, |e| {
                SidebarRow::Session(e.record.id.clone())
            }),
        }
    }

    /// Moves the highlight one row up or down, stopping at either end.
    fn move_sidebar_cursor(&mut self, down: bool) {
        let rows = self.sidebar_rows();
        let Some(current) = self.sidebar_cursor() else {
            return;
        };
        let at = rows.iter().position(|r| *r == current).unwrap_or(0);
        let next = if down {
            (at + 1).min(rows.len().saturating_sub(1))
        } else {
            at.saturating_sub(1)
        };
        if let Some(&row) = rows.get(next) {
            self.sidebar_cursor = Some(self.row_of(row));
        }
    }

    /// Swaps the highlighted session with its neighbor on screen, one row up or down.
    ///
    /// The neighbor must sit in the same list. The shell above the first active row
    /// and the border between the active and the paused list both stop the move, so
    /// a session never changes list this way. Inside a searched paused list the
    /// neighbor is the next match on screen, and the sessions the search hides keep
    /// their places.
    fn move_highlighted(&mut self, down: bool) {
        let rows = self.sidebar_rows();
        let Some(Focus::Entry(from)) = self.sidebar_cursor() else {
            return;
        };
        let Some(at) = rows.iter().position(|r| *r == Focus::Entry(from)) else {
            return;
        };
        let neighbor = if down {
            rows.get(at + 1)
        } else {
            at.checked_sub(1).and_then(|i| rows.get(i))
        };
        let Some(&Focus::Entry(to)) = neighbor else {
            return;
        };
        let same_list = match (self.entries.get(from), self.entries.get(to)) {
            (Some(a), Some(b)) => a.record.state == b.record.state,
            _ => false,
        };
        if !same_list {
            return;
        }
        let focused = self
            .entries
            .get(self.entry_focus)
            .map(|e| e.record.id.clone());
        self.entries.swap(from, to);
        if let Some(id) = focused {
            self.focus_id(&id);
        }
        // The order is saved on the next tick, as a keystroke's time is.
        self.dirty = true;
    }

    /// Feeds the sidebar. No key reaches a child while the sidebar holds the keys.
    fn sidebar_key(&mut self, key: &KeyEvent) {
        use ratatui::crossterm::event::KeyCode;
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up if shift => self.move_highlighted(false),
            KeyCode::Down if shift => self.move_highlighted(true),
            KeyCode::Char('K') => self.move_highlighted(false),
            KeyCode::Char('J') => self.move_highlighted(true),
            KeyCode::Up | KeyCode::Char('k') => self.move_sidebar_cursor(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_sidebar_cursor(true),
            KeyCode::Enter => {
                match self.sidebar_cursor() {
                    Some(Focus::Shell) => self.focus_shell(),
                    Some(Focus::Entry(index)) => self.set_focus(index),
                    None => {}
                }
                self.reveal_chosen();
                self.set_pane(Pane::Panel);
            }
            KeyCode::Esc => self.set_pane(Pane::Panel),
            // Only a deliberate key starts a search. A letter typed while the user
            // believes the session holds the keys would otherwise land in the box.
            KeyCode::Char('/') => self.filter_focused = true,
            _ => {}
        }
    }

    /// The archive list, when that is the form on screen.
    #[must_use]
    pub fn archive_list(&self) -> Option<&ArchiveList> {
        match self.modal.as_ref() {
            Some(Modal::Archive(list)) => Some(list),
            _ => None,
        }
    }

    /// The archived entries whose name holds `filter`, ignoring case, newest first.
    #[must_use]
    pub fn archived_matches(&self, filter: &str) -> Vec<usize> {
        let needle = filter.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.record.is_archived())
            .filter(|(_, entry)| needle.is_empty() || entry.name().to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect()
    }

    /// The paused entries the filter admits, in list order.
    ///
    /// The interface and the search box both read this, so what `Enter` lands on is
    /// always the first row on screen.
    #[must_use]
    pub fn paused_matches(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.record.is_paused())
            .filter(|(_, entry)| needle.is_empty() || entry.name().to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect()
    }

    /// Replaces the last copy, so a copy made in another tab pastes here too.
    pub fn set_clipboard(&mut self, text: String) {
        self.clipboard = text;
    }

    /// The last copy, which a middle click pastes.
    #[must_use]
    pub fn clipboard(&self) -> &str {
        &self.clipboard
    }

    /// True while the shortcut table is on screen.
    #[must_use]
    pub fn help_open(&self) -> bool {
        matches!(self.modal, Some(Modal::Help))
    }

    /// The settings row under the cursor.
    #[must_use]
    pub fn settings_cursor(&self) -> usize {
        self.settings_cursor
    }

    /// True while the settings are on screen.
    #[must_use]
    pub fn settings_open(&self) -> bool {
        matches!(self.modal, Some(Modal::Settings))
    }

    /// The rename form, when that is the form on screen.
    #[must_use]
    pub fn rename_form(&self) -> Option<&Rename> {
        match self.modal.as_ref() {
            Some(Modal::Rename(rename)) => Some(rename),
            _ => None,
        }
    }

    #[must_use]
    pub fn status(&self) -> &str {
        let text = match self.status_until {
            // A timed message retires on its own, so a report of the last run never
            // sits on the bar for the whole session.
            Some(until) if self.now > until => "",
            _ => &self.status_text,
        };
        // The warning waits under every other message, because the message that
        // follows a start would hide it at once.
        if text.is_empty() && self.mic_missing {
            NO_MIC
        } else {
            text
        }
    }

    /// Shows a message for a fixed number of seconds.
    fn set_status_for(&mut self, text: impl Into<String>, secs: u64) {
        self.status_text = text.into();
        self.status_until = Some(self.now.saturating_add(secs));
    }

    /// Shows a message until something replaces it.
    fn set_status(&mut self, text: impl Into<String>) {
        self.status_text = text.into();
        self.status_until = None;
    }

    fn clear_status(&mut self) {
        self.status_text.clear();
        self.status_until = None;
    }

    /// True while the interface is closing. The loop keeps drawing, and the screen
    /// shows the spinner rather than the sessions.
    #[must_use]
    pub fn closing(&self) -> bool {
        self.closing.is_some()
    }

    /// Which spinner frame the closing screen is on, out of `frames`.
    #[must_use]
    pub fn spinner_frame(&self, frames: usize) -> usize {
        let polls = self.closing.as_ref().map_or(0, |c| c.polls);
        if frames == 0 {
            return 0;
        }
        usize::try_from(polls / SPINNER_EVERY % frames as u64).unwrap_or(0)
    }

    /// Hands over the settings read at startup.
    pub fn set_config(&mut self, config: Config) {
        self.config = config;
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn set_hooks_installed(&mut self, installed: bool) {
        self.hooks_installed = installed;
    }

    #[must_use]
    pub fn hooks_installed(&self) -> bool {
        self.hooks_installed
    }

    pub fn set_keyboard_enhanced(&mut self, enhanced: bool) {
        self.keyboard_enhanced = enhanced;
    }

    #[must_use]
    pub fn keyboard_enhanced(&self) -> bool {
        self.keyboard_enhanced
    }

    pub fn set_nerd_mode(&mut self, on: bool) {
        self.nerd_mode = on;
    }

    /// Shows a message on the bar until something replaces it. The browser reports
    /// what happened to a tab through this.
    pub fn notify(&mut self, text: impl Into<String>) {
        self.set_status(text);
    }

    /// The most urgent marker among the sessions, which the tab of this project
    /// shows.
    #[must_use]
    pub fn attention(&self) -> Attention {
        self.entries
            .iter()
            .map(|e| e.attention)
            .max()
            .unwrap_or(Attention::None)
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
            self.selection = None;
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
        self.now = deps.clock.now_secs();
        if self.closing.is_some() {
            // Everything is on its way out. Only a second quit means anything, and
            // no byte may reach a child that was already asked to leave.
            if host_action(key) == Some(HostAction::Quit) {
                self.force_close();
            }
            return Ok(());
        }
        if self.modal.is_some() {
            return self.modal_key(key, deps);
        }
        match host_action(key) {
            Some(HostAction::Quit) => self.request_close(deps),
            // Tabs are the browser's business, not a project's. A project that
            // receives one of these keys does nothing with it.
            Some(
                HostAction::FocusTab(_)
                | HostAction::NewTab
                | HostAction::CloseTab
                | HostAction::MoveTabLeft
                | HostAction::MoveTabRight,
            ) => {}
            Some(HostAction::NewSession) => self.open_form(),
            Some(HostAction::TogglePause) => self.toggle_pause(deps)?,
            Some(HostAction::NerdMode) => self.nerd_mode = !self.nerd_mode,
            Some(HostAction::FocusShell) => {
                self.focus_shell();
                self.reveal_chosen();
                self.set_pane(Pane::Panel);
            }
            Some(HostAction::TogglePane) => {
                let next = self.next_pane();
                self.set_pane(next);
            }
            Some(HostAction::ToggleGit) => {
                self.git.toggle();
                self.set_pane(if self.git.is_open() {
                    Pane::Git
                } else {
                    Pane::Panel
                });
                self.poll_git(deps);
            }
            Some(HostAction::DeleteSession) => self.open_delete(),
            Some(HostAction::UndoPaste) => self.undo_paste()?,
            Some(HostAction::RenameSession) => self.open_rename(),
            Some(HostAction::ShowHelp) => self.modal = Some(Modal::Help),
            Some(HostAction::ShowSettings) => self.modal = Some(Modal::Settings),
            Some(HostAction::ToggleArchive) => self.archive_focused(deps)?,
            Some(HostAction::ShowArchive) => self.open_archive(),
            Some(HostAction::FindSession) => {
                if self.filter_focused {
                    self.set_pane(Pane::Panel);
                } else {
                    self.set_pane(Pane::Sidebar);
                    self.filter_focused = true;
                }
                self.selection = None;
            }
            // Half a panel, the step a pager uses.
            Some(HostAction::ScrollUp) => self.scroll_focus(i32::from(self.rows / 2)),
            Some(HostAction::ScrollDown) => self.scroll_focus(-i32::from(self.rows / 2)),
            None => {
                if self.filter_focused {
                    self.filter_key(key);
                    return Ok(());
                }
                if self.pane == Pane::Sidebar {
                    self.sidebar_key(key);
                    return Ok(());
                }
                if self.pane == Pane::Git {
                    self.git_key(key, deps);
                    return Ok(());
                }
                // A full-screen program pages its own text. Anything else keeps
                // its history in culm's scrollback, so culm pages that.
                if crate::keys::is_plain_page_key(key)
                    && self
                        .focused_session()
                        .is_some_and(|s| !s.alternate_screen())
                {
                    let half = i32::from(self.rows / 2);
                    let up = key.code == ratatui::crossterm::event::KeyCode::PageUp;
                    self.scroll_focus(if up { half } else { -half });
                    return Ok(());
                }
                // The shell keeps `Ctrl+z`, because its job control brings a
                // suspended job back.
                if self.config.block_ctrl_z && is_ctrl_z(key) && !self.shell_focused {
                    self.set_status_for(CTRL_Z_BLOCKED, CTRL_Z_NOTICE_SECS);
                    return Ok(());
                }
                if crate::keys::is_ctrl_enter(key) && !self.shell_focused {
                    return self.send_key_to_focus(crate::keys::CTRL_ENTER, deps);
                }
                let cursor = self
                    .focused_session()
                    .map(Session::cursor_keys)
                    .unwrap_or_default();
                if let Some(bytes) = crate::keys::encode(key, cursor) {
                    // Claude Code fills `Ctrl+v` from the clipboard, so to the undo
                    // it is a paste like any other.
                    if is_ctrl_v(key) && !self.shell_focused {
                        return self.paste_to_focus(None, bytes, deps);
                    }
                    self.send_key_to_focus(&bytes, deps)?;
                }
            }
        }
        Ok(())
    }

    /// Forwards a paste to the visible session as one bracketed block.
    pub fn on_paste(&mut self, text: &str, deps: &Deps<'_>) -> Result<()> {
        match self.modal.as_mut() {
            Some(Modal::NewSession(form)) => {
                form.name.push_str(text);
                return Ok(());
            }
            // The confirmation, the help and the settings take no text.
            Some(Modal::ConfirmDelete(_) | Modal::Help | Modal::Settings) => return Ok(()),
            Some(Modal::Rename(rename)) => {
                rename.name.push_str(text);
                return Ok(());
            }
            Some(Modal::Archive(list)) => {
                list.filter.push_str(text);
                list.cursor = 0;
                return Ok(());
            }
            None => {}
        }
        if self.filter_focused {
            self.filter.push_str(text);
            return Ok(());
        }
        // The keys are in the sidebar or the changes panel, so the paste is not the
        // session's either.
        if matches!(self.pane, Pane::Sidebar | Pane::Git) {
            return Ok(());
        }
        let bytes = crate::keys::encode_paste(text);
        if self.shell_focused {
            return self.send_to_focus(&bytes);
        }
        self.paste_to_focus(Some(text), bytes, deps)
    }

    /// Sends a key to the visible session. A paste still held for a Claude Code
    /// session goes first, so the session reads the input in the order it was given.
    fn send_key_to_focus(&mut self, bytes: &[u8], deps: &Deps<'_>) -> Result<()> {
        if let Some(id) = self.visible().map(|e| e.record.id.clone()) {
            let now = deps.clock.now_millis();
            let held = self.pastes.entry(id).or_default().key(now);
            for paste in held {
                self.send_to_focus(&paste)?;
            }
        }
        self.send_to_focus(bytes)
    }

    /// Sends a paste to the visible Claude Code session, or holds it until the gap
    /// after the last input has passed.
    fn paste_to_focus(
        &mut self,
        text: Option<&str>,
        bytes: Vec<u8>,
        deps: &Deps<'_>,
    ) -> Result<()> {
        let Some(id) = self.visible().map(|e| e.record.id.clone()) else {
            return Ok(());
        };
        let now = deps.clock.now_millis();
        match self.pastes.entry(id).or_default().paste(text, bytes, now) {
            Some(bytes) => self.send_to_focus(&bytes),
            None => Ok(()),
        }
    }

    /// Sends the held pastes whose gap has passed. The event loop calls this every
    /// pass, for every tab, so a paste reaches its session after a switch as well.
    pub fn flush_pastes(&mut self, deps: &Deps<'_>) {
        let now = deps.clock.now_millis();
        for entry in &mut self.entries {
            let Some(track) = self.pastes.get_mut(&entry.record.id) else {
                continue;
            };
            if !track.holds() {
                continue;
            }
            for paste in track.due(now) {
                if let Some(session) = entry.live.as_mut() {
                    session.scroll_to_bottom();
                    let _ = session.send(&paste);
                }
            }
        }
    }

    /// Takes back the last paste in the visible Claude Code session.
    fn undo_paste(&mut self) -> Result<()> {
        if self.shell_focused {
            self.set_status_for(UNDO_NO_SHELL, UNDO_NOTICE_SECS);
            return Ok(());
        }
        let Some(id) = self.visible().map(|e| e.record.id.clone()) else {
            return Ok(());
        };
        match self
            .pastes
            .get_mut(&id)
            .and_then(crate::paste::PasteTrack::undo)
        {
            Some(bytes) if bytes.is_empty() => Ok(()),
            Some(bytes) => self.send_to_focus(&bytes),
            None => {
                self.set_status_for(UNDO_NO_PASTE, UNDO_NOTICE_SECS);
                Ok(())
            }
        }
    }

    /// Forwards bytes to the visible session, and clears a permission marker.
    ///
    /// No hook fires when the user answers a permission prompt, so the keystroke that
    /// answers it is the only signal culm receives. A focus change sends no bytes and
    /// therefore still clears nothing.
    fn send_to_focus(&mut self, bytes: &[u8]) -> Result<()> {
        // Typing replaces what the selection was for, as it does in a terminal.
        self.selection = None;
        if self.shell_focused {
            if let Some(shell) = self.shell.as_mut() {
                // A terminal snaps back to the live output when the user types.
                shell.scroll_to_bottom();
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
        // A pause and a resume stamp `last_active` too, so the input carries its own
        // mark: the recap only wants to know whether the user did anything.
        if entry.record.last_active != self.now || entry.record.last_input != self.now {
            entry.record.last_active = self.now;
            entry.record.last_input = self.now;
            self.dirty = true;
        }
        if let Some(session) = entry.live.as_mut() {
            session.scroll_to_bottom();
            session.send(bytes)?;
        }
        Ok(())
    }

    /// The session behind the visible panel, whichever it is.
    fn focused_session(&self) -> Option<&Session> {
        if self.shell_focused {
            return self.shell.as_ref();
        }
        self.entries.get(self.entry_focus)?.live.as_ref()
    }

    fn focused_session_mut(&mut self) -> Option<&mut Session> {
        if self.shell_focused {
            return self.shell.as_mut();
        }
        self.entries.get_mut(self.entry_focus)?.live.as_mut()
    }

    /// One notch of the wheel over the panel.
    ///
    /// A child that asked for mouse reporting scrolls its own history, so the notch
    /// goes to the child. Claude Code is such a child, and its own scrollback is the
    /// conversation. A child that wants no mouse, such as a shell, leaves culm to
    /// move its scrollback instead.
    fn wheel(&mut self, up: bool, column: u16, row: u16, panel: Rect) {
        if self.modal.is_some() {
            return;
        }
        // While a selection rides on a self-scrolling child, each notch waits for the
        // last one to be measured.
        let grabbed = self
            .focused_session()
            .is_some_and(|s| s.mouse_encoding().is_some());
        if grabbed && self.selection.is_some() && self.pending_scroll.is_some() {
            if self.held_wheel.len() < HELD_NOTCHES {
                self.held_wheel.push_back(HeldNotch {
                    up,
                    column,
                    row,
                    panel,
                });
            }
            return;
        }
        let Some(session) = self.focused_session_mut() else {
            return;
        };
        let (moved, forwarded) = match session.mouse_encoding() {
            Some(encoding) => {
                // The panel border sits outside the child, and a terminal counts from
                // one, so the border column and row become the origin.
                let bytes = crate::keys::encode_wheel(
                    up,
                    column.saturating_sub(panel.x),
                    row.saturating_sub(panel.y),
                    encoding,
                );
                let _ = session.send(&bytes);
                // The child moves its own text, and not now but whenever it repaints.
                // Keep the rows as they are so the move can be measured later.
                (0, Some((session.row_fingerprint(), session.row_texts())))
            }
            None => (
                session.scroll_by(if up { WHEEL_LINES } else { -WHEEL_LINES }),
                None,
            ),
        };
        self.carry_selection(moved);
        if let Some((before, before_text)) = forwarded
            && self.selection.is_some()
            && self.pending_scroll.is_none()
        {
            self.pending_scroll = Some(PendingScroll {
                before,
                before_text,
                tries: 0,
            });
        }
    }

    /// Measures how far a child moved its own text after culm handed it a wheel notch,
    /// and carries the selection by that distance.
    ///
    /// The event loop calls this every pass. It costs nothing until a notch has gone
    /// out to a child that paints its own scrolling, and it is the only way a
    /// selection keeps marking its text on such a panel.
    pub fn settle_scroll(&mut self) {
        let Some(mut pending) = self.pending_scroll.take() else {
            return;
        };
        if self.selection.is_none() {
            self.release_held_wheel();
            return;
        }
        let Some(now) = self.focused_session().map(Session::row_fingerprint) else {
            return;
        };
        pending.tries = pending.tries.saturating_add(1);
        if now == pending.before {
            // Nothing has arrived yet. A child that ignores the notch would otherwise
            // hold the selection for ever, so the wait ends, and the notches behind
            // it would move nothing either.
            if pending.tries < SCROLL_SETTLE_TRIES {
                self.pending_scroll = Some(pending);
            } else {
                self.held_wheel.clear();
            }
            return;
        }
        match row_shift(&pending.before, &now) {
            Some(lines) => {
                if let Some(now_text) = self.focused_session().map(Session::row_texts) {
                    self.seen.moved(&pending.before_text, &now_text, lines);
                }
                self.carry_selection(lines);
            }
            // The panel changed in a way that is not a scroll, so the marks no longer
            // name the text the user chose.
            None => {
                self.selection = None;
                self.seen.clear();
            }
        }
        self.release_held_wheel();
    }

    /// Sends the next held notch, or every one once no selection waits on them.
    fn release_held_wheel(&mut self) {
        while let Some(notch) = self.held_wheel.pop_front() {
            self.wheel(notch.up, notch.column, notch.row, notch.panel);
            if self.pending_scroll.is_some() {
                break;
            }
        }
    }

    /// Moves the visible panel through its scrollback. A positive count goes up.
    fn scroll_focus(&mut self, lines: i32) {
        if self.modal.is_some() {
            return;
        }
        let Some(session) = self.focused_session() else {
            return;
        };
        let moved = session.scroll_by(lines);
        self.carry_selection(moved);
    }

    /// Moves a selection with the text it marks, so scrolling never drops it.
    ///
    /// A selection names panel rows, and the text under them moves down the panel by
    /// exactly the distance the view went back. Rows that leave the panel are kept
    /// rather than clamped, so scrolling the other way brings the same text back.
    fn carry_selection(&mut self, lines: i32) {
        if lines == 0 {
            return;
        }
        if let Some(selection) = self.selection.as_mut() {
            selection.anchor.0 = selection.anchor.0.saturating_add(lines);
            selection.head.0 = selection.head.0.saturating_add(lines);
        }
    }

    fn open_form(&mut self) {
        if self.active_count() >= MAX_ACTIVE {
            self.status_text =
                format!("{MAX_ACTIVE} active sessions is the limit. pause one first.");
            return;
        }
        self.clear_status();
        self.selection = None;
        self.modal = Some(Modal::NewSession(NewSession::default()));
    }

    /// Opens the delete confirmation. Offered only for a paused session, which is how
    /// the refusal to delete a running session reaches the user.
    fn open_delete(&mut self) {
        let Some(index) = self.focused_entry() else {
            return;
        };
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if entry.live.is_some() {
            let name = entry.record.name.clone();
            self.set_status(format!("{name} is running. pause it first."));
            return;
        }
        let name = entry.record.name.clone();
        self.clear_status();
        self.selection = None;
        self.modal = Some(Modal::ConfirmDelete(ConfirmDelete {
            index,
            name,
            answer: Answer::No,
            archive: None,
        }));
    }

    /// Asks to delete the highlighted archived session. An archived session has no
    /// process, so it needs no pause first, and no restore either.
    fn open_archived_delete(&mut self) {
        let Some(Modal::Archive(list)) = self.modal.take() else {
            return;
        };
        let Some((index, name)) = self
            .archived_matches(&list.filter)
            .get(list.cursor)
            .and_then(|&i| Some((i, self.entries.get(i)?.record.name.clone())))
        else {
            self.modal = Some(Modal::Archive(list));
            return;
        };
        self.modal = Some(Modal::ConfirmDelete(ConfirmDelete {
            index,
            name,
            answer: Answer::No,
            archive: Some(list),
        }));
    }

    /// Feeds the search box. No key reaches a child while the box holds the focus.
    ///
    /// `Enter` lands on the first match and hands the keys back to the panel, and the
    /// filter stays, so the list still shows what was searched for.
    fn filter_key(&mut self, key: &ratatui::crossterm::event::KeyEvent) {
        use ratatui::crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.set_pane(Pane::Panel);
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Enter => {
                if let Some(&index) = self.paused_matches().first() {
                    self.set_focus(index);
                    self.reveal_chosen();
                }
                self.set_pane(Pane::Panel);
            }
            KeyCode::Char(c) => self.filter.push(c),
            _ => {}
        }
    }

    /// Opens the rename form, pre-filled with the current name. A paused session
    /// renames as readily as a running one, because only the record changes.
    fn open_rename(&mut self) {
        let Some(index) = self.focused_entry() else {
            self.status_text = "the shell has no name".into();
            return;
        };
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        let name = entry.record.name.clone();
        self.clear_status();
        self.selection = None;
        self.modal = Some(Modal::Rename(Rename { index, name }));
    }

    /// Renames a session. The slug, the branch, and every worktree path stay as they
    /// were, so nothing on disk moves.
    pub fn rename_session(&mut self, index: usize, name: &str, deps: &Deps<'_>) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            self.status_text = "a session needs a name".into();
            return Ok(());
        }
        let Some(entry) = self.entries.get_mut(index) else {
            return Ok(());
        };
        entry.record.name = name.to_string();
        self.status_text = format!("renamed to {name}");
        self.save(deps)
    }

    /// Removes the session and its transcript. Worktrees and branches are left alone.
    pub fn delete_session(&mut self, index: usize, deps: &Deps<'_>) -> Result<()> {
        let Some(entry) = self.entries.get(index) else {
            return Ok(());
        };
        if entry.live.is_some() {
            self.status_text = format!("{} is running. pause it first.", entry.record.name);
            return Ok(());
        }
        let record = entry.record.clone();
        deps.store.remove_transcript(&record.cwd, &record.id)?;
        self.entries.remove(index);
        // The entries after the deleted one move up one, so a focus past it moves
        // with the session it names.
        if index < self.entry_focus {
            self.entry_focus -= 1;
        }
        if self.entry_focus >= self.entries.len() {
            self.entry_focus = self.entries.len().saturating_sub(1);
        }
        self.status_text = if record.repos.is_empty() {
            format!("{} deleted", record.name)
        } else {
            format!("{} deleted. its worktrees are still on disk.", record.name)
        };
        self.save(deps)
    }

    fn modal_key(&mut self, key: &KeyEvent, deps: &Deps<'_>) -> Result<()> {
        use ratatui::crossterm::event::KeyCode;
        match self.modal.as_mut() {
            Some(Modal::ConfirmDelete(confirm)) => {
                match key.code {
                    KeyCode::Esc => self.modal = confirm.archive.take().map(Modal::Archive),
                    // A letter moves the cursor onto a button. It never answers on its
                    // own, so deleting still takes two keys.
                    KeyCode::Char('y' | 'Y') => confirm.answer = Answer::Yes,
                    KeyCode::Char('n' | 'N') => confirm.answer = Answer::No,
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                        confirm.answer = match confirm.answer {
                            Answer::Yes => Answer::No,
                            Answer::No => Answer::Yes,
                        };
                    }
                    KeyCode::Enter => {
                        let confirm = confirm.clone();
                        self.modal = confirm.archive.clone().map(Modal::Archive);
                        if confirm.confirmed() {
                            self.delete_session(confirm.index, deps)?;
                            self.clamp_archive_cursor();
                        }
                    }
                    _ => {}
                }
                return Ok(());
            }
            Some(Modal::Settings) => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                        self.settings_cursor = self.settings_cursor.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        self.settings_cursor = (self.settings_cursor + 1).min(SETTINGS_ROWS - 1);
                    }
                    _ => {}
                }
                // Space flips the switch the cursor is on.
                if key.code == KeyCode::Char(' ') {
                    match self.settings_cursor {
                        0 => self.config.recap_on_pause = !self.config.recap_on_pause,
                        1 => self.config.changes_maximized = !self.config.changes_maximized,
                        2 => self.config.block_ctrl_z = !self.config.block_ctrl_z,
                        _ => self.config.copy_markdown = !self.config.copy_markdown,
                    }
                    // Settings are global, so they are written at once rather than
                    // waiting on a project to be saved.
                    deps.store.save_config(&self.config)?;
                } else if matches!(key.code, KeyCode::Esc | KeyCode::Enter)
                    || host_action(key) == Some(HostAction::ShowSettings)
                {
                    self.modal = None;
                }
                return Ok(());
            }
            Some(Modal::Archive(_)) => return self.archive_key(key, deps),
            Some(Modal::Help) => {
                // The help changes nothing, so any key that dismisses a dialog
                // dismisses it, and no other key reaches a child.
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter)
                    || host_action(key) == Some(HostAction::ShowHelp)
                {
                    self.modal = None;
                }
                return Ok(());
            }
            Some(Modal::Rename(rename)) => {
                match key.code {
                    KeyCode::Esc => self.modal = None,
                    KeyCode::Backspace => {
                        rename.name.pop();
                    }
                    KeyCode::Char(c) => rename.name.push(c),
                    KeyCode::Enter => {
                        let rename = rename.clone();
                        self.modal = None;
                        self.rename_session(rename.index, &rename.name, deps)?;
                    }
                    _ => {}
                }
                return Ok(());
            }
            Some(Modal::NewSession(_)) => {}
            None => return Ok(()),
        }
        let Some(Modal::NewSession(form)) = self.modal.as_mut() else {
            return Ok(());
        };
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Enter => {
                let form = form.clone();
                self.modal = None;
                self.create_session(&form, deps)?;
            }
            KeyCode::Backspace => {
                form.name.pop();
            }
            KeyCode::Char(c) => form.name.push(c),
            _ => {}
        }
        Ok(())
    }

    /// Creates a session at the project root and starts it.
    pub fn create_session(&mut self, form: &NewSession, deps: &Deps<'_>) -> Result<()> {
        if self.active_count() >= MAX_ACTIVE {
            self.status_text =
                format!("{MAX_ACTIVE} active sessions is the limit. pause one first.");
            return Ok(());
        }
        let name = form.name.trim();
        if name.is_empty() {
            self.status_text = "a session needs a name".into();
            return Ok(());
        }
        let taken: Vec<String> = self.entries.iter().map(|e| e.record.slug.clone()).collect();
        let slug = unique_slug(
            &slugify(name),
            &taken.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let mut record = SessionRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            slug,
            state: SessionState::Active,
            cwd: self.project.root.clone(),
            repos: Vec::new(),
            ticket: None,
            port: None,
            started: false,
            last_active: deps.clock.now_secs(),
            last_input: 0,
            recap: None,
            recap_at: 0,
            archived_from: None,
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
                // A session made from the shell is still the one the user wants
                // to see, so the shell lets go of the panel.
                self.shell_focused = false;
                self.reveal_chosen();
                self.set_pane(Pane::Panel);
                self.clear_status();
                self.save(deps)?;
            }
            Err(e) => self.status_text = format!("{name} did not start: {e}"),
        }
        Ok(())
    }

    fn focus_slug_of_last_active(&mut self) {
        if let Some(i) = self.entries.iter().rposition(Entry::is_active) {
            self.entry_focus = i;
        }
    }

    /// Opens the archive list with an empty search.
    fn open_archive(&mut self) {
        self.clear_status();
        self.selection = None;
        self.filter_focused = false;
        self.modal = Some(Modal::Archive(ArchiveList::default()));
    }

    /// Feeds the archive list. Typing searches, the arrows move the highlight, and
    /// `Enter` or the archive binding restores the highlighted session.
    fn archive_key(&mut self, key: &KeyEvent, deps: &Deps<'_>) -> Result<()> {
        use ratatui::crossterm::event::KeyCode;
        match host_action(key) {
            Some(HostAction::ShowArchive) => {
                self.modal = None;
                return Ok(());
            }
            Some(HostAction::ToggleArchive) => return self.restore_highlighted(deps),
            Some(HostAction::DeleteSession) => {
                self.open_archived_delete();
                return Ok(());
            }
            _ => {}
        }
        let Some(Modal::Archive(list)) = self.modal.as_mut() else {
            return Ok(());
        };
        list.notice = None;
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Enter => return self.restore_highlighted(deps),
            KeyCode::Up => list.cursor = list.cursor.saturating_sub(1),
            KeyCode::Down => list.cursor = list.cursor.saturating_add(1),
            KeyCode::Backspace => {
                list.filter.pop();
                list.cursor = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                list.filter.push(c);
                list.cursor = 0;
            }
            _ => {}
        }
        self.clamp_archive_cursor();
        Ok(())
    }

    /// Holds the highlight on a row that exists, after the matches changed.
    fn clamp_archive_cursor(&mut self) {
        let Some(Modal::Archive(list)) = self.modal.as_ref() else {
            return;
        };
        let last = self.archived_matches(&list.filter).len().saturating_sub(1);
        if let Some(Modal::Archive(list)) = self.modal.as_mut() {
            list.cursor = list.cursor.min(last);
        }
    }

    fn restore_highlighted(&mut self, deps: &Deps<'_>) -> Result<()> {
        let Some(list) = self.archive_list() else {
            return Ok(());
        };
        let Some(&index) = self.archived_matches(&list.filter).get(list.cursor) else {
            return Ok(());
        };
        self.restore_archived(index, deps)
    }

    /// Moves the focused session to the archive. A running session is stopped as a
    /// pause stops it, but no recap is taken, whatever the setting says.
    pub fn archive_focused(&mut self, deps: &Deps<'_>) -> Result<()> {
        if self.shell_focused {
            self.set_status("the shell cannot be archived");
            return Ok(());
        }
        let index = self.entry_focus;
        let Some(entry) = self.entries.get_mut(index) else {
            return Ok(());
        };
        if entry.record.is_archived() {
            return Ok(());
        }
        if let Some(mut session) = entry.live.take() {
            session.terminate().ok();
            session.wait_for_exit(TERMINATE_GRACE);
        }
        entry.record.archived_from = Some(entry.record.state);
        entry.record.state = SessionState::Archived;
        entry.record.last_active = deps.clock.now_secs();
        entry.attention = Attention::None;
        entry.rss = 0;
        let name = entry.record.name.clone();
        self.selection = None;
        self.reorder();
        self.focus_neighbor_of(index);
        self.set_status(format!("{name} archived. Alt+Shift+L lists the archive."));
        self.save(deps)
    }

    /// Focuses the row that took the place of one that left the sidebar, or the row
    /// above it when it was the last, or the shell when no row is left.
    fn focus_neighbor_of(&mut self, index: usize) {
        // `reorder` keeps every archived entry after the rest, so the rows the
        // sidebar shows are exactly the first `shown` entries.
        let shown = self
            .entries
            .iter()
            .filter(|e| !e.record.is_archived())
            .count();
        if shown == 0 {
            self.focus_shell();
        } else {
            self.set_focus(index.min(shown - 1));
        }
    }

    /// Returns an archived session to the list it left: active sessions start
    /// again, paused sessions stay without a process.
    ///
    /// A session whose transcript is gone cannot be resumed, so it leaves the
    /// archive instead, and the list says why.
    pub fn restore_archived(&mut self, index: usize, deps: &Deps<'_>) -> Result<()> {
        let Some(entry) = self.entries.get(index) else {
            return Ok(());
        };
        if !entry.record.is_archived() {
            return Ok(());
        }
        let record = entry.record.clone();
        if !self.has_transcript(&record, deps) {
            return self.drop_lost_archive(index, &record, deps);
        }
        let target = record.restore_state();
        if target == SessionState::Active && self.active_count() >= MAX_ACTIVE {
            self.archive_notice(format!(
                "{MAX_ACTIVE} active sessions is the limit. pause one, then restore {}.",
                record.name
            ));
            return Ok(());
        }
        let started = if target == SessionState::Active {
            Some(self.start(&record, deps))
        } else {
            None
        };
        let Some(entry) = self.entries.get_mut(index) else {
            return Ok(());
        };
        entry.record.archived_from = None;
        entry.record.last_active = deps.clock.now_secs();
        let status = match started {
            Some(Ok(session)) => {
                entry.record.started = true;
                entry.record.state = SessionState::Active;
                entry.live = Some(session);
                format!("{} restored to the active list", record.name)
            }
            // The session is out of the archive either way. A start that failed
            // leaves it paused, as a failed start does when a project opens.
            Some(Err(e)) => {
                entry.record.state = SessionState::Paused;
                format!("{} restored as paused. it did not start: {e}", record.name)
            }
            None => {
                entry.record.state = SessionState::Paused;
                format!("{} restored to the paused list", record.name)
            }
        };
        self.modal = None;
        if self
            .entries
            .get(index)
            .is_some_and(|e| e.record.is_paused())
        {
            self.lift(&record.id);
        }
        self.reorder();
        self.focus_id(&record.id);
        self.shell_focused = false;
        self.selection = None;
        self.set_pane(Pane::Panel);
        self.set_status(status);
        self.save(deps)
    }

    /// Removes an archived session whose transcript no longer exists, and says so.
    fn drop_lost_archive(
        &mut self,
        index: usize,
        record: &SessionRecord,
        deps: &Deps<'_>,
    ) -> Result<()> {
        let focused = self
            .entries
            .get(self.entry_focus)
            .map(|e| e.record.id.clone());
        self.entries.remove(index);
        if let Some(id) = focused {
            self.focus_id(&id);
        }
        if self.entry_focus >= self.entries.len() {
            self.entry_focus = self.entries.len().saturating_sub(1);
        }
        let mut text = format!(
            "{} could not be restored: its conversation no longer exists, \
             so it was removed from the archive.",
            record.name
        );
        if !record.repos.is_empty() {
            text.push_str(" its worktrees are still on disk.");
        }
        self.archive_notice(text);
        self.clamp_archive_cursor();
        self.save(deps)
    }

    /// Reports a failed restore inside the archive list, and on the bar as well, so
    /// the message survives the list being closed.
    fn archive_notice(&mut self, text: String) {
        if let Some(Modal::Archive(list)) = self.modal.as_mut() {
            list.notice = Some(text.clone());
        }
        self.set_status(text);
    }

    /// Pauses the focused session, or resumes it when it is already paused.
    pub fn toggle_pause(&mut self, deps: &Deps<'_>) -> Result<()> {
        if self.shell_focused {
            return Ok(());
        }
        let now = deps.clock.now_secs();
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
            let interacted = entry.record.last_input;
            entry.record.state = SessionState::Paused;
            entry.record.last_active = now;
            entry.attention = Attention::None;
            entry.rss = 0;
            self.set_status(format!("{} paused", record.name));
            self.start_recap(self.entry_focus, interacted, deps);
            self.lift(&record.id);
        } else {
            if self.active_count() >= MAX_ACTIVE {
                self.status_text =
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
                    entry.record.last_active = now;
                    entry.live = Some(session);
                    self.status_text = format!("{} resumed", record.name);
                }
                Err(e) => {
                    self.status_text = format!("{} did not resume: {e}", record.name);
                    return Ok(());
                }
            }
        }

        self.reorder();
        self.focus_id(&record.id);
        self.save(deps)
    }

    /// Asks a paused session what it was doing, when the setting asks for it.
    ///
    /// The child is already gone by now: Claude Code refuses to resume a session it
    /// still lists as running, so the recap can only follow the pause, never run
    /// beside it.
    fn start_recap(&mut self, index: usize, interacted: u64, deps: &Deps<'_>) {
        if !self.config.recap_on_pause || self.recap_work.is_some() {
            return;
        }
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        // Nothing happened since the last recap, so the old one still describes the
        // session and the model is not asked again.
        if entry.record.recap.is_some() && interacted <= entry.record.recap_at {
            return;
        }
        // A session killed young never wrote a transcript, and there is nothing to
        // read. That is not a failure worth reporting.
        if !self.has_transcript(&entry.record, deps) {
            return;
        }
        let (id, name, cwd) = (
            entry.record.id.clone(),
            entry.record.name.clone(),
            entry.record.cwd.clone(),
        );
        match deps.recapper.start(&id, &cwd) {
            Ok(job) => {
                self.set_status(format!("recapping {name}…"));
                self.recap_work = Some(RecapWork {
                    id,
                    name,
                    job,
                    deadline: deps.clock.now_secs().saturating_add(RECAP_LIMIT_SECS),
                });
            }
            Err(_) => self.store_recap(&id, RECAP_UNAVAILABLE.to_string(), deps),
        }
    }

    /// True when Claude Code has written a transcript for this session.
    fn has_transcript(&self, record: &SessionRecord, deps: &Deps<'_>) -> bool {
        let dir = crate::project::transcript_dir_name(&record.cwd);
        deps.store
            .list_transcripts(&dir)
            .map(|files| files.iter().any(|f| f.id == record.id))
            .unwrap_or(false)
    }

    /// Carries a recap forward by one pass. The loop calls this every time round.
    pub fn poll_recap(&mut self, deps: &Deps<'_>) {
        let Some(mut work) = self.recap_work.take() else {
            return;
        };
        let answer = match work.job.poll() {
            Some(Ok(text)) => crate::recap::clean(&text),
            Some(Err(_)) => String::new(),
            None => {
                if deps.clock.now_secs() <= work.deadline {
                    self.recap_work = Some(work);
                    return;
                }
                // The model reads a whole transcript, and a long one can outlast any
                // patience the user has. Give up rather than hold the session open.
                work.job.kill();
                String::new()
            }
        };
        let name = work.name.clone();
        let text = if answer.is_empty() {
            RECAP_UNAVAILABLE.to_string()
        } else {
            answer
        };
        self.store_recap(&work.id, text, deps);
        self.set_status(format!("{name} recapped"));
    }

    /// Keeps a recap against the session it belongs to, found by id because the list
    /// reorders as sessions pause.
    fn store_recap(&mut self, id: &str, text: String, deps: &Deps<'_>) {
        let now = deps.clock.now_secs();
        if let Some(entry) = self.entries.iter_mut().find(|e| e.record.id == id) {
            entry.record.recap = Some(text);
            entry.record.recap_at = now;
            self.dirty = true;
        }
    }

    /// Once a second: reap a child that ended on its own, and sample memory.
    pub fn on_tick(&mut self, probe: &dyn MemoryProbe, deps: &Deps<'_>) -> Result<()> {
        self.now = deps.clock.now_secs();
        // A closing project reaps its own children in `poll_close`, and a shell that
        // leaves during the close, or after it, must stay gone.
        if self.closing.is_some() || self.quit {
            return Ok(());
        }
        let mut reaped = Vec::new();
        for entry in &mut self.entries {
            let Some(session) = entry.live.as_mut() else {
                continue;
            };
            if session.has_exited() {
                entry.live = None;
                entry.record.state = SessionState::Paused;
                entry.rss = 0;
                reaped.push(entry.record.id.clone());
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
        for id in &reaped {
            self.lift(id);
        }
        if !reaped.is_empty() {
            let id = self
                .entries
                .get(self.entry_focus)
                .map(|e| e.record.id.clone());
            self.reorder();
            if let Some(id) = id {
                self.focus_id(&id);
            }
            self.save(deps)?;
        } else if self.dirty {
            self.save(deps)?;
        }
        Ok(())
    }

    /// Marks the session the hook belongs to. A payload for an unknown session is
    /// dropped, because every Claude Code session on the machine runs the hook.
    pub fn on_hook(&mut self, event: &HookEvent) {
        // A finished turn is when the session's changes settle, so that is when the
        // changes panel reads git again. Reading on every tool call would show half
        // made edits and cost a git call each.
        if event.hook_event_name == "Stop" && self.git.follows(&event.session_id) {
            self.git.mark_stale();
        }
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.record.id == event.session_id)
        {
            entry.attention = entry.attention.apply(event);
        }
    }

    /// Applies a command from the command line to this project's sessions. The tick
    /// writes a change, as it writes every other change to a record.
    pub fn command(&mut self, command: &crate::command::Command) -> crate::command::Reply {
        let mut records: Vec<&mut SessionRecord> =
            self.entries.iter_mut().map(|e| &mut e.record).collect();
        let reply = crate::command::apply(&mut records, command);
        if reply.changed {
            self.dirty = true;
            // A registered worktree is a tab of the changes panel, so the panel
            // works its tabs out again.
            self.git.mark_stale();
        }
        reply
    }

    /// Ends every child and saves. Active sessions stay active, so the next open
    /// resumes them.
    /// Answers `Ctrl+q`. The first press starts the close, a second cuts it short.
    fn request_close(&mut self, deps: &Deps<'_>) {
        match self.closing.as_mut() {
            Some(closing) => closing.forced = true,
            None => self.begin_close(deps),
        }
    }

    /// Asks every child to leave and hands the wait to the loop, so the screen keeps
    /// drawing instead of freezing for the whole grace period.
    pub fn begin_close(&mut self, deps: &Deps<'_>) {
        self.modal = None;
        self.selection = None;
        for entry in &mut self.entries {
            if let Some(session) = entry.live.as_mut() {
                session.terminate().ok();
            }
        }
        if let Some(shell) = self.shell.as_mut() {
            shell.terminate().ok();
        }
        // One grace for the sessions and the shell together, not one each.
        self.closing = Some(Closing {
            deadline: deps.clock.now_secs().saturating_add(CLOSE_GRACE_SECS),
            polls: 0,
            forced: false,
        });
    }

    /// Carries the close forward by one pass. The loop calls this every time round.
    ///
    /// Children that have left are dropped as they go. When the last one is gone, or
    /// the grace runs out, or the user asked twice, the rest are killed and the state
    /// is saved.
    pub fn poll_close(&mut self, deps: &Deps<'_>) -> Result<()> {
        let Some(mut closing) = self.closing.take() else {
            return Ok(());
        };
        closing.polls = closing.polls.saturating_add(1);
        for entry in &mut self.entries {
            if entry.live.as_mut().is_some_and(Session::has_exited) {
                entry.live = None;
            }
        }
        if self.shell.as_mut().is_some_and(Session::has_exited) {
            self.shell = None;
        }
        let waiting = self.entries.iter().any(|e| e.live.is_some()) || self.shell.is_some();
        let out_of_time = deps.clock.now_secs() > closing.deadline;
        if waiting && !closing.forced && !out_of_time {
            self.closing = Some(closing);
            return Ok(());
        }
        for entry in &mut self.entries {
            if let Some(session) = entry.live.as_mut() {
                session.kill().ok();
            }
            entry.live = None;
        }
        if let Some(shell) = self.shell.as_mut() {
            shell.kill().ok();
        }
        self.shell = None;
        // Only a quit the user cut short is worth reporting next time.
        // A recap is a child as well. Left alone it would outlive culm, and its
        // answer would reach no one.
        if let Some(mut work) = self.recap_work.take() {
            work.job.kill();
        }
        self.project.forced_quit = closing.forced;
        self.closing = None;
        self.quit = true;
        self.save(deps)
    }

    /// Closes without handing the wait back to a loop. A caller that owns no loop,
    /// and every test that wants the end state at once, uses this.
    pub fn shutdown(&mut self, deps: &Deps<'_>) -> Result<()> {
        self.begin_close(deps);
        let deadline = Instant::now() + TERMINATE_GRACE;
        while !self.quit {
            self.poll_close(deps)?;
            if Instant::now() > deadline {
                // The clock the poll reads is injected and may not move on its own,
                // so this stops a test from spinning for ever.
                self.force_close();
            }
        }
        Ok(())
    }

    /// Cuts the wait short. The children that are left get killed on the next pass.
    pub fn force_close(&mut self) {
        if let Some(closing) = self.closing.as_mut() {
            closing.forced = true;
        }
    }

    /// Maps a click or a drag onto the sidebar, with no modifier held.
    pub fn on_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        hit: &crate::ui::HitBox,
        deps: &Deps<'_>,
    ) {
        self.on_mouse_with(kind, column, row, KeyModifiers::NONE, hit, deps);
    }

    /// Maps a click or a drag onto the sidebar.
    ///
    /// `Shift` on a click extends the selection instead of starting a new one, which
    /// is how a terminal grows a selection over text that arrived after the first drag.
    pub fn on_mouse_with(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
        hit: &crate::ui::HitBox,
        deps: &Deps<'_>,
    ) {
        let at = Position { x: column, y: row };
        // A confirmation sits on top of everything, so its buttons answer first.
        if let MouseEventKind::Down(MouseButton::Left) = kind
            && let Some(Modal::ConfirmDelete(confirm)) = self.modal.as_mut()
            && let Some((_, answer)) = hit.buttons.iter().find(|(rect, _)| rect.contains(at))
        {
            confirm.answer = *answer;
            let confirm = confirm.clone();
            self.modal = None;
            if confirm.confirmed() {
                let _ = self.delete_session(confirm.index, deps);
            }
            return;
        }
        // The keys follow the mouse. A move, a drag and a press each give the keys to
        // the half of the screen under the pointer, except while the separator is
        // dragged, which crosses from one half into the other on purpose.
        if !self.dragging
            && matches!(
                kind,
                MouseEventKind::Moved | MouseEventKind::Drag(_) | MouseEventKind::Down(_)
            )
        {
            if hit.in_sidebar(at) {
                self.set_pane(Pane::Sidebar);
            } else if hit.git.as_ref().is_some_and(|g| g.area.contains(at)) {
                self.set_pane(Pane::Git);
            } else if hit.panel.contains(at) {
                self.set_pane(Pane::Panel);
            }
        }
        if self.git_mouse(kind, at, hit, deps) {
            return;
        }
        match kind {
            MouseEventKind::Down(MouseButton::Left)
                if column == hit.separator_col && hit.in_sidebar(at) =>
            {
                self.dragging = true;
            }
            MouseEventKind::Down(MouseButton::Left) if hit.in_sidebar(at) => {
                if hit.search_row == Some(row) {
                    self.filter_focused = true;
                    return;
                }
                match hit.rows.iter().find(|(r, _)| *r == row) {
                    Some((_, Focus::Shell)) => {
                        self.focus_shell();
                        self.reveal_chosen();
                    }
                    Some((_, Focus::Entry(index))) => {
                        self.set_focus(*index);
                        self.reveal_chosen();
                    }
                    None => {}
                }
                // The highlight lands on the row that was clicked.
                self.sidebar_cursor = Some(self.row_of(self.focus()));
            }
            MouseEventKind::Down(MouseButton::Left)
                if self.modal.is_none() && hit.inner.contains(at) =>
            {
                let cell = cell_of(at, hit.inner);
                match self.selection.as_mut() {
                    // A modifier keeps the anchor where it was, so the selection
                    // reaches the new point rather than starting again.
                    //
                    // Alt is the gesture that works, because a terminal keeps
                    // shift and the mouse for its own selection even while an
                    // application holds the mouse. kitty maps `shift+left press`
                    // to `mouse_selection` for a grabbed application, so that click
                    // never arrives here. Shift is still accepted for a terminal
                    // that does forward it.
                    Some(selection)
                        if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
                    {
                        selection.head = cell;
                    }
                    _ => {
                        self.selection = Some(Selection {
                            anchor: cell,
                            head: cell,
                        });
                        self.seen.clear();
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let width = column.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
                // A drag that ends where it started costs no write.
                if width != self.sidebar_width {
                    self.sidebar_width = width;
                    self.dirty = true;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selection.is_some() => {
                if let Some(selection) = self.selection.as_mut() {
                    selection.head = cell_of(at, hit.inner);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging = false;
                self.dragging_git = false;
                self.dragging_git_list = false;
                self.finish_selection(deps);
            }
            MouseEventKind::Down(MouseButton::Middle)
                if self.modal.is_none() && hit.inner.contains(at) =>
            {
                let text = self.clipboard.clone();
                if !text.is_empty() {
                    // A middle click pastes exactly as a terminal paste does, so the
                    // child receives one bracketed block rather than many keystrokes.
                    let _ = self.on_paste(&text, deps);
                }
            }
            MouseEventKind::ScrollUp if hit.panel.contains(at) => {
                self.wheel(true, column, row, hit.panel);
            }
            MouseEventKind::ScrollDown if hit.panel.contains(at) => {
                self.wheel(false, column, row, hit.panel);
            }
            _ => {}
        }
    }

    /// The mouse over the changes panel and its border. True when the event was the
    /// panel's.
    fn git_mouse(
        &mut self,
        kind: MouseEventKind,
        at: Position,
        hit: &crate::ui::HitBox,
        deps: &Deps<'_>,
    ) -> bool {
        if self.modal.is_some() {
            return false;
        }
        let Some(git) = hit.git.as_ref() else {
            return false;
        };
        match kind {
            // A maximized panel has no session beside it, so its border moves
            // nothing.
            MouseEventKind::Down(MouseButton::Left)
                if git.separator_col == Some(at.x)
                    && git.area.contains(at)
                    && self.git_split() != GitSplit::Full =>
            {
                self.dragging_git = true;
                true
            }
            MouseEventKind::Down(MouseButton::Left)
                if git.list_rule_col == Some(at.x) && git.body.contains(at) =>
            {
                self.dragging_git_list = true;
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_git => {
                // The panel ends at the right edge, so its width is what lies right
                // of the pointer, as a share of the room right of the sidebar.
                let right = git.area.x.saturating_add(git.area.width);
                let room = right.saturating_sub(self.sidebar_width).max(1);
                let width = right.saturating_sub(at.x);
                let percent =
                    u16::try_from(u32::from(width) * 100 / u32::from(room)).unwrap_or(u16::MAX);
                // The sizes are saved on the next tick, as the sidebar width is.
                if self.git.set_width(percent) {
                    self.dirty = true;
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_git_list => {
                if self.git.drag_list_to(at.x, git.body) {
                    self.dirty = true;
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) if git.area.contains(at) => {
                self.git.on_click(at, git, deps.git);
                true
            }
            MouseEventKind::ScrollUp if git.area.contains(at) => {
                self.git.on_wheel(true, at, git);
                true
            }
            MouseEventKind::ScrollDown if git.area.contains(at) => {
                self.git.on_wheel(false, at, git);
                true
            }
            _ => false,
        }
    }

    /// The changes panel.
    #[must_use]
    pub fn git(&self) -> &GitView {
        &self.git
    }

    /// Uncovers the session the user just chose. A maximized changes panel hides
    /// every session, so it closes. A panel beside the session stays, and follows
    /// the new choice on the next poll.
    fn reveal_chosen(&mut self) {
        if self.git_split() != GitSplit::Full {
            return;
        }
        self.git.close();
        if self.pane == Pane::Git {
            self.pane = Pane::Panel;
        }
    }

    /// Where the changes panel sits.
    #[must_use]
    pub fn git_split(&self) -> GitSplit {
        self.git.split(self.config.changes_maximized)
    }

    /// The next pane `Alt+Shift+O` gives the keys to: the sidebar, the session when
    /// it is on screen, and the changes panel when it is open.
    fn next_pane(&self) -> Pane {
        let order = [Pane::Sidebar, Pane::Panel, Pane::Git];
        let shown = |p: &Pane| match p {
            Pane::Sidebar => true,
            Pane::Panel => self.git_split() != GitSplit::Full,
            Pane::Git => self.git.is_open(),
        };
        let at = order.iter().position(|p| *p == self.pane()).unwrap_or(0);
        (1..=order.len())
            .filter_map(|step| order.get((at + step) % order.len()))
            .find(|p| shown(p))
            .copied()
            .unwrap_or(Pane::Panel)
    }

    /// The repositories the changes panel shows, each with its tab label.
    ///
    /// For a session: the worktrees it registered, then every repository the
    /// session wrote a file in, read from its transcript. No layout rule is needed,
    /// so a project that keeps worktrees anywhere, or none, works the same. For the
    /// shell: the repositories registered with the project.
    ///
    /// A session that registered a worktree says where its own work is. Any other
    /// repository it wrote in, such as a docs repository every session writes to,
    /// is shared, and lists only the files this session wrote.
    fn panel_repositories(&mut self, deps: &Deps<'_>) -> Vec<RepoTab> {
        // A registered worktree carries the repository's name. Any other tab is
        // named by its directory.
        let mut paths: Vec<(Option<String>, std::path::PathBuf)> = Vec::new();
        let mut own: Vec<Option<Vec<String>>> = Vec::new();
        match self.visible() {
            Some(entry) => {
                let registered = entry.record.repos.len();
                paths.extend(
                    entry
                        .record
                        .repos
                        .iter()
                        .map(|r| (Some(r.name.clone()), r.worktree.clone())),
                );
                own.resize(registered, None);
                let dir = crate::project::transcript_dir_name(&entry.record.cwd);
                let files = deps
                    .store
                    .read_transcript(&dir, &entry.record.id)
                    .map(|text| crate::transcript::edited_files(&text))
                    .unwrap_or_default();
                for top in self.git.repositories_of(&files, deps.git) {
                    if paths.iter().any(|(_, p)| *p == top) {
                        continue;
                    }
                    let mine = (registered > 0).then(|| {
                        files
                            .iter()
                            .filter_map(|f| f.strip_prefix(&top).ok())
                            .map(|f| f.to_string_lossy().into_owned())
                            .collect()
                    });
                    paths.push((None, top));
                    own.push(mine);
                }
            }
            None => paths.extend(
                self.project
                    .repos
                    .iter()
                    .map(|r| (Some(r.name.clone()), r.path.clone())),
            ),
        }
        own.resize(paths.len(), None);
        label_repositories(&paths, &self.project.root)
            .into_iter()
            .zip(own)
            .map(|((label, path), own)| RepoTab { label, path, own })
            .collect()
    }

    /// The ticket and port number of the visible session, for the panel title.
    fn panel_caption(&self) -> Option<String> {
        let record = &self.visible()?.record;
        let parts: Vec<String> = record
            .ticket
            .iter()
            .cloned()
            .chain(record.port.map(|p| format!("port {p}")))
            .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    fn git_key(&mut self, key: &KeyEvent, deps: &Deps<'_>) {
        let left = self.git.on_key(key, deps.git) == GitKey::Leave;
        // `r` asks for a read, which needs the tabs worked out again first.
        self.poll_git(deps);
        if left {
            let back = if self.git_split() == GitSplit::Full {
                Pane::Sidebar
            } else {
                Pane::Panel
            };
            self.set_pane(back);
        }
    }

    /// Points the changes panel at what the user looks at, and reads git when
    /// something asked for it. The loop calls this every pass, and it costs nothing
    /// until the focus moves, a turn ends, or the panel opens.
    pub fn poll_git(&mut self, deps: &Deps<'_>) {
        if !self.git.is_open() {
            return;
        }
        let owner = match self.visible() {
            Some(entry) => Owner::Session(entry.record.id.clone()),
            None => Owner::Shell,
        };
        self.git.follow(owner);
        self.git.caption = self.panel_caption();
        if self.git.wants_read() {
            let repos = self.panel_repositories(deps);
            self.git.set_repos(repos);
        }
        self.git.refresh(deps.git, deps.clock.now_secs());
        self.git.poll_colors();
    }

    /// Reads the text under the selection when the button comes up.
    ///
    /// A press and a release on one cell is a click, not a drag, and it selects
    /// nothing. The highlight stays after a real drag, as it does in a terminal,
    /// until the next click or the next keystroke.
    fn finish_selection(&mut self, deps: &Deps<'_>) {
        let Some(selection) = self.selection else {
            return;
        };
        if selection.anchor == selection.head {
            self.selection = None;
            return;
        }
        let Some((start, end)) = self.selection() else {
            return;
        };
        let Some(session) = self.focused_session() else {
            self.selection = None;
            return;
        };
        // A child that scrolls itself leaves no scrollback, so the rows it moved off
        // the panel come from what culm kept while the selection rode along.
        let mut text = if session.mouse_encoding().is_some() && !self.seen.is_empty() {
            crate::seen::text_between(&self.seen, &session.row_texts(), start, end)
        } else {
            session.text_between(start, end)
        };
        if text.is_empty() {
            self.selection = None;
            return;
        }
        let mut how = "";
        if self.config.copy_markdown
            && let Some(rebuilt) = self.rebuild_copy(&text, start, deps)
        {
            if rebuilt.markdown {
                how = " as markdown";
            }
            text = rebuilt.text;
        }
        self.status_text = format!("copied {} chars{how}", text.chars().count());
        self.clipboard = text.clone();
        self.copied = Some(text);
    }

    /// Rebuilds a copy of a Claude Code panel from the session's transcript. `None`
    /// for the shell, which has no transcript and whose lines are all real.
    fn rebuild_copy(
        &self,
        text: &str,
        start: (i32, u16),
        deps: &Deps<'_>,
    ) -> Option<crate::copytext::Rebuilt> {
        let entry = self.visible()?;
        let session = entry.live.as_ref()?;
        let dir = crate::project::transcript_dir_name(&entry.record.cwd);
        // A transcript that cannot be read leaves the screen lines to the line rule.
        let index = deps
            .store
            .read_transcript(&dir, &entry.record.id)
            .map(|t| crate::copytext::TranscriptIndex::new(&t))
            .unwrap_or_default();
        let above = (start.0 - COPY_CONTEXT_ROWS, 0);
        let last = (start.0 - 1, self.cols.saturating_sub(1));
        let before = if session.mouse_encoding().is_some() && !self.seen.is_empty() {
            crate::seen::text_between(&self.seen, &session.row_texts(), above, last)
        } else if start.0 > 0 {
            session.text_between((above.0.max(0), 0), last)
        } else {
            String::new()
        };
        Some(crate::copytext::rebuild(text, &before, &index, self.cols))
    }

    /// Resizes every running session to the panel. Sessions that are not visible are
    /// resized as well, so a switch never shows a stale layout.
    pub fn resize_all(&mut self, rows: u16, cols: u16) -> Result<()> {
        // The loop calls this every frame. A child lays its text out again only at a
        // new size, and then no kept row holds.
        if (rows, cols) != (self.rows, self.cols) {
            self.seen.clear();
        }
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

    /// Active sessions first, paused sessions next, archived sessions last.
    ///
    /// The active and the paused part keep their order, because the user arranges
    /// both by hand. A session that enters the paused list is lifted to its top
    /// first, by `lift`, so an untouched list still reads newest first.
    fn reorder(&mut self) {
        let focused = self
            .entries
            .get(self.entry_focus)
            .map(|e| e.record.id.clone());
        let mut active: Vec<Entry> = Vec::new();
        let mut paused: Vec<Entry> = Vec::new();
        let mut archived: Vec<Entry> = Vec::new();
        for entry in self.entries.drain(..) {
            match entry.record.state {
                SessionState::Active => active.push(entry),
                SessionState::Paused => paused.push(entry),
                SessionState::Archived => archived.push(entry),
            }
        }
        // The archive takes no hand order, so it is ordered by last use, newest
        // first. The sort is stable, so two sessions of the same age keep the order
        // they were added in.
        archived.sort_by(|a, b| b.record.last_active.cmp(&a.record.last_active));
        active.append(&mut paused);
        active.append(&mut archived);
        self.entries = active;
        if let Some(id) = focused {
            self.focus_id(&id);
        }
        if self.entry_focus >= self.entries.len() {
            self.entry_focus = self.entries.len().saturating_sub(1);
        }
    }

    /// Puts a session that just entered the paused list ahead of every other entry,
    /// so the next `reorder` leaves it at the top of the paused list. The focus stays
    /// on the session it was on.
    fn lift(&mut self, id: &str) {
        let Some(index) = self.entries.iter().position(|e| e.record.id == id) else {
            return;
        };
        let focused = self
            .entries
            .get(self.entry_focus)
            .map(|e| e.record.id.clone());
        let entry = self.entries.remove(index);
        self.entries.insert(0, entry);
        if let Some(focused) = focused {
            self.focus_id(&focused);
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
        self.dirty = false;
        self.project.sessions = self.entries.iter().map(|e| e.record.clone()).collect();
        self.project.view.sidebar_width = self.sidebar_width;
        self.project.view.changes_width = self.git.width();
        self.project.view.changes_list = self.git.list_width();
        deps.store.save_project(&self.project)
    }
}

/// Names each repository tab by its directory. Two tabs with the same directory
/// name, such as a main checkout and its worktree, are named by their path from the
/// project root instead, or by their full path when they sit outside it.
fn label_repositories(
    paths: &[(Option<String>, std::path::PathBuf)],
    root: &std::path::Path,
) -> Vec<(String, std::path::PathBuf)> {
    let short = |(name, path): &(Option<String>, std::path::PathBuf)| {
        name.clone().unwrap_or_else(|| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        })
    };
    paths
        .iter()
        .map(|entry| {
            let name = short(entry);
            let path = &entry.1;
            let clash = paths.iter().filter(|p| short(p) == name).count() > 1;
            let label = if clash {
                path.strip_prefix(root)
                    .map_or_else(|_| path.display().to_string(), |r| r.display().to_string())
            } else {
                name
            };
            (label, path.clone())
        })
        .collect()
}

/// A screen position as a cell inside the panel, clamped to the panel.
fn cell_of(at: Position, inner: Rect) -> (i32, u16) {
    let last_row = inner.height.saturating_sub(1);
    let last_col = inner.width.saturating_sub(1);
    (
        i32::from(at.y.saturating_sub(inner.y).min(last_row)),
        at.x.saturating_sub(inner.x).min(last_col),
    )
}

/// How far a panel's text moved between two row fingerprints, positive downward.
///
/// A repaint counts as a scroll when enough written rows line up at one offset.
/// Blank rows are ignored, because a panel holds many and they match anywhere.
/// `None` means the panel was redrawn rather than scrolled.
fn row_shift(before: &[u64], now: &[u64]) -> Option<i32> {
    let written = before.iter().filter(|&&row| row != 0).count();
    if written == 0 {
        return None;
    }
    let height = i32::try_from(before.len()).unwrap_or(0);
    let mut best: Option<(usize, i32)> = None;
    for shift in -(height - 1)..height {
        let score = aligned_rows(before, now, shift);
        // The smallest move wins a tie, because a panel of repeated lines would
        // otherwise pick an offset at random.
        let better = match best {
            None => true,
            Some((top, at)) => score > top || (score == top && shift.abs() < at.abs()),
        };
        if better {
            best = Some((score, shift));
        }
    }
    let (score, shift) = best?;
    if score < 2 || score.saturating_mul(SCROLL_MATCH_SHARE) < written {
        return None;
    }
    Some(shift)
}

/// How many written rows of `before` sit `shift` rows further down in `now`.
fn aligned_rows(before: &[u64], now: &[u64], shift: i32) -> usize {
    before
        .iter()
        .enumerate()
        .filter(|&(_, &row)| row != 0)
        .filter(|&(at, &row)| {
            let Ok(at) = i32::try_from(at) else {
                return false;
            };
            usize::try_from(at + shift)
                .ok()
                .and_then(|to| now.get(to))
                .is_some_and(|&found| found == row)
        })
        .count()
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    /// A panel of six rows, where each number stands for one line of text.
    fn rows(values: [u64; 6]) -> Vec<u64> {
        values.to_vec()
    }

    #[test]
    fn a_panel_scrolled_down_by_two_measures_as_two() {
        let before = rows([11, 12, 13, 14, 15, 16]);
        let after = rows([9, 10, 11, 12, 13, 14]);
        assert_eq!(row_shift(&before, &after), Some(2));
    }

    #[test]
    fn a_panel_scrolled_the_other_way_measures_as_a_negative() {
        let before = rows([11, 12, 13, 14, 15, 16]);
        let after = rows([13, 14, 15, 16, 17, 18]);
        assert_eq!(row_shift(&before, &after), Some(-2));
    }

    #[test]
    fn a_panel_that_did_not_move_measures_as_zero() {
        let before = rows([11, 12, 13, 14, 15, 16]);
        let after = rows([11, 12, 13, 14, 99, 16]);
        assert_eq!(
            row_shift(&before, &after),
            Some(0),
            "one row was rewritten, and the rest stayed where they were"
        );
    }

    #[test]
    fn a_fresh_screen_measures_as_nothing() {
        let before = rows([11, 12, 13, 14, 15, 16]);
        let after = rows([21, 22, 23, 24, 25, 26]);
        assert_eq!(row_shift(&before, &after), None);
    }

    #[test]
    fn blank_rows_never_decide_the_distance() {
        // Only two rows carry text. A blank row hashes to zero and must not line up
        // with the blank rows of the panel after the move.
        let before = rows([0, 0, 31, 32, 0, 0]);
        let after = rows([0, 0, 0, 0, 31, 32]);
        assert_eq!(row_shift(&before, &after), Some(2));
    }

    #[test]
    fn an_empty_panel_measures_as_nothing() {
        assert_eq!(row_shift(&rows([0; 6]), &rows([0; 6])), None);
    }

    #[test]
    fn the_smallest_move_wins_when_a_panel_repeats_itself() {
        let before = rows([7, 7, 7, 7, 7, 7]);
        let after = rows([7, 7, 7, 7, 7, 7]);
        assert_eq!(row_shift(&before, &after), Some(0));
    }
}
