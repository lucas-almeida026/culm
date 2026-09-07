//! Importing native Claude Code sessions. These use the fakes, so they start no
//! process and touch no file system.
//!
//! Test code may use `expect` with a message. Library code may not.
#![allow(clippy::expect_used)]

use std::path::PathBuf;

use clap::Parser;
use culm::cli::{Cli, Outcome, import_sessions};
use culm::project::{Project, SessionState};
use culm::store::Store;
use culm::testing::{FakeGit, FakeNamer, FakeSpawner, MemoryStore};

const ROOT: &str = "/home/x/spm";
const ROOT_DIR: &str = "-home-x-spm";
const WORKTREE_DIR: &str = "-home-x-spm--worktrees-api-mate-feat-a";

fn project() -> Project {
    Project::new("spm", ROOT)
}

/// One transcript line holding a first prompt, from the working directory `cwd`.
fn prompt_line(cwd: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","isSidechain":false,"cwd":"{cwd}","message":{{"role":"user","content":"{text}"}}}}"#
    )
}

fn title_line(title: &str) -> String {
    format!(r#"{{"type":"custom-title","customTitle":"{title}"}}"#)
}

struct Fakes {
    store: MemoryStore,
    namer: FakeNamer,
    git: FakeGit,
}

impl Fakes {
    fn new() -> Self {
        Self {
            store: MemoryStore::new(),
            namer: FakeNamer::default(),
            git: FakeGit::default(),
        }
    }

    /// Registers the project and saves it, so the command line can resolve it.
    fn register(&self) -> Project {
        let mut registry = culm::project::Registry::default();
        registry.add(ROOT);
        self.store.save_registry(&registry).expect("registry saves");
        let project = project();
        self.store.put_project(&project);
        project
    }
}

#[test]
fn a_titled_transcript_keeps_its_uuid_and_takes_the_slugified_title() {
    let f = Fakes::new();
    let mut project = project();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[
            prompt_line(ROOT, "fix the login bug"),
            title_line("CLUB-1020 fix login"),
        ]
        .join("\n"),
    );

    let report = import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    assert_eq!(report.imported, 1);
    assert_eq!(report.named, 0, "a titled transcript needs no namer");
    let session = &project.sessions[0];
    assert_eq!(session.id, "aaaaaaaa-1111-2222-3333-444444444444");
    assert_eq!(session.slug, "club-1020-fix-login");
    assert_eq!(session.state, SessionState::Paused);
    assert_eq!(session.cwd, PathBuf::from(ROOT));
    assert_eq!(session.last_active, 900);
    assert!(f.namer.asked().is_empty());
}

#[test]
fn an_untitled_transcript_is_named_by_the_namer() {
    let f = Fakes::new();
    f.namer.answer("login bug", "fix the login bug");
    let mut project = project();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &prompt_line(ROOT, "the login bug is back"),
    );

    let report = import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    assert_eq!(report.named, 1);
    assert_eq!(project.sessions[0].slug, "fix-the-login-bug");
    assert_eq!(f.namer.asked(), vec!["the login bug is back".to_string()]);
}

#[test]
fn a_namer_failure_falls_back_to_the_uuid() {
    let f = Fakes::new();
    f.namer.fail_from_now_on();
    let mut project = project();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &prompt_line(ROOT, "the login bug is back"),
    );

    let report = import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    assert_eq!(report.imported, 1, "the session still imports");
    assert_eq!(project.sessions[0].slug, "session-aaaaaaaa");
    assert_eq!(report.skipped.len(), 1, "the fallback is reported");
}

#[test]
fn a_transcript_with_nothing_to_name_it_by_is_skipped() {
    let f = Fakes::new();
    let mut project = project();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        r#"{"type":"mode","mode":"normal"}"#,
    );

    let report = import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    assert_eq!(report.imported, 0);
    assert_eq!(report.skipped.len(), 1);
    assert!(project.sessions.is_empty());
}

#[test]
fn a_transcript_under_a_worktree_is_imported_with_its_own_working_directory() {
    let f = Fakes::new();
    let worktree = "/home/x/spm/.worktrees/api-mate-feat-a";
    let mut project = project();
    f.store.put_transcript(
        WORKTREE_DIR,
        "bbbbbbbb-1111-2222-3333-444444444444",
        900,
        &[
            prompt_line(worktree, "work on the api"),
            title_line("api work"),
        ]
        .join("\n"),
    );

    import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    assert_eq!(project.sessions[0].cwd, PathBuf::from(worktree));
}

#[test]
fn a_sibling_directory_with_a_longer_name_is_left_alone() {
    let f = Fakes::new();
    let mut project = project();
    f.store.put_transcript(
        "-home-x-spm2",
        "cccccccc-1111-2222-3333-444444444444",
        900,
        &[
            prompt_line("/home/x/spm2", "another project"),
            title_line("other"),
        ]
        .join("\n"),
    );

    let report = import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    assert_eq!(report.imported, 0, "spm2 is a different project");
    assert!(project.sessions.is_empty());
}

#[test]
fn a_second_import_adds_nothing() {
    let f = Fakes::new();
    let mut project = project();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[prompt_line(ROOT, "fix it"), title_line("fix it")].join("\n"),
    );
    import_sessions(&f.store, &f.namer, &mut project).expect("first import");

    let report = import_sessions(&f.store, &f.namer, &mut project).expect("second import");

    assert_eq!(report.imported, 0);
    assert_eq!(project.sessions.len(), 1);
}

#[test]
fn a_later_import_adds_only_the_transcripts_that_appeared_since() {
    let f = Fakes::new();
    let mut project = project();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[prompt_line(ROOT, "the first one"), title_line("first")].join("\n"),
    );
    import_sessions(&f.store, &f.namer, &mut project).expect("first import");
    project.sessions[0].name = "renamed by hand".into();

    f.store.put_transcript(
        ROOT_DIR,
        "bbbbbbbb-1111-2222-3333-444444444444",
        1_000,
        &[prompt_line(ROOT, "the second one"), title_line("second")].join("\n"),
    );
    let report = import_sessions(&f.store, &f.namer, &mut project).expect("second import");

    assert_eq!(report.imported, 1);
    assert_eq!(project.sessions.len(), 2);
    assert_eq!(
        project.sessions[0].name, "renamed by hand",
        "an existing record is never rewritten"
    );
    assert_eq!(project.sessions[1].slug, "second");
}

#[test]
fn an_active_session_is_never_imported_twice() {
    let f = Fakes::new();
    let mut project = project();
    project.sessions.push(culm::project::SessionRecord {
        id: "aaaaaaaa-1111-2222-3333-444444444444".into(),
        name: "already here".into(),
        slug: "already-here".into(),
        state: SessionState::Active,
        cwd: PathBuf::from(ROOT),
        repos: Vec::new(),
        started: true,
        last_active: 5,
    });
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[prompt_line(ROOT, "fix it"), title_line("fix it")].join("\n"),
    );

    let report = import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    assert_eq!(report.imported, 0);
    assert_eq!(project.sessions.len(), 1);
    assert_eq!(project.sessions[0].state, SessionState::Active);
}

#[test]
fn two_transcripts_with_one_title_keep_distinct_slugs() {
    let f = Fakes::new();
    let mut project = project();
    for id in [
        "aaaaaaaa-1111-2222-3333-444444444444",
        "bbbbbbbb-1111-2222-3333-444444444444",
    ] {
        f.store.put_transcript(
            ROOT_DIR,
            id,
            900,
            &[prompt_line(ROOT, "fix it"), title_line("fix it")].join("\n"),
        );
    }

    import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    let slugs: Vec<&str> = project.sessions.iter().map(|s| s.slug.as_str()).collect();
    assert_eq!(slugs, vec!["fix-it", "fix-it-2"]);
}

#[test]
fn the_import_reaches_the_store() {
    let f = Fakes::new();
    let mut project = project();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[prompt_line(ROOT, "fix it"), title_line("fix it")].join("\n"),
    );

    import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    let saved = f.store.project("spm").expect("the project is saved");
    assert_eq!(saved.sessions.len(), 1);
}

#[test]
fn project_new_with_the_flag_registers_and_imports() {
    let f = Fakes::new();
    let dir = std::env::temp_dir().join(format!("culm-import-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let root = std::fs::canonicalize(&dir).expect("canonical");
    let encoded = culm::project::transcript_dir_name(&root);
    f.store.put_transcript(
        &encoded,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[
            prompt_line(&root.display().to_string(), "fix it"),
            title_line("fix it"),
        ]
        .join("\n"),
    );

    culm::cli::run(
        Cli::parse_from([
            "culm",
            "project",
            "new",
            &root.display().to_string(),
            "--import-native-sessions",
        ]),
        &f.store,
        &f.git,
        &f.namer,
        std::path::Path::new("/usr/bin/culm"),
        std::path::Path::new("/tmp"),
    )
    .expect("project new succeeds");

    let slug = f.store.load_registry().expect("registry").projects[0]
        .slug
        .clone();
    let saved = f.store.project(&slug).expect("the project is saved");
    assert_eq!(saved.sessions.len(), 1);
    assert_eq!(saved.sessions[0].slug, "fix-it");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn open_with_the_flag_imports_before_the_interface_starts() {
    let f = Fakes::new();
    f.register();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[prompt_line(ROOT, "fix it"), title_line("fix it")].join("\n"),
    );

    let outcome = culm::cli::run(
        Cli::parse_from(["culm", "open", "spm", "--import-native-sessions"]),
        &f.store,
        &f.git,
        &f.namer,
        std::path::Path::new("/usr/bin/culm"),
        std::path::Path::new("/tmp"),
    )
    .expect("open succeeds");

    match outcome {
        Outcome::Open(project) => assert_eq!(project.sessions.len(), 1),
        Outcome::Done => panic!("open must hand back a project"),
    }
}

#[test]
fn project_import_works_on_an_existing_project() {
    let f = Fakes::new();
    f.register();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[prompt_line(ROOT, "fix it"), title_line("fix it")].join("\n"),
    );

    culm::cli::run(
        Cli::parse_from(["culm", "project", "import", "--project", "spm"]),
        &f.store,
        &f.git,
        &f.namer,
        std::path::Path::new("/usr/bin/culm"),
        std::path::Path::new("/tmp"),
    )
    .expect("import succeeds");

    let saved = f.store.project("spm").expect("the project is saved");
    assert_eq!(saved.sessions.len(), 1);
}

#[test]
fn an_imported_session_resumes_its_own_transcript() {
    let f = Fakes::new();
    let mut project = project();
    f.store.put_transcript(
        ROOT_DIR,
        "aaaaaaaa-1111-2222-3333-444444444444",
        900,
        &[prompt_line(ROOT, "fix it"), title_line("fix it")].join("\n"),
    );
    import_sessions(&f.store, &f.namer, &mut project).expect("import runs");

    let spawner = FakeSpawner::new(Vec::new());
    let clock = culm::testing::FakeClock::new(1_000);
    let deps = culm::app::Deps {
        spawner: &spawner,
        git: &f.git,
        store: &f.store,
        clock: &clock,
    };
    let mut app = culm::app::App::open(project, &deps, 20, 50);
    app.set_focus(0);
    app.toggle_pause(&deps).expect("resume");

    let spec = spawner
        .spawn_named("fix-it")
        .expect("the session started")
        .spec;
    assert!(
        spec.has_flag("--resume", "aaaaaaaa-1111-2222-3333-444444444444"),
        "the imported id is the one resumed"
    );
}
