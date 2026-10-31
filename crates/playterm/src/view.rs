//! Terminal widget: paints the grid and turns keyboard/IME/wheel input into session input.

use eframe::egui::{
    self, Color32, Event, EventFilter, FontFamily, FontId, ImeEvent, Pos2, Rect, Sense,
    Shape, Stroke, Vec2,
};
use playterm_term::{Cell, CursorShape, Flags, Rgb, TermMode};

use crate::boxdraw;
use crate::input::{key_bytes, paste_bytes};
use crate::session::Session;

pub const FONT_SIZE: f32 = 15.0;

pub fn bold_family() -> FontFamily {
    FontFamily::Name("mono-bold".into())
}

#[derive(Clone, Copy)]
struct CellSize {
    w: f32,
    h: f32,
}

fn cell_size(ctx: &egui::Context) -> CellSize {
    let font = FontId::monospace(FONT_SIZE);
    ctx.fonts_mut(|f| CellSize { w: f.glyph_width(&font, 'M'), h: f.row_height(&font) })
}

fn c32(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

/// Shows the terminal of `session` in all remaining space of `ui`.
///
/// `autofocus` grabs keyboard focus when nothing else has it (false while a dialog is open).
pub fn show(ui: &mut egui::Ui, session: &mut Session, autofocus: bool) {
    let cell = cell_size(ui.ctx());
    let rect = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(rect, Sense::click());

    if response.clicked() || (autofocus && ui.memory(|m| m.focused().is_none())) {
        response.request_focus();
    }
    let focused = response.has_focus();
    if focused {
        // Keep Tab/arrows/Escape for the terminal instead of egui focus navigation.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
    }

    let cols = (rect.width() / cell.w).floor().max(2.0) as usize;
    let rows = (rect.height() / cell.h).floor().max(1.0) as usize;
    session.resize(cols, rows);

    if focused {
        handle_keyboard(ui, session);
    }
    if response.hovered() {
        handle_wheel(ui, session, cell.h);
    }

    paint(ui, session, rect, cell, focused);

    if focused {
        let cursor = cursor_rect(session, rect.min, cell);
        ui.ctx().output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Terminal,
                rect,
                cursor_rect: cursor,
                should_interrupt_composition: false,
            })
        });
    }
}

fn handle_keyboard(ui: &egui::Ui, session: &mut Session) {
    let events = ui.input(|i| i.events.clone());
    let mode = *session.terminal.term().mode();
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    // Alt+letter is sent as ESC+letter from the key event; drop the text event that may follow.
    let mut skip_text = false;

    for event in events {
        match event {
            Event::Text(text) => {
                if !std::mem::take(&mut skip_text) {
                    session.send_text(&text);
                }
            }
            Event::Key { key, pressed: true, modifiers, .. } => {
                if let Some(bytes) = key_bytes(key, modifiers, app_cursor) {
                    skip_text = modifiers.alt && !modifiers.ctrl;
                    session.send_bytes(bytes);
                }
            }
            // egui turns Ctrl+C / Ctrl+X into clipboard events; in a terminal they are signals.
            Event::Copy => session.send_bytes(vec![0x03]),
            Event::Cut => session.send_bytes(vec![0x18]),
            Event::Paste(text) => {
                let text = paste_bytes(&text, mode.contains(TermMode::BRACKETED_PASTE));
                session.send_text(&text);
            }
            Event::Ime(ImeEvent::Preedit { text, .. }) => session.preedit = text,
            Event::Ime(ImeEvent::Commit(text)) => {
                session.preedit.clear();
                session.send_text(&text);
            }
            _ => {}
        }
    }
}

fn handle_wheel(ui: &egui::Ui, session: &mut Session, cell_h: f32) {
    let dy = ui.input(|i| i.smooth_scroll_delta().y);
    if dy == 0.0 {
        return;
    }
    session.scroll_accum += dy;
    let lines = (session.scroll_accum / cell_h).trunc() as i32;
    if lines == 0 {
        return;
    }
    session.scroll_accum -= lines as f32 * cell_h;

    let mode = *session.terminal.term().mode();
    if mode.contains(TermMode::ALT_SCREEN) {
        // Full-screen apps (less, vim, ...) have no scrollback: send arrow keys instead.
        if mode.contains(TermMode::ALTERNATE_SCROLL) {
            let app_cursor = mode.contains(TermMode::APP_CURSOR);
            let key = if lines > 0 { egui::Key::ArrowUp } else { egui::Key::ArrowDown };
            let one = key_bytes(key, egui::Modifiers::NONE, app_cursor).unwrap_or_default();
            session.send_bytes(one.repeat(lines.unsigned_abs() as usize));
        }
    } else {
        session.terminal.scroll_lines(lines);
    }
}

fn cursor_rect(session: &Session, origin: Pos2, cell: CellSize) -> Rect {
    let term = session.terminal.term();
    let point = term.grid().cursor.point;
    let row = point.line.0 + term.grid().display_offset() as i32;
    Rect::from_min_size(
        Pos2::new(origin.x + point.column.0 as f32 * cell.w, origin.y + row as f32 * cell.h),
        Vec2::new(cell.w, cell.h),
    )
}

/// A horizontal run of narrow ASCII glyphs sharing one style, drawn as a single galley.
struct TextRun {
    row: i32,
    start_col: usize,
    next_col: usize,
    text: String,
    color: Color32,
    bold: bool,
}

struct BgRun {
    row: i32,
    start_col: usize,
    end_col: usize,
    color: Color32,
}

struct Frame<'a> {
    painter: &'a egui::Painter,
    origin: Pos2,
    cell: CellSize,
    regular: FontId,
    bold: FontId,
    backgrounds: Vec<Shape>,
    foregrounds: Vec<Shape>,
    run: Option<TextRun>,
    bg_run: Option<BgRun>,
}

impl Frame<'_> {
    fn pos(&self, row: i32, col: usize) -> Pos2 {
        Pos2::new(self.origin.x + col as f32 * self.cell.w, self.origin.y + row as f32 * self.cell.h)
    }

    fn font(&self, bold: bool) -> FontId {
        if bold { self.bold.clone() } else { self.regular.clone() }
    }

    fn background(&mut self, row: i32, col: usize, width: usize, color: Option<Color32>) {
        if let (Some(run), Some(color)) = (&mut self.bg_run, color)
            && run.row == row
            && run.end_col == col
            && run.color == color
        {
            run.end_col = col + width;
            return;
        }
        self.flush_background();
        if let Some(color) = color {
            self.bg_run = Some(BgRun { row, start_col: col, end_col: col + width, color });
        }
    }

    fn flush_background(&mut self) {
        if let Some(run) = self.bg_run.take() {
            let min = self.pos(run.row, run.start_col);
            let max = self.pos(run.row + 1, run.end_col);
            self.backgrounds.push(Shape::rect_filled(Rect::from_min_max(min, max), 0.0, run.color));
        }
    }

    fn text(&mut self, row: i32, col: usize, ch: char, color: Color32, bold: bool) {
        if let Some(run) = &mut self.run
            && run.row == row
            && run.next_col == col
            && (ch == ' ' || (run.color == color && run.bold == bold))
        {
            run.text.push(ch);
            run.next_col += 1;
            return;
        }
        self.flush_text();
        if ch != ' ' {
            self.run = Some(TextRun { row, start_col: col, next_col: col + 1, text: ch.into(), color, bold });
        }
    }

    fn flush_text(&mut self) {
        if let Some(run) = self.run.take() {
            let text = run.text.trim_end().to_owned();
            if !text.is_empty() {
                let galley = self.painter.layout_no_wrap(text, self.font(run.bold), run.color);
                self.foregrounds.push(Shape::galley(self.pos(run.row, run.start_col), galley, run.color));
            }
        }
    }

    /// Non-ASCII or wide glyphs are drawn one by one so fallback fonts with different advances
    /// cannot shift the rest of the line off the grid.
    fn glyph(&mut self, row: i32, col: usize, width: usize, text: String, color: Color32, bold: bool) {
        self.flush_text();
        let galley = self.painter.layout_no_wrap(text, self.font(bold), color);
        let slot = width as f32 * self.cell.w;
        let mut pos = self.pos(row, col);
        if width > 1 {
            pos.x += ((slot - galley.size().x) / 2.0).max(0.0);
        }
        self.foregrounds.push(Shape::galley(pos, galley, color));
    }

    fn decorations(&mut self, row: i32, col: usize, width: usize, flags: Flags, color: Color32) {
        let left = self.pos(row, col);
        let right_x = left.x + width as f32 * self.cell.w;
        let stroke = Stroke::new(1.0, color);
        if flags.intersects(Flags::ALL_UNDERLINES) {
            let y = left.y + self.cell.h - 1.5;
            self.foregrounds.push(Shape::line_segment([Pos2::new(left.x, y), Pos2::new(right_x, y)], stroke));
        }
        if flags.contains(Flags::STRIKEOUT) {
            let y = left.y + self.cell.h / 2.0;
            self.foregrounds.push(Shape::line_segment([Pos2::new(left.x, y), Pos2::new(right_x, y)], stroke));
        }
    }
}

fn cell_text(cell: &Cell) -> String {
    let mut s = String::from(cell.c);
    if let Some(zw) = cell.zerowidth() {
        s.extend(zw);
    }
    s
}

fn paint(ui: &egui::Ui, session: &Session, rect: Rect, cell: CellSize, focused: bool) {
    let painter = ui.painter_at(rect);
    let pixels_per_point = ui.ctx().pixels_per_point();
    let term = session.terminal.term();
    let palette = session.terminal.palette();
    let content = term.renderable_content();
    let colors = content.colors;
    let default_bg = palette.background(colors);
    let cursor_color = c32(palette.cursor(colors));
    painter.rect_filled(rect, 0.0, c32(default_bg));

    let offset = content.display_offset as i32;
    let cursor = content.cursor;
    let cursor_row = cursor.point.line.0 + offset;
    let cursor_col = cursor.point.column.0;
    let cursor_visible = cursor.shape != CursorShape::Hidden && content.mode.contains(TermMode::SHOW_CURSOR);
    let block_cursor =
        focused && cursor_visible && cursor.shape == CursorShape::Block && session.preedit.is_empty();

    let mut frame = Frame {
        painter: &painter,
        origin: rect.min,
        cell,
        regular: FontId::monospace(FONT_SIZE),
        bold: FontId::new(FONT_SIZE, bold_family()),
        backgrounds: Vec::new(),
        foregrounds: Vec::new(),
        run: None,
        bg_run: None,
    };

    for indexed in content.display_iter {
        let row = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        let cell = indexed.cell;
        let cell_rect = Rect::from_min_size(frame.pos(row, col), Vec2::new(frame.cell.w, frame.cell.h));
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

        let (fg, bg) = palette.cell_colors(cell, colors);
        let (fg, bg) = if block_cursor && row == cursor_row && col == cursor_col {
            (c32(bg), Some(cursor_color))
        } else {
            (c32(fg), (bg != default_bg).then(|| c32(bg)))
        };
        frame.background(row, col, width, bg);

        let bold = cell.flags.contains(Flags::BOLD);
        let ch = cell.c;
        if cell.flags.contains(Flags::HIDDEN) || ch == '\0' || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            frame.text(row, col, ' ', fg, bold);
        } else if ch.is_ascii() && width == 1 && cell.zerowidth().is_none() {
            frame.text(row, col, ch, fg, bold);
        } else if !boxdraw::draw(ch, cell_rect, fg, pixels_per_point, &mut frame.foregrounds) {
            frame.glyph(row, col, width, cell_text(cell), fg, bold);
        }

        if cell.flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) {
            frame.decorations(row, col, width, cell.flags, fg);
        }
    }
    frame.flush_text();
    frame.flush_background();

    painter.extend(frame.backgrounds);
    painter.extend(frame.foregrounds);

    let cursor_cell = Rect::from_min_size(
        Pos2::new(rect.min.x + cursor_col as f32 * cell.w, rect.min.y + cursor_row as f32 * cell.h),
        Vec2::new(cell.w, cell.h),
    );

    if focused && !session.preedit.is_empty() {
        // Show the IME composition (e.g. a Hangul syllable being assembled) at the cursor.
        let galley = painter.layout_no_wrap(
            session.preedit.clone(),
            FontId::monospace(FONT_SIZE),
            c32(default_bg),
        );
        let preedit_rect = Rect::from_min_size(cursor_cell.min, Vec2::new(galley.size().x.max(cell.w), cell.h));
        painter.rect_filled(preedit_rect, 0.0, cursor_color);
        painter.galley(cursor_cell.min, galley, c32(default_bg));
    } else if cursor_visible && !block_cursor {
        let stroke = Stroke::new(1.0, cursor_color);
        match cursor.shape {
            CursorShape::Beam => {
                painter.rect_filled(Rect::from_min_size(cursor_cell.min, Vec2::new(2.0, cell.h)), 0.0, cursor_color);
            }
            CursorShape::Underline => {
                let min = Pos2::new(cursor_cell.min.x, cursor_cell.max.y - 2.0);
                painter.rect_filled(Rect::from_min_size(min, Vec2::new(cell.w, 2.0)), 0.0, cursor_color);
            }
            _ => {
                painter.rect_stroke(cursor_cell.shrink(0.5), 0.0, stroke, egui::StrokeKind::Inside);
            }
        }
    }
}
