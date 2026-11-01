//! "설정" window: terminal font, size, color scheme and scrollback. Changes are previewed in
//! the open tabs right away; Cancel puts the old settings back.

use eframe::egui::{self, Color32, FontId, RichText, Sense, Vec2};
use playterm_config::{AppSettings, Language, PreventSleep, UiTheme};
use playterm_i18n::{t, tr};
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
        tr!("파일: {}", "File: {}", name.unwrap_or_else(|| font.to_owned()))
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
            ui.heading(t("설정", "Settings"));
            ui.add_space(8.0);
            egui::Grid::new("settings-grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                self.ui_rows(ui);
                self.font_row(ui);
                self.size_row(ui);
                self.theme_row(ui);

                ui.label(t("스크롤백", "Scrollback"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.draft.scrollback)
                            .range(AppSettings::MIN_SCROLLBACK..=AppSettings::MAX_SCROLLBACK)
                            .speed(100)
                            .suffix(t(" 줄", " lines")),
                    );
                    ui.weak(t("탭마다 보관하는 지난 출력", "Past output kept for each tab"));
                });
                ui.end_row();
                self.sleep_row(ui);
                self.timeout_row(ui);
                self.clipboard_rows(ui);
            });
            ui.add_space(8.0);
            self.preview(ui);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(t("확인", "OK")).clicked() {
                    result = Some(SettingsResult::Save(self.draft.clone()));
                }
                if ui.button(t("취소", "Cancel")).clicked() {
                    result = Some(SettingsResult::Cancel(self.original.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("기본값으로", "Reset to defaults")).clicked() {
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
        ui.label(t("글꼴", "Font"));
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("font")
                .width(200.0)
                .height(420.0)
                .selected_text(font_label(self.fonts, &self.draft.font))
                // Stay open while typing in the search field; a pick closes it.
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| self.font_list(ui));
            if ui
                .button(t("파일에서…", "From file…"))
                .on_hover_text(t("TTF / OTF / TTC 글꼴 파일", "TTF / OTF / TTC font file"))
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title(t("터미널 글꼴 파일", "Terminal font file"))
                    .add_filter(t("글꼴", "Fonts"), &["ttf", "otf", "ttc"])
                    .pick_file()
            {
                self.draft.font = path.display().to_string();
            }
        });
        ui.end_row();
    }

    /// Language and the light/dark look of the program.
    fn ui_rows(&mut self, ui: &mut egui::Ui) {
        ui.label("언어 / Language");
        egui::ComboBox::from_id_salt("language").selected_text(self.draft.language.name()).show_ui(ui, |ui| {
            for lang in Language::ALL {
                ui.selectable_value(&mut self.draft.language, lang, lang.name());
            }
        });
        ui.end_row();

        ui.label(t("화면 테마", "UI theme"));
        let name = |theme: UiTheme| match theme {
            UiTheme::Dark => t("다크", "Dark"),
            UiTheme::Light => t("라이트", "Light"),
            UiTheme::System => t("Windows 설정 따르기", "Follow Windows"),
        };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ui-theme").width(200.0).selected_text(name(self.draft.ui_theme)).show_ui(
                ui,
                |ui| {
                    for t in UiTheme::ALL {
                        ui.selectable_value(&mut self.draft.ui_theme, t, name(t));
                    }
                },
            );
            ui.weak(t(
                "메뉴·창의 밝기 (터미널 색은 아래 색 테마)",
                "Brightness of menus and windows (terminal colors: color theme below)",
            ));
        });
        ui.end_row();
    }

    /// All installed families: fixed-width ones first, then the rest.
    fn font_list(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.font_filter)
                .hint_text(tr!("글꼴 검색 ({}개)", "Search fonts ({})", self.fonts.len()))
                .desired_width(f32::INFINITY),
        );
        ui.separator();
        let filter = self.font_filter.trim().to_lowercase();
        let shown = |f: &&FontChoice| {
            filter.is_empty() || f.name.to_lowercase().contains(&filter) || f.key.to_lowercase().contains(&filter)
        };
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
        ui.weak(t("고정폭 글꼴", "Fixed-width fonts"));
        list(ui, true);
        ui.add_space(4.0);
        ui.weak(t(
            "기타 글꼴 (고정폭이 아니어서 글자 간격이 고르지 않을 수 있음)",
            "Other fonts (not fixed-width, so letter spacing may be uneven)",
        ));
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
            PreventSleep::Off => t("끔", "Off"),
            PreventSleep::System => t("연결 중에는 절전 모드 안 함", "No sleep while connected"),
            PreventSleep::Display => t("연결 중에는 화면도 켜 둠", "Keep the screen on while connected"),
        };
        ui.label(t("절전 방지", "Prevent sleep"));
        egui::ComboBox::from_id_salt("prevent-sleep")
            .width(200.0)
            .selected_text(name(self.draft.prevent_sleep))
            .show_ui(ui, |ui| {
                for p in PreventSleep::ALL {
                    ui.selectable_value(&mut self.draft.prevent_sleep, p, name(p));
                }
            })
            .response
            .on_hover_text(t(
                "세션이 연결되어 있는 동안 Windows 가 유휴 상태로 절전에 들어가 연결이 끊기는 것을 막습니다.\n덮개를 닫거나 직접 절전을 고르면 그대로 절전합니다.",
                "Stops Windows from going to sleep when idle, which would drop the connections, while a session is connected.\nClosing the lid or choosing Sleep still works.",
            ));
        ui.end_row();
    }

    /// Which keys and mouse buttons copy and paste in the terminal.
    fn clipboard_rows(&mut self, ui: &mut egui::Ui) {
        let keys = &mut self.draft.clipboard;
        ui.label(t("복사", "Copy"));
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut keys.copy_on_select, t("선택하면 바로 복사", "Copy on select")).on_hover_text(t(
                "마우스로 드래그하거나 더블클릭해 선택하면 바로 클립보드에 복사합니다 (PuTTY 방식).",
                "Text selected by dragging or double-clicking goes to the clipboard right away (as in PuTTY).",
            ));
            ui.checkbox(&mut keys.ctrl_c, "Ctrl+C").on_hover_text(t(
                "선택 영역이 있을 때만 복사하고, 없으면 ^C (중단 신호)를 보냅니다.\n끄면 Ctrl+C 는 항상 ^C 를 보냅니다.",
                "Copies only when there is a selection, otherwise sends ^C (interrupt).\nOff: Ctrl+C always sends ^C.",
            ));
            ui.checkbox(&mut keys.ctrl_shift_c, "Ctrl+Shift+C");
            ui.checkbox(&mut keys.ctrl_insert, "Ctrl+Insert");
        });
        ui.end_row();

        ui.label(t("붙여넣기", "Paste"));
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut keys.ctrl_v, "Ctrl+V").on_hover_text(t(
                "끄면 Ctrl+V 는 ^V 로 원격에 전달됩니다 (vim 의 블록 선택 등).",
                "Off: Ctrl+V is sent as ^V (vim's block selection and the like).",
            ));
            ui.checkbox(&mut keys.ctrl_shift_v, "Ctrl+Shift+V");
            ui.checkbox(&mut keys.shift_insert, "Shift+Insert");
            ui.checkbox(&mut keys.right_click_paste, t("오른쪽 클릭", "Right click")).on_hover_text(t(
                "터미널을 마우스 오른쪽 버튼으로 클릭하면 붙여넣습니다 (PuTTY 방식).\n메뉴는 Ctrl+오른쪽 클릭으로 엽니다.",
                "Right-clicking the terminal pastes (as in PuTTY).\nCtrl+right click opens the menu.",
            ));
            ui.checkbox(&mut keys.middle_click_paste, t("가운데 클릭", "Middle click"));
        });
        ui.end_row();

        ui.label(t("붙여넣기 확인", "Confirm paste"));
        ui.checkbox(&mut keys.confirm_multiline_paste, t("여러 줄을 붙여넣기 전에 확인", "Ask before pasting several lines"))
            .on_hover_text(t(
                "붙여넣을 내용이 여러 줄이면 줄마다 Enter 가 입력되어 명령으로 바로 실행될 수 있어, 미리 보여 주고 확인을 받습니다.\n원격 프로그램이 붙여넣기 보호(bracketed paste)를 켠 경우에는 묻지 않습니다.",
                "Several lines each end with Enter and may run as commands right away, so they are shown first for confirmation.\nNot asked when the remote program turned on bracketed paste.",
            ));
        ui.end_row();

        ui.label(t("원격 복사", "Remote copy"));
        ui.checkbox(&mut keys.remote_copy, t("원격 프로그램이 클립보드에 쓰기 허용 (OSC 52)", "Let remote programs set the clipboard (OSC 52)"))
            .on_hover_text(t(
                "tmux · vim 등이 복사한 내용을 이 PC 클립보드에 넣을 수 있습니다. 지금 보고 있는 탭만, 1MB 까지이고, 쓸 때마다 화면 아래에 알립니다.\n원격에서 클립보드를 읽는 것은 항상 막혀 있습니다.",
                "tmux, vim and the like can put what they copy on this PC's clipboard: only from the tab on screen, up to 1 MB, with a notice each time.\nReading the clipboard from the remote side is always refused.",
            ));
        ui.end_row();
    }

    fn timeout_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t("연결 시간 제한", "Connection timeout"));
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.draft.connect_timeout)
                    .range(AppSettings::MIN_CONNECT_TIMEOUT..=AppSettings::MAX_CONNECT_TIMEOUT)
                    .speed(1)
                    .suffix(t(" 초", " s")),
            )
            .on_hover_text(t(
                "SSH · Telnet · RDP 서버가 이 시간 동안 응답하지 않으면 연결을 포기합니다 (TCP 연결부터 로그인까지).\n호스트 키 확인이나 비밀번호 입력을 기다리는 시간은 세지 않습니다. 새 연결부터 적용됩니다.",
                "Gives up when an SSH, Telnet or RDP server does not answer for this long (from the TCP connect to the login).\nTime spent confirming a host key or typing a password does not count. Applies to new connections.",
            ));
            ui.weak(tr!(
                "응답 없는 서버를 기다리는 최대 시간 ({}~{}초)",
                "Longest wait for a server that does not answer ({}–{} s)",
                AppSettings::MIN_CONNECT_TIMEOUT,
                AppSettings::MAX_CONNECT_TIMEOUT
            ));
        });
        ui.end_row();
    }

    fn size_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t("크기", "Size"));
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.draft.font_size)
                    .range(AppSettings::MIN_FONT_SIZE..=AppSettings::MAX_FONT_SIZE)
                    .speed(0.5)
                    .fixed_decimals(1)
                    .suffix(" pt"),
            );
            ui.weak(t("Ctrl+휠 · Ctrl+= / Ctrl+- / Ctrl+0", "Ctrl+wheel · Ctrl+= / Ctrl+- / Ctrl+0"));
        });
        ui.end_row();
    }

    fn theme_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t("색 테마", "Color theme"));
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
            // The Hangul sample stays in both languages: it previews the font's Korean glyphs.
            let failed = t("연결 실패 ", "connection failed ");
            line(ui, &[("error: ", c(9)), (failed, fg), ("warning: ", c(11)), ("한글 가나다 0Oo1lI", fg)]);
            ui.horizontal(|ui| {
                ui.label(RichText::new("┌──┬──┐ ").font(font.clone()).color(fg));
                let selected = RichText::new(t("선택 영역", "selection")).font(font.clone()).color(fg);
                ui.label(selected.background_color(color(theme.selection)));
            });
        });
    }
}
