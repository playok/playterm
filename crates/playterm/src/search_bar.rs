//! Find bar above the terminal (Ctrl+Shift+F): searches the screen and scrollback, highlights
//! the visible matches, selects the current one and says which of how many it is. It takes a
//! strip of its own at the top of the terminal area, so it never covers output.

use eframe::egui::{self, Color32, Key, Rect, RichText, UiBuilder};
use playterm_i18n::{t, tr};
use playterm_term::{SearchDirection, SearchMatch, SearchOptions, Terminal, TextSearch};

use crate::icon;

/// Background of the other visible matches (the current one is shown as the selection).
pub const MATCH_BG: Color32 = Color32::from_rgb(0xF0, 0xC0, 0x40);
pub const MATCH_FG: Color32 = Color32::BLACK;

/// Height of the strip the bar takes above the terminal text.
pub const HEIGHT: f32 = 34.0;

/// Matches counted at most (a count over a huge scrollback must not slow typing down).
const MAX_COUNTED: usize = 10_000;

#[derive(Default)]
pub struct SearchBar {
    query: String,
    options: SearchOptions,
    search: Option<TextSearch>,
    /// Why the text is not a valid regular expression.
    error: Option<String>,
    current: Option<SearchMatch>,
    /// Every match in the history, oldest first, as of the last search.
    matches: Vec<SearchMatch>,
    not_found: bool,
    focus_requested: bool,
}

/// What the user did with the bar this frame.
pub enum BarAction {
    None,
    Close,
}

/// "3 / 12": the current match among all of them (none current: just the count).
pub fn count_label(current: Option<usize>, total: usize, capped: bool) -> String {
    let total = if capped { format!("{total}+") } else { total.to_string() };
    match current {
        Some(i) => format!("{} / {total}", i + 1),
        None => tr!("search_bar.found", arg0 = total),
    }
}

impl SearchBar {
    /// A bar starting with `text` (e.g. the selected text).
    pub fn new(text: String) -> Self {
        let mut bar = Self { query: text, ..Self::default() };
        bar.compile();
        bar
    }

    /// Puts the keyboard focus back in the text field (Ctrl+Shift+F while open).
    pub fn focus(&mut self) {
        self.focus_requested = false;
    }

    /// Builds the search from the text and the options.
    fn compile(&mut self) {
        self.current = None;
        self.matches.clear();
        match TextSearch::with_options(&self.query, self.options) {
            Ok(search) => {
                self.search = search;
                self.error = None;
            }
            Err(e) => {
                self.search = None;
                self.error = Some(e);
            }
        }
    }

    fn step(&mut self, terminal: &mut Terminal, direction: SearchDirection) {
        let Some(search) = &mut self.search else {
            self.current = None;
            self.not_found = false;
            return;
        };
        // Output may have arrived since the last count.
        self.matches = terminal.all_matches(search, MAX_COUNTED);
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

    /// The "3 / 12" text, or why there is none.
    fn status(&self, ui: &egui::Ui) -> Option<RichText> {
        if let Some(e) = &self.error {
            let first = e.lines().next().unwrap_or_default().to_owned();
            return Some(RichText::new(tr!("search_bar.bad_regular_expression", first = first)).color(ui.visuals().error_fg_color));
        }
        if self.not_found {
            return Some(RichText::new(t!("search_bar.no_matches")).color(ui.visuals().error_fg_color));
        }
        self.search.as_ref()?;
        let current = self.current.as_ref().and_then(|c| self.matches.iter().position(|m| m == c));
        Some(RichText::new(count_label(current, self.matches.len(), self.matches.len() >= MAX_COUNTED)).weak())
    }

    /// Draws the bar across `strip`, the space above the terminal text.
    pub fn show(&mut self, ui: &mut egui::Ui, terminal: &mut Terminal, strip: Rect) -> BarAction {
        let mut action = BarAction::None;
        let mut step = None;
        ui.painter().rect_filled(strip, 0.0, ui.visuals().panel_fill);
        ui.scope_builder(UiBuilder::new().max_rect(strip.shrink2(egui::vec2(6.0, 4.0))), |ui| {
            ui.horizontal_centered(|ui| {
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.query).hint_text(t!("search_bar.find_including_scrollback")).desired_width(220.0),
                );
                if !self.focus_requested {
                    edit.request_focus();
                    self.focus_requested = true;
                }
                let mut changed = edit.changed();
                changed |= ui.toggle_value(&mut self.options.case_sensitive, "Aa").on_hover_text(t!("search_bar.match_case")).changed();
                changed |= ui.toggle_value(&mut self.options.regex, ".*").on_hover_text(t!("search_bar.regular_expression")).changed();
                if changed {
                    self.compile();
                    step = Some(SearchDirection::Older);
                }
                let (enter, shift, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
                if edit.lost_focus() && enter {
                    step = Some(if shift { SearchDirection::Newer } else { SearchDirection::Older });
                    edit.request_focus();
                }
                if escape && (edit.has_focus() || edit.lost_focus()) {
                    action = BarAction::Close;
                }

                if ui.add(icon::icon_button(icon::UP).small()).on_hover_text(t!("search_bar.up_enter")).clicked() {
                    step = Some(SearchDirection::Older);
                }
                if ui.add(icon::icon_button(icon::DOWN).small()).on_hover_text(t!("search_bar.down_shift_enter")).clicked() {
                    step = Some(SearchDirection::Newer);
                }
                if let Some(status) = self.status(ui) {
                    ui.label(status);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(icon::icon_button(icon::CLOSE).small()).on_hover_text(t!("search_bar.close_esc")).clicked() {
                        action = BarAction::Close;
                    }
                });
            });
        });
        let line = egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);
        ui.painter().hline(strip.x_range(), strip.bottom(), line);
        if let Some(direction) = step {
            self.step(terminal, direction);
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use playterm_term::Charset;

    #[test]
    fn counts_and_steps_through_matches() {
        assert_eq!(count_label(Some(2), 12, false), "3 / 12");
        assert_eq!(count_label(Some(0), 10_000, true), "1 / 10000+");

        let mut term = Terminal::new(20, 4, Charset::Utf8, 100);
        term.feed(b"alpha 1\r\nbeta\r\nalpha 2\r\ngamma\r\nalpha 3\r\n");
        let mut bar = SearchBar::new("alpha".into());
        bar.step(&mut term, SearchDirection::Older);
        assert_eq!(bar.matches.len(), 3);
        let at = |bar: &SearchBar| bar.matches.iter().position(|m| Some(m) == bar.current.as_ref());
        assert_eq!(at(&bar), Some(2), "the newest first");
        bar.step(&mut term, SearchDirection::Older);
        assert_eq!(at(&bar), Some(1));

        // A bad regular expression is reported, not searched.
        bar.query = "alpha (".into();
        bar.options.regex = true;
        bar.compile();
        assert!(bar.error.is_some() && bar.search.is_none());
        bar.options.regex = false;
        bar.compile();
        assert!(bar.error.is_none() && bar.search.is_some(), "literal text is fine");
    }
}
