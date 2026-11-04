//! Which keys and mouse buttons copy and paste in the terminal (Tools → Settings → Copy / paste).
//!
//! egui's own clipboard shortcuts are turned off (`NativeOptions::clipboard_shortcuts`): they
//! turn Ctrl+C and Ctrl+Insert alike into one Copy event (Ctrl+V and Shift+Insert into Paste,
//! Ctrl+X and Shift+Delete into Cut), so the terminal could not tell them apart or let them
//! through as keys. The terminal now reads the keys itself; for the other widgets (text
//! fields) [`widget_shortcuts`] makes the usual events from them again.

use eframe::egui::{self, Event, Key, Modifiers};
use playterm_config::ClipboardKeys;

/// What a key does in the terminal instead of being sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipAction {
    Copy,
    Paste,
}

/// Ctrl alone (no Shift / Alt).
fn ctrl_only(m: Modifiers) -> bool {
    m.ctrl && !m.shift && !m.alt
}

/// macOS: Cmd alone, the usual modifier of the clipboard keys there.
fn cmd_only(m: Modifiers) -> bool {
    cfg!(target_os = "macos") && m.mac_cmd && !m.shift && !m.alt && !m.ctrl
}

/// Shift alone.
fn shift_only(m: Modifiers) -> bool {
    m.shift && !m.ctrl && !m.alt
}

/// The clipboard action of a key press in the terminal, if `keys` give it one. Ctrl+C copies
/// only when something is selected (it is the interrupt signal otherwise). Ctrl+Shift+C/V are
/// app shortcuts, handled before the terminal sees them.
pub fn terminal_action(key: Key, m: Modifiers, keys: &ClipboardKeys, has_selection: bool) -> Option<ClipAction> {
    match key {
        // macOS: Cmd+C / Cmd+V, as in every Mac app (Cmd never reaches the shell anyway).
        Key::C if cmd_only(m) && has_selection => Some(ClipAction::Copy),
        Key::V if cmd_only(m) => Some(ClipAction::Paste),
        Key::C if ctrl_only(m) && keys.ctrl_c && has_selection => Some(ClipAction::Copy),
        Key::Insert if ctrl_only(m) && keys.ctrl_insert => Some(ClipAction::Copy),
        Key::V if ctrl_only(m) && keys.ctrl_v => Some(ClipAction::Paste),
        Key::Insert if shift_only(m) && keys.shift_insert => Some(ClipAction::Paste),
        _ => None,
    }
}

/// The key shown next to Copy in menus: the first one turned on.
pub fn copy_hint(keys: &ClipboardKeys) -> &'static str {
    [(keys.ctrl_shift_c, "Ctrl+Shift+C"), (keys.ctrl_c, "Ctrl+C"), (keys.ctrl_insert, "Ctrl+Insert")]
        .into_iter()
        .find_map(|(on, label)| on.then_some(label))
        .unwrap_or("")
}

/// The key shown next to Paste in menus: the first one turned on.
pub fn paste_hint(keys: &ClipboardKeys) -> &'static str {
    [(keys.ctrl_shift_v, "Ctrl+Shift+V"), (keys.ctrl_v, "Ctrl+V"), (keys.shift_insert, "Shift+Insert")]
        .into_iter()
        .find_map(|(on, label)| on.then_some(label))
        .unwrap_or("")
}

/// Turns the standard clipboard keys into Copy / Cut / Paste events, as egui would, for text
/// fields and other widgets (called when no terminal or remote desktop has the keyboard):
/// Ctrl+C / Ctrl+Insert, Ctrl+X / Shift+Delete, Ctrl+V / Shift+Insert.
pub fn widget_shortcuts(events: &mut [Event], clipboard: impl Fn() -> Option<String>) {
    for event in events.iter_mut() {
        let Event::Key { key, pressed: true, modifiers: m, .. } = *event else { continue };
        *event = match key {
            Key::C if ctrl_only(m) || cmd_only(m) => Event::Copy,
            Key::Insert if ctrl_only(m) => Event::Copy,
            Key::X if ctrl_only(m) || cmd_only(m) => Event::Cut,
            Key::Delete if shift_only(m) => Event::Cut,
            Key::V if ctrl_only(m) || cmd_only(m) => match clipboard() {
                Some(text) => Event::Paste(text),
                None => continue,
            },
            Key::Insert if shift_only(m) => match clipboard() {
                Some(text) => Event::Paste(text),
                None => continue,
            },
            _ => continue,
        };
    }
}

/// Whether `response` was clicked with the mouse button that pastes, if any: the right button
/// (unless Ctrl is held, which opens the context menu) or the middle one.
pub fn paste_click(response: &egui::Response, keys: &ClipboardKeys, ctrl: bool) -> bool {
    (keys.right_click_paste && response.secondary_clicked() && !ctrl) || (keys.middle_click_paste && response.middle_clicked())
}

#[cfg(test)]
mod tests {

    #[test]
    #[cfg(target_os = "macos")]
    fn cmd_copies_and_pastes_on_macos() {
        let none = ClipboardKeys { ctrl_c: false, ctrl_v: false, ..ClipboardKeys::default() };
        let cmd = Modifiers::MAC_CMD;
        assert_eq!(terminal_action(Key::C, cmd, &none, true), Some(ClipAction::Copy), "whatever the Ctrl settings");
        assert_eq!(terminal_action(Key::C, cmd, &none, false), None, "nothing selected: nothing to copy");
        assert_eq!(terminal_action(Key::V, cmd, &none, false), Some(ClipAction::Paste));
        let mut events = vec![Event::Key { key: Key::X, physical_key: None, pressed: true, repeat: false, modifiers: cmd }];
        widget_shortcuts(&mut events, || None);
        assert!(matches!(events[0], Event::Cut), "text fields: Cmd+X cuts");
    }
    use super::*;

    const CTRL: Modifiers = Modifiers::CTRL;
    const SHIFT: Modifiers = Modifiers::SHIFT;

    #[test]
    fn terminal_keys_follow_the_settings() {
        let all = ClipboardKeys::default();
        assert_eq!(terminal_action(Key::C, CTRL, &all, true), Some(ClipAction::Copy));
        assert_eq!(terminal_action(Key::C, CTRL, &all, false), None, "^C without a selection");
        assert_eq!(terminal_action(Key::Insert, CTRL, &all, false), Some(ClipAction::Copy));
        assert_eq!(terminal_action(Key::V, CTRL, &all, false), None, "Ctrl+V goes to the terminal by default (#152)");
        let ctrl_v = ClipboardKeys { ctrl_v: true, ..ClipboardKeys::default() };
        assert_eq!(terminal_action(Key::V, CTRL, &ctrl_v, false), Some(ClipAction::Paste), "when turned on");
        assert_eq!(terminal_action(Key::Insert, SHIFT, &all, false), Some(ClipAction::Paste));
        assert_eq!(terminal_action(Key::Insert, Modifiers::NONE, &all, false), None);
        assert_eq!(terminal_action(Key::V, CTRL.plus(SHIFT), &all, false), None, "an app shortcut");

        let none = ClipboardKeys { ctrl_c: false, ctrl_insert: false, ctrl_v: false, shift_insert: false, ..ClipboardKeys::default() };
        for (key, m) in [(Key::C, CTRL), (Key::Insert, CTRL), (Key::V, CTRL), (Key::Insert, SHIFT)] {
            assert_eq!(terminal_action(key, m, &none, true), None, "{key:?} goes to the terminal");
        }
    }

    #[test]
    fn hints_name_the_first_key_turned_on() {
        let mut keys = ClipboardKeys::default();
        assert_eq!((copy_hint(&keys), paste_hint(&keys)), ("Ctrl+Shift+C", "Ctrl+Shift+V"));
        keys.ctrl_shift_c = false;
        keys.ctrl_shift_v = false;
        keys.ctrl_v = false;
        assert_eq!((copy_hint(&keys), paste_hint(&keys)), ("Ctrl+C", "Shift+Insert"));
        keys.ctrl_c = false;
        keys.ctrl_insert = false;
        assert_eq!(copy_hint(&keys), "");
    }

    #[test]
    fn widgets_get_the_usual_clipboard_events() {
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let mut events = vec![
            key(Key::C, CTRL),
            key(Key::Insert, CTRL),
            key(Key::X, CTRL),
            key(Key::Delete, SHIFT),
            key(Key::V, CTRL),
            key(Key::Insert, SHIFT),
            key(Key::A, CTRL),
            key(Key::C, CTRL.plus(SHIFT)),
        ];
        widget_shortcuts(&mut events, || Some("text".into()));
        assert!(matches!(events[0], Event::Copy));
        assert!(matches!(events[1], Event::Copy));
        assert!(matches!(events[2], Event::Cut));
        assert!(matches!(events[3], Event::Cut));
        assert!(matches!(&events[4], Event::Paste(t) if t == "text"));
        assert!(matches!(&events[5], Event::Paste(t) if t == "text"));
        assert!(matches!(events[6], Event::Key { key: Key::A, .. }), "other keys stay");
        assert!(matches!(events[7], Event::Key { key: Key::C, .. }), "Ctrl+Shift+C is not Copy");

        // An empty clipboard pastes nothing; the key stays a key.
        let mut events = vec![key(Key::V, CTRL)];
        widget_shortcuts(&mut events, || None);
        assert!(matches!(events[0], Event::Key { key: Key::V, .. }));
    }
}
