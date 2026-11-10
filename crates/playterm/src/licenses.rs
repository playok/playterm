//! Help → Open-source licenses: playterm's own license and those of the open-source crates built
//! into it. The crate list and license texts are collected by `build.rs` from `cargo metadata`,
//! so they always match what the executable was built from.

use eframe::egui::{self, RichText};
use playterm_i18n::{t, tr};

use crate::icon;

/// An open-source crate built into the program. Crates linked in several versions (each used
/// by different crates) are one component with all of them.
pub struct Component {
    pub name: &'static str,
    /// Oldest first.
    pub versions: &'static [&'static str],
    /// SPDX license expression, e.g. "MIT OR Apache-2.0".
    pub license: &'static str,
    pub url: &'static str,
    /// Its license texts, as indexes into [`LICENSE_TEXTS`].
    pub texts: &'static [usize],
}

include!(concat!(env!("OUT_DIR"), "/licenses.rs"));

/// playterm's own license.
pub const OWN_LICENSE: &str = include_str!("../../../LICENSE");

/// Whether `c` matches the search `filter` (name or license, ignoring case).
fn matches(c: &Component, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    filter.is_empty() || c.name.to_lowercase().contains(&filter) || c.license.to_lowercase().contains(&filter)
}

/// `text` cut to `max` characters with "…": a header label does not wrap, and a long license
/// expression (ring's lists seven licenses) would widen the dialog to the whole window.
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max - 1).collect();
    short.push('…');
    short
}

/// A license text, small and monospaced, wrapped to the width.
fn license_text(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).monospace().size(11.0)).wrap());
}

/// The width of the dialog's contents. Everything in it is bounded by this: an unbounded
/// widget would grow the modal to the whole window, which then keeps that size.
const WIDTH: f32 = 640.0;

#[derive(Default)]
pub struct LicensesDialog {
    filter: String,
}

impl LicensesDialog {
    /// Shows the dialog. Returns `true` once it is closed.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut done = false;
        let close = egui::Modal::new(egui::Id::unique("licenses"))
            .backdrop_color(crate::app::theme::MODAL_BACKDROP)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.heading(t!("common.open_source_licenses"));
                ui.add_space(6.0);
                ui.label(t!("licenses.playterm_is_open_source_software_released_under_the"));
                egui::CollapsingHeader::new(t!("licenses.playterm_license_mit"))
                    .id_salt("own-license")
                    .show(ui, |ui| license_text(ui, OWN_LICENSE));
                ui.add_space(8.0);
                ui.label(tr!("licenses.this_program_includes_the_open_source_components_below", arg0 = COMPONENTS.len()));
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter).hint_text(t!("licenses.search_by_name_or_license")).desired_width(WIDTH),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, false]).show(ui, |ui| {
                    for c in COMPONENTS.iter().filter(|c| matches(c, &self.filter)) {
                        let title = format!("{} {}  —  {}", c.name, c.versions.join(" · "), shorten(c.license, 48));
                        egui::CollapsingHeader::new(title).id_salt(("component", c.name)).show(ui, |ui| {
                            ui.add(egui::Label::new(tr!("licenses.license", arg0 = c.license)).wrap());
                            ui.hyperlink(c.url);
                            if c.texts.is_empty() {
                                ui.weak(t!("licenses.see_the_repository_above_for_the_full_license"));
                            }
                            for (i, &text) in c.texts.iter().enumerate() {
                                if i > 0 {
                                    ui.separator();
                                }
                                license_text(ui, LICENSE_TEXTS[text]);
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                done = ui.add(icon::button(icon::CLOSE, t!("common.close"))).clicked();
            })
            .should_close();
        done || close
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_are_listed_with_licenses() {
        assert!(COMPONENTS.len() > 100, "the dependency tree was collected");
        for name in ["egui", "eframe", "alacritty_terminal", "russh", "tokio", "ironrdp-session"] {
            assert!(COMPONENTS.iter().any(|c| c.name == name), "{name} is listed");
        }
        assert!(!COMPONENTS.iter().any(|c| c.name.starts_with("playterm")), "our own crates are not listed");
        let without: Vec<&str> = COMPONENTS.iter().filter(|c| c.texts.is_empty()).map(|c| c.name).collect();
        assert!(without.is_empty(), "every component has its license text: {without:?}");
        // Procedural macros run in the compiler only.
        for name in ["syn", "serde_derive", "thiserror-impl"] {
            assert!(!COMPONENTS.iter().any(|c| c.name == name), "{name} is not listed");
        }
        // A crate linked in two versions is listed once, with both.
        let names: std::collections::BTreeSet<&str> = COMPONENTS.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), COMPONENTS.len(), "each name once");
        let base64 = COMPONENTS.iter().find(|c| c.name == "base64").unwrap();
        assert!(base64.versions.len() >= 2, "{:?}", base64.versions);
        for c in COMPONENTS {
            assert!(!c.license.is_empty(), "{} has a license", c.name);
            assert!(c.texts.iter().all(|&i| i < LICENSE_TEXTS.len()));
        }
        // Crates without a license file of their own get the standard text.
        let russh = COMPONENTS.iter().find(|c| c.name == "russh").unwrap();
        assert!(!russh.texts.is_empty());
        assert!(OWN_LICENSE.starts_with("MIT License"));
    }

    #[test]
    fn searching() {
        let c = Component { name: "serialport", versions: &["4"], license: "MPL-2.0", url: "", texts: &[] };
        assert!(matches(&c, ""));
        assert!(matches(&c, "Serial"));
        assert!(matches(&c, "mpl"));
        assert!(!matches(&c, "mit"));
    }

    #[test]
    fn long_licenses_are_shortened_in_titles() {
        assert_eq!(shorten("MIT", 5), "MIT");
        assert_eq!(shorten("Apache-2.0 OR MIT", 8), "Apache-…");
        assert!(COMPONENTS.iter().all(|c| shorten(c.license, 48).chars().count() <= 48));
    }
}
