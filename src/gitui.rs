//! Draws the changes panel, laid out as GitHub Desktop lays out a repository: a
//! list on the left, and the diff of the picked file on the right, old version
//! beside new.

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::diff::{ChangeKind, FileChange, LineColors, LineKind, Row, Side};
use crate::gitview::{
    DiffView, GitHit, GitList, GitTab, GitView, ListCursor, WrappedDiff, list_columns,
};
use crate::ui::{CURSOR_FILL, pane_block, truncate};

// Every color of the diff is a fixed RGB value, never one of the sixteen theme
// colors, so no theme can draw text and fill in the same color.
const REMOVED_LINE: Color = Color::Rgb(0x4a, 0x1f, 0x1f);
const REMOVED_WORD: Color = Color::Rgb(0x8c, 0x2f, 0x2f);
const ADDED_LINE: Color = Color::Rgb(0x1f, 0x3d, 0x26);
const ADDED_WORD: Color = Color::Rgb(0x2f, 0x73, 0x40);
const HUNK_FILL: Color = Color::Rgb(0x30, 0x30, 0x38);
const HUNK_TEXT: Color = Color::Rgb(0x9a, 0x9a, 0xb0);
const GUTTER_TEXT: Color = Color::Rgb(0x80, 0x80, 0x80);
const PLAIN_TEXT: Color = Color::Rgb(0xd5, 0xc4, 0xa1);
const DIM: Color = Color::Rgb(0x92, 0x83, 0x74);
const RULE: Color = Color::Rgb(0x50, 0x49, 0x45);

/// Draws the panel into `area` and reports where its parts landed.
pub fn draw_git(f: &mut Frame, view: &GitView, area: Rect, focused: bool) -> GitHit {
    let title = match view.caption.as_deref() {
        Some(caption) => format!(" changes · {caption} "),
        None => " changes ".to_string(),
    };
    let block = pane_block(title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut hit = GitHit {
        area,
        separator_col: Some(area.x),
        ..GitHit::default()
    };
    if inner.height < 4 || inner.width < 20 {
        return hit;
    }
    if view.repos.is_empty() {
        f.render_widget(
            Paragraph::new(if view.follows_shell() {
                "this project registers no repository. add one with: culm project repo add <path>"
            } else {
                "this session has registered no worktree and has not written a file in a git \
                 repository yet. a tab appears for every worktree it registers with culm \
                 session worktree add, and for every repository it writes in, after its turn \
                 ends."
            })
            .style(Style::default().fg(DIM))
            .wrap(Wrap { trim: false }),
            inner,
        );
        return hit;
    }

    // The repository tabs.
    let mut x = inner.x;
    let mut spans = Vec::new();
    for (index, repo) in view.repos.iter().enumerate() {
        let label = format!(" {} ", repo.name);
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        let style = if index == view.repo {
            Style::default()
                .bg(CURSOR_FILL)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(DIM)
        };
        hit.repo_tabs
            .push((Rect::new(x, inner.y, width.min(inner.width), 1), index));
        x = x.saturating_add(width).saturating_add(1);
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(Line::from(spans), Rect { height: 1, ..inner });

    let body = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };
    let Some(repo) = view.repos.get(view.repo) else {
        return hit;
    };

    // The left side: the tab row, then one list or two.
    let left_width = list_columns(view.tab, view.list_percent(), body.width);
    hit.body = body;
    hit.list_rule_col = (left_width > 0).then(|| body.x + left_width - 1);
    let left = Rect {
        width: left_width,
        ..body
    };
    let diff_area = Rect {
        x: body.x + left_width,
        width: body.width - left_width,
        ..body
    };

    // A shared repository says how many changes it left out, so the list never
    // looks complete when it is not.
    let changes_label = if repo.hidden > 0 {
        format!("Changes ({}+{})", repo.changes.len(), repo.hidden)
    } else {
        format!("Changes ({})", repo.changes.len())
    };
    let half = left.width / 2;
    for (i, (label, tab)) in [
        (changes_label.as_str(), GitTab::Changes),
        ("History", GitTab::History),
    ]
    .into_iter()
    .enumerate()
    {
        let rect = Rect {
            x: left.x + half * u16::try_from(i).unwrap_or(0),
            y: left.y,
            width: if i == 0 { half } else { left.width - half },
            height: 1,
        };
        let style = if tab == view.tab {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(DIM)
        };
        f.render_widget(Paragraph::new(label).centered().style(style), rect);
        hit.mode_tabs.push((rect, tab));
    }
    let lists = Rect {
        y: left.y + 1,
        height: left.height.saturating_sub(1),
        ..left
    };

    if let Some(error) = repo.error.as_deref() {
        f.render_widget(
            Paragraph::new(error.to_string())
                .style(Style::default().fg(DIM))
                .wrap(Wrap { trim: false })
                .block(rule_right()),
            lists,
        );
    } else {
        match view.tab {
            GitTab::Changes => {
                let rect = draw_files(
                    f,
                    &repo.changes,
                    &repo.files,
                    focused && view.list() == GitList::Files,
                    if repo.hidden > 0 {
                        "no change this session wrote. a shows every change"
                    } else {
                        "no local changes"
                    },
                    lists,
                );
                hit.lists
                    .push((rect, GitList::Files, repo.files.offset.get(), 1));
            }
            GitTab::History => {
                // The commits take a little more than half, as their subjects are
                // longer than paths.
                let commit_width = lists.width * 11 / 20;
                let commits = Rect {
                    width: commit_width,
                    ..lists
                };
                let files = Rect {
                    x: lists.x + commit_width,
                    width: lists.width - commit_width,
                    ..lists
                };
                let rect =
                    draw_commits(f, view, focused && view.list() == GitList::Commits, commits);
                hit.lists
                    .push((rect, GitList::Commits, repo.commit_list.offset.get(), 2));
                let rect = draw_files(
                    f,
                    &repo.commit_files,
                    &repo.commit_file_list,
                    focused && view.list() == GitList::CommitFiles,
                    "",
                    files,
                );
                hit.lists.push((
                    rect,
                    GitList::CommitFiles,
                    repo.commit_file_list.offset.get(),
                    1,
                ));
            }
        }
    }

    hit.diff = draw_diff(f, view, diff_area);
    hit
}

/// A thin rule on the right edge, between a list and what sits right of it.
fn rule_right() -> Block<'static> {
    Block::new()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(RULE))
}

/// The color of the mark in front of a path.
fn kind_color(kind: ChangeKind) -> Color {
    match kind {
        ChangeKind::Added | ChangeKind::Untracked => Color::Rgb(0x8e, 0xc0, 0x7c),
        ChangeKind::Modified => Color::Rgb(0xfa, 0xbd, 0x2f),
        ChangeKind::Deleted => Color::Rgb(0xfb, 0x49, 0x34),
        ChangeKind::Renamed => Color::Rgb(0x83, 0xa5, 0x98),
        ChangeKind::Conflicted => Color::Rgb(0xd3, 0x86, 0x9b),
    }
}

/// A list of files, one row each: the mark, the folder dimmed, the name bright. A
/// path too long keeps its end, because the name is what tells files apart.
fn draw_files(
    f: &mut Frame,
    files: &[FileChange],
    cursor: &ListCursor,
    focused: bool,
    empty: &str,
    area: Rect,
) -> Rect {
    let block = rule_right();
    let inner = block.inner(area);
    f.render_widget(block, area);
    if files.is_empty() {
        f.render_widget(
            Paragraph::new(empty.to_string()).style(Style::default().fg(DIM)),
            inner,
        );
        return inner;
    }
    let room = usize::from(inner.height);
    let first = cursor.first_visible(files.len(), room);
    let width = usize::from(inner.width);
    let lines: Vec<Line<'static>> = files
        .iter()
        .enumerate()
        .skip(first)
        .take(room)
        .map(|(index, file)| {
            let picked = index == cursor.at;
            let fill = if picked {
                Style::default().bg(CURSOR_FILL)
            } else {
                Style::default()
            };
            let name_style = if picked && focused {
                fill.fg(Color::Rgb(0xfb, 0xf1, 0xc7))
                    .add_modifier(Modifier::BOLD)
            } else {
                fill.fg(PLAIN_TEXT)
            };
            let path_room = width.saturating_sub(3);
            let (folder, name) = match file.path.rsplit_once('/') {
                Some((dir, name)) => (format!("{dir}/"), name.to_string()),
                None => (String::new(), file.path.clone()),
            };
            let (folder, name) = fit_path(&folder, &name, path_room);
            let used = 3 + folder.chars().count() + name.chars().count();
            Line::from(vec![
                Span::styled(" ", fill),
                Span::styled(file.kind.symbol(), fill.fg(kind_color(file.kind))),
                Span::styled(" ", fill),
                Span::styled(folder, fill.fg(DIM)),
                Span::styled(name, name_style),
                Span::styled(" ".repeat(width.saturating_sub(used)), fill),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
    inner
}

/// Shortens a path to `room` columns: the folder loses its start first, then the
/// name does.
fn fit_path(folder: &str, name: &str, room: usize) -> (String, String) {
    let name_len = name.chars().count();
    let folder_len = folder.chars().count();
    if name_len + folder_len <= room {
        return (folder.to_string(), name.to_string());
    }
    if name_len + 2 <= room {
        let keep = room - name_len - 1;
        let tail: String = folder.chars().skip(folder_len - keep).collect();
        return (format!("…{tail}"), name.to_string());
    }
    (String::new(), truncate(name, room))
}

/// The history list. Two rows per commit, as GitHub Desktop draws it: the
/// subject, then the author and the age.
fn draw_commits(f: &mut Frame, view: &GitView, focused: bool, area: Rect) -> Rect {
    let block = rule_right();
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(repo) = view.repos.get(view.repo) else {
        return inner;
    };
    if repo.commits.is_empty() {
        f.render_widget(
            Paragraph::new("no commits yet").style(Style::default().fg(DIM)),
            inner,
        );
        return inner;
    }
    let room = usize::from(inner.height / 2);
    let first = repo.commit_list.first_visible(repo.commits.len(), room);
    let width = usize::from(inner.width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, commit) in repo.commits.iter().enumerate().skip(first).take(room) {
        let picked = index == repo.commit_list.at;
        let fill = if picked {
            Style::default().bg(CURSOR_FILL)
        } else {
            Style::default()
        };
        let subject_style = if picked && focused {
            fill.fg(Color::Rgb(0xfb, 0xf1, 0xc7))
                .add_modifier(Modifier::BOLD)
        } else {
            fill.fg(PLAIN_TEXT).add_modifier(Modifier::BOLD)
        };
        let subject = truncate(&commit.subject, width.saturating_sub(1));
        let pad = width.saturating_sub(1 + subject.chars().count());
        lines.push(Line::from(vec![
            Span::styled(" ", fill),
            Span::styled(subject, subject_style),
            Span::styled(" ".repeat(pad), fill),
        ]));
        let meta = truncate(
            &format!(
                "{} · {}",
                commit.author,
                crate::diff::ago(view.now(), commit.time)
            ),
            width.saturating_sub(1),
        );
        let pad = width.saturating_sub(1 + meta.chars().count());
        lines.push(Line::from(vec![
            Span::styled(" ", fill),
            Span::styled(meta, fill.fg(DIM)),
            Span::styled(" ".repeat(pad), fill),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
    inner
}

/// The diff: a header with the file, then the old version left and the new right.
fn draw_diff(f: &mut Frame, view: &GitView, area: Rect) -> Rect {
    if area.height < 2 || area.width < 10 {
        return Rect::default();
    }
    let header = Rect { height: 1, ..area };
    let body = Rect {
        y: area.y + 1,
        height: area.height - 1,
        ..area
    };
    view.diff_rows.set(body.height);
    let Some(diff) = view.diff.as_ref() else {
        f.render_widget(
            Paragraph::new(" pick a file to see its changes").style(Style::default().fg(DIM)),
            body,
        );
        return body;
    };
    f.render_widget(
        Paragraph::new(format!(
            " {}",
            truncate(&diff.title, usize::from(area.width).saturating_sub(1))
        ))
        .style(Style::default().fg(PLAIN_TEXT).add_modifier(Modifier::BOLD)),
        header,
    );
    if let Some(notice) = diff.notice.as_deref() {
        f.render_widget(
            Paragraph::new(format!(" {notice}")).style(Style::default().fg(DIM)),
            body,
        );
        return body;
    }

    let half = body.width / 2;
    // One column between the versions, so a wrapped line never runs into the other.
    let left = Rect {
        width: half.saturating_sub(1),
        ..body
    };
    let rule = Rect {
        x: body.x + left.width,
        width: 1,
        ..body
    };
    f.render_widget(
        Block::new()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(RULE)),
        rule,
    );
    let right = Rect {
        x: body.x + half,
        width: body.width - half,
        ..body
    };
    {
        let mut cache = diff.cache.borrow_mut();
        if cache.as_ref().is_none_or(|c| c.width != body.width) {
            *cache = Some(wrap_diff(diff, left.width, right.width, body.width));
        }
    }
    let cache = diff.cache.borrow();
    let Some(wrapped) = cache.as_ref() else {
        return body;
    };
    let total = wrapped.left.len();
    let scroll = diff
        .scroll
        .get()
        .min(total.saturating_sub(usize::from(body.height)));
    diff.scroll.set(scroll);
    let take = usize::from(body.height);
    let slice = |lines: &[Line<'static>]| -> Vec<Line<'static>> {
        lines.iter().skip(scroll).take(take).cloned().collect()
    };
    f.render_widget(Paragraph::new(slice(&wrapped.left)), left);
    f.render_widget(Paragraph::new(slice(&wrapped.right)), right);
    body
}

/// Wraps every row of the diff to the two column widths, row for row, so a long
/// line on one side keeps the other side level with it.
fn wrap_diff(diff: &DiffView, left_width: u16, right_width: u16, width: u16) -> WrappedDiff {
    let max_line = diff.old_lines.len().max(diff.new_lines.len());
    let gutter = max_line.to_string().len() + 1;
    let mut out = WrappedDiff {
        width,
        left: Vec::new(),
        right: Vec::new(),
    };
    for row in &diff.rows {
        match row {
            Row::Hunk(text) => {
                let fill = Style::default().bg(HUNK_FILL);
                out.left.push(padded(
                    vec![Span::styled(format!(" {text}"), fill.fg(HUNK_TEXT))],
                    usize::from(left_width),
                    fill,
                ));
                out.right
                    .push(padded(Vec::new(), usize::from(right_width), fill));
            }
            Row::Pair { left, right } => {
                let mut l = side_rows(
                    left.as_ref(),
                    &diff.old_lines,
                    &diff.old_colors,
                    gutter,
                    usize::from(left_width),
                );
                let mut r = side_rows(
                    right.as_ref(),
                    &diff.new_lines,
                    &diff.new_colors,
                    gutter,
                    usize::from(right_width),
                );
                let rows = l.len().max(r.len());
                l.resize(rows, filler(left.as_ref(), usize::from(left_width)));
                r.resize(rows, filler(right.as_ref(), usize::from(right_width)));
                out.left.extend(l);
                out.right.extend(r);
            }
        }
    }
    out
}

/// The fill of a line by what happened to it.
fn line_fill(kind: LineKind) -> Style {
    match kind {
        LineKind::Context => Style::default(),
        LineKind::Removed => Style::default().bg(REMOVED_LINE),
        LineKind::Added => Style::default().bg(ADDED_LINE),
    }
}

/// The rows below the first of a wrapped line, or the blank beside a line that
/// has no partner.
fn filler(side: Option<&Side>, width: usize) -> Line<'static> {
    let fill = side.map_or_else(Style::default, |s| line_fill(s.kind));
    padded(Vec::new(), width, fill)
}

/// The rows one line of one side takes: the line number and the mark on the first
/// row, the text wrapped across as many rows as it needs.
fn side_rows(
    side: Option<&Side>,
    lines: &[String],
    colors: &[LineColors],
    gutter: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(side) = side else {
        return vec![padded(Vec::new(), width, Style::default())];
    };
    let fill = line_fill(side.kind);
    let text = lines.get(side.line).map_or("", String::as_str);
    let empty = Vec::new();
    let line_colors = colors.get(side.line).unwrap_or(&empty);
    let mark = match side.kind {
        LineKind::Context => " ",
        LineKind::Removed => "-",
        LineKind::Added => "+",
    };
    let text_width = width.saturating_sub(gutter + 2).max(1);
    let emphasis = match side.kind {
        LineKind::Removed => Some(REMOVED_WORD),
        LineKind::Added => Some(ADDED_WORD),
        LineKind::Context => None,
    };
    let wrapped = wrap_styled(
        text,
        line_colors,
        &side.emphasis,
        emphasis,
        fill,
        text_width,
    );
    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, spans)| {
            let number = if i == 0 {
                format!("{:>w$} ", side.line + 1, w = gutter - 1)
            } else {
                " ".repeat(gutter)
            };
            let mark = if i == 0 { mark } else { " " };
            let mut row = vec![
                Span::styled(number, fill.fg(GUTTER_TEXT)),
                Span::styled(format!("{mark} "), fill.fg(GUTTER_TEXT)),
            ];
            row.extend(spans);
            padded(row, width, fill)
        })
        .collect()
}

/// Pads a row with its fill to `width`, so the fill reaches the column's edge.
fn padded(mut spans: Vec<Span<'static>>, width: usize, fill: Style) -> Line<'static> {
    let used: usize = spans.iter().map(Span::width).sum();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), fill));
    }
    Line::from(spans)
}

/// Cuts a line into rows of `width` columns, each a run of spans that share one
/// style: the syntax color, on the line's fill, on the word fill where it changed.
fn wrap_styled(
    text: &str,
    colors: &LineColors,
    emphasis: &[Range<usize>],
    word_fill: Option<Color>,
    fill: Style,
    width: usize,
) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut row_width = 0usize;
    let mut run = String::new();
    let mut run_style = fill;
    let mut color_at = 0usize;
    for (byte, ch) in text.char_indices() {
        while colors.get(color_at).is_some_and(|(_, r)| r.end <= byte) {
            color_at += 1;
        }
        let fg = colors
            .get(color_at)
            .filter(|(_, r)| r.contains(&byte))
            .map_or(PLAIN_TEXT, |(c, _)| *c);
        let mut style = fill.fg(fg);
        if let Some(word) = word_fill
            && emphasis.iter().any(|r| r.contains(&byte))
        {
            style = style.bg(word);
        }
        let w = ch.width().unwrap_or(0);
        if row_width + w > width && row_width > 0 {
            if !run.is_empty() {
                row.push(Span::styled(std::mem::take(&mut run), run_style));
            }
            rows.push(std::mem::take(&mut row));
            row_width = 0;
        }
        if style != run_style && !run.is_empty() {
            row.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = style;
        run.push(ch);
        row_width += w;
    }
    if !run.is_empty() {
        row.push(Span::styled(run, run_style));
    }
    rows.push(row);
    rows
}
