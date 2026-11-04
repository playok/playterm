//! The window's frame: what the app does on every frame (eframe::App).

use super::*;

impl eframe::App for PlaytermApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Text fields get the usual clipboard keys; a terminal or remote desktop with the
        // keyboard reads them itself (see `clip_keys`).
        if !self.dock.has_focus() {
            clip_keys::widget_shortcuts(&mut raw_input.events, clipboard::read_text);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.guard_quit(&ctx);
        let active_id = self.sessions.get(self.active).map(|s| s.id);
        let downloads = self.zmodem_dir.clone();
        let ask = self.settings.zmodem_ask;
        for session in &mut self.sessions {
            session.poll(&downloads, ask);
            if session.in_zmodem() {
                // Timeouts of the transfer, also while nothing arrives.
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
            if let Some(at) = session.macro_wake() {
                // A macro's pause or time limit ends.
                ctx.request_repaint_after(at.saturating_duration_since(Instant::now()) + Duration::from_millis(5));
            }
            if let Some(text) = session.clipboard_store.take() {
                // A program on the remote side set the clipboard (OSC 52). Only the tab the
                // user is looking at may, within a size limit, and they see it happen: a
                // background host must not plant text for a later paste.
                if !self.settings.clipboard.remote_copy || Some(session.id) != active_id {
                    log::info!("clipboard write from {} ignored", session.tab_title());
                } else if text.len() > MAX_REMOTE_COPY {
                    log::info!("clipboard write of {} bytes from {} ignored", text.len(), session.tab_title());
                } else {
                    crate::toast::remote_copied(&ctx, &session.tab_title(), &text);
                    ctx.copy_text(text);
                }
            }
        }
        self.zmodem_folders(&ctx);
        // "Save password" from a login dialog, once that login has succeeded. Waits while another
        // dialog is open if the vault still has to be unlocked (that needs a dialog of its own).
        if (self.modal.is_none() || self.vault.is_unlocked())
            && let Some((id, password)) = self.sessions.iter_mut().find_map(Session::take_password_to_save)
        {
            self.save_site_password(id, password);
        }
        self.console_requests();
        // A window without tabs still has a prompt to type at.
        self.ensure_main_tab();
        self.dropped_files(&ctx);
        let connected = self.sessions.iter().any(|s| s.state == SessionState::Connected);
        self.sleep_guard.set(if connected { self.settings.prevent_sleep } else { PreventSleep::Off });
        self.screensaver_guard.set(self.settings.prevent_screensaver);

        self.updater.poll(&self.settings.skipped_update);
        let prompt_open = self.show_session_prompts(&ctx);
        if !prompt_open {
            if self.modal.is_none() && !self.updater.is_open() {
                self.shortcuts(&ctx);
            }
            // App modals are shown after the site manager so they stack above it.
            self.show_site_manager(&ctx);
            self.show_modal(&ctx);
            // Waits for other dialogs (one found at start-up does not cover a login prompt).
            if self.modal.is_none() && self.site_manager.is_none() {
                self.update_dialog(&ctx);
            }
        }
        let modal_open = prompt_open || self.modal.is_some() || self.updater.is_open();
        // Beside the (non-modal) site manager a terminal takes the keyboard only when clicked,
        // so typing after opening the manager does not reach a shell.
        let autofocus = !modal_open && self.site_manager.is_none();

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        // Only the toolbar: messages and the simultaneous input state show over the terminal
        // (see `notices`), so the terminal keeps its size when they come and go (#118, #132).
        if self.settings.toolbar {
            egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        }
        if let Some(info) = self.info.take() {
            crate::toast::info(&ctx, &info);
        }
        // Added before the side panel so it spans the whole window width.
        if self.settings.status_bar {
            let session = self.sessions.get(self.active);
            let awake = status_bar::Awake { sleep: self.sleep_guard.active(), screensaver: self.screensaver_guard.active() };
            egui::Panel::bottom("status").show(ui, |ui| status_bar::show(ui, session, awake));
        }
        if self.show_sidebar {
            let vault = self.vault_state();
            // Never more than 40 % of the window, so the terminal keeps its room (#141).
            let max = (ui.available_width() * 0.4).max(160.0);
            let action = egui::Panel::left("sessions")
                .resizable(true)
                .default_size(220.0)
                .min_size(140.0)
                .max_size(max)
                .show(ui, |ui| self.sidebar.show(ui, &self.store, vault))
                .inner;
            if let Some(action) = action {
                self.sidebar_action(action);
            }
        }

        // Slides in from the right edge; added before the terminal so it takes its room. On a
        // tab without SFTP (local, Telnet, serial, RDP) it folds into a thin strip, so the
        // terminal keeps its width, and opens again on an SSH tab.
        if self.sftp_panel && !self.sessions.get(self.active).is_some_and(|s| s.sftp.is_some() || s.can_sftp()) {
            self.sftp_strip_ui(ui);
        } else {
            let mut sftp_panel = self.sftp_panel;
            egui::Panel::right("sftp-panel").resizable(true).default_size(340.0).min_size(240.0).drag_to_open(false).show_collapsible(
                ui,
                &mut sftp_panel,
                |ui| self.sftp_panel_ui(ui),
            );
            // Closed with its ✕ (inside) or by dragging the edge (outside).
            self.sftp_panel = self.sftp_panel && sftp_panel;
        }

        for session in &mut self.sessions {
            if let Some(browser) = &mut session.sftp {
                browser.poll();
                browser.show(&ctx, session.id);
                if browser.has_running_transfers() {
                    // Progress arrives at most every 100 ms; keep the bars moving.
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }

        if self.settings.compose_bar {
            let connected = self.sessions.iter().filter(|s| s.state == SessionState::Connected).count();
            let names: Vec<String> = self.settings.macros.iter().map(|m| m.name.clone()).collect();
            let action = egui::Panel::bottom("compose").show(ui, |ui| self.compose.show(ui, connected, &names)).inner;
            match action {
                Some(compose_bar::ComposeAction::Send(send)) => self.send_composed(send),
                Some(compose_bar::ComposeAction::Macro { k, to_all }) => self.run_macro_in(k, to_all),
                Some(compose_bar::ComposeAction::ManageMacros) => {
                    self.modal = Some(Modal::Macros(Box::new(crate::macros::MacroDialog::new(&self.settings.macros))))
                }
                None => {}
            }
        }

        let style = self.view_style();
        let central = egui::CentralPanel::no_frame()
            .show(ui, |ui| self.dock.show(ui, &mut self.sessions, self.active, autofocus, self.broadcast, &style));
        let output = central.inner;
        self.notices(&ctx, central.response.rect);
        if let Some(px) = output.active_px {
            self.view_px = px;
        }
        self.dock_output(&ctx, output);
        self.show_windows(&ctx);
        if let Some(changed) = self.settings_unsaved {
            if changed.elapsed() >= SETTINGS_SAVE_DELAY {
                self.save_settings();
            } else {
                ctx.request_repaint_after(SETTINGS_SAVE_DELAY);
            }
        }

        crate::toast::show(&ctx);
        self.update_window_title(&ctx);
    }
}

impl PlaytermApp {
    /// What the tabs and panes of the window being drawn did this frame.
    pub(super) fn dock_output(&mut self, ctx: &egui::Context, output: crate::dock::DockOutput) {
        // RDP tabs not on screen (another tab, a shortcut switched away) hold no keys or buttons.
        let window = self.current_window;
        for s in self.sessions.iter_mut().filter(|s| s.window == window) {
            if s.is_rdp() && (!output.shown.contains(&s.id) || !s.shows_rdp()) {
                s.release_rdp_input();
            }
        }
        if let Some(i) = output.activate.and_then(|id| self.index_of(id)) {
            self.active = i;
        }
        if output.view.zoom_steps != 0.0 {
            self.zoom(output.view.zoom_steps);
        }
        self.broadcast_input(&output.view.inputs);
        self.track_tab_drag(output.dragging);
        if let Some(command) = output.command {
            self.run(ctx, command);
        }
    }
}
