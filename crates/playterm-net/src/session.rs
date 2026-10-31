use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, mpsc as std_mpsc};

use tokio::sync::{mpsc, oneshot};

use crate::forward::ForwardRule;
use crate::keepalive::KeepAlive;
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
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [Protocol::Ssh, Protocol::Telnet, Protocol::Serial];

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh => 22,
            Protocol::Telnet => 23,
            Protocol::Serial => 0,
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Protocol::Ssh => "SSH",
            Protocol::Telnet => "Telnet",
            Protocol::Serial => "Serial",
        })
    }
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
    /// Offer keys from running SSH agents (Windows OpenSSH agent, Pageant).
    pub use_agent: bool,
    /// Agent named pipe to use instead of `SSH_AUTH_SOCK` / the OpenSSH service pipe.
    /// When set, Pageant is not consulted.
    pub agent_pipe: Option<String>,
    /// Port forwardings started once the SSH session is authenticated.
    pub forwards: Vec<ForwardRule>,
    /// Line settings for [`Protocol::Serial`].
    pub serial: SerialSettings,
    /// Keeps the connection from timing out while idle.
    pub keepalive: KeepAlive,
    /// Value for the `TERM` variable / Telnet terminal type.
    pub term_type: String,
    pub cols: u16,
    pub rows: u16,
    /// known_hosts file used to verify SSH host keys.
    pub known_hosts: PathBuf,
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
            .field("term_type", &self.term_type)
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
    /// Session ended. `Some` carries an error description.
    Closed(Option<String>),
}

#[derive(Debug)]
pub struct HostKeyCheck {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
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
    Resize { cols: u16, rows: u16 },
    OpenSftp,
    /// Serial BREAK / Telnet IAC BRK.
    SendBreak,
    Close,
}

/// UI-side handle to a running session. Dropping it closes the session.
pub struct SessionHandle {
    commands: mpsc::UnboundedSender<SessionCommand>,
    events: std_mpsc::Receiver<SessionEvent>,
}

impl SessionHandle {
    pub(crate) fn new(waker: Waker) -> (Self, mpsc::UnboundedReceiver<SessionCommand>, EventSink) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = std_mpsc::channel();
        (Self { commands: cmd_tx, events: ev_rx }, cmd_rx, EventSink { tx: ev_tx, waker })
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
    }

    /// Sends a BREAK signal (serial line break, Telnet `IAC BRK`).
    pub fn send_break(&self) {
        let _ = self.commands.send(SessionCommand::SendBreak);
    }

    /// Opens an SFTP channel on this SSH connection; answered with [`SessionEvent::Sftp`].
    pub fn open_sftp(&self) {
        let _ = self.commands.send(SessionCommand::OpenSftp);
    }

    pub fn try_recv(&self) -> Option<SessionEvent> {
        self.events.try_recv().ok()
    }
}

#[derive(Clone)]
pub(crate) struct EventSink {
    tx: std_mpsc::Sender<SessionEvent>,
    waker: Waker,
}

impl EventSink {
    pub fn send(&self, event: SessionEvent) {
        let _ = self.tx.send(event);
        (self.waker)();
    }

    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    pub fn notice(&self, text: impl Into<String>) {
        self.send(SessionEvent::Notice(text.into()));
    }

    /// Asks the user whether to trust an unknown host key. A closed UI counts as "no".
    pub async fn confirm_host_key(
        &self,
        host: &str,
        port: u16,
        algorithm: String,
        fingerprint: String,
    ) -> bool {
        let (reply, rx) = oneshot::channel();
        self.send(SessionEvent::HostKeyCheck(HostKeyCheck {
            host: host.to_owned(),
            port,
            algorithm,
            fingerprint,
            reply,
        }));
        rx.await.unwrap_or(false)
    }

    /// Asks the user for credentials. `None` means cancelled.
    pub async fn prompt(
        &self,
        title: String,
        instructions: String,
        fields: Vec<PromptField>,
    ) -> Option<Vec<String>> {
        let (reply, rx) = oneshot::channel();
        self.send(SessionEvent::AuthPrompt(AuthPrompt { title, instructions, fields, reply }));
        rx.await.ok().flatten()
    }
}
