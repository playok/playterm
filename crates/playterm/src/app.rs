use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};
use playterm_config::{AppSettings, Language, PreventSleep, ProfileStore, SessionProfile, UiTheme, Vault};
use playterm_i18n::{t, tr};
use playterm_net::{ConnectParams, NetRuntime, Waker};
use playterm_term::{LogMode, Palette, Theme};
use zeroize::Zeroizing;

use crate::autologin::LoginScript;
use crate::console::Request;
use crate::dialogs::{self, QuickConnect, QuickConnectResult, VaultDialog, VaultMode, VaultResult};
use crate::import_dialog::{ImportDialog, ImportResult};
use crate::logging::{self, LogPlan};
use crate::menu::{self, Command, MenuState};
use crate::power::{ScreenSaverGuard, SleepGuard};
use crate::profile_form::expand_home;
use crate::search_bar::SearchBar;
use crate::session::{Session, SessionSpec, SessionState, TermSetup};
use crate::settings_dialog::{SettingsDialog, SettingsResult};
use crate::sidebar::{self, Sidebar, SidebarAction, VaultState};
use crate::site_manager::{SiteManager, SiteManagerResult};
use crate::view::UserInput;
use crate::{clipboard, fonts, status_bar, view};

const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);
/// App shortcuts, consumed before the terminal sees the key events. Ctrl+Shift+Tab must come
/// before Ctrl+Tab, whose pattern also matches it.
const SHORTCUTS: &[(KeyboardShortcut, Shortcut)] = &[
    (KeyboardShortcut::new(CTRL_SHIFT, Key::T), Shortcut::QuickConnect),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::N), Shortcut::NewTab),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::S), Shortcut::SiteManager),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::B), Shortcut::Sidebar),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::F), Shortcut::Find),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::I), Shortcut::Broadcast),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::E), Shortcut::Sftp),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::X), Shortcut::SftpPanel),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::Tab), Shortcut::PrevTab),
    (KeyboardShortcut::new(Modifiers::CTRL, Key::Tab), Shortcut::NextTab),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::W), Shortcut::CloseTab),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::D), Shortcut::DuplicateTab),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::C), Shortcut::Copy),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::V), Shortcut::Paste),
    (KeyboardShortcut::new(Modifiers::NONE, Key::F11), Shortcut::Fullscreen),
    (KeyboardShortcut::new(Modifiers::CTRL, Key::Equals), Shortcut::ZoomIn),
    (KeyboardShortcut::new(Modifiers::CTRL, Key::Plus), Shortcut::ZoomIn),
    (KeyboardShortcut::new(Modifiers::CTRL, Key::Minus), Shortcut::ZoomOut),
    (KeyboardShortcut::new(Modifiers::CTRL, Key::Num0), Shortcut::ZoomReset),
];

#[derive(Clone, Copy)]
enum Shortcut {
    QuickConnect,
    NewTab,
    SiteManager,
    Sidebar,
    Find,
    Broadcast,
    Sftp,
    SftpPanel,
    PrevTab,
    NextTab,
    CloseTab,
    DuplicateTab,
    Copy,
    Paste,
    Fullscreen,
    ZoomIn,
    ZoomOut,
    ZoomReset,
}

/// What to do once the vault dialog succeeds (or is cancelled).
enum Pending {
    /// Just unlock (sidebar button).
    Unlock,
    /// Connect a saved profile, using its stored password, in the given tab if any.
    Connect(String, Option<u64>),
    /// Store a password typed in the site manager (and connect, in the given tab if any).
    StoreSecret { id: String, password: Zeroizing<String>, connect: bool, target: Option<u64> },
}

enum Modal {
    QuickConnect(Box<QuickConnect>),
    Vault(VaultDialog, Pending),
    ConfirmDelete(String),
    Shortcuts,
    About,
    Settings(Box<SettingsDialog>),
    Import(Box<ImportDialog>),
}

/// Font size changes from zooming are saved once they have settled for this long.
const SETTINGS_SAVE_DELAY: Duration = Duration::from_secs(1);

/// Simultaneous input: the banner and the receiving tabs.
const BROADCAST_COLOR: Color32 = Color32::from_rgb(230, 140, 30);

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
    /// Last informational message (not an error), shown the same way.
    info: Option<String>,
    window_title: String,
    always_on_top: bool,
    /// Tab (at its prompt) that the connection being opened should use instead of a new tab.
    /// Only set for the duration of one call; see [`Self::connect_in`].
    connect_target: Option<u64>,
    /// Prompt tab that opened the quick-connect dialog or the site manager, so a connection
    /// made from there replaces that prompt.
    dialog_target: Option<u64>,
    settings: AppSettings,
    settings_path: PathBuf,
    /// Zoom changes not saved yet, and when the last one happened.
    settings_unsaved: Option<Instant>,
    /// Ctrl+wheel / pinch steps not yet turned into a whole font size step.
    zoom_accum: f32,
    /// Simultaneous input: typing in the active tab also goes to the other included tabs.
    broadcast: bool,
    /// Holds off Windows sleep while sessions are connected.
    sleep_guard: SleepGuard,
    /// Keeps the screen saver away while the Tools menu item is checked.
    screensaver_guard: ScreenSaverGuard,
    /// The SFTP explorer side panel is open.
    sftp_panel: bool,
}

impl PlaytermApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<SessionProfile>) -> anyhow::Result<Self> {
        // Ctrl+= / Ctrl+- / Ctrl+0 size the terminal font instead of the whole window.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);

        let config_dir = config_dir().context(t("사용자 설정 폴더를 찾을 수 없습니다", "Cannot find the user settings folder"))?;
        let config_dir = config_dir.as_path();
        // Settings first: they choose the language of the messages below.
        let settings_path = config_dir.join("settings.toml");
        let mut settings_error = None;
        let settings = AppSettings::load(&settings_path).unwrap_or_else(|e| {
            settings_error = Some(e);
            AppSettings::default()
        });
        apply_ui(&cc.egui_ctx, &settings);
        let (store, mut status) = load_profiles(&config_dir.join("sessions.toml"));
        if let Some(e) = settings_error {
            status = Some(tr!(
                "설정 파일을 읽을 수 없어 기본값을 씁니다: {e}",
                "Cannot read the settings file, using the defaults: {e}"
            ));
        }
        if let Err(e) = fonts::install(&cc.egui_ctx, &settings.font) {
            status = Some(e);
        }

        let ctx = cc.egui_ctx.clone();
        let mut app = Self {
            net: NetRuntime::new().context(t("네트워크 런타임 시작 실패", "Could not start the network runtime"))?,
            waker: Arc::new(move || ctx.request_repaint()),
            known_hosts: config_dir.join("known_hosts"),
            vault: Vault::new(config_dir.join("vault.toml")),
            modal: None,
            store,
            sessions: Vec::new(),
            active: 0,
            next_id: 1,
            sidebar: Sidebar::default(),
            show_sidebar: true,
            site_manager: None,
            status,
            info: None,
            window_title: String::new(),
            always_on_top: false,
            connect_target: None,
            dialog_target: None,
            settings,
            settings_path,
            settings_unsaved: None,
            zoom_accum: 0.0,
            broadcast: false,
            sleep_guard: SleepGuard::default(),
            screensaver_guard: ScreenSaverGuard::default(),
            sftp_panel: false,
        };
        for profile in initial {
            app.open_session(&profile, None, None);
        }
        Ok(app)
    }

    // -- sessions -------------------------------------------------------------------------------

    fn index_of(&self, id: u64) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    /// Size for a new tab: that of the current one (the real size is applied on the first frame).
    fn tab_size(&self) -> (u16, u16) {
        match self.sessions.get(self.active) {
            Some(s) => (s.terminal.cols() as u16, s.terminal.rows() as u16),
            None => (80, 24),
        }
    }

    /// Opens a tab with the local command prompt.
    fn open_local(&mut self) {
        let (cols, rows) = self.tab_size();
        let setup = self.term_setup();
        self.sessions.push(Session::local(self.next_id, cols, rows, &setup));
        self.next_id += 1;
        self.active = self.sessions.len() - 1;
    }

    /// Connects `spec` in the tab it was typed in, in the current tab if that is an unused
    /// prompt, or else in a new tab.
    fn open_spec(&mut self, spec: SessionSpec) {
        let target = self.connect_target.take().or_else(|| {
            self.sessions.get(self.active).filter(|s| s.state == SessionState::Local).map(|s| s.id)
        });
        if let Some(i) = target.and_then(|id| self.index_of(id)).filter(|&i| self.sessions[i].in_console()) {
            self.sessions[i].start(spec, &self.net, self.waker.clone());
            self.active = i;
            return;
        }
        self.connect_in_new_tab(spec);
    }

    fn connect_in_new_tab(&mut self, mut spec: SessionSpec) {
        (spec.params.cols, spec.params.rows) = self.tab_size();
        let setup = self.term_setup();
        let session = Session::connect(self.next_id, spec, &self.net, self.waker.clone(), &setup);
        self.next_id += 1;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
    }

    /// Runs `connect` (which may open a connection, or a vault dialog that opens one later)
    /// so that the connection replaces the prompt of tab `target`.
    fn connect_in(&mut self, target: Option<u64>, connect: impl FnOnce(&mut Self)) {
        self.connect_target = target;
        connect(self);
        // Consumed by `open_spec`, or carried into a pending vault step; never left behind.
        self.connect_target = None;
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
            keepalive: profile.keepalive.clone(),
            term_type: profile.term_type.clone(),
            cols: 80,
            rows: 24,
            known_hosts: self.known_hosts.clone(),
        };
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
        self.open_spec(SessionSpec { params, charset: profile.charset, name, log, login, site });
    }

    /// Connects a saved profile, unlocking the vault first if its password is stored there.
    fn connect_saved(&mut self, id: &str) {
        let Some(profile) = self.store.get(id).cloned() else { return };
        // SSH logs in with it; Telnet and serial sites type it at the prompt (automatic login).
        let wants_secret = profile.save_password;
        if wants_secret && !self.vault.is_unlocked() && self.vault.exists() {
            let dialog = VaultDialog::new(
                VaultMode::Unlock,
                tr!(
                    "'{}' 의 저장된 비밀번호를 사용합니다.",
                    "Uses the saved password of '{}'.",
                    profile.display_name()
                ),
            );
            let target = self.connect_target.take();
            self.modal = Some(Modal::Vault(dialog, Pending::Connect(id.to_owned(), target)));
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
        self.report(t("사이트 저장 실패", "Could not save the site"), r);

        // If the vault is locked, a stale secret is pruned on the next unlock instead.
        if !save_password && self.vault.is_unlocked() {
            let r = self.vault.remove(&id);
            self.report(t("볼트 갱신 실패", "Could not update the password vault"), r);
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
            self.report(t("비밀번호 저장 실패", "Could not save the password"), r);
            if connect {
                self.connect_saved(&id);
            }
            return;
        }
        let (mode, reason) = if self.vault.exists() {
            (VaultMode::Unlock, t("비밀번호를 저장하려면 볼트를 잠금 해제하세요.", "Unlock the password vault to save the password."))
        } else {
            (VaultMode::Create, t("처음으로 비밀번호를 저장합니다.", "Saving a password for the first time."))
        };
        let target = self.connect_target.take();
        let pending = Pending::StoreSecret { id, password, connect, target };
        self.modal = Some(Modal::Vault(VaultDialog::new(mode, reason), pending));
    }

    /// Runs after the vault was unlocked or created.
    fn vault_ready(&mut self, pending: Pending) {
        // Drop secrets of deleted profiles and of profiles that no longer save passwords.
        let store = &self.store;
        let r = self.vault.retain(|id| store.get(id).is_some_and(|p| p.save_password));
        self.report(t("볼트 정리 실패", "Could not clean up the password vault"), r);
        match pending {
            Pending::Unlock => {}
            Pending::Connect(id, target) => self.connect_in(target, |app| app.connect_saved(&id)),
            Pending::StoreSecret { id, password, connect, target } => {
                let r = self.vault.set(&id, &password);
                self.report(t("비밀번호 저장 실패", "Could not save the password"), r);
                if connect {
                    self.connect_in(target, |app| app.connect_saved(&id));
                }
            }
        }
    }

    /// The vault dialog was dismissed: carry on without stored secrets.
    fn vault_cancelled(&mut self, pending: Pending) {
        match pending {
            Pending::Unlock => {}
            Pending::Connect(id, target) => {
                if let Some(p) = self.store.get(&id).cloned() {
                    self.connect_in(target, |app| app.open_session(&p, None, Some(p.display_name())));
                }
            }
            Pending::StoreSecret { id, password, connect, target } => {
                self.status = Some(t("비밀번호는 저장되지 않았습니다", "The password was not saved").into());
                if connect && let Some(p) = self.store.get(&id).cloned() {
                    let password = password.to_string();
                    self.connect_in(target, |app| app.open_session(&p, Some(password), Some(p.display_name())));
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
        self.report(t("사이트 저장 실패", "Could not save the site"), r);
        if let Some(secret) = secret {
            let r = self.vault.set(&copy.id, &secret);
            self.report(t("비밀번호 복사 실패", "Could not copy the password"), r);
        }
        Some(copy)
    }

    /// Opens the site manager, with `select` selected or a new site started. `target` is the
    /// prompt tab it was opened from, if any.
    fn open_site_manager(&mut self, select: Option<&str>, new_site: bool, target: Option<u64>) {
        let mut manager = SiteManager::default();
        if new_site {
            manager.new_site();
        } else if let Some(p) = select.and_then(|id| self.store.get(id)) {
            manager.select(p, has_secret(&self.vault, p));
        }
        self.site_manager = Some(manager);
        self.dialog_target = target;
    }

    /// Opens the quick-connect dialog; `target` is the prompt tab it was opened from, if any.
    fn open_quick_connect(&mut self, target: Option<u64>) {
        self.modal = Some(Modal::QuickConnect(Box::new(QuickConnect::new())));
        self.dialog_target = target;
    }

    fn sidebar_action(&mut self, action: SidebarAction) {
        match action {
            SidebarAction::Connect(id) => self.connect_saved(&id),
            SidebarAction::Manage(id) => self.open_site_manager(id.as_deref(), false, None),
            SidebarAction::New => self.open_site_manager(None, true, None),
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
        }
        if self.site_manager.is_none() {
            self.dialog_target = None;
        }
    }

    // -- local prompt ---------------------------------------------------------------------------

    /// Carries out commands typed at the prompt of any tab.
    fn console_requests(&mut self) {
        let pending: Vec<(u64, Request)> = self
            .sessions
            .iter_mut()
            .flat_map(|s| {
                let id = s.id;
                std::mem::take(&mut s.requests).into_iter().map(move |r| (id, r))
            })
            .collect();
        for (id, request) in pending {
            self.console_request(id, request);
        }
    }

    fn console_request(&mut self, id: u64, request: Request) {
        let Some(i) = self.index_of(id) else { return };
        let mut output = None;
        match request {
            Request::Connect(profile) => {
                self.connect_in(Some(id), |app| app.open_session(&profile, None, None));
                return;
            }
            Request::OpenSite(query) => match sidebar::find(&self.numbered_sites(), &query) {
                Ok(site) => {
                    self.connect_in(Some(id), |app| app.connect_saved(&site));
                    return;
                }
                Err(e) => output = Some(format!("\x1b[31m{e}\x1b[0m")),
            },
            Request::Sites(filter) => output = Some(self.site_list(&filter)),
            Request::SiteManager => {
                let selected = self.sidebar.selected().map(str::to_owned);
                self.open_site_manager(selected.as_deref(), false, Some(id));
            }
            Request::NewSite => self.open_site_manager(None, true, Some(id)),
            Request::QuickConnect => self.open_quick_connect(Some(id)),
            Request::Reconnect if self.sessions[i].has_target() => {
                let waker = self.waker.clone();
                self.sessions[i].reconnect(&self.net, waker);
                return;
            }
            Request::Reconnect => output = Some(t("다시 연결할 대상이 없습니다", "Nothing to reconnect to").into()),
            Request::Exit => {
                self.close_session(i);
                return;
            }
        }
        let session = &mut self.sessions[i];
        if let Some(text) = output {
            session.print(&text);
        }
        session.prompt();
    }

    /// Saved sites in `sites` order (the numbers `open N` accepts).
    fn numbered_sites(&self) -> Vec<&SessionProfile> {
        sidebar::grouped(&self.store, "").into_iter().flat_map(|(_, items)| items).collect()
    }

    /// `sites` output: numbered sites matching `filter`.
    fn site_list(&self, filter: &str) -> String {
        use crate::console::pad;
        let sites = self.numbered_sites();
        if sites.is_empty() {
            return t(
                "저장된 사이트가 없습니다. open 으로 사이트 관리 창을 열어 추가하세요.",
                "No saved sites. Type open to add one in the Site Manager.",
            )
            .into();
        }
        let mut out = format!(
            "\x1b[90m{}  {}  {}  {}\x1b[0m\n",
            pad(t("번호", "No."), 4),
            pad(t("이름", "Name"), 20),
            pad(t("접속", "Connection"), 34),
            t("폴더", "Folder")
        );
        let mut shown = 0;
        // Numbers count every site, so `open N` works on a filtered list too.
        for (n, p) in sites.iter().enumerate().filter(|(_, p)| sidebar::matches(p, filter)) {
            shown += 1;
            out.push_str(&format!(
                "{:>4}  {}  {}  {}\n",
                n + 1,
                pad(&p.display_name(), 20),
                pad(&sidebar::summary(p), 34),
                p.folder
            ));
        }
        if shown == 0 {
            out.push_str(&tr!("'{}' 와 일치하는 사이트가 없습니다\n", "No sites match '{}'\n", filter.trim()));
        }
        out.push_str(t("\x1b[90mopen 번호|이름 으로 접속\x1b[0m", "\x1b[90mopen <number|name> to connect\x1b[0m"));
        out
    }

    // -- simultaneous input ---------------------------------------------------------------------

    /// The connected tabs, other than the active one, that receive simultaneous input.
    fn broadcast_targets(&self) -> Vec<usize> {
        if !self.broadcast {
            return Vec::new();
        }
        (0..self.sessions.len())
            .filter(|&i| i != self.active)
            .filter(|&i| self.sessions[i].in_broadcast && self.sessions[i].state == SessionState::Connected)
            .collect()
    }

    /// Repeats input typed in the active tab in the other receiving tabs.
    fn broadcast_input(&mut self, inputs: &[UserInput]) {
        if inputs.is_empty() {
            return;
        }
        for i in self.broadcast_targets() {
            for input in inputs {
                input.send_to(&mut self.sessions[i]);
            }
        }
    }

    // -- settings -------------------------------------------------------------------------------

    fn term_setup(&self) -> TermSetup {
        TermSetup { palette: Palette::from_theme(Theme::named(&self.settings.theme)), scrollback: self.settings.scrollback }
    }

    fn view_style(&self) -> view::ViewStyle {
        let selection = Theme::rgb(Theme::named(&self.settings.theme).selection);
        view::ViewStyle {
            font_size: self.settings.font_size,
            selection: Color32::from_rgb(selection.r, selection.g, selection.b),
        }
    }

    /// Makes `new` the current settings, updating fonts and open tabs where they changed.
    fn apply_settings(&mut self, ctx: &egui::Context, new: AppSettings) {
        let new = new.clamped();
        apply_ui(ctx, &new);
        if new.font != self.settings.font
            && let Err(e) = fonts::install(ctx, &new.font)
        {
            self.status = Some(e);
        }
        if new.theme != self.settings.theme {
            let palette = Palette::from_theme(Theme::named(&new.theme));
            for s in &mut self.sessions {
                s.terminal.set_palette(palette.clone());
            }
        }
        if new.scrollback != self.settings.scrollback {
            for s in &mut self.sessions {
                s.terminal.set_scrollback(new.scrollback);
            }
        }
        self.settings = new;
    }

    fn save_settings(&mut self) {
        self.settings_unsaved = None;
        let r = self.settings.save(&self.settings_path);
        self.report(t("설정 저장 실패", "Could not save the settings"), r);
    }

    /// Changes the font size (zoom), saving it once the zooming stops.
    fn set_font_size(&mut self, size: f32) {
        let size = size.clamp(AppSettings::MIN_FONT_SIZE, AppSettings::MAX_FONT_SIZE);
        if size != self.settings.font_size {
            self.settings.font_size = size;
            self.settings_unsaved = Some(Instant::now());
        }
    }

    /// Ctrl+wheel / pinch: one point per whole step, so the size stays tidy.
    fn zoom(&mut self, steps: f32) {
        self.zoom_accum += steps;
        while self.zoom_accum >= 1.0 {
            self.zoom_accum -= 1.0;
            self.set_font_size(self.settings.font_size.floor() + 1.0);
        }
        while self.zoom_accum <= -1.0 {
            self.zoom_accum += 1.0;
            self.set_font_size(self.settings.font_size.ceil() - 1.0);
        }
    }

    // -- commands -------------------------------------------------------------------------------

    fn run(&mut self, ctx: &egui::Context, command: Command) {
        let n = self.sessions.len();
        match command {
            Command::QuickConnect => self.open_quick_connect(None),
            Command::NewTab => self.open_local(),
            Command::SiteManager => {
                let selected = self.sidebar.selected().map(str::to_owned);
                self.open_site_manager(selected.as_deref(), false, None);
            }
            Command::NewSite => self.open_site_manager(None, true, None),
            Command::ImportPutty => self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store)))),
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
                    self.broadcast_input(&[UserInput::Paste(text)]);
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
            Command::ToggleStatusBar => {
                self.settings.status_bar = !self.settings.status_bar;
                self.save_settings();
            }
            Command::ToggleFullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }

            Command::Sftp(i) => {
                if let Some(s) = self.sessions.get_mut(i) {
                    s.open_sftp();
                }
            }
            Command::ToggleSftpPanel => self.sftp_panel = !self.sftp_panel,
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
                    .set_title(t("로그 파일", "Log file"))
                    .set_directory(&dir)
                    .set_file_name(logging::file_name(&session.tab_title()))
                    .add_filter(t("로그", "Logs"), &["log", "txt"])
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
            Command::Duplicate(i) => match self.sessions.get(i).map(Session::spec) {
                // Always a new tab, even when the current one is an unused prompt.
                Some(Some(spec)) => self.connect_in_new_tab(spec),
                Some(None) => self.open_local(),
                None => {}
            },
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
            Command::ToggleBroadcast => self.broadcast = !self.broadcast,
            Command::ToggleInBroadcast(i) => {
                if let Some(s) = self.sessions.get_mut(i) {
                    s.in_broadcast = !s.in_broadcast;
                }
            }
            Command::CloseAll => {
                for mut s in self.sessions.drain(..) {
                    s.close();
                }
                self.active = 0;
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
                let level =
                    if self.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            }

            Command::ZoomIn => self.set_font_size(self.settings.font_size.floor() + 1.0),
            Command::ZoomOut => self.set_font_size(self.settings.font_size.ceil() - 1.0),
            Command::ZoomReset => self.set_font_size(AppSettings::DEFAULT_FONT_SIZE),
            Command::Settings => self.modal = Some(Modal::Settings(Box::new(SettingsDialog::new(&self.settings)))),
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
                QuickConnectResult::Cancel => self.dialog_target = None,
                QuickConnectResult::Connect { profile, password, save } => {
                    if save {
                        let r = self.store.upsert((*profile).clone());
                        self.report(t("사이트 저장 실패", "Could not save the site"), r);
                    }
                    let name = save.then(|| profile.display_name());
                    let target = self.dialog_target.take();
                    self.connect_in(target, |app| app.open_session(&profile, password.map(|p| p.to_string()), name));
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
            Modal::Import(mut dialog) => match dialog.show(ctx) {
                ImportResult::Open => self.modal = Some(Modal::Import(dialog)),
                ImportResult::Cancel => {}
                ImportResult::Import(profiles) => {
                    let count = profiles.len();
                    let folder = profiles.first().map(|p| p.folder.clone()).unwrap_or_default();
                    for p in profiles {
                        let r = self.store.upsert(p);
                        self.report(t("사이트 저장 실패", "Could not save the site"), r);
                    }
                    let place =
                        if folder.is_empty() { String::new() } else { tr!(" ('{folder}' 폴더)", " (folder '{folder}')") };
                    self.info = Some(tr!(
                        "PuTTY 세션 {count}개를 가져왔습니다{place}",
                        "Imported {count} PuTTY sessions{place}"
                    ));
                    self.show_sidebar = true;
                }
            },
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
                let text = tr!("'{name}' 사이트를 삭제할까요?", "Delete site '{name}'?");
                match dialogs::confirm(ctx, t("사이트 삭제", "Delete site"), &text, t("삭제", "Delete")) {
                    None => self.modal = Some(Modal::ConfirmDelete(id)),
                    Some(false) => {}
                    Some(true) => {
                        let r = self.store.remove(&id);
                        self.report(t("사이트 삭제 실패", "Could not delete the site"), r);
                        if self.vault.is_unlocked() {
                            let r = self.vault.remove(&id);
                            self.report(t("볼트 갱신 실패", "Could not update the password vault"), r);
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
            let site = self.sessions[index].site().and_then(|id| self.store.get(id)).cloned();
            if let Some(relogin) = dialogs::auth(ctx, &mut self.sessions[index], site.is_some()) {
                if relogin.save
                    && let Some(mut profile) = site
                {
                    profile.username = relogin.user.clone();
                    let r = self.store.upsert(profile);
                    self.report(t("사이트 저장 실패", "Could not save the site"), r);
                }
                let waker = self.waker.clone();
                self.sessions[index].relogin(relogin.user, relogin.password, &self.net, waker);
            }
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
                Shortcut::NewTab => Command::NewTab,
                Shortcut::SiteManager => Command::SiteManager,
                Shortcut::Sidebar => Command::ToggleSidebar,
                Shortcut::Find => Command::Find,
                Shortcut::Broadcast => Command::ToggleBroadcast,
                Shortcut::Sftp => Command::Sftp(i),
                Shortcut::SftpPanel => Command::ToggleSftpPanel,
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
            };
            self.run(ctx, command);
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let state = MenuState {
            sessions: &self.sessions,
            active: self.active,
            show_sidebar: self.show_sidebar,
            show_status_bar: self.settings.status_bar,
            vault: self.vault_state(),
            always_on_top: self.always_on_top,
            broadcast: self.broadcast,
            prevent_screensaver: self.settings.prevent_screensaver,
            sftp_panel: self.sftp_panel,
        };
        if let Some(command) = menu::menu_bar(ui, &state) {
            let ctx = ui.ctx().clone();
            self.run(&ctx, command);
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut self.show_sidebar, t("사이트", "Sites"))
                .on_hover_text(t("사이트 목록 (Ctrl+Shift+B)", "Site list (Ctrl+Shift+B)"));
            if ui.button(t("+ 빠른 연결", "+ Quick Connect")).on_hover_text("Ctrl+Shift+T").clicked() {
                action = Some(Command::QuickConnect);
            }
            if self.broadcast {
                ui.separator();
                let others = self.broadcast_targets().len();
                ui.label(
                    RichText::new(tr!("동시 입력 켜짐 · 다른 탭 {others}개", "Typing into all tabs · {others} other"))
                        .strong()
                        .color(BROADCAST_COLOR),
                )
                .on_hover_text(t(
                    "이 탭에 치는 내용이 주황색 탭에도 똑같이 입력됩니다. 탭 우클릭으로 뺄 수 있습니다.",
                    "What you type here also goes to the orange tabs. Right-click a tab to leave it out.",
                ));
                if ui.button(t("끄기", "Turn off")).on_hover_text("Ctrl+Shift+I").clicked() {
                    action = Some(Command::ToggleBroadcast);
                }
            }
            ui.separator();
            let receiving = |s: &Session| self.broadcast && s.in_broadcast && s.state == SessionState::Connected;
            for (i, s) in self.sessions.iter().enumerate() {
                let dot = match s.state {
                    SessionState::Local => Color32::from_rgb(140, 140, 140),
                    SessionState::Connecting => Color32::from_rgb(220, 180, 40),
                    SessionState::Connected => Color32::from_rgb(60, 190, 90),
                    SessionState::Closed => Color32::from_rgb(200, 70, 70),
                };
                ui.label(RichText::new("●").color(dot));
                if s.is_logging() {
                    ui.label(RichText::new("REC").small().color(Color32::from_rgb(220, 60, 60)))
                        .on_hover_text(s.terminal.log().map(|l| l.path().display().to_string()).unwrap_or_default());
                }
                let title = RichText::new(s.tab_title());
                let title = if receiving(s) { title.color(BROADCAST_COLOR) } else { title };
                let tab = ui.selectable_label(i == self.active, title);
                if tab.clicked() {
                    self.active = i;
                }
                if tab.middle_clicked() {
                    action = Some(Command::Close(i));
                }
                tab.context_menu(|ui| {
                    menu::session_items(ui, &mut action, i, s);
                    ui.separator();
                    let mark = if s.in_broadcast { "✔" } else { "  " };
                    if ui.button(format!("{mark} {}", t("동시 입력에 포함", "Include in simultaneous input"))).clicked() {
                        action = Some(Command::ToggleInBroadcast(i));
                        ui.close();
                    }
                    ui.separator();
                    let entries = [
                        (t("복제", "Duplicate"), Command::Duplicate(i)),
                        (t("다시 연결", "Reconnect"), Command::Reconnect(i)),
                        (t("닫기", "Close"), Command::Close(i)),
                        (t("다른 탭 모두 닫기", "Close other tabs"), Command::CloseOthers(i)),
                    ];
                    for (label, a) in entries {
                        if ui.button(label).clicked() {
                            action = Some(a);
                            ui.close();
                        }
                    }
                });
                if ui.small_button("✕").on_hover_text(t("탭 닫기 (Ctrl+Shift+W)", "Close tab (Ctrl+Shift+W)")).clicked() {
                    action = Some(Command::Close(i));
                }
                ui.add_space(6.0);
            }
            if self.sessions.get(self.active).is_some_and(|s| s.can_sftp())
                && ui.button("SFTP").on_hover_text(t("파일 브라우저 (Ctrl+Shift+E)", "File browser (Ctrl+Shift+E)")).clicked()
            {
                action = Some(Command::Sftp(self.active));
            }
            if self.sessions.get(self.active).is_some_and(|s| s.state == SessionState::Closed) {
                ui.separator();
                if ui.button(t("다시 연결", "Reconnect")).clicked() {
                    action = Some(Command::Reconnect(self.active));
                }
            }
            if let Some(status) = &self.status {
                ui.separator();
                let label = ui
                    .add(egui::Label::new(RichText::new(status).color(ui.visuals().error_fg_color)).sense(egui::Sense::click()))
                    .on_hover_text(t("클릭해서 닫기", "Click to dismiss"));
                if label.clicked() {
                    self.status = None;
                }
            }
            if let Some(info) = &self.info {
                ui.separator();
                let label = ui
                    .add(egui::Label::new(RichText::new(info).weak()).sense(egui::Sense::click()))
                    .on_hover_text(t("클릭해서 닫기", "Click to dismiss"));
                if label.clicked() {
                    self.info = None;
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
        let panel = self.sftp_panel;
        match self.sessions.get_mut(self.active).and_then(|s| s.sftp.as_mut()).filter(|b| b.open || panel) {
            Some(browser) => browser.upload_paths(paths),
            None => {
                self.status = Some(
                    t(
                        "파일을 올리려면 먼저 SFTP 탐색기(Ctrl+Shift+X)나 SFTP 창(Ctrl+Shift+E)을 여세요",
                        "Open the SFTP explorer (Ctrl+Shift+X) or the SFTP window (Ctrl+Shift+E) first to upload files",
                    )
                    .into(),
                )
            }
        }
    }

    /// The SFTP explorer side panel: the active tab's remote files.
    fn sftp_panel_ui(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        ui.horizontal(|ui| {
            ui.strong(t("SFTP 탐색기", "SFTP explorer"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.small_button("✕").on_hover_text(t("닫기 (Ctrl+Shift+X)", "Close (Ctrl+Shift+X)")).clicked();
            });
        });
        if close {
            self.sftp_panel = false;
        }
        let Some(session) = self.sessions.get_mut(self.active) else { return };
        ui.weak(session.tab_title());
        ui.separator();
        if session.sftp.is_none() && !session.can_sftp() {
            ui.weak(t(
                "SSH 로 연결된 탭에서 원격 파일을 보고 올리거나 받을 수 있습니다.",
                "Shows, uploads and downloads remote files in a tab connected over SSH.",
            ));
            return;
        }
        if let Some(browser) = &mut session.sftp {
            browser.panel(ui);
        } else if let Some(err) = session.sftp_error() {
            ui.colored_label(ui.visuals().error_fg_color, tr!("SFTP 열기 실패: {err}", "Could not open SFTP: {err}"));
            if ui.button(t("다시 시도", "Try again")).clicked() {
                session.retry_sftp();
            }
        } else {
            session.ensure_sftp();
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(t("SFTP 여는 중…", "Opening SFTP…"));
            });
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

/// The folder of the settings, sites, vault and known hosts. `PLAYTERM_CONFIG_DIR` allows a
/// portable install (or an isolated test profile).
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("PLAYTERM_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => directories::ProjectDirs::from("", "", "playterm").map(|d| d.config_dir().to_owned()),
    }
}

/// Language and light/dark look of the windows (the terminal colors are separate).
fn apply_ui(ctx: &egui::Context, settings: &AppSettings) {
    playterm_i18n::set_english(settings.language == Language::English);
    ctx.set_theme(match settings.ui_theme {
        UiTheme::Dark => egui::ThemePreference::Dark,
        UiTheme::Light => egui::ThemePreference::Light,
        UiTheme::System => egui::ThemePreference::System,
    });
    ctx.set_visuals_of(egui::Theme::Light, soft_light());
}

/// egui's light look with the pure whites toned down to light grays (easier on the eyes,
/// close to Windows 11 light mode): panels, dialogs and text fields a step darker, buttons and
/// separators darker still so they keep their contrast.
fn soft_light() -> egui::Visuals {
    let gray = Color32::from_gray;
    let mut v = egui::Visuals::light();
    v.panel_fill = gray(234);
    v.window_fill = gray(240);
    v.extreme_bg_color = gray(246); // text fields
    v.code_bg_color = gray(222);
    let w = &mut v.widgets;
    w.noninteractive.weak_bg_fill = gray(234);
    w.noninteractive.bg_fill = gray(234);
    w.noninteractive.bg_stroke.color = gray(178); // separators
    for (state, fill) in [(&mut w.inactive, 218), (&mut w.hovered, 206), (&mut w.open, 206), (&mut w.active, 160)] {
        state.weak_bg_fill = gray(fill);
        state.bg_fill = gray(fill);
    }
    v
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
        let msg = tr!(
            "세션 파일을 읽을 수 없어 {} 로 옮겼습니다: {err}",
            "Cannot read the sessions file, moved it to {}: {err}",
            backup.display()
        );
        (ProfileStore::empty(path), Some(msg))
    } else {
        // Could not move it: keep working in memory but write elsewhere.
        let mut fallback = path.as_os_str().to_owned();
        fallback.push(".recovered");
        let msg = tr!("세션 파일을 읽을 수 없습니다: {err}", "Cannot read the sessions file: {err}");
        (ProfileStore::empty(PathBuf::from(fallback)), Some(msg))
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
        self.console_requests();
        // A window without tabs still has a prompt to type at.
        if self.sessions.is_empty() {
            self.open_local();
        }
        self.dropped_files(&ctx);
        let connected = self.sessions.iter().any(|s| s.state == SessionState::Connected);
        self.sleep_guard.set(if connected { self.settings.prevent_sleep } else { PreventSleep::Off });
        self.screensaver_guard.set(self.settings.prevent_screensaver);

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
        // Added before the side panel so it spans the whole window width.
        if self.settings.status_bar {
            let session = self.sessions.get(self.active);
            let awake = status_bar::Awake {
                sleep: self.sleep_guard.active(),
                screensaver: self.screensaver_guard.active(),
            };
            egui::Panel::bottom("status").show(ui, |ui| status_bar::show(ui, session, awake));
        }
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

        // Slides in from the right edge; added before the terminal so it takes its room.
        let mut sftp_panel = self.sftp_panel;
        egui::Panel::right("sftp-panel")
            .resizable(true)
            .default_size(340.0)
            .min_size(240.0)
            .drag_to_open(false)
            .show_collapsible(ui, &mut sftp_panel, |ui| self.sftp_panel_ui(ui));
        // Closed with its ✕ (inside) or by dragging the edge (outside).
        self.sftp_panel = self.sftp_panel && sftp_panel;

        for session in &mut self.sessions {
            let title = session.tab_title();
            if let Some(browser) = &mut session.sftp {
                browser.poll();
                browser.show(&ctx, session.id, &title);
                if browser.has_running_transfers() {
                    // Progress arrives at most every 100 ms; keep the bars moving.
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }

        let style = self.view_style();
        let output = egui::CentralPanel::no_frame()
            .show(ui, |ui| self.sessions.get_mut(self.active).map(|s| view::show(ui, s, !modal_open, &style)))
            .inner;
        if let Some(output) = output {
            if output.zoom_steps != 0.0 {
                self.zoom(output.zoom_steps);
            }
            self.broadcast_input(&output.inputs);
        }
        if let Some(changed) = self.settings_unsaved {
            if changed.elapsed() >= SETTINGS_SAVE_DELAY {
                self.save_settings();
            } else {
                ctx.request_repaint_after(SETTINGS_SAVE_DELAY);
            }
        }

        self.update_window_title(&ctx);
    }
}
