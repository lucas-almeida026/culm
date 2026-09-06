//! Binary entry point. Wires the real implementations and runs the loop.
//! Keep this file thin. Logic belongs in the library, where tests can reach it.

use std::time::{Duration, Instant};

use anyhow::Result;
use culm::app::App;
use culm::pty::{SessionSpec, SystemPtySpawner};
use culm::ui;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::{Terminal, backend::CrosstermBackend};

/// Frame ceiling. Output from a busy session must not drive redraws.
const FRAME_BUDGET: Duration = Duration::from_millis(8);

fn main() -> Result<()> {
    let cwd = std::env::current_dir()?;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    // The kitty protocol reports a modified key as one event, which removes the
    // ESC prefix race described in FINDINGS.md.
    let kitty_keys = supports_keyboard_enhancement().unwrap_or(false);
    if kitty_keys {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let mut app = App::new();
    let spawner = SystemPtySpawner;
    app.add_session(&spawner, &SessionSpec::new("session-1", "claude", &cwd))?;

    let mut hit = ui::HitBox {
        sidebar_width: ui::SIDEBAR_WIDTH,
        first_session_row: 2,
        panel: terminal.get_frame().area(),
    };
    let mut last_draw = Instant::now();

    while !app.quit_requested() {
        // Input first, so a keystroke never waits behind a frame.
        if event::poll(Duration::from_millis(4))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(&k)?,
                Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                    app.on_click(m.column, m.row, hit.sidebar_width, hit.first_session_row);
                }
                _ => {}
            }
        }

        if last_draw.elapsed() < FRAME_BUDGET {
            continue;
        }
        last_draw = Instant::now();
        terminal.draw(|f| hit = ui::draw(f, &app))?;

        let (rows, cols) = ui::panel_size(hit.panel);
        app.resize_all(rows, cols)?;
    }

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
    Ok(())
}
