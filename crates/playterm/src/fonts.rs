//! Loads Windows system fonts for the terminal: Consolas for Latin, Malgun Gothic for Hangul.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

use crate::view::bold_family;

fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(windir).join("Fonts")
}

pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let dir = fonts_dir();

    // (name, file, bold)
    let candidates = [
        ("consolas", "consola.ttf", false),
        ("malgun", "malgun.ttf", false),
        ("consolas-bold", "consolab.ttf", true),
        ("malgun-bold", "malgunbd.ttf", true),
    ];

    let mut regular = Vec::new();
    let mut bold = Vec::new();
    for (name, file, is_bold) in candidates {
        match std::fs::read(dir.join(file)) {
            Ok(bytes) => {
                defs.font_data.insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
                if is_bold { &mut bold } else { &mut regular }.push(name.to_owned());
            }
            Err(e) => log::warn!("font {file} not available: {e}"),
        }
    }

    // egui's built-in monospace fonts stay as the last fallback (symbols, emoji).
    let builtin = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let mono: Vec<String> = regular.iter().cloned().chain(builtin).collect();
    let mono_bold: Vec<String> = bold.into_iter().chain(mono.iter().cloned()).collect();

    defs.families.insert(FontFamily::Monospace, mono);
    defs.families.insert(bold_family(), mono_bold);
    ctx.set_fonts(defs);
}
