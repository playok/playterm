//! Menu bar at the top of the window, and the per-session menu shared with the tab context menu.

use eframe::egui::{self, Button};
use playterm_config::TabColor;
use playterm_i18n::{t, tr};
use playterm_term::LogMode;

use crate::keys::{Shortcut, hint};
use crate::session::Session;
use crate::sidebar::VaultState;

/// Something the user asked for from the menu bar, a shortcut or a context menu.
/// Commands that take an index act on that tab.
#[derive(Clone)]
pub enum Command {
    QuickConnect,
    /// Connect this saved site (start screen).
    ConnectSite(String),
    NewTab,
    SiteManager,
    /// The site manager on this saved site (the connection line above a terminal).
    EditSite(String),
    NewSite,
    ImportPutty,
    ImportXshell,
    ImportSecureCrt,
    Quit,

    Copy,
    Paste,
    SelectAll,
    Find,
    ClearScrollback,

    ToggleSidebar,
    ToggleToolbar,
    ToggleStatusBar,
    ToggleComposeBar,
    ToggleFullscreen,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Settings,
    TrustedHosts,

    /// The SFTP explorer sliding in at the right edge, following the active tab.
    ToggleSftpPanel,
    Break(usize),
    StartLog(usize, LogMode),
    StopLog(usize),
    RevealLog(usize),
    OpenLogFolder,
    LockVault,
    SetUpVault,
    UnlockVault,

    Activate(usize),
    NextTab,
    PrevTab,
    Duplicate(usize),
    /// Moves tab `i`, connection and all, to a new window of its own.
    OpenInNewWindow(usize),
    /// Moves tab `i` from the window it was moved out to back to the main window.
    MoveToMain(usize),
    /// Another playterm window (a new process) with a local prompt.
    NewWindow,
    Reconnect(usize),
    /// Ends tab `i`'s connection; the tab stays, to reconnect.
    Disconnect(usize),
    Close(usize),
    CloseOthers(usize),
    CloseAll,
    /// Opens the tab closed last again.
    ReopenClosed,
    /// Simultaneous input: what is typed in the active tab also goes to the other tabs.
    ToggleBroadcast,
    /// Whether this tab receives simultaneous input.
    ToggleInBroadcast(usize),
    SetTabColor(usize, Option<TabColor>),

    TogglePreventScreensaver,

    Minimize,
    ToggleMaximize,
    ToggleAlwaysOnTop,

    Shortcuts,
    About,
    /// The project's GitHub page (questions, bug reports).
    ProjectPage,
    Licenses,
    CheckUpdates,
    /// The dialog of the newer release found earlier.
    ShowUpdate,
    /// Run macro `k` (of the settings) in the active tab and the tabs receiving simultaneous input.
    RunMacro(usize),
    StopMacro,
    Macros,
}

/// What the menus need to know to enable and check their items.
pub struct MenuState<'a> {
    pub sessions: &'a [Session],
    pub active: usize,
    pub show_sidebar: bool,
    pub show_toolbar: bool,
    pub show_status_bar: bool,
    pub show_compose_bar: bool,
    pub vault: VaultState,
    pub always_on_top: bool,
    pub broadcast: bool,
    pub prevent_screensaver: bool,
    pub sftp_panel: bool,
    /// Keys shown next to Copy and Paste.
    pub copy_hint: &'static str,
    pub paste_hint: &'static str,
    /// A newer release that was found and not installed or skipped.
    pub update: Option<String>,
    pub macros: &'a [playterm_config::Macro],
    /// A closed tab can be opened again.
    pub can_reopen: bool,
}

/// Adds a menu item; a click records `command` and closes the menu.
/// The shortcut shown in a menu: the Windows one, or the Mac one on macOS.
pub(crate) fn keys(other: &'static str, mac: &'static str) -> &'static str {
    if cfg!(target_os = "macos") { mac } else { other }
}

fn item(ui: &mut egui::Ui, out: &mut Option<Command>, label: &str, shortcut: &str, enabled: bool, command: Command) {
    let button = Button::new((lead(ui, false), label)).shortcut_text(shortcut);
    if ui.add_enabled(enabled, button).clicked() {
        *out = Some(command);
        ui.close();
    }
}

/// The slot in front of a menu item's text: a check mark, or the same space left blank, so
/// every item's text starts at the same place (#135).
pub(crate) fn lead(ui: &egui::Ui, checked: bool) -> egui::RichText {
    let color = if checked { ui.visuals().strong_text_color() } else { egui::Color32::TRANSPARENT };
    crate::icon::OK.rich_text().color(color)
}

/// A submenu inside a menu, its title lined up with the items around it.
pub(crate) fn submenu<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) {
    let lead = lead(ui, false);
    ui.menu_button((lead, title), add);
}

/// A menu item with a check mark.
fn check(ui: &mut egui::Ui, out: &mut Option<Command>, label: &str, shortcut: &str, checked: bool, command: Command) {
    if ui.add(Button::new((lead(ui, checked), label)).shortcut_text(shortcut)).clicked() {
        *out = Some(command);
        ui.close();
    }
}

pub fn color32(c: TabColor) -> egui::Color32 {
    let [r, g, b] = c.rgb();
    egui::Color32::from_rgb(r, g, b)
}

/// "Tab color" submenu for tab `i`, whose color is `current`.
pub fn tab_color_menu(ui: &mut egui::Ui, out: &mut Option<Command>, i: usize, current: Option<TabColor>) {
    submenu(ui, t!("common.tab_color"), |ui| {
        if ui.add(Button::selectable(current.is_none(), t!("common.none"))).clicked() {
            *out = Some(Command::SetTabColor(i, None));
            ui.close();
        }
        for c in TabColor::ALL {
            let clicked = ui
                .horizontal(|ui| {
                    ui.label(egui::RichText::new("■").color(color32(c)));
                    ui.add(Button::selectable(current == Some(c), c.name())).clicked()
                })
                .inner;
            if clicked {
                *out = Some(Command::SetTabColor(i, Some(c)));
                ui.close();
            }
        }
    });
}

/// BREAK and logging for tab `i`. Used by the tools menu and the tab context menu.
pub fn session_items(ui: &mut egui::Ui, out: &mut Option<Command>, i: usize, s: &Session) {
    item(ui, out, t!("menu.send_break"), "", s.can_break(), Command::Break(i));
    submenu(ui, t!("menu.log"), |ui| {
        if s.terminal.log().is_some() {
            item(ui, out, t!("menu.stop_logging"), "", true, Command::StopLog(i));
            item(ui, out, t!("menu.show_log_file"), "", true, Command::RevealLog(i));
        } else {
            let text = Command::StartLog(i, LogMode::Text { timestamps: true });
            item(ui, out, t!("menu.start_text_log"), "", true, text);
            item(ui, out, t!("menu.start_raw_log"), "", true, Command::StartLog(i, LogMode::Raw));
        }
        item(ui, out, t!("menu.open_log_folder"), "", true, Command::OpenLogFolder);
    });
}

pub fn menu_bar(ui: &mut egui::Ui, st: &MenuState<'_>) -> Option<Command> {
    let mut out = None;
    let out = &mut out;
    let n = st.sessions.len();
    let active = st.sessions.get(st.active);
    let i = st.active;
    let (maximized, fullscreen) = ui.input(|inp| (inp.viewport().maximized.unwrap_or(false), inp.viewport().fullscreen.unwrap_or(false)));

    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button(t!("menu.file"), |ui| {
            item(ui, out, t!("common.new_tab"), &hint(Shortcut::NewTab), true, Command::NewTab);
            item(ui, out, t!("common.new_window"), &hint(Shortcut::NewWindow), true, Command::NewWindow);
            item(ui, out, t!("menu.quick_connect"), &hint(Shortcut::QuickConnect), true, Command::QuickConnect);
            item(ui, out, t!("menu.site_manager"), &hint(Shortcut::SiteManager), true, Command::SiteManager);
            item(ui, out, t!("menu.new_site"), "", true, Command::NewSite);
            submenu(ui, t!("menu.import"), |ui| {
                item(ui, out, t!("menu.putty_sessions"), "", true, Command::ImportPutty);
                item(ui, out, t!("menu.xshell_sessions"), "", true, Command::ImportXshell);
                item(ui, out, t!("menu.securecrt_sessions"), "", true, Command::ImportSecureCrt);
            });
            ui.separator();
            item(ui, out, t!("menu.exit"), keys("Alt+F4", "⌘Q"), true, Command::Quit);
        });

        ui.menu_button(t!("menu.edit"), |ui| {
            let has_selection = active.is_some_and(|s| s.terminal.has_selection());
            item(ui, out, t!("common.copy"), st.copy_hint, has_selection, Command::Copy);
            item(ui, out, t!("common.paste"), st.paste_hint, active.is_some(), Command::Paste);
            item(ui, out, t!("common.select_all"), "", active.is_some(), Command::SelectAll);
            item(ui, out, t!("menu.find"), &hint(Shortcut::Find), active.is_some(), Command::Find);
            ui.separator();
            item(ui, out, t!("menu.clear_scrollback"), "", active.is_some(), Command::ClearScrollback);
        });

        ui.menu_button(t!("menu.view"), |ui| {
            check(ui, out, t!("menu.site_list"), &hint(Shortcut::Sidebar), st.show_sidebar, Command::ToggleSidebar);
            check(ui, out, t!("menu.toolbar"), "", st.show_toolbar, Command::ToggleToolbar);
            check(ui, out, t!("menu.status_bar"), "", st.show_status_bar, Command::ToggleStatusBar);
            check(ui, out, t!("common.command_input_bar"), &hint(Shortcut::ComposeBar), st.show_compose_bar, Command::ToggleComposeBar);
            check(ui, out, t!("common.full_screen"), &hint(Shortcut::Fullscreen), fullscreen, Command::ToggleFullscreen);
            ui.separator();
            item(ui, out, t!("common.larger_font"), &hint(Shortcut::ZoomIn), true, Command::ZoomIn);
            item(ui, out, t!("common.smaller_font"), &hint(Shortcut::ZoomOut), true, Command::ZoomOut);
            item(ui, out, t!("menu.default_size"), &hint(Shortcut::ZoomReset), true, Command::ZoomReset);
        });

        ui.menu_button(t!("menu.tools"), |ui| {
            match active {
                Some(s) => session_items(ui, out, i, s),
                None => {
                    item(ui, out, t!("menu.send_break"), "", false, Command::Break(i));
                    item(ui, out, t!("menu.open_log_folder"), "", true, Command::OpenLogFolder);
                }
            }
            check(ui, out, t!("menu.sftp_explorer_side_panel"), &hint(Shortcut::SftpPanel), st.sftp_panel, Command::ToggleSftpPanel);
            submenu(ui, t!("common.macros"), |ui| {
                let connected = active.is_some_and(|s| s.state == crate::session::SessionState::Connected);
                for (k, m) in st.macros.iter().enumerate() {
                    item(ui, out, &m.name, "", connected, Command::RunMacro(k));
                }
                if st.macros.is_empty() {
                    ui.weak(t!("common.no_macros_saved"));
                }
                ui.separator();
                let running = active.is_some_and(|s| s.macro_name().is_some());
                item(ui, out, t!("menu.stop_the_running_macro"), "", running, Command::StopMacro);
                item(ui, out, t!("common.manage_macros"), "", true, Command::Macros);
            });
            ui.separator();
            match st.vault {
                VaultState::Missing => item(ui, out, t!("menu.set_up_saved_passwords"), "", true, Command::SetUpVault),
                VaultState::Locked => item(ui, out, t!("menu.unlock_saved_passwords"), "", true, Command::UnlockVault),
                VaultState::Unlocked => item(ui, out, t!("menu.lock_saved_passwords"), "", true, Command::LockVault),
            }
            if crate::power::SUPPORTED {
                ui.separator();
                check(ui, out, t!("menu.prevent_screen_saver"), "", st.prevent_screensaver, Command::TogglePreventScreensaver);
            }
            ui.separator();
            item(ui, out, t!("menu.remembered_host_keys_and_certificates"), "", true, Command::TrustedHosts);
            item(ui, out, t!("menu.settings"), &hint(Shortcut::Settings), true, Command::Settings);
        });

        ui.menu_button(t!("menu.tab"), |ui| {
            item(ui, out, t!("common.new_tab"), &hint(Shortcut::NewTab), true, Command::NewTab);
            ui.separator();
            item(ui, out, t!("common.next_tab"), &hint(Shortcut::NextTab), n > 1, Command::NextTab);
            item(ui, out, t!("common.previous_tab"), &hint(Shortcut::PrevTab), n > 1, Command::PrevTab);
            ui.separator();
            item(ui, out, t!("common.duplicate_tab"), &hint(Shortcut::DuplicateTab), active.is_some(), Command::Duplicate(i));
            item(ui, out, t!("common.move_to_new_window"), "", active.is_some(), Command::OpenInNewWindow(i));
            if let Some(s) = active {
                tab_color_menu(ui, out, i, s.tab_color);
            }
            let can_reconnect = active.is_some_and(|s| s.has_target());
            item(ui, out, t!("common.reconnect"), "", can_reconnect, Command::Reconnect(i));
            let can_disconnect = active.is_some_and(Session::can_disconnect);
            item(ui, out, t!("common.disconnect"), "", can_disconnect, Command::Disconnect(i));
            ui.separator();
            item(ui, out, t!("common.close_tab"), &hint(Shortcut::CloseTab), active.is_some(), Command::Close(i));
            item(ui, out, t!("common.close_other_tabs"), "", n > 1, Command::CloseOthers(i));
            item(ui, out, t!("menu.close_all_tabs"), "", n > 0, Command::CloseAll);
            item(ui, out, t!("common.reopen_closed_tab"), &hint(Shortcut::ReopenClosed), st.can_reopen, Command::ReopenClosed);
            ui.separator();
            check(ui, out, t!("common.type_into_all_tabs"), &hint(Shortcut::Broadcast), st.broadcast, Command::ToggleBroadcast);
            if n > 0 {
                ui.separator();
                for (k, title) in Session::numbered_titles(st.sessions).into_iter().enumerate() {
                    if ui.add(Button::new((lead(ui, k == i), title))).clicked() {
                        *out = Some(Command::Activate(k));
                        ui.close();
                    }
                }
            }
        });

        ui.menu_button(t!("menu.window"), |ui| {
            item(ui, out, t!("menu.minimize"), "", true, Command::Minimize);
            let label = if maximized { t!("menu.restore") } else { t!("menu.maximize") };
            item(ui, out, label, "", !fullscreen, Command::ToggleMaximize);
            check(ui, out, t!("common.full_screen"), &hint(Shortcut::Fullscreen), fullscreen, Command::ToggleFullscreen);
            check(ui, out, t!("menu.always_on_top"), "", st.always_on_top, Command::ToggleAlwaysOnTop);
        });

        ui.menu_button(t!("menu.help"), |ui| {
            item(ui, out, t!("common.keyboard_shortcuts"), "", true, Command::Shortcuts);
            ui.separator();
            item(ui, out, t!("menu.about_playterm"), "", true, Command::About);
            item(ui, out, t!("common.open_source_licenses"), "", true, Command::Licenses);
            item(ui, out, t!("common.questions_and_bug_reports_github"), "", true, Command::ProjectPage);
            ui.separator();
            item(ui, out, t!("menu.check_for_updates"), "", true, Command::CheckUpdates);
        });

        if let Some(version) = &st.update {
            let text = egui::RichText::new(tr!("menu.new_version", version = version)).color(ui.visuals().selection.stroke.color);
            if ui.button(text).on_hover_text(t!("menu.install_the_update")).clicked() {
                *out = Some(Command::ShowUpdate);
            }
        }
    });
    out.take()
}
