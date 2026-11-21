//! Telnet client (RFC 854) with the options a terminal needs:
//! BINARY (856), ECHO (857), SGA (858), TTYPE (1091), NAWS (1073).
//!
//! Negotiation is passive: we never start negotiating, so connecting to a raw TCP service
//! that does not speak Telnet does not inject IAC bytes into its stream.

use anyhow::Context as _;
use playterm_i18n::t;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

trait TerminalStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> TerminalStream for T {}

pub(crate) async fn run(params: ConnectParams, commands: mpsc::UnboundedReceiver<SessionCommand>, sink: EventSink) -> anyhow::Result<()> {
    run_with_tls(params, commands, sink, tokio_native_tls::native_tls::TlsConnector::new).await
}

async fn run_with_tls(
    params: ConnectParams,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    sink: EventSink,
    tls_connector: impl FnOnce() -> Result<tokio_native_tls::native_tls::TlsConnector, tokio_native_tls::native_tls::Error>,
) -> anyhow::Result<()> {
    let telnet = matches!(params.protocol, crate::Protocol::Telnet | crate::Protocol::TelnetTls);
    let stream = sink
        .connecting(&params, async {
            let mut tcp = crate::proxy::connect(&params, &sink).await?;
            tcp.set_nodelay(true)?;
            if params.protocol == crate::Protocol::TelnetTls {
                let tls = tls_connector()?;
                let stream = tokio_native_tls::TlsConnector::from(tls).connect(&params.host, tcp).await?;
                return Ok(Box::new(stream) as Box<dyn TerminalStream>);
            }
            if params.protocol == crate::Protocol::Rlogin {
                let local = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
                let remote = if params.username.is_empty() { &local } else { &params.username };
                anyhow::ensure!(![local.as_str(), remote, &params.term_type].iter().any(|s| s.contains('\0')), "Invalid rlogin field");
                tcp.write_all(format!("\0{local}\0{remote}\0{}/38400\0", params.term_type).as_bytes()).await?;
                anyhow::ensure!(tcp.read_u8().await? == 0, "rlogin server rejected login");
            }
            Ok(Box::new(tcp) as Box<dyn TerminalStream>)
        })
        .await?;
    sink.send(SessionEvent::Connected);

    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut codec = TelnetCodec::new(&params.term_type, params.cols, params.rows);
    let mut buf = vec![0u8; 16 * 1024];
    let mut idle = IdleTimer::new(&params.keepalive, telnet, &sink);

    loop {
        tokio::select! {
            n = reader.read(&mut buf) => {
                let n = n.context(t!("common.receive_error"))?;
                if n == 0 {
                    return Ok(());
                }
                if !telnet { sink.data(buf[..n].to_vec()).await; continue; }
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
                    if telnet { writer.write_all(&codec.encode_input(&data)).await?; } else { writer.write_all(&data).await?; }
                    idle.touch();
                }
                Some(SessionCommand::Resize { cols, rows }) => {
                    if telnet && let Some(naws) = codec.resize(cols, rows) {
                        writer.write_all(&naws).await?;
                    }
                }
                Some(SessionCommand::SendBreak) => {
                    if !telnet { continue; }
                    writer.write_all(&[IAC, BRK]).await?;
                    sink.notice(t!("common.sent_break"));
                }
                Some(SessionCommand::Rdp(_)) => {}
                Some(SessionCommand::ShellDir) => sink.send(SessionEvent::ShellDir(None)),
                Some(SessionCommand::OpenSftp) => {
                    sink.send(SessionEvent::Sftp(Err(t!("telnet.sftp_is_not_available_in_telnet_sessions")
                    .into())));
                }
                Some(SessionCommand::Close) | None => {
                    let _ = writer.shutdown().await;
                    return Ok(());
                }
            },
            _ = sleep_until(idle.deadline()) => {
                let bytes = match &idle.text {
                    Some(text) if telnet => codec.encode_input(text),
                    Some(text) => text.clone(),
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

    fn test_params(port: u16) -> ConnectParams {
        ConnectParams {
            protocol: crate::Protocol::Telnet,
            proxy: String::new(),
            proxy_username: String::new(),
            proxy_remote_dns: true,
            host: "127.0.0.1".into(),
            port,
            username: String::new(),
            password: None,
            identity_file: None,
            default_identity_files: Vec::new(),
            use_agent: false,
            ssh_legacy_algorithms: false,
            certificate_file: None,
            agent_forwarding: false,
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
            connect_timeout: ConnectParams::DEFAULT_CONNECT_TIMEOUT,
            jump: None,
            x11: None,
        }
    }

    async fn next_event(handle: &crate::session::SessionHandle) -> SessionEvent {
        loop {
            if let Some(event) = handle.try_recv() {
                return event;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    async fn check_terminal(handle: &crate::session::SessionHandle) {
        assert!(matches!(next_event(handle).await, SessionEvent::Connected));
        handle.send(b"ping".to_vec());
        let mut output = Vec::new();
        while output.len() < 4 {
            let SessionEvent::Data(bytes) = next_event(handle).await else { panic!("expected terminal output") };
            output.extend(bytes);
        }
        assert_eq!(output, b"pong");
        handle.close();
    }

    #[tokio::test]
    async fn rlogin_over_authenticated_connect_relay() {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let device = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let destination = device.local_addr().unwrap();
            let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut p = test_params(destination.port());
            p.protocol = crate::Protocol::Rlogin;
            p.username = "remote".into();
            p.proxy = format!("http://{}", proxy.local_addr().unwrap());
            p.proxy_username = "test".into();
            let relay = tokio::spawn(async move {
                let (mut client, _) = proxy.accept().await.unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    header.push(client.read_u8().await.unwrap());
                }
                let header = String::from_utf8(header).unwrap();
                assert!(header.starts_with(&format!("CONNECT {destination} HTTP/1.1\r\n")));
                assert!(header.contains("\r\nProxy-Authorization: Basic dGVzdDpwYXNz\r\n"));
                let mut upstream = tokio::net::TcpStream::connect(destination).await.unwrap();
                client.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                tokio::io::copy_bidirectional(&mut client, &mut upstream).await.unwrap();
            });
            let server = tokio::spawn(async move {
                let (mut socket, _) = device.accept().await.unwrap();
                let mut login = Vec::new();
                while login.iter().filter(|&&b| b == 0).count() < 4 {
                    login.push(socket.read_u8().await.unwrap());
                }
                assert!(login.ends_with(b"\0remote\0xterm/38400\0"));
                socket.write_u8(0).await.unwrap();
                let mut data = [0; 4];
                socket.read_exact(&mut data).await.unwrap();
                assert_eq!(&data, b"ping");
                socket.write_all(b"pong").await.unwrap();
            });
            let (handle, commands, sink) = crate::session::SessionHandle::new(std::sync::Arc::new(|| {}));
            let client = tokio::spawn(run(p, commands, sink));
            let SessionEvent::AuthPrompt(prompt) = next_event(&handle).await else { panic!("expected proxy prompt") };
            prompt.answer(Some(vec!["pass".into()]));
            check_terminal(&handle).await;
            client.await.unwrap().unwrap();
            server.await.unwrap();
            relay.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn telnet_over_trusted_local_tls_server() {
        use tokio_rustls::rustls;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let identity = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let root = tokio_native_tls::native_tls::Certificate::from_der(identity.cert.der()).unwrap();
            let key = rustls::pki_types::PrivateKeyDer::Pkcs8(identity.signing_key.serialize_der().into());
            let config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![identity.cert.der().clone()], key)
                .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut p = test_params(listener.local_addr().unwrap().port());
            p.protocol = crate::Protocol::TelnetTls;
            p.host = "localhost".into();
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut tls = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config)).accept(tcp).await.unwrap();
                let mut data = [0; 4];
                tls.read_exact(&mut data).await.unwrap();
                assert_eq!(&data, b"ping");
                tls.write_all(b"pong").await.unwrap();
                tls.shutdown().await.unwrap();
            });
            let (handle, commands, sink) = crate::session::SessionHandle::new(std::sync::Arc::new(|| {}));
            let client = tokio::spawn(run_with_tls(p, commands, sink, move || {
                tokio_native_tls::native_tls::TlsConnector::builder().add_root_certificate(root).build()
            }));
            check_terminal(&handle).await;
            client.await.unwrap().unwrap();
            server.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn raw_over_http_proxy_preserves_bytes_and_remote_hostname() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut p = test_params(5555);
        p.protocol = crate::Protocol::Raw;
        p.host = "device.invalid".into();
        p.proxy = format!("http://{}", listener.local_addr().unwrap());
        let (handle, commands, sink) = crate::session::SessionHandle::new(std::sync::Arc::new(|| {}));
        let task = tokio::spawn(run(p, commands, sink));
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(tcp.read_u8().await.unwrap());
        }
        assert!(header.starts_with(b"CONNECT device.invalid:5555 HTTP/1.1\r\n"));
        tcp.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        handle.send(vec![255, 13, 0]);
        let mut data = [0; 3];
        tcp.read_exact(&mut data).await.unwrap();
        assert_eq!(data, [255, 13, 0]);
        handle.close();
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn socks_proxies_receive_remote_hostname() {
        for scheme in ["socks4", "socks5"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut p = test_params(23);
            p.protocol = crate::Protocol::Raw;
            p.host = "router.invalid".into();
            p.proxy = format!("{scheme}://{}", listener.local_addr().unwrap());
            let (handle, commands, sink) = crate::session::SessionHandle::new(std::sync::Arc::new(|| {}));
            let task = tokio::spawn(run(p, commands, sink));
            let (mut tcp, _) = listener.accept().await.unwrap();
            if scheme == "socks5" {
                let mut hello = [0; 3];
                tcp.read_exact(&mut hello).await.unwrap();
                assert_eq!(hello, [5, 1, 0]);
                tcp.write_all(&[5, 0]).await.unwrap();
                let mut connect = vec![0; 7 + b"router.invalid".len()];
                tcp.read_exact(&mut connect).await.unwrap();
                assert_eq!(&connect[5..connect.len() - 2], b"router.invalid");
                tcp.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 23]).await.unwrap();
            } else {
                let mut connect = vec![0; 10 + b"router.invalid".len()];
                tcp.read_exact(&mut connect).await.unwrap();
                assert_eq!(&connect[9..connect.len() - 1], b"router.invalid");
                tcp.write_all(&[0, 90, 0, 23, 127, 0, 0, 1]).await.unwrap();
            }
            handle.send(vec![42]);
            assert_eq!(tcp.read_u8().await.unwrap(), 42);
            handle.close();
            task.await.unwrap().unwrap();
        }
    }
    #[tokio::test]
    async fn tls_rejects_plaintext_and_rlogin_requires_ack() {
        for protocol in [crate::Protocol::TelnetTls, crate::Protocol::Rlogin] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut p = test_params(listener.local_addr().unwrap().port());
            p.protocol = protocol;
            p.username = "remote".into();
            let (_handle, commands, sink) = crate::session::SessionHandle::new(std::sync::Arc::new(|| {}));
            let task = tokio::spawn(run(p, commands, sink));
            let (mut tcp, _) = listener.accept().await.unwrap();
            if protocol == crate::Protocol::Rlogin {
                let mut handshake = Vec::new();
                while handshake.iter().filter(|&&b| b == 0).count() < 4 {
                    handshake.push(tcp.read_u8().await.unwrap());
                }
                assert!(handshake.ends_with(b"\0remote\0xterm/38400\0"));
                tcp.write_all(&[1]).await.unwrap();
            } else {
                tcp.write_all(b"plaintext login prompt\r\n").await.unwrap();
            }
            drop(tcp);
            assert!(task.await.unwrap().is_err());
        }
    }
    #[tokio::test]
    async fn keepalive_nop_and_text() {
        use std::sync::Arc;

        use crate::keepalive::{KeepAlive, KeepAliveMode};
        use crate::session::SessionHandle;

        for (mode, expected) in [(KeepAliveMode::Protocol, vec![IAC, NOP]), (KeepAliveMode::Text, b"\r\n".to_vec())] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let params = ConnectParams { keepalive: KeepAlive { mode, interval: 1, text: r"\r".into() }, ..test_params(port) };
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
