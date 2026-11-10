//! Command input bar under the terminal: write a command (or several lines) and send it to the
//! current tab or to all connected tabs. Enter sends, Shift+Enter starts a new line, ↑/↓ recall
//! earlier input. Its Macros button runs a saved macro in the same tabs.

use eframe::egui::{self, Key, Modifiers};
use playterm_i18n::{t, tr};

use crate::icon;

const MAX_HISTORY: usize = 100;

/// What the bar asks for.
pub enum ComposeAction {
    Send(Send),
    /// Run saved macro `k` in the tabs the bar sends to (`to_all` as in [`Send`]).
    Macro {
        k: usize,
        to_all: bool,
    },
    /// Open "Manage macros".
    ManageMacros,
}

/// A message to type into the tabs.
pub struct Send {
    /// Lines to type, each followed by Enter. Empty: just Enter.
    pub text: String,
    /// All connected tabs instead of the current one.
    pub to_all: bool,
}

#[derive(Default)]
pub struct ComposeBar {
    text: String,
    /// Earlier messages, oldest first.
    history: Vec<String>,
    /// Position while browsing the history with ↑/↓.
    history_pos: Option<usize>,
    to_all: bool,
    focus_requested: bool,
}

impl ComposeBar {
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    /// `connected`: number of connected tabs (for the "all tabs" choice); `macros`: the names of
    /// the saved macros.
    pub fn show(&mut self, ui: &mut egui::Ui, connected: usize, macros: &[String]) -> Option<ComposeAction> {
        let mut action = None;
        let id = ui.make_persistent_id("compose-input");
        let focused = ui.memory(|m| m.has_focus(id));
        let mut send = false;
        if focused {
            // Taken before the text field sees them: Enter sends, ↑/↓ browse the history.
            // `consume_key` ignores Shift, so Shift+Enter (new line) is checked for first.
            send = ui.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter));
            if !self.text.contains('\n') {
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
                    self.history_step(true);
                }
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
                    self.history_step(false);
                }
            }
        }

        ui.horizontal(|ui| {
            let controls = 320.0;
            let width = (ui.available_width() - controls).max(120.0);
            // The full hint would wrap (and make the bar taller) when the side panels leave
            // little room; the short one says just enough.
            let hint = if width >= 560.0 {
                t!("compose_bar.command_to_send_enter_sends_shift_enter_new")
            } else if width >= 220.0 {
                t!("compose_bar.command_enter_sends")
            } else {
                // Narrower still: a word that fits on one line (#165).
                t!("compose_bar.command")
            };
            let edit = ui.add(
                egui::TextEdit::multiline(&mut self.text)
                    .id(id)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(1)
                    .desired_width(width)
                    .hint_text(hint),
            );
            if edit.changed() {
                self.history_pos = None;
            }
            if std::mem::take(&mut self.focus_requested) {
                edit.request_focus();
            }
            let all = tr!("compose_bar.all_tabs", connected = connected);
            egui::ComboBox::from_id_salt("compose-target")
                .width(110.0)
                .selected_text(if self.to_all { all.as_str() } else { t!("compose_bar.this_tab") })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.to_all, false, t!("compose_bar.this_tab"));
                    ui.selectable_value(&mut self.to_all, true, all.as_str());
                })
                .response
                .on_hover_text(t!("compose_bar.all_tabs_every_connected_tab_except_tabs_with"));
            if ui.add(icon::button(icon::SEND, t!("compose_bar.send"))).on_hover_text("Enter").clicked() {
                send = true;
                edit.request_focus();
            }
            ui.menu_button(t!("common.macros"), |ui| {
                for (k, name) in macros.iter().enumerate() {
                    if ui.button(name).clicked() {
                        action = Some(ComposeAction::Macro { k, to_all: self.to_all });
                    }
                }
                if macros.is_empty() {
                    ui.weak(t!("common.no_macros_saved"));
                }
                ui.separator();
                if ui.button(t!("common.manage_macros")).clicked() {
                    action = Some(ComposeAction::ManageMacros);
                }
            })
            .response
            .on_hover_text(t!("compose_bar.runs_a_saved_macro_in_the_tabs_the"));
        });

        if !send {
            return action;
        }
        let text = std::mem::take(&mut self.text);
        self.history_pos = None;
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        Some(ComposeAction::Send(Send { text, to_all: self.to_all }))
    }

    fn history_step(&mut self, back: bool) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() - 1;
        self.history_pos = match (self.history_pos, back) {
            (None, true) => Some(last),
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i < last => Some(i + 1),
            (_, false) => None,
        };
        self.text = self.history_pos.map(|i| self.history[i].clone()).unwrap_or_default();
    }
}

/// The lines to type for `text`; an empty message is a bare Enter.
pub fn lines(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    if lines.is_empty() { vec![""] } else { lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines() {
        assert_eq!(lines(""), vec![""]);
        assert_eq!(lines("show ver"), vec!["show ver"]);
        assert_eq!(lines("conf t\r\nint g0/1\n"), vec!["conf t", "int g0/1"]);
        assert_eq!(lines("a\n\nb"), vec!["a", "", "b"]);
    }

    #[test]
    fn history_browsing() {
        let mut bar = ComposeBar { history: vec!["one".into(), "two".into()], ..Default::default() };
        bar.history_step(true);
        assert_eq!(bar.text, "two");
        bar.history_step(true);
        assert_eq!(bar.text, "one");
        bar.history_step(true);
        assert_eq!(bar.text, "one", "stays at the oldest");
        bar.history_step(false);
        assert_eq!(bar.text, "two");
        bar.history_step(false);
        assert_eq!(bar.text, "", "past the newest: empty again");
    }
}
