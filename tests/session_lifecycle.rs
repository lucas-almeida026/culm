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
    };

    app.on_mouse(MouseEventKind::Down(MouseButton::Left), 5, 2, &hit);

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
        rows: vec![(2, Focus::Entry(0)), (3, Focus::Entry(1))],
        panel: Rect::new(30, 0, 50, 20),
    };

    app.on_mouse(MouseEventKind::Down(MouseButton::Left), 60, 2, &hit);

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
    };

    app.on_mouse(MouseEventKind::Down(MouseButton::Left), 5, 4, &hit);
    assert_eq!(app.focus(), Focus::Entry(0));

    app.on_mouse(MouseEventKind::Down(MouseButton::Left), 5, 1, &hit);
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
    for c in "feat A".chars() {
        app.on_key(&key(KeyCode::Char(c), KeyModifiers::NONE), &f.deps())
            .expect("the name is typed");
    }
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

#[test]
fn the_wrong_name_deletes_nothing() {
    let f = Fakes::new();
    let mut app = App::new(project());
    app.create_session(&form("one", [false, false]), &f.deps())
        .expect("one starts");
    app.toggle_pause(&f.deps()).expect("pause succeeds");

    app.on_key(&key(KeyCode::Char('X'), KeyModifiers::ALT), &f.deps())
        .expect("the confirmation opens");
    for c in "onx".chars() {
        app.on_key(&key(KeyCode::Char(c), KeyModifiers::NONE), &f.deps())
            .expect("the name is typed");
    }
    app.on_key(&key(KeyCode::Enter, KeyModifiers::NONE), &f.deps())
        .expect("enter is handled");

    assert_eq!(app.entries().len(), 1, "the session survives");
    assert!(f.store.removed_transcripts().is_empty());
    assert!(app.confirm_delete().is_some(), "the form stays open");
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
        app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit);
    }

    assert_eq!(scrollback_of(&app), 12, "three lines per notch");
    assert!(
        !screen_of(&app).contains("line-199"),
        "the view is held above the live output"
    );

    for _ in 0..4 {
        app.on_mouse(MouseEventKind::ScrollDown, 40, 5, &hit);
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
        app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit);
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

    app.on_mouse(MouseEventKind::ScrollUp, 5, 3, &hit);

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
        app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit);
    }
    let top = scrollback_of(&app);
    assert!(top > 0, "the buffer holds history");
    assert!(top < 600, "the view stops at the oldest line it kept");
    assert!(screen_of(&app).contains("line-000"));

    for _ in 0..500 {
        app.on_mouse(MouseEventKind::ScrollDown, 40, 5, &hit);
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

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit);

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

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit);

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

    app.on_mouse(MouseEventKind::ScrollDown, 31, 1, &hit);

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

    app.on_mouse(MouseEventKind::ScrollUp, 40, 5, &hit);

    let pty = f.spawner.spawn_named("one").expect("one spawned").pty;
    assert_eq!(pty.written_utf8(), "", "a shell gets no mouse bytes");
    assert_eq!(scrollback_of(&app), 3);
}
