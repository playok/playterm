//! Status bar under the terminal: connection state, target, encoding, terminal size, bytes
//! received/sent and the Caps Lock / Num Lock state.

use eframe::egui::{self, Color32, RichText};
use playterm_i18n::t;

use crate::session::{Session, SessionState};
use crate::sftp_view::human_size;

/// Caps Lock and Num Lock as the keyboard has them now.
#[cfg(windows)]
pub fn lock_keys() -> (bool, bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL, VK_NUMLOCK};
    // SAFETY: plain query; the low bit is the toggle state.
    let on = |vk: u16| unsafe { GetKeyState(i32::from(vk)) } & 1 != 0;
    (on(VK_CAPITAL), on(VK_NUMLOCK))
}

#[cfg(not(windows))]
pub fn lock_keys() -> (bool, bool) {
    (false, false)
}

fn state_text(state: &SessionState) -> (&'static str, Color32) {
    match state {
        SessionState::Local => (t("로컬 프롬프트", "Local prompt"), Color32::from_rgb(140, 140, 140)),
        SessionState::Connecting => (t("연결 중", "Connecting"), Color32::from_rgb(220, 180, 40)),
        SessionState::Connected => (t("연결됨", "Connected"), Color32::from_rgb(60, 190, 90)),
        SessionState::Closed => (t("연결 끊김", "Disconnected"), Color32::from_rgb(200, 70, 70)),
    }
}

/// A lock key indicator: lit when on, dim when off.
fn lock(ui: &mut egui::Ui, label: &str, on: bool) {
    let text = RichText::new(label).small().monospace();
    let text = if on { text.strong().color(ui.visuals().strong_text_color()) } else { text.weak() };
    ui.label(text).on_hover_text(format!("{label} {}", if on { t("켜짐", "on") } else { t("꺼짐", "off") }));
}

/// What playterm is keeping Windows from doing.
pub struct Awake {
    /// Sleep, held off for the connected sessions.
    pub sleep: bool,
    pub screensaver: bool,
}

pub fn show(ui: &mut egui::Ui, session: Option<&Session>, awake: Awake) {
    let (caps, num) = lock_keys();
    ui.horizontal(|ui| {
        if let Some(s) = session {
            let (text, color) = state_text(&s.state);
            ui.label(RichText::new("●").color(color));
            ui.label(RichText::new(text).small());
            if s.has_target() {
                ui.separator();
                ui.label(RichText::new(s.target()).small());
                if !s.is_rdp() {
                    ui.separator();
                    ui.label(RichText::new(s.charset_label()).small());
                }
            }
            if s.is_logging() {
                ui.separator();
                ui.label(RichText::new("REC").small().color(Color32::from_rgb(220, 60, 60)));
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            lock(ui, "NUM", num);
            lock(ui, "CAPS", caps);
            if awake.screensaver {
                ui.separator();
                ui.label(RichText::new(t("화면보호기 방지", "No screen saver")).small()).on_hover_text(t(
                    "화면보호기가 켜지지 않습니다 (도구 → 화면보호기 진입 방지)",
                    "The screen saver will not start (Tools → Prevent screen saver)",
                ));
            }
            if awake.sleep {
                ui.separator();
                ui.label(RichText::new(t("절전 방지", "No sleep")).small()).on_hover_text(t(
                    "연결된 세션이 있어 Windows 가 절전 모드로 들어가지 않습니다 (설정에서 변경)",
                    "Windows will not go to sleep while sessions are connected (change in Settings)",
                ));
            }
            let Some(s) = session else { return };
            ui.separator();
            // An RDP desktop is not counted (its traffic is the picture, not terminal data).
            if !s.is_rdp() {
                ui.label(RichText::new(format!("↑ {}", human_size(s.tx_bytes))).small().monospace())
                    .on_hover_text(t("이 연결에서 보낸 데이터 (입력, 응답)", "Data sent on this connection (input, replies)"));
                ui.label(RichText::new(format!("↓ {}", human_size(s.rx_bytes))).small().monospace())
                    .on_hover_text(t("이 연결에서 받은 데이터 (화면 출력)", "Data received on this connection (screen output)"));
                ui.separator();
            }
            if s.is_rdp() {
                let (w, h) = s.rdp.size;
                ui.label(RichText::new(format!("{w}×{h} px")).small().monospace())
                    .on_hover_text(t("원격 데스크톱 해상도", "Remote desktop resolution"));
            } else {
                ui.label(RichText::new(format!("{}×{}", s.terminal.cols(), s.terminal.rows())).small().monospace())
                    .on_hover_text(t("터미널 크기 (열×행)", "Terminal size (columns×rows)"));
            }
        });
    });
}
