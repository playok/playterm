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
        let env = std::env::var("SSH_AUTH_SOCK").ok();
        let path = platform_agent_path(params.agent_pipe.as_deref(), env.as_deref(), cfg!(windows));
        #[cfg(windows)]
        let endpoints = match path {
            Some(path) => vec![Endpoint::Pipe(path.to_owned())],
            None => vec![Endpoint::Pipe(r"\\.\pipe\openssh-ssh-agent".into()), Endpoint::Pageant],
        };
        #[cfg(unix)]
        let endpoints = path.map(|path| vec![Endpoint::Socket(path.to_owned())]).unwrap_or_default();
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

/// Windows named-pipe prefix, compared without case because Windows paths are case-insensitive.
const WINDOWS_PIPE_PREFIX: &str = r"\\.\pipe\";

fn is_windows_pipe_path(path: &str) -> bool {
    path.get(..WINDOWS_PIPE_PREFIX.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(WINDOWS_PIPE_PREFIX))
}

/// `C:\...` or `C:/...`: a Windows path that cannot be a Unix agent socket.
fn is_windows_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/')
}

/// Skip paths from another platform before selecting the configured agent or environment.
/// A usable explicit path remains authoritative; connection errors must not switch agents.
fn platform_agent_path<'a>(configured: Option<&'a str>, env: Option<&'a str>, windows: bool) -> Option<&'a str> {
    [configured, env].into_iter().flatten().map(str::trim).find(|path| {
        !path.is_empty() && if windows { is_windows_pipe_path(path) } else { !path.starts_with(r"\\") && !is_windows_drive_path(path) }
    })
}

#[cfg(test)]
mod tests {
    use super::platform_agent_path;

    #[test]
    fn foreign_platform_paths_do_not_hide_environment_agents() {
        let pipe = r"\\.\pipe\openssh-ssh-agent";
        let upper_pipe = r"\\.\PIPE\openssh-ssh-agent";
        let socket = "/tmp/test-agent.sock";
        assert_eq!(platform_agent_path(Some(pipe), Some(socket), false), Some(socket));
        assert_eq!(platform_agent_path(Some(upper_pipe), Some(socket), false), Some(socket));
        assert_eq!(platform_agent_path(Some(socket), Some(pipe), true), Some(pipe));
        assert_eq!(platform_agent_path(Some(upper_pipe), Some(pipe), true), Some(upper_pipe));
        assert_eq!(platform_agent_path(Some(upper_pipe), None, true), Some(upper_pipe));
        assert_eq!(platform_agent_path(Some(socket), None, true), None);
        assert_eq!(platform_agent_path(Some(r"C:\agent.sock"), Some(socket), false), Some(socket));
        assert_eq!(platform_agent_path(Some(r"C:/agent.sock"), None, false), None);
        assert_eq!(platform_agent_path(Some(r"C:\agent.sock"), None, true), None);
    }

    #[test]
    fn explicit_agents_take_priority_and_blank_or_foreign_paths_are_skipped() {
        for (windows, first, second) in [(false, "/tmp/first.sock", "/tmp/second.sock"), (true, r"\\.\pipe\first", r"\\.\pipe\second")] {
            assert_eq!(platform_agent_path(Some(first), Some(second), windows), Some(first));
            assert_eq!(platform_agent_path(Some("  "), Some(second), windows), Some(second));
            assert_eq!(platform_agent_path(None, Some(&format!(" {second} ")), windows), Some(second));
            assert_eq!(platform_agent_path(None, Some("  "), windows), None);
            assert_eq!(platform_agent_path(None, None, windows), None);
        }
        assert_eq!(platform_agent_path(None, Some(r"\\.\pipe\agent"), false), None);
        assert_eq!(platform_agent_path(None, Some("/tmp/agent.sock"), true), None);
    }
}
