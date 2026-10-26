//! The rows a self-scrolling child moved off the panel while a selection was held.
//!
//! Claude Code paints its own viewport, so nothing it scrolls away reaches the
//! `vt100` scrollback. culm already lines the rows up before and after each wheel
//! notch to carry a selection. The same comparison names every row that moved with
//! the text, and culm keeps those rows. A row Claude Code draws over the text, such
//! as the prompt it pins to the top or its "Jump to bottom" notice, stays in place
//! while the text moves, so it never counts. A spike on 2026-10-26 scrolled one
//! session up 120 notches in a 40 row panel: the 386 rows kept all equaled a render
//! of the whole conversation, and no row was missing.

use std::collections::BTreeMap;

/// The fewest visible characters a notice drawn at the right end of a row holds.
/// Less than this is as likely a word that two rows share.
const NOTICE_CHARS: usize = 8;

/// Rows by panel row at the view culm holds now. A row above the panel is negative.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeenRows {
    rows: BTreeMap<i32, String>,
    /// True once the first move was recorded.
    started: bool,
}

impl SeenRows {
    pub fn clear(&mut self) {
        self.rows.clear();
        self.started = false;
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The child moved its text `shift` rows down the panel between `before` and
    /// `now`. Every kept row moves with it, and every row that moved too is kept.
    pub fn moved(&mut self, before: &[String], now: &[String], shift: i32) {
        let old = std::mem::take(&mut self.rows);
        self.rows = old
            .into_iter()
            .map(|(row, text)| (row.saturating_add(shift), text))
            .collect();
        if !self.started {
            // The panel before the first move is the text as it was, past what
            // stays in place. The rows at its bottom leave under the prompt box
            // with this move, so no later panel would show them again.
            self.started = true;
            for (i, text) in before.iter().enumerate() {
                let (Ok(from), Some(now_row)) = (i32::try_from(i), now.get(i)) else {
                    continue;
                };
                // A row that stays where it was belongs to the frame, as the prompt
                // box does, unless it also moved with the text, as the repeated rule
                // of a table does.
                let to = usize::try_from(from.saturating_add(shift)).ok();
                let moved_too = to.and_then(|t| now.get(t)) == Some(text);
                if now_row != text || moved_too {
                    self.rows
                        .insert(from.saturating_add(shift), without_notice(text, now_row));
                }
            }
        }
        for (i, text) in before.iter().enumerate() {
            let Ok(from) = i32::try_from(i) else {
                continue;
            };
            let to = from.saturating_add(shift);
            let Some(landed) = usize::try_from(to).ok().and_then(|t| now.get(t)) else {
                continue;
            };
            if landed == text {
                self.rows.insert(to, text.clone());
            }
        }
    }

    #[must_use]
    pub fn row(&self, row: i32) -> Option<&str> {
        self.rows.get(&row).map(String::as_str)
    }
}

/// `row` without a notice drawn at its right end that also sits, unchanged, at the
/// end of `same_place`, the row at the same place after the move.
fn without_notice(row: &str, same_place: &str) -> String {
    let common: usize = row
        .chars()
        .rev()
        .zip(same_place.chars().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let cut = row.chars().count() - common;
    let notice: String = row.chars().skip(cut).collect();
    let head: String = row.chars().take(cut).collect();
    let visible = notice.chars().filter(|c| !c.is_whitespace()).count();
    // A notice stands apart from the text it covers.
    if visible >= NOTICE_CHARS && (head.ends_with("  ") || notice.starts_with("  ")) {
        head.trim_end().to_string()
    } else {
        row.to_string()
    }
}

/// The text between two cells, both ends included, for a child that scrolls
/// itself. A kept row wins over the panel, because the panel row may carry a notice
/// drawn over the text. `live` holds the rows on the panel now.
#[must_use]
pub fn text_between(
    seen: &SeenRows,
    live: &[String],
    start: (i32, u16),
    end: (i32, u16),
) -> String {
    let mut out = Vec::new();
    for row in start.0..=end.0 {
        let text = seen
            .row(row)
            .or_else(|| {
                usize::try_from(row)
                    .ok()
                    .and_then(|r| live.get(r))
                    .map(String::as_str)
            })
            .unwrap_or("");
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 { Some(end.1) } else { None };
        out.push(columns(text, from, to).trim_end().to_string());
    }
    out.join("\n")
}

/// The part of a row from column `from` to column `to`, both included. A wide
/// character takes two columns.
fn columns(text: &str, from: u16, to: Option<u16>) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if to.is_some_and(|to| col > usize::from(to)) {
            break;
        }
        if col >= usize::from(from) {
            out.push(c);
        }
        col += w;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|t| (*t).to_string()).collect()
    }

    #[test]
    fn the_first_panel_is_kept_past_its_frame() {
        let mut seen = SeenRows::default();
        let before = rows(&["c", "d", "e", "> prompt"]);
        let now = rows(&["a", "b", "c", "> prompt"]);
        seen.moved(&before, &now, 2);
        assert_eq!(seen.row(2), Some("c"));
        assert_eq!(
            seen.row(3),
            Some("d"),
            "rows that left under the frame are kept"
        );
        assert_eq!(seen.row(4), Some("e"));
        assert_eq!(
            seen.row(5),
            None,
            "the prompt box stayed in place, so it is frame"
        );
        assert_eq!(
            seen.row(0),
            None,
            "a row seen once later is not confirmed yet"
        );
    }

    #[test]
    fn a_row_drawn_over_the_text_never_counts() {
        let mut seen = SeenRows::default();
        seen.moved(
            &rows(&["e", "f", "g", "h"]),
            &rows(&["c", "d", "e", "f"]),
            2,
        );
        // Scrolled now, so the first row is a pinned prompt.
        let before = rows(&["pinned", "d", "e", "f"]);
        let now = rows(&["pinned", "b", "c", "d"]);
        seen.moved(&before, &now, 2);
        assert_eq!(
            seen.row(3),
            Some("d"),
            "the row the pinned prompt covered moved"
        );
        assert_eq!(
            seen.row(2),
            None,
            "c was seen once, so it waits for the next move"
        );
        assert_eq!(
            seen.row(0),
            None,
            "the pinned prompt did not move with the text"
        );
        assert_eq!(seen.row(4), Some("e"));
    }

    #[test]
    fn a_notice_at_the_end_of_a_row_is_cut_away() {
        let mut seen = SeenRows::default();
        let notice = "    Jump to bottom (ctrl+End) ↓";
        let before = rows(&["a", &format!("   M android/gradle.properties{notice}")]);
        let now = rows(&["z", &format!("   M .vscode/launch.json      {notice}")]);
        seen.moved(&before, &now, 1);
        assert_eq!(seen.row(2), Some("   M android/gradle.properties"));
    }

    #[test]
    fn rows_after_the_first_move_count_only_once_they_moved_with_the_text() {
        let mut seen = SeenRows::default();
        seen.moved(&rows(&["c", "d"]), &rows(&["b", "c"]), 1);
        seen.moved(&rows(&["b", "x"]), &rows(&["a", "b"]), 1);
        assert_eq!(seen.row(1), Some("b"));
        assert_eq!(
            seen.row(2),
            Some("c"),
            "a row drawn over a kept row in a later panel does not replace it"
        );
    }

    #[test]
    fn kept_rows_follow_every_later_move() {
        let mut seen = SeenRows::default();
        seen.moved(&rows(&["c", "d"]), &rows(&["b", "c"]), 1);
        seen.moved(&rows(&["b", "c"]), &rows(&["a", "b"]), 1);
        assert_eq!(seen.row(2), Some("c"));
        assert_eq!(seen.row(3), Some("d"));
        assert_eq!(seen.row(1), Some("b"));
    }

    #[test]
    fn a_selection_reads_kept_rows_past_the_panel_and_cuts_its_ends() {
        let mut seen = SeenRows::default();
        seen.moved(&rows(&["third", "fourth"]), &rows(&["first", "second"]), 2);
        let live = rows(&["first row", "second row"]);
        let text = text_between(&seen, &live, (0, 6), (3, 3));
        assert_eq!(text, "row\nsecond row\nthird\nfour");
    }

    #[test]
    fn a_rule_that_repeats_with_the_move_is_text() {
        let mut seen = SeenRows::default();
        // A table rule every three rows sits in place after a move of three rows.
        let before = rows(&["├─┤", "a", "b", "├─┤", "c", "> prompt"]);
        let now = rows(&["├─┤", "x", "y", "├─┤", "a", "> prompt"]);
        seen.moved(&before, &now, 3);
        assert_eq!(seen.row(3), Some("├─┤"), "the rule moved with the text");
        assert_eq!(seen.row(4), Some("a"));
        assert_eq!(seen.row(8), None, "the prompt box only stayed in place");
    }

    #[test]
    fn a_wide_character_takes_two_columns() {
        assert_eq!(columns("日本語", 2, Some(3)), "本");
    }
}
