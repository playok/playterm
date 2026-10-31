use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

const fn rgb(hex: u32) -> Rgb {
    Rgb { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8 }
}

/// Default 16 ANSI colors (dark theme, close to Windows Terminal "Campbell").
const ANSI: [Rgb; 16] = [
    rgb(0x0C0C0C),
    rgb(0xC50F1F),
    rgb(0x13A10E),
    rgb(0xC19C00),
    rgb(0x0037DA),
    rgb(0x881798),
    rgb(0x3A96DD),
    rgb(0xCCCCCC),
    rgb(0x767676),
    rgb(0xE74856),
    rgb(0x16C60C),
    rgb(0xF9F1A5),
    rgb(0x3B78FF),
    rgb(0xB4009E),
    rgb(0x61D6D6),
    rgb(0xF2F2F2),
];

/// Full color table indexed like `alacritty_terminal::term::color::Colors`:
/// 0..256 xterm palette, then the named special colors.
#[derive(Clone, Debug)]
pub struct Palette {
    table: [Rgb; COUNT],
}

impl Default for Palette {
    fn default() -> Self {
        let mut table = [Rgb::default(); COUNT];
        table[..16].copy_from_slice(&ANSI);

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

        let fg = rgb(0xCCCCCC);
        table[NamedColor::Foreground as usize] = fg;
        table[NamedColor::Background as usize] = rgb(0x0C0C0C);
        table[NamedColor::Cursor as usize] = rgb(0xE0E0E0);
        table[NamedColor::BrightForeground as usize] = rgb(0xF2F2F2);
        table[NamedColor::DimForeground as usize] = dim(fg);
        for i in 0..8 {
            table[NamedColor::DimBlack as usize + i] = dim(ANSI[i]);
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
    fn bold_brightens_and_inverse_swaps() {
        let p = Palette::default();
        let none = Colors::default();
        let mut cell = Cell { fg: Color::Named(NamedColor::Red), ..Cell::default() };
        cell.flags = Flags::BOLD;
        assert_eq!(p.cell_colors(&cell, &none).0, ANSI[9]);

        cell.flags = Flags::INVERSE;
        let (fg, bg) = p.cell_colors(&cell, &none);
        assert_eq!(fg, p.background(&none));
        assert_eq!(bg, ANSI[1]);
    }
}
