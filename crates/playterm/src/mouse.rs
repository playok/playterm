//! xterm mouse reporting: turns mouse events into the escape sequences an application
//! asked for with DECSET 1000/1002/1003 (what to report) and 1005/1006 (how to encode it).

use playterm_term::TermMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion with no button held (mode 1003 only).
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Whether the application currently wants mouse events.
pub fn reporting(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Escape sequence for one mouse event at a 0-based viewport cell, or `None` when the current
/// mode does not report it (or the position cannot be encoded).
pub fn encode(mode: TermMode, button: MouseButton, action: MouseAction, col: usize, row: usize, mods: MouseMods) -> Option<Vec<u8>> {
    if !reporting(mode) {
        return None;
    }
    let wheel = matches!(button, MouseButton::WheelUp | MouseButton::WheelDown);
    match action {
        // 1003 reports every movement; 1002 only while a button is held.
        MouseAction::Motion => {
            let allowed = mode.contains(TermMode::MOUSE_MOTION) || (mode.contains(TermMode::MOUSE_DRAG) && button != MouseButton::None);
            if !allowed || wheel {
                return None;
            }
        }
        // Wheel "buttons" have no release.
        MouseAction::Release if wheel => return None,
        MouseAction::Press if button == MouseButton::None => return None,
        _ => {}
    }

    let sgr = mode.contains(TermMode::SGR_MOUSE);
    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    // Legacy encodings cannot say which button was released.
    if action == MouseAction::Release && !sgr {
        code = 3;
    }
    if action == MouseAction::Motion {
        code += 32;
    }
    code += 4 * u32::from(mods.shift) + 8 * u32::from(mods.alt) + 16 * u32::from(mods.ctrl);

    let (x, y) = (col as u32 + 1, row as u32 + 1);
    if sgr {
        let suffix = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{suffix}").into_bytes());
    }

    let mut out = b"\x1b[M".to_vec();
    out.push(32 + code as u8);
    if mode.contains(TermMode::UTF8_MOUSE) {
        // 1005: coordinates as UTF-8 characters, up to 2015.
        for v in [x, y] {
            let c = char::from_u32(32 + v).filter(|_| v <= 2015)?;
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    } else {
        // X10 byte encoding stops at 223.
        for v in [x, y] {
            if v > 223 {
                return None;
            }
            out.push(32 + v as u8);
        }
    }
    Some(out)
}

/// DECSET 1004 focus report.
pub fn focus(mode: TermMode, focused: bool) -> Option<&'static [u8]> {
    mode.contains(TermMode::FOCUS_IN_OUT).then_some(if focused { b"\x1b[I" } else { b"\x1b[O" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLICK: TermMode = TermMode::MOUSE_REPORT_CLICK;
    const NO_MODS: MouseMods = MouseMods { shift: false, alt: false, ctrl: false };

    fn enc(mode: TermMode, b: MouseButton, a: MouseAction, col: usize, row: usize) -> Option<String> {
        encode(mode, b, a, col, row, NO_MODS).map(|v| String::from_utf8_lossy(&v).into_owned())
    }

    #[test]
    fn nothing_without_mouse_mode() {
        assert_eq!(enc(TermMode::empty(), MouseButton::Left, MouseAction::Press, 0, 0), None);
        assert_eq!(focus(TermMode::empty(), true), None);
    }

    #[test]
    fn sgr_press_release_and_wheel() {
        let mode = CLICK | TermMode::SGR_MOUSE;
        assert_eq!(enc(mode, MouseButton::Left, MouseAction::Press, 4, 2).unwrap(), "\x1b[<0;5;3M");
        assert_eq!(enc(mode, MouseButton::Right, MouseAction::Release, 4, 2).unwrap(), "\x1b[<2;5;3m");
        assert_eq!(enc(mode, MouseButton::WheelDown, MouseAction::Press, 0, 0).unwrap(), "\x1b[<65;1;1M");
        assert_eq!(enc(mode, MouseButton::WheelUp, MouseAction::Release, 0, 0), None);
        // Large terminals are fine with SGR.
        assert_eq!(enc(mode, MouseButton::Middle, MouseAction::Press, 299, 99).unwrap(), "\x1b[<1;300;100M");
    }

    #[test]
    fn modifiers_add_to_the_button_code() {
        let mode = CLICK | TermMode::SGR_MOUSE;
        let mods = MouseMods { shift: true, alt: true, ctrl: true };
        let out = encode(mode, MouseButton::Left, MouseAction::Press, 0, 0, mods).unwrap();
        assert_eq!(out, b"\x1b[<28;1;1M");
    }

    #[test]
    fn motion_depends_on_mode() {
        let click = CLICK | TermMode::SGR_MOUSE;
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        let any = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(enc(click, MouseButton::Left, MouseAction::Motion, 1, 1), None);
        assert_eq!(enc(drag, MouseButton::Left, MouseAction::Motion, 1, 1).unwrap(), "\x1b[<32;2;2M");
        assert_eq!(enc(drag, MouseButton::None, MouseAction::Motion, 1, 1), None);
        assert_eq!(enc(any, MouseButton::None, MouseAction::Motion, 1, 1).unwrap(), "\x1b[<35;2;2M");
        assert_eq!(enc(any, MouseButton::None, MouseAction::Press, 1, 1), None);
    }

    #[test]
    fn legacy_x10_encoding() {
        let out = encode(CLICK, MouseButton::Left, MouseAction::Press, 4, 2, NO_MODS).unwrap();
        assert_eq!(out, vec![0x1b, b'[', b'M', 32, 32 + 5, 32 + 3]);
        let out = encode(CLICK, MouseButton::Left, MouseAction::Release, 4, 2, NO_MODS).unwrap();
        assert_eq!(out[3], 32 + 3, "release is button 3 in legacy mode");
        // Column 224 does not fit in one byte.
        assert_eq!(encode(CLICK, MouseButton::Left, MouseAction::Press, 223, 0, NO_MODS), None);
        assert!(encode(CLICK, MouseButton::Left, MouseAction::Press, 222, 0, NO_MODS).is_some());
    }

    #[test]
    fn utf8_encoding_extends_the_range() {
        let mode = CLICK | TermMode::UTF8_MOUSE;
        let out = encode(mode, MouseButton::Left, MouseAction::Press, 299, 0, NO_MODS).unwrap();
        let mut expected = vec![0x1b, b'[', b'M', 32];
        expected.extend_from_slice(char::from_u32(32 + 300).unwrap().to_string().as_bytes());
        expected.push(32 + 1);
        assert_eq!(out, expected);
        assert_eq!(encode(mode, MouseButton::Left, MouseAction::Press, 2015, 0, NO_MODS), None);
    }

    #[test]
    fn focus_reports() {
        assert_eq!(focus(TermMode::FOCUS_IN_OUT, true), Some(&b"\x1b[I"[..]));
        assert_eq!(focus(TermMode::FOCUS_IN_OUT, false), Some(&b"\x1b[O"[..]));
    }
}
