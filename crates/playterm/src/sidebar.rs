//! Saved-site list on the left side of the window. Sites are edited in the site manager;
//! this list is for quick access.

use eframe::egui::{self, RichText};
use playterm_config::{ProfileStore, SessionProfile};
use playterm_i18n::{t, tr};

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

/// Whether a site's name, host, user or folder contains `filter` (ignoring case).
pub fn matches(p: &SessionProfile, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    filter.is_empty()
        || [&p.display_name(), &p.host, &p.username, &p.folder].iter().any(|s| s.to_lowercase().contains(&filter))
}

/// Sites matching `filter` (see [`matches`]), grouped by folder (top level first) and sorted
/// by name within each folder.
pub fn grouped<'a>(store: &'a ProfileStore, filter: &str) -> Vec<(String, Vec<&'a SessionProfile>)> {
    let mut sorted: Vec<&SessionProfile> = store.profiles().iter().filter(|p| matches(p, filter)).collect();
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

/// Where a site connects: `SSH user@host:22`, `Telnet [::1]:23`, `Serial COM3 9600 8N1`.
pub fn summary(p: &SessionProfile) -> String {
    let host = if p.host.contains(':') { format!("[{}]", p.host) } else { p.host.clone() };
    match p.protocol {
        playterm_net::Protocol::Serial => format!("Serial {} {}", p.serial.port, p.serial.line_summary()),
        playterm_net::Protocol::Ssh if !p.username.is_empty() => format!("SSH {}@{host}:{}", p.username, p.port),
        proto => format!("{proto} {host}:{}", p.port),
    }
}

/// Finds a site in `sites` (numbered from 1, in `sites` order) by exact name, by number, or
/// by a part of its name or host that only one site has. Returns its id or what went wrong.
pub fn find(sites: &[&SessionProfile], query: &str) -> Result<String, String> {
    let query = query.trim();
    let q = query.to_lowercase();
    if let Some(p) = sites.iter().find(|p| p.display_name().to_lowercase() == q) {
        return Ok(p.id.clone());
    }
    let number = q.parse::<usize>().ok();
    if let Some(p) = number.and_then(|n| n.checked_sub(1)).and_then(|k| sites.get(k)) {
        return Ok(p.id.clone());
    }
    // Not a valid number: it may still be part of a name or an address (`open 192`).
    let hits: Vec<&&SessionProfile> = sites
        .iter()
        .filter(|p| p.display_name().to_lowercase().contains(&q) || p.host.to_lowercase().contains(&q))
        .collect();
    match hits.as_slice() {
        [p] => Ok(p.id.clone()),
        [] if number.is_some() => {
            Err(tr!(
                "{query}번 사이트가 없고, '{query}' 와 일치하는 사이트도 없습니다 (sites 로 확인)",
                "There is no site number {query} and no site matches '{query}' (check with sites)"
            ))
        }
        [] => Err(tr!(
            "'{query}' 사이트를 찾을 수 없습니다 (sites 로 목록 보기)",
            "Site '{query}' not found (type sites for a list)"
        )),
        many => {
            let names: Vec<String> = many.iter().map(|p| p.display_name()).collect();
            Err(tr!("여러 사이트가 일치합니다: {}", "Several sites match: {}", names.join(", ")))
        }
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
            ui.strong(t("사이트", "Sites"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(t("관리", "Manage"))
                    .on_hover_text(t("사이트 관리 (Ctrl+Shift+S)", "Site Manager (Ctrl+Shift+S)"))
                    .clicked()
                {
                    action = Some(SidebarAction::Manage(self.selected.clone()));
                }
                if ui.button("+").on_hover_text(t("새 사이트", "New site")).clicked() {
                    action = Some(SidebarAction::New);
                }
            });
        });
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(t("검색", "Search")).desired_width(f32::INFINITY));

        ui.horizontal(|ui| {
            let (text, button) = match vault {
                VaultState::Missing => (t("비밀번호 볼트: 없음", "Password vault: none"), None),
                VaultState::Locked => (
                    t("비밀번호 볼트: 잠김", "Password vault: locked"),
                    Some((t("잠금 해제", "Unlock"), SidebarAction::UnlockVault)),
                ),
                VaultState::Unlocked => (
                    t("비밀번호 볼트: 열림", "Password vault: unlocked"),
                    Some((t("잠그기", "Lock"), SidebarAction::LockVault)),
                ),
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
                ui.weak(t("저장된 사이트가 없습니다.\n[+] 로 추가하세요.", "No saved sites.\nAdd one with [+]."));
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
            // A site with a tab color shows it as a bar before its name.
            let response = ui
                .push_id(&p.id, |ui| {
                    ui.horizontal(|ui| {
                        if let Some(color) = p.tab_color.map(crate::menu::color32) {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(4.0, 16.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 1.0, color);
                        }
                        ui.selectable_label(selected, p.display_name())
                    })
                    .inner
                })
                .inner
                .on_hover_text(tr!("{}  (더블클릭: 연결)", "{}  (double-click: connect)", summary(p)));
            if response.clicked() {
                self.selected = Some(p.id.clone());
            }
            if response.double_clicked() {
                *action = Some(SidebarAction::Connect(p.id.clone()));
            }
            response.context_menu(|ui| {
                let entries = [
                    (t("연결", "Connect"), SidebarAction::Connect(p.id.clone())),
                    (t("편집…", "Edit…"), SidebarAction::Manage(Some(p.id.clone()))),
                    (t("복제", "Duplicate"), SidebarAction::Duplicate(p.id.clone())),
                    (t("삭제", "Delete"), SidebarAction::Delete(p.id.clone())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use playterm_net::Protocol;

    fn site(id: &str, name: &str, host: &str) -> SessionProfile {
        SessionProfile { id: id.into(), name: name.into(), host: host.into(), ..Default::default() }
    }

    #[test]
    fn finds_sites() {
        let a = site("a", "웹서버", "192.168.0.10");
        let b = site("b", "DB", "db.example.com");
        let c = site("c", "10", "10.0.0.1");
        let sites = [&a, &b, &c];
        assert_eq!(find(&sites, "웹서버"), Ok("a".into()));
        assert_eq!(find(&sites, "db"), Ok("b".into()), "exact name, ignoring case");
        assert_eq!(find(&sites, "2"), Ok("b".into()), "number");
        assert_eq!(find(&sites, "10"), Ok("c".into()), "an exact name wins over the number");
        assert_eq!(find(&sites, "192"), Ok("a".into()), "no site 192: part of an address");
        assert_eq!(find(&sites, "example"), Ok("b".into()));
        assert!(find(&sites, "0").unwrap_err().contains("여러 사이트"), "no site 0, two addresses with 0");
        assert!(find(&sites, "99").unwrap_err().contains("99번"));
        assert!(find(&sites, "nope").unwrap_err().contains("찾을 수 없습니다"));
    }

    #[test]
    fn summaries() {
        let mut p = site("a", "", "web");
        p.username = "me".into();
        assert_eq!(summary(&p), "SSH me@web:22");
        p.host = "::1".into();
        assert_eq!(summary(&p), "SSH me@[::1]:22");
        p.protocol = Protocol::Telnet;
        p.port = 23;
        assert_eq!(summary(&p), "Telnet [::1]:23");
    }
}
