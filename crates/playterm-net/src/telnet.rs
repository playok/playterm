//! Telnet client (RFC 854) with the options a terminal needs:
//! BINARY (856), ECHO (857), SGA (858), TTYPE (1091), NAWS (1073).
//!
//! Negotiation is passive: we never start negotiating, so connecting to a raw TCP service
//! that does not speak Telnet does not inject IAC bytes into its stream.

use anyhow::Context as _;
use playterm_i18n::t;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::keepalive::{IdleTimer, sleep_until};
use crate::session::{ConnectParams, EventSink, SessionCommand, SessionEvent};

const SE: u8 = 240;
const NOP: u8 = 241;
const BRK: u8 = 243;
const SB: u8 = 250;
const WILL: u8 = 251;
const WONT: u8 = 252;
const DO: u8 = 253;
const DONT: u8 = 254;
const IAC: u8 = 255;

const OPT_BINARY: u8 = 0;
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_TTYPE: u8 = 24;
const OPT_NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// Upper bound for a subnegotiation payload; anything longer is garbage and is truncated.
const MAX_SB_LEN: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Data,
    Iac,
    Verb(u8),
    Sub,
    SubIac,
}

/// Bytes produced by [`TelnetCodec::receive`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Received {
    /// Terminal data with all Telnet commands removed.
    pub data: Vec<u8>,
    /// Negotiation replies to send back to the server.
    pub reply: Vec<u8>,
}

/// Sans-IO Telnet protocol state machine.
pub struct TelnetCodec {
    state: State,
    sub: Vec<u8>,
    last_was_cr: bool,
    /// Options we perform (we said WILL, server said DO).
    local: [bool; 256],
    /// Options the server performs (server said WILL, we said DO).
    remote: [bool; 256],
    term_type: String,
    cols: u16,
    rows: u16,
}

impl TelnetCodec {
    pub fn new(term_type: &str, cols: u16, rows: u16) -> Self {
        Self {
            state: State::Data,
            sub: Vec::new(),
            last_was_cr: false,
            local: [false; 256],
            remote: [false; 256],
            term_type: term_type.to_owned(),
            cols,
            rows,
        }
    }

    pub fn receive(&mut self, input: &[u8]) -> Received {
        let mut out = Received::default();
        for &b in input {
            self.state = match self.state {
                State::Data if b == IAC => State::Iac,
                State::Data => {
                    // In NVT mode a bare carriage return is sent as CR NUL; drop the NUL.
                    let skip = self.last_was_cr && b == 0 && !self.remote[OPT_BINARY as usize];
                    self.last_was_cr = b == b'\r';
                    if !skip {
                        out.data.push(b);
                    }
                    State::Data
                }
                State::Iac => match b {
                    IAC => {
                        out.data.push(IAC);
                        self.last_was_cr = false;
                        State::Data
                    }
                    WILL | WONT | DO | DONT => State::Verb(b),
                    SB => {
                        self.sub.clear();
                        State::Sub
                    }
                    // NOP, GA, AYT, ... carry no data for us.
                    _ => State::Data,
                },
                State::Verb(verb) => {
                    self.negotiate(verb, b, &mut out.reply);
                    State::Data
                }
                State::Sub if b == IAC => State::SubIac,
                State::Sub => {
                    if self.sub.len() < MAX_SB_LEN {
                        self.sub.push(b);
                    }
                    State::Sub
                }
                State::SubIac => match b {
                    SE => {
                        self.subnegotiation(&mut out.reply);
                        State::Data
                    }
                    IAC => {
                        if self.sub.len() < MAX_SB_LEN {
                            self.sub.push(IAC);
                        }
                        State::Sub
                    }
                    _ => State::Data,
                },
            };
        }
        out
    }

    /// Escapes user input for the wire. Return (CR) is sent as CR LF, the Telnet newline,
    /// unless we negotiated binary transmission.
    pub fn encode_input(&self, input: &[u8]) -> Vec<u8> {
        let binary = self.local[OPT_BINARY as usize];
        let mut out = Vec::with_capacity(input.len() + 4);
        for (i, &b) in input.iter().enumerate() {
            match b {
                IAC => out.extend_from_slice(&[IAC, IAC]),
                b'\r' if !binary => {
                    out.push(b'\r');
                    if input.get(i + 1) != Some(&b'\n') {
                        out.push(b'\n');
                    }
                }
                _ => out.push(b),
            }
        }
        out
    }

    /// Records the new window size; returns a NAWS update if the server asked for them.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Option<Vec<u8>> {
        self.cols = cols;
        self.rows = rows;
        if self.local[OPT_NAWS as usize] {
            let mut out = Vec::new();
            self.write_naws(&mut out);
            Some(out)
        } else {
            None
        }
    }

    /// Whether the server echoes our input (i.e. we should not echo locally).
    pub fn remote_echo(&self) -> bool {
        self.remote[OPT_ECHO as usize]
    }

    fn negotiate(&mut self, verb: u8, opt: u8, reply: &mut Vec<u8>) {
        let i = opt as usize;
        match verb {
            WILL => {
                if matches!(opt, OPT_BINARY | OPT_ECHO | OPT_SGA) {
                    if !self.remote[i] {
                        self.remote[i] = true;
                        reply.extend_from_slice(&[IAC, DO, opt]);
                    }
                } else {
                    reply.extend_from_slice(&[IAC, DONT, opt]);
                }
            }
            WONT => {
                if self.remote[i] {
                    self.remote[i] = false;
                    reply.extend_from_slice(&[IAC, DONT, opt]);
                }
            }
            DO => {
                if matches!(opt, OPT_BINARY | OPT_SGA | OPT_TTYPE | OPT_NAWS) {
                    if !self.local[i] {
                        self.local[i] = true;
                        reply.extend_from_slice(&[IAC, WILL, opt]);
                    }
                    if opt == OPT_NAWS {
                        self.write_naws(reply);
                    }
                } else {
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                }
            }
            DONT => {
                if self.local[i] {
                    self.local[i] = false;
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                }
            }
            _ => unreachable!("only called with negotiation verbs"),
        }
    }

    fn subnegotiation(&mut self, reply: &mut Vec<u8>) {
        if self.sub.as_slice() == [OPT_TTYPE, TTYPE_SEND] && self.local[OPT_TTYPE as usize] {
            reply.extend_from_slice(&[IAC, SB, OPT_TTYPE, TTYPE_IS]);
            for &b in self.term_type.as_bytes() {
                push_escaped(reply, b);
            }
            reply.extend_from_slice(&[IAC, SE]);
        }
    }

    fn write_naws(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[IAC, SB, OPT_NAWS]);
        for b in self.cols.to_be_bytes().into_iter().chain(self.rows.to_be_bytes()) {
            push_escaped(out, b);
        }
        out.extend_from_slice(&[IAC, SE]);
    }
}

fn push_escaped(out: &mut Vec<u8>, b: u8) {
    if b == IAC {
        out.push(IAC);
    }
    out.push(b);
}

pub(crate) async fn run(
    params: ConnectParams,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    sink: EventSink,
) -> anyhow::Result<()> {
    let stream = sink
        .connecting(&params, async {
            TcpStream::connect((params.host.as_str(), params.port))
                .await
                .map_err(|e| crate::session::connect_error(&params.host, params.port, e))
        })
        .await?;
    stream.set_nodelay(true)?;
    sink.send(SessionEvent::Connected);

    let (mut reader, mut writer) = stream.into_split();
    let mut codec = TelnetCodec::new(&params.term_type, params.cols, params.rows);
    let mut buf = vec![0u8; 16 * 1024];
    let mut idle = IdleTimer::new(&params.keepalive, true, &sink);

    loop {
        tokio::select! {
            n = reader.read(&mut buf) => {
                let n = n.context(t("수신 오류", "Receive error"))?;
                if n == 0 {
                    return Ok(());
                }
                let received = codec.receive(&buf[..n]);
                if !received.reply.is_empty() {
                    writer.write_all(&received.reply).await?;
                }
                if !received.data.is_empty() {
                    sink.data(received.data).await;
                }
            }
            cmd = commands.recv() => match cmd {
                Some(SessionCommand::Input(data)) => {
                    writer.write_all(&codec.encode_input(&data)).await?;
                    idle.touch();
                }
                Some(SessionCommand::Resize { cols, rows }) => {
                    if let Some(naws) = codec.resize(cols, rows) {
                        writer.write_all(&naws).await?;
                    }
                }
                Some(SessionCommand::SendBreak) => {
                    writer.write_all(&[IAC, BRK]).await?;
                    sink.notice(t("BREAK 신호를 보냈습니다", "Sent BREAK"));
                }
                Some(SessionCommand::Rdp(_)) => {}
                Some(SessionCommand::ShellDir) => sink.send(SessionEvent::ShellDir(None)),
                Some(SessionCommand::OpenSftp) => {
                    sink.send(SessionEvent::Sftp(Err(t(
                        "Telnet 세션에서는 SFTP를 쓸 수 없습니다",
                        "SFTP is not available in Telnet sessions",
                    )
                    .into())));
                }
                Some(SessionCommand::Close) | None => {
                    let _ = writer.shutdown().await;
                    return Ok(());
                }
            },
            _ = sleep_until(idle.deadline()) => {
                let bytes = match &idle.text {
                    Some(text) => codec.encode_input(text),
                    None => vec![IAC, NOP],
                };
                writer.write_all(&bytes).await?;
                idle.touch();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_data_passes_through() {
        let mut c = TelnetCodec::new("xterm", 80, 24);
        let r = c.receive(b"login: ");
        assert_eq!(r.data, b"login: ");
        assert!(r.reply.is_empty());
    }

    #[test]
    fn accepts_echo_and_sga_refuses_unknown() {
        let mut c = TelnetCodec::new("xterm", 80, 24);
        let r = c.receive(&[IAC, WILL, OPT_ECHO, IAC, WILL, OPT_SGA, IAC, WILL, 99, b'x']);
        assert_eq!(r.data, b"x");
        assert_eq!(r.reply, vec![IAC, DO, OPT_ECHO, IAC, DO, OPT_SGA, IAC, DONT, 99]);
        assert!(c.remote_echo());

        // Repeated WILL for an enabled option must not be acknowledged again (no loops).
        assert!(c.receive(&[IAC, WILL, OPT_ECHO]).reply.is_empty());
    }

    #[test]
    fn naws_sent_on_do_and_resize() {
        let mut c = TelnetCodec::new("xterm", 80, 24);
        assert_eq!(c.resize(100, 30), None);
        let r = c.receive(&[IAC, DO, OPT_NAWS]);
        assert_eq!(r.reply, vec![IAC, WILL, OPT_NAWS, IAC, SB, OPT_NAWS, 0, 100, 0, 30, IAC, SE]);
        // A dimension byte of 255 must be doubled.
        assert_eq!(c.resize(255, 24), Some(vec![IAC, SB, OPT_NAWS, 0, IAC, IAC, 0, 24, IAC, SE]));
    }

    #[test]
    fn terminal_type_subnegotiation() {
        let mut c = TelnetCodec::new("xterm-256color", 80, 24);
        let r = c.receive(&[IAC, DO, OPT_TTYPE, IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, SE]);
        let mut expected = vec![IAC, WILL, OPT_TTYPE, IAC, SB, OPT_TTYPE, TTYPE_IS];
        expected.extend_from_slice(b"xterm-256color");
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(r.reply, expected);
    }

    #[test]
    fn commands_split_across_reads() {
        let mut c = TelnetCodec::new("xterm", 80, 24);
        let a = c.receive(&[b'a', IAC]);
        let b = c.receive(&[WILL]);
        let d = c.receive(&[OPT_ECHO, b'b']);
        assert_eq!([a.data, b.data, d.data].concat(), b"ab");
        assert_eq!(d.reply, vec![IAC, DO, OPT_ECHO]);
    }

    #[test]
    fn iac_escaping_and_cr_nul() {
        let mut c = TelnetCodec::new("xterm", 80, 24);
        assert_eq!(c.receive(&[b'x', IAC, IAC, b'\r', 0, b'y']).data, vec![b'x', IAC, b'\r', b'y']);
        assert_eq!(c.encode_input(&[IAC, b'\r']), vec![IAC, IAC, b'\r', b'\n']);
        assert_eq!(c.encode_input(b"a\r\nb"), b"a\r\nb");
    }

    #[tokio::test]
    async fn keepalive_nop_and_text() {
        use std::sync::Arc;

        use crate::keepalive::{KeepAlive, KeepAliveMode};
        use crate::session::SessionHandle;

        for (mode, expected) in [(KeepAliveMode::Protocol, vec![IAC, NOP]), (KeepAliveMode::Text, b"\r\n".to_vec())] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let params = ConnectParams {
                protocol: crate::Protocol::Telnet,
                host: "127.0.0.1".into(),
                port,
                username: String::new(),
                password: None,
                identity_file: None,
                default_identity_files: Vec::new(),
                use_agent: false,
                ssh_legacy_algorithms: false,
                agent_pipe: None,
                forwards: Vec::new(),
                serial: Default::default(),
                keepalive: KeepAlive { mode, interval: 1, text: "\\r".into() },
                rdp: Default::default(),
                desktop: (1024, 768),
                term_type: "xterm".into(),
                cols: 80,
                rows: 24,
                known_hosts: Default::default(),
                connect_timeout: ConnectParams::DEFAULT_CONNECT_TIMEOUT,
                jump: None,
                x11: None,
            };
            let (handle, commands, sink) = SessionHandle::new(Arc::new(|| {}));
            let task = tokio::spawn(run(params, commands, sink));
            let (mut server, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; expected.len()];
            server.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, expected, "{mode:?}");
            handle.close();
            task.await.unwrap().unwrap();
        }
    }

    #[test]
    fn binary_mode_sends_raw_cr() {
        let mut c = TelnetCodec::new("xterm", 80, 24);
        c.receive(&[IAC, DO, OPT_BINARY]);
        assert_eq!(c.encode_input(b"\r"), b"\r");
    }
}
