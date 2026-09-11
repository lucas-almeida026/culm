//! Integration tests. These use the fakes, so they start no process and touch no
//! file system.
//!
//! Test code may use `expect` with a message. Library code may not.
#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use culm::app::{App, Deps, Field, Focus, NewSession};
use culm::hooks::{Attention, HookEvent};
use culm::project::{Project, Repository, SessionRecord, SessionState};
use culm::pty::SessionSpec;
use culm::store::Store;
use culm::testing::{FakeClock, FakeGit, FakeMemoryProbe, FakeSpawner, MemoryStore};
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
        last_active: 0,
    }
}

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

/// Every spawn except the shell at position 0, which is not a Claude Code session.
fn session_specs(f: &Fakes) -> Vec<SessionSpec> {
    f.spawner
        .specs()
        .into_iter()
        .filter(|s| s.name != "shell")
        .collect()
}

struct Fakes {
    spawner: FakeSpawner,
    git: FakeGit,
    store: MemoryStore,
    clock: FakeClock,
}

impl Fakes {
    fn new() -> Self {
        Self::with_output(&[])
    }

    fn with_output(output: &[u8]) -> Self {
        Self {
            spawner: FakeSpawner::new(output.to_vec()),
            git: FakeGit::default(),
            store: MemoryStore::new(),
            clock: FakeClock::new(1_000),
        }
    }

    fn deps(&self) -> Deps<'_> {
        Deps {
            spawner: &self.spawner,
            git: &self.git,
            store: &self.store,
            clock: &self.clock,
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

    let specs = session_specs(&f);
    assert_eq!(specs.len(), 2);
    for spec in specs {
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

    assert_eq!(session_specs(&f).len(), 1, "only the active session starts");
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
    assert_eq!(
        app.focused_entry(),
        Some(1),
        "the focus follows the paused session"
    );
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
        ..Default::default()
    });
    app.on_hook(&HookEvent {
        session_id: "a-session-culm-does-not-own".into(),
        hook_event_name: "Stop".into(),
        ..Default::default()
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
        ..Default::default()
    });

    app.set_focus(0);
    app.set_focus(1);
    app.set_focus(0);

    assert_eq!(app.entries()[0].attention, Attention::Done);
}

#[test]
fn answering_a_permission_prompt_clears_the_marker() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let id = app.entries()[0].record.id.clone();
    app.on_hook(&HookEvent {
        session_id: id,
        hook_event_name: "PermissionRequest".into(),
        ..Default::default()
    });
    assert_eq!(app.entries()[0].attention, Attention::NeedsPermission);

    app.on_key(&key(KeyCode::Char('1'), KeyModifiers::NONE), &f.deps())
        .expect("the answer is forwarded");

    assert_eq!(
        app.entries()[0].attention,
        Attention::None,
        "no hook reports an answered prompt, so the keystroke clears the marker"
    );
    let pty = f.spawner.last_pty().expect("a pty exists");
    assert_eq!(
        pty.written_utf8(),
        "1",
        "the answer still reaches the child"
    );
}

#[test]
fn a_key_sent_to_one_session_leaves_another_session_marked() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    for i in 0..2 {
        let id = app.entries()[i].record.id.clone();
        app.on_hook(&HookEvent {
            session_id: id,
            hook_event_name: "PermissionRequest".into(),
            ..Default::default()
        });
    }
    app.set_focus(0);

    app.on_key(&key(KeyCode::Char('1'), KeyModifiers::NONE), &f.deps())
        .expect("the answer is forwarded");

    assert_eq!(app.entries()[0].attention, Attention::None);
    assert_eq!(app.entries()[1].attention, Attention::NeedsPermission);
}

#[test]
fn a_paste_also_answers_a_permission_prompt() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let id = app.entries()[0].record.id.clone();
    app.on_hook(&HookEvent {
        session_id: id,
        hook_event_name: "PermissionRequest".into(),
        ..Default::default()
    });

    app.on_paste("yes").expect("the paste is forwarded");

    assert_eq!(app.entries()[0].attention, Attention::None);
}

#[test]
fn a_keystroke_leaves_a_done_marker_for_the_hook_to_clear() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let id = app.entries()[0].record.id.clone();
    app.on_hook(&HookEvent {
        session_id: id.clone(),
        hook_event_name: "Stop".into(),
        ..Default::default()
    });

    app.on_key(&key(KeyCode::Char('h'), KeyModifiers::NONE), &f.deps())
        .expect("the key is forwarded");

    assert_eq!(
        app.entries()[0].attention,
        Attention::Done,
        "only a permission marker answers to a keystroke"
    );

    app.on_hook(&HookEvent {
        session_id: id,
        hook_event_name: "UserPromptSubmit".into(),
        ..Default::default()
    });
    assert_eq!(app.entries()[0].attention, Attention::None);
}

#[test]
fn a_late_tool_event_never_clears_a_done_marker() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let id = app.entries()[0].record.id.clone();
    let hook = |name: &str| HookEvent {
        session_id: id.clone(),
        hook_event_name: name.into(),
        ..Default::default()
    };

    app.on_hook(&hook("Stop"));
    assert_eq!(app.entries()[0].attention, Attention::Done);

    // A background subagent or a tool hook can reach the socket after Stop, because
    // the hook processes of one event run in parallel.
    app.on_hook(&hook("PostToolUse"));
    app.on_hook(&hook("SubagentStop"));
    assert_eq!(
        app.entries()[0].attention,
        Attention::Done,
        "the done marker survives an event that arrives after the turn"
    );

    app.on_hook(&hook("UserPromptSubmit"));
    assert_eq!(app.entries()[0].attention, Attention::None);
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

    assert_eq!(app.focused_entry(), Some(1));
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

    assert_eq!(app.focused_entry(), Some(0));
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

    assert_eq!(app.new_session_form().map(|m| m.name.as_str()), Some("hi"));
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
        app.new_session_form().map(|m| m.chosen.clone()),
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
        rows: vec![(2, Focus::Entry(0)), (3, Focus::Entry(1))],
        panel: Rect::new(30, 0, 50, 20),
        inner: culm::ui::inner_of(Rect::new(30, 0, 50, 20)),
        search_row: None,
        ..HitBox::default()
    };

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        5,
        2,
        &hit,
        &f.deps(),
    );

    assert_eq!(app.focused_entry(), Some(0));
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
        inner: culm::ui::inner_of(Rect::new(30, 0, 50, 20)),
        search_row: None,
        ..HitBox::default()
    };
    let _ = &f;

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        29,
        5,
        &hit,
        &f.deps(),
    );
    app.on_mouse(
        MouseEventKind::Drag(MouseButton::Left),
        45,
        5,
        &hit,
        &f.deps(),
    );
    assert_eq!(app.sidebar_width(), 45);

    app.on_mouse(
        MouseEventKind::Drag(MouseButton::Left),
        200,
        5,
        &hit,
        &f.deps(),
    );
    assert_eq!(app.sidebar_width(), culm::app::SIDEBAR_MAX);

    app.on_mouse(
        MouseEventKind::Drag(MouseButton::Left),
        1,
        5,
        &hit,
        &f.deps(),
    );
    assert_eq!(app.sidebar_width(), culm::app::SIDEBAR_MIN);

    app.on_mouse(MouseEventKind::Up(MouseButton::Left), 1, 5, &hit, &f.deps());
    app.on_mouse(
        MouseEventKind::Drag(MouseButton::Left),
        50,
        5,
        &hit,
        &f.deps(),
    );
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
        rows: vec![(2, Focus::Entry(0)), (3, Focus::Entry(1))],
        panel: Rect::new(30, 0, 50, 20),
        inner: culm::ui::inner_of(Rect::new(30, 0, 50, 20)),
        search_row: None,
        ..HitBox::default()
    };

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        60,
        2,
        &hit,
        &f.deps(),
    );

    assert_eq!(app.focused_entry(), Some(1));
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

#[test]
fn the_shell_starts_at_the_project_root_and_holds_the_focus() {
    let f = Fakes::new();
    let app = App::open(project(), &f.deps(), 20, 60);

    assert_eq!(app.focus(), Focus::Shell, "position 0 is focused on open");
    assert!(app.focused_entry().is_none());
    assert!(app.shell().is_some());
    let shell = f.spawner.spawn_named("shell").expect("the shell spawned");
    assert_eq!(shell.spec.cwd, PathBuf::from(ROOT));
}

#[test]
fn the_shell_carries_no_culm_socket() {
    let f = Fakes::new();
    let _ = App::open(project(), &f.deps(), 20, 60);

    let shell = f.spawner.spawn_named("shell").expect("the shell spawned");
    assert!(
        !shell.spec.env.iter().any(|(k, _)| k == "CULM_SOCKET"),
        "a claude started by hand in the shell must post no marker culm cannot place"
    );
    assert!(shell.spec.args.is_empty());
}

#[test]
fn the_shell_does_not_count_toward_the_nine_session_limit() {
    let f = Fakes::new();
    let mut app = App::open(project(), &f.deps(), 20, 60);
    for n in 1..=9 {
        app.create_session(&form(&format!("s{n}"), [false, false]), &f.deps())
            .expect("session starts");
    }

    assert_eq!(app.active_count(), 9);
    assert_eq!(session_specs(&f).len(), 9);
    app.on_key(&key(KeyCode::Char('0'), KeyModifiers::ALT), &f.deps())
        .expect("alt+0 is handled");
    assert_eq!(app.focus(), Focus::Shell, "position 0 stays reachable");
}

#[test]
fn a_shell_that_exits_is_restarted_on_the_next_tick() {
    let f = Fakes::new();
    let probe = FakeMemoryProbe::new(0);
    let mut app = App::open(project(), &f.deps(), 20, 60);
    f.spawner
        .spawn_named("shell")
        .expect("the shell spawned")
        .child
        .exit_on_its_own();

    app.on_tick(&probe, &f.deps()).expect("tick succeeds");

    assert!(
        app.shell().is_some(),
        "position 0 always holds a live shell"
    );
    assert_eq!(
        f.spawner
            .spawns()
            .iter()
            .filter(|s| s.spec.name == "shell")
            .count(),
        2
    );
}

#[test]
fn pause_does_nothing_while_the_shell_is_focused() {
    let f = Fakes::new();
    let mut app = App::open(project(), &f.deps(), 20, 60);
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.focus_shell();

    app.toggle_pause(&f.deps()).expect("the call succeeds");

    assert_eq!(app.active_count(), 1, "the session is untouched");
    assert_eq!(app.focus(), Focus::Shell);
}

#[test]
fn keys_reach_the_shell_when_it_is_focused() {
    let f = Fakes::new();
    let mut app = App::open(project(), &f.deps(), 20, 60);
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let session = f.spawner.spawn_named("one").expect("one spawned");
    app.focus_shell();

    app.on_key(&key(KeyCode::Char('l'), KeyModifiers::NONE), &f.deps())
        .expect("the key is forwarded");

    let shell = f.spawner.spawn_named("shell").expect("the shell spawned");
    assert_eq!(shell.pty.written_utf8(), "l");
    assert_eq!(session.pty.written_utf8(), "", "no key reached the session");
}

#[test]
fn the_shell_is_never_saved_to_the_project_file() {
    let f = Fakes::new();
    let mut app = App::open(project(), &f.deps(), 20, 60);
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");

    let saved = f.store.project("spm").expect("the project was saved");
    assert_eq!(saved.sessions.len(), 1);
    assert_eq!(saved.sessions[0].name, "one");
}

#[test]
fn focusing_the_shell_keeps_the_place_in_the_session_list() {
    let f = Fakes::new();
    let mut app = App::open(project(), &f.deps(), 20, 60);
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.create_session(&form("two", [false, false]), &f.deps())
        .expect("two starts");
    app.set_focus(1);

    app.focus_shell();
    app.set_focus(1);

    assert_eq!(app.focus(), Focus::Entry(1));
}

#[test]
fn a_click_on_the_shell_row_focuses_the_shell() {
    let f = Fakes::new();
    let mut app = App::open(project(), &f.deps(), 20, 60);
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let hit = HitBox {
        sidebar_width: 30,
        separator_col: 29,
        rows: vec![(1, Focus::Shell), (4, Focus::Entry(0))],
        panel: Rect::new(30, 0, 50, 20),
        inner: culm::ui::inner_of(Rect::new(30, 0, 50, 20)),
        search_row: None,
        ..HitBox::default()
    };

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        5,
        4,
        &hit,
        &f.deps(),
    );
    assert_eq!(app.focus(), Focus::Entry(0));

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        5,
        1,
        &hit,
        &f.deps(),
    );
    assert_eq!(app.focus(), Focus::Shell);
}

#[test]
fn deleting_a_paused_session_removes_its_record_and_transcript() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("feat A", [true, false]), &f.deps())
        .expect("one starts");
    let record = app.entries()[0].record.clone();
    app.toggle_pause(&f.deps()).expect("pause succeeds");

    app.on_key(&key(KeyCode::Char('X'), KeyModifiers::ALT), &f.deps())
        .expect("the confirmation opens");
    app.on_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &f.deps())
        .expect("yes is picked");
    app.on_key(&key(KeyCode::Enter, KeyModifiers::NONE), &f.deps())
        .expect("the delete runs");

    assert!(app.entries().is_empty());
    assert_eq!(
        f.store.removed_transcripts(),
        vec![(record.cwd.clone(), record.id.clone())]
    );
    let saved = f.store.project("spm").expect("the project was saved");
    assert!(saved.sessions.is_empty());
}

#[test]
fn delete_leaves_the_worktree_and_the_branch_alone() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("feat A", [true, true]), &f.deps())
        .expect("one starts");
    let created = f.git.calls();
    app.toggle_pause(&f.deps()).expect("pause succeeds");

    app.delete_session(0, &f.deps()).expect("delete succeeds");

    assert_eq!(
        f.git.calls(),
        created,
        "git is not asked to remove anything"
    );
    assert!(app.status().contains("worktrees are still on disk"));
}

#[test]
fn delete_is_refused_while_the_session_runs() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");

    app.on_key(&key(KeyCode::Char('X'), KeyModifiers::ALT), &f.deps())
        .expect("the key is handled");

    assert!(app.confirm_delete().is_none(), "no confirmation opens");
    assert!(app.status().contains("pause it first"));

    app.delete_session(0, &f.deps())
        .expect("the direct call also refuses");
    assert_eq!(app.entries().len(), 1);
    assert!(f.store.removed_transcripts().is_empty());
}

/// Opens the confirmation on a paused session named `one`.
fn confirming_delete(f: &Fakes) -> App {
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.toggle_pause(&f.deps()).expect("pause succeeds");
    app.on_key(&key(KeyCode::Char('X'), KeyModifiers::ALT), &f.deps())
        .expect("the confirmation opens");
    app
}

#[test]
fn the_confirmation_starts_on_no() {
    let f = Fakes::new();
    let app = confirming_delete(&f);

    assert_eq!(
        app.confirm_delete()
            .map(culm::app::ConfirmDelete::confirmed),
        Some(false),
        "a stray Enter must never delete"
    );
}

#[test]
fn enter_alone_cancels_because_no_holds_the_cursor() {
    let f = Fakes::new();
    let mut app = confirming_delete(&f);

    app.on_key(&key(KeyCode::Enter, KeyModifiers::NONE), &f.deps())
        .expect("enter is handled");

    assert!(app.confirm_delete().is_none(), "the form closes");
    assert_eq!(app.entries().len(), 1, "the session survives");
    assert!(f.store.removed_transcripts().is_empty());
}

#[test]
fn n_then_enter_cancels() {
    let f = Fakes::new();
    let mut app = confirming_delete(&f);

    app.on_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &f.deps())
        .expect("yes is picked");
    app.on_key(&key(KeyCode::Char('n'), KeyModifiers::NONE), &f.deps())
        .expect("no is picked back");
    app.on_key(&key(KeyCode::Enter, KeyModifiers::NONE), &f.deps())
        .expect("enter is handled");

    assert_eq!(app.entries().len(), 1);
    assert!(f.store.removed_transcripts().is_empty());
}

#[test]
fn a_letter_alone_never_deletes() {
    let f = Fakes::new();
    let mut app = confirming_delete(&f);

    app.on_key(&key(KeyCode::Char('y'), KeyModifiers::NONE), &f.deps())
        .expect("yes is picked");

    assert!(app.confirm_delete().is_some(), "the form is still open");
    assert_eq!(app.entries().len(), 1, "deleting still takes the Enter");
}

#[test]
fn an_arrow_moves_between_the_buttons() {
    let f = Fakes::new();
    let mut app = confirming_delete(&f);

    app.on_key(&key(KeyCode::Left, KeyModifiers::NONE), &f.deps())
        .expect("the arrow is handled");
    assert_eq!(
        app.confirm_delete()
            .map(culm::app::ConfirmDelete::confirmed),
        Some(true)
    );

    app.on_key(&key(KeyCode::Tab, KeyModifiers::NONE), &f.deps())
        .expect("tab is handled");
    assert_eq!(
        app.confirm_delete()
            .map(culm::app::ConfirmDelete::confirmed),
        Some(false)
    );
}

#[test]
fn a_paste_never_answers_the_confirmation() {
    let f = Fakes::new();
    let mut app = confirming_delete(&f);

    app.on_paste("yes").expect("the paste is handled");

    assert!(app.confirm_delete().is_some(), "the form is still open");
    assert_eq!(app.entries().len(), 1);
}

#[test]
fn a_click_on_yes_deletes_and_a_click_on_no_does_not() {
    let f = Fakes::new();
    let mut app = confirming_delete(&f);
    let mut hit = panel_hit();
    hit.buttons = vec![
        (Rect::new(40, 6, 5, 1), culm::app::Answer::Yes),
        (Rect::new(47, 6, 4, 1), culm::app::Answer::No),
    ];

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        48,
        6,
        &hit,
        &f.deps(),
    );
    assert!(app.confirm_delete().is_none(), "the form closes");
    assert_eq!(app.entries().len(), 1, "no deletes nothing");

    let mut app = confirming_delete(&f);
    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        41,
        6,
        &hit,
        &f.deps(),
    );
    assert!(app.entries().is_empty(), "yes deletes");
    assert_eq!(f.store.removed_transcripts().len(), 1);
}

#[test]
fn cancelling_the_confirmation_deletes_nothing() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.toggle_pause(&f.deps()).expect("pause succeeds");

    app.on_key(&key(KeyCode::Char('X'), KeyModifiers::ALT), &f.deps())
        .expect("the confirmation opens");
    app.on_key(&key(KeyCode::Esc, KeyModifiers::NONE), &f.deps())
        .expect("escape cancels");

    assert!(app.confirm_delete().is_none());
    assert_eq!(app.entries().len(), 1);
    assert!(f.store.removed_transcripts().is_empty());
}

#[test]
fn delete_does_nothing_while_the_shell_is_focused() {
    let f = Fakes::new();
    let mut app = App::open(project(), &f.deps(), 20, 60);
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.toggle_pause(&f.deps()).expect("pause succeeds");
    app.focus_shell();

    app.on_key(&key(KeyCode::Char('X'), KeyModifiers::ALT), &f.deps())
        .expect("the key is handled");

    assert!(app.confirm_delete().is_none());
    assert_eq!(app.entries().len(), 1);
}

#[test]
fn rm_removes_the_registry_entry_the_state_and_the_root_transcripts() {
    let f = Fakes::new();
    let mut registry = culm::project::Registry::default();
    registry.add(ROOT);
    f.store.save_registry(&registry).expect("registry saves");
    f.store.put_project(&project());
    f.store
        .put_claude_dirs(["-home-x-spm", "-home-x-spm--worktrees-api-mate-feat-a"]);

    culm::cli::run(
        culm::cli::Cli::parse_from(["culm", "project", "rm", "spm", "--force"]),
        &f.store,
        &f.git,
        &culm::testing::FakeNamer::default(),
        std::path::Path::new("/usr/bin/culm"),
        std::path::Path::new("/tmp"),
    )
    .expect("rm succeeds");

    assert!(f.store.load_registry().expect("load").projects.is_empty());
    assert!(f.store.project("spm").is_none());
    assert_eq!(
        f.store.list_claude_dirs().expect("list"),
        vec!["-home-x-spm--worktrees-api-mate-feat-a".to_string()],
        "without --recursive only the root transcripts go"
    );
    assert!(
        f.git.removed().is_empty(),
        "no worktree without --recursive"
    );
}

#[test]
fn rm_recursive_takes_every_transcript_under_the_root_and_the_worktrees() {
    let f = Fakes::new();
    let mut registry = culm::project::Registry::default();
    registry.add(ROOT);
    f.store.save_registry(&registry).expect("registry saves");

    let mut project = project();
    project.sessions = vec![SessionRecord {
        id: "id-1".into(),
        name: "feat A".into(),
        slug: "feat-a".into(),
        state: SessionState::Paused,
        cwd: PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a"),
        repos: vec![culm::project::SessionRepo {
            name: "api-mate".into(),
            worktree: PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a"),
            branch: "feat-a".into(),
        }],
        started: true,
        last_active: 0,
    }];
    f.store.put_project(&project);
    f.store.put_claude_dirs([
        "-home-x-spm",
        "-home-x-spm--worktrees-api-mate-feat-a",
        "-home-x-other",
    ]);
    f.git.mark_dirty("/home/x/spm/.worktrees/api-mate-feat-a");

    culm::cli::run(
        culm::cli::Cli::parse_from(["culm", "project", "rm", "spm", "--force", "--recursive"]),
        &f.store,
        &f.git,
        &culm::testing::FakeNamer::default(),
        std::path::Path::new("/usr/bin/culm"),
        std::path::Path::new("/tmp"),
    )
    .expect("rm succeeds");

    assert_eq!(
        f.store.list_claude_dirs().expect("list"),
        vec!["-home-x-other".to_string()],
        "a project outside the root is untouched"
    );
    assert_eq!(
        f.git.removed(),
        vec![PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a")]
    );
    assert!(
        f.git.calls().is_empty(),
        "no branch is created or touched during a removal"
    );
}

#[test]
fn rm_refuses_an_unknown_project() {
    let f = Fakes::new();
    let result = culm::cli::run(
        culm::cli::Cli::parse_from(["culm", "project", "rm", "absent", "--force"]),
        &f.store,
        &f.git,
        &culm::testing::FakeNamer::default(),
        std::path::Path::new("/usr/bin/culm"),
        std::path::Path::new("/tmp"),
    );
    assert!(result.is_err());
}

/// Two hundred numbered lines, enough to fill a small panel and its scrollback.
fn numbered_lines() -> Vec<u8> {
    (0..200)
        .map(|n| format!("line-{n:03}\r\n"))
        .collect::<String>()
        .into_bytes()
}

fn panel_hit() -> HitBox {
    HitBox {
        sidebar_width: 30,
        separator_col: 29,
        rows: Vec::new(),
        panel: Rect::new(30, 0, 50, 10),
        inner: culm::ui::inner_of(Rect::new(30, 0, 50, 10)),
        search_row: None,
        ..HitBox::default()
    }
}

/// A session filled with `numbered_lines`, on a panel small enough to scroll.
fn scrolled_session(f: &Fakes) -> App {
    let mut app = App::new(project());
    app.resize_all(6, 40).expect("resize succeeds");
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    let session = app.entries()[0].live.as_ref().expect("one is live");
    assert!(
        session.wait_for_text("line-199", Duration::from_secs(1)),
        "the session drained its output"
    );
    app
}

fn scrollback_of(app: &App) -> usize {
    app.entries()[0]
        .live
        .as_ref()
        .expect("one is live")
        .scrollback()
}

fn screen_of(app: &App) -> String {
    app.entries()[0]
        .live
        .as_ref()
        .expect("one is live")
        .screen_text()
}

#[test]
fn the_wheel_scrolls_the_visible_session_back() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = scrolled_session(&f);
    let hit = panel_hit();
    assert!(screen_of(&app).contains("line-199"));

    for _ in 0..4 {
        app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());
    }

    assert_eq!(scrollback_of(&app), 12, "three lines per notch");
    assert!(
        !screen_of(&app).contains("line-199"),
        "the view is held above the live output"
    );

    for _ in 0..4 {
        app.on_mouse(MouseEventKind::ScrollDown, 40, 5, &hit, &f.deps());
    }
    assert_eq!(scrollback_of(&app), 0);
    assert!(screen_of(&app).contains("line-199"));
}

#[test]
fn a_keystroke_snaps_the_session_back_to_the_bottom() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = scrolled_session(&f);
    let hit = panel_hit();
    for _ in 0..4 {
        app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());
    }
    assert!(scrollback_of(&app) > 0);

    app.on_key(&key(KeyCode::Char('h'), KeyModifiers::NONE), &f.deps())
        .expect("the key is forwarded");

    assert_eq!(scrollback_of(&app), 0, "typing returns to the live output");
    assert!(screen_of(&app).contains("line-199"));
}

#[test]
fn the_wheel_over_the_sidebar_scrolls_nothing() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = scrolled_session(&f);
    let hit = panel_hit();

    app.on_mouse(MouseEventKind::ScrollUp, 5, 3, &hit, &f.deps());

    assert_eq!(scrollback_of(&app), 0, "the wheel belongs to the panel");
}

#[test]
fn shift_page_up_scrolls_half_a_panel() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = scrolled_session(&f);

    app.on_key(&key(KeyCode::PageUp, KeyModifiers::SHIFT), &f.deps())
        .expect("the host key is handled");

    assert_eq!(scrollback_of(&app), 3, "half of a six row panel");
    app.on_key(&key(KeyCode::PageDown, KeyModifiers::SHIFT), &f.deps())
        .expect("the host key is handled");
    assert_eq!(scrollback_of(&app), 0);
}

#[test]
fn scrolling_stops_at_both_ends_of_the_buffer() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = scrolled_session(&f);
    let hit = panel_hit();

    for _ in 0..200 {
        app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());
    }
    let top = scrollback_of(&app);
    assert!(top > 0, "the buffer holds history");
    assert!(top < 600, "the view stops at the oldest line it kept");
    assert!(screen_of(&app).contains("line-000"));

    for _ in 0..500 {
        app.on_mouse(MouseEventKind::ScrollDown, 40, 5, &hit, &f.deps());
    }
    assert_eq!(scrollback_of(&app), 0, "the view stops at the live output");
}

#[test]
fn the_wheel_does_nothing_while_a_form_is_open() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = scrolled_session(&f);
    let hit = panel_hit();
    app.on_key(&key(KeyCode::Char('N'), KeyModifiers::ALT), &f.deps())
        .expect("the form opens");

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());

    assert_eq!(scrollback_of(&app), 0);
}

/// Output that turns on mouse reporting with the SGR encoding, the way Claude Code
/// does, followed by ordinary lines.
fn mouse_grabbing_output() -> Vec<u8> {
    let mut v = b"\x1b[?1000h\x1b[?1006h".to_vec();
    v.extend(numbered_lines());
    v
}

#[test]
fn the_wheel_goes_to_a_child_that_asked_for_the_mouse() {
    let f = Fakes::with_output(&mouse_grabbing_output());
    let mut app = scrolled_session(&f);
    let hit = panel_hit();

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());

    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;
    assert_eq!(
        pty.written_utf8(),
        "\x1b[<64;10;5M",
        "the notch reaches the child, which owns its own history"
    );
    assert_eq!(
        scrollback_of(&app),
        0,
        "culm does not also move its own scrollback"
    );
}

#[test]
fn the_wheel_down_reaches_the_child_as_the_other_button() {
    let f = Fakes::with_output(&mouse_grabbing_output());
    let mut app = scrolled_session(&f);
    let hit = panel_hit();

    app.on_mouse(MouseEventKind::ScrollDown, 31, 1, &hit, &f.deps());

    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;
    assert_eq!(
        pty.written_utf8(),
        "\x1b[<65;1;1M",
        "the panel border is the origin"
    );
}

#[test]
fn the_wheel_still_scrolls_culm_when_the_child_wants_no_mouse() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = scrolled_session(&f);
    let hit = panel_hit();

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());

    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;
    assert_eq!(pty.written_utf8(), "", "a shell gets no mouse bytes");
    assert_eq!(scrollback_of(&app), 3);
}

#[test]
fn typing_stamps_the_session_with_the_time_of_the_keystroke() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("session starts");

    f.clock.set(4_242);
    app.on_key(&key(KeyCode::Char('h'), KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");

    assert_eq!(app.entries()[0].record.last_active, 4_242);
}

#[test]
fn a_stamp_from_a_keystroke_reaches_the_store_on_the_next_tick() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("session starts");
    let saves = f.store.saves();

    f.clock.set(4_242);
    app.on_key(&key(KeyCode::Char('h'), KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");
    assert_eq!(f.store.saves(), saves, "typing never costs a file write");

    app.on_tick(&FakeMemoryProbe::new(0), &f.deps())
        .expect("the tick runs");

    let saved = f.store.project("spm").expect("the project is saved");
    assert_eq!(saved.sessions[0].last_active, 4_242);
}

#[test]
fn pausing_and_resuming_both_stamp_the_session() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("session starts");

    f.clock.set(100);
    app.toggle_pause(&f.deps()).expect("pause");
    assert_eq!(app.entries()[0].record.last_active, 100);

    f.clock.set(200);
    app.toggle_pause(&f.deps()).expect("resume");
    assert_eq!(app.entries()[0].record.last_active, 200);
}

/// One active session named `one`, ready for a rename.
fn one_session(f: &Fakes) -> App {
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("session starts");
    app
}

#[test]
fn a_rename_changes_the_name_and_keeps_the_slug() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    let slug = app.entries()[0].record.slug.clone();

    app.on_key(&key(KeyCode::Char('R'), KeyModifiers::ALT), &f.deps())
        .expect("the form opens");
    assert_eq!(
        app.rename_form().map(|r| r.name.as_str()),
        Some("one"),
        "the form starts from the current name"
    );
    for c in " renamed".chars() {
        app.on_key(&key(KeyCode::Char(c), KeyModifiers::NONE), &f.deps())
            .expect("the key is handled");
    }
    app.on_key(&key(KeyCode::Enter, KeyModifiers::NONE), &f.deps())
        .expect("the rename applies");

    assert_eq!(app.entries()[0].record.name, "one renamed");
    assert_eq!(
        app.entries()[0].record.slug,
        slug,
        "the slug names the branch and the worktree, so it never moves"
    );
    let saved = f.store.project("spm").expect("the project is saved");
    assert_eq!(saved.sessions[0].name, "one renamed");
}

#[test]
fn a_paused_session_can_be_renamed() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.toggle_pause(&f.deps()).expect("pause");

    app.on_key(&key(KeyCode::Char('R'), KeyModifiers::ALT), &f.deps())
        .expect("the form opens");
    app.on_key(&key(KeyCode::Backspace, KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");
    app.on_key(&key(KeyCode::Char('2'), KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");
    app.on_key(&key(KeyCode::Enter, KeyModifiers::NONE), &f.deps())
        .expect("the rename applies");

    assert_eq!(app.entries()[0].record.name, "on2");
}

#[test]
fn escape_leaves_the_old_name() {
    let f = Fakes::new();
    let mut app = one_session(&f);

    app.on_key(&key(KeyCode::Char('R'), KeyModifiers::ALT), &f.deps())
        .expect("the form opens");
    app.on_key(&key(KeyCode::Char('x'), KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");
    app.on_key(&key(KeyCode::Esc, KeyModifiers::NONE), &f.deps())
        .expect("the form closes");

    assert!(app.rename_form().is_none());
    assert_eq!(app.entries()[0].record.name, "one");
}

#[test]
fn an_empty_name_is_refused() {
    let f = Fakes::new();
    let mut app = one_session(&f);

    app.rename_session(0, "   ", &f.deps())
        .expect("the rename is handled");

    assert_eq!(app.entries()[0].record.name, "one");
    assert_eq!(app.status(), "a session needs a name");
}

#[test]
fn keys_go_to_the_rename_form_and_never_to_the_child() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;

    app.on_key(&key(KeyCode::Char('R'), KeyModifiers::ALT), &f.deps())
        .expect("the form opens");
    app.on_key(&key(KeyCode::Char('z'), KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");
    app.on_paste("pasted").expect("the paste is handled");

    assert_eq!(pty.written_utf8(), "", "no byte reached the session");
    assert_eq!(
        app.rename_form().map(|r| r.name.as_str()),
        Some("onezpasted")
    );
}

#[test]
fn the_shell_has_no_name_to_rename() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.focus_shell();

    app.on_key(&key(KeyCode::Char('R'), KeyModifiers::ALT), &f.deps())
        .expect("the key is handled");

    assert!(app.rename_form().is_none());
    assert_eq!(app.status(), "the shell has no name");
}

/// A session whose screen already holds `settled`, so a drag reads known text.
fn text_session(f: &Fakes, settled: &str) -> App {
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("session starts");
    let session = app.entries()[0].live.as_ref().expect("live");
    assert!(session.wait_for_text(settled, Duration::from_secs(1)));
    app
}

/// The panel of `panel_hit` starts at column 30, and its border takes one cell, so
/// cell (row, col) sits at this screen position.
fn at(row: u16, col: u16) -> (u16, u16) {
    (31 + col, 1 + row)
}

fn drag(app: &mut App, f: &Fakes, hit: &HitBox, from: (u16, u16), to: (u16, u16)) {
    let d = f.deps();
    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        from.0,
        from.1,
        hit,
        &d,
    );
    app.on_mouse(MouseEventKind::Drag(MouseButton::Left), to.0, to.1, hit, &d);
    app.on_mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1, hit, &d);
}

#[test]
fn a_drag_over_the_panel_copies_the_text_under_it() {
    let f = Fakes::with_output(b"hello world\r\nsecond line\r\n");
    let mut app = text_session(&f, "second line");
    let hit = panel_hit();

    drag(&mut app, &f, &hit, at(0, 0), at(0, 4));

    assert_eq!(app.clipboard(), "hello");
    assert_eq!(app.take_copy().as_deref(), Some("hello"));
    assert_eq!(app.take_copy(), None, "the copy is handed over once");
    assert_eq!(app.status(), "copied 5 chars");
}

#[test]
fn a_drag_across_rows_copies_both_rows() {
    let f = Fakes::with_output(b"hello world\r\nsecond line\r\n");
    let mut app = text_session(&f, "second line");
    let hit = panel_hit();

    drag(&mut app, &f, &hit, at(0, 6), at(1, 5));

    assert_eq!(app.clipboard(), "world\nsecond");
}

#[test]
fn a_drag_backwards_copies_the_same_text() {
    let f = Fakes::with_output(b"hello world\r\nsecond line\r\n");
    let mut app = text_session(&f, "second line");
    let hit = panel_hit();

    drag(&mut app, &f, &hit, at(0, 4), at(0, 0));

    assert_eq!(app.clipboard(), "hello");
}

#[test]
fn a_click_without_a_drag_selects_nothing() {
    let f = Fakes::with_output(b"hello world\r\n");
    let mut app = text_session(&f, "hello world");
    let hit = panel_hit();

    drag(&mut app, &f, &hit, at(0, 3), at(0, 3));

    assert!(app.selection().is_none());
    assert_eq!(app.take_copy(), None);
    assert_eq!(app.clipboard(), "");
}

#[test]
fn a_middle_click_pastes_the_last_copy_as_one_bracketed_block() {
    let f = Fakes::with_output(b"hello world\r\n");
    let mut app = text_session(&f, "hello world");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 4));
    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Middle),
        at(2, 2).0,
        at(2, 2).1,
        &hit,
        &f.deps(),
    );

    assert_eq!(pty.written_utf8(), "\x1b[200~hello\x1b[201~");
}

#[test]
fn a_middle_click_with_nothing_copied_sends_nothing() {
    let f = Fakes::with_output(b"hello world\r\n");
    let mut app = text_session(&f, "hello world");
    let hit = panel_hit();
    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Middle),
        at(0, 0).0,
        at(0, 0).1,
        &hit,
        &f.deps(),
    );

    assert_eq!(pty.written_utf8(), "");
}

#[test]
fn a_keystroke_clears_the_selection() {
    let f = Fakes::with_output(b"hello world\r\n");
    let mut app = text_session(&f, "hello world");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 4));
    assert!(app.selection().is_some());

    app.on_key(&key(KeyCode::Char('x'), KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");

    assert!(app.selection().is_none());
    assert_eq!(
        app.clipboard(),
        "hello",
        "the copy survives, so a later middle click still pastes it"
    );
}

#[test]
fn the_wheel_carries_the_selection_with_the_text_it_marks() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = text_session(&f, "line-199");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 7));
    assert_eq!(app.clipboard(), "line-177");

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());

    assert_eq!(
        app.selection(),
        Some(((3, 0), (3, 7))),
        "one notch moved the text three rows down the panel, and the selection with it"
    );
}

/// Writes bytes straight into a session's screen, standing in for the child. The
/// fake spawner replays one fixed stream, so a repaint after the wheel arrives here.
fn repaint(app: &App, bytes: &[u8]) {
    let session = app.entries()[0].live.as_ref().expect("one is live");
    let mut parser = session.parser().lock().expect("the parser is free");
    parser.process(bytes);
}

/// Redraws a panel of numbered lines starting at `first`, which is what a child that
/// owns its own scrolling does when it moves its conversation.
fn redraw_from(app: &App, first: usize, rows: usize) {
    let mut out = b"\x1b[2J\x1b[H".to_vec();
    for n in first..first + rows {
        // No newline after the last row, which would scroll the panel one further.
        if n > first {
            out.extend(b"\r\n");
        }
        out.extend(format!("line-{n:03}").into_bytes());
    }
    repaint(app, &out);
}

#[test]
fn a_forwarded_wheel_carries_the_selection_by_what_the_child_moved() {
    let f = Fakes::with_output(&mouse_grabbing_output());
    let mut app = text_session(&f, "line-199");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 7));
    assert_eq!(app.clipboard(), "line-177");

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());
    // The child answers the notch by repainting two lines further back.
    redraw_from(&app, 175, 24);
    app.settle_scroll();

    assert_eq!(
        app.selection(),
        Some(((2, 0), (2, 7))),
        "line-177 is now two rows further down, and the marks moved with it"
    );
}

#[test]
fn a_forwarded_wheel_drops_the_selection_when_the_child_drew_a_fresh_screen() {
    let f = Fakes::with_output(&mouse_grabbing_output());
    let mut app = text_session(&f, "line-199");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 7));

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());
    repaint(
        &app,
        b"\x1b[2J\x1b[Hnothing here matches what was on the panel before\r\n",
    );
    app.settle_scroll();

    assert!(
        app.selection().is_none(),
        "that was a new screen, not a scroll, so the marks name nothing"
    );
}

#[test]
fn a_child_that_ignores_the_wheel_gives_the_selection_back() {
    let f = Fakes::with_output(&mouse_grabbing_output());
    let mut app = text_session(&f, "line-199");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 7));

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());
    // The panel never changes, so the wait runs out rather than lasting for ever.
    for _ in 0..200 {
        app.settle_scroll();
    }
    redraw_from(&app, 175, 24);
    app.settle_scroll();

    assert_eq!(
        app.selection(),
        Some(((0, 0), (0, 7))),
        "the measurement gave up, so a later repaint never moves the marks"
    );
}

#[test]
fn a_wheel_the_child_handles_leaves_the_selection_where_it_is() {
    let f = Fakes::with_output(&mouse_grabbing_output());
    let mut app = text_session(&f, "line-199");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 7));

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());

    assert_eq!(
        app.selection(),
        Some(((0, 0), (0, 7))),
        "the child repaints its own panel, so culm's view never moved"
    );
}

#[test]
fn shift_page_up_carries_the_selection_too() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = text_session(&f, "line-199");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 7));

    app.on_key(&key(KeyCode::PageUp, KeyModifiers::SHIFT), &f.deps())
        .expect("the panel scrolls");

    assert_eq!(
        app.selection(),
        Some(((12, 0), (12, 7))),
        "half of a twenty-four row panel"
    );
}

#[test]
fn alt_click_extends_the_selection_instead_of_starting_a_new_one() {
    let f = Fakes::with_output(b"hello world\r\nsecond line\r\n");
    let mut app = text_session(&f, "second line");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 4));
    assert_eq!(app.clipboard(), "hello");
    let d = f.deps();

    app.on_mouse_with(
        MouseEventKind::Down(MouseButton::Left),
        at(1, 5).0,
        at(1, 5).1,
        KeyModifiers::ALT,
        &hit,
        &d,
    );
    app.on_mouse_with(
        MouseEventKind::Up(MouseButton::Left),
        at(1, 5).0,
        at(1, 5).1,
        KeyModifiers::ALT,
        &hit,
        &d,
    );

    assert_eq!(app.clipboard(), "hello world\nsecond");
}

#[test]
fn alt_click_extends_a_selection_that_scrolling_carried() {
    let f = Fakes::with_output(&numbered_lines());
    let mut app = text_session(&f, "line-199");
    let hit = panel_hit();
    drag(&mut app, &f, &hit, at(0, 0), at(0, 7));
    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit, &f.deps());
    let d = f.deps();

    app.on_mouse_with(
        MouseEventKind::Down(MouseButton::Left),
        at(5, 7).0,
        at(5, 7).1,
        KeyModifiers::ALT,
        &hit,
        &d,
    );
    app.on_mouse_with(
        MouseEventKind::Up(MouseButton::Left),
        at(5, 7).0,
        at(5, 7).1,
        KeyModifiers::ALT,
        &hit,
        &d,
    );

    assert_eq!(
        app.clipboard(),
        "line-177\nline-178\nline-179",
        "the anchor still marks the line it was put on before the scroll"
    );
}

#[test]
fn a_modifier_with_nothing_selected_starts_a_selection() {
    let f = Fakes::with_output(b"hello world\r\n");
    let mut app = text_session(&f, "hello world");
    let hit = panel_hit();

    app.on_mouse_with(
        MouseEventKind::Down(MouseButton::Left),
        at(0, 2).0,
        at(0, 2).1,
        KeyModifiers::ALT,
        &hit,
        &f.deps(),
    );

    assert_eq!(app.selection(), Some(((0, 2), (0, 2))));
}

#[test]
fn text_carried_off_the_panel_is_read_back_out_of_the_scrollback() {
    let f = Fakes::with_output(&numbered_lines());
    let app = text_session(&f, "line-199");
    let session = app.entries()[0].live.as_ref().expect("one is live");
    assert_eq!(session.text_between((0, 0), (0, 7)), "line-177");

    assert_eq!(session.scroll_by(24), 24, "a whole panel of scrollback");

    assert_eq!(
        session.text_between((24, 0), (24, 7)),
        "line-177",
        "the same line, now one whole panel below the view"
    );
    assert_eq!(
        session.scrollback(),
        24,
        "the reader put the view back where it found it"
    );
}

#[test]
fn a_selection_taller_than_the_panel_reads_every_row_of_it() {
    let f = Fakes::with_output(&numbered_lines());
    let app = text_session(&f, "line-199");
    let session = app.entries()[0].live.as_ref().expect("one is live");
    session.scroll_by(24);

    let text = session.text_between((24, 0), (26, 7));

    assert_eq!(text, "line-177\nline-178\nline-179");
}

#[test]
fn a_drag_over_the_sidebar_never_copies() {
    let f = Fakes::with_output(b"hello world\r\n");
    let mut app = text_session(&f, "hello world");
    let hit = panel_hit();

    drag(&mut app, &f, &hit, (5, 2), (5, 6));

    assert!(app.selection().is_none());
    assert_eq!(app.take_copy(), None);
}

#[test]
fn no_selection_starts_while_a_form_is_open() {
    let f = Fakes::with_output(b"hello world\r\n");
    let mut app = text_session(&f, "hello world");
    let hit = panel_hit();
    app.on_key(&key(KeyCode::Char('N'), KeyModifiers::ALT), &f.deps())
        .expect("the form opens");

    drag(&mut app, &f, &hit, at(0, 0), at(0, 4));

    assert!(app.selection().is_none());
    assert_eq!(app.take_copy(), None);
}

#[test]
fn a_selection_over_the_shell_copies_from_the_shell() {
    let f = Fakes::with_output(b"a shell prompt\r\n");
    let mut app = App::open(project(), &f.deps(), 20, 50);
    let hit = panel_hit();
    let shell = app.shell().expect("the shell runs");
    assert!(shell.wait_for_text("a shell prompt", Duration::from_secs(1)));

    drag(&mut app, &f, &hit, at(0, 0), at(0, 6));

    assert_eq!(app.clipboard(), "a shell");
}

/// Three sessions, paused in a known order, each one second apart.
fn three_paused(f: &Fakes) -> App {
    let mut app = App::new(project());
    for (name, at) in [("alpha", 100), ("beta", 200), ("gamma", 300)] {
        f.clock.set(at);
        app.create_session(&form(name, [false, false]), &f.deps())
            .expect("session starts");
        app.toggle_pause(&f.deps()).expect("pause");
    }
    app
}

#[test]
fn paused_sessions_are_listed_most_recent_first() {
    let f = Fakes::new();
    let app = three_paused(&f);

    let names: Vec<&str> = app.entries().iter().map(culm::app::Entry::name).collect();
    assert_eq!(names, vec!["gamma", "beta", "alpha"]);
}

#[test]
fn resuming_a_session_moves_it_to_the_front_of_the_paused_list_when_it_pauses_again() {
    let f = Fakes::new();
    let mut app = three_paused(&f);
    let alpha = app
        .entries()
        .iter()
        .position(|e| e.name() == "alpha")
        .expect("alpha is listed");
    app.set_focus(alpha);

    f.clock.set(400);
    app.toggle_pause(&f.deps()).expect("resume");
    f.clock.set(500);
    app.toggle_pause(&f.deps()).expect("pause again");

    let names: Vec<&str> = app.entries().iter().map(culm::app::Entry::name).collect();
    assert_eq!(names, vec!["alpha", "gamma", "beta"]);
}

#[test]
fn the_active_half_keeps_its_order_because_a_digit_addresses_it() {
    let f = Fakes::new();
    let mut app = App::new(project());
    for (name, at) in [("alpha", 300), ("beta", 200), ("gamma", 100)] {
        f.clock.set(at);
        app.create_session(&form(name, [false, false]), &f.deps())
            .expect("session starts");
    }

    let names: Vec<&str> = app.entries().iter().map(culm::app::Entry::name).collect();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);
}

fn find(app: &mut App, f: &Fakes) {
    app.on_key(&key(KeyCode::Char('F'), KeyModifiers::ALT), &f.deps())
        .expect("the search box opens");
}

fn type_into(app: &mut App, f: &Fakes, text: &str) {
    for c in text.chars() {
        app.on_key(&key(KeyCode::Char(c), KeyModifiers::NONE), &f.deps())
            .expect("the key is handled");
    }
}

#[test]
fn typing_in_the_search_box_filters_the_paused_list() {
    let f = Fakes::new();
    let mut app = three_paused(&f);

    find(&mut app, &f);
    type_into(&mut app, &f, "be");

    let matched: Vec<&str> = app
        .paused_matches()
        .into_iter()
        .map(|i| app.entries()[i].name())
        .collect();
    assert_eq!(matched, vec!["beta"]);
}

#[test]
fn the_filter_ignores_case() {
    let f = Fakes::new();
    let mut app = three_paused(&f);

    find(&mut app, &f);
    type_into(&mut app, &f, "GAM");

    assert_eq!(app.paused_matches().len(), 1);
}

#[test]
fn a_backspace_widens_the_filter_again() {
    let f = Fakes::new();
    let mut app = three_paused(&f);

    find(&mut app, &f);
    type_into(&mut app, &f, "beta");
    app.on_key(&key(KeyCode::Backspace, KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");

    assert_eq!(app.filter(), "bet");
    assert_eq!(app.paused_matches().len(), 1);
}

#[test]
fn escape_clears_the_filter_and_returns_keys_to_the_panel() {
    let f = Fakes::new();
    let mut app = three_paused(&f);
    find(&mut app, &f);
    type_into(&mut app, &f, "beta");

    app.on_key(&key(KeyCode::Esc, KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");

    assert_eq!(app.filter(), "");
    assert!(!app.filter_focused());
    assert_eq!(app.paused_matches().len(), 3);
}

#[test]
fn enter_focuses_the_first_match_and_keeps_the_filter() {
    let f = Fakes::new();
    let mut app = three_paused(&f);
    find(&mut app, &f);
    type_into(&mut app, &f, "alp");

    app.on_key(&key(KeyCode::Enter, KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");

    assert!(!app.filter_focused());
    assert_eq!(
        app.filter(),
        "alp",
        "the list still shows what was searched"
    );
    let focused = app.focused_entry().expect("a session holds the focus");
    assert_eq!(app.entries()[focused].name(), "alpha");
}

#[test]
fn keys_go_to_the_search_box_and_never_to_the_child() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("session starts");
    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;

    find(&mut app, &f);
    type_into(&mut app, &f, "abc");
    app.on_paste("pasted").expect("the paste is handled");

    assert_eq!(pty.written_utf8(), "", "no byte reached the session");
    assert_eq!(app.filter(), "abcpasted");
}

#[test]
fn an_alt_action_still_works_while_the_search_box_is_focused() {
    let f = Fakes::new();
    let mut app = three_paused(&f);
    find(&mut app, &f);

    app.on_key(&key(KeyCode::Char('0'), KeyModifiers::ALT), &f.deps())
        .expect("the key is handled");

    assert_eq!(app.focus(), Focus::Shell);
}

#[test]
fn a_click_on_the_search_row_focuses_the_box() {
    let f = Fakes::new();
    let mut app = three_paused(&f);
    let mut hit = panel_hit();
    hit.search_row = Some(7);

    app.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        5,
        7,
        &hit,
        &f.deps(),
    );

    assert!(app.filter_focused());
}

/// A registered project, ready for a rename.
fn registered(f: &Fakes, slug: &str, root: &str) {
    let mut registry = f.store.load_registry().expect("load");
    registry.add_as(root, slug).expect("the slug is free");
    f.store.save_registry(&registry).expect("registry saves");
    f.store.put_project(&Project::new(slug, root));
}

fn cli(f: &Fakes, args: &[&str]) -> anyhow::Result<culm::cli::Outcome> {
    culm::cli::run(
        culm::cli::Cli::parse_from(args),
        &f.store,
        &f.git,
        &culm::testing::FakeNamer::default(),
        std::path::Path::new("/usr/bin/culm"),
        std::path::Path::new(ROOT),
    )
}

/// A real directory, because `culm project new` canonicalizes the path it is given.
fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("culm-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::canonicalize(&dir).expect("canonical")
}

#[test]
fn project_new_takes_the_name_from_the_flag() {
    let f = Fakes::new();
    let root = temp_root("named");

    cli(
        &f,
        &[
            "culm",
            "project",
            "new",
            &root.display().to_string(),
            "--name",
            "Billing Rewrite",
        ],
    )
    .expect("project new succeeds");

    let registry = f.store.load_registry().expect("load");
    assert_eq!(registry.projects[0].slug, "billing-rewrite");
    assert_eq!(registry.projects[0].root, root);
    assert!(
        f.store.project("billing-rewrite").is_some(),
        "the state file is named after the project"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn project_new_without_the_flag_still_takes_the_directory_name() {
    let f = Fakes::new();
    let root = temp_root("unnamed");

    cli(&f, &["culm", "project", "new", &root.display().to_string()])
        .expect("project new succeeds");

    let registry = f.store.load_registry().expect("load");
    assert_eq!(
        registry.projects[0].slug,
        culm::project::slugify(&root.file_name().expect("name").to_string_lossy())
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn project_new_refuses_a_name_another_project_holds() {
    let f = Fakes::new();
    registered(&f, "billing", "/home/x/other");
    let root = temp_root("collides");

    let result = cli(
        &f,
        &[
            "culm",
            "project",
            "new",
            &root.display().to_string(),
            "--name",
            "billing",
        ],
    );

    assert!(result.is_err(), "a taken name is refused, never numbered");
    assert_eq!(
        f.store.load_registry().expect("load").projects.len(),
        1,
        "nothing is registered"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn altering_the_name_moves_the_registry_entry_and_the_state_file() {
    let f = Fakes::new();
    registered(&f, "spm", ROOT);

    cli(&f, &["culm", "project", "alter", "name", "Billing Rewrite"]).expect("the rename succeeds");

    let registry = f.store.load_registry().expect("load");
    assert_eq!(registry.projects[0].slug, "billing-rewrite");
    assert_eq!(registry.projects[0].root, PathBuf::from(ROOT));
    let moved = f.store.project("billing-rewrite").expect("the state moved");
    assert_eq!(moved.slug, "billing-rewrite");
    assert_eq!(moved.root, PathBuf::from(ROOT));
    assert!(
        f.store.project("spm").is_none(),
        "the old state file is gone"
    );
}

#[test]
fn open_finds_the_project_under_its_new_name() {
    let f = Fakes::new();
    registered(&f, "spm", ROOT);
    cli(&f, &["culm", "project", "alter", "name", "billing"]).expect("the rename succeeds");

    let outcome = cli(&f, &["culm", "open", "billing"]).expect("open succeeds");

    match outcome {
        culm::cli::Outcome::Open(project) => assert_eq!(project.slug, "billing"),
        culm::cli::Outcome::Done => panic!("open must hand back a project"),
    }
    assert!(
        cli(&f, &["culm", "open", "spm"]).is_err(),
        "the old name no longer resolves"
    );
}

#[test]
fn altering_the_name_keeps_the_sessions_and_the_repositories() {
    let f = Fakes::new();
    let mut original = project();
    original.sessions = vec![record("one", SessionState::Paused)];
    let mut registry = culm::project::Registry::default();
    registry.add(ROOT);
    f.store.save_registry(&registry).expect("registry saves");
    f.store.put_project(&original);

    cli(&f, &["culm", "project", "alter", "name", "billing"]).expect("the rename succeeds");

    let moved = f.store.project("billing").expect("the state moved");
    assert_eq!(moved.sessions, original.sessions);
    assert_eq!(moved.repos, original.repos);
}

#[test]
fn altering_to_a_taken_name_changes_nothing() {
    let f = Fakes::new();
    registered(&f, "spm", ROOT);
    registered(&f, "billing", "/home/x/billing");

    let result = cli(&f, &["culm", "project", "alter", "name", "billing"]);

    assert!(result.is_err());
    let registry = f.store.load_registry().expect("load");
    assert!(
        registry.find("spm").is_some(),
        "the old name still resolves"
    );
    assert_eq!(
        registry.find("billing").map(|p| p.root.as_path()),
        Some(std::path::Path::new("/home/x/billing")),
        "the other project keeps its root"
    );
    assert!(f.store.project("spm").is_some());
}

#[test]
fn an_empty_project_name_is_refused() {
    let f = Fakes::new();
    registered(&f, "spm", ROOT);

    assert!(cli(&f, &["culm", "project", "alter", "name", "   "]).is_err());
    assert!(f.store.project("spm").is_some());
}

#[test]
fn altering_names_the_project_that_owns_the_working_directory() {
    let f = Fakes::new();
    registered(&f, "spm", ROOT);
    registered(&f, "other", "/home/x/other");

    cli(&f, &["culm", "project", "alter", "name", "billing"]).expect("the rename succeeds");

    let registry = f.store.load_registry().expect("load");
    assert!(registry.find("billing").is_some());
    assert!(
        registry.find("other").is_some(),
        "a project the working directory does not belong to is untouched"
    );
}

#[test]
fn altering_a_named_project_reaches_it_from_anywhere() {
    let f = Fakes::new();
    registered(&f, "spm", ROOT);
    registered(&f, "other", "/home/x/other");

    cli(
        &f,
        &[
            "culm",
            "project",
            "alter",
            "name",
            "billing",
            "--project",
            "other",
        ],
    )
    .expect("the rename succeeds");

    let registry = f.store.load_registry().expect("load");
    assert!(registry.find("billing").is_some());
    assert!(registry.find("spm").is_some());
}

#[test]
fn altering_a_name_to_the_one_it_already_has_is_not_a_failure() {
    let f = Fakes::new();
    registered(&f, "spm", ROOT);

    cli(&f, &["culm", "project", "alter", "name", "spm"]).expect("the command succeeds");

    assert!(f.store.project("spm").is_some(), "the state is still there");
}

#[test]
fn repo_list_names_every_repository_of_the_project_at_the_working_directory() {
    let f = Fakes::new();
    let mut registry = culm::project::Registry::default();
    registry.add(ROOT);
    f.store.save_registry(&registry).expect("registry saves");
    f.store.put_project(&project());

    cli(&f, &["culm", "project", "repo", "list"]).expect("the list runs");

    let listed = f.store.project("spm").expect("the project is there");
    assert_eq!(
        listed
            .repos
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        vec!["api-mate", "crm-mate"],
        "listing reads the repositories and changes none of them"
    );
    assert_eq!(f.store.saves(), 0, "a list writes nothing");
}

#[test]
fn repo_list_reaches_another_project_by_name() {
    let f = Fakes::new();
    registered(&f, "other", "/home/x/other");

    cli(
        &f,
        &["culm", "project", "repo", "list", "--project", "other"],
    )
    .expect("the list runs");
}

#[test]
fn repo_list_refuses_a_directory_that_belongs_to_no_project() {
    let f = Fakes::new();

    let result = cli(&f, &["culm", "project", "repo", "list"]);

    assert!(
        result.is_err(),
        "there is no project to list repositories of"
    );
}

#[test]
fn alt_shift_h_opens_and_closes_the_shortcut_table() {
    let f = Fakes::new();
    let mut app = one_session(&f);

    app.on_key(&key(KeyCode::Char('H'), KeyModifiers::ALT), &f.deps())
        .expect("the table opens");
    assert!(app.help_open());

    app.on_key(&key(KeyCode::Char('H'), KeyModifiers::ALT), &f.deps())
        .expect("the same key closes it");
    assert!(!app.help_open());
}

#[test]
fn escape_closes_the_shortcut_table() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    app.on_key(&key(KeyCode::Char('H'), KeyModifiers::ALT), &f.deps())
        .expect("the table opens");

    app.on_key(&key(KeyCode::Esc, KeyModifiers::NONE), &f.deps())
        .expect("escape closes it");

    assert!(!app.help_open());
}

#[test]
fn no_key_reaches_the_child_while_the_shortcut_table_is_open() {
    let f = Fakes::new();
    let mut app = one_session(&f);
    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;
    app.on_key(&key(KeyCode::Char('H'), KeyModifiers::ALT), &f.deps())
        .expect("the table opens");

    app.on_key(&key(KeyCode::Char('z'), KeyModifiers::NONE), &f.deps())
        .expect("the key is handled");
    app.on_paste("pasted").expect("the paste is handled");

    assert_eq!(pty.written_utf8(), "");
    assert!(app.help_open(), "an ordinary key leaves the table open");
}

#[test]
fn the_shortcut_table_opens_over_the_shell_as_well() {
    let f = Fakes::new();
    let mut app = App::open(project(), &f.deps(), 20, 60);

    app.on_key(&key(KeyCode::Char('H'), KeyModifiers::ALT), &f.deps())
        .expect("the table opens");

    assert!(app.help_open());
}
