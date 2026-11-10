//! Remembered host identities. Removal is explicit; the next connection still uses TOFU.

use std::path::PathBuf;

use eframe::egui;
use playterm_i18n::{t, tr};
use playterm_net::{TrustKind, TrustedHost, remove_trusted_host, trusted_hosts};

pub struct TrustDialog {
    path: PathBuf,
    entries: Vec<TrustedHost>,
    pending: Option<TrustedHost>,
    error: Option<String>,
    search: String,
}

impl TrustDialog {
    pub fn new(path: PathBuf) -> Self {
        let mut dialog = Self { path, entries: Vec::new(), pending: None, error: None, search: String::new() };
        dialog.refresh();
        dialog
    }

    fn refresh(&mut self) {
        match trusted_hosts(&self.path) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Returns true when the user closes the manager.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut close = false;
        let mut refresh = false;
        let response =
            egui::Modal::new(egui::Id::unique("trusted-hosts")).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, |ui| {
                ui.set_width(580.0);
                if let Some(entry) = self.pending.clone() {
                    ui.heading(t!("trust_dialog.remove_remembered_host_identity"));
                    ui.label(tr!("trust_dialog.remove_the_entry_for", arg0 = entry.host));
                    ui.monospace(&entry.algorithm);
                    ui.monospace(&entry.fingerprint);
                    ui.label(t!("trust_dialog.after_removal_the_next_connection_asks_you_to"));
                    ui.horizontal(|ui| {
                        if ui.button(t!("trust_dialog.confirm_removal")).clicked() {
                            match remove_trusted_host(&self.path, &entry) {
                                Ok(()) => {
                                    self.pending = None;
                                    refresh = true;
                                }
                                Err(e) => self.error = Some(e.to_string()),
                            }
                        }
                        if ui.button(t!("common.cancel")).clicked() {
                            self.pending = None;
                            self.error = None;
                        }
                    });
                } else {
                    ui.heading(t!("trust_dialog.remembered_host_keys_and_certificates"));
                    ui.label(t!("trust_dialog.trust_records_for_ssh_hosts_including_jump_hosts"));
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(t!("trust_dialog.search_hosts")));
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(330.0).show(ui, |ui| {
                        if self.entries.is_empty() {
                            ui.label(t!("trust_dialog.no_remembered_hosts"));
                        }
                        for entry in &self.entries {
                            if !entry.host.to_lowercase().contains(&self.search.to_lowercase()) {
                                continue;
                            }
                            ui.push_id((entry.kind as u8, entry.line), |ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(match entry.kind {
                                        TrustKind::Ssh => "SSH",
                                        TrustKind::Rdp => "RDP",
                                    });
                                    ui.label(&entry.host);
                                    if ui.button(t!("trust_dialog.remove")).clicked() {
                                        self.pending = Some(entry.clone());
                                    }
                                });
                                ui.label(tr!("trust_dialog.line", arg0 = entry.algorithm, arg1 = entry.line));
                                ui.monospace(&entry.fingerprint);
                                ui.separator();
                            });
                        }
                    });
                    ui.horizontal(|ui| {
                        refresh = ui.button(t!("trust_dialog.refresh")).clicked();
                        close = ui.button(t!("common.close")).clicked();
                    });
                }
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
        if refresh {
            self.refresh();
        }
        if response.should_close() && self.pending.is_some() {
            self.pending = None;
            false
        } else {
            close || response.should_close()
        }
    }
}
