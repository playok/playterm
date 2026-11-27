//! The toolbar under the menu bar: the commands used most, as icons (as in Xshell), and a box to
//! type an address to connect to. Each button shows its name and shortcut in a tooltip, and has
//! that name for screen readers.

use eframe::egui::containers::menu::MenuButton;
use eframe::egui::{self, Role, WidgetInfo};
use playterm_i18n::{t, tr};

use crate::icon::{self, MaterialIcon};
use crate::keys::{Shortcut, hint};
use crate::menu::{self, Command};
use crate::session::{Session, SessionState};
use playterm_term::Charset;

/// What the toolbar shows.
pub struct ToolbarState<'a> {
    pub active: Option<&'a Session>,
    /// Index of the active tab.
    pub i: usize,
    pub sidebar: bool,
    pub sftp_panel: bool,
    pub compose_bar: bool,
    pub broadcast: bool,
    pub copy_hint: &'static str,
    pub paste_hint: &'static str,
}

/// A toolbar button and what it does.
struct Tool {
    icon: MaterialIcon,
    name: String,
    shortcut: String,
    enabled: bool,
    /// The state of a switch, drawn as selected while on.
    on: Option<bool>,
    action: Action,
}

/// What a toolbar button does when clicked.
enum Action {
    Run(Command),
    /// Opens the encoding menu of tab `i` (the charset is the current one).
    CharsetMenu(usize, Charset),
}

fn tool(icon: MaterialIcon, name: &'static str, shortcut: impl Into<String>, enabled: bool, on: Option<bool>, command: Command) -> Tool {
    Tool { icon, name: name.to_owned(), shortcut: shortcut.into(), enabled, on, action: Action::Run(command) }
}

/// The toolbar's buttons, in groups (separated by a line).
fn groups(ui: &egui::Ui, st: &ToolbarState<'_>) -> Vec<Vec<Tool>> {
    let active = st.active;
    let i = st.i;
    let fullscreen = ui.input(|inp| inp.viewport().fullscreen.unwrap_or(false));
    let sidebar = if st.sidebar {
        tool(icon::SIDEBAR_HIDE, t!("common.hide_the_site_list"), hint(Shortcut::Sidebar), true, None, Command::ToggleSidebar)
    } else {
        tool(icon::SIDEBAR_SHOW, t!("toolbar.show_the_site_list"), hint(Shortcut::Sidebar), true, None, Command::ToggleSidebar)
    };
    // One click on the toolbar must not end a working session: reconnecting is for a closed
    // tab here (the menus still offer it for a connected one).
    let can_reconnect = active.is_some_and(|s| s.has_target() && s.state == SessionState::Closed);
    let can_disconnect = active.is_some_and(Session::can_disconnect);
    let has_selection = active.is_some_and(|s| s.terminal.has_selection());
    let full = if fullscreen {
        tool(icon::FULLSCREEN_EXIT, t!("toolbar.exit_full_screen"), hint(Shortcut::Fullscreen), true, None, Command::ToggleFullscreen)
    } else {
        tool(icon::FULLSCREEN, t!("common.full_screen"), hint(Shortcut::Fullscreen), true, None, Command::ToggleFullscreen)
    };
    // The name carries the current encoding, so the tooltip and screen readers say it too.
    let encoding = match active {
        Some(s) => Tool {
            icon: icon::ENCODING,
            name: tr!("toolbar.encoding", charset = s.charset_label()),
            shortcut: String::new(),
            enabled: s.can_change_charset(),
            on: None,
            action: Action::CharsetMenu(i, s.charset()),
        },
        None => Tool {
            icon: icon::ENCODING,
            name: t!("menu.encoding").to_owned(),
            shortcut: String::new(),
            enabled: false,
            on: None,
            action: Action::CharsetMenu(i, Charset::Auto),
        },
    };
    vec![
        vec![sidebar],
        vec![
            tool(icon::NEW_TAB, t!("common.new_tab"), hint(Shortcut::NewTab), true, None, Command::NewTab),
            tool(icon::QUICK_CONNECT, t!("common.quick_connect"), hint(Shortcut::QuickConnect), true, None, Command::QuickConnect),
            tool(icon::SITE_MANAGER, t!("common.site_manager"), hint(Shortcut::SiteManager), true, None, Command::SiteManager),
        ],
        vec![
            tool(icon::RECONNECT, t!("common.reconnect"), "", can_reconnect, None, Command::Reconnect(i)),
            tool(icon::DISCONNECT, t!("common.disconnect"), "", can_disconnect, None, Command::Disconnect(i)),
        ],
        vec![
            tool(icon::COPY, t!("common.copy"), st.copy_hint, has_selection, None, Command::Copy),
            tool(icon::PASTE, t!("common.paste"), st.paste_hint, active.is_some(), None, Command::Paste),
            tool(icon::FIND, t!("toolbar.find"), hint(Shortcut::Find), active.is_some(), None, Command::Find),
        ],
        vec![encoding],
        vec![
            tool(icon::FONT_SMALLER, t!("common.smaller_font"), hint(Shortcut::ZoomOut), true, None, Command::ZoomOut),
            tool(icon::FONT_LARGER, t!("common.larger_font"), hint(Shortcut::ZoomIn), true, None, Command::ZoomIn),
        ],
        vec![
            // Always on, as the menu item is (#119): on a tab without SFTP the panel folds into a
            // strip and opens on the next SSH tab.
            tool(icon::SFTP, t!("common.sftp_explorer"), hint(Shortcut::SftpPanel), true, Some(st.sftp_panel), Command::ToggleSftpPanel),
            tool(
                icon::COMPOSE_BAR,
                t!("common.command_input_bar"),
                hint(Shortcut::ComposeBar),
                true,
                Some(st.compose_bar),
                Command::ToggleComposeBar,
            ),
            tool(
                icon::BROADCAST,
                t!("common.type_into_all_tabs"),
                hint(Shortcut::Broadcast),
                true,
                Some(st.broadcast),
                Command::ToggleBroadcast,
            ),
        ],
        vec![full, tool(icon::SETTINGS, t!("common.settings"), hint(Shortcut::Settings), true, None, Command::Settings)],
    ]
}

/// The group after which the address box sits (new tab, quick connect, site manager).
const ADDRESS_AFTER_GROUP: usize = 1;

/// The box to type an address into (`user@host`, `ssh://...`, an IP address or a domain, a
/// site's name); Enter connects. Narrower when the window is.
fn address_box(ui: &mut egui::Ui, address: &mut String, out: &mut Option<Command>) {
    let width = (ui.available_width() * 0.3).clamp(110.0, 240.0);
    let name = t!("toolbar.connect_to_an_address");
    let response = ui.add(
        egui::TextEdit::singleline(address).hint_text(t!("toolbar.host_or_user_host")).desired_width(width).id_salt("toolbar-address"),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, name));
    let response = response.on_hover_text(t!("toolbar.type_user_host_an_ip_address_or_a"));
    if response.lost_focus() {
        // The key that ends the typing (Enter, Escape) is the box's: the terminal, which takes
        // the keyboard back in this same frame, must not get it too.
        let (enter, _) = ui.input_mut(|i| {
            (i.consume_key(egui::Modifiers::NONE, egui::Key::Enter), i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        });
        if enter && !address.trim().is_empty() {
            *out = Some(Command::ConnectAddress(address.trim().to_owned()));
        }
    }
}

/// Draws the toolbar's buttons and address box in `ui` (a horizontal layout), returning what
/// was asked for. It stays on one line: the groups that do not fit go into a "»" menu at its
/// end (#144).
pub fn show(ui: &mut egui::Ui, st: &ToolbarState<'_>, address: &mut String) -> Option<Command> {
    let mut out = None;
    let groups = groups(ui, st);
    // Room a button takes, measured on the first one drawn (the same for all of them).
    let width_id = egui::Id::unique("toolbar-button-width");
    let mut button_w: f32 = ui.data(|d| d.get_temp(width_id)).unwrap_or(MIN_GUESS);
    let gap = ui.spacing().item_spacing.x;
    let separator = 2.0 * gap + 1.0;
    let more_w = button_w + separator;
    let mut hidden: Vec<&Tool> = Vec::new();
    for (k, group) in groups.iter().enumerate() {
        let need = group.len() as f32 * button_w + if k > 0 { separator } else { 0.0 };
        let last = k + 1 == groups.len();
        let room = ui.available_width() - if last { 0.0 } else { more_w };
        if !hidden.is_empty() || (k > 0 && need > room) {
            hidden.extend(group);
            continue;
        }
        if k > 0 {
            ui.separator();
        }
        for tool in group {
            let response = match &tool.action {
                Action::Run(command) => {
                    let response = button(ui, tool);
                    if response.clicked() {
                        out = Some(command.clone());
                    }
                    response
                }
                Action::CharsetMenu(i, current) => menu_button(ui, tool, |ui| menu::charset_items(ui, &mut out, *i, *current)),
            };
            button_w = response.rect.width() + gap;
        }
        if k == ADDRESS_AFTER_GROUP {
            ui.separator();
            address_box(ui, address, &mut out);
        }
    }
    ui.data_mut(|d| d.insert_temp(width_id, button_w));
    if !hidden.is_empty() {
        ui.separator();
        let more = ui.menu_button(egui::RichText::new("»").strong(), |ui| {
            for tool in &hidden {
                if let Action::CharsetMenu(i, current) = &tool.action {
                    ui.add_enabled_ui(tool.enabled, |ui| {
                        menu::submenu(ui, &tool.name, |ui| menu::charset_items(ui, &mut out, *i, *current));
                    });
                    continue;
                }
                let Action::Run(command) = &tool.action else { continue };
                let mut item = icon::button(tool.icon, tool.name.as_str()).shortcut_text(tool.shortcut.as_str());
                if tool.on == Some(true) {
                    item = item.selected(true);
                }
                if ui.add_enabled(tool.enabled, item).clicked() {
                    out = Some(command.clone());
                    ui.close();
                }
            }
        });
        more.response.on_hover_text(t!("toolbar.more"));
    }
    out
}

/// A guess at a button's room before the first one is drawn.
const MIN_GUESS: f32 = 32.0;

/// A toolbar button that opens a menu (`add_menu`) below it; the same look and names as [`button`].
fn menu_button(ui: &mut egui::Ui, tool: &Tool, add_menu: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let tip = &tool.name;
    let selected = tool.on.unwrap_or(false);
    let button = icon::icon_button(tool.icon).frame_when_inactive(selected).selected(selected);
    let (response, _) = ui.add_enabled_ui(tool.enabled, |ui| MenuButton::from_button(button).ui(ui, add_menu)).inner;
    response.widget_info(|| WidgetInfo::labeled(Role::Button, tool.enabled, &tool.name));
    response.on_hover_text(tip).on_disabled_hover_text(tip)
}

/// A toolbar button: its icon, with its name (and shortcut) in its tooltip.
fn button(ui: &mut egui::Ui, tool: &Tool) -> egui::Response {
    let tip = if tool.shortcut.is_empty() { tool.name.to_owned() } else { format!("{} ({})", tool.name, tool.shortcut) };
    let selected = tool.on.unwrap_or(false);
    let button = icon::icon_button(tool.icon).frame_when_inactive(selected).selected(selected);
    let response = ui.add_enabled(tool.enabled, button);
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::Button, tool.enabled, tool.name.clone());
        info.selected = tool.on;
        info
    });
    response.on_hover_text(&tip).on_disabled_hover_text(&tip)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names the toolbar's buttons have for screen readers.
    fn names(state: &ToolbarState<'_>) -> Vec<String> {
        names_in(state, f32::INFINITY)
    }

    /// The same, in a toolbar `width` points wide.
    fn names_in(state: &ToolbarState<'_>, width: f32) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut defs = egui::FontDefinitions::default();
        icon::add_font(&mut defs);
        ctx.set_fonts(defs);
        ctx.enable_accesskit();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            ui.set_max_width(width);
            ui.horizontal(|ui| show(ui, state, &mut String::new()));
        });
        out.textures_delta.clear();
        let update = out.platform_output.accesskit_update.expect("accesskit is on");
        update
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == egui::accesskit::Role::Button)
            .filter_map(|(_, n)| n.label().map(str::to_owned))
            .collect()
    }

    #[test]
    fn every_button_has_a_name() {
        let state = ToolbarState {
            active: None,
            i: 0,
            sidebar: true,
            sftp_panel: false,
            compose_bar: false,
            broadcast: false,
            copy_hint: "Ctrl+Shift+C",
            paste_hint: "Ctrl+Shift+V",
        };
        let names = names(&state);
        let expected = [
            t!("common.hide_the_site_list"),
            t!("common.new_tab"),
            t!("common.quick_connect"),
            t!("common.reconnect"),
            t!("common.disconnect"),
            t!("common.copy"),
            t!("common.paste"),
            t!("common.sftp_explorer"),
            t!("common.settings"),
        ];
        for name in expected {
            assert!(names.iter().any(|n| n == name), "{name} missing from {names:?}");
        }
    }

    /// A narrow window: the toolbar keeps one line, the rest under "»" (#144).
    #[test]
    fn a_narrow_toolbar_moves_buttons_into_a_menu() {
        let state = ToolbarState {
            active: None,
            i: 0,
            sidebar: true,
            sftp_panel: false,
            compose_bar: false,
            broadcast: false,
            copy_hint: "",
            paste_hint: "",
        };
        let wide = names_in(&state, f32::INFINITY);
        assert!(!wide.iter().any(|n| n == "»"), "all fit: {wide:?}");
        let narrow = names_in(&state, 260.0);
        assert!(narrow.iter().any(|n| n == "»"), "the rest in a menu: {narrow:?}");
        assert!(!narrow.iter().any(|n| n == t!("common.settings")), "settings went into the menu: {narrow:?}");
        assert!(narrow.iter().any(|n| n == t!("common.new_tab")), "the first groups stay: {narrow:?}");
    }
}
