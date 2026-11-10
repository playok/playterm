//! "설정" window: terminal font, size, color scheme and scrollback. Changes are previewed in
//! the open tabs right away; Cancel puts the old settings back.

use eframe::egui::{self, Color32, FontId, RichText, Sense, Vec2};
use playterm_config::{AppSettings, Language, PreventSleep, UiTheme};
use playterm_i18n::{t, tr};
use playterm_term::{THEMES, Theme};

use crate::fonts::{self, FontChoice};
use crate::icon;
use crate::local_fs;

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
    /// The section on show.
    section: Section,
}

fn color(hex: u32) -> Color32 {
    let c = Theme::rgb(hex);
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Display name of a font setting: the family's (Korean) name, or a font file's name.
fn font_label(fonts: &[FontChoice], font: &str) -> String {
    if fonts::is_file(font) {
        let name = std::path::Path::new(font).file_name().map(|n| n.to_string_lossy().into_owned());
        tr!("settings_dialog.file", arg0 = name.unwrap_or_else(|| font.to_owned()))
    } else {
        fonts.iter().find(|f| f.matches(font)).map_or_else(|| font.to_owned(), |f| f.display().to_owned())
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

/// Height of a settings section (the tallest: the display section with its preview).
const SECTION_HEIGHT: f32 = 330.0;
/// Width of a settings section.
const SECTION_WIDTH: f32 = 520.0;
/// The smallest a section gets in a small window (it scrolls then).
const MIN_SECTION_WIDTH: f32 = 280.0;
const MIN_SECTION_HEIGHT: f32 = 160.0;
/// Room the dialog leaves to the window's edges.
const DIALOG_MARGIN: f32 = 40.0;
/// The dialog's heading, buttons and frame, around the section.
const DIALOG_CHROME: f32 = 90.0;

/// A hint after a setting, dim. Kept short to fit the section (wrapped text would change the
/// layout from frame to frame).
fn hint(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.weak(text)
}

/// Width of the list of sections at the left.
const SECTION_LIST_WIDTH: f32 = 130.0;

/// The settings, by subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Section {
    Display,
    Terminal,
    Clipboard,
    Connection,
    Transfers,
    Updates,
}

impl Section {
    const ALL: [Section; 6] =
        [Section::Display, Section::Terminal, Section::Clipboard, Section::Connection, Section::Transfers, Section::Updates];

    fn name(self) -> &'static str {
        match self {
            Section::Display => t!("settings_dialog.display"),
            Section::Terminal => t!("settings_dialog.terminal"),
            Section::Clipboard => t!("settings_dialog.keyboard_clipboard"),
            Section::Connection => t!("settings_dialog.connection"),
            Section::Transfers => t!("settings_dialog.file_transfer"),
            Section::Updates => t!("settings_dialog.updates"),
        }
    }

    /// Puts the settings shown in this section back to their defaults. Everything else stays,
    /// above all what the user made (macros, recent sites, the skipped update, the site view)
    /// and the language.
    fn reset(self, draft: &mut AppSettings) {
        let d = AppSettings::default();
        match self {
            Section::Display => {
                draft.ui_theme = d.ui_theme;
                draft.font = d.font;
                draft.font_size = d.font_size;
                draft.theme = d.theme;
            }
            Section::Terminal => draft.scrollback = d.scrollback,
            Section::Clipboard => {
                draft.clipboard = d.clipboard;
                draft.shortcuts = d.shortcuts;
                draft.option_as_meta = d.option_as_meta;
            }
            Section::Connection => {
                draft.connect_timeout = d.connect_timeout;
                draft.confirm_close = d.confirm_close;
                draft.prevent_sleep = d.prevent_sleep;
            }
            Section::Transfers => {
                draft.zmodem_dir = d.zmodem_dir;
                draft.zmodem_ask = d.zmodem_ask;
            }
            Section::Updates => draft.check_updates = d.check_updates,
        }
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
            section: Section::Display,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> SettingsResult {
        let mut result = None;
        let modal = egui::Modal::new(egui::Id::unique("settings")).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, |ui| {
            // As big as the window leaves room for: a small window (a screen split in two, a large
            // text scale) scrolls the section instead of cutting the dialog off.
            let room = ui.ctx().content_rect().size();
            let width = (room.x - DIALOG_MARGIN - SECTION_LIST_WIDTH - 25.0).clamp(MIN_SECTION_WIDTH, SECTION_WIDTH);
            let height = (room.y - DIALOG_MARGIN - DIALOG_CHROME).clamp(MIN_SECTION_HEIGHT, SECTION_HEIGHT);
            ui.set_width(SECTION_LIST_WIDTH + width + 16.0);
            ui.heading(t!("common.settings"));
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                // The sections down the left (#140); the dialog keeps one size whichever is on
                // show, so nothing moves under the pointer (#143).
                ui.vertical(|ui| {
                    ui.set_width(SECTION_LIST_WIDTH);
                    for section in Section::ALL {
                        let button =
                            egui::Button::selectable(self.section == section, section.name()).min_size(egui::vec2(SECTION_LIST_WIDTH, 0.0));
                        if ui.add(button).clicked() {
                            self.section = section;
                        }
                    }
                });
                // A line as tall as the section (a separator would take the whole height).
                let (line, _) = ui.allocate_exact_size(egui::vec2(9.0, height), Sense::hover());
                ui.painter().vline(line.center().x, line.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
                let size = egui::vec2(width, height);
                let rect = egui::Rect::from_min_size(ui.cursor().min, size);
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
                    ui.set_min_size(size);
                    ui.set_max_size(size);
                    egui::ScrollArea::both().id_salt("settings-section").auto_shrink([false, false]).show(ui, |ui| self.section_ui(ui));
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.add(icon::primary(ui, icon::OK, t!("common.ok"))).clicked() {
                    result = Some(SettingsResult::Save(self.draft.clone()));
                }
                if ui.add(icon::button(icon::CANCEL, t!("common.cancel"))).clicked() {
                    result = Some(SettingsResult::Cancel(self.original.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let tip = tr!("settings_dialog.puts_only_the_settings_back_to_their_defaults", arg0 = self.section.name());
                    if ui.add(icon::button(icon::RESET, t!("settings_dialog.reset_to_defaults"))).on_hover_text(tip).clicked() {
                        self.section.reset(&mut self.draft);
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

    /// The settings of the section on show.
    fn section_ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new(("settings-grid", self.section))
            .num_columns(2)
            .spacing([12.0, 10.0])
            // A bounded column: wrapping hints keep one layout from frame to frame.
            .max_col_width(SECTION_WIDTH - 140.0)
            .show(ui, |ui| match self.section {
                Section::Display => {
                    self.ui_rows(ui);
                    self.font_row(ui);
                    self.size_row(ui);
                    self.theme_row(ui);
                }
                Section::Terminal => self.scrollback_row(ui),
                Section::Clipboard => self.clipboard_rows(ui),
                Section::Connection => {
                    self.timeout_row(ui);
                    if crate::power::SUPPORTED {
                        self.sleep_row(ui);
                    }
                }
                Section::Transfers => self.zmodem_row(ui),
                Section::Updates => self.update_row(ui),
            });
        if self.section == Section::Display {
            ui.add_space(8.0);
            self.preview(ui);
        }
    }

    fn scrollback_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("settings_dialog.scrollback"));
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.draft.scrollback)
                    .range(AppSettings::MIN_SCROLLBACK..=AppSettings::MAX_SCROLLBACK)
                    .speed(100)
                    .suffix(t!("settings_dialog.lines")),
            );
            hint(ui, t!("settings_dialog.past_output_kept_for_each_tab"));
        });
        ui.end_row();
    }

    fn font_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("settings_dialog.font"));
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("font")
                .width(200.0)
                .height(420.0)
                .selected_text(font_label(self.fonts, &self.draft.font))
                // Stay open while typing in the search field; a pick closes it.
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| self.font_list(ui));
            if ui
                .add(icon::button(icon::BROWSE, t!("settings_dialog.from_file")))
                .on_hover_text(t!("settings_dialog.ttf_otf_ttc_font_file"))
                .clicked()
                && let Some(path) = crate::picker::Picker::new(crate::picker::Kind::File, t!("settings_dialog.terminal_font_file"))
                    .filter(t!("settings_dialog.fonts"), &["ttf", "otf", "ttc"])
                    .pick_one()
            {
                self.draft.font = path.display().to_string();
            }
        });
        ui.end_row();
    }

    /// Language and the light/dark look of the program.
    fn ui_rows(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("language.label"));
        egui::ComboBox::from_id_salt("language").selected_text(self.draft.language.name()).show_ui(ui, |ui| {
            for lang in Language::ALL {
                ui.selectable_value(&mut self.draft.language, lang, lang.name());
            }
        });
        ui.end_row();

        ui.label(t!("settings_dialog.ui_theme"));
        let name = |theme: UiTheme| match theme {
            UiTheme::Dark => t!("settings_dialog.dark"),
            UiTheme::Light => t!("settings_dialog.light"),
            UiTheme::System => t!("settings_dialog.follow_the_system"),
        };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ui-theme").width(200.0).selected_text(name(self.draft.ui_theme)).show_ui(ui, |ui| {
                for t in UiTheme::ALL {
                    ui.selectable_value(&mut self.draft.ui_theme, t, name(t));
                }
            });
            hint(ui, t!("settings_dialog.menus_and_windows"))
                .on_hover_text(t!("settings_dialog.the_terminal_colors_are_the_color_theme_below"));
        });
        ui.end_row();
    }

    /// All installed families: fixed-width ones first, then the rest.
    fn font_list(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.font_filter)
                .hint_text(tr!("settings_dialog.search_fonts", arg0 = self.fonts.len()))
                .desired_width(f32::INFINITY),
        );
        ui.separator();
        let filter = self.font_filter.trim().to_lowercase();
        let shown =
            |f: &&FontChoice| filter.is_empty() || f.name.to_lowercase().contains(&filter) || f.key.to_lowercase().contains(&filter);
        let mut picked = None;
        let mut list = |ui: &mut egui::Ui, monospace: bool| {
            let mut fonts: Vec<&FontChoice> = self.fonts.iter().filter(|f| f.monospace == monospace).filter(shown).collect();
            // By the name shown in the current language.
            fonts.sort_by_cached_key(|f| f.display().to_lowercase());
            for f in fonts {
                if ui.selectable_label(f.matches(&self.draft.font), f.display()).clicked() {
                    picked = Some(f.key.clone());
                }
            }
        };
        ui.weak(t!("settings_dialog.fixed_width_fonts"));
        list(ui, true);
        ui.add_space(4.0);
        ui.weak(t!("settings_dialog.other_fonts_not_fixed_width_so_letter_spacing"));
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

    fn sleep_row(&mut self, ui: &mut egui::Ui) {
        let name = |p: PreventSleep| match p {
            PreventSleep::Off => t!("common.off"),
            PreventSleep::System => t!("settings_dialog.no_sleep_while_connected"),
            PreventSleep::Display => t!("settings_dialog.keep_the_screen_on_while_connected"),
        };
        ui.label(t!("settings_dialog.prevent_sleep"));
        egui::ComboBox::from_id_salt("prevent-sleep")
            .width(200.0)
            .selected_text(name(self.draft.prevent_sleep))
            .show_ui(ui, |ui| {
                for p in PreventSleep::ALL {
                    ui.selectable_value(&mut self.draft.prevent_sleep, p, name(p));
                }
            })
            .response
            .on_hover_text(t!("settings_dialog.stops_windows_from_going_to_sleep_when_idle"));
        ui.end_row();
    }

    /// Which keys and mouse buttons copy and paste in the terminal.
    fn clipboard_rows(&mut self, ui: &mut egui::Ui) {
        if cfg!(target_os = "macos") {
            ui.label(t!("settings_dialog.option_key"));
            ui.checkbox(&mut self.draft.option_as_meta, t!("settings_dialog.use_as_meta_esc_key"))
                .on_hover_text(t!("settings_dialog.on_option_letter_goes_to_the_shell_as"));
            ui.end_row();
        }
        // App shortcuts that take keys terminal programs use too (#125): off, the keys go to
        // the terminal.
        if !cfg!(target_os = "macos") {
            ui.label(t!("settings_dialog.shortcuts"));
            ui.horizontal(|ui| {
                let s = &mut self.draft.shortcuts;
                ui.checkbox(&mut s.alt_arrows_switch_tabs, t!("settings_dialog.alt_switch_tabs"))
                    .on_hover_text(t!("settings_dialog.off_alt_go_to_the_terminal_word_movement"));
                ui.checkbox(&mut s.ctrl_zoom, t!("settings_dialog.ctrl_0_font_size"))
                    .on_hover_text(t!("settings_dialog.off_these_keys_go_to_the_terminal_ctrl"));
            });
            ui.end_row();
        }
        let keys = &mut self.draft.clipboard;
        ui.label(t!("common.copy"));
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut keys.copy_on_select, t!("settings_dialog.copy_on_select"))
                .on_hover_text(t!("settings_dialog.text_selected_by_dragging_or_double_clicking_goes"));
            ui.checkbox(&mut keys.ctrl_c, "Ctrl+C").on_hover_text(t!("settings_dialog.copies_only_when_there_is_a_selection_otherwise"));
            ui.checkbox(&mut keys.ctrl_shift_c, "Ctrl+Shift+C");
            ui.checkbox(&mut keys.ctrl_insert, "Ctrl+Insert");
        });
        ui.end_row();

        ui.label(t!("common.paste"));
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut keys.ctrl_v, "Ctrl+V").on_hover_text(t!("settings_dialog.off_ctrl_v_is_sent_as_v_vim"));
            ui.checkbox(&mut keys.ctrl_shift_v, "Ctrl+Shift+V");
            ui.checkbox(&mut keys.shift_insert, "Shift+Insert");
            ui.checkbox(&mut keys.right_click_paste, t!("common.right_click"))
                .on_hover_text(t!("settings_dialog.right_clicking_the_terminal_pastes_as_in_putty"));
            ui.checkbox(&mut keys.middle_click_paste, t!("common.middle_click"));
        });
        ui.end_row();

        ui.label(t!("settings_dialog.confirm_paste"));
        ui.checkbox(&mut keys.confirm_multiline_paste, t!("settings_dialog.ask_before_pasting_several_lines"))
            .on_hover_text(t!("settings_dialog.several_lines_each_end_with_enter_and_may"));
        ui.end_row();

        ui.label(t!("settings_dialog.remote_copy"));
        ui.checkbox(&mut keys.remote_copy, t!("settings_dialog.let_remote_programs_set_the_clipboard_osc_52"))
            .on_hover_text(t!("settings_dialog.tmux_vim_and_the_like_can_put_what"));
        ui.end_row();
    }

    fn timeout_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("settings_dialog.connection_timeout"));
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.draft.connect_timeout)
                    .range(AppSettings::MIN_CONNECT_TIMEOUT..=AppSettings::MAX_CONNECT_TIMEOUT)
                    .speed(1)
                    .suffix(t!("common.s")),
            )
            .on_hover_text(t!("settings_dialog.gives_up_when_an_ssh_telnet_or_rdp"));
            hint(
                ui,
                &tr!(
                    "settings_dialog.longest_wait_for_a_server_that_does_not",
                    arg0 = AppSettings::MIN_CONNECT_TIMEOUT,
                    arg1 = AppSettings::MAX_CONNECT_TIMEOUT
                ),
            );
        });
        ui.end_row();

        ui.label(t!("settings_dialog.confirm_close"));
        ui.checkbox(&mut self.draft.confirm_close, t!("settings_dialog.ask_before_closing_connected_tabs_or_quitting"))
            .on_hover_text(t!("settings_dialog.closing_a_connected_or_connecting_tab_ends_its"));
        ui.end_row();
    }

    fn zmodem_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("settings_dialog.zmodem_downloads"));
        ui.horizontal(|ui| {
            let folder = local_fs::zmodem_folder(&self.draft.zmodem_dir);
            if ui.add(icon::button(icon::BROWSE, t!("settings_dialog.change"))).clicked()
                && let Some(dir) =
                    crate::picker::Picker::new(crate::picker::Kind::Folder, t!("settings_dialog.folder_for_files_received_with_zmodem"))
                        .directory(&folder)
                        .pick_one()
            {
                self.draft.zmodem_dir = dir.display().to_string();
            }
            if !self.draft.zmodem_dir.is_empty() && ui.add(icon::button(icon::RESET, t!("settings_dialog.default"))).clicked() {
                self.draft.zmodem_dir.clear();
            }
            ui.add(egui::Label::new(egui::RichText::new(folder.display().to_string()).weak()).truncate())
                .on_hover_text(t!("settings_dialog.where_files_sent_with_sz_on_the_server"));
        });
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut self.draft.zmodem_ask, t!("settings_dialog.ask_for_the_folder_each_time"))
            .on_hover_text(t!("settings_dialog.when_sz_starts_sending_a_window_asks_where"));
        ui.end_row();
    }

    fn update_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("settings_dialog.updates"));
        ui.checkbox(&mut self.draft.check_updates, t!("settings_dialog.check_for_a_new_version_at_start_up"))
            .on_hover_text(t!("settings_dialog.looks_up_the_latest_release_on_github_and"));
        ui.end_row();
    }

    fn size_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("common.size"));
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.draft.font_size)
                    .range(AppSettings::MIN_FONT_SIZE..=AppSettings::MAX_FONT_SIZE)
                    .speed(0.5)
                    .fixed_decimals(1)
                    .suffix(" pt"),
            );
            hint(ui, t!("settings_dialog.ctrl_wheel_ctrl_ctrl_ctrl_0"));
        });
        ui.end_row();
    }

    fn theme_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t!("settings_dialog.color_theme"));
        ui.horizontal(|ui| {
            let dark = ui.visuals().dark_mode;
            let auto = t!("settings_dialog.follow_the_ui_theme");
            let shown = if self.draft.theme.eq_ignore_ascii_case(Theme::AUTO) { auto.to_owned() } else { self.draft.theme.clone() };
            egui::ComboBox::from_id_salt("theme").width(200.0).selected_text(shown).show_ui(ui, |ui| {
                // First: the scheme that goes with the light or dark look (#162).
                ui.selectable_value(&mut self.draft.theme, Theme::AUTO.to_owned(), auto).on_hover_text(tr!(
                    "settings_dialog.dark_light",
                    arg0 = Theme::AUTO_DARK,
                    arg1 = Theme::AUTO_LIGHT
                ));
                for theme in THEMES {
                    ui.horizontal(|ui| {
                        swatch(ui, theme);
                        ui.selectable_value(&mut self.draft.theme, theme.name.to_owned(), theme.name);
                    });
                }
            });
            swatch(ui, Theme::effective(&self.draft.theme, dark));
        });
        ui.end_row();
    }

    /// Sample output in the chosen scheme (the font is already the previewed one).
    fn preview(&self, ui: &mut egui::Ui) {
        let theme = Theme::effective(&self.draft.theme, ui.visuals().dark_mode);
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
            // The localized sample previews the glyphs needed by the selected UI language.
            let failed = t!("settings_dialog.connection_failed");
            line(ui, &[("error: ", c(9)), (failed, fg), ("warning: ", c(11)), (t!("settings_dialog.font_sample"), fg)]);
            ui.horizontal(|ui| {
                ui.label(RichText::new("┌──┬──┐ ").font(font.clone()).color(fg));
                let selected = RichText::new(t!("settings_dialog.selection")).font(font.clone()).color(fg);
                ui.label(selected.background_color(color(theme.selection)));
            });
        });
    }
}

#[cfg(test)]
mod section_tests {
    use super::*;

    /// The text the dialog draws with `section` on show, and the dialog's height.
    fn render(section: Section) -> (String, f32, f32) {
        let ctx = egui::Context::default();
        let mut dialog = SettingsDialog::new(&AppSettings::default());
        dialog.section = section;
        let (mut text, mut height, mut left) = (String::new(), 0.0, 0.0);
        for _ in 0..3 {
            // A modal is laid out on a later pass.
            let mut out = ctx.run_ui(Default::default(), |ui| {
                let _ = dialog.show(ui.ctx());
            });
            out.textures_delta.clear();
            let mut texts = Vec::new();
            let mut bottom = 0.0f32;
            let mut top = f32::MAX;
            for s in &out.shapes {
                if let egui::Shape::Text(t) = &s.shape {
                    texts.push(t.galley.text().to_owned());
                    top = top.min(t.pos.y);
                    bottom = bottom.max(t.pos.y + t.galley.size().y);
                    if t.galley.text() == "설정" {
                        left = t.pos.x;
                    }
                }
            }
            text = texts.join("\n");
            height = bottom - top;
        }
        (text, height, left)
    }

    #[test]
    fn each_section_shows_its_own_settings() {
        playterm_i18n::set_language(playterm_i18n::Language::KOREAN);
        let expect = [
            (Section::Display, "글꼴"),
            (Section::Terminal, "스크롤백"),
            (Section::Clipboard, "붙여넣기"),
            (Section::Connection, "연결 시간 제한"),
            (Section::Transfers, "ZMODEM"),
            (Section::Updates, "업데이트"),
        ];
        let mut heights = Vec::new();
        let mut lefts = Vec::new();
        for (section, label) in expect {
            let (text, height, left) = render(section);
            lefts.push(left);
            assert!(text.contains(label), "{section:?}: {label} missing in\n{text}");
            heights.push(height);
        }
        // Scrollback is only in its own section.
        assert!(!render(Section::Display).0.contains("스크롤백"));
        // The dialog keeps its height (the OK row stays put) from section to section.
        let (min, max) = heights.iter().fold((f32::MAX, 0.0f32), |(a, b), &h| (a.min(h), b.max(h)));
        assert!(max - min < 2.0, "heights {heights:?}");
        // And its width: it stays where it is (#143).
        assert!(lefts.iter().all(|&x| (x - lefts[0]).abs() < 0.5), "left edges {lefts:?}");
    }

    /// A small window (500×400 points: 750×600 pixels at 150 %): the whole dialog stays inside
    /// it, its buttons too, and the section scrolls.
    #[test]
    fn the_dialog_fits_a_small_window() {
        playterm_i18n::set_language(playterm_i18n::Language::KOREAN);
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 400.0));
        let mut dialog = SettingsDialog::new(&AppSettings::default());
        let mut texts = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                let _ = dialog.show(ui.ctx());
            });
            out.textures_delta.clear();
            texts = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_owned(), egui::Rect::from_min_size(t.pos, t.galley.size()))),
                    _ => None,
                })
                .collect();
        }
        for needed in ["설정", "화면", "확인", "취소", "기본값으로"] {
            let (_, rect) = texts.iter().find(|(t, _)| t.contains(needed)).unwrap_or_else(|| panic!("{needed} missing: {texts:?}"));
            assert!(screen.contains_rect(*rect), "{needed} at {rect:?} is outside the window");
        }
    }

    /// "Reset to defaults" (#121) resets the section on show and nothing else: macros, recent
    /// sites and the other sections stay.
    #[test]
    fn reset_keeps_what_the_user_made() {
        let mut s = AppSettings {
            font_size: 22.0,
            scrollback: 1234,
            macros: vec![playterm_config::Macro { name: "deploy".into(), script: "ls".into() }],
            recent_sites: vec!["site".into()],
            skipped_update: "9.9.9".into(),
            language: Language::ENGLISH,
            ..AppSettings::default()
        };
        Section::Display.reset(&mut s);
        assert_eq!(s.font_size, AppSettings::default().font_size, "the section on show is reset");
        assert_eq!(s.scrollback, 1234, "other sections stay");
        assert_eq!(s.macros.len(), 1);
        assert_eq!(s.recent_sites, ["site"]);
        assert_eq!(s.skipped_update, "9.9.9");
        assert_eq!(s.language, Language::ENGLISH);
        Section::Terminal.reset(&mut s);
        assert_eq!(s.scrollback, AppSettings::default().scrollback);
        assert_eq!(s.macros.len(), 1);
    }
}
