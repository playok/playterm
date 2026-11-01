//! The picture of an RDP desktop in a tab, and the keyboard and mouse sent to it.
//!
//! Typed characters (and IME results such as Hangul) go as Unicode, so they arrive the same
//! whatever the remote keyboard layout. Keys that do not type text (Enter, arrows, F-keys,
//! modifiers) and keys held with Ctrl, Alt or Windows go as scan codes.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Event, EventFilter, Key, Rect, Sense, TextureOptions};
use playterm_net::{RdpFrame, RdpInput, RdpMouseButton, scancode};

use crate::session::Session;
use crate::view::ViewOutput;

/// How long the tab size must stay put before the desktop is resized to it.
const RESIZE_DELAY: Duration = Duration::from_millis(400);

/// The remote screen of one tab and the input state sent with it.
#[derive(Default)]
pub struct RdpScreen {
    /// A frame from the network not yet shown.
    pub pending: Option<RdpFrame>,
    texture: Option<egui::TextureHandle>,
    /// Size of the remote desktop.
    pub size: (u16, u16),
    /// Scan codes sent as pressed (released again when the view loses focus).
    down: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    last_pos: Option<(u16, u16)>,
    /// Wheel movement not yet sent, in 1/120 notches.
    wheel: egui::Vec2,
    /// Size of the tab in pixels, for the desktop size of (re)connections.
    pub view_px: (u16, u16),
    /// A tab size waiting to become the desktop size, and since when.
    resize: Option<(Instant, (u16, u16))>,
    requested: Option<(u16, u16)>,
    had_focus: bool,
}

impl RdpScreen {
    /// Something to show (the first frame arrived).
    pub fn has_picture(&self) -> bool {
        self.texture.is_some() || self.pending.is_some()
    }

    /// Forgets the picture (a new connection).
    pub fn reset(&mut self) {
        let view_px = self.view_px;
        *self = Self { view_px, ..Self::default() };
    }
}

fn scan(key: Key) -> Option<u16> {
    use Key::*;
    Some(match key {
        Escape => scancode::ESCAPE,
        Num1 | Exclamationmark => 0x02,
        Num2 => 0x03,
        Num3 => 0x04,
        Num4 => 0x05,
        Num5 => 0x06,
        Num6 => 0x07,
        Num7 => 0x08,
        Num8 => 0x09,
        Num9 => 0x0A,
        Num0 => 0x0B,
        Minus => 0x0C,
        Equals | Plus => 0x0D,
        Backspace => scancode::BACKSPACE,
        Tab => scancode::TAB,
        Q => 0x10,
        W => 0x11,
        E => 0x12,
        R => 0x13,
        T => 0x14,
        Y => 0x15,
        U => 0x16,
        I => 0x17,
        O => 0x18,
        P => 0x19,
        OpenBracket | OpenCurlyBracket => 0x1A,
        CloseBracket | CloseCurlyBracket => 0x1B,
        Enter => scancode::ENTER,
        ControlLeft => scancode::LEFT_CTRL,
        A => 0x1E,
        S => 0x1F,
        D => 0x20,
        F => 0x21,
        G => 0x22,
        H => 0x23,
        J => 0x24,
        K => 0x25,
        L => 0x26,
        Semicolon | Colon => 0x27,
        Quote => 0x28,
        Backtick => 0x29,
        ShiftLeft => scancode::LEFT_SHIFT,
        Backslash | Pipe => 0x2B,
        Z => 0x2C,
        X => scancode::X,
        C => scancode::C,
        V => scancode::V,
        B => 0x30,
        N => 0x31,
        M => 0x32,
        Comma => 0x33,
        Period => 0x34,
        Slash | Questionmark => 0x35,
        ShiftRight => 0x36,
        AltLeft => scancode::LEFT_ALT,
        Space => 0x39,
        CapsLock => 0x3A,
        F1 => 0x3B,
        F2 => 0x3C,
        F3 => 0x3D,
        F4 => 0x3E,
        F5 => 0x3F,
        F6 => 0x40,
        F7 => 0x41,
        F8 => 0x42,
        F9 => 0x43,
        F10 => 0x44,
        NumLock => 0x45,
        ScrollLock => 0x46,
        IntlBackslash => 0x56,
        F11 => 0x57,
        F12 => 0x58,
        ControlRight => 0xE01D,
        PrintScreen => 0xE037,
        AltRight => 0xE038,
        Home => 0xE047,
        ArrowUp => 0xE048,
        PageUp => 0xE049,
        ArrowLeft => 0xE04B,
        ArrowRight => 0xE04D,
        End => 0xE04F,
        ArrowDown => 0xE050,
        PageDown => 0xE051,
        Insert => 0xE052,
        Delete => 0xE053,
        SuperLeft => 0xE05B,
        SuperRight => 0xE05C,
        Menu => 0xE05D,
        _ => return None,
    })
}

/// Keys whose press types a character (sent as text instead, unless a shortcut modifier is held).
fn types_text(key: Key) -> bool {
    use Key::*;
    matches!(
        key,
        A | B | C | D | E | F | G | H | I | J | K | L | M | N | O | P | Q | R | S | T | U | V | W | X | Y | Z
            | Num0 | Num1 | Num2 | Num3 | Num4 | Num5 | Num6 | Num7 | Num8 | Num9
            | Space | Minus | Equals | Plus | OpenBracket | CloseBracket | OpenCurlyBracket | CloseCurlyBracket
            | Backslash | Pipe | Semicolon | Colon | Quote | Backtick | Comma | Period | Slash | Questionmark
            | Exclamationmark | IntlBackslash
    )
}

/// The largest rectangle with the desktop's aspect inside `area`, never enlarged past 1:1.
fn picture_rect(area: Rect, size: (u16, u16), ppp: f32) -> Rect {
    let natural = egui::vec2(f32::from(size.0), f32::from(size.1)) / ppp;
    if natural.x <= 0.0 || natural.y <= 0.0 {
        return area;
    }
    let scale = (area.width() / natural.x).min(area.height() / natural.y).min(1.0);
    Rect::from_center_size(area.center(), natural * scale)
}

/// Shows the desktop of `session` in all remaining space and sends input while focused.
pub fn show(ui: &mut egui::Ui, session: &mut Session, autofocus: bool) -> ViewOutput {
    let rect = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    if response.clicked() || response.secondary_clicked() || (autofocus && ui.memory(|m| m.focused().is_none())) {
        response.request_focus();
    }
    let behind_modal = !ui.memory(|m| m.allows_interaction(ui.layer_id()));
    let focused = response.has_focus() && !behind_modal && ui.input(|i| i.focused);
    if response.has_focus() {
        // Tab, arrows and Escape belong to the remote desktop.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
    }

    let ppp = ui.ctx().pixels_per_point();
    let tab_px = ((rect.width() * ppp) as u16, (rect.height() * ppp) as u16);
    let fits_tab = session.rdp_fits_tab();
    let screen = &mut session.rdp;
    screen.view_px = tab_px;

    if let Some(frame) = screen.pending.take() {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [usize::from(frame.width), usize::from(frame.height)],
            &frame.rgba,
        );
        match &mut screen.texture {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => {
                let name = format!("rdp-{}", session.id);
                screen.texture = Some(ui.ctx().load_texture(name, image, TextureOptions::LINEAR));
            }
        }
        screen.size = (frame.width, frame.height);
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::BLACK);
    let picture = picture_rect(rect, screen.size, ppp);
    if let Some(texture) = &screen.texture {
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(texture.id(), picture, uv, Color32::WHITE);
    }

    let mut inputs = Vec::new();
    // Desktop coordinates of a point on the screen (clamped to the picture).
    let to_desktop = |pos: egui::Pos2| -> (u16, u16) {
        let rel = (pos - picture.min) / picture.size();
        let x = (rel.x.clamp(0.0, 1.0) * f32::from(screen.size.0.saturating_sub(1))).round() as u16;
        let y = (rel.y.clamp(0.0, 1.0) * f32::from(screen.size.1.saturating_sub(1))).round() as u16;
        (x, y)
    };

    if !behind_modal {
        let events = ui.input(|i| i.events.clone());
        let hovered = response.hovered();
        for event in &events {
            match event {
                Event::PointerMoved(pos) if (hovered || !screen.buttons.is_empty()) && picture.contains(*pos) => {
                    let p = to_desktop(*pos);
                    if screen.last_pos != Some(p) {
                        screen.last_pos = Some(p);
                        inputs.push(RdpInput::MouseMove { x: p.0, y: p.1 });
                    }
                }
                Event::PointerButton { pos, button, pressed, .. } => {
                    let (rdp_button, id) = match button {
                        egui::PointerButton::Primary => (RdpMouseButton::Left, 0),
                        egui::PointerButton::Secondary => (RdpMouseButton::Right, 1),
                        egui::PointerButton::Middle => (RdpMouseButton::Middle, 2),
                        _ => continue,
                    };
                    // Presses on the picture; releases of buttons pressed there wherever they are.
                    if *pressed && picture.contains(*pos) && hovered {
                        screen.buttons.insert(id);
                    } else if *pressed || !screen.buttons.remove(&id) {
                        continue;
                    }
                    let (x, y) = to_desktop(*pos);
                    inputs.push(RdpInput::MouseButton { button: rdp_button, pressed: *pressed, x, y });
                }
                Event::MouseWheel { unit, delta, .. } if hovered => {
                    let per = match unit {
                        egui::MouseWheelUnit::Line => 120.0,
                        egui::MouseWheelUnit::Page => 360.0,
                        egui::MouseWheelUnit::Point => 120.0 / 50.0,
                    };
                    screen.wheel += *delta * per;
                }
                _ => {}
            }
        }
        let (x, y) = screen.last_pos.unwrap_or((0, 0));
        for (amount, horizontal) in [(screen.wheel.y, false), (screen.wheel.x, true)] {
            let notches = amount.trunc();
            if notches != 0.0 {
                // Right is positive for the remote side's horizontal wheel, as for egui.
                let delta = notches.clamp(-255.0, 255.0) as i16;
                inputs.push(RdpInput::Wheel { delta, horizontal, x, y });
            }
        }
        screen.wheel = egui::vec2(screen.wheel.x.fract(), screen.wheel.y.fract());
        if hovered {
            // The remote pointer is drawn into the picture.
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        }

        if focused {
            for event in &events {
                keyboard_event(event, screen, &mut inputs);
            }
        }
    }

    // Losing focus (another window, a dialog) must not leave keys or buttons held remotely.
    if screen.had_focus && !focused {
        for code in std::mem::take(&mut screen.down) {
            inputs.push(RdpInput::Key { scancode: code, pressed: false });
        }
    }
    screen.had_focus = focused;

    if focused {
        ui.ctx().output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: picture,
                cursor_rect: Rect::from_min_size(picture.min, egui::vec2(1.0, 16.0)),
                should_interrupt_composition: false,
            })
        });
    }

    // Follow the tab size once it has settled.
    if fits_tab && screen.size != (0, 0) && tab_px.0 > 0 && tab_px.1 > 0 {
        let want = tab_px;
        let differs = |a: (u16, u16), b: (u16, u16)| a.0.abs_diff(b.0) > 4 || a.1.abs_diff(b.1) > 2;
        if differs(want, screen.size) && screen.requested.is_none_or(|r| differs(r, want)) {
            match screen.resize {
                Some((since, size)) if size == want && since.elapsed() >= RESIZE_DELAY => {
                    screen.resize = None;
                    screen.requested = Some(want);
                    inputs.push(RdpInput::Resize { width: want.0, height: want.1 });
                }
                Some((_, size)) if size == want => ui.ctx().request_repaint_after(RESIZE_DELAY),
                _ => {
                    screen.resize = Some((Instant::now(), want));
                    ui.ctx().request_repaint_after(RESIZE_DELAY);
                }
            }
        } else {
            screen.resize = None;
        }
    }

    session.send_rdp(inputs);
    ViewOutput::default()
}

fn keyboard_event(event: &Event, screen: &mut RdpScreen, inputs: &mut Vec<RdpInput>) {
    let mut press = |code: u16, inputs: &mut Vec<RdpInput>| {
        screen.down.insert(code);
        inputs.push(RdpInput::Key { scancode: code, pressed: true });
    };
    match event {
        Event::Key { key, physical_key, pressed, modifiers, .. } => {
            let key = physical_key.unwrap_or(*key);
            let Some(code) = scan(key) else { return };
            if *pressed {
                let shortcut = modifiers.ctrl || modifiers.alt || modifiers.mac_cmd;
                if !types_text(key) || shortcut {
                    press(code, inputs);
                }
            } else if screen.down.remove(&code) {
                inputs.push(RdpInput::Key { scancode: code, pressed: false });
            }
        }
        Event::Text(text) => inputs.push(RdpInput::Text(text.clone())),
        Event::Ime(egui::ImeEvent::Commit(text)) if !text.is_empty() => inputs.push(RdpInput::Text(text.clone())),
        // egui turns Ctrl+C / X / V into these (the key's release still comes as a key event).
        Event::Copy => press(scancode::C, inputs),
        Event::Cut => press(scancode::X, inputs),
        Event::Paste(_) | Event::PasteImage(_) => press(scancode::V, inputs),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_codes() {
        assert_eq!(scan(Key::A), Some(0x1E));
        assert_eq!(scan(Key::ArrowUp), Some(0xE048));
        assert_eq!(scan(Key::ControlRight), Some(0xE01D));
        assert!(types_text(Key::A) && !types_text(Key::Enter) && !types_text(Key::F5));
    }

    #[test]
    fn picture_fits_and_keeps_aspect() {
        let area = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 500.0));
        // 1:1 when it fits.
        let r = picture_rect(area, (800, 400), 1.0);
        assert_eq!(r.size(), egui::vec2(800.0, 400.0));
        // Scaled down with the same aspect when it does not.
        let r = picture_rect(area, (2000, 2000), 1.0);
        assert_eq!(r.size(), egui::vec2(500.0, 500.0));
        assert_eq!(r.center(), area.center());
    }

    #[test]
    fn typing_and_shortcuts() {
        let mut s = RdpScreen::default();
        let mut out = Vec::new();
        let key = |key, pressed, ctrl| Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers { ctrl, command: ctrl, ..Default::default() },
        };
        // A plain letter types text (the key itself is not sent, nor its release).
        keyboard_event(&key(Key::A, true, false), &mut s, &mut out);
        keyboard_event(&Event::Text("a".into()), &mut s, &mut out);
        keyboard_event(&key(Key::A, false, false), &mut s, &mut out);
        assert_eq!(out, vec![RdpInput::Text("a".into())]);
        out.clear();
        // Ctrl+A goes as keys, released as pressed.
        keyboard_event(&key(Key::ControlLeft, true, true), &mut s, &mut out);
        keyboard_event(&key(Key::A, true, true), &mut s, &mut out);
        keyboard_event(&key(Key::A, false, true), &mut s, &mut out);
        keyboard_event(&key(Key::ControlLeft, false, false), &mut s, &mut out);
        let k = |scancode, pressed| RdpInput::Key { scancode, pressed };
        assert_eq!(out, vec![k(0x1D, true), k(0x1E, true), k(0x1E, false), k(0x1D, false)]);
        assert!(s.down.is_empty());
    }
}
