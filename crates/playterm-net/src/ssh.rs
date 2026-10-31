use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use playterm_i18n::{t, tr};
use russh::client;
use russh::keys::{HashAlg, PublicKeyOrCertificate, known_hosts};
use russh::{ChannelMsg, Disconnect};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::forward::{self, ForwardKind, ForwardRule};
use crate::keepalive::{IdleTimer, KeepAliveMode, sleep_until};
use crate::session::{ConnectParams, EventSink, SessionCommand, SessionEvent};
use crate::sftp::SftpConnection;
use crate::ssh_auth::authenticate;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    sink: EventSink,
    /// Remote (-R) rules, to route channels the server opens back to us.
    remote_forwards: Vec<ForwardRule>,
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

        match known_hosts::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(true),
            Ok(false) => {
                let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
                let accepted = self
                    .sink
                    .confirm_host_key(&self.host, self.port, key.algorithm().to_string(), fingerprint)
                    .await;
                if accepted
                    && let Err(e) =
                        known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts)
                {
                    self.sink.notice(tr!("known_hosts 저장 실패: {e}", "Could not save known_hosts: {e}"));
                }
                Ok(accepted)
            }
            Err(russh::keys::Error::KeyChanged { line }) => bail!(tr!(
                "경고: 호스트 키가 변경되었습니다! 중간자 공격일 수 있습니다. \
                 정상적인 변경이라면 {} 의 {line}번째 줄을 삭제하세요.",
                "WARNING: the host key has changed! This could be a man-in-the-middle attack. \
                 If the change is legitimate, delete line {line} of {}.",
                self.known_hosts.display()
            )),
            Err(e) => Err(e).context(t("known_hosts 확인 실패", "Could not check known_hosts")),
        }
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
}

pub(crate) async fn run(
    params: ConnectParams,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    sink: EventSink,
) -> anyhow::Result<()> {
    // Only the TCP connect is bounded by a timeout: the handshake may wait on the user
    // (host key confirmation, password prompt) for as long as they need.
    let stream = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((params.host.as_str(), params.port)),
    )
    .await
    .context(t("연결 시간 초과", "Connection timed out"))?
    .with_context(|| tr!("{}:{} 에 연결할 수 없습니다", "Cannot connect to {}:{}", params.host, params.port))?;
    stream.set_nodelay(true)?;

    // keepalive@openssh.com requests also notice a dead connection (no reply 3 times).
    let config = Arc::new(client::Config {
        keepalive_interval: (params.keepalive.mode != KeepAliveMode::Off).then(|| params.keepalive.period()),
        ..Default::default()
    });
    let handler = ClientHandler {
        host: params.host.clone(),
        port: params.port,
        known_hosts: params.known_hosts.clone(),
        sink: sink.clone(),
        remote_forwards: params.forwards.iter().filter(|r| r.kind == ForwardKind::Remote).cloned().collect(),
    };
    let mut session = client::connect_stream(config, stream, handler).await?;

    authenticate(&mut session, &params, &sink).await?;
    // Shared with forwarding listeners and SFTP, which open their own channels.
    let session = Arc::new(session);
    let mut tasks = JoinSet::new();
    forward::start(&session, &params.forwards, &sink, &mut tasks).await;

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
    channel.request_shell(false).await?;
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
                Some(SessionCommand::OpenSftp) => {
                    let (session, sink) = (session.clone(), sink.clone());
                    tasks.spawn(async move {
                        let result = open_sftp(&session, &sink).await.map_err(|e| format!("{e:#}"));
                        sink.send(SessionEvent::Sftp(result));
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

async fn open_sftp(session: &client::Handle<ClientHandler>, sink: &EventSink) -> anyhow::Result<SftpConnection> {
    let channel = session.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
        .await
        .context(t("서버가 SFTP를 지원하지 않습니다", "The server does not support SFTP"))?;
    Ok(SftpConnection::new(sftp, sink.waker()))
}
