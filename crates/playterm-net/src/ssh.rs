use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use russh::client;
use russh::keys::{HashAlg, PublicKeyOrCertificate, known_hosts};
use russh::{ChannelMsg, Disconnect};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::session::{ConnectParams, EventSink, SessionCommand, SessionEvent};
use crate::ssh_auth::authenticate;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    sink: EventSink,
}

impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            bail!("인증서 기반 호스트 키는 아직 지원하지 않습니다");
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
                    self.sink.notice(format!("known_hosts 저장 실패: {e}"));
                }
                Ok(accepted)
            }
            Err(russh::keys::Error::KeyChanged { line }) => bail!(
                "경고: 호스트 키가 변경되었습니다! 중간자 공격일 수 있습니다. \
                 정상적인 변경이라면 {} 의 {line}번째 줄을 삭제하세요.",
                self.known_hosts.display()
            ),
            Err(e) => Err(e).context("known_hosts 확인 실패"),
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
    .context("연결 시간 초과")?
    .with_context(|| format!("{}:{} 에 연결할 수 없습니다", params.host, params.port))?;
    stream.set_nodelay(true)?;

    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let handler = ClientHandler {
        host: params.host.clone(),
        port: params.port,
        known_hosts: params.known_hosts.clone(),
        sink: sink.clone(),
    };
    let mut session = client::connect_stream(config, stream, handler).await?;

    authenticate(&mut session, &params, &sink).await?;

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
    loop {
        tokio::select! {
            msg = reader.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    sink.send(SessionEvent::Data(data.to_vec()));
                }
                Some(ChannelMsg::ExitStatus { exit_status }) if exit_status != 0 => {
                    sink.notice(format!("원격 셸 종료 코드: {exit_status}"));
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            cmd = commands.recv() => match cmd {
                Some(SessionCommand::Input(data)) => writer.data_bytes(data).await?,
                Some(SessionCommand::Resize { cols, rows }) => {
                    writer.window_change(cols.into(), rows.into(), 0, 0).await?;
                }
                Some(SessionCommand::Close) | None => break,
            },
        }
    }

    let _ = session.disconnect(Disconnect::ByApplication, "", "en").await;
    Ok(())
}
