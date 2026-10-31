//! SSH port forwarding: local (-L), remote (-R) and dynamic SOCKS5 (-D).

use std::fmt;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use playterm_i18n::{t, tr};
use russh::client;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use crate::session::EventSink;
use crate::ssh::ClientHandler;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    /// Listen locally, connect from the server (`ssh -L`).
    #[default]
    Local,
    /// Listen on the server, connect from here (`ssh -R`).
    Remote,
    /// Local SOCKS5 proxy, connections made from the server (`ssh -D`).
    Dynamic,
}

impl ForwardKind {
    pub const ALL: [ForwardKind; 3] = [ForwardKind::Local, ForwardKind::Remote, ForwardKind::Dynamic];

    pub fn letter(self) -> char {
        match self {
            ForwardKind::Local => 'L',
            ForwardKind::Remote => 'R',
            ForwardKind::Dynamic => 'D',
        }
    }
}

impl fmt::Display for ForwardKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ForwardKind::Local => t("로컬 (L)", "Local (L)"),
            ForwardKind::Remote => t("원격 (R)", "Remote (R)"),
            ForwardKind::Dynamic => t("동적 SOCKS (D)", "Dynamic SOCKS (D)"),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForwardRule {
    pub kind: ForwardKind,
    /// Listening address; empty = loopback (L/D) or the server's default (R).
    pub bind_host: String,
    pub bind_port: u16,
    /// Target; unused for dynamic forwarding.
    pub dest_host: String,
    pub dest_port: u16,
}

impl ForwardRule {
    /// Parses OpenSSH syntax: `[bind:]port:host:hostport` for L/R, `[bind:]port` for D.
    /// IPv6 addresses go in brackets.
    pub fn parse(kind: ForwardKind, spec: &str) -> Result<Self, String> {
        let parts = split_colons(spec);
        let port = |s: &str| s.parse::<u16>().map_err(|_| tr!("잘못된 포트 '{s}': {spec}", "Invalid port '{s}': {spec}"));
        let (bind, rest): (&str, &[String]) = match (kind, parts.len()) {
            (ForwardKind::Dynamic, 1) | (_, 3) => ("", &parts[..]),
            (ForwardKind::Dynamic, 2) | (_, 4) => (&parts[0], &parts[1..]),
            _ => return Err(tr!("형식이 잘못되었습니다: {spec}", "Invalid format: {spec}")),
        };
        let mut rule = ForwardRule { kind, bind_host: bind.to_owned(), bind_port: port(&rest[0])?, ..Default::default() };
        if kind != ForwardKind::Dynamic {
            rule.dest_host = rest[1].clone();
            rule.dest_port = port(&rest[2])?;
            if rule.dest_host.is_empty() {
                return Err(tr!("대상 호스트가 없습니다: {spec}", "Missing destination host: {spec}"));
            }
        }
        Ok(rule)
    }

    /// Local listening address for L/D rules.
    fn local_bind(&self) -> &str {
        if self.bind_host.is_empty() { "127.0.0.1" } else { &self.bind_host }
    }
}

impl fmt::Display for ForwardRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bind = match self.kind {
            ForwardKind::Remote if self.bind_host.is_empty() => tr!("(서버):{}", "(server):{}", self.bind_port),
            ForwardKind::Remote => tr!("(서버){}:{}", "(server){}:{}", self.bind_host, self.bind_port),
            _ => format!("{}:{}", self.local_bind(), self.bind_port),
        };
        match self.kind {
            ForwardKind::Dynamic => write!(f, "D {bind} (SOCKS5)"),
            k => write!(f, "{} {bind} → {}:{}", k.letter(), self.dest_host, self.dest_port),
        }
    }
}

/// Splits on ':' outside `[...]`, removing the brackets.
fn split_colons(spec: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut in_brackets = false;
    for c in spec.chars() {
        match c {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            ':' if !in_brackets => parts.push(String::new()),
            c => parts.last_mut().expect("never empty").push(c),
        }
    }
    parts
}

// -------------------------------------------------------------------------------------------------
// Runtime

type Session = Arc<client::Handle<ClientHandler>>;

/// Starts every rule. Listener tasks live in `tasks` and stop when it is dropped/aborted.
pub(crate) async fn start(session: &Session, rules: &[ForwardRule], sink: &EventSink, tasks: &mut JoinSet<()>) {
    for rule in rules {
        match rule.kind {
            ForwardKind::Local | ForwardKind::Dynamic => {
                let addr = (rule.local_bind().to_owned(), rule.bind_port);
                match TcpListener::bind(addr).await {
                    Ok(listener) => {
                        sink.notice(tr!("포워딩 시작: {rule}", "Port forwarding started: {rule}"));
                        tasks.spawn(accept_loop(listener, rule.clone(), session.clone(), sink.clone()));
                    }
                    Err(e) => sink.notice(tr!("포워딩 실패 ({rule}): {e}", "Port forwarding failed ({rule}): {e}")),
                }
            }
            ForwardKind::Remote => match session.tcpip_forward(rule.bind_host.clone(), rule.bind_port.into()).await {
                Ok(_) => sink.notice(tr!("포워딩 시작: {rule}", "Port forwarding started: {rule}")),
                Err(e) => sink.notice(tr!(
                    "포워딩 실패 ({rule}): 서버가 거부했습니다 ({e})",
                    "Port forwarding failed ({rule}): refused by the server ({e})"
                )),
            },
        }
    }
}

async fn accept_loop(listener: TcpListener, rule: ForwardRule, session: Session, sink: EventSink) {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                log::warn!("forward accept failed: {e}");
                continue;
            }
        };
        let (rule, session, sink) = (rule.clone(), session.clone(), sink.clone());
        tokio::spawn(async move {
            if let Err(e) = handle_local(tcp, peer, &rule, &session).await {
                sink.notice(tr!("포워딩 연결 실패 ({rule}): {e:#}", "Forwarded connection failed ({rule}): {e:#}"));
            }
        });
    }
}

async fn handle_local(
    mut tcp: TcpStream,
    peer: std::net::SocketAddr,
    rule: &ForwardRule,
    session: &Session,
) -> anyhow::Result<()> {
    let (host, port) = match rule.kind {
        ForwardKind::Dynamic => socks5_accept(&mut tcp).await?,
        _ => (rule.dest_host.clone(), rule.dest_port),
    };
    let opened = session
        .channel_open_direct_tcpip(host.clone(), port.into(), peer.ip().to_string(), peer.port().into())
        .await;
    if rule.kind == ForwardKind::Dynamic {
        socks5_reply(&mut tcp, opened.is_ok()).await?;
    }
    let channel = opened.with_context(|| tr!("{host}:{port} 에 연결할 수 없습니다", "Cannot connect to {host}:{port}"))?;
    let mut stream = channel.into_stream();
    tokio::io::copy_bidirectional(&mut tcp, &mut stream).await?;
    Ok(())
}

/// Serves a server-initiated `forwarded-tcpip` channel for a remote rule.
pub(crate) async fn handle_remote(channel: russh::Channel<client::Msg>, rule: ForwardRule) -> anyhow::Result<()> {
    let mut tcp = TcpStream::connect((rule.dest_host.as_str(), rule.dest_port))
        .await
        .with_context(|| {
            tr!("{}:{} 에 연결할 수 없습니다", "Cannot connect to {}:{}", rule.dest_host, rule.dest_port)
        })?;
    let mut stream = channel.into_stream();
    tokio::io::copy_bidirectional(&mut tcp, &mut stream).await?;
    Ok(())
}

// -------------------------------------------------------------------------------------------------
// SOCKS5 (RFC 1928), CONNECT only, no authentication

const SOCKS_VERSION: u8 = 5;

/// Reads the greeting and CONNECT request; returns the requested destination.
async fn socks5_accept<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S) -> anyhow::Result<(String, u16)> {
    let [version, n_methods] = read_array(s).await?;
    if version != SOCKS_VERSION {
        bail!(tr!("SOCKS5 클라이언트가 아닙니다 (버전 {version})", "Not a SOCKS5 client (version {version})"));
    }
    let mut methods = vec![0u8; n_methods.into()];
    s.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        s.write_all(&[SOCKS_VERSION, 0xFF]).await?;
        bail!(t(
            "SOCKS5: 인증 없는 연결을 지원하지 않는 클라이언트",
            "SOCKS5: the client does not support connecting without authentication"
        ));
    }
    s.write_all(&[SOCKS_VERSION, 0]).await?;

    let [_, command, _, address_type] = read_array(s).await?;
    let host = match address_type {
        1 => std::net::Ipv4Addr::from(read_array::<4, _>(s).await?).to_string(),
        3 => {
            let [len] = read_array(s).await?;
            let mut name = vec![0u8; len.into()];
            s.read_exact(&mut name).await?;
            String::from_utf8(name).context(t("SOCKS5: 잘못된 호스트 이름", "SOCKS5: invalid host name"))?
        }
        4 => std::net::Ipv6Addr::from(read_array::<16, _>(s).await?).to_string(),
        other => bail!(tr!("SOCKS5: 알 수 없는 주소 형식 {other}", "SOCKS5: unknown address type {other}")),
    };
    let port = u16::from_be_bytes(read_array(s).await?);
    if command != 1 {
        // 7 = command not supported
        s.write_all(&[SOCKS_VERSION, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        bail!(tr!(
            "SOCKS5: CONNECT 외의 명령({command})은 지원하지 않습니다",
            "SOCKS5: only CONNECT is supported (got command {command})"
        ));
    }
    Ok((host, port))
}

async fn socks5_reply<S: AsyncWrite + Unpin>(s: &mut S, ok: bool) -> std::io::Result<()> {
    // 0 = succeeded, 5 = connection refused; bound address left as 0.0.0.0:0.
    s.write_all(&[SOCKS_VERSION, if ok { 0 } else { 5 }, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

async fn read_array<const N: usize, S: AsyncRead + Unpin>(s: &mut S) -> std::io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    s.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openssh_specs() {
        let r = ForwardRule::parse(ForwardKind::Local, "8080:db:5432").unwrap();
        assert_eq!((r.bind_host.as_str(), r.bind_port, r.dest_host.as_str(), r.dest_port), ("", 8080, "db", 5432));
        assert_eq!(r.to_string(), "L 127.0.0.1:8080 → db:5432");

        let r = ForwardRule::parse(ForwardKind::Remote, "0.0.0.0:9000:localhost:3000").unwrap();
        assert_eq!((r.bind_host.as_str(), r.bind_port), ("0.0.0.0", 9000));

        let r = ForwardRule::parse(ForwardKind::Local, "[::1]:8080:[fe80::1]:22").unwrap();
        assert_eq!((r.bind_host.as_str(), r.dest_host.as_str()), ("::1", "fe80::1"));

        let r = ForwardRule::parse(ForwardKind::Dynamic, "1080").unwrap();
        assert_eq!(r.to_string(), "D 127.0.0.1:1080 (SOCKS5)");
        assert_eq!(ForwardRule::parse(ForwardKind::Dynamic, "0.0.0.0:1080").unwrap().bind_host, "0.0.0.0");
    }

    #[test]
    fn rejects_bad_specs() {
        assert!(ForwardRule::parse(ForwardKind::Local, "8080").is_err());
        assert!(ForwardRule::parse(ForwardKind::Local, "x:db:1").is_err());
        assert!(ForwardRule::parse(ForwardKind::Local, "1::1").is_err());
        assert!(ForwardRule::parse(ForwardKind::Dynamic, "a:b:c").is_err());
    }

    #[tokio::test]
    async fn socks5_domain_connect() {
        let (mut client, mut server) = tokio::io::duplex(256);
        let server_side = tokio::spawn(async move {
            let target = socks5_accept(&mut server).await.unwrap();
            socks5_reply(&mut server, true).await.unwrap();
            target
        });
        client.write_all(&[5, 1, 0]).await.unwrap();
        assert_eq!(read_array::<2, _>(&mut client).await.unwrap(), [5, 0]);
        let mut req = vec![5, 1, 0, 3, 7];
        req.extend_from_slice(b"example");
        req.extend_from_slice(&443u16.to_be_bytes());
        client.write_all(&req).await.unwrap();
        assert_eq!(read_array::<10, _>(&mut client).await.unwrap()[..2], [5, 0]);
        assert_eq!(server_side.await.unwrap(), ("example".to_owned(), 443));
    }

    #[tokio::test]
    async fn socks5_rejects_auth_only_clients_and_bind() {
        let (mut client, mut server) = tokio::io::duplex(256);
        let task = tokio::spawn(async move { socks5_accept(&mut server).await });
        client.write_all(&[5, 1, 2]).await.unwrap(); // only username/password
        assert_eq!(read_array::<2, _>(&mut client).await.unwrap(), [5, 0xFF]);
        assert!(task.await.unwrap().is_err());

        let (mut client, mut server) = tokio::io::duplex(256);
        let task = tokio::spawn(async move { socks5_accept(&mut server).await });
        client.write_all(&[5, 1, 0, 5, 2, 0, 1, 127, 0, 0, 1, 0, 80]).await.unwrap(); // BIND
        let _ = read_array::<2, _>(&mut client).await.unwrap();
        assert_eq!(read_array::<10, _>(&mut client).await.unwrap()[1], 7);
        assert!(task.await.unwrap().is_err());
    }
}
