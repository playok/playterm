//! "사이트 관리" window: the saved-site tree on the left, the selected site's settings on the
//! right. Saving, deleting and connecting are carried out by the app. It is a window of its own,
//! not a modal: the terminals stay usable beside it (e.g. to read a connection error while
//! changing the port).

use eframe::egui::{self, Button, Key, Modifiers, RichText};
use playterm_config::{ProfileStore, SessionProfile, SiteView, folder_name, normalize_folder, parent_folder};
use playterm_i18n::{t, tr};
use zeroize::Zeroizing;

use crate::icon;
use crate::profile_form::{FormOptions, ProfileForm};
use crate::{sidebar, site_tree};

const LIST_WIDTH: f32 = 290.0;
const FORM_WIDTH: f32 = 540.0;
/// The grid needs room for its columns; the form gives some up.
const GRID_LIST_WIDTH: f32 = 460.0;
const GRID_FORM_WIDTH: f32 = 470.0;
const BODY_HEIGHT: f32 = 440.0;

pub enum SiteManagerResult {
    Open,
    Close,
    /// Save a site. With `connect` the site is also connected; with `connect` or `close`
    /// the manager closes.
    Save {
        profile: Box<SessionProfile>,
        password: Option<Zeroizing<String>>,
        connect: bool,
        close: bool,
    },
    /// Connect a saved, unchanged site and close the manager.
    Connect(String),
    /// Duplicate a saved site; the app selects the copy with [`SiteManager::select`].
    Duplicate(String),
    /// Ask to delete a saved site.
    Delete(String),
    CreateFolder(String),
    /// Rename (or move) a folder with everything in it.
    RenameFolder {
        from: String,
        to: String,
    },
    /// Remove a folder; what is in it moves up a level.
    DeleteFolder(String),
    MoveSite {
        id: String,
        folder: String,
    },
}

/// The folder dialogs.
enum FolderPrompt {
    New { parent: String, name: String, error: Option<String> },
    Rename { path: String, parent: String, name: String, error: Option<String> },
    Delete { path: String },
}

impl FolderPrompt {
    fn new_in(parent: String) -> Self {
        FolderPrompt::New { parent, name: String::new(), error: None }
    }

    fn rename(path: &str) -> Self {
        FolderPrompt::Rename {
            path: path.to_owned(),
            parent: parent_folder(path).to_owned(),
            name: folder_name(path).to_owned(),
            error: None,
        }
    }

    fn set_error(&mut self, e: String) {
        if let FolderPrompt::New { error, .. } | FolderPrompt::Rename { error, .. } = self {
            *error = Some(e);
        }
    }

    /// The change asked for, or why it cannot be made.
    fn result(&self, store: &ProfileStore) -> Result<SiteManagerResult, String> {
        match self {
            FolderPrompt::Delete { path } => Ok(SiteManagerResult::DeleteFolder(path.clone())),
            FolderPrompt::New { parent, name, .. } | FolderPrompt::Rename { parent, name, .. } => {
                let name = name.trim();
                if name.is_empty() {
                    return Err(t!("site_manager.enter_a_folder_name").into());
                }
                if name.contains('/') {
                    return Err(t!("site_manager.a_folder_name_cannot_contain_it_separates_levels").into());
                }
                let path = normalize_folder(&format!("{parent}/{name}"));
                if let FolderPrompt::Rename { path: from, .. } = self
                    && from == &path
                {
                    return Ok(SiteManagerResult::Open);
                }
                if store.folders().contains(&path) {
                    return Err(t!("site_manager.a_folder_with_this_name_exists").into());
                }
                Ok(match self {
                    FolderPrompt::Rename { path: from, .. } => SiteManagerResult::RenameFolder { from: from.clone(), to: path },
                    _ => SiteManagerResult::CreateFolder(path),
                })
            }
        }
    }
}

/// Where a site connects, for the grid: `host:port`, or the serial port.
fn target(p: &SessionProfile) -> String {
    match p.protocol {
        playterm_net::Protocol::Serial => format!("{} {}", p.serial.port, p.serial.baud),
        _ if p.host.contains(':') => format!("[{}]:{}", p.host, p.port),
        _ => format!("{}:{}", p.host, p.port),
    }
}

#[derive(Clone, PartialEq)]
enum Selection {
    Site(String),
    /// A site not saved yet, in this folder.
    New(String),
}

/// Where to go once unsaved changes were saved or discarded.
#[derive(Clone)]
enum Next {
    Select(Option<Selection>),
    Close,
}

#[derive(Default)]
pub struct SiteManager {
    filter: String,
    selected: Option<Selection>,
    form: Option<ProfileForm>,
    /// Asking whether to save the changes before going to `Next`.
    unsaved: Option<Next>,
    /// Tree or grid (kept in the settings by the app).
    pub view: SiteView,
    /// The folder last clicked: where a new site or folder goes.
    focus_folder: Option<String>,
    folder_prompt: Option<FolderPrompt>,
    /// Asked for while open (the sidebar, a menu): gone to on the next frame, after asking about
    /// unsaved changes.
    pending: Option<Next>,
    /// Names of the saved macros, for "macro after connecting" (set by the app).
    pub macros: Vec<String>,
}

/// Whether the vault (probably) holds a password for a site.
pub type SecretCheck<'a> = &'a dyn Fn(&SessionProfile) -> bool;

impl SiteManager {
    /// Selects a saved site, discarding any unsaved edits.
    pub fn select(&mut self, profile: &SessionProfile, has_secret: bool) {
        self.selected = Some(Selection::Site(profile.id.clone()));
        self.form = Some(ProfileForm::new(profile.clone(), has_secret));
        self.unsaved = None;
    }

    /// Shows the saved state of the selected site again after the app changed it (moved it,
    /// renamed its folder), unless it is being edited.
    pub fn refresh(&mut self, store: &ProfileStore, has_secret: SecretCheck<'_>) {
        if !self.is_dirty()
            && let Some(id) = self.selected_site().map(str::to_owned)
            && let Some(p) = store.get(&id)
        {
            self.select(p, has_secret(p));
        }
    }

    /// Goes to a saved site, or a new one, once any unsaved changes are dealt with (the manager
    /// was asked for again while open).
    pub fn request(&mut self, site: Option<&str>, new_site: bool) {
        let selection = match site {
            _ if new_site => Some(Selection::New(self.target_folder())),
            Some(id) => Some(Selection::Site(id.to_owned())),
            None => return,
        };
        if self.selected != selection {
            self.pending = Some(Next::Select(selection));
        }
    }

    /// Starts a new, unsaved site.
    pub fn new_site(&mut self) {
        self.set_selection(Some(Selection::New(String::new())), None, &|_| false);
    }

    fn set_selection(&mut self, selection: Option<Selection>, store: Option<&ProfileStore>, has_secret: SecretCheck<'_>) {
        self.form = match &selection {
            None => None,
            Some(Selection::New(folder)) => Some(ProfileForm::new(SessionProfile { folder: folder.clone(), ..Default::default() }, false)),
            Some(Selection::Site(id)) => store.and_then(|s| s.get(id)).map(|p| ProfileForm::new(p.clone(), has_secret(p))),
        };
        self.selected = if self.form.is_some() { selection } else { None };
    }

    fn is_dirty(&self) -> bool {
        self.form.as_ref().is_some_and(|f| f.is_dirty())
    }

    fn selected_site(&self) -> Option<&str> {
        match &self.selected {
            Some(Selection::Site(id)) => Some(id),
            _ => None,
        }
    }

    /// Folder of the selected site, for a new site created next to it.
    fn current_folder(&self) -> String {
        self.form.as_ref().map(|f| f.profile.folder.trim().to_owned()).unwrap_or_default()
    }

    /// Goes to `next`, first asking about unsaved changes.
    fn go(&mut self, next: Next, store: &ProfileStore, has_secret: SecretCheck<'_>) -> SiteManagerResult {
        if self.is_dirty() {
            self.unsaved = Some(next);
            return SiteManagerResult::Open;
        }
        self.apply(next, store, has_secret)
    }

    fn apply(&mut self, next: Next, store: &ProfileStore, has_secret: SecretCheck<'_>) -> SiteManagerResult {
        match next {
            Next::Close => SiteManagerResult::Close,
            Next::Select(selection) => {
                self.set_selection(selection, Some(store), has_secret);
                SiteManagerResult::Open
            }
        }
    }

    /// Keep the draft until the app acknowledges a successful disk transaction.
    fn save(&mut self, connect: bool, close: bool, _has_secret: SecretCheck<'_>) -> Option<SiteManagerResult> {
        let (profile, password) = self.form.as_mut()?.validate()?;
        Some(SiteManagerResult::Save { profile: Box::new(profile), password, connect, close })
    }

    /// A failed save must not navigate away from the still-unsaved form.
    pub fn save_failed(&mut self) {
        self.pending = None;
    }

    /// Shows the window. `keys_free`: no terminal has the keyboard, so Escape is for this window.
    pub fn show(&mut self, ctx: &egui::Context, store: &ProfileStore, has_secret: SecretCheck<'_>, keys_free: bool) -> SiteManagerResult {
        // The selected site was deleted.
        if let Some(id) = self.selected_site()
            && store.get(id).is_none()
        {
            self.selected = None;
            self.form = None;
        }

        let mut next = self.pending.take();
        let mut result = SiteManagerResult::Open;
        let folders = store.folders();
        let mut open = true;
        let window = egui::Window::new(t!("common.site_manager"))
            .id(egui::Id::unique("site-manager"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                // Give the body a fixed size, or the vertical separator and scroll areas would
                // stretch to the whole screen.
                let (list_width, form_width) = match self.view {
                    SiteView::Tree => (LIST_WIDTH, FORM_WIDTH),
                    SiteView::Grid => (GRID_LIST_WIDTH, GRID_FORM_WIDTH),
                };
                let body = egui::vec2(list_width + form_width + 2.0 * ui.spacing().item_spacing.x + 6.0, BODY_HEIGHT);
                ui.allocate_ui_with_layout(body, egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    ui.set_min_size(body);
                    ui.vertical(|ui| {
                        ui.set_width(list_width);
                        self.tree(ui, store, &mut next, &mut result);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(form_width);
                        self.details(ui, &folders, &mut result, has_secret);
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.weak(tr!("site_manager.sites", arg0 = store.profiles().len()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(icon::button(icon::CLOSE, t!("common.close"))).clicked() {
                            next = Some(Next::Close);
                        }
                    });
                });
            });
        if !open {
            next = Some(Next::Close);
        }
        // Escape closes, unless a terminal has the keyboard (Escape is the shell's then) or
        // something of the manager's own is open above it.
        if keys_free
            && window.is_some()
            && self.folder_prompt.is_none()
            && self.unsaved.is_none()
            && !egui::Popup::is_any_open(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            next = Some(Next::Close);
        }
        if let Some(next) = next {
            result = self.go(next, store, has_secret);
        }
        if let Some(r) = self.unsaved_prompt(ctx, store, has_secret) {
            result = r;
        }
        if let Some(r) = self.folder_dialog(ctx, store) {
            result = r;
        }
        result
    }

    fn tree(&mut self, ui: &mut egui::Ui, store: &ProfileStore, next: &mut Option<Next>, result: &mut SiteManagerResult) {
        let site = self.selected_site().map(str::to_owned);
        let dirty = self.is_dirty();
        // One line (#157): duplicate and delete as icons, named in their tooltips.
        ui.horizontal(|ui| {
            if ui.add(icon::button(icon::NEW_SITE, t!("common.new_site"))).clicked() {
                *next = Some(Next::Select(Some(Selection::New(self.target_folder()))));
            }
            if ui.add(icon::button(icon::NEW_FOLDER, t!("common.new_folder"))).clicked() {
                self.folder_prompt = Some(FolderPrompt::new_in(self.target_folder()));
            }
            if icon::named_icon_button(ui, icon::DUPLICATE, t!("common.duplicate"), site.is_some() && !dirty)
                .on_disabled_hover_text(t!("site_manager.select_a_saved_site_save_your_changes_first"))
                .clicked()
                && let Some(id) = &site
            {
                *result = SiteManagerResult::Duplicate(id.clone());
            }
            if icon::named_icon_button(ui, icon::DELETE, t!("common.delete"), self.selected.is_some()).clicked() {
                match &site {
                    Some(id) => *result = SiteManagerResult::Delete(id.clone()),
                    // An unsaved site is simply dropped.
                    None => self.set_selection(None, None, &|_| false),
                }
            }
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, SiteView::Tree, t!("site_manager.tree")).on_hover_text(t!("site_manager.by_folder"));
            ui.selectable_value(&mut self.view, SiteView::Grid, t!("site_manager.grid"))
                .on_hover_text(t!("site_manager.one_row_per_site_type_target_folder"));
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(t!("common.search")).desired_width(f32::INFINITY));
        });
        ui.add_space(2.0);

        let folders = store.folders();
        egui::ScrollArea::both().id_salt("site-tree").auto_shrink([false, false]).show(ui, |ui| {
            if matches!(self.selected, Some(Selection::New(_))) {
                let _ = ui.selectable_label(true, RichText::new(t!("common.new_site")).italics());
            }
            if store.profiles().is_empty() && folders.len() <= 1 {
                ui.weak(t!("site_manager.no_saved_sites_add_one_with_new_site"));
            }
            let tree = site_tree::build(store, &self.filter);
            match self.view {
                SiteView::Tree => self.folder_items(ui, &tree, &folders, next, result),
                SiteView::Grid => self.grid(ui, &tree, &folders, next, result),
            }
            // The empty space below: new site or folder at the top level.
            let rest = ui.allocate_response(ui.available_size().max(egui::vec2(0.0, 24.0)), egui::Sense::click());
            rest.context_menu(|ui| {
                if ui.button(t!("common.new_site")).clicked() {
                    *next = Some(Next::Select(Some(Selection::New(String::new()))));
                    ui.close();
                }
                if ui.button(t!("site_manager.new_folder")).clicked() {
                    self.folder_prompt = Some(FolderPrompt::new_in(String::new()));
                    ui.close();
                }
            });
        });
    }

    /// The folders inside `folder` (as tree rows with their menus), then its sites.
    fn folder_items(
        &mut self,
        ui: &mut egui::Ui,
        folder: &site_tree::Folder,
        folders: &[String],
        next: &mut Option<Next>,
        result: &mut SiteManagerResult,
    ) {
        for sub in &folder.folders {
            let response = site_tree::folder(ui, sub, "site-manager-folder", |ui| self.folder_items(ui, sub, folders, next, result));
            if response.clicked() {
                self.focus_folder = Some(sub.path.clone());
            }
            response.context_menu(|ui| {
                if ui.button(t!("site_manager.new_site_here")).clicked() {
                    *next = Some(Next::Select(Some(Selection::New(sub.path.clone()))));
                    ui.close();
                }
                if ui.button(t!("site_manager.new_folder_inside")).clicked() {
                    self.folder_prompt = Some(FolderPrompt::new_in(sub.path.clone()));
                    ui.close();
                }
                ui.separator();
                if ui.button(t!("site_manager.rename")).clicked() {
                    self.folder_prompt = Some(FolderPrompt::rename(&sub.path));
                    ui.close();
                }
                if ui.button(t!("site_manager.delete")).clicked() {
                    self.folder_prompt = Some(FolderPrompt::Delete { path: sub.path.clone() });
                    ui.close();
                }
            });
        }
        for p in &folder.sites {
            let selected = self.selected_site() == Some(p.id.as_str());
            let response = ui
                .push_id(("site", &p.id), |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        site_tree::row_lead(ui, p.tab_color.map(crate::menu::color32));
                        site_tree::badge(ui, p.protocol);
                        ui.selectable_label(selected, p.display_name())
                    })
                    .inner
                })
                .inner;
            self.site_response(response, p, selected, folders, next, result);
        }
    }

    /// One row per site (sorted by folder, then name): type, name, target, user, folder.
    fn grid(
        &mut self,
        ui: &mut egui::Ui,
        tree: &site_tree::Folder,
        folders: &[String],
        next: &mut Option<Next>,
        result: &mut SiteManagerResult,
    ) {
        fn collect<'a>(folder: &site_tree::Folder<'a>, out: &mut Vec<&'a SessionProfile>) {
            out.extend(folder.sites.iter().copied());
            for sub in &folder.folders {
                collect(sub, out);
            }
        }
        let mut sites = Vec::new();
        collect(tree, &mut sites);
        sites.sort_by_key(|p| (p.folder.to_lowercase(), p.display_name().to_lowercase()));
        egui::Grid::new("site-grid").num_columns(5).striped(true).spacing([10.0, 4.0]).show(ui, |ui| {
            ui.label("");
            ui.strong(t!("common.name"));
            ui.strong(t!("site_manager.target"));
            ui.strong(t!("common.user"));
            ui.strong(t!("common.folder"));
            ui.end_row();
            for p in sites {
                let selected = self.selected_site() == Some(p.id.as_str());
                site_tree::badge(ui, p.protocol);
                let response = ui.push_id(("grid-site", &p.id), |ui| ui.selectable_label(selected, p.display_name())).inner;
                self.site_response(response, p, selected, folders, next, result);
                ui.label(target(p));
                ui.label(&p.username);
                ui.weak(if p.folder.is_empty() { "—" } else { &p.folder });
                ui.end_row();
            }
        });
    }

    /// Selecting (click), connecting (double click) and the menu of a site's row.
    fn site_response(
        &mut self,
        response: egui::Response,
        p: &SessionProfile,
        selected: bool,
        folders: &[String],
        next: &mut Option<Next>,
        result: &mut SiteManagerResult,
    ) {
        let response = response.on_hover_text(tr!("site_manager.double_click_connect", arg0 = sidebar::summary(p)));
        if response.clicked() && !selected {
            *next = Some(Next::Select(Some(Selection::Site(p.id.clone()))));
        }
        let idle = !self.is_dirty() && self.unsaved.is_none();
        // The first click of the double click selected it; connect only if nothing is pending.
        if response.double_clicked() && selected && idle {
            *result = SiteManagerResult::Connect(p.id.clone());
        }
        // Changing other sites is fine; the one being edited must be saved first.
        let free = idle || !selected;
        response.context_menu(|ui| {
            if ui.add_enabled(idle, icon::button(icon::CONNECT, t!("common.connect"))).clicked() {
                *result = SiteManagerResult::Connect(p.id.clone());
                ui.close();
            }
            if ui.add_enabled(idle, icon::button(icon::DUPLICATE, t!("common.duplicate"))).clicked() {
                *result = SiteManagerResult::Duplicate(p.id.clone());
                ui.close();
            }
            ui.add_enabled_ui(free, |ui| {
                ui.menu_button(t!("site_manager.move_to_folder"), |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for folder in folders {
                            let label = if folder.is_empty() { t!("site_manager.top_level").to_owned() } else { folder.clone() };
                            if ui.add_enabled(folder != &p.folder, Button::new(label)).clicked() {
                                *result = SiteManagerResult::MoveSite { id: p.id.clone(), folder: folder.clone() };
                                ui.close();
                            }
                        }
                    });
                });
            });
            ui.separator();
            if ui.add_enabled(free, icon::button(icon::DELETE, t!("common.delete"))).clicked() {
                *result = SiteManagerResult::Delete(p.id.clone());
                ui.close();
            }
        });
    }

    /// Where a new site or folder goes: the folder last clicked, else the selected site's.
    fn target_folder(&self) -> String {
        self.focus_folder.clone().unwrap_or_else(|| self.current_folder())
    }

    /// The new-folder, rename and delete dialogs for folders. Returns the change to make.
    fn folder_dialog(&mut self, ctx: &egui::Context, store: &ProfileStore) -> Option<SiteManagerResult> {
        let prompt = self.folder_prompt.as_mut()?;
        let renaming = matches!(prompt, FolderPrompt::Rename { .. });
        let mut answer = None;
        let modal =
            egui::Modal::new(egui::Id::unique("site-folder-dialog")).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, |ui| {
                ui.set_width(360.0);
                match prompt {
                    FolderPrompt::New { parent, name, error } | FolderPrompt::Rename { parent, name, error, .. } => {
                        ui.heading(if renaming { t!("site_manager.rename_folder") } else { t!("common.new_folder") });
                        if !parent.is_empty() {
                            ui.weak(tr!("site_manager.in", parent = parent));
                        }
                        let field = ui.add(egui::TextEdit::singleline(name).hint_text(t!("site_manager.folder_name")));
                        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if !enter {
                            field.request_focus();
                        }
                        if let Some(e) = error {
                            ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                        }
                        let ok = icon::primary(ui, icon::OK, t!("common.ok"));
                        let (main, cancel) = icon::ok_cancel_row(ui, ok, true, icon::cancel());
                        if main.clicked() || enter {
                            answer = Some(true);
                        }
                        if cancel.clicked() {
                            answer = Some(false);
                        }
                    }
                    FolderPrompt::Delete { path } => {
                        ui.heading(t!("site_manager.delete_folder"));
                        let parent = parent_folder(path);
                        let up = if parent.is_empty() {
                            t!("site_manager.the_top_level").to_owned()
                        } else {
                            tr!("site_manager.message", arg0 = parent)
                        };
                        ui.label(tr!("site_manager.delete_the_folder", arg0 = folder_name(path)));
                        ui.weak(tr!("site_manager.its_sites_and_the_folders_inside_it_are", arg0 = store.sites_in_folder(path), up = up));
                        let delete = icon::danger(ui, icon::DELETE, t!("common.delete"));
                        let (main, cancel) = icon::ok_cancel_row(ui, delete, true, icon::cancel());
                        if main.clicked() {
                            answer = Some(true);
                        }
                        if cancel.clicked() {
                            answer = Some(false);
                        }
                    }
                }
            });
        if answer.is_none() && modal.should_close() {
            answer = Some(false);
        }
        match answer? {
            false => {
                self.folder_prompt = None;
                None
            }
            true => match prompt.result(store) {
                Ok(result) => {
                    self.folder_prompt = None;
                    if let SiteManagerResult::CreateFolder(path) | SiteManagerResult::RenameFolder { to: path, .. } = &result {
                        self.focus_folder = Some(path.clone());
                    }
                    if matches!(result, SiteManagerResult::DeleteFolder(_)) {
                        self.focus_folder = None;
                    }
                    Some(result)
                }
                Err(e) => {
                    prompt.set_error(e);
                    None
                }
            },
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, folders: &[String], result: &mut SiteManagerResult, has_secret: SecretCheck<'_>) {
        let is_new = matches!(self.selected, Some(Selection::New(_)));
        let site = self.selected_site().map(str::to_owned);
        let Some(form) = &mut self.form else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.weak(t!("site_manager.select_a_site_on_the_left_or_click")));
            return;
        };

        let dirty = form.is_dirty();
        ui.horizontal(|ui| {
            let title =
                if is_new && form.profile.name.trim().is_empty() { t!("common.new_site").to_owned() } else { form.profile.display_name() };
            ui.strong(title);
            if is_new {
                ui.weak(t!("site_manager.not_saved"));
            } else if dirty {
                ui.weak(t!("site_manager.modified"));
            }
        });
        ui.separator();

        let mut submit = false;
        egui::ScrollArea::vertical().id_salt("site-form").max_height(BODY_HEIGHT - 70.0).auto_shrink([false, false]).show(ui, |ui| {
            submit = form.ui(ui, &FormOptions { meta: true, save_password: true, folders, macros: &self.macros });
        });
        ui.separator();

        let mut connect = false;
        let mut revert = false;
        ui.horizontal(|ui| {
            connect = ui.add(icon::button(icon::CONNECT, t!("common.connect"))).clicked();
            submit |= ui.add_enabled(dirty || is_new, icon::primary(ui, icon::SAVE, t!("common.save"))).clicked();
            revert = ui.add_enabled(dirty && !is_new, icon::button(icon::REVERT, t!("site_manager.revert"))).clicked();
        });

        if connect {
            match &site {
                Some(id) if !dirty => *result = SiteManagerResult::Connect(id.clone()),
                _ => {
                    if let Some(r) = self.save(true, true, has_secret) {
                        *result = r;
                    }
                }
            }
        } else if submit && (dirty || is_new) {
            if let Some(r) = self.save(false, false, has_secret) {
                *result = r;
            }
        } else if revert && let Some(form) = &self.form {
            let original = form.initial_profile().clone();
            let secret = has_secret(&original);
            self.select(&original, secret);
        }
    }

    /// "Save changes?" asked before leaving an edited site. Returns a result once answered.
    fn unsaved_prompt(&mut self, ctx: &egui::Context, store: &ProfileStore, has_secret: SecretCheck<'_>) -> Option<SiteManagerResult> {
        let next = self.unsaved.clone()?;
        let name = match (&self.selected, &self.form) {
            (Some(Selection::New(_)), Some(f)) if f.profile.name.trim().is_empty() => t!("common.new_site").to_owned(),
            (_, Some(f)) => f.profile.display_name(),
            _ => String::new(),
        };
        #[derive(Clone, Copy)]
        enum Answer {
            Save,
            Discard,
            Cancel,
        }
        let mut answer = None;
        let modal =
            egui::Modal::new(egui::Id::unique("site-manager-unsaved")).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading(t!("site_manager.unsaved_changes"));
                ui.label(tr!("site_manager.save_the_changes_to", name = name));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(icon::primary(ui, icon::SAVE, t!("common.save"))).clicked() {
                        answer = Some(Answer::Save);
                    }
                    if ui.add(icon::button(icon::DONT_SAVE, t!("site_manager.don_t_save"))).clicked() {
                        answer = Some(Answer::Discard);
                    }
                    if ui.add(icon::button(icon::CANCEL, t!("common.cancel"))).clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
            });
        if answer.is_none() && modal.should_close() {
            answer = Some(Answer::Cancel);
        }
        let answer = answer?;
        self.unsaved = None;
        match answer {
            Answer::Cancel => Some(SiteManagerResult::Open),
            Answer::Discard => Some(self.apply(next, store, has_secret)),
            // Invalid input: stay here; the form shows what is wrong.
            Answer::Save => {
                let close = matches!(next, Next::Close);
                let saved = self.save(false, close, has_secret)?;
                if let Next::Select(selection) = next {
                    self.pending = Some(Next::Select(selection));
                }
                Some(saved)
            }
        }
    }
}
