//! SFTP file browser of one SSH session, shown as a window and/or in the side panel.

use std::collections::BTreeSet;
use std::path::PathBuf;

use eframe::egui::{self, RichText};

use crate::file_icons;
use playterm_i18n::{t, tr};
use playterm_net::{
    RemoteEntry, SftpConnection, SftpEvent, TransferState, is_safe_local_name, remote_join, remote_parent,
};

struct Transfer {
    id: u64,
    label: String,
    done: u64,
    total: u64,
    state: TransferState,
    /// Refresh the listing when an upload into the current directory ends.
    upload: bool,
    /// Where a download is saved, for "open folder".
    local: Option<PathBuf>,
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
    /// The user asked to go to the terminal's current directory (see `Session::poll`).
    pub follow_requested: bool,
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
    /// Opens at `start` (the terminal's current directory, when known), else the login
    /// directory.
    pub fn new(conn: SftpConnection, start: Option<String>) -> Self {
        conn.list(start.as_deref().unwrap_or("."));
        Self {
            conn,
            open: true,
            follow_requested: false,
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

    /// Shows `dir` (the terminal's current directory).
    pub fn go_to(&mut self, dir: &str) {
        self.navigate(dir);
    }

    pub fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some((text.into(), error));
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
                        Err(e) if self.cwd.is_empty() => {
                            // The first directory could not be listed (no permission, say):
                            // the login directory instead.
                            self.status = Some((e, true));
                            self.navigate(".");
                        }
                        Err(e) => {
                            self.path_input = self.cwd.clone();
                            self.status = Some((e, true));
                        }
                    }
                }
                SftpEvent::OpDone { what, result } => {
                    self.status = Some(match result {
                        Ok(()) => (tr!("{what} 완료", "{what}: done"), false),
                        Err(e) => (tr!("{what} 실패: {e}", "{what} failed: {e}"), true),
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

    fn track(&mut self, id: u64, label: String, upload: bool, local: Option<PathBuf>) {
        let state = TransferState::Running;
        self.transfers.push(Transfer { id, label, done: 0, total: 0, state, upload, local });
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
            self.track(id, format!("↑ {name}"), true, None);
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
            match rfd::FileDialog::new().set_title(t("다운로드 위치", "Download to")).set_file_name(&single.name).save_file() {
                Some(path) => vec![(single.clone(), path)],
                None => return,
            }
        } else {
            match rfd::FileDialog::new().set_title(t("다운로드할 폴더 선택", "Choose a folder to download to")).pick_folder() {
                Some(dir) => {
                    // The server names these: one that could reach outside the folder is refused.
                    let (safe, unsafe_names): (Vec<_>, Vec<_>) =
                        items.into_iter().partition(|e| is_safe_local_name(&e.name));
                    if !unsafe_names.is_empty() {
                        let names: Vec<String> = unsafe_names.iter().map(|e| format!("{:?}", e.name)).collect();
                        let text = tr!(
                            "이 PC 에 저장할 수 없는 이름이라 받지 않았습니다: {}",
                            "Not downloaded, the names cannot be saved on this PC: {}",
                            names.join(", ")
                        );
                        self.status = Some((text, true));
                    }
                    safe.into_iter().map(|e| { let p = dir.join(&e.name); (e, p) }).collect()
                }
                None => return,
            }
        };
        for (entry, local) in targets {
            let id = self.conn.download(&remote_join(&self.cwd, &entry.name), &local, entry.is_dir);
            self.track(id, format!("↓ {}", entry.name), false, Some(local));
        }
    }

    fn selected_entries(&self) -> Vec<RemoteEntry> {
        self.entries.iter().filter(|e| self.selected.contains(&e.name)).cloned().collect()
    }

    /// The window (while [`Self::open`]) and the browser's dialogs (new folder, rename, delete),
    /// which the side panel uses too.
    pub fn show(&mut self, ctx: &egui::Context, id: u64, title: &str) {
        if self.open {
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
        }
        self.show_prompt(ctx, id);
    }

    /// The browser laid out for the narrow side panel: path on its own line, wrapped buttons,
    /// name and size columns (the other details in the tooltip).
    pub fn panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.nav_buttons(ui);
            self.path_field(ui, ui.available_width());
        });
        ui.horizontal_wrapped(|ui| self.action_buttons(ui));
        ui.separator();
        self.listing(ui, true);
        ui.weak(t(
            "더블클릭: 폴더 열기 / 파일 다운로드 · 탐색기에서 파일을 끌어다 놓으면 업로드",
            "Double-click: open a folder / download a file · Drop files from Explorer to upload",
        ));
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.nav_buttons(ui);
            self.path_field(ui, 260.0);
            ui.separator();
            self.action_buttons(ui);
        });
        ui.separator();
        self.listing(ui, false);
    }

    fn nav_buttons(&mut self, ui: &mut egui::Ui) {
        if ui
            .button(t("터미널 위치", "Terminal's folder"))
            .on_hover_text(t(
                "터미널(셸)의 현재 디렉터리로 갑니다 (Linux 서버)",
                "Goes to the current directory of the terminal's shell (Linux servers)",
            ))
            .clicked()
        {
            self.follow_requested = true;
        }
        if ui.button(t("상위", "Up")).clicked() && !self.cwd.is_empty() {
            let parent = remote_parent(&self.cwd);
            self.navigate(&parent);
        }
        if ui.button(t("새로고침", "Refresh")).clicked() {
            self.refresh();
        }
    }

    fn path_field(&mut self, ui: &mut egui::Ui, width: f32) {
        let path = ui.add(egui::TextEdit::singleline(&mut self.path_input).desired_width(width));
        if path.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let target = self.path_input.trim().to_owned();
            self.navigate(&target);
        }
    }

    fn action_buttons(&mut self, ui: &mut egui::Ui) {
        {
            if ui.button(t("새 폴더", "New folder")).clicked() {
                self.prompt = Some(Prompt::NewFolder(String::new()));
            }
            if ui
                .button(t("업로드", "Upload"))
                .on_hover_text(t("파일 선택 (창에 끌어다 놓아도 됩니다)", "Choose files (or drop them on the window)"))
                .clicked()
                && let Some(files) = rfd::FileDialog::new().set_title(t("업로드할 파일", "Files to upload")).pick_files()
            {
                self.upload_paths(files);
            }
            if ui.button(t("폴더 업로드", "Upload folder")).clicked()
                && let Some(dir) = rfd::FileDialog::new().set_title(t("업로드할 폴더", "Folder to upload")).pick_folder()
            {
                self.upload_paths(vec![dir]);
            }
            let has_selection = !self.selected.is_empty();
            if ui.add_enabled(has_selection, egui::Button::new(t("다운로드", "Download"))).clicked() {
                let items = self.selected_entries();
                self.download(items);
            }
            if ui.add_enabled(has_selection, egui::Button::new(t("삭제", "Delete"))).clicked() {
                let items = self.selected_entries().into_iter().map(|e| (e.name, e.is_dir)).collect();
                self.prompt = Some(Prompt::Delete(items));
            }
        }
    }

    /// Entries, transfers and the status line.
    fn listing(&mut self, ui: &mut egui::Ui, compact: bool) {
        let transfers_height = if self.transfers.is_empty() { 0.0 } else { 120.0 };
        let list_height = (ui.available_height() - transfers_height - 30.0).max(80.0);
        egui::ScrollArea::vertical().id_salt("sftp-list").max_height(list_height).auto_shrink([false, false]).show(
            ui,
            |ui| {
                if self.loading && self.entries.is_empty() {
                    ui.spinner();
                }
                self.entry_grid(ui, compact);
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

    /// `compact`: name and size only (side panel); long names are cut short.
    fn entry_grid(&mut self, ui: &mut egui::Ui, compact: bool) {
        let mut enter_dir = None;
        let mut download = None;
        let mut prompt = None;
        let columns = if compact { 2 } else { 4 };
        // Side panel: the name column takes the width the size column leaves.
        let name_width = (ui.available_width() - 110.0).max(80.0);
        egui::Grid::new("sftp-grid").num_columns(columns).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
            ui.strong(t("이름", "Name"));
            ui.strong(t("크기", "Size"));
            if !compact {
                ui.strong(t("수정 시각", "Modified"));
                ui.strong(t("권한", "Permissions"));
            }
            ui.end_row();
            for entry in &self.entries {
                let mut text = RichText::new(&entry.name);
                if entry.is_dir {
                    text = text.strong();
                }
                if entry.name.starts_with('.') {
                    // Hidden files, as `ls` leaves them out.
                    text = text.weak();
                }
                let selected = self.selected.contains(&entry.name);
                let response = ui
                    .push_id(&entry.name, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if compact {
                                ui.set_min_width(name_width);
                                ui.set_max_width(name_width);
                            }
                            file_icons::icon(ui, file_icons::kind(entry), entry.is_symlink);
                            if compact {
                                let details = format!(
                                    "{}\n{}",
                                    entry.mtime.map(format_time).unwrap_or_default(),
                                    entry.permissions.map(|m| mode_string(m, entry.is_dir)).unwrap_or_default()
                                );
                                ui.add(egui::Button::selectable(selected, text).truncate())
                                    .on_hover_text(format!("{}\n{}", entry.name, details.trim()))
                            } else {
                                ui.selectable_label(selected, text)
                            }
                        })
                        .inner
                    })
                    .inner;
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
                    if ui.button(t("다운로드", "Download")).clicked() {
                        download = Some(vec![entry.clone()]);
                        ui.close();
                    }
                    if ui.button(t("이름 바꾸기", "Rename")).clicked() {
                        prompt = Some(Prompt::Rename { from: entry.name.clone(), to: entry.name.clone() });
                        ui.close();
                    }
                    if ui.button(t("삭제", "Delete")).clicked() {
                        prompt = Some(Prompt::Delete(vec![(entry.name.clone(), entry.is_dir)]));
                        ui.close();
                    }
                });
                ui.label(if entry.is_dir { String::new() } else { human_size(entry.size) });
                if !compact {
                    ui.label(entry.mtime.map(format_time).unwrap_or_default());
                    ui.monospace(entry.permissions.map(|m| mode_string(m, entry.is_dir)).unwrap_or_default());
                }
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
            ui.strong(t("전송", "Transfers"));
            if ui.small_button(t("완료 항목 지우기", "Clear finished")).clicked() {
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
                            let width = (ui.available_width() - 50.0).clamp(60.0, 220.0);
                            ui.add(egui::ProgressBar::new(frac).text(text).desired_width(width));
                            // `t` is the transfer here, hence the full path.
                            if ui.small_button(playterm_i18n::t("취소", "Cancel")).clicked() {
                                self.conn.cancel(t.id);
                            }
                        }
                        TransferState::Finished => {
                            ui.label(tr!("완료 ({})", "Done ({})", human_size(t.done)));
                            // `t` is the transfer here, hence the full path.
                            if let Some(path) = &t.local
                                && ui
                                    .small_button(playterm_i18n::t("폴더 열기", "Open folder"))
                                    .on_hover_text(tr!(
                                        "탐색기에서 보기: {}",
                                        "Show in Explorer: {}",
                                        path.display()
                                    ))
                                    .clicked()
                            {
                                crate::logging::reveal(path);
                            }
                        }
                        TransferState::Cancelled => {
                            ui.weak(playterm_i18n::t("취소됨", "Cancelled"));
                        }
                        TransferState::Failed(e) => {
                            ui.colored_label(ui.visuals().error_fg_color, tr!("실패: {e}", "Failed: {e}"));
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
                    ui.heading(t("새 폴더", "New folder"));
                    let r = ui.text_edit_singleline(name);
                    // Check Enter before re-focusing: Enter makes the field give up focus.
                    if enter(ui, &r) { done = Some(true) } else { r.request_focus() }
                }
                Prompt::Rename { from, to } => {
                    ui.heading(tr!("'{from}' 이름 바꾸기", "Rename '{from}'"));
                    let r = ui.text_edit_singleline(to);
                    if enter(ui, &r) { done = Some(true) } else { r.request_focus() }
                }
                Prompt::Delete(items) => {
                    ui.heading(t("삭제", "Delete"));
                    let names: Vec<&str> = items.iter().map(|(n, _)| n.as_str()).collect();
                    ui.label(tr!("{} 을(를) 삭제할까요?", "Delete {}?", names.join(", ")));
                    if items.iter().any(|(_, dir)| *dir) {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            t("폴더는 안의 내용까지 모두 삭제됩니다.", "Folders are deleted with everything in them."),
                        );
                    }
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(t("확인", "OK")).clicked() {
                    done = Some(true);
                }
                if ui.button(t("취소", "Cancel")).clicked() {
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
