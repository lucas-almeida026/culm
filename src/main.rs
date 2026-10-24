//! Binary entry point. Wires the real implementations and runs the loop.
//! Keep this file thin. Logic belongs in the library, where tests can reach it.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use culm::app::Deps;
use culm::browser::{Browser, BrowserHit};
use culm::cli::{Cli, OpenPlan, Outcome};
use culm::clock::SystemClock;
use culm::git::SystemGit;
use culm::namer::ClaudeNamer;
use culm::pty::SystemPtySpawner;
use culm::recap::ClaudeRecapper;
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

    match cli::run(
        parsed,
        &store,
        &SystemGit,
        &ClaudeNamer,
        &hooks::SocketHookListener,
        &exe,
        &cwd,
    )? {
        Outcome::Done => Ok(()),
        Outcome::Open(plan) => open(*plan, &store),
    }
}

fn open(plan: OpenPlan, store: &FsStore) -> Result<()> {
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

    let result = run(&mut terminal, plan, store, installed, kitty_keys);

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
    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    plan: OpenPlan,
    store: &FsStore,
    hooks_installed: bool,
    keyboard_enhanced: bool,
) -> Result<()> {
    let spawner = SystemPtySpawner;
    let git = SystemGit;
    let probe = ProcMemoryProbe;
    let clock = SystemClock;
    let recapper = ClaudeRecapper;
    let listener = hooks::SocketHookListener;
    let mic = culm::mic::PactlMicrophones;
    let deps = Deps {
        spawner: &spawner,
        git: &git,
        store,
        clock: &clock,
        recapper: &recapper,
        hooks: &listener,
        mic: &mic,
    };

    // The first sessions start before the first draw, so every panel is sized from
    // the terminal rather than from a frame.
    let area = terminal.get_frame().area();
    let config = store.load_config().unwrap_or_default();
    let mut browser = Browser::open(plan.projects, plan.workspace, area, config, &deps)?;
    browser.set_hooks_installed(hooks_installed);
    browser.set_keyboard_enhanced(keyboard_enhanced);

    let mut hit = BrowserHit::default();
    let mut last_draw = Instant::now();
    let mut last_tick = Instant::now();
    let mut fps_since = Instant::now();
    let mut frames = 0u32;

    while !browser.quit_requested() {
        // Input first, so a keystroke never waits behind a frame.
        if event::poll(Duration::from_millis(4))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => browser.on_key(&k, &deps)?,
                Event::Paste(text) => browser.on_paste(&text, &deps)?,
                Event::Mouse(m) => {
                    browser.on_mouse(m.kind, m.column, m.row, m.modifiers, &hit, &deps);
                    // Only the loop owns the output stream, so the copy is written
                    // here. One escape, and no cursor movement, so the drawn buffer
                    // stays valid.
                    if let Some(text) = browser.take_copy() {
                        let backend = terminal.backend_mut();
                        backend.write_all(&clipboard::osc52(&text))?;
                        backend.flush()?;
                    }
                }
                _ => {}
            }
        }

        // Hooks, recaps, scroll measurement, and closes all move a pass at a time,
        // so the screen keeps drawing while children leave.
        browser.poll(&deps)?;

        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            browser.on_tick(&probe, &deps)?;
        }

        if last_draw.elapsed() < FRAME_BUDGET {
            continue;
        }
        last_draw = Instant::now();
        let mut area = Rect::default();
        terminal.draw(|f| {
            area = f.area();
            hit = ui::draw_browser(f, &browser);
        })?;

        frames += 1;
        let elapsed = fps_since.elapsed();
        if elapsed >= Duration::from_secs(1) {
            browser.set_fps(f64::from(frames) / elapsed.as_secs_f64());
            frames = 0;
            fps_since = Instant::now();
        }

        browser.resize(area)?;
    }
    Ok(())
}
