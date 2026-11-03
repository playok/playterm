use std::borrow::Cow;
use crate::x11::{self, X11Forward};
use std::future::Future;
use std::pin::Pin;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use playterm_i18n::{t, tr};
use russh::client;
use russh::keys::{Algorithm, HashAlg, PublicKeyOrCertificate, known_hosts};
use russh::{ChannelMsg, Disconnect};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::forward::{self, ForwardKind, ForwardRule};
use crate::keepalive::{IdleTimer, KeepAliveMode, sleep_until};
use crate::session::{ConnectParams, EventSink, SessionCommand, SessionEvent};
use crate::sftp::SftpConnection;
use crate::ssh_auth::authenticate;

pub(crate) struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    sink: EventSink,
    /// Remote (-R) rules, to route channels the server opens back to us.
    remote_forwards: Vec<ForwardRule>,
    /// X11 forwarding asked for: where the server's `x11` channels go.
    x11: Option<Arc<X11Forward>>,
}

impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            bail!(t("인증서 기반 호스트 키는 아직 지원하지 않습니다", "Certificate host keys are not supported yet"));
        };

        let known = known_hosts::known_host_keys_path(&self.host, self.port, &self.known_hosts)
            .context(t("known_hosts 확인 실패", "Could not check known_hosts"))?;
        if known.iter().any(|(_, recorded)| recorded.key_data() == key.key_data()) {
            return Ok(true);
        }
        // Any other key for a host we know is a changed key, whatever its type: a
        // man-in-the-middle offering only another algorithm must not pass as a first visit.
        if !known.is_empty() {
            let types: Vec<String> = known.iter().map(|(_, k)| k.algorithm().to_string()).collect();
            let lines: Vec<String> = known.iter().map(|(line, _)| line.to_string()).collect();
            bail!(tr!(
                "경고: 호스트 키가 변경되었습니다! 저장된 키({})가 아닌 {} 키를 받았습니다. 중간자 공격일 수 있습니다. \
                 정상적인 변경이라면 {} 의 {}번째 줄을 삭제하세요.",
                "WARNING: the host key has changed! Got a {1} key instead of the known one ({0}). This could be a \
                 man-in-the-middle attack. If the change is legitimate, delete line {3} of {2}.",
                types.join(", "),
                key.algorithm(),
                self.known_hosts.display(),
                lines.join(", ")
            ));
        }
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let accepted =
            self.sink.confirm_host_key(&self.host, self.port, key.algorithm().to_string(), fingerprint).await;
        if accepted && let Err(e) = known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            self.sink.notice(tr!("known_hosts 저장 실패: {e}", "Could not save known_hosts: {e}"));
        }
        Ok(accepted)
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        self.sink.notice(banner.to_owned());
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        // Servers report the bound address in different spellings (``, `localhost`,
        // `127.0.0.1`), so rules are matched by port; two -R rules cannot share one anyway.
        let Some(rule) = self.remote_forwards.iter().find(|r| u32::from(r.bind_port) == connected_port).cloned()
        else {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        };
        reply.accept().await;
        let sink = self.sink.clone();
        tokio::spawn(async move {
            if let Err(e) = forward::handle_remote(channel, rule.clone()).await {
                sink.notice(tr!("포워딩 연결 실패 ({rule}): {e:#}", "Forwarded connection failed ({rule}): {e:#}"));
            }
        });
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        // Only when this session asked for it (a server must not open X11 channels uninvited).
        let Some(x11) = self.x11.clone() else {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        };
        reply.accept().await;
        let sink = self.sink.clone();
        tokio::spawn(async move {
            if let Err(e) = x11.relay(channel.into_stream()).await {
                sink.notice(tr!("X11 포워딩: {e:#}", "X11 forwarding: {e:#}"));
            }
        });
        Ok(())
    }
}

/// Host key algorithms in `order`, those already known for the host first, as OpenSSH does: a
/// server with several host keys then shows the one on record, instead of another type that
/// would be refused as a changed key.
fn prefer_known_host_keys(order: &[Algorithm], host: &str, port: u16, known_hosts: &Path) -> Vec<Algorithm> {
    let known: Vec<Algorithm> = known_hosts::known_host_keys_path(host, port, known_hosts)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, key)| key.algorithm())
        .collect();
    prefer(order, &known)
}

/// `order` with the algorithms of the `known` kinds moved to the front (RSA in any hash).
fn prefer(order: &[Algorithm], known: &[Algorithm]) -> Vec<Algorithm> {
    let same_kind = |a: &Algorithm, b: &Algorithm| match (a, b) {
        (Algorithm::Rsa { .. }, Algorithm::Rsa { .. }) => true,
        _ => a == b,
    };
    let (mut first, rest): (Vec<Algorithm>, Vec<Algorithm>) =
        order.iter().cloned().partition(|a| known.iter().any(|k| same_kind(a, k)));
    first.extend(rest);
    first
}

/// Jump hosts a connection goes through, kept open for as long as it lasts.
type Hops = Vec<client::Handle<ClientHandler>>;
/// A connection being made: the session and the jump hosts it goes through.
type Connecting<'a> = Pin<Box<dyn Future<Output = anyhow::Result<(client::Handle<ClientHandler>, Hops)>> + Send + 'a>>;

/// An authenticated SSH session with `params.host`: over TCP, or through `params.jump` (a
/// direct-tcpip channel of the jump host's session, which may itself go through another).
/// Returns the session and the jump hosts' sessions, outermost last.
fn connect_ssh<'a>(
    params: &'a ConnectParams,
    sink: &'a EventSink,
    x11: Option<Arc<X11Forward>>,
) -> Connecting<'a> {
    Box::pin(async move {
        // keepalive@openssh.com requests also notice a dead connection (no reply 3 times).
        let mut preferred = russh::Preferred::default();
        preferred.key =
            Cow::Owned(prefer_known_host_keys(&preferred.key, &params.host, params.port, &params.known_hosts));
        let config = Arc::new(client::Config {
            keepalive_interval: (params.keepalive.mode != KeepAliveMode::Off).then(|| params.keepalive.period()),
            preferred,
            ..Default::default()
        });
        let handler = ClientHandler {
            host: params.host.clone(),
            port: params.port,
            known_hosts: params.known_hosts.clone(),
            sink: sink.clone(),
            remote_forwards: params.forwards.iter().filter(|r| r.kind == ForwardKind::Remote).cloned().collect(),
            x11,
        };
        let (mut session, hops) = match &params.jump {
            None => {
                let stream = TcpStream::connect((params.host.as_str(), params.port)).await.with_context(|| {
                    tr!("{}:{} 에 연결할 수 없습니다", "Cannot connect to {}:{}", params.host, params.port)
                })?;
                stream.set_nodelay(true)?;
                (client::connect_stream(config, stream, handler).await?, Hops::new())
            }
            Some(jump) => {
                let (bastion, mut hops) = connect_ssh(jump, sink, None).await?;
                let channel = bastion
                    .channel_open_direct_tcpip(params.host.as_str(), params.port.into(), "127.0.0.1", 0)
                    .await
                    .with_context(|| {
                        tr!(
                            "점프 호스트 {} 에서 {}:{} 에 연결할 수 없습니다",
                            "The jump host {} cannot reach {}:{}",
                            jump.host,
                            params.host,
                            params.port
                        )
                    })?;
                let session = client::connect_stream(config, channel.into_stream(), handler).await?;
                hops.push(bastion);
                (session, hops)
            }
        };
        authenticate(&mut session, params, sink).await?;
        Ok((session, hops))
    })
}

/// TCP (or the jump hosts), the SSH handshake, authentication, forwardings and the shell
/// channel.
async fn open(
    params: &ConnectParams,
    sink: &EventSink,
) -> anyhow::Result<(Arc<client::Handle<ClientHandler>>, russh::Channel<client::Msg>, JoinSet<()>, Hops)> {
    let x11 = match &params.x11 {
        Some(display) => Some(Arc::new(X11Forward::new(display).await?)),
        None => None,
    };
    let (session, hops) = connect_ssh(params, sink, x11.clone()).await?;
    // Shared with forwarding listeners and SFTP, which open their own channels.
    let session = Arc::new(session);
    let mut tasks = JoinSet::new();
    forward::start(&session, &params.forwards, sink, &mut tasks).await;

    let channel = session.channel_open_session().await?;
    channel
        .request_pty(
            false,
            &params.term_type,
            params.cols.into(),
            params.rows.into(),
            0,
            0,
            &[],
        )
        .await?;
    if let Some(x11) = &x11 {
        channel.request_x11(false, false, x11::AUTH_PROTOCOL, x11.fake_cookie_hex(), x11.screen).await?;
        sink.notice(tr!("X11 포워딩: 서버의 X 프로그램을 {} 에 표시합니다", "X11 forwarding: X programs on the server show on {}", x11.display));
    }
    channel.request_shell(false).await?;
    Ok((session, channel, tasks, hops))
}

pub(crate) async fn run(
    params: ConnectParams,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    sink: EventSink,
) -> anyhow::Result<()> {
    // Up to the shell, within the connect time limit; prompts (host key, password) pause it.
    // The jump hosts' sessions stay open (held here) for as long as this one.
    let (session, channel, mut tasks, _hops) = sink.connecting(&params, open(&params, &sink)).await?;
    sink.send(SessionEvent::Connected);

    let (mut reader, writer) = channel.split();
    let mut idle = IdleTimer::new(&params.keepalive, false, &sink);
    loop {
        tokio::select! {
            msg = reader.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    sink.send(SessionEvent::Data(data.to_vec()));
                }
                Some(ChannelMsg::ExitStatus { exit_status }) if exit_status != 0 => {
                    sink.notice(tr!("원격 셸 종료 코드: {exit_status}", "Remote shell exit code: {exit_status}"));
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            cmd = commands.recv() => match cmd {
                Some(SessionCommand::Input(data)) => {
                    writer.data_bytes(data).await?;
                    idle.touch();
                }
                Some(SessionCommand::Resize { cols, rows }) => {
                    writer.window_change(cols.into(), rows.into(), 0, 0).await?;
                }
                Some(SessionCommand::Rdp(_)) => {}
                Some(SessionCommand::OpenSftp) => {
                    let (session, sink) = (session.clone(), sink.clone());
                    tasks.spawn(async move {
                        let result = open_sftp(&session, &sink).await.map_err(|e| format!("{e:#}"));
                        sink.send(SessionEvent::Sftp(result));
                    });
                }
                Some(SessionCommand::ShellDir) => {
                    let (session, sink) = (session.clone(), sink.clone());
                    tasks.spawn(async move {
                        let dir = shell_dir(&session).await.unwrap_or_else(|e| {
                            log::info!("shell directory not found: {e:#}");
                            None
                        });
                        sink.send(SessionEvent::ShellDir(dir));
                    });
                }
                Some(SessionCommand::SendBreak) => {
                    sink.notice(t("SSH 세션에서는 BREAK 신호를 지원하지 않습니다", "SSH sessions do not support BREAK"))
                }
                Some(SessionCommand::Close) | None => break,
            },
            _ = sleep_until(idle.deadline()) => {
                if let Some(text) = &idle.text {
                    writer.data_bytes(text.clone()).await?;
                }
                idle.touch();
            }
        }
    }

    tasks.abort_all();
    let _ = session.disconnect(Disconnect::ByApplication, "", "en").await;
    Ok(())
}

/// The current directory of the interactive shell, found by `shell_dir.sh` in a second channel
/// of this connection. Nothing is typed into the shell, and nothing shows in the terminal.
async fn shell_dir(session: &client::Handle<ClientHandler>) -> anyhow::Result<Option<String>> {
    const SCRIPT: &str = include_str!("shell_dir.sh");
    const LIMIT: usize = 4096;
    let query = async {
        let mut channel = session.channel_open_session().await?;
        channel.exec(true, "sh -s").await?;
        // LF only, whatever the checkout did to line endings: `sh` would choke on CR.
        channel.data(SCRIPT.replace("\r\n", "\n").as_bytes()).await?;
        channel.eof().await?;
        let mut out = Vec::new();
        let mut status = None;
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } if out.len() < LIMIT => out.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                // The exit status comes before the end of the channel.
                ChannelMsg::Eof | ChannelMsg::Close if status.is_some() => break,
                _ => {}
            }
        }
        anyhow::Ok((status, out))
    };
    let (status, out) = tokio::time::timeout(Duration::from_secs(5), query).await.context("timed out")??;
    let dir = String::from_utf8_lossy(&out).trim_end_matches(['\r', '\n']).to_owned();
    Ok((status == Some(0) && dir.starts_with('/') && !dir.contains('\n')).then_some(dir))
}

async fn open_sftp(session: &client::Handle<ClientHandler>, sink: &EventSink) -> anyhow::Result<SftpConnection> {
    let channel = session.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
        .await
        .context(t("서버가 SFTP를 지원하지 않습니다", "The server does not support SFTP"))?;
    Ok(SftpConnection::new(sftp, sink.waker()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::EcdsaCurve;

    #[test]
    fn known_host_key_kinds_come_first() {
        let order = russh::Preferred::default().key.into_owned();
        assert_eq!(prefer(&order, &[]), order, "unknown host: the usual order");
        let rsa = prefer(&order, &[Algorithm::Rsa { hash: None }]);
        assert!(matches!(rsa[..3], [Algorithm::Rsa { .. }, Algorithm::Rsa { .. }, Algorithm::Rsa { .. }]), "{rsa:?}");
        assert_eq!(rsa.len(), order.len());
        let ecdsa = Algorithm::Ecdsa { curve: EcdsaCurve::NistP256 };
        assert_eq!(prefer(&order, std::slice::from_ref(&ecdsa))[0], ecdsa);
    }
}
