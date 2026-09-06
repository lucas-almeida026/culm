//! Key translation. Pure functions, so every rule here is unit tested.
//!
//! Two rules govern this module:
//! 1. The host reserves as few keys as possible. Everything else belongs to the child.
//! 2. `Alt` is the only leader. No function key is reserved.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the host does with a key that it handles itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAction {
    Quit,
    Focus(usize),
    FocusShell,
    NewSession,
    TogglePause,
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
    if !k.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char('0') => Some(HostAction::FocusShell),
        KeyCode::Char(c @ '1'..='9') => Some(HostAction::Focus(c as usize - '1' as usize)),
        KeyCode::Char('N') => Some(HostAction::NewSession),
        KeyCode::Char('n') if shift => Some(HostAction::NewSession),
        KeyCode::Char('P') => Some(HostAction::TogglePause),
        KeyCode::Char('p') if shift => Some(HostAction::TogglePause),
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
    fn a_bare_alt_letter_belongs_to_the_session() {
        assert_eq!(
            host_action(&key(KeyCode::Char('n'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            host_action(&key(KeyCode::Char('p'), KeyModifiers::ALT)),
            None
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
