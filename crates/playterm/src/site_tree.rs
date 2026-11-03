//! The saved sites as a tree of folders (`운영/웹` is `웹` inside `운영`), shared by the site
//! list and the site manager: folder rows with a folder icon, and a badge with each site's
//! connection type (SSH, Telnet, serial, RDP).



use eframe::egui::{self, Color32, RichText};
use eframe::egui::collapsing_header::CollapsingState;
use playterm_config::{ProfileStore, SessionProfile, folder_name};
use playterm_i18n::t;
use playterm_net::Protocol;

use crate::file_icons::{self, FileKind};
use crate::sidebar;

/// A folder with the folders and sites in it (the root has path "").
#[derive(Default)]
pub struct Folder<'a> {
    pub path: String,
    pub folders: Vec<Folder<'a>>,
    pub sites: Vec<&'a SessionProfile>,
}

impl Folder<'_> {
    pub fn name(&self) -> &str {
        folder_name(&self.path)
    }

    /// Sites in this folder and in the folders inside it.
    pub fn site_count(&self) -> usize {
        self.sites.len() + self.folders.iter().map(Folder::site_count).sum::<usize>()
    }
}

/// The folder tree of `store` with the sites that match `filter`. While filtering, folders
/// without a matching site are left out.
pub fn build<'a>(store: &'a ProfileStore, filter: &str) -> Folder<'a> {
    let mut root = Folder::default();
    for path in store.folders().iter().filter(|f| !f.is_empty()) {
        node(&mut root, path);
    }
    for p in store.profiles().iter().filter(|p| sidebar::matches(p, filter)) {
        node(&mut root, &p.folder).sites.push(p);
    }
    sort(&mut root);
    if !filter.trim().is_empty() {
        prune(&mut root);
    }
    root
}

/// The folder at `path` under `root`, made (with the folders above it) if missing.
fn node<'f, 'a>(root: &'f mut Folder<'a>, path: &str) -> &'f mut Folder<'a> {
    let mut current = root;
    let mut so_far = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        if !so_far.is_empty() {
            so_far.push('/');
        }
        so_far.push_str(part);
        let index = match current.folders.iter().position(|f| f.path == so_far) {
            Some(i) => i,
            None => {
                current.folders.push(Folder { path: so_far.clone(), ..Default::default() });
                current.folders.len() - 1
            }
        };
        current = &mut current.folders[index];
    }
    current
}

fn sort(folder: &mut Folder) {
    folder.folders.sort_by_key(|f| f.name().to_lowercase());
    folder.sites.sort_by_key(|p| p.display_name().to_lowercase());
    folder.folders.iter_mut().for_each(sort);
}

fn prune(folder: &mut Folder) {
    folder.folders.iter_mut().for_each(prune);
    folder.folders.retain(|f| f.site_count() > 0);
}

/// A folder row (arrow, folder icon, name; a click on the name opens or closes it) with
/// `add_body` drawing its contents when open. Returns the name's response, for menus.
pub fn folder<R>(ui: &mut egui::Ui, folder: &Folder, salt: &str, add_body: impl FnOnce(&mut egui::Ui) -> R) -> egui::Response {
    let id = ui.make_persistent_id((salt, &folder.path));
    let (_, header, _) = CollapsingState::load_with_default_open(ui.ctx(), id, true)
        .show_header(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            file_icons::icon(ui, FileKind::Folder, false);
            ui.add(egui::Label::new(RichText::new(folder.name()).strong()).sense(egui::Sense::click()).selectable(false))
        })
        .body(add_body);
    let response = header.inner;
    if response.clicked()
        && let Some(mut state) = CollapsingState::load(ui.ctx(), id)
    {
        state.toggle(ui);
        state.store(ui.ctx());
    }
    response
}

/// Width of the tab-color bar before a site.
pub const COLOR_BAR: f32 = 4.0;

/// The start of a site row: room where a folder row has its expand arrow, then the tab-color
/// bar (its space kept when the site has no color). So every site's badge and name line up,
/// at the top level and inside folders, under the folder icons of their level.
pub fn row_lead(ui: &mut egui::Ui, color: Option<Color32>) {
    // A folder row's icon comes after its expand button (the indent) and the header's margin
    // (2 px); the badge here comes after this space, the bar and one item spacing. Measured on
    // both trees (item spacing 8 in the sidebar, 4 in the site manager).
    let spacing = ui.spacing().item_spacing.x;
    let arrow = (ui.spacing().indent + 2.0 - COLOR_BAR - spacing).max(0.0);
    ui.add_space(arrow);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(COLOR_BAR, 16.0), egui::Sense::hover());
    if let Some(color) = color {
        ui.painter().rect_filled(rect, 1.0, color);
    }
}

/// The color and short name of a connection type.
fn kind(protocol: Protocol) -> (&'static str, Color32) {
    match protocol {
        Protocol::Ssh => ("SSH", Color32::from_rgb(46, 140, 80)),
        Protocol::Telnet => ("TEL", Color32::from_rgb(200, 120, 30)),
        Protocol::Serial => ("COM", Color32::from_rgb(140, 80, 190)),
        Protocol::Rdp => ("RDP", Color32::from_rgb(40, 110, 200)),
    }
}

/// A small colored badge with the connection type of a site.
pub fn badge(ui: &mut egui::Ui, protocol: Protocol) {
    let (text, color) = kind(protocol);
    let name = match protocol {
        Protocol::Ssh => "SSH",
        Protocol::Telnet => "Telnet",
        Protocol::Serial => t("시리얼", "Serial"),
        Protocol::Rdp => t("원격 데스크톱 (RDP)", "Remote Desktop (RDP)"),
    };
    ui.add(
        egui::Label::new(RichText::new(text).monospace().size(10.0).strong().color(Color32::WHITE).background_color(color))
            .selectable(false),
    )
    .on_hover_text(name);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(sites: &[(&str, &str)], folders: &[&str]) -> (tempfile::TempDir, ProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::load(dir.path().join("sessions.toml")).unwrap();
        for (name, folder) in sites {
            store.upsert(SessionProfile { name: (*name).into(), folder: (*folder).into(), ..Default::default() }).unwrap();
        }
        for f in folders {
            store.create_folder(f).unwrap();
        }
        (dir, store)
    }

    fn shape(f: &Folder) -> String {
        let mut out = String::new();
        for sub in &f.folders {
            out.push_str(&format!("{}[{}] ", sub.name(), shape(sub).trim_end()));
        }
        for s in &f.sites {
            out.push_str(&format!("{} ", s.name));
        }
        out
    }

    #[test]
    fn nested_folders_with_their_sites() {
        let (_dir, store) = store_with(&[("web1", "운영/웹"), ("db1", "운영"), ("lab", ""), ("Zeta", "운영/웹")], &["빈 폴더"]);
        assert_eq!(shape(&build(&store, "")).trim(), "빈 폴더[] 운영[웹[web1 Zeta] db1] lab");
        // While searching, only folders with a match are shown.
        assert_eq!(shape(&build(&store, "web")).trim(), "운영[웹[web1]]");
        assert_eq!(build(&store, "").site_count(), 4);
    }
}
