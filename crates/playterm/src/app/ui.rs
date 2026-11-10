//! Drawing the window: menu bar, tab bar, side panels and dialogs.

use super::*;
use crate::keys::Shortcut;

impl PlaytermApp {
    /// Shows the app-level modal, if any, and applies its outcome.
    pub(super) fn show_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.take() else { return };
        match modal {
            Modal::QuickConnect(mut dialog) => match dialog.show(ctx) {
                QuickConnectResult::Open => self.modal = Some(Modal::QuickConnect(dialog)),
                QuickConnectResult::Cancel => self.dialog_target = None,
                QuickConnectResult::Connect { profile, password, save } => {
                    if save {
                        let r = self.store.upsert((*profile).clone());
                        self.report(t!("common.could_not_save_the_site"), r);
                    }
                    let name = save.then(|| profile.display_name());
                    let target = self.dialog_target.take();
                    self.connect_in(target, |app| app.open_session(&profile, password.map(Password::from), name));
                }
            },
            Modal::Shortcuts => {
                if !dialogs::shortcuts(ctx, &self.settings.clipboard, &self.settings.shortcuts) {
                    self.modal = Some(Modal::Shortcuts);
                }
            }
            Modal::Notice(title, text) => {
                if !dialogs::notice(ctx, &title, &text) {
                    self.modal = Some(Modal::Notice(title, text));
                }
            }
            Modal::About => {
                if !dialogs::about(ctx) {
                    self.modal = Some(Modal::About);
                }
            }
            Modal::Licenses(mut dialog) => {
                if !dialog.show(ctx) {
                    self.modal = Some(Modal::Licenses(dialog));
                }
            }
            Modal::Macros(mut dialog) => match dialog.show(ctx) {
                crate::macros::MacroDialogResult::Open => self.modal = Some(Modal::Macros(dialog)),
                crate::macros::MacroDialogResult::Cancel => {}
                crate::macros::MacroDialogResult::Save(macros) => {
                    self.settings.macros = macros;
                    self.save_settings();
                }
            },
            Modal::ConfirmPasswordMacro(plan) => {
                let targets: Vec<String> = plan
                    .targets
                    .iter()
                    .filter_map(|id| self.sessions.iter().find(|session| session.id == *id))
                    .map(Session::macro_target_label)
                    .collect();
                match dialogs::confirm_password_macro(ctx, &plan.name, &targets) {
                    None => self.modal = Some(Modal::ConfirmPasswordMacro(plan)),
                    Some(false) => {}
                    Some(true) => self.execute_macro_plan(plan),
                }
            }
            Modal::TrustedHosts(mut dialog) => {
                if !dialog.show(ctx) {
                    self.modal = Some(Modal::TrustedHosts(dialog));
                }
            }
            Modal::Settings(mut dialog) => match dialog.show(ctx) {
                SettingsResult::Open => self.modal = Some(Modal::Settings(dialog)),
                SettingsResult::Changed(s) => {
                    self.apply_settings(ctx, s);
                    self.modal = Some(Modal::Settings(dialog));
                }
                SettingsResult::Save(s) => {
                    self.apply_settings(ctx, s);
                    self.save_settings();
                }
                SettingsResult::Cancel(original) => self.apply_settings(ctx, original),
            },
            Modal::Import(mut dialog) => match dialog.show(ctx, &self.store) {
                ImportResult::Open => self.modal = Some(Modal::Import(dialog)),
                ImportResult::Cancel => {}
                ImportResult::Import { source, folder, profiles } => {
                    let count = profiles.len();
                    for p in profiles {
                        let r = self.store.upsert(p);
                        self.report(t!("common.could_not_save_the_site"), r);
                    }
                    let place = if folder.is_empty() { String::new() } else { tr!("app.ui.folder", folder = folder) };
                    let imported = tr!("app.ui.imported_sessions", arg0 = source.name(), count = count, place = place);
                    // SecureCRT keeps passwords for its sessions; they stay behind, which the user
                    // acknowledges rather than finds out at the first connection. The notice says
                    // what was imported too.
                    if source == ImportSource::SecureCrt && count > 0 {
                        let text = format!("{imported}.\n\n{}", crate::import_dialog::password_notice_text());
                        self.modal = Some(Modal::Notice(crate::import_dialog::password_notice_title().to_owned(), text));
                    } else {
                        self.info = Some(imported);
                    }
                    self.show_sidebar = true;
                }
            },
            Modal::Vault(mut dialog, pending) => match dialog.show(ctx) {
                VaultResult::Open => self.modal = Some(Modal::Vault(dialog, pending)),
                VaultResult::Cancel => self.vault_cancelled(pending),
                VaultResult::Submit(master) => {
                    match self.vault_worker.start(self.vault.path().to_owned(), dialog.mode, master, ctx.clone()) {
                        Ok(task) => {
                            dialog.clear_secrets();
                            self.modal = Some(Modal::VaultWorking(dialog, pending, task));
                        }
                        Err(e) => {
                            dialog.fail(e.to_string());
                            self.modal = Some(Modal::Vault(dialog, pending));
                        }
                    }
                }
                VaultResult::Reset => {
                    match self.vault.reset() {
                        Ok(()) => dialog.switch_to_create(),
                        Err(e) => dialog.fail(e.to_string()),
                    }
                    self.modal = Some(Modal::Vault(dialog, pending));
                }
            },
            Modal::VaultWorking(mut dialog, pending, task) => match task.poll() {
                Some(Ok(vault)) => {
                    self.vault = vault;
                    self.vault_ready(pending);
                }
                Some(Err(e)) => {
                    dialog.fail(e.to_string());
                    self.modal = Some(Modal::Vault(dialog, pending));
                }
                None => {
                    if dialog.show_working(ctx, task.can_cancel()) && task.cancel() {
                        self.vault_cancelled(pending);
                    } else {
                        self.modal = Some(Modal::VaultWorking(dialog, pending, task));
                    }
                }
            },
            Modal::Template(mut dialog) => match dialog.show(ctx, &self.store) {
                TemplateResult::Open => self.modal = Some(Modal::Template(dialog)),
                TemplateResult::Cancel => {}
                TemplateResult::Save(template) => {
                    let name = template.name.clone();
                    let r = self.store.upsert_template(template);
                    self.report(t!("app.ui.could_not_save_the_template"), r);
                    self.info = Some(tr!("app.ui.saved_template_double_click_it_under_templates_in", name = name));
                    self.show_sidebar = true;
                }
            },
            Modal::ConfirmDeleteTemplate(id) => {
                let name = self.store.template(&id).map(|tpl| tpl.name.clone()).unwrap_or_default();
                let text = tr!("app.ui.delete_template_its_sites_stay", name = name);
                match dialogs::confirm(ctx, t!("app.ui.delete_template"), &text, t!("common.delete")) {
                    None => self.modal = Some(Modal::ConfirmDeleteTemplate(id)),
                    Some(false) => {}
                    Some(true) => {
                        let r = self.store.remove_template(&id);
                        self.report(t!("app.ui.could_not_delete_the_template"), r);
                    }
                }
            }
            Modal::ConfirmClose(request, mut dont_ask) => match self.ask_close(ctx, request, &mut dont_ask) {
                None => self.modal = Some(Modal::ConfirmClose(request, dont_ask)),
                Some(false) => {}
                Some(true) => {
                    if dont_ask {
                        self.settings.confirm_close = false;
                        self.save_settings();
                    }
                    self.close_now(ctx, request);
                }
            },
            Modal::ConfirmDelete(id) => {
                let name = self.store.get(&id).map(|p| p.display_name()).unwrap_or_default();
                let text = tr!("app.ui.delete_site", name = name);
                match dialogs::confirm(ctx, t!("app.ui.delete_site_2"), &text, t!("common.delete")) {
                    None => self.modal = Some(Modal::ConfirmDelete(id)),
                    Some(false) => {}
                    Some(true) => match self.store.remove(&id) {
                        Ok(()) => self.forget_secret(&id),
                        Err(e) => self.report(t!("app.ui.could_not_delete_the_site"), Err(e)),
                    },
                }
            }
        }
    }

    /// Prompts from the network layer take priority over app dialogs.
    pub(super) fn show_session_prompts(&mut self, ctx: &egui::Context) -> bool {
        // Each window asks for its own tabs.
        let window = self.current_window;
        if let Some(index) = self.sessions.iter().position(|s| s.window == window && s.host_key.is_some()) {
            self.active = index;
            dialogs::host_key(ctx, &mut self.sessions[index]);
            return true;
        }
        if let Some(index) = self.sessions.iter().position(|s| s.window == window && s.auth.is_some()) {
            self.active = index;
            let site = self.sessions[index].site().and_then(|id| self.store.get(id)).cloned();
            if let Some(relogin) = dialogs::auth(ctx, &mut self.sessions[index], site.is_some()) {
                if relogin.save
                    && let Some(mut profile) = site
                {
                    profile.username = relogin.user.clone();
                    let r = self.store.upsert(profile);
                    self.report(t!("common.could_not_save_the_site"), r);
                }
                let waker = self.waker.clone();
                self.sessions[index].relogin(relogin.user, relogin.password, &self.net, waker);
            }
            return true;
        }
        if let Some(index) = self.sessions.iter().position(|s| s.window == window && s.pending_paste.is_some()) {
            self.active = index;
            let text = self.sessions[index].pending_paste.clone().unwrap_or_default();
            let others: Vec<String> = self.broadcast_targets().into_iter().map(|k| self.sessions[k].tab_title()).collect();
            match dialogs::confirm_paste(ctx, &text, &others) {
                None => {}
                Some(false) => self.sessions[index].pending_paste = None,
                Some(true) => {
                    self.sessions[index].pending_paste = None;
                    view::paste(&mut self.sessions[index], &text);
                    self.broadcast_input(&[UserInput::Paste(text)]);
                }
            }
            return true;
        }
        false
    }

    pub(super) fn update_dialog(&mut self, ctx: &egui::Context) {
        let connected = self.sessions.iter().filter(|s| s.state == SessionState::Connected).count();
        match self.updater.show(ctx, connected) {
            None => {}
            Some(update::Action::Skip(version)) => {
                self.settings.skipped_update = version;
                self.save_settings();
            }
            Some(update::Action::Restart(exe)) => match std::process::Command::new(&exe).spawn() {
                // The update dialog already said how many connections end.
                Ok(_) => {
                    self.quit_confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close)
                }
                Err(e) => self.status = Some(tr!("app.ui.cannot_start", arg0 = exe.display(), e = e)),
            },
        }
    }

    /// App shortcuts, consumed before the terminal sees the key events.
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        let clip = self.settings.clipboard;
        let shortcuts = self.settings.shortcuts;
        for binding in crate::keys::bindings() {
            let what = binding.what;
            // Turned off in the settings: the key goes to the terminal.
            let off = !crate::keys::enabled(binding, &shortcuts, clip.ctrl_shift_c, clip.ctrl_shift_v);
            // A window tabs were moved to has no menus or side panels: only what works on its tabs.
            let tab_only = matches!(
                what,
                Shortcut::Find
                    | Shortcut::Broadcast
                    | Shortcut::PrevTab
                    | Shortcut::NextTab
                    | Shortcut::CloseTab
                    | Shortcut::DuplicateTab
                    | Shortcut::Copy
                    | Shortcut::Paste
                    | Shortcut::Fullscreen
                    | Shortcut::ZoomIn
                    | Shortcut::ZoomOut
                    | Shortcut::ZoomReset
            );
            let off = off || (self.current_window != MAIN_WINDOW && !tab_only);
            if off || !ctx.input_mut(|i| i.consume_shortcut(&binding.keys)) {
                continue;
            }
            let i = self.active;
            let command = match what {
                Shortcut::QuickConnect => Command::QuickConnect,
                Shortcut::NewTab => Command::NewTab,
                Shortcut::ReopenClosed => Command::ReopenClosed,
                Shortcut::SiteManager => Command::SiteManager,
                Shortcut::Sidebar => Command::ToggleSidebar,
                Shortcut::Find => Command::Find,
                Shortcut::Broadcast => Command::ToggleBroadcast,
                Shortcut::SftpPanel => Command::ToggleSftpPanel,
                Shortcut::ComposeBar => Command::ToggleComposeBar,
                Shortcut::PrevTab => Command::PrevTab,
                Shortcut::NextTab => Command::NextTab,
                Shortcut::CloseTab => Command::Close(i),
                Shortcut::DuplicateTab => Command::Duplicate(i),
                Shortcut::Copy => Command::Copy,
                Shortcut::Paste => Command::Paste,
                Shortcut::Fullscreen => Command::ToggleFullscreen,
                Shortcut::ZoomIn => Command::ZoomIn,
                Shortcut::ZoomOut => Command::ZoomOut,
                Shortcut::ZoomReset => Command::ZoomReset,
                Shortcut::NewWindow => Command::NewWindow,
                Shortcut::Settings => Command::Settings,
                Shortcut::Quit => Command::Quit,
            };
            self.run(ctx, command);
        }
    }

    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let state = MenuState {
            sessions: &self.sessions,
            active: self.active,
            show_sidebar: self.show_sidebar,
            show_toolbar: self.settings.toolbar,
            show_status_bar: self.settings.status_bar,
            show_compose_bar: self.settings.compose_bar,
            vault: self.vault_state(),
            always_on_top: self.always_on_top,
            broadcast: self.broadcast,
            prevent_screensaver: self.settings.prevent_screensaver,
            sftp_panel: self.sftp_panel,
            copy_hint: clip_keys::copy_hint(&self.settings.clipboard),
            paste_hint: clip_keys::paste_hint(&self.settings.clipboard),
            update: self.updater.pending().map(|v| v.to_string()),
            macros: &self.settings.macros,
            can_reopen: self.can_reopen(),
        };
        if let Some(command) = menu::menu_bar(ui, &state) {
            let ctx = ui.ctx().clone();
            self.run(&ctx, command);
        }
    }

    /// The toolbar: one line, whatever the window's width (#144).
    pub(super) fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let state = crate::toolbar::ToolbarState {
            active: self.sessions.get(self.active),
            i: self.active,
            sidebar: self.show_sidebar,
            sftp_panel: self.sftp_panel,
            compose_bar: self.settings.compose_bar,
            broadcast: self.broadcast,
            copy_hint: clip_keys::copy_hint(&self.settings.clipboard),
            paste_hint: clip_keys::paste_hint(&self.settings.clipboard),
        };
        let action = ui.horizontal(|ui| crate::toolbar::show(ui, &state)).inner;
        if let Some(command) = action {
            let ctx = ui.ctx().clone();
            self.run(&ctx, command);
        }
    }

    /// What the app has to say, over the top right of the terminal area `area`: simultaneous
    /// input while it is on, and the last error until dismissed. Drawn over the terminal so it
    /// keeps its size when they come and go (#118, #132).
    pub(super) fn notices(&mut self, ctx: &egui::Context, area: egui::Rect) {
        // Below the tab bar and the connection line.
        let mut top = area.top() + 64.0;
        let right = area.right() - 12.0;
        let mut action = None;
        if self.broadcast {
            let others = self.broadcast_targets().len();
            let (text, tip) = broadcast_status(others);
            let shown = egui::Area::new(egui::Id::unique("notice-broadcast"))
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::RIGHT_TOP)
                .fixed_pos(egui::pos2(right, top))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.5, theme::broadcast_color(ui.visuals()))).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(text).strong().color(theme::broadcast_color(ui.visuals()))).on_hover_text(tip);
                            if ui
                                .add(icon::button(icon::TURN_OFF, t!("app.ui.turn_off")).small())
                                .on_hover_text(crate::keys::hint(Shortcut::Broadcast))
                                .clicked()
                            {
                                action = Some(Command::ToggleBroadcast);
                            }
                        });
                    });
                });
            top = shown.response.rect.bottom() + 6.0;
        }
        if let Some(status) = self.status.clone() {
            let mut dismiss = false;
            egui::Area::new(egui::Id::unique("notice-error"))
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::RIGHT_TOP)
                .fixed_pos(egui::pos2(right, top))
                .show(ctx, |ui| {
                    let error = ui.visuals().error_fg_color;
                    egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.5, error)).show(ui, |ui| {
                        ui.set_max_width((area.width() - 48.0).clamp(200.0, 520.0));
                        ui.horizontal_top(|ui| {
                            ui.add(egui::Label::new(RichText::new(&status).color(error)).wrap());
                            dismiss = ui.add(icon::icon_button(icon::CLOSE).small()).on_hover_text(t!("app.ui.dismiss")).clicked();
                        });
                    });
                });
            if dismiss {
                self.status = None;
            }
        }
        if let Some(command) = action {
            self.run(ctx, command);
        }
    }

    pub(super) fn update_window_title(&mut self, ctx: &egui::Context) {
        let title = match self.sessions.get(self.active) {
            Some(s) => match s.site_label() {
                Some(label) => format!("[{label}] {} - playterm", s.tab_title()),
                None => format!("{} - playterm", s.tab_title()),
            },
            None => "playterm".to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// Files dropped on the window are uploaded into the active session's SFTP directory.
    pub(super) fn dropped_files(&mut self, ctx: &egui::Context) {
        let paths: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| p.exists()).collect());
        if paths.is_empty() {
            return;
        }
        let panel = self.sftp_panel;
        match self.sessions.get_mut(self.active).and_then(|s| s.sftp.as_mut()).filter(|_| panel) {
            Some(browser) => browser.upload_paths(paths),
            None => {
                let keys = crate::keys::hint(Shortcut::SftpPanel);
                self.status = Some(tr!("app.ui.open_the_sftp_explorer_first_to_upload_files", keys = keys))
            }
        }
    }

    /// The SFTP explorer folded to a strip, for a tab it cannot show (see [`SFTP_STRIP`]).
    pub(super) fn sftp_strip_ui(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("sftp-strip").resizable(false).exact_size(SFTP_STRIP).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                ui.add(icon::icon_button(icon::SFTP).frame(false))
                    .on_hover_text(t!("app.ui.sftp_explorer_shows_uploads_and_downloads_remote_files"));
                if ui
                    .add(icon::icon_button(icon::CLOSE).small())
                    .on_hover_text(format!("{} ({})", t!("common.close"), crate::keys::hint(Shortcut::SftpPanel)))
                    .clicked()
                {
                    self.sftp_panel = false;
                }
            });
        });
    }

    /// The SFTP explorer side panel: the active tab's remote files.
    pub(super) fn sftp_panel_ui(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        ui.horizontal(|ui| {
            ui.strong(t!("common.sftp_explorer"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui
                    .add(icon::icon_button(icon::CLOSE).small())
                    .on_hover_text(format!("{} ({})", t!("common.close"), crate::keys::hint(Shortcut::SftpPanel)))
                    .clicked();
            });
        });
        if close {
            self.sftp_panel = false;
        }
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        ui.weak(session.tab_title());
        ui.separator();
        if session.sftp.is_none() && !session.can_sftp() {
            ui.weak(t!("app.ui.shows_uploads_and_downloads_remote_files_in_a"));
            return;
        }
        if let Some(browser) = &mut session.sftp {
            browser.panel(ui);
        } else if let Some(err) = session.sftp_error() {
            ui.colored_label(ui.visuals().error_fg_color, tr!("app.ui.could_not_open_sftp", err = err));
            if ui.add(icon::button(icon::RETRY, t!("common.try_again"))).clicked() {
                session.retry_sftp();
            }
        } else {
            session.ensure_sftp();
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(t!("app.ui.opening_sftp"));
            });
        }
    }

    pub(super) fn vault_state(&self) -> VaultState {
        if self.vault.is_unlocked() {
            VaultState::Unlocked
        } else if self.vault.exists() {
            VaultState::Locked
        } else {
            VaultState::Missing
        }
    }
}

/// Width of the SFTP explorer folded on a tab without SFTP.
pub(super) const SFTP_STRIP: f32 = 34.0;
