//! The changes panel: which worktrees it reads, when it reads them, and what a
//! pick shows. These use the fakes, so they start no process and read no disk.
//!
//! Test code may use `expect` with a message. Library code may not.
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use culm::app::{App, Deps, NewSession, Pane};
use culm::diff::{LineKind, Row};
use culm::gitview::{GitSplit, GitTab};
use culm::hooks::HookEvent;
use culm::project::{Project, Repository};
use culm::store::Store;
use culm::testing::{FakeClock, FakeGit, FakeHookListener, FakeRecapper, FakeSpawner, MemoryStore};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

const ROOT: &str = "/home/x/spm";
const API_A: &str = "/home/x/spm/.worktrees/api-mate-feat-a";
const CRM_A: &str = "/home/x/spm/.worktrees/crm-mate-feat-a";
const API_B: &str = "/home/x/spm/.worktrees/api-mate-feat-b";

fn project() -> Project {
    let mut project = Project::new("spm", ROOT);
    project.repos = vec![
        Repository {
            name: "api-mate".into(),
            path: PathBuf::from("/home/x/spm/api-mate"),
        },
        Repository {
            name: "crm-mate".into(),
            path: PathBuf::from("/elsewhere/crm-mate"),
        },
    ];
    project
}

fn form(name: &str) -> NewSession {
    NewSession { name: name.into() }
}

/// Creates a session and registers `worktrees` for it, each as `(repo, path)`, as
/// `culm session worktree add` would.
fn session_with(app: &mut App, f: &Fakes, name: &str, worktrees: &[(&str, &str)]) -> String {
    app.create_session(&form(name), &f.deps())
        .expect("the session starts");
    let id = app
        .entries()
        .iter()
        .find(|e| e.record.name == name)
        .expect("the session is listed")
        .record
        .id
        .clone();
    for (repo, path) in worktrees {
        let reply = app.command(&culm::command::Command::WorktreeAdd {
            session: id.clone(),
            worktree: culm::project::SessionRepo {
                name: (*repo).into(),
                worktree: PathBuf::from(path),
                branch: name.into(),
            },
        });
        assert!(reply.ok, "{}", reply.text);
    }
    id
}

struct Fakes {
    spawner: FakeSpawner,
    git: FakeGit,
    store: MemoryStore,
    clock: FakeClock,
    recapper: FakeRecapper,
    hooks: FakeHookListener,
    mic: culm::testing::FakeMicrophones,
}

impl Fakes {
    fn new() -> Self {
        Self {
            spawner: FakeSpawner::new(Vec::new()),
            git: FakeGit::default(),
            store: MemoryStore::new(),
            clock: FakeClock::new(1_000),
            recapper: FakeRecapper::new(),
            hooks: FakeHookListener::new(),
            mic: culm::testing::FakeMicrophones::default(),
        }
    }

    fn deps(&self) -> Deps<'_> {
        Deps {
            spawner: &self.spawner,
            git: &self.git,
            store: &self.store,
            recapper: &self.recapper,
            clock: &self.clock,
            hooks: &self.hooks,
            mic: &self.mic,
        }
    }
}

fn alt_shift(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT | KeyModifiers::SHIFT)
}

fn plain(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// One modified file, as `git status --porcelain=v2 -z` prints it.
fn modified(path: &str) -> Vec<u8> {
    format!("1 .M N... 100644 100644 100644 aaa aaa {path}\0").into_bytes()
}

fn stop(session_id: &str) -> HookEvent {
    HookEvent {
        session_id: session_id.to_string(),
        hook_event_name: "Stop".into(),
        ..HookEvent::default()
    }
}

/// An app with one session, `feat-a`, that registered a worktree of both
/// repositories.
fn one_session(f: &Fakes) -> App {
    let mut app = App::new(project());
    session_with(
        &mut app,
        f,
        "feat-a",
        &[("api-mate", API_A), ("crm-mate", CRM_A)],
    );
    app
}

#[test]
fn alt_shift_g_opens_the_panel_on_the_worktrees_of_the_visible_session() {
    let f = Fakes::new();
    f.git.set_status(Path::new(API_A), &modified("src/lib.rs"));
    let mut app = one_session(&f);

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    let view = app.git();
    assert!(view.is_open());
    assert_eq!(app.pane(), Pane::Git, "the panel takes the keys");
    let tabs: Vec<(&str, &Path)> = view
        .repos
        .iter()
        .map(|r| (r.name.as_str(), r.path.as_path()))
        .collect();
    assert_eq!(
        tabs,
        vec![
            ("api-mate", Path::new(API_A)),
            ("crm-mate", Path::new(CRM_A))
        ],
        "one tab per repository, each on the session's own worktree"
    );
    assert_eq!(view.repos[0].changes.len(), 1);
}

#[test]
fn the_first_change_shows_head_beside_the_working_file() {
    let f = Fakes::new();
    f.git.set_status(Path::new(API_A), &modified("src/lib.rs"));
    f.git
        .set_blob(Path::new(API_A), "HEAD", "src/lib.rs", "a\nlet x = 1;\nc\n");
    f.git
        .set_working_file(Path::new(API_A), "src/lib.rs", "a\nlet x = 2;\nc\n");
    let mut app = one_session(&f);

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    let diff = app.git().diff.as_ref().expect("the first file is picked");
    assert_eq!(diff.title, "src/lib.rs");
    let changed: Vec<(LineKind, LineKind)> = diff
        .rows
        .iter()
        .filter_map(|row| match row {
            Row::Pair {
                left: Some(l),
                right: Some(r),
            } if l.kind != LineKind::Context => Some((l.kind, r.kind)),
            _ => None,
        })
        .collect();
    assert_eq!(changed, vec![(LineKind::Removed, LineKind::Added)]);
}

#[test]
fn the_panel_reads_git_again_only_when_the_followed_session_stops() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    let id = app.entries()[0].record.id.clone();
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    assert_eq!(
        f.git.status_reads(Path::new(API_A)),
        1,
        "opening reads once"
    );

    app.poll_git(&f.deps());
    app.on_hook(&HookEvent {
        session_id: id.clone(),
        hook_event_name: "PostToolUse".into(),
        ..HookEvent::default()
    });
    app.poll_git(&f.deps());
    assert_eq!(
        f.git.status_reads(Path::new(API_A)),
        1,
        "a pass and a tool call read nothing"
    );

    f.git.set_status(Path::new(API_A), &modified("src/new.rs"));
    app.on_hook(&stop(&id));
    app.poll_git(&f.deps());
    assert_eq!(
        f.git.status_reads(Path::new(API_A)),
        2,
        "a finished turn reads"
    );
    assert_eq!(app.git().repos[0].changes[0].path, "src/new.rs");
}

#[test]
fn a_stop_from_another_session_reads_nothing() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    app.on_hook(&stop("someone-else"));
    app.poll_git(&f.deps());

    assert_eq!(f.git.status_reads(Path::new(API_A)), 1);
}

#[test]
fn r_reads_git_again_on_demand() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    app.on_key(&plain(KeyCode::Char('r')), &f.deps())
        .expect("a key");

    assert_eq!(f.git.status_reads(Path::new(API_A)), 2);
}

#[test]
fn switching_sessions_points_the_panel_at_the_other_worktrees() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    session_with(&mut app, &f, "feat-b", &[("api-mate", API_B)]);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    assert_eq!(app.git().repos[0].path, Path::new(API_B));

    app.set_focus(0);
    app.poll_git(&f.deps());

    let paths: Vec<&Path> = app.git().repos.iter().map(|r| r.path.as_path()).collect();
    assert_eq!(paths, vec![Path::new(API_A), Path::new(CRM_A)]);
}

#[test]
fn the_shell_shows_the_repositories_where_they_live() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.focus_shell();

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    let paths: Vec<&Path> = app.git().repos.iter().map(|r| r.path.as_path()).collect();
    assert_eq!(
        paths,
        vec![
            Path::new("/home/x/spm/api-mate"),
            Path::new("/elsewhere/crm-mate")
        ]
    );
}

#[test]
fn the_history_tab_shows_a_commit_file_against_its_parent() {
    let f = Fakes::new();
    let wt = Path::new(API_A);
    f.git
        .set_log(wt, b"c0ffee\x1fc0f\x1fAda\x1f900\x1ffeat: add b\0");
    f.git.set_commit_files(wt, "c0ffee", b"M\0b.rs\0");
    f.git.set_blob(wt, "c0ffee^1", "b.rs", "old\n");
    f.git.set_blob(wt, "c0ffee", "b.rs", "new\n");
    let mut app = one_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    app.on_key(&plain(KeyCode::Tab), &f.deps()).expect("a key");

    let view = app.git();
    assert_eq!(view.tab, GitTab::History);
    assert_eq!(view.repos[0].commits[0].subject, "feat: add b");
    let diff = view
        .diff
        .as_ref()
        .expect("the commit's first file is picked");
    assert_eq!(diff.title, "c0f b.rs");
    assert_eq!(diff.old_lines, vec!["old".to_string()]);
    assert_eq!(diff.new_lines, vec!["new".to_string()]);
}

#[test]
fn escape_gives_the_keys_back_to_the_session() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    app.on_key(&plain(KeyCode::Esc), &f.deps()).expect("a key");

    assert_eq!(app.pane(), Pane::Panel);
    assert!(app.git().is_open(), "the panel stays open");
}

#[test]
fn alt_shift_g_again_closes_the_panel() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    assert_eq!(app.git_split(), GitSplit::Closed);
    assert_eq!(app.pane(), Pane::Panel);
}

#[test]
fn the_maximized_setting_opens_the_panel_over_the_session() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    let mut config = app.config().clone();
    config.changes_maximized = true;
    app.set_config(config);

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    assert_eq!(app.git_split(), GitSplit::Full);

    app.on_key(&alt_shift('O'), &f.deps()).expect("a key");
    assert_eq!(
        app.pane(),
        Pane::Sidebar,
        "the hidden session is skipped when the keys move"
    );

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    assert_eq!(
        app.git_split(),
        GitSplit::Full,
        "it opens maximized every time"
    );
}

#[test]
fn alt_shift_m_is_no_longer_taken_from_the_session() {
    assert_eq!(culm::keys::host_action(&alt_shift('M')), None);
}

#[test]
fn space_on_the_second_setting_flips_the_maximized_panel_and_saves_it() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.on_key(&alt_shift('S'), &f.deps()).expect("a key");

    app.on_key(&plain(KeyCode::Down), &f.deps()).expect("a key");
    app.on_key(&plain(KeyCode::Char(' ')), &f.deps())
        .expect("a key");

    assert!(app.config().changes_maximized);
    assert!(!app.config().recap_on_pause, "the other switch stays");
    assert!(
        f.store.load_config().expect("saved").changes_maximized,
        "a setting is written at once"
    );
}

#[test]
fn alt_shift_o_steps_through_all_three_panes() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    let mut seen = Vec::new();
    for _ in 0..3 {
        app.on_key(&alt_shift('O'), &f.deps()).expect("a key");
        seen.push(app.pane());
    }

    assert_eq!(seen, vec![Pane::Sidebar, Pane::Panel, Pane::Git]);
}

#[test]
fn the_panel_opens_on_half_the_room_the_sidebar_leaves() {
    let area = Rect::new(0, 0, 130, 40);

    let layout = culm::ui::layout(area, 30, GitSplit::Beside(50));

    let git = layout.git.expect("the panel has a place");
    assert_eq!(layout.panel.width, 50);
    assert_eq!(git.width, 50);
    assert_eq!(git.x, 80);
}

#[test]
fn a_full_panel_leaves_the_session_its_size() {
    let area = Rect::new(0, 0, 130, 40);

    let full = culm::ui::layout(area, 30, GitSplit::Full);
    let closed = culm::ui::layout(area, 30, GitSplit::Closed);

    assert_eq!(
        full.panel, closed.panel,
        "the hidden session keeps running at full size"
    );
    assert_eq!(full.git, Some(closed.panel));
}

#[test]
fn syntax_colors_arrive_after_the_diff_without_holding_it() {
    let f = Fakes::new();
    f.git.set_status(Path::new(API_A), &modified("src/lib.rs"));
    f.git
        .set_blob(Path::new(API_A), "HEAD", "src/lib.rs", "fn a() {}\n");
    f.git
        .set_working_file(Path::new(API_A), "src/lib.rs", "fn b() {}\n");
    let mut app = one_session(&f);

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    let diff = app.git().diff.as_ref().expect("the diff shows at once");
    assert!(diff.new_colors.is_empty(), "plain first");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.git().diff.as_ref().is_some_and(|d| d.coloring())
        && std::time::Instant::now() < deadline
    {
        app.poll_git(&f.deps());
        std::thread::yield_now();
    }

    let diff = app.git().diff.as_ref().expect("the diff stays");
    assert!(!diff.new_colors.is_empty(), "the colors landed");
}

/// One Edit call of a transcript, as Claude Code writes it.
fn edit_line(path: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "cwd": ROOT,
        "message": {"content": [
            {"type": "tool_use", "name": "Edit", "input": {"file_path": path}}
        ]}
    })
    .to_string()
}

/// An app with one session that registered no worktree.
fn bare_session(f: &Fakes) -> (App, String) {
    let mut app = App::new(project());
    app.create_session(&form("club-1005"), &f.deps())
        .expect("the session starts");
    let id = app.entries()[0].record.id.clone();
    (app, id)
}

fn transcript_dir() -> String {
    culm::project::transcript_dir_name(Path::new(ROOT))
}

#[test]
fn the_tabs_are_the_repositories_the_session_wrote_in() {
    let f = Fakes::new();
    f.git.add_toplevel("/home/x/spm/.worktrees/CLUB-1005/api");
    f.git.add_toplevel("/home/x/spm/docs");
    let (mut app, id) = bare_session(&f);
    let text = [
        edit_line("/home/x/spm/.worktrees/CLUB-1005/api/src/offer.ts"),
        edit_line("/home/x/spm/docs/refinement/CLUB-1005.md"),
        edit_line("/tmp/scratch/notes.md"),
        edit_line("/home/x/spm/.worktrees/CLUB-1005/api/src/share.ts"),
    ]
    .join("\n");
    f.store.put_transcript(&transcript_dir(), &id, 1, &text);

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    let tabs: Vec<(&str, &Path)> = app
        .git()
        .repos
        .iter()
        .map(|r| (r.name.as_str(), r.path.as_path()))
        .collect();
    assert_eq!(
        tabs,
        vec![
            ("api", Path::new("/home/x/spm/.worktrees/CLUB-1005/api")),
            ("docs", Path::new("/home/x/spm/docs")),
        ],
        "a file outside every repository makes no tab, and a repository shows once"
    );
}

#[test]
fn a_repository_the_session_starts_writing_in_appears_after_its_turn() {
    let f = Fakes::new();
    f.git.add_toplevel("/home/x/spm/api");
    f.git.add_toplevel("/home/x/spm/app");
    let (mut app, id) = bare_session(&f);
    f.store.put_transcript(
        &transcript_dir(),
        &id,
        1,
        &edit_line("/home/x/spm/api/a.ts"),
    );
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    assert_eq!(app.git().repos.len(), 1);

    let text = [
        edit_line("/home/x/spm/api/a.ts"),
        edit_line("/home/x/spm/app/b.dart"),
    ]
    .join("\n");
    f.store.put_transcript(&transcript_dir(), &id, 2, &text);
    app.on_hook(&stop(&id));
    app.poll_git(&f.deps());

    let names: Vec<&str> = app.git().repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["api", "app"]);
}

#[test]
fn a_checkout_and_its_worktree_with_one_name_are_told_apart() {
    let f = Fakes::new();
    f.git.add_toplevel("/home/x/spm/app");
    f.git.add_toplevel("/home/x/spm/.worktrees/CLUB-1005/app");
    let (mut app, id) = bare_session(&f);
    let text = [
        edit_line("/home/x/spm/app/main.dart"),
        edit_line("/home/x/spm/.worktrees/CLUB-1005/app/main.dart"),
    ]
    .join("\n");
    f.store.put_transcript(&transcript_dir(), &id, 1, &text);

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    let names: Vec<&str> = app.git().repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["app", ".worktrees/CLUB-1005/app"]);
}

#[test]
fn a_session_that_wrote_nothing_has_no_tabs() {
    let f = Fakes::new();
    let (mut app, _) = bare_session(&f);

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    assert!(app.git().repos.is_empty());
}

/// A hit box for a panel drawn beside the session, as a frame would report it.
fn panel_hit(app: &App) -> culm::ui::HitBox {
    let backend = ratatui::backend::TestBackend::new(130, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("a terminal");
    let mut hit = culm::ui::HitBox::default();
    terminal
        .draw(|f| hit = culm::ui::draw(f, app))
        .expect("a frame");
    hit
}

fn mouse(app: &mut App, f: &Fakes, kind: MouseEventKind, column: u16, hit: &culm::ui::HitBox) {
    app.on_mouse(kind, column, 10, hit, &f.deps());
}

#[test]
fn dragging_the_panel_border_resizes_it_and_the_size_is_saved() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    let hit = panel_hit(&app);
    let border = hit
        .git
        .as_ref()
        .and_then(|g| g.separator_col)
        .expect("a border");

    mouse(
        &mut app,
        &f,
        MouseEventKind::Down(MouseButton::Left),
        border,
        &hit,
    );
    mouse(
        &mut app,
        &f,
        MouseEventKind::Drag(MouseButton::Left),
        60,
        &hit,
    );
    mouse(
        &mut app,
        &f,
        MouseEventKind::Up(MouseButton::Left),
        60,
        &hit,
    );
    app.shutdown(&f.deps()).expect("a close");

    let saved = f
        .store
        .load_project("spm")
        .expect("a read")
        .expect("the project");
    assert_eq!(
        saved.view.changes_width,
        Some(70),
        "70 of the 100 columns right of the sidebar"
    );
    let reopened = App::new(saved);
    assert_eq!(reopened.git_split(), GitSplit::Closed);
    assert_eq!(
        reopened.git().width(),
        Some(70),
        "the width comes back on open"
    );
}

#[test]
fn dragging_the_list_rule_resizes_the_list_and_the_size_is_saved() {
    let f = Fakes::new();
    f.git.set_status(Path::new(API_A), &modified("src/lib.rs"));
    let mut app = one_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    let hit = panel_hit(&app);
    let git = hit.git.clone().expect("the panel");
    let rule = git.list_rule_col.expect("a rule");

    mouse(
        &mut app,
        &f,
        MouseEventKind::Down(MouseButton::Left),
        rule,
        &hit,
    );
    mouse(
        &mut app,
        &f,
        MouseEventKind::Drag(MouseButton::Left),
        rule + 8,
        &hit,
    );
    mouse(
        &mut app,
        &f,
        MouseEventKind::Up(MouseButton::Left),
        rule + 8,
        &hit,
    );
    app.shutdown(&f.deps()).expect("a close");

    let saved = f
        .store
        .load_project("spm")
        .expect("a read")
        .expect("the project");
    let percent = saved.view.changes_list.expect("the list share is saved");
    assert!(percent > 20, "the list grew, got {percent}");
}

#[test]
fn the_list_stops_growing_at_its_widest_on_a_wide_panel() {
    use culm::gitview::list_columns;
    assert_eq!(
        list_columns(GitTab::Changes, 60, 300),
        40,
        "a share never passes 40 columns"
    );
    assert_eq!(
        list_columns(GitTab::History, 60, 300),
        64,
        "two lists, eight fifths as wide"
    );
    assert_eq!(
        list_columns(GitTab::Changes, 10, 100),
        20,
        "and never drops under 20"
    );
}

fn two_sessions_maximized(f: &Fakes, maximized: bool) -> App {
    let mut app = one_session(f);
    session_with(&mut app, f, "feat-b", &[("api-mate", API_B)]);
    let mut config = app.config().clone();
    config.changes_maximized = maximized;
    app.set_config(config);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    app
}

#[test]
fn choosing_a_session_closes_a_maximized_panel() {
    let f = Fakes::new();
    let mut app = two_sessions_maximized(&f, true);
    assert_eq!(app.git_split(), GitSplit::Full);

    app.on_key(&alt_shift('O'), &f.deps()).expect("a key");
    app.on_key(&plain(KeyCode::Char('k')), &f.deps())
        .expect("a key");
    app.on_key(&plain(KeyCode::Enter), &f.deps())
        .expect("a key");

    assert_eq!(app.git_split(), GitSplit::Closed);
    assert_eq!(app.pane(), Pane::Panel, "the chosen session takes the keys");
}

#[test]
fn alt_zero_closes_a_maximized_panel_to_show_the_shell() {
    let f = Fakes::new();
    let mut app = two_sessions_maximized(&f, true);

    app.on_key(
        &KeyEvent::new(KeyCode::Char('0'), KeyModifiers::ALT),
        &f.deps(),
    )
    .expect("a key");

    assert_eq!(app.git_split(), GitSplit::Closed);
    assert_eq!(app.pane(), Pane::Panel);
}

#[test]
fn choosing_a_session_keeps_a_panel_beside_it_open() {
    let f = Fakes::new();
    let mut app = two_sessions_maximized(&f, false);

    app.on_key(&alt_shift('O'), &f.deps()).expect("a key");
    app.on_key(&plain(KeyCode::Char('k')), &f.deps())
        .expect("a key");
    app.on_key(&plain(KeyCode::Enter), &f.deps())
        .expect("a key");

    assert!(matches!(app.git_split(), GitSplit::Beside(_)));
}

#[test]
fn a_worktree_registered_while_the_panel_is_open_appears_without_waiting_for_a_turn() {
    let f = Fakes::new();
    let (mut app, id) = bare_session(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");
    assert!(app.git().repos.is_empty());

    let reply = app.command(&culm::command::Command::WorktreeAdd {
        session: id,
        worktree: culm::project::SessionRepo {
            name: "api-mate".into(),
            worktree: PathBuf::from(API_A),
            branch: "feature/x".into(),
        },
    });
    assert!(reply.ok);
    app.poll_git(&f.deps());

    let tabs: Vec<&str> = app.git().repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(tabs, vec!["api-mate"]);
}

/// A session with a registered worktree that also wrote one file in a docs
/// repository other sessions write to.
fn session_with_shared_docs(f: &Fakes) -> App {
    f.git.add_toplevel("/home/x/spm/docs");
    f.git.set_status(
        Path::new("/home/x/spm/docs"),
        &[
            modified("refinement/CLUB-1.md"),
            modified("refinement/CLUB-2.md"),
        ]
        .concat(),
    );
    let mut app = App::new(project());
    let id = session_with(&mut app, f, "club-1", &[("api-mate", API_A)]);
    f.store.put_transcript(
        &transcript_dir(),
        &id,
        1,
        &edit_line("/home/x/spm/docs/refinement/CLUB-1.md"),
    );
    app
}

#[test]
fn a_shared_repository_lists_only_the_files_this_session_wrote() {
    let f = Fakes::new();
    let mut app = session_with_shared_docs(&f);

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    let docs = &app.git().repos[1];
    assert_eq!(docs.path, Path::new("/home/x/spm/docs"));
    let paths: Vec<&str> = docs.changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, vec!["refinement/CLUB-1.md"]);
    assert_eq!(docs.hidden, 1, "the panel says one change was left out");
}

#[test]
fn the_a_key_shows_every_change_of_a_shared_repository_and_back() {
    let f = Fakes::new();
    let mut app = session_with_shared_docs(&f);
    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    app.on_key(&plain(KeyCode::Char('a')), &f.deps())
        .expect("a key");
    assert_eq!(app.git().repos[1].changes.len(), 2);
    assert_eq!(app.git().repos[1].hidden, 0);

    app.on_key(&plain(KeyCode::Char('a')), &f.deps())
        .expect("a key");
    assert_eq!(app.git().repos[1].changes.len(), 1);
}

#[test]
fn a_session_without_worktrees_still_sees_every_change() {
    let f = Fakes::new();
    f.git.add_toplevel("/home/x/spm/docs");
    f.git.set_status(
        Path::new("/home/x/spm/docs"),
        &[
            modified("refinement/CLUB-1.md"),
            modified("refinement/CLUB-2.md"),
        ]
        .concat(),
    );
    let (mut app, id) = bare_session(&f);
    f.store.put_transcript(
        &transcript_dir(),
        &id,
        1,
        &edit_line("/home/x/spm/docs/refinement/CLUB-1.md"),
    );

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    assert_eq!(
        app.git().repos[0].changes.len(),
        2,
        "with no worktree registered, no repository counts as shared"
    );
}

#[test]
fn the_panel_title_names_the_ticket_and_the_port_number() {
    let f = Fakes::new();
    let (mut app, id) = bare_session(&f);
    for command in [
        culm::command::Command::Ticket {
            session: id.clone(),
            ticket: Some("CLUB-1005".into()),
        },
        culm::command::Command::Port {
            session: id,
            port: Some(4),
        },
    ] {
        assert!(app.command(&command).ok);
    }

    app.on_key(&alt_shift('G'), &f.deps()).expect("a key");

    assert_eq!(app.git().caption.as_deref(), Some("CLUB-1005 · port 4"));
}
