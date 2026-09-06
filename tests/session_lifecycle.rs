//! Integration tests. These use the fakes, so they start no process and touch no
//! file system.
//!
//! Test code may use `expect` with a message. Library code may not.
#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use culm::app::{App, Deps, Field, NewSession};
use culm::hooks::{Attention, HookEvent};
use culm::project::{Project, Repository, SessionRecord, SessionState};
use culm::store::Store;
use culm::testing::{FakeGit, FakeMemoryProbe, FakeSpawner, MemoryStore};
use culm::ui::HitBox;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

const ROOT: &str = "/home/x/spm";

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

fn form(name: &str, chosen: [bool; 2]) -> NewSession {
    NewSession {
        name: name.into(),
        chosen: chosen.to_vec(),
        field: Field::Name,
    }
}

fn record(name: &str, state: SessionState) -> SessionRecord {
    SessionRecord {
        id: format!("id-{name}"),
        name: name.into(),
        slug: name.into(),
        state,
        cwd: PathBuf::from(ROOT),
        repos: Vec::new(),
        started: true,
    }
}

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

struct Fakes {
    spawner: FakeSpawner,
    git: FakeGit,
    store: MemoryStore,
}

impl Fakes {
    fn new() -> Self {
        Self {
            spawner: FakeSpawner::new(Vec::new()),
            git: FakeGit::default(),
            store: MemoryStore::new(),
        }
    }

    fn with_output(output: &[u8]) -> Self {
        Self {
            spawner: FakeSpawner::new(output.to_vec()),
            git: FakeGit::default(),
            store: MemoryStore::new(),
        }
    }

    fn deps(&self) -> Deps<'_> {
        Deps {
            spawner: &self.spawner,
            git: &self.git,
            store: &self.store,
        }
    }
}

#[test]
fn session_output_reaches_the_screen() {
    let f = Fakes::with_output(b"hello from the session");
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("session starts");

    let session = app.entries()[0].live.as_ref().expect("session is live");
    assert!(session.wait_for_text("hello from the session", Duration::from_secs(1)));
}

#[test]
fn a_new_session_gets_a_worktree_per_chosen_repo_and_a_shared_branch() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("feat A", [true, true]), &f.deps())
        .expect("session starts");

    let calls = f.git.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].path,
        PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a")
    );
    assert_eq!(
        calls[1].path,
        PathBuf::from("/home/x/spm/.worktrees/crm-mate-feat-a"),
        "a repository outside the root still lands under .worktrees"
    );
    assert!(
        calls.iter().all(|c| c.branch == "feat-a"),
        "one branch name is shared across every repository of the session"
    );
}

#[test]
fn a_new_session_starts_with_a_generated_session_id_in_its_first_worktree() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("feat A", [true, false]), &f.deps())
        .expect("session starts");

    let id = app.entries()[0].record.id.clone();
    assert!(!id.is_empty());
    let spec = &f.spawner.specs()[0];
    assert!(spec.has_flag("--session-id", &id));
    assert!(spec.has_flag("--add-dir", ROOT));
    assert!(!spec.args.contains(&"--resume".to_string()));
    assert_eq!(
        spec.cwd,
        PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a")
    );
    assert!(
        spec.args.iter().any(|a| a.contains("api-mate-feat-a")),
        "the system prompt names the worktree"
    );
}

#[test]
fn a_session_with_no_repository_runs_at_the_project_root() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("ask", [false, false]), &f.deps())
        .expect("session starts");

    let spec = &f.spawner.specs()[0];
    assert_eq!(spec.cwd, PathBuf::from(ROOT));
    assert!(f.git.calls().is_empty());
    assert!(!spec.args.contains(&"--append-system-prompt".to_string()));
}

#[test]
fn reopening_a_project_resumes_active_sessions_eagerly() {
    let f = Fakes::new();
    let mut saved = project();
    saved.sessions = vec![
        record("one", SessionState::Active),
        record("two", SessionState::Active),
    ];

    let app = App::open(saved, &f.deps(), 20, 60);

    assert_eq!(f.spawner.count(), 2);
    for spec in f.spawner.specs() {
        assert!(
            spec.has_flag("--resume", &format!("id-{}", spec.name)),
            "a session that ran before reloads its transcript"
        );
    }
    assert!(app.entries().iter().all(|e| e.live.is_some()));
}

#[test]
fn paused_sessions_get_no_process_on_open() {
    let f = Fakes::new();
    let mut saved = project();
    saved.sessions = vec![
        record("one", SessionState::Active),
        record("two", SessionState::Paused),
    ];

    let app = App::open(saved, &f.deps(), 20, 60);

    assert_eq!(f.spawner.count(), 1, "only the active session starts");
    assert_eq!(app.active_count(), 1);
    assert!(app.entries()[0].live.is_some());
    assert!(app.entries()[1].live.is_none());
}

#[test]
fn a_session_that_fails_to_start_leaves_the_rest_running() {
    let f = Fakes::new();
    let mut saved = project();
    saved.sessions = vec![record("one", SessionState::Active)];
    f.spawner.fail_from_now_on();

    let app = App::open(saved, &f.deps(), 20, 60);

    assert_eq!(app.active_count(), 0);
    assert!(!app.status().is_empty());
}

#[test]
fn pause_sends_sigterm_and_moves_the_session_to_the_paused_half() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    let child = f.spawner.spawn_named("one").expect("one was spawned").child;

    app.set_focus(0);
    app.toggle_pause(&f.deps()).expect("pause succeeds");

    assert!(child.terminated(), "a pause asks the child to exit");
    assert_eq!(app.active_count(), 1);
    assert_eq!(
        app.entries()[0].name(),
        "two",
        "the active half comes first"
    );
    assert_eq!(app.entries()[1].name(), "one");
    assert!(app.entries()[1].live.is_none());
    assert_eq!(app.focus(), 1, "the focus follows the paused session");
}

#[test]
fn resume_restarts_a_paused_session_with_its_transcript() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let id = app.entries()[0].record.id.clone();

    app.toggle_pause(&f.deps()).expect("pause succeeds");
    app.toggle_pause(&f.deps()).expect("resume succeeds");

    assert_eq!(app.active_count(), 1);
    assert!(app.entries()[0].live.is_some());
    let last = f.spawner.specs().pop().expect("a second spawn happened");
    assert!(last.has_flag("--resume", &id));
}

#[test]
fn a_child_that_exits_becomes_paused_on_the_next_tick() {
    let f = Fakes::new();
    let probe = FakeMemoryProbe::new(0);
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    f.spawner
        .last_child()
        .expect("a child exists")
        .exit_on_its_own();

    app.on_tick(&probe, &f.deps()).expect("tick succeeds");

    assert_eq!(app.active_count(), 0);
    assert_eq!(app.entries()[0].record.state, SessionState::Paused);
    assert!(app.entries()[0].live.is_none());
}

#[test]
fn memory_is_sampled_for_every_running_session() {
    let f = Fakes::new();
    let probe = FakeMemoryProbe::new(436 * 1024 * 1024);
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");

    app.on_tick(&probe, &f.deps()).expect("tick succeeds");

    assert_eq!(app.entries()[0].rss, 436 * 1024 * 1024);
}

#[test]
fn a_tenth_active_session_is_refused() {
    let f = Fakes::new();
    let mut app = App::new(project());
    for n in 1..=9 {
        app.create_session(&form(&format!("s{n}"), [false, false]), &f.deps())
            .expect("session starts");
    }
    assert_eq!(app.active_count(), 9);

    app.create_session(&form("s10", [false, false]), &f.deps())
        .expect("the call succeeds and refuses the session");

    assert_eq!(app.active_count(), 9);
    assert_eq!(f.spawner.count(), 9, "no tenth process is started");
    assert!(app.status().contains("limit"));
}

#[test]
fn a_git_failure_aborts_before_the_spawn() {
    let f = Fakes::new();
    f.git.fail_from_now_on();
    let mut app = App::new(project());

    app.create_session(&form("feat A", [true, false]), &f.deps())
        .expect("the call succeeds and reports the failure");

    assert_eq!(
        f.spawner.count(),
        0,
        "no process starts without its worktree"
    );
    assert!(
        app.entries().is_empty(),
        "no half-made session reaches the list"
    );
    assert!(app.status().contains("worktree"));
}

#[test]
fn hook_events_mark_the_matching_session_only() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    let id = app.entries()[0].record.id.clone();

    app.on_hook(&HookEvent {
        session_id: id,
        hook_event_name: "PermissionRequest".into(),
        tool_name: None,
    });
    app.on_hook(&HookEvent {
        session_id: "a-session-culm-does-not-own".into(),
        hook_event_name: "Stop".into(),
        tool_name: None,
    });

    assert_eq!(app.entries()[0].attention, Attention::NeedsPermission);
    assert_eq!(app.entries()[1].attention, Attention::None);
}

#[test]
fn focusing_a_session_never_clears_its_marker() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    let id = app.entries()[0].record.id.clone();
    app.on_hook(&HookEvent {
        session_id: id,
        hook_event_name: "Stop".into(),
        tool_name: None,
    });

    app.set_focus(0);
    app.set_focus(1);
    app.set_focus(0);

    assert_eq!(app.entries()[0].attention, Attention::Done);
}

#[test]
fn keys_reach_the_visible_session_only() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    app.set_focus(0);

    app.on_key(&key(KeyCode::Char('x'), KeyModifiers::NONE), &f.deps())
        .expect("key is forwarded");

    let one = f.spawner.spawn_named("one").expect("one exists");
    let two = f.spawner.spawn_named("two").expect("two exists");
    assert_eq!(one.pty.written_utf8(), "x");
    assert_eq!(two.pty.written_utf8(), "");
}

#[test]
fn paste_reaches_the_focused_session_bracketed() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");

    app.on_paste("first\nsecond\nthird")
        .expect("paste is forwarded");

    let pty = f.spawner.last_pty().expect("a pty exists");
    assert_eq!(
        pty.written_utf8(),
        "\x1b[200~first\nsecond\nthird\x1b[201~",
        "three lines arrive as one block, not as three submissions"
    );
}

#[test]
fn alt_digit_switches_the_visible_session() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");

    app.on_key(&key(KeyCode::Char('2'), KeyModifiers::ALT), &f.deps())
        .expect("host key is handled");

    assert_eq!(app.focus(), 1);
    assert_eq!(app.visible().map(culm::app::Entry::name), Some("two"));
}

#[test]
fn a_digit_past_the_active_half_leaves_the_focus_alone() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");

    app.on_key(&key(KeyCode::Char('5'), KeyModifiers::ALT), &f.deps())
        .expect("host key is handled");

    assert_eq!(app.focus(), 0);
}

#[test]
fn keys_go_to_the_form_and_never_to_the_child_while_it_is_open() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let pty = f.spawner.last_pty().expect("a pty exists");

    app.on_key(&key(KeyCode::Char('N'), KeyModifiers::ALT), &f.deps())
        .expect("the form opens");
    for c in "hi".chars() {
        app.on_key(&key(KeyCode::Char(c), KeyModifiers::NONE), &f.deps())
            .expect("the form takes the key");
    }

    assert_eq!(app.modal().map(|m| m.name.as_str()), Some("hi"));
    assert_eq!(pty.written_utf8(), "", "no key reached the child");
}

#[test]
fn the_form_toggles_a_repository_with_a_digit() {
    let f = Fakes::new();
    let mut app = App::new(project());

    app.on_key(&key(KeyCode::Char('N'), KeyModifiers::ALT), &f.deps())
        .expect("the form opens");
    app.on_key(&key(KeyCode::Tab, KeyModifiers::NONE), &f.deps())
        .expect("the field switches");
    app.on_key(&key(KeyCode::Char('2'), KeyModifiers::NONE), &f.deps())
        .expect("the repository toggles");

    assert_eq!(
        app.modal().map(|m| m.chosen.clone()),
        Some(vec![false, true])
    );
}

#[test]
fn a_click_on_a_sidebar_row_switches_the_visible_session() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    let hit = HitBox {
        sidebar_width: 30,
        separator_col: 29,
        rows: vec![(2, 0), (3, 1)],
        panel: Rect::new(30, 0, 50, 20),
    };

    app.on_mouse(MouseEventKind::Down(MouseButton::Left), 5, 2, &hit);

    assert_eq!(app.focus(), 0);
}

#[test]
fn dragging_the_separator_resizes_the_sidebar_within_its_limits() {
    let f = Fakes::new();
    let mut app = App::new(project());
    let hit = HitBox {
        sidebar_width: 30,
        separator_col: 29,
        rows: Vec::new(),
        panel: Rect::new(30, 0, 50, 20),
    };
    let _ = &f;

    app.on_mouse(MouseEventKind::Down(MouseButton::Left), 29, 5, &hit);
    app.on_mouse(MouseEventKind::Drag(MouseButton::Left), 45, 5, &hit);
    assert_eq!(app.sidebar_width(), 45);

    app.on_mouse(MouseEventKind::Drag(MouseButton::Left), 200, 5, &hit);
    assert_eq!(app.sidebar_width(), culm::app::SIDEBAR_MAX);

    app.on_mouse(MouseEventKind::Drag(MouseButton::Left), 1, 5, &hit);
    assert_eq!(app.sidebar_width(), culm::app::SIDEBAR_MIN);

    app.on_mouse(MouseEventKind::Up(MouseButton::Left), 1, 5, &hit);
    app.on_mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5, &hit);
    assert_eq!(
        app.sidebar_width(),
        culm::app::SIDEBAR_MIN,
        "a drag that did not start on the separator changes nothing"
    );
}

#[test]
fn a_click_inside_the_panel_never_changes_the_focus() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    app.set_focus(1);
    let hit = HitBox {
        sidebar_width: 30,
        separator_col: 29,
        rows: vec![(2, 0), (3, 1)],
        panel: Rect::new(30, 0, 50, 20),
    };

    app.on_mouse(MouseEventKind::Down(MouseButton::Left), 60, 2, &hit);

    assert_eq!(app.focus(), 1);
}

#[test]
fn every_session_is_resized_not_only_the_visible_one() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");

    app.resize_all(20, 60).expect("resize succeeds");

    for spawn in f.spawner.spawns() {
        assert_eq!(spawn.pty.size(), (20, 60));
    }
}

#[test]
fn state_is_saved_after_every_mutation() {
    let f = Fakes::new();
    let mut app = App::new(project());

    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let after_create = f.store.saves();
    assert!(after_create >= 1);

    app.toggle_pause(&f.deps()).expect("pause succeeds");
    assert!(f.store.saves() > after_create, "a pause is saved too");

    let saved = f.store.project("spm").expect("the project was saved");
    assert_eq!(saved.sessions.len(), 1);
    assert_eq!(saved.sessions[0].state, SessionState::Paused);
    assert!(saved.sessions[0].started, "a resume reloads the transcript");
}

#[test]
fn quitting_leaves_active_sessions_active_so_the_next_open_resumes_them() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    app.set_focus(1);
    app.toggle_pause(&f.deps()).expect("two pauses");

    app.on_key(&key(KeyCode::Char('q'), KeyModifiers::CONTROL), &f.deps())
        .expect("quit is handled");

    assert!(app.quit_requested());
    let saved = f.store.project("spm").expect("the project was saved");
    let states: Vec<SessionState> = saved.sessions.iter().map(|s| s.state).collect();
    assert_eq!(states, vec![SessionState::Active, SessionState::Paused]);

    let reopened = App::open(saved, &f.deps(), 20, 60);
    assert_eq!(reopened.active_count(), 1);
}

#[test]
fn the_registry_and_the_project_survive_a_round_trip() {
    let store = MemoryStore::new();
    let mut registry = culm::project::Registry::default();
    let entry = registry.add(ROOT);
    store.save_registry(&registry).expect("registry saves");
    store.save_project(&project()).expect("project saves");

    let loaded = store.load_registry().expect("registry loads");
    assert_eq!(
        loaded
            .owner_of(&PathBuf::from("/home/x/spm/api-mate"))
            .map(|p| p.slug.clone()),
        Some(entry.slug)
    );
    let project = store
        .load_project("spm")
        .expect("project loads")
        .expect("project exists");
    assert_eq!(project.repos.len(), 2);
}
