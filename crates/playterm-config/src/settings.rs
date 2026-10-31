//! Application preferences (`settings.toml`): terminal font, size, colors and scrollback.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fsutil::write_atomic;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// A font name from the built-in list (e.g. `Consolas`, `D2Coding`) or a font file path.
    pub font: String,
    /// Terminal font size in points.
    pub font_size: f32,
    /// Color scheme name (see `playterm_term::THEMES`).
    pub theme: String,
    /// Lines of history kept per tab.
    pub scrollback: usize,
}

impl AppSettings {
    pub const MIN_FONT_SIZE: f32 = 8.0;
    pub const MAX_FONT_SIZE: f32 = 40.0;
    pub const DEFAULT_FONT_SIZE: f32 = 15.0;
    pub const MIN_SCROLLBACK: usize = 100;
    pub const MAX_SCROLLBACK: usize = 200_000;

    /// Loads `path`; a missing file gives the defaults.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Self>(&text)
                .map(Self::clamped)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(path, text.as_bytes())
    }

    /// Values brought into their valid ranges (hand-edited files).
    pub fn clamped(mut self) -> Self {
        self.font_size = if self.font_size.is_finite() {
            self.font_size.clamp(Self::MIN_FONT_SIZE, Self::MAX_FONT_SIZE)
        } else {
            Self::DEFAULT_FONT_SIZE
        };
        self.scrollback = self.scrollback.clamp(Self::MIN_SCROLLBACK, Self::MAX_SCROLLBACK);
        self
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            font: "Consolas".into(),
            font_size: Self::DEFAULT_FONT_SIZE,
            theme: "Campbell".into(),
            scrollback: 10_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        assert_eq!(AppSettings::load(&path).unwrap(), AppSettings::default());

        let s = AppSettings { font: "D2Coding".into(), font_size: 18.5, theme: "Dracula".into(), scrollback: 5000 };
        s.save(&path).unwrap();
        assert_eq!(AppSettings::load(&path).unwrap(), s);

        // Missing keys take defaults; out-of-range values are clamped.
        std::fs::write(&path, "font_size = 99\nscrollback = 1\n").unwrap();
        let s = AppSettings::load(&path).unwrap();
        assert_eq!((s.font.as_str(), s.font_size, s.scrollback), ("Consolas", 40.0, 100));

        std::fs::write(&path, "font_size = \"big\"").unwrap();
        assert!(AppSettings::load(&path).is_err());
    }
}
