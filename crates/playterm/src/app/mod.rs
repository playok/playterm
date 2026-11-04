use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};
use playterm_config::{AppSettings, Language, PreventSleep, ProfileStore, SessionProfile, TemplateLayout, UiTheme, Vault};
use playterm_i18n::{t, tr};
use playterm_net::{ConnectParams, NetRuntime, Protocol, Waker};
use playterm_term::{LogMode, Palette, Theme};
use zeroize::Zeroizing;

use crate::autologin::LoginScript;
use crate::compose_bar::{self, ComposeBar};
use crate::console::Request;
use crate::dialogs::{self, QuickConnect, QuickConnectResult, VaultDialog, VaultMode, VaultResult};
use crate::dock::{BROADCAST_COLOR, Dock};
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

mod sessions;
mod vault;
mod prompt;

const CTRL_SHIFT: Modifiers = Modifiers::CTRL.plus(Modifiers::SHIFT);
/// Largest text a remote program may put on the clipboard (OSC 52).
const MAX_REMOTE_COPY: usize = 1024 * 1024;
/// App shortcuts, consumed before the terminal sees the key events. Ctrl+Shift+Tab must come
/// before Ctrl+Tab, whose pattern also matches it.
const SHORTCUTS: &[(KeyboardShortcut, Shortcut)] = &[
    (KeyboardShortcut::new(CTRL_SHIFT, Key::T), Shortcut::QuickConnect),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::N), Shortcut::NewTab),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::S), Shortcut::SiteManager),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::B), Shortcut::Sidebar),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::F), Shortcut::Find),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::I), Shortcut::Broadcast),
    // Ctrl+Shift+E opened the SFTP window, which the side panel replaced.
    (KeyboardShortcut::new(CTRL_SHIFT, Key::E), Shortcut::SftpPanel),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::X), Shortcut::SftpPanel),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::M), Shortcut::ComposeBar),
    (KeyboardShortcut::new(CTRL_SHIFT, Key::Tab), Shortcut::PrevTab),
    (KeyboardShortcut::new(Modifiers::CTRL, Key::Tab), Shortcut::NextTab),
    (KeyboardShortcut::new(Modifiers::ALT, Key::ArrowLeft), Shortcut::PrevTab),
    (KeyboardShortcut::new(Modifiers::ALT, Key::ArrowRight), Shortcut::NextTab),
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
    SftpPanel,
    ComposeBar,
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
    NewWindow,
    Settings,
    Quit,
}

/// macOS: the Cmd shortcuts Mac apps share, besides the ones above (Cmd never reaches the
/// shell, so these take nothing from the terminal).
const MAC_SHORTCUTS: &[(KeyboardShortcut, Shortcut)] = &[
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::T), Shortcut::NewTab),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::W), Shortcut::CloseTab),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::N), Shortcut::NewWindow),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::F), Shortcut::Find),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::Comma), Shortcut::Settings),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::Q), Shortcut::Quit),
    (KeyboardShortcut::new(Modifiers::MAC_CMD.plus(Modifiers::SHIFT), Key::OpenBracket), Shortcut::PrevTab),
    (KeyboardShortcut::new(Modifiers::MAC_CMD.plus(Modifiers::SHIFT), Key::CloseBracket), Shortcut::NextTab),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::Equals), Shortcut::ZoomIn),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::Plus), Shortcut::ZoomIn),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::Minus), Shortcut::ZoomOut),
    (KeyboardShortcut::new(Modifiers::MAC_CMD, Key::Num0), Shortcut::ZoomReset),
];

/// The app shortcuts of this platform.
fn shortcut_table() -> impl Iterator<Item = &'static (KeyboardShortcut, Shortcut)> {
    let mac: &[(KeyboardShortcut, Shortcut)] = if cfg!(target_os = "macos") { MAC_SHORTCUTS } else { &[] };
    SHORTCUTS.iter().chain(mac)
}

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
    ConfirmDelete(String),
    Template(Box<TemplateDialog>),
    ConfirmDeleteTemplate(String),
    Shortcuts,
    About,
    Licenses(Box<LicensesDialog>),
    Settings(Box<SettingsDialog>),
    Import(Box<ImportDialog>),
    Macros(Box<crate::macros::MacroDialog>),
}

/// Font size changes from zooming are saved once they have settled for this long.
const SETTINGS_SAVE_DELAY: Duration = Duration::from_secs(1);

/// Starts another playterm process (its own window). With a profile, the new window connects
/// it: the profile goes through a temporary file that the new process reads and deletes. It
/// holds no password; a saved site's comes from the vault there, others are asked again.
fn open_new_window(profile: Option<&SessionProfile>) -> Result<(), String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(exe);
    if let Some(profile) = profile {
        let name = format!("playterm-open-{}-{}.toml", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, profile.to_toml()?).map_err(|e| e.to_string())?;
        cmd.arg("--open").arg(&path);
    }
    cmd.spawn().map(drop).map_err(|e| e.to_string())
}

/// What a saved password of `p` is for. Stored with the password (encrypted), so a site that
/// now points somewhere else (sessions.toml is plain text) does not get it.
pub fn vault_target(p: &SessionProfile) -> String {
    let scheme = match p.protocol {
        Protocol::Ssh => "ssh",
        Protocol::Telnet => "telnet",
        Protocol::Rdp => "rdp",
        Protocol::Serial => return format!("serial://{}", p.serial.port),
    };
    format!("{scheme}://{}@{}:{}", p.username, p.host.to_lowercase(), p.port)
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
    /// The tabs and how they are docked (split side by side or above each other).
    dock: Dock,
    /// New releases on GitHub.
    updater: Updater,
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
            updater: Updater::default(),
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

    // -- simultaneous input ---------------------------------------------------------------------

    /// The connected tabs, other than the active one, that receive simultaneous input.
    fn broadcast_targets(&self) -> Vec<usize> {
        // Only what is typed into a connected tab is repeated: text typed at a local prompt
        // (`exit`, `ssh root@db`) must not run in every connected shell.
        let active_connected = self.sessions.get(self.active).is_some_and(|s| s.state == SessionState::Connected);
        if !self.broadcast || !active_connected {
            return Vec::new();
        }
        (0..self.sessions.len())
            .filter(|&i| i != self.active)
            .filter(|&i| self.sessions[i].in_broadcast && self.sessions[i].state == SessionState::Connected)
            .collect()
    }

    /// Types a message from the command input bar: into the current tab (and the tabs receiving
    /// simultaneous input), or into every connected tab.
    fn send_composed(&mut self, send: compose_bar::Send) {
        let mut targets = vec![self.active];
        if send.to_all {
            targets.extend(
                (0..self.sessions.len())
                    .filter(|&i| i != self.active)
                    .filter(|&i| self.sessions[i].in_broadcast && self.sessions[i].state == SessionState::Connected),
            );
        } else {
            targets.extend(self.broadcast_targets());
        }
        for i in targets {
            let Some(session) = self.sessions.get_mut(i) else { continue };
            for line in compose_bar::lines(&send.text) {
                session.send_text(line);
                session.send_bytes(vec![b'\r']);
            }
        }
    }

    /// Runs macro `k` in the active tab and the tabs receiving simultaneous input.
    fn run_macro(&mut self, k: usize) {
        self.run_macro_in(k, false);
    }

    /// Runs macro `k` in the active tab and the tabs receiving simultaneous input, or with
    /// `to_all` in every connected tab included in simultaneous input (the command bar's choice).
    fn run_macro_in(&mut self, k: usize, to_all: bool) {
        let Some(m) = self.settings.macros.get(k) else { return };
        let steps = match crate::macros::parse(&m.script) {
            Ok(steps) => steps,
            Err(e) => {
                self.status = Some(tr!("매크로 '{}' 를 실행할 수 없습니다: {e}", "Cannot run macro '{}': {e}", m.name));
                return;
            }
        };
        let name = m.name.clone();
        let mut targets = vec![self.active];
        if to_all {
            targets.extend(
                (0..self.sessions.len())
                    .filter(|&i| i != self.active)
                    .filter(|&i| self.sessions[i].in_broadcast && self.sessions[i].state == SessionState::Connected),
            );
        } else {
            targets.extend(self.broadcast_targets());
        }
        for i in targets {
            if let Some(session) = self.sessions.get_mut(i) {
                session.run_macro(&name, steps.clone());
            }
        }
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
        let recent = self
            .settings
            .recent_sites
            .iter()
            .filter_map(|id| self.store.get(id))
            .take(5)
            .map(|p| view::RecentSite { id: p.id.clone(), name: p.display_name(), protocol: p.protocol })
            .collect();
        view::ViewStyle {
            font_size: self.settings.font_size,
            selection: Color32::from_rgb(selection.r, selection.g, selection.b),
            clipboard: self.settings.clipboard,
            recent,
            // Only macOS offers the choice (Option also types accented letters there).
            alt_is_meta: !cfg!(target_os = "macos") || self.settings.option_as_meta,
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
        if new.zmodem_dir != self.settings.zmodem_dir {
            self.zmodem_dir = local_fs::zmodem_folder(&new.zmodem_dir);
            self.zmodem_last = None;
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
            Command::ImportPutty => {
                self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store, ImportSource::Putty))));
            }
            Command::ImportXshell => {
                self.modal = Some(Modal::Import(Box::new(ImportDialog::new(&self.store, ImportSource::Xshell))));
            }
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),

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
            Command::LockVault => self.lock_vault(),
            Command::UnlockVault => self.unlock_vault(),
            Command::SetUpVault => self.set_up_vault(),

            Command::Activate(i) if i < n => self.active = i,
            Command::Activate(_) => {}
            Command::NextTab if n > 0 => self.active = (self.active + 1) % n,
            Command::PrevTab if n > 0 => self.active = (self.active + n - 1) % n,
            Command::NextTab | Command::PrevTab => {}
            Command::OpenInNewWindow(i) => {
                let profile = self.sessions.get(i).and_then(Session::profile).cloned();
                let r = open_new_window(profile.as_ref());
                self.report(t("새 창을 열 수 없습니다", "Cannot open a new window"), r);
            }
            Command::NewWindow => {
                let r = open_new_window(None);
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
                if !dialogs::shortcuts(ctx, &self.settings.clipboard) {
                    self.modal = Some(Modal::Shortcuts);
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
                    let source = source.name();
                    for p in profiles {
                        let r = self.store.upsert(p);
                        self.report(t("사이트 저장 실패", "Could not save the site"), r);
                    }
                    let place = if folder.is_empty() { String::new() } else { tr!(" ('{folder}' 폴더)", " (folder '{folder}')") };
                    self.info = Some(tr!("{source} 세션 {count}개를 가져왔습니다{place}", "Imported {count} {source} sessions{place}"));
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
            Modal::Template(mut dialog) => match dialog.show(ctx, &self.store) {
                TemplateResult::Open => self.modal = Some(Modal::Template(dialog)),
                TemplateResult::Cancel => {}
                TemplateResult::Save(template) => {
                    let name = template.name.clone();
                    let r = self.store.upsert_template(template);
                    self.report(t("템플릿 저장 실패", "Could not save the template"), r);
                    self.info = Some(tr!(
                        "'{name}' 템플릿을 저장했습니다 (사이트 목록 위 '템플릿' 에서 더블클릭으로 엽니다)",
                        "Saved template '{name}' (double-click it under 'Templates' in the site list)"
                    ));
                    self.show_sidebar = true;
                }
            },
            Modal::ConfirmDeleteTemplate(id) => {
                let name = self.store.template(&id).map(|tpl| tpl.name.clone()).unwrap_or_default();
                let text = tr!("'{name}' 템플릿을 삭제할까요? (사이트는 그대로 남습니다)", "Delete template '{name}'? (Its sites stay.)");
                match dialogs::confirm(ctx, t("템플릿 삭제", "Delete template"), &text, t("삭제", "Delete")) {
                    None => self.modal = Some(Modal::ConfirmDeleteTemplate(id)),
                    Some(false) => {}
                    Some(true) => {
                        let r = self.store.remove_template(&id);
                        self.report(t("템플릿 삭제 실패", "Could not delete the template"), r);
                    }
                }
            }
            Modal::ConfirmDelete(id) => {
                let name = self.store.get(&id).map(|p| p.display_name()).unwrap_or_default();
                let text = tr!("'{name}' 사이트를 삭제할까요?", "Delete site '{name}'?");
                match dialogs::confirm(ctx, t("사이트 삭제", "Delete site"), &text, t("삭제", "Delete")) {
                    None => self.modal = Some(Modal::ConfirmDelete(id)),
                    Some(false) => {}
                    Some(true) => {
                        let r = self.store.remove(&id);
                        self.report(t("사이트 삭제 실패", "Could not delete the site"), r);
                        self.forget_secret(&id);
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
        if let Some(index) = self.sessions.iter().position(|s| s.pending_paste.is_some()) {
            self.active = index;
            let text = self.sessions[index].pending_paste.clone().unwrap_or_default();
            match dialogs::confirm_paste(ctx, &text) {
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

    fn update_dialog(&mut self, ctx: &egui::Context) {
        let connected = self.sessions.iter().filter(|s| s.state == SessionState::Connected).count();
        match self.updater.show(ctx, connected) {
            None => {}
            Some(update::Action::Skip(version)) => {
                self.settings.skipped_update = version;
                self.save_settings();
            }
            Some(update::Action::Restart(exe)) => match std::process::Command::new(&exe).spawn() {
                Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => self.status = Some(tr!("{} 를 시작할 수 없습니다: {e}", "Cannot start {}: {e}", exe.display())),
            },
        }
    }

    /// App shortcuts, consumed before the terminal sees the key events.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let keys = self.settings.clipboard;
        for (shortcut, what) in shortcut_table() {
            // Turned off in the settings: the key goes to the terminal.
            let off = match what {
                Shortcut::Copy => !keys.ctrl_shift_c,
                Shortcut::Paste => !keys.ctrl_shift_v,
                _ => false,
            };
            if off || !ctx.input_mut(|i| i.consume_shortcut(shortcut)) {
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

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let state = MenuState {
            sessions: &self.sessions,
            active: self.active,
            show_sidebar: self.show_sidebar,
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
        };
        if let Some(command) = menu::menu_bar(ui, &state) {
            let ctx = ui.ctx().clone();
            self.run(&ctx, command);
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            // A plain icon: showing the list is a secondary switch, not the first thing to look
            // at (the list has its own "Sites" heading).
            let (glyph, tip) = if self.show_sidebar {
                (icon::SIDEBAR_HIDE, t("사이트 목록 숨기기 (Ctrl+Shift+B)", "Hide the site list (Ctrl+Shift+B)"))
            } else {
                (icon::SIDEBAR_SHOW, t("사이트 목록 보이기 (Ctrl+Shift+B)", "Show the site list (Ctrl+Shift+B)"))
            };
            if ui.add(icon::icon_button(glyph).frame(false)).on_hover_text(tip).clicked() {
                self.show_sidebar = !self.show_sidebar;
            }
            if ui.add(icon::button(icon::ADD, t("빠른 연결", "Quick Connect"))).on_hover_text("Ctrl+Shift+T").clicked() {
                action = Some(Command::QuickConnect);
            }
            if self.broadcast {
                ui.separator();
                let others = self.broadcast_targets().len();
                let (text, tip) = broadcast_status(others);
                ui.label(RichText::new(text).strong().color(BROADCAST_COLOR)).on_hover_text(tip);
                if ui.add(icon::button(icon::TURN_OFF, t("끄기", "Turn off"))).on_hover_text("Ctrl+Shift+I").clicked() {
                    action = Some(Command::ToggleBroadcast);
                }
            }
            if self.sessions.get(self.active).is_some_and(|s| s.can_sftp())
                && ui
                    .add(icon::button(icon::SFTP, "SFTP"))
                    .on_hover_text(t("SFTP 탐색기 (Ctrl+Shift+X)", "SFTP explorer (Ctrl+Shift+X)"))
                    .clicked()
            {
                action = Some(Command::ToggleSftpPanel);
            }
            if self.sessions.get(self.active).is_some_and(|s| s.state == SessionState::Closed) {
                ui.separator();
                if ui.add(icon::button(icon::RECONNECT, t("다시 연결", "Reconnect"))).clicked() {
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
    fn dropped_files(&mut self, ctx: &egui::Context) {
        let paths: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| p.exists()).collect());
        if paths.is_empty() {
            return;
        }
        let panel = self.sftp_panel;
        match self.sessions.get_mut(self.active).and_then(|s| s.sftp.as_mut()).filter(|_| panel) {
            Some(browser) => browser.upload_paths(paths),
            None => {
                self.status = Some(
                    t(
                        "파일을 올리려면 먼저 SFTP 탐색기(Ctrl+Shift+X)를 여세요 (SFTP 가 없는 장비는 서버에서 rz 를 실행하세요)",
                        "Open the SFTP explorer (Ctrl+Shift+X) first to upload files (on devices without SFTP, run rz on the server)",
                    )
                    .into(),
                )
            }
        }
    }

    /// The SFTP explorer folded to a strip, for a tab it cannot show (see [`SFTP_STRIP`]).
    fn sftp_strip_ui(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("sftp-strip").resizable(false).exact_size(SFTP_STRIP).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                ui.add(icon::icon_button(icon::SFTP).frame(false)).on_hover_text(t(
                    "SFTP 탐색기: SSH 로 연결된 탭에서 원격 파일을 보고 올리거나 받을 수 있습니다.",
                    "SFTP explorer: shows, uploads and downloads remote files in a tab connected over SSH.",
                ));
                if ui.add(icon::icon_button(icon::CLOSE).small()).on_hover_text(t("닫기 (Ctrl+Shift+X)", "Close (Ctrl+Shift+X)")).clicked()
                {
                    self.sftp_panel = false;
                }
            });
        });
    }

    /// The SFTP explorer side panel: the active tab's remote files.
    fn sftp_panel_ui(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        ui.horizontal(|ui| {
            ui.strong(t("SFTP 탐색기", "SFTP explorer"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui
                    .add(icon::icon_button(icon::CLOSE).small())
                    .on_hover_text(t("닫기 (Ctrl+Shift+X)", "Close (Ctrl+Shift+X)"))
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
            if ui.add(icon::button(icon::RETRY, t("다시 시도", "Try again"))).clicked() {
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
    ctx.set_visuals_of(egui::Theme::Dark, readable_dark());
    ctx.set_visuals_of(egui::Theme::Light, soft_light());
    ctx.all_styles_mut(readable_text);
}

/// Size of small UI text (hints, the status bar, the saved-passwords line). egui's 9 px is
/// hard to read on 1× monitors.
const SMALL_TEXT: f32 = 12.0;

/// egui's text sizes with readable small text.
fn readable_text(style: &mut egui::Style) {
    style.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(SMALL_TEXT));
}

/// What the simultaneous-input indicator says with `others` tabs receiving, and its tooltip.
fn broadcast_status(others: usize) -> (String, &'static str) {
    if others == 0 {
        (
            t("동시 입력 켜짐 · 받을 탭 없음", "Typing into all tabs · no other tab").into(),
            t(
                "입력을 함께 받을 연결된 탭이 없습니다. 다른 탭을 연결하거나, 탭 우클릭 → 동시 입력에 포함으로 넣으세요.",
                "No other connected tab takes the input. Connect another tab, or right-click a tab → Include in simultaneous input.",
            ),
        )
    } else {
        (
            tr!("동시 입력 켜짐 · 다른 탭 {others}개", "Typing into all tabs · {others} other"),
            t(
                "이 탭에 치는 내용이 주황색 테두리의 탭에도 똑같이 입력됩니다. 탭 우클릭으로 뺄 수 있습니다.",
                "What you type here also goes to the tabs framed in orange. Right-click a tab to leave it out.",
            ),
        )
    }
}

/// Width of the SFTP explorer folded on a tab without SFTP.
const SFTP_STRIP: f32 = 34.0;

/// Weak text (hints, placeholders, status details) at the WCAG AA contrast for body text,
/// 4.5:1: egui makes it from the text color at 60 % alpha, which gives about 2.7:1 on the dark
/// panels. Still dimmer than normal text (gray 140 on dark), so it reads as secondary.
const DARK_WEAK_TEXT: u8 = 134;
const LIGHT_WEAK_TEXT: u8 = 100;

/// egui's dark look with readable weak text.
fn readable_dark() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.weak_text_color = Some(Color32::from_gray(DARK_WEAK_TEXT));
    v
}

/// egui's light look with the pure whites toned down to light grays (easier on the eyes,
/// close to Windows 11 light mode): panels, dialogs and text fields a step darker, buttons and
/// separators darker still so they keep their contrast.
fn soft_light() -> egui::Visuals {
    let gray = Color32::from_gray;
    let mut v = egui::Visuals::light();
    v.weak_text_color = Some(gray(LIGHT_WEAK_TEXT));
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

impl PlaytermApp {
    /// Asks where to save the ZMODEM downloads that just started, and hands the answers to
    /// their tabs. The question is a window of its own: the app (and the transfer) go on.
    fn zmodem_folders(&mut self, ctx: &egui::Context) {
        let start = self.zmodem_last.clone().filter(|dir| dir.is_dir()).unwrap_or_else(|| self.zmodem_dir.clone());
        for session in &mut self.sessions {
            if !session.take_zmodem_question() {
                continue;
            }
            let title =
                tr!("ZMODEM 으로 받은 파일을 저장할 폴더 - {}", "Folder for the files received with ZMODEM - {}", session.tab_title());
            let (start, ctx) = (start.clone(), ctx.clone());
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(rfd::FileDialog::new().set_title(title).set_directory(&start).pick_folder());
                ctx.request_repaint();
            });
            self.zmodem_questions.push((session.id, rx));
        }
        let mut answers = Vec::new();
        self.zmodem_questions.retain(|(id, rx)| match rx.try_recv() {
            Ok(dir) => {
                answers.push((*id, dir));
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                answers.push((*id, None));
                false
            }
        });
        for (id, dir) in answers {
            if let Some(dir) = &dir {
                self.zmodem_last = Some(dir.clone());
            }
            if let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) {
                session.zmodem_folder_chosen(dir);
            }
        }
        self.zmodem_uploads(ctx);
    }

    /// Asks which files to send to the ZMODEM uploads (`rz`) that just started, and hands the
    /// answers to their tabs.
    fn zmodem_uploads(&mut self, ctx: &egui::Context) {
        let start = self.upload_last.clone().filter(|dir| dir.is_dir()).unwrap_or_else(local_fs::home);
        for session in &mut self.sessions {
            if !session.take_upload_question() {
                continue;
            }
            let title = tr!("ZMODEM 으로 보낼 파일 - {}", "Files to send with ZMODEM - {}", session.tab_title());
            let (start, ctx) = (start.clone(), ctx.clone());
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(rfd::FileDialog::new().set_title(title).set_directory(&start).pick_files());
                ctx.request_repaint();
            });
            self.upload_questions.push((session.id, rx));
        }
        let mut answers = Vec::new();
        self.upload_questions.retain(|(id, rx)| match rx.try_recv() {
            Ok(files) => {
                answers.push((*id, files));
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                answers.push((*id, None));
                false
            }
        });
        for (id, files) in answers {
            if let Some(dir) = files.as_ref().and_then(|f| f.first()).and_then(|f| f.parent()) {
                self.upload_last = Some(dir.to_owned());
            }
            if let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) {
                session.zmodem_files_chosen(files);
            }
        }
    }
}

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
        if self.sessions.is_empty() {
            self.open_local();
        }
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
        egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        // Added before the side panel so it spans the whole window width.
        if self.settings.status_bar {
            let session = self.sessions.get(self.active);
            let awake = status_bar::Awake { sleep: self.sleep_guard.active(), screensaver: self.screensaver_guard.active() };
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
        let output = egui::CentralPanel::no_frame()
            .show(ui, |ui| self.dock.show(ui, &mut self.sessions, self.active, autofocus, self.broadcast, &style))
            .inner;
        if let Some(px) = output.active_px {
            self.view_px = px;
        }
        // RDP tabs not on screen (another tab, a shortcut switched away) hold no keys or buttons.
        for s in &mut self.sessions {
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
        if let Some(command) = output.command {
            self.run(&ctx, command);
        }
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
}

#[cfg(test)]
mod contrast_tests {
    use super::*;

    #[test]
    fn a_password_goes_only_where_it_was_saved_for() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = playterm_config::Vault::new(dir.path().join("vault.bin"));
        vault.create("master").unwrap();
        let site = SessionProfile { id: "a".into(), host: "db01".into(), port: 22, username: "ops".into(), ..Default::default() };
        assert!(matches!(bound_secret(&vault, &site), Ok(None)), "no password saved");
        vault.set("a", &vault_target(&site), "s3cret").unwrap();
        assert_eq!(bound_secret(&vault, &site).unwrap().as_deref().map(String::as_str), Some("s3cret"));
        // The file was changed to point elsewhere: the password is not handed out.
        let moved = SessionProfile { host: "evil.example".into(), ..site.clone() };
        assert_eq!(bound_secret(&vault, &moved).unwrap_err(), vault_target(&site));
        let other_user = SessionProfile { username: "root".into(), ..site.clone() };
        assert!(bound_secret(&vault, &other_user).is_err());
    }

    /// WCAG relative luminance of a gray.
    fn luminance(gray: u8) -> f64 {
        let c = gray as f64 / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (la, lb) = (luminance(a.r()), luminance(b.r()));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn the_ui_follows_the_system_theme_by_default() {
        let ctx = egui::Context::default();
        apply_ui(&ctx, &AppSettings::default());
        for system in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            let input = egui::RawInput { system_theme: Some(system), ..Default::default() };
            let mut out = ctx.run_ui(input, |_| {});
            out.textures_delta.clear();
            assert_eq!(ctx.theme(), system, "follows the system as it changes");
        }
        // A theme chosen in the settings wins over the system's.
        apply_ui(&ctx, &AppSettings { ui_theme: UiTheme::Dark, ..AppSettings::default() });
        let input = egui::RawInput { system_theme: Some(egui::Theme::Light), ..Default::default() };
        let mut out = ctx.run_ui(input, |_| {});
        out.textures_delta.clear();
        assert_eq!(ctx.theme(), egui::Theme::Dark);
    }

    #[test]
    fn broadcast_status_says_when_nothing_receives() {
        playterm_i18n::set_english(false);
        assert!(broadcast_status(0).0.contains("받을 탭 없음"));
        assert!(broadcast_status(3).0.contains("다른 탭 3개"));
    }

    #[test]
    fn small_text_is_readable() {
        let mut style = egui::Style::default();
        readable_text(&mut style);
        let small = style.text_styles[&egui::TextStyle::Small].size;
        assert!(small >= 12.0, "{small}");
        // Still no bigger than body text, so it stays secondary.
        assert!(small <= style.text_styles[&egui::TextStyle::Body].size);
    }

    #[test]
    fn weak_text_is_readable_but_weaker() {
        for v in [readable_dark(), soft_light()] {
            let weak = v.weak_text_color();
            for bg in [v.panel_fill, v.window_fill, v.extreme_bg_color] {
                assert!(contrast(weak, bg) >= 4.5, "{weak:?} on {bg:?}: {:.2}", contrast(weak, bg));
            }
            // Normal text stands out more than weak text.
            assert!(contrast(v.text_color(), v.panel_fill) > contrast(weak, v.panel_fill));
        }
    }
}
