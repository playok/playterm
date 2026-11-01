//! Keyboard → VT byte sequence translation (xterm conventions).

use eframe::egui::{Key, Modifiers};

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(mods: Modifiers) -> u8 {
    1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl)
}

/// Cursor-style key (arrows, Home, End, F1-F4): `ESC [ X`, `ESC O X` or `ESC [ 1 ; m X`.
fn cursor_key(final_byte: u8, mods: Modifiers, app_cursor: bool) -> Vec<u8> {
    let m = modifier_param(mods);
    if m > 1 {
        format!("\x1b[1;{m}{}", final_byte as char).into_bytes()
    } else if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// F1-F4 always use SS3 when unmodified.
fn ss3_key(final_byte: u8, mods: Modifiers) -> Vec<u8> {
    cursor_key(final_byte, mods, true)
}

/// Tilde-style key: `ESC [ n ~` or `ESC [ n ; m ~`.
fn tilde_key(code: u8, mods: Modifiers) -> Vec<u8> {
    let m = modifier_param(mods);
    if m > 1 { format!("\x1b[{code};{m}~") } else { format!("\x1b[{code}~") }.into_bytes()
}

fn letter(key: Key) -> Option<u8> {
    let name = key.name();
    (name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase()).then(|| name.as_bytes()[0])
}

/// Bytes for a key press, or `None` if the key produces no terminal input by itself
/// (printable characters arrive separately as text events).
pub fn key_bytes(key: Key, mods: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let bytes = match key {
        Key::Enter => b"\r".to_vec(),
        Key::Backspace if mods.ctrl => vec![0x08],
        Key::Backspace => vec![0x7f],
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => vec![0x1b],

        Key::ArrowUp => cursor_key(b'A', mods, app_cursor),
        Key::ArrowDown => cursor_key(b'B', mods, app_cursor),
        Key::ArrowRight => cursor_key(b'C', mods, app_cursor),
        Key::ArrowLeft => cursor_key(b'D', mods, app_cursor),
        Key::Home => cursor_key(b'H', mods, app_cursor),
        Key::End => cursor_key(b'F', mods, app_cursor),

        Key::Insert => tilde_key(2, mods),
        Key::Delete => tilde_key(3, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),

        Key::F1 => ss3_key(b'P', mods),
        Key::F2 => ss3_key(b'Q', mods),
        Key::F3 => ss3_key(b'R', mods),
        Key::F4 => ss3_key(b'S', mods),
        Key::F5 => tilde_key(15, mods),
        Key::F6 => tilde_key(17, mods),
        Key::F7 => tilde_key(18, mods),
        Key::F8 => tilde_key(19, mods),
        Key::F9 => tilde_key(20, mods),
        Key::F10 => tilde_key(21, mods),
        Key::F11 => tilde_key(23, mods),
        Key::F12 => tilde_key(24, mods),

        _ if mods.ctrl && !mods.alt => return ctrl_key(key),
        _ if mods.alt && !mods.ctrl => {
            // Meta: ESC prefix. Only letters, so AltGr layouts and text input stay untouched.
            let l = letter(key)?;
            let c = if mods.shift { l } else { l.to_ascii_lowercase() };
            vec![0x1b, c]
        }
        _ => return None,
    };
    Some(bytes)
}

fn ctrl_key(key: Key) -> Option<Vec<u8>> {
    if let Some(l) = letter(key) {
        return Some(vec![l - b'A' + 1]);
    }
    let b = match key {
        Key::Space | Key::Num2 => 0x00,
        Key::OpenBracket | Key::Num3 => 0x1b,
        Key::Backslash | Key::Num4 => 0x1c,
        Key::CloseBracket | Key::Num5 => 0x1d,
        Key::Num6 => 0x1e,
        Key::Minus | Key::Num7 => 0x1f,
        Key::Slash => 0x1f,
        Key::Num8 => 0x7f,
        _ => return None,
    };
    Some(vec![b])
}

/// Prepares clipboard text for sending: newlines become CR (what Enter sends), and
/// bracketed-paste markers are added when the remote application asked for them.
pub fn paste_bytes(text: &str, bracketed: bool) -> String {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        // Pasted text must not end the paste early: an end marker in it (even one that only
        // forms once another is removed, "\x1b[20\x1b[201~1~") would let the rest run as typed
        // commands. Like xterm, no escape (ESC, or the 8-bit CSI) goes through at all.
        let inner: String = normalized.chars().filter(|&c| c != '\x1b' && c != '\u{9b}').collect();
        format!("\x1b[200~{inner}\x1b[201~")
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    #[test]
    fn arrows_follow_cursor_mode() {
        assert_eq!(key_bytes(Key::ArrowUp, NONE, false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, NONE, true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(Key::ArrowLeft, Modifiers::CTRL, true).unwrap(), b"\x1b[1;5D");
    }

    #[test]
    fn control_letters() {
        assert_eq!(key_bytes(Key::C, Modifiers::CTRL, false).unwrap(), vec![0x03]);
        assert_eq!(key_bytes(Key::Z, Modifiers::CTRL, false).unwrap(), vec![0x1a]);
        assert_eq!(key_bytes(Key::OpenBracket, Modifiers::CTRL, false).unwrap(), vec![0x1b]);
        assert_eq!(key_bytes(Key::Space, Modifiers::CTRL, false).unwrap(), vec![0x00]);
        assert_eq!(key_bytes(Key::A, NONE, false), None);
    }

    #[test]
    fn function_and_editing_keys() {
        assert_eq!(key_bytes(Key::F1, NONE, false).unwrap(), b"\x1bOP");
        assert_eq!(key_bytes(Key::F5, NONE, false).unwrap(), b"\x1b[15~");
        assert_eq!(key_bytes(Key::Delete, Modifiers::SHIFT, false).unwrap(), b"\x1b[3;2~");
        assert_eq!(key_bytes(Key::Tab, Modifiers::SHIFT, false).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn alt_letter_is_meta() {
        assert_eq!(key_bytes(Key::B, Modifiers::ALT, false).unwrap(), b"\x1bb");
        // AltGr (Ctrl+Alt) must not be intercepted.
        assert_eq!(key_bytes(Key::Q, Modifiers::ALT | Modifiers::CTRL, false), None);
    }

    #[test]
    fn paste_normalizes_newlines() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), "a\rb\rc");
        assert_eq!(paste_bytes("x\x1b[201~y", true), "\x1b[200~x[201~y\x1b[201~");
    }

    #[test]
    fn pasted_text_cannot_end_a_bracketed_paste() {
        for payload in ["a\x1b[201~rm -rf ~\n", "\x1b[20\x1b[201~1~rm -rf ~\n", "a\u{9b}201~b", "\x1b\x1b[201~[201~x"] {
            let sent = paste_bytes(payload, true);
            let inner = &sent["\x1b[200~".len()..sent.len() - "\x1b[201~".len()];
            assert!(!inner.contains('\x1b') && !inner.contains('\u{9b}'), "{payload:?} -> {sent:?}");
            assert_eq!(sent.matches("\x1b[201~").count(), 1, "only the real end marker: {sent:?}");
        }
        // Without bracketed paste the text goes as it is (newlines as Enter).
        assert_eq!(paste_bytes("a\nb", false), "a\rb");
    }
}
