use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::deadline::Deadline;
use crate::forward::ForwardRule;
use crate::keepalive::KeepAlive;
use crate::rdp::{RdpFrame, RdpInput, RdpSettings, SharedScreen};
use crate::serial::SerialSettings;
use crate::sftp::SftpConnection;

/// Called from network threads whenever a new event is queued, so the UI can wake up.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Ssh,
    Telnet,
    /// Local COM port; `host`/`port` are unused, see [`ConnectParams::serial`].
    Serial,
    /// Windows Remote Desktop; shown as a picture instead of a terminal.
    Rdp,
}

impl Protocol {
    pub const ALL: [Protocol; 4] = [Protocol::Ssh, Protocol::Telnet, Protocol::Serial, Protocol::Rdp];

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh => 22,
            Protocol::Telnet => 23,
            Protocol::Serial => 0,
            Protocol::Rdp => 3389,
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Protocol::Ssh => "SSH",
            Protocol::Telnet => "Telnet",
            Protocol::Serial => "Serial",
            Protocol::Rdp => "RDP",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X11Request {
    /// Where the X server is (e.g. `localhost:0.0`).
    pub display: String,
    /// The server's X programs get the whole display, keyboard included (`ssh -Y`); otherwise
    /// they get an untrusted cookie, or nothing when none can be made.
    pub trusted: bool,
}

#[derive(Clone)]
pub struct ConnectParams {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    /// SSH user name. Unused for Telnet (the remote login prompt asks for it).
    pub username: String,
    pub password: Option<String>,
    /// Private key chosen for this connection; tried first.
    pub identity_file: Option<PathBuf>,
    /// Keys tried after the agent if they exist (`~/.ssh/id_ed25519`, ...).
    pub default_identity_files: Vec<PathBuf>,
    /// Offer keys from running SSH agents (Windows OpenSSH agent, Pageant; `SSH_AUTH_SOCK` on Unix).
    pub use_agent: bool,
    /// Agent to use instead of `SSH_AUTH_SOCK` / the OpenSSH service pipe: a named pipe on
    /// Windows (Pageant is then not consulted), a Unix socket path elsewhere.
    pub agent_pipe: Option<String>,
    /// Port forwardings started once the SSH session is authenticated.
    pub forwards: Vec<ForwardRule>,
    /// Line settings for [`Protocol::Serial`].
    pub serial: SerialSettings,
    /// Keeps the connection from timing out while idle.
    pub keepalive: KeepAlive,
    /// Options for [`Protocol::Rdp`].
    pub rdp: RdpSettings,
    /// Size of the tab in pixels: the RDP desktop size when it follows the tab.
    pub desktop: (u16, u16),
    /// Value for the `TERM` variable / Telnet terminal type.
    pub term_type: String,
    pub cols: u16,
    pub rows: u16,
    /// known_hosts file used to verify SSH host keys.
    pub known_hosts: PathBuf,
    /// Longest wait for the server while connecting (TCP, handshake, login), not counting
    /// time spent waiting on the user. Unused for serial ports.
    pub connect_timeout: Duration,
    /// SSH: reach the host through this one (a bastion), like OpenSSH's ProxyJump / -J. It can
    /// have a jump host of its own (A → B → this host).
    pub jump: Option<Box<ConnectParams>>,
    /// SSH: X11 forwarding (`ssh -X`, or with `trusted` `ssh -Y`).
    pub x11: Option<X11Request>,
}

impl ConnectParams {
    /// The time limit for connecting when no setting says otherwise.
    pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
}

impl fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectParams")
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("identity_file", &self.identity_file)
            .field("use_agent", &self.use_agent)
            .field("forwards", &self.forwards)
            .field("keepalive", &self.keepalive)
            .field("rdp", &self.rdp)
            .field("term_type", &self.term_type)
            .field("connect_timeout", &self.connect_timeout)
            .finish_non_exhaustive()
    }
}

/// Event from a session task to the UI.
#[derive(Debug)]
pub enum SessionEvent {
    /// Transport is up and the remote shell is ready for input.
    Connected,
    /// Raw bytes from the remote side, in the session's wire charset.
    Data(Vec<u8>),
    /// Informational text to show in the terminal (e.g. SSH banner).
    Notice(String),
    /// The SSH host key is not in known_hosts; the user must decide.
    HostKeyCheck(HostKeyCheck),
    /// The server is asking for credentials.
    AuthPrompt(AuthPrompt),
    /// Answer to [`SessionHandle::open_sftp`].
    Sftp(Result<SftpConnection, String>),
    /// Answer to [`SessionHandle::query_shell_dir`]: the remote shell's current directory, if
    /// it could be found.
    ShellDir(Option<String>),
    /// Session ended. `Some` carries an error description.
    Closed(Option<String>),
}

#[derive(Debug)]
pub struct HostKeyCheck {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    /// An RDP server's TLS certificate (kept in `known_rdp_hosts`) rather than an SSH host key.
    pub certificate: bool,
    reply: oneshot::Sender<bool>,
}

impl HostKeyCheck {
    /// `true` trusts the key and records it in known_hosts.
    pub fn answer(self, accept: bool) {
        let _ = self.reply.send(accept);
    }
}

#[derive(Debug, Clone)]
pub struct PromptField {
    pub label: String,
    /// Whether the answer may be shown while typing (false for passwords).
    pub echo: bool,
}

#[derive(Debug)]
pub struct AuthPrompt {
    pub title: String,
    pub instructions: String,
    pub fields: Vec<PromptField>,
    /// The user being logged in, when the answers are that user's (password,
    /// keyboard-interactive); `None` for other questions such as a key passphrase. The UI may
    /// offer to log in as someone else, which takes a new connection: servers do not allow
    /// changing the user in the middle of authentication.
    pub user: Option<String>,
    /// The (single, hidden) answer is the account password: it may be saved for the site. Not
    /// for passphrases or for a second factor (a one-time code after the password).
    pub account_password: bool,
    reply: oneshot::Sender<Option<Vec<String>>>,
}

impl AuthPrompt {
    /// `None` cancels authentication.
    pub fn answer(self, answers: Option<Vec<String>>) {
        let _ = self.reply.send(answers);
    }
}

#[derive(Debug)]
pub(crate) enum SessionCommand {
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    OpenSftp,
    /// Find the remote shell's current directory (SSH).
    ShellDir,
    /// Serial BREAK / Telnet IAC BRK.
    SendBreak,
    /// Keyboard and mouse for an RDP desktop.
    Rdp(Vec<RdpInput>),
    Close,
}

/// Output received and not yet taken by the UI may not grow past this: the connection then
/// stops reading until the UI catches up (the server waits, as flow control has it), so a
/// flood of output cannot fill the memory.
pub const MAX_PENDING_OUTPUT: usize = 8 * 1024 * 1024;

/// Received output waiting in the event queue, shared by the connection and the UI.
struct Backlog {
    bytes: std::sync::atomic::AtomicUsize,
    /// The UI took some, or went away.
    taken: tokio::sync::Notify,
    closed: std::sync::atomic::AtomicBool,
    /// Set when the UI closes the session: a connection still being set up stops at once
    /// (the command queue is only read once connected).
    cancel: tokio::sync::watch::Sender<bool>,
}

impl Default for Backlog {
    fn default() -> Self {
        Self {
            bytes: Default::default(),
            taken: Default::default(),
            closed: Default::default(),
            cancel: tokio::sync::watch::Sender::new(false),
        }
    }
}

/// UI-side handle to a running session. Dropping it closes the session.
pub struct SessionHandle {
    commands: mpsc::UnboundedSender<SessionCommand>,
    events: std_mpsc::Receiver<SessionEvent>,
    /// The RDP screen and what changed since the UI last looked.
    screen: Arc<Mutex<SharedScreen>>,
    backlog: Arc<Backlog>,
    waker: Waker,
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        // A connection waiting for the UI to take output must not wait for ever.
        self.backlog.closed.store(true, std::sync::atomic::Ordering::Release);
        self.backlog.taken.notify_waiters();
        self.backlog.cancel.send_replace(true);
    }
}

impl SessionHandle {
    pub(crate) fn new(waker: Waker) -> (Self, mpsc::UnboundedReceiver<SessionCommand>, EventSink) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = std_mpsc::channel();
        let screen = Arc::new(Mutex::new(SharedScreen::default()));
        let backlog = Arc::new(Backlog::default());
        let sink =
            EventSink { tx: ev_tx, waker: waker.clone(), screen: screen.clone(), deadline: Deadline::default(), backlog: backlog.clone() };
        (Self { commands: cmd_tx, events: ev_rx, screen, backlog, waker }, cmd_rx, sink)
    }

    /// Keyboard and mouse input for an RDP session.
    pub fn send_rdp(&self, inputs: Vec<RdpInput>) {
        if !inputs.is_empty() {
            let _ = self.commands.send(SessionCommand::Rdp(inputs));
        }
    }

    /// The part of the RDP screen that changed since the last call.
    pub fn take_frame(&self) -> Option<RdpFrame> {
        self.screen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }

    pub fn send(&self, data: Vec<u8>) {
        if !data.is_empty() {
            let _ = self.commands.send(SessionCommand::Input(data));
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.commands.send(SessionCommand::Resize { cols, rows });
    }

    pub fn close(&self) {
        let _ = self.commands.send(SessionCommand::Close);
        // Still connecting (or waiting on a host key / password answer): stop that too.
        self.backlog.cancel.send_replace(true);
    }

    /// Sends a BREAK signal (serial line break, Telnet `IAC BRK`).
    pub fn send_break(&self) {
        let _ = self.commands.send(SessionCommand::SendBreak);
    }

    /// Opens an SFTP channel on this SSH connection; answered with [`SessionEvent::Sftp`].
    pub fn open_sftp(&self) {
        let _ = self.commands.send(SessionCommand::OpenSftp);
    }

    /// Asks for the current directory of the remote shell (SSH on Linux servers); answered
    /// with [`SessionEvent::ShellDir`].
    pub fn query_shell_dir(&self) {
        let _ = self.commands.send(SessionCommand::ShellDir);
    }

    pub fn try_recv(&self) -> Option<SessionEvent> {
        let event = self.events.try_recv().ok()?;
        if let SessionEvent::Data(data) = &event {
            self.backlog.bytes.fetch_sub(data.len(), std::sync::atomic::Ordering::AcqRel);
            self.backlog.taken.notify_waiters();
        }
        Some(event)
    }

    /// Received output not yet taken with [`SessionHandle::try_recv`], in bytes.
    pub fn pending_output(&self) -> usize {
        self.backlog.bytes.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Asks the UI for another frame (to take events it left for later).
    pub fn wake(&self) {
        (self.waker)();
    }
}

/// Why a TCP connection to `host:port` failed, in words that say what to check (#146), with the
/// system's own message after them.
pub(crate) fn connect_error(host: &str, port: u16, e: std::io::Error) -> anyhow::Error {
    use std::io::ErrorKind;
    // Windows: WSAHOST_NOT_FOUND, WSANO_DATA; elsewhere the resolver says "lookup".
    let unknown_host = matches!(e.raw_os_error(), Some(11001 | 11004)) || e.to_string().to_lowercase().contains("lookup");
    let why = if unknown_host {
        Some(playterm_i18n::tr!(
            "'{host}' 라는 호스트를 찾을 수 없습니다. 주소의 철자와 DNS·VPN 연결을 확인하세요",
            "No host named '{host}' was found. Check the address and the DNS / VPN connection"
        ))
    } else {
        match e.kind() {
            ErrorKind::ConnectionRefused => Some(playterm_i18n::tr!(
                "서버가 포트 {port} 에서 접속을 받지 않습니다. 서버(서비스)가 켜져 있는지, 포트 번호가 맞는지 확인하세요",
                "The server does not take connections on port {port}. Check that the server (service) runs and the port is right"
            )),
            ErrorKind::TimedOut => Some(playterm_i18n::tr!(
                "서버가 응답하지 않습니다. 주소와 방화벽, 네트워크(VPN)를 확인하세요",
                "The server does not answer. Check the address, the firewall and the network (VPN)"
            )),
            ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable => Some(playterm_i18n::tr!(
                "이 PC 에서 그 주소로 가는 길이 없습니다. 네트워크 연결과 VPN 을 확인하세요",
                "This PC has no route to that address. Check the network connection and the VPN"
            )),
            _ => None,
        }
    };
    let head = playterm_i18n::tr!("{host}:{port} 에 연결할 수 없습니다", "Cannot connect to {host}:{port}");
    match why {
        Some(why) => anyhow::anyhow!("{head}: {why} ({e})"),
        None => anyhow::Error::new(e).context(head),
    }
}

#[derive(Clone)]
pub(crate) struct EventSink {
    tx: std_mpsc::Sender<SessionEvent>,
    waker: Waker,
    screen: Arc<Mutex<SharedScreen>>,
    /// The connect time limit; paused while a prompt waits on the user.
    deadline: Deadline,
    backlog: Arc<Backlog>,
}

impl EventSink {
    /// Runs `connecting` within the connect time limit of `params` (see [`Deadline`]).
    pub(crate) async fn connecting<T>(
        &self,
        params: &ConnectParams,
        connecting: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let mut cancel = self.backlog.cancel.subscribe();
        tokio::select! {
            result = self.deadline.limit(params.connect_timeout, connecting) => result,
            _ = cancel.wait_for(|&cancelled| cancelled) => {
                Err(anyhow::anyhow!(playterm_i18n::t("연결을 취소했습니다", "Connection cancelled")))
            }
        }
    }

    pub fn send(&self, event: SessionEvent) {
        let _ = self.tx.send(event);
        (self.waker)();
    }

    /// Hands received output to the UI, first waiting while it has [`MAX_PENDING_OUTPUT`]
    /// or more still to take: reading from the connection pauses meanwhile. Returns at once
    /// when the UI is gone.
    pub(crate) async fn data(&self, data: Vec<u8>) {
        use std::sync::atomic::Ordering;
        if data.is_empty() {
            return;
        }
        loop {
            let taken = self.backlog.taken.notified();
            if self.backlog.closed.load(Ordering::Acquire) || self.backlog.bytes.load(Ordering::Acquire) < MAX_PENDING_OUTPUT {
                break;
            }
            // Woken when the UI takes some; the timeout only guards against a missed wake.
            let _ = tokio::time::timeout(std::time::Duration::from_millis(200), taken).await;
        }
        self.backlog.bytes.fetch_add(data.len(), Ordering::AcqRel);
        self.send(SessionEvent::Data(data));
    }

    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// The RDP screen shared with the UI.
    pub(crate) fn screen(&self) -> Arc<Mutex<SharedScreen>> {
        self.screen.clone()
    }

    /// Wakes the UI (the RDP screen changed).
    pub(crate) fn wake(&self) {
        (self.waker)();
    }

    pub fn notice(&self, text: impl Into<String>) {
        self.send(SessionEvent::Notice(text.into()));
    }

    /// Asks the user whether to trust an unknown host key. A closed UI counts as "no".
    pub async fn confirm_host_key(&self, host: &str, port: u16, algorithm: String, fingerprint: String) -> bool {
        let (reply, rx) = oneshot::channel();
        self.send(SessionEvent::HostKeyCheck(HostKeyCheck {
            host: host.to_owned(),
            port,
            algorithm,
            fingerprint,
            certificate: false,
            reply,
        }));
        let _waiting_on_user = self.deadline.pause();
        rx.await.unwrap_or(false)
    }

    /// Asks the user whether to trust an RDP server certificate seen for the first time.
    pub async fn confirm_certificate(&self, host: &str, port: u16, fingerprint: String) -> bool {
        let (reply, rx) = oneshot::channel();
        self.send(SessionEvent::HostKeyCheck(HostKeyCheck {
            host: host.to_owned(),
            port,
            algorithm: "X.509 (TLS)".into(),
            fingerprint,
            certificate: true,
            reply,
        }));
        let _waiting_on_user = self.deadline.pause();
        rx.await.unwrap_or(false)
    }

    /// Asks the user for credentials (of `user`, see [`AuthPrompt::user`]; `account_password`
    /// see [`AuthPrompt::account_password`]). `None` means cancelled.
    pub async fn prompt(
        &self,
        title: String,
        instructions: String,
        fields: Vec<PromptField>,
        user: Option<&str>,
        account_password: bool,
    ) -> Option<Vec<String>> {
        let (reply, rx) = oneshot::channel();
        let user = user.map(str::to_owned);
        self.send(SessionEvent::AuthPrompt(AuthPrompt { title, instructions, fields, user, account_password, reply }));
        let _waiting_on_user = self.deadline.pause();
        rx.await.ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_waits_for_the_ui_past_the_limit() {
        let (handle, _commands, sink) = SessionHandle::new(Arc::new(|| {}));
        const CHUNK: usize = 1024 * 1024;
        const CHUNKS: usize = 32;
        let sender = tokio::spawn(async move {
            for _ in 0..CHUNKS {
                sink.data(vec![b'x'; CHUNK]).await;
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // The connection stopped at the limit instead of queueing everything.
        assert!(!sender.is_finished());
        let pending = handle.pending_output();
        assert!((MAX_PENDING_OUTPUT..=MAX_PENDING_OUTPUT + CHUNK).contains(&pending), "{pending}");
        // Taking output lets it go on; all of it arrives.
        let mut received = 0;
        while received < CHUNK * CHUNKS {
            match handle.try_recv() {
                Some(SessionEvent::Data(d)) => received += d.len(),
                Some(_) => {}
                None => tokio::time::sleep(std::time::Duration::from_millis(1)).await,
            }
            assert!(handle.pending_output() <= MAX_PENDING_OUTPUT + CHUNK);
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), sender).await.unwrap().unwrap();
        assert_eq!(handle.pending_output(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_closed_tab_does_not_keep_the_connection_waiting() {
        let (handle, _commands, sink) = SessionHandle::new(Arc::new(|| {}));
        sink.data(vec![0; MAX_PENDING_OUTPUT]).await;
        let waiting = tokio::spawn(async move { sink.data(vec![0; 10]).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "full: it waits");
        drop(handle);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting).await.expect("released when the tab closed").unwrap();
    }

    /// Disconnecting while the host key question is still open ends the connecting at once,
    /// instead of after the answer (the command queue is only read once connected).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_stops_a_connection_waiting_on_a_question() {
        let (handle, _commands, sink) = SessionHandle::new(Arc::new(|| {}));
        let params = ConnectParams {
            protocol: Protocol::Ssh,
            host: "example.com".into(),
            port: 22,
            username: String::new(),
            password: None,
            identity_file: None,
            default_identity_files: Vec::new(),
            use_agent: false,
            agent_pipe: None,
            forwards: Vec::new(),
            serial: Default::default(),
            keepalive: Default::default(),
            rdp: Default::default(),
            desktop: (1024, 768),
            term_type: "xterm".into(),
            cols: 80,
            rows: 24,
            known_hosts: Default::default(),
            connect_timeout: Duration::from_secs(60),
            jump: None,
            x11: None,
        };
        let asking = sink.clone();
        let connecting = tokio::spawn(async move {
            sink.connecting(&params, async move {
                let trusted = asking.confirm_host_key("example.com", 22, "ssh-ed25519".into(), "SHA256:x".into()).await;
                Ok(trusted)
            })
            .await
        });
        // The question reaches the UI; nobody answers it.
        let question = loop {
            match handle.try_recv() {
                Some(SessionEvent::HostKeyCheck(q)) => break q,
                Some(_) => {}
                None => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        };
        handle.close();
        let result = tokio::time::timeout(Duration::from_secs(1), connecting).await.expect("stopped at once").unwrap();
        assert!(result.is_err(), "cancelled, not connected: {result:?}");
        drop(question);
    }
}

#[cfg(test)]
mod connect_error_tests {
    use super::connect_error;
    use std::io::{Error, ErrorKind};

    /// The common failures say what to check (#146), and keep the system's message.
    #[test]
    fn common_failures_say_what_to_check() {
        playterm_i18n::set_english(false);
        let refused = connect_error("127.0.0.1", 22, Error::from(ErrorKind::ConnectionRefused)).to_string();
        assert!(refused.contains("포트 22 에서 접속을 받지 않습니다"), "{refused}");
        assert!(refused.starts_with("127.0.0.1:22 에 연결할 수 없습니다"), "{refused}");
        let unknown = connect_error("nosuch.example", 22, Error::from_raw_os_error(11001)).to_string();
        assert!(unknown.contains("'nosuch.example' 라는 호스트를 찾을 수 없습니다"), "{unknown}");
        let other = connect_error("h", 1, Error::other("odd")).to_string();
        assert_eq!(other, "h:1 에 연결할 수 없습니다", "anything else keeps the plain head (the cause follows as context)");
    }
}
