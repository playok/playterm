//! The look of the windows: light or dark, readable small and weak text.

use super::*;

/// Language and light/dark look of the windows (the terminal colors are separate).
pub(super) fn apply_ui(ctx: &egui::Context, settings: &AppSettings) {
    playterm_i18n::set_language(settings.language);
    ctx.set_theme(match settings.ui_theme {
        UiTheme::Dark => egui::ThemePreference::Dark,
        UiTheme::Light => egui::ThemePreference::Light,
        UiTheme::System => egui::ThemePreference::System,
    });
    ctx.set_visuals_of(egui::Theme::Dark, readable_dark());
    ctx.set_visuals_of(egui::Theme::Light, soft_light());
    ctx.all_styles_mut(readable_text);
}

/// WCAG contrast ratio of two colors (1 to 21), for the tests that keep the colors readable.
#[cfg(test)]
pub(crate) fn contrast(a: Color32, b: Color32) -> f32 {
    let luminance = |c: Color32| {
        let [r, g, b, _] = egui::Rgba::from(c).to_array();
        0.2126 * r + 0.7152 * g + 0.0722 * b
    };
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The color of simultaneous input (titles, frames, its notice): orange, darker on the light
/// look so it reads there too (#159).
pub(crate) fn broadcast_color(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode { Color32::from_rgb(230, 140, 30) } else { Color32::from_rgb(158, 76, 0) }
}

/// Behind a modal dialog: the window dimmed enough that the dialog is where the eye goes (#154).
pub(crate) const MODAL_BACKDROP: Color32 = Color32::from_black_alpha(150);

/// Size of small UI text (hints, the status bar, the saved-passwords line). egui's 9 px is
/// hard to read on 1× monitors.
pub(super) const SMALL_TEXT: f32 = 12.0;

/// egui's text sizes with readable small text.
pub(super) fn readable_text(style: &mut egui::Style) {
    style.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(SMALL_TEXT));
}

/// Weak text (hints, placeholders, status details) at the WCAG AA contrast for body text,
/// 4.5:1: egui makes it from the text color at 60 % alpha, which gives about 2.7:1 on the dark
/// panels. Still dimmer than normal text (gray 140 on dark), so it reads as secondary.
pub(super) const DARK_WEAK_TEXT: u8 = 134;

pub(super) const LIGHT_WEAK_TEXT: u8 = 100;

/// egui's dark look with readable weak text.
pub(super) fn readable_dark() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.weak_text_color = Some(Color32::from_gray(DARK_WEAK_TEXT));
    v
}

/// egui's light look with the pure whites toned down to light grays (easier on the eyes,
/// close to Windows 11 light mode): panels, dialogs and text fields a step darker, buttons and
/// separators darker still so they keep their contrast.
pub(super) fn soft_light() -> egui::Visuals {
    let gray = Color32::from_gray;
    let mut v = egui::Visuals::light();
    v.weak_text_color = Some(gray(LIGHT_WEAK_TEXT));
    v.panel_fill = gray(234);
    v.window_fill = gray(240);
    v.extreme_bg_color = gray(246); // text fields
    // Links dark enough to read on the light panels (#160).
    v.hyperlink_color = Color32::from_rgb(0, 95, 184);
    v.code_bg_color = gray(222);
    let w = &mut v.widgets;
    w.noninteractive.weak_bg_fill = gray(234);
    w.noninteractive.bg_fill = gray(234);
    w.noninteractive.bg_stroke.color = gray(178); // separators
    for (state, fill) in [(&mut w.inactive, 218), (&mut w.hovered, 206), (&mut w.open, 206), (&mut w.active, 160)] {
        state.weak_bg_fill = gray(fill);
        state.bg_fill = gray(fill);
    }
    // Text fields (and buttons) show their edge: the field is barely lighter than the dialog,
    // and an edge needs 3:1 to be seen (#161).
    w.inactive.bg_stroke = egui::Stroke::new(1.0, gray(130));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of a gray.
    fn luminance(gray: u8) -> f64 {
        let c = gray as f64 / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (la, lb) = (luminance(a.r()), luminance(b.r()));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn the_ui_follows_the_system_theme_by_default() {
        let ctx = egui::Context::default();
        apply_ui(&ctx, &AppSettings::default());
        for system in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            let input = egui::RawInput { system_theme: Some(system), ..Default::default() };
            let mut out = ctx.run_ui(input, |_| {});
            out.textures_delta.clear();
            assert_eq!(ctx.theme(), system, "follows the system as it changes");
        }
        // A theme chosen in the settings wins over the system's.
        apply_ui(&ctx, &AppSettings { ui_theme: UiTheme::Dark, ..AppSettings::default() });
        let input = egui::RawInput { system_theme: Some(egui::Theme::Light), ..Default::default() };
        let mut out = ctx.run_ui(input, |_| {});
        out.textures_delta.clear();
        assert_eq!(ctx.theme(), egui::Theme::Dark);
    }

    #[test]
    fn small_text_is_readable() {
        let mut style = egui::Style::default();
        readable_text(&mut style);
        let small = style.text_styles[&egui::TextStyle::Small].size;
        assert!(small >= 12.0, "{small}");
        // Still no bigger than body text, so it stays secondary.
        assert!(small <= style.text_styles[&egui::TextStyle::Body].size);
    }

    #[test]
    fn weak_text_is_readable_but_weaker() {
        for v in [readable_dark(), soft_light()] {
            let weak = v.weak_text_color();
            for bg in [v.panel_fill, v.window_fill, v.extreme_bg_color] {
                assert!(contrast(weak, bg) >= 4.5, "{weak:?} on {bg:?}: {:.2}", contrast(weak, bg));
            }
            // Normal text stands out more than weak text.
            assert!(contrast(v.text_color(), v.panel_fill) > contrast(weak, v.panel_fill));
        }
    }

    /// The colors fixed in #156, #158, #159, #160 and #161 read on both looks.
    #[test]
    fn themed_colors_keep_their_contrast() {
        use playterm_config::TabColor;
        for v in [readable_dark(), soft_light()] {
            let look = if v.dark_mode { "dark" } else { "light" };
            for c in TabColor::ALL.into_iter().map(crate::menu::color32).map(Some).chain([None]) {
                let fill = crate::view::line_fill(&v, c);
                let ratio = super::contrast(v.strong_text_color(), fill);
                assert!(ratio >= 4.5, "{look}: connection line {c:?}: {ratio:.2}");
            }
            let orange = broadcast_color(&v);
            for bg in [v.panel_fill, v.window_fill] {
                let ratio = super::contrast(orange, bg);
                assert!(ratio >= 4.5, "{look}: simultaneous input {ratio:.2}");
            }
            let ratio = super::contrast(v.hyperlink_color, v.window_fill);
            assert!(ratio >= 4.5, "{look}: links {ratio:.2}");
        }
        let light = soft_light();
        let ratio = super::contrast(light.widgets.inactive.bg_stroke.color, light.window_fill);
        assert!(ratio >= 3.0, "light: text field edges {ratio:.2}");
        for c in TabColor::ALL.into_iter().map(crate::menu::color32) {
            let ratio = super::contrast(crate::dock::on_color(c), c);
            assert!(ratio >= 4.5, "tab label chip on {c:?}: {ratio:.2}");
        }
    }
}
