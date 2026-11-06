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
        let response = egui::Modal::new(egui::Id::unique("trusted-hosts"))
            .backdrop_color(crate::app::theme::MODAL_BACKDROP)
            .show(ctx, |ui| {
                ui.set_width(580.0);
                if let Some(entry) = self.pending.clone() {
                    ui.heading(t("저장된 호스트 신뢰 삭제", "Remove remembered host identity"));
                    ui.label(tr!("{} 항목을 삭제할까요?", "Remove the entry for {}?", entry.host));
                    ui.monospace(&entry.algorithm);
                    ui.monospace(&entry.fingerprint);
                    ui.label(t(
                        "삭제하면 다음 연결에서 새 키·인증서의 지문을 다시 확인해야 합니다. 예상하지 못한 변경은 중간자 공격일 수 있습니다. 서버 관리자에게 새 지문을 확인한 뒤에만 신뢰하세요.",
                        "After removal, the next connection asks you to verify the new key or certificate fingerprint. An unexpected change could be a man-in-the-middle attack. Verify the new fingerprint with the server administrator before trusting it.",
                    ));
                    ui.horizontal(|ui| {
                        if ui.button(t("삭제 확인", "Confirm removal")).clicked() {
                            match remove_trusted_host(&self.path, &entry) {
                                Ok(()) => {
                                    self.pending = None;
                                    refresh = true;
                                }
                                Err(e) => self.error = Some(e.to_string()),
                            }
                        }
                        if ui.button(t("취소", "Cancel")).clicked() {
                            self.pending = None;
                            self.error = None;
                        }
                    });
                } else {
                    ui.heading(t("저장된 호스트 키와 인증서", "Remembered host keys and certificates"));
                    ui.label(t(
                        "SSH 호스트(점프 호스트 포함)와 직접 연결한 RDP 서버의 신뢰 기록입니다. 변경된 키는 자동으로 교체하지 않습니다.",
                        "Trust records for SSH hosts (including jump hosts) and direct RDP servers. Changed keys are never replaced automatically.",
                    ));
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(t("호스트 검색", "Search hosts")));
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(330.0).show(ui, |ui| {
                        if self.entries.is_empty() {
                            ui.label(t("저장된 호스트가 없습니다", "No remembered hosts"));
                        }
                        for entry in &self.entries {
                            if !entry.host.to_lowercase().contains(&self.search.to_lowercase()) {
                                continue;
                            }
                            ui.push_id((entry.kind as u8, entry.line), |ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(match entry.kind { TrustKind::Ssh => "SSH", TrustKind::Rdp => "RDP" });
                                    ui.label(&entry.host);
                                    if ui.button(t("삭제…", "Remove…")).clicked() {
                                        self.pending = Some(entry.clone());
                                    }
                                });
                                ui.label(tr!("{} · {}번째 줄", "{} · line {}", entry.algorithm, entry.line));
                                ui.monospace(&entry.fingerprint);
                                ui.separator();
                            });
                        }
                    });
                    ui.horizontal(|ui| {
                        refresh = ui.button(t("새로 고침", "Refresh")).clicked();
                        close = ui.button(t("닫기", "Close")).clicked();
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
