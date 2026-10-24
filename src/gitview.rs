//! The changes panel: one tab per repository of the visible session, each on that
//! session's worktree, with a list of changes or of commits, and the diff of the
//! file picked from the list.
//!
//! Read-only. The panel reads git only when it opens, when the session it follows
//! changes, when Claude Code finishes a turn, and when the user asks.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

use crate::diff::{
    self, Commit, Content, FileChange, Highlighter, LineColors, Row, parse_log, parse_name_status,
    parse_status,
};
use crate::git::Git;

/// Commits the history list reads.
const HISTORY_LIMIT: usize = 200;

/// Rows one notch of the wheel scrolls.
const WHEEL_ROWS: usize = 3;

/// The narrowest the panel gets when dragged.
pub const GIT_MIN_WIDTH: u16 = 40;

/// The two lists the left of the panel switches between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GitTab {
    #[default]
    Changes,
    History,
}

/// Which list of the panel holds the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitList {
    /// The changed files of the working tree.
    Files,
    Commits,
    /// The files the selected commit changed.
    CommitFiles,
}

/// Where the panel sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitSplit {
    Closed,
    /// Beside the session, on this share of the room the sidebar leaves, in percent.
    Beside(u16),
    /// In place of the session.
    Full,
}

/// The share of the room the panel opens on, until the user drags it.
pub const GIT_DEFAULT_PERCENT: u16 = 50;
/// The narrowest and widest share a drag reaches. Both sides stay usable.
pub const GIT_MIN_PERCENT: u16 = 20;
pub const GIT_MAX_PERCENT: u16 = 85;

/// The share of the panel its list takes, until the user drags it.
pub const LIST_DEFAULT_PERCENT: u16 = 20;
pub const LIST_MIN_PERCENT: u16 = 10;
pub const LIST_MAX_PERCENT: u16 = 60;

/// The narrowest and widest the changes list gets, in columns. A share alone would
/// grow the list past any path worth reading on a wide or maximized panel.
const LIST_MIN_COLS: u16 = 20;
const LIST_MAX_COLS: u16 = 40;

/// The history holds two lists, commits and their files, so it takes this many
/// fifths of the width the changes list takes.
const HISTORY_FIFTHS: u16 = 8;

/// The columns the left of the panel takes on `tab`, for a panel `body` columns
/// wide with the list at `percent`. The diff always keeps 20 columns.
#[must_use]
pub fn list_columns(tab: GitTab, percent: u16, body: u16) -> u16 {
    let base = (u32::from(body) * u32::from(percent) / 100) as u16;
    let base = base.clamp(LIST_MIN_COLS, LIST_MAX_COLS);
    let width = match tab {
        GitTab::Changes => base,
        GitTab::History => base * HISTORY_FIFTHS / 5,
    };
    width.min(body.saturating_sub(20))
}

/// A list's highlighted row and the first row on screen.
///
/// The drawing keeps the highlight in view, and the drawing holds the panel only
/// to read it, so the offset is a `Cell`.
#[derive(Debug, Default)]
pub struct ListCursor {
    pub at: usize,
    pub offset: Cell<usize>,
    /// True when the highlight moved and the next frame must bring it into view.
    /// The wheel scrolls a list without moving the highlight, and clears this.
    pub follow: Cell<bool>,
}

impl ListCursor {
    fn set(&mut self, at: usize) {
        self.at = at;
        self.follow.set(true);
    }

    /// The first row to draw, for a list of `len` items with room for `room`.
    #[must_use]
    pub fn first_visible(&self, len: usize, room: usize) -> usize {
        let room = room.max(1);
        let mut offset = self.offset.get();
        if self.follow.get() {
            if self.at < offset {
                offset = self.at;
            } else if self.at >= offset + room {
                offset = self.at + 1 - room;
            }
            self.follow.set(false);
        }
        offset = offset.min(len.saturating_sub(room));
        self.offset.set(offset);
        offset
    }
}

/// One repository tab.
#[derive(Debug)]
pub struct RepoView {
    pub name: String,
    pub path: PathBuf,
    pub changes: Vec<FileChange>,
    pub files: ListCursor,
    pub commits: Vec<Commit>,
    pub commit_list: ListCursor,
    pub commit_files: Vec<FileChange>,
    pub commit_file_list: ListCursor,
    /// Why git could not be read, shown in place of the lists.
    pub error: Option<String>,
}

impl RepoView {
    fn new(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            changes: Vec::new(),
            files: ListCursor::default(),
            commits: Vec::new(),
            commit_list: ListCursor::default(),
            commit_files: Vec::new(),
            commit_file_list: ListCursor::default(),
            error: None,
        }
    }
}

/// The diff on screen, built once per pick and drawn many times.
#[derive(Debug)]
pub struct DiffView {
    /// What the header names: the file, and the commit for a file of the history.
    pub title: String,
    pub rows: Vec<Row>,
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub old_colors: Vec<LineColors>,
    pub new_colors: Vec<LineColors>,
    /// Shown instead of rows: a binary file, a file too large, or no text change.
    pub notice: Option<String>,
    /// The first screen row drawn, counted in wrapped rows.
    pub scroll: Cell<usize>,
    /// The wrapped rows of the last width drawn. Wrapping a long file every frame
    /// would cost the frame, so it is done once per width.
    pub cache: RefCell<Option<WrappedDiff>>,
    /// The syntax colors, while a thread still works them out. Coloring reads the
    /// whole file, about a tenth of a millisecond per line, so a file of a few
    /// hundred lines would hold the frame. The diff shows plain until they land.
    colors: Option<Receiver<(Vec<LineColors>, Vec<LineColors>)>>,
}

/// A diff wrapped to one width: both columns, row for row.
#[derive(Debug)]
pub struct WrappedDiff {
    pub width: u16,
    pub left: Vec<Line<'static>>,
    pub right: Vec<Line<'static>>,
}

impl DiffView {
    fn notice(title: String, notice: &str) -> Self {
        Self {
            title,
            rows: Vec::new(),
            old_lines: Vec::new(),
            new_lines: Vec::new(),
            old_colors: Vec::new(),
            new_colors: Vec::new(),
            notice: Some(notice.to_string()),
            scroll: Cell::new(0),
            cache: RefCell::new(None),
            colors: None,
        }
    }

    /// Takes the colors once the thread has them. True when they landed.
    fn take_colors(&mut self) -> bool {
        let Some(rx) = self.colors.as_ref() else {
            return false;
        };
        let Ok((old, new)) = rx.try_recv() else {
            return false;
        };
        self.old_colors = old;
        self.new_colors = new;
        self.colors = None;
        // The wrapped rows carry the old colors, so they are wrapped again.
        *self.cache.borrow_mut() = None;
        true
    }

    /// True while the colors are still on their way.
    #[must_use]
    pub fn coloring(&self) -> bool {
        self.colors.is_some()
    }

    fn scroll_by(&self, rows: isize) {
        let next = self.scroll.get().saturating_add_signed(rows);
        let max = self
            .cache
            .borrow()
            .as_ref()
            .map_or(usize::MAX, |c| c.left.len().saturating_sub(1));
        self.scroll.set(next.min(max));
    }
}

/// Whose worktrees the panel shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// A session, by id. It shows the worktrees the session edits.
    Session(String),
    /// The shell. It shows the repositories of the project where they live.
    Shell,
}

/// Where the parts of the panel landed on the last frame, so a click finds them.
#[derive(Debug, Clone, Default)]
pub struct GitHit {
    /// The whole panel, border included.
    pub area: Rect,
    /// The column of the left border. A drag there resizes the panel.
    pub separator_col: Option<u16>,
    /// The column of the rule right of the list. A drag there resizes the list.
    pub list_rule_col: Option<u16>,
    /// The panel inside its border, below the repository tabs: the list and the
    /// diff together. A drag of the list is measured against it.
    pub body: Rect,
    pub repo_tabs: Vec<(Rect, usize)>,
    pub mode_tabs: Vec<(Rect, GitTab)>,
    /// Each list: its rows, which list it is, its first item, and the rows per item.
    pub lists: Vec<(Rect, GitList, usize, u16)>,
    pub diff: Rect,
}

/// What a key did inside the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitKey {
    Done,
    /// `Esc`: the keys go back to the session.
    Leave,
}

/// The changes panel of one project.
#[derive(Debug, Default)]
pub struct GitView {
    open: bool,
    /// The share of the room the user dragged the panel to, in percent. `None`
    /// until a drag, which reads as `GIT_DEFAULT_PERCENT`.
    width: Option<u16>,
    /// The share of the panel the user dragged the list to, in percent.
    list_width: Option<u16>,
    owner: Option<Owner>,
    pub repos: Vec<RepoView>,
    pub repo: usize,
    pub tab: GitTab,
    list: Option<GitList>,
    pub diff: Option<DiffView>,
    stale: bool,
    highlighter: Option<Arc<Highlighter>>,
    /// Unix seconds, for "14 minutes ago".
    now: u64,
    /// Rows of the diff on the last frame. A page key moves by half of it.
    pub diff_rows: Cell<u16>,
    /// The repository top of each directory already asked about. `None` means no
    /// repository holds the directory.
    toplevels: std::collections::HashMap<PathBuf, Option<PathBuf>>,
}

impl GitView {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Where the panel sits. `maximized` is the global setting: a panel opens
    /// over the whole session when it is on.
    #[must_use]
    pub fn split(&self, maximized: bool) -> GitSplit {
        match (self.open, maximized) {
            (false, _) => GitSplit::Closed,
            (true, true) => GitSplit::Full,
            (true, false) => GitSplit::Beside(self.width.unwrap_or(GIT_DEFAULT_PERCENT)),
        }
    }

    /// Takes back the sizes the project saved.
    pub fn restore(&mut self, width: Option<u16>, list_width: Option<u16>) {
        self.width = width.map(|w| w.clamp(GIT_MIN_PERCENT, GIT_MAX_PERCENT));
        self.list_width = list_width.map(|w| w.clamp(LIST_MIN_PERCENT, LIST_MAX_PERCENT));
    }

    /// The panel's share of the room, when the user dragged it.
    #[must_use]
    pub fn width(&self) -> Option<u16> {
        self.width
    }

    /// The list's share of the panel, when the user dragged it.
    #[must_use]
    pub fn list_width(&self) -> Option<u16> {
        self.list_width
    }

    /// The list's share of the panel, dragged or default.
    #[must_use]
    pub fn list_percent(&self) -> u16 {
        self.list_width.unwrap_or(LIST_DEFAULT_PERCENT)
    }

    #[must_use]
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Opens the panel, or closes it.
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.open {
            self.stale = true;
        }
    }

    /// Sets the panel's share of the room. True when it changed.
    pub fn set_width(&mut self, percent: u16) -> bool {
        let percent = Some(percent.clamp(GIT_MIN_PERCENT, GIT_MAX_PERCENT));
        let changed = self.width != percent;
        self.width = percent;
        changed
    }

    /// Moves the rule right of the list to `column`. True when the share changed.
    pub fn drag_list_to(&mut self, column: u16, body: Rect) -> bool {
        if body.width == 0 {
            return false;
        }
        let mut width = u32::from(column.saturating_sub(body.x)) + 1;
        if self.tab == GitTab::History {
            width = width * 5 / u32::from(HISTORY_FIFTHS);
        }
        let percent = u16::try_from(width * 100 / u32::from(body.width)).unwrap_or(u16::MAX);
        self.set_list_width(percent)
    }

    /// Sets the list's share of the panel. True when it changed.
    pub fn set_list_width(&mut self, percent: u16) -> bool {
        let percent = Some(percent.clamp(LIST_MIN_PERCENT, LIST_MAX_PERCENT));
        let changed = self.list_width != percent;
        self.list_width = percent;
        changed
    }

    /// The list that holds the keys on the current tab.
    #[must_use]
    pub fn list(&self) -> GitList {
        match (self.tab, self.list) {
            (GitTab::Changes, _) => GitList::Files,
            (GitTab::History, Some(GitList::CommitFiles)) => GitList::CommitFiles,
            (GitTab::History, _) => GitList::Commits,
        }
    }

    /// Asks for a fresh read on the next pass.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// True when the panel shows the shell's repositories rather than a session's.
    #[must_use]
    pub fn follows_shell(&self) -> bool {
        self.owner == Some(Owner::Shell)
    }

    /// True when the panel follows this session.
    #[must_use]
    pub fn follows(&self, session_id: &str) -> bool {
        matches!(&self.owner, Some(Owner::Session(id)) if id == session_id)
    }

    /// Points the panel at whatever the user looks at. A new owner gets fresh tabs
    /// and a fresh read. The same owner keeps its picks.
    pub fn follow(&mut self, owner: Owner) {
        if self.owner.as_ref() == Some(&owner) {
            return;
        }
        self.owner = Some(owner);
        self.repos.clear();
        self.repo = 0;
        self.diff = None;
        self.stale = true;
    }

    /// True when the next `refresh` reads git, so the caller works out the tabs
    /// first.
    #[must_use]
    pub fn wants_read(&self) -> bool {
        self.open && self.stale
    }

    /// Sets the repository tabs. A tab already open keeps its picks, and the tab on
    /// screen stays on screen while it is still listed.
    pub fn set_repos(&mut self, repos: Vec<(String, PathBuf)>) {
        let current = self.current().map(|r| r.path.clone());
        let mut old = std::mem::take(&mut self.repos);
        self.repos = repos
            .into_iter()
            .map(
                |(name, path)| match old.iter().position(|r| r.path == path) {
                    Some(at) => {
                        let mut kept = old.swap_remove(at);
                        kept.name = name;
                        kept
                    }
                    None => RepoView::new(name, path),
                },
            )
            .collect();
        self.repo = current
            .and_then(|p| self.repos.iter().position(|r| r.path == p))
            .unwrap_or(0);
    }

    /// The repository that holds each file, in the order the files come, each
    /// repository once. A directory is asked about once per panel, so a session that
    /// edits many files in one folder costs one git call.
    pub fn repositories_of(&mut self, files: &[PathBuf], git: &dyn Git) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        for file in files {
            let Some(dir) = file.parent() else {
                continue;
            };
            let top = match self.toplevels.get(dir) {
                Some(top) => top.clone(),
                None => {
                    let top = git.toplevel(dir).ok().flatten();
                    self.toplevels.insert(dir.to_path_buf(), top.clone());
                    top
                }
            };
            if let Some(top) = top
                && !found.contains(&top)
            {
                found.push(top);
            }
        }
        found
    }

    /// Takes the syntax colors of the diff on screen once they are ready.
    pub fn poll_colors(&mut self) {
        if let Some(diff) = self.diff.as_mut() {
            diff.take_colors();
        }
    }

    /// Reads git again when something asked for it. Costs nothing otherwise.
    pub fn refresh(&mut self, git: &dyn Git, now: u64) {
        if !self.open || !self.stale {
            return;
        }
        self.stale = false;
        self.now = now;
        for repo in &mut self.repos {
            load_repo(repo, git);
        }
        if self.highlighter.is_none() {
            self.highlighter = Some(Arc::new(Highlighter::new()));
        }
        self.load_commit_files(git);
        self.load_diff(git);
    }

    fn current(&self) -> Option<&RepoView> {
        self.repos.get(self.repo)
    }

    fn current_mut(&mut self) -> Option<&mut RepoView> {
        self.repos.get_mut(self.repo)
    }

    /// Reads the files of the highlighted commit.
    fn load_commit_files(&mut self, git: &dyn Git) {
        let Some(repo) = self.current_mut() else {
            return;
        };
        let Some(commit) = repo.commits.get(repo.commit_list.at) else {
            repo.commit_files.clear();
            return;
        };
        let keep = repo
            .commit_files
            .get(repo.commit_file_list.at)
            .map(|f| f.path.clone());
        repo.commit_files = git
            .commit_files(&repo.path, &commit.sha)
            .map(|out| parse_name_status(&out))
            .unwrap_or_default();
        let at = keep
            .and_then(|p| repo.commit_files.iter().position(|f| f.path == p))
            .unwrap_or(0);
        repo.commit_file_list.set(at);
    }

    /// Builds the diff of the highlighted file of the current list.
    fn load_diff(&mut self, git: &dyn Git) {
        let Some(repo) = self.current() else {
            self.diff = None;
            return;
        };
        let picked = match self.tab {
            GitTab::Changes => repo.changes.get(repo.files.at).map(|file| {
                let old = (!file.kind.is_new())
                    .then(|| git.blob(&repo.path, "HEAD", file.before()).ok().flatten())
                    .flatten();
                let new = (file.kind != diff::ChangeKind::Deleted)
                    .then(|| git.working_file(&repo.path, &file.path).ok().flatten())
                    .flatten();
                (file.path.clone(), file.path.clone(), old, new)
            }),
            GitTab::History => {
                let commit = repo.commits.get(repo.commit_list.at);
                let file = repo.commit_files.get(repo.commit_file_list.at);
                commit.zip(file).map(|(commit, file)| {
                    let parent = format!("{}^1", commit.sha);
                    let old = (!file.kind.is_new())
                        .then(|| git.blob(&repo.path, &parent, file.before()).ok().flatten())
                        .flatten();
                    let new = (file.kind != diff::ChangeKind::Deleted)
                        .then(|| git.blob(&repo.path, &commit.sha, &file.path).ok().flatten())
                        .flatten();
                    (
                        file.path.clone(),
                        format!("{} {}", commit.short, file.path),
                        old,
                        new,
                    )
                })
            }
        };
        let Some((path, title, old, new)) = picked else {
            self.diff = None;
            return;
        };
        let keep_scroll = self
            .diff
            .as_ref()
            .filter(|d| d.title == title)
            .map(|d| d.scroll.get());
        let view = build_diff(
            &path,
            title,
            old.as_deref(),
            new.as_deref(),
            self.highlighter.clone(),
        );
        if let Some(scroll) = keep_scroll {
            view.scroll.set(scroll);
        }
        self.diff = Some(view);
    }

    /// Feeds a key. No key reaches a child while the panel holds the keys.
    pub fn on_key(&mut self, key: &KeyEvent, git: &dyn Git) -> GitKey {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return GitKey::Done;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let half = isize::try_from(self.diff_rows.get() / 2)
            .unwrap_or(1)
            .max(1);
        match key.code {
            KeyCode::Esc => return GitKey::Leave,
            KeyCode::Tab | KeyCode::BackTab => {
                self.tab = match self.tab {
                    GitTab::Changes => GitTab::History,
                    GitTab::History => GitTab::Changes,
                };
                self.reload_pick(git);
            }
            KeyCode::Char('[') => self.switch_repo(self.repo.saturating_sub(1), git),
            KeyCode::Char(']') => self.switch_repo(self.repo + 1, git),
            // The caller reads on its next pass, after it works out the tabs again.
            KeyCode::Char('r') => self.stale = true,
            KeyCode::Left | KeyCode::Char('h') if self.tab == GitTab::History => {
                self.list = Some(GitList::Commits);
            }
            KeyCode::Right | KeyCode::Char('l') if self.tab == GitTab::History => {
                self.list = Some(GitList::CommitFiles);
            }
            KeyCode::Up if shift => self.scroll_diff(-1),
            KeyCode::Down if shift => self.scroll_diff(1),
            KeyCode::Char('K') => self.scroll_diff(-1),
            KeyCode::Char('J') => self.scroll_diff(1),
            KeyCode::PageUp => self.scroll_diff(-half),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_diff(half),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1, git),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1, git),
            KeyCode::Home | KeyCode::Char('g') => self.move_cursor(isize::MIN / 2, git),
            KeyCode::End | KeyCode::Char('G') => self.move_cursor(isize::MAX / 2, git),
            _ => {}
        }
        GitKey::Done
    }

    fn scroll_diff(&self, rows: isize) {
        if let Some(diff) = self.diff.as_ref() {
            diff.scroll_by(rows);
        }
    }

    fn switch_repo(&mut self, to: usize, git: &dyn Git) {
        if to < self.repos.len() && to != self.repo {
            self.repo = to;
            self.reload_pick(git);
        }
    }

    fn reload_pick(&mut self, git: &dyn Git) {
        if self.tab == GitTab::History {
            self.load_commit_files(git);
        }
        self.load_diff(git);
    }

    /// Moves the highlight of the list that holds the keys, and shows what it lands
    /// on.
    fn move_cursor(&mut self, by: isize, git: &dyn Git) {
        let list = self.list();
        let Some(repo) = self.current_mut() else {
            return;
        };
        let (cursor, len) = match list {
            GitList::Files => (&mut repo.files, repo.changes.len()),
            GitList::Commits => (&mut repo.commit_list, repo.commits.len()),
            GitList::CommitFiles => (&mut repo.commit_file_list, repo.commit_files.len()),
        };
        if len == 0 {
            return;
        }
        let at = cursor
            .at
            .saturating_add_signed(by)
            .min(len.saturating_sub(1));
        if at == cursor.at {
            return;
        }
        cursor.set(at);
        if list == GitList::Commits {
            self.load_commit_files(git);
        }
        self.load_diff(git);
    }

    /// A press inside the panel: a repository tab, a list tab, or a row.
    pub fn on_click(&mut self, at: Position, hit: &GitHit, git: &dyn Git) {
        if let Some(&(_, index)) = hit.repo_tabs.iter().find(|(r, _)| r.contains(at)) {
            self.switch_repo(index, git);
            return;
        }
        if let Some(&(_, tab)) = hit.mode_tabs.iter().find(|(r, _)| r.contains(at)) {
            if tab != self.tab {
                self.tab = tab;
                self.reload_pick(git);
            }
            return;
        }
        let Some(&(rect, list, first, per_item)) = hit.lists.iter().find(|(r, ..)| r.contains(at))
        else {
            return;
        };
        if self.tab == GitTab::History {
            self.list = Some(list);
        }
        let item = first + usize::from((at.y - rect.y) / per_item.max(1));
        let Some(repo) = self.current_mut() else {
            return;
        };
        let (cursor, len) = match list {
            GitList::Files => (&mut repo.files, repo.changes.len()),
            GitList::Commits => (&mut repo.commit_list, repo.commits.len()),
            GitList::CommitFiles => (&mut repo.commit_file_list, repo.commit_files.len()),
        };
        if item >= len {
            return;
        }
        cursor.set(item);
        if list == GitList::Commits {
            self.load_commit_files(git);
        }
        self.load_diff(git);
    }

    /// One notch of the wheel: a list scrolls without moving its highlight, and the
    /// diff scrolls.
    pub fn on_wheel(&mut self, up: bool, at: Position, hit: &GitHit) {
        let rows = if up {
            -(WHEEL_ROWS as isize)
        } else {
            WHEEL_ROWS as isize
        };
        if hit.diff.contains(at) {
            self.scroll_diff(rows);
            return;
        }
        let Some(&(rect, list, _, per_item)) = hit.lists.iter().find(|(r, ..)| r.contains(at))
        else {
            return;
        };
        let Some(repo) = self.current() else {
            return;
        };
        let (cursor, len) = match list {
            GitList::Files => (&repo.files, repo.changes.len()),
            GitList::Commits => (&repo.commit_list, repo.commits.len()),
            GitList::CommitFiles => (&repo.commit_file_list, repo.commit_files.len()),
        };
        let room = usize::from(rect.height / per_item.max(1));
        let max = len.saturating_sub(room);
        let next = cursor.offset.get().saturating_add_signed(rows).min(max);
        cursor.offset.set(next);
        cursor.follow.set(false);
    }
}

/// Reads the changes and the history of one repository, keeping each highlight on
/// the item it was on.
fn load_repo(repo: &mut RepoView, git: &dyn Git) {
    let keep_file = repo.changes.get(repo.files.at).map(|f| f.path.clone());
    let keep_commit = repo.commits.get(repo.commit_list.at).map(|c| c.sha.clone());
    match git.status(&repo.path) {
        Ok(out) => {
            repo.changes = parse_status(&out);
            repo.error = None;
        }
        Err(e) => {
            repo.changes.clear();
            repo.error = Some(e.to_string());
        }
    }
    repo.commits = git
        .log(&repo.path, HISTORY_LIMIT)
        .map(|out| parse_log(&out))
        .unwrap_or_default();
    let file = keep_file
        .and_then(|p| repo.changes.iter().position(|f| f.path == p))
        .unwrap_or(0);
    repo.files
        .set(file.min(repo.changes.len().saturating_sub(1)));
    let commit = keep_commit
        .and_then(|sha| repo.commits.iter().position(|c| c.sha == sha))
        .unwrap_or(0);
    repo.commit_list.set(commit);
}

/// Lays the two versions side by side and colors them.
fn build_diff(
    path: &str,
    title: String,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    highlighter: Option<Arc<Highlighter>>,
) -> DiffView {
    let (old, new) = match (diff::decode(old), diff::decode(new)) {
        (Content::Text(old), Content::Text(new)) => (old, new),
        (Content::Binary, _) | (_, Content::Binary) => {
            return DiffView::notice(title, "binary file");
        }
        _ => return DiffView::notice(title, "file too large to show"),
    };
    let rows = diff::side_by_side(&old, &new, diff::CONTEXT_LINES);
    if rows.is_empty() {
        return DiffView::notice(title, "no change to the text");
    }
    let colors = highlighter.map(|h| {
        let (tx, rx) = std::sync::mpsc::channel();
        let (path, old, new) = (path.to_string(), old.clone(), new.clone());
        // A pick that moves on before the colors land drops the receiver, and the
        // send then fails quietly.
        std::thread::spawn(move || {
            let _ = tx.send((h.colors(&path, &old), h.colors(&path, &new)));
        });
        rx
    });
    DiffView {
        title,
        rows,
        old_lines: diff::lines_of(&old)
            .into_iter()
            .map(str::to_string)
            .collect(),
        new_lines: diff::lines_of(&new)
            .into_iter()
            .map(str::to_string)
            .collect(),
        old_colors: Vec::new(),
        new_colors: Vec::new(),
        notice: None,
        scroll: Cell::new(0),
        cache: RefCell::new(None),
        colors,
    }
}
