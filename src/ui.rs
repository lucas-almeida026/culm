//! Rendering, and the hit boxes that map a click back to a session or to the
//! separator. One session is visible at a time, in the manner of a browser with
//! vertical tabs. Sessions that are not visible are still parsed, never drawn.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Entry, Field, Focus};
use crate::hooks::Attention;
use crate::stats::format_bytes;

/// Where the sidebar rows and the drag handle sit, refreshed on every draw.
#[derive(Debug, Clone)]
pub struct HitBox {
    pub sidebar_width: u16,
    /// The column of the sidebar's right border. A drag starts here.
    pub separator_col: u16,
    /// Screen row of each sidebar row, and what it focuses.
    pub rows: Vec<(u16, Focus)>,
    pub panel: Rect,
}

impl Default for HitBox {
    fn default() -> Self {
        Self {
            sidebar_width: crate::app::SIDEBAR_DEFAULT,
            separator_col: crate::app::SIDEBAR_DEFAULT - 1,
            rows: Vec::new(),
            panel: Rect::new(0, 0, 0, 0),
        }
    }
}

fn color_of(attention: Attention) -> Color {
    match attention {
        Attention::None => Color::Gray,
        Attention::Done => Color::Green,
        Attention::NeedsAnswer => Color::Yellow,
        Attention::NeedsPermission => Color::Red,
    }
}

pub fn draw(f: &mut Frame, app: &App) -> HitBox {
    let vertical = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area());
    let (body, status_bar) = (vertical[0], vertical[1]);
    let columns =
        Layout::horizontal([Constraint::Length(app.sidebar_width()), Constraint::Min(10)])
            .split(body);
    let (sidebar, panel) = (columns[0], columns[1]);

    let rows = draw_sidebar(f, app, sidebar);
    draw_panel(f, app, panel);
    draw_status(f, app, status_bar);
    if app.nerd_mode() {
        draw_fps(f, app, panel);
    }
    if let Some(form) = app.modal() {
        draw_form(f, app, form, panel);
    }

    HitBox {
        sidebar_width: sidebar.width,
        separator_col: sidebar.x + sidebar.width.saturating_sub(1),
        rows,
        panel,
    }
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) -> Vec<(u16, Focus)> {
    let inner_width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows: Vec<(u16, Focus)> = Vec::new();

    rows.push((area.y + 1 + lines.len() as u16, Focus::Shell));
    lines.push(shell_line(app, inner_width));
    lines.push(Line::from(""));

    lines.push(header("active"));
    let mut number = 0usize;
    for (index, entry) in app.entries().iter().enumerate() {
        if !entry.is_active() {
            continue;
        }
        number += 1;
        rows.push((area.y + 1 + lines.len() as u16, Focus::Entry(index)));
        lines.push(entry_line(
            entry,
            Some(number),
            app.focus() == Focus::Entry(index),
            inner_width,
        ));
    }
    if number == 0 {
        lines.push(dim("  none"));
    }

    lines.push(Line::from(""));
    lines.push(header("paused"));
    let mut paused = 0usize;
    for (index, entry) in app.entries().iter().enumerate() {
        if entry.is_active() {
            continue;
        }
        paused += 1;
        rows.push((area.y + 1 + lines.len() as u16, Focus::Entry(index)));
        lines.push(entry_line(
            entry,
            None,
            app.focus() == Focus::Entry(index),
            inner_width,
        ));
    }
    if paused == 0 {
        lines.push(dim("  none"));
    }

    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("culm")),
        area,
    );
    rows
}

/// Position 0. It carries no marker, so the prefix stays blank and the names below
/// keep their alignment.
fn shell_line(app: &App, inner_width: usize) -> Line<'static> {
    let focused = app.focus() == Focus::Shell;
    let base = if focused {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::White)
    };
    let rss = if app.shell_rss() > 0 {
        format!("{} ", format_bytes(app.shell_rss()))
    } else {
        String::new()
    };
    let label = "   0 shell";
    let pad = inner_width.saturating_sub(label.chars().count() + rss.chars().count());
    Line::from(vec![
        Span::styled(label.to_string(), base),
        Span::styled(" ".repeat(pad), base),
        Span::styled(rss, base),
    ])
}

fn header(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {text}"),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn dim(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(Color::DarkGray),
    ))
}

/// One session row: the marker, the digit that focuses it, the name, and the memory
/// of its process tree.
fn entry_line(
    entry: &Entry,
    number: Option<usize>,
    focused: bool,
    inner_width: usize,
) -> Line<'static> {
    let base = if focused {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else if entry.is_active() {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let marker = Span::styled(
        entry.attention.emoji().to_string(),
        if focused {
            base
        } else {
            base.fg(color_of(entry.attention))
        },
    );
    let label = match number {
        Some(n) => format!(" {n} "),
        None => "   ".to_string(),
    };
    let rss = if entry.is_active() && entry.rss > 0 {
        format!("{} ", format_bytes(entry.rss))
    } else {
        String::new()
    };

    let fixed = marker.width() + label.len() + rss.len();
    let room = inner_width.saturating_sub(fixed);
    let name = truncate(entry.name(), room);
    let pad = room.saturating_sub(name.chars().count());

    Line::from(vec![
        marker,
        Span::styled(label, base),
        Span::styled(name, base),
        Span::styled(" ".repeat(pad), base),
        Span::styled(rss, base),
    ])
}

fn truncate(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    text.chars()
        .take(room.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
    if app.focus() == Focus::Shell {
        match app.shell() {
            Some(shell) => draw_terminal(f, shell, " shell ".to_string(), area),
            None => f.render_widget(
                Paragraph::new("no shell")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(Block::bordered().title(" shell ")),
                area,
            ),
        }
        return;
    }
    let Some(entry) = app.visible() else {
        let hint = if app.project().repos.is_empty() {
            "no session yet.\n\nAlt+Shift+N creates one.\n\nthis project holds no repository. \
             add one with:\n  culm project repo add <path>"
        } else {
            "no session yet.\n\nAlt+Shift+N creates one."
        };
        f.render_widget(
            Paragraph::new(hint)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::bordered().title(" culm ")),
            area,
        );
        return;
    };

    let title = format!(" {} ", entry.name());
    match entry.live.as_ref() {
        Some(session) => draw_terminal(f, session, title, area),
        None => {
            f.render_widget(
                Paragraph::new("paused\n\nAlt+Shift+P resumes this session.")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(Block::bordered().title(title)),
                area,
            );
        }
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = Vec::new();
    if !app.status().is_empty() {
        spans.push(Span::styled(
            format!(" {}", app.status()),
            Style::default().fg(Color::Yellow),
        ));
    } else {
        spans.push(Span::styled(
            " Alt+0 shell   Alt+<n> focus   Alt+Shift+N new   Alt+Shift+P pause   Ctrl+q quit",
            Style::default().fg(Color::DarkGray),
        ));
    }
    if !app.hooks_installed() {
        spans.push(Span::styled(
            "   markers off, run: culm hooks install",
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_fps(f: &mut Frame, app: &App, panel: Rect) {
    let text = format!(" {:.0} fps ", app.fps());
    let width = text.chars().count() as u16;
    if panel.width <= width + 2 {
        return;
    }
    let area = Rect {
        x: panel.x + panel.width - width - 1,
        y: panel.y,
        width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Black).bg(Color::Magenta)),
        area,
    );
}

fn draw_form(f: &mut Frame, app: &App, form: &crate::app::NewSession, panel: Rect) {
    let height = (form.chosen.len() as u16)
        .saturating_add(7)
        .min(panel.height);
    let width = 60.min(panel.width.saturating_sub(2));
    let area = Rect {
        x: panel.x + (panel.width.saturating_sub(width)) / 2,
        y: panel.y + (panel.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let name_style = if form.field == Field::Name {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::White)
    };
    let mut lines = vec![
        Line::from(vec![
            Span::raw(" name  "),
            Span::styled(format!("{} ", form.name), name_style),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            if form.field == Field::Repos {
                " repositories  (1-9 toggles)"
            } else {
                " repositories"
            },
            Style::default().fg(Color::Cyan),
        )),
    ];
    if app.project().repos.is_empty() {
        lines.push(dim("  none. culm project repo add <path>"));
    }
    for (i, repo) in app.project().repos.iter().enumerate() {
        let on = form.chosen.get(i).copied().unwrap_or(false);
        lines.push(Line::from(Span::styled(
            format!(
                "  {} {} {}",
                i + 1,
                if on { "[x]" } else { "[ ]" },
                repo.name
            ),
            if on {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Gray)
            },
        )));
    }
    lines.push(Line::from(""));
    lines.push(dim(" Tab switches field   Enter creates   Esc cancels"));

    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" new session ")),
        area,
    );
}

fn draw_terminal(f: &mut Frame, session: &crate::session::Session, title: String, area: Rect) {
    let guard = match session.parser().lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    f.render_widget(
        PseudoTerminal::new(guard.screen()).block(Block::bordered().title(title)),
        area,
    );
}

/// Panel geometry in rows and columns, once borders are removed.
#[must_use]
pub fn panel_size(panel: Rect) -> (u16, u16) {
    (
        panel.height.saturating_sub(2),
        panel.width.saturating_sub(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_name_is_truncated_with_an_ellipsis() {
        assert_eq!(truncate("feature-branch", 6), "featu…");
        assert_eq!(truncate("short", 10), "short");
    }
}
