//! The window: tabs, the keys that concern them, workspaces, and the quit. These use
//! the fakes, so they start no process and touch no file system.
//!
//! Test code may use `expect` with a message. Library code may not.
#![allow(clippy::expect_used)]

use clap::Parser;
use culm::app::{Deps, Field, NewSession};
use culm::browser::{Browser, MAX_TABS};
use culm::browser::{BrowserHit, BrowserModal, SaveChoice, WorkspaceOrigin, needs_save_prompt};
use culm::hooks::{Attention, HookEvent};
use culm::project::Project;
use culm::store::Store;
use culm::testing::{FakeClock, FakeGit, FakeHookListener, FakeRecapper, FakeSpawner, MemoryStore};
use culm::workspace::Workspaces;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

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
        Self::with_output(&[])
    }

    /// A store that knows the projects `a` to `i`, each rooted in its own directory,
    /// and children that print `output`.
    fn with_output(output: &[u8]) -> Self {
        let f = Self {
            spawner: FakeSpawner::new(output.to_vec()),
            git: FakeGit::default(),
            store: MemoryStore::new(),
            clock: FakeClock::new(1_000),
            recapper: FakeRecapper::new(),
            hooks: FakeHookListener::new(),
            mic: culm::testing::FakeMicrophones::default(),
        };
        let mut registry = f.store.load_registry().expect("load");
        for slug in ["a", "b", "c", "d", "e", "f", "g", "h", "i"] {
            let root = format!("/home/x/{slug}");
            registry.add_as(&root, slug).expect("the slug is free");
            f.store.put_project(&Project::new(slug, &root));
        }
        f.store.save_registry(&registry).expect("registry saves");
        f
    }

    fn deps(&self) -> Deps<'_> {
        Deps {
            spawner: &self.spawner,
            git: &self.git,
            store: &self.store,
            clock: &self.clock,
            recapper: &self.recapper,
            hooks: &self.hooks,
            mic: &self.mic,
        }
    }

    fn project(&self, slug: &str) -> Project {
        self.store.project(slug).expect("the project is registered")
    }

    fn open(&self, slugs: &[&str]) -> Browser {
        self.open_from(slugs, None)
    }

    fn open_from(&self, slugs: &[&str], origin: Option<WorkspaceOrigin>) -> Browser {
        let projects = slugs.iter().map(|s| self.project(s)).collect();
        Browser::open(
            projects,
            origin,
            Rect::new(0, 0, 120, 30),
            culm::config::Config::default(),
            &self.deps(),
        )
        .expect("the window opens")
    }
}

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

fn alt(c: char) -> KeyEvent {
    key(KeyCode::Char(c), KeyModifiers::ALT)
}

fn alt_shift(c: char) -> KeyEvent {
    key(KeyCode::Char(c), KeyModifiers::ALT | KeyModifiers::SHIFT)
}

fn ctrl_q() -> KeyEvent {
    key(KeyCode::Char('q'), KeyModifiers::CONTROL)
}

fn press(b: &mut Browser, f: &Fakes, k: KeyEvent) {
    b.on_key(&k, &f.deps()).expect("the key is handled");
}

fn enter() -> KeyEvent {
    key(KeyCode::Enter, KeyModifiers::NONE)
}

fn type_text(b: &mut Browser, f: &Fakes, text: &str) {
    for c in text.chars() {
        press(b, f, key(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

fn focused_slug(b: &Browser) -> String {
    b.app().project().slug.clone()
}

fn new_session(b: &mut Browser, f: &Fakes, name: &str) {
    b.app_mut()
        .create_session(
            &NewSession {
                name: name.into(),
                chosen: Vec::new(),
                field: Field::Name,
            },
            &f.deps(),
        )
        .expect("the session starts");
}

/// Runs the loop until the window has closed, as the event loop would.
fn finish_quit(b: &mut Browser, f: &Fakes) {
    for _ in 0..1_000 {
        if b.quit_requested() {
            return;
        }
        b.poll(&f.deps()).expect("a pass of the loop");
    }
    panic!("the window never closed");
}

fn workspace(name: &str, projects: &[&str]) -> WorkspaceOrigin {
    WorkspaceOrigin {
        name: name.into(),
        projects: projects.iter().map(|p| (*p).to_string()).collect(),
    }
}

// Tabs

#[test]
fn every_project_opens_as_a_tab_and_the_first_is_focused() {
    let f = Fakes::new();
    let b = f.open(&["a", "b", "c"]);

    assert_eq!(b.open_projects(), ["a", "b", "c"]);
    assert_eq!(focused_slug(&b), "a");
}

#[test]
fn alt_and_a_digit_focus_a_tab() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b", "c"]);

    press(&mut b, &f, alt('3'));
    assert_eq!(focused_slug(&b), "c");
    press(&mut b, &f, alt('1'));
    assert_eq!(focused_slug(&b), "a");
    press(&mut b, &f, alt('7'));
    assert_eq!(
        focused_slug(&b),
        "a",
        "a digit past the last tab changes nothing"
    );
}

#[test]
fn alt_nine_reaches_the_focused_child() {
    let f = Fakes::new();
    let mut b = f.open(&["a"]);
    new_session(&mut b, &f, "one");

    press(&mut b, &f, alt('9'));

    let pty = f.spawner.spawn_named("one").expect("one exists").pty;
    assert_eq!(pty.written_utf8(), "\x1b9");
}

#[test]
fn a_click_on_a_tab_focuses_it() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    let hit = BrowserHit {
        tabs: vec![(Rect::new(0, 0, 5, 1), 0), (Rect::new(5, 0, 5, 1), 1)],
        ..BrowserHit::default()
    };

    b.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        7,
        0,
        KeyModifiers::NONE,
        &hit,
        &f.deps(),
    );

    assert_eq!(focused_slug(&b), "b");
}

#[test]
fn keys_reach_only_the_focused_tabs_visible_session() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    new_session(&mut b, &f, "in-a");
    press(&mut b, &f, alt('2'));
    new_session(&mut b, &f, "in-b");

    type_text(&mut b, &f, "x");

    let a = f.spawner.spawn_named("in-a").expect("in-a exists").pty;
    let in_b = f.spawner.spawn_named("in-b").expect("in-b exists").pty;
    assert_eq!(a.written_utf8(), "");
    assert_eq!(in_b.written_utf8(), "x");
}

#[test]
fn each_tab_keeps_its_own_shell() {
    let f = Fakes::new();
    let _b = f.open(&["a", "b"]);

    let shells: Vec<_> = f
        .spawner
        .specs()
        .into_iter()
        .filter(|s| s.name == "shell")
        .map(|s| s.cwd)
        .collect();
    assert_eq!(
        shells,
        [
            std::path::PathBuf::from("/home/x/a"),
            std::path::PathBuf::from("/home/x/b")
        ]
    );
}

#[test]
fn every_tab_is_sized_below_the_tab_bar_even_when_not_on_screen() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    press(&mut b, &f, alt('2'));
    new_session(&mut b, &f, "in-b");
    press(&mut b, &f, alt('1'));

    b.resize(Rect::new(0, 0, 100, 40)).expect("resize");

    let pty = f.spawner.spawn_named("in-b").expect("in-b exists").pty;
    // One row of tabs, one of bottom bar, and two of panel border.
    assert_eq!(pty.size().0, 36);
}

// Markers

#[test]
fn the_tab_shows_the_most_urgent_marker_of_its_sessions() {
    let f = Fakes::new();
    let mut b = f.open(&["a"]);
    new_session(&mut b, &f, "one");
    new_session(&mut b, &f, "two");
    let ids: Vec<String> = b
        .app()
        .entries()
        .iter()
        .map(|e| e.record.id.clone())
        .collect();
    assert_eq!(
        b.app().attention(),
        Attention::None,
        "no marker, only padding"
    );

    f.hooks.send(
        "a",
        HookEvent {
            session_id: ids[0].clone(),
            hook_event_name: "Stop".into(),
            ..HookEvent::default()
        },
    );
    b.poll(&f.deps()).expect("a pass");
    assert_eq!(b.app().attention(), Attention::Done);

    f.hooks.send(
        "a",
        HookEvent {
            session_id: ids[1].clone(),
            hook_event_name: "PermissionRequest".into(),
            ..HookEvent::default()
        },
    );
    b.poll(&f.deps()).expect("a pass");
    assert_eq!(
        b.app().attention(),
        Attention::NeedsPermission,
        "a request outranks a finished turn"
    );
}

#[test]
fn a_hook_event_marks_only_the_tab_it_belongs_to() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    press(&mut b, &f, alt('2'));
    new_session(&mut b, &f, "in-b");
    let id = b.app().entries()[0].record.id.clone();
    press(&mut b, &f, alt('1'));

    f.hooks.send(
        "b",
        HookEvent {
            session_id: id,
            hook_event_name: "PermissionRequest".into(),
            ..HookEvent::default()
        },
    );
    b.poll(&f.deps()).expect("a pass");

    assert_eq!(b.tabs()[0].app().attention(), Attention::None);
    assert_eq!(b.tabs()[1].app().attention(), Attention::NeedsPermission);
}

#[test]
fn a_project_open_in_another_culm_is_refused_and_the_rest_open() {
    let f = Fakes::new();
    f.hooks.open_elsewhere("b");

    let b = f.open(&["a", "b"]);

    assert_eq!(b.open_projects(), ["a"]);
    assert!(
        b.app().status().contains("another culm"),
        "{}",
        b.app().status()
    );
}

#[test]
fn a_window_whose_every_project_is_open_elsewhere_does_not_open() {
    let f = Fakes::new();
    f.hooks.open_elsewhere("a");

    let opened = Browser::open(
        vec![f.project("a")],
        None,
        Rect::new(0, 0, 120, 30),
        culm::config::Config::default(),
        &f.deps(),
    );

    assert!(opened.is_err());
}

// Closing a tab

#[test]
fn closing_a_tab_stops_its_sessions_and_keeps_the_others_running() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    new_session(&mut b, &f, "in-a");
    press(&mut b, &f, alt('2'));
    new_session(&mut b, &f, "in-b");

    press(&mut b, &f, alt_shift('W'));

    assert_eq!(b.open_projects(), ["a"]);
    assert_eq!(focused_slug(&b), "a");
    let in_b = f.spawner.spawn_named("in-b").expect("in-b exists");
    assert!(
        in_b.child.terminated(),
        "the closed tab's session was asked to leave"
    );
    let in_a = f.spawner.spawn_named("in-a").expect("in-a exists");
    assert!(!in_a.child.terminated(), "the other tab keeps running");
    assert_eq!(f.hooks.released(), ["b"]);
    b.poll(&f.deps()).expect("a pass");
    assert!(!b.quit_requested());
}

#[test]
fn closing_the_last_tab_quits() {
    let f = Fakes::new();
    let mut b = f.open(&["a"]);

    press(&mut b, &f, alt_shift('W'));
    finish_quit(&mut b, &f);

    assert!(b.quit_requested());
}

// The picker

#[test]
fn the_picker_lists_only_projects_not_already_open() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "c"]);

    press(&mut b, &f, alt_shift('T'));

    let Some(BrowserModal::Picker(picker)) = b.modal() else {
        panic!("the picker is open");
    };
    let listed: Vec<&str> = picker.matches().iter().map(|p| p.slug.as_str()).collect();
    assert_eq!(listed, ["b", "d", "e", "f", "g", "h", "i"]);
}

#[test]
fn a_picked_project_opens_as_the_last_tab_and_takes_the_focus() {
    let f = Fakes::new();
    let mut b = f.open(&["a"]);

    press(&mut b, &f, alt_shift('T'));
    type_text(&mut b, &f, "d");
    press(&mut b, &f, enter());

    assert_eq!(b.open_projects(), ["a", "d"]);
    assert_eq!(focused_slug(&b), "d");
    assert!(b.modal().is_none());
}

#[test]
fn a_ninth_tab_is_refused() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b", "c", "d", "e", "f", "g", "h"]);
    assert_eq!(b.tabs().len(), MAX_TABS);

    press(&mut b, &f, alt_shift('T'));

    assert!(b.modal().is_none(), "no picker opens");
    assert!(b.app().status().contains("limit"), "{}", b.app().status());
}

// Quitting and the save prompt

#[test]
fn needs_save_prompt_counts_an_opened_a_closed_and_a_moved_tab() {
    let open = |p: &[&str]| p.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert!(!needs_save_prompt(None, &open(&["a"])));
    assert!(needs_save_prompt(None, &open(&["a", "b"])));
    let origin = workspace("w", &["a", "b"]);
    assert!(!needs_save_prompt(Some(&origin), &open(&["a", "b"])));
    assert!(
        needs_save_prompt(Some(&origin), &open(&["b", "a"])),
        "a move"
    );
    assert!(needs_save_prompt(Some(&origin), &open(&["a"])), "a close");
    assert!(
        needs_save_prompt(Some(&origin), &open(&["a", "b", "c"])),
        "an open"
    );
}

#[test]
fn a_single_project_quits_without_a_prompt() {
    let f = Fakes::new();
    let mut b = f.open(&["a"]);

    press(&mut b, &f, ctrl_q());

    assert!(b.quitting());
    finish_quit(&mut b, &f);
}

#[test]
fn two_projects_outside_a_workspace_ask_before_quitting() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);

    press(&mut b, &f, ctrl_q());

    let Some(BrowserModal::SavePrompt(prompt)) = b.modal() else {
        panic!("the save prompt is open");
    };
    assert_eq!(
        prompt.choices,
        [SaveChoice::SaveAsNew, SaveChoice::DontSave]
    );
    assert!(!b.quitting(), "nothing closes before the answer");
}

#[test]
fn an_unchanged_workspace_quits_without_a_prompt() {
    let f = Fakes::new();
    let mut b = f.open_from(&["a", "b"], Some(workspace("w", &["a", "b"])));

    press(&mut b, &f, ctrl_q());

    assert!(b.quitting());
}

#[test]
fn a_workspace_that_gained_a_tab_offers_an_update() {
    let f = Fakes::new();
    let mut all = Workspaces::default();
    all.create("w", &["a".into(), "b".into()]).expect("created");
    f.store.put_workspaces(&all);
    let mut b = f.open_from(&["a", "b"], Some(workspace("w", &["a", "b"])));
    press(&mut b, &f, alt_shift('T'));
    type_text(&mut b, &f, "c");
    press(&mut b, &f, enter());

    press(&mut b, &f, ctrl_q());
    let Some(BrowserModal::SavePrompt(prompt)) = b.modal() else {
        panic!("the save prompt is open");
    };
    assert_eq!(prompt.choices[0], SaveChoice::Update);
    press(&mut b, &f, enter());
    finish_quit(&mut b, &f);

    let saved = f.store.workspaces();
    assert_eq!(
        saved.find("w").map(|w| w.projects.clone()),
        Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
    );
}

#[test]
fn a_workspace_that_lost_a_tab_asks_too() {
    let f = Fakes::new();
    let mut b = f.open_from(&["a", "b"], Some(workspace("w", &["a", "b"])));
    press(&mut b, &f, alt_shift('W'));

    press(&mut b, &f, ctrl_q());

    assert!(matches!(b.modal(), Some(BrowserModal::SavePrompt(_))));
}

#[test]
fn save_as_new_writes_the_workspace_and_then_quits() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    press(&mut b, &f, ctrl_q());

    press(&mut b, &f, enter());
    type_text(&mut b, &f, "Billing Work");
    press(&mut b, &f, enter());
    finish_quit(&mut b, &f);

    let saved = f.store.workspaces();
    assert_eq!(
        saved.find("billing-work").map(|w| w.projects.clone()),
        Some(vec!["a".to_string(), "b".to_string()])
    );
}

#[test]
fn a_taken_or_empty_workspace_name_is_refused_and_the_prompt_stays() {
    let f = Fakes::new();
    let mut all = Workspaces::default();
    all.create("taken", &[]).expect("created");
    f.store.put_workspaces(&all);
    let mut b = f.open(&["a", "b"]);
    press(&mut b, &f, ctrl_q());
    press(&mut b, &f, enter());

    press(&mut b, &f, enter());
    let Some(BrowserModal::SavePrompt(prompt)) = b.modal() else {
        panic!("the prompt stays open");
    };
    assert!(prompt.error.is_some(), "an empty name is refused");

    type_text(&mut b, &f, "taken");
    press(&mut b, &f, enter());
    let Some(BrowserModal::SavePrompt(prompt)) = b.modal() else {
        panic!("the prompt stays open");
    };
    assert!(
        prompt
            .error
            .as_deref()
            .is_some_and(|e| e.contains("already exists")),
        "{:?}",
        prompt.error
    );
    assert!(!b.quitting());
}

#[test]
fn dont_save_writes_no_workspace_and_still_saves_every_project() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    new_session(&mut b, &f, "kept");
    press(&mut b, &f, ctrl_q());

    press(&mut b, &f, key(KeyCode::Down, KeyModifiers::NONE));
    press(&mut b, &f, enter());
    finish_quit(&mut b, &f);

    assert!(f.store.workspaces().workspaces.is_empty());
    let a = f.store.project("a").expect("a was saved");
    assert_eq!(a.sessions.len(), 1, "the project kept its session");
    assert!(a.sessions[0].is_active(), "and remembers it was active");
}

#[test]
fn escape_cancels_the_quit_and_stops_nothing() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    new_session(&mut b, &f, "one");
    press(&mut b, &f, ctrl_q());

    press(&mut b, &f, key(KeyCode::Esc, KeyModifiers::NONE));

    assert!(b.modal().is_none());
    assert!(!b.quitting());
    let one = f.spawner.spawn_named("one").expect("one exists");
    assert!(!one.child.terminated());
}

#[test]
fn a_quit_pressed_again_on_the_prompt_is_ignored() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    press(&mut b, &f, ctrl_q());

    press(&mut b, &f, ctrl_q());

    assert!(matches!(b.modal(), Some(BrowserModal::SavePrompt(_))));
    assert!(!b.quitting());
}

#[test]
fn a_second_quit_while_closing_cuts_every_tab_short() {
    let f = Fakes::new();
    f.spawner.children_ignore_terminate();
    let mut b = f.open(&["a"]);
    new_session(&mut b, &f, "stubborn");
    press(&mut b, &f, ctrl_q());
    b.poll(&f.deps()).expect("a pass");
    assert!(
        !b.quit_requested(),
        "the stubborn child holds the close open"
    );

    press(&mut b, &f, ctrl_q());
    b.poll(&f.deps()).expect("a pass");

    assert!(b.quit_requested());
    let stubborn = f.spawner.spawn_named("stubborn").expect("stubborn exists");
    assert!(stubborn.child.killed());
}

// What every tab shares

#[test]
fn a_setting_changed_in_one_tab_reaches_every_tab() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);

    press(&mut b, &f, alt_shift('S'));
    press(&mut b, &f, key(KeyCode::Char(' '), KeyModifiers::NONE));
    press(&mut b, &f, key(KeyCode::Esc, KeyModifiers::NONE));

    assert!(b.tabs()[1].app().config().recap_on_pause);
}

#[test]
fn nerd_mode_reaches_every_tab() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);

    press(&mut b, &f, alt_shift('D'));

    assert!(b.tabs()[1].app().nerd_mode());
}

#[test]
fn a_copy_in_one_tab_pastes_in_another() {
    let f = Fakes::with_output(b"hello world");
    let mut b = f.open(&["a", "b"]);
    new_session(&mut b, &f, "one");
    let session = b
        .app()
        .visible()
        .and_then(|e| e.live.as_ref())
        .expect("one runs");
    assert!(session.wait_for_text("hello", std::time::Duration::from_secs(1)));
    let panel = Rect::new(30, 1, 60, 20);
    let hit = BrowserHit {
        tabs: Vec::new(),
        app: culm::ui::HitBox {
            sidebar_width: 30,
            separator_col: 29,
            panel,
            inner: culm::ui::inner_of(panel),
            ..culm::ui::HitBox::default()
        },
    };
    let d = f.deps();
    b.on_mouse(
        MouseEventKind::Down(MouseButton::Left),
        31,
        2,
        KeyModifiers::NONE,
        &hit,
        &d,
    );
    b.on_mouse(
        MouseEventKind::Drag(MouseButton::Left),
        35,
        2,
        KeyModifiers::NONE,
        &hit,
        &d,
    );
    b.on_mouse(
        MouseEventKind::Up(MouseButton::Left),
        35,
        2,
        KeyModifiers::NONE,
        &hit,
        &d,
    );

    assert_eq!(b.take_copy().as_deref(), Some("hello"));
    assert_eq!(
        b.tabs()[1].app().clipboard(),
        "hello",
        "the other tab pastes it"
    );
}

// The command line

fn cli(f: &Fakes, args: &[&str]) -> anyhow::Result<culm::cli::Outcome> {
    culm::cli::run(
        culm::cli::Cli::parse_from(args),
        &f.store,
        &f.git,
        &culm::testing::FakeNamer::default(),
        &f.hooks,
        std::path::Path::new("/usr/bin/culm"),
        std::path::Path::new("/home/x/a"),
    )
}

fn opened(outcome: culm::cli::Outcome) -> (Vec<String>, Option<String>) {
    match outcome {
        culm::cli::Outcome::Open(plan) => (
            plan.projects.iter().map(|p| p.slug.clone()).collect(),
            plan.workspace.map(|w| w.name),
        ),
        culm::cli::Outcome::Done => panic!("open must hand back projects"),
    }
}

#[test]
fn open_with_two_projects_opens_two_tabs() {
    let f = Fakes::new();
    let (slugs, origin) = opened(cli(&f, &["culm", "open", "b", "a"]).expect("open"));
    assert_eq!(slugs, ["b", "a"]);
    assert_eq!(origin, None);
}

#[test]
fn bare_culm_opens_the_project_that_owns_the_directory() {
    let f = Fakes::new();
    let (slugs, _) = opened(cli(&f, &["culm"]).expect("open"));
    assert_eq!(slugs, ["a"]);
}

#[test]
fn a_repeated_project_opens_once() {
    let f = Fakes::new();
    let (slugs, _) = opened(cli(&f, &["culm", "open", "a", "b", "a"]).expect("open"));
    assert_eq!(slugs, ["a", "b"]);
}

#[test]
fn a_ninth_project_on_the_command_line_is_refused() {
    let f = Fakes::new();
    let nine = ["culm", "open", "a", "b", "c", "d", "e", "f", "g", "h", "i"];
    assert!(cli(&f, &nine).is_err());
}

#[test]
fn open_w_opens_the_workspace_projects_in_order() {
    let f = Fakes::new();
    cli(&f, &["culm", "workspace", "new", "Work", "c", "a"]).expect("created");

    let (slugs, origin) = opened(cli(&f, &["culm", "open", "-w", "work"]).expect("open"));

    assert_eq!(slugs, ["c", "a"]);
    assert_eq!(origin.as_deref(), Some("work"));
}

#[test]
fn open_w_skips_a_project_that_was_removed() {
    let f = Fakes::new();
    let mut all = Workspaces::default();
    all.create("w", &["a".into(), "ghost".into()])
        .expect("created");
    f.store.put_workspaces(&all);

    let (slugs, _) = opened(cli(&f, &["culm", "open", "-w", "w"]).expect("open"));

    assert_eq!(slugs, ["a"]);
}

#[test]
fn open_w_with_an_unknown_name_is_refused() {
    let f = Fakes::new();
    assert!(cli(&f, &["culm", "open", "-w", "nope"]).is_err());
}

#[test]
fn a_workspace_refuses_an_unknown_project() {
    let f = Fakes::new();
    assert!(cli(&f, &["culm", "workspace", "new", "w", "ghost"]).is_err());
    assert!(f.store.workspaces().workspaces.is_empty());
}

#[test]
fn the_workspace_commands_add_remove_rename_and_delete() {
    let f = Fakes::new();
    cli(&f, &["culm", "workspace", "new", "w", "a"]).expect("created");
    cli(&f, &["culm", "workspace", "add", "w", "b"]).expect("added");
    cli(&f, &["culm", "workspace", "remove", "w", "a"]).expect("removed");
    cli(&f, &["culm", "workspace", "rename", "w", "renamed"]).expect("renamed");
    assert_eq!(
        f.store
            .workspaces()
            .find("renamed")
            .map(|w| w.projects.clone()),
        Some(vec!["b".to_string()])
    );

    cli(&f, &["culm", "workspace", "rm", "renamed"]).expect("deleted");
    assert!(f.store.workspaces().workspaces.is_empty());
}

#[test]
fn renaming_a_project_renames_it_in_every_workspace() {
    let f = Fakes::new();
    cli(&f, &["culm", "workspace", "new", "w", "a", "b"]).expect("created");

    cli(
        &f,
        &["culm", "project", "alter", "name", "core", "--project", "a"],
    )
    .expect("renamed");

    assert_eq!(
        f.store.workspaces().find("w").map(|w| w.projects.clone()),
        Some(vec!["core".to_string(), "b".to_string()])
    );
}

#[test]
fn a_project_open_in_a_window_is_not_renamed() {
    let f = Fakes::new();
    let b = f.open(&["a"]);

    let err = cli(
        &f,
        &["culm", "project", "alter", "name", "core", "--project", "a"],
    )
    .expect_err("the rename is refused");

    assert!(err.to_string().contains("open in culm"), "{err}");
    assert!(f.store.project("a").is_some(), "the project keeps its name");
    assert!(f.store.project("core").is_none());
    drop(b);
}

// The tab bar, as drawn

fn render(b: &Browser) -> ratatui::Terminal<ratatui::backend::TestBackend> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12))
        .expect("the test backend starts");
    terminal
        .draw(|f| {
            culm::ui::draw_browser(f, b);
        })
        .expect("the window draws");
    terminal
}

fn row_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..80)
        .filter_map(|x| buffer.cell(ratatui::layout::Position { x, y }))
        .map(|cell| cell.symbol().to_string())
        .collect()
}

#[test]
fn the_tab_bar_holds_each_name_with_room_for_its_marker() {
    let f = Fakes::new();
    let b = f.open(&["a", "b"]);

    let terminal = render(&b);

    assert!(
        row_text(&terminal, 0).starts_with("   a    b "),
        "{:?}",
        row_text(&terminal, 0)
    );
}

#[test]
fn the_focused_tab_is_filled_and_the_others_are_not() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    press(&mut b, &f, alt('2'));

    let terminal = render(&b);
    let buffer = terminal.backend().buffer();
    let bg = |x: u16| {
        buffer
            .cell(ratatui::layout::Position { x, y: 0 })
            .map(|c| c.bg)
    };

    // `a` sits at column 3 and `b` at column 8.
    assert_ne!(bg(3), Some(ratatui::style::Color::Yellow));
    assert_eq!(bg(8), Some(ratatui::style::Color::Yellow));
    assert_eq!(bg(0), bg(1), "the marker column is never filled");
}

#[test]
fn the_tab_bar_draws_the_marker_of_a_session_that_needs_the_user() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);
    press(&mut b, &f, alt('2'));
    new_session(&mut b, &f, "asks");
    let id = b.app().entries()[0].record.id.clone();
    press(&mut b, &f, alt('1'));
    f.hooks.send(
        "b",
        HookEvent {
            session_id: id,
            hook_event_name: "PermissionRequest".into(),
            ..HookEvent::default()
        },
    );
    b.poll(&f.deps()).expect("a pass");

    let terminal = render(&b);

    assert!(
        row_text(&terminal, 0).contains("🔐"),
        "{:?}",
        row_text(&terminal, 0)
    );
}

#[test]
fn a_tab_bar_too_narrow_for_every_name_shortens_them() {
    let f = Fakes::new();
    let mut registry = f.store.load_registry().expect("load");
    let long: Vec<String> = (1..=8).map(|n| format!("a-rather-long-name-{n}")).collect();
    for slug in &long {
        let root = format!("/home/x/{slug}");
        registry.add_as(&root, slug).expect("free");
        f.store.put_project(&Project::new(slug, &root));
    }
    f.store.save_registry(&registry).expect("saved");
    let names: Vec<&str> = long.iter().map(String::as_str).collect();
    let b = f.open(&names);

    let terminal = render(&b);

    assert!(
        row_text(&terminal, 0).contains('…'),
        "{:?}",
        row_text(&terminal, 0)
    );
}

// Moving tabs

fn alt_shift_arrow(code: KeyCode) -> KeyEvent {
    key(code, KeyModifiers::ALT | KeyModifiers::SHIFT)
}

/// Three tabs, three columns wide each, starting at column 0.
fn three_tab_hit() -> BrowserHit {
    BrowserHit {
        tabs: vec![
            (Rect::new(0, 0, 3, 1), 0),
            (Rect::new(3, 0, 3, 1), 1),
            (Rect::new(6, 0, 3, 1), 2),
        ],
        ..BrowserHit::default()
    }
}

fn mouse(b: &mut Browser, f: &Fakes, kind: MouseEventKind, column: u16, hit: &BrowserHit) {
    b.on_mouse(kind, column, 0, KeyModifiers::NONE, hit, &f.deps());
}

#[test]
fn alt_shift_right_moves_the_focused_tab_and_the_focus_follows_it() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b", "c"]);

    press(&mut b, &f, alt_shift_arrow(KeyCode::Right));

    assert_eq!(b.open_projects(), ["b", "a", "c"]);
    assert_eq!(focused_slug(&b), "a");
    press(&mut b, &f, alt_shift_arrow(KeyCode::Left));
    assert_eq!(b.open_projects(), ["a", "b", "c"]);
}

#[test]
fn a_tab_at_either_end_goes_no_further() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b"]);

    press(&mut b, &f, alt_shift_arrow(KeyCode::Left));
    assert_eq!(b.open_projects(), ["a", "b"]);
    press(&mut b, &f, alt('2'));
    press(&mut b, &f, alt_shift_arrow(KeyCode::Right));
    assert_eq!(b.open_projects(), ["a", "b"]);
}

#[test]
fn a_moved_tab_is_reached_by_the_digit_of_its_new_place() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b", "c"]);
    press(&mut b, &f, alt_shift_arrow(KeyCode::Right));

    press(&mut b, &f, alt('1'));

    assert_eq!(focused_slug(&b), "b");
}

#[test]
fn dragging_a_tab_carries_it_past_every_tab_it_crosses() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b", "c"]);
    let hit = three_tab_hit();

    mouse(&mut b, &f, MouseEventKind::Down(MouseButton::Left), 1, &hit);
    mouse(&mut b, &f, MouseEventKind::Drag(MouseButton::Left), 7, &hit);
    mouse(&mut b, &f, MouseEventKind::Up(MouseButton::Left), 7, &hit);

    assert_eq!(b.open_projects(), ["b", "c", "a"]);
    assert_eq!(focused_slug(&b), "a");
}

#[test]
fn a_drag_on_a_stale_hit_box_moves_the_tab_once() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b", "c"]);
    let hit = three_tab_hit();

    mouse(&mut b, &f, MouseEventKind::Down(MouseButton::Left), 1, &hit);
    // Two drag events land before the next frame redraws the bar.
    mouse(&mut b, &f, MouseEventKind::Drag(MouseButton::Left), 4, &hit);
    mouse(&mut b, &f, MouseEventKind::Drag(MouseButton::Left), 4, &hit);

    assert_eq!(b.open_projects(), ["b", "a", "c"]);
}

#[test]
fn a_click_on_a_tab_without_a_drag_moves_nothing() {
    let f = Fakes::new();
    let mut b = f.open(&["a", "b", "c"]);
    let hit = three_tab_hit();

    mouse(&mut b, &f, MouseEventKind::Down(MouseButton::Left), 4, &hit);
    mouse(&mut b, &f, MouseEventKind::Up(MouseButton::Left), 4, &hit);

    assert_eq!(b.open_projects(), ["a", "b", "c"]);
    assert_eq!(focused_slug(&b), "b");
}

#[test]
fn a_reordered_workspace_offers_to_save_the_new_order() {
    let f = Fakes::new();
    let mut all = Workspaces::default();
    all.create("w", &["a".into(), "b".into()]).expect("created");
    f.store.put_workspaces(&all);
    let mut b = f.open_from(&["a", "b"], Some(workspace("w", &["a", "b"])));
    press(&mut b, &f, alt_shift_arrow(KeyCode::Right));

    press(&mut b, &f, ctrl_q());
    let Some(BrowserModal::SavePrompt(prompt)) = b.modal() else {
        panic!("a new order asks before the quit");
    };
    assert_eq!(prompt.choices[0], SaveChoice::Update);
    press(&mut b, &f, enter());
    finish_quit(&mut b, &f);

    assert_eq!(
        f.store.workspaces().find("w").map(|w| w.projects.clone()),
        Some(vec!["b".to_string(), "a".to_string()])
    );
}
