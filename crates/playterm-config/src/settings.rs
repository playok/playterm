//! Application preferences (`settings.toml`): terminal font, size, colors and scrollback.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fsutil::write_atomic;

/// Language of the user interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "ko")]
    Korean,
    #[serde(rename = "en")]
    English,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::Korean, Language::English];

    /// The language's own name, the same in either UI language.
    pub fn name(self) -> &'static str {
        match self {
            Language::Korean => "한국어",
            Language::English => "English",
        }
    }
}

/// Light or dark windows, menus and dialogs (the terminal has its own color theme).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiTheme {
    Dark,
    Light,
    /// Follow the light/dark mode of the system (Windows app mode, macOS appearance), also
    /// when it changes while playterm runs. The default.
    #[default]
    System,
}

impl UiTheme {
    pub const ALL: [UiTheme; 3] = [UiTheme::System, UiTheme::Dark, UiTheme::Light];
}

/// How the site manager lists the sites.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteView {
    /// Folders and the sites in them.
    #[default]
    Tree,
    /// One row per site with its connection details.
    Grid,
}

/// Keeping Windows awake while sessions are connected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreventSleep {
    Off,
    /// The PC does not go to sleep on its own while a session is connected.
    #[default]
    System,
    /// ...and the screen stays on too.
    Display,
}

impl PreventSleep {
    pub const ALL: [PreventSleep; 3] = [PreventSleep::Off, PreventSleep::System, PreventSleep::Display];
}

/// App shortcuts that take keys programs in the terminal also use; off, those keys go to the
/// terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutKeys {
    /// Alt+← / Alt+→ switch tabs (shells and editors move by words with them otherwise).
    pub alt_arrows_switch_tabs: bool,
    /// Ctrl+= / Ctrl+- / Ctrl+0 change the font size.
    pub ctrl_zoom: bool,
}

impl Default for ShortcutKeys {
    fn default() -> Self {
        Self { alt_arrows_switch_tabs: false, ctrl_zoom: true }
    }
}

/// Which keys and mouse buttons copy and paste in the terminal. A key that is off goes to the
/// remote side as a key (Ctrl+V as ^V, Shift+Insert as Insert, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardKeys {
    /// Selecting text with the mouse copies it right away (PuTTY style).
    pub copy_on_select: bool,
    /// Ctrl+C copies when there is a selection (^C otherwise).
    pub ctrl_c: bool,
    pub ctrl_shift_c: bool,
    pub ctrl_insert: bool,
    pub ctrl_v: bool,
    pub ctrl_shift_v: bool,
    pub shift_insert: bool,
    /// The right button pastes (Ctrl+right click opens the menu).
    pub right_click_paste: bool,
    pub middle_click_paste: bool,
    /// Programs on the remote side may put text on the clipboard (OSC 52, e.g. tmux or vim
    /// yanks). Only the current tab, with a notice each time; never read.
    pub remote_copy: bool,
    /// Ask before pasting several lines where each line would run as a command (no bracketed
    /// paste).
    pub confirm_multiline_paste: bool,
}

impl Default for ClipboardKeys {
    fn default() -> Self {
        Self {
            copy_on_select: false,
            ctrl_c: true,
            ctrl_shift_c: true,
            ctrl_insert: true,
            // Off: vim's block selection and the shell's ^V need it (#152); Ctrl+Shift+V and
            // Shift+Insert paste.
            ctrl_v: false,
            ctrl_shift_v: true,
            shift_insert: true,
            right_click_paste: false,
            middle_click_paste: false,
            remote_copy: true,
            confirm_multiline_paste: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// A font name from the built-in list (e.g. `Consolas`, `D2Coding`) or a font file path.
    pub font: String,
    /// Terminal font size in points.
    pub font_size: f32,
    /// Color scheme name (see `playterm_term::THEMES`).
    pub theme: String,
    /// Lines of history kept per tab.
    pub scrollback: usize,
    /// Show the toolbar under the menu bar.
    pub toolbar: bool,
    /// Show the status bar under the terminal.
    pub status_bar: bool,
    /// Show the command input bar under the terminal.
    pub compose_bar: bool,
    pub language: Language,
    pub ui_theme: UiTheme,
    pub prevent_sleep: PreventSleep,
    /// Keep the screen saver from starting while playterm runs (Tools menu).
    pub prevent_screensaver: bool,
    /// Seconds to wait for a server while connecting before giving up (prompts not counted).
    pub connect_timeout: u32,
    /// Ask before closing tabs (or quitting) while their connections are up.
    pub confirm_close: bool,
    /// Copy / paste keys of the terminal.
    pub clipboard: ClipboardKeys,
    /// App shortcuts that can be given back to the terminal.
    pub shortcuts: ShortcutKeys,
    /// Tree or grid in the site manager.
    pub site_view: SiteView,
    /// Folder ZMODEM downloads (`sz` on the server) are saved in; empty = the Downloads folder.
    pub zmodem_dir: String,
    /// Ask for the folder each time a ZMODEM download starts (`zmodem_dir` is where the
    /// question starts).
    pub zmodem_ask: bool,
    /// Look for a new release on GitHub at start-up.
    pub check_updates: bool,
    /// A release version the user chose to skip ("Skip this version"); not offered again.
    pub skipped_update: String,
    /// Saved sites connected to lately, newest first (their ids), for the start screen.
    pub recent_sites: Vec<String>,
    /// macOS: Option+key is sent as Meta (ESC and the key), like Alt elsewhere. Off: Option
    /// types the character macOS gives it (é, ©, …).
    pub option_as_meta: bool,
    /// Named command sequences run in a tab (Tools → Macros), in menu order.
    pub macros: Vec<Macro>,
}

/// A macro: lines to type, with `@wait`, `@sleep`, … steps between them (see the app's
/// `macros` module). Holds no passwords.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Macro {
    pub name: String,
    pub script: String,
}

impl AppSettings {
    /// Recent sites kept.
    pub const MAX_RECENT: usize = 8;

    /// Puts site `id` first among the recent ones.
    pub fn remember_site(&mut self, id: &str) {
        self.recent_sites.retain(|r| r != id);
        self.recent_sites.insert(0, id.to_owned());
        self.recent_sites.truncate(Self::MAX_RECENT);
    }
}

impl AppSettings {
    pub const MIN_FONT_SIZE: f32 = 8.0;
    pub const MAX_FONT_SIZE: f32 = 40.0;
    pub const DEFAULT_FONT_SIZE: f32 = 15.0;
    /// The terminal font every installation of the platform has (on Linux, the one most
    /// distributions install).
    pub const DEFAULT_FONT: &str = if cfg!(windows) {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    };
    pub const MIN_SCROLLBACK: usize = 100;
    pub const MAX_SCROLLBACK: usize = 200_000;
    pub const MIN_CONNECT_TIMEOUT: u32 = 3;
    pub const MAX_CONNECT_TIMEOUT: u32 = 300;
    pub const DEFAULT_CONNECT_TIMEOUT: u32 = 15;

    /// Loads `path`; a missing file gives the defaults.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Self>(&text)
                .map(Self::clamped)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(path, text.as_bytes())
    }

    /// Values brought into their valid ranges (hand-edited files).
    pub fn clamped(mut self) -> Self {
        self.font_size = if self.font_size.is_finite() {
            self.font_size.clamp(Self::MIN_FONT_SIZE, Self::MAX_FONT_SIZE)
        } else {
            Self::DEFAULT_FONT_SIZE
        };
        self.scrollback = self.scrollback.clamp(Self::MIN_SCROLLBACK, Self::MAX_SCROLLBACK);
        self.connect_timeout = self.connect_timeout.clamp(Self::MIN_CONNECT_TIMEOUT, Self::MAX_CONNECT_TIMEOUT);
        self
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            font: Self::DEFAULT_FONT.into(),
            font_size: Self::DEFAULT_FONT_SIZE,
            theme: "auto".into(),
            scrollback: 10_000,
            toolbar: true,
            status_bar: true,
            compose_bar: false,
            language: Language::Korean,
            ui_theme: UiTheme::System,
            prevent_sleep: PreventSleep::System,
            prevent_screensaver: false,
            connect_timeout: Self::DEFAULT_CONNECT_TIMEOUT,
            confirm_close: true,
            clipboard: ClipboardKeys::default(),
            shortcuts: ShortcutKeys::default(),
            site_view: SiteView::Tree,
            zmodem_dir: String::new(),
            zmodem_ask: true,
            check_updates: true,
            skipped_update: String::new(),
            recent_sites: Vec::new(),
            option_as_meta: true,
            macros: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn recent_sites_newest_first_without_repeats() {
        let mut s = AppSettings::default();
        for id in ["a", "b", "a", "c"] {
            s.remember_site(id);
        }
        assert_eq!(s.recent_sites, ["c", "a", "b"]);
        for i in 0..20 {
            s.remember_site(&i.to_string());
        }
        assert_eq!(s.recent_sites.len(), AppSettings::MAX_RECENT);
    }
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert_eq!(AppSettings::load(&path).unwrap(), AppSettings::default());

        let s = AppSettings {
            font: "D2Coding".into(),
            font_size: 18.5,
            theme: "Dracula".into(),
            scrollback: 5000,
            toolbar: false,
            status_bar: false,
            compose_bar: true,
            language: Language::English,
            ui_theme: UiTheme::Light,
            prevent_sleep: PreventSleep::Display,
            prevent_screensaver: true,
            connect_timeout: 60,
            confirm_close: false,
            clipboard: ClipboardKeys { copy_on_select: true, ctrl_v: true, right_click_paste: true, ..Default::default() },
            shortcuts: ShortcutKeys { alt_arrows_switch_tabs: true, ctrl_zoom: false },
            site_view: SiteView::Grid,
            zmodem_dir: "D:\\받은 파일".into(),
            zmodem_ask: false,
            check_updates: false,
            skipped_update: "0.3.0".into(),
            recent_sites: vec!["w1".into()],
            option_as_meta: false,
            macros: vec![Macro { name: "점검".into(), script: "show version\n@wait #\nshow ip int brief".into() }],
        };
        s.save(&path).unwrap();
        assert_eq!(AppSettings::load(&path).unwrap(), s);

        // Missing keys take defaults; out-of-range values are clamped.
        std::fs::write(&path, "font_size = 99\nscrollback = 1\nconnect_timeout = 0\n").unwrap();
        let s = AppSettings::load(&path).unwrap();
        assert_eq!((s.font.as_str(), s.font_size, s.scrollback), (AppSettings::DEFAULT_FONT, 40.0, 100));
        assert_eq!(s.connect_timeout, AppSettings::MIN_CONNECT_TIMEOUT, "no endless wait");
        std::fs::write(&path, "theme = \"Dracula\"\n").unwrap();
        assert_eq!(AppSettings::load(&path).unwrap().connect_timeout, 15, "older files get the default");
        assert_eq!(AppSettings::load(&path).unwrap().clipboard, ClipboardKeys::default());
        assert_eq!(AppSettings::load(&path).unwrap().ui_theme, UiTheme::System, "the system's light or dark by default");
        std::fs::write(&path, "ui_theme = \"dark\"\n").unwrap();
        assert_eq!(AppSettings::load(&path).unwrap().ui_theme, UiTheme::Dark, "a choice made is kept");
        std::fs::write(&path, "[clipboard]\ncopy_on_select = true\n").unwrap();
        let keys = AppSettings::load(&path).unwrap().clipboard;
        assert!(keys.copy_on_select && keys.ctrl_shift_c, "keys left out keep their defaults");

        std::fs::write(&path, "font_size = \"big\"").unwrap();
        assert!(AppSettings::load(&path).is_err());
    }
}
