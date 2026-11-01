//! Help → Open-source licenses: playterm's own license and those of the open-source crates built
//! into it. The crate list and license texts are collected by `build.rs` from `cargo metadata`,
//! so they always match what the executable was built from.

use eframe::egui::{self, RichText};
use playterm_i18n::{t, tr};

/// An open-source crate built into the program.
pub struct Component {
    pub name: &'static str,
    pub version: &'static str,
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
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.heading(t("오픈소스 라이선스", "Open-source licenses"));
                ui.add_space(6.0);
                ui.label(t(
                    "playterm 은 MIT 라이선스로 배포되는 오픈소스 소프트웨어입니다.",
                    "playterm is open-source software released under the MIT License.",
                ));
                egui::CollapsingHeader::new(t("playterm 라이선스 (MIT)", "playterm license (MIT)"))
                    .id_salt("own-license")
                    .show(ui, |ui| license_text(ui, OWN_LICENSE));
                ui.add_space(8.0);
                ui.label(tr!(
                    "이 프로그램에는 아래 오픈소스 구성 요소 {}개가 포함되어 있으며, 각각 표시된 라이선스를 따릅니다. 항목을 펼치면 라이선스 전문과 소스 저장소가 보입니다.",
                    "This program includes the {} open-source components below, each under the license shown. Expand one to see its full license text and source repository.",
                    COMPONENTS.len()
                ));
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text(t("이름 또는 라이선스로 찾기", "Search by name or license"))
                        .desired_width(WIDTH),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, false]).show(ui, |ui| {
                    for c in COMPONENTS.iter().filter(|c| matches(c, &self.filter)) {
                        let title = format!("{} {}  —  {}", c.name, c.version, shorten(c.license, 48));
                        egui::CollapsingHeader::new(title).id_salt(("component", c.name, c.version)).show(ui, |ui| {
                            ui.add(egui::Label::new(tr!("라이선스: {}", "License: {}", c.license)).wrap());
                            ui.hyperlink(c.url);
                            if c.texts.is_empty() {
                                ui.weak(t(
                                    "라이선스 전문은 위 저장소를 참고하세요.",
                                    "See the repository above for the full license text.",
                                ));
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
                done = ui.button(t("닫기", "Close")).clicked();
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
        let c = Component { name: "serialport", version: "4", license: "MPL-2.0", url: "", texts: &[] };
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
