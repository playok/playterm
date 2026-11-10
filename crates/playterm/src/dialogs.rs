//! Modal dialogs. Each `show` returns what the user decided; the app performs the action.

use eframe::egui;
use playterm_config::{ClipboardKeys, SessionProfile, ShortcutKeys};
use playterm_i18n::{t, tr};
use zeroize::{Zeroize, Zeroizing};

use crate::icon;
use crate::profile_form::{FormOptions, ProfileForm};
use crate::session::Session;

fn modal(id: impl std::hash::Hash + std::fmt::Debug, ctx: &egui::Context, add: impl FnOnce(&mut egui::Ui)) -> bool {
    egui::Modal::new(egui::Id::unique(id)).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, add).should_close()
}

// ---------------------------------------------------------------------------------------------
// Quick connect

pub enum QuickConnectResult {
    Open,
    Cancel,
    Connect { profile: Box<SessionProfile>, password: Option<Zeroizing<String>>, save: bool },
}

pub struct QuickConnect {
    form: ProfileForm,
    save: bool,
}

impl QuickConnect {
    pub fn new() -> Self {
        Self { form: ProfileForm::new(SessionProfile::default(), false), save: false }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> QuickConnectResult {
        let mut result = QuickConnectResult::Open;
        let close = modal("quick-connect", ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(t!("common.quick_connect"));
            ui.add_space(8.0);
            let mut submit = self.form.ui(ui, &FormOptions { meta: false, save_password: false, folders: &[], macros: &[] });
            ui.add_space(8.0);
            let (main, cancel) = ui
                .horizontal(|ui| {
                    let connect = icon::primary(ui, icon::CONNECT, t!("common.connect"));
                    let buttons = icon::ok_cancel_row(ui, connect, true, icon::cancel());
                    ui.add_space(12.0);
                    ui.checkbox(&mut self.save, t!("dialogs.save_to_the_site_list"));
                    buttons
                })
                .inner;
            submit |= main.clicked();
            if cancel.clicked() {
                result = QuickConnectResult::Cancel;
            }
            if submit && let Some((profile, password)) = self.form.validate() {
                result = QuickConnectResult::Connect { profile: Box::new(profile), password, save: self.save };
            }
        });
        if close && matches!(result, QuickConnectResult::Open) {
            result = QuickConnectResult::Cancel;
        }
        result
    }
}

// ---------------------------------------------------------------------------------------------
// Vault

/// Shortest master password a new vault accepts.
pub const MIN_MASTER_LENGTH: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VaultMode {
    Unlock,
    Create,
}

pub enum VaultResult {
    Open,
    Cancel,
    Submit(Zeroizing<String>),
    /// User confirmed deleting the vault after forgetting the master password.
    Reset,
}

pub struct VaultDialog {
    pub mode: VaultMode,
    /// Why the vault is needed, shown at the top.
    reason: String,
    master: Zeroizing<String>,
    confirm: Zeroizing<String>,
    error: Option<String>,
    confirm_reset: bool,
    focus_requested: bool,
}

impl VaultDialog {
    pub fn new(mode: VaultMode, reason: impl Into<String>) -> Self {
        Self {
            mode,
            reason: reason.into(),
            master: Zeroizing::default(),
            confirm: Zeroizing::default(),
            error: None,
            confirm_reset: false,
            focus_requested: false,
        }
    }

    /// Shows an error from the attempted unlock/create and clears the input.
    pub fn fail(&mut self, message: String) {
        self.error = Some(message);
        self.clear_secrets();
        self.focus_requested = false;
    }

    pub fn clear_secrets(&mut self) {
        self.master.zeroize();
        self.confirm.zeroize();
    }

    /// The computation stays outside the UI thread. Cancellation is available until commit.
    pub fn show_working(&self, ctx: &egui::Context, can_cancel: bool) -> bool {
        let mut cancel = false;
        let close = modal("vault-working", ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(match self.mode {
                VaultMode::Unlock => t!("dialogs.unlocking_saved_passwords"),
                VaultMode::Create => t!("dialogs.creating_the_password_vault"),
            });
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if can_cancel { t!("dialogs.please_wait") } else { t!("dialogs.finishing") });
            });
            cancel = ui.add_enabled(can_cancel, icon::button(icon::CANCEL, t!("common.cancel"))).clicked();
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
        can_cancel && (cancel || close)
    }

    /// Switches to creating a new vault (after a reset).
    pub fn switch_to_create(&mut self) {
        let reason = std::mem::take(&mut self.reason);
        *self = Self::new(VaultMode::Create, reason);
    }

    pub fn show(&mut self, ctx: &egui::Context) -> VaultResult {
        let mut result = VaultResult::Open;
        let close = modal("vault", ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(match self.mode {
                VaultMode::Unlock => t!("dialogs.unlock_saved_passwords"),
                VaultMode::Create => t!("dialogs.set_master_password"),
            });
            if !self.reason.is_empty() {
                ui.label(&self.reason);
            }
            if self.mode == VaultMode::Create {
                ui.label(t!("dialogs.saved_passwords_are_encrypted_with_this_master_password"));
            }
            ui.add_space(6.0);

            let mut enter = false;
            ui.label(t!("dialogs.master_password"));
            let first = ui.add(egui::TextEdit::singleline(&mut *self.master).password(true));
            secret_field(ui, &first);
            enter |= first.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !self.focus_requested {
                first.request_focus();
                self.focus_requested = true;
            }
            // Making one: checked while typing (#139), as it cannot be recovered once forgotten.
            let mut ready = true;
            if self.mode == VaultMode::Create {
                let length = self.master.chars().count();
                let short = length < MIN_MASTER_LENGTH;
                let (color, text) = if short {
                    (ui.visuals().warn_fg_color, tr!("dialogs.at_least_characters", arg0 = MIN_MASTER_LENGTH, length = length))
                } else {
                    (ui.visuals().weak_text_color(), tr!("dialogs.characters", length = length))
                };
                ui.label(egui::RichText::new(text).small().color(color));
                ui.label(t!("dialogs.confirm_master_password"));
                let second = ui.add(egui::TextEdit::singleline(&mut *self.confirm).password(true));
                secret_field(ui, &second);
                enter |= second.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let matches = *self.master == *self.confirm;
                if !self.confirm.is_empty() {
                    let (color, text) = if matches {
                        (ui.visuals().weak_text_color(), t!("dialogs.they_match"))
                    } else {
                        (ui.visuals().error_fg_color, t!("dialogs.the_passwords_do_not_match"))
                    };
                    ui.label(egui::RichText::new(text).small().color(color));
                }
                ready = !short && matches;
            }

            if let Some(err) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if self.mode == VaultMode::Unlock { t!("common.unlock") } else { t!("dialogs.create") };
                let icon = if self.mode == VaultMode::Unlock { icon::UNLOCK } else { icon::VAULT_SET_UP };
                if ui.add_enabled(ready, icon::primary(ui, icon, label)).clicked() || (enter && ready) {
                    if self.master.is_empty() {
                        self.error = Some(t!("dialogs.enter_the_master_password").into());
                    } else if self.mode == VaultMode::Create && *self.master != *self.confirm {
                        self.error = Some(t!("dialogs.the_passwords_do_not_match").into());
                    } else {
                        result = VaultResult::Submit(self.master.clone());
                    }
                }
                if ui.add(icon::button(icon::CANCEL, t!("common.cancel"))).clicked() {
                    result = VaultResult::Cancel;
                }
            });

            if self.mode == VaultMode::Unlock {
                ui.add_space(6.0);
                if !self.confirm_reset {
                    if ui.small_button(t!("dialogs.forgot_the_master_password")).clicked() {
                        self.confirm_reset = true;
                    }
                } else {
                    ui.colored_label(ui.visuals().warn_fg_color, t!("dialogs.resetting_starts_over_without_the_saved_passwords_the"));
                    ui.horizontal(|ui| {
                        if ui.add(icon::danger(ui, icon::RESET, t!("dialogs.reset"))).clicked() {
                            result = VaultResult::Reset;
                        }
                        if ui.add(icon::button(icon::BACK, t!("dialogs.back"))).clicked() {
                            self.confirm_reset = false;
                        }
                    });
                }
            }
        });
        if close && matches!(result, VaultResult::Open) {
            result = VaultResult::Cancel;
        }
        result
    }
}

// ---------------------------------------------------------------------------------------------
// Simple confirmation

/// Explicit per-run confirmation: a macro cannot infer whether remote input will echo.
pub fn confirm_password_macro(ctx: &egui::Context, name: &str, targets: &[String]) -> Option<bool> {
    let mut answer = None;
    let close = modal("password-macro", ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(t!("dialogs.macro_sends_a_password"));
        ui.label(name);
        ui.label(t!("dialogs.this_macro_sends_without_verifying_a_password_prompt"));
        for target in targets.iter().take(6) {
            ui.label(target);
        }
        if targets.len() > 6 {
            ui.label(tr!("dialogs.more_targets", arg0 = targets.len() - 6));
        }
        ui.add_space(8.0);
        let (run, cancel) = icon::ok_cancel_row(ui, icon::primary(ui, icon::SEND, t!("dialogs.run_after_checking")), true, icon::cancel());
        if run.clicked() {
            answer = Some(true);
        }
        if cancel.clicked() {
            answer = Some(false);
        }
    });
    if close && answer.is_none() { Some(false) } else { answer }
}

/// Asks before deleting something. `Some(true)` = confirmed, `Some(false)` = cancelled, `None` =
/// still open.
pub fn confirm(ctx: &egui::Context, title: &str, text: &str, ok_label: &str) -> Option<bool> {
    let mut answer = None;
    let close = modal(("confirm", title), ctx, |ui| {
        ui.set_width(320.0);
        ui.heading(title);
        ui.label(text);
        ui.add_space(8.0);
        // Its callers delete something: it looks like it (#133).
        let ok = icon::danger(ui, icon::DELETE, ok_label);
        let (main, cancel) = icon::ok_cancel_row(ui, ok, true, icon::cancel());
        if main.clicked() {
            answer = Some(true);
        }
        if cancel.clicked() {
            answer = Some(false);
        }
    });
    if close && answer.is_none() { Some(false) } else { answer }
}

/// A notice with an OK button (`title`, `text`). True once it is dismissed.
pub fn notice(ctx: &egui::Context, title: &str, text: &str) -> bool {
    let mut done = false;
    let close = modal(("notice", title), ctx, |ui| {
        ui.set_width(380.0);
        ui.heading(title);
        ui.add_space(4.0);
        ui.label(text);
        ui.add_space(8.0);
        let ok = ui.add(icon::primary(ui, icon::OK, t!("common.ok")));
        if !ui.memory(|m| m.focused().is_some()) {
            ok.request_focus();
        }
        done = ok.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    done || close
}

/// What closing would end, for [`confirm_close`].
pub struct CloseQuestion<'a> {
    pub title: &'a str,
    /// The button that closes (`닫기`, `끝내기`).
    pub ok_label: &'a str,
    /// Titles of the tabs whose connections would end.
    pub tabs: &'a [String],
    /// How many of them are moving files (ZMODEM, SFTP).
    pub transferring: usize,
}

/// Asks before closing tabs whose connections are up. `Some(true)` = close; `dont_ask` is the
/// "do not ask again" box.
pub fn confirm_close(ctx: &egui::Context, q: &CloseQuestion<'_>, dont_ask: &mut bool) -> Option<bool> {
    const SHOWN: usize = 6;
    let mut answer = None;
    let close = modal(("confirm-close", q.title), ctx, |ui| {
        ui.set_width(380.0);
        ui.heading(q.title);
        ui.label(tr!("dialogs.connections_will_end", arg0 = q.tabs.len()));
        ui.add_space(4.0);
        for name in q.tabs.iter().take(SHOWN) {
            ui.label(format!("• {name}"));
        }
        if q.tabs.len() > SHOWN {
            ui.weak(tr!("dialogs.and_more", arg0 = q.tabs.len() - SHOWN));
        }
        if q.transferring > 0 {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr!("dialogs.tabs_are_transferring_files_the_transfers_stop", arg0 = q.transferring))
                    .strong()
                    .color(ui.visuals().warn_fg_color),
            );
        }
        ui.add_space(8.0);
        ui.checkbox(dont_ask, t!("dialogs.do_not_ask_again_undo_in_settings_connection"));
        ui.add_space(6.0);
        let ok = icon::primary(ui, icon::CLOSE, q.ok_label);
        let (main, cancel) = icon::ok_cancel_row(ui, ok, true, icon::cancel());
        // Enter is the highlighted button, as in the system's dialogs; Esc cancels. (Cancel
        // with the keyboard focus takes Enter for itself.)
        let enter = !cancel.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        if main.clicked() || enter {
            answer = Some(true);
        }
        if cancel.clicked() {
            answer = Some(false);
        }
    });
    if close && answer.is_none() { Some(false) } else { answer }
}

// ---------------------------------------------------------------------------------------------
// Help

/// Asks before pasting several lines that would each run as a command. `Some(true)`: paste.
/// `others`: the titles of the other tabs the paste also goes to (simultaneous input).
pub fn confirm_paste(ctx: &egui::Context, text: &str, others: &[String]) -> Option<bool> {
    const SHOWN_LINES: usize = 8;
    const SHOWN_CHARS: usize = 100;
    let lines: Vec<&str> = text.lines().collect();
    let mut preview: Vec<String> = lines
        .iter()
        .take(SHOWN_LINES)
        .map(|l| {
            let l = crate::session::printable(l);
            if l.chars().count() > SHOWN_CHARS { format!("{}…", l.chars().take(SHOWN_CHARS).collect::<String>()) } else { l }
        })
        .collect();
    if lines.len() > SHOWN_LINES {
        preview.push(tr!("dialogs.more_lines", arg0 = lines.len() - SHOWN_LINES));
    }
    let mut answer = None;
    let close = modal("confirm-paste", ctx, |ui| {
        ui.set_width(520.0);
        ui.heading(t!("dialogs.paste_several_lines"));
        ui.label(tr!("dialogs.this_pastes_lines_each_ends_with_enter_so", arg0 = lines.len()));
        if !others.is_empty() {
            ui.add_space(4.0);
            let warning = tr!("dialogs.simultaneous_input_this_also_pastes_into_other_tabs", arg0 = others.len(), arg1 = others.join(", "));
            ui.label(egui::RichText::new(warning).strong().color(ui.visuals().warn_fg_color));
        }
        ui.add_space(6.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(egui::RichText::new(preview.join("\n")).monospace()).wrap());
        });
        ui.add_space(8.0);
        let paste = icon::primary(ui, icon::PASTE, t!("common.paste"));
        let (main, cancel) = icon::ok_cancel_row(ui, paste, true, icon::cancel());
        if main.clicked() {
            answer = Some(true);
        }
        if cancel.clicked() {
            answer = Some(false);
        }
        ui.weak(t!("dialogs.can_be_turned_off_with_confirm_paste_in"));
    });
    if close && answer.is_none() {
        answer = Some(false);
    }
    answer
}

/// Keys and what they do, in the current language.
/// The app's shortcuts come from their table (`keys`), so the list always says what the keys do
/// (#128); the clipboard keys and mouse gestures follow.
fn shortcut_list(keys: &ClipboardKeys, shortcuts: &ShortcutKeys) -> Vec<(String, &'static str)> {
    let mut list = crate::keys::list(shortcuts);
    let copy: Vec<&str> = [(keys.ctrl_shift_c, "Ctrl+Shift+C"), (keys.ctrl_insert, "Ctrl+Insert")]
        .into_iter()
        .filter_map(|(on, k)| on.then_some(k))
        .collect();
    if !copy.is_empty() {
        list.push((copy.join(", "), t!("common.copy")));
    }
    if keys.ctrl_c {
        let what = t!("dialogs.copy_if_there_is_a_selection_otherwise_send");
        list.push(("Ctrl+C".into(), what));
    }
    if keys.copy_on_select {
        list.push((t!("dialogs.select_by_dragging").into(), t!("dialogs.copies_right_away")));
    }
    let paste: Vec<&str> = [
        (keys.ctrl_shift_v, "Ctrl+Shift+V"),
        (keys.ctrl_v, "Ctrl+V"),
        (keys.shift_insert, "Shift+Insert"),
        (keys.right_click_paste, t!("common.right_click")),
        (keys.middle_click_paste, t!("common.middle_click")),
    ]
    .into_iter()
    .filter_map(|(on, k)| on.then_some(k))
    .collect();
    if !paste.is_empty() {
        list.push((paste.join(", "), t!("common.paste")));
    }
    list.extend(
        [
            (t!("dialogs.shift_drag"), t!("dialogs.select_locally_even_in_programs_that_use_the")),
            (t!("dialogs.ctrl_wheel"), t!("dialogs.font_size")),
            (t!("dialogs.drag_a_tab"), t!("dialogs.beside_above_or_below_another_tab_split_the")),
            (t!("dialogs.middle_click_a_tab"), t!("dialogs.close_the_tab")),
        ]
        .into_iter()
        .map(|(k, what)| (k.to_owned(), what)),
    );
    list
}

/// Keyboard shortcut list. Returns `true` once closed.
pub fn shortcuts(ctx: &egui::Context, keys: &ClipboardKeys, shortcuts: &ShortcutKeys) -> bool {
    let mut done = false;
    let close = modal("shortcuts", ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(t!("common.keyboard_shortcuts"));
        ui.add_space(6.0);
        // Scrolls in a short window, so Close stays on screen. Its height is set from the rows,
        // so the dialog has its size from the first frame (it does not grow into place).
        let list = shortcut_list(keys, shortcuts);
        let row = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let max = (ui.ctx().content_rect().height() - 160.0).max(200.0);
        let height = (list.len() as f32 * row).min(max);
        egui::ScrollArea::vertical().min_scrolled_height(height).max_height(height).show(ui, |ui| {
            egui::Grid::new("shortcut-list").num_columns(2).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
                for (combo, what) in list {
                    ui.monospace(combo);
                    ui.label(what);
                    ui.end_row();
                }
            });
        });
        ui.add_space(8.0);
        done = ui.add(icon::button(icon::CLOSE, t!("common.close"))).clicked();
    });
    done || close
}

/// Who makes playterm, and where to ask questions or report bugs.
pub const AUTHOR: &str = "playok";
pub const PROJECT_PAGE: &str = "https://github.com/playok/playterm";

/// "About" box. Returns `true` once closed.
pub fn about(ctx: &egui::Context) -> bool {
    let mut done = false;
    let close = modal("about", ctx, |ui| {
        ui.set_width(340.0);
        ui.vertical_centered(|ui| {
            ui.heading("playterm");
            ui.label(tr!("dialogs.version", arg0 = env!("CARGO_PKG_VERSION")));
            ui.add_space(6.0);
            ui.label(t!("dialogs.ssh_telnet_serial_remote_desktop_terminal"));
            ui.weak(t!("dialogs.sftp_port_forwarding_session_logs_korean_cp949_support"));
            ui.add_space(6.0);
            ui.label(tr!("dialogs.made_by", arg0 = AUTHOR));
            ui.hyperlink_to(t!("common.questions_and_bug_reports_github"), PROJECT_PAGE).on_hover_text(PROJECT_PAGE);
            ui.weak(PROJECT_PAGE);
            ui.add_space(4.0);
            ui.weak(t!("dialogs.mit_license_open_source_components_help_open_source"));
            ui.add_space(8.0);
            done = ui.add(icon::primary(ui, icon::OK, t!("common.ok"))).clicked();
        });
    });
    done || close
}

// ---------------------------------------------------------------------------------------------
// Per-session prompts from the network layer

pub fn host_key(ctx: &egui::Context, session: &mut Session) {
    let mut decision = None;
    egui::Modal::new(egui::Id::unique(("host-key", session.id))).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, |ui| {
        let check = session.host_key.as_ref().expect("checked by caller");
        ui.set_width(460.0);
        ui.heading(t!("dialogs.unknown_host"));
        ui.add_space(6.0);
        if check.certificate {
            ui.label(tr!("dialogs.the_rdp_server_certificate_of_is_new_not", arg0 = check.host, arg1 = check.port));
        } else {
            ui.label(tr!("dialogs.the_host_key_of_is_not_in_known", arg0 = check.host, arg1 = check.port));
        }
        ui.add_space(6.0);
        egui::Grid::new("hk").num_columns(2).show(ui, |ui| {
            ui.label(t!("dialogs.key_type"));
            ui.monospace(&check.algorithm);
            ui.end_row();
            ui.label(t!("dialogs.fingerprint"));
            ui.monospace(&check.fingerprint);
            ui.end_row();
        });
        ui.add_space(10.0);
        let trust = icon::primary(ui, icon::TRUST, t!("dialogs.trust_and_connect"));
        let (main, cancel) = icon::ok_cancel_row(ui, trust, true, icon::cancel());
        if main.clicked() {
            decision = Some(true);
        }
        if cancel.clicked() {
            decision = Some(false);
        }
    });
    if let Some(accept) = decision
        && let Some(check) = session.host_key.take()
    {
        check.answer(accept);
    }
}

/// Log in again as another user: the user name was changed in the password dialog.
pub struct Relogin {
    pub user: String,
    /// The password typed for that user, if the dialog asked for exactly a password.
    pub password: Option<playterm_net::Password>,
    /// Also change the user name of the saved site.
    pub save: bool,
}

/// The credentials dialog. For a login (password, keyboard-interactive) the user name can be
/// corrected too; that answers with a [`Relogin`] instead of the server's questions.
/// `saved_site`: the tab was opened from a saved site, whose user name could be updated and
/// whose password can be saved ("save password", stored once the login succeeds).
pub fn auth(ctx: &egui::Context, session: &mut Session, saved_site: bool) -> Option<Relogin> {
    let mut submit = None;
    let mut skip_key = false;
    let modal =
        egui::Modal::new(egui::Id::unique(("auth", session.id))).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, |ui| {
            let pending = session.auth.as_mut().expect("checked by caller");
            ui.set_width(380.0);
            ui.heading(&pending.prompt.title);
            if !pending.prompt.instructions.trim().is_empty() {
                ui.label(pending.prompt.instructions.trim());
            }
            ui.add_space(6.0);
            let mut enter = false;
            let original_user = pending.prompt.user.clone();
            if let Some(original) = &original_user {
                ui.label(t!("common.user"));
                let edit = ui.add(egui::TextEdit::singleline(&mut pending.user));
                enter |= edit.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
                if pending.user.trim() != original.as_str() {
                    ui.weak(t!("dialogs.changing_the_user_connects_again_and_logs_in"));
                    if saved_site {
                        ui.checkbox(&mut pending.save_user, t!("dialogs.save_it_in_the_site_too"));
                    }
                }
                ui.add_space(4.0);
            }
            for (i, field) in pending.prompt.fields.iter().enumerate() {
                ui.label(&field.label);
                let edit = ui.add(egui::TextEdit::singleline(&mut *pending.answers[i]).password(!field.echo));
                if !field.echo {
                    secret_field(ui, &edit);
                }
                // Check before (re)focusing: Enter makes the field surrender focus this frame.
                enter |= edit.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
                if i == 0 && !pending.focus_requested {
                    edit.request_focus();
                    pending.focus_requested = true;
                }
            }
            if saved_site && pending.is_password() {
                ui.add_space(4.0);
                ui.checkbox(&mut pending.save_password, t!("dialogs.save_password"))
                    .on_hover_text(t!("dialogs.once_the_login_succeeds_it_is_saved_in"));
            }
            ui.add_space(10.0);
            if pending.prompt.can_skip && ui.button(t!("dialogs.skip_this_key")).clicked() {
                skip_key = true;
            }
            let user_ok = original_user.is_none() || !pending.user.trim().is_empty();
            let ok = icon::primary(ui, icon::OK, t!("common.ok"));
            let (main, cancel) = icon::ok_cancel_row(ui, ok, user_ok, icon::cancel());
            if main.clicked() || (enter && user_ok) {
                submit = Some(true);
            }
            if cancel.clicked() {
                submit = Some(false);
            }
        });
    // Authentication may arrive while the reconnect button is still being released.
    // Do not treat that opening click on the backdrop as a cancellation.
    if modal.response.should_close()
        || (modal.is_top_modal && !modal.any_popup_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)))
    {
        submit = Some(false);
        skip_key = false;
    }
    if skip_key {
        session.save_password_on_login(None);
        session.auth.take()?.prompt.skip();
        return None;
    }
    let ok = submit?;
    let pending = session.auth.take()?;
    // Saved only after a successful login; a later prompt (wrong password) replaces it.
    let save = (ok && saved_site && pending.save_password && pending.is_password())
        .then(|| pending.answers.first().cloned().filter(|p| !p.is_empty()))
        .flatten();
    session.save_password_on_login(save);
    let user = pending.user.trim().to_owned();
    if ok
        && let Some(original) = &pending.prompt.user
        && &user != original
    {
        // A password prompt's answer is that user's password; other questions start over.
        let password = pending.is_password().then(|| pending.answers.first().filter(|p| !p.is_empty()).map(|p| p.clone().into())).flatten();
        pending.prompt.answer(None);
        return Some(Relogin { user, password, save: pending.save_user });
    }
    // The answers move to the network task; what stays here is zeroed when dropped.
    let answers = ok.then(|| pending.answers.iter().map(|a| a.to_string()).collect());
    pending.prompt.answer(answers);
    None
}

/// Called each frame right after a password field.
///
/// - egui keeps an undo history for every text field, password fields too: copies of what was
///   typed (the master password included) that stay in memory after the dialog is closed. It is
///   cleared, so no copy outlives the frame.
/// - The input method is switched off while the field has focus, as Windows does for its own
///   password boxes: with Korean input on, `secret` would be typed as `ㄴㄷㅊㄱㄷㅅ`, hidden
///   behind the dots, and the login would fail for no visible reason.
pub fn secret_field(ui: &egui::Ui, field: &egui::Response) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), field.id) {
        state.clear_undoer();
        state.store(ui.ctx(), field.id);
    }
    if field.has_focus() {
        ui.ctx().output_mut(|o| o.ime = None);
        // Caps Lock turns a typed password into another one: say so inside the field (its
        // right end, so the layout around it stays as it is).
        if crate::status_bar::lock_keys().map(|(caps, _)| caps).unwrap_or(false) {
            ui.painter().text(
                field.rect.right_center() - egui::vec2(6.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                t!("dialogs.caps_lock_on"),
                egui::TextStyle::Small.resolve(ui.style()),
                ui.visuals().warn_fg_color,
            );
        }
    }
}

#[cfg(test)]
mod about_tests {
    use super::*;

    /// Every piece of text the About box draws.
    fn about_text() -> String {
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            // A modal is laid out on the second pass.
            let mut out = ctx.run_ui(Default::default(), |ui| {
                about(ui.ctx());
            });
            out.textures_delta.clear();
            texts = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
        }
        texts.join("\n")
    }

    #[test]
    fn about_names_the_author_and_where_to_ask() {
        let text = about_text();
        assert!(text.contains(AUTHOR), "{text}");
        assert!(text.contains(PROJECT_PAGE), "{text}");
        assert!(text.contains("GitHub"), "{text}");
    }
}
