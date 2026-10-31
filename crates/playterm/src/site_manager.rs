//! "사이트 관리" window: the saved-site tree on the left, the selected site's settings on the
//! right. Saving, deleting and connecting are carried out by the app.

use eframe::egui::{self, Button, Key, Modifiers, RichText};
use playterm_config::{ProfileStore, SessionProfile};
use zeroize::Zeroizing;

use crate::profile_form::{FormOptions, ProfileForm};
use crate::sidebar;

const LIST_WIDTH: f32 = 240.0;
const FORM_WIDTH: f32 = 540.0;
const BODY_HEIGHT: f32 = 440.0;

pub enum SiteManagerResult {
    Open,
    Close,
    /// Save a site. With `connect` the site is also connected; with `connect` or `close`
    /// the manager closes.
    Save { profile: Box<SessionProfile>, password: Option<Zeroizing<String>>, connect: bool, close: bool },
    /// Connect a saved, unchanged site and close the manager.
    Connect(String),
    /// Duplicate a saved site; the app selects the copy with [`SiteManager::select`].
    Duplicate(String),
    /// Ask to delete a saved site.
    Delete(String),
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

    /// Starts a new, unsaved site.
    pub fn new_site(&mut self) {
        self.set_selection(Some(Selection::New(String::new())), None, &|_| false);
    }

    fn set_selection(&mut self, selection: Option<Selection>, store: Option<&ProfileStore>, has_secret: SecretCheck<'_>) {
        self.form = match &selection {
            None => None,
            Some(Selection::New(folder)) => {
                Some(ProfileForm::new(SessionProfile { folder: folder.clone(), ..Default::default() }, false))
            }
            Some(Selection::Site(id)) => {
                store.and_then(|s| s.get(id)).map(|p| ProfileForm::new(p.clone(), has_secret(p)))
            }
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

    /// Validates the form. On success the form is reset to the saved state and the
    /// returned result asks the app to store it.
    fn save(&mut self, connect: bool, close: bool, has_secret: SecretCheck<'_>) -> Option<SiteManagerResult> {
        let (profile, password) = self.form.as_mut()?.validate()?;
        let secret = profile.save_password && (password.is_some() || has_secret(&profile));
        self.select(&profile, secret);
        Some(SiteManagerResult::Save { profile: Box::new(profile), password, connect, close })
    }

    pub fn show(&mut self, ctx: &egui::Context, store: &ProfileStore, has_secret: SecretCheck<'_>) -> SiteManagerResult {
        // The selected site was deleted.
        if let Some(id) = self.selected_site()
            && store.get(id).is_none()
        {
            self.selected = None;
            self.form = None;
        }

        let mut next = None;
        let mut result = SiteManagerResult::Open;
        let folders = store.folders();
        let modal = egui::Modal::new(egui::Id::unique("site-manager")).show(ctx, |ui| {
            ui.heading("사이트 관리");
            ui.add_space(6.0);
            // A modal has no natural height limit: give the body a fixed size, or the vertical
            // separator and scroll areas would stretch to the whole screen.
            let body = egui::vec2(LIST_WIDTH + FORM_WIDTH + 2.0 * ui.spacing().item_spacing.x + 6.0, BODY_HEIGHT);
            ui.allocate_ui_with_layout(body, egui::Layout::left_to_right(egui::Align::Min), |ui| {
                ui.set_min_size(body);
                ui.vertical(|ui| {
                    ui.set_width(LIST_WIDTH);
                    self.tree(ui, store, &mut next, &mut result);
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(FORM_WIDTH);
                    self.details(ui, &folders, &mut result, has_secret);
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.weak(format!("사이트 {}개", store.profiles().len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("닫기").clicked() {
                        next = Some(Next::Close);
                    }
                });
            });
        });
        // Escape closes; a click outside does not (too easy to lose edits).
        if modal.is_top_modal
            && !modal.any_popup_open
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
        result
    }

    fn tree(
        &mut self,
        ui: &mut egui::Ui,
        store: &ProfileStore,
        next: &mut Option<Next>,
        result: &mut SiteManagerResult,
    ) {
        ui.horizontal(|ui| {
            if ui.button("새 사이트").clicked() {
                *next = Some(Next::Select(Some(Selection::New(self.current_folder()))));
            }
            let site = self.selected_site().map(str::to_owned);
            let dirty = self.is_dirty();
            if ui
                .add_enabled(site.is_some() && !dirty, Button::new("복제"))
                .on_disabled_hover_text("저장된 사이트를 선택하세요 (변경 사항은 먼저 저장)")
                .clicked()
                && let Some(id) = &site
            {
                *result = SiteManagerResult::Duplicate(id.clone());
            }
            if ui.add_enabled(self.selected.is_some(), Button::new("삭제")).clicked() {
                match &site {
                    Some(id) => *result = SiteManagerResult::Delete(id.clone()),
                    // An unsaved site is simply dropped.
                    None => self.set_selection(None, None, &|_| false),
                }
            }
        });
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("검색").desired_width(f32::INFINITY));
        ui.add_space(2.0);

        egui::ScrollArea::vertical().id_salt("site-tree").auto_shrink([false, false]).show(ui, |ui| {
            if matches!(self.selected, Some(Selection::New(_))) {
                let _ = ui.selectable_label(true, RichText::new("새 사이트").italics());
            }
            if store.profiles().is_empty() {
                ui.weak("저장된 사이트가 없습니다.\n[새 사이트] 로 추가하세요.");
            }
            for (folder, items) in sidebar::grouped(store, &self.filter) {
                if folder.is_empty() {
                    self.items(ui, &items, next, result);
                } else {
                    egui::CollapsingHeader::new(RichText::new(&folder).strong())
                        .id_salt(("site-folder", &folder))
                        .default_open(true)
                        .show(ui, |ui| self.items(ui, &items, next, result));
                }
            }
        });
    }

    fn items(
        &self,
        ui: &mut egui::Ui,
        items: &[&SessionProfile],
        next: &mut Option<Next>,
        result: &mut SiteManagerResult,
    ) {
        for p in items {
            let selected = self.selected_site() == Some(p.id.as_str());
            let response = ui
                .push_id(("site", &p.id), |ui| ui.selectable_label(selected, p.display_name()))
                .inner
                .on_hover_text(format!("{}  (더블클릭: 연결)", sidebar::summary(p)));
            if response.clicked() && !selected {
                *next = Some(Next::Select(Some(Selection::Site(p.id.clone()))));
            }
            // The first click of the double click selected it; connect only if nothing is pending.
            if response.double_clicked() && selected && !self.is_dirty() && self.unsaved.is_none() {
                *result = SiteManagerResult::Connect(p.id.clone());
            }
        }
    }

    fn details(
        &mut self,
        ui: &mut egui::Ui,
        folders: &[String],
        result: &mut SiteManagerResult,
        has_secret: SecretCheck<'_>,
    ) {
        let is_new = matches!(self.selected, Some(Selection::New(_)));
        let site = self.selected_site().map(str::to_owned);
        let Some(form) = &mut self.form else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.weak("왼쪽에서 사이트를 선택하거나 [새 사이트] 를 누르세요."));
            return;
        };

        let dirty = form.is_dirty();
        ui.horizontal(|ui| {
            let title = if is_new && form.profile.name.trim().is_empty() {
                "새 사이트".to_owned()
            } else {
                form.profile.display_name()
            };
            ui.strong(title);
            if is_new {
                ui.weak("(저장 안 됨)");
            } else if dirty {
                ui.weak("(수정됨)");
            }
        });
        ui.separator();

        let mut submit = false;
        egui::ScrollArea::vertical()
            .id_salt("site-form")
            .max_height(BODY_HEIGHT - 70.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                submit = form.ui(ui, &FormOptions { meta: true, save_password: true, folders });
            });
        ui.separator();

        let mut connect = false;
        let mut revert = false;
        ui.horizontal(|ui| {
            connect = ui.button("연결").clicked();
            submit |= ui.add_enabled(dirty || is_new, Button::new("저장")).clicked();
            revert = ui.add_enabled(dirty && !is_new, Button::new("되돌리기")).clicked();
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
    fn unsaved_prompt(
        &mut self,
        ctx: &egui::Context,
        store: &ProfileStore,
        has_secret: SecretCheck<'_>,
    ) -> Option<SiteManagerResult> {
        let next = self.unsaved.clone()?;
        let name = match (&self.selected, &self.form) {
            (Some(Selection::New(_)), Some(f)) if f.profile.name.trim().is_empty() => "새 사이트".to_owned(),
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
        let modal = egui::Modal::new(egui::Id::unique("site-manager-unsaved")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.heading("저장하지 않은 변경 사항");
            ui.label(format!("'{name}' 의 변경 사항을 저장할까요?"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("저장").clicked() {
                    answer = Some(Answer::Save);
                }
                if ui.button("저장 안 함").clicked() {
                    answer = Some(Answer::Discard);
                }
                if ui.button("취소").clicked() {
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
                    self.set_selection(selection, Some(store), has_secret);
                }
                Some(saved)
            }
        }
    }
}
