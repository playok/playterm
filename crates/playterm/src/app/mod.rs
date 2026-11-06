use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use eframe::egui::{self, Color32, RichText};
use playterm_config::{AppSettings, Language, PreventSleep, ProfileStore, SessionProfile, TemplateLayout, UiTheme, Vault};
use playterm_i18n::{t, tr};
use playterm_net::{ConnectParams, NetRuntime, Password, Protocol, Waker};
use playterm_term::{LogMode, Palette, Theme};
use zeroize::Zeroizing;

use crate::autologin::LoginScript;
use crate::compose_bar::{self, ComposeBar};
use crate::console::Request;
use crate::dialogs::{self, QuickConnect, QuickConnectResult, VaultDialog, VaultMode, VaultResult};
use crate::dock::Dock;
use crate::icon;
use crate::import_dialog::{ImportDialog, ImportResult, ImportSource};
use crate::licenses::LicensesDialog;
use crate::logging::{self, LogPlan};
use crate::menu::{self, Command, MenuState};
use crate::power::{ScreenSaverGuard, SleepGuard};
use crate::profile_form::expand_home;
use crate::search_bar::SearchBar;
use crate::session::{Session, SessionSpec, SessionState, TermSetup, printable};
use crate::settings_dialog::{SettingsDialog, SettingsResult};
use crate::sidebar::{self, Sidebar, SidebarAction, VaultState};
use crate::site_manager::{SiteManager, SiteManagerResult};
use crate::template_dialog::{TemplateDialog, TemplateResult};
use crate::update::{self, Updater};
use crate::view::UserInput;
use crate::{clip_keys, clipboard, fonts, local_fs, status_bar, view};

mod broadcast;
mod closing;
mod commands;
mod frame;
mod prompt;
mod sessions;
mod settings;
pub(crate) mod theme;
mod ui;
mod vault;
mod vault_worker;
mod windows;
mod zmodem;

use self::broadcast::broadcast_status;
use self::closing::{CloseRequest, ClosedTab};
use self::theme::apply_ui;
pub use self::windows::MAIN_WINDOW;
use self::windows::Windows;

/// Largest text a remote program may put on the clipboard (OSC 52).
const MAX_REMOTE_COPY: usize = 1024 * 1024;

/// What to do once the vault dialog succeeds (or is cancelled).
enum Pending {
    /// Just unlock (sidebar button).
    Unlock,
    /// Connect a saved profile, using its stored password, in the given tab if any.
    Connect(String, Option<u64>),
    /// Reconnect the tab with this id, with its site's stored password.
    Reconnect(u64),
    /// Connect several saved profiles, laid out this way.
    ConnectMany(Vec<String>, TemplateLayout),
    /// Store a password typed in the site manager (and connect, in the given tab if any).
    StoreSecret { id: String, password: Zeroizing<String>, connect: bool, target: Option<u64> },
    /// Store the password of a successful login ("save password" in the login dialog).
    SaveLogin { id: String, password: Zeroizing<String> },
}

enum Modal {
    QuickConnect(Box<QuickConnect>),
    Vault(VaultDialog, Pending),
    VaultWorking(VaultDialog, Pending, vault_worker::Task),
    ConfirmDelete(String),
    Template(Box<TemplateDialog>),
    ConfirmDeleteTemplate(String),
    Shortcuts,
    About,
    Licenses(Box<LicensesDialog>),
    Settings(Box<SettingsDialog>),
    Import(Box<ImportDialog>),
    Macros(Box<crate::macros::MacroDialog>),
    ConfirmPasswordMacro(broadcast::MacroPlan),
    /// Closing would end connections: asks first; the flag is the "do not ask again" box.
    ConfirmClose(CloseRequest, bool),
    /// A notice to acknowledge: title and text.
    Notice(String, String),
}

/// Font size changes from zooming are saved once they have settled for this long.
const SETTINGS_SAVE_DELAY: Duration = Duration::from_secs(1);

/// Starts another playterm process (its own window) with a local prompt.
fn open_new_window() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe).spawn().map(drop).map_err(|e| e.to_string())
}

/// What a saved password of `p` is for. Stored with the password (encrypted), so a site that
/// now points somewhere else (sessions.toml is plain text) does not get it.
///
/// An RDP site with an RD Gateway names the gateway too, and whether it logs on to it with the
/// site's account (which sends this password to the gateway) or its own: adding or changing the
/// gateway is a change of where the password goes. Without a gateway the target is as before.
pub fn vault_target(p: &SessionProfile) -> String {
    let scheme = match p.protocol {
        Protocol::Ssh => "ssh",
        Protocol::Telnet => "telnet",
        Protocol::Rdp => "rdp",
        Protocol::Serial => return format!("serial://{}", p.serial.port),
    };
    let target = format!("{scheme}://{}@{}:{}", p.username, p.host.to_lowercase(), p.port);
    match (p.protocol, p.rdp.gateway()) {
        (Protocol::Rdp, Some(gateway)) => {
            let login = match p.rdp.gateway_user() {
                Some(user) => format!("user {user:?}"),
                None => "site account".to_owned(),
            };
            format!("{target} via gateway {} ({login})", gateway_endpoint(gateway))
        }
        _ => target,
    }
}

/// An RD Gateway `host[:port]` as `host:port`, lowercase, with the default port 443 written out,
/// so the same gateway written another way is the same target.
fn gateway_endpoint(gateway: &str) -> String {
    let gateway = gateway.trim().to_lowercase();
    let has_port = match gateway.rsplit_once(':') {
        // `[v6]:port` or `host:port`, not a bare IPv6 address.
        Some((host, port)) => !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && (host.ends_with(']') || !host.contains(':')),
        None => false,
    };
    if has_port { gateway } else { format!("{gateway}:443") }
}

/// Whether a password saved without a target (by a version before targets) may be bound to
/// where `p` points now. Not for an RDP site with a gateway: gateways came after targets, so the
/// gateway was added to the file since, and is not trusted with the password unseen.
fn may_bind_unbound(p: &SessionProfile) -> bool {
    !(p.protocol == Protocol::Rdp && p.rdp.gateway().is_some())
}

/// The saved password of `profile`, when it was saved for where the profile points now.
/// `Err` holds the target it was saved for (empty when unknown, from an older version): the
/// profile was changed since, for instance by editing its file, and the password must not
/// follow it to another host.
fn bound_secret(vault: &playterm_config::Vault, profile: &SessionProfile) -> Result<Option<Zeroizing<String>>, String> {
    let Some(secret) = vault.get(&profile.id) else { return Ok(None) };
    match vault.target(&profile.id) {
        Some(saved) if saved == vault_target(profile) => Ok(Some(Zeroizing::new(secret.to_owned()))),
        Some(saved) => Err(saved.to_owned()),
        None => Err(String::new()),
    }
}

/// A change to a saved password that waits for the vault to be unlocked.
enum VaultTodo {
    /// The site was deleted or no longer saves its password.
    Forget(String),
    /// The user changed the site: its password goes with the new target.
    Rebind(String),
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
    vault_worker: vault_worker::Worker,
    /// Changes made while the vault was locked, applied when it is unlocked.
    vault_todo: Vec<VaultTodo>,
    sessions: Vec<Session>,
    active: usize,
    next_id: u64,
    sidebar: Sidebar,
    show_sidebar: bool,
    /// The site manager window; app modals (vault, delete confirmation) stack above it.
    site_manager: Option<SiteManager>,
    /// The site manager was just opened: the terminal gives up the keyboard to it.
    site_manager_opened: bool,
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
    /// Where ZMODEM downloads are saved (from the settings).
    zmodem_dir: PathBuf,
    /// The folder last picked for a ZMODEM download, where the next question starts.
    zmodem_last: Option<PathBuf>,
    /// Open questions for the folder of a ZMODEM download: the tab, and the answer to come.
    zmodem_questions: Vec<(u64, mpsc::Receiver<Option<PathBuf>>)>,
    /// Open questions for the files of a ZMODEM upload, and the folder they were last picked in.
    upload_questions: Vec<(u64, mpsc::Receiver<Option<Vec<PathBuf>>>)>,
    upload_last: Option<PathBuf>,
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
    /// Command input bar under the terminal (shown per `settings.compose_bar`).
    compose: ComposeBar,
    /// Size of the active tab's area in pixels: the size an RDP desktop opened now would get.
    view_px: (u16, u16),
    /// The tabs and how they are docked (split side by side or above each other), in the window
    /// being drawn.
    dock: Dock,
    /// The window being drawn: [`MAIN_WINDOW`] or one tabs were moved out to. `active`, `dock`
    /// and `window_title` are that window's.
    current_window: u64,
    /// The windows tabs were moved out to.
    windows: Windows,
    /// New releases on GitHub.
    updater: Updater,
    /// Tabs closed lately, to open again (see [`closing`]).
    closed_tabs: Vec<ClosedTab>,
    /// The user agreed to quit with connections up: the next close request goes through.
    quit_confirmed: bool,
    /// The windows have the dark look (for the "auto" terminal colors).
    dark: bool,
    /// A modal dialog was open last frame (see `ui`).
    modal_was_open: bool,
    /// Closes a tooltip a dialog opened over (see `tooltips`).
    tooltip_reset: crate::tooltips::TooltipReset,
    /// The terminal scheme the open tabs were last given.
    palette_theme: &'static str,
}

impl PlaytermApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<SessionProfile>) -> anyhow::Result<Self> {
        let config_dir = config_dir().context(t("사용자 설정 폴더를 찾을 수 없습니다", "Cannot find the user settings folder"))?;
        Self::new_in(cc, initial, &config_dir)
    }

    /// The app with its settings, sites and vault in `config_dir` (tests give each their own).
    pub fn new_in(cc: &eframe::CreationContext<'_>, initial: Vec<SessionProfile>, config_dir: &Path) -> anyhow::Result<Self> {
        // Ctrl+= / Ctrl+- / Ctrl+0 size the terminal font instead of the whole window.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);

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
            status = Some(tr!("설정 파일을 읽을 수 없어 기본값을 씁니다: {e}", "Cannot read the settings file, using the defaults: {e}"));
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
            vault_worker: vault_worker::Worker::default(),
            vault_todo: Vec::new(),
            modal: None,
            store,
            sessions: Vec::new(),
            active: 0,
            next_id: 1,
            sidebar: Sidebar::default(),
            show_sidebar: true,
            site_manager: None,
            site_manager_opened: false,
            status,
            info: None,
            window_title: String::new(),
            always_on_top: false,
            connect_target: None,
            dialog_target: None,
            zmodem_dir: local_fs::zmodem_folder(&settings.zmodem_dir),
            zmodem_last: None,
            zmodem_questions: Vec::new(),
            upload_questions: Vec::new(),
            upload_last: None,
            settings,
            settings_path,
            settings_unsaved: None,
            zoom_accum: 0.0,
            broadcast: false,
            sleep_guard: SleepGuard::default(),
            screensaver_guard: ScreenSaverGuard::default(),
            sftp_panel: false,
            compose: ComposeBar::default(),
            view_px: (1280, 800),
            dock: Dock::default(),
            current_window: MAIN_WINDOW,
            windows: Windows::new(),
            updater: Updater::default(),
            closed_tabs: Vec::new(),
            quit_confirmed: false,
            dark: true,
            modal_was_open: false,
            tooltip_reset: Default::default(),
            palette_theme: "",
        };
        // Development builds (cargo run) are not replaced by releases unless asked.
        if app.settings.check_updates && !cfg!(debug_assertions) {
            app.updater.check(&cc.egui_ctx, false);
        }
        for profile in initial {
            // A saved site (opened in a new window) connects as one: with its vault password.
            if app.store.get(&profile.id).is_some() {
                app.connect_saved(&profile.id);
            } else {
                app.open_session(&profile, None, None);
            }
        }
        Ok(app)
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

/// What the screen-flow tests (`ui_tests`) look at.
#[cfg(test)]
impl PlaytermApp {
    pub(crate) fn test_sessions(&mut self) -> &mut [Session] {
        &mut self.sessions
    }
    pub(crate) fn test_site_manager_open(&self) -> bool {
        self.site_manager.is_some()
    }
    pub(crate) fn test_modal_open(&self) -> bool {
        self.modal.is_some()
    }
    pub(crate) fn test_settings(&self) -> &AppSettings {
        &self.settings
    }
    pub(crate) fn test_store(&self) -> &ProfileStore {
        &self.store
    }
    pub(crate) fn test_broadcast(&self) -> bool {
        self.broadcast
    }
    pub(crate) fn test_window_count(&self) -> usize {
        self.windows.count()
    }
    pub(crate) fn test_run(&mut self, ctx: &egui::Context, command: Command) {
        self.run(ctx, command);
    }
}
