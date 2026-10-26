//! The text a copy hands over, rebuilt from what the panel shows.
//!
//! Claude Code breaks every line itself to fit the panel, so a copy of its screen
//! holds a line break wherever a line met the edge. culm finds the copied words in
//! the transcript, where each answer and each prompt keeps its original markdown,
//! and hands over that markdown. Words the transcript does not hold, such as a tool
//! line or command output, come from the screen, with every line that only met the
//! edge joined to the line above it.
//!
//! A spike on 2026-10-26 measured both halves on one session rendered by Claude
//! Code 2.1.310 at 80, 120, and 200 columns. The line rule joined no real break in
//! 1,229 pairs of ordinary lines. The transcript gave 71 to 82 percent of the words
//! of 1,170 simulated selections, and 1,119 of them kept exactly their words.

use std::collections::HashMap;

use serde_json::Value;

/// The fewest words in a row that count as a match in the transcript. A shorter run
/// matches too many places to trust.
const MIN_RUN: usize = 3;

/// Screen words before the selection that break a tie between two places.
const CONTEXT_WORDS: usize = 8;

/// Columns Claude Code leaves free at the right edge before it breaks a line.
const EDGE_SLACK: usize = 2;

/// One word of the transcript.
#[derive(Debug, Clone)]
struct Word {
    lower: String,
    block: usize,
    start: usize,
    end: usize,
}

/// Every answer and prompt of one transcript, indexed by word.
#[derive(Debug, Clone, Default)]
pub struct TranscriptIndex {
    blocks: Vec<String>,
    words: Vec<Word>,
    by_word: HashMap<String, Vec<usize>>,
}

impl TranscriptIndex {
    /// Indexes the text of every answer and every typed prompt.
    #[must_use]
    pub fn new(transcript: &str) -> Self {
        let mut index = Self::default();
        for line in transcript.lines() {
            // A cheap test first, because tool results make up most of the file.
            if !line.contains("\"text\"") && !line.contains("\"content\":\"") {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match value.get("type").and_then(Value::as_str) {
                Some("assistant") => {
                    let parts = value
                        .pointer("/message/content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten();
                    for part in parts {
                        if part.get("type").and_then(Value::as_str) == Some("text")
                            && let Some(text) = part.get("text").and_then(Value::as_str)
                        {
                            index.push(text);
                        }
                    }
                }
                Some("user") => {
                    if let Some(text) = value.pointer("/message/content").and_then(Value::as_str) {
                        index.push(text);
                    }
                }
                _ => {}
            }
        }
        index
    }

    fn push(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let block = self.blocks.len();
        for (start, end) in word_spans(text) {
            let lower = text[start..end].to_lowercase();
            self.by_word
                .entry(lower.clone())
                .or_default()
                .push(self.words.len());
            self.words.push(Word {
                lower,
                block,
                start,
                end,
            });
        }
        self.blocks.push(text.to_string());
    }

    /// True when the transcript held no answer and no prompt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

/// What a copy hands over, and how much of it came from the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebuilt {
    pub text: String,
    /// True when at least one run of words came from the transcript.
    pub markdown: bool,
}

/// Rebuilds a selection of a Claude Code panel.
///
/// `before` is the screen text just above the selection, which breaks a tie when
/// the same words appear twice. `cols` is the panel width.
#[must_use]
pub fn rebuild(selection: &str, before: &str, index: &TranscriptIndex, cols: u16) -> Rebuilt {
    let spans = word_spans(selection);
    let lower: Vec<String> = spans
        .iter()
        .map(|&(a, b)| selection[a..b].to_lowercase())
        .collect();
    let context: Vec<String> = word_spans(before)
        .into_iter()
        .map(|(a, b)| before[a..b].to_lowercase())
        .collect();

    let mut out = String::new();
    let mut markdown = false;
    let mut raw_from = 0;
    let mut p = 0;
    while p < lower.len() {
        let Some((run, at)) = best_run(index, &lower, p, &context) else {
            p += 1;
            continue;
        };
        if run < MIN_RUN {
            p += 1;
            continue;
        }
        let first = &index.words[at];
        let last = &index.words[at + run - 1];
        let text = &index.blocks[first.block];
        let sel_first = &selection[spans[p].0..spans[p].1];
        let sel_last = &selection[spans[p + run - 1].0..spans[p + run - 1].1];
        // An edge word cut mid-word keeps only the part on screen.
        let mut a = cut(text, first.end.saturating_sub(sel_first.len()), first.start);
        let mut b = cut(text, last.start + sel_last.len(), last.end).min(last.end);
        a = a.max(first.start);
        // Markers around a whole word belong to it. Inside a word cut mid-way, a
        // `_` is part of the word.
        if a == first.start {
            while a > 0 && is_marker(text.as_bytes()[a - 1]) {
                a -= 1;
            }
        }
        if b == last.end {
            while b < text.len() && is_marker(text.as_bytes()[b]) {
                b += 1;
            }
        }
        let mut gap = &selection[raw_from..spans[p].0];
        // Where the run starts a screen row, the markdown's own line start replaces
        // the indent and the bullet the screen draws. Mid-row, the gap is text.
        if raw_from == 0 || gap.contains('\n') {
            let line_start = text[..a].rfind('\n').map_or(0, |i| i + 1);
            let screen_head = gap.rfind('\n').map_or(0, |i| i + 1);
            let tail = &gap[screen_head..];
            let head = &text[line_start..a];
            // The answer bullet and the indent are drawing. The prompt mark says
            // who wrote the line, so it stays.
            let drawing = matches!(tail.trim(), "" | "●");
            if head.trim().is_empty() && drawing {
                if !tail.is_empty() {
                    a = line_start;
                }
                gap = &gap[..screen_head];
            } else if is_line_marker(head) && !tail.trim().is_empty() && is_screen_marker(tail) {
                a = line_start;
                gap = &gap[..screen_head];
            } else if tail.trim().is_empty() {
                gap = &gap[..screen_head];
            }
        }
        out.push_str(&rejoin(gap, cols));
        out.push_str(&balance(&text[a..b], text, a));
        markdown = true;
        raw_from = spans[p + run - 1].1;
        p += run;
    }
    out.push_str(&rejoin(&selection[raw_from..], cols));
    Rebuilt {
        text: out,
        markdown,
    }
}

/// The longest run of selection words, from `p`, that the transcript holds in one
/// block, and where it starts. Equal runs go to the place the screen context fits.
fn best_run(
    index: &TranscriptIndex,
    sel: &[String],
    p: usize,
    context: &[String],
) -> Option<(usize, usize)> {
    let mut starts: Vec<usize> = index.by_word.get(&sel[p]).cloned().unwrap_or_default();
    if p == 0 {
        // The first word may be cut mid-word, so it matches as the end of a word.
        starts.extend(
            index
                .words
                .iter()
                .enumerate()
                .filter(|(_, w)| w.lower != sel[0] && w.lower.ends_with(sel[0].as_str()))
                .map(|(i, _)| i),
        );
    }
    let mut best: Option<(usize, usize, usize)> = None;
    for start in starts {
        let block = index.words[start].block;
        let mut q = p;
        let mut j = start;
        while q < sel.len()
            && j < index.words.len()
            && index.words[j].block == block
            && (q == p
                || index.words[j].lower == sel[q]
                || (q + 1 == sel.len() && index.words[j].lower.starts_with(sel[q].as_str())))
        {
            q += 1;
            j += 1;
        }
        let run = q - p;
        let fit = context_fit(index, start, context, &sel[..p]);
        if best.is_none_or(|(r, f, _)| (run, fit) > (r, f)) {
            best = Some((run, fit, start));
        }
    }
    best.map(|(run, _, start)| (run, start))
}

/// How many of the words before the selection run match the words before `start`.
fn context_fit(
    index: &TranscriptIndex,
    start: usize,
    context: &[String],
    head: &[String],
) -> usize {
    context
        .iter()
        .chain(head)
        .rev()
        .take(CONTEXT_WORDS)
        .enumerate()
        .filter(|(k, word)| {
            start
                .checked_sub(k + 1)
                .is_some_and(|j| index.words[j].lower == **word)
        })
        .count()
}

/// Joins every line of screen text that only met the panel edge to the line above.
///
/// A line continues the one above when it opens no block, sits at the indent of
/// the text above, give or take one space, and its first word would not have fit
/// on the line above.
#[must_use]
pub fn rejoin(text: &str, cols: u16) -> String {
    let rows: Vec<&str> = text.split('\n').collect();
    let mut out = String::with_capacity(text.len());
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            let prev = rows[i - 1];
            if !row.trim().is_empty() && !prev.trim().is_empty() && continues(prev, row, cols) {
                let kept = out.trim_end_matches(' ').len();
                out.truncate(kept);
                out.push(' ');
                out.push_str(row.trim());
                continue;
            }
            out.push('\n');
        }
        out.push_str(row);
    }
    out
}

fn continues(prev: &str, row: &str, cols: u16) -> bool {
    let body = row.trim_start_matches(' ');
    if opens_block(body) {
        return false;
    }
    let step = indent(row).abs_diff(content_indent(prev));
    if step > 1 {
        return false;
    }
    let first = body.split(' ').next().unwrap_or("");
    width(prev.trim_end()) + 1 + width(first) > usize::from(cols).saturating_sub(EDGE_SLACK)
}

fn indent(row: &str) -> usize {
    row.len() - row.trim_start_matches(' ').len()
}

/// The column where the text of a row starts, past any bullet or list number.
/// A bullet such as `●` takes three bytes and one column, so this counts columns.
fn content_indent(row: &str) -> usize {
    let body = row.trim_start_matches(' ');
    indent(row) + width(&body[..list_marker_len(body)])
}

/// The length in bytes of a bullet or list number that opens `body`, with its space.
fn list_marker_len(body: &str) -> usize {
    for bullet in ["● ", "• ", "- ", "* "] {
        if body.starts_with(bullet) {
            return bullet.len();
        }
    }
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    let letter =
        usize::from(digits == 0 && body.chars().next().is_some_and(|c| c.is_ascii_lowercase()));
    let head = digits + letter;
    if head > 0
        && head <= 3
        && matches!(body.as_bytes().get(head), Some(b'.' | b')'))
        && body.as_bytes().get(head + 1) == Some(&b' ')
    {
        return head + 2;
    }
    0
}

fn opens_block(body: &str) -> bool {
    list_marker_len(body) > 0
        || ["❯", "│", "┌", "└", "├", "─", "```", "✻", "⎿"]
            .iter()
            .any(|m| body.starts_with(m))
}

fn width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// The start and end of every word, as byte offsets.
fn word_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        let word = c.is_alphanumeric() || c == '_';
        match (word, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }
    spans
}

/// `at` when it falls on a character boundary of `text`, or `fallback`.
fn cut(text: &str, at: usize, fallback: usize) -> usize {
    if at <= text.len() && text.is_char_boundary(at) {
        at
    } else {
        fallback
    }
}

fn is_marker(byte: u8) -> bool {
    matches!(byte, b'*' | b'_' | b'`' | b'~')
}

/// True for the markdown before a word that only opens its line: a bullet, a list
/// number, or a heading.
fn is_line_marker(head: &str) -> bool {
    let head = head.trim();
    head.is_empty()
        || ["-", "*", "+"].contains(&head)
        || head.chars().all(|c| c == '#')
        || (head.len() <= 4
            && head.ends_with(['.', ')'])
            && head[..head.len() - 1].chars().all(|c| c.is_ascii_digit()))
}

/// True for screen text before a word that only opens its line: spaces, a bullet,
/// or a list number.
fn is_screen_marker(head: &str) -> bool {
    let body = head.trim_start_matches(' ');
    body.trim().is_empty() || list_marker_len(body) == body.len() || body.trim() == "●"
}

/// Closes a code span or a bold run the selection cut open. The text before the cut,
/// on the same line, says which side the cut fell on.
fn balance(md: &str, text: &str, a: usize) -> String {
    let line_start = text[..a].rfind('\n').map_or(0, |i| i + 1);
    let mut md = md.to_string();
    for mark in ["**", "`"] {
        if md.matches(mark).count() % 2 == 1 {
            if text[line_start..a].matches(mark).count() % 2 == 1 {
                md.insert_str(0, mark);
            } else {
                md.push_str(mark);
            }
        }
    }
    md
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    fn transcript(answers: &[&str]) -> String {
        answers
            .iter()
            .map(|a| {
                serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "text", "text": a}]}
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    const ANSWER: &str = "Pronto: o **git worktree prune** limpou o registro dessa worktree. A branch não está mais presa a nenhuma worktree, e agora o delete funciona:\n\n- Use `-D`, e não `-d`.\n- O repositório base está na main local.";

    fn index() -> TranscriptIndex {
        TranscriptIndex::new(&transcript(&[ANSWER]))
    }

    #[test]
    fn a_wrapped_answer_comes_back_as_its_markdown() {
        let screen = "  ● Pronto: o git worktree prune limpou o registro dessa worktree. A branch não está\n    mais presa a nenhuma worktree, e agora o delete funciona:";
        let got = rebuild(screen, "", &index(), 90);
        assert!(got.markdown);
        assert_eq!(
            got.text,
            "Pronto: o **git worktree prune** limpou o registro dessa worktree. A branch não está mais presa a nenhuma worktree, e agora o delete funciona:"
        );
    }

    #[test]
    fn a_selection_cut_mid_word_keeps_only_what_was_selected() {
        let got = rebuild("tree prune limpou o regis", "", &index(), 90);
        assert_eq!(got.text, "**tree prune** limpou o regis");
        let got = rebuild("orktree prune limpou", "", &index(), 90);
        assert_eq!(
            got.text, "**orktree prune** limpou",
            "the bold the cut opened is closed"
        );
    }

    #[test]
    fn a_selection_from_a_list_bullet_takes_the_markdown_marker() {
        let screen = "  - Use -D, e não -d.\n  - O repositório base está na main local.";
        let got = rebuild(screen, "", &index(), 90);
        assert_eq!(
            got.text,
            "- Use `-D`, e não `-d`.\n- O repositório base está na main local."
        );
    }

    #[test]
    fn a_run_that_starts_a_paragraph_mid_row_keeps_the_space_before_it() {
        let index = TranscriptIndex::new(&transcript(&[
            "As rotas novas não existem na v2.48.0.",
            "A API de volta à v2.48.0 resolve.",
        ]));
        let got = rebuild("não existem na v2. A API de volta", "", &index, 90);
        assert_eq!(got.text, "não existem na v2. A API de volta");
    }

    #[test]
    fn a_word_cut_mid_way_takes_no_marker_from_inside_it() {
        let index = TranscriptIndex::new(&transcript(&[
            "O PDF em `pague_menos_politica.pdf` caiu, e o outro também.",
        ]));
        let got = rebuild("menos_politica.pdf caiu, e o outro", "", &index, 90);
        assert_eq!(got.text, "`menos_politica.pdf` caiu, e o outro");
    }

    #[test]
    fn a_selection_that_starts_at_the_word_takes_no_list_number() {
        let index = TranscriptIndex::new(&transcript(&["1. Exclusão de conta: fora do ar."]));
        let got = rebuild("Exclusão de conta: fora", "", &index, 90);
        assert_eq!(got.text, "Exclusão de conta: fora");
        let got = rebuild("  1. Exclusão de conta: fora", "", &index, 90);
        assert_eq!(
            got.text, "1. Exclusão de conta: fora",
            "a selected number is kept"
        );
    }

    #[test]
    fn words_the_transcript_lacks_come_from_the_screen() {
        let screen = "funciona:\n\n  ✻ Worked for 22s · done Thursday 2:57 PM";
        let got = rebuild(screen, "", &index(), 90);
        assert_eq!(got.text, screen, "one word is too short a run to trust");
        let screen = "o delete funciona:\n\n  ✻ Worked for 22s";
        let got = rebuild(screen, "", &index(), 90);
        assert_eq!(got.text, "o delete funciona:\n\n  ✻ Worked for 22s");
    }

    #[test]
    fn the_screen_context_picks_between_two_equal_runs() {
        let index = TranscriptIndex::new(&transcript(&[
            "Primeiro: rode os testes agora.",
            "Depois: rode os **testes** agora.",
        ]));
        assert_eq!(
            rebuild("rode os testes", "Depois:", &index, 90).text,
            "rode os **testes**"
        );
        assert_eq!(
            rebuild("rode os testes", "Primeiro:", &index, 90).text,
            "rode os testes"
        );
    }

    #[test]
    fn a_prompt_counts_as_a_source_too() {
        let line = serde_json::json!({"type": "user", "message": {"content": "deleta as **pastas** antigas"}});
        let index = TranscriptIndex::new(&line.to_string());
        let got = rebuild("❯ deleta as pastas antigas", "", &index, 90);
        assert_eq!(got.text, "❯ deleta as **pastas** antigas");
    }

    #[test]
    fn a_line_that_met_the_edge_joins_the_line_above() {
        let screen = "  Isso já gerou pares de contatos aprovados para a\n  mesma pessoa.";
        assert_eq!(
            rejoin(screen, 52),
            "  Isso já gerou pares de contatos aprovados para a mesma pessoa."
        );
    }

    #[test]
    fn an_answer_bullet_counts_as_one_column() {
        let screen = "  ● Pronto: o registro dessa worktree saiu. A branch\n    está livre.";
        assert_eq!(
            rejoin(screen, 56),
            "  ● Pronto: o registro dessa worktree saiu. A branch está livre."
        );
    }

    #[test]
    fn a_short_line_keeps_its_break() {
        let screen = "  Pendente:\n  o link de exclusão.";
        assert_eq!(rejoin(screen, 52), screen);
    }

    #[test]
    fn a_new_list_item_keeps_its_break() {
        let screen = "  - um item que chega perto da borda do painel ok\n  - outro item";
        assert_eq!(rejoin(screen, 52), screen);
    }

    #[test]
    fn a_list_item_continues_at_its_text_indent() {
        let screen = "  - Dados perdidos: o telefone e o e-mail do\n    cartão foram apagados.";
        assert_eq!(
            rejoin(screen, 46),
            "  - Dados perdidos: o telefone e o e-mail do cartão foram apagados."
        );
    }

    #[test]
    fn a_table_keeps_its_rows() {
        let screen = "  │ GET /v2/offers/detail/1 │ 401, porque as rotas │\n  │                         │ exigem token.        │";
        assert_eq!(rejoin(screen, 56), screen);
    }
}
