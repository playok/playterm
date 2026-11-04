//! Opening tabs: connection parameters from sites (jump hosts, X11, the macro after connecting), new tabs and prompts.

use super::*;

impl PlaytermApp {
    pub(super) fn index_of(&self, id: u64) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    /// Size for a new tab: that of the current one (the real size is applied on the first frame).
    pub(super) fn tab_size(&self) -> (u16, u16) {
        match self.sessions.get(self.active) {
            Some(s) => (s.terminal.cols() as u16, s.terminal.rows() as u16),
            None => (80, 24),
        }
    }

    /// Opens a tab with the local command prompt.
    pub(super) fn open_local(&mut self) {
        let (cols, rows) = self.tab_size();
        let setup = self.term_setup();
        let mut session = Session::local(self.next_id, cols, rows, &setup);
        session.window = self.current_window;
        self.sessions.push(session);
        self.next_id += 1;
        self.active = self.sessions.len() - 1;
    }

    /// Connects `spec` in the tab it was typed in, in the current tab if that is an unused
    /// prompt, or else in a new tab.
    pub(super) fn open_spec(&mut self, spec: SessionSpec) {
        let target =
            self.connect_target.take().or_else(|| self.sessions.get(self.active).filter(|s| s.state == SessionState::Local).map(|s| s.id));
        if let Some(i) = target.and_then(|id| self.index_of(id)).filter(|&i| self.sessions[i].in_console()) {
            self.sessions[i].start(spec, &self.net, self.waker.clone());
            if self.sessions[i].window == self.current_window {
                self.active = i;
            }
            return;
        }
        self.connect_in_new_tab(spec);
    }

    pub(super) fn connect_in_new_tab(&mut self, mut spec: SessionSpec) {
        (spec.params.cols, spec.params.rows) = self.tab_size();
        let setup = self.term_setup();
        let mut session = Session::connect(self.next_id, spec, &self.net, self.waker.clone(), &setup);
        session.window = self.current_window;
        self.next_id += 1;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
    }

    /// Runs `connect` (which may open a connection, or a vault dialog that opens one later)
    /// so that the connection replaces the prompt of tab `target`.
    pub(super) fn connect_in(&mut self, target: Option<u64>, connect: impl FnOnce(&mut Self)) {
        self.connect_target = target;
        connect(self);
        // Consumed by `open_spec`, or carried into a pending vault step; never left behind.
        self.connect_target = None;
    }

    pub(super) fn open_session(&mut self, profile: &SessionProfile, password: Option<String>, name: Option<String>) {
        let mut params = self.connect_params(profile, password);
        params.jump = self.jump_params(profile, 0);
        let log = profile.log.auto.then(|| LogPlan {
            dir: logging::log_dir(&profile.log.dir),
            mode: if profile.log.raw { LogMode::Raw } else { LogMode::Text { timestamps: profile.log.timestamps } },
        });
        let login = (profile.auto_login.enabled && !profile.auto_login.steps.is_empty()).then(|| LoginScript {
            steps: profile.auto_login.steps.clone(),
            user: profile.username.clone(),
            password: params.password.clone().map(Zeroizing::new),
        });
        let site = self.store.get(&profile.id).map(|p| p.id.clone());
        let tab_color = profile.tab_color;
        let profile_charset = profile.charset;
        let after_connect = self.after_connect_macro(profile);
        let profile = Some(Box::new(profile.clone()));
        self.open_spec(SessionSpec { params, charset: profile_charset, name, log, login, site, tab_color, profile, after_connect });
    }

    /// The macro `profile` runs once connected, read for running; a missing or broken one is
    /// reported and left out.
    pub(super) fn after_connect_macro(&mut self, profile: &SessionProfile) -> Option<(String, Vec<crate::macros::Step>)> {
        let name = profile.after_connect_macro.trim();
        if name.is_empty() || profile.protocol == Protocol::Rdp {
            return None;
        }
        let Some(m) = self.settings.macros.iter().find(|m| m.name == name) else {
            self.status =
                Some(tr!("연결 후 매크로 '{name}' 가 없어 실행하지 않습니다", "The macro '{name}' to run after connecting does not exist"));
            return None;
        };
        match crate::macros::parse(&m.script) {
            Ok(steps) => Some((m.name.clone(), steps)),
            Err(e) => {
                self.status =
                    Some(tr!("연결 후 매크로 '{name}' 를 실행할 수 없습니다: {e}", "Cannot run the macro '{name}' after connecting: {e}"));
                None
            }
        }
    }

    /// The jump host `profile` goes through, if any: a saved SSH site named (or with the id)
    /// `jump_host`, with its saved password when the vault is open and its own jump host, or
    /// else `user@host:port` (the site's user and port 22 when left out), logging in with keys
    /// or a prompt. Chains stop after a few hops, so a loop of sites cannot hang.
    pub(super) fn jump_params(&mut self, profile: &SessionProfile, depth: usize) -> Option<Box<ConnectParams>> {
        const MAX_HOPS: usize = 4;
        let text = profile.jump_host.trim();
        if profile.protocol != Protocol::Ssh || text.is_empty() || depth >= MAX_HOPS {
            return None;
        }
        let site = self
            .store
            .profiles()
            .iter()
            .find(|p| p.protocol == Protocol::Ssh && p.id != profile.id && (p.id == text || p.display_name() == text))
            .cloned();
        let mut params = match site {
            Some(site) => {
                let password = if site.save_password && self.vault.is_unlocked() { self.saved_password(&site) } else { None };
                let mut params = self.connect_params(&site, password);
                params.jump = self.jump_params(&site, depth + 1);
                params
            }
            None => {
                let address = crate::profile_form::split_address(text);
                let bastion = SessionProfile {
                    protocol: Protocol::Ssh,
                    host: address.host,
                    port: address.port.unwrap_or(22),
                    username: address.user.unwrap_or_else(|| profile.username.clone()),
                    use_agent: true,
                    ..SessionProfile::default()
                };
                self.connect_params(&bastion, None)
            }
        };
        // The jump host only carries the connection: no forwardings of its own.
        params.forwards.clear();
        params.x11 = None;
        Some(Box::new(params))
    }

    /// How to connect to `profile` (without its jump host).
    pub(super) fn connect_params(&self, profile: &SessionProfile, password: Option<String>) -> ConnectParams {
        let identity_file = (!profile.identity_file.is_empty()).then(|| expand_home(&profile.identity_file));
        // Like OpenSSH, the usual key files are tried when present.
        let default_identity_files = ["id_ed25519", "id_ecdsa", "id_rsa"]
            .iter()
            .map(|name| expand_home(&format!("~/.ssh/{name}")))
            .filter(|p| p.is_file() && Some(p) != identity_file.as_ref())
            .collect();
        ConnectParams {
            protocol: profile.protocol,
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            password,
            identity_file,
            default_identity_files,
            use_agent: profile.use_agent,
            agent_pipe: None,
            forwards: profile.forwards.clone(),
            serial: profile.serial.clone(),
            keepalive: profile.keepalive.clone(),
            rdp: profile.rdp.clone(),
            desktop: self.view_px,
            term_type: profile.term_type.clone(),
            cols: 80,
            rows: 24,
            known_hosts: self.known_hosts.clone(),
            connect_timeout: std::time::Duration::from_secs(self.settings.connect_timeout.into()),
            jump: None,
            x11: (profile.protocol == Protocol::Ssh && profile.x11_forwarding).then(|| playterm_net::X11Request {
                display: match profile.x11_display.trim() {
                    "" => playterm_net::x11::default_display(),
                    display => display.to_owned(),
                },
                trusted: profile.x11_trusted,
            }),
        }
    }

    /// Connects a saved profile, unlocking the vault first if its password is stored there.
    pub(super) fn connect_saved(&mut self, id: &str) {
        let Some(profile) = self.store.get(id).cloned() else { return };
        // For the start screen's "recent" list.
        self.settings.remember_site(id);
        self.settings_unsaved = Some(Instant::now());
        // SSH logs in with it; Telnet and serial sites type it at the prompt (automatic login).
        let wants_secret = profile.save_password;
        if wants_secret && !self.vault.is_unlocked() && self.vault.exists() {
            let dialog = VaultDialog::new(
                VaultMode::Unlock,
                tr!("'{}' 의 저장된 비밀번호를 사용합니다.", "Uses the saved password of '{}'.", profile.display_name()),
            );
            let target = self.connect_target.take();
            self.modal = Some(Modal::Vault(dialog, Pending::Connect(id.to_owned(), target)));
            return;
        }
        let password = if wants_secret { self.saved_password(&profile) } else { None };
        self.open_session(&profile, password, Some(profile.display_name()));
    }

    /// The saved password of `profile`, if it was saved for where the site points now.
    pub(super) fn saved_password(&mut self, profile: &SessionProfile) -> Option<String> {
        let secret = self.vault.get(&profile.id)?.to_owned();
        let now = vault_target(profile);
        match self.vault.target(&profile.id) {
            Some(saved) if saved == now => Some(secret),
            Some(saved) => {
                self.status = Some(tr!(
                    "'{}' 의 접속 대상이 비밀번호를 저장할 때와 달라({saved} → {now}) 저장된 비밀번호를 보내지 않았습니다. \
                     직접 바꾼 것이라면 로그인할 때 '비밀번호 저장' 을 다시 선택하세요.",
                    "The target of '{}' differs from when its password was saved ({saved} → {now}), so the saved password was not sent. \
                     If you changed it, choose 'Save password' again when logging in.",
                    profile.display_name()
                ));
                None
            }
            // Saved by an older version; bound to the site on this unlock.
            None => {
                let r = self.vault.rebind(&profile.id, &now);
                self.report(t("볼트 갱신 실패", "Could not update the password vault"), r);
                Some(secret)
            }
        }
    }

    /// Saves `password` for site `id` in the (unlocked) vault, for where the site points now.
    pub(super) fn set_secret(&mut self, id: &str, password: &str) -> Result<(), playterm_config::VaultError> {
        let target = self.store.get(id).map(vault_target).unwrap_or_default();
        self.vault.set(id, &target, password)
    }

    /// Locks the vault and takes the passwords it gave out back from the open tabs, so a
    /// locked vault really means no saved password is at hand.
    pub(super) fn lock_vault(&mut self) {
        self.vault.lock();
        for session in &mut self.sessions {
            session.forget_password();
        }
    }

    /// Reconnects tab `i`, unlocking the vault first if its site's password was taken back.
    pub(super) fn reconnect(&mut self, i: usize) {
        let session = &self.sessions[i];
        if session.lacks_saved_password() && !self.vault.is_unlocked() && self.vault.exists() {
            let name = session.profile().map(|p| p.display_name()).unwrap_or_default();
            let reason = tr!("'{name}' 의 저장된 비밀번호를 사용합니다.", "Uses the saved password of '{name}'.");
            self.modal = Some(Modal::Vault(VaultDialog::new(VaultMode::Unlock, reason), Pending::Reconnect(session.id)));
            return;
        }
        self.restore_and_reconnect(i);
    }

    /// Reconnects tab `i` with its site's saved password if the vault is open.
    pub(super) fn restore_and_reconnect(&mut self, i: usize) {
        if self.sessions[i].lacks_saved_password()
            && self.vault.is_unlocked()
            && let Some(profile) = self.sessions[i].profile().cloned()
            && let Some(password) = self.saved_password(&profile)
        {
            self.sessions[i].restore_password(password);
        }
        let waker = self.waker.clone();
        self.sessions[i].reconnect(&self.net, waker);
    }

    /// Forgets the password of `id` now, or on the next unlock if the vault is locked.
    pub(super) fn forget_secret(&mut self, id: &str) {
        if self.vault.is_unlocked() {
            let r = self.vault.remove(id);
            self.report(t("볼트 갱신 실패", "Could not update the password vault"), r);
        } else {
            self.vault_todo.push(VaultTodo::Forget(id.to_owned()));
        }
    }

    /// Connects several saved sites (one vault unlock for all of them), then lays them out.
    pub(super) fn connect_many(&mut self, ids: Vec<String>, layout: TemplateLayout) {
        let ids: Vec<String> = ids.into_iter().filter(|id| self.store.get(id).is_some()).collect();
        if ids.is_empty() {
            return;
        }
        let wants_secret = ids.iter().filter_map(|id| self.store.get(id)).any(|p| p.save_password);
        if wants_secret && !self.vault.is_unlocked() && self.vault.exists() {
            let reason = tr!("사이트 {}개의 저장된 비밀번호를 사용합니다.", "Uses the saved passwords of {} sites.", ids.len());
            self.modal = Some(Modal::Vault(VaultDialog::new(VaultMode::Unlock, reason), Pending::ConnectMany(ids, layout)));
            return;
        }
        self.open_many(&ids, layout, true);
    }

    /// Opens the sites (with their stored passwords if `secrets`) and arranges their tabs.
    pub(super) fn open_many(&mut self, ids: &[String], layout: TemplateLayout, secrets: bool) {
        let mut opened = Vec::new();
        for id in ids {
            let Some(profile) = self.store.get(id).cloned() else { continue };
            if secrets {
                self.connect_saved(id);
            } else {
                self.open_session(&profile, None, Some(profile.display_name()));
            }
            if let Some(s) = self.sessions.get(self.active) {
                opened.push(s.id);
            }
        }
        self.dock.arrange(opened, layout);
    }

    /// Connects every site of a template.
    pub(super) fn open_template(&mut self, id: &str) {
        let Some(template) = self.store.template(id).cloned() else { return };
        if !template.sites.iter().any(|s| self.store.get(s).is_some()) {
            self.status = Some(tr!("'{}' 템플릿에 사이트가 없습니다", "Template '{}' has no sites", template.name));
            return;
        }
        self.connect_many(template.sites, template.layout);
    }

    pub(super) fn close_session(&mut self, index: usize) {
        let mut session = self.sessions.remove(index);
        session.close();
        if self.active > index || self.active >= self.sessions.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.fix_active();
        self.ensure_main_tab();
    }
}
