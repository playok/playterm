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
            Section::Display => t("화면", "Display"),
            Section::Terminal => t("터미널", "Terminal"),
            Section::Clipboard => t("키보드·클립보드", "Keyboard & clipboard"),
            Section::Connection => t("연결", "Connection"),
            Section::Transfers => t("파일 전송", "File transfer"),
            Section::Updates => t("업데이트", "Updates"),
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
            ui.heading(t("설정", "Settings"));
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
                if ui.add(icon::primary(ui, icon::OK, t("확인", "OK"))).clicked() {
                    result = Some(SettingsResult::Save(self.draft.clone()));
                }
                if ui.add(icon::button(icon::CANCEL, t("취소", "Cancel"))).clicked() {
                    result = Some(SettingsResult::Cancel(self.original.clone()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let tip = tr!(
                        "'{}' 섹션의 설정만 기본값으로 되돌립니다 (확인을 눌러야 저장됩니다)",
                        "Puts only the '{}' settings back to their defaults (saved with OK)",
                        self.section.name()
                    );
                    if ui.add(icon::button(icon::RESET, t("기본값으로", "Reset to defaults"))).on_hover_text(tip).clicked() {
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
        ui.label(t("스크롤백", "Scrollback"));
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.draft.scrollback)
                    .range(AppSettings::MIN_SCROLLBACK..=AppSettings::MAX_SCROLLBACK)
                    .speed(100)
                    .suffix(t(" 줄", " lines")),
            );
            hint(ui, t("탭마다 보관하는 지난 출력", "Past output kept for each tab"));
        });
        ui.end_row();
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
                .add(icon::button(icon::BROWSE, t("파일에서…", "From file…")))
                .on_hover_text(t("TTF / OTF / TTC 글꼴 파일", "TTF / OTF / TTC font file"))
                .clicked()
                && let Some(path) = crate::picker::Picker::new(crate::picker::Kind::File, t("터미널 글꼴 파일", "Terminal font file"))
                    .filter(t("글꼴", "Fonts"), &["ttf", "otf", "ttc"])
                    .pick_one()
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
            UiTheme::System => t("시스템 설정 따르기", "Follow the system"),
        };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ui-theme").width(200.0).selected_text(name(self.draft.ui_theme)).show_ui(ui, |ui| {
                for t in UiTheme::ALL {
                    ui.selectable_value(&mut self.draft.ui_theme, t, name(t));
                }
            });
            hint(ui, t("메뉴·창의 밝기", "Menus and windows"))
                .on_hover_text(t("터미널 색은 아래 색 테마에서 고릅니다", "The terminal colors are the color theme below"));
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
        if cfg!(target_os = "macos") {
            ui.label(t("Option 키", "Option key"));
            ui.checkbox(&mut self.draft.option_as_meta, t("Meta 키로 쓰기 (ESC + 키)", "Use as Meta (ESC + key)")).on_hover_text(t(
                "켜면 Option+글자가 셸의 Alt 단축키(예: Option+B 한 단어 뒤로)로 갑니다. 끄면 macOS 가 만드는 문자(é, © …)를 입력합니다.",
                "On: Option+letter goes to the shell as an Alt shortcut (e.g. Option+B, word back). Off: it types the character macOS makes (é, ©, …).",
            ));
            ui.end_row();
        }
        // App shortcuts that take keys terminal programs use too (#125): off, the keys go to
        // the terminal.
        if !cfg!(target_os = "macos") {
            ui.label(t("단축키", "Shortcuts"));
            ui.horizontal(|ui| {
                let s = &mut self.draft.shortcuts;
                ui.checkbox(&mut s.alt_arrows_switch_tabs, t("Alt+←/→ 탭 전환", "Alt+←/→ switch tabs")).on_hover_text(t(
                    "끄면 Alt+←/→ 가 터미널로 갑니다 (셸·편집기의 단어 이동). 탭 전환은 Ctrl+Tab 으로도 됩니다.",
                    "Off: Alt+←/→ go to the terminal (word movement in shells and editors). Ctrl+Tab also switches tabs.",
                ));
                ui.checkbox(&mut s.ctrl_zoom, t("Ctrl+= / - / 0 글꼴 크기", "Ctrl+= / - / 0 font size")).on_hover_text(t(
                    "끄면 이 키들이 터미널로 갑니다. 글꼴 크기는 Ctrl+휠과 보기 메뉴로도 바꿉니다.",
                    "Off: these keys go to the terminal. Ctrl+wheel and the View menu still change the font size.",
                ));
            });
            ui.end_row();
        }
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
            hint(ui, &tr!(
                "응답 없는 서버를 기다리는 최대 시간 ({}~{}초)",
                "Longest wait for a server that does not answer ({}–{} s)",
                AppSettings::MIN_CONNECT_TIMEOUT,
                AppSettings::MAX_CONNECT_TIMEOUT
            ));
        });
        ui.end_row();

        ui.label(t("닫기 확인", "Confirm close"));
        ui.checkbox(&mut self.draft.confirm_close, t("연결된 탭을 닫거나 끝낼 때 확인", "Ask before closing connected tabs or quitting"))
            .on_hover_text(t(
                "연결 중이거나 연결된 탭을 닫으면 그 연결이 끊어지므로, 미리 보여 주고 확인을 받습니다.",
                "Closing a connected (or connecting) tab ends its connection, so it is shown first for confirmation.",
            ));
        ui.end_row();
    }

    fn zmodem_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t("ZMODEM 받을 폴더", "ZMODEM downloads"));
        ui.horizontal(|ui| {
            let folder = local_fs::zmodem_folder(&self.draft.zmodem_dir);
            if ui.add(icon::button(icon::BROWSE, t("바꾸기…", "Change…"))).clicked()
                && let Some(dir) = crate::picker::Picker::new(crate::picker::Kind::Folder, t("ZMODEM 으로 받은 파일을 저장할 폴더", "Folder for files received with ZMODEM"))
                    .directory(&folder)
                    .pick_one()
            {
                self.draft.zmodem_dir = dir.display().to_string();
            }
            if !self.draft.zmodem_dir.is_empty() && ui.add(icon::button(icon::RESET, t("기본 폴더", "Default"))).clicked() {
                self.draft.zmodem_dir.clear();
            }
            ui.add(egui::Label::new(egui::RichText::new(folder.display().to_string()).weak()).truncate())
                .on_hover_text(t(
                    "서버에서 sz 로 보낸 파일이 저장되는 곳입니다. 같은 이름의 파일이 있으면 덮어쓰지 않고 '이름 (1)' 로 저장합니다.",
                    "Where files sent with sz on the server are saved. A file with the same name is not replaced: the new one is saved as 'name (1)'.",
                ));
        });
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut self.draft.zmodem_ask, t("받을 때마다 저장할 폴더 묻기", "Ask for the folder each time"))
            .on_hover_text(t(
                "sz 로 받기가 시작되면 저장할 폴더를 묻습니다 (위의 폴더에서 시작).\n고르는 동안에도 임시 폴더에 받고, 받기와 선택이 모두 끝나면 고른 폴더로 옮깁니다.\n창을 취소하면 받기를 취소하고 받은 파일을 지웁니다.",
                "When sz starts sending, a window asks where to save (starting at the folder above).\nThe files are received into a temporary folder meanwhile and moved to the chosen one when both are done.\nCancelling the window cancels the download and deletes what was received.",
            ));
        ui.end_row();
    }

    fn update_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t("업데이트", "Updates"));
        ui.checkbox(&mut self.draft.check_updates, t("시작할 때 새 버전 확인", "Check for a new version at start-up")).on_hover_text(t(
            "GitHub 의 최신 릴리스를 확인해 새 버전이 있으면 알려 줍니다. 설치는 확인을 받은 뒤에만 합니다.
도움말 → 업데이트 확인 으로 언제든 직접 확인할 수 있습니다.",
            "Looks up the latest release on GitHub and tells you about a new version. Nothing is installed without asking.
Help → Check for updates checks at any time.",
        ));
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
            hint(ui, t("Ctrl+휠 · Ctrl+= / Ctrl+- / Ctrl+0", "Ctrl+wheel · Ctrl+= / Ctrl+- / Ctrl+0"));
        });
        ui.end_row();
    }

    fn theme_row(&mut self, ui: &mut egui::Ui) {
        ui.label(t("색 테마", "Color theme"));
        ui.horizontal(|ui| {
            let dark = ui.visuals().dark_mode;
            let auto = t("화면 테마 따라가기", "Follow the UI theme");
            let shown = if self.draft.theme.eq_ignore_ascii_case(Theme::AUTO) { auto.to_owned() } else { self.draft.theme.clone() };
            egui::ComboBox::from_id_salt("theme").width(200.0).selected_text(shown).show_ui(ui, |ui| {
                // First: the scheme that goes with the light or dark look (#162).
                ui.selectable_value(&mut self.draft.theme, Theme::AUTO.to_owned(), auto).on_hover_text(tr!(
                    "다크: {} · 라이트: {}",
                    "Dark: {} · Light: {}",
                    Theme::AUTO_DARK,
                    Theme::AUTO_LIGHT
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
        playterm_i18n::set_english(false);
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
        playterm_i18n::set_english(false);
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
            language: Language::English,
            ..AppSettings::default()
        };
        Section::Display.reset(&mut s);
        assert_eq!(s.font_size, AppSettings::default().font_size, "the section on show is reset");
        assert_eq!(s.scrollback, 1234, "other sections stay");
        assert_eq!(s.macros.len(), 1);
        assert_eq!(s.recent_sites, ["site"]);
        assert_eq!(s.skipped_update, "9.9.9");
        assert_eq!(s.language, Language::English);
        Section::Terminal.reset(&mut s);
        assert_eq!(s.scrollback, AppSettings::default().scrollback);
        assert_eq!(s.macros.len(), 1);
    }
}
