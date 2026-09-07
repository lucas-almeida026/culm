//! Binary entry point. Wires the real implementations and runs the loop.
//! Keep this file thin. Logic belongs in the library, where tests can reach it.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use culm::app::{App, Deps, SIDEBAR_DEFAULT};
use culm::cli::{Cli, Outcome};
use culm::clock::SystemClock;
use culm::git::SystemGit;
use culm::namer::ClaudeNamer;
use culm::project::Project;
use culm::pty::SystemPtySpawner;
use culm::stats::ProcMemoryProbe;
use culm::store::{FsStore, Store};
use culm::{cli, clipboard, hooks, ui};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::layout::Rect;
use ratatui::{Terminal, backend::CrosstermBackend};

/// Frame ceiling. Output from a busy session must not drive redraws.
const FRAME_BUDGET: Duration = Duration::from_millis(8);
/// How often memory is sampled and an ended child is reaped.
const TICK: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    let parsed = Cli::parse();
    let store = FsStore::new()?;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("culm"));
    let cwd = std::env::current_dir()?;

    match cli::run(parsed, &store, &SystemGit, &ClaudeNamer, &exe, &cwd)? {
        Outcome::Done => Ok(()),
        Outcome::Open(project) => open(*project, &store),
    }
}

fn open(project: Project, store: &FsStore) -> Result<()> {
    let socket = hooks::socket_path(&project.slug);
    let hook_rx = hooks::listen(&socket).ok();
    let installed = store
        .read_claude_settings()
        .map(|s| hooks::is_installed(&s))
        .unwrap_or(false);

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    // The kitty protocol reports a modified key as one event. It is an enhancement,
    // never a requirement: a terminal that reports no support runs the legacy path.
    let kitty_keys = supports_keyboard_enhancement().unwrap_or(false);
    if kitty_keys {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = run(&mut terminal, project, store, installed, hook_rx.as_ref());

    disable_raw_mode()?;
    if kitty_keys {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    let _ = std::fs::remove_file(&socket);
    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    project: Project,
    store: &FsStore,
    hooks_installed: bool,
    hook_rx: Option<&std::sync::mpsc::Receiver<hooks::HookEvent>>,
) -> Result<()> {
    let spawner = SystemPtySpawner;
    let git = SystemGit;
    let probe = ProcMemoryProbe;
    let clock = SystemClock;
    let deps = Deps {
        spawner: &spawner,
        git: &git,
        store,
        clock: &clock,
    };

    // The first sessions start before the first draw, so the panel size is derived
    // from the terminal rather than from a frame.
    let area = terminal.get_frame().area();
    let panel = Rect {
        x: SIDEBAR_DEFAULT,
        y: 0,
        width: area.width.saturating_sub(SIDEBAR_DEFAULT),
        height: area.height.saturating_sub(1),
    };
    let (rows, cols) = ui::panel_size(panel);

    let mut app = App::open(project, &deps, rows, cols);
    app.set_hooks_installed(hooks_installed);

    let mut hit = ui::HitBox::default();
    let mut last_draw = Instant::now();
    let mut last_tick = Instant::now();
    let mut fps_since = Instant::now();
    let mut frames = 0u32;

    while !app.quit_requested() {
        // Input first, so a keystroke never waits behind a frame.
        if event::poll(Duration::from_millis(4))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(&k, &deps)?,
                Event::Paste(text) => app.on_paste(&text)?,
                Event::Mouse(m) => {
                    app.on_mouse(m.kind, m.column, m.row, &hit);
                    // Only the loop owns the output stream, so the copy is written
                    // here. One escape, and no cursor movement, so the drawn buffer
                    // stays valid.
                    if let Some(text) = app.take_copy() {
                        let backend = terminal.backend_mut();
                        backend.write_all(&clipboard::osc52(&text))?;
                        backend.flush()?;
                    }
                }
                _ => {}
            }
        }

        if let Some(rx) = hook_rx {
            while let Ok(event) = rx.try_recv() {
                app.on_hook(&event);
            }
        }

        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            app.on_tick(&probe, &deps)?;
        }

        if last_draw.elapsed() < FRAME_BUDGET {
            continue;
        }
        last_draw = Instant::now();
        terminal.draw(|f| hit = ui::draw(f, &app))?;

        frames += 1;
        let elapsed = fps_since.elapsed();
        if elapsed >= Duration::from_secs(1) {
            app.set_fps(f64::from(frames) / elapsed.as_secs_f64());
            frames = 0;
            fps_since = Instant::now();
        }

        let (rows, cols) = ui::panel_size(hit.panel);
        app.resize_all(rows, cols)?;
    }
    Ok(())
}
