//! Reading the system clipboard on demand (menu "Paste", Ctrl+Shift+V).
//!
//! Writing goes through egui (`Context::copy_text`); egui only delivers clipboard
//! contents for its own Ctrl+V shortcut, so explicit reads use arboard directly.

pub fn read_text() -> Option<String> {
    let text = arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|e| log::warn!("clipboard read failed: {e}"))
        .ok()?;
    (!text.is_empty()).then(|| text.replace("\r\n", "\n"))
}
