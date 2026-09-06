//! Integration tests. These use the fake spawner, so they start no process and
//! touch no file system.
//!
//! Test code may use `expect` with a message. Library code may not.
#![allow(clippy::expect_used)]

use std::time::Duration;

use culm::app::App;
use culm::pty::SessionSpec;
use culm::testing::FakeSpawner;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn spec(name: &str) -> SessionSpec {
    SessionSpec::new(name, "fake", "/tmp").with_size(10, 40)
}

#[test]
fn session_output_reaches_the_screen() {
    let spawner = FakeSpawner::new(b"hello from the session".to_vec());
    let mut app = App::new();
    app.add_session(&spawner, &spec("one"))
        .expect("session starts");

    let session = &app.sessions()[0];
    assert!(session.wait_for_text("hello from the session", Duration::from_secs(1)));
}

#[test]
fn keys_reach_the_visible_session_only() {
    let spawner_a = FakeSpawner::new(Vec::new());
    let spawner_b = FakeSpawner::new(Vec::new());
    let mut app = App::new();
    app.add_session(&spawner_a, &spec("one"))
        .expect("session one starts");
    app.add_session(&spawner_b, &spec("two"))
        .expect("session two starts");

    app.on_key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
        .expect("key is forwarded");

    let a = spawner_a.last_pty().expect("pty a exists");
    let b = spawner_b.last_pty().expect("pty b exists");
    assert_eq!(a.written_utf8(), "x");
    assert_eq!(b.written_utf8(), "");
}

#[test]
fn alt_digit_switches_the_visible_session() {
    let spawner = FakeSpawner::new(Vec::new());
    let mut app = App::new();
    app.add_session(&spawner, &spec("one"))
        .expect("session one starts");
    app.add_session(&spawner, &spec("two"))
        .expect("session two starts");

    app.on_key(&KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT))
        .expect("host key is handled");

    assert_eq!(app.focus(), 1);
    assert_eq!(app.visible().map(culm::session::Session::name), Some("two"));
}

#[test]
fn a_click_on_a_sidebar_row_switches_the_visible_session() {
    let spawner = FakeSpawner::new(Vec::new());
    let mut app = App::new();
    app.add_session(&spawner, &spec("one"))
        .expect("session one starts");
    app.add_session(&spawner, &spec("two"))
        .expect("session two starts");

    app.on_click(5, 3, 30, 2);

    assert_eq!(app.focus(), 1);
}

#[test]
fn every_session_is_resized_not_only_the_visible_one() {
    let spawner_a = FakeSpawner::new(Vec::new());
    let spawner_b = FakeSpawner::new(Vec::new());
    let mut app = App::new();
    app.add_session(&spawner_a, &spec("one"))
        .expect("session one starts");
    app.add_session(&spawner_b, &spec("two"))
        .expect("session two starts");

    app.resize_all(20, 60).expect("resize succeeds");

    assert_eq!(spawner_a.last_pty().map(|p| p.size()), Some((20, 60)));
    assert_eq!(spawner_b.last_pty().map(|p| p.size()), Some((20, 60)));
}
