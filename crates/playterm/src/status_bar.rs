//! Status bar under the terminal: connection state, target, encoding, terminal size, bytes
//! received/sent and the Caps Lock / Num Lock state.

use eframe::egui::{self, Color32, RichText};
use playterm_i18n::t;

use crate::session::{Session, SessionState};
use crate::sftp_view::human_size;

/// Caps Lock and Num Lock as the keyboard has them now; `None` where they cannot be read
/// (outside Windows), so nothing claims they are off.
#[cfg(windows)]
pub fn lock_keys() -> Option<(bool, bool)> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL, VK_NUMLOCK};
    // SAFETY: plain query; the low bit is the toggle state.
    let on = |vk: u16| unsafe { GetKeyState(i32::from(vk)) } & 1 != 0;
    Some((on(VK_CAPITAL), on(VK_NUMLOCK)))
}

#[cfg(not(windows))]
pub fn lock_keys() -> Option<(bool, bool)> {
    None
}

fn state_text(state: &SessionState) -> (&'static str, Color32) {
    match state {
        SessionState::Local => (t!("common.local_prompt"), Color32::from_rgb(140, 140, 140)),
        SessionState::Connecting => (t!("status_bar.connecting"), Color32::from_rgb(220, 180, 40)),
        SessionState::Connected => (t!("common.connected"), Color32::from_rgb(60, 190, 90)),
        SessionState::Closed => (t!("common.disconnected"), Color32::from_rgb(200, 70, 70)),
    }
}

/// The lock keys to show: only those that are on (a dim "CAPS" looked the same on and off).
fn lit_locks(keys: Option<(bool, bool)>) -> Vec<&'static str> {
    let Some((caps, num)) = keys else { return Vec::new() };
    [(num, "NUM"), (caps, "CAPS")].into_iter().filter(|(on, _)| *on).map(|(_, label)| label).collect()
}

/// A lock key that is on.
fn lock(ui: &mut egui::Ui, label: &str) {
    let text = RichText::new(label).small().monospace().strong().color(ui.visuals().warn_fg_color);
    ui.label(text).on_hover_text(format!("{label} {}", t!("status_bar.on")));
}

/// What playterm is keeping Windows from doing.
pub struct Awake {
    /// Sleep, held off for the connected sessions.
    pub sleep: bool,
    pub screensaver: bool,
}

pub fn show(ui: &mut egui::Ui, session: Option<&Session>, awake: Awake) {
    let locks = lit_locks(lock_keys());
    // The site's color at the left edge, as on its tab and connection line.
    let site_color = session.and_then(|s| s.tab_color).map(crate::menu::color32);
    if let Some(color) = site_color {
        let bar = ui.max_rect();
        ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(4.0, bar.height())), 0.0, color);
    }
    ui.horizontal(|ui| {
        if site_color.is_some() {
            ui.add_space(4.0);
        }
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
            for label in &locks {
                lock(ui, label);
            }
            if awake.screensaver {
                ui.separator();
                ui.label(RichText::new(t!("status_bar.no_screen_saver")).small())
                    .on_hover_text(t!("status_bar.the_screen_saver_will_not_start_tools_prevent"));
            }
            if awake.sleep {
                ui.separator();
                ui.label(RichText::new(t!("status_bar.no_sleep")).small())
                    .on_hover_text(t!("status_bar.windows_will_not_go_to_sleep_while_sessions"));
            }
            let Some(s) = session else { return };
            ui.separator();
            // An RDP desktop is not counted (its traffic is the picture, not terminal data).
            if !s.is_rdp() {
                ui.label(RichText::new(format!("↑ {}", human_size(s.tx_bytes))).small().monospace())
                    .on_hover_text(t!("status_bar.data_sent_on_this_connection_input_replies"));
                ui.label(RichText::new(format!("↓ {}", human_size(s.rx_bytes))).small().monospace())
                    .on_hover_text(t!("status_bar.data_received_on_this_connection_screen_output"));
                ui.separator();
            }
            if s.is_rdp() {
                let (w, h) = s.rdp.size;
                ui.label(RichText::new(format!("{w}×{h} px")).small().monospace())
                    .on_hover_text(t!("status_bar.remote_desktop_resolution"));
            } else {
                ui.label(RichText::new(format!("{}×{}", s.terminal.cols(), s.terminal.rows())).small().monospace())
                    .on_hover_text(t!("status_bar.terminal_size_columns_rows"));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lit_lock_keys_show() {
        assert!(lit_locks(None).is_empty(), "unknown: nothing shown");
        assert!(lit_locks(Some((false, false))).is_empty());
        assert_eq!(lit_locks(Some((true, false))), ["CAPS"]);
        assert_eq!(lit_locks(Some((true, true))), ["NUM", "CAPS"], "right to left: CAPS ends up first");
    }
}
