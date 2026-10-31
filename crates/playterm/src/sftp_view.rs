//! SFTP file browser window for one SSH session.

use std::collections::BTreeSet;
use std::path::PathBuf;

use eframe::egui::{self, RichText};
use playterm_net::{RemoteEntry, SftpConnection, SftpEvent, TransferState, remote_join, remote_parent};

struct Transfer {
    id: u64,
    label: String,
    done: u64,
    total: u64,
    state: TransferState,
    /// Refresh the listing when an upload into the current directory ends.
    upload: bool,
}

/// Modal sub-dialogs of the browser.
enum Prompt {
    NewFolder(String),
    Rename { from: String, to: String },
    Delete(Vec<(String, bool)>),
}

pub struct SftpBrowser {
    conn: SftpConnection,
    pub open: bool,
    /// Canonical current directory; empty until the first listing arrives.
    cwd: String,
    path_input: String,
    entries: Vec<RemoteEntry>,
    selected: BTreeSet<String>,
    loading: bool,
    status: Option<(String, bool)>,
    transfers: Vec<Transfer>,
    prompt: Option<Prompt>,
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// `drwxr-xr-x` style string from a Unix mode.
pub fn mode_string(mode: u32, is_dir: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

fn format_time(secs: u32) -> String {
    use chrono::TimeZone as _;
    match chrono::Local.timestamp_opt(secs.into(), 0) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => String::new(),
    }
}

impl SftpBrowser {
    pub fn new(conn: SftpConnection) -> Self {
        conn.list(".");
        Self {
            conn,
            open: true,
            cwd: String::new(),
            path_input: String::new(),
            entries: Vec::new(),
            selected: BTreeSet::new(),
            loading: true,
            status: None,
            transfers: Vec::new(),
            prompt: None,
        }
    }

    fn navigate(&mut self, path: &str) {
        self.loading = true;
        self.conn.list(path);
    }

    fn refresh(&mut self) {
        if !self.cwd.is_empty() {
            let cwd = self.cwd.clone();
            self.navigate(&cwd);
        }
    }

    /// Applies pending SFTP events.
    pub fn poll(&mut self) {
        while let Some(event) = self.conn.try_recv() {
            match event {
                SftpEvent::Listing { path, result } => {
                    self.loading = false;
                    match result {
                        Ok(entries) => {
                            if path != self.cwd {
                                self.selected.clear();
                            }
                            self.selected.retain(|n| entries.iter().any(|e| &e.name == n));
                            self.cwd = path.clone();
                            self.path_input = path;
                            self.entries = entries;
                        }
                        Err(e) => {
                            self.path_input = self.cwd.clone();
                            self.status = Some((e, true));
                        }
                    }
                }
                SftpEvent::OpDone { what, result } => {
                    self.status = Some(match result {
                        Ok(()) => (format!("{what} 완료"), false),
                        Err(e) => (format!("{what} 실패: {e}"), true),
                    });
                    self.refresh();
                }
                SftpEvent::Transfer(u) => {
                    let Some(t) = self.transfers.iter_mut().find(|t| t.id == u.id) else { continue };
                    t.done = u.done;
                    t.total = u.total;
                    let ended = t.state == TransferState::Running && u.state != TransferState::Running;
                    t.state = u.state;
                    if ended && t.upload {
                        self.refresh();
                    }
                }
            }
        }
    }

    pub fn has_running_transfers(&self) -> bool {
        self.transfers.iter().any(|t| t.state == TransferState::Running)
    }

    fn track(&mut self, id: u64, label: String, upload: bool) {
        self.transfers.push(Transfer { id, label, done: 0, total: 0, state: TransferState::Running, upload });
    }

    /// Uploads local files or folders into the current directory.
    pub fn upload_paths(&mut self, paths: Vec<PathBuf>) {
        if self.cwd.is_empty() {
            return;
        }
        for path in paths {
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            let remote = remote_join(&self.cwd, &name);
            let id = self.conn.upload(&path, &remote);
            self.track(id, format!("↑ {name}"), true);
        }
    }

    fn download(&mut self, items: Vec<RemoteEntry>) {
        if items.is_empty() {
            return;
        }
        // One file: ask where to save it. Several items or folders: pick a target folder.
        let targets: Vec<(RemoteEntry, PathBuf)> = if let [single] = items.as_slice()
            && !single.is_dir
        {
            match rfd::FileDialog::new().set_title("다운로드 위치").set_file_name(&single.name).save_file() {
                Some(path) => vec![(single.clone(), path)],
                None => return,
            }
        } else {
            match rfd::FileDialog::new().set_title("다운로드할 폴더 선택").pick_folder() {
                Some(dir) => items.into_iter().map(|e| { let p = dir.join(&e.name); (e, p) }).collect(),
                None => return,
            }
        };
        for (entry, local) in targets {
            let id = self.conn.download(&remote_join(&self.cwd, &entry.name), &local, entry.is_dir);
            self.track(id, format!("↓ {}", entry.name), false);
        }
    }

    fn selected_entries(&self) -> Vec<RemoteEntry> {
        self.entries.iter().filter(|e| self.selected.contains(&e.name)).cloned().collect()
    }

    pub fn show(&mut self, ctx: &egui::Context, id: u64, title: &str) {
        let mut open = self.open;
        egui::Window::new(format!("SFTP — {title}"))
            .id(egui::Id::unique(("sftp", id)))
            .open(&mut open)
            .default_size([680.0, 440.0])
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .resizable(true)
            .show(ctx, |ui| self.contents(ui));
        self.open = open;
        self.show_prompt(ctx, id);
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("상위").clicked() && !self.cwd.is_empty() {
                let parent = remote_parent(&self.cwd);
                self.navigate(&parent);
            }
            if ui.button("새로고침").clicked() {
                self.refresh();
            }
            let path = ui.add(egui::TextEdit::singleline(&mut self.path_input).desired_width(260.0));
            if path.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let target = self.path_input.trim().to_owned();
                self.navigate(&target);
            }
            ui.separator();
            if ui.button("새 폴더").clicked() {
                self.prompt = Some(Prompt::NewFolder(String::new()));
            }
            if ui.button("업로드").on_hover_text("파일 선택 (창에 끌어다 놓아도 됩니다)").clicked()
                && let Some(files) = rfd::FileDialog::new().set_title("업로드할 파일").pick_files()
            {
                self.upload_paths(files);
            }
            if ui.button("폴더 업로드").clicked()
                && let Some(dir) = rfd::FileDialog::new().set_title("업로드할 폴더").pick_folder()
            {
                self.upload_paths(vec![dir]);
            }
            let has_selection = !self.selected.is_empty();
            if ui.add_enabled(has_selection, egui::Button::new("다운로드")).clicked() {
                let items = self.selected_entries();
                self.download(items);
            }
            if ui.add_enabled(has_selection, egui::Button::new("삭제")).clicked() {
                let items = self.selected_entries().into_iter().map(|e| (e.name, e.is_dir)).collect();
                self.prompt = Some(Prompt::Delete(items));
            }
        });
        ui.separator();

        let transfers_height = if self.transfers.is_empty() { 0.0 } else { 120.0 };
        let list_height = (ui.available_height() - transfers_height - 30.0).max(80.0);
        egui::ScrollArea::vertical().id_salt("sftp-list").max_height(list_height).auto_shrink([false, false]).show(
            ui,
            |ui| {
                if self.loading && self.entries.is_empty() {
                    ui.spinner();
                }
                self.entry_grid(ui);
            },
        );

        if !self.transfers.is_empty() {
            ui.separator();
            self.transfer_list(ui);
        }
        if let Some((text, error)) = &self.status {
            let color = if *error { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
            ui.colored_label(color, text);
        }
    }

    fn entry_grid(&mut self, ui: &mut egui::Ui) {
        let mut enter_dir = None;
        let mut download = None;
        let mut prompt = None;
        egui::Grid::new("sftp-grid").num_columns(4).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
            ui.strong("이름");
            ui.strong("크기");
            ui.strong("수정 시각");
            ui.strong("권한");
            ui.end_row();
            for entry in &self.entries {
                let label = if entry.is_dir { format!("{}/", entry.name) } else { entry.name.clone() };
                let mut text = RichText::new(label);
                if entry.is_dir {
                    text = text.strong();
                }
                let selected = self.selected.contains(&entry.name);
                let response = ui.push_id(&entry.name, |ui| ui.selectable_label(selected, text)).inner;
                if response.clicked() {
                    if ui.input(|i| i.modifiers.ctrl) {
                        if !self.selected.remove(&entry.name) {
                            self.selected.insert(entry.name.clone());
                        }
                    } else {
                        self.selected = BTreeSet::from([entry.name.clone()]);
                    }
                }
                if response.double_clicked() {
                    if entry.is_dir { enter_dir = Some(entry.name.clone()) } else { download = Some(vec![entry.clone()]) }
                }
                response.context_menu(|ui| {
                    if ui.button("다운로드").clicked() {
                        download = Some(vec![entry.clone()]);
                        ui.close();
                    }
                    if ui.button("이름 바꾸기").clicked() {
                        prompt = Some(Prompt::Rename { from: entry.name.clone(), to: entry.name.clone() });
                        ui.close();
                    }
                    if ui.button("삭제").clicked() {
                        prompt = Some(Prompt::Delete(vec![(entry.name.clone(), entry.is_dir)]));
                        ui.close();
                    }
                });
                ui.label(if entry.is_dir { String::new() } else { human_size(entry.size) });
                ui.label(entry.mtime.map(format_time).unwrap_or_default());
                ui.monospace(entry.permissions.map(|m| mode_string(m, entry.is_dir)).unwrap_or_default());
                ui.end_row();
            }
        });
        if let Some(name) = enter_dir {
            let path = remote_join(&self.cwd, &name);
            self.navigate(&path);
        }
        if let Some(items) = download {
            self.download(items);
        }
        if prompt.is_some() {
            self.prompt = prompt;
        }
    }

    fn transfer_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("전송");
            if ui.small_button("완료 항목 지우기").clicked() {
                self.transfers.retain(|t| t.state == TransferState::Running);
            }
        });
        egui::ScrollArea::vertical().id_salt("sftp-transfers").max_height(90.0).show(ui, |ui| {
            for t in &self.transfers {
                ui.horizontal(|ui| {
                    ui.label(&t.label);
                    match &t.state {
                        TransferState::Running => {
                            let frac = if t.total > 0 { t.done as f32 / t.total as f32 } else { 0.0 };
                            let text = format!("{} / {}", human_size(t.done), human_size(t.total));
                            ui.add(egui::ProgressBar::new(frac).text(text).desired_width(220.0));
                            if ui.small_button("취소").clicked() {
                                self.conn.cancel(t.id);
                            }
                        }
                        TransferState::Finished => {
                            ui.label(format!("완료 ({})", human_size(t.done)));
                        }
                        TransferState::Cancelled => {
                            ui.weak("취소됨");
                        }
                        TransferState::Failed(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, format!("실패: {e}"));
                        }
                    }
                });
            }
        });
    }

    fn show_prompt(&mut self, ctx: &egui::Context, id: u64) {
        let Some(prompt) = &mut self.prompt else { return };
        let mut done: Option<bool> = None;
        let modal = egui::Modal::new(egui::Id::unique(("sftp-prompt", id))).show(ctx, |ui| {
            ui.set_width(340.0);
            let enter = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            match prompt {
                Prompt::NewFolder(name) => {
                    ui.heading("새 폴더");
                    let r = ui.text_edit_singleline(name);
                    // Check Enter before re-focusing: Enter makes the field give up focus.
                    if enter(ui, &r) { done = Some(true) } else { r.request_focus() }
                }
                Prompt::Rename { from, to } => {
                    ui.heading(format!("'{from}' 이름 바꾸기"));
                    let r = ui.text_edit_singleline(to);
                    if enter(ui, &r) { done = Some(true) } else { r.request_focus() }
                }
                Prompt::Delete(items) => {
                    ui.heading("삭제");
                    let names: Vec<&str> = items.iter().map(|(n, _)| n.as_str()).collect();
                    ui.label(format!("{} 을(를) 삭제할까요?", names.join(", ")));
                    if items.iter().any(|(_, dir)| *dir) {
                        ui.colored_label(ui.visuals().warn_fg_color, "폴더는 안의 내용까지 모두 삭제됩니다.");
                    }
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("확인").clicked() {
                    done = Some(true);
                }
                if ui.button("취소").clicked() {
                    done = Some(false);
                }
            });
        });
        if modal.should_close() && done.is_none() {
            done = Some(false);
        }
        let Some(confirmed) = done else { return };
        let prompt = self.prompt.take().expect("checked above");
        if !confirmed {
            return;
        }
        match prompt {
            Prompt::NewFolder(name) if !name.trim().is_empty() => self.conn.mkdir(&remote_join(&self.cwd, name.trim())),
            Prompt::Rename { from, to } if !to.trim().is_empty() && to.trim() != from => {
                self.conn.rename(&remote_join(&self.cwd, &from), &remote_join(&self.cwd, to.trim()));
            }
            Prompt::Delete(items) => {
                for (name, is_dir) in items {
                    self.conn.remove(&remote_join(&self.cwd, &name), is_dir);
                }
                self.selected.clear();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(mode_string(0o755, true), "drwxr-xr-x");
        assert_eq!(mode_string(0o100644, false), "-rw-r--r--");
    }
}
