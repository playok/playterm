//! The tabs and their docking layout: every session is a pane of an [`egui_tiles::Tree`], so a
//! tab can be dragged out of its tab bar and dropped beside, above or below another group of
//! tabs (or into it) to see several sessions at once. Groups and splits are resized by
//! dragging the gaps between them.
//!
//! The sessions themselves stay in the app's list (its order is the tab order of shortcuts and
//! menus); the tree only holds their ids and is brought in line with the list every frame.

use eframe::egui::{self, Color32, Id, Rect, Response, RichText, Sense, Stroke, StrokeKind, TextStyle, Ui, WidgetText};
use egui_tiles::{Behavior, Container, ContainerKind, SimplificationOptions, TabState, Tile, TileId, Tiles, UiResponse};
use playterm_config::TemplateLayout;
use playterm_i18n::t;

use crate::icon::{self, MaterialIcon};
use crate::menu::{self, Command};
use crate::rdp_view;
use crate::session::{Session, SessionState};
use crate::view::{self, ViewOutput, ViewStyle};

/// Tab title color of sessions receiving simultaneous input.
pub const BROADCAST_COLOR: Color32 = Color32::from_rgb(230, 140, 30);
/// Width of the frame around the panes that take part in simultaneous input.
const BROADCAST_FRAME: f32 = 2.0;

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
            SessionState::Local => (icon::STATE_LOCAL, Color32::from_rgb(140, 140, 140), t("로컬 프롬프트", "Local prompt")),
            SessionState::Connecting => {
                (icon::STATE_CONNECTING, Color32::from_rgb(220, 180, 40), t("연결 중…", "Connecting…"))
            }
            SessionState::Connected => (icon::STATE_CONNECTED, Color32::from_rgb(60, 190, 90), t("연결됨", "Connected")),
            SessionState::Closed => (icon::STATE_CLOSED, Color32::from_rgb(200, 70, 70), t("연결 끊김", "Disconnected")),
        };
        Self { icon, color, label }
    }
}

/// The docking layout of the session tabs.
pub struct Dock {
    tree: egui_tiles::Tree<u64>,
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
}

impl Default for Dock {
    fn default() -> Self {
        Self { tree: egui_tiles::Tree::empty(Id::unique("playterm-dock")), shown_active: None, focused: None, arrange: None }
    }
}

impl Dock {
    /// Adds tabs for new sessions (to the group of the active one), drops those of closed ones and
    /// brings the active session's tab to the front.
    fn sync(&mut self, sessions: &[Session], active: Option<u64>) {
        let tiles = &self.tree.tiles;
        let gone: Vec<TileId> = tiles
            .iter()
            .filter_map(|(&tile, t)| match t {
                Tile::Pane(id) if !sessions.iter().any(|s| s.id == *id) => Some(tile),
                _ => None,
            })
            .collect();
        for tile in gone {
            self.tree.remove_recursively(tile);
        }

        let group = self.group_of(self.shown_active).or_else(|| self.group_of(active)).or_else(|| self.any_group());
        for s in sessions {
            if self.tree.tiles.find_pane(&s.id).is_some() {
                continue;
            }
            let tile = self.tree.tiles.insert_pane(s.id);
            match group.and_then(|g| self.tree.tiles.get_mut(g)) {
                Some(Tile::Container(Container::Tabs(tabs))) => tabs.add_child(tile),
                _ => {
                    let tabs = self.tree.tiles.insert_tab_tile(vec![tile]);
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
    /// in; that group stays beside them if it has other tabs.
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
        let groups: Vec<TileId> = panes.iter().map(|&pane| tiles.insert_tab_tile(vec![pane])).collect();
        let arranged = tiles.insert_container(Container::new(kind, groups));
        let home_empty = matches!(tiles.get(home), Some(Tile::Container(c)) if c.is_empty());
        let replacement = if home_empty { arranged } else { tiles.insert_horizontal_tile(vec![home, arranged]) };
        match home_parent {
            Some(parent) => {
                if let Some(Tile::Container(container)) = tiles.get_mut(parent) {
                    let _ = container.replace_child(home, replacement);
                }
            }
            None => self.tree.root = Some(replacement),
        }
        if home_empty {
            tiles.remove(home);
        }
    }

    /// The tab group holding the session `id`.
    fn group_of(&self, id: Option<u64>) -> Option<TileId> {
        let tile = self.tree.tiles.find_pane(&id?)?;
        let parent = self.tree.tiles.parent_of(tile)?;
        matches!(self.tree.tiles.get(parent), Some(Tile::Container(Container::Tabs(_)))).then_some(parent)
    }

    fn any_group(&self) -> Option<TileId> {
        self.tree
            .tiles
            .iter()
            .find(|(_, t)| matches!(t, Tile::Container(Container::Tabs(_))))
            .map(|(&id, _)| id)
    }

    /// More than one tab group is on screen.
    fn is_split(&self) -> bool {
        self.tree
            .tiles
            .iter()
            .filter(|(id, t)| matches!(t, Tile::Container(Container::Tabs(_))) && self.tree.is_visible(**id))
            .count()
            > 1
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
        let dragged = self.tree.dragged_id(ui.ctx()).and_then(|t| self.tree.tiles.get_pane(&t).copied());
        let mut behavior = DockView {
            sessions,
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
        let output = if session.shows_rdp() {
            rdp_view::show(ui, session, autofocus)
        } else {
            view::show(ui, session, autofocus, self.style)
        };
        if output.focused {
            self.focused = Some(id);
        }
        // What is typed goes to every pane in an orange frame: hard to miss while it is on.
        if self.broadcast && session.in_broadcast {
            ui.painter().rect_stroke(pane_rect, 0.0, Stroke::new(BROADCAST_FRAME, BROADCAST_COLOR), StrokeKind::Inside);
        }
        self.out.view.zoom_steps += output.zoom_steps;
        if output.command.is_some() {
            self.out.command = output.command;
        }
        self.out.view.inputs.extend(output.inputs);
        self.out.shown.push(id);
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &u64) -> WidgetText {
        self.index(*pane).map(|i| self.sessions[i].tab_title()).unwrap_or_default().into()
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
            BROADCAST_COLOR
        } else if s.state == SessionState::Closed && !state.active {
            // A closed tab in the background takes no input: its title steps back. (The tab on
            // show keeps a readable title on its highlight; its icon says it is closed.)
            ui.visuals().weak_text_color()
        } else if state.active {
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().text_color()
        };
        let mut parts = vec![
            (mark.icon.rich_text().color(mark.color), font.clone()),
            (RichText::new(" "), font.clone()),
        ];
        if let Some(label) = s.site_label() {
            let color = s.tab_color.map_or(ui.visuals().strong_text_color(), crate::menu::color32);
            parts.push((RichText::new(format!("{label} ")).strong().color(color), font.clone()));
        }
        if s.is_logging() {
            parts.push((RichText::new("REC ").color(Color32::from_rgb(220, 60, 60)), TextStyle::Small.resolve(ui.style())));
        }
        parts.push((RichText::new(s.tab_title()).color(text_color), font.clone()));
        let galleys: Vec<_> = parts
            .into_iter()
            .map(|(text, font)| WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, font))
            .collect();
        // No close button: it was easy to hit by mistake. Tabs close from their context menu,
        // with a middle click or with Ctrl+Shift+W.
        let margin = 8.0;
        let width = galleys.iter().map(|g| g.size().x).sum::<f32>() + 2.0 * margin;
        let (_, rect) = ui.allocate_space(egui::vec2(width, ui.available_height()));
        let response = ui.interact(rect, id, Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::Grab);

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
            for galley in galleys {
                let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
                x += galley.size().x;
                painter.galley(pos, galley, text_color);
            }
        }

        if response.middle_clicked() {
            self.out.command = Some(Command::Close(i));
        }
        if response.clicked() {
            self.out.activate = Some(s.id);
        }

        let command = &mut self.out.command;
        response.context_menu(|ui| {
            menu::session_items(ui, command, i, s);
            ui.separator();
            let mark = if s.in_broadcast { "✔" } else { "  " };
            if ui.button(format!("{mark} {}", t("동시 입력에 포함", "Include in simultaneous input"))).clicked() {
                *command = Some(Command::ToggleInBroadcast(i));
                ui.close();
            }
            menu::tab_color_menu(ui, command, i, s.tab_color);
            ui.separator();
            let entries = [
                (t("복제", "Duplicate"), Command::Duplicate(i)),
                (t("새 창으로 열기", "Open in new window"), Command::OpenInNewWindow(i)),
                (t("다시 연결", "Reconnect"), Command::Reconnect(i)),
                (t("닫기", "Close"), Command::Close(i)),
                (t("다른 탭 모두 닫기", "Close other tabs"), Command::CloseOthers(i)),
            ];
            for (label, c) in entries {
                if ui.button(label).clicked() {
                    *command = Some(c);
                    ui.close();
                }
            }
        });
        response.on_hover_text(format!(
            "{}\n{}",
            mark.label,
            t("끌어서 옆이나 위아래로 놓으면 화면을 나눠 봅니다", "Drag beside, above or below another tab to split the view")
        ))
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
    fn arranges_sessions_opened_together() {
        // Only the new sessions: they take the place of their group.
        let mut dock = dock_with(&[&[1, 2, 3]]);
        dock.apply_arrange(&[1, 2, 3], TemplateLayout::Grid);
        assert_eq!(shape(&dock), "Grid(Tabs(1) Tabs(2) Tabs(3))");
        // Other tabs in the group stay, beside the arrangement.
        let mut dock = dock_with(&[&[9, 1, 2]]);
        dock.apply_arrange(&[1, 2], TemplateLayout::Rows);
        assert_eq!(shape(&dock), "Horizontal(Tabs(9) Vertical(Tabs(1) Tabs(2)))");
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
