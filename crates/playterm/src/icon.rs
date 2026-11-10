//! Icons on buttons: Material Symbols (rounded, outlined), drawn in their own font family so
//! they never mix with the symbols of egui's default fonts. Each action has one icon, used
//! wherever that action appears.
//!
//! The icon names come from `egui_material_icons`; the font is a subset with only the icons
//! below (`assets/icons/material-symbols-subset.ttf`, a few kilobytes instead of 1.2 MB),
//! made by `assets/make_icon_font.py`. After adding an icon, run that script again.

use std::sync::Arc;

use eframe::egui::{Button, FontData, FontDefinitions, WidgetText};
pub use egui_material_icons::MaterialIcon;
use egui_material_icons::icons;

/// The Material Symbols Rounded subset (Apache License 2.0, see Help → Open-source licenses).
const FONT: &[u8] = include_bytes!("../../../assets/icons/material-symbols-subset.ttf");
const FONT_NAME: &str = "material-symbols-subset";

/// Defines the icon of each action and [`ALL`], the list the icon font must cover.
macro_rules! icons {
    ($($name:ident = $icon:ident),* $(,)?) => {
        $(pub const $name: MaterialIcon = icons::$icon;)*
        /// Every icon above, by name.
        #[cfg(test)]
        const ALL: &[(&str, MaterialIcon)] = &[$((stringify!($name), $name)),*];
    };
}

icons! {
    OK = ICON_CHECK,
    CANCEL = ICON_CLOSE,
    CLOSE = ICON_CLOSE,
    SAVE = ICON_SAVE,
    DONT_SAVE = ICON_BLOCK,
    REVERT = ICON_UNDO,
    DELETE = ICON_DELETE,
    CONNECT = ICON_LOGIN,
    RECONNECT = ICON_REPLAY,
    RETRY = ICON_REFRESH,
    REFRESH = ICON_REFRESH,
    BROWSE = ICON_FOLDER_OPEN,
    OPEN_FOLDER = ICON_FOLDER_OPEN,
    NEW_SITE = ICON_ADD,
    ADD = ICON_ADD,
    NEW_FOLDER = ICON_CREATE_NEW_FOLDER,
    DUPLICATE = ICON_CONTENT_COPY,
    UPLOAD = ICON_UPLOAD,
    UPLOAD_FOLDER = ICON_DRIVE_FOLDER_UPLOAD,
    DOWNLOAD = ICON_DOWNLOAD,
    UP = ICON_ARROW_UPWARD,
    DOWN = ICON_ARROW_DOWNWARD,
    UNLOCK = ICON_LOCK_OPEN,
    LOCK = ICON_LOCK,
    SIDEBAR_SHOW = ICON_LEFT_PANEL_OPEN,
    SIDEBAR_HIDE = ICON_LEFT_PANEL_CLOSE,
    VAULT_SET_UP = ICON_KEY,
    TERMINAL_DIR = ICON_TERMINAL,
    IMPORT = ICON_INPUT,
    SEND = ICON_SEND,
    PASTE = ICON_CONTENT_PASTE,
    TRUST = ICON_VERIFIED_USER,
    RESET = ICON_RESTART_ALT,
    BACK = ICON_ARROW_BACK,
    UPDATE = ICON_SYSTEM_UPDATE_ALT,
    WEB_PAGE = ICON_OPEN_IN_NEW,
    LATER = ICON_SCHEDULE,
    SKIP = ICON_SKIP_NEXT,
    RESTART = ICON_RESTART_ALT,
    TEMPLATE = ICON_BOOKMARK_ADD,
    MANAGE = ICON_SETTINGS,
    CLEAR = ICON_CLEAR_ALL,
    TURN_OFF = ICON_POWER_SETTINGS_NEW,
    SFTP = ICON_FOLDER,
    STATE_LOCAL = ICON_TERMINAL,
    STATE_CONNECTING = ICON_SYNC,
    STATE_CONNECTED = ICON_CHECK_CIRCLE,
    STATE_CLOSED = ICON_LINK_OFF,
    NEW_TAB = ICON_ADD_BOX,
    QUICK_CONNECT = ICON_BOLT,
    SITE_MANAGER = ICON_DNS,
    DISCONNECT = ICON_LINK_OFF,
    COPY = ICON_CONTENT_COPY,
    FIND = ICON_SEARCH,
    FONT_SMALLER = ICON_TEXT_DECREASE,
    FONT_LARGER = ICON_TEXT_INCREASE,
    COMPOSE_BAR = ICON_KEYBOARD,
    BROADCAST = ICON_CELL_TOWER,
    FULLSCREEN = ICON_FULLSCREEN,
    FULLSCREEN_EXIT = ICON_FULLSCREEN_EXIT,
    SETTINGS = ICON_SETTINGS,
}

/// A button with `icon` before `text`.
pub fn button<'a>(icon: MaterialIcon, text: impl Into<WidgetText>) -> Button<'a> {
    Button::new((icon.rich_text(), text.into()))
}

/// The main action of a dialog (Connect, Save, OK…): filled with the selection color, so it
/// stands out from Cancel and it is clear what Enter does.
pub fn primary<'a>(ui: &eframe::egui::Ui, icon: MaterialIcon, text: impl Into<WidgetText>) -> Button<'a> {
    button(icon, text).fill(ui.visuals().selection.bg_fill)
}

/// An action that destroys something (Delete): filled in a dark red.
pub fn danger<'a>(ui: &eframe::egui::Ui, icon: MaterialIcon, text: impl Into<WidgetText>) -> Button<'a> {
    button(icon, text).fill(ui.visuals().error_fg_color.gamma_multiply(0.45))
}

/// Whether the main action comes after Cancel (macOS) or before it (Windows and others).
pub const MAIN_LAST: bool = cfg!(target_os = "macos");

/// A dialog's main button and Cancel side by side, in the platform's order. Returns their
/// responses as (main, cancel).
pub fn ok_cancel_row(
    ui: &mut eframe::egui::Ui,
    main: Button<'_>,
    main_enabled: bool,
    cancel: Button<'_>,
) -> (eframe::egui::Response, eframe::egui::Response) {
    ui.horizontal(|ui| {
        if MAIN_LAST {
            let c = ui.add(cancel);
            (ui.add_enabled(main_enabled, main), c)
        } else {
            let m = ui.add_enabled(main_enabled, main);
            (m, ui.add(cancel))
        }
    })
    .inner
}

/// Cancel, as every dialog has it.
pub fn cancel<'a>() -> Button<'a> {
    button(CANCEL, playterm_i18n::t!("common.cancel"))
}

/// Smallest size of a button that shows only an icon: the recommended minimum target, so a
/// small ✕ or ▲ is not hard to hit (the icon itself keeps its size).
pub const MIN_ICON_BUTTON: f32 = 24.0;

/// A button showing only `icon` (give it a tooltip with `on_hover_text`), at least
/// [`MIN_ICON_BUTTON`] square.
pub fn icon_button<'a>(icon: MaterialIcon) -> Button<'a> {
    Button::new(icon.rich_text()).min_size(eframe::egui::vec2(MIN_ICON_BUTTON, MIN_ICON_BUTTON))
}

/// An icon-only button named `name`: in its tooltip (also while disabled) and for screen
/// readers, which would otherwise read the icon's private-use character.
pub fn named_icon_button(ui: &mut eframe::egui::Ui, icon: MaterialIcon, name: &str, enabled: bool) -> eframe::egui::Response {
    use eframe::egui::{Role, WidgetInfo};
    let response = ui.add_enabled(enabled, icon_button(icon));
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, name));
    response.on_hover_text(name).on_disabled_hover_text(name)
}

/// Adds the icon font to `defs` (which [`crate::fonts::install`] then installs), as the only
/// font of the family [`MaterialIcon::rich_text`] draws with.
pub fn add_font(defs: &mut FontDefinitions) {
    let mut data = FontData::from_static(FONT);
    // Centres the icons on the label text: without this they sit about 1.5 px low (at the UI
    // font size), and egui_material_icons' own +0.05 for egui's default font puts them 2.5 px
    // low. Measured on the buttons: -0.075 leaves at most half a pixel.
    data.tweak.y_offset_factor = -0.075;
    defs.font_data.insert(FONT_NAME.to_owned(), Arc::new(data));
    defs.families.insert(OK.font_family(), vec![FONT_NAME.to_owned()]);
}

#[cfg(test)]
mod tests {
    use eframe::egui::FontFamily;

    use super::*;

    #[test]
    fn icon_font_is_registered() {
        let mut defs = FontDefinitions::default();
        add_font(&mut defs);
        assert!(defs.font_data.contains_key(FONT_NAME));
        assert_eq!(defs.families[&OK.font_family()], [FONT_NAME]);
        // Not mixed into the text fonts: their symbols keep their own look.
        assert!(!defs.families[&FontFamily::Proportional].iter().any(|f| f == FONT_NAME));
    }

    #[test]
    fn icon_buttons_are_big_enough_to_hit() {
        let ctx = eframe::egui::Context::default();
        let mut size = eframe::egui::Vec2::ZERO;
        let mut defs = FontDefinitions::default();
        add_font(&mut defs);
        ctx.set_fonts(defs);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            size = ui.add(icon_button(CLOSE).small()).rect.size();
        });
        out.textures_delta.clear(); // no renderer here
        assert!(size.x >= MIN_ICON_BUTTON && size.y >= MIN_ICON_BUTTON, "{size:?}");
    }

    #[test]
    fn main_button_follows_the_platform_order() {
        let ctx = eframe::egui::Context::default();
        let (mut main_x, mut cancel_x) = (0.0, 0.0);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let main = primary(ui, OK, "OK");
            let (m, c) = ok_cancel_row(ui, main, true, cancel());
            (main_x, cancel_x) = (m.rect.left(), c.rect.left());
        });
        out.textures_delta.clear();
        if cfg!(target_os = "macos") {
            assert!(main_x > cancel_x, "macOS: [Cancel] [OK]");
        } else {
            assert!(main_x < cancel_x, "Windows: [OK] [Cancel]");
        }
    }

    #[test]
    fn icons_are_in_the_font() {
        let face = ttf_parser::Face::parse(FONT, 0).expect("the icon font parses");
        for (name, icon) in ALL {
            let c = icon.codepoint.chars().next().unwrap();
            assert!(face.glyph_index(c).is_some(), "{name} (U+{:04X}) is not in the icon font: run assets/make_icon_font.py", c as u32);
        }
    }
}
