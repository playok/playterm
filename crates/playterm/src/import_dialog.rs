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
                Some(dir) => ui.weak(tr!("import_dialog.folder", arg0 = dir.display())),
                None => ui.label(tr!("import_dialog.the_sessions_folder_was_not_found", source = source)),
            };
            if ui.add(icon::button(icon::BROWSE, t!("import_dialog.other_folder")).small()).clicked() {
                let mut picker =
                    crate::picker::Picker::new(crate::picker::Kind::Folder, tr!("import_dialog.sessions_folder", source = source));
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
        let modal = egui::Modal::new(egui::Id::unique(("session-import", source))).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(
            ctx,
            |ui| {
                ui.set_width(600.0);
                ui.heading(tr!("import_dialog.import_sessions", source = source));
                ui.add_space(4.0);
                if self.source == ImportSource::SecureCrt {
                    password_alert(ui);
                    ui.add_space(4.0);
                }
                self.location(ui, store);
                if self.truncated {
                    ui.colored_label(ui.visuals().warn_fg_color, tr!("import_dialog.the_folder_is_too_big_so_only_part", source = source));
                }
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                } else if self.items.is_empty() {
                    ui.label(tr!("import_dialog.there_are_no_sessions_to_import", source = source));
                    if self.source == ImportSource::Putty {
                        ui.weak(tr!("import_dialog.registry_hkey_current_user", arg0 = putty::sessions_key()));
                    }
                } else {
                    ui.weak(match self.source {
                        ImportSource::Putty => t!("import_dialog.passwords_are_not_imported_because_putty_does_not"),
                        ImportSource::Xshell => t!("import_dialog.xshell_s_subfolders_are_kept_inside_the_folder"),
                        ImportSource::SecureCrt => t!("import_dialog.securecrt_s_subfolders_are_kept_inside_the_folder"),
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.small_button(t!("common.select_all")).clicked() {
                            for item in self.items.iter_mut().filter(|i| i.session.profile.is_ok()) {
                                item.checked = true;
                            }
                        }
                        if ui.small_button(t!("import_dialog.select_none")).clicked() {
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
                    ui.label(t!("common.folder"));
                    let edit = egui::TextEdit::singleline(&mut self.folder).hint_text(t!("common.none")).desired_width(160.0);
                    if ui.add(edit).changed() {
                        self.mark_duplicates(store, false);
                    }
                });
                ui.add_space(8.0);
                let count = self.items.iter().filter(|i| i.checked).count();
                let import = icon::primary(ui, icon::IMPORT, tr!("import_dialog.import", count = count));
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
            },
        );
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
    t!("import_dialog.passwords_are_not_imported")
}

/// The notice that SecureCRT's saved passwords are not imported, and what to do instead.
pub fn password_notice_text() -> &'static str {
    t!("import_dialog.the_passwords_securecrt_saved_are_encrypted_and_are")
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
                ui.label(RichText::new(t!("import_dialog.already_exists")).color(ui.visuals().warn_fg_color));
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
