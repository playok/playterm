//! Menu and shortcut commands.

use super::*;

impl PlaytermApp {
    pub(super) fn run(&mut self, ctx: &egui::Context, command: Command) {
        let n = self.sessions.len();
        match command {
            Command::QuickConnect => self.open_quick_connect(None),
            Command::NewTab => self.open_local(),
            Command::SiteManager => {
                let selected = self.sidebar.selected().map(str::to_owned);
                self.open_site_manager(selected.as_deref(), false, None);
            }
            Command::NewSite => self.open_site_manager(None, true, None),
            Command::ImportPutty => {
                self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store, ImportSource::Putty))));
            }
            Command::ImportXshell => {
                self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store, ImportSource::Xshell))));
            }
            Command::ImportSecureCrt => {
                self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store, ImportSource::SecureCrt))));
            }
            Command::Quit => self.request_close(ctx, CloseRequest::Quit),

            Command::Copy => {
                if let Some(s) = self.sessions.get_mut(self.active) {
                    view::copy_selection(ctx, s);
                }
            }
            Command::Paste => {
                let keys = self.settings.clipboard;
                if let Some(s) = self.sessions.get_mut(self.active)
                    && let Some(text) = clipboard::read_text()
                {
                    let mut inputs = Vec::new();
                    view::request_paste(s, text, &mut inputs, &keys);
                    self.broadcast_input(&inputs);
                }
            }
            Command::SelectAll => {
                if let Some(s) = self.sessions.get_mut(self.active) {
                    s.terminal.select_all();
                }
            }
            Command::Find => {
                if let Some(s) = self.sessions.get_mut(self.active) {
                    match &mut s.search {
                        Some(bar) => bar.focus(),
                        None => {
                            // Start with the selected text when it is a short single line.
                            let text = s.terminal.selection_text().filter(|t| !t.contains('\n') && t.len() <= 100);
                            s.search = Some(SearchBar::new(text.unwrap_or_default()));
                        }
                    }
                }
            }
            Command::ClearScrollback => {
                if let Some(s) = self.sessions.get_mut(self.active) {
                    s.terminal.clear_scrollback();
                }
            }

            Command::ToggleSidebar => self.show_sidebar = !self.show_sidebar,
            Command::ToggleToolbar => {
                self.settings.toolbar = !self.settings.toolbar;
                self.save_settings();
            }
            Command::ToggleStatusBar => {
                self.settings.status_bar = !self.settings.status_bar;
                self.save_settings();
            }
            Command::ToggleComposeBar => {
                self.settings.compose_bar = !self.settings.compose_bar;
                if self.settings.compose_bar {
                    self.compose.request_focus();
                }
                self.save_settings();
            }
            Command::ToggleFullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }

            Command::ToggleSftpPanel => {
                self.sftp_panel = !self.sftp_panel;
                // Opening the panel goes to where the user is in the terminal.
                if self.sftp_panel
                    && let Some(s) = self.sessions.get_mut(self.active)
                    && s.sftp.is_some()
                {
                    s.follow_shell_dir(false);
                }
            }
            Command::Break(i) => {
                if let Some(s) = self.sessions.get_mut(i) {
                    s.send_break();
                }
            }
            Command::StopLog(i) => {
                if let Some(s) = self.sessions.get_mut(i) {
                    s.stop_log();
                }
            }
            Command::StartLog(i, mode) => {
                let Some(session) = self.sessions.get_mut(i) else { return };
                let dir = logging::default_log_dir();
                let _ = std::fs::create_dir_all(&dir);
                if let Some(path) = crate::picker::Picker::new(crate::picker::Kind::Save, t("로그 파일", "Log file"))
                    .directory(&dir)
                    .file_name(logging::file_name(&session.tab_title()))
                    .filter(t("로그", "Logs"), &["log", "txt"])
                    .pick_one()
                {
                    session.start_log(&path, mode);
                }
            }
            Command::RevealLog(i) => {
                if let Some(log) = self.sessions.get(i).and_then(|s| s.terminal.log()) {
                    logging::reveal(log.path());
                }
            }
            Command::OpenLogFolder => logging::reveal(&logging::default_log_dir()),
            Command::LockVault => self.lock_vault(),
            Command::UnlockVault => self.unlock_vault(),
            Command::SetUpVault => self.set_up_vault(),

            Command::Activate(i) if i < n => {
                // A tab in another window (from the Tab menu): that window comes to the front.
                let window = self.sessions[i].window;
                if window == self.current_window {
                    self.active = i;
                } else {
                    self.show_tab_in(ctx, i);
                }
            }
            Command::Activate(_) => {}
            Command::NextTab | Command::PrevTab => {
                // The tabs of this window, in order.
                let tabs = self.window_tabs();
                if let Some(k) = tabs.iter().position(|&i| i == self.active) {
                    let m = tabs.len();
                    let next = if matches!(command, Command::NextTab) { k + 1 } else { k + m - 1 };
                    self.active = tabs[next % m];
                }
            }
            Command::OpenInNewWindow(i) => self.detach_tab(ctx, i, None),
            Command::MoveToMain(i) => self.move_tab(ctx, i, MAIN_WINDOW),
            Command::NewWindow => {
                let r = open_new_window();
                self.report(t("새 창을 열 수 없습니다", "Cannot open a new window"), r);
            }
            Command::Duplicate(i) => match self.sessions.get(i).map(Session::spec) {
                // Always a new tab, even when the current one is an unused prompt.
                Some(Some(spec)) => self.connect_in_new_tab(spec),
                Some(None) => self.open_local(),
                None => {}
            },
            Command::Reconnect(i) if i < n => {
                self.reconnect(i);
                self.active = i;
            }
            Command::Disconnect(i) => {
                if let Some(s) = self.sessions.get_mut(i) {
                    s.close();
                }
            }
            Command::Close(i) if i < n => {
                let id = self.sessions[i].id;
                self.request_close(ctx, CloseRequest::Tab(id));
            }
            Command::Reconnect(_) | Command::Close(_) => {}
            // The other tabs of this window; those of other windows stay.
            Command::CloseOthers(i) if i < n => {
                let keep = self.sessions[i].id;
                self.request_close(ctx, CloseRequest::Others(keep));
            }
            Command::ReopenClosed => self.reopen_closed(),
            Command::CloseOthers(_) => {}
            Command::ToggleBroadcast => {
                self.broadcast = !self.broadcast;
                if self.broadcast {
                    let (text, tip) = broadcast_status(self.broadcast_targets().len());
                    crate::toast::message(ctx, &text, tip);
                }
            }
            Command::SetTabColor(i, color) => {
                if let Some(s) = self.sessions.get_mut(i) {
                    s.tab_color = color;
                }
            }
            Command::ToggleInBroadcast(i) => {
                if let Some(s) = self.sessions.get_mut(i) {
                    s.in_broadcast = !s.in_broadcast;
                }
            }
            Command::CloseAll => {
                let window = self.current_window;
                self.request_close(ctx, CloseRequest::All(window));
            }

            Command::TogglePreventScreensaver => {
                self.settings.prevent_screensaver = !self.settings.prevent_screensaver;
                self.save_settings();
            }

            Command::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            Command::ToggleMaximize => {
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
            Command::ToggleAlwaysOnTop => {
                self.always_on_top = !self.always_on_top;
                let level = if self.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            }

            Command::ZoomIn => self.set_font_size(self.settings.font_size.floor() + 1.0),
            Command::ZoomOut => self.set_font_size(self.settings.font_size.ceil() - 1.0),
            Command::ZoomReset => self.set_font_size(AppSettings::DEFAULT_FONT_SIZE),
            Command::Settings => self.modal = Some(Modal::Settings(Box::new(SettingsDialog::new(&self.settings)))),
            Command::RunMacro(k) => self.run_macro(k),
            Command::StopMacro => {
                let mut targets = vec![self.active];
                targets.extend(self.broadcast_targets());
                for i in targets {
                    if let Some(session) = self.sessions.get_mut(i) {
                        session.stop_macro();
                    }
                }
            }
            Command::Macros => self.modal = Some(Modal::Macros(Box::new(crate::macros::MacroDialog::new(&self.settings.macros)))),
            Command::Shortcuts => self.modal = Some(Modal::Shortcuts),
            Command::About => self.modal = Some(Modal::About),
            Command::EditSite(id) => self.open_site_manager(Some(&id), false, None),
            Command::ConnectSite(id) => self.connect_saved(&id),
            Command::ProjectPage => ctx.open_url(egui::OpenUrl::new_tab(dialogs::PROJECT_PAGE)),
            Command::Licenses => self.modal = Some(Modal::Licenses(Box::default())),
            Command::CheckUpdates => self.updater.check(ctx, true),
            Command::ShowUpdate => self.updater.reopen(),
        }
    }
}

impl PlaytermApp {
    /// Closes the tabs matching `which`.
    pub(super) fn close_where(&mut self, which: impl Fn(&Session) -> bool) {
        let (closing, keep): (Vec<Session>, Vec<Session>) = std::mem::take(&mut self.sessions).into_iter().partition(|s| which(s));
        self.sessions = keep;
        for mut s in closing {
            self.remember_closed(&s);
            s.close();
        }
    }
}
