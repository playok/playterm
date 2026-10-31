//! Loads Windows system fonts for the terminal: Consolas for Latin, Malgun Gothic for Hangul.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

use crate::view::bold_family;

fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(windir).join("Fonts")
}

/// Memory-maps a font file instead of copying it to the heap: Malgun Gothic alone is ~12 MB
/// per weight, and mapped pages are shared with the system and only loaded when used.
fn load(path: &Path) -> std::io::Result<FontData> {
    let file = std::fs::File::open(path)?;
    // SAFETY: the mapping is read-only. Windows refuses to truncate or delete a file while a
    // mapping of it exists, and system fonts are owned by TrustedInstaller, so the bytes
    // cannot change underneath us.
    match unsafe { memmap2::Mmap::map(&file) } {
        Ok(map) => Ok(FontData::from_blob(Arc::new(map), 0)),
        Err(_) => std::fs::read(path).map(FontData::from_owned),
    }
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
        match load(&dir.join(file)) {
            Ok(data) => {
                defs.font_data.insert(name.to_owned(), Arc::new(data));
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
