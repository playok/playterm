//! Closing tabs: asking first when their connections are up, and opening closed tabs again.

use super::*;

/// Tabs closed lately that can be opened again, newest last.
const MAX_CLOSED: usize = 10;

/// What the user asked to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseRequest {
    /// The tab with this id.
    Tab(u64),
    /// The other tabs of this tab's window.
    Others(u64),
    /// The tabs of this window.
    All(u64),
    /// The whole app (every window).
    Quit,
}

/// A closed tab, to open again: a saved site through the usual way (its password from the
/// vault), or what a quick connection was made with, without its password.
pub(super) enum ClosedTab {
    Site(String),
    Spec(Box<SessionSpec>),
}

/// Whether closing `s` would end a connection.
fn live(s: &Session) -> bool {
    s.can_disconnect()
}

/// Whether `s` is moving files.
fn transferring(s: &Session) -> bool {
    s.in_zmodem() || s.sftp.as_ref().is_some_and(|b| b.has_running_transfers())
}

impl PlaytermApp {
    /// The tabs `request` would close.
    fn closing(&self, request: CloseRequest) -> Vec<usize> {
        let window_of = |id: u64| self.sessions.iter().find(|s| s.id == id).map(|s| s.window);
        (0..self.sessions.len())
            .filter(|&i| {
                let s = &self.sessions[i];
                match request {
                    CloseRequest::Tab(id) => s.id == id,
                    CloseRequest::Others(keep) => window_of(keep) == Some(s.window) && s.id != keep,
                    CloseRequest::All(window) => s.window == window,
                    CloseRequest::Quit => true,
                }
            })
            .collect()
    }

    /// Closes what `request` names, first asking if that ends connections (and the setting
    /// says to ask).
    pub(super) fn request_close(&mut self, ctx: &egui::Context, request: CloseRequest) {
        let live = self.closing(request).into_iter().any(|i| live(&self.sessions[i]));
        if live && self.settings.confirm_close {
            self.modal = Some(Modal::ConfirmClose(request, false));
        } else {
            self.close_now(ctx, request);
        }
    }

    /// The question of [`Modal::ConfirmClose`]; `Some(true)` once the user agreed.
    pub(super) fn ask_close(&self, ctx: &egui::Context, request: CloseRequest, dont_ask: &mut bool) -> Option<bool> {
        let closing: Vec<&Session> = self.closing(request).into_iter().map(|i| &self.sessions[i]).filter(|s| live(s)).collect();
        let tabs: Vec<String> = closing.iter().map(|s| s.tab_title()).collect();
        let transferring = closing.iter().filter(|s| transferring(s)).count();
        let (title, ok_label) = match request {
            CloseRequest::Quit => (t("playterm 끝내기", "Quit playterm"), t("끝내기", "Quit")),
            CloseRequest::Tab(_) => (t("탭 닫기", "Close tab"), t("닫기", "Close")),
            CloseRequest::Others(_) | CloseRequest::All(_) => (t("탭 닫기", "Close tabs"), t("모두 닫기", "Close all")),
        };
        dialogs::confirm_close(ctx, &dialogs::CloseQuestion { title, ok_label, tabs: &tabs, transferring }, dont_ask)
    }

    /// Closes what `request` names, without asking.
    pub(super) fn close_now(&mut self, ctx: &egui::Context, request: CloseRequest) {
        match request {
            CloseRequest::Tab(id) => {
                if let Some(i) = self.index_of(id) {
                    self.close_session(i);
                }
            }
            CloseRequest::Others(keep) => {
                let Some(window) = self.index_of(keep).map(|i| self.sessions[i].window) else { return };
                self.close_where(|s| s.window == window && s.id != keep);
                self.active = self.index_of(keep).unwrap_or(0);
            }
            CloseRequest::All(window) => {
                self.close_where(|s| s.window == window);
                self.active = 0;
                self.fix_active();
                self.ensure_main_tab();
            }
            CloseRequest::Quit => {
                self.quit_confirmed = true;
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
            }
        }
    }

    /// The main window is being closed (its ✕, Alt+F4): held back while connections are up
    /// until the user agrees.
    pub(super) fn guard_quit(&mut self, ctx: &egui::Context) {
        if self.quit_confirmed || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let live = self.sessions.iter().any(live);
        if !live || !self.settings.confirm_close {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        // Another dialog first has to be closed; the question then comes with the next try.
        if self.modal.is_none() {
            self.modal = Some(Modal::ConfirmClose(CloseRequest::Quit, false));
        }
    }

    /// Keeps `s`, being closed, to open again.
    pub(super) fn remember_closed(&mut self, s: &Session) {
        let closed = match (s.site(), s.spec()) {
            (Some(site), _) if self.store.get(site).is_some() => ClosedTab::Site(site.to_owned()),
            (_, Some(mut spec)) => {
                // Not kept after the tab is gone; asked again when it opens.
                spec.params.password = None;
                if let Some(login) = &mut spec.login {
                    login.password = None;
                }
                ClosedTab::Spec(Box::new(spec))
            }
            _ => return,
        };
        self.closed_tabs.push(closed);
        if self.closed_tabs.len() > MAX_CLOSED {
            self.closed_tabs.remove(0);
        }
    }

    /// Opens the tab closed last again, in a new tab.
    pub(super) fn reopen_closed(&mut self) {
        match self.closed_tabs.pop() {
            Some(ClosedTab::Site(id)) => {
                self.open_local();
                self.connect_saved(&id);
            }
            Some(ClosedTab::Spec(spec)) => self.connect_in_new_tab(*spec),
            None => {}
        }
    }

    pub(super) fn can_reopen(&self) -> bool {
        !self.closed_tabs.is_empty()
    }
}
