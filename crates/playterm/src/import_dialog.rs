//! "세션 가져오기" dialog: lists the sessions saved by PuTTY or Xshell to pick from.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use playterm_config::{ProfileStore, SessionProfile, normalize_folder};
use playterm_i18n::{t, tr};

use crate::{putty, sidebar, xshell};

/// Where sessions are imported from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportSource {
    Putty,
    Xshell,
}

impl ImportSource {
    fn name(self) -> &'static str {
        match self {
            Self::Putty => "PuTTY",
            Self::Xshell => "Xshell",
        }
    }
}

/// One saved session of another program and what it becomes here.
#[derive(Clone, Debug)]
pub struct ImportedSession {
    pub name: String,
    /// Its folder in the other program ("" at the top, `a/b` below), kept under the folder
    /// chosen in the dialog.
    pub folder: String,
    /// The site, or why it cannot be imported.
    pub profile: Result<SessionProfile, String>,
    /// Settings that were not carried over or need attention.
    pub notes: Vec<String>,
}

pub enum ImportResult {
    Open,
    Cancel,
    Import(Vec<SessionProfile>),
}

struct Item {
    session: ImportedSession,
    checked: bool,
    /// A site with the same address (or the same name in the target folder) exists.
    duplicate: bool,
}

pub struct ImportDialog {
    source: ImportSource,
    /// The Xshell Sessions folder read (none found: `None`).
    dir: Option<PathBuf>,
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

/// Folder `sub` (from the other program) under `base` (chosen in the dialog).
fn target_folder(base: &str, sub: &str) -> String {
    normalize_folder(&format!("{base}/{sub}"))
}

impl ImportDialog {
    pub fn new(store: &ProfileStore, source: ImportSource) -> Self {
        let dir = match source {
            ImportSource::Putty => None,
            ImportSource::Xshell => xshell::default_dir(),
        };
        let mut dialog =
            Self { source, dir, items: Vec::new(), folder: source.name().to_owned(), error: None };
        dialog.load(store);
        dialog
    }

    /// (Re)reads the sessions and marks those that already exist.
    fn load(&mut self, store: &ProfileStore) {
        let loaded = match (self.source, &self.dir) {
            (ImportSource::Putty, _) => putty::load(&putty::sessions_key()),
            (ImportSource::Xshell, Some(dir)) => xshell::load(dir),
            (ImportSource::Xshell, None) => Ok(Vec::new()),
        };
        let (sessions, error) = match loaded {
            Ok(s) => (s, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        self.error = error;
        self.items = sessions
            .into_iter()
            .map(|session| {
                let folder = target_folder(&self.folder, &session.folder);
                let duplicate = session.profile.as_ref().is_ok_and(|p| {
                    store.profiles().iter().any(|e| same_address(e, p) || (e.name == p.name && e.folder == folder))
                });
                let checked = session.profile.is_ok() && !duplicate;
                Item { session, checked, duplicate }
            })
            .collect();
    }

    /// The Xshell folder line, with a button to read another folder.
    fn location(&mut self, ui: &mut egui::Ui, store: &ProfileStore) {
        if self.source != ImportSource::Xshell {
            return;
        }
        ui.horizontal(|ui| {
            match &self.dir {
                Some(dir) => ui.weak(tr!("폴더: {}", "Folder: {}", dir.display())),
                None => ui.label(t("Xshell 세션 폴더를 찾지 못했습니다.", "The Xshell sessions folder was not found.")),
            };
            if ui.small_button(t("다른 폴더…", "Other folder…")).clicked() {
                let mut dialog = rfd::FileDialog::new().set_title(t("Xshell 세션 폴더", "Xshell sessions folder"));
                if let Some(dir) = &self.dir {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(dir) = dialog.pick_folder() {
                    self.dir = Some(dir);
                    self.load(store);
                }
            }
        });
    }

    pub fn show(&mut self, ctx: &egui::Context, store: &ProfileStore) -> ImportResult {
        let mut result = ImportResult::Open;
        let source = self.source.name();
        let modal = egui::Modal::new(egui::Id::unique(("session-import", source))).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.heading(tr!("{source} 세션 가져오기", "Import {source} sessions"));
            ui.add_space(4.0);
            self.location(ui, store);
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if self.items.is_empty() {
                ui.label(tr!("가져올 {source} 세션이 없습니다.", "There are no {source} sessions to import."));
                if self.source == ImportSource::Putty {
                    ui.weak(tr!(
                        "(레지스트리 HKEY_CURRENT_USER\\{})",
                        "(registry HKEY_CURRENT_USER\\{})",
                        putty::sessions_key()
                    ));
                }
            } else {
                ui.weak(match self.source {
                    ImportSource::Putty => t(
                        "비밀번호는 PuTTY 가 저장하지 않으므로 가져오지 않습니다. 이미 있는 사이트는 기본으로 선택 해제됩니다.",
                        "Passwords are not imported because PuTTY does not save them. Sites that already exist are unchecked by default.",
                    ),
                    ImportSource::Xshell => t(
                        "Xshell 의 하위 폴더는 아래 폴더 안에 그대로 만듭니다. 비밀번호는 가져오지 않으며, 이미 있는 사이트는 기본으로 선택 해제됩니다.",
                        "Xshell's subfolders are kept inside the folder below. Passwords are not imported, and sites that already exist are unchecked by default.",
                    ),
                });
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
                        .filter_map(|i| {
                            let folder = target_folder(&folder, &i.session.folder);
                            i.session.profile.clone().ok().map(|p| SessionProfile { folder, ..p })
                        })
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
            let name = match item.session.folder.as_str() {
                "" => item.session.name.clone(),
                folder => format!("{folder}/{}", item.session.name),
            };
            ui.checkbox(&mut item.checked, RichText::new(name).strong());
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
