//! Creating and editing a template: a name, the sites it opens (in order) and their layout.

use eframe::egui::{self, RichText};
use playterm_config::{ProfileStore, SessionProfile, SiteTemplate, TemplateLayout};
use playterm_i18n::{t, tr};

use crate::icon;
use crate::sidebar;

pub enum TemplateResult {
    Open,
    Cancel,
    Save(SiteTemplate),
}

pub struct TemplateDialog {
    template: SiteTemplate,
    is_new: bool,
    error: Option<String>,
}

impl TemplateDialog {
    /// A new template with these sites.
    pub fn new(sites: Vec<String>) -> Self {
        Self { template: SiteTemplate { sites, ..Default::default() }, is_new: true, error: None }
    }

    pub fn edit(template: SiteTemplate) -> Self {
        Self { template, is_new: false, error: None }
    }

    pub fn show(&mut self, ctx: &egui::Context, store: &ProfileStore) -> TemplateResult {
        let mut result = TemplateResult::Open;
        let close = egui::Modal::new(egui::Id::unique("template-dialog"))
            .backdrop_color(crate::app::theme::MODAL_BACKDROP)
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading(if self.is_new { t("템플릿 만들기", "New template") } else { t("템플릿 편집", "Edit template") });
                ui.add_space(8.0);
                let mut submit = false;
                egui::Grid::new("template-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(t("이름", "Name"));
                    let name = ui.add(
                        egui::TextEdit::singleline(&mut self.template.name)
                            .hint_text(t("예: 운영 서버", "e.g. Production"))
                            .desired_width(f32::INFINITY),
                    );
                    if self.is_new && self.template.name.is_empty() {
                        name.request_focus();
                    }
                    submit |= name.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.end_row();
                    ui.label(t("배치", "Layout"));
                    ui.horizontal_wrapped(|ui| {
                        for layout in TemplateLayout::ALL {
                            ui.radio_value(&mut self.template.layout, layout, layout.label());
                        }
                    });
                    ui.end_row();
                });
                ui.add_space(8.0);
                self.sites_ui(ui, store);
                ui.add_space(8.0);
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                let save = icon::primary(ui, icon::SAVE, t("저장", "Save"));
                let (main, cancel) = icon::ok_cancel_row(ui, save, true, icon::cancel());
                submit |= main.clicked();
                if cancel.clicked() {
                    result = TemplateResult::Cancel;
                }
                if submit {
                    self.template.name = self.template.name.trim().to_owned();
                    self.template.sites.retain(|id| store.get(id).is_some());
                    if self.template.name.is_empty() {
                        self.error = Some(t("이름을 입력하세요", "Enter a name").into());
                    } else if self.template.sites.is_empty() {
                        self.error = Some(t("사이트를 하나 이상 넣으세요", "Add at least one site").into());
                    } else {
                        result = TemplateResult::Save(self.template.clone());
                    }
                }
            })
            .should_close();
        if close && matches!(result, TemplateResult::Open) {
            result = TemplateResult::Cancel;
        }
        result
    }

    /// The sites in opening order, with buttons to reorder and remove them, and a menu to add more.
    fn sites_ui(&mut self, ui: &mut egui::Ui, store: &ProfileStore) {
        let sites = &mut self.template.sites;
        sites.retain(|id| store.get(id).is_some());
        ui.label(RichText::new(tr!("사이트 ({}개, 여는 순서)", "Sites ({}, in opening order)", sites.len())).strong());
        let mut swap = None;
        let mut remove = None;
        egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
            for (i, id) in sites.iter().enumerate() {
                let Some(p) = store.get(id) else { continue };
                ui.horizontal(|ui| {
                    ui.label(format!("{}.", i + 1));
                    ui.label(p.display_name()).on_hover_text(sidebar::summary(p));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(icon::icon_button(icon::CLOSE).small()).on_hover_text(t("빼기", "Remove")).clicked() {
                            remove = Some(i);
                        }
                        if ui.add_enabled(i + 1 < sites.len(), icon::icon_button(icon::DOWN).small()).clicked() {
                            swap = Some((i, i + 1));
                        }
                        if ui.add_enabled(i > 0, icon::icon_button(icon::UP).small()).clicked() {
                            swap = Some((i - 1, i));
                        }
                    });
                });
            }
        });
        if let Some((a, b)) = swap {
            sites.swap(a, b);
        }
        if let Some(i) = remove {
            sites.remove(i);
        }
        let others: Vec<&SessionProfile> = store.profiles().iter().filter(|p| !sites.contains(&p.id)).collect();
        ui.add_enabled_ui(!others.is_empty(), |ui| {
            ui.menu_button(t("+ 사이트 추가", "+ Add site"), |ui| {
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for (folder, items) in sidebar::grouped(store, "") {
                        let items: Vec<&&SessionProfile> = items.iter().filter(|p| !sites.contains(&p.id)).collect();
                        if items.is_empty() {
                            continue;
                        }
                        if !folder.is_empty() {
                            ui.label(RichText::new(&folder).weak());
                        }
                        for p in items {
                            if ui.button(p.display_name()).clicked() {
                                sites.push(p.id.clone());
                            }
                        }
                    }
                });
            });
        });
    }
}
