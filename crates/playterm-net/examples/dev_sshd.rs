//! Minimal SSH server for manually testing playterm without a real host.
//!
//! `cargo run -p playterm-net --example dev_sshd` listens on 127.0.0.1:2222,
//! accepts user `tester` / password `secret`, and runs a toy line-echo "shell".

use std::net::SocketAddr;
use std::sync::Arc;

use rand::SeedableRng;
use rand::rngs::StdRng;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};

#[derive(Clone, Default)]
struct DevServer {
    line: Vec<u8>,
}

impl server::Server for DevServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        Self::default()
    }
}

impl server::Handler for DevServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == "tester" && password == "secret" {
            return Ok(Auth::Accept);
        }
        let methods: &[MethodKind] = &[MethodKind::Password];
        Ok(Auth::Reject { proceed_with_methods: Some(MethodSet::from(methods)), partial_success: false })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        let banner = "\x1b[1;32mplayterm dev_sshd\x1b[0m 에 오신 것을 환영합니다\r\n\
                      ┌────────┬────────┐\r\n│ 이름   │ 상태   │\r\n├────────┼────────┤\r\n\
                      │ web01  │ \x1b[32m정상\x1b[0m   │\r\n│ db01   │ \x1b[31m점검\x1b[0m   │\r\n\
                      └────────┴────────┘\r\n$ ";
        session.data(channel, banner.as_bytes().to_vec())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        for &b in data {
            match b {
                b'\r' => {
                    let line = String::from_utf8_lossy(&self.line).into_owned();
                    self.line.clear();
                    session.data(channel, format!("\r\nyou typed: {line}\r\n$ ").into_bytes())?;
                }
                0x7f => {
                    if self.line.pop().is_some() {
                        session.data(channel, b"\x08 \x08".to_vec())?;
                    }
                }
                _ => {
                    self.line.push(b);
                    session.data(channel, vec![b])?;
                }
            }
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = Arc::new(server::Config {
        // Fixed seed: the host key stays the same across runs, so known_hosts keeps matching.
        keys: vec![PrivateKey::random(&mut StdRng::seed_from_u64(0x706c_6179), Algorithm::Ed25519).expect("key generation")],
        methods: MethodSet::from(&[MethodKind::Password][..]),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:2222").await?;
    println!("dev_sshd listening on 127.0.0.1:2222 (tester / secret)");
    DevServer::default().run_on_socket(config, &listener).await
}
