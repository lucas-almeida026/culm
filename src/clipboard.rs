//! The escape that puts text on the terminal's clipboard. Pure, so the bytes are
//! unit tested.
//!
//! culm holds the mouse, because the sidebar and the wheel need it, so the host
//! terminal never sees a drag over the panel and never copies for the user. OSC 52
//! hands the text back to the terminal, which owns the real clipboard.

/// The standard alphabet of RFC 4648.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 with padding, per RFC 4648. Hand written, because one escape does not
/// justify a dependency.
#[must_use]
pub fn base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let bytes = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let packed = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            // A chunk of one byte fills two characters, a chunk of two fills three,
            // and the rest is padding.
            if i <= chunk.len() {
                let index = ((packed >> shift) & 63) as usize;
                out.push(char::from(ALPHABET[index]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The OSC 52 escape that copies `text`.
///
/// The target is `cp`: the clipboard and the primary selection at once, so that
/// `Ctrl+V` in one window and a middle click in another both find the text.
#[must_use]
pub fn osc52(text: &str) -> Vec<u8> {
    let mut v = b"\x1b]52;cp;".to_vec();
    v.extend_from_slice(base64(text.as_bytes()).as_bytes());
    // BEL terminates the string. Every terminal that answers OSC 52 accepts it.
    v.push(0x07);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_carries_every_byte_value() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64(&all).len(), 344, "256 bytes fill 344 characters");
    }

    #[test]
    fn osc52_targets_the_clipboard_and_the_primary_selection() {
        assert_eq!(osc52("foo"), b"\x1b]52;cp;Zm9v\x07".to_vec());
    }
}
