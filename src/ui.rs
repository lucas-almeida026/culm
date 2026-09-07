//! Rendering, and the hit boxes that map a click back to a session or to the
//! separator. One session is visible at a time, in the manner of a browser with
//! vertical tabs. Sessions that are not visible are still parsed, never drawn.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, Wrap};
use tui_term::widget::PseudoTerminal;

use crate::app::{Answer, App, ConfirmDelete, Entry, Field, Focus, Modal, Rename};
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
    /// The panel without its border. A selection is measured from this corner.
    pub inner: Rect,
    /// The row holding the search box. A click there focuses it.
    pub search_row: Option<u16>,
    /// Where each button of the open confirmation sits. A click answers it.
    pub buttons: Vec<(Rect, Answer)>,
}

impl Default for HitBox {
    fn default() -> Self {
        Self {
            sidebar_width: crate::app::SIDEBAR_DEFAULT,
            separator_col: crate::app::SIDEBAR_DEFAULT - 1,
            rows: Vec::new(),
            panel: Rect::new(0, 0, 0, 0),
            inner: Rect::new(0, 0, 0, 0),
            search_row: None,
            buttons: Vec::new(),
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

    let (rows, search_row) = draw_sidebar(f, app, sidebar);
    draw_panel(f, app, panel);
    if let Some((start, end)) = app.selection() {
        highlight(f, inner_of(panel), start, end);
    }
    draw_status(f, app, status_bar);
    if app.nerd_mode() {
        draw_fps(f, app, panel);
    }
    let mut buttons = Vec::new();
    match app.modal() {
        Some(Modal::NewSession(form)) => draw_form(f, app, form, panel),
        Some(Modal::ConfirmDelete(confirm)) => buttons = draw_confirm(f, confirm, panel),
        Some(Modal::Rename(rename)) => draw_rename(f, rename, panel),
        Some(Modal::Help) => draw_help(f, panel),
        None => {}
    }

    HitBox {
        sidebar_width: sidebar.width,
        separator_col: sidebar.x + sidebar.width.saturating_sub(1),
        rows,
        panel,
        inner: inner_of(panel),
        search_row,
        buttons,
    }
}

/// The panel without its border, which is where the child's cells sit.
#[must_use]
pub fn inner_of(panel: Rect) -> Rect {
    Rect {
        x: panel.x.saturating_add(1),
        y: panel.y.saturating_add(1),
        width: panel.width.saturating_sub(2),
        height: panel.height.saturating_sub(2),
    }
}

/// Draws the sidebar and reports where each row landed, and where the search box is.
fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) -> (Vec<(u16, Focus)>, Option<u16>) {
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
    let search_row = area.y + 1 + lines.len() as u16;
    lines.push(search_line(app, inner_width));

    let matches = app.paused_matches();
    for &index in &matches {
        let Some(entry) = app.entries().get(index) else {
            continue;
        };
        rows.push((area.y + 1 + lines.len() as u16, Focus::Entry(index)));
        lines.push(entry_line(
            entry,
            None,
            app.focus() == Focus::Entry(index),
            inner_width,
        ));
    }
    if matches.is_empty() {
        lines.push(dim(if app.filter().is_empty() {
            "  none"
        } else {
            "  no match"
        }));
    }

    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("culm")),
        area,
    );
    (rows, Some(search_row))
}

/// The search box. It filters the paused list on every keystroke.
fn search_line(app: &App, inner_width: usize) -> Line<'static> {
    let style = if app.filter_focused() {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let text = format!(" / {}", app.filter());
    let pad = inner_width.saturating_sub(text.chars().count());
    Line::from(vec![
        Span::styled(text, style),
        Span::styled(" ".repeat(pad), style),
    ])
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
            Some(shell) => draw_terminal(f, shell, title_of("shell", shell), area),
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
        Some(session) => draw_terminal(f, session, title_of(entry.name(), session), area),
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

/// The pointer to the shortcut table. It never changes, so the user always knows
/// where the bindings are without the bar carrying all of them.
const HELP_HINT: &str = " press Alt+Shift+H for shortcuts ";

/// The bottom bar. The left half points at the help, and the right half carries
/// whatever last happened.
fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let hint_width = u16::try_from(HELP_HINT.chars().count()).unwrap_or(0);
    let columns =
        Layout::horizontal([Constraint::Length(hint_width), Constraint::Min(0)]).split(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            HELP_HINT,
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ))),
        columns[0],
    );

    let mut spans = vec![Span::styled(
        format!(" {}", app.status()),
        Style::default().fg(Color::Yellow),
    )];
    if !app.hooks_installed() {
        spans.push(Span::styled(
            "   markers off, run: culm hooks install",
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), columns[1]);
}

/// Every binding culm reserves, and what each one does.
const SHORTCUTS: [(&str, &str); 14] = [
    ("Alt+0", "focus the shell at position 0"),
    ("Alt+1 to Alt+9", "focus an active session"),
    ("Alt+Shift+N", "create a session"),
    ("Alt+Shift+P", "pause or resume the focused session"),
    ("Alt+Shift+R", "rename the focused session"),
    ("Alt+Shift+F", "search the paused list"),
    ("Alt+Shift+X", "delete the focused paused session"),
    ("Alt+Shift+D", "toggle nerd mode, for the frame rate"),
    ("Alt+Shift+H", "this table"),
    ("Ctrl+q", "quit"),
    ("Shift+PageUp/Down", "scroll half a panel"),
    ("wheel", "scroll, or reach a mouse-aware child"),
    ("drag", "select, and copy on release"),
    ("middle click", "paste the last copy"),
];

/// The shortcut table. Everything else the bar used to list lives here.
fn draw_help(f: &mut Frame, panel: Rect) {
    let width = 64.min(panel.width.saturating_sub(2));
    let height = u16::try_from(SHORTCUTS.len() + 3)
        .unwrap_or(u16::MAX)
        .min(panel.height);
    let area = Rect {
        x: panel.x + (panel.width.saturating_sub(width)) / 2,
        y: panel.y + (panel.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let rows = SHORTCUTS.iter().map(|(keys, does)| {
        Row::new([
            Cell::from(Span::styled(
                format!(" {keys}"),
                Style::default().fg(Color::Cyan),
            )),
            Cell::from(Span::styled(*does, Style::default().fg(Color::Gray))),
        ])
    });
    f.render_widget(Clear, area);
    f.render_widget(
        Table::new(rows, [Constraint::Length(20), Constraint::Min(10)])
            .block(Block::bordered().title(" shortcuts "))
            .column_spacing(1),
        area,
    );
    // The hint sits on the last row inside the border, below the table.
    let hint = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(2),
        width: area.width.saturating_sub(2),
        height: 1,
    };
    if area.height > 2 {
        f.render_widget(Paragraph::new(dim(" Esc closes")), hint);
    }
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

/// The panel title, with an arrow when the view sits above the live output, so that
/// a held-back panel never looks like a stalled session.
fn title_of(name: &str, session: &crate::session::Session) -> String {
    match session.scrollback() {
        0 => format!(" {name} "),
        n => format!(" {name}  ↑{n} "),
    }
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

/// The delete confirmation, as two buttons.
///
/// The cursor starts on `no`, because deletion has no undo. Returns where each button
/// landed, so a click answers the same question the keys do.
fn draw_confirm(f: &mut Frame, confirm: &ConfirmDelete, panel: Rect) -> Vec<(Rect, Answer)> {
    let width = 60.min(panel.width.saturating_sub(2));
    // Eight lines of content, plus the two border rows.
    let height = 10.min(panel.height);
    let area = Rect {
        x: panel.x + (panel.width.saturating_sub(width)) / 2,
        y: panel.y + (panel.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let lines = vec![
        Line::from(Span::styled(
            format!(" delete {} and its transcript?", confirm.name),
            Style::default().fg(Color::Red),
        )),
        Line::from(""),
        dim(" the worktrees and the branch stay on disk."),
        dim(" this cannot be undone."),
        Line::from(""),
        Line::from(vec![
            Span::raw(" "),
            button(YES, confirm.answer == Answer::Yes, Color::Red),
            Span::raw("  "),
            button(NO, confirm.answer == Answer::No, Color::Green),
        ]),
        Line::from(""),
        dim(" y or n picks   Enter answers   Esc cancels"),
    ];
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" delete session ")),
        area,
    );

    // The button row is the sixth line, one row below the border, and the two labels
    // sit after the leading space with two spaces between them. `ui::tests` renders
    // the form and reads the buffer back, so these offsets never drift silently.
    let row = area.y + 1 + 5;
    if row >= area.y + area.height.saturating_sub(1) {
        return Vec::new();
    }
    let left = area.x + 2;
    let yes = u16::try_from(YES.len()).unwrap_or(0);
    vec![
        (Rect::new(left, row, yes, 1), Answer::Yes),
        (
            Rect::new(left + yes + 2, row, u16::try_from(NO.len()).unwrap_or(0), 1),
            Answer::No,
        ),
    ]
}

/// The two button labels. The brackets keep them reading as buttons on a terminal
/// that shows no color.
const YES: &str = "[ yes ]";
const NO: &str = "[ no ]";

/// One button. The cursor is a filled label, and the rest is an outline.
fn button(label: &str, focused: bool, color: Color) -> Span<'static> {
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Span::styled(label.to_string(), style)
}

/// Marks the selected cells by reversing them, over whatever the child drew.
///
/// The style is applied to the finished buffer rather than to the child's screen,
/// because the child owns its own colors and must not learn about the selection.
fn highlight(f: &mut Frame, inner: Rect, start: (u16, u16), end: (u16, u16)) {
    let buffer = f.buffer_mut();
    let last_row = end.0.min(inner.height.saturating_sub(1));
    for row in start.0..=last_row {
        let first_col = if row == start.0 { start.1 } else { 0 };
        let last_col = if row == end.0 {
            end.1
        } else {
            inner.width.saturating_sub(1)
        };
        for col in first_col..=last_col.min(inner.width.saturating_sub(1)) {
            let at = Position {
                x: inner.x + col,
                y: inner.y + row,
            };
            if let Some(cell) = buffer.cell_mut(at) {
                cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

/// The rename form. Only the displayed name changes, so the hint says so and no
/// confirmation is asked for.
fn draw_rename(f: &mut Frame, rename: &Rename, panel: Rect) {
    let width = 60.min(panel.width.saturating_sub(2));
    let height = 7.min(panel.height);
    let area = Rect {
        x: panel.x + (panel.width.saturating_sub(width)) / 2,
        y: panel.y + (panel.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let lines = vec![
        Line::from(vec![
            Span::raw(" name  "),
            Span::styled(
                format!("{} ", rename.name),
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ),
        ]),
        Line::from(""),
        dim(" the branch and the worktree keep their names."),
        Line::from(""),
        dim(" Enter applies   Esc cancels"),
    ];
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" rename session ")),
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
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::app::ConfirmDelete;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn a_long_name_is_truncated_with_an_ellipsis() {
        assert_eq!(truncate("feature-branch", 6), "featu…");
        assert_eq!(truncate("short", 10), "short");
    }

    /// The button rectangles are computed from the layout by hand, so this renders the
    /// form and reads the buffer back to prove each rectangle covers its own label.
    #[test]
    fn each_confirmation_button_rectangle_covers_its_label() {
        let confirm = ConfirmDelete {
            index: 0,
            name: "feat A".into(),
            answer: Answer::No,
        };
        let panel = Rect::new(10, 0, 70, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("the test backend starts");
        let mut buttons = Vec::new();
        terminal
            .draw(|f| buttons = draw_confirm(f, &confirm, panel))
            .expect("the form draws");

        let buffer = terminal.backend().buffer();
        let read = |rect: Rect| {
            (rect.x..rect.x + rect.width)
                .filter_map(|x| buffer.cell(Position { x, y: rect.y }))
                .map(|cell| cell.symbol().to_string())
                .collect::<String>()
        };
        assert_eq!(buttons.len(), 2);
        assert_eq!(read(buttons[0].0), "[ yes ]");
        assert_eq!(buttons[0].1, Answer::Yes);
        assert_eq!(read(buttons[1].0), "[ no ]");
        assert_eq!(buttons[1].1, Answer::No);
    }

    /// The table is drawn at a fixed width, so a description that outgrows its
    /// column would be silently cut on screen.
    #[test]
    fn every_shortcut_description_fits_its_column() {
        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("the test backend starts");
        terminal
            .draw(|f| draw_help(f, Rect::new(0, 0, 80, 24)))
            .expect("the table draws");

        let buffer = terminal.backend().buffer();
        // `Cell` here is the table cell, so the buffer cell is reached by closure.
        let text: String = buffer
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        for (keys, does) in SHORTCUTS {
            assert!(text.contains(keys), "{keys} is missing from the table");
            assert!(text.contains(does), "the description of {keys} is cut off");
        }
    }

    #[test]
    fn a_panel_too_short_for_the_button_row_reports_no_button() {
        let confirm = ConfirmDelete {
            index: 0,
            name: "feat A".into(),
            answer: Answer::No,
        };
        let mut terminal =
            Terminal::new(TestBackend::new(80, 20)).expect("the test backend starts");
        let mut buttons = Vec::new();
        terminal
            .draw(|f| buttons = draw_confirm(f, &confirm, Rect::new(10, 0, 70, 4)))
            .expect("the form draws");

        assert!(buttons.is_empty(), "no click lands on a row never drawn");
    }
}
