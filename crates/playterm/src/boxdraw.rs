//! Procedural rendering of box-drawing (U+2500–U+257F) and block elements (U+2580–U+259F).
//!
//! Font glyphs for these rarely fill the whole cell, which leaves gaps between rows in
//! TUI frames (mc, htop, router menus). Drawing them as rectangles keeps lines connected.

use eframe::egui::{Color32, Pos2, Rect, Shape};

/// Line weights toward (up, down, left, right): 0 none, 1 light, 2 heavy, 3 double.
/// Indexed by `c - U+2500`; `None` marks characters left to the font (diagonals).
const LINES: [Option<[u8; 4]>; 128] = {
    const fn l(s: &[u8; 4]) -> Option<[u8; 4]> {
        Some([s[0] - b'0', s[1] - b'0', s[2] - b'0', s[3] - b'0'])
    }
    [
        // 2500 ─ ━ │ ┃ ┄ ┅ ┆ ┇ ┈ ┉ ┊ ┋ ┌ ┍ ┎ ┏
        l(b"0011"), l(b"0022"), l(b"1100"), l(b"2200"), l(b"0011"), l(b"0022"), l(b"1100"), l(b"2200"),
        l(b"0011"), l(b"0022"), l(b"1100"), l(b"2200"), l(b"0101"), l(b"0102"), l(b"0201"), l(b"0202"),
        // 2510 ┐ ┑ ┒ ┓ └ ┕ ┖ ┗ ┘ ┙ ┚ ┛ ├ ┝ ┞ ┟
        l(b"0110"), l(b"0120"), l(b"0210"), l(b"0220"), l(b"1001"), l(b"1002"), l(b"2001"), l(b"2002"),
        l(b"1010"), l(b"1020"), l(b"2010"), l(b"2020"), l(b"1101"), l(b"1102"), l(b"2101"), l(b"1201"),
        // 2520 ┠ ┡ ┢ ┣ ┤ ┥ ┦ ┧ ┨ ┩ ┪ ┫ ┬ ┭ ┮ ┯
        l(b"2201"), l(b"2102"), l(b"1202"), l(b"2202"), l(b"1110"), l(b"1120"), l(b"2110"), l(b"1210"),
        l(b"2210"), l(b"2120"), l(b"1220"), l(b"2220"), l(b"0111"), l(b"0121"), l(b"0112"), l(b"0122"),
        // 2530 ┰ ┱ ┲ ┳ ┴ ┵ ┶ ┷ ┸ ┹ ┺ ┻ ┼ ┽ ┾ ┿
        l(b"0211"), l(b"0221"), l(b"0212"), l(b"0222"), l(b"1011"), l(b"1021"), l(b"1012"), l(b"1022"),
        l(b"2011"), l(b"2021"), l(b"2012"), l(b"2022"), l(b"1111"), l(b"1121"), l(b"1112"), l(b"1122"),
        // 2540 ╀ ╁ ╂ ╃ ╄ ╅ ╆ ╇ ╈ ╉ ╊ ╋ ╌ ╍ ╎ ╏
        l(b"2111"), l(b"1211"), l(b"2211"), l(b"2121"), l(b"2112"), l(b"1221"), l(b"1212"), l(b"2122"),
        l(b"1222"), l(b"2221"), l(b"2212"), l(b"2222"), l(b"0011"), l(b"0022"), l(b"1100"), l(b"2200"),
        // 2550 ═ ║ ╒ ╓ ╔ ╕ ╖ ╗ ╘ ╙ ╚ ╛ ╜ ╝ ╞ ╟
        l(b"0033"), l(b"3300"), l(b"0103"), l(b"0301"), l(b"0303"), l(b"0130"), l(b"0310"), l(b"0330"),
        l(b"1003"), l(b"3001"), l(b"3003"), l(b"1030"), l(b"3010"), l(b"3030"), l(b"1103"), l(b"3301"),
        // 2560 ╠ ╡ ╢ ╣ ╤ ╥ ╦ ╧ ╨ ╩ ╪ ╫ ╬ ╭ ╮ ╯
        l(b"3303"), l(b"1130"), l(b"3310"), l(b"3330"), l(b"0133"), l(b"0311"), l(b"0333"), l(b"1033"),
        l(b"3011"), l(b"3033"), l(b"1133"), l(b"3311"), l(b"3333"), l(b"0101"), l(b"0110"), l(b"1010"),
        // 2570 ╰ ╱ ╲ ╳ ╴ ╵ ╶ ╷ ╸ ╹ ╺ ╻ ╼ ╽ ╾ ╿
        l(b"1001"), None, None, None, l(b"0010"), l(b"1000"), l(b"0001"), l(b"0100"),
        l(b"0020"), l(b"2000"), l(b"0002"), l(b"0200"), l(b"0012"), l(b"1200"), l(b"0021"), l(b"2100"),
    ]
};

struct Pen {
    ppp: f32,
    color: Color32,
}

impl Pen {
    fn snap(&self, v: f32) -> f32 {
        (v * self.ppp).round() / self.ppp
    }

    fn rect(&self, out: &mut Vec<Shape>, x0: f32, y0: f32, x1: f32, y1: f32) {
        let r = Rect::from_min_max(
            Pos2::new(self.snap(x0), self.snap(y0)),
            Pos2::new(self.snap(x1), self.snap(y1)),
        );
        out.push(Shape::rect_filled(r, 0.0, self.color));
    }
}

/// Draws `c` into `cell` if it is a supported box/block character. Returns `false` otherwise.
pub fn draw(c: char, cell: Rect, color: Color32, pixels_per_point: f32, out: &mut Vec<Shape>) -> bool {
    let code = c as u32;
    let pen = Pen { ppp: pixels_per_point, color };
    match code {
        0x2500..=0x257F => match LINES[(code - 0x2500) as usize] {
            Some(weights) => {
                lines(&pen, cell, weights, out);
                true
            }
            None => false,
        },
        0x2580..=0x259F => blocks(&pen, cell, code, out),
        _ => false,
    }
}

fn lines(pen: &Pen, cell: Rect, [up, down, left, right]: [u8; 4], out: &mut Vec<Shape>) {
    // One physical pixel minimum; scales with the cell for large fonts.
    let light = (cell.width() / 8.0).round().max(1.0 / pen.ppp).max(1.0);
    let thickness = |w: u8| if w == 2 { light * 2.0 } else { light };
    let gap = light; // half distance between the two strokes of a double line
    let c = cell.center();
    let (cx, cy) = (pen.snap(c.x), pen.snap(c.y));

    // How far a segment must reach past the center to join the perpendicular strokes.
    let reach = |a: u8, b: u8| -> f32 {
        let w = a.max(b);
        match w {
            0 => 0.0,
            3 => gap + light / 2.0,
            _ => thickness(w) / 2.0,
        }
    };
    let h_reach = reach(up, down);
    let v_reach = reach(left, right);

    let mut horizontal = |w: u8, x0: f32, x1: f32| match w {
        0 => {}
        3 => {
            for dy in [-gap, gap] {
                pen.rect(out, x0, cy + dy - light / 2.0, x1, cy + dy + light / 2.0);
            }
        }
        _ => {
            let t = thickness(w);
            pen.rect(out, x0, cy - t / 2.0, x1, cy + t / 2.0);
        }
    };
    horizontal(left, cell.left(), cx + h_reach);
    horizontal(right, cx - h_reach, cell.right());

    let mut vertical = |w: u8, y0: f32, y1: f32| match w {
        0 => {}
        3 => {
            for dx in [-gap, gap] {
                pen.rect(out, cx + dx - light / 2.0, y0, cx + dx + light / 2.0, y1);
            }
        }
        _ => {
            let t = thickness(w);
            pen.rect(out, cx - t / 2.0, y0, cx + t / 2.0, y1);
        }
    };
    vertical(up, cell.top(), cy + v_reach);
    vertical(down, cy - v_reach, cell.bottom());
}

fn blocks(pen: &Pen, cell: Rect, code: u32, out: &mut Vec<Shape>) -> bool {
    let (x0, y0, w, h) = (cell.left(), cell.top(), cell.width(), cell.height());
    // Fractions of the cell: (left, top, right, bottom).
    let mut part = |l: f32, t: f32, r: f32, b: f32| pen.rect(out, x0 + w * l, y0 + h * t, x0 + w * r, y0 + h * b);
    match code {
        0x2580 => part(0.0, 0.0, 1.0, 0.5),
        0x2581..=0x2588 => {
            let eighths = (code - 0x2580) as f32;
            part(0.0, 1.0 - eighths / 8.0, 1.0, 1.0);
        }
        0x2589..=0x258F => {
            let eighths = (0x2590 - code) as f32;
            part(0.0, 0.0, eighths / 8.0, 1.0);
        }
        0x2590 => part(0.5, 0.0, 1.0, 1.0),
        0x2591..=0x2593 => {
            let alpha = [0.25, 0.5, 0.75][(code - 0x2591) as usize];
            out.push(Shape::rect_filled(cell, 0.0, pen.color.gamma_multiply(alpha)));
        }
        0x2594 => part(0.0, 0.0, 1.0, 0.125),
        0x2595 => part(0.875, 0.0, 1.0, 1.0),
        0x2596..=0x259F => {
            // Quadrant bits: upper-left, upper-right, lower-left, lower-right.
            const QUADS: [u8; 10] = [0b0010, 0b0001, 0b1000, 0b1011, 0b1001, 0b1110, 0b1101, 0b0100, 0b0110, 0b0111];
            let q = QUADS[(code - 0x2596) as usize];
            if q & 0b1000 != 0 {
                part(0.0, 0.0, 0.5, 0.5);
            }
            if q & 0b0100 != 0 {
                part(0.5, 0.0, 1.0, 0.5);
            }
            if q & 0b0010 != 0 {
                part(0.0, 0.5, 0.5, 1.0);
            }
            if q & 0b0001 != 0 {
                part(0.5, 0.5, 1.0, 1.0);
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> Rect {
        Rect::from_min_size(Pos2::ZERO, eframe::egui::vec2(8.0, 16.0))
    }

    #[test]
    fn table_spot_checks() {
        let w = |c: char| LINES[(c as u32 - 0x2500) as usize];
        assert_eq!(w('─'), Some([0, 0, 1, 1]));
        assert_eq!(w('┌'), Some([0, 1, 0, 1]));
        assert_eq!(w('┼'), Some([1, 1, 1, 1]));
        assert_eq!(w('╋'), Some([2, 2, 2, 2]));
        assert_eq!(w('╔'), Some([0, 3, 0, 3]));
        assert_eq!(w('╯'), Some([1, 0, 1, 0]));
        assert_eq!(w('╳'), None);
    }

    #[test]
    fn vertical_line_spans_full_cell_height() {
        let mut out = Vec::new();
        assert!(draw('│', cell(), Color32::WHITE, 1.0, &mut out));
        let rects: Vec<Rect> = out
            .iter()
            .map(|s| match s {
                Shape::Rect(r) => r.rect,
                _ => panic!("expected rect"),
            })
            .collect();
        // Up and down halves meet at the center and together cover the whole cell.
        assert_eq!(rects.len(), 2);
        assert_eq!((rects[0].top(), rects[0].bottom()), (0.0, 8.0));
        assert_eq!((rects[1].top(), rects[1].bottom()), (8.0, 16.0));
    }

    #[test]
    fn blocks_and_unsupported() {
        let mut out = Vec::new();
        assert!(draw('█', cell(), Color32::WHITE, 1.0, &mut out));
        assert!(draw('▚', cell(), Color32::WHITE, 1.0, &mut out));
        assert!(!draw('A', cell(), Color32::WHITE, 1.0, &mut out));
        assert!(!draw('╱', cell(), Color32::WHITE, 1.0, &mut out));
    }
}
