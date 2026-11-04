//! Windows that tabs were moved out to. They are viewports of this process, so a tab keeps its
//! connection, screen and scrollback when it moves from one window to another: with the tab's
//! "Move to new window", by dragging it out of its window, or by dragging it onto another one.
//!
//! The sessions stay in the app's one list; each says which window it is in. What belongs to a
//! window (its dock, its active tab, its title) is swapped into the app while that window is
//! drawn, so the commands and dialogs work on the window they were given in.

use super::*;

/// The window the app started with. Closing it quits.
pub const MAIN_WINDOW: u64 = 0;

/// How far the new window's corner is from the pointer, in points, when a tab is dropped
/// outside: the pointer lands on the new window's tab bar.
const DROP_OFFSET: egui::Vec2 = egui::vec2(60.0, 12.0);

/// What each window has of its own, swapped into the app while it is drawn.
#[derive(Default)]
pub(super) struct WindowState {
    dock: Dock,
    /// The active tab (an id: indices move as tabs close).
    active: Option<u64>,
    title: String,
}

/// Where a window is on screen, to find the window a tab is dropped on.
#[derive(Default, Clone, Copy)]
pub(super) struct Placement {
    /// The outer rectangle in physical pixels, as of the window's last frame.
    rect: Option<egui::Rect>,
    /// When it last had the keyboard focus: of overlapping windows, the one on top.
    focused_at: Option<Instant>,
}

impl Placement {
    fn update(&mut self, ctx: &egui::Context) {
        let ppp = ctx.pixels_per_point();
        let (rect, focused) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().focused));
        self.rect = rect.map(|r| egui::Rect::from_min_max((r.min.to_vec2() * ppp).to_pos2(), (r.max.to_vec2() * ppp).to_pos2()));
        if focused == Some(true) {
            self.focused_at = Some(Instant::now());
        }
    }
}

/// A window tabs were moved out to.
pub(super) struct TabWindow {
    id: u64,
    viewport: egui::ViewportId,
    builder: egui::ViewportBuilder,
    state: WindowState,
    place: Placement,
    /// A tab drag left this window and ended outside it, where egui did not see the button go up.
    stop_drag: bool,
}

/// A tab being dragged, perhaps out of its window.
#[derive(Clone, Copy)]
pub(super) struct TabDrag {
    session: u64,
    from: u64,
}

/// The windows besides the main one, and the tab drag between them.
#[derive(Default)]
pub(super) struct Windows {
    list: Vec<TabWindow>,
    next_id: u64,
    /// The main window's active tab while another window is drawn.
    main_active: Option<u64>,
    main_place: Placement,
    drag: Option<TabDrag>,
    /// The window under a dragged tab, other than its own: it shows that it takes the tab.
    drop_target: Option<u64>,
    icon: Option<Arc<egui::IconData>>,
}

impl Windows {
    pub(super) fn new() -> Self {
        let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../../assets/playterm.png")).ok().map(Arc::new);
        Self { next_id: MAIN_WINDOW + 1, icon, ..Self::default() }
    }

    #[cfg(test)]
    pub(super) fn count(&self) -> usize {
        self.list.len()
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut TabWindow> {
        self.list.iter_mut().find(|w| w.id == id)
    }

    fn viewport(&self, id: u64) -> Option<egui::ViewportId> {
        if id == MAIN_WINDOW { Some(egui::ViewportId::ROOT) } else { self.list.iter().find(|w| w.id == id).map(|w| w.viewport) }
    }

    /// The window at `pos` (physical pixels on the screen); of overlapping ones, the one focused last.
    fn at(&self, pos: egui::Pos2) -> Option<u64> {
        std::iter::once((MAIN_WINDOW, self.main_place))
            .chain(self.list.iter().map(|w| (w.id, w.place)))
            .filter(|(_, place)| place.rect.is_some_and(|r| r.contains(pos)))
            .max_by_key(|(_, place)| place.focused_at)
            .map(|(id, _)| id)
    }
}

impl PlaytermApp {
    /// Makes the tab with this id the active one of `window`.
    fn activate_in(&mut self, window: u64, id: u64) {
        if window == self.current_window {
            if let Some(i) = self.index_of(id) {
                self.active = i;
            }
        } else if window == MAIN_WINDOW {
            self.windows.main_active = Some(id);
        } else if let Some(w) = self.windows.get_mut(window) {
            w.state.active = Some(id);
        }
    }

    /// Makes the active tab one of the window being drawn, when it no longer is (it closed or
    /// moved): the nearest tab before it, or else the first.
    pub(super) fn fix_active(&mut self) {
        let window = self.current_window;
        if self.sessions.get(self.active).is_some_and(|s| s.window == window) {
            return;
        }
        let end = self.active.min(self.sessions.len());
        let before = self.sessions[..end].iter().rposition(|s| s.window == window);
        if let Some(i) = before.or_else(|| self.sessions.iter().position(|s| s.window == window)) {
            self.active = i;
        }
    }

    /// The main window always has a tab: a prompt when its last one closed or moved away.
    pub(super) fn ensure_main_tab(&mut self) {
        if self.current_window == MAIN_WINDOW && !self.sessions.iter().any(|s| s.window == MAIN_WINDOW) {
            self.open_local();
        }
    }

    /// The tabs of the window being drawn, in tab order.
    pub(super) fn window_tabs(&self) -> Vec<usize> {
        (0..self.sessions.len()).filter(|&i| self.sessions[i].window == self.current_window).collect()
    }

    /// Brings window `window` to the front.
    pub(super) fn focus_window(&self, ctx: &egui::Context, window: u64) {
        if let Some(viewport) = self.windows.viewport(window) {
            ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Focus);
        }
    }

    /// Makes tab `i` the active one of its window, and brings that window to the front.
    pub(super) fn show_tab_in(&mut self, ctx: &egui::Context, i: usize) {
        let Some(s) = self.sessions.get(i) else { return };
        let (window, id) = (s.window, s.id);
        self.activate_in(window, id);
        self.focus_window(ctx, window);
    }

    /// Moves tab `i` to window `to`, where it becomes the active tab.
    pub(super) fn move_tab(&mut self, ctx: &egui::Context, i: usize, to: u64) {
        let Some(session) = self.sessions.get_mut(i) else { return };
        if session.window == to {
            return;
        }
        let id = session.id;
        session.window = to;
        // Keys and buttons held down in the old window are not released in the new one.
        session.release_rdp_input();
        self.activate_in(to, id);
        self.fix_active();
        self.ensure_main_tab();
        self.focus_window(ctx, to);
    }

    /// Moves tab `i` to a new window, with its top left corner at `at` (screen points) if given.
    pub(super) fn detach_tab(&mut self, ctx: &egui::Context, i: usize, at: Option<egui::Pos2>) {
        if i >= self.sessions.len() {
            return;
        }
        let id = self.windows.next_id;
        self.windows.next_id += 1;
        let (inner, outer) = ctx.input(|inp| (inp.viewport().inner_rect, inp.viewport().outer_rect));
        let size = inner.map_or(egui::vec2(1000.0, 640.0), |r| r.size());
        let at = at.or_else(|| outer.map(|r| r.min + egui::vec2(40.0, 40.0)));
        let mut builder = egui::ViewportBuilder::default().with_title("playterm").with_inner_size(size).with_min_inner_size([420.0, 260.0]);
        if let Some(at) = at {
            builder = builder.with_position(at);
        }
        if let Some(icon) = &self.windows.icon {
            builder = builder.with_icon(icon.clone());
        }
        let window = TabWindow {
            id,
            viewport: egui::ViewportId::from_hash_of(("playterm-window", id)),
            builder,
            state: WindowState { dock: Dock::new(id), active: None, title: String::new() },
            place: Placement::default(),
            stop_drag: false,
        };
        self.windows.list.push(window);
        self.move_tab(ctx, i, id);
    }

    /// Swaps the state of window `id` into the app, to draw it.
    fn enter_window(&mut self, id: u64) {
        let Some(w) = self.windows.list.iter_mut().find(|w| w.id == id) else { return };
        std::mem::swap(&mut self.dock, &mut w.state.dock);
        std::mem::swap(&mut self.window_title, &mut w.state.title);
        let active = w.state.active;
        self.current_window = id;
        self.active = active.and_then(|a| self.index_of(a)).unwrap_or(self.active);
        self.fix_active();
    }

    /// Swaps the state of window `id` back out of the app.
    fn leave_window(&mut self, id: u64) {
        let active = self.sessions.get(self.active).filter(|s| s.window == id).map(|s| s.id);
        let Some(w) = self.windows.list.iter_mut().find(|w| w.id == id) else { return };
        std::mem::swap(&mut self.dock, &mut w.state.dock);
        std::mem::swap(&mut self.window_title, &mut w.state.title);
        w.state.active = active.or(w.state.active);
        self.current_window = MAIN_WINDOW;
    }

    /// Draws the windows tabs were moved out to (after the main window, in its frame), and
    /// finishes a tab drag that ended outside the window it started in.
    pub(super) fn show_windows(&mut self, ctx: &egui::Context) {
        self.windows.main_place.update(ctx);
        // A window whose last tab closed or moved away closes.
        let sessions = &self.sessions;
        self.windows.list.retain(|w| sessions.iter().any(|s| s.window == w.id));

        let ids: Vec<u64> = self.windows.list.iter().map(|w| w.id).collect();
        if !ids.is_empty() {
            self.windows.main_active = self.sessions.get(self.active).map(|s| s.id);
            for id in ids {
                let Some(w) = self.windows.list.iter().find(|w| w.id == id) else { continue };
                let (viewport, builder) = (w.viewport, w.builder.clone());
                ctx.show_viewport_immediate(viewport, builder, |ui, _| self.window_ui(ui, id));
            }
            self.current_window = MAIN_WINDOW;
            if let Some(i) = self.windows.main_active.take().and_then(|id| self.index_of(id)) {
                self.active = i;
            }
            self.fix_active();
            self.ensure_main_tab();
        }
        self.finish_tab_drag(ctx);
        if self.windows.drop_target == Some(MAIN_WINDOW) {
            paint_drop_target(ctx);
        }
    }

    /// One frame of window `id`: its tabs and the dialogs of its sessions.
    fn window_ui(&mut self, ui: &mut egui::Ui, id: u64) {
        let ctx = ui.ctx().clone();
        self.enter_window(id);
        if ctx.input(|i| i.viewport().close_requested()) {
            // Closing the window gives its tabs back to the main window, connections and all.
            let mut back = None;
            for s in self.sessions.iter_mut().filter(|s| s.window == id) {
                s.window = MAIN_WINDOW;
                s.release_rdp_input();
                back = back.or(Some(s.id));
            }
            if let Some(active) = self.sessions.get(self.active).map(|s| s.id).or(back) {
                self.windows.main_active = Some(active);
            }
            self.leave_window(id);
            return;
        }
        if let Some(w) = self.windows.get_mut(id) {
            w.place.update(&ctx);
            if std::mem::take(&mut w.stop_drag) {
                ctx.stop_dragging();
            }
        }
        // The main window does this in `raw_input_hook`, which eframe calls for it alone.
        if !self.dock.has_focus() {
            ctx.input_mut(|i| clip_keys::widget_shortcuts(&mut i.events, clipboard::read_text));
        }

        self.console_requests();
        let dialogs_open = self.modal.is_some() || self.site_manager.is_some();
        let prompt_open = self.show_session_prompts(&ctx);
        if !prompt_open {
            self.shortcuts(&ctx);
        }
        if self.settings.status_bar {
            let session = self.sessions.get(self.active);
            let awake = status_bar::Awake { sleep: self.sleep_guard.active(), screensaver: self.screensaver_guard.active() };
            egui::Panel::bottom("status").show(ui, |ui| status_bar::show(ui, session, awake));
        }
        let style = self.view_style();
        let output = egui::CentralPanel::no_frame()
            .show(ui, |ui| self.dock.show(ui, &mut self.sessions, self.active, !prompt_open, self.broadcast, &style))
            .inner;
        self.dock_output(&ctx, output);
        // A dialog opened from here (by a tab's menu) is shown in the main window.
        if !dialogs_open && (self.modal.is_some() || self.site_manager.is_some()) {
            self.focus_window(&ctx, MAIN_WINDOW);
        }
        if self.windows.drop_target == Some(id) {
            paint_drop_target(&ctx);
        }
        self.update_window_title(&ctx);
        self.leave_window(id);
    }

    /// Keeps track of a tab dragged in the window being drawn.
    pub(super) fn track_tab_drag(&mut self, dragging: Option<u64>) {
        match dragging {
            Some(session) => self.windows.drag = Some(TabDrag { session, from: self.current_window }),
            // The drag ended inside its window (dropped there, or Escape).
            None if self.windows.drag.is_some_and(|d| d.from == self.current_window) => self.windows.drag = None,
            None => {}
        }
    }

    /// A tab drag that left its window: egui stops seeing the pointer there, so the button and
    /// the pointer are read from the system. Dropped on another window, the tab moves there;
    /// dropped outside every window, it gets a new window.
    fn finish_tab_drag(&mut self, ctx: &egui::Context) {
        self.windows.drop_target = None;
        let Some(drag) = self.windows.drag else { return };
        let Some((pos, down)) = crate::platform::global_pointer() else {
            self.windows.drag = None;
            return;
        };
        let target = self.windows.at(pos);
        if down {
            self.windows.drop_target = target.filter(|&w| w != drag.from);
            // Nothing else wakes the app while the pointer is outside the window.
            ctx.request_repaint();
            return;
        }
        self.windows.drag = None;
        let Some(i) = self.index_of(drag.session).filter(|&i| self.sessions[i].window == drag.from) else { return };
        if target == Some(drag.from) {
            // Dropped back in its own window, where egui took care of it.
            return;
        }
        let alone = self.sessions.iter().filter(|s| s.window == drag.from).count() == 1;
        if target.is_none() && alone && drag.from != MAIN_WINDOW {
            // The only tab of its window: dropping it outside would just make the same window again.
            return;
        }
        if drag.from == MAIN_WINDOW {
            ctx.stop_dragging();
        } else if let Some(w) = self.windows.get_mut(drag.from) {
            w.stop_drag = true;
        }
        match target {
            Some(to) => self.move_tab(ctx, i, to),
            None => {
                let scale = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0);
                self.detach_tab(ctx, i, Some((pos.to_vec2() / scale).to_pos2() - DROP_OFFSET));
            }
        }
    }
}

/// The frame of a window a dragged tab is over: dropping it there moves the tab to this window.
fn paint_drop_target(ctx: &egui::Context) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("playterm-tab-drop")));
    let color = ctx.global_style().visuals.selection.stroke.color;
    painter.rect_stroke(ctx.content_rect().shrink(2.0), 0.0, egui::Stroke::new(3.0, color), egui::StrokeKind::Inside);
}
