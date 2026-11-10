//! The tabs and their docking layout: every session is a pane of an [`egui_tiles::Tree`], so a
//! tab can be dragged out of its tab bar and dropped beside, above or below another group of
//! tabs (or into it) to see several sessions at once. Groups and splits are resized by
//! dragging the gaps between them.
//!
//! The sessions themselves stay in the app's list (its order is the tab order of shortcuts and
//! menus); the tree only holds their ids and is brought in line with the list every frame.
//! Every window has a dock of its own, holding the sessions that are in that window.

use eframe::egui::{self, Color32, Id, Rect, Response, RichText, Sense, Stroke, StrokeKind, TextStyle, Ui, WidgetText};
use egui_tiles::{Behavior, Container, ContainerKind, SimplificationOptions, TabState, Tile, TileId, Tiles, UiResponse};
use playterm_config::TemplateLayout;
use playterm_i18n::t;

use crate::icon::{self, MaterialIcon};
use crate::menu::{self, Command};
use crate::rdp_view;
use crate::session::{Session, SessionState};
use crate::view::{self, ViewOutput, ViewStyle};

/// Width of the frame around the panes that take part in simultaneous input.
const BROADCAST_FRAME: f32 = 2.0;
/// Longest tab title; longer ones end in "…".
const TAB_TITLE_MAX: f32 = 180.0;
/// Size of a tab's ✕.
const CLOSE_SIZE: f32 = 16.0;

/// Black or white, whichever reads better on `background`.
pub fn on_color(background: Color32) -> Color32 {
    let [r, g, b, _] = egui::Rgba::from(background).to_array();
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if luminance > 0.179 { Color32::BLACK } else { Color32::WHITE }
}

/// How a tab shows its connection state: a shape as well as a color, so the states stay
/// apart without telling red from green, and a word in the tooltip.
struct StateMark {
    icon: MaterialIcon,
    color: Color32,
    label: &'static str,
}

impl StateMark {
    fn of(state: &SessionState) -> Self {
        let (icon, color, label) = match state {
            SessionState::Local => (icon::STATE_LOCAL, Color32::from_rgb(140, 140, 140), t!("common.local_prompt")),
            SessionState::Connecting => (icon::STATE_CONNECTING, Color32::from_rgb(220, 180, 40), t!("common.connecting")),
            SessionState::Connected => (icon::STATE_CONNECTED, Color32::from_rgb(60, 190, 90), t!("common.connected")),
            SessionState::Closed => (icon::STATE_CLOSED, Color32::from_rgb(200, 70, 70), t!("common.disconnected")),
        };
        Self { icon, color, label }
    }
}

/// The docking layout of the session tabs.
pub struct Dock {
    tree: egui_tiles::Tree<u64>,
    /// The window this dock is in.
    window: u64,
    /// The active session as last shown in the tree (its tab group gets new tabs).
    shown_active: Option<u64>,
    /// The pane that had keyboard focus last frame.
    focused: Option<u64>,
    /// Sessions just opened together, to lay out once their tabs exist.
    arrange: Option<(Vec<u64>, TemplateLayout)>,
}

/// What the tabs and panes did this frame.
#[derive(Default)]
pub struct DockOutput {
    /// A menu entry or button of a tab.
    pub command: Option<Command>,
    /// A session to make the active one (its tab or pane was clicked, or it was dropped).
    pub activate: Option<u64>,
    /// Typed input and zoom from the panes.
    pub view: ViewOutput,
    /// Sessions drawn this frame.
    pub shown: Vec<u64>,
    /// Size in pixels of the active session's pane.
    pub active_px: Option<(u16, u16)>,
    /// The session whose tab is being dragged (perhaps out of the window).
    pub dragging: Option<u64>,
}

impl Default for Dock {
    fn default() -> Self {
        Self::new(crate::app::MAIN_WINDOW)
    }
}

impl Dock {
    /// The dock of window `window`: it shows the sessions in that window.
    pub fn new(window: u64) -> Self {
        let tree = egui_tiles::Tree::empty(Id::unique(("playterm-dock", window)));
        Self { tree, window, shown_active: None, focused: None, arrange: None }
    }

    /// Adds tabs for new sessions (to the group of the active one), drops those of closed ones
    /// (and of those moved to another window) and brings the active session's tab to the front.
    fn sync(&mut self, sessions: &[Session], active: Option<u64>) {
        let tiles = &self.tree.tiles;
        let window = self.window;
        let gone: Vec<TileId> = tiles
            .iter()
            .filter_map(|(&tile, t)| match t {
                Tile::Pane(id) if !sessions.iter().any(|s| s.id == *id && s.window == window) => Some(tile),
                _ => None,
            })
            .collect();
        for tile in gone {
            self.tree.remove_recursively(tile);
        }

        let mut group = self.group_of(self.shown_active).or_else(|| self.group_of(active)).or_else(|| self.any_group());
        for s in sessions.iter().filter(|s| s.window == window) {
            if self.tree.tiles.find_pane(&s.id).is_some() {
                continue;
            }
            let tile = self.tree.tiles.insert_pane(s.id);
            match group.and_then(|g| self.tree.tiles.get_mut(g)) {
                Some(Tile::Container(Container::Tabs(tabs))) => tabs.add_child(tile),
                _ => {
                    let tabs = self.tree.tiles.insert_tab_tile(vec![tile]);
                    group = Some(tabs);
                    self.tree.root = Some(match self.tree.root {
                        Some(root) => self.tree.tiles.insert_horizontal_tile(vec![root, tabs]),
                        None => tabs,
                    });
                }
            }
        }

        if active != self.shown_active {
            if let Some(id) = active {
                self.tree.make_active(|_, tile| matches!(tile, Tile::Pane(p) if *p == id));
            }
            self.shown_active = active;
        }
    }

    /// Whether a terminal or remote desktop had the keyboard focus last frame.
    pub fn has_focus(&self) -> bool {
        self.focused.is_some()
    }

    /// Lays out these sessions (opened together, so tabs of one group) side by side, stacked or in
    /// a grid on the next frame.
    pub fn arrange(&mut self, ids: Vec<u64>, layout: TemplateLayout) {
        self.arrange = Some((ids, layout));
    }

    /// Gives each pane of `ids` a group of its own, arranged in the place of the group they were
    /// in, which they share evenly: the other tabs of that group go behind the first one (#142)
    /// instead of keeping half the room beside them.
    fn apply_arrange(&mut self, ids: &[u64], layout: TemplateLayout) {
        let kind = match layout {
            TemplateLayout::Tabs => return,
            TemplateLayout::Columns => ContainerKind::Horizontal,
            TemplateLayout::Rows => ContainerKind::Vertical,
            TemplateLayout::Grid => ContainerKind::Grid,
        };
        let tiles = &mut self.tree.tiles;
        let panes: Vec<TileId> = ids.iter().filter_map(|id| tiles.find_pane(id)).collect();
        if panes.len() < 2 {
            return;
        }
        let Some(home) = tiles.parent_of(panes[0]) else { return };
        let home_parent = tiles.parent_of(home);
        for &pane in &panes {
            if let Some(parent) = tiles.parent_of(pane)
                && let Some(Tile::Container(container)) = tiles.get_mut(parent)
            {
                container.remove_child(pane);
            }
        }
        // The tabs left in the group join the first new one, behind its session.
        let rest: Vec<TileId> = match tiles.get(home) {
            Some(Tile::Container(c)) => c.children().copied().collect(),
            _ => Vec::new(),
        };
        if let Some(Tile::Container(c)) = tiles.get_mut(home) {
            for &child in &rest {
                c.remove_child(child);
            }
        }
        let mut first = vec![panes[0]];
        first.extend(rest);
        let mut groups = vec![tiles.insert_tab_tile(first)];
        groups.extend(panes[1..].iter().map(|&pane| tiles.insert_tab_tile(vec![pane])));
        if let Some(Tile::Container(Container::Tabs(tabs))) = tiles.get_mut(groups[0]) {
            tabs.set_active(panes[0]);
        }
        let arranged = tiles.insert_container(Container::new(kind, groups));
        match home_parent {
            Some(parent) => {
                if let Some(Tile::Container(container)) = tiles.get_mut(parent) {
                    let _ = container.replace_child(home, arranged);
                }
            }
            None => self.tree.root = Some(arranged),
        }
        tiles.remove(home);
    }

    /// The tab group holding the session `id`.
    fn group_of(&self, id: Option<u64>) -> Option<TileId> {
        let tile = self.tree.tiles.find_pane(&id?)?;
        let parent = self.tree.tiles.parent_of(tile)?;
        matches!(self.tree.tiles.get(parent), Some(Tile::Container(Container::Tabs(_)))).then_some(parent)
    }

    fn any_group(&self) -> Option<TileId> {
        self.tree.tiles.iter().find(|(_, t)| matches!(t, Tile::Container(Container::Tabs(_)))).map(|(&id, _)| id)
    }

    /// More than one tab group is on screen.
    fn is_split(&self) -> bool {
        self.tree.tiles.iter().filter(|(id, t)| matches!(t, Tile::Container(Container::Tabs(_))) && self.tree.is_visible(**id)).count() > 1
    }

    /// Shows the tab groups and the sessions in them in all remaining space.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        sessions: &mut [Session],
        active: usize,
        autofocus: bool,
        broadcast: bool,
        style: &ViewStyle,
    ) -> DockOutput {
        let active_id = sessions.get(active).map(|s| s.id);
        self.sync(sessions, active_id);
        if let Some((ids, layout)) = self.arrange.take() {
            self.apply_arrange(&ids, layout);
        }
        // The active session changed (a shortcut, a new tab): its pane takes the keyboard from the
        // pane that had it.
        if self.focused.is_some() && self.focused != active_id {
            ui.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        let titles = Session::numbered_titles(sessions);
        let dragged = self.tree.dragged_id(ui.ctx()).and_then(|t| self.tree.tiles.get_pane(&t).copied());
        let mut behavior = DockView {
            titles,
            sessions,
            detached: self.window != crate::app::MAIN_WINDOW,
            active: active_id,
            active_tile: active_id.and_then(|id| self.tree.tiles.find_pane(&id)),
            outline: self.is_split(),
            autofocus,
            broadcast,
            style,
            dropped: false,
            focused: None,
            out: DockOutput::default(),
        };
        self.tree.ui(&mut behavior, ui);
        let mut out = behavior.out;
        out.dragging = self.tree.dragged_id(ui.ctx()).and_then(|t| self.tree.tiles.get_pane(&t).copied());
        if behavior.dropped {
            // A tab dragged somewhere else is the one the user works with next.
            out.activate = out.activate.or(dragged);
        }
        // A pane that just got the keyboard (clicked) becomes the active session.
        if behavior.focused.is_some() && behavior.focused != self.focused {
            out.activate = out.activate.or(behavior.focused);
        }
        self.focused = behavior.focused;
        out
    }
}

/// Draws the tabs and panes for [`egui_tiles`].
struct DockView<'a> {
    sessions: &'a mut [Session],
    /// The sessions' titles, numbered where they repeat (in `sessions` order).
    titles: Vec<String>,
    /// The dock of a window the tabs were moved out to (not the main window).
    detached: bool,
    active: Option<u64>,
    active_tile: Option<TileId>,
    /// Outline the active pane (when more than one group is on screen).
    outline: bool,
    autofocus: bool,
    broadcast: bool,
    style: &'a ViewStyle,
    dropped: bool,
    focused: Option<u64>,
    out: DockOutput,
}

impl DockView<'_> {
    fn index(&self, id: u64) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }
}

impl Behavior<u64> for DockView<'_> {
    fn pane_ui(&mut self, ui: &mut Ui, _tile: TileId, pane: &mut u64) -> UiResponse {
        let id = *pane;
        let Some(i) = self.index(id) else { return UiResponse::None };
        let is_active = self.active == Some(id);
        if is_active {
            let area = ui.available_rect_before_wrap();
            let ppp = ui.ctx().pixels_per_point();
            self.out.active_px = Some(((area.width() * ppp) as u16, (area.height() * ppp) as u16));
        }
        let pane_rect = ui.available_rect_before_wrap();
        let session = &mut self.sessions[i];
        let autofocus = self.autofocus && is_active;
        let output =
            if session.shows_rdp() { rdp_view::show(ui, session, autofocus) } else { view::show(ui, session, autofocus, self.style) };
        if output.focused {
            self.focused = Some(id);
        }
        // What is typed goes to every pane in an orange frame: hard to miss while it is on.
        // Only panes that take the input get the full frame; one included but not connected
        // gets a faint one, as its title stays plain (#164).
        if self.broadcast && session.in_broadcast {
            let stroke = if session.state == SessionState::Connected {
                Stroke::new(BROADCAST_FRAME, crate::app::theme::broadcast_color(ui.visuals()))
            } else {
                Stroke::new(1.0, crate::app::theme::broadcast_color(ui.visuals()).gamma_multiply(0.35))
            };
            ui.painter().rect_stroke(pane_rect, 0.0, stroke, StrokeKind::Inside);
        }
        self.out.view.zoom_steps += output.zoom_steps;
        if output.command.is_some() {
            self.out.command = output.command;
        }
        if output.reconnect {
            self.out.command = Some(Command::Reconnect(i));
        }
        self.out.view.inputs.extend(output.inputs);
        self.out.shown.push(id);
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &u64) -> WidgetText {
        self.index(*pane).map(|i| self.titles[i].clone()).unwrap_or_default().into()
    }

    fn tab_ui(&mut self, tiles: &mut Tiles<u64>, ui: &mut Ui, id: Id, tile_id: TileId, state: &TabState) -> Response {
        let Some(i) = tiles.get_pane(&tile_id).and_then(|&p| self.index(p)) else {
            // A group dropped into a tab bar shows as one tab of its own.
            let response = ui.add(egui::Button::selectable(state.active, "…").sense(Sense::click_and_drag()));
            return ui.interact(response.rect, id, Sense::click_and_drag());
        };
        let s = &self.sessions[i];
        let focused = self.active == Some(s.id);
        let receiving = self.broadcast && s.in_broadcast && s.state == SessionState::Connected;

        let font = TextStyle::Button.resolve(ui.style());
        let mark = StateMark::of(&s.state);
        let text_color = if receiving {
            crate::app::theme::broadcast_color(ui.visuals())
        } else if s.state == SessionState::Closed && !state.active {
            // A closed tab in the background takes no input: its title steps back. (The tab on
            // show keeps a readable title on its highlight; its icon says it is closed.)
            ui.visuals().weak_text_color()
        } else if state.active {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().text_color()
        };
        // Icon, label chip, REC, title: the title alone is cut short ("…") past
        // [`TAB_TITLE_MAX`] (#130); the full one is in the tooltip.
        let title_galley = WidgetText::from(RichText::new(&self.titles[i]).color(text_color)).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            TAB_TITLE_MAX,
            font.clone(),
        );
        let truncated = title_galley.elided;
        let mark_galley = WidgetText::from(mark.icon.rich_text().color(mark.color)).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            font.clone(),
        );
        // The site's label on a chip of its color, in black or white (whichever reads), so it
        // stays readable on the tinted tab (#156).
        let chip = s.site_label().zip(s.tab_color.map(menu::color32)).map(|(label, color)| {
            let galley = WidgetText::from(RichText::new(label).strong().color(on_color(color))).into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                font.clone(),
            );
            (galley, color)
        });
        let plain_label = if chip.is_none() {
            s.site_label().map(|label| {
                WidgetText::from(RichText::new(label).strong().color(ui.visuals().strong_text_color())).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    font.clone(),
                )
            })
        } else {
            None
        };
        let rec = s.is_logging().then(|| {
            WidgetText::from(RichText::new("REC").color(Color32::from_rgb(220, 60, 60))).into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                TextStyle::Small.resolve(ui.style()),
            )
        });
        let gap = 5.0;
        let chip_pad = 4.0;
        let margin = 8.0;
        let mut width = margin + mark_galley.size().x + gap;
        if let Some((g, _)) = &chip {
            width += g.size().x + 2.0 * chip_pad + gap;
        }
        if let Some(g) = &plain_label {
            width += g.size().x + gap;
        }
        if let Some(g) = &rec {
            width += g.size().x + gap;
        }
        // Room for the ✕ is always kept, so the tab does not change width when it shows.
        width += title_galley.size().x + gap + CLOSE_SIZE + margin / 2.0;
        let (_, rect) = ui.allocate_space(egui::vec2(width, ui.available_height()));
        let response = ui.interact(rect, id, Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::Grab);
        // The ✕: only on the tab under the pointer and the active one, so it is found where
        // expected but not hit by mistake on the others (#129).
        let close_rect = Rect::from_center_size(
            egui::pos2(rect.right() - margin / 2.0 - CLOSE_SIZE / 2.0, rect.center().y),
            egui::vec2(CLOSE_SIZE, CLOSE_SIZE),
        );
        let show_close = (response.hovered() || focused || ui.rect_contains_pointer(close_rect)) && !state.is_being_dragged;
        let close = show_close.then(|| ui.interact(close_rect, id.with("close"), Sense::click()));

        if ui.is_rect_visible(rect) && !state.is_being_dragged {
            let visuals = ui.visuals();
            let fill = if focused {
                visuals.selection.bg_fill
            } else if state.active {
                visuals.widgets.inactive.weak_bg_fill
            } else if response.hovered() {
                visuals.widgets.hovered.weak_bg_fill
            } else {
                visuals.panel_fill
            };
            let painter = ui.painter();
            painter.rect(rect.shrink(1.0), 4.0, fill, Stroke::NONE, StrokeKind::Inside);
            if let Some(color) = s.tab_color.map(menu::color32) {
                painter.rect_filled(rect.shrink(1.0), 4.0, color.gamma_multiply(0.3));
                let bar = Rect::from_min_max(
                    egui::pos2(rect.left() + 3.0, rect.bottom() - 4.0),
                    egui::pos2(rect.right() - 3.0, rect.bottom() - 1.0),
                );
                painter.rect_filled(bar, 1.5, color);
            }
            let mut x = rect.left() + margin;
            let draw = |galley: std::sync::Arc<egui::Galley>, x: &mut f32| {
                let pos = egui::pos2(*x, rect.center().y - galley.size().y / 2.0);
                *x += galley.size().x + gap;
                painter.galley(pos, galley, text_color);
            };
            draw(mark_galley, &mut x);
            if let Some((galley, color)) = chip {
                let size = galley.size() + egui::vec2(2.0 * chip_pad, 2.0);
                let chip_rect = Rect::from_min_size(egui::pos2(x, rect.center().y - size.y / 2.0), size);
                painter.rect_filled(chip_rect, 3.0, color);
                x += chip_pad;
                draw(galley, &mut x);
                x += chip_pad;
            }
            if let Some(galley) = plain_label {
                draw(galley, &mut x);
            }
            if let Some(galley) = rec {
                draw(galley, &mut x);
            }
            draw(title_galley, &mut x);
            if let Some(close) = &close {
                let color = if close.hovered() { visuals.strong_text_color() } else { visuals.weak_text_color() };
                if close.hovered() {
                    painter.rect_filled(close_rect, 3.0, visuals.widgets.hovered.bg_fill);
                }
                painter.text(close_rect.center(), egui::Align2::CENTER_CENTER, "✕", TextStyle::Small.resolve(ui.style()), color);
            }
        }

        if close.as_ref().is_some_and(Response::clicked) || response.middle_clicked() {
            self.out.command = Some(Command::Close(i));
        } else if response.clicked() {
            self.out.activate = Some(s.id);
        }

        let command = &mut self.out.command;
        response.context_menu(|ui| {
            menu::session_items(ui, command, i, s);
            ui.separator();
            let item = egui::Button::new((menu::lead(ui, s.in_broadcast), t!("dock.include_in_simultaneous_input")));
            if ui.add(item).clicked() {
                *command = Some(Command::ToggleInBroadcast(i));
                ui.close();
            }
            menu::tab_color_menu(ui, command, i, s.tab_color);
            ui.separator();
            let mut entries =
                vec![(t!("common.duplicate"), Command::Duplicate(i)), (t!("common.move_to_new_window"), Command::OpenInNewWindow(i))];
            if self.detached {
                entries.push((t!("dock.move_to_main_window"), Command::MoveToMain(i)));
            }
            entries.push((t!("common.reconnect"), Command::Reconnect(i)));
            for (label, c) in entries {
                if ui.add(egui::Button::new((menu::lead(ui, false), label))).clicked() {
                    *command = Some(c);
                    ui.close();
                }
            }
            if ui.add_enabled(s.can_disconnect(), egui::Button::new((menu::lead(ui, false), t!("common.disconnect")))).clicked() {
                *command = Some(Command::Disconnect(i));
                ui.close();
            }
            ui.separator();
            for (label, c) in [(t!("common.close"), Command::Close(i)), (t!("common.close_other_tabs"), Command::CloseOthers(i))] {
                if ui.add(egui::Button::new((menu::lead(ui, false), label))).clicked() {
                    *command = Some(c);
                    ui.close();
                }
            }
        });
        // What the tab is, not how to drag it (#131; that is in Help → Keyboard shortcuts): its
        // state and where it connects, with the full title when the tab shows it cut short.
        let what = if s.has_target() { format!("{} · {}", mark.label, s.target()) } else { mark.label.to_owned() };
        response.on_hover_text(if truncated { format!("{}\n{what}", self.titles[i]) } else { what })
    }

    /// A "▾" at the right of a tab bar with several tabs: the list of them, to pick one when the
    /// bar is too narrow to show them all (#130).
    fn top_bar_right_ui(&mut self, tiles: &Tiles<u64>, ui: &mut Ui, _tile_id: TileId, tabs: &egui_tiles::Tabs, _scroll: &mut f32) {
        let entries: Vec<(u64, usize)> =
            tabs.children.iter().filter_map(|c| tiles.get_pane(c)).filter_map(|&p| self.index(p).map(|i| (p, i))).collect();
        if entries.len() < 2 {
            return;
        }
        let list = ui.menu_button("▾", |ui| {
            for (id, i) in entries {
                let mark = StateMark::of(&self.sessions[i].state);
                let item = icon::button(mark.icon, self.titles[i].as_str()).selected(self.active == Some(id));
                if ui.add(item).clicked() {
                    self.out.activate = Some(id);
                    ui.close();
                }
            }
        });
        list.response.on_hover_text(t!("dock.the_tabs_of_this_group"));
    }

    fn tab_bar_height(&self, style: &egui::Style) -> f32 {
        style.spacing.interact_size.y + 8.0
    }

    fn tab_bar_color(&self, visuals: &egui::Visuals) -> Color32 {
        visuals.panel_fill
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        4.0
    }

    fn min_size(&self) -> f32 {
        80.0
    }

    fn simplification_options(&self) -> SimplificationOptions {
        SimplificationOptions { all_panes_must_have_tabs: true, ..SimplificationOptions::default() }
    }

    fn paint_on_top_of_tile(&self, painter: &egui::Painter, style: &egui::Style, tile_id: TileId, rect: Rect) {
        if self.outline && Some(tile_id) == self.active_tile {
            painter.rect_stroke(rect, 0.0, Stroke::new(2.0, style.visuals.selection.stroke.color), StrokeKind::Inside);
        }
    }

    fn cycle_tabs_with_keyboard(&self) -> bool {
        // Ctrl+Tab is the app's own shortcut (all tabs, in order).
        false
    }

    fn on_edit(&mut self, action: egui_tiles::EditAction) {
        if action == egui_tiles::EditAction::TileDropped {
            self.dropped = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids of the panes in each tab group, and the kinds of the containers above them.
    fn shape(dock: &Dock) -> String {
        fn walk(tiles: &Tiles<u64>, tile: TileId, out: &mut String) {
            match tiles.get(tile) {
                Some(Tile::Pane(id)) => out.push_str(&id.to_string()),
                Some(Tile::Container(c)) => {
                    out.push_str(&format!("{:?}(", c.kind()));
                    for (i, child) in c.children().enumerate() {
                        if i > 0 {
                            out.push(' ');
                        }
                        walk(tiles, *child, out);
                    }
                    out.push(')');
                }
                None => out.push('?'),
            }
        }
        let mut out = String::new();
        if let Some(root) = dock.tree.root {
            walk(&dock.tree.tiles, root, &mut out);
        }
        out
    }

    fn dock_with(groups: &[&[u64]]) -> Dock {
        let mut dock = Dock::default();
        let tiles = &mut dock.tree.tiles;
        let tabs: Vec<TileId> = groups
            .iter()
            .map(|ids| {
                let panes = ids.iter().map(|&id| tiles.insert_pane(id)).collect();
                tiles.insert_tab_tile(panes)
            })
            .collect();
        dock.tree.root = Some(if tabs.len() == 1 { tabs[0] } else { tiles.insert_horizontal_tile(tabs) });
        dock
    }

    #[test]
    fn sessions_added_before_first_frame_share_one_tab_group() {
        let setup = crate::session::TermSetup { palette: playterm_term::Palette::default(), scrollback: 100 };
        let sessions: Vec<_> = (1..=30).map(|id| Session::local(id, 80, 24, &setup)).collect();
        let mut dock = Dock::default();
        dock.sync(&sessions, Some(30));
        assert!(!dock.is_split(), "startup URLs must not create thirty narrow columns");
        assert_eq!(dock.tree.tiles.iter().filter(|(_, tile)| matches!(tile, Tile::Pane(_))).count(), 30);
        dock.sync(&sessions, Some(1));
        assert!(!dock.is_split());
    }

    #[test]
    fn arranges_sessions_opened_together() {
        // Only the new sessions: they take the place of their group.
        let mut dock = dock_with(&[&[1, 2, 3]]);
        dock.apply_arrange(&[1, 2, 3], TemplateLayout::Grid);
        assert_eq!(shape(&dock), "Grid(Tabs(1) Tabs(2) Tabs(3))");
        // Other tabs in the group go behind the first new session: the new ones share the
        // room evenly (#142).
        let mut dock = dock_with(&[&[9, 1, 2]]);
        dock.apply_arrange(&[1, 2], TemplateLayout::Rows);
        assert_eq!(shape(&dock), "Vertical(Tabs(1 9) Tabs(2))");
        // In a split view, only that group's place is used.
        let mut dock = dock_with(&[&[7], &[1, 2]]);
        dock.apply_arrange(&[1, 2], TemplateLayout::Columns);
        assert_eq!(shape(&dock), "Horizontal(Tabs(7) Horizontal(Tabs(1) Tabs(2)))");
        // Tabs, or a single session: nothing to do.
        let mut dock = dock_with(&[&[1, 2]]);
        dock.apply_arrange(&[1, 2], TemplateLayout::Tabs);
        dock.apply_arrange(&[1], TemplateLayout::Grid);
        assert_eq!(shape(&dock), "Tabs(1 2)");
    }
}

#[cfg(test)]
mod state_mark_tests {
    use super::*;

    #[test]
    fn every_state_has_its_own_shape() {
        let states = [SessionState::Local, SessionState::Connecting, SessionState::Connected, SessionState::Closed];
        let marks: Vec<StateMark> = states.iter().map(StateMark::of).collect();
        for (i, a) in marks.iter().enumerate() {
            for b in &marks[i + 1..] {
                assert_ne!(a.icon.codepoint, b.icon.codepoint, "{} / {}", a.label, b.label);
                assert_ne!(a.label, b.label);
            }
        }
    }
}
