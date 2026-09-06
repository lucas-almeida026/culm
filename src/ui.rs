//! Rendering. One session is visible at a time, in the manner of a browser with
//! vertical tabs. Sessions that are not visible are still parsed, never drawn.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use tui_term::widget::PseudoTerminal;

use crate::app::App;

pub const SIDEBAR_WIDTH: u16 = 30;

/// Where the session list starts, so a click maps back to a session.
#[derive(Debug, Clone, Copy)]
pub struct HitBox {
    pub sidebar_width: u16,
    pub first_session_row: u16,
    pub panel: Rect,
}

pub fn draw(f: &mut Frame, app: &App) -> HitBox {
    let cols = Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(10)])
        .split(f.area());
    let (sidebar, panel) = (cols[0], cols[1]);

    let mut lines = vec![Line::from(Span::styled(
        " sessions",
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    for (i, s) in app.sessions().iter().enumerate() {
        let style = if i == app.focus() {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(
            format!(" {}:{}", i + 1, s.name()),
            style,
        )));
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("culm")),
        sidebar,
    );

    if let Some(s) = app.visible() {
        let guard = match s.parser().lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let title = format!(" {} ", s.name());
        f.render_widget(
            PseudoTerminal::new(guard.screen()).block(Block::bordered().title(title)),
            panel,
        );
    }

    HitBox {
        sidebar_width: sidebar.width,
        first_session_row: sidebar.y + 2,
        panel,
    }
}

/// Panel geometry in rows and columns, once borders are removed.
#[must_use]
pub fn panel_size(panel: Rect) -> (u16, u16) {
    (
        panel.height.saturating_sub(2),
        panel.width.saturating_sub(2),
    )
}
