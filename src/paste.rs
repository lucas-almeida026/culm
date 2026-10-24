//! Undo of the last paste in a Claude Code session.
//!
//! culm never sees the prompt Claude Code edits, so culm cannot remove a paste by
//! itself. Claude Code has its own undo, on `Ctrl+_`, and culm sends that. Claude
//! Code 2.1.310 merges edits that arrive within about one second of each other into
//! one undo step. A spike on 2026-10-24 measured the merge at a gap of 1.0 s and
//! none at 1.1 s. So culm holds a paste that follows other input too closely, and
//! the paste becomes an undo step of its own.
//!
//! A large paste shows as `[Pasted text #1 +N lines]`, and the same paste again
//! expands it. The expansion is a second undo step, so the undo of an expanded paste
//! sends two.

use std::collections::VecDeque;

/// The shortest gap culm keeps before a paste, in milliseconds.
pub const PASTE_GAP_MS: u64 = 1000;

/// Claude Code's undo, `Ctrl+_`.
pub const UNDO: &[u8] = b"\x1f";

/// One paste that waits for its gap.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    bytes: Vec<u8>,
    due: u64,
}

/// One paste the undo can take back.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Paste {
    /// The pasted text. `None` for `Ctrl+v`, which Claude Code fills from the
    /// clipboard itself.
    text: Option<String>,
    /// The undo steps the paste takes in Claude Code: two once expanded.
    steps: usize,
}

/// The input of one session, as far as the undo of a paste needs it.
#[derive(Debug, Clone, Default)]
pub struct PasteTrack {
    /// When the last bytes reached the session, in milliseconds.
    last_sent: Option<u64>,
    held: VecDeque<Held>,
    /// The pastes since the last key, oldest first.
    pastes: Vec<Paste>,
}

impl PasteTrack {
    /// A key goes out. Returns the held pastes, which go first to keep the order
    /// the user gave. The key ends the run of pastes the undo can take back.
    pub fn key(&mut self, now: u64) -> Vec<Vec<u8>> {
        self.last_sent = Some(now);
        self.pastes.clear();
        self.held.drain(..).map(|h| h.bytes).collect()
    }

    /// A paste. Returns the bytes when they can go at once, or holds them.
    pub fn paste(&mut self, text: Option<&str>, bytes: Vec<u8>, now: u64) -> Option<Vec<u8>> {
        match self.pastes.last_mut() {
            // The same text again expands the paste Claude Code collapsed. A
            // short paste is never collapsed, so a repeat inserts it twice, and
            // the undo then takes both. Claude Code's threshold is unknown here.
            Some(last) if text.is_some() && last.text.as_deref() == text && last.steps == 1 => {
                last.steps = 2;
            }
            _ => self.pastes.push(Paste {
                text: text.map(str::to_owned),
                steps: 1,
            }),
        }
        let after = self.held.back().map(|h| h.due).or(self.last_sent);
        let due = after
            .map_or(now, |t| t.saturating_add(PASTE_GAP_MS))
            .max(now);
        if self.held.is_empty() && due <= now {
            self.last_sent = Some(now);
            return Some(bytes);
        }
        self.held.push_back(Held { bytes, due });
        None
    }

    /// The held pastes whose gap has passed, in order.
    pub fn due(&mut self, now: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while self.held.front().is_some_and(|h| h.due <= now) {
            if let Some(held) = self.held.pop_front() {
                out.push(held.bytes);
                self.last_sent = Some(now);
            }
        }
        out
    }

    /// True while a paste waits.
    #[must_use]
    pub fn holds(&self) -> bool {
        !self.held.is_empty()
    }

    /// Takes back the last paste. Returns the undo bytes to send, which are empty
    /// when the paste was still held. `None` when the last edit was no paste.
    pub fn undo(&mut self) -> Option<Vec<u8>> {
        let paste = self.pastes.pop()?;
        // A held paste never reached the session, so it costs no undo step.
        let unsent = usize::from(self.held.pop_back().is_some());
        Some(UNDO.repeat(paste.steps.saturating_sub(unsent)))
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    fn bytes(text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }

    #[test]
    fn a_paste_after_a_pause_goes_at_once() {
        let mut track = PasteTrack::default();
        track.key(0);
        assert_eq!(track.paste(Some("a"), bytes("a"), 1000), Some(bytes("a")));
        assert!(!track.holds());
    }

    #[test]
    fn a_paste_right_after_a_key_waits_for_the_gap() {
        let mut track = PasteTrack::default();
        track.key(0);
        assert_eq!(track.paste(Some("a"), bytes("a"), 300), None);
        assert!(track.due(999).is_empty());
        assert_eq!(track.due(1000), vec![bytes("a")]);
    }

    #[test]
    fn two_quick_pastes_go_one_gap_apart() {
        let mut track = PasteTrack::default();
        assert_eq!(track.paste(Some("a"), bytes("a"), 0), Some(bytes("a")));
        assert_eq!(track.paste(Some("b"), bytes("b"), 100), None);
        assert_eq!(track.paste(Some("c"), bytes("c"), 200), None);
        assert_eq!(track.due(1000), vec![bytes("b")]);
        assert!(track.due(1999).is_empty());
        assert_eq!(track.due(2000), vec![bytes("c")]);
    }

    #[test]
    fn a_key_sends_the_held_pastes_first() {
        let mut track = PasteTrack::default();
        track.key(0);
        track.paste(Some("a"), bytes("a"), 100);
        assert_eq!(track.key(200), vec![bytes("a")]);
        assert!(!track.holds());
    }

    #[test]
    fn the_undo_of_a_sent_paste_is_one_step() {
        let mut track = PasteTrack::default();
        track.paste(Some("a"), bytes("a"), 0);
        assert_eq!(track.undo(), Some(UNDO.to_vec()));
        assert_eq!(track.undo(), None, "nothing else to take back");
    }

    #[test]
    fn the_undo_of_an_expanded_paste_is_two_steps() {
        let mut track = PasteTrack::default();
        track.paste(Some("big"), bytes("big"), 0);
        track.paste(Some("big"), bytes("big"), 5000);
        assert_eq!(track.undo(), Some(UNDO.repeat(2)));
    }

    #[test]
    fn the_undo_of_a_held_paste_sends_nothing() {
        let mut track = PasteTrack::default();
        track.key(0);
        track.paste(Some("a"), bytes("a"), 100);
        assert_eq!(track.undo(), Some(Vec::new()));
        assert!(!track.holds(), "the paste never goes");
    }

    #[test]
    fn the_undo_of_a_held_expansion_takes_back_the_first_paste() {
        let mut track = PasteTrack::default();
        track.paste(Some("big"), bytes("big"), 0);
        track.paste(Some("big"), bytes("big"), 100);
        assert_eq!(track.undo(), Some(UNDO.to_vec()));
        assert!(!track.holds());
    }

    #[test]
    fn pastes_come_back_one_per_undo_until_a_key() {
        let mut track = PasteTrack::default();
        track.paste(Some("a"), bytes("a"), 0);
        track.paste(Some("b"), bytes("b"), 5000);
        assert_eq!(track.undo(), Some(UNDO.to_vec()));
        assert_eq!(track.undo(), Some(UNDO.to_vec()));
        assert_eq!(track.undo(), None);
    }

    #[test]
    fn a_key_after_a_paste_leaves_nothing_to_undo() {
        let mut track = PasteTrack::default();
        track.paste(Some("a"), bytes("a"), 0);
        track.key(5000);
        assert_eq!(track.undo(), None);
    }

    #[test]
    fn ctrl_v_never_counts_as_an_expansion() {
        let mut track = PasteTrack::default();
        track.paste(None, bytes("\x16"), 0);
        track.paste(None, bytes("\x16"), 5000);
        assert_eq!(track.undo(), Some(UNDO.to_vec()));
        assert_eq!(track.undo(), Some(UNDO.to_vec()));
    }
}
