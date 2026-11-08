//! Per-session agent forwarding. No agent is exposed until an interactive session opts in.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, bail};
use playterm_i18n::t;
use russh::keys::agent::AgentSessionBinding;
use russh::keys::agent::client::{AgentClient, AgentStream};

use crate::session::ConnectParams;

type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;
pub(crate) type Shared = Arc<Mutex<Option<Forward>>>;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
enum Endpoint {
    #[cfg(windows)]
    Pipe(String),
    #[cfg(windows)]
    Pageant,
    #[cfg(unix)]
    Socket(String),
}

impl Endpoint {
    async fn connect(&self) -> anyhow::Result<Agent> {
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            Ok(match self {
                #[cfg(windows)]
                Self::Pipe(path) => AgentClient::connect_named_pipe(path).await?.dynamic(),
                #[cfg(windows)]
                Self::Pageant => AgentClient::connect_pageant().await?.dynamic(),
                #[cfg(unix)]
                Self::Socket(path) => AgentClient::connect_uds(path).await?.dynamic(),
            })
        })
        .await
        .context(t("SSH 에이전트 연결 시간 초과", "SSH agent connection timed out"))?
    }
}

#[derive(Clone)]
pub(crate) struct Forward {
    endpoint: Endpoint,
    binding: AgentSessionBinding,
}

impl Forward {
    pub(crate) async fn prepare(params: &ConnectParams, binding: AgentSessionBinding) -> anyhow::Result<Self> {
        // Resolve once. A later channel must not silently switch agents or reread the environment.
        let configured = params.agent_pipe.as_deref().map(str::trim).filter(|s| !s.is_empty());
        #[cfg(windows)]
        let endpoints = {
            let env = std::env::var("SSH_AUTH_SOCK").ok().filter(|s| s.starts_with(r"\\.\pipe\"));
            match configured.or(env.as_deref()) {
                Some(path) => vec![Endpoint::Pipe(path.to_owned())],
                None => vec![Endpoint::Pipe(r"\\.\pipe\openssh-ssh-agent".into()), Endpoint::Pageant],
            }
        };
        #[cfg(unix)]
        let endpoints = {
            let env = std::env::var("SSH_AUTH_SOCK").ok();
            configured
                .or(env.as_deref())
                .filter(|s| !s.starts_with(r"\\"))
                .map(|path| vec![Endpoint::Socket(path.to_owned())])
                .unwrap_or_default()
        };
        for endpoint in endpoints {
            let forward = Self { endpoint, binding: binding.clone() };
            match forward.connect().await {
                Ok(_) => return Ok(forward),
                Err(e) => log::debug!("agent forwarding unavailable: {e:#}"),
            }
        }
        bail!(t("포워딩할 SSH 에이전트에 연결할 수 없습니다", "Cannot connect to an SSH agent for forwarding"))
    }

    async fn connect(&self) -> anyhow::Result<Agent> {
        let mut agent = self.endpoint.connect().await?;
        // Mark each relayed connection as remote so OpenSSH can enforce destination constraints.
        // Older agents reject unknown extensions; only that explicit rejection is compatible.
        match tokio::time::timeout(CONNECT_TIMEOUT, agent.bind_session(&self.binding, true)).await? {
            Ok(()) | Err(russh::keys::Error::AgentFailure) => {}
            Err(e) => return Err(e.into()),
        }
        Ok(agent)
    }

    pub(crate) async fn relay(
        self,
        channel: russh::Channel<russh::client::Msg>,
        reply: russh::client::ChannelOpenHandle,
    ) -> anyhow::Result<()> {
        let agent = match self.connect().await {
            Ok(agent) => agent,
            Err(e) => {
                reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
                return Err(e);
            }
        };
        reply.accept().await;
        tokio::io::copy_bidirectional(&mut agent.into_inner(), &mut channel.into_stream()).await?;
        Ok(())
    }
}
