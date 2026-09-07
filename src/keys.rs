//! Key translation. Pure functions, so every rule here is unit tested.
//!
//! Two rules govern this module:
//! 1. The host reserves as few keys as possible. Everything else belongs to the child.
//! 2. `Alt` is the only leader. No function key is reserved. The one exception is
//!    `Shift+PageUp` and `Shift+PageDown`, which every terminal reserves for its own
//!    scrollback, and which Claude Code does not use.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the host does with a key that it handles itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAction {
    Quit,
    Focus(usize),
    FocusShell,
    NewSession,
    TogglePause,
    DeleteSession,
    RenameSession,
    FindSession,
    ShowHelp,
    ScrollUp,
    ScrollDown,
    NerdMode,
}

/// Returns the host action for a key, or `None` when the key belongs to the session.
///
/// A shifted letter arrives as the uppercase character on every path, and the kitty
/// protocol adds the shift modifier as well. Both forms are accepted, and the bare
/// lowercase letter is left to the child.
#[must_use]
pub fn host_action(k: &KeyEvent) -> Option<HostAction> {
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('q') {
        return Some(HostAction::Quit);
    }
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    // Scrollback keeps the binding every terminal already uses for it. Plain
    // `PageUp` and `PageDown` still belong to the child.
    if shift {
        match k.code {
            KeyCode::PageUp => return Some(HostAction::ScrollUp),
            KeyCode::PageDown => return Some(HostAction::ScrollDown),
            _ => {}
        }
    }
    if !k.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    match k.code {
        KeyCode::Char('0') => Some(HostAction::FocusShell),
        KeyCode::Char(c @ '1'..='9') => Some(HostAction::Focus(c as usize - '1' as usize)),
        KeyCode::Char('N') => Some(HostAction::NewSession),
        KeyCode::Char('n') if shift => Some(HostAction::NewSession),
        KeyCode::Char('P') => Some(HostAction::TogglePause),
        KeyCode::Char('p') if shift => Some(HostAction::TogglePause),
        KeyCode::Char('X') => Some(HostAction::DeleteSession),
        KeyCode::Char('x') if shift => Some(HostAction::DeleteSession),
        KeyCode::Char('R') => Some(HostAction::RenameSession),
        KeyCode::Char('r') if shift => Some(HostAction::RenameSession),
        KeyCode::Char('F') => Some(HostAction::FindSession),
        KeyCode::Char('f') if shift => Some(HostAction::FindSession),
        KeyCode::Char('H') => Some(HostAction::ShowHelp),
        KeyCode::Char('h') if shift => Some(HostAction::ShowHelp),
        KeyCode::Char('D') => Some(HostAction::NerdMode),
        KeyCode::Char('d') if shift => Some(HostAction::NerdMode),
        _ => None,
    }
}

/// Translates a key event into the bytes a terminal sends to a child process.
#[must_use]
pub fn encode(k: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);

    let base: Vec<u8> = match k.code {
        KeyCode::Char(c) if ctrl => vec![(c.to_ascii_lowercase() as u8) & 0x1f],
        KeyCode::Char(c) => {
            let mut b = [0u8; 4];
            c.encode_utf8(&mut b).as_bytes().to_vec()
        }
        // Shift+Enter carries no legacy encoding of its own, so a plain terminal
        // collapses it onto Enter and the prompt submits. Claude Code's own
        // `/terminal-setup` binds it to ESC then CR, checked on 2026-09-07 against
        // the CLI version in FINDINGS.md, so that is what culm sends. With Alt also
        // held the arm below adds the same prefix, so the two agree.
        KeyCode::Enter if shift && !alt => vec![0x1b, b'\r'],
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => arrow(b'D', shift, ctrl),
        KeyCode::Right => arrow(b'C', shift, ctrl),
        KeyCode::Up => arrow(b'A', shift, ctrl),
        KeyCode::Down => arrow(b'B', shift, ctrl),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        _ => return None,
    };

    if alt && !ctrl {
        let mut v = vec![0x1b];
        v.extend_from_slice(&base);
        return Some(v);
    }
    Some(base)
}

/// How a child asked for mouse events to be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncoding {
    /// The original single-byte encoding. Breaks past column 223.
    Default,
    /// The SGR encoding, which carries any coordinate.
    Sgr,
}

/// Wheel up and wheel down, as xterm numbers them.
const WHEEL_UP: u8 = 64;
const WHEEL_DOWN: u8 = 65;

/// Encodes one wheel notch for a child that asked for mouse reporting.
///
/// `column` and `row` are one-based, counted inside the panel, as a terminal
/// reports them.
#[must_use]
pub fn encode_wheel(up: bool, column: u16, row: u16, encoding: MouseEncoding) -> Vec<u8> {
    let button = if up { WHEEL_UP } else { WHEEL_DOWN };
    match encoding {
        MouseEncoding::Sgr => format!("\x1b[<{button};{column};{row}M").into_bytes(),
        MouseEncoding::Default => {
            // Every field is offset by 32, and one byte holds each, so a coordinate
            // past 223 has no encoding and the event is dropped by the child.
            let mut v = b"\x1b[M".to_vec();
            v.push(32 + button);
            v.push(u8::try_from(column.saturating_add(32)).unwrap_or(u8::MAX));
            v.push(u8::try_from(row.saturating_add(32)).unwrap_or(u8::MAX));
            v
        }
    }
}

/// Wraps pasted text so the child receives it as one block, not as many keystrokes.
#[must_use]
pub fn encode_paste(text: &str) -> Vec<u8> {
    let mut v = b"\x1b[200~".to_vec();
    v.extend_from_slice(text.as_bytes());
    v.extend_from_slice(b"\x1b[201~");
    v
}

fn arrow(letter: u8, shift: bool, ctrl: bool) -> Vec<u8> {
    let m = match (shift, ctrl) {
        (false, false) => return vec![0x1b, b'[', letter],
        (true, false) => b'2',
        (false, true) => b'5',
        (true, true) => b'6',
    };
    vec![0x1b, b'[', b'1', b';', m, letter]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn shift_tab_becomes_csi_z() {
        let bytes = encode(&key(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(bytes, Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn control_letter_becomes_a_control_byte() {
        let bytes = encode(&key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(bytes, Some(vec![0x12]));
    }

    #[test]
    fn shift_enter_inserts_a_newline_instead_of_submitting() {
        assert_eq!(
            encode(&key(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(vec![0x1b, b'\r']),
            "Claude Code binds shift+enter to ESC then CR in its own terminal setup"
        );
    }

    #[test]
    fn plain_enter_still_submits() {
        assert_eq!(
            encode(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(vec![b'\r'])
        );
    }

    #[test]
    fn alt_enter_and_shift_enter_agree() {
        let alt = encode(&key(KeyCode::Enter, KeyModifiers::ALT));
        let shift = encode(&key(KeyCode::Enter, KeyModifiers::SHIFT));
        let both = encode(&key(
            KeyCode::Enter,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(alt, shift, "both mean a newline that does not submit");
        assert_eq!(both, shift, "holding both never doubles the prefix");
    }

    #[test]
    fn shift_enter_is_not_a_host_action() {
        assert_eq!(
            host_action(&key(KeyCode::Enter, KeyModifiers::SHIFT)),
            None,
            "the key belongs to the child"
        );
    }

    #[test]
    fn escape_reaches_the_child_unchanged() {
        assert_eq!(
            encode(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(vec![0x1b])
        );
    }

    #[test]
    fn paste_is_bracketed() {
        let bytes = encode_paste("a\nb");
        assert_eq!(bytes, b"\x1b[200~a\nb\x1b[201~".to_vec());
    }

    #[test]
    fn alt_digit_focuses_a_session() {
        let action = host_action(&key(KeyCode::Char('2'), KeyModifiers::ALT));
        assert_eq!(action, Some(HostAction::Focus(1)));
    }

    #[test]
    fn control_q_quits() {
        let action = host_action(&key(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert_eq!(action, Some(HostAction::Quit));
    }

    #[test]
    fn alt_shift_n_creates_a_session_on_both_keyboard_paths() {
        let legacy = host_action(&key(KeyCode::Char('N'), KeyModifiers::ALT));
        let kitty = host_action(&key(
            KeyCode::Char('n'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(legacy, Some(HostAction::NewSession));
        assert_eq!(kitty, Some(HostAction::NewSession));
    }

    #[test]
    fn alt_shift_p_toggles_the_pause_on_both_keyboard_paths() {
        let legacy = host_action(&key(KeyCode::Char('P'), KeyModifiers::ALT));
        let kitty = host_action(&key(
            KeyCode::Char('p'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(legacy, Some(HostAction::TogglePause));
        assert_eq!(kitty, Some(HostAction::TogglePause));
    }

    #[test]
    fn alt_shift_x_deletes_a_session_on_both_keyboard_paths() {
        let legacy = host_action(&key(KeyCode::Char('X'), KeyModifiers::ALT));
        let kitty = host_action(&key(
            KeyCode::Char('x'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(legacy, Some(HostAction::DeleteSession));
        assert_eq!(kitty, Some(HostAction::DeleteSession));
    }

    #[test]
    fn alt_shift_r_renames_on_both_keyboard_paths() {
        let legacy = host_action(&key(KeyCode::Char('R'), KeyModifiers::ALT));
        let kitty = host_action(&key(
            KeyCode::Char('r'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(legacy, Some(HostAction::RenameSession));
        assert_eq!(kitty, Some(HostAction::RenameSession));
    }

    #[test]
    fn alt_shift_f_finds_on_both_keyboard_paths() {
        let legacy = host_action(&key(KeyCode::Char('F'), KeyModifiers::ALT));
        let kitty = host_action(&key(
            KeyCode::Char('f'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(legacy, Some(HostAction::FindSession));
        assert_eq!(kitty, Some(HostAction::FindSession));
    }

    #[test]
    fn alt_shift_h_opens_the_help_on_both_keyboard_paths() {
        let legacy = host_action(&key(KeyCode::Char('H'), KeyModifiers::ALT));
        let kitty = host_action(&key(
            KeyCode::Char('h'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(legacy, Some(HostAction::ShowHelp));
        assert_eq!(kitty, Some(HostAction::ShowHelp));
    }

    #[test]
    fn a_bare_alt_letter_belongs_to_the_session() {
        assert_eq!(
            host_action(&key(KeyCode::Char('n'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            host_action(&key(KeyCode::Char('p'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            host_action(&key(KeyCode::Char('x'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            host_action(&key(KeyCode::Char('r'), KeyModifiers::ALT)),
            None,
            "Alt+r is the readline yank, and it belongs to the child"
        );
        assert_eq!(
            host_action(&key(KeyCode::Char('f'), KeyModifiers::ALT)),
            None,
            "Alt+f moves a word forward, and it belongs to the child"
        );
        assert_eq!(
            host_action(&key(KeyCode::Char('h'), KeyModifiers::ALT)),
            None,
            "Alt+h is the readline backward delete, and it belongs to the child"
        );
    }

    #[test]
    fn alt_zero_reaches_the_shell() {
        assert_eq!(
            host_action(&key(KeyCode::Char('0'), KeyModifiers::ALT)),
            Some(HostAction::FocusShell)
        );
    }

    #[test]
    fn a_bare_zero_belongs_to_the_session() {
        assert_eq!(
            host_action(&key(KeyCode::Char('0'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn a_wheel_notch_is_encoded_for_a_child_that_asked_for_it() {
        assert_eq!(
            encode_wheel(true, 5, 3, MouseEncoding::Sgr),
            b"\x1b[<64;5;3M".to_vec()
        );
        assert_eq!(
            encode_wheel(false, 5, 3, MouseEncoding::Sgr),
            b"\x1b[<65;5;3M".to_vec()
        );
    }

    #[test]
    fn the_default_encoding_offsets_every_field_by_thirty_two() {
        assert_eq!(
            encode_wheel(true, 1, 1, MouseEncoding::Default),
            vec![0x1b, b'[', b'M', 96, 33, 33]
        );
    }

    #[test]
    fn shift_page_keys_scroll_the_session() {
        assert_eq!(
            host_action(&key(KeyCode::PageUp, KeyModifiers::SHIFT)),
            Some(HostAction::ScrollUp)
        );
        assert_eq!(
            host_action(&key(KeyCode::PageDown, KeyModifiers::SHIFT)),
            Some(HostAction::ScrollDown)
        );
    }

    #[test]
    fn a_plain_page_key_belongs_to_the_session() {
        assert_eq!(host_action(&key(KeyCode::PageUp, KeyModifiers::NONE)), None);
        assert_eq!(
            host_action(&key(KeyCode::PageDown, KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            encode(&key(KeyCode::PageUp, KeyModifiers::NONE)),
            Some(b"\x1b[5~".to_vec())
        );
    }

    #[test]
    fn no_function_key_is_reserved() {
        for n in 1..=12 {
            assert_eq!(host_action(&key(KeyCode::F(n), KeyModifiers::NONE)), None);
        }
    }

    #[test]
    fn an_ordinary_key_belongs_to_the_session() {
        assert_eq!(
            host_action(&key(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
    }
}
