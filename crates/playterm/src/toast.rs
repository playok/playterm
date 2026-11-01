//! A short notice that slides up from the bottom of the window and fades away, e.g.
//! `복사됨 "abc…xyz" · 128자` after copying terminal text, so a wrong selection is noticed.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Align, Align2};
use playterm_i18n::tr;

/// How long a notice stays, from appearing to gone, in seconds.
const SHOW_SECS: f64 = 1.8;
/// Sliding up at the start.
const SLIDE_SECS: f64 = 0.2;
/// Fading out at the end.
const FADE_SECS: f64 = 0.45;
/// Distance slid, in points.
const RISE: f32 = 24.0;
/// Distance from the bottom of the window (above the status bar and command input bar).
const BOTTOM: f32 = 64.0;
/// Characters shown from each end of the copied text.
const ENDS: usize = 3;

#[derive(Clone)]
struct Toast {
    what: String,
    snippet: String,
    since: f64,
}

fn id() -> egui::Id {
    egui::Id::unique("toast")
}

/// The first and last few characters of `text` around "…" (all of it when short), with line
/// breaks and tabs made visible.
pub fn snippet(text: &str) -> String {
    let visible = |c: char| match c {
        '\n' => '⏎',
        '\t' => '→',
        c if c.is_control() => '·',
        c => c,
    };
    let chars: Vec<char> = text.chars().filter(|&c| c != '\r').map(visible).collect();
    if chars.len() <= 2 * ENDS + 1 {
        return chars.into_iter().collect();
    }
    let head: String = chars[..ENDS].iter().collect();
    let tail: String = chars[chars.len() - ENDS..].iter().collect();
    format!("{head}…{tail}")
}

/// Shows that `text` was copied.
pub fn copied(ctx: &egui::Context, text: &str) {
    let chars = text.chars().filter(|&c| c != '\r').count();
    let lines = text.lines().count();
    let what = if lines > 1 {
        tr!("복사됨 · {}자 · {}줄", "Copied · {} chars · {} lines", chars, lines)
    } else {
        tr!("복사됨 · {}자", "Copied · {} chars", chars)
    };
    let toast = Toast { what, snippet: snippet(text), since: ctx.input(|i| i.time) };
    ctx.data_mut(|d| d.insert_temp(id(), toast));
    ctx.request_repaint();
}

/// Where a notice of age `age` seconds is: (points below its resting place, opacity).
fn placement(age: f64) -> (f32, f32) {
    let slide = (age / SLIDE_SECS).clamp(0.0, 1.0) as f32;
    // Ease out: fast at first, settling at the top.
    let rise = 1.0 - (1.0 - slide).powi(3);
    let fade_in = slide;
    let fade_out = ((SHOW_SECS - age) / FADE_SECS).clamp(0.0, 1.0) as f32;
    (RISE * (1.0 - rise), fade_in.min(fade_out))
}

/// Draws the current notice, if any, over everything else.
pub fn show(ctx: &egui::Context) {
    let Some(toast) = ctx.data(|d| d.get_temp::<Toast>(id())) else { return };
    let age = ctx.input(|i| i.time) - toast.since;
    if !(0.0..SHOW_SECS).contains(&age) {
        ctx.data_mut(|d| d.remove::<Toast>(id()));
        return;
    }
    let (below, opacity) = placement(age);
    egui::Area::new(id().with("area"))
        .anchor(Align2::CENTER_BOTTOM, [0.0, -BOTTOM + below])
        .order(egui::Order::Tooltip)
        .interactable(false)
        .show(ctx, |ui| {
            ui.multiply_opacity(opacity);
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                // One line in one font: separate labels (or the terminal's monospace font for
                // the snippet) sit on different baselines, and the text looks off-center.
                let font = egui::TextStyle::Body.resolve(ui.style());
                let format = |color| TextFormat { font_id: font.clone(), color, valign: Align::Center, ..Default::default() };
                let mut line = LayoutJob::default();
                line.append(&toast.what, 0.0, format(ui.visuals().strong_text_color()));
                line.append(&format!("“{}”", toast.snippet), 12.0, format(ui.visuals().hyperlink_color));
                ui.add(egui::Label::new(line).wrap_mode(egui::TextWrapMode::Extend));
            });
        });
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_show_both_ends() {
        assert_eq!(snippet("hello"), "hello");
        assert_eq!(snippet("abcdefg"), "abcdefg", "short text is shown whole");
        assert_eq!(snippet("abcdefgh"), "abc…fgh");
        assert_eq!(snippet("첫 줄입니다\r\n둘째 줄"), "첫 줄…째 줄");
        assert_eq!(snippet("ab\ncd\te"), "ab⏎cd→e", "line breaks and tabs are visible");
        assert_eq!(snippet("line one\n"), "lin…ne⏎", "a trailing line break shows");
    }

    #[test]
    fn slides_up_then_fades() {
        let (below, opacity) = placement(0.0);
        assert_eq!((below, opacity), (RISE, 0.0), "starts below, invisible");
        let (below, opacity) = placement(SLIDE_SECS);
        assert_eq!((below, opacity), (0.0, 1.0), "then rests, fully visible");
        let (_, opacity) = placement(SHOW_SECS - FADE_SECS / 2.0);
        assert!(opacity > 0.4 && opacity < 0.6, "fading: {opacity}");
        assert_eq!(placement(SHOW_SECS).1, 0.0);
    }
}
