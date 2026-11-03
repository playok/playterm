//! One open tab: a terminal screen, bound to a network session while connected and running
//! the local command prompt ([`crate::console`]) while not.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use playterm_config::{SessionProfile, TabColor};
use playterm_i18n::{t, tr};
use playterm_net::ping::{self, Ping, PingEvent, PingOptions, PingStats};
use playterm_net::zmodem::{self, Outcome, Start};
use playterm_net::{AuthPrompt, ConnectParams, HostKeyCheck, NetRuntime, Protocol, SessionEvent, SessionHandle, Waker};
use playterm_term::{Charset, LogMode, Palette, Point, Terminal, TerminalEvent};
use zeroize::Zeroizing;

use crate::autologin::{LoginAction, LoginRunner, LoginScript};
use crate::console::{self, KeyResult, LineEditor, Parsed, Request};
use crate::local_fs;
use crate::logging::{self, LogPlan};
use crate::rdp_view::RdpScreen;
use crate::search_bar::SearchBar;
use crate::sftp_view::{SftpBrowser, human_size};

/// How often the progress line of a ZMODEM download is redrawn.
const ZMODEM_REDRAW: Duration = Duration::from_millis(200);

/// How a new tab's terminal starts out (from the app settings).
#[derive(Clone)]
pub struct TermSetup {
    pub palette: Palette,
    pub scrollback: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Local command prompt; never connected.
    Local,
    Connecting,
    Connected,
    Closed,
}

/// An authentication prompt waiting for the user, with the answers typed so far.
pub struct PendingAuth {
    pub prompt: AuthPrompt,
    /// Zeroed when dropped.
    pub answers: Vec<Zeroizing<String>>,
    pub focus_requested: bool,
    /// User name as edited in the dialog (see [`AuthPrompt::user`]).
    pub user: String,
    /// Also store an edited user name in the saved site.
    pub save_user: bool,
    /// Save the typed password in the vault once the login succeeds.
    pub save_password: bool,
}

impl PendingAuth {
    /// A login prompt asking for exactly the account password (not a passphrase, not a
    /// one-time code after the password).
    pub fn is_password(&self) -> bool {
        self.prompt.account_password
            && self.prompt.user.is_some()
            && matches!(self.prompt.fields.as_slice(), [only] if !only.echo)
    }
}

/// `text` with control characters (escape sequences, carriage returns, ...) made harmless for
/// the terminal: line breaks and tabs stay, anything else becomes `?`.
/// How long a connection has been up: "just now", "12 min", "2 h 5 min".
pub fn elapsed(d: Duration) -> String {
    let minutes = d.as_secs() / 60;
    match minutes {
        0 => t("방금", "just now").into(),
        1..=59 => tr!("{minutes}분", "{minutes} min"),
        _ => {
            let (h, m) = (minutes / 60, minutes % 60);
            if m == 0 { tr!("{h}시간", "{h} h") } else { tr!("{h}시간 {m}분", "{h} h {m} min") }
        }
    }
}

pub fn printable(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|c| if c.is_control() && c != '\n' && c != '\t' { '?' } else { c })
        .collect()
}

/// What a tab was opened with; enough to duplicate it.
#[derive(Clone)]
pub struct SessionSpec {
    pub params: ConnectParams,
    pub charset: Charset,
    /// Saved-profile name, used as the tab title.
    pub name: Option<String>,
    /// Start logging as soon as the tab opens.
    pub log: Option<LogPlan>,
    /// Answer login prompts automatically.
    pub login: Option<LoginScript>,
    /// Id of the saved site this was opened from.
    pub site: Option<String>,
    /// The site's tab color.
    pub tab_color: Option<TabColor>,
    /// The profile the connection was opened from, to open it again in a new window.
    pub profile: Option<Box<SessionProfile>>,
}

/// Mouse reporting state (application mouse modes).
#[derive(Default)]
pub struct MouseReport {
    /// Button pressed while reporting, so its release and drags are reported too.
    pub pressed: Option<crate::mouse::MouseButton>,
    /// Last reported cell; motion is only reported when it changes.
    pub last_cell: Option<(usize, usize)>,
    /// Focus state last frame, for DECSET 1004 focus reports.
    pub last_focus: bool,
    /// Fractional wheel notches not yet reported (touchpads scroll in small steps).
    pub wheel_accum: f32,
}

/// Mouse selection gesture state.
#[derive(Default)]
pub struct ClickState {
    /// Time and cell of the last primary press, for double/triple-click detection.
    pub last: Option<(f64, Point)>,
    pub count: u8,
    pub dragging: bool,
}

/// The connection a tab was last opened with.
struct Remote {
    params: ConnectParams,
    handle: SessionHandle,
}

/// A `ping` running at the prompt.
struct PingRun {
    ping: Ping,
    host: String,
    size: u16,
    /// The address the name resolved to.
    addr: Option<std::net::IpAddr>,
    stats: PingStats,
}

/// A ZMODEM download whose folder the user is asked for. It is received into `staging`
/// meanwhile; the files move to the chosen folder once both the transfer and the choice are done.
struct ZmodemPick {
    staging: PathBuf,
    chosen: Option<PathBuf>,
    /// The transfer is over: the files received completely and how it ended.
    ended: Option<(Vec<PathBuf>, Outcome)>,
    /// Some file could not be moved: the staging folder stays.
    keep: bool,
}

impl ZmodemPick {
    /// A new staging folder (under the system's temporary folder) for a download of tab `id`.
    fn new(id: u64) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!("{}-{id}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let staging = std::env::temp_dir().join("playterm-zmodem").join(name);
        Self { staging, chosen: None, ended: None, keep: false }
    }
}

impl Drop for ZmodemPick {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.staging);
        }
    }
}

/// A ZMODEM download and what the screen shows of it.
struct ZmodemRun {
    download: zmodem::Download,
    /// Saved files already listed on the screen.
    listed: usize,
    /// When the progress line was last drawn.
    drawn: Option<Instant>,
    /// Its end is not reported (the user was told already).
    silent: bool,
}

pub struct Session {
    pub id: u64,
    charset: Charset,
    pub name: Option<String>,
    pub terminal: Terminal,
    pub state: SessionState,
    /// Title set by the remote side via OSC 0/2.
    pub remote_title: Option<String>,
    pub host_key: Option<HostKeyCheck>,
    pub auth: Option<PendingAuth>,
    /// IME composition in progress (not yet sent).
    pub preedit: String,
    /// Fractional wheel scroll not yet applied, in pixels.
    pub scroll_accum: f32,
    /// Clipboard text the remote side asked us to store (OSC 52).
    pub clipboard_store: Option<String>,
    /// Several lines waiting for the user to confirm the paste (see `view::request_paste`).
    pub pending_paste: Option<String>,
    pub click: ClickState,
    pub mouse: MouseReport,
    pub sftp: Option<SftpBrowser>,
    /// The find bar, while open.
    pub search: Option<SearchBar>,
    /// Receives what is typed in another tab while simultaneous input is on.
    pub in_broadcast: bool,
    /// Color of the tab in the tab bar.
    pub tab_color: Option<TabColor>,
    /// The remote screen of an RDP connection.
    pub rdp: RdpScreen,
    /// An SFTP channel was requested and has not answered yet.
    sftp_pending: bool,
    /// Where a new SFTP browser opens: the shell's directory, if it arrived first.
    sftp_start: Option<String>,
    /// A question for the shell's directory is out; `true` if the user asked (reports failure).
    shell_dir_request: Option<bool>,
    /// Why SFTP could not be opened on this connection; the side panel does not retry by itself.
    sftp_error: Option<String>,
    /// Auto-log settings of the profile, kept so duplicates log to their own file.
    log_plan: Option<LogPlan>,
    /// The site's automatic login, kept for reconnects and duplicates.
    login_script: Option<LoginScript>,
    /// Saved site the connection was opened from.
    site: Option<String>,
    /// When the current connection came up.
    connected_since: Option<Instant>,
    /// A fresh local tab nothing was typed into yet: the start screen shows over it.
    pub start_screen: bool,
    /// Profile the connection was opened from.
    profile: Option<Box<SessionProfile>>,
    /// Password typed in the login dialog with "save password" on; stored in the vault for the
    /// site once the connection is up (so a wrong one is never saved).
    save_on_login: Option<Zeroizing<String>>,
    /// Automatic login in progress on the current connection.
    login: Option<LoginRunner>,
    remote: Option<Remote>,
    console: LineEditor,
    /// Current folder of the prompt (`cd`, `ls`).
    cwd: std::path::PathBuf,
    /// Commands typed at the prompt that the app has to carry out.
    pub requests: Vec<Request>,
    /// Bytes received from and sent to the remote side on the current connection.
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Spots `sz` (the start of a ZMODEM download) in the output.
    zmodem_detector: zmodem::Detector,
    /// A ZMODEM download in progress: the output goes to it instead of the screen.
    zmodem: Option<ZmodemRun>,
    /// Where ZMODEM downloads are saved, and whether the folder is asked for each time (the
    /// settings, see [`Session::poll`]).
    zmodem_dir: PathBuf,
    zmodem_ask: bool,
    /// A download whose folder is being asked for (it may be running or over).
    zmodem_pick: Option<ZmodemPick>,
    /// The app has to ask for the folder of `zmodem_pick` (see [`Session::take_zmodem_question`]).
    zmodem_question: bool,
    /// `ping` running at the prompt (until it ends or Ctrl+C).
    ping: Option<PingRun>,
}

impl Session {
    /// A tab with the local command prompt.
    pub fn local(id: u64, cols: u16, rows: u16, setup: &TermSetup) -> Self {
        let mut session = Self::blank(id, cols, rows, Charset::Utf8, setup);
        // The start screen over the terminal explains the tab (in the UI font: Hangul in the
        // terminal grid would be spread over two cells a letter).
        session.start_screen = true;
        session.prompt();
        session
    }

    /// A new tab connecting right away.
    pub fn connect(id: u64, spec: SessionSpec, net: &NetRuntime, waker: Waker, setup: &TermSetup) -> Self {
        let mut session = Self::blank(id, spec.params.cols, spec.params.rows, spec.charset, setup);
        session.start(spec, net, waker);
        session
    }

    fn blank(id: u64, cols: u16, rows: u16, charset: Charset, setup: &TermSetup) -> Self {
        let mut terminal = Terminal::new(cols.into(), rows.into(), charset, setup.scrollback);
        terminal.set_palette(setup.palette.clone());
        Self {
            id,
            charset,
            name: None,
            terminal,
            state: SessionState::Local,
            remote_title: None,
            host_key: None,
            auth: None,
            preedit: String::new(),
            scroll_accum: 0.0,
            clipboard_store: None,
            pending_paste: None,
            click: ClickState::default(),
            mouse: MouseReport::default(),
            sftp: None,
            search: None,
            in_broadcast: true,
            tab_color: None,
            rdp: RdpScreen::default(),
            sftp_pending: false,
            sftp_start: None,
            shell_dir_request: None,
            sftp_error: None,
            log_plan: None,
            login_script: None,
            site: None,
            connected_since: None,
            start_screen: false,
            profile: None,
            save_on_login: None,
            login: None,
            remote: None,
            console: LineEditor::default(),
            cwd: local_fs::home(),
            requests: Vec::new(),
            rx_bytes: 0,
            tx_bytes: 0,
            zmodem_detector: zmodem::Detector::default(),
            zmodem: None,
            zmodem_dir: PathBuf::new(),
            zmodem_ask: false,
            zmodem_pick: None,
            zmodem_question: false,
            ping: None,
        }
    }

    /// Connects this tab (from the prompt, or as a new tab), keeping the screen contents.
    pub fn start(&mut self, spec: SessionSpec, net: &NetRuntime, waker: Waker) {
        self.start_screen = false;
        let SessionSpec { mut params, charset, name, log, login, site, tab_color, profile } = spec;
        if let Some(remote) = &self.remote {
            remote.handle.close();
        }
        self.ping = None;
        self.leave_prompt();
        self.end_zmodem();
        // A log belongs to one connection: the new one follows its own profile's setting.
        self.stop_log();
        params.cols = self.terminal.cols() as u16;
        params.rows = self.terminal.rows() as u16;
        self.charset = charset;
        self.terminal.set_charset(charset);
        self.name = name;
        self.remote_title = None;
        self.host_key = None;
        self.auth = None;
        self.sftp = None;
        self.sftp_start = None;
        self.shell_dir_request = None;
        self.sftp_pending = false;
        self.sftp_error = None;
        self.requests.clear();
        self.log_plan = log.clone();
        self.login = login.clone().map(LoginRunner::new);
        self.login_script = login;
        self.site = site;
        self.tab_color = tab_color;
        self.profile = profile;
        self.save_on_login = None;
        self.rx_bytes = 0;
        self.tx_bytes = 0;
        // An RDP desktop that follows the tab starts at the tab's size (when it is known).
        if self.rdp.view_px.0 > 0 {
            params.desktop = self.rdp.view_px;
        }
        self.rdp.reset();
        let handle = net.connect(params.clone(), waker);
        self.remote = Some(Remote { params, handle });
        self.state = SessionState::Connecting;
        let target = self.target();
        self.terminal.feed_local(&tr!("\x1b[90m{target} 에 연결 중...\x1b[0m\r\n", "\x1b[90mConnecting to {target}...\x1b[0m\r\n"));
        if let Some(plan) = log {
            let path = logging::new_log_path(&plan.dir, &self.tab_title());
            self.start_log(&path, plan.mode);
        }
    }

    /// The site's label ("운영"), if it has one.
    pub fn site_label(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.tab_label.trim()).filter(|l| !l.is_empty())
    }

    /// Where input goes, to copy: `user@host:22`, `COM3`. `None` for a local tab.
    pub fn address(&self) -> Option<String> {
        let Remote { params: p, .. } = self.remote.as_ref()?;
        Some(match p.protocol {
            Protocol::Serial => p.serial.port.clone(),
            _ if p.username.is_empty() => format!("{}:{}", printable(&p.host), p.port),
            _ => format!("{}@{}:{}", printable(&p.username), printable(&p.host), p.port),
        })
    }

    /// The line above the terminal: `SSH · user@host:22 · Connected 12 min · UTF-8`.
    pub fn connection_line(&self) -> Option<String> {
        let Remote { params: p, .. } = self.remote.as_ref()?;
        let state = match self.state {
            SessionState::Connected => match self.connected_since {
                Some(since) => format!("{} {}", t("연결됨", "Connected"), elapsed(since.elapsed())),
                None => t("연결됨", "Connected").into(),
            },
            SessionState::Connecting => t("연결 중…", "Connecting…").into(),
            SessionState::Closed => t("연결 끊김", "Disconnected").into(),
            SessionState::Local => return None,
        };
        let mut parts = Vec::new();
        if let Some(label) = self.site_label() {
            parts.push(format!("[{label}]"));
        }
        parts.push(p.protocol.to_string());
        parts.push(self.address()?);
        if p.protocol == Protocol::Serial {
            parts.push(p.serial.line_summary());
        }
        parts.push(state);
        if p.protocol != Protocol::Rdp {
            parts.push(self.charset_label());
        }
        Some(parts.join(" · "))
    }

    /// `SSH host:22`, `Serial COM3 (9600 8N1)`.
    pub fn target(&self) -> String {
        let Some(Remote { params: p, .. }) = &self.remote else { return t("로컬", "Local").into() };
        match p.protocol {
            Protocol::Serial => format!("Serial {} ({})", p.serial.port, p.serial.line_summary()),
            proto => format!("{proto} {}:{}", printable(&p.host), p.port),
        }
    }

    pub fn start_log(&mut self, path: &std::path::Path, mode: LogMode) {
        let title = self.tab_title();
        match self.terminal.start_log(path, mode, &title) {
            Ok(()) => self.notice(&tr!("로그 기록 시작: {}", "Logging started: {}", path.display()), "90"),
            Err(e) => self.notice(
                &tr!("로그 파일을 만들 수 없습니다 ({}): {e}", "Cannot create the log file ({}): {e}", path.display()),
                "31",
            ),
        }
    }

    pub fn stop_log(&mut self) {
        if let Some(path) = self.terminal.stop_log() {
            self.notice(&tr!("로그 기록 중지: {}", "Logging stopped: {}", path.display()), "90");
        }
    }

    pub fn is_logging(&self) -> bool {
        self.terminal.log().is_some()
    }

    fn protocol(&self) -> Option<Protocol> {
        self.remote.as_ref().map(|r| r.params.protocol)
    }

    pub fn is_rdp(&self) -> bool {
        self.protocol() == Some(Protocol::Rdp)
    }

    /// The remote desktop is up and has a picture (before that, the terminal shows progress).
    pub fn shows_rdp(&self) -> bool {
        self.is_rdp() && self.state == SessionState::Connected && self.rdp.has_picture()
    }

    /// The RDP desktop follows the size of the tab.
    pub fn rdp_fits_tab(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.params.rdp.fits_tab())
    }

    /// The part of the remote desktop that changed since the last call.
    pub fn take_rdp_frame(&self) -> Option<playterm_net::RdpFrame> {
        self.remote.as_ref()?.handle.take_frame()
    }

    /// Releases keys and buttons held on the remote desktop (the tab is not shown).
    pub fn release_rdp_input(&mut self) {
        let inputs = self.rdp.release_all();
        self.send_rdp(inputs);
    }

    /// Keyboard and mouse for the remote desktop.
    pub fn send_rdp(&mut self, inputs: Vec<playterm_net::RdpInput>) {
        if !inputs.is_empty()
            && self.state == SessionState::Connected
            && let Some(remote) = &self.remote
        {
            remote.handle.send_rdp(inputs);
        }
    }

    pub fn can_break(&self) -> bool {
        self.state == SessionState::Connected && matches!(self.protocol(), Some(Protocol::Serial | Protocol::Telnet))
    }

    pub fn send_break(&mut self) {
        if self.can_break()
            && let Some(remote) = &self.remote
        {
            remote.handle.send_break();
        }
    }

    /// Whether the tab was ever connected (and so can reconnect or be duplicated).
    pub fn has_target(&self) -> bool {
        self.remote.is_some()
    }

    /// Starts a fresh connection with the same parameters, keeping the screen contents.
    pub fn reconnect(&mut self, net: &NetRuntime, waker: Waker) {
        let Some(remote) = &mut self.remote else { return };
        remote.handle.close();
        remote.params.cols = self.terminal.cols() as u16;
        remote.params.rows = self.terminal.rows() as u16;
        if self.rdp.view_px.0 > 0 {
            remote.params.desktop = self.rdp.view_px;
        }
        self.rdp.reset();
        remote.handle = net.connect(remote.params.clone(), waker);
        self.ping = None;
        self.leave_prompt();
        self.end_zmodem();
        let target = self.target();
        self.terminal.feed_local(&tr!(
            "\x1b[90m{target} 에 다시 연결 중...\x1b[0m\r\n",
            "\x1b[90mReconnecting to {target}...\x1b[0m\r\n"
        ));
        self.state = SessionState::Connecting;
        self.host_key = None;
        self.auth = None;
        self.requests.clear();
        self.login = self.login_script.clone().map(LoginRunner::new);
        self.rx_bytes = 0;
        self.tx_bytes = 0;
    }

    /// Forgets the password this tab connects with (the vault was locked): a reconnect asks
    /// again, or for the vault to be unlocked.
    pub fn forget_password(&mut self) {
        use zeroize::Zeroize as _;
        if let Some(remote) = &mut self.remote
            && let Some(mut password) = remote.params.password.take()
        {
            password.zeroize();
        }
        if let Some(script) = &mut self.login_script {
            script.password = None;
        }
        self.login = None;
        self.save_on_login = None;
    }

    /// Whether this tab's site saves its password but the tab has none (the vault was locked).
    pub fn lacks_saved_password(&self) -> bool {
        self.profile.as_ref().is_some_and(|p| p.save_password)
            && self.remote.as_ref().is_some_and(|r| r.params.password.is_none())
    }

    /// Gives the tab its saved password again (the vault was unlocked to reconnect).
    pub fn restore_password(&mut self, password: String) {
        if let Some(script) = &mut self.login_script {
            script.password = Some(Zeroizing::new(password.clone()));
        }
        if let Some(remote) = &mut self.remote {
            remote.params.password = Some(password);
        }
    }

    /// What this tab is connected with; `None` for a tab that never connected.
    pub fn spec(&self) -> Option<SessionSpec> {
        Some(SessionSpec {
            params: self.remote.as_ref()?.params.clone(),
            charset: self.charset,
            name: self.name.clone(),
            log: self.log_plan.clone(),
            login: self.login_script.clone(),
            site: self.site.clone(),
            tab_color: self.tab_color,
            profile: self.profile.clone(),
        })
    }

    /// The profile this tab's connection was opened from; `None` for a local prompt.
    pub fn profile(&self) -> Option<&SessionProfile> {
        self.profile.as_deref()
    }

    /// Saved site this tab's connection was opened from.
    pub fn site(&self) -> Option<&str> {
        self.site.as_deref()
    }

    /// Remembers (or forgets, with `None`) a password to save once the login succeeds.
    pub fn save_password_on_login(&mut self, password: Option<Zeroizing<String>>) {
        self.save_on_login = password;
    }

    /// The site and password to save, once the connection is up.
    pub fn take_password_to_save(&mut self) -> Option<(String, Zeroizing<String>)> {
        if self.state != SessionState::Connected {
            return None;
        }
        let password = self.save_on_login.take()?;
        Some((self.site.clone()?, password))
    }

    /// Connects again as another user (the user name was mistyped), with `password` if one was
    /// typed for them.
    pub fn relogin(&mut self, user: String, password: Option<String>, net: &NetRuntime, waker: Waker) {
        let Some(remote) = &mut self.remote else { return };
        remote.params.username = user.clone();
        remote.params.password = password;
        if let Some(script) = &mut self.login_script {
            script.user = user;
        }
        self.reconnect(net, waker);
    }

    pub fn tab_title(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let Some(Remote { params, .. }) = &self.remote else { return t("로컬", "Local").into() };
        match &self.remote_title {
            Some(t) if !t.is_empty() => t.clone(),
            _ if params.username.is_empty() => params.host.clone(),
            _ => format!("{}@{}", params.username, params.host),
        }
    }

    /// Processes pending network and terminal events; a ZMODEM download saves into `downloads`,
    /// or with `ask` into the folder the user picks (see [`Session::take_zmodem_question`]).
    /// Returns `true` if anything changed.
    pub fn poll(&mut self, downloads: &Path, ask: bool) -> bool {
        if self.zmodem_dir != downloads {
            self.zmodem_dir = downloads.to_owned();
        }
        self.zmodem_ask = ask;
        let mut changed = false;
        changed |= self.ping_output();
        // A quiet ZMODEM sender is reminded, and given up after a while.
        if let Some(run) = &mut self.zmodem {
            let reply = run.download.tick(Instant::now());
            changed |= !reply.is_empty() || run.download.outcome().is_some();
            self.send_raw(reply);
            self.zmodem_show();
        }
        // The SFTP browser's "terminal's folder" button.
        if self.sftp.as_mut().is_some_and(|b| std::mem::take(&mut b.follow_requested)) {
            self.follow_shell_dir(true);
        }
        // The first picture of an RDP connection (the view takes later changes itself).
        if !self.rdp.has_picture()
            && let Some(frame) = self.remote.as_ref().and_then(|r| r.handle.take_frame())
        {
            self.rdp.pending = Some(frame);
            changed = true;
        }
        while let Some(event) = self.remote.as_ref().and_then(|r| r.handle.try_recv()) {
            changed = true;
            match event {
                SessionEvent::Connected => {
                    self.state = SessionState::Connected;
                    self.connected_since = Some(Instant::now());
                }
                SessionEvent::Data(bytes) => {
                    self.rx_bytes += bytes.len() as u64;
                    self.output(&bytes);
                }
                SessionEvent::Notice(text) => self.notice(&text, "33"),
                SessionEvent::HostKeyCheck(check) => self.host_key = Some(check),
                SessionEvent::AuthPrompt(prompt) => {
                    let answers = (0..prompt.fields.len()).map(|_| Zeroizing::new(String::new())).collect();
                    let user = prompt.user.clone().unwrap_or_default();
                    // Ticked already when it was ticked at the last (wrong) try, or when the site
                    // saves its password (then the saved one was wrong).
                    let save_password =
                        self.save_on_login.is_some() || self.profile.as_ref().is_some_and(|p| p.save_password);
                    self.auth =
                        Some(PendingAuth { prompt, answers, focus_requested: false, user, save_user: false, save_password });
                }
                SessionEvent::ShellDir(dir) => {
                    let asked = self.shell_dir_request.take().unwrap_or(false);
                    match (dir, &mut self.sftp) {
                        (Some(dir), Some(browser)) => browser.go_to(&dir),
                        (Some(dir), None) => self.sftp_start = Some(dir),
                        (None, Some(browser)) if asked => browser.set_status(
                            t(
                                "터미널의 현재 디렉터리를 알 수 없습니다 (Linux 서버의 SSH 셸에서 됩니다)",
                                "The terminal's current directory is not known (works with SSH shells on Linux servers)",
                            ),
                            true,
                        ),
                        _ => {}
                    }
                }
                SessionEvent::Sftp(result) => {
                    self.sftp_pending = false;
                    match result {
                        Ok(conn) => {
                            self.sftp = Some(SftpBrowser::new(conn, self.sftp_start.take()));
                        }
                        Err(e) => {
                            self.notice(&tr!("SFTP 열기 실패: {e}", "Could not open SFTP: {e}"), "31");
                            self.sftp_error = Some(e);
                        }
                    }
                }
                SessionEvent::Closed(reason) => {
                    if self.end_zmodem() {
                        self.notice(t("연결이 끊겨 ZMODEM 받기를 중단했습니다", "The connection closed; the ZMODEM download stopped"), "31");
                    }
                    self.state = SessionState::Closed;
                    self.connected_since = None;
                    self.rdp.reset();
                    self.save_on_login = None;
                    self.host_key = None;
                    self.auth = None;
                    self.sftp = None;
                    self.sftp_start = None;
                    self.shell_dir_request = None;
                    self.sftp_pending = false;
                    self.sftp_error = None;
                    self.terminal.reset_modes();
                    match reason {
                        Some(reason) => self.notice(&tr!("연결 종료: {reason}", "Disconnected: {reason}"), "31"),
                        None => self.notice(t("연결이 종료되었습니다", "Disconnected"), "90"),
                    }
                    self.terminal.feed_local(t(
                        "\x1b[90mr: 다시 연결 · help: 명령 목록\x1b[0m\r\n",
                        "\x1b[90mr: reconnect · help: list of commands\x1b[0m\r\n",
                    ));
                    self.prompt();
                }
            }
        }

        for event in self.terminal.take_events() {
            changed = true;
            match event {
                TerminalEvent::Reply(bytes) => {
                    if let Some(remote) = &self.remote {
                        self.tx_bytes += bytes.len() as u64;
                        remote.handle.send(bytes);
                    }
                }
                TerminalEvent::Title(title) => self.remote_title = title,
                TerminalEvent::ClipboardStore(text) => self.clipboard_store = Some(text),
                TerminalEvent::LogFailed(e) => {
                    self.notice(&tr!("로그 기록 실패, 중지합니다: {e}", "Logging failed and was stopped: {e}"), "31")
                }
                TerminalEvent::Bell => {}
            }
        }
        changed
    }

    /// Output of the remote side: onto the screen, or into a ZMODEM download.
    fn output(&mut self, bytes: &[u8]) {
        if let Some(run) = &mut self.zmodem {
            let reply = run.download.feed(bytes, Instant::now());
            self.send_raw(reply);
            self.zmodem_show();
            return;
        }
        match self.zmodem_detector.scan(bytes) {
            None => {
                self.terminal.feed(bytes);
                self.auto_login(bytes.contains(&b'\n'));
            }
            Some(Start::Download { at, rest }) => {
                self.terminal.feed(&bytes[..at]);
                self.login = None;
                // The sender gives up when it is not answered for about 20 seconds, so the
                // download starts right away, into a staging folder while the folder is asked
                // for. (Not when the files of an earlier one still wait for their folder.)
                if self.zmodem_ask && self.zmodem_pick.is_none() {
                    let pick = ZmodemPick::new(self.id);
                    let staging = pick.staging.clone();
                    self.zmodem_pick = Some(pick);
                    self.zmodem_question = true;
                    let line = t(
                        "ZMODEM 받기: 저장할 폴더를 고르는 동안 임시 폴더에 받습니다 · Ctrl+C 로 취소",
                        "ZMODEM download: received into a temporary folder while you choose where to save · Ctrl+C cancels",
                    );
                    self.begin_zmodem(staging, &bytes[rest..], line);
                } else {
                    let dir = self.zmodem_dir.clone();
                    let line =
                        tr!("ZMODEM 받기: {} 에 저장합니다 · Ctrl+C 로 취소", "ZMODEM download into {} · Ctrl+C cancels", dir.display());
                    self.begin_zmodem(dir, &bytes[rest..], &line);
                }
            }
            Some(Start::Upload { at, .. }) => {
                // `rz` waits for files from us, which is not supported: stop it instead of
                // leaving it waiting.
                self.terminal.feed(&bytes[..at]);
                self.send_raw(zmodem::CANCEL.to_vec());
                self.notice(
                    t(
                        "서버의 rz (ZMODEM 보내기) 는 지원하지 않아 취소했습니다. 파일을 보내려면 SFTP 를 쓰세요.",
                        "rz (ZMODEM upload) on the server is not supported and was cancelled. Use SFTP to send files.",
                    ),
                    "33",
                );
            }
        }
    }

    /// Starts receiving into `dir`, with the output after the sender's first header; `line`
    /// tells the user.
    fn begin_zmodem(&mut self, dir: PathBuf, rest: &[u8], line: &str) {
        self.terminal.feed_local(&format!("\r\n\x1b[36m[{}]\x1b[0m\r\n", printable(line)));
        let (download, reply) = zmodem::Download::begin(dir, rest, Instant::now());
        self.zmodem = Some(ZmodemRun { download, listed: 0, drawn: None, silent: false });
        self.send_raw(reply);
        self.zmodem_show();
    }

    /// Whether the app has to ask for the folder of a ZMODEM download that just started (once),
    /// and answer with [`Session::zmodem_folder_chosen`].
    pub fn take_zmodem_question(&mut self) -> bool {
        std::mem::take(&mut self.zmodem_question)
    }

    /// The folder the user picked for the ZMODEM download asked about, or `None` (cancelled):
    /// the download is cancelled then and what it received deleted.
    pub fn zmodem_folder_chosen(&mut self, dir: Option<PathBuf>) {
        let Some(pick) = &mut self.zmodem_pick else { return };
        let running = pick.ended.is_none();
        match dir {
            Some(dir) if running => {
                let line = tr!("ZMODEM: 받기가 끝나면 {} 에 저장합니다", "ZMODEM: the files go to {} when the download is done", dir.display());
                pick.chosen = Some(dir);
                // Below the progress line, which goes on.
                self.terminal.feed_local(&format!("\r\x1b[K\x1b[36m[{}]\x1b[0m\r\n", printable(&line)));
                if let Some(run) = &mut self.zmodem {
                    run.drawn = None;
                }
                self.zmodem_show();
            }
            Some(dir) => self.zmodem_move(&dir),
            None => {
                let pick = self.zmodem_pick.take();
                if running && let Some(run) = &mut self.zmodem {
                    // It goes on until what the sender had sent is dropped; its end says nothing.
                    let reply = run.download.cancel(Instant::now());
                    run.silent = true;
                    self.send_raw(reply);
                    self.terminal.feed_local("\r\x1b[K");
                }
                drop(pick);
                self.notice(
                    if running {
                        t(
                            "저장할 폴더를 고르지 않아 ZMODEM 받기를 취소하고 받은 파일을 지웠습니다",
                            "No folder was chosen: the ZMODEM download was cancelled and what it received deleted",
                        )
                    } else {
                        t("저장할 폴더를 고르지 않아 ZMODEM 으로 받은 파일을 지웠습니다", "No folder was chosen: the files received with ZMODEM were deleted")
                    },
                    "33",
                );
            }
        }
    }

    /// The transfer of the download asked about is over: its files move now if the folder is
    /// known, or wait for it.
    fn zmodem_pick_ended(&mut self, files: Vec<PathBuf>, outcome: Outcome) {
        let Some(pick) = &mut self.zmodem_pick else { return };
        let chosen = pick.chosen.clone();
        let count = files.len();
        if count == 0 {
            self.zmodem_pick = None;
            match outcome {
                Outcome::Finished => self.notice(t("ZMODEM 받기 완료: 받은 파일이 없습니다", "ZMODEM download done: no files were received"), "32"),
                outcome => self.zmodem_notice(&outcome, 0, Path::new("")),
            }
            return;
        }
        pick.ended = Some((files, outcome.clone()));
        if let Some(dir) = chosen {
            self.zmodem_move(&dir);
            return;
        }
        let wait = t("저장할 폴더를 고르면 그곳으로 옮깁니다", "they move to the folder you choose");
        match outcome {
            Outcome::Finished => self.notice(&tr!("ZMODEM 받기 완료: 파일 {count}개 · {wait}", "ZMODEM download done: {count} file(s), {wait}"), "36"),
            Outcome::Cancelled => self.notice(
                &tr!("ZMODEM 받기를 취소했습니다 (끝까지 받은 파일 {count}개) · {wait}", "ZMODEM download cancelled ({count} file(s) received completely), {wait}"),
                "33",
            ),
            Outcome::Failed(e) => {
                self.notice(&tr!("ZMODEM 받기 실패: {e} (끝까지 받은 파일 {count}개) · {wait}", "ZMODEM download failed: {e} ({count} file(s) received completely), {wait}"), "31")
            }
        }
    }

    /// Moves the files of the finished download asked about into `dir`.
    fn zmodem_move(&mut self, dir: &Path) {
        let Some(mut pick) = self.zmodem_pick.take() else { return };
        let Some((files, outcome)) = pick.ended.take() else { return };
        let mut failed = Vec::new();
        for file in &files {
            if let Err(e) = zmodem::move_into(file, dir) {
                let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                failed.push(format!("{name}: {e}"));
            }
        }
        self.zmodem_notice(&outcome, files.len() - failed.len(), dir);
        if !failed.is_empty() {
            pick.keep = true;
            let (staging, failed) = (pick.staging.display(), failed.join(", "));
            self.notice(
                &tr!("옮기지 못한 파일은 {staging} 에 남겨 두었습니다: {failed}", "Files that could not be moved are left in {staging}: {failed}"),
                "31",
            );
        }
    }

    /// How a ZMODEM download ended, with `saved` files in `dir`.
    fn zmodem_notice(&mut self, outcome: &Outcome, saved: usize, dir: &Path) {
        let dir = dir.display();
        match outcome {
            Outcome::Finished => {
                self.notice(&tr!("ZMODEM 받기 완료: 파일 {saved}개를 {dir} 에 저장했습니다", "ZMODEM download done: {saved} file(s) saved in {dir}"), "32")
            }
            Outcome::Cancelled => self.notice(
                &tr!("ZMODEM 받기를 취소했습니다 (끝까지 받은 파일 {saved}개)", "ZMODEM download cancelled ({saved} file(s) received completely)"),
                "33",
            ),
            Outcome::Failed(e) => self.notice(&tr!("ZMODEM 받기 실패: {e}", "ZMODEM download failed: {e}"), "31"),
        }
    }

    /// Whether a ZMODEM download is running (it needs regular [`Session::poll`]s).
    pub fn in_zmodem(&self) -> bool {
        self.zmodem.is_some()
    }

    /// Shows how the ZMODEM download goes (each saved file, the file being received) and wraps
    /// it up when it is over.
    fn zmodem_show(&mut self) {
        let Some(run) = &mut self.zmodem else { return };
        let mut text = String::new();
        for path in &run.download.saved()[run.listed..] {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            text += &format!("\r\x1b[K  \x1b[32m✓\x1b[0m {} ({})\r\n", printable(&name), human_size(size));
            run.drawn = None;
        }
        run.listed = run.download.saved().len();
        if let Some(outcome) = run.download.outcome().cloned() {
            let saved = run.download.saved().to_vec();
            let dir = run.download.dir().to_owned();
            let rest = run.download.take_passthrough();
            let silent = run.silent;
            self.zmodem = None;
            self.terminal.feed_local(&format!("{text}\r\x1b[K"));
            if silent {
            } else if self.zmodem_pick.as_ref().is_some_and(|p| p.ended.is_none()) {
                self.zmodem_pick_ended(saved, outcome);
            } else {
                self.zmodem_notice(&outcome, saved.len(), &dir);
            }
            // What the server printed after the transfer (its prompt).
            if !rest.is_empty() {
                self.output(&rest);
            }
            return;
        }
        let now = Instant::now();
        if let Some(p) = run.download.progress()
            && run.drawn.is_none_or(|at| now.duration_since(at) >= ZMODEM_REDRAW)
        {
            run.drawn = Some(now);
            let amount = match p.size {
                Some(size) if size > 0 => format!(
                    "{} / {} ({}%)",
                    human_size(p.received),
                    human_size(size),
                    (p.received.min(size) * 100) / size
                ),
                _ => human_size(p.received),
            };
            text += &format!("\r\x1b[K  {}  {amount}", printable(p.name));
        }
        if !text.is_empty() {
            self.terminal.feed_local(&text);
        }
    }

    /// Stops a ZMODEM download without a word (the connection is going away); its unfinished
    /// file is deleted. Returns whether one was running.
    fn end_zmodem(&mut self) -> bool {
        self.zmodem_detector = zmodem::Detector::default();
        let running = self.zmodem.take().is_some();
        // Files received completely still wait for their folder; a cut transfer is dropped.
        if self.zmodem_pick.as_ref().is_some_and(|p| p.ended.is_none()) {
            self.zmodem_pick = None;
        }
        running
    }

    /// Protocol bytes (not typed input) for the remote side.
    fn send_raw(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(remote) = &self.remote {
            self.tx_bytes += bytes.len() as u64;
            remote.handle.send(bytes);
        }
    }

    /// Answers the prompt the output now ends at, if it is the next login step.
    fn auto_login(&mut self, newline: bool) {
        let Some(runner) = &mut self.login else { return };
        let action = runner.on_output(newline, &self.terminal.cursor_line_text(), std::time::Instant::now());
        if runner.done() {
            self.login = None;
        }
        match action {
            Some(LoginAction::Send(text)) => {
                if let Some(remote) = &self.remote {
                    let bytes = self.terminal.encode_input(&text);
                    self.tx_bytes += bytes.len() as u64;
                    remote.handle.send(bytes);
                }
            }
            Some(LoginAction::Stop(message)) => self.notice(&message, "33"),
            None => {}
        }
    }

    /// Writes a local status line into the terminal, in the given SGR color. The text can come
    /// from a server (an SSH banner, an error message): its control characters are shown, not
    /// run, so it cannot set the clipboard or title or draw fake status lines.
    fn notice(&mut self, text: &str, sgr: &str) {
        let body = printable(text.trim_end()).replace('\n', "\r\n");
        self.terminal.feed_local(&format!("\r\n\x1b[{sgr}m[{body}]\x1b[0m\r\n"));
    }

    /// Whether keyboard input goes to the local prompt.
    pub fn in_console(&self) -> bool {
        matches!(self.state, SessionState::Local | SessionState::Closed)
    }

    /// Key presses and other control input (already encoded).
    pub fn send_bytes(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.start_screen = false;
        // During a ZMODEM download typing does not reach the server; Ctrl+C, Ctrl+X or Esc
        // cancel the download.
        if let Some(run) = &mut self.zmodem {
            if bytes.contains(&0x03) || bytes.contains(&0x18) || bytes == [0x1b] {
                let reply = run.download.cancel(Instant::now());
                self.send_raw(reply);
                self.zmodem_show();
            }
            return;
        }
        // Any input of the user's own ends a pending automatic login, except Enter, which
        // wakes up consoles that only show their prompt then.
        if bytes == b"\r" {
            if let Some(login) = &mut self.login {
                login.extend(std::time::Instant::now());
            }
        } else {
            self.login = None;
        }
        if self.in_console() {
            self.console_key(&bytes);
            return;
        }
        if self.state == SessionState::Connected
            && let Some(remote) = &self.remote
        {
            self.terminal.scroll_to_bottom();
            self.terminal.clear_selection();
            self.tx_bytes += bytes.len() as u64;
            remote.handle.send(bytes);
        }
    }

    /// The encoding for the status bar: the setting, or what automatic detection found.
    pub fn charset_label(&self) -> String {
        match (self.charset, self.terminal.detected_charset()) {
            (Charset::Auto, None) => t("자동 감지 중 (UTF-8 로 표시)", "Detecting (shown as UTF-8)").into(),
            (Charset::Auto, Some(found)) => tr!("{found} (자동 감지)", "{found} (auto-detected)"),
            (set, _) => set.to_string(),
        }
    }

    /// Typed or pasted text.
    pub fn send_text(&mut self, text: &str) {
        // Typing yourself takes over from an automatic login still waiting for its prompt.
        self.login = None;
        self.start_screen = false;
        if self.in_console() {
            self.console_text(text);
            return;
        }
        let bytes = self.terminal.encode_input(text);
        self.send_bytes(bytes);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if self.terminal.resize(cols, rows)
            && let Some(remote) = &self.remote
        {
            remote.handle.resize(cols as u16, rows as u16);
        }
    }

    pub fn can_sftp(&self) -> bool {
        self.protocol() == Some(Protocol::Ssh) && self.state == SessionState::Connected
    }

    // -- local prompt ---------------------------------------------------------------------------

    /// Makes room for connection output: removes the prompt (and anything typed after it)
    /// when it is showing, or else starts a new line.
    fn leave_prompt(&mut self) {
        if self.in_console() {
            let mut out = String::new();
            self.console.set_width(self.terminal.cols());
            self.console.erase(&mut out);
            self.terminal.feed_local(&out);
        } else {
            self.terminal.feed_local("\r\n");
        }
    }

    /// Shows a fresh prompt line.
    pub fn prompt(&mut self) {
        self.console.set_width(self.terminal.cols());
        let folder = local_fs::display(&self.cwd);
        self.console.set_prompt(format!("\x1b[1;36mplayterm\x1b[0m \x1b[34m{folder}\x1b[0m> "));
        let mut out = String::new();
        self.console.prompt(&mut out);
        self.terminal.feed_local(&out);
    }

    /// Prints command output (plain `\n` line breaks) above the prompt.
    pub fn print(&mut self, text: &str) {
        let mut body = text.replace("\r\n", "\n").replace('\n', "\r\n");
        if !body.ends_with("\r\n") {
            body.push_str("\r\n");
        }
        self.terminal.feed_local(&body);
    }

    fn console_text(&mut self, text: &str) {
        if !self.requests.is_empty() || self.ping.is_some() {
            return;
        }
        self.terminal.scroll_to_bottom();
        self.terminal.clear_selection();
        self.console.set_width(self.terminal.cols());
        let mut out = String::new();
        let lines = self.console.text(text, &mut out);
        self.terminal.feed_local(&out);
        for line in lines {
            // Once a command is handed to the app (it may connect), the rest of a paste is dropped.
            if !self.requests.is_empty() || !self.in_console() {
                break;
            }
            self.run_line(&line);
        }
    }

    fn console_key(&mut self, bytes: &[u8]) {
        // A running ping takes no input but Ctrl+C, which stops it.
        if self.ping.is_some() {
            if bytes.contains(&0x03) {
                self.terminal.feed_local("^C\r\n");
                self.end_ping();
            }
            return;
        }
        if !self.requests.is_empty() {
            return;
        }
        self.terminal.scroll_to_bottom();
        self.terminal.clear_selection();
        self.console.set_width(self.terminal.cols());
        let mut out = String::new();
        let result = self.console.key(bytes, &mut out);
        self.terminal.feed_local(&out);
        match result {
            KeyResult::None => {}
            KeyResult::Line(line) => self.run_line(&line),
            KeyResult::Clear => self.clear_screen(),
        }
    }

    fn clear_screen(&mut self) {
        self.terminal.feed_local("\x1b[H\x1b[2J");
        self.terminal.clear_scrollback();
        self.prompt();
    }

    fn run_line(&mut self, line: &str) {
        match console::parse(line) {
            Parsed::Empty => self.prompt(),
            Parsed::Help => {
                self.print(console::help());
                self.prompt();
            }
            Parsed::Clear => self.clear_screen(),
            Parsed::Ls(opts) => {
                match local_fs::list(&self.cwd, &opts, self.terminal.cols()) {
                    Ok(text) => self.print(&text),
                    Err(e) => self.print(&format!("\x1b[31m{e}\x1b[0m")),
                }
                self.prompt();
            }
            Parsed::Cd(dir) => {
                match local_fs::change_dir(&self.cwd, dir.as_deref()) {
                    Ok(dir) => self.cwd = dir,
                    Err(e) => self.print(&format!("\x1b[31m{e}\x1b[0m")),
                }
                self.prompt();
            }
            Parsed::Pwd => {
                let cwd = self.cwd.display().to_string();
                self.print(&cwd);
                self.prompt();
            }
            Parsed::Error(e) => {
                self.print(&format!("\x1b[31m{e}\x1b[0m"));
                self.prompt();
            }
            // The app answers with output and a new prompt, or connects.
            Parsed::Request(request) => self.requests.push(request),
        }
    }

    /// Starts `ping` at the prompt; its lines appear as the answers come (see `ping_output`).
    pub fn start_ping(&mut self, host: String, options: PingOptions, waker: Waker) {
        let size = options.size;
        let ping = Ping::start(host.clone(), options, waker);
        self.ping = Some(PingRun { ping, host, size, addr: None, stats: PingStats::default() });
    }

    /// Prints what the running ping reports. Returns whether anything came.
    fn ping_output(&mut self) -> bool {
        let Some(run) = &mut self.ping else { return false };
        let mut text = String::new();
        let mut ended = false;
        while let Some(event) = run.ping.try_recv() {
            run.stats.record(&event);
            match event {
                PingEvent::Start(addr) => {
                    run.addr = Some(addr);
                    let target = if run.host == addr.to_string() { run.host.clone() } else { format!("{} [{addr}]", run.host) };
                    let size = run.size;
                    text += &tr!(
                        "{target} 에 ping 을 보냅니다 ({size}바이트 데이터) \x1b[90m· Ctrl+C 로 중지\x1b[0m\n",
                        "Pinging {target} with {size} bytes of data \x1b[90m· Ctrl+C stops\x1b[0m\n"
                    );
                }
                PingEvent::Reply { from, bytes, time_ms, ttl } => {
                    let time = if time_ms == 0 { "<1ms".to_owned() } else { format!("={time_ms}ms") };
                    let ttl = ttl.map(|ttl| format!(" TTL={ttl}")).unwrap_or_default();
                    text += &tr!("{from} 의 응답: 바이트={bytes} 시간{time}{ttl}\n", "Reply from {from}: bytes={bytes} time{time}{ttl}\n");
                }
                PingEvent::TimedOut => {
                    text += &format!("\x1b[33m{}\x1b[0m\n", t("요청 시간이 만료되었습니다.", "Request timed out."));
                }
                PingEvent::Failed { from, status } => {
                    let what = ping::status_text(status);
                    text += &match from {
                        Some(from) => tr!("\x1b[33m{from} 의 응답: {what}\x1b[0m\n", "\x1b[33mReply from {from}: {what}\x1b[0m\n"),
                        None => format!("\x1b[33m{what}\x1b[0m\n"),
                    };
                }
                PingEvent::Error(e) => {
                    text += &format!("\x1b[31m{}\x1b[0m\n", printable(&e));
                    ended = true;
                }
                PingEvent::Done => ended = true,
            }
        }
        let changed = !text.is_empty() || ended;
        if !text.is_empty() {
            self.print(text.trim_end_matches('\n'));
        }
        if ended {
            self.end_ping();
        }
        changed
    }

    /// Ends the ping (it finished, or Ctrl+C): prints its statistics and the prompt again.
    fn end_ping(&mut self) {
        let Some(run) = self.ping.take() else { return };
        let stats = &run.stats;
        if let Some(addr) = run.addr
            && stats.sent > 0
        {
            let (sent, received, lost, loss) = (stats.sent, stats.received, stats.lost(), stats.loss_percent());
            let mut text = tr!(
                "\n{addr} 에 대한 ping 통계:\n    패킷: 보냄 = {sent}, 받음 = {received}, 손실 = {lost} ({loss}% 손실)",
                "\nPing statistics for {addr}:\n    Packets: Sent = {sent}, Received = {received}, Lost = {lost} ({loss}% loss)"
            );
            if stats.received > 0 {
                let (min, max, average) = (stats.min_ms, stats.max_ms, stats.average_ms());
                text += &tr!(
                    "\n왕복 시간(밀리초):\n    최소 = {min}ms, 최대 = {max}ms, 평균 = {average}ms",
                    "\nApproximate round trip times in milli-seconds:\n    Minimum = {min}ms, Maximum = {max}ms, Average = {average}ms"
                );
            }
            self.print(&text);
        }
        drop(run);
        self.prompt();
    }

    /// Asks for the shell's current directory; the SFTP browser goes there once it is known.
    /// `asked`: the user asked for it, so a failure is reported.
    pub fn follow_shell_dir(&mut self, asked: bool) {
        if self.can_sftp()
            && let Some(remote) = &self.remote
        {
            self.shell_dir_request = Some(asked || self.shell_dir_request == Some(true));
            remote.handle.query_shell_dir();
        }
    }

    /// Starts SFTP for the side panel. Does nothing after a failure on this connection until
    /// [`Self::retry_sftp`].
    pub fn ensure_sftp(&mut self) {
        if self.sftp.is_none() && self.sftp_error.is_none() && self.can_sftp() {
            self.request_sftp();
        }
    }

    pub fn retry_sftp(&mut self) {
        self.sftp_error = None;
        self.ensure_sftp();
    }

    /// Why SFTP could not be opened on this connection.
    pub fn sftp_error(&self) -> Option<&str> {
        self.sftp_error.as_deref()
    }

    fn request_sftp(&mut self) {
        if !self.sftp_pending
            && let Some(remote) = &self.remote
        {
            self.sftp_pending = true;
            self.sftp_start = None;
            remote.handle.open_sftp();
            // Opens where the user is in the terminal.
            self.follow_shell_dir(false);
        }
    }

    pub fn close(&mut self) {
        if let Some(remote) = &self.remote {
            remote.handle.close();
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn elapsed_reads_naturally() {
        playterm_i18n::set_english(false);
        assert_eq!(elapsed(Duration::from_secs(20)), "방금");
        assert_eq!(elapsed(Duration::from_secs(12 * 60 + 5)), "12분");
        assert_eq!(elapsed(Duration::from_secs(2 * 3600)), "2시간");
        assert_eq!(elapsed(Duration::from_secs(2 * 3600 + 5 * 60)), "2시간 5분");
    }
    use super::*;

    #[test]
    fn server_text_cannot_run_escape_sequences() {
        let banner = "Welcome\r\n\x1b]52;c;ZXZpbA==\x07\x1b[2J[verified]\rfake\tok";
        let shown = printable(banner);
        assert!(!shown.contains('\x1b') && !shown.contains('\x07') && !shown.contains('\r'), "{shown:?}");
        assert_eq!(shown, "Welcome\n?]52;c;ZXZpbA==??[2J[verified]?fake\tok");
    }
}
