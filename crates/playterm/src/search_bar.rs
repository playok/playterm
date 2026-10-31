//! Find bar over the terminal (Ctrl+Shift+F): searches the screen and scrollback, highlights
//! the visible matches and selects the current one.

use eframe::egui::{self, Color32, Key, Rect, RichText, UiBuilder, Vec2};
use playterm_term::{SearchDirection, SearchMatch, Terminal, TextSearch};

/// Background of the other visible matches (the current one is shown as the selection).
pub const MATCH_BG: Color32 = Color32::from_rgb(0xF0, 0xC0, 0x40);
pub const MATCH_FG: Color32 = Color32::BLACK;

const WIDTH: f32 = 380.0;

#[derive(Default)]
pub struct SearchBar {
    query: String,
    search: Option<TextSearch>,
    current: Option<SearchMatch>,
    not_found: bool,
    focus_requested: bool,
}

/// What the user did with the bar this frame.
pub enum BarAction {
    None,
    Close,
}

impl SearchBar {
    /// A bar starting with `text` (e.g. the selected text).
    pub fn new(text: String) -> Self {
        let mut bar = Self { query: text, ..Self::default() };
        bar.search = TextSearch::new(&bar.query);
        bar
    }

    /// Puts the keyboard focus back in the text field (Ctrl+Shift+F while open).
    pub fn focus(&mut self) {
        self.focus_requested = false;
    }

    fn step(&mut self, terminal: &mut Terminal, direction: SearchDirection) {
        let Some(search) = &mut self.search else {
            self.current = None;
            self.not_found = false;
            return;
        };
        match terminal.find(search, self.current.as_ref(), direction) {
            Some(m) => {
                terminal.show_match(&m);
                self.current = Some(m);
                self.not_found = false;
            }
            None => {
                self.current = None;
                self.not_found = true;
                terminal.clear_selection();
            }
        }
    }

    /// The matches on screen, for highlighting.
    pub fn visible_matches(&mut self, terminal: &Terminal) -> Vec<SearchMatch> {
        match &mut self.search {
            Some(search) => terminal.visible_matches(search),
            None => Vec::new(),
        }
    }

    /// Draws the bar in the top right corner of the terminal area `rect`.
    pub fn show(&mut self, ui: &mut egui::Ui, terminal: &mut Terminal, rect: Rect) -> BarAction {
        let size = Vec2::new(WIDTH.min(rect.width() - 16.0).max(200.0), 36.0);
        let bar_rect = Rect::from_min_size(egui::pos2(rect.right() - size.x - 8.0, rect.top() + 6.0), size);
        let mut action = BarAction::None;
        let mut step = None;
        ui.scope_builder(UiBuilder::new().max_rect(bar_rect), |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.query).hint_text("찾기 (스크롤백 포함)").desired_width(170.0),
                    );
                    if !self.focus_requested {
                        edit.request_focus();
                        self.focus_requested = true;
                    }
                    if edit.changed() {
                        self.search = TextSearch::new(&self.query);
                        self.current = None;
                        step = Some(SearchDirection::Older);
                    }
                    let (enter, shift, escape) =
                        ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
                    if edit.lost_focus() && enter {
                        step = Some(if shift { SearchDirection::Newer } else { SearchDirection::Older });
                        edit.request_focus();
                    }
                    if escape && (edit.has_focus() || edit.lost_focus()) {
                        action = BarAction::Close;
                    }

                    if self.not_found {
                        ui.label(RichText::new("없음").color(ui.visuals().error_fg_color));
                    }
                    if ui.small_button("▲").on_hover_text("위로 (Enter)").clicked() {
                        step = Some(SearchDirection::Older);
                    }
                    if ui.small_button("▼").on_hover_text("아래로 (Shift+Enter)").clicked() {
                        step = Some(SearchDirection::Newer);
                    }
                    if ui.small_button("✕").on_hover_text("닫기 (Esc)").clicked() {
                        action = BarAction::Close;
                    }
                });
            });
        });
        if let Some(direction) = step {
            self.step(terminal, direction);
        }
        action
    }
}
