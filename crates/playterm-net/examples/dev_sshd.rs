//! Minimal SSH server for manually testing playterm without a real host.
//!
//! `cargo run -p playterm-net --example dev_sshd [-- OPTIONS]` listens on 127.0.0.1:2222,
//! accepts user `tester` with password `secret`, and runs a toy line-echo "shell".
//!
//! Options:
//! - `--authorized-keys FILE`: also accept keys listed in FILE (OpenSSH `authorized_keys` format)
//! - `--sftp-root DIR`: serve DIR over SFTP (default: current directory)
//! - `--shell-dir DIR`: what the toy shell's "current directory" is, as playterm asks for it to
//!   open SFTP there (an SFTP path, default `/`)
//! - `--kbd-interactive`: also offer keyboard-interactive, the way OpenSSH does with PAM (a
//!   `Password: ` question, then an empty round before success)
//!
//! Port forwarding requests (direct-tcpip, tcpip-forward on 127.0.0.1) are honoured.

#[path = "../tests/common/sftp_fs.rs"]
mod sftp_fs;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rand::SeedableRng;
use rand::rngs::StdRng;
use russh::keys::{Algorithm, PrivateKey, PublicKey, parse_public_key_base64};
use russh::server::{self, Auth, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone)]
struct DevServer {
    authorized: Arc<Vec<PublicKey>>,
    sftp_root: PathBuf,
    /// Session channels not yet claimed by a shell (an SFTP subsystem request takes them).
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    shells: Arc<Mutex<HashSet<ChannelId>>>,
    /// Exec channels waiting for their script (see `exec_request`).
    scripts: Arc<Mutex<HashSet<ChannelId>>>,
    shell_dir: String,
    line: Vec<u8>,
    /// Offer keyboard-interactive too.
    kbd: bool,
    /// Keyboard-interactive round of this client: 1 = password asked, 2 = empty round sent.
    kbd_round: u8,
}

impl server::Server for DevServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        Self {
            channels: Arc::default(),
            shells: Arc::default(),
            scripts: Arc::default(),
            line: Vec::new(),
            kbd_round: 0,
            ..self.clone()
        }
    }
}

fn methods(kbd: bool) -> MethodSet {
    let mut methods = vec![MethodKind::PublicKey, MethodKind::Password];
    if kbd {
        methods.push(MethodKind::KeyboardInteractive);
    }
    MethodSet::from(&methods[..])
}

fn reject(kbd: bool) -> Auth {
    Auth::Reject { proceed_with_methods: Some(methods(kbd)), partial_success: false }
}

/// Reads the key blobs of an OpenSSH `authorized_keys` file.
fn load_authorized(path: &str) -> Vec<PublicKey> {
    let text = std::fs::read_to_string(path).expect("authorized keys file");
    text.lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter_map(|b64| parse_public_key_base64(b64).ok())
        .collect()
}

impl server::Handler for DevServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        println!("password auth for {user:?}");
        Ok(if user == "tester" && password == "secret" { Auth::Accept } else { reject(self.kbd) })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let answers: Vec<String> =
            response.map(|r| r.map(|b| String::from_utf8_lossy(&b).into_owned()).collect()).unwrap_or_default();
        println!("keyboard-interactive for {user:?}, round {}, {} answers", self.kbd_round, answers.len());
        let ask = |prompts: Vec<(std::borrow::Cow<'static, str>, bool)>| Auth::Partial {
            name: "".into(),
            instructions: "".into(),
            prompts: prompts.into(),
        };
        Ok(match self.kbd_round {
            0 => {
                self.kbd_round = 1;
                ask(vec![("Password: ".into(), false)])
            }
            1 if user == "tester" && answers.first().map(String::as_str) == Some("secret") => {
                self.kbd_round = 2;
                ask(Vec::new())
            }
            2 => Auth::Accept,
            _ => {
                self.kbd_round = 0;
                reject(self.kbd)
            }
        })
    }

    async fn auth_publickey(&mut self, user: &str, key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        let known = self.authorized.iter().any(|k| k.key_data() == key.public_key.key_data());
        Ok(if user == "tester" && known { Auth::Accept } else { reject(self.kbd) })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
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

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(mut tcp) = TcpStream::connect((host, port as u16)).await else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        reply.accept().await;
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut tcp, &mut channel.into_stream()).await;
        });
        Ok(())
    }

    async fn tcpip_forward(&mut self, address: &str, port: &mut u32, session: &mut Session) -> Result<bool, Self::Error> {
        let Ok(listener) = TcpListener::bind(("127.0.0.1", *port as u16)).await else { return Ok(false) };
        let (handle, address, port) = (session.handle(), address.to_owned(), *port);
        tokio::spawn(async move {
            while let Ok((mut tcp, peer)) = listener.accept().await {
                let channel = handle
                    .channel_open_forwarded_tcpip(address.clone(), port, peer.ip().to_string(), peer.port().into())
                    .await;
                if let Ok(channel) = channel {
                    tokio::spawn(async move {
                        let _ = tokio::io::copy_bidirectional(&mut tcp, &mut channel.into_stream()).await;
                    });
                }
            }
        });
        Ok(true)
    }

    /// playterm's question for the shell's directory (`sh -s` with a script on stdin): answered
    /// with a fixed directory once the script has arrived.
    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.channels.lock().unwrap().remove(&channel);
        if data == b"sh -s" {
            self.scripts.lock().unwrap().insert(channel);
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        if self.scripts.lock().unwrap().remove(&channel) {
            session.data(channel, format!("{}\n", self.shell_dir).into_bytes())?;
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
        }
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.channels.lock().unwrap().remove(&channel);
        self.shells.lock().unwrap().insert(channel);
        let banner = "\x1b[1;32mplayterm dev_sshd\x1b[0m 에 오신 것을 환영합니다\r\n\
                      ┌────────┬────────┐\r\n│ 이름   │ 상태   │\r\n├────────┼────────┤\r\n\
                      │ web01  │ \x1b[32m정상\x1b[0m   │\r\n│ db01   │ \x1b[31m점검\x1b[0m   │\r\n\
                      └────────┴────────┘\r\n$ ";
        session.data(channel, banner.as_bytes().to_vec())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if !self.shells.lock().unwrap().contains(&channel) {
            return Ok(());
        }
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

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).map(|i| args.get(i + 1).unwrap_or_else(|| panic!("{name} needs a value")).clone())
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let authorized = arg_value(&args, "--authorized-keys").map(|p| load_authorized(&p)).unwrap_or_default();
    let sftp_root = arg_value(&args, "--sftp-root").map(PathBuf::from).unwrap_or(std::env::current_dir()?);
    let shell_dir = arg_value(&args, "--shell-dir").unwrap_or_else(|| "/".into());
    let kbd = args.iter().any(|a| a == "--kbd-interactive");

    let config = Arc::new(server::Config {
        // Fixed seed: the host key stays the same across runs, so known_hosts keeps matching.
        keys: vec![PrivateKey::random(&mut StdRng::seed_from_u64(0x706c_6179), Algorithm::Ed25519).expect("key generation")],
        methods: methods(kbd),
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:2222").await?;
    println!(
        "dev_sshd listening on 127.0.0.1:2222 (tester / secret, {} authorized keys, sftp root {})",
        authorized.len(),
        sftp_root.display()
    );
    let server = DevServer {
        authorized: Arc::new(authorized),
        sftp_root,
        channels: Arc::default(),
        shells: Arc::default(),
        scripts: Arc::default(),
        shell_dir,
        line: Vec::new(),
        kbd,
        kbd_round: 0,
    };
    { server }.run_on_socket(config, &listener).await
}
