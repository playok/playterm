//! One open tab: a terminal screen, bound to a network session while connected and running
//! the local command prompt ([`crate::console`]) while not.

use playterm_net::{AuthPrompt, ConnectParams, HostKeyCheck, NetRuntime, Protocol, SessionEvent, SessionHandle, Waker};
use playterm_term::{Charset, LogMode, Palette, Point, Terminal, TerminalEvent};

use crate::autologin::{LoginAction, LoginRunner, LoginScript};
use crate::console::{self, KeyResult, LineEditor, Parsed, Request};
use crate::local_fs;
use crate::logging::{self, LogPlan};
use crate::search_bar::SearchBar;
use crate::sftp_view::SftpBrowser;

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
    pub answers: Vec<String>,
    pub focus_requested: bool,
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
    pub click: ClickState,
    pub mouse: MouseReport,
    pub sftp: Option<SftpBrowser>,
    /// The find bar, while open.
    pub search: Option<SearchBar>,
    /// An SFTP channel was requested and has not answered yet.
    sftp_pending: bool,
    /// Auto-log settings of the profile, kept so duplicates log to their own file.
    log_plan: Option<LogPlan>,
    /// The site's automatic login, kept for reconnects and duplicates.
    login_script: Option<LoginScript>,
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
}

impl Session {
    /// A tab with the local command prompt.
    pub fn local(id: u64, cols: u16, rows: u16, setup: &TermSetup) -> Self {
        let mut session = Self::blank(id, cols, rows, Charset::Utf8, setup);
        session.terminal.feed_local(
            "\x1b[1mplayterm\x1b[0m \x1b[90m- 연결되지 않은 탭입니다. \
             ssh, telnet, open(사이트 관리), sites(사이트 목록), ls 등을 입력하세요. help: 명령 목록\x1b[0m\r\n\r\n",
        );
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
            click: ClickState::default(),
            mouse: MouseReport::default(),
            sftp: None,
            search: None,
            sftp_pending: false,
            log_plan: None,
            login_script: None,
            login: None,
            remote: None,
            console: LineEditor::default(),
            cwd: local_fs::home(),
            requests: Vec::new(),
            rx_bytes: 0,
            tx_bytes: 0,
        }
    }

    /// Connects this tab (from the prompt, or as a new tab), keeping the screen contents.
    pub fn start(&mut self, spec: SessionSpec, net: &NetRuntime, waker: Waker) {
        let SessionSpec { mut params, charset, name, log, login } = spec;
        if let Some(remote) = &self.remote {
            remote.handle.close();
        }
        self.leave_prompt();
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
        self.sftp_pending = false;
        self.requests.clear();
        self.log_plan = log.clone();
        self.login = login.clone().map(LoginRunner::new);
        self.login_script = login;
        self.rx_bytes = 0;
        self.tx_bytes = 0;
        let handle = net.connect(params.clone(), waker);
        self.remote = Some(Remote { params, handle });
        self.state = SessionState::Connecting;
        let target = self.target();
        self.terminal.feed_local(&format!("\x1b[90m{target} 에 연결 중...\x1b[0m\r\n"));
        if let Some(plan) = log {
            let path = logging::new_log_path(&plan.dir, &self.tab_title());
            self.start_log(&path, plan.mode);
        }
    }

    /// `SSH host:22`, `Serial COM3 (9600 8N1)`.
    pub fn target(&self) -> String {
        let Some(Remote { params: p, .. }) = &self.remote else { return "로컬".into() };
        match p.protocol {
            Protocol::Serial => format!("Serial {} ({})", p.serial.port, p.serial.line_summary()),
            proto => format!("{proto} {}:{}", p.host, p.port),
        }
    }

    pub fn start_log(&mut self, path: &std::path::Path, mode: LogMode) {
        let title = self.tab_title();
        match self.terminal.start_log(path, mode, &title) {
            Ok(()) => self.notice(&format!("로그 기록 시작: {}", path.display()), "90"),
            Err(e) => self.notice(&format!("로그 파일을 만들 수 없습니다 ({}): {e}", path.display()), "31"),
        }
    }

    pub fn stop_log(&mut self) {
        if let Some(path) = self.terminal.stop_log() {
            self.notice(&format!("로그 기록 중지: {}", path.display()), "90");
        }
    }

    pub fn is_logging(&self) -> bool {
        self.terminal.log().is_some()
    }

    fn protocol(&self) -> Option<Protocol> {
        self.remote.as_ref().map(|r| r.params.protocol)
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
        remote.handle = net.connect(remote.params.clone(), waker);
        self.leave_prompt();
        let target = self.target();
        self.terminal.feed_local(&format!("\x1b[90m{target} 에 다시 연결 중...\x1b[0m\r\n"));
        self.state = SessionState::Connecting;
        self.host_key = None;
        self.auth = None;
        self.requests.clear();
        self.login = self.login_script.clone().map(LoginRunner::new);
        self.rx_bytes = 0;
        self.tx_bytes = 0;
    }

    /// What this tab is connected with; `None` for a tab that never connected.
    pub fn spec(&self) -> Option<SessionSpec> {
        Some(SessionSpec {
            params: self.remote.as_ref()?.params.clone(),
            charset: self.charset,
            name: self.name.clone(),
            log: self.log_plan.clone(),
            login: self.login_script.clone(),
        })
    }

    pub fn tab_title(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let Some(Remote { params, .. }) = &self.remote else { return "로컬".into() };
        match &self.remote_title {
            Some(t) if !t.is_empty() => t.clone(),
            _ if params.username.is_empty() => params.host.clone(),
            _ => format!("{}@{}", params.username, params.host),
        }
    }

    /// Processes pending network and terminal events. Returns `true` if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.remote.as_ref().and_then(|r| r.handle.try_recv()) {
            changed = true;
            match event {
                SessionEvent::Connected => self.state = SessionState::Connected,
                SessionEvent::Data(bytes) => {
                    self.rx_bytes += bytes.len() as u64;
                    self.terminal.feed(&bytes);
                    self.auto_login(bytes.contains(&b'\n'));
                }
                SessionEvent::Notice(text) => self.notice(&text, "33"),
                SessionEvent::HostKeyCheck(check) => self.host_key = Some(check),
                SessionEvent::AuthPrompt(prompt) => {
                    let answers = vec![String::new(); prompt.fields.len()];
                    self.auth = Some(PendingAuth { prompt, answers, focus_requested: false });
                }
                SessionEvent::Sftp(result) => {
                    self.sftp_pending = false;
                    match result {
                        Ok(conn) => self.sftp = Some(SftpBrowser::new(conn)),
                        Err(e) => self.notice(&format!("SFTP 열기 실패: {e}"), "31"),
                    }
                }
                SessionEvent::Closed(reason) => {
                    self.state = SessionState::Closed;
                    self.host_key = None;
                    self.auth = None;
                    self.sftp = None;
                    self.sftp_pending = false;
                    self.terminal.reset_modes();
                    match reason {
                        Some(reason) => self.notice(&format!("연결 종료: {reason}"), "31"),
                        None => self.notice("연결이 종료되었습니다", "90"),
                    }
                    self.terminal.feed_local("\x1b[90mr: 다시 연결 · help: 명령 목록\x1b[0m\r\n");
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
                TerminalEvent::LogFailed(e) => self.notice(&format!("로그 기록 실패, 중지합니다: {e}"), "31"),
                TerminalEvent::Bell => {}
            }
        }
        changed
    }

    /// Answers the prompt the output now ends at, if it is the next login step.
    fn auto_login(&mut self, newline: bool) {
        let Some(runner) = &mut self.login else { return };
        let action = runner.on_output(newline, &self.terminal.cursor_line_text());
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

    /// Writes a local status line into the terminal, in the given SGR color.
    fn notice(&mut self, text: &str, sgr: &str) {
        let body = text.trim_end().replace("\r\n", "\n").replace('\n', "\r\n");
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

    pub fn charset(&self) -> Charset {
        self.charset
    }

    /// Typed or pasted text.
    pub fn send_text(&mut self, text: &str) {
        // Typing yourself takes over from an automatic login still waiting for its prompt.
        self.login = None;
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
        if !self.requests.is_empty() {
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
                self.print(console::HELP);
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

    /// Shows the SFTP browser, opening the SFTP channel on first use.
    pub fn open_sftp(&mut self) {
        if let Some(browser) = &mut self.sftp {
            browser.open = true;
        } else if self.can_sftp() && !self.sftp_pending {
            self.sftp_pending = true;
            if let Some(remote) = &self.remote {
                remote.handle.open_sftp();
            }
        }
    }

    pub fn close(&mut self) {
        if let Some(remote) = &self.remote {
            remote.handle.close();
        }
    }
}
