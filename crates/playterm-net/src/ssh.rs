use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::{HashAlg, PublicKeyOrCertificate, known_hosts};
use russh::{ChannelMsg, Disconnect, MethodKind, MethodSet};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::session::{ConnectParams, EventSink, PromptField, SessionCommand, SessionEvent};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_AUTH_ROUNDS: usize = 3;

struct ClientHandler {
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

async fn authenticate(
    session: &mut client::Handle<ClientHandler>,
    params: &ConnectParams,
    sink: &EventSink,
) -> anyhow::Result<()> {
    let user = params.username.as_str();
    if user.is_empty() {
        bail!("SSH 사용자 이름이 필요합니다");
    }

    // First attempt: the stored password if we have one, otherwise "none" to learn which
    // methods the server accepts.
    let first = match &params.password {
        Some(password) => session.authenticate_password(user, password).await?,
        None => session.authenticate_none(user).await?,
    };
    let mut methods: Option<MethodSet> = match first {
        AuthResult::Success => return Ok(()),
        AuthResult::Failure { remaining_methods, .. } => Some(remaining_methods),
    };
    // A stored password may also answer a single keyboard-interactive password prompt
    // (servers with `PasswordAuthentication no` but PAM keyboard-interactive).
    let mut autofill = params.password.clone();

    let target = format!("{user}@{}", params.host);
    // Some servers advertise keyboard-interactive but reject it without asking anything;
    // after that we fall back to plain password prompts.
    let mut keyboard_interactive_usable = true;
    let mut attempts = 0;
    while attempts < MAX_AUTH_ROUNDS {
        let allows = |kind: MethodKind| methods.as_ref().is_none_or(|m| m.contains(&kind));

        let outcome = if keyboard_interactive_usable && allows(MethodKind::KeyboardInteractive) {
            match keyboard_interactive(session, user, &target, &mut autofill, sink).await? {
                Outcome::Unavailable(remaining) => {
                    keyboard_interactive_usable = false;
                    methods = Some(remaining);
                    continue;
                }
                outcome => outcome,
            }
        } else if allows(MethodKind::Password) {
            let Some(answers) = sink
                .prompt(
                    format!("{target} 비밀번호"),
                    String::new(),
                    vec![PromptField { label: "비밀번호".into(), echo: false }],
                )
                .await
            else {
                bail!("인증이 취소되었습니다");
            };
            let password = answers.into_iter().next().unwrap_or_default();
            match session.authenticate_password(user, password).await? {
                AuthResult::Success => Outcome::Success,
                AuthResult::Failure { remaining_methods, .. } => Outcome::Failure(remaining_methods),
            }
        } else {
            let names: Vec<&str> = methods.iter().flat_map(|m| m.iter()).map(<&str>::from).collect();
            bail!(
                "서버가 허용하는 인증 방식({})을 아직 지원하지 않습니다",
                names.join(", ")
            );
        };

        attempts += 1;
        match outcome {
            Outcome::Success => return Ok(()),
            Outcome::Cancelled => bail!("인증이 취소되었습니다"),
            Outcome::Unavailable(remaining) | Outcome::Failure(remaining) => {
                sink.notice("인증 실패, 다시 시도하세요.");
                methods = Some(remaining);
            }
        }
    }
    bail!("인증 실패: 시도 횟수를 초과했습니다")
}

enum Outcome {
    Success,
    Failure(MethodSet),
    /// Rejected before any question was asked: the method is not really available.
    Unavailable(MethodSet),
    Cancelled,
}

async fn keyboard_interactive(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    target: &str,
    autofill: &mut Option<String>,
    sink: &EventSink,
) -> anyhow::Result<Outcome> {
    let mut response = session
        .authenticate_keyboard_interactive_start(user, None::<String>)
        .await?;
    let mut asked = false;
    loop {
        response = match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(Outcome::Success),
            KeyboardInteractiveAuthResponse::Failure { remaining_methods, .. } => {
                return Ok(if asked {
                    Outcome::Failure(remaining_methods)
                } else {
                    Outcome::Unavailable(remaining_methods)
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                asked |= !prompts.is_empty();
                let answers = if prompts.is_empty() {
                    Vec::new()
                } else if let [only] = prompts.as_slice()
                    && !only.echo
                    && let Some(password) = autofill.take()
                {
                    vec![password]
                } else {
                    let title = if name.is_empty() { format!("{target} 인증") } else { name };
                    let fields = prompts
                        .into_iter()
                        .map(|p| PromptField { label: p.prompt.trim().to_owned(), echo: p.echo })
                        .collect();
                    match sink.prompt(title, instructions, fields).await {
                        Some(answers) => answers,
                        None => return Ok(Outcome::Cancelled),
                    }
                };
                session.authenticate_keyboard_interactive_respond(answers).await?
            }
        };
    }
}
