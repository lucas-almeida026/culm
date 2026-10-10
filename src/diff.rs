//! What the changes panel shows: git's reports, parsed, and two versions of a file
//! laid side by side. Pure, so every rule here is tested on the bytes git prints.

use std::ops::Range;

use ratatui::style::Color;
use similar::{ChangeTag, DiffOp, TextDiff};

/// The largest file the panel diffs. A larger file is named, not shown, because the
/// diff and the colors of a huge file would stall the frame.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;

/// The bytes git reads to decide a file is binary. The same rule git uses.
const BINARY_PROBE: usize = 8000;

/// Lines of unchanged text kept around each change.
pub const CONTEXT_LINES: usize = 3;

/// A tab stop in the diff. A tab is expanded before the diff, so every column the
/// panel counts is a real column.
const TAB: &str = "    ";

/// How a file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    /// A file git does not track yet. It reads as added.
    Untracked,
    /// A file with a merge conflict.
    Conflicted,
}

impl ChangeKind {
    /// The mark in front of the path. One column, so the paths stay aligned.
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            ChangeKind::Added | ChangeKind::Untracked => "+",
            ChangeKind::Modified => "•",
            ChangeKind::Deleted => "-",
            ChangeKind::Renamed => "→",
            ChangeKind::Conflicted => "!",
        }
    }

    /// True when the file has no earlier version to compare with.
    #[must_use]
    pub fn is_new(self) -> bool {
        matches!(self, ChangeKind::Added | ChangeKind::Untracked)
    }
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub kind: ChangeKind,
    pub path: String,
    /// The path before a rename.
    pub old_path: Option<String>,
}

impl FileChange {
    /// The path of the earlier version.
    #[must_use]
    pub fn before(&self) -> &str {
        self.old_path.as_deref().unwrap_or(&self.path)
    }
}

/// One commit of the history list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub author: String,
    /// Unix seconds of the author date.
    pub time: u64,
    pub subject: String,
}

/// Parses `git status --porcelain=v2 -z --untracked-files=all`.
///
/// The changes are what the working tree holds against `HEAD`, staged or not,
/// because culm never stages and the user asked what the session changed.
#[must_use]
pub fn parse_status(out: &[u8]) -> Vec<FileChange> {
    let text = String::from_utf8_lossy(out);
    let mut tokens = text.split('\0');
    let mut changes = Vec::new();
    while let Some(token) = tokens.next() {
        let kind_of = |xy: &str| -> Option<ChangeKind> {
            let mut chars = xy.chars();
            let x = chars.next().unwrap_or('.');
            let y = chars.next().unwrap_or('.');
            match (x, y) {
                // Added to the index, then deleted from the disk: nothing against HEAD.
                ('A', 'D') => None,
                ('A', _) => Some(ChangeKind::Added),
                ('D', _) | (_, 'D') => Some(ChangeKind::Deleted),
                _ => Some(ChangeKind::Modified),
            }
        };
        if let Some(rest) = token.strip_prefix("1 ") {
            // XY sub mH mI mW hH hI path. The path is last and may hold spaces.
            let fields: Vec<&str> = rest.splitn(8, ' ').collect();
            if let (Some(xy), Some(path)) = (fields.first(), fields.get(7))
                && let Some(kind) = kind_of(xy)
            {
                changes.push(FileChange {
                    kind,
                    path: (*path).to_string(),
                    old_path: None,
                });
            }
        } else if let Some(rest) = token.strip_prefix("2 ") {
            // XY sub mH mI mW hH hI Xscore path, then the original path as a token.
            let fields: Vec<&str> = rest.splitn(9, ' ').collect();
            let old = tokens.next().map(str::to_string);
            if let Some(path) = fields.get(8) {
                changes.push(FileChange {
                    kind: ChangeKind::Renamed,
                    path: (*path).to_string(),
                    old_path: old,
                });
            }
        } else if let Some(rest) = token.strip_prefix("u ") {
            let fields: Vec<&str> = rest.splitn(10, ' ').collect();
            if let Some(path) = fields.get(9) {
                changes.push(FileChange {
                    kind: ChangeKind::Conflicted,
                    path: (*path).to_string(),
                    old_path: None,
                });
            }
        } else if let Some(path) = token.strip_prefix("? ") {
            changes.push(FileChange {
                kind: ChangeKind::Untracked,
                path: path.to_string(),
                old_path: None,
            });
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes
}

/// Parses `git log -z` in the format of `crate::git::LOG_FORMAT`.
#[must_use]
pub fn parse_log(out: &[u8]) -> Vec<Commit> {
    String::from_utf8_lossy(out)
        .split('\0')
        .filter_map(|record| {
            let record = record.trim_matches('\n');
            let mut fields = record.splitn(5, '\x1f');
            let sha = fields.next()?.to_string();
            let short = fields.next()?.to_string();
            let author = fields.next()?.to_string();
            let time = fields.next()?.parse().unwrap_or(0);
            let subject = fields.next()?.to_string();
            (!sha.is_empty()).then_some(Commit {
                sha,
                short,
                author,
                time,
                subject,
            })
        })
        .collect()
}

/// Parses `--name-status -z`: a status token, then one path, or two for a rename
/// or a copy.
#[must_use]
pub fn parse_name_status(out: &[u8]) -> Vec<FileChange> {
    let text = String::from_utf8_lossy(out);
    let mut tokens = text
        .split('\0')
        .map(|t| t.trim_matches('\n'))
        .filter(|t| !t.is_empty());
    let mut changes = Vec::new();
    while let Some(status) = tokens.next() {
        let Some(letter) = status.chars().next() else {
            continue;
        };
        let change = match letter {
            'R' | 'C' => {
                let old = tokens.next().map(str::to_string);
                let Some(path) = tokens.next() else { break };
                FileChange {
                    kind: if letter == 'R' {
                        ChangeKind::Renamed
                    } else {
                        ChangeKind::Added
                    },
                    path: path.to_string(),
                    old_path: if letter == 'R' { old } else { None },
                }
            }
            _ => {
                let Some(path) = tokens.next() else { break };
                FileChange {
                    kind: match letter {
                        'A' => ChangeKind::Added,
                        'D' => ChangeKind::Deleted,
                        'U' => ChangeKind::Conflicted,
                        _ => ChangeKind::Modified,
                    },
                    path: path.to_string(),
                    old_path: None,
                }
            }
        };
        changes.push(change);
    }
    changes
}

/// How long ago a commit was made, in the words the history list shows.
#[must_use]
pub fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    let (count, unit) = match secs {
        0..60 => return "just now".into(),
        60..3_600 => (secs / 60, "minute"),
        3_600..86_400 => (secs / 3_600, "hour"),
        86_400..2_592_000 => (secs / 86_400, "day"),
        2_592_000..31_536_000 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} ago")
}

/// One side of a diff, ready to compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Binary,
    TooLarge,
}

/// Turns the bytes of one side into text. A missing side is empty text, so a new
/// file diffs against nothing. Tabs are expanded and carriage returns dropped.
#[must_use]
pub fn decode(bytes: Option<&[u8]>) -> Content {
    let Some(bytes) = bytes else {
        return Content::Text(String::new());
    };
    if bytes.len() > MAX_FILE_BYTES {
        return Content::TooLarge;
    }
    if bytes.iter().take(BINARY_PROBE).any(|&b| b == 0) {
        return Content::Binary;
    }
    let text = String::from_utf8_lossy(bytes);
    Content::Text(text.replace('\t', TAB).replace('\r', ""))
}

/// The text lines of a file, without their line ends.
#[must_use]
pub fn lines_of(text: &str) -> Vec<&str> {
    text.split_inclusive('\n')
        .map(|l| l.strip_suffix('\n').unwrap_or(l))
        .collect()
}

/// Whether a line is the same on both sides, gone, or new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Removed,
    Added,
}

/// One line on one side of the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    /// The index of the line in its version, from zero.
    pub line: usize,
    pub kind: LineKind,
    /// The bytes of the line that changed against the line beside it. Empty when the
    /// two lines are too different for a word to matter.
    pub emphasis: Vec<Range<usize>>,
}

/// One row of the side-by-side view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The start of a block of changes, as git writes it: `@@ -27,7 +27,7 @@`.
    Hunk(String),
    /// The old line on the left, the new line on the right. A side is `None` where
    /// the other side has a line with no partner.
    Pair {
        left: Option<Side>,
        right: Option<Side>,
    },
}

/// Lays two versions side by side, with `context` unchanged lines around each
/// block of changes.
///
/// A block that removes some lines and adds others pairs them row by row, as
/// GitHub Desktop does, and the words that differ between a pair are emphasised.
#[must_use]
pub fn side_by_side(old: &str, new: &str, context: usize) -> Vec<Row> {
    let old_lines = lines_of(old);
    let new_lines = lines_of(new);
    let diff = TextDiff::from_slices(&old_lines, &new_lines);
    let mut rows = Vec::new();
    for group in diff.grouped_ops(context) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        rows.push(Row::Hunk(format!(
            "@@ -{} +{} @@",
            hunk_range(&old_range),
            hunk_range(&new_range)
        )));
        let mut ops = group.iter().peekable();
        while let Some(op) = ops.next() {
            match *op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } => {
                    for i in 0..len {
                        rows.push(Row::Pair {
                            left: Some(plain(old_index + i, LineKind::Context)),
                            right: Some(plain(new_index + i, LineKind::Context)),
                        });
                    }
                }
                DiffOp::Delete {
                    old_index, old_len, ..
                } => {
                    // A removal followed at once by an addition is a replacement.
                    if let Some(&&DiffOp::Insert {
                        new_index, new_len, ..
                    }) = ops.peek()
                    {
                        ops.next();
                        pair_block(
                            &mut rows,
                            (old_index, old_len),
                            (new_index, new_len),
                            &old_lines,
                            &new_lines,
                        );
                    } else {
                        for i in 0..old_len {
                            rows.push(Row::Pair {
                                left: Some(plain(old_index + i, LineKind::Removed)),
                                right: None,
                            });
                        }
                    }
                }
                DiffOp::Insert {
                    new_index, new_len, ..
                } => {
                    for i in 0..new_len {
                        rows.push(Row::Pair {
                            left: None,
                            right: Some(plain(new_index + i, LineKind::Added)),
                        });
                    }
                }
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => pair_block(
                    &mut rows,
                    (old_index, old_len),
                    (new_index, new_len),
                    &old_lines,
                    &new_lines,
                ),
            }
        }
    }
    rows
}

/// A hunk range as git writes it: the first line from one, then the count. An
/// empty range names the line before it, so a new file reads `-0,0`.
fn hunk_range(range: &Range<usize>) -> String {
    let len = range.end - range.start;
    let start = if len == 0 {
        range.start
    } else {
        range.start + 1
    };
    format!("{start},{len}")
}

fn plain(line: usize, kind: LineKind) -> Side {
    Side {
        line,
        kind,
        emphasis: Vec::new(),
    }
}

/// Pairs removed lines with added lines, row by row.
fn pair_block(
    rows: &mut Vec<Row>,
    (old_index, old_len): (usize, usize),
    (new_index, new_len): (usize, usize),
    old_lines: &[&str],
    new_lines: &[&str],
) {
    for i in 0..old_len.max(new_len) {
        let mut left = (i < old_len).then(|| plain(old_index + i, LineKind::Removed));
        let mut right = (i < new_len).then(|| plain(new_index + i, LineKind::Added));
        if let (Some(l), Some(r)) = (left.as_mut(), right.as_mut()) {
            let a = old_lines.get(l.line).copied().unwrap_or("");
            let b = new_lines.get(r.line).copied().unwrap_or("");
            if let Some((ea, eb)) = word_emphasis(a, b) {
                l.emphasis = ea;
                r.emphasis = eb;
            }
        }
        rows.push(Row::Pair { left, right });
    }
}

/// The share of a pair, in percent, that must stay the same for the changed words
/// to be marked. Below it the two lines are unrelated, and marking every word of
/// both says nothing the line color does not.
const SAME_SHARE_PERCENT: usize = 40;

/// The changed bytes of one line.
pub type Emphasis = Vec<Range<usize>>;

/// The byte ranges that differ between two lines, word by word.
#[must_use]
pub fn word_emphasis(a: &str, b: &str) -> Option<(Emphasis, Emphasis)> {
    let diff = TextDiff::from_words(a, b);
    let (mut at_a, mut at_b, mut same) = (0usize, 0usize, 0usize);
    let (mut marks_a, mut marks_b) = (Vec::new(), Vec::new());
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                at_a += len;
                at_b += len;
                same += len;
            }
            ChangeTag::Delete => {
                push_range(&mut marks_a, at_a..at_a + len);
                at_a += len;
            }
            ChangeTag::Insert => {
                push_range(&mut marks_b, at_b..at_b + len);
                at_b += len;
            }
        }
    }
    let longest = a.len().max(b.len());
    if longest == 0 || same * 100 < longest * SAME_SHARE_PERCENT {
        return None;
    }
    Some((marks_a, marks_b))
}

/// Appends a range, merging it into the last one when the two touch.
fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if let Some(last) = ranges.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    ranges.push(range);
}

/// The colors of one line: a foreground color for each byte range.
pub type LineColors = Vec<(Color, Range<usize>)>;

/// The largest file that gets syntax colors. Coloring reads the whole file, line by
/// line, and a larger file would hold the frame.
const MAX_COLORED_BYTES: usize = 256 * 1024;

/// Syntax colors, from the grammars and the themes `bat` ships.
pub struct Highlighter {
    syntaxes: syntect::parsing::SyntaxSet,
    theme: syntect::highlighting::Theme,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Highlighter")
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    /// Loads the grammars and the gruvbox theme. Costs a few milliseconds, so the
    /// panel builds one when it first opens and keeps it.
    #[must_use]
    pub fn new() -> Self {
        let themes = two_face::theme::extra();
        Self {
            syntaxes: two_face::syntax::extra_newlines(),
            theme: themes
                .get(two_face::theme::EmbeddedThemeName::GruvboxDark)
                .clone(),
        }
    }

    /// The colors of every line of `text`, which is the file at `path`. A file with
    /// no known grammar, or too large to color, gets no colors.
    #[must_use]
    pub fn colors(&self, path: &str, text: &str) -> Vec<LineColors> {
        use syntect::easy::HighlightLines;
        use syntect::util::LinesWithEndings;
        if text.len() > MAX_COLORED_BYTES {
            return Vec::new();
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        let extension = name.rsplit_once('.').map_or(name, |(_, ext)| ext);
        let syntax = self
            .syntaxes
            .find_syntax_by_extension(extension)
            .or_else(|| self.syntaxes.find_syntax_by_extension(name))
            .or_else(|| {
                text.lines()
                    .next()
                    .and_then(|first| self.syntaxes.find_syntax_by_first_line(first))
            });
        let Some(syntax) = syntax else {
            return Vec::new();
        };
        let mut lines = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(text) {
            let Ok(regions) = lines.highlight_line(line, &self.syntaxes) else {
                return Vec::new();
            };
            let content = line.trim_end_matches('\n').len();
            let mut at = 0usize;
            let mut colors = Vec::new();
            for (style, piece) in regions {
                let end = (at + piece.len()).min(content);
                if end > at {
                    let c = style.foreground;
                    colors.push((Color::Rgb(c.r, c.g, c.b), at..end));
                }
                at += piece.len();
            }
            out.push(colors);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn status_reads_every_kind_of_change_against_head() {
        let out = b"1 .M N... 100644 100644 100644 aaa aaa src/main.rs\0\
1 A. N... 000000 100644 100644 000 bbb new file.rs\0\
1 .D N... 100644 100644 000000 ccc ccc gone.rs\0\
2 R. N... 100644 100644 100644 ddd ddd R100 lib/after.rs\0lib/before.rs\0\
? notes.md\0";
        let changes = parse_status(out);
        let kinds: Vec<(ChangeKind, &str)> =
            changes.iter().map(|c| (c.kind, c.path.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (ChangeKind::Deleted, "gone.rs"),
                (ChangeKind::Renamed, "lib/after.rs"),
                (ChangeKind::Added, "new file.rs"),
                (ChangeKind::Untracked, "notes.md"),
                (ChangeKind::Modified, "src/main.rs"),
            ]
        );
        let renamed = changes
            .iter()
            .find(|c| c.kind == ChangeKind::Renamed)
            .expect("the rename is listed");
        assert_eq!(renamed.before(), "lib/before.rs");
    }

    #[test]
    fn a_file_added_then_deleted_is_no_change() {
        let out = b"1 AD N... 000000 100644 000000 000 bbb ghost.rs\0";
        assert!(parse_status(out).is_empty());
    }

    #[test]
    fn the_log_keeps_a_subject_with_separators_in_it() {
        let out = b"abc123\x1fabc\x1fAda\x1f1700000000\x1ffix: a | b\0\ndef456\x1fdef\x1fBo\x1f1600000000\x1ffeat: c\0";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "fix: a | b");
        assert_eq!(commits[1].sha, "def456");
        assert_eq!(commits[1].time, 1_600_000_000);
    }

    #[test]
    fn name_status_reads_renames_with_both_paths() {
        let out = b"\nM\0src/a.rs\0R087\0old.rs\0new.rs\0A\0b.rs\0D\0c.rs\0";
        let changes = parse_name_status(out);
        assert_eq!(changes.len(), 4);
        assert_eq!(changes[1].kind, ChangeKind::Renamed);
        assert_eq!(changes[1].before(), "old.rs");
        assert_eq!(changes[1].path, "new.rs");
        assert_eq!(changes[3].kind, ChangeKind::Deleted);
    }

    #[test]
    fn ago_reads_like_a_person_would_say_it() {
        assert_eq!(ago(1000, 1000), "just now");
        assert_eq!(ago(14 * 60, 0), "14 minutes ago");
        assert_eq!(ago(3_600, 0), "1 hour ago");
        assert_eq!(ago(3 * 86_400, 0), "3 days ago");
    }

    #[test]
    fn a_nul_byte_marks_a_binary_file() {
        assert_eq!(decode(Some(b"PNG\0\x01")), Content::Binary);
        assert_eq!(decode(None), Content::Text(String::new()));
        assert_eq!(decode(Some(b"a\tb\r\n")), Content::Text("a    b\n".into()));
    }

    #[test]
    fn a_changed_line_pairs_with_its_new_version_and_marks_the_word() {
        let old = "one\ntwo\nlet x = 1;\nfour\n";
        let new = "one\ntwo\nlet x = 2;\nfour\n";
        let rows = side_by_side(old, new, 1);
        assert_eq!(rows[0], Row::Hunk("@@ -2,3 +2,3 @@".into()));
        let Row::Pair {
            left: Some(left),
            right: Some(right),
        } = &rows[2]
        else {
            panic!("the change is a pair, got {:?}", rows[2]);
        };
        assert_eq!(left.kind, LineKind::Removed);
        assert_eq!(right.kind, LineKind::Added);
        // Words end at whitespace, so the mark covers the word the digit sits in.
        assert_eq!(&"let x = 1;"[left.emphasis[0].clone()], "1;");
        assert_eq!(&"let x = 2;"[right.emphasis[0].clone()], "2;");
        assert_eq!(rows.len(), 4, "a hunk, two lines of context, and the pair");
    }

    #[test]
    fn a_new_file_is_all_added_lines_on_the_right() {
        let rows = side_by_side("", "a\nb\n", 3);
        assert_eq!(rows[0], Row::Hunk("@@ -0,0 +1,2 @@".into()));
        assert!(rows[1..].iter().all(|r| matches!(
            r,
            Row::Pair {
                left: None,
                right: Some(Side {
                    kind: LineKind::Added,
                    ..
                })
            }
        )));
    }

    #[test]
    fn far_apart_changes_get_a_hunk_each() {
        let old: String = (0..30).map(|i| format!("{i}\n")).collect();
        let new: String = (0..30)
            .map(|i| match i {
                2 => "two\n".to_string(),
                27 => "twenty-seven\n".to_string(),
                _ => format!("{i}\n"),
            })
            .collect();
        let rows = side_by_side(&old, &new, 3);
        let hunks = rows.iter().filter(|r| matches!(r, Row::Hunk(_))).count();
        assert_eq!(hunks, 2);
    }

    #[test]
    fn unrelated_lines_mark_no_words() {
        assert_eq!(word_emphasis("fn main() {}", "// a comment here"), None);
    }

    #[test]
    fn rust_gets_colors_and_an_unknown_file_does_not() {
        let h = Highlighter::new();
        let colors = h.colors("src/main.rs", "fn main() {}\n");
        assert_eq!(colors.len(), 1);
        assert!(colors[0].len() > 1, "a keyword and a name differ in color");
        assert!(h.colors("blob.unknownext", "plain words\n").is_empty());
    }
}
