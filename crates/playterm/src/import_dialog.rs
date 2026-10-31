//! "PuTTY 세션 가져오기" dialog: lists PuTTY's saved sessions to pick from.

use eframe::egui::{self, RichText};
use playterm_config::{ProfileStore, SessionProfile};
use playterm_i18n::{t, tr};

use crate::putty::{self, PuttySession};
use crate::sidebar;

pub enum ImportResult {
    Open,
    Cancel,
    Import(Vec<SessionProfile>),
}

struct Item {
    session: PuttySession,
    checked: bool,
    /// A site with the same address (or the same name in the target folder) exists.
    duplicate: bool,
}

pub struct ImportDialog {
    items: Vec<Item>,
    folder: String,
    error: Option<String>,
}

/// Same protocol, host, port and user.
fn same_address(a: &SessionProfile, b: &SessionProfile) -> bool {
    a.protocol == b.protocol
        && a.host.eq_ignore_ascii_case(&b.host)
        && a.port == b.port
        && a.username == b.username
        && a.serial.port == b.serial.port
}

impl ImportDialog {
    pub fn new(store: &ProfileStore) -> Self {
        let folder = "PuTTY".to_owned();
        let (sessions, error) = match putty::load(&putty::sessions_key()) {
            Ok(s) => (s, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let items = sessions
            .into_iter()
            .map(|session| {
                let duplicate = session.profile.as_ref().is_ok_and(|p| {
                    store.profiles().iter().any(|e| same_address(e, p) || (e.name == p.name && e.folder == folder))
                });
                let checked = session.profile.is_ok() && !duplicate;
                Item { session, checked, duplicate }
            })
            .collect();
        Self { items, folder, error }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> ImportResult {
        let mut result = ImportResult::Open;
        let modal = egui::Modal::new(egui::Id::unique("putty-import")).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.heading(t("PuTTY 세션 가져오기", "Import PuTTY sessions"));
            ui.add_space(4.0);
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if self.items.is_empty() {
                ui.label(t("가져올 PuTTY 세션이 없습니다.", "There are no PuTTY sessions to import."));
                ui.weak(tr!(
                    "(레지스트리 HKEY_CURRENT_USER\\{})",
                    "(registry HKEY_CURRENT_USER\\{})",
                    putty::sessions_key()
                ));
            } else {
                ui.weak(t(
                    "비밀번호는 PuTTY 가 저장하지 않으므로 가져오지 않습니다. 이미 있는 사이트는 기본으로 선택 해제됩니다.",
                    "Passwords are not imported because PuTTY does not save them. Sites that already exist are unchecked by default.",
                ));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.small_button(t("모두 선택", "Select all")).clicked() {
                        for item in self.items.iter_mut().filter(|i| i.session.profile.is_ok()) {
                            item.checked = true;
                        }
                    }
                    if ui.small_button(t("모두 해제", "Select none")).clicked() {
                        for item in &mut self.items {
                            item.checked = false;
                        }
                    }
                });
                egui::ScrollArea::vertical().max_height(340.0).auto_shrink([false, true]).show(ui, |ui| {
                    for item in &mut self.items {
                        self::row(ui, item);
                    }
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(t("폴더", "Folder"));
                ui.add(egui::TextEdit::singleline(&mut self.folder).hint_text(t("없음", "None")).desired_width(160.0));
            });
            ui.add_space(8.0);
            let count = self.items.iter().filter(|i| i.checked).count();
            ui.horizontal(|ui| {
                if ui.add_enabled(count > 0, egui::Button::new(tr!("가져오기 ({count})", "Import ({count})"))).clicked() {
                    let folder = self.folder.trim().to_owned();
                    let profiles = self
                        .items
                        .iter()
                        .filter(|i| i.checked)
                        .filter_map(|i| i.session.profile.clone().ok())
                        .map(|p| SessionProfile { folder: folder.clone(), ..p })
                        .collect();
                    result = ImportResult::Import(profiles);
                }
                if ui.button(t("취소", "Cancel")).clicked() {
                    result = ImportResult::Cancel;
                }
            });
        });
        if matches!(result, ImportResult::Open) && modal.should_close() {
            result = ImportResult::Cancel;
        }
        result
    }
}

fn row(ui: &mut egui::Ui, item: &mut Item) {
    ui.horizontal(|ui| match &item.session.profile {
        Ok(p) => {
            ui.checkbox(&mut item.checked, RichText::new(&item.session.name).strong());
            ui.weak(sidebar::summary(p));
            if item.duplicate {
                ui.label(RichText::new(t("이미 있음", "Already exists")).color(ui.visuals().warn_fg_color));
            }
        }
        Err(reason) => {
            ui.add_enabled(false, egui::Checkbox::new(&mut false, &item.session.name));
            ui.colored_label(ui.visuals().error_fg_color, reason);
        }
    });
    for note in &item.session.notes {
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            ui.weak(format!("· {note}"));
        });
    }
}
