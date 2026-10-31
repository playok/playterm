//! Saved-session list on the left side of the window.

use eframe::egui::{self, RichText};
use playterm_config::{ProfileStore, SessionProfile};

pub enum SidebarAction {
    Connect(String),
    Edit(String),
    Duplicate(String),
    Delete(String),
    New,
    LockVault,
    UnlockVault,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VaultState {
    Missing,
    Locked,
    Unlocked,
}

#[derive(Default)]
pub struct Sidebar {
    filter: String,
    selected: Option<String>,
}

impl Sidebar {
    pub fn show(&mut self, ui: &mut egui::Ui, store: &ProfileStore, vault: VaultState) -> Option<SidebarAction> {
        let mut action = None;

        ui.horizontal(|ui| {
            ui.strong("세션");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+").on_hover_text("새 세션").clicked() {
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

        let filter = self.filter.trim().to_lowercase();
        let matches = |p: &SessionProfile| {
            filter.is_empty()
                || [&p.display_name(), &p.host, &p.username, &p.folder]
                    .iter()
                    .any(|s| s.to_lowercase().contains(&filter))
        };
        let mut sorted: Vec<&SessionProfile> = store.profiles().iter().filter(|p| matches(p)).collect();
        sorted.sort_by_key(|p| p.display_name().to_lowercase());

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if store.profiles().is_empty() {
                ui.weak("저장된 세션이 없습니다.\n[+] 로 추가하세요.");
                return;
            }
            for folder in store.folders() {
                let items: Vec<&SessionProfile> =
                    sorted.iter().copied().filter(|p| p.folder.trim() == folder).collect();
                if items.is_empty() {
                    continue;
                }
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
                .on_hover_text(match p.protocol {
                    playterm_net::Protocol::Serial => {
                        format!("Serial {} {}  (더블클릭: 연결)", p.serial.port, p.serial.line_summary())
                    }
                    proto => format!("{proto} {}:{}  (더블클릭: 연결)", p.host, p.port),
                });
            if response.clicked() {
                self.selected = Some(p.id.clone());
            }
            if response.double_clicked() {
                *action = Some(SidebarAction::Connect(p.id.clone()));
            }
            response.context_menu(|ui| {
                let entries = [
                    ("연결", SidebarAction::Connect(p.id.clone())),
                    ("편집", SidebarAction::Edit(p.id.clone())),
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
