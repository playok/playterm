//! An SSH server on this PC for tests of the app (feature `testing`, never in a release): user
//! `tester`, password `secret`, a line-echo "shell" and SFTP of a folder.
//!
//! ```ignore
//! let server = playterm_net::testing::SshServer::start(sftp_root);
//! server.trust(&known_hosts);   // or let the app ask about the unknown host key
//! // connect to 127.0.0.1:server.port
//! ```

#[path = "../tests/common/sftp_fs.rs"]
#[allow(dead_code)]
mod sftp_fs;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use russh::keys::{Algorithm, PrivateKey, known_hosts};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};

pub const USER: &str = "tester";
pub const PASSWORD: &str = "secret";
/// What the shell shows first.
pub const BANNER: &str = "test shell ready\r\n$ ";

/// A running server (it stops with the test process).
pub struct SshServer {
    pub port: u16,
    host_key: russh::keys::PublicKey,
}

impl SshServer {
    /// Starts a server on 127.0.0.1 (a free port) serving `sftp_root` over SFTP.
    pub fn start(sftp_root: impl Into<PathBuf>) -> Self {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("host key");
        let host_key = key.public_key().clone();
        let config =
            Arc::new(server::Config { keys: vec![key], methods: MethodSet::from(&[MethodKind::Password][..]), ..Default::default() });
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = std_listener.local_addr().expect("address").port();
        std_listener.set_nonblocking(true).expect("non-blocking");
        let server = TestShell { sftp_root: sftp_root.into(), ..Default::default() };
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(std_listener).expect("listener");
                let _ = { server }.run_on_socket(config, &listener).await;
            });
        });
        Self { port, host_key }
    }

    /// Records the server's host key in `known_hosts`, as if it had been trusted before.
    pub fn trust(&self, known_hosts: &Path) {
        known_hosts::learn_known_hosts_path("127.0.0.1", self.port, &self.host_key, known_hosts).expect("known_hosts");
    }
}

#[derive(Clone, Default)]
struct TestShell {
    sftp_root: PathBuf,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    shells: Arc<Mutex<HashSet<ChannelId>>>,
    line: Vec<u8>,
}

impl server::Server for TestShell {
    type Handler = Self;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        Self { channels: Arc::default(), shells: Arc::default(), line: Vec::new(), ..self.clone() }
    }
}

impl server::Handler for TestShell {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == USER && password == PASSWORD {
            Auth::Accept
        } else {
            Auth::Reject { proceed_with_methods: Some(MethodSet::from(&[MethodKind::Password][..])), partial_success: false }
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().unwrap().insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        let channel = self.channels.lock().unwrap().remove(&id);
        match channel {
            Some(channel) if name == "sftp" => {
                session.channel_success(id)?;
                russh_sftp::server::run(channel.into_stream(), sftp_fs::FsHandler::new(&self.sftp_root)).await;
            }
            _ => session.channel_failure(id)?,
        }
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, _: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        // No shell-directory answers: the app then opens SFTP at the login directory.
        self.channels.lock().unwrap().remove(&channel);
        session.channel_failure(channel)
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.channels.lock().unwrap().remove(&channel);
        self.shells.lock().unwrap().insert(channel);
        session.data(channel, BANNER.as_bytes().to_vec())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if !self.shells.lock().unwrap().contains(&channel) {
            return Ok(());
        }
        for &b in data {
            if b == b'\r' {
                let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                session.data(channel, format!("\r\nyou typed: {line}\r\n$ ").into_bytes())?;
            } else {
                self.line.push(b);
                session.data(channel, vec![b])?;
            }
        }
        Ok(())
    }
}
