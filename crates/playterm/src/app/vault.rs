//! Saved sites and the password vault: saving, duplicating and deleting sites, the vault's passwords and its locking.

use super::*;

impl PlaytermApp {
    pub(super) fn report<E: std::fmt::Display>(&mut self, what: &str, result: Result<(), E>) {
        if let Err(e) = result {
            self.status = Some(format!("{what}: {e}"));
        }
    }

    pub(super) fn save_profile(&mut self, profile: SessionProfile, password: Option<Zeroizing<String>>, connect: bool) {
        let id = profile.id.clone();
        let save_password = profile.save_password;
        let r = self.store.upsert(profile);
        self.report(t("사이트 저장 실패", "Could not save the site"), r);

        if !save_password {
            self.forget_secret(&id);
        } else if password.is_none() {
            // The user edited the site here: its saved password follows the new target.
            if self.vault.is_unlocked() {
                let target = self.store.get(&id).map(vault_target).unwrap_or_default();
                let r = self.vault.rebind(&id, &target);
                self.report(t("저장된 비밀번호 갱신 실패", "Could not update the saved passwords"), r);
            } else {
                self.vault_todo.push(VaultTodo::Rebind(id.clone()));
            }
        }
        match password {
            Some(password) if save_password => self.store_secret(id, password, connect),
            Some(password) if connect => {
                // Not saved: use it for this connection only.
                if let Some(p) = self.store.get(&id).cloned() {
                    self.open_session(&p, Some(password.into()), Some(p.display_name()));
                }
            }
            _ if connect => self.connect_saved(&id),
            _ => {}
        }
    }

    /// Saves the password typed at a successful login for site `id`, unlocking (or creating)
    /// the vault first if needed. The site is marked as saving its password only once it is
    /// stored, so a cancelled vault dialog changes nothing.
    pub(super) fn save_site_password(&mut self, id: String, password: Zeroizing<String>) {
        if self.store.get(&id).is_none() {
            return;
        }
        if self.vault.is_unlocked() {
            self.finish_save_login(&id, &password);
            return;
        }
        let (mode, reason) = if self.vault.exists() {
            (
                VaultMode::Unlock,
                t("비밀번호를 저장하려면 저장된 비밀번호를 잠금 해제하세요.", "Unlock the saved passwords to save the password."),
            )
        } else {
            (VaultMode::Create, t("처음으로 비밀번호를 저장합니다.", "Saving a password for the first time."))
        };
        self.modal = Some(Modal::Vault(VaultDialog::new(mode, reason), Pending::SaveLogin { id, password }));
    }

    /// Stores a login password in the (unlocked) vault and marks the site as saving it.
    pub(super) fn finish_save_login(&mut self, id: &str, password: &str) {
        if let Err(e) = self.set_secret(id, password) {
            self.status = Some(format!("{}: {e}", t("비밀번호 저장 실패", "Could not save the password")));
            return;
        }
        let Some(mut profile) = self.store.get(id).cloned() else { return };
        if !profile.save_password {
            profile.save_password = true;
            let r = self.store.upsert(profile.clone());
            self.report(t("사이트 저장 실패", "Could not save the site"), r);
        }
        self.info = Some(tr!("'{}' 의 비밀번호를 저장했습니다", "Saved the password of '{}'", profile.display_name()));
    }

    pub(super) fn store_secret(&mut self, id: String, password: Zeroizing<String>, connect: bool) {
        if self.vault.is_unlocked() {
            let r = self.set_secret(&id, &password);
            self.report(t("비밀번호 저장 실패", "Could not save the password"), r);
            if connect {
                self.connect_saved(&id);
            }
            return;
        }
        let (mode, reason) = if self.vault.exists() {
            (
                VaultMode::Unlock,
                t("비밀번호를 저장하려면 저장된 비밀번호를 잠금 해제하세요.", "Unlock the saved passwords to save the password."),
            )
        } else {
            (VaultMode::Create, t("처음으로 비밀번호를 저장합니다.", "Saving a password for the first time."))
        };
        let target = self.connect_target.take();
        let pending = Pending::StoreSecret { id, password, connect, target };
        self.modal = Some(Modal::Vault(VaultDialog::new(mode, reason), pending));
    }

    /// Runs after the vault was unlocked or created.
    pub(super) fn vault_ready(&mut self, pending: Pending) {
        // Changes made while it was locked. Nothing else is removed: the site list may be
        // incomplete (unreadable file, another window), and a lost password cannot come back.
        for todo in std::mem::take(&mut self.vault_todo) {
            let r = match todo {
                VaultTodo::Forget(id) => self.vault.remove(&id),
                VaultTodo::Rebind(id) => match self.store.get(&id).map(vault_target) {
                    Some(target) => self.vault.rebind(&id, &target),
                    None => Ok(()),
                },
            };
            self.report(t("저장된 비밀번호 갱신 실패", "Could not update the saved passwords"), r);
        }
        // Passwords saved by older versions get the target of their site as it is now.
        let store = &self.store;
        let r = self.vault.bind_missing(|id| store.get(id).filter(|p| p.save_password && may_bind_unbound(p)).map(vault_target));
        self.report(t("저장된 비밀번호 갱신 실패", "Could not update the saved passwords"), r);
        match pending {
            Pending::Unlock => {}
            Pending::Connect(id, target) => self.connect_in(target, |app| app.connect_saved(&id)),
            Pending::ConnectMany(ids, layout) => self.open_many(&ids, layout, true),
            Pending::Reconnect(id) => {
                if let Some(i) = self.index_of(id) {
                    self.restore_and_reconnect(i);
                }
            }
            Pending::StoreSecret { id, password, connect, target } => {
                let r = self.set_secret(&id, &password);
                self.report(t("비밀번호 저장 실패", "Could not save the password"), r);
                if connect {
                    self.connect_in(target, |app| app.connect_saved(&id));
                }
            }
            Pending::SaveLogin { id, password } => self.finish_save_login(&id, &password),
        }
    }

    /// The vault dialog was dismissed: carry on without stored secrets.
    pub(super) fn vault_cancelled(&mut self, pending: Pending) {
        match pending {
            Pending::Unlock => {}
            Pending::Connect(id, target) => {
                if let Some(p) = self.store.get(&id).cloned() {
                    self.connect_in(target, |app| app.open_session(&p, None, Some(p.display_name())));
                }
            }
            // Each site asks for its password.
            Pending::ConnectMany(ids, layout) => self.open_many(&ids, layout, false),
            Pending::Reconnect(id) => {
                if let Some(i) = self.index_of(id) {
                    let waker = self.waker.clone();
                    self.sessions[i].reconnect(&self.net, waker);
                }
            }
            Pending::SaveLogin { .. } => {
                self.status = Some(t("비밀번호는 저장되지 않았습니다", "The password was not saved").into());
            }
            Pending::StoreSecret { id, password, connect, target } => {
                self.status = Some(t("비밀번호는 저장되지 않았습니다", "The password was not saved").into());
                if connect && let Some(p) = self.store.get(&id).cloned() {
                    let password = Password::from(password);
                    self.connect_in(target, |app| app.open_session(&p, Some(password), Some(p.display_name())));
                }
            }
        }
    }

    /// Saves a copy of a site (with its stored password, if the vault is open). Returns the copy.
    pub(super) fn duplicate_profile(&mut self, id: &str) -> Option<SessionProfile> {
        let original = self.store.get(id)?.clone();
        let mut copy = match original.duplicate() {
            Ok(copy) => copy,
            Err(e) => {
                self.status = Some(format!("{}: {e}", t("사이트 복사 실패", "Could not duplicate the site")));
                return None;
            }
        };
        // Only a password saved for where the site points now goes with the copy: the copy is
        // bound to its own target, so a changed target would otherwise launder the password.
        let secret = match bound_secret(&self.vault, &original) {
            Ok(secret) => secret,
            Err(saved) => {
                let now = vault_target(&original);
                let saved = if saved.is_empty() { t("알 수 없음", "unknown").to_owned() } else { saved };
                self.status = Some(tr!(
                    "'{}' 의 접속 대상이 비밀번호를 저장할 때와 달라({saved} → {now}) 복제본에 비밀번호를 복사하지 않았습니다",
                    "The target of '{}' differs from when its password was saved ({saved} → {now}), so the password was not copied",
                    original.display_name()
                ));
                None
            }
        };
        copy.save_password &= secret.is_some();
        let r = self.store.upsert(copy.clone());
        self.report(t("사이트 저장 실패", "Could not save the site"), r);
        if let Some(secret) = secret {
            let r = self.vault.set(&copy.id, &vault_target(&copy), &secret);
            self.report(t("비밀번호 복사 실패", "Could not copy the password"), r);
        }
        Some(copy)
    }

    /// Opens the site manager, with `select` selected or a new site started. `target` is the
    /// prompt tab it was opened from, if any.
    pub(super) fn open_site_manager(&mut self, select: Option<&str>, new_site: bool, target: Option<u64>) {
        // Already open (it is not modal): it goes there itself, keeping unsaved edits in view.
        if let Some(manager) = &mut self.site_manager {
            manager.request(select, new_site);
            if target.is_some() {
                self.dialog_target = target;
            }
            return;
        }
        let mut manager = SiteManager::default();
        manager.view = self.settings.site_view;
        if new_site {
            manager.new_site();
        } else if let Some(p) = select.and_then(|id| self.store.get(id)) {
            manager.select(p, has_secret(&self.vault, p));
        }
        self.site_manager = Some(manager);
        self.site_manager_opened = true;
        self.dialog_target = target;
    }

    /// Opens the quick-connect dialog; `target` is the prompt tab it was opened from, if any.
    pub(super) fn open_quick_connect(&mut self, target: Option<u64>) {
        self.modal = Some(Modal::QuickConnect(Box::new(QuickConnect::new())));
        self.dialog_target = target;
    }

    pub(super) fn sidebar_action(&mut self, action: SidebarAction) {
        match action {
            SidebarAction::Connect(id) => self.connect_saved(&id),
            SidebarAction::ConnectMany(ids, layout) => self.connect_many(ids, layout),
            SidebarAction::NewTemplate(ids) => self.modal = Some(Modal::Template(Box::new(TemplateDialog::new(ids)))),
            SidebarAction::OpenTemplate(id) => self.open_template(&id),
            SidebarAction::EditTemplate(id) => {
                if let Some(template) = self.store.template(&id).cloned() {
                    self.modal = Some(Modal::Template(Box::new(TemplateDialog::edit(template))));
                }
            }
            SidebarAction::DeleteTemplate(id) => self.modal = Some(Modal::ConfirmDeleteTemplate(id)),
            SidebarAction::Manage(id) => self.open_site_manager(id.as_deref(), false, None),
            SidebarAction::New => self.open_site_manager(None, true, None),
            SidebarAction::Hide => self.show_sidebar = false,
            SidebarAction::ImportPutty => {
                self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store, ImportSource::Putty))));
            }
            SidebarAction::ImportXshell => {
                self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store, ImportSource::Xshell))));
            }
            SidebarAction::ImportSecureCrt => {
                self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store, ImportSource::SecureCrt))));
            }
            SidebarAction::Duplicate(id) => {
                self.duplicate_profile(&id);
            }
            SidebarAction::Delete(id) => self.modal = Some(Modal::ConfirmDelete(id)),
            SidebarAction::LockVault => self.lock_vault(),
            SidebarAction::UnlockVault => self.unlock_vault(),
            SidebarAction::SetUpVault => self.set_up_vault(),
        }
    }

    pub(super) fn unlock_vault(&mut self) {
        self.modal = Some(Modal::Vault(VaultDialog::new(VaultMode::Unlock, ""), Pending::Unlock));
    }

    /// Sets the master password before any password is saved (the sidebar's "Set up").
    pub(super) fn set_up_vault(&mut self) {
        let reason = t(
            "사이트 비밀번호를 저장해 두면 접속할 때 다시 묻지 않습니다.",
            "With site passwords saved, connecting does not ask for them again.",
        );
        self.modal = Some(Modal::Vault(VaultDialog::new(VaultMode::Create, reason), Pending::Unlock));
    }

    /// Shows the site manager, if open, and applies its outcome.
    pub(super) fn show_site_manager(&mut self, ctx: &egui::Context) {
        let Some(mut manager) = self.site_manager.take() else { return };
        if std::mem::take(&mut self.site_manager_opened) {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        let vault = &self.vault;
        let keys_free = !self.dock.has_focus() && self.modal.is_none();
        manager.macros = self.settings.macros.iter().map(|m| m.name.clone()).collect();
        let result = manager.show(ctx, &self.store, &|p| has_secret(vault, p), keys_free);
        let target = self.dialog_target;
        match result {
            SiteManagerResult::Open => self.site_manager = Some(manager),
            SiteManagerResult::Close => {}
            SiteManagerResult::Save { profile, password, connect, close } => {
                if !connect && !close {
                    self.site_manager = Some(manager);
                }
                self.connect_in(target, |app| app.save_profile(*profile, password, connect));
            }
            SiteManagerResult::Connect(id) => self.connect_in(target, |app| app.connect_saved(&id)),
            SiteManagerResult::Duplicate(id) => {
                if let Some(copy) = self.duplicate_profile(&id) {
                    manager.select(&copy, has_secret(&self.vault, &copy));
                }
                self.site_manager = Some(manager);
            }
            SiteManagerResult::Delete(id) => {
                self.site_manager = Some(manager);
                self.modal = Some(Modal::ConfirmDelete(id));
            }
            SiteManagerResult::CreateFolder(path) => {
                let r = self.store.create_folder(&path);
                self.report(t("폴더를 만들 수 없습니다", "Could not create the folder"), r);
                self.site_manager = Some(manager);
            }
            SiteManagerResult::RenameFolder { from, to } => {
                let r = self.store.rename_folder(&from, &to);
                self.report(t("폴더 이름을 바꿀 수 없습니다", "Could not rename the folder"), r);
                manager.refresh(&self.store, &|p| has_secret(&self.vault, p));
                self.site_manager = Some(manager);
            }
            SiteManagerResult::DeleteFolder(path) => {
                let r = self.store.delete_folder(&path);
                self.report(t("폴더를 삭제할 수 없습니다", "Could not delete the folder"), r);
                manager.refresh(&self.store, &|p| has_secret(&self.vault, p));
                self.site_manager = Some(manager);
            }
            SiteManagerResult::MoveSite { id, folder } => {
                let r = self.store.move_site(&id, &folder);
                self.report(t("사이트를 옮길 수 없습니다", "Could not move the site"), r);
                manager.refresh(&self.store, &|p| has_secret(&self.vault, p));
                self.site_manager = Some(manager);
            }
        }
        // Tree or grid, remembered for the next time.
        if let Some(manager) = &self.site_manager
            && manager.view != self.settings.site_view
        {
            self.settings.site_view = manager.view;
            self.settings_unsaved = Some(Instant::now());
        }
        if self.site_manager.is_none() {
            self.dialog_target = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_goes_only_where_it_was_saved_for() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = playterm_config::Vault::new(dir.path().join("vault.bin"));
        vault.create("master").unwrap();
        let site =
            SessionProfile { id: "a".into(), host: "db01".into(), port: 22, username: "ops".into(), ..SessionProfile::new().unwrap() };
        assert!(matches!(bound_secret(&vault, &site), Ok(None)), "no password saved");
        vault.set("a", &vault_target(&site), "s3cret").unwrap();
        assert_eq!(bound_secret(&vault, &site).unwrap().as_deref().map(String::as_str), Some("s3cret"));
        // The file was changed to point elsewhere: the password is not handed out.
        let moved = SessionProfile { host: "evil.example".into(), ..site.clone() };
        assert_eq!(bound_secret(&vault, &moved).unwrap_err(), vault_target(&site));
        let other_user = SessionProfile { username: "root".into(), ..site.clone() };
        assert!(bound_secret(&vault, &other_user).is_err());
    }

    fn rdp_site(gateway: &str, gateway_user: &str) -> SessionProfile {
        let rdp = playterm_net::RdpSettings { gateway: gateway.into(), gateway_user: gateway_user.into(), ..Default::default() };
        SessionProfile {
            id: "r".into(),
            protocol: Protocol::Rdp,
            host: "desk01".into(),
            port: 3389,
            username: "ops".into(),
            rdp,
            ..SessionProfile::new().unwrap()
        }
    }

    #[test]
    fn a_new_or_changed_rd_gateway_does_not_get_the_saved_password() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = playterm_config::Vault::new(dir.path().join("vault.bin"));
        vault.create("master").unwrap();
        let direct = rdp_site("", "");
        assert_eq!(vault_target(&direct), "rdp://ops@desk01:3389", "direct sites keep their target");
        vault.set("r", &vault_target(&direct), "s3cret").unwrap();
        assert!(bound_secret(&vault, &direct).is_ok());
        // A gateway added to the file afterwards, or a gateway user cleared to fall back on the
        // site's account: the password would go to the gateway.
        assert!(bound_secret(&vault, &rdp_site("gw.example", "")).is_err());

        let approved = rdp_site("gw.example", "");
        vault.set("r", &vault_target(&approved), "s3cret").unwrap();
        assert!(bound_secret(&vault, &approved).is_ok());
        // The same gateway written another way.
        for same in ["GW.example", " gw.example:443 ", "gw.example:443"] {
            assert!(bound_secret(&vault, &rdp_site(same, "")).is_ok(), "{same:?}");
        }
        for changed in [rdp_site("other.example", ""), rdp_site("gw.example:8443", ""), rdp_site("", "")] {
            assert!(bound_secret(&vault, &changed).is_err(), "{:?}", changed.rdp);
        }

        // Its own gateway account, then back to the site's: the password would go to the gateway.
        let own = rdp_site("gw.example", "gwuser");
        vault.set("r", &vault_target(&own), "s3cret").unwrap();
        assert!(bound_secret(&vault, &own).is_ok());
        assert!(bound_secret(&vault, &rdp_site("gw.example", "")).is_err());
        assert!(bound_secret(&vault, &rdp_site("gw.example", "other")).is_err());
    }

    #[test]
    fn gateway_endpoints_compare_with_their_port() {
        assert_eq!(super::super::gateway_endpoint("GW.Example"), "gw.example:443");
        assert_eq!(super::super::gateway_endpoint("gw.example:8443"), "gw.example:8443");
        assert_eq!(super::super::gateway_endpoint("[::1]:8443"), "[::1]:8443");
        assert_eq!(super::super::gateway_endpoint("[::1]"), "[::1]:443");
        assert!(!may_bind_unbound(&rdp_site("gw.example", "")));
        assert!(may_bind_unbound(&rdp_site("", "")));
    }
}
