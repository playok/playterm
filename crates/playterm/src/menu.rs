//! Menu bar at the top of the window, and the per-session menu shared with the tab context menu.

use eframe::egui::{self, Button};
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
    Quit,

    Copy,
    Paste,
    SelectAll,
    ClearScrollback,

    ToggleSidebar,
    ToggleFullscreen,

    Sftp(usize),
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
    Reconnect(usize),
    Close(usize),
    CloseOthers(usize),
    CloseAll,

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
    pub vault: VaultState,
    pub always_on_top: bool,
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

/// SFTP, BREAK and logging for tab `i`. Used by the tools menu and the tab context menu.
pub fn session_items(ui: &mut egui::Ui, out: &mut Option<Command>, i: usize, s: &Session) {
    item(ui, out, "SFTP 파일 브라우저", "Ctrl+Shift+F", s.can_sftp(), Command::Sftp(i));
    item(ui, out, "BREAK 신호 보내기", "", s.can_break(), Command::Break(i));
    ui.menu_button("로그", |ui| {
        if s.terminal.log().is_some() {
            item(ui, out, "로그 기록 중지", "", true, Command::StopLog(i));
            item(ui, out, "로그 파일 위치 열기", "", true, Command::RevealLog(i));
        } else {
            item(ui, out, "텍스트 로그 시작…", "", true, Command::StartLog(i, LogMode::Text { timestamps: true }));
            item(ui, out, "원본(raw) 로그 시작…", "", true, Command::StartLog(i, LogMode::Raw));
        }
        item(ui, out, "로그 폴더 열기", "", true, Command::OpenLogFolder);
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
        ui.menu_button("파일", |ui| {
            item(ui, out, "새 탭", "Ctrl+Shift+N", true, Command::NewTab);
            item(ui, out, "빠른 연결…", "Ctrl+Shift+T", true, Command::QuickConnect);
            item(ui, out, "사이트 관리…", "Ctrl+Shift+S", true, Command::SiteManager);
            item(ui, out, "새 사이트…", "", true, Command::NewSite);
            ui.separator();
            item(ui, out, "끝내기", "Alt+F4", true, Command::Quit);
        });

        ui.menu_button("편집", |ui| {
            let has_selection = active.is_some_and(|s| s.terminal.has_selection());
            item(ui, out, "복사", "Ctrl+Shift+C", has_selection, Command::Copy);
            item(ui, out, "붙여넣기", "Ctrl+Shift+V", active.is_some(), Command::Paste);
            item(ui, out, "모두 선택", "", active.is_some(), Command::SelectAll);
            ui.separator();
            item(ui, out, "스크롤백 지우기", "", active.is_some(), Command::ClearScrollback);
        });

        ui.menu_button("보기", |ui| {
            check(ui, out, "사이트 목록", "Ctrl+Shift+B", st.show_sidebar, Command::ToggleSidebar);
            check(ui, out, "전체 화면", "F11", fullscreen, Command::ToggleFullscreen);
        });

        ui.menu_button("도구", |ui| {
            match active {
                Some(s) => session_items(ui, out, i, s),
                None => {
                    item(ui, out, "SFTP 파일 브라우저", "Ctrl+Shift+F", false, Command::Sftp(i));
                    item(ui, out, "BREAK 신호 보내기", "", false, Command::Break(i));
                    item(ui, out, "로그 폴더 열기", "", true, Command::OpenLogFolder);
                }
            }
            ui.separator();
            match st.vault {
                VaultState::Missing => item(ui, out, "비밀번호 볼트 없음", "", false, Command::UnlockVault),
                VaultState::Locked => item(ui, out, "비밀번호 볼트 잠금 해제…", "", true, Command::UnlockVault),
                VaultState::Unlocked => item(ui, out, "비밀번호 볼트 잠그기", "", true, Command::LockVault),
            }
        });

        ui.menu_button("탭", |ui| {
            item(ui, out, "새 탭", "Ctrl+Shift+N", true, Command::NewTab);
            ui.separator();
            item(ui, out, "다음 탭", "Ctrl+Tab", n > 1, Command::NextTab);
            item(ui, out, "이전 탭", "Ctrl+Shift+Tab", n > 1, Command::PrevTab);
            ui.separator();
            item(ui, out, "탭 복제", "Ctrl+Shift+D", active.is_some(), Command::Duplicate(i));
            item(ui, out, "다시 연결", "", active.is_some_and(|s| s.has_target()), Command::Reconnect(i));
            ui.separator();
            item(ui, out, "탭 닫기", "Ctrl+Shift+W", active.is_some(), Command::Close(i));
            item(ui, out, "다른 탭 모두 닫기", "", n > 1, Command::CloseOthers(i));
            item(ui, out, "모든 탭 닫기", "", n > 0, Command::CloseAll);
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

        ui.menu_button("창", |ui| {
            item(ui, out, "최소화", "", true, Command::Minimize);
            let label = if maximized { "이전 크기로" } else { "최대화" };
            item(ui, out, label, "", !fullscreen, Command::ToggleMaximize);
            check(ui, out, "전체 화면", "F11", fullscreen, Command::ToggleFullscreen);
            check(ui, out, "항상 위에 표시", "", st.always_on_top, Command::ToggleAlwaysOnTop);
        });

        ui.menu_button("도움말", |ui| {
            item(ui, out, "단축키", "", true, Command::Shortcuts);
            ui.separator();
            item(ui, out, "playterm 정보", "", true, Command::About);
        });
    });
    out.take()
}
