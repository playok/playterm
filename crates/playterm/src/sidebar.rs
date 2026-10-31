//! Saved-site list on the left side of the window. Sites are edited in the site manager;
//! this list is for quick access.

use eframe::egui::{self, RichText};
use playterm_config::{ProfileStore, SessionProfile};

pub enum SidebarAction {
    Connect(String),
    /// Open the site manager, with this site selected if any.
    Manage(Option<String>),
    /// Open the site manager on a new site.
    New,
    Duplicate(String),
    Delete(String),
    LockVault,
    UnlockVault,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VaultState {
    Missing,
    Locked,
    Unlocked,
}

/// Sites matching `filter` (name, host, user or folder), grouped by folder (top level first)
/// and sorted by name within each folder.
pub fn grouped<'a>(store: &'a ProfileStore, filter: &str) -> Vec<(String, Vec<&'a SessionProfile>)> {
    let filter = filter.trim().to_lowercase();
    let matches = |p: &SessionProfile| {
        filter.is_empty()
            || [&p.display_name(), &p.host, &p.username, &p.folder].iter().any(|s| s.to_lowercase().contains(&filter))
    };
    let mut sorted: Vec<&SessionProfile> = store.profiles().iter().filter(|p| matches(p)).collect();
    sorted.sort_by_key(|p| p.display_name().to_lowercase());
    store
        .folders()
        .into_iter()
        .map(|folder| {
            let items: Vec<&SessionProfile> = sorted.iter().copied().filter(|p| p.folder.trim() == folder).collect();
            (folder, items)
        })
        .filter(|(_, items)| !items.is_empty())
        .collect()
}

/// Hover text for a site entry.
pub fn summary(p: &SessionProfile) -> String {
    match p.protocol {
        playterm_net::Protocol::Serial => format!("Serial {} {}", p.serial.port, p.serial.line_summary()),
        proto => format!("{proto} {}:{}", p.host, p.port),
    }
}

#[derive(Default)]
pub struct Sidebar {
    filter: String,
    selected: Option<String>,
}

impl Sidebar {
    /// The site last clicked in the list.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, store: &ProfileStore, vault: VaultState) -> Option<SidebarAction> {
        let mut action = None;

        ui.horizontal(|ui| {
            ui.strong("사이트");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("관리").on_hover_text("사이트 관리 (Ctrl+Shift+S)").clicked() {
                    action = Some(SidebarAction::Manage(self.selected.clone()));
                }
                if ui.button("+").on_hover_text("새 사이트").clicked() {
                    action = Some(SidebarAction::New);
                }
            });
        });
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("검색").desired_width(f32::INFINITY));

        ui.horizontal(|ui| {
            let (text, button) = match vault {
                VaultState::Missing => ("비밀번호 볼트: 없음", None),
                VaultState::Locked => ("비밀번호 볼트: 잠김", Some(("잠금 해제", SidebarAction::UnlockVault))),
                VaultState::Unlocked => ("비밀번호 볼트: 열림", Some(("잠그기", SidebarAction::LockVault))),
            };
            ui.small(text);
            if let Some((label, a)) = button
                && ui.small_button(label).clicked()
            {
                action = Some(a);
            }
        });
        ui.separator();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if store.profiles().is_empty() {
                ui.weak("저장된 사이트가 없습니다.\n[+] 로 추가하세요.");
                return;
            }
            for (folder, items) in grouped(store, &self.filter) {
                if folder.is_empty() {
                    self.items(ui, &items, &mut action);
                } else {
                    egui::CollapsingHeader::new(RichText::new(&folder).strong())
                        .id_salt(("folder", &folder))
                        .default_open(true)
                        .show(ui, |ui| self.items(ui, &items, &mut action));
                }
            }
        });
        action
    }

    fn items(&mut self, ui: &mut egui::Ui, items: &[&SessionProfile], action: &mut Option<SidebarAction>) {
        for p in items {
            let selected = self.selected.as_deref() == Some(p.id.as_str());
            // Explicit id: auto ids inside a collapsing body are not stable across frames,
            // which loses the context-menu popup state.
            let response = ui
                .push_id(&p.id, |ui| ui.selectable_label(selected, p.display_name()))
                .inner
                .on_hover_text(format!("{}  (더블클릭: 연결)", summary(p)));
            if response.clicked() {
                self.selected = Some(p.id.clone());
            }
            if response.double_clicked() {
                *action = Some(SidebarAction::Connect(p.id.clone()));
            }
            response.context_menu(|ui| {
                let entries = [
                    ("연결", SidebarAction::Connect(p.id.clone())),
                    ("편집…", SidebarAction::Manage(Some(p.id.clone()))),
                    ("복제", SidebarAction::Duplicate(p.id.clone())),
                    ("삭제", SidebarAction::Delete(p.id.clone())),
                ];
                for (label, a) in entries {
                    if ui.button(label).clicked() {
                        *action = Some(a);
                        ui.close();
                    }
                }
            });
        }
    }
}
