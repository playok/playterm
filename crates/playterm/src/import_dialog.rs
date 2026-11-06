//! "세션 가져오기" dialog: lists the sessions saved by PuTTY, Xshell or SecureCRT to pick from.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use playterm_config::{ProfileStore, SessionProfile, normalize_folder};
use playterm_i18n::{t, tr};

use crate::icon;
use crate::{putty, securecrt, sidebar, xshell};

/// Where sessions are imported from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportSource {
    Putty,
    Xshell,
    SecureCrt,
}

impl ImportSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Putty => "PuTTY",
            Self::Xshell => "Xshell",
            Self::SecureCrt => "SecureCRT",
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
    /// The sites to add, from `source`, under `folder` (the folder chosen in the dialog).
    Import {
        source: ImportSource,
        folder: String,
        profiles: Vec<SessionProfile>,
    },
}

struct Item {
    session: ImportedSession,
    checked: bool,
    /// A site with the same address (or the same name in the target folder) exists.
    duplicate: bool,
}

pub struct ImportDialog {
    source: ImportSource,
    /// The Xshell or SecureCRT sessions folder read (none found: `None`).
    dir: Option<PathBuf>,
    items: Vec<Item>,
    folder: String,
    /// Only part of the sessions folder was read (it is too big).
    truncated: bool,
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
            ImportSource::SecureCrt => securecrt::default_dir(),
        };
        let mut dialog = Self { source, dir, items: Vec::new(), folder: source.name().to_owned(), truncated: false, error: None };
        dialog.load(store);
        dialog
    }

    /// (Re)reads the sessions and marks those that already exist.
    fn load(&mut self, store: &ProfileStore) {
        let loaded = match (self.source, &self.dir) {
            (ImportSource::Putty, _) => putty::load(&putty::sessions_key()).map(|s| (s, false)),
            (ImportSource::Xshell, Some(dir)) => xshell::load(dir).map(|l| (l.sessions, l.truncated)),
            (ImportSource::SecureCrt, Some(dir)) => securecrt::load(dir).map(|l| (l.sessions, l.truncated)),
            (_, None) => Ok((Vec::new(), false)),
        };
        let (sessions, truncated, error) = match loaded {
            Ok((s, truncated)) => (s, truncated, None),
            Err(e) => (Vec::new(), false, Some(e)),
        };
        self.error = error;
        self.truncated = truncated;
        self.items = sessions.into_iter().map(|session| Item { session, checked: false, duplicate: false }).collect();
        self.mark_duplicates(store, true);
    }

    /// Marks the sessions whose site already exists (same address, or the same name in the
    /// folder it would go to). Sessions whose mark changes, or all of them with `reset`, are
    /// checked unless they exist, so a folder edit keeps the defaults right.
    fn mark_duplicates(&mut self, store: &ProfileStore, reset: bool) {
        for item in &mut self.items {
            let Ok(p) = &item.session.profile else { continue };
            let folder = target_folder(&self.folder, &item.session.folder);
            let duplicate = store.profiles().iter().any(|e| same_address(e, p) || (e.name == p.name && e.folder == folder));
            if reset || duplicate != item.duplicate {
                item.checked = !duplicate;
            }
            item.duplicate = duplicate;
        }
    }

    /// The sessions folder line (Xshell, SecureCRT), with a button to read another folder.
    fn location(&mut self, ui: &mut egui::Ui, store: &ProfileStore) {
        if self.source == ImportSource::Putty {
            return;
        }
        let source = self.source.name();
        ui.horizontal(|ui| {
            match &self.dir {
                Some(dir) => ui.weak(tr!("폴더: {}", "Folder: {}", dir.display())),
                None => ui.label(tr!("{source} 세션 폴더를 찾지 못했습니다.", "The {source} sessions folder was not found.")),
            };
            if ui.add(icon::button(icon::BROWSE, t("다른 폴더…", "Other folder…")).small()).clicked() {
                let mut picker =
                    crate::picker::Picker::new(crate::picker::Kind::Folder, tr!("{source} 세션 폴더", "{source} sessions folder"));
                if let Some(dir) = &self.dir {
                    picker = picker.directory(dir);
                }
                if let Some(dir) = picker.pick_one() {
                    self.dir = Some(dir);
                    self.load(store);
                }
            }
        });
    }

    pub fn show(&mut self, ctx: &egui::Context, store: &ProfileStore) -> ImportResult {
        let mut result = ImportResult::Open;
        let source = self.source.name();
        let modal = egui::Modal::new(egui::Id::unique(("session-import", source))).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.heading(tr!("{source} 세션 가져오기", "Import {source} sessions"));
            ui.add_space(4.0);
            if self.source == ImportSource::SecureCrt {
                password_alert(ui);
                ui.add_space(4.0);
            }
            self.location(ui, store);
            if self.truncated {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    tr!(
                        "폴더가 너무 커서 일부만 읽었습니다. {source} 세션 폴더를 직접 고르세요.",
                        "The folder is too big, so only part of it was read. Pick the {source} sessions folder itself.",
                    ),
                );
            }
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
                    ImportSource::SecureCrt => t(
                        "SecureCRT 의 하위 폴더는 아래 폴더 안에 그대로 만듭니다. 이미 있는 사이트는 기본으로 선택 해제됩니다.",
                        "SecureCRT's subfolders are kept inside the folder below. Sites that already exist are unchecked by default.",
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
                let edit = egui::TextEdit::singleline(&mut self.folder).hint_text(t("없음", "None")).desired_width(160.0);
                if ui.add(edit).changed() {
                    self.mark_duplicates(store, false);
                }
            });
            ui.add_space(8.0);
            let count = self.items.iter().filter(|i| i.checked).count();
            let import = icon::primary(ui, icon::IMPORT, tr!("가져오기 ({count})", "Import ({count})"));
            let (main, cancel) = icon::ok_cancel_row(ui, import, count > 0, icon::cancel());
            {
                if main.clicked() {
                    let folder = normalize_folder(&self.folder);
                    let profiles = self
                        .items
                        .iter()
                        .filter(|i| i.checked)
                        .filter_map(|i| {
                            let folder = target_folder(&folder, &i.session.folder);
                            i.session.profile.clone().ok().map(|p| SessionProfile { folder, ..p })
                        })
                        .collect();
                    result = ImportResult::Import { source: self.source, folder, profiles };
                }
                if cancel.clicked() {
                    result = ImportResult::Cancel;
                }
            }
        });
        if matches!(result, ImportResult::Open) && modal.should_close() {
            result = ImportResult::Cancel;
        }
        result
    }
}

/// The notice that saved passwords stay behind, shown at the top of the SecureCRT import.
fn password_alert(ui: &mut egui::Ui) {
    let warn = ui.visuals().warn_fg_color;
    egui::Frame::new().stroke(egui::Stroke::new(1.0, warn)).corner_radius(4.0).inner_margin(egui::Margin::symmetric(10, 8)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("⚠ {}", password_notice_title())).color(warn).strong());
            ui.label(password_notice_text());
        },
    );
}

/// Title of the notice that SecureCRT's saved passwords are not imported.
pub fn password_notice_title() -> &'static str {
    t("비밀번호는 가져오지 않습니다", "Passwords are not imported")
}

/// The notice that SecureCRT's saved passwords are not imported, and what to do instead.
pub fn password_notice_text() -> &'static str {
    t(
        "SecureCRT 에 저장된 비밀번호는 암호화되어 있어 가져오지 않습니다. 가져온 사이트에 처음 연결할 때 비밀번호를 입력하고 '비밀번호 저장' 을 선택하면 playterm 볼트(마스터 비밀번호로 암호화)에 저장됩니다.",
        "The passwords SecureCRT saved are encrypted and are not imported. Type the password the first time you connect to an imported site and choose 'Save password': it is kept in playterm's vault (encrypted with the master password).",
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn site(name: &str, host: &str, folder: &str) -> SessionProfile {
        SessionProfile {
            name: name.into(),
            host: host.into(),
            username: "u".into(),
            folder: folder.into(),
            ..SessionProfile::new().unwrap()
        }
    }

    fn dialog(sessions: Vec<ImportedSession>, folder: &str) -> ImportDialog {
        ImportDialog {
            source: ImportSource::Xshell,
            dir: None,
            items: sessions.into_iter().map(|session| Item { session, checked: false, duplicate: false }).collect(),
            folder: folder.into(),
            truncated: false,
            error: None,
        }
    }

    fn session(name: &str, host: &str, folder: &str) -> ImportedSession {
        ImportedSession { name: name.into(), folder: folder.into(), profile: Ok(site(name, host, "")), notes: Vec::new() }
    }

    #[test]
    fn duplicates_follow_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::load(dir.path().join("sessions.toml")).unwrap();
        store.upsert(site("web", "old-web", "Servers/운영")).unwrap();
        store.upsert(site("other", "db.example.com", "elsewhere")).unwrap();

        let mut d = dialog(vec![session("web", "new-web", "운영"), session("db", "db.example.com", "")], "Xshell");
        d.mark_duplicates(&store, true);
        let state = |d: &ImportDialog| d.items.iter().map(|i| (i.duplicate, i.checked)).collect::<Vec<_>>();
        // "web" goes to Xshell/운영 (no clash); "db" has the address of an existing site.
        assert_eq!(state(&d), [(false, true), (true, false)]);

        // Into Servers, "web" lands next to the existing "web": marked and unchecked.
        d.folder = "Servers".into();
        d.mark_duplicates(&store, false);
        assert_eq!(state(&d), [(true, false), (true, false)]);

        // A choice the user made is kept while the mark does not change.
        d.items[1].checked = true;
        d.folder = "Servers ".into();
        d.mark_duplicates(&store, false);
        assert_eq!(state(&d), [(true, false), (true, true)]);

        // Back to a free folder: checked again.
        d.folder = "New".into();
        d.mark_duplicates(&store, false);
        assert_eq!(state(&d)[0], (false, true));
    }
}
