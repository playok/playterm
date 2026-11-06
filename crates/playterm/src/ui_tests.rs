//! Screen flows of the whole app, driven as a user would through egui_kittest: widgets found by
//! their labels (as screen readers see them), clicked and typed into, and the app's state
//! checked after each step.
//!
//! OS dialogs (file pickers) are not driven here; the flows that need one are left out.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, Node};

use crate::app::{MAIN_WINDOW, PlaytermApp};
use crate::menu::Command;
use crate::session::SessionState;

type App = Harness<'static, PlaytermApp>;

/// The app with its own settings folder (Korean, no update check).
fn app(dir: &Path) -> App {
    std::fs::write(dir.join("settings.toml"), "check_updates = false\nlanguage = \"ko\"\n").unwrap();
    let dir = dir.to_owned();
    // The window's fonts come from the system through eframe, which the harness does not run:
    // Hangul is drawn as boxes here (widgets are found by their labels, not pixels).
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .allow_missing_glyphs()
        .build_eframe(move |cc| PlaytermApp::new_in(cc, Vec::new(), &dir).expect("the app starts"));
    harness.run_steps(4);
    harness
}

/// A widget's text as a user reads it: its label without the icon in front (icons are drawn
/// from the private-use area of the icon font).
fn label(node: &Node<'_>) -> String {
    let label = node.accesskit_node().label().unwrap_or_default();
    match label.chars().next() {
        Some(c) if ('\u{E000}'..='\u{F8FF}').contains(&c) => label[c.len_utf8()..].trim_start().to_owned(),
        _ => label,
    }
}

/// A button whose text is `text` (after its icon, if it has one).
fn button<'t>(harness: &'t App, text: &str) -> Node<'t> {
    harness.query_all_by_role(Role::Button).find(|n| label(n) == text).unwrap_or_else(|| panic!("no button {text:?}"))
}

fn has_button(harness: &App, text: &str) -> bool {
    harness.query_all_by_role(Role::Button).any(|n| label(&n) == text)
}

/// Opens menu `menu` and clicks its item `item` (menu items end with their shortcut, which
/// differs between Windows and macOS).
fn menu(harness: &mut App, menu: &str, item: &str) {
    click(harness, menu);
    menu_item(harness, item);
}

/// Clicks item `item` of the open menu (or submenu).
fn menu_item(harness: &mut App, item: &str) {
    harness
        .query_all_by_role(Role::Button)
        // A check item reads "✔ item" when on, "   item" when off.
        .find(|n| label(n).trim_start_matches(['✔', ' ']).starts_with(item))
        .unwrap_or_else(|| {
            let all: Vec<String> = harness.query_all_by_role(Role::Button).map(|n| format!("{:?}", n.accesskit_node().label())).collect();
            panic!("no menu item {item:?}: {all:?}")
        })
        .click();
    harness.run_steps(3);
}

/// Clicks, then lets the app draw a few frames.
fn click(harness: &mut App, text: &str) {
    button(harness, text).click();
    harness.run_steps(3);
}

/// Replaces the text of a text field.
fn type_into(harness: &App, node: &Node<'_>, text: &str) {
    node.focus();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    node.type_text(text);
}

/// A click at a point of the window (not on a widget found by label).
fn click_at(harness: &mut App, pos: egui::Pos2) {
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    harness.run_steps(3);
}

fn press(harness: &mut App, key: egui::Key) {
    harness.key_press(key);
    harness.run_steps(3);
}

/// The combo box showing `value`.
fn combo<'t>(harness: &'t App, value: &str) -> Node<'t> {
    harness
        .query_all_by_role(Role::ComboBox)
        .find(|n| n.accesskit_node().value().as_deref() == Some(value))
        .unwrap_or_else(|| panic!("no combo box showing {value:?}"))
}

/// Whether some text on the screen reads `text`.
fn has_label_text(harness: &App, text: &str) -> bool {
    harness.query_all_by_role(Role::Label).any(|n| n.accesskit_node().value().as_deref() == Some(text))
}

/// The text fields of the site manager's window, in order: the tree's search, the site's name,
/// folder, tab label, host, port, user, ...
fn site_fields(harness: &App) -> Vec<Node<'_>> {
    harness.get_by_role_and_label(Role::Window, "사이트 관리").query_all_by_role(Role::TextInput).collect()
}

/// Where the open dialog (or window `title`) is, after the app has drawn some frames: it must
/// not move or grow by itself.
fn settles(harness: &mut App, what: &str, rect: impl Fn(&App) -> egui::Rect) {
    harness.run_steps(5);
    let first = rect(harness);
    harness.run_steps(20);
    let later = rect(harness);
    assert!(
        (first.min - later.min).length() < 0.5 && (first.size() - later.size()).length() < 0.5,
        "{what} moved by itself: {first:?} -> {later:?}"
    );
}

/// The rectangle around a dialog's content: from its first to its last widget.
fn dialog_rect(harness: &App) -> egui::Rect {
    let dialog = harness.get_by_role(Role::Dialog);
    dialog
        .query_all_by(|n| matches!(n.role(), Role::Button | Role::TextInput | Role::ComboBox | Role::CheckBox))
        .map(|n| n.rect())
        .reduce(|a, b| a.union(b))
        .expect("a dialog with widgets")
}

/// Runs frames until `done`, with time passing (for network replies).
fn run_until(harness: &mut App, what: &str, mut done: impl FnMut(&mut App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(harness) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A device on this PC that speaks plain Telnet: shows `router#`, answers each line with
/// `out:<line>`. Returns its port.
fn fake_device() -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        // Every connection made to it (a tab opened again connects anew).
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            std::thread::spawn(move || {
                let _ = stream.write_all(b"router# ");
                let reader = BufReader::new(stream.try_clone().unwrap());
                for line in reader.split(b'\r').map_while(Result::ok) {
                    let line = String::from_utf8_lossy(&line).trim_matches(['\n', '\0']).to_owned();
                    if stream.write_all(format!("\r\nout:{line}\r\nrouter# ").as_bytes()).is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

/// Connects the start screen's tab to a fake Telnet device through Quick Connect.
fn connect_telnet(harness: &mut App, port: u16) {
    click(harness, "빠른 연결");
    let dialog = harness.get_by_role(Role::Dialog);
    dialog.get_by_role_and_label(Role::RadioButton, "Telnet").click();
    harness.run_steps(2);
    let dialog = harness.get_by_role(Role::Dialog);
    let inputs: Vec<Node<'_>> = dialog.query_all_by_role(Role::TextInput).collect();
    type_into(harness, &inputs[0], "127.0.0.1");
    type_into(harness, &inputs[1], &port.to_string());
    harness.run_steps(2);
    click(harness, "연결");
    run_until(harness, "the device's prompt", |h| {
        let s = &mut h.state_mut().test_sessions()[0];
        s.state == SessionState::Connected && s.screen_contains("router#")
    });
}

/// Closing a connected tab asks first (#122); a closed tab opens again (#123).
#[test]
fn closing_a_connected_tab_asks_and_it_opens_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    let port = fake_device();
    connect_telnet(&mut harness, port);

    // Asked, and Cancel keeps the connection.
    menu(&mut harness, "탭", "탭 닫기");
    assert!(harness.state().test_modal_open(), "asked before closing");
    assert!(has_label_text(&harness, "연결 1개가 끊어집니다."));
    click(&mut harness, "취소");
    assert!(!harness.state().test_modal_open());
    assert_eq!(harness.state_mut().test_sessions()[0].state, SessionState::Connected, "still connected");

    // Agreed: the tab goes (a fresh prompt takes its place).
    menu(&mut harness, "탭", "탭 닫기");
    click(&mut harness, "닫기");
    assert_eq!(harness.state_mut().test_sessions().len(), 1);
    assert_eq!(harness.state_mut().test_sessions()[0].state, SessionState::Local);

    // Opened again: the same device, connected anew.
    menu(&mut harness, "탭", "닫은 탭 다시 열기");
    run_until(&mut harness, "the device again", |h| {
        h.state_mut().test_sessions().iter_mut().any(|s| s.state == SessionState::Connected && s.screen_contains("router#"))
    });

    // A closed (not connected) tab closes without a question.
    let n = harness.state_mut().test_sessions().len();
    let local = harness.state_mut().test_sessions().iter().position(|s| s.state == SessionState::Local);
    if let Some(i) = local {
        let ctx = harness.ctx.clone();
        harness.state_mut().test_run(&ctx, Command::Close(i));
        harness.run_steps(2);
        assert!(!harness.state().test_modal_open(), "nothing to lose: no question");
        assert_eq!(harness.state_mut().test_sessions().len(), n - 1);
    }

    // Enter is the highlighted "Close".
    let connected = |h: &mut App| h.state_mut().test_sessions().iter().filter(|s| s.state == SessionState::Connected).count();
    let before = connected(&mut harness);
    menu(&mut harness, "탭", "탭 닫기");
    assert!(harness.state().test_modal_open(), "asked again");
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(!harness.state().test_modal_open(), "Enter answered");
    assert_eq!(connected(&mut harness), before - 1, "and closed the tab");
}

/// "Disconnect" is in the tab menu (#117) and ends the connection, keeping the tab.
#[test]
fn the_tab_menu_disconnects() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    let port = fake_device();
    connect_telnet(&mut harness, port);
    menu(&mut harness, "탭", "연결 끊기");
    run_until(&mut harness, "the tab to close its connection", |h| h.state_mut().test_sessions()[0].state == SessionState::Closed);
    assert_eq!(harness.state_mut().test_sessions().len(), 1, "the tab stays");
}

#[test]
fn quick_connect_opens_a_telnet_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    let port = fake_device();

    // The start screen's Quick Connect.
    click(&mut harness, "빠른 연결");
    let dialog = harness.get_by_role(Role::Dialog);
    dialog.get_by_role_and_label(Role::RadioButton, "Telnet").click();
    harness.run_steps(2);
    let dialog = harness.get_by_role(Role::Dialog);
    let inputs: Vec<Node<'_>> = dialog.query_all_by_role(Role::TextInput).collect();
    type_into(&harness, &inputs[0], "127.0.0.1");
    type_into(&harness, &inputs[1], &port.to_string());
    harness.run_steps(2);
    click(&mut harness, "연결");

    assert!(!harness.state().test_modal_open(), "the dialog closes");
    run_until(&mut harness, "the device's prompt", |h| {
        let s = &mut h.state_mut().test_sessions()[0];
        s.state == SessionState::Connected && s.screen_contains("router#")
    });
    // The local prompt's tab became the connection.
    assert_eq!(harness.state_mut().test_sessions().len(), 1);
}

#[test]
fn site_manager_saves_asks_and_closes_with_escape() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());

    // New site from the start screen: the manager opens on an empty site.
    click(&mut harness, "새 사이트");
    assert!(harness.state().test_site_manager_open());
    let fields = site_fields(&harness);
    type_into(&harness, &fields[1], "db01");
    type_into(&harness, &fields[4], "10.0.0.5");
    harness.run_steps(2);
    // SSH needs a user: without one the form says so and nothing is saved.
    click(&mut harness, "저장");
    assert!(has_label_text(&harness, "SSH 사용자 이름을 입력하세요"));
    assert!(harness.state().test_store().profiles().is_empty());
    let fields = site_fields(&harness);
    type_into(&harness, &fields[6], "ops");
    harness.run_steps(2);
    click(&mut harness, "저장");
    let sites = harness.state().test_store().profiles().to_vec();
    assert_eq!(sites.len(), 1);
    assert_eq!((sites[0].display_name(), sites[0].host.as_str()), ("db01".to_owned(), "10.0.0.5"));
    assert!(has_label_text(&harness, "db01"), "the sidebar lists it");
    assert!(std::fs::read_to_string(dir.path().join("sessions.toml")).unwrap().contains("10.0.0.5"), "saved to disk");

    // Editing, then going to a new site: asked whether to save the changes first.
    let fields = site_fields(&harness);
    type_into(&harness, &fields[4], "10.0.0.6");
    harness.run_steps(2);
    let window = harness.get_by_role_and_label(Role::Window, "사이트 관리");
    window.query_all_by_role(Role::Button).find(|n| label(n) == "새 사이트").expect("the window's New site").click();
    harness.run_steps(3);
    assert!(has_button(&harness, "저장 안 함"), "asks about the unsaved change");
    click(&mut harness, "저장 안 함");
    assert_eq!(harness.state().test_store().profiles()[0].host, "10.0.0.5", "not saved");

    // With the keyboard in the manager, Escape closes it.
    press(&mut harness, egui::Key::Escape);
    press(&mut harness, egui::Key::Escape);
    assert!(!harness.state().test_site_manager_open(), "Escape closes the manager");

    // Open again, then click the terminal beside it: Escape is the terminal's now.
    // The key as this platform writes it (Ctrl+Shift+S, or ⌃⇧S on macOS).
    let manage = format!("사이트 관리 ({})", crate::keys::hint(crate::keys::Shortcut::SiteManager));
    click(&mut harness, &manage);
    assert!(harness.state().test_site_manager_open());
    let window = harness.get_by_role_and_label(Role::Window, "사이트 관리").rect();
    click_at(&mut harness, egui::pos2(window.center().x, window.max.y + 40.0));
    press(&mut harness, egui::Key::Escape);
    assert!(harness.state().test_site_manager_open(), "Escape in the terminal does not close the manager");
}

#[test]
fn settings_preview_then_cancel_or_keep() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    // The harness has no system theme: "follow the system" shows dark.
    assert_eq!(harness.ctx.theme(), egui::Theme::Dark);

    // Light, previewed at once; Cancel puts it back.
    menu(&mut harness, "도구", "설정…");
    combo(&harness, "시스템 설정 따르기").click();
    harness.run_steps(2);
    click(&mut harness, "라이트");
    assert_eq!(harness.ctx.theme(), egui::Theme::Light, "previewed while the dialog is open");
    click(&mut harness, "취소");
    assert!(!harness.state().test_modal_open());
    assert_eq!(harness.ctx.theme(), egui::Theme::Dark, "Cancel undoes the preview");
    assert_eq!(harness.state().test_settings().ui_theme, playterm_config::UiTheme::System);

    // Light, then OK: kept and saved.
    menu(&mut harness, "도구", "설정…");
    combo(&harness, "시스템 설정 따르기").click();
    harness.run_steps(2);
    click(&mut harness, "라이트");
    click(&mut harness, "확인");
    assert_eq!(harness.ctx.theme(), egui::Theme::Light);
    assert_eq!(harness.state().test_settings().ui_theme, playterm_config::UiTheme::Light);
    let saved = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
    assert!(saved.contains("ui_theme = \"light\""), "{saved}");
}

#[test]
fn macros_are_checked_saved_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    menu(&mut harness, "도구", "매크로");
    menu_item(&mut harness, "매크로 관리…");
    click(&mut harness, "추가");

    let dialog = harness.get_by_role(Role::Dialog);
    let name = dialog.query_all_by_role(Role::TextInput).next().expect("the name field");
    type_into(&harness, &name, "점검");
    let script = dialog.query_all_by_role(Role::MultilineTextInput).next().expect("the script field");
    script.focus();
    script.type_text("show version\n@bogus");
    harness.run_steps(3);
    // A mistake in the script: said, and Save is off.
    assert!(harness.query_all_by_role(Role::Label).any(|n| n.accesskit_node().value().is_some_and(|v| v.contains("@bogus"))));
    assert!(button(&harness, "저장").accesskit_node().is_disabled());

    // Fixed: saved into the settings and listed in the menu (not runnable without a connection).
    let script = harness.get_by_role(Role::Dialog).query_all_by_role(Role::MultilineTextInput).next().unwrap();
    type_into(&harness, &script, "show version\n@wait #");
    harness.run_steps(3);
    assert!(!button(&harness, "저장").accesskit_node().is_disabled());
    click(&mut harness, "저장");
    assert!(!harness.state().test_modal_open());
    let macros = &harness.state().test_settings().macros;
    assert_eq!((macros.len(), macros[0].name.as_str(), macros[0].script.as_str()), (1, "점검", "show version\n@wait #"));
    assert!(std::fs::read_to_string(dir.path().join("settings.toml")).unwrap().contains("점검"));
    menu(&mut harness, "도구", "매크로");
    let item = harness.query_all_by_role(Role::Button).find(|n| label(n).trim_start().starts_with("점검")).expect("listed in the menu");
    assert!(item.accesskit_node().is_disabled(), "off without a connected tab");
}

#[test]
fn password_macro_requires_confirmation_before_any_bytes_are_sent() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    connect_telnet(&mut harness, fake_device());
    harness.state_mut().test_sessions()[0].restore_password("macro-test-secret".into());
    menu(&mut harness, "도구", "매크로");
    menu_item(&mut harness, "매크로 관리…");
    click(&mut harness, "추가");
    let dialog = harness.get_by_role(Role::Dialog);
    type_into(&harness, &dialog.query_all_by_role(Role::TextInput).next().unwrap(), "민감 매크로");
    type_into(&harness, &dialog.query_all_by_role(Role::MultilineTextInput).next().unwrap(), "{password}");
    harness.run_steps(3);
    click(&mut harness, "저장");
    let before = harness.state_mut().test_sessions()[0].tx_bytes;
    menu(&mut harness, "도구", "매크로");
    menu_item(&mut harness, "민감 매크로");
    assert!(has_label_text(&harness, "비밀번호를 전송하는 매크로"));
    assert_eq!(harness.state_mut().test_sessions()[0].tx_bytes, before);
    click(&mut harness, "취소");
    harness.run_steps(20);
    assert_eq!(harness.state_mut().test_sessions()[0].tx_bytes, before);
    menu(&mut harness, "도구", "매크로");
    menu_item(&mut harness, "민감 매크로");
    click(&mut harness, "확인 후 실행");
    run_until(&mut harness, "the explicitly confirmed macro", |h| {
        h.state_mut().test_sessions()[0].screen_contains("out:macro-test-secret")
    });
    assert!(!std::fs::read_to_string(dir.path().join("settings.toml")).unwrap().contains("macro-test-secret"));
}

/// Each tab's id and window.
fn tab_windows(harness: &mut App) -> Vec<(u64, u64)> {
    harness.state_mut().test_sessions().iter().map(|s| (s.id, s.window)).collect()
}

#[test]
fn a_tab_moves_to_a_window_of_its_own_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    let first = tab_windows(&mut harness)[0].0;
    menu(&mut harness, "탭", "새 창으로 분리");
    let tabs = tab_windows(&mut harness);
    assert!(tabs.iter().any(|&(id, w)| id == first && w != MAIN_WINDOW), "moved out: {tabs:?}");
    assert!(tabs.iter().any(|&(_, w)| w == MAIN_WINDOW), "the main window got a prompt: {tabs:?}");
    assert_eq!(harness.state().test_window_count(), 1);

    // A second tab joins the new window, then both come back.
    let i = tabs.iter().position(|&(_, w)| w == MAIN_WINDOW).unwrap();
    let ctx = harness.ctx.clone();
    harness.state_mut().test_run(&ctx, Command::OpenInNewWindow(i));
    harness.run_steps(3);
    assert_eq!(harness.state().test_window_count(), 2, "a window of its own");
    assert_eq!(tab_windows(&mut harness).iter().filter(|&&(_, w)| w == MAIN_WINDOW).count(), 1, "a new prompt again");

    for k in 0..tab_windows(&mut harness).len() {
        let ctx = harness.ctx.clone();
        harness.state_mut().test_run(&ctx, Command::MoveToMain(k));
    }
    harness.run_steps(3);
    assert!(tab_windows(&mut harness).iter().all(|&(_, w)| w == MAIN_WINDOW));
    assert_eq!(harness.state().test_window_count(), 0, "windows without tabs close");
    assert!(tab_windows(&mut harness).iter().any(|&(id, _)| id == first), "the same session, not a new connection");
}

#[test]
fn simultaneous_input_turns_on_and_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    assert!(!harness.state().test_broadcast());
    menu(&mut harness, "탭", "모든 탭에 동시 입력");
    assert!(harness.state().test_broadcast());
    assert!(
        harness.query_all_by_role(Role::Label).any(|n| n.accesskit_node().value().is_some_and(|v| v.starts_with("동시 입력 켜짐"))),
        "the tab bar says so"
    );
    menu(&mut harness, "탭", "모든 탭에 동시 입력");
    assert!(!harness.state().test_broadcast());
    assert!(!harness.query_all_by_role(Role::Label).any(|n| n.accesskit_node().value().is_some_and(|v| v.starts_with("동시 입력 켜짐"))));
}

#[test]
fn dialogs_stay_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = app(dir.path());
    for (what, open) in [
        ("Quick Connect", &(|h: &mut App| click(h, "빠른 연결")) as &dyn Fn(&mut App)),
        ("Settings", &|h: &mut App| menu(h, "도구", "설정…")),
        ("Macros", &|h: &mut App| {
            menu(h, "도구", "매크로");
            menu_item(h, "매크로 관리…");
        }),
        ("Keyboard shortcuts", &|h: &mut App| menu(h, "도움말", "단축키")),
        ("About", &|h: &mut App| menu(h, "도움말", "playterm 정보")),
    ] {
        open(&mut harness);
        assert!(harness.state().test_modal_open(), "{what} opened");
        settles(&mut harness, what, dialog_rect);
        press(&mut harness, egui::Key::Escape);
        harness.run_steps(3);
        assert!(!harness.state().test_modal_open(), "{what} closes with Escape");
    }
    // The site manager is a window of its own.
    click(&mut harness, "새 사이트");
    settles(&mut harness, "Site Manager", |h| h.get_by_role_and_label(Role::Window, "사이트 관리").rect());
}

#[test]
fn ssh_login_then_sftp_explorer_retries() {
    let dir = tempfile::tempdir().unwrap();
    // The server's SFTP folder is not there yet: the explorer's first listing fails.
    let root = dir.path().join("remote");
    let server = playterm_net::testing::SshServer::start(&root);
    let mut harness = app(dir.path());

    // Quick Connect over SSH.
    click(&mut harness, "빠른 연결");
    let dialog = harness.get_by_role(Role::Dialog);
    let inputs: Vec<Node<'_>> = dialog.query_all_by_role(Role::TextInput).collect();
    type_into(&harness, &inputs[0], "127.0.0.1");
    type_into(&harness, &inputs[1], &server.port.to_string());
    type_into(&harness, &inputs[2], playterm_net::testing::USER);
    harness.run_steps(2);
    click(&mut harness, "연결");

    // An unknown host: its key is shown, and trusting it is remembered.
    run_until(&mut harness, "the host key question", |h| has_label_text(h, "알 수 없는 호스트"));
    click(&mut harness, "신뢰하고 연결");
    // The password, asked for in a dialog.
    run_until(&mut harness, "the password question", |h| h.query_by_role(Role::PasswordInput).is_some());
    harness.get_by_role(Role::PasswordInput).type_text(playterm_net::testing::PASSWORD);
    harness.run_steps(2);
    click(&mut harness, "확인");
    run_until(&mut harness, "the shell", |h| {
        let s = &mut h.state_mut().test_sessions()[0];
        s.state == SessionState::Connected && s.screen_contains("test shell ready")
    });
    let known = std::fs::read_to_string(dir.path().join("known_hosts")).unwrap();
    assert!(known.contains(&format!("[127.0.0.1]:{}", server.port)), "the trusted key is in known_hosts: {known}");

    // The SFTP explorer: the folder cannot be listed, so it says so and offers to try again
    // (instead of asking again and again).
    menu(&mut harness, "도구", "SFTP 탐색기");
    run_until(&mut harness, "the retry button", |h| has_button(h, "다시 시도"));
    harness.run_steps(30);
    assert!(has_button(&harness, "다시 시도"), "it waits for the user");

    // The folder is there now: trying again lists it.
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.txt"), "hi").unwrap();
    click(&mut harness, "다시 시도");
    run_until(&mut harness, "the listing", |h| {
        has_label_text(h, "hello.txt") || h.query_all_by_role(Role::Button).any(|n| label(&n) == "hello.txt")
    });
    assert!(!has_button(&harness, "다시 시도"));
}

/// A Telnet device on this PC whose `rz` takes a file over ZMODEM into `dir` (with the
/// project's own receiver). Returns its port, and a channel that gets the files it saved.
fn rz_device(dir: std::path::PathBuf) -> (u16, std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let _ = stream.write_all(b"device$ ");
        let mut typed = Vec::new();
        let mut buf = [0u8; 4096];
        // Echo until "rz" and Enter (Telnet sends Enter as CR NUL or CR LF).
        while !typed.windows(3).any(|w| w == b"rz\r") {
            let Ok(n) = stream.read(&mut buf) else { return };
            if n == 0 {
                return;
            }
            typed.extend_from_slice(&buf[..n]);
            let _ = stream.write_all(&buf[..n]);
        }
        // rz: the receiver's first header (ZRINIT), then take what the app sends.
        let now = std::time::Instant::now();
        let (mut download, reply) = playterm_net::zmodem::Download::begin(dir, b"000000000000\r\n", now);
        let _ = stream.write_all(&reply);
        stream.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while download.outcome().is_none() && Instant::now() < deadline {
            let reply = match stream.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => download.feed(&buf[..n], std::time::Instant::now()),
                Err(_) => download.tick(std::time::Instant::now()),
            };
            let _ = stream.write_all(&reply);
        }
        let _ = tx.send(download.saved().to_vec());
        let _ = stream.write_all(b"\r\ndevice$ ");
        std::thread::sleep(Duration::from_secs(5));
    });
    (port, rx)
}

#[test]
fn rz_on_the_device_asks_for_files_and_sends_them() {
    let dir = tempfile::tempdir().unwrap();
    let device_dir = dir.path().join("device");
    std::fs::create_dir_all(&device_dir).unwrap();
    let local = dir.path().join("firmware.bin");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&local, &data).unwrap();
    // The file picker the app opens is answered with that file.
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let _picker = {
        let (asked, local) = (asked.clone(), local.clone());
        crate::picker::fake::answer_with(move |p| {
            asked.lock().unwrap().push((p.kind, p.title.clone()));
            Some(vec![local.clone()])
        })
    };
    let (port, saved) = rz_device(device_dir.clone());
    let mut harness = app(dir.path());

    click(&mut harness, "빠른 연결");
    harness.get_by_role(Role::Dialog).get_by_role_and_label(Role::RadioButton, "Telnet").click();
    harness.run_steps(2);
    let dialog = harness.get_by_role(Role::Dialog);
    let inputs: Vec<Node<'_>> = dialog.query_all_by_role(Role::TextInput).collect();
    type_into(&harness, &inputs[0], "127.0.0.1");
    type_into(&harness, &inputs[1], &port.to_string());
    harness.run_steps(2);
    click(&mut harness, "연결");
    run_until(&mut harness, "the device's prompt", |h| h.state_mut().test_sessions()[0].screen_contains("device$"));

    // Typed into the terminal, as the user would.
    harness.event(egui::Event::Text("rz".into()));
    harness.key_press(egui::Key::Enter);
    run_until(&mut harness, "the upload to finish", |h| h.state_mut().test_sessions()[0].screen_contains("ZMODEM 보내기 완료"));

    let files = saved.recv_timeout(Duration::from_secs(5)).expect("the device received");
    assert_eq!(files.len(), 1);
    assert_eq!(std::fs::read(&files[0]).unwrap(), data, "the file arrived whole");
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1, "asked once");
    assert_eq!(asked[0].0, crate::picker::Kind::Files);
    assert!(asked[0].1.contains("ZMODEM"), "{:?}", asked[0].1);
}
