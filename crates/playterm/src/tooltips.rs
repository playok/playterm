//! A tooltip left over when a dialog opens (#153).
//!
//! egui keeps a tooltip that is open while the pointer stays on its widget, also once a modal
//! dialog covers that widget: it does not ask whether the widget is still hovered. One frame
//! without a pointer closes it; the frame after puts the pointer back where it was, and from
//! then on the dialog's backdrop keeps the widgets below it from being hovered.

use eframe::egui::{self, Event, Pos2};

/// Where closing the tooltips is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TooltipReset {
    #[default]
    Idle,
    /// The next frame goes without a pointer.
    Hide(Pos2),
    /// The pointer comes back here (unless it moved meanwhile).
    Restore(Pos2),
}

impl TooltipReset {
    /// A dialog opened this frame.
    pub fn start(&mut self, ctx: &egui::Context) {
        if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
            *self = Self::Hide(pos);
            ctx.request_repaint();
        }
    }

    /// Changes the input of a frame about to run (from `raw_input_hook`).
    pub fn apply(&mut self, ctx: &egui::Context, events: &mut Vec<Event>) {
        match std::mem::take(self) {
            Self::Idle => {}
            Self::Hide(pos) => {
                let pos = events.iter().rev().find_map(moved_to).unwrap_or(pos);
                events.push(Event::PointerGone);
                *self = Self::Restore(pos);
                ctx.request_repaint();
            }
            Self::Restore(pos) => {
                if !events.iter().any(|e| matches!(e, Event::PointerMoved(_) | Event::PointerGone)) {
                    events.insert(0, Event::PointerMoved(pos));
                }
            }
        }
    }
}

fn moved_to(event: &Event) -> Option<Pos2> {
    match event {
        Event::PointerMoved(p) => Some(*p),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A label with a tooltip at the bottom, under a modal dialog from frame `modal_from`.
    /// Returns, for each frame, whether the tooltip was drawn.
    fn frames(reset: bool) -> Vec<bool> {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        let pointer = egui::pos2(20.0, 590.0);
        let mut tooltip_reset = TooltipReset::Idle;
        let mut shown = Vec::new();
        let mut was_open = false;
        for k in 0..14 {
            let open = k >= 6;
            let mut input = egui::RawInput { time: Some(0.2 * f64::from(k + 1)), screen_rect: Some(screen), ..Default::default() };
            if k == 0 {
                input.events.push(Event::PointerMoved(pointer));
            }
            if reset {
                tooltip_reset.apply(&ctx, &mut input.events);
            }
            let mut out = ctx.run_ui(input, |ui| {
                egui::Panel::bottom("status").show(ui, |ui| {
                    ui.label("LABEL").on_hover_text("TIPTEXT");
                });
                if open {
                    egui::Modal::new(egui::Id::unique("dialog")).show(ui.ctx(), |ui| ui.label("dialog"));
                }
                if open && !was_open && reset {
                    tooltip_reset.start(ui.ctx());
                }
            });
            was_open = open;
            out.textures_delta.clear();
            shown.push(format!("{:?}", out.shapes).contains("TIPTEXT"));
        }
        shown
    }

    #[test]
    fn a_dialog_closes_the_tooltip_under_it() {
        let before = frames(false);
        assert!(before[5], "the tooltip shows before the dialog: {before:?}");
        // What egui does by itself: the tooltip stays on top of the dialog.
        assert!(before[13], "egui keeps it: {before:?}");
        let after = frames(true);
        assert!(after[5], "{after:?}");
        assert!(!after[8..].iter().any(|&s| s), "gone once the dialog is open: {after:?}");
    }
}
