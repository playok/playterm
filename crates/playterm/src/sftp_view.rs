//! SFTP file browser of one SSH session, shown in the SFTP explorer side panel.

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
    /// The user asked to go to the terminal's current directory (see `Session::poll`).
    pub follow_requested: bool,
    /// Canonical current directory; empty until the first listing arrives.
    cwd: String,
    path_input: String,
    entries: Vec<RemoteEntry>,
    selected: BTreeSet<String>,
    /// The entry last clicked: where a Shift+click range starts.
    anchor: Option<String>,
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
            follow_requested: false,
            cwd: String::new(),
            path_input: String::new(),
            entries: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
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
                                self.anchor = None;
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

    /// A click on entry `name` with these modifiers: alone, Ctrl toggles it, Shift selects the
    /// range from the last clicked entry (Ctrl+Shift adds the range).
    fn click(&mut self, name: &str, ctrl: bool, shift: bool) {
        let order: Vec<&str> = self.entries.iter().map(|e| e.name.as_str()).collect();
        select(&mut self.selected, &mut self.anchor, &order, name, ctrl, shift);
    }

    fn select_all(&mut self) {
        self.selected = self.entries.iter().map(|e| e.name.clone()).collect();
    }

    /// Asks before deleting the selected entries.
    fn delete_selected(&mut self) {
        let items: Vec<(String, bool)> = self.selected_entries().into_iter().map(|e| (e.name, e.is_dir)).collect();
        if !items.is_empty() {
            self.prompt = Some(Prompt::Delete(items));
        }
    }

    fn selected_entries(&self) -> Vec<RemoteEntry> {
        self.entries.iter().filter(|e| self.selected.contains(&e.name)).cloned().collect()
    }

    /// The browser's dialogs (new folder, rename, delete), opened from the side panel.
    pub fn show(&mut self, ctx: &egui::Context, id: u64) {
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
        self.listing(ui);
        ui.weak(t(
            "더블클릭: 폴더 열기 / 파일 다운로드 · Ctrl·Shift+클릭, Ctrl+A: 여러 개 선택 · 탐색기에서 파일을 끌어다 놓으면 업로드",
            "Double-click: open a folder / download a file · Ctrl/Shift+click, Ctrl+A: select several · Drop files from Explorer to upload",
        ));
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
            let selected_count = self.selected.len();
            let count = |label: &str| match selected_count {
                0 | 1 => label.to_owned(),
                n => format!("{label} ({n})"),
            };
            if ui.add_enabled(has_selection, egui::Button::new(count(t("다운로드", "Download")))).clicked() {
                let items = self.selected_entries();
                self.download(items);
            }
            if ui.add_enabled(has_selection, egui::Button::new(count(t("삭제", "Delete")))).clicked() {
                self.delete_selected();
            }
        }
    }

    /// Entries, transfers and the status line.
    fn listing(&mut self, ui: &mut egui::Ui) {
        let transfers_height = if self.transfers.is_empty() { 0.0 } else { 120.0 };
        let list_height = (ui.available_height() - transfers_height - 30.0).max(80.0);
        let list = egui::ScrollArea::vertical().id_salt("sftp-list").max_height(list_height).auto_shrink([false, false]).show(
            ui,
            |ui| {
                if self.loading && self.entries.is_empty() {
                    ui.spinner();
                }
                self.entry_grid(ui)
            },
        );
        // A click in the list gives it the keyboard (away from the terminal): Ctrl+A selects
        // all, Delete deletes the selection, Escape clears it.
        let focus = ui.interact(list.inner_rect, ui.scope_id().with("sftp-list-keys"), egui::Sense::focusable_noninteractive());
        if list.inner {
            focus.request_focus();
        }
        if focus.has_focus() && self.prompt.is_none() {
            let (all, delete, clear) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if all {
                self.select_all();
            }
            if delete {
                self.delete_selected();
            }
            if clear {
                self.selected.clear();
            }
        }

        if !self.transfers.is_empty() {
            ui.separator();
            self.transfer_list(ui);
        }
        if let Some((text, error)) = &self.status {
            let color = if *error { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
            ui.colored_label(color, text);
        }
    }

    /// Name and size columns, sized for the side panel (long names are cut short; the
    /// modification time and permissions are in the tooltip). Returns whether an entry was
    /// clicked.
    fn entry_grid(&mut self, ui: &mut egui::Ui) -> bool {
        let mut enter_dir = None;
        let mut download = None;
        let mut prompt = None;
        let mut clicked = None;
        let mut select_all = None;
        // The name column takes the width the size column leaves.
        let name_width = (ui.available_width() - 110.0).max(80.0);
        egui::Grid::new("sftp-grid").num_columns(2).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
            ui.horizontal(|ui| {
                // Selects or clears them all.
                let total = self.entries.len();
                let mut all = total > 0 && self.selected.len() == total;
                let partly = !all && !self.selected.is_empty();
                let check = egui::Checkbox::without_text(&mut all).indeterminate(partly);
                if ui.add_enabled(total > 0, check).on_hover_text(t("모두 선택 (Ctrl+A)", "Select all (Ctrl+A)")).changed() {
                    select_all = Some(all);
                }
                ui.strong(t("이름", "Name"));
            });
            ui.strong(t("크기", "Size"));
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
                            ui.set_min_width(name_width);
                            ui.set_max_width(name_width);
                            file_icons::icon(ui, file_icons::kind(entry), entry.is_symlink);
                            let details = format!(
                                "{}\n{}",
                                entry.mtime.map(format_time).unwrap_or_default(),
                                entry.permissions.map(|m| mode_string(m, entry.is_dir)).unwrap_or_default()
                            );
                            ui.add(egui::Button::selectable(selected, text).truncate())
                                .on_hover_text(format!("{}\n{}", entry.name, details.trim()))
                        })
                        .inner
                    })
                    .inner;
                if response.clicked() {
                    let m = ui.input(|i| i.modifiers);
                    clicked = Some((entry.name.clone(), m.command, m.shift));
                }
                // A right click on an entry outside the selection is about that entry alone.
                if response.secondary_clicked() && !selected {
                    clicked = Some((entry.name.clone(), false, false));
                }
                let many = selected && self.selected.len() > 1;
                if response.double_clicked() {
                    if entry.is_dir { enter_dir = Some(entry.name.clone()) } else { download = Some(vec![entry.clone()]) }
                }
                response.context_menu(|ui| {
                    if many {
                        // The whole selection.
                        let n = self.selected.len();
                        let chosen: Vec<RemoteEntry> =
                            self.entries.iter().filter(|e| self.selected.contains(&e.name)).cloned().collect();
                        if ui.button(tr!("선택한 {n}개 다운로드", "Download {n} selected")).clicked() {
                            download = Some(chosen.clone());
                            ui.close();
                        }
                        if ui.button(tr!("선택한 {n}개 삭제", "Delete {n} selected")).clicked() {
                            prompt = Some(Prompt::Delete(chosen.into_iter().map(|e| (e.name, e.is_dir)).collect()));
                            ui.close();
                        }
                        return;
                    }
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
                ui.end_row();
            }
        });
        if let Some(name) = enter_dir {
            let path = remote_join(&self.cwd, &name);
            self.navigate(&path);
        }
        if let Some((name, ctrl, shift)) = &clicked {
            self.click(name, *ctrl, *shift);
        }
        match select_all {
            Some(true) => self.select_all(),
            Some(false) => self.selected.clear(),
            None => {}
        }
        if let Some(items) = download {
            self.download(items);
        }
        if prompt.is_some() {
            self.prompt = prompt;
        }
        clicked.is_some()
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
                    const SHOWN: usize = 8;
                    let mut names: Vec<&str> = items.iter().take(SHOWN).map(|(n, _)| n.as_str()).collect();
                    let more = items.len().saturating_sub(SHOWN);
                    let more_text = tr!("외 {more}개", "and {more} more");
                    if more > 0 {
                        names.push(&more_text);
                    }
                    if items.len() > 1 {
                        ui.label(tr!("{}개를 삭제할까요?", "Delete {} items?", items.len()));
                        ui.weak(names.join(", "));
                    } else {
                        ui.label(tr!("{} 을(를) 삭제할까요?", "Delete {}?", names.join(", ")));
                    }
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

/// Applies a click on `name` to the selection (`order`: the entries as listed). Plain: just
/// it. Ctrl: toggled. Shift: the range from `anchor` (Ctrl+Shift: added to the selection).
fn select(selected: &mut BTreeSet<String>, anchor: &mut Option<String>, order: &[&str], name: &str, ctrl: bool, shift: bool) {
    if shift {
        let start = anchor.as_deref().and_then(|a| order.iter().position(|n| *n == a));
        let end = order.iter().position(|n| *n == name);
        if let (Some(a), Some(b)) = (start, end) {
            if !ctrl {
                selected.clear();
            }
            selected.extend(order[a.min(b)..=a.max(b)].iter().map(|n| n.to_string()));
            return;
        }
    }
    if ctrl {
        if !selected.remove(name) {
            selected.insert(name.to_owned());
        }
    } else {
        *selected = BTreeSet::from([name.to_owned()]);
    }
    *anchor = Some(name.to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selecting_with_ctrl_and_shift() {
        let order = ["d1", "a.txt", "b.txt", "c.txt", "e.log"];
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        let names = |s: &BTreeSet<String>| order.iter().filter(|n| s.contains(**n)).copied().collect::<Vec<_>>();
        select(&mut sel, &mut anchor, &order, "a.txt", false, false);
        select(&mut sel, &mut anchor, &order, "c.txt", false, true);
        assert_eq!(names(&sel), ["a.txt", "b.txt", "c.txt"], "Shift: the range");
        select(&mut sel, &mut anchor, &order, "b.txt", true, false);
        assert_eq!(names(&sel), ["a.txt", "c.txt"], "Ctrl: toggled off");
        select(&mut sel, &mut anchor, &order, "e.log", true, true);
        assert_eq!(names(&sel), ["a.txt", "b.txt", "c.txt", "e.log"], "Ctrl+Shift: range added from b.txt");
        select(&mut sel, &mut anchor, &order, "d1", false, true);
        assert_eq!(names(&sel), ["d1", "a.txt", "b.txt"], "Shift again: a new range from the same anchor (b.txt)");
        select(&mut sel, &mut anchor, &order, "c.txt", false, false);
        assert_eq!(names(&sel), ["c.txt"], "a plain click selects one");
        // Shift with no anchor in the list (it was refreshed away): just that one.
        anchor = Some("gone".into());
        select(&mut sel, &mut anchor, &order, "a.txt", false, true);
        assert_eq!(names(&sel), ["a.txt"]);
    }

    #[test]
    fn formatting() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(mode_string(0o755, true), "drwxr-xr-x");
        assert_eq!(mode_string(0o100644, false), "-rw-r--r--");
    }
}
