//! One open tab: a terminal screen bound to a network session.

use playterm_net::{AuthPrompt, ConnectParams, HostKeyCheck, NetRuntime, SessionEvent, SessionHandle, Waker};
use playterm_term::{Charset, Point, Terminal, TerminalEvent};

const SCROLLBACK_LINES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
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
}

/// Mouse selection gesture state.
#[derive(Default)]
pub struct ClickState {
    /// Time and cell of the last primary press, for double/triple-click detection.
    pub last: Option<(f64, Point)>,
    pub count: u8,
    pub dragging: bool,
}

pub struct Session {
    pub id: u64,
    pub params: ConnectParams,
    pub charset: Charset,
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
    handle: SessionHandle,
}

impl Session {
    pub fn connect(id: u64, spec: SessionSpec, net: &NetRuntime, waker: Waker) -> Self {
        let SessionSpec { params, charset, name } = spec;
        let mut terminal = Terminal::new(params.cols.into(), params.rows.into(), charset, SCROLLBACK_LINES);
        terminal.feed_local(&format!(
            "\x1b[90m{} {}:{} 에 연결 중...\x1b[0m\r\n",
            params.protocol, params.host, params.port
        ));
        let handle = net.connect(params.clone(), waker);
        Self {
            id,
            params,
            charset,
            name,
            terminal,
            state: SessionState::Connecting,
            remote_title: None,
            host_key: None,
            auth: None,
            preedit: String::new(),
            scroll_accum: 0.0,
            clipboard_store: None,
            click: ClickState::default(),
            handle,
        }
    }

    /// Starts a fresh connection with the same parameters, keeping the screen contents.
    pub fn reconnect(&mut self, net: &NetRuntime, waker: Waker) {
        self.handle.close();
        self.params.cols = self.terminal.cols() as u16;
        self.params.rows = self.terminal.rows() as u16;
        self.terminal.feed_local(&format!(
            "\r\n\x1b[90m{}:{} 에 다시 연결 중...\x1b[0m\r\n",
            self.params.host, self.params.port
        ));
        self.handle = net.connect(self.params.clone(), waker);
        self.state = SessionState::Connecting;
        self.host_key = None;
        self.auth = None;
    }

    pub fn spec(&self) -> SessionSpec {
        SessionSpec { params: self.params.clone(), charset: self.charset, name: self.name.clone() }
    }

    pub fn tab_title(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        match &self.remote_title {
            Some(t) if !t.is_empty() => t.clone(),
            _ if self.params.username.is_empty() => self.params.host.clone(),
            _ => format!("{}@{}", self.params.username, self.params.host),
        }
    }

    /// Processes pending network and terminal events. Returns `true` if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.handle.try_recv() {
            changed = true;
            match event {
                SessionEvent::Connected => self.state = SessionState::Connected,
                SessionEvent::Data(bytes) => self.terminal.feed(&bytes),
                SessionEvent::Notice(text) => self.notice(&text, "33"),
                SessionEvent::HostKeyCheck(check) => self.host_key = Some(check),
                SessionEvent::AuthPrompt(prompt) => {
                    let answers = vec![String::new(); prompt.fields.len()];
                    self.auth = Some(PendingAuth { prompt, answers, focus_requested: false });
                }
                SessionEvent::Closed(reason) => {
                    self.state = SessionState::Closed;
                    self.host_key = None;
                    self.auth = None;
                    match reason {
                        Some(reason) => self.notice(&format!("연결 종료: {reason}"), "31"),
                        None => self.notice("연결이 종료되었습니다", "90"),
                    }
                }
            }
        }

        for event in self.terminal.take_events() {
            changed = true;
            match event {
                TerminalEvent::Reply(bytes) => self.handle.send(bytes),
                TerminalEvent::Title(title) => self.remote_title = title,
                TerminalEvent::ClipboardStore(text) => self.clipboard_store = Some(text),
                TerminalEvent::Bell => {}
            }
        }
        changed
    }

    /// Writes a local status line into the terminal, in the given SGR color.
    fn notice(&mut self, text: &str, sgr: &str) {
        let body = text.trim_end().replace("\r\n", "\n").replace('\n', "\r\n");
        self.terminal.feed_local(&format!("\r\n\x1b[{sgr}m[{body}]\x1b[0m\r\n"));
    }

    pub fn send_bytes(&mut self, bytes: Vec<u8>) {
        if self.state == SessionState::Connected && !bytes.is_empty() {
            self.terminal.scroll_to_bottom();
            self.terminal.clear_selection();
            self.handle.send(bytes);
        }
    }

    pub fn send_text(&mut self, text: &str) {
        let bytes = self.terminal.encode_input(text);
        self.send_bytes(bytes);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if self.terminal.resize(cols, rows) {
            self.handle.resize(cols as u16, rows as u16);
        }
    }

    pub fn close(&mut self) {
        self.handle.close();
    }
}
