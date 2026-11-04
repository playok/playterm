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

use crate::app::PlaytermApp;
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
        let Ok((mut stream, _)) = listener.accept() else { return };
        let _ = stream.write_all(b"router# ");
        let reader = BufReader::new(stream.try_clone().unwrap());
        for line in reader.split(b'\r').map_while(Result::ok) {
            let line = String::from_utf8_lossy(&line).trim_matches(['\n', '\0']).to_owned();
            if stream.write_all(format!("\r\nout:{line}\r\nrouter# ").as_bytes()).is_err() {
                break;
            }
        }
    });
    port
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
    click(&mut harness, "관리");
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
