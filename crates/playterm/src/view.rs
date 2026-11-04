//! Terminal widget: paints the grid and turns keyboard/IME/wheel input into session input.

use eframe::egui::{self, Color32, Event, EventFilter, FontFamily, FontId, ImeEvent, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use playterm_config::ClipboardKeys;
use playterm_i18n::{t, tr};
use playterm_term::{Cell, CursorShape, Flags, Point, Rgb, SearchMatch, SelectionType, Side, TermMode};

use crate::clip_keys::{self, ClipAction};
use crate::input::{key_bytes, paste_bytes};
use crate::mouse::{self, MouseAction, MouseButton, MouseMods};
use crate::search_bar::{self, BarAction};
use crate::session::{Session, SessionState};
use crate::{boxdraw, clipboard, toast};

/// How the terminal is drawn (from the app settings).
#[derive(Clone, Debug)]
pub struct ViewStyle {
    pub font_size: f32,
    pub selection: Color32,
    pub clipboard: ClipboardKeys,
    /// Sites connected to lately, newest first, for the start screen.
    pub recent: Vec<RecentSite>,
    /// Alt (Option on macOS) + key is sent as Meta: ESC and the key.
    pub alt_is_meta: bool,
}

/// A recently connected site, as the start screen lists it.
#[derive(Clone, Debug)]
pub struct RecentSite {
    pub id: String,
    pub name: String,
    pub protocol: playterm_net::Protocol,
}

/// What the terminal widget asks of the app this frame.
#[derive(Default)]
pub struct ViewOutput {
    /// Ctrl+wheel notches or a touchpad pinch, in font size steps (positive = larger).
    pub zoom_steps: f32,
    /// What was typed in this tab, for simultaneous input to other tabs.
    pub inputs: Vec<UserInput>,
    /// The view has the keyboard focus.
    pub focused: bool,
    /// Something clicked in the view asks the app for this (the connection line).
    pub command: Option<crate::menu::Command>,
    /// The connection line's "Reconnect" was clicked (the tab's index is the dock's to know).
    pub reconnect: bool,
}

/// Keyboard input as typed (before encoding), so it can be repeated in another tab with that
/// tab's own key modes and charset.
#[derive(Clone, Debug)]
pub enum UserInput {
    Text(String),
    Key {
        key: egui::Key,
        modifiers: egui::Modifiers,
    },
    /// Control bytes that are the same everywhere (Ctrl+C, Ctrl+X).
    Bytes(Vec<u8>),
    Paste(String),
}

impl UserInput {
    /// Types this into `session`.
    pub fn send_to(&self, session: &mut Session) {
        // A remote desktop takes typed text; terminal key sequences mean nothing there.
        if session.is_rdp() {
            if let UserInput::Text(text) | UserInput::Paste(text) = self {
                session.send_text(text);
            }
            return;
        }
        match self {
            UserInput::Text(text) => session.send_text(text),
            UserInput::Key { key, modifiers } => {
                let app_cursor = session.terminal.term().mode().contains(TermMode::APP_CURSOR);
                if let Some(bytes) = key_bytes(*key, *modifiers, app_cursor) {
                    session.send_bytes(bytes);
                }
            }
            UserInput::Bytes(bytes) => session.send_bytes(bytes.clone()),
            UserInput::Paste(text) => paste(session, text),
        }
    }
}
/// Max time between presses that still counts as a double/triple click, in seconds.
const MULTI_CLICK_SECS: f64 = 0.4;

pub fn bold_family() -> FontFamily {
    FontFamily::Name("mono-bold".into())
}

#[derive(Clone, Copy)]
struct CellSize {
    w: f32,
    h: f32,
}

fn cell_size(ctx: &egui::Context, font_size: f32) -> CellSize {
    let font = FontId::monospace(font_size);
    ctx.fonts_mut(|f| CellSize { w: f.glyph_width(&font, 'M'), h: f.row_height(&font) })
}

fn c32(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

/// Shows the terminal of `session` in all remaining space of `ui`.
///
/// `autofocus` grabs keyboard focus when nothing else has it (false while a dialog is open).
pub fn show(ui: &mut egui::Ui, session: &mut Session, autofocus: bool, style: &ViewStyle) -> ViewOutput {
    let mut output = ViewOutput::default();
    let cell = cell_size(ui.ctx(), style.font_size);
    // The whole area takes clicks and is painted in the terminal background; the text grid
    // sits inside it with a margin, so the first column does not touch the border.
    let full = ui.available_rect_before_wrap();
    // Where input goes, on a line of its own above the terminal (remote tabs).
    let line = session.connection_line();
    let area = match &line {
        Some(text) => {
            let (strip, rest) = full.split_top_bottom_at_y(full.top() + LINE_HEIGHT);
            match connection_line(ui, session, strip, text) {
                Some(LineAction::EditSite(id)) => output.command = Some(crate::menu::Command::EditSite(id)),
                Some(LineAction::Reconnect) => output.reconnect = true,
                None => {}
            }
            rest
        }
        None => full,
    };
    let response = ui.allocate_rect(area, Sense::click_and_drag());
    // The find bar takes a strip of its own at the top: it never covers output.
    let (bar_strip, grid_area) = match session.search {
        Some(_) => {
            let (strip, rest) = area.split_top_bottom_at_y(area.top() + search_bar::HEIGHT);
            (Some(strip), rest)
        }
        None => (None, area),
    };
    let rect = text_rect(grid_area);

    if response.clicked() || (autofocus && ui.memory(|m| m.focused().is_none())) {
        response.request_focus();
    }
    // A modal above the terminal (confirmations, SFTP prompts) must not leak keys into the
    // shell even though the terminal may still hold keyboard focus underneath it.
    let behind_modal = !ui.memory(|m| m.allows_interaction(ui.layer_id()));
    let focused = response.has_focus() && !behind_modal;
    if focused {
        // Keep Tab/arrows/Escape for the terminal instead of egui focus navigation.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(response.id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true })
        });
    }

    let cols = (rect.width() / cell.w).floor().max(2.0) as usize;
    let rows = (rect.height() / cell.h).floor().max(1.0) as usize;
    session.resize(cols, rows);

    let mode = *session.terminal.term().mode();
    // Focus reports follow the OS window as well (switching to another application).
    let has_focus = focused && ui.input(|i| i.focused);
    if let Some(report) = mouse::focus(mode, has_focus).filter(|_| has_focus != session.mouse.last_focus) {
        session.send_bytes(report.to_vec());
    }
    session.mouse.last_focus = has_focus;

    output.focused = response.has_focus();
    if focused {
        handle_keyboard(ui, session, &style.clipboard, style.alt_is_meta, &mut output.inputs);
    }

    // Mouse goes to the application when it asked for it (vim `mouse=a`, htop, mc, tmux),
    // except with Shift held, which always selects locally like xterm and Windows Terminal.
    // A gesture already in progress keeps its route until the button is released.
    let shift = ui.input(|i| i.modifiers.shift);
    let report = !session.click.dragging && (session.mouse.pressed.is_some() || (mouse::reporting(mode) && !shift && !behind_modal));
    if report {
        report_mouse(ui, session, &response, rect, cell, mode);
    } else {
        handle_mouse(ui, session, &response, rect, cell, &style.clipboard);
    }
    if response.hovered() && !behind_modal {
        ui.ctx().set_cursor_icon(if report { egui::CursorIcon::Default } else { egui::CursorIcon::Text });
        // Ctrl+wheel (and a touchpad pinch) sizes the font instead of scrolling.
        let (zoom, ctrl) = ui.input(|i| (i.zoom_delta(), i.modifiers.ctrl));
        if ctrl {
            output.zoom_steps = zoom_wheel_steps(ui);
        } else if zoom != 1.0 {
            // A pinch: about one step per 10 %.
            output.zoom_steps = zoom.ln() / 1.1f32.ln();
        } else {
            handle_wheel(ui, session, rect, cell, report);
        }
    }
    if !report {
        let ctrl = ui.input(|i| i.modifiers.ctrl);
        if clip_keys::paste_click(&response, &style.clipboard, ctrl) {
            if let Some(text) = clipboard::read_text() {
                request_paste(session, text, &mut output.inputs, &style.clipboard);
            }
        } else {
            response.context_menu(|ui| context_menu(ui, session, &style.clipboard));
        }
    }

    let matches = match session.search.as_mut() {
        Some(bar) => bar.visible_matches(&session.terminal),
        None => Vec::new(),
    };
    paint(ui, session, area, rect, cell, focused, style, &matches);
    if session.start_screen && session.state == SessionState::Local {
        output.command = output.command.or(start_screen(ui, rect, style));
    }
    if let (Some(bar), Some(strip)) = (session.search.as_mut(), bar_strip)
        && let BarAction::Close = bar.show(ui, &mut session.terminal, strip)
    {
        session.search = None;
    }

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
    output
}

fn handle_keyboard(ui: &egui::Ui, session: &mut Session, keys: &ClipboardKeys, alt_is_meta: bool, inputs: &mut Vec<UserInput>) {
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
                    inputs.push(UserInput::Text(text));
                }
            }
            Event::Key { key, pressed: true, modifiers, .. }
                if let Some(action) = clip_keys::terminal_action(key, modifiers, keys, session.terminal.has_selection()) =>
            {
                match action {
                    ClipAction::Copy => {
                        copy_selection(ui.ctx(), session);
                    }
                    ClipAction::Paste => {
                        if let Some(text) = clipboard::read_text() {
                            request_paste(session, text, inputs, keys);
                        }
                    }
                }
            }
            Event::Key { key, pressed: true, modifiers, .. } => {
                // Option not as Meta (macOS setting): the character macOS made of it comes as
                // text; the key itself is sent as without Option.
                let modifiers = if alt_is_meta { modifiers } else { egui::Modifiers { alt: false, ..modifiers } };
                if let Some(bytes) = key_bytes(key, modifiers, app_cursor) {
                    skip_text = modifiers.alt && !modifiers.ctrl;
                    session.send_bytes(bytes);
                    inputs.push(UserInput::Key { key, modifiers });
                }
            }
            // egui's clipboard shortcuts are off (see `clip_keys`); a keyboard's own Copy / Cut /
            // Paste keys still arrive this way.
            Event::Copy => {
                if !copy_selection(ui.ctx(), session) {
                    session.send_bytes(vec![0x03]);
                    inputs.push(UserInput::Bytes(vec![0x03]));
                }
            }
            Event::Cut => {
                session.send_bytes(vec![0x18]);
                inputs.push(UserInput::Bytes(vec![0x18]));
            }
            Event::Paste(text) => request_paste(session, text, inputs, keys),
            Event::Ime(ImeEvent::Preedit { text, .. }) => session.preedit = text,
            Event::Ime(ImeEvent::Commit(text)) => {
                session.preedit.clear();
                session.send_text(&text);
                inputs.push(UserInput::Text(text));
            }
            _ => {}
        }
    }
}

/// Copies the selection to the clipboard and clears it. Returns `false` if nothing was selected.
pub fn copy_selection(ctx: &egui::Context, session: &mut Session) -> bool {
    match session.terminal.selection_text() {
        Some(text) => {
            toast::copied(ctx, &text);
            ctx.copy_text(text);
            session.terminal.clear_selection();
            true
        }
        None => false,
    }
}

/// Pastes `text` (recording it in `inputs` for simultaneous input), or holds it in
/// [`Session::pending_paste`] for the user to confirm first (see [`needs_paste_confirm`]).
pub fn request_paste(session: &mut Session, text: String, inputs: &mut Vec<UserInput>, keys: &ClipboardKeys) {
    if keys.confirm_multiline_paste && needs_paste_confirm(session, &text) {
        session.pending_paste = Some(text);
    } else {
        paste(session, &text);
        inputs.push(UserInput::Paste(text));
    }
}

/// Whether pasting `text` would run several commands: more than one line (a single trailing
/// line break aside) into a connected tab whose program did not ask for bracketed paste, so
/// every line ends with Enter.
pub fn needs_paste_confirm(session: &Session, text: &str) -> bool {
    let body = text.strip_suffix("\r\n").or_else(|| text.strip_suffix('\n')).unwrap_or(text);
    session.state == SessionState::Connected
        && !session.terminal.term().mode().contains(TermMode::BRACKETED_PASTE)
        && body.contains(['\n', '\r'])
}

pub fn paste(session: &mut Session, text: &str) {
    let bracketed = session.terminal.term().mode().contains(TermMode::BRACKETED_PASTE);
    session.send_text(&paste_bytes(text, bracketed));
}

fn context_menu(ui: &mut egui::Ui, session: &mut Session, keys: &ClipboardKeys) {
    let copy = egui::Button::new(t("복사", "Copy")).shortcut_text(clip_keys::copy_hint(keys));
    if ui.add_enabled(session.terminal.has_selection(), copy).clicked() {
        copy_selection(ui.ctx(), session);
        ui.close();
    }
    if ui.add(egui::Button::new(t("붙여넣기", "Paste")).shortcut_text(clip_keys::paste_hint(keys))).clicked() {
        if let Some(text) = clipboard::read_text() {
            request_paste(session, text, &mut Vec::new(), keys);
        }
        ui.close();
    }
    ui.separator();
    if ui.button(t("모두 선택", "Select all")).clicked() {
        session.terminal.select_all();
        ui.close();
    }
}

/// Grid point and cell half under a screen position (clamped to the terminal area).
fn cell_at(session: &Session, rect: Rect, cell: CellSize, pos: Pos2) -> (Point, Side) {
    let x = ((pos.x - rect.left()) / cell.w).max(0.0);
    let y = ((pos.y - rect.top()) / cell.h).max(0.0);
    let col = x.floor() as usize;
    let side = if col >= session.terminal.cols() || x.fract() >= 0.5 { Side::Right } else { Side::Left };
    (session.terminal.viewport_point(y.floor() as usize, col), side)
}

fn handle_mouse(ui: &egui::Ui, session: &mut Session, response: &egui::Response, rect: Rect, cell: CellSize, keys: &ClipboardKeys) {
    let (pressed, down, pos, shift, time) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.modifiers.shift, i.time));
    let Some(pos) = pos else { return };

    if pressed && response.hovered() {
        let (point, side) = cell_at(session, rect, cell, pos);
        let click = &mut session.click;
        click.count = match click.last {
            Some((t, p)) if time - t < MULTI_CLICK_SECS && p == point => click.count % 3 + 1,
            _ => 1,
        };
        click.last = Some((time, point));
        click.dragging = true;

        if click.count == 1 && shift && session.terminal.has_selection() {
            session.terminal.update_selection(point, side);
        } else {
            let ty = match click.count {
                1 => SelectionType::Simple,
                2 => SelectionType::Semantic,
                _ => SelectionType::Lines,
            };
            session.terminal.start_selection(ty, point, side);
        }
    } else if session.click.dragging {
        if !down {
            session.click.dragging = false;
            // Copy on select: the selection stays on screen.
            if keys.copy_on_select
                && let Some(text) = session.terminal.selection_text().filter(|t| !t.is_empty())
            {
                toast::copied(ui.ctx(), &text);
                ui.ctx().copy_text(text);
            }
            return;
        }
        // Dragging past the top/bottom edge scrolls through the history.
        if pos.y < rect.top() {
            session.terminal.scroll_lines(1);
            ui.ctx().request_repaint();
        } else if pos.y > rect.bottom() {
            session.terminal.scroll_lines(-1);
            ui.ctx().request_repaint();
        }
        let clamped = Pos2::new(pos.x, pos.y.clamp(rect.top(), rect.bottom() - 1.0));
        let (point, side) = cell_at(session, rect, cell, clamped);
        session.terminal.update_selection(point, side);
    }
}

/// 0-based viewport cell under a screen position, clamped to the grid.
fn viewport_cell(session: &Session, rect: Rect, cell: CellSize, pos: Pos2) -> (usize, usize) {
    let col = ((pos.x - rect.left()) / cell.w).floor().max(0.0) as usize;
    let row = ((pos.y - rect.top()) / cell.h).floor().max(0.0) as usize;
    (col.min(session.terminal.cols() - 1), row.min(session.terminal.rows() - 1))
}

fn mouse_mods(m: egui::Modifiers) -> MouseMods {
    MouseMods { shift: m.shift, alt: m.alt, ctrl: m.ctrl }
}

/// Forwards clicks, drags and motion to the application as xterm mouse reports.
fn report_mouse(ui: &egui::Ui, session: &mut Session, response: &egui::Response, rect: Rect, cell: CellSize, mode: TermMode) {
    let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
    let over_terminal = |pos: Pos2| response.hovered() && rect.contains(pos);

    for event in events {
        match event {
            Event::PointerButton { pos, button, pressed, modifiers } => {
                let button = match button {
                    egui::PointerButton::Primary => MouseButton::Left,
                    egui::PointerButton::Secondary => MouseButton::Right,
                    egui::PointerButton::Middle => MouseButton::Middle,
                    _ => continue,
                };
                let (col, row) = viewport_cell(session, rect, cell, pos);
                let action = if pressed {
                    // Presses outside the terminal belong to other widgets.
                    if !over_terminal(pos) || session.mouse.pressed.is_some() {
                        continue;
                    }
                    session.mouse.pressed = Some(button);
                    MouseAction::Press
                } else {
                    // Releases are reported wherever they happen, but only for our own press.
                    if session.mouse.pressed != Some(button) {
                        continue;
                    }
                    session.mouse.pressed = None;
                    MouseAction::Release
                };
                session.mouse.last_cell = Some((col, row));
                if let Some(bytes) = mouse::encode(mode, button, action, col, row, mouse_mods(modifiers)) {
                    session.send_bytes(bytes);
                }
            }
            Event::PointerMoved(pos) => {
                let at = viewport_cell(session, rect, cell, pos);
                if session.mouse.last_cell == Some(at) {
                    continue; // only cell changes are reported
                }
                let button = match session.mouse.pressed {
                    Some(b) => b,
                    None if over_terminal(pos) => MouseButton::None,
                    None => continue,
                };
                session.mouse.last_cell = Some(at);
                if let Some(bytes) = mouse::encode(mode, button, MouseAction::Motion, at.0, at.1, mouse_mods(modifiers)) {
                    session.send_bytes(bytes);
                }
            }
            _ => {}
        }
    }
}

/// Wheel events for the application: one report per wheel notch, like xterm. Uses the raw
/// events (smoothed deltas would turn one notch into 1 or 2 reports depending on rounding).
fn report_wheel(ui: &egui::Ui, session: &mut Session, rect: Rect, cell: CellSize) {
    let (events, hover) = ui.input(|i| (i.events.clone(), i.pointer.hover_pos()));
    let Some(pos) = hover else { return };
    let mode = *session.terminal.term().mode();
    for event in events {
        let Event::MouseWheel { unit, delta, modifiers, .. } = event else { continue };
        session.mouse.wheel_accum += match unit {
            egui::MouseWheelUnit::Line => delta.y,
            egui::MouseWheelUnit::Point => delta.y / cell.h, // precision touchpads
            egui::MouseWheelUnit::Page => delta.y * session.terminal.rows() as f32,
        };
        let notches = session.mouse.wheel_accum.trunc() as i32;
        if notches == 0 {
            continue;
        }
        session.mouse.wheel_accum -= notches as f32;
        let (col, row) = viewport_cell(session, rect, cell, pos);
        // Content moving down (positive delta) means scrolling up.
        let button = if notches > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
        let one = mouse::encode(mode, button, MouseAction::Press, col, row, mouse_mods(modifiers)).unwrap_or_default();
        session.send_bytes(one.repeat(notches.unsigned_abs() as usize));
    }
}

/// Ctrl+wheel: one step per wheel notch (touchpads scroll in points; 50 points = one notch).
fn zoom_wheel_steps(ui: &egui::Ui) -> f32 {
    ui.input(|i| {
        i.events
            .iter()
            .map(|e| match e {
                Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta, .. } => delta.y,
                Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, .. } => delta.y / 50.0,
                Event::MouseWheel { unit: egui::MouseWheelUnit::Page, delta, .. } => delta.y * 3.0,
                _ => 0.0,
            })
            .sum()
    })
}

fn handle_wheel(ui: &egui::Ui, session: &mut Session, rect: Rect, cell: CellSize, report: bool) {
    if report {
        report_wheel(ui, session, rect, cell);
        return;
    }
    let dy = ui.input(|i| i.smooth_scroll_delta().y);
    if dy == 0.0 {
        return;
    }
    session.scroll_accum += dy;
    let lines = (session.scroll_accum / cell.h).trunc() as i32;
    if lines == 0 {
        return;
    }
    session.scroll_accum -= lines as f32 * cell.h;

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
    Rect::from_min_size(Pos2::new(origin.x + point.column.0 as f32 * cell.w, origin.y + row as f32 * cell.h), Vec2::new(cell.w, cell.h))
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
        let Some(run) = self.run.take() else { return };
        let text = run.text.trim_end().to_owned();
        if text.is_empty() {
            return;
        }
        let cells = text.chars().count();
        let galley = self.painter.layout_no_wrap(text.clone(), self.font(run.bold), run.color);
        // One galley per run is cheap, but only stays on the grid when every glyph is exactly
        // one cell wide (not true of some fonts' bold faces); otherwise place each glyph.
        if (galley.size().x - cells as f32 * self.cell.w).abs() < 0.5 {
            self.foregrounds.push(Shape::galley(self.pos(run.row, run.start_col), galley, run.color));
            return;
        }
        for (i, ch) in text.chars().enumerate().filter(|(_, c)| *c != ' ') {
            let galley = self.painter.layout_no_wrap(ch.into(), self.font(run.bold), run.color);
            self.foregrounds.push(Shape::galley(self.pos(run.row, run.start_col + i), galley, run.color));
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

/// What a fresh local tab shows over its terminal: recent sites and ways to connect, and how
/// to use the prompt instead. Gone as soon as something is typed.
fn start_screen(ui: &mut egui::Ui, rect: Rect, style: &ViewStyle) -> Option<crate::menu::Command> {
    use crate::menu::Command;
    // Inside the pane, whatever its size (#145): too small for the card, the prompt's own
    // hint says what to type.
    if rect.width() < 240.0 || rect.height() < 200.0 {
        return None;
    }
    let width = 440.0_f32.min(rect.width() - 24.0);
    // Centred on the card's height of the last frame (about 260 points the first time).
    let height_id = egui::Id::unique("start-card-height");
    let height: f32 = ui.data(|d| d.get_temp(height_id)).unwrap_or(260.0);
    let top = (rect.center().y - rect.height() * 0.1 - height / 2.0)
        .clamp(rect.top() + 12.0, (rect.bottom() - height - 12.0).max(rect.top() + 12.0));
    let card = Rect::from_min_max(egui::pos2(rect.center().x - width / 2.0, top), egui::pos2(rect.center().x + width / 2.0, rect.bottom()));
    let mut command = None;
    let shown = ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        egui::Frame::popup(ui.style())
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_width(width - 32.0);
                ui.heading("playterm");
                ui.weak(t("연결할 곳을 고르세요.", "Choose where to connect."));
                ui.add_space(8.0);
                if !style.recent.is_empty() {
                    ui.strong(t("최근 연결", "Recent"));
                    for site in &style.recent {
                        ui.horizontal(|ui| {
                            crate::site_tree::badge(ui, site.protocol);
                            if ui.link(&site.name).on_hover_text(t("연결", "Connect")).clicked() {
                                command = Some(Command::ConnectSite(site.id.clone()));
                            }
                        });
                    }
                    ui.add_space(8.0);
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.add(crate::icon::primary(ui, crate::icon::ADD, t("빠른 연결", "Quick Connect"))).clicked() {
                        command = Some(Command::QuickConnect);
                    }
                    if ui.add(crate::icon::button(crate::icon::NEW_SITE, t("새 사이트", "New site"))).clicked() {
                        command = Some(Command::NewSite);
                    }
                    if ui.add(crate::icon::button(crate::icon::IMPORT, t("PuTTY 가져오기", "Import PuTTY"))).clicked() {
                        command = Some(Command::ImportPutty);
                    }
                    if ui.add(crate::icon::button(crate::icon::IMPORT, t("Xshell 가져오기", "Import Xshell"))).clicked() {
                        command = Some(Command::ImportXshell);
                    }
                });
                ui.add_space(8.0);
                ui.weak(t(
                    "명령으로 쓰려면 아무 키나 누르세요 — ssh, telnet, open, sites, ls, help …",
                    "To use commands, just type — ssh, telnet, open, sites, ls, help …",
                ));
            })
            .response
            .rect
            .height()
    });
    ui.data_mut(|d| d.insert_temp(height_id, shown.inner));
    command
}

/// Height of the connection line above a remote terminal.
const LINE_HEIGHT: f32 = 22.0;

/// What the connection line's buttons ask for.
enum LineAction {
    EditSite(String),
    Reconnect,
}

/// `a` moved towards `b` by `t` (0 to 1), channel by channel.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// The connection line's background: the panel tinted with the site's color, light on a light
/// look and dark on a dark one, so its text stays readable on both (#158).
pub(crate) fn line_fill(visuals: &egui::Visuals, color: Option<Color32>) -> Color32 {
    match color {
        Some(c) if visuals.dark_mode => mix(visuals.panel_fill, c, 0.30),
        Some(c) => mix(visuals.panel_fill, c, 0.16),
        None => visuals.panel_fill,
    }
}

/// The connection line: label, protocol, address and state, in the site's color, ending in "…"
/// when the pane is narrow (#149). Buttons at its right end reconnect a closed tab (#120) and
/// open a saved site's settings (#137); the rest of the line does nothing on a click, and its
/// context menu copies the address.
fn connection_line(ui: &mut egui::Ui, session: &Session, strip: Rect, text: &str) -> Option<LineAction> {
    let visuals = ui.visuals().clone();
    let color = session.tab_color.map(crate::menu::color32);
    let painter = ui.painter_at(strip);
    painter.rect_filled(strip, 0.0, line_fill(&visuals, color));
    if let Some(c) = color {
        painter.rect_filled(Rect::from_min_size(strip.min, Vec2::new(3.0, strip.height())), 0.0, c);
    }
    painter.hline(strip.x_range(), strip.bottom() - 0.5, egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color));

    let id = egui::Id::unique(("connection-line", session.id));
    let response = ui.interact(strip, id, Sense::click());
    let site = session.site().map(str::to_owned);
    let mut action = None;
    // The buttons, from the right end.
    let mut right = strip.right() - 4.0;
    if let Some(id) = &site {
        let button = egui::Button::new(crate::icon::MANAGE.rich_text()).frame(false);
        if line_button(ui, &mut right, strip, button, t("사이트 설정", "Site settings")) {
            action = Some(LineAction::EditSite(id.clone()));
        }
    }
    if session.state == SessionState::Closed && session.has_target() {
        let button = crate::icon::button(crate::icon::RECONNECT, t("다시 연결", "Reconnect"));
        if line_button(ui, &mut right, strip, button, t("같은 곳에 다시 연결", "Connect to the same place again")) {
            action = Some(LineAction::Reconnect);
        }
    }

    let font = egui::TextStyle::Small.resolve(ui.style());
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, visuals.strong_text_color());
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - strip.left() - 14.0).max(0.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let truncated = galley.elided;
    painter.galley(egui::pos2(strip.left() + 10.0, strip.center().y - galley.size().y / 2.0), galley, visuals.strong_text_color());

    let response = if truncated { response.on_hover_text(text) } else { response };
    response.context_menu(|ui| {
        if let Some(address) = session.address()
            && ui.button(tr!("주소 복사: {}", "Copy the address: {}", address)).clicked()
        {
            ui.ctx().copy_text(address);
            ui.close();
        }
    });
    action
}

/// A small button at the right end of the connection line, left of `right`, which moves past
/// it. Returns whether it was clicked.
fn line_button(ui: &mut egui::Ui, right: &mut f32, strip: Rect, button: egui::Button<'_>, tip: &str) -> bool {
    let slot = Rect::from_min_max(egui::pos2(strip.left(), strip.top() + 1.0), egui::pos2(*right, strip.bottom() - 1.0));
    let response = ui.scope_builder(egui::UiBuilder::new().max_rect(slot).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
        ui.add(button.small())
    });
    let response = response.inner;
    *right = response.rect.left() - 4.0;
    response.on_hover_text(tip).clicked()
}

/// Space between the edge of the terminal area and its text.
const PADDING: f32 = 6.0;

/// Where the text grid of a terminal area goes: inside it by [`PADDING`] (nothing left out
/// when the area is too small for that).
fn text_rect(area: Rect) -> Rect {
    let inner = area.shrink(PADDING);
    if inner.is_positive() { inner } else { area }
}

/// Draws the terminal: `area` in the background color, the text grid in `rect` within it.
#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &egui::Ui,
    session: &Session,
    area: Rect,
    rect: Rect,
    cell: CellSize,
    focused: bool,
    style: &ViewStyle,
    matches: &[SearchMatch],
) {
    let painter = ui.painter_at(area);
    let pixels_per_point = ui.ctx().pixels_per_point();
    let term = session.terminal.term();
    let palette = session.terminal.palette();
    let content = term.renderable_content();
    let colors = content.colors;
    let default_bg = palette.background(colors);
    let cursor_color = c32(palette.cursor(colors));
    painter.rect_filled(area, 0.0, c32(default_bg));

    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor = content.cursor;
    let cursor_row = cursor.point.line.0 + offset;
    let cursor_col = cursor.point.column.0;
    let cursor_visible = cursor.shape != CursorShape::Hidden && content.mode.contains(TermMode::SHOW_CURSOR);
    let block_cursor = focused && cursor_visible && cursor.shape == CursorShape::Block && session.preedit.is_empty();

    let mut frame = Frame {
        painter: &painter,
        origin: rect.min,
        cell,
        regular: FontId::monospace(style.font_size),
        bold: FontId::new(style.font_size, bold_family()),
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
        let selected = selection.is_some_and(|s| s.contains_cell(&indexed, cursor.point, cursor.shape));
        let (fg, bg) = if block_cursor && row == cursor_row && col == cursor_col {
            (c32(bg), Some(cursor_color))
        } else if selected {
            (c32(fg), Some(style.selection))
        } else if matches.iter().any(|m| m.contains(&indexed.point)) {
            (search_bar::MATCH_FG, Some(search_bar::MATCH_BG))
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
        let galley = painter.layout_no_wrap(session.preedit.clone(), FontId::monospace(style.font_size), c32(default_bg));
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

#[cfg(test)]
mod padding_tests {
    use super::*;

    #[test]
    fn text_sits_inside_a_margin() {
        let area = Rect::from_min_size(Pos2::new(220.0, 110.0), Vec2::new(600.0, 400.0));
        let text = text_rect(area);
        assert_eq!(text.min, area.min + Vec2::splat(PADDING));
        assert_eq!(text.max, area.max - Vec2::splat(PADDING));
        // Too small for a margin: the area itself.
        let tiny = Rect::from_min_size(Pos2::ZERO, Vec2::new(8.0, 8.0));
        assert_eq!(text_rect(tiny), tiny);
    }
}
