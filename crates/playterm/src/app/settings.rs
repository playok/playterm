//! Applying and saving the settings, and the font size.

use super::*;

impl PlaytermApp {
    pub(super) fn term_setup(&self) -> TermSetup {
        TermSetup { palette: Palette::from_theme(Theme::named(&self.settings.theme)), scrollback: self.settings.scrollback }
    }

    pub(super) fn view_style(&self) -> view::ViewStyle {
        let selection = Theme::rgb(Theme::named(&self.settings.theme).selection);
        let recent = self
            .settings
            .recent_sites
            .iter()
            .filter_map(|id| self.store.get(id))
            .take(5)
            .map(|p| view::RecentSite { id: p.id.clone(), name: p.display_name(), protocol: p.protocol })
            .collect();
        view::ViewStyle {
            font_size: self.settings.font_size,
            selection: Color32::from_rgb(selection.r, selection.g, selection.b),
            clipboard: self.settings.clipboard,
            recent,
            // Only macOS offers the choice (Option also types accented letters there).
            alt_is_meta: !cfg!(target_os = "macos") || self.settings.option_as_meta,
        }
    }

    /// Makes `new` the current settings, updating fonts and open tabs where they changed.
    pub(super) fn apply_settings(&mut self, ctx: &egui::Context, new: AppSettings) {
        let new = new.clamped();
        apply_ui(ctx, &new);
        if new.font != self.settings.font
            && let Err(e) = fonts::install(ctx, &new.font)
        {
            self.status = Some(e);
        }
        if new.theme != self.settings.theme {
            let palette = Palette::from_theme(Theme::named(&new.theme));
            for s in &mut self.sessions {
                s.terminal.set_palette(palette.clone());
            }
        }
        if new.scrollback != self.settings.scrollback {
            for s in &mut self.sessions {
                s.terminal.set_scrollback(new.scrollback);
            }
        }
        if new.zmodem_dir != self.settings.zmodem_dir {
            self.zmodem_dir = local_fs::zmodem_folder(&new.zmodem_dir);
            self.zmodem_last = None;
        }
        self.settings = new;
    }

    pub(super) fn save_settings(&mut self) {
        self.settings_unsaved = None;
        let r = self.settings.save(&self.settings_path);
        self.report(t("설정 저장 실패", "Could not save the settings"), r);
    }

    /// Changes the font size (zoom), saving it once the zooming stops.
    pub(super) fn set_font_size(&mut self, size: f32) {
        let size = size.clamp(AppSettings::MIN_FONT_SIZE, AppSettings::MAX_FONT_SIZE);
        if size != self.settings.font_size {
            self.settings.font_size = size;
            self.settings_unsaved = Some(Instant::now());
        }
    }

    /// Ctrl+wheel / pinch: one point per whole step, so the size stays tidy.
    pub(super) fn zoom(&mut self, steps: f32) {
        self.zoom_accum += steps;
        while self.zoom_accum >= 1.0 {
            self.zoom_accum -= 1.0;
            self.set_font_size(self.settings.font_size.floor() + 1.0);
        }
        while self.zoom_accum <= -1.0 {
            self.zoom_accum += 1.0;
            self.set_font_size(self.settings.font_size.ceil() - 1.0);
        }
    }
}
