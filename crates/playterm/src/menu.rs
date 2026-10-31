//! Menu bar at the top of the window, and the per-session menu shared with the tab context menu.

use eframe::egui::{self, Button};
use playterm_config::TabColor;
use playterm_i18n::t;
use playterm_term::LogMode;

use crate::session::Session;
use crate::sidebar::VaultState;

/// Something the user asked for from the menu bar, a shortcut or a context menu.
/// Commands that take an index act on that tab.
#[derive(Clone)]
pub enum Command {
    QuickConnect,
    NewTab,
    SiteManager,
    NewSite,
    ImportPutty,
    Quit,

    Copy,
    Paste,
    SelectAll,
    Find,
    ClearScrollback,

    ToggleSidebar,
    ToggleStatusBar,
    ToggleComposeBar,
    ToggleFullscreen,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Settings,

    Sftp(usize),
    /// The SFTP explorer sliding in at the right edge, following the active tab.
    ToggleSftpPanel,
    Break(usize),
    StartLog(usize, LogMode),
    StopLog(usize),
    RevealLog(usize),
    OpenLogFolder,
    LockVault,
    UnlockVault,

    Activate(usize),
    NextTab,
    PrevTab,
    Duplicate(usize),
    /// Another playterm window (a new process) with tab `i`'s connection.
    OpenInNewWindow(usize),
    /// Another playterm window with a local prompt.
    NewWindow,
    Reconnect(usize),
    Close(usize),
    CloseOthers(usize),
    CloseAll,
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
}

/// What the menus need to know to enable and check their items.
pub struct MenuState<'a> {
    pub sessions: &'a [Session],
    pub active: usize,
    pub show_sidebar: bool,
    pub show_status_bar: bool,
    pub show_compose_bar: bool,
    pub vault: VaultState,
    pub always_on_top: bool,
    pub broadcast: bool,
    pub prevent_screensaver: bool,
    pub sftp_panel: bool,
}

/// Adds a menu item; a click records `command` and closes the menu.
fn item(ui: &mut egui::Ui, out: &mut Option<Command>, label: &str, shortcut: &str, enabled: bool, command: Command) {
    let button = Button::new(label).shortcut_text(shortcut);
    if ui.add_enabled(enabled, button).clicked() {
        *out = Some(command);
        ui.close();
    }
}

/// A menu item with a check mark.
fn check(ui: &mut egui::Ui, out: &mut Option<Command>, label: &str, shortcut: &str, checked: bool, command: Command) {
    let mark = if checked { "✔" } else { "  " };
    if ui.add(Button::new(format!("{mark} {label}")).shortcut_text(shortcut)).clicked() {
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
    ui.menu_button(t("탭 색상", "Tab color"), |ui| {
        if ui.add(Button::selectable(current.is_none(), t("없음", "None"))).clicked() {
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

/// SFTP, BREAK and logging for tab `i`. Used by the tools menu and the tab context menu.
pub fn session_items(ui: &mut egui::Ui, out: &mut Option<Command>, i: usize, s: &Session) {
    item(ui, out, t("SFTP 파일 브라우저", "SFTP file browser"), "Ctrl+Shift+E", s.can_sftp(), Command::Sftp(i));
    item(ui, out, t("BREAK 신호 보내기", "Send BREAK"), "", s.can_break(), Command::Break(i));
    ui.menu_button(t("로그", "Log"), |ui| {
        if s.terminal.log().is_some() {
            item(ui, out, t("로그 기록 중지", "Stop logging"), "", true, Command::StopLog(i));
            item(ui, out, t("로그 파일 위치 열기", "Show log file"), "", true, Command::RevealLog(i));
        } else {
            let text = Command::StartLog(i, LogMode::Text { timestamps: true });
            item(ui, out, t("텍스트 로그 시작…", "Start text log…"), "", true, text);
            item(ui, out, t("원본(raw) 로그 시작…", "Start raw log…"), "", true, Command::StartLog(i, LogMode::Raw));
        }
        item(ui, out, t("로그 폴더 열기", "Open log folder"), "", true, Command::OpenLogFolder);
    });
}

pub fn menu_bar(ui: &mut egui::Ui, st: &MenuState<'_>) -> Option<Command> {
    let mut out = None;
    let out = &mut out;
    let n = st.sessions.len();
    let active = st.sessions.get(st.active);
    let i = st.active;
    let (maximized, fullscreen) =
        ui.input(|inp| (inp.viewport().maximized.unwrap_or(false), inp.viewport().fullscreen.unwrap_or(false)));

    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button(t("파일", "File"), |ui| {
            item(ui, out, t("새 탭", "New tab"), "Ctrl+Shift+N", true, Command::NewTab);
            item(ui, out, t("새 창", "New window"), "", true, Command::NewWindow);
            item(ui, out, t("빠른 연결…", "Quick Connect…"), "Ctrl+Shift+T", true, Command::QuickConnect);
            item(ui, out, t("사이트 관리…", "Site Manager…"), "Ctrl+Shift+S", true, Command::SiteManager);
            item(ui, out, t("새 사이트…", "New site…"), "", true, Command::NewSite);
            item(ui, out, t("PuTTY 세션 가져오기…", "Import PuTTY sessions…"), "", true, Command::ImportPutty);
            ui.separator();
            item(ui, out, t("끝내기", "Exit"), "Alt+F4", true, Command::Quit);
        });

        ui.menu_button(t("편집", "Edit"), |ui| {
            let has_selection = active.is_some_and(|s| s.terminal.has_selection());
            item(ui, out, t("복사", "Copy"), "Ctrl+Shift+C", has_selection, Command::Copy);
            item(ui, out, t("붙여넣기", "Paste"), "Ctrl+Shift+V", active.is_some(), Command::Paste);
            item(ui, out, t("모두 선택", "Select all"), "", active.is_some(), Command::SelectAll);
            item(ui, out, t("찾기…", "Find…"), "Ctrl+Shift+F", active.is_some(), Command::Find);
            ui.separator();
            item(ui, out, t("스크롤백 지우기", "Clear scrollback"), "", active.is_some(), Command::ClearScrollback);
        });

        ui.menu_button(t("보기", "View"), |ui| {
            check(ui, out, t("사이트 목록", "Site list"), "Ctrl+Shift+B", st.show_sidebar, Command::ToggleSidebar);
            check(ui, out, t("상태 표시줄", "Status bar"), "", st.show_status_bar, Command::ToggleStatusBar);
            check(
                ui,
                out,
                t("명령 입력창", "Command input bar"),
                "Ctrl+Shift+M",
                st.show_compose_bar,
                Command::ToggleComposeBar,
            );
            check(ui, out, t("전체 화면", "Full screen"), "F11", fullscreen, Command::ToggleFullscreen);
            ui.separator();
            item(ui, out, t("글꼴 크게", "Larger font"), "Ctrl+=", true, Command::ZoomIn);
            item(ui, out, t("글꼴 작게", "Smaller font"), "Ctrl+-", true, Command::ZoomOut);
            item(ui, out, t("기본 크기", "Default size"), "Ctrl+0", true, Command::ZoomReset);
        });

        ui.menu_button(t("도구", "Tools"), |ui| {
            match active {
                Some(s) => session_items(ui, out, i, s),
                None => {
                    item(ui, out, t("SFTP 파일 브라우저", "SFTP file browser"), "Ctrl+Shift+E", false, Command::Sftp(i));
                    item(ui, out, t("BREAK 신호 보내기", "Send BREAK"), "", false, Command::Break(i));
                    item(ui, out, t("로그 폴더 열기", "Open log folder"), "", true, Command::OpenLogFolder);
                }
            }
            check(
                ui,
                out,
                t("SFTP 탐색기 (옆 패널)", "SFTP explorer (side panel)"),
                "Ctrl+Shift+X",
                st.sftp_panel,
                Command::ToggleSftpPanel,
            );
            ui.separator();
            match st.vault {
                VaultState::Missing => {
                    item(ui, out, t("비밀번호 볼트 없음", "No password vault"), "", false, Command::UnlockVault)
                }
                VaultState::Locked => {
                    item(ui, out, t("비밀번호 볼트 잠금 해제…", "Unlock password vault…"), "", true, Command::UnlockVault)
                }
                VaultState::Unlocked => {
                    item(ui, out, t("비밀번호 볼트 잠그기", "Lock password vault"), "", true, Command::LockVault)
                }
            }
            ui.separator();
            check(
                ui,
                out,
                t("화면보호기 진입 방지", "Prevent screen saver"),
                "",
                st.prevent_screensaver,
                Command::TogglePreventScreensaver,
            );
            ui.separator();
            item(ui, out, t("설정…", "Settings…"), "", true, Command::Settings);
        });

        ui.menu_button(t("탭", "Tab"), |ui| {
            item(ui, out, t("새 탭", "New tab"), "Ctrl+Shift+N", true, Command::NewTab);
            ui.separator();
            item(ui, out, t("다음 탭", "Next tab"), "Ctrl+Tab", n > 1, Command::NextTab);
            item(ui, out, t("이전 탭", "Previous tab"), "Ctrl+Shift+Tab", n > 1, Command::PrevTab);
            ui.separator();
            item(ui, out, t("탭 복제", "Duplicate tab"), "Ctrl+Shift+D", active.is_some(), Command::Duplicate(i));
            item(ui, out, t("새 창으로 열기", "Open in new window"), "", active.is_some(), Command::OpenInNewWindow(i));
            if let Some(s) = active {
                tab_color_menu(ui, out, i, s.tab_color);
            }
            let can_reconnect = active.is_some_and(|s| s.has_target());
            item(ui, out, t("다시 연결", "Reconnect"), "", can_reconnect, Command::Reconnect(i));
            ui.separator();
            item(ui, out, t("탭 닫기", "Close tab"), "Ctrl+Shift+W", active.is_some(), Command::Close(i));
            item(ui, out, t("다른 탭 모두 닫기", "Close other tabs"), "", n > 1, Command::CloseOthers(i));
            item(ui, out, t("모든 탭 닫기", "Close all tabs"), "", n > 0, Command::CloseAll);
            ui.separator();
            check(
                ui,
                out,
                t("모든 탭에 동시 입력", "Type into all tabs"),
                "Ctrl+Shift+I",
                st.broadcast,
                Command::ToggleBroadcast,
            );
            if n > 0 {
                ui.separator();
                for (k, s) in st.sessions.iter().enumerate() {
                    if ui.add(Button::selectable(k == i, s.tab_title())).clicked() {
                        *out = Some(Command::Activate(k));
                        ui.close();
                    }
                }
            }
        });

        ui.menu_button(t("창", "Window"), |ui| {
            item(ui, out, t("최소화", "Minimize"), "", true, Command::Minimize);
            let label = if maximized { t("이전 크기로", "Restore") } else { t("최대화", "Maximize") };
            item(ui, out, label, "", !fullscreen, Command::ToggleMaximize);
            check(ui, out, t("전체 화면", "Full screen"), "F11", fullscreen, Command::ToggleFullscreen);
            check(ui, out, t("항상 위에 표시", "Always on top"), "", st.always_on_top, Command::ToggleAlwaysOnTop);
        });

        ui.menu_button(t("도움말", "Help"), |ui| {
            item(ui, out, t("단축키", "Keyboard shortcuts"), "", true, Command::Shortcuts);
            ui.separator();
            item(ui, out, t("playterm 정보", "About playterm"), "", true, Command::About);
        });
    });
    out.take()
}
