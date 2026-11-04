//! The toolbar under the menu bar: the commands used most, as icons (as in Xshell). Each shows
//! its name and shortcut in a tooltip, and has that name for screen readers.

use eframe::egui::{self, Role, WidgetInfo};
use playterm_i18n::t;

use crate::icon::{self, MaterialIcon};
use crate::menu::{Command, keys};
use crate::session::{Session, SessionState};

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

/// Draws the toolbar's buttons in `ui` (a horizontal layout), returning the one clicked.
pub fn show(ui: &mut egui::Ui, st: &ToolbarState<'_>) -> Option<Command> {
    let mut out = None;
    let out = &mut out;
    let active = st.active;
    let i = st.i;
    let fullscreen = ui.input(|inp| inp.viewport().fullscreen.unwrap_or(false));

    let (glyph, name) = if st.sidebar {
        (icon::SIDEBAR_HIDE, t("사이트 목록 숨기기", "Hide the site list"))
    } else {
        (icon::SIDEBAR_SHOW, t("사이트 목록 보이기", "Show the site list"))
    };
    tool(ui, out, glyph, name, "Ctrl+Shift+B", true, None, Command::ToggleSidebar);
    ui.separator();

    tool(ui, out, icon::NEW_TAB, t("새 탭", "New tab"), keys("Ctrl+Shift+N", "⌘T"), true, None, Command::NewTab);
    tool(ui, out, icon::QUICK_CONNECT, t("빠른 연결", "Quick Connect"), "Ctrl+Shift+T", true, None, Command::QuickConnect);
    tool(ui, out, icon::SITE_MANAGER, t("사이트 관리", "Site Manager"), "Ctrl+Shift+S", true, None, Command::SiteManager);
    ui.separator();

    // One click on the toolbar must not end a working session: reconnecting is for a closed
    // tab here (the menus still offer it for a connected one).
    let can_reconnect = active.is_some_and(|s| s.has_target() && s.state == SessionState::Closed);
    let can_disconnect = active.is_some_and(Session::can_disconnect);
    tool(ui, out, icon::RECONNECT, t("다시 연결", "Reconnect"), "", can_reconnect, None, Command::Reconnect(i));
    tool(ui, out, icon::DISCONNECT, t("연결 끊기", "Disconnect"), "", can_disconnect, None, Command::Disconnect(i));
    ui.separator();

    let has_selection = active.is_some_and(|s| s.terminal.has_selection());
    tool(ui, out, icon::COPY, t("복사", "Copy"), st.copy_hint, has_selection, None, Command::Copy);
    tool(ui, out, icon::PASTE, t("붙여넣기", "Paste"), st.paste_hint, active.is_some(), None, Command::Paste);
    tool(ui, out, icon::FIND, t("찾기", "Find"), keys("Ctrl+Shift+F", "⌘F"), active.is_some(), None, Command::Find);
    ui.separator();

    tool(ui, out, icon::FONT_SMALLER, t("글꼴 작게", "Smaller font"), keys("Ctrl+-", "⌘-"), true, None, Command::ZoomOut);
    tool(ui, out, icon::FONT_LARGER, t("글꼴 크게", "Larger font"), keys("Ctrl+=", "⌘="), true, None, Command::ZoomIn);
    ui.separator();

    // Open, the SFTP explorer can always be closed from here.
    let sftp = st.sftp_panel || active.is_some_and(|s| s.sftp.is_some() || s.can_sftp());
    tool(ui, out, icon::SFTP, t("SFTP 탐색기", "SFTP explorer"), "Ctrl+Shift+X", sftp, Some(st.sftp_panel), Command::ToggleSftpPanel);
    let compose = (icon::COMPOSE_BAR, t("명령 입력창", "Command input bar"));
    tool(ui, out, compose.0, compose.1, "Ctrl+Shift+M", true, Some(st.compose_bar), Command::ToggleComposeBar);
    let broadcast = (icon::BROADCAST, t("모든 탭에 동시 입력", "Type into all tabs"));
    tool(ui, out, broadcast.0, broadcast.1, "Ctrl+Shift+I", true, Some(st.broadcast), Command::ToggleBroadcast);
    ui.separator();

    let (glyph, name) = if fullscreen {
        (icon::FULLSCREEN_EXIT, t("전체 화면 끝내기", "Exit full screen"))
    } else {
        (icon::FULLSCREEN, t("전체 화면", "Full screen"))
    };
    tool(ui, out, glyph, name, "F11", true, None, Command::ToggleFullscreen);
    tool(ui, out, icon::SETTINGS, t("설정", "Settings"), keys("", "⌘,"), true, None, Command::Settings);
    out.take()
}

/// A toolbar button: `icon`, with `name` (and `shortcut`) in its tooltip. `on` is the state of a
/// switch, drawn as selected while on.
#[allow(clippy::too_many_arguments)]
fn tool(
    ui: &mut egui::Ui,
    out: &mut Option<Command>,
    icon: MaterialIcon,
    name: &str,
    shortcut: &str,
    enabled: bool,
    on: Option<bool>,
    command: Command,
) {
    let tip = if shortcut.is_empty() { name.to_owned() } else { format!("{name} ({shortcut})") };
    let selected = on.unwrap_or(false);
    let button = icon::icon_button(icon).frame_when_inactive(selected).selected(selected);
    let response = ui.add_enabled(enabled, button);
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::Button, enabled, name);
        info.selected = on;
        info
    });
    if response.on_hover_text(&tip).on_disabled_hover_text(&tip).clicked() {
        *out = Some(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names the toolbar's buttons have for screen readers.
    fn names(state: &ToolbarState<'_>) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut defs = egui::FontDefinitions::default();
        icon::add_font(&mut defs);
        ctx.set_fonts(defs);
        ctx.enable_accesskit();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            ui.horizontal(|ui| show(ui, state));
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
            t("사이트 목록 숨기기", "Hide the site list"),
            t("새 탭", "New tab"),
            t("빠른 연결", "Quick Connect"),
            t("다시 연결", "Reconnect"),
            t("연결 끊기", "Disconnect"),
            t("복사", "Copy"),
            t("붙여넣기", "Paste"),
            t("SFTP 탐색기", "SFTP explorer"),
            t("설정", "Settings"),
        ];
        for name in expected {
            assert!(names.iter().any(|n| n == name), "{name} missing from {names:?}");
        }
    }
}
