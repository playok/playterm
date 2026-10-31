use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};
use playterm_config::{ProfileStore, SessionProfile, Vault};
use playterm_net::{ConnectParams, NetRuntime, Protocol, Waker};
use playterm_term::LogMode;
use zeroize::Zeroizing;

use crate::dialogs::{self, QuickConnect, QuickConnectResult, VaultDialog, VaultMode, VaultResult};
use crate::logging::{self, LogPlan};
use crate::menu::{self, Command, MenuState};
use crate::profile_form::expand_home;
use crate::session::{Session, SessionSpec, SessionState};
use crate::sidebar::{Sidebar, SidebarAction, VaultState};
use crate::site_manager::{SiteManager, SiteManagerResult};
use crate::{clipboard, fonts, view};

const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);
/// App shortcuts, consumed before the terminal sees the key events. Ctrl+Shift+Tab must come
/// before Ctrl+Tab, whose pattern also matches it.
const SHORTCUTS: &[(KeyboardShortcut, Shortcut)] = &[
    (KeyboardShortcut::new(CTRL_SHIFT, Key::T), Shortcut::QuickConnect),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::S), Shortcut::SiteManager),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::B), Shortcut::Sidebar),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::F), Shortcut::Sftp),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::Tab), Shortcut::PrevTab),
    (KeyboardShortcut::new(Modifiers::CTRL, Key::Tab), Shortcut::NextTab),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::W), Shortcut::CloseTab),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::D), Shortcut::DuplicateTab),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::C), Shortcut::Copy),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::V), Shortcut::Paste),
    (KeyboardShortcut::new(Modifiers::NONE, Key::F11), Shortcut::Fullscreen),
];

#[derive(Clone, Copy)]
enum Shortcut {
    QuickConnect,
    SiteManager,
    Sidebar,
    Sftp,
    PrevTab,
    NextTab,
    CloseTab,
    DuplicateTab,
    Copy,
    Paste,
    Fullscreen,
}

/// What to do once the vault dialog succeeds (or is cancelled).
enum Pending {
    /// Just unlock (sidebar button).
    Unlock,
    /// Connect a saved profile, using its stored password.
    Connect(String),
    /// Store a password typed in the session editor.
    StoreSecret { id: String, password: Zeroizing<String>, connect: bool },
}

enum Modal {
    QuickConnect(Box<QuickConnect>),
    Vault(VaultDialog, Pending),
    ConfirmDelete(String),
    Shortcuts,
    About,
}

/// Whether the vault holds a password for `p` (while locked, the profile flag is the best guess).
fn has_secret(vault: &Vault, p: &SessionProfile) -> bool {
    if vault.is_unlocked() { vault.get(&p.id).is_some() } else { p.save_password && vault.exists() }
}

pub struct PlaytermApp {
    net: NetRuntime,
    waker: Waker,
    known_hosts: PathBuf,
    store: ProfileStore,
    vault: Vault,
    sessions: Vec<Session>,
    active: usize,
    next_id: u64,
    sidebar: Sidebar,
    show_sidebar: bool,
    /// The site manager window; app modals (vault, delete confirmation) stack above it.
    site_manager: Option<SiteManager>,
    modal: Option<Modal>,
    /// Last error, shown in the tab bar until clicked.
    status: Option<String>,
    window_title: String,
    always_on_top: bool,
}

impl PlaytermApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<SessionProfile>) -> anyhow::Result<Self> {
        fonts::install(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        // PLAYTERM_CONFIG_DIR allows a portable install (or an isolated test profile).
        let config_dir = match std::env::var_os("PLAYTERM_CONFIG_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => directories::ProjectDirs::from("", "", "playterm")
                .context("사용자 설정 폴더를 찾을 수 없습니다")?
                .config_dir()
                .to_owned(),
        };
        let config_dir = config_dir.as_path();
        let (store, status) = load_profiles(&config_dir.join("sessions.toml"));

        let ctx = cc.egui_ctx.clone();
        let mut app = Self {
            net: NetRuntime::new().context("네트워크 런타임 시작 실패")?,
            waker: Arc::new(move || ctx.request_repaint()),
            known_hosts: config_dir.join("known_hosts"),
            vault: Vault::new(config_dir.join("vault.toml")),
            modal: (initial.is_empty() && store.profiles().is_empty())
                .then(|| Modal::QuickConnect(Box::new(QuickConnect::new()))),
            store,
            sessions: Vec::new(),
            active: 0,
            next_id: 1,
            sidebar: Sidebar::default(),
            show_sidebar: true,
            site_manager: None,
            status,
            window_title: String::new(),
            always_on_top: false,
        };
        for profile in initial {
            app.open_session(&profile, None, None);
        }
        Ok(app)
    }

    // -- sessions -------------------------------------------------------------------------------

    fn open_spec(&mut self, mut spec: SessionSpec) {
        // The real size is applied on the first frame; this is only the initial PTY request.
        if let Some(s) = self.sessions.get(self.active) {
            spec.params.cols = s.terminal.cols() as u16;
            spec.params.rows = s.terminal.rows() as u16;
        }
        let session = Session::connect(self.next_id, spec, &self.net, self.waker.clone());
        self.next_id += 1;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
    }

    fn open_session(&mut self, profile: &SessionProfile, password: Option<String>, name: Option<String>) {
        let identity_file = (!profile.identity_file.is_empty()).then(|| expand_home(&profile.identity_file));
        // Like OpenSSH, the usual key files are tried when present.
        let default_identity_files = ["id_ed25519", "id_ecdsa", "id_rsa"]
            .iter()
            .map(|name| expand_home(&format!("~/.ssh/{name}")))
            .filter(|p| p.is_file() && Some(p) != identity_file.as_ref())
            .collect();
        let params = ConnectParams {
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
            term_type: profile.term_type.clone(),
            cols: 80,
            rows: 24,
            known_hosts: self.known_hosts.clone(),
        };
        let log = profile.log.auto.then(|| LogPlan {
            dir: logging::log_dir(&profile.log.dir),
            mode: if profile.log.raw { LogMode::Raw } else { LogMode::Text { timestamps: profile.log.timestamps } },
        });
        self.open_spec(SessionSpec { params, charset: profile.charset, name, log });
    }

    /// Connects a saved profile, unlocking the vault first if its password is stored there.
    fn connect_saved(&mut self, id: &str) {
        let Some(profile) = self.store.get(id).cloned() else { return };
        let wants_secret = profile.save_password && profile.protocol == Protocol::Ssh;
        if wants_secret && !self.vault.is_unlocked() && self.vault.exists() {
            let dialog = VaultDialog::new(
                VaultMode::Unlock,
                format!("'{}' 의 저장된 비밀번호를 사용합니다.", profile.display_name()),
            );
            self.modal = Some(Modal::Vault(dialog, Pending::Connect(id.to_owned())));
            return;
        }
        let password = if wants_secret { self.vault.get(id).map(str::to_owned) } else { None };
        self.open_session(&profile, password, Some(profile.display_name()));
    }

    fn close_session(&mut self, index: usize) {
        let mut session = self.sessions.remove(index);
        session.close();
        if self.active > index || self.active >= self.sessions.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    // -- profiles & vault -----------------------------------------------------------------------

    fn report<E: std::fmt::Display>(&mut self, what: &str, result: Result<(), E>) {
        if let Err(e) = result {
            self.status = Some(format!("{what}: {e}"));
        }
    }

    fn save_profile(&mut self, profile: SessionProfile, password: Option<Zeroizing<String>>, connect: bool) {
        let id = profile.id.clone();
        let save_password = profile.save_password;
        let r = self.store.upsert(profile);
        self.report("사이트 저장 실패", r);

        // If the vault is locked, a stale secret is pruned on the next unlock instead.
        if !save_password && self.vault.is_unlocked() {
            let r = self.vault.remove(&id);
            self.report("볼트 갱신 실패", r);
        }
        match password {
            Some(password) if save_password => self.store_secret(id, password, connect),
            Some(password) if connect => {
                // Not saved: use it for this connection only.
                if let Some(p) = self.store.get(&id).cloned() {
                    self.open_session(&p, Some(password.to_string()), Some(p.display_name()));
                }
            }
            _ if connect => self.connect_saved(&id),
            _ => {}
        }
    }

    fn store_secret(&mut self, id: String, password: Zeroizing<String>, connect: bool) {
        if self.vault.is_unlocked() {
            let r = self.vault.set(&id, &password);
            self.report("비밀번호 저장 실패", r);
            if connect {
                self.connect_saved(&id);
            }
            return;
        }
        let (mode, reason) = if self.vault.exists() {
            (VaultMode::Unlock, "비밀번호를 저장하려면 볼트를 잠금 해제하세요.")
        } else {
            (VaultMode::Create, "처음으로 비밀번호를 저장합니다.")
        };
        self.modal =
            Some(Modal::Vault(VaultDialog::new(mode, reason), Pending::StoreSecret { id, password, connect }));
    }

    /// Runs after the vault was unlocked or created.
    fn vault_ready(&mut self, pending: Pending) {
        // Drop secrets of deleted profiles and of profiles that no longer save passwords.
        let store = &self.store;
        let r = self.vault.retain(|id| store.get(id).is_some_and(|p| p.save_password));
        self.report("볼트 정리 실패", r);
        match pending {
            Pending::Unlock => {}
            Pending::Connect(id) => self.connect_saved(&id),
            Pending::StoreSecret { id, password, connect } => {
                let r = self.vault.set(&id, &password);
                self.report("비밀번호 저장 실패", r);
                if connect {
                    self.connect_saved(&id);
                }
            }
        }
    }

    /// The vault dialog was dismissed: carry on without stored secrets.
    fn vault_cancelled(&mut self, pending: Pending) {
        match pending {
            Pending::Unlock => {}
            Pending::Connect(id) => {
                if let Some(p) = self.store.get(&id).cloned() {
                    self.open_session(&p, None, Some(p.display_name()));
                }
            }
            Pending::StoreSecret { id, password, connect } => {
                self.status = Some("비밀번호는 저장되지 않았습니다".into());
                if connect && let Some(p) = self.store.get(&id).cloned() {
                    self.open_session(&p, Some(password.to_string()), Some(p.display_name()));
                }
            }
        }
    }

    /// Saves a copy of a site (with its stored password, if the vault is open). Returns the copy.
    fn duplicate_profile(&mut self, id: &str) -> Option<SessionProfile> {
        let mut copy = self.store.get(id)?.duplicate();
        let secret = self.vault.get(id).map(|s| Zeroizing::new(s.to_owned()));
        copy.save_password &= secret.is_some();
        let r = self.store.upsert(copy.clone());
        self.report("사이트 저장 실패", r);
        if let Some(secret) = secret {
            let r = self.vault.set(&copy.id, &secret);
            self.report("비밀번호 복사 실패", r);
        }
        Some(copy)
    }

    /// Opens the site manager, with `select` selected or a new site started.
    fn open_site_manager(&mut self, select: Option<&str>, new_site: bool) {
        let mut manager = SiteManager::default();
        if new_site {
            manager.new_site();
        } else if let Some(p) = select.and_then(|id| self.store.get(id)) {
            manager.select(p, has_secret(&self.vault, p));
        }
        self.site_manager = Some(manager);
    }

    fn sidebar_action(&mut self, action: SidebarAction) {
        match action {
            SidebarAction::Connect(id) => self.connect_saved(&id),
            SidebarAction::Manage(id) => self.open_site_manager(id.as_deref(), false),
            SidebarAction::New => self.open_site_manager(None, true),
            SidebarAction::Duplicate(id) => {
                self.duplicate_profile(&id);
            }
            SidebarAction::Delete(id) => self.modal = Some(Modal::ConfirmDelete(id)),
            SidebarAction::LockVault => self.vault.lock(),
            SidebarAction::UnlockVault => self.unlock_vault(),
        }
    }

    fn unlock_vault(&mut self) {
        self.modal = Some(Modal::Vault(VaultDialog::new(VaultMode::Unlock, ""), Pending::Unlock));
    }

    /// Shows the site manager, if open, and applies its outcome.
    fn show_site_manager(&mut self, ctx: &egui::Context) {
        let Some(mut manager) = self.site_manager.take() else { return };
        let vault = &self.vault;
        let result = manager.show(ctx, &self.store, &|p| has_secret(vault, p));
        match result {
            SiteManagerResult::Open => self.site_manager = Some(manager),
            SiteManagerResult::Close => {}
            SiteManagerResult::Save { profile, password, connect, close } => {
                if !connect && !close {
                    self.site_manager = Some(manager);
                }
                self.save_profile(*profile, password, connect);
            }
            SiteManagerResult::Connect(id) => self.connect_saved(&id),
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
        }
    }

    // -- commands -------------------------------------------------------------------------------

    fn run(&mut self, ctx: &egui::Context, command: Command) {
        let n = self.sessions.len();
        match command {
            Command::QuickConnect => self.modal = Some(Modal::QuickConnect(Box::new(QuickConnect::new()))),
            Command::SiteManager => {
                let selected = self.sidebar.selected().map(str::to_owned);
                self.open_site_manager(selected.as_deref(), false);
            }
            Command::NewSite => self.open_site_manager(None, true),
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),

            Command::Copy => {
                if let Some(s) = self.sessions.get_mut(self.active) {
                    view::copy_selection(ctx, s);
                }
            }
            Command::Paste => {
                if let Some(s) = self.sessions.get_mut(self.active)
                    && let Some(text) = clipboard::read_text()
                {
                    view::paste(s, &text);
                }
            }
            Command::SelectAll => {
                if let Some(s) = self.sessions.get_mut(self.active) {
                    s.terminal.select_all();
                }
            }
            Command::ClearScrollback => {
                if let Some(s) = self.sessions.get_mut(self.active) {
                    s.terminal.clear_scrollback();
                }
            }

            Command::ToggleSidebar => self.show_sidebar = !self.show_sidebar,
            Command::ToggleFullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }

            Command::Sftp(i) => {
                if let Some(s) = self.sessions.get_mut(i) {
                    s.open_sftp();
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
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("로그 파일")
                    .set_directory(&dir)
                    .set_file_name(logging::file_name(&session.tab_title()))
                    .add_filter("로그", &["log", "txt"])
                    .save_file()
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
            Command::LockVault => self.vault.lock(),
            Command::UnlockVault => self.unlock_vault(),

            Command::Activate(i) if i < n => self.active = i,
            Command::Activate(_) => {}
            Command::NextTab if n > 0 => self.active = (self.active + 1) % n,
            Command::PrevTab if n > 0 => self.active = (self.active + n - 1) % n,
            Command::NextTab | Command::PrevTab => {}
            Command::Duplicate(i) => {
                if let Some(s) = self.sessions.get(i) {
                    let spec = s.spec();
                    self.open_spec(spec);
                }
            }
            Command::Reconnect(i) if i < n => {
                let waker = self.waker.clone();
                self.sessions[i].reconnect(&self.net, waker);
                self.active = i;
            }
            Command::Close(i) if i < n => self.close_session(i),
            Command::Reconnect(_) | Command::Close(_) => {}
            Command::CloseOthers(i) if i < n => {
                let keep = self.sessions.remove(i);
                for mut s in self.sessions.drain(..) {
                    s.close();
                }
                self.sessions.push(keep);
                self.active = 0;
            }
            Command::CloseOthers(_) => {}
            Command::CloseAll => {
                for mut s in self.sessions.drain(..) {
                    s.close();
                }
                self.active = 0;
            }

            Command::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            Command::ToggleMaximize => {
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }
            Command::ToggleAlwaysOnTop => {
                self.always_on_top = !self.always_on_top;
                let level =
                    if self.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            }

            Command::Shortcuts => self.modal = Some(Modal::Shortcuts),
            Command::About => self.modal = Some(Modal::About),
        }
    }

    // -- ui -------------------------------------------------------------------------------------

    /// Shows the app-level modal, if any, and applies its outcome.
    fn show_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.take() else { return };
        match modal {
            Modal::QuickConnect(mut dialog) => match dialog.show(ctx) {
                QuickConnectResult::Open => self.modal = Some(Modal::QuickConnect(dialog)),
                QuickConnectResult::Cancel => {}
                QuickConnectResult::Connect { profile, password, save } => {
                    if save {
                        let r = self.store.upsert((*profile).clone());
                        self.report("사이트 저장 실패", r);
                    }
                    let name = save.then(|| profile.display_name());
                    self.open_session(&profile, password.map(|p| p.to_string()), name);
                }
            },
            Modal::Shortcuts => {
                if !dialogs::shortcuts(ctx) {
                    self.modal = Some(Modal::Shortcuts);
                }
            }
            Modal::About => {
                if !dialogs::about(ctx) {
                    self.modal = Some(Modal::About);
                }
            }
            Modal::Vault(mut dialog, pending) => match dialog.show(ctx) {
                VaultResult::Open => self.modal = Some(Modal::Vault(dialog, pending)),
                VaultResult::Cancel => self.vault_cancelled(pending),
                VaultResult::Submit(master) => {
                    let result = match dialog.mode {
                        VaultMode::Unlock => self.vault.unlock(&master),
                        VaultMode::Create => self.vault.create(&master),
                    };
                    match result {
                        Ok(()) => self.vault_ready(pending),
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
            Modal::ConfirmDelete(id) => {
                let name = self.store.get(&id).map(|p| p.display_name()).unwrap_or_default();
                match dialogs::confirm(ctx, "사이트 삭제", &format!("'{name}' 사이트를 삭제할까요?"), "삭제") {
                    None => self.modal = Some(Modal::ConfirmDelete(id)),
                    Some(false) => {}
                    Some(true) => {
                        let r = self.store.remove(&id);
                        self.report("사이트 삭제 실패", r);
                        if self.vault.is_unlocked() {
                            let r = self.vault.remove(&id);
                            self.report("볼트 갱신 실패", r);
                        }
                    }
                }
            }
        }
    }

    /// Prompts from the network layer take priority over app dialogs.
    fn show_session_prompts(&mut self, ctx: &egui::Context) -> bool {
        if let Some(index) = self.sessions.iter().position(|s| s.host_key.is_some()) {
            self.active = index;
            dialogs::host_key(ctx, &mut self.sessions[index]);
            return true;
        }
        if let Some(index) = self.sessions.iter().position(|s| s.auth.is_some()) {
            self.active = index;
            dialogs::auth(ctx, &mut self.sessions[index]);
            return true;
        }
        false
    }

    /// App shortcuts, consumed before the terminal sees the key events.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        for (shortcut, what) in SHORTCUTS {
            if !ctx.input_mut(|i| i.consume_shortcut(shortcut)) {
                continue;
            }
            let i = self.active;
            let command = match what {
                Shortcut::QuickConnect => Command::QuickConnect,
                Shortcut::SiteManager => Command::SiteManager,
                Shortcut::Sidebar => Command::ToggleSidebar,
                Shortcut::Sftp => Command::Sftp(i),
                Shortcut::PrevTab => Command::PrevTab,
                Shortcut::NextTab => Command::NextTab,
                Shortcut::CloseTab => Command::Close(i),
                Shortcut::DuplicateTab => Command::Duplicate(i),
                Shortcut::Copy => Command::Copy,
                Shortcut::Paste => Command::Paste,
                Shortcut::Fullscreen => Command::ToggleFullscreen,
            };
            self.run(ctx, command);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let state = MenuState {
            sessions: &self.sessions,
            active: self.active,
            show_sidebar: self.show_sidebar,
            vault: self.vault_state(),
            always_on_top: self.always_on_top,
        };
        if let Some(command) = menu::menu_bar(ui, &state) {
            let ctx = ui.ctx().clone();
            self.run(&ctx, command);
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut self.show_sidebar, "사이트").on_hover_text("사이트 목록 (Ctrl+Shift+B)");
            if ui.button("+ 빠른 연결").on_hover_text("Ctrl+Shift+T").clicked() {
                action = Some(Command::QuickConnect);
            }
            ui.separator();
            for (i, s) in self.sessions.iter().enumerate() {
                let dot = match s.state {
                    SessionState::Connecting => Color32::from_rgb(220, 180, 40),
                    SessionState::Connected => Color32::from_rgb(60, 190, 90),
                    SessionState::Closed => Color32::from_rgb(200, 70, 70),
                };
                ui.label(RichText::new("●").color(dot));
                if s.is_logging() {
                    ui.label(RichText::new("REC").small().color(Color32::from_rgb(220, 60, 60)))
                        .on_hover_text(s.terminal.log().map(|l| l.path().display().to_string()).unwrap_or_default());
                }
                let tab = ui.selectable_label(i == self.active, s.tab_title());
                if tab.clicked() {
                    self.active = i;
                }
                if tab.middle_clicked() {
                    action = Some(Command::Close(i));
                }
                tab.context_menu(|ui| {
                    menu::session_items(ui, &mut action, i, s);
                    ui.separator();
                    let entries = [
                        ("복제", Command::Duplicate(i)),
                        ("다시 연결", Command::Reconnect(i)),
                        ("닫기", Command::Close(i)),
                        ("다른 탭 모두 닫기", Command::CloseOthers(i)),
                    ];
                    for (label, a) in entries {
                        if ui.button(label).clicked() {
                            action = Some(a);
                            ui.close();
                        }
                    }
                });
                if ui.small_button("✕").on_hover_text("탭 닫기 (Ctrl+Shift+W)").clicked() {
                    action = Some(Command::Close(i));
                }
                ui.add_space(6.0);
            }
            if self.sessions.get(self.active).is_some_and(|s| s.can_sftp())
                && ui.button("SFTP").on_hover_text("파일 브라우저 (Ctrl+Shift+F)").clicked()
            {
                action = Some(Command::Sftp(self.active));
            }
            if self.sessions.get(self.active).is_some_and(|s| s.state == SessionState::Closed) {
                ui.separator();
                if ui.button("다시 연결").clicked() {
                    action = Some(Command::Reconnect(self.active));
                }
            }
            if let Some(status) = &self.status {
                ui.separator();
                let label = ui
                    .add(egui::Label::new(RichText::new(status).color(ui.visuals().error_fg_color)).sense(egui::Sense::click()))
                    .on_hover_text("클릭해서 닫기");
                if label.clicked() {
                    self.status = None;
                }
            }
        });

        if let Some(command) = action {
            let ctx = ui.ctx().clone();
            self.run(&ctx, command);
        }
    }

    fn update_window_title(&mut self, ctx: &egui::Context) {
        let title = match self.sessions.get(self.active) {
            Some(s) => format!("{} - playterm", s.tab_title()),
            None => "playterm".to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// Files dropped on the window are uploaded into the active session's SFTP directory.
    fn dropped_files(&mut self, ctx: &egui::Context) {
        let paths: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| p.exists()).collect());
        if paths.is_empty() {
            return;
        }
        match self.sessions.get_mut(self.active).and_then(|s| s.sftp.as_mut()).filter(|b| b.open) {
            Some(browser) => browser.upload_paths(paths),
            None => self.status = Some("파일을 올리려면 먼저 SFTP 창을 여세요 (Ctrl+Shift+F)".into()),
        }
    }

    fn vault_state(&self) -> VaultState {
        if self.vault.is_unlocked() {
            VaultState::Unlocked
        } else if self.vault.exists() {
            VaultState::Locked
        } else {
            VaultState::Missing
        }
    }
}

/// Loads `sessions.toml`. A file that cannot be parsed is moved aside, never overwritten.
fn load_profiles(path: &Path) -> (ProfileStore, Option<String>) {
    let err = match ProfileStore::load(path) {
        Ok(store) => return (store, None),
        Err(e) => e,
    };
    let mut backup = path.as_os_str().to_owned();
    backup.push(".broken");
    let backup = PathBuf::from(backup);
    if std::fs::rename(path, &backup).is_ok() {
        let msg = format!("세션 파일을 읽을 수 없어 {} 로 옮겼습니다: {err}", backup.display());
        (ProfileStore::empty(path), Some(msg))
    } else {
        // Could not move it: keep working in memory but write elsewhere.
        let mut fallback = path.as_os_str().to_owned();
        fallback.push(".recovered");
        (ProfileStore::empty(PathBuf::from(fallback)), Some(format!("세션 파일을 읽을 수 없습니다: {err}")))
    }
}

impl eframe::App for PlaytermApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        for session in &mut self.sessions {
            session.poll();
            if let Some(text) = session.clipboard_store.take() {
                ctx.copy_text(text);
            }
        }
        self.dropped_files(&ctx);

        let prompt_open = self.show_session_prompts(&ctx);
        if !prompt_open {
            if self.modal.is_none() && self.site_manager.is_none() {
                self.shortcuts(&ctx);
            }
            // App modals are shown after the site manager so they stack above it.
            self.show_site_manager(&ctx);
            self.show_modal(&ctx);
        }
        let modal_open = prompt_open || self.modal.is_some() || self.site_manager.is_some();

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        if self.show_sidebar {
            let vault = self.vault_state();
            let action = egui::Panel::left("sessions")
                .resizable(true)
                .default_size(220.0)
                .min_size(140.0)
                .show(ui, |ui| self.sidebar.show(ui, &self.store, vault))
                .inner;
            if let Some(action) = action {
                self.sidebar_action(action);
            }
        }

        for session in &mut self.sessions {
            let title = session.tab_title();
            if let Some(browser) = &mut session.sftp {
                browser.poll();
                if browser.open {
                    browser.show(&ctx, session.id, &title);
                }
                if browser.has_running_transfers() {
                    // Progress arrives at most every 100 ms; keep the bars moving.
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }

        egui::CentralPanel::no_frame().show(ui, |ui| match self.sessions.get_mut(self.active) {
            Some(session) => view::show(ui, session, !modal_open),
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        "왼쪽 사이트 목록에서 더블클릭하거나 [파일 > 사이트 관리] (Ctrl+Shift+S),\n\
                         [+ 빠른 연결] (Ctrl+Shift+T) 로 접속하세요",
                    )
                });
            }
        });

        self.update_window_title(&ctx);
    }
}
