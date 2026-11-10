//! The app's keyboard shortcuts in one table: what the keys do, how menus, tooltips and the
//! shortcut list show them, and which ones the settings can turn off (#125, #128).

use eframe::egui::{Key, KeyboardShortcut, ModifierNames, Modifiers};
use playterm_config::ShortcutKeys;
use playterm_i18n::t;

/// Something a shortcut does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    QuickConnect,
    NewTab,
    ReopenClosed,
    SiteManager,
    Sidebar,
    Find,
    Broadcast,
    SftpPanel,
    ComposeBar,
    PrevTab,
    NextTab,
    CloseTab,
    DuplicateTab,
    Copy,
    Paste,
    Fullscreen,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NewWindow,
    Settings,
    Quit,
}

/// A group of keys the settings can turn off, so they reach the terminal instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Optional {
    /// Alt+← / Alt+→ switching tabs (word movement in shells and editors otherwise).
    AltArrows,
    /// Ctrl+= / Ctrl+- / Ctrl+0 changing the font size.
    CtrlZoom,
    /// Ctrl+Shift+C / Ctrl+Shift+V (the clipboard settings).
    CopyKey,
    PasteKey,
}

/// Keys and what they do.
pub struct Binding {
    pub keys: KeyboardShortcut,
    pub what: Shortcut,
    pub optional: Option<Optional>,
}

const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);

const fn bind(modifiers: Modifiers, key: Key, what: Shortcut) -> Binding {
    Binding { keys: KeyboardShortcut::new(modifiers, key), what, optional: None }
}

const fn optional(modifiers: Modifiers, key: Key, what: Shortcut, optional: Optional) -> Binding {
    Binding { keys: KeyboardShortcut::new(modifiers, key), what, optional: Some(optional) }
}

/// The shortcuts of every platform. The first binding of an action is the one menus show.
/// Ctrl+Shift+Tab must come before Ctrl+Tab, whose pattern also matches it.
const BINDINGS: &[Binding] = &[
    // As in Windows Terminal and browsers (#126); Ctrl+Shift+N, the old key, still works.
    bind(CTRL_SHIFT, Key::T, Shortcut::NewTab),
    bind(CTRL_SHIFT, Key::N, Shortcut::NewTab),
    bind(CTRL_SHIFT, Key::O, Shortcut::QuickConnect),
    bind(CTRL_SHIFT, Key::R, Shortcut::ReopenClosed),
    bind(CTRL_SHIFT, Key::S, Shortcut::SiteManager),
    bind(CTRL_SHIFT, Key::B, Shortcut::Sidebar),
    bind(CTRL_SHIFT, Key::F, Shortcut::Find),
    bind(CTRL_SHIFT, Key::I, Shortcut::Broadcast),
    bind(CTRL_SHIFT, Key::X, Shortcut::SftpPanel),
    // Ctrl+Shift+E opened the SFTP window, which the side panel replaced.
    bind(CTRL_SHIFT, Key::E, Shortcut::SftpPanel),
    bind(CTRL_SHIFT, Key::M, Shortcut::ComposeBar),
    bind(CTRL_SHIFT, Key::Tab, Shortcut::PrevTab),
    bind(Modifiers::CTRL, Key::Tab, Shortcut::NextTab),
    optional(Modifiers::ALT, Key::ArrowLeft, Shortcut::PrevTab, Optional::AltArrows),
    optional(Modifiers::ALT, Key::ArrowRight, Shortcut::NextTab, Optional::AltArrows),
    bind(CTRL_SHIFT, Key::W, Shortcut::CloseTab),
    bind(CTRL_SHIFT, Key::D, Shortcut::DuplicateTab),
    optional(CTRL_SHIFT, Key::C, Shortcut::Copy, Optional::CopyKey),
    optional(CTRL_SHIFT, Key::V, Shortcut::Paste, Optional::PasteKey),
    bind(Modifiers::NONE, Key::F11, Shortcut::Fullscreen),
    optional(Modifiers::CTRL, Key::Equals, Shortcut::ZoomIn, Optional::CtrlZoom),
    optional(Modifiers::CTRL, Key::Plus, Shortcut::ZoomIn, Optional::CtrlZoom),
    optional(Modifiers::CTRL, Key::Minus, Shortcut::ZoomOut, Optional::CtrlZoom),
    optional(Modifiers::CTRL, Key::Num0, Shortcut::ZoomReset, Optional::CtrlZoom),
    // As in VS Code and Windows Terminal (#127).
    bind(Modifiers::CTRL, Key::Comma, Shortcut::Settings),
];

/// macOS: the Cmd shortcuts Mac apps share (Cmd never reaches the shell, so these take nothing
/// from the terminal). Shown in menus before the ones above.
const MAC_BINDINGS: &[Binding] = &[
    bind(Modifiers::MAC_CMD, Key::T, Shortcut::NewTab),
    bind(Modifiers::MAC_CMD.plus(Modifiers::SHIFT), Key::T, Shortcut::ReopenClosed),
    bind(Modifiers::MAC_CMD, Key::W, Shortcut::CloseTab),
    bind(Modifiers::MAC_CMD, Key::N, Shortcut::NewWindow),
    bind(Modifiers::MAC_CMD, Key::F, Shortcut::Find),
    bind(Modifiers::MAC_CMD, Key::Comma, Shortcut::Settings),
    bind(Modifiers::MAC_CMD, Key::Q, Shortcut::Quit),
    bind(Modifiers::MAC_CMD.plus(Modifiers::SHIFT), Key::OpenBracket, Shortcut::PrevTab),
    bind(Modifiers::MAC_CMD.plus(Modifiers::SHIFT), Key::CloseBracket, Shortcut::NextTab),
    bind(Modifiers::MAC_CMD, Key::Equals, Shortcut::ZoomIn),
    bind(Modifiers::MAC_CMD, Key::Plus, Shortcut::ZoomIn),
    bind(Modifiers::MAC_CMD, Key::Minus, Shortcut::ZoomOut),
    bind(Modifiers::MAC_CMD, Key::Num0, Shortcut::ZoomReset),
];

/// The bindings of this platform, the Mac ones first on macOS.
pub fn bindings() -> impl Iterator<Item = &'static Binding> {
    let mac: &[Binding] = if cfg!(target_os = "macos") { MAC_BINDINGS } else { &[] };
    mac.iter().chain(BINDINGS)
}

/// Whether `binding` is on with these settings.
pub fn enabled(binding: &Binding, keys: &ShortcutKeys, copy: bool, paste: bool) -> bool {
    match binding.optional {
        None => true,
        Some(Optional::AltArrows) => keys.alt_arrows_switch_tabs,
        Some(Optional::CtrlZoom) => keys.ctrl_zoom,
        Some(Optional::CopyKey) => copy,
        Some(Optional::PasteKey) => paste,
    }
}

/// A key as menus show it: symbols for punctuation and arrows.
fn key_label(key: Key) -> &'static str {
    match key {
        Key::Equals => "=",
        Key::Plus => "+",
        Key::Minus => "-",
        Key::Comma => ",",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        key => key.name(),
    }
}

/// `Ctrl+Shift+T`, or `⇧⌘T` on macOS.
pub fn format(keys: &KeyboardShortcut) -> String {
    let mac = cfg!(target_os = "macos");
    let names = if mac { ModifierNames::SYMBOLS } else { ModifierNames::NAMES };
    let mut s = names.format(&keys.modifiers, mac);
    if !s.is_empty() {
        s += names.concat;
    }
    s + key_label(keys.logical_key)
}

/// The keys menus and tooltips show for `what` (its first binding); empty when it has none.
pub fn hint(what: Shortcut) -> String {
    bindings().find(|b| b.what == what).map(|b| format(&b.keys)).unwrap_or_default()
}

/// What `what` does, for the shortcut list.
pub fn describe(what: Shortcut) -> &'static str {
    match what {
        Shortcut::NewTab => t!("keys.new_tab_local_prompt"),
        Shortcut::QuickConnect => t!("common.quick_connect"),
        Shortcut::ReopenClosed => t!("common.reopen_closed_tab"),
        Shortcut::SiteManager => t!("common.site_manager"),
        Shortcut::Sidebar => t!("keys.show_hide_the_site_list"),
        Shortcut::Find => {
            t!("keys.find_including_scrollback_enter_up_shift_enter_down")
        }
        Shortcut::Broadcast => t!("keys.type_into_all_tabs_on_off"),
        Shortcut::SftpPanel => t!("keys.sftp_explorer_side_panel_on_off"),
        Shortcut::ComposeBar => t!("keys.command_input_bar_on_off"),
        Shortcut::PrevTab => t!("common.previous_tab"),
        Shortcut::NextTab => t!("common.next_tab"),
        Shortcut::CloseTab => t!("common.close_tab"),
        Shortcut::DuplicateTab => t!("common.duplicate_tab"),
        Shortcut::Copy => t!("common.copy"),
        Shortcut::Paste => t!("common.paste"),
        Shortcut::Fullscreen => t!("common.full_screen"),
        Shortcut::ZoomIn => t!("common.larger_font"),
        Shortcut::ZoomOut => t!("common.smaller_font"),
        Shortcut::ZoomReset => t!("keys.default_font_size"),
        Shortcut::NewWindow => t!("common.new_window"),
        Shortcut::Settings => t!("common.settings"),
        Shortcut::Quit => t!("common.quit"),
    }
}

/// The shortcut list: each action with all its keys that are on (`Ctrl+Tab / Alt+→`), in
/// table order. Copy and paste are left to the caller, which knows all their keys.
pub fn list(keys: &ShortcutKeys) -> Vec<(String, &'static str)> {
    let mut out: Vec<(Shortcut, Vec<String>)> = Vec::new();
    // Ctrl+Plus is the same key as Ctrl+= on most keyboards: listed once.
    for b in bindings().filter(|b| enabled(b, keys, false, false) && b.keys.logical_key != Key::Plus) {
        let text = format(&b.keys);
        match out.iter_mut().find(|(what, _)| *what == b.what) {
            Some((_, all)) => {
                if !all.contains(&text) {
                    all.push(text);
                }
            }
            None => out.push((b.what, vec![text])),
        }
    }
    out.into_iter().map(|(what, all)| (all.join(" / "), describe(what))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_read_as_people_write_them() {
        if cfg!(target_os = "macos") {
            return;
        }
        assert_eq!(hint(Shortcut::NewTab), "Ctrl+Shift+T");
        assert_eq!(hint(Shortcut::QuickConnect), "Ctrl+Shift+O");
        assert_eq!(hint(Shortcut::Settings), "Ctrl+,");
        assert_eq!(hint(Shortcut::ZoomIn), "Ctrl+=");
        assert_eq!(hint(Shortcut::Quit), "", "no shortcut of its own outside macOS");
    }

    /// Alt+arrows are off unless turned on; turned on, they are listed and work.
    #[test]
    fn optional_keys_follow_the_settings() {
        let off = ShortcutKeys::default();
        let alt =
            |keys: &ShortcutKeys| bindings().filter(|b| b.optional == Some(Optional::AltArrows)).all(|b| enabled(b, keys, true, true));
        assert!(!alt(&off), "off by default (#125)");
        let on = ShortcutKeys { alt_arrows_switch_tabs: true, ..off };
        assert!(alt(&on));
        assert!(list(&on).iter().any(|(k, _)| k.contains('→')));
        assert!(!list(&off).iter().any(|(k, _)| k.contains('→')));
    }

    /// Every action a menu shows a key for has that key in the table, and no two actions
    /// share keys.
    #[test]
    fn one_action_per_key() {
        let all: Vec<&Binding> = bindings().collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(a.keys != b.keys || a.what == b.what, "{} twice", format(&a.keys));
            }
        }
    }
}
