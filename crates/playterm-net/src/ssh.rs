use crate::x11::{self, X11Forward};
use std::borrow::Cow;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
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
    agent_forward: crate::agent_forward::Shared,
    /// Dropping the SSH handler aborts every relay, including a stalled local agent.
    agent_tasks: JoinSet<()>,
}

impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: russh::Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        while self.agent_tasks.try_join_next().is_some() {}
        let forward = self.agent_forward.lock().unwrap().clone();
        let Some(forward) = forward.filter(|_| self.agent_tasks.len() < 16) else {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        };
        let sink = self.sink.clone();
        self.agent_tasks.spawn(async move {
            if let Err(e) = forward.relay(channel, reply).await {
                sink.notice(tr!("ssh.ssh_agent_forwarding", error = format!("{e:#}")));
            }
        });
        Ok(())
    }

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let authorities = crate::host_certificate::load(&self.known_hosts, &self.host, self.port)?;
        let key = match server_key {
            PublicKeyOrCertificate::Certificate(cert) => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
                authorities.verify(cert, &self.host, now)?;
                self.sink.notice(t!("ssh.verified_ssh_host_certificate_from_a_trusted_ca"));
                return Ok(true);
            }
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
        };
        if authorities.is_revoked(key) {
            bail!(t!("ssh.the_ssh_host_key_is_revoked"));
        }
        if authorities.configured() {
            bail!(t!("ssh.a_host_certificate_from_the_trusted_ca_is"));
        }

        crate::trust::read(&self.known_hosts).context(t!("ssh.could_not_check_known_hosts"))?;
        let known =
            known_hosts::known_host_keys_path(&self.host, self.port, &self.known_hosts).context(t!("ssh.could_not_check_known_hosts"))?;
        if known.iter().any(|(_, recorded)| recorded.key_data() == key.key_data()) {
            return Ok(true);
        }
        // Any other key for a host we know is a changed key, whatever its type: a
        // man-in-the-middle offering only another algorithm must not pass as a first visit.
        if !known.is_empty() {
            let types: Vec<String> = known.iter().map(|(_, k)| k.algorithm().to_string()).collect();
            let lines: Vec<String> = known.iter().map(|(line, _)| line.to_string()).collect();
            bail!(tr!(
                "ssh.warning_the_host_key_has_changed_got_a",
                arg0 = types.join(", "),
                arg1 = key.algorithm(),
                arg2 = self.known_hosts.display(),
                arg3 = lines.join(", ")
            ));
        }
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let accepted = self.sink.confirm_host_key(&self.host, self.port, key.algorithm().to_string(), fingerprint).await;
        if accepted {
            let _lock = crate::trust::lock(&self.known_hosts)?;
            // Another window may have learned a key while this confirmation was open.
            let latest = known_hosts::known_host_keys_path(&self.host, self.port, &self.known_hosts)?;
            if !latest.is_empty() && !latest.iter().any(|(_, recorded)| recorded.key_data() == key.key_data()) {
                bail!(t!("ssh.the_host_key_changed_while_confirming_reconnect_and"));
            }
            if latest.is_empty() {
                known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts)
                    .context(t!("ssh.could_not_save_known_hosts"))?;
            }
        }
        Ok(accepted)
    }

    async fn auth_banner(&mut self, banner: &str, _session: &mut client::Session) -> Result<(), Self::Error> {
        self.sink.notice(banner.to_owned());
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let Some(rule) = remote_forward_rule(&self.remote_forwards, connected_address, connected_port).cloned() else {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        };
        reply.accept().await;
        let sink = self.sink.clone();
        tokio::spawn(async move {
            if let Err(e) = forward::handle_remote(channel, rule.clone()).await {
                sink.notice(tr!("common.forwarded_connection_failed", error = format!("{e:#}"), rule = rule));
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
                sink.notice(tr!("ssh.x11_forwarding", error = format!("{e:#}")));
            }
        });
        Ok(())
    }
}

fn normalized_bind(address: &str) -> String {
    let address = address.trim();
    let address = address.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(address);
    if address.is_empty() {
        return "localhost".into();
    }
    address.parse::<std::net::IpAddr>().map_or_else(|_| address.to_ascii_lowercase(), |ip| ip.to_string())
}

/// Exact addresses win. A loopback spelling is compatible only when it identifies one rule;
/// an empty/localhost report must not choose between distinct IPv4 and IPv6 destinations.
fn remote_forward_rule<'a>(rules: &'a [ForwardRule], address: &str, port: u32) -> Option<&'a ForwardRule> {
    let address = normalized_bind(address);
    let candidates = || rules.iter().filter(|rule| rule.kind == ForwardKind::Remote && u32::from(rule.bind_port) == port);
    let mut exact = candidates().filter(|rule| normalized_bind(&rule.bind_host) == address);
    if let Some(rule) = exact.next() {
        return exact.next().is_none().then_some(rule);
    }
    let loopback = |name: &str| matches!(name, "localhost" | "127.0.0.1" | "::1");
    if !loopback(&address) {
        return None;
    }
    let mut compatible = candidates().filter(|rule| {
        let requested = normalized_bind(&rule.bind_host);
        loopback(&requested) && (requested == "localhost" || address == "localhost")
    });
    let rule = compatible.next()?;
    compatible.next().is_none().then_some(rule)
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
    let (mut first, rest): (Vec<Algorithm>, Vec<Algorithm>) = order.iter().cloned().partition(|a| known.iter().any(|k| same_kind(a, k)));
    first.extend(rest);
    first
}

/// Jump hosts a connection goes through, kept open for as long as it lasts.
type Hops = Vec<client::Handle<ClientHandler>>;
/// A connection being made: the session and the jump hosts it goes through.
type Connecting<'a> = Pin<Box<dyn Future<Output = anyhow::Result<(client::Handle<ClientHandler>, Hops)>> + Send + 'a>>;

fn algorithm_preferences(legacy: bool) -> russh::Preferred {
    let mut preferred = russh::Preferred::default();
    if legacy {
        preferred.kex.to_mut().extend([russh::kex::DH_G14_SHA1, russh::kex::DH_G1_SHA1]);
        preferred.cipher.to_mut().extend([russh::cipher::AES_128_CBC, russh::cipher::AES_192_CBC, russh::cipher::AES_256_CBC]);
        preferred.mac.to_mut().extend([russh::mac::HMAC_SHA1_ETM, russh::mac::HMAC_SHA1]);
    }
    preferred
}

/// An authenticated SSH session with `params.host`: over TCP, or through `params.jump` (a
/// direct-tcpip channel of the jump host's session, which may itself go through another).
/// Returns the session and the jump hosts' sessions, outermost last.
fn connect_ssh<'a>(
    params: &'a ConnectParams,
    sink: &'a EventSink,
    x11: Option<Arc<X11Forward>>,
    agent_forward: crate::agent_forward::Shared,
) -> Connecting<'a> {
    Box::pin(async move {
        // keepalive@openssh.com requests also notice a dead connection (no reply 3 times).
        let mut preferred = algorithm_preferences(params.ssh_legacy_algorithms);
        preferred.key = Cow::Owned(prefer_known_host_keys(&preferred.key, &params.host, params.port, &params.known_hosts));
        // Prefer the certificate when offered. Missing CA trust then rejects the
        // certificate instead of silently falling back to bare-key TOFU.
        preferred.host_key_certificates = Cow::Owned(preferred.key.to_vec());
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
            agent_forward,
            agent_tasks: JoinSet::new(),
        };
        let (mut session, hops) = match &params.jump {
            None => {
                let stream = TcpStream::connect((params.host.as_str(), params.port))
                    .await
                    .map_err(|e| crate::session::connect_error(&params.host, params.port, e))?;
                stream.set_nodelay(true)?;
                (client::connect_stream(config, stream, handler).await?, Hops::new())
            }
            Some(jump) => {
                let (bastion, mut hops) = connect_ssh(jump, sink, None, Arc::default()).await?;
                let channel = bastion
                    .channel_open_direct_tcpip(params.host.as_str(), params.port.into(), "127.0.0.1", 0)
                    .await
                    .with_context(|| tr!("ssh.the_jump_host_cannot_reach", arg0 = jump.host, arg1 = params.host, arg2 = params.port))?;
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
        Some(request) => Some(Arc::new(X11Forward::new(&request.display, request.trusted).await?)),
        None => None,
    };
    let agent_forward = crate::agent_forward::Shared::default();
    let (session, hops) = connect_ssh(params, sink, x11.clone(), agent_forward.clone()).await?;
    // Shared with forwarding listeners and SFTP, which open their own channels.
    let session = Arc::new(session);
    let mut tasks = JoinSet::new();
    forward::start(&session, &params.forwards, sink, &mut tasks).await;

    let mut channel = session.channel_open_session().await?;
    if params.agent_forwarding {
        let binding = session.agent_session_binding().cloned().context(t!("ssh.cannot_bind_the_ssh_agent_to_this_session"))?;
        let forward = crate::agent_forward::Forward::prepare(params, binding).await?;
        // The server may open channels immediately after acknowledging this request.
        *agent_forward.lock().unwrap() = Some(forward);
        channel.agent_forward(true).await?;
        match channel.wait().await {
            Some(ChannelMsg::Success) => sink.notice(t!("ssh.ssh_agent_forwarding_enabled")),
            _ => {
                *agent_forward.lock().unwrap() = None;
                bail!(t!("ssh.the_server_refused_ssh_agent_forwarding"));
            }
        }
    }
    channel.request_pty(false, &params.term_type, params.cols.into(), params.rows.into(), 0, 0, &[]).await?;
    if let Some(x11) = &x11 {
        channel.request_x11(false, false, x11::AUTH_PROTOCOL, x11.fake_cookie_hex(), x11.screen).await?;
        match x11.refusal() {
            Some(why) => sink.notice(tr!("ssh.x11_forwarding_2", why = why)),
            None => sink.notice(tr!("ssh.x11_forwarding_x_programs_on_the_server_show", arg0 = x11.display)),
        }
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
                    sink.data(data.to_vec()).await;
                }
                Some(ChannelMsg::ExitStatus { exit_status }) if exit_status != 0 => {
                    sink.notice(tr!("ssh.remote_shell_exit_code", exit_status = exit_status));
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
                    sink.notice(t!("ssh.ssh_sessions_do_not_support_break"))
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
    let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await.context(t!("ssh.the_server_does_not_support_sftp"))?;
    Ok(SftpConnection::new(sftp, sink.waker()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_preferences_append_without_weakening_defaults() {
        let safe = super::algorithm_preferences(false);
        let legacy = super::algorithm_preferences(true);
        assert!(!safe.kex.contains(&russh::kex::DH_G14_SHA1));
        assert!(!safe.kex.contains(&russh::kex::DH_G1_SHA1));
        assert!(!safe.cipher.contains(&russh::cipher::AES_128_CBC));
        assert!(!safe.mac.contains(&russh::mac::HMAC_SHA1));
        assert!(legacy.kex.starts_with(&safe.kex));
        assert!(legacy.cipher.starts_with(&safe.cipher));
        assert!(legacy.mac.starts_with(&safe.mac));
    }
    use super::*;
    use russh::keys::EcdsaCurve;

    #[test]
    fn remote_rules_preserve_addresses_and_only_allow_unambiguous_loopback_aliases() {
        let rule = |host: &str, dest| ForwardRule {
            kind: ForwardKind::Remote,
            bind_host: host.into(),
            bind_port: 9000,
            dest_port: dest,
            ..Default::default()
        };
        let rules = [rule("127.0.0.1", 1), rule("127.0.0.2", 2), rule("::1", 3)];
        for (address, dest) in [("127.0.0.1", 1), ("127.0.0.2", 2), ("[0:0:0:0:0:0:0:1]", 3)] {
            assert_eq!(remote_forward_rule(&rules, address, 9000).unwrap().dest_port, dest);
        }
        for address in ["", "localhost", "203.0.113.1", "0.0.0.0"] {
            assert!(remote_forward_rule(&rules, address, 9000).is_none(), "ambiguous or unmatched {address}");
        }
        assert!(remote_forward_rule(&rules, "127.0.0.1", 9001).is_none());
        for requested in ["", "localhost", "127.0.0.1", "::1"] {
            for reported in ["", "LOCALHOST"] {
                assert!(remote_forward_rule(&[rule(requested, 1)], reported, 9000).is_some());
            }
        }
        assert!(remote_forward_rule(&[rule("localhost", 1)], "127.0.0.1", 9000).is_some());
        assert!(remote_forward_rule(&[rule("localhost", 1)], "::1", 9000).is_some());
        assert!(remote_forward_rule(&[rule("127.0.0.2", 1)], "localhost", 9000).is_none());
        assert!(remote_forward_rule(&[rule("127.0.0.1", 1), rule("127.0.0.1", 2)], "127.0.0.1", 9000).is_none());
    }

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
