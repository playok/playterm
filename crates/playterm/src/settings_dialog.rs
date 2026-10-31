//! "설정" window: terminal font, size, color scheme and scrollback. Changes are previewed in
//! the open tabs right away; Cancel puts the old settings back.

use eframe::egui::{self, Color32, FontId, RichText, Sense, Vec2};
use playterm_config::AppSettings;
use playterm_term::{THEMES, Theme};

use crate::fonts::{self, FontChoice};

pub enum SettingsResult {
    Open,
    /// Preview these settings (not saved yet).
    Changed(AppSettings),
    /// Apply and save.
    Save(AppSettings),
    /// Put these (the settings from before the dialog) back.
    Cancel(AppSettings),
}

pub struct SettingsDialog {
    original: AppSettings,
    draft: AppSettings,
    /// What the app was last told to show.
    shown: AppSettings,
    fonts: &'static [FontChoice],
    /// Narrows the font list.
    font_filter: String,
}

fn color(hex: u32) -> Color32 {
    let c = Theme::rgb(hex);
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Display name of a font setting: the family's (Korean) name, or a font file's name.
fn font_label(fonts: &[FontChoice], font: &str) -> String {
    if fonts::is_file(font) {
        let name = std::path::Path::new(font).file_name().map(|n| n.to_string_lossy().into_owned());
        format!("파일: {}", name.unwrap_or_else(|| font.to_owned()))
    } else {
        fonts.iter().find(|f| f.matches(font)).map_or_else(|| font.to_owned(), |f| f.name.clone())
    }
}

/// A strip of the scheme's background and its 8 normal colors.
fn swatch(ui: &mut egui::Ui, theme: &Theme) {
    let size = Vec2::new(10.0 * 9.0, 14.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, color(theme.background));
    for (i, hex) in theme.ansi[1..8].iter().chain([&theme.foreground]).enumerate() {
        let min = rect.min + Vec2::new(10.0 * (i + 1) as f32, 3.0);
        painter.rect_filled(egui::Rect::from_min_size(min, Vec2::new(8.0, 8.0)), 1.0, color(*hex));
    }
}

impl SettingsDialog {
    pub fn new(current: &AppSettings) -> Self {
        Self {
            original: current.clone(),
            draft: current.clone(),
            shown: current.clone(),
            fonts: fonts::available(),
            font_filter: String::new(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> SettingsResult {
        let mut result = None;
        let modal = egui::Modal::new(egui::Id::unique("settings")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("설정");
            ui.add_space(8.0);
            egui::Grid::new("settings-grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                self.font_row(ui);
                self.size_row(ui);
                self.theme_row(ui);

                ui.label("스크롤백");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.draft.scrollback)
                            .range(AppSettings::MIN_SCROLLBACK..=AppSettings::MAX_SCROLLBACK)
                            .speed(100)
                            .suffix(" 줄"),
                    );
                    ui.weak("탭마다 보관하는 지난 출력");
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            self.preview(ui);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("확인").clicked() {
                    result = Some(SettingsResult::Save(self.draft.clone()));
                }
                if ui.button("취소").clicked() {
                    result = Some(SettingsResult::Cancel(self.original.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("기본값으로").clicked() {
                        self.draft = AppSettings::default();
                    }
                });
            });
        });
        if result.is_none() && modal.should_close() {
            result = Some(SettingsResult::Cancel(self.original.clone()));
        }
        if let Some(r) = result {
            return r;
        }
        if self.draft != self.shown {
            self.shown = self.draft.clone();
            return SettingsResult::Changed(self.draft.clone());
        }
        SettingsResult::Open
    }

    fn font_row(&mut self, ui: &mut egui::Ui) {
        ui.label("글꼴");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("font")
                .width(200.0)
                .height(420.0)
                .selected_text(font_label(self.fonts, &self.draft.font))
                // Stay open while typing in the search field; a pick closes it.
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| self.font_list(ui));
            if ui.button("파일에서…").on_hover_text("TTF / OTF / TTC 글꼴 파일").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("터미널 글꼴 파일")
                    .add_filter("글꼴", &["ttf", "otf", "ttc"])
                    .pick_file()
            {
                self.draft.font = path.display().to_string();
            }
        });
        ui.end_row();
    }

    /// All installed families: fixed-width ones first, then the rest.
    fn font_list(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.font_filter)
                .hint_text(format!("글꼴 검색 ({}개)", self.fonts.len()))
                .desired_width(f32::INFINITY),
        );
        ui.separator();
        let filter = self.font_filter.trim().to_lowercase();
        let shown = |f: &&FontChoice| filter.is_empty() || f.name.to_lowercase().contains(&filter);
        let mut picked = None;
        let mut list = |ui: &mut egui::Ui, monospace: bool| {
            for f in self.fonts.iter().filter(|f| f.monospace == monospace).filter(shown) {
                if ui.selectable_label(f.matches(&self.draft.font), &f.name).clicked() {
                    picked = Some(f.key.clone());
                }
            }
        };
        ui.weak("고정폭 글꼴");
        list(ui, true);
        ui.add_space(4.0);
        ui.weak("기타 글꼴 (고정폭이 아니어서 글자 간격이 고르지 않을 수 있음)");
        list(ui, false);
        if fonts::is_file(&self.draft.font) {
            ui.separator();
            let _ = ui.selectable_label(true, font_label(self.fonts, &self.draft.font));
        }
        if let Some(name) = picked {
            self.draft.font = name;
            ui.close();
        }
    }

    fn size_row(&mut self, ui: &mut egui::Ui) {
        ui.label("크기");
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.draft.font_size)
                    .range(AppSettings::MIN_FONT_SIZE..=AppSettings::MAX_FONT_SIZE)
                    .speed(0.5)
                    .fixed_decimals(1)
                    .suffix(" pt"),
            );
            ui.weak("Ctrl+휠 · Ctrl+= / Ctrl+- / Ctrl+0");
        });
        ui.end_row();
    }

    fn theme_row(&mut self, ui: &mut egui::Ui) {
        ui.label("색 테마");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("theme").width(200.0).selected_text(&self.draft.theme).show_ui(ui, |ui| {
                for theme in THEMES {
                    ui.horizontal(|ui| {
                        swatch(ui, theme);
                        ui.selectable_value(&mut self.draft.theme, theme.name.to_owned(), theme.name);
                    });
                }
            });
            swatch(ui, Theme::named(&self.draft.theme));
        });
        ui.end_row();
    }

    /// Sample output in the chosen scheme (the font is already the previewed one).
    fn preview(&self, ui: &mut egui::Ui) {
        let theme = Theme::named(&self.draft.theme);
        let font = FontId::monospace(self.draft.font_size.min(20.0));
        let c = |i: usize| color(theme.ansi[i]);
        let fg = color(theme.foreground);
        egui::Frame::new().fill(color(theme.background)).inner_margin(8.0).corner_radius(4.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let line = |ui: &mut egui::Ui, parts: &[(&str, Color32)]| {
                ui.horizontal(|ui| {
                    for (text, col) in parts {
                        ui.label(RichText::new(*text).font(font.clone()).color(*col));
                    }
                });
            };
            line(ui, &[("user@host", c(10)), (":", fg), ("~/src", c(12)), ("$ ls -l", fg)]);
            line(ui, &[("drwxr-xr-x  ", fg), ("build/", c(12)), ("   ", fg), ("run.sh", c(10)), ("   README.md", fg)]);
            line(ui, &[("error: ", c(9)), ("연결 실패 ", fg), ("warning: ", c(11)), ("한글 가나다 0Oo1lI", fg)]);
            ui.horizontal(|ui| {
                ui.label(RichText::new("┌──┬──┐ ").font(font.clone()).color(fg));
                let selected = RichText::new("선택 영역").font(font.clone()).color(fg);
                ui.label(selected.background_color(color(theme.selection)));
            });
        });
    }
}
