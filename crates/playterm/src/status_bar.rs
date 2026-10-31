//! Status bar under the terminal: connection state, target, encoding, terminal size, bytes
//! received/sent and the Caps Lock / Num Lock state.

use eframe::egui::{self, Color32, RichText};

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
        SessionState::Local => ("로컬 프롬프트", Color32::from_rgb(140, 140, 140)),
        SessionState::Connecting => ("연결 중", Color32::from_rgb(220, 180, 40)),
        SessionState::Connected => ("연결됨", Color32::from_rgb(60, 190, 90)),
        SessionState::Closed => ("연결 끊김", Color32::from_rgb(200, 70, 70)),
    }
}

/// A lock key indicator: lit when on, dim when off.
fn lock(ui: &mut egui::Ui, label: &str, on: bool) {
    let text = RichText::new(label).small().monospace();
    let text = if on { text.strong().color(ui.visuals().strong_text_color()) } else { text.weak() };
    ui.label(text).on_hover_text(format!("{label} {}", if on { "켜짐" } else { "꺼짐" }));
}

pub fn show(ui: &mut egui::Ui, session: Option<&Session>) {
    let (caps, num) = lock_keys();
    ui.horizontal(|ui| {
        if let Some(s) = session {
            let (text, color) = state_text(&s.state);
            ui.label(RichText::new("●").color(color));
            ui.label(RichText::new(text).small());
            if s.has_target() {
                ui.separator();
                ui.label(RichText::new(s.target()).small());
                ui.separator();
                ui.label(RichText::new(s.charset().to_string()).small());
            }
            if s.is_logging() {
                ui.separator();
                ui.label(RichText::new("REC").small().color(Color32::from_rgb(220, 60, 60)));
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            lock(ui, "NUM", num);
            lock(ui, "CAPS", caps);
            let Some(s) = session else { return };
            ui.separator();
            ui.label(RichText::new(format!("↑ {}", human_size(s.tx_bytes))).small().monospace())
                .on_hover_text("이 연결에서 보낸 데이터 (입력, 응답)");
            ui.label(RichText::new(format!("↓ {}", human_size(s.rx_bytes))).small().monospace())
                .on_hover_text("이 연결에서 받은 데이터 (화면 출력)");
            ui.separator();
            ui.label(RichText::new(format!("{}×{}", s.terminal.cols(), s.terminal.rows())).small().monospace())
                .on_hover_text("터미널 크기 (열×행)");
        });
    });
}
