use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

const fn rgb(hex: u32) -> Rgb {
    Rgb { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8 }
}

/// A color scheme: the 16 ANSI colors plus the default text, background, cursor and
/// selection colors (as `0xRRGGBB`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub ansi: [u32; 16],
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub selection: u32,
}

impl Theme {
    /// The default scheme (close to Windows Terminal "Campbell").
    pub const DEFAULT: &'static Theme = &THEMES[0];

    /// A built-in scheme by name, or the default one.
    pub fn named(name: &str) -> &'static Theme {
        THEMES.iter().find(|t| t.name.eq_ignore_ascii_case(name)).unwrap_or(Theme::DEFAULT)
    }

    pub fn rgb(hex: u32) -> Rgb {
        rgb(hex)
    }
}

/// Built-in color schemes (values as in Windows Terminal and the schemes' own projects).
pub const THEMES: &[Theme] = &[
    Theme {
        name: "Campbell",
        ansi: [
            0x0C0C0C, 0xC50F1F, 0x13A10E, 0xC19C00, 0x0037DA, 0x881798, 0x3A96DD, 0xCCCCCC, //
            0x767676, 0xE74856, 0x16C60C, 0xF9F1A5, 0x3B78FF, 0xB4009E, 0x61D6D6, 0xF2F2F2,
        ],
        foreground: 0xCCCCCC,
        background: 0x0C0C0C,
        cursor: 0xE0E0E0,
        selection: 0x264F78,
    },
    Theme {
        name: "One Half Dark",
        ansi: [
            0x282C34, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xDCDFE4, //
            0x5A6374, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xDCDFE4,
        ],
        foreground: 0xDCDFE4,
        background: 0x282C34,
        cursor: 0xDCDFE4,
        selection: 0x3E4451,
    },
    Theme {
        name: "One Half Light",
        ansi: [
            0x383A42, 0xE45649, 0x50A14F, 0xC18301, 0x0184BC, 0xA626A4, 0x0997B3, 0xFAFAFA, //
            0x4F525D, 0xDF6C75, 0x98C379, 0xE4C07A, 0x61AFEF, 0xC577DD, 0x56B5C1, 0xFFFFFF,
        ],
        foreground: 0x383A42,
        background: 0xFAFAFA,
        cursor: 0x4F525D,
        selection: 0xBFCEFF,
    },
    Theme {
        name: "Solarized Dark",
        ansi: [
            0x073642, 0xDC322F, 0x859900, 0xB58900, 0x268BD2, 0xD33682, 0x2AA198, 0xEEE8D5, //
            0x002B36, 0xCB4B16, 0x586E75, 0x657B83, 0x839496, 0x6C71C4, 0x93A1A1, 0xFDF6E3,
        ],
        foreground: 0x839496,
        background: 0x002B36,
        cursor: 0x93A1A1,
        selection: 0x274B57,
    },
    Theme {
        name: "Solarized Light",
        ansi: [
            0x073642, 0xDC322F, 0x859900, 0xB58900, 0x268BD2, 0xD33682, 0x2AA198, 0xEEE8D5, //
            0x002B36, 0xCB4B16, 0x586E75, 0x657B83, 0x839496, 0x6C71C4, 0x93A1A1, 0xFDF6E3,
        ],
        foreground: 0x657B83,
        background: 0xFDF6E3,
        cursor: 0x586E75,
        selection: 0xE4DDC8,
    },
    Theme {
        name: "Tango Dark",
        ansi: [
            0x000000, 0xCC0000, 0x4E9A06, 0xC4A000, 0x3465A4, 0x75507B, 0x06989A, 0xD3D7CF, //
            0x555753, 0xEF2929, 0x8AE234, 0xFCE94F, 0x729FCF, 0xAD7FA8, 0x34E2E2, 0xEEEEEC,
        ],
        foreground: 0xD3D7CF,
        background: 0x000000,
        cursor: 0xD3D7CF,
        selection: 0x2E4B70,
    },
    Theme {
        name: "Dracula",
        ansi: [
            0x21222C, 0xFF5555, 0x50FA7B, 0xF1FA8C, 0xBD93F9, 0xFF79C6, 0x8BE9FD, 0xF8F8F2, //
            0x6272A4, 0xFF6E6E, 0x69FF94, 0xFFFFA5, 0xD6ACFF, 0xFF92DF, 0xA4FFFF, 0xFFFFFF,
        ],
        foreground: 0xF8F8F2,
        background: 0x282A36,
        cursor: 0xF8F8F2,
        selection: 0x44475A,
    },
    Theme {
        name: "Gruvbox Dark",
        ansi: [
            0x282828, 0xCC241D, 0x98971A, 0xD79921, 0x458588, 0xB16286, 0x689D6A, 0xA89984, //
            0x928374, 0xFB4934, 0xB8BB26, 0xFABD2F, 0x83A598, 0xD3869B, 0x8EC07C, 0xEBDBB2,
        ],
        foreground: 0xEBDBB2,
        background: 0x282828,
        cursor: 0xEBDBB2,
        selection: 0x504945,
    },
];

/// Full color table indexed like `alacritty_terminal::term::color::Colors`:
/// 0..256 xterm palette, then the named special colors.
#[derive(Clone, Debug)]
pub struct Palette {
    table: [Rgb; COUNT],
}

impl Default for Palette {
    fn default() -> Self {
        Self::from_theme(Theme::DEFAULT)
    }
}

impl Palette {
    /// The xterm 256-color table with `theme`'s 16 ANSI colors and default colors.
    pub fn from_theme(theme: &Theme) -> Self {
        let mut table = [Rgb::default(); COUNT];
        for (slot, hex) in table.iter_mut().zip(theme.ansi) {
            *slot = rgb(hex);
        }

        // 6x6x6 color cube.
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        for i in 0..216 {
            table[16 + i] = Rgb { r: STEPS[i / 36], g: STEPS[(i / 6) % 6], b: STEPS[i % 6] };
        }
        // Grayscale ramp.
        for i in 0..24 {
            let v = 8 + 10 * i as u8;
            table[232 + i] = Rgb { r: v, g: v, b: v };
        }

        let fg = rgb(theme.foreground);
        table[NamedColor::Foreground as usize] = fg;
        table[NamedColor::Background as usize] = rgb(theme.background);
        table[NamedColor::Cursor as usize] = rgb(theme.cursor);
        table[NamedColor::BrightForeground as usize] = fg;
        table[NamedColor::DimForeground as usize] = dim(fg);
        for i in 0..8 {
            table[NamedColor::DimBlack as usize + i] = dim(rgb(theme.ansi[i]));
        }

        Self { table }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb { r: (c.r as u16 * 2 / 3) as u8, g: (c.g as u16 * 2 / 3) as u8, b: (c.b as u16 * 2 / 3) as u8 }
}

impl Palette {
    /// Looks up a palette index, honoring runtime overrides set by the remote (OSC 4/10/11).
    pub fn index(&self, idx: usize, overrides: &Colors) -> Rgb {
        overrides[idx].unwrap_or(self.table[idx])
    }

    pub fn foreground(&self, overrides: &Colors) -> Rgb {
        self.index(NamedColor::Foreground as usize, overrides)
    }

    pub fn background(&self, overrides: &Colors) -> Rgb {
        self.index(NamedColor::Background as usize, overrides)
    }

    pub fn cursor(&self, overrides: &Colors) -> Rgb {
        self.index(NamedColor::Cursor as usize, overrides)
    }

    fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(i) => self.index(i as usize, overrides),
            Color::Named(n) => self.index(n as usize, overrides),
        }
    }

    /// Effective (foreground, background) of a cell after bold/dim/inverse/hidden.
    pub fn cell_colors(&self, cell: &Cell, overrides: &Colors) -> (Rgb, Rgb) {
        let flags = cell.flags;
        let fg_color = match cell.fg {
            Color::Named(n) if flags.contains(Flags::DIM) => Color::Named(n.to_dim()),
            Color::Named(n) if flags.contains(Flags::BOLD) && (n as usize) < 8 => {
                Color::Named(n.to_bright())
            }
            Color::Indexed(i) if flags.contains(Flags::BOLD) && i < 8 => Color::Indexed(i + 8),
            c => c,
        };
        let mut fg = self.resolve(fg_color, overrides);
        if flags.contains(Flags::DIM) && !matches!(fg_color, Color::Named(_)) {
            fg = dim(fg);
        }
        let mut bg = self.resolve(cell.bg, overrides);

        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        (fg, bg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_cube_and_gray() {
        let p = Palette::default();
        let none = Colors::default();
        assert_eq!(p.index(196, &none), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(p.index(232, &none), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(p.index(255, &none), Rgb { r: 238, g: 238, b: 238 });
    }

    #[test]
    fn themes() {
        let none = Colors::default();
        let light = Palette::from_theme(Theme::named("one half light"));
        assert_eq!(light.background(&none), Rgb { r: 0xFA, g: 0xFA, b: 0xFA });
        assert_eq!(light.index(1, &none), Rgb { r: 0xE4, g: 0x56, b: 0x49 });
        // The 256-color cube is the same for every scheme.
        assert_eq!(light.index(196, &none), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(Theme::named("no such theme").name, "Campbell");
        let names: std::collections::HashSet<_> = THEMES.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), THEMES.len(), "theme names are unique");
    }

    #[test]
    fn bold_brightens_and_inverse_swaps() {
        let p = Palette::default();
        let none = Colors::default();
        let mut cell = Cell { fg: Color::Named(NamedColor::Red), ..Cell::default() };
        cell.flags = Flags::BOLD;
        assert_eq!(p.cell_colors(&cell, &none).0, rgb(Theme::DEFAULT.ansi[9]));

        cell.flags = Flags::INVERSE;
        let (fg, bg) = p.cell_colors(&cell, &none);
        assert_eq!(fg, p.background(&none));
        assert_eq!(bg, rgb(Theme::DEFAULT.ansi[1]));
    }
}
