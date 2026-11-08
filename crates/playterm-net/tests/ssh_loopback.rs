//! End-to-end SSH tests against an in-process russh server on 127.0.0.1.

#[path = "common/certificate_agent.rs"]
mod certificate_agent;
mod common;

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use playterm_net::{
    AuthPrompt, ConnectParams, ForwardKind, ForwardRule, NetRuntime, Protocol, SessionEvent, SessionHandle, SftpConnection, SftpEvent,
    TransferState,
};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey, known_hosts};
use russh::server::{self, Auth, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Pty};
use tokio::net::TcpListener;

#[derive(Clone)]
struct TestServer {
    user_ca: Arc<Mutex<Option<PublicKey>>>,
    agent_events: Arc<Mutex<Vec<String>>>,
    allow_agent: Arc<std::sync::atomic::AtomicBool>,
    forward_address: Arc<Mutex<Option<String>>>,
    accepted_keys: Vec<PublicKey>,
    sftp_gate: Option<Arc<common::sftp_fs::TransferGate>>,
    auth_calls: Arc<Mutex<Vec<String>>>,
    /// Public keys accepted for user `tester`.
    keys: Arc<Vec<PublicKey>>,
    /// Directory served over SFTP.
    sftp_root: PathBuf,
    /// Session channels not yet claimed by a shell, so a subsystem request can take them.
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    /// Channels running the echo "shell"; other channels (SFTP, forwards) carry raw streams.
    shells: Arc<Mutex<std::collections::HashSet<ChannelId>>>,
    /// Exec channels waiting for their script (see `exec_request`).
    scripts: Arc<Mutex<std::collections::HashSet<ChannelId>>>,
}

/// What came back from this PC's display through the test server's X11 channel.
static X11_REPLY: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// An X11 connection setup packet (little endian) with this authorization.
fn x11_setup(name: &[u8], data: &[u8]) -> Vec<u8> {
    let pad = |n: usize| vec![0u8; (4 - n % 4) % 4];
    let mut out = vec![b'l', 0, 11, 0, 0, 0];
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(name);
    out.extend(pad(name.len()));
    out.extend_from_slice(data);
    out.extend(pad(data.len()));
    out
}

/// What the test server says the shell's current directory is.
const SHELL_DIR_ANSWER: &str = "/docs";

impl server::Server for TestServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        // Channel ids are per connection: each connection keeps its own channel sets (a
        // connection through a jump host would otherwise see the bastion's ids as its own).
        Self { accepted_keys: Vec::new(), channels: Arc::default(), shells: Arc::default(), scripts: Arc::default(), ..self.clone() }
    }
}

/// Like OpenSSH: after a failure, keep offering every method.
fn reject() -> Auth {
    let methods: &[MethodKind] = &[MethodKind::PublicKey, MethodKind::Password, MethodKind::KeyboardInteractive];
    Auth::Reject { proceed_with_methods: Some(MethodSet::from(methods)), partial_success: false }
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_openssh_certificate(&mut self, user: &str, cert: &russh::keys::Certificate) -> Result<Auth, Self::Error> {
        use russh::keys::HashAlg;
        self.auth_calls.lock().unwrap().push("certificate".into());
        let ca = self.user_ca.lock().unwrap().clone();
        let Some(ca) = ca else { return Ok(reject()) };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        if cert.cert_type() != russh::keys::ssh_key::certificate::CertType::User
            || !cert.valid_principals().iter().any(|p| p == user)
            || cert.validate_at(now, &[ca.fingerprint(HashAlg::Sha256)]).is_err()
        {
            return Ok(reject());
        }
        Ok(if user == "keyotp" {
            Auth::Reject { proceed_with_methods: Some(MethodSet::from(&[MethodKind::KeyboardInteractive][..])), partial_success: true }
        } else {
            Auth::Accept
        })
    }

    async fn agent_request(&mut self, _channel: ChannelId, _session: &mut Session) -> Result<bool, Self::Error> {
        self.agent_events.lock().unwrap().push("requested".into());
        Ok(self.allow_agent.load(std::sync::atomic::Ordering::SeqCst))
    }

    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        Ok(if user.starts_with("twokeys") {
            Auth::Reject { proceed_with_methods: Some(MethodSet::from(&[MethodKind::PublicKey][..])), partial_success: false }
        } else {
            reject()
        })
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.auth_calls.lock().unwrap().push("password".into());
        Ok(match (user, password) {
            ("tester", "secret") => Auth::Accept,
            // Password, then a one-time code (keyboard-interactive).
            ("otp", "secret") => {
                Auth::Reject { proceed_with_methods: Some(MethodSet::from(&[MethodKind::KeyboardInteractive][..])), partial_success: true }
            }
            _ => reject(),
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        self.auth_calls.lock().unwrap().push("keyboard-interactive".into());
        if user != "otp" && user != "keyotp" && !(user == "twokeysotp" && self.accepted_keys.len() == 2) {
            return Ok(reject());
        }
        Ok(match response {
            None => Auth::Partial { name: "".into(), instructions: "".into(), prompts: vec![("Verification code: ".into(), false)].into() },
            Some(mut answers) => {
                if answers.next().as_deref() == Some(b"123456".as_slice()) {
                    Auth::Accept
                } else {
                    Auth::Reject {
                        proceed_with_methods: Some(MethodSet::from(&[MethodKind::KeyboardInteractive][..])),
                        partial_success: false,
                    }
                }
            }
        })
    }

    async fn auth_publickey(&mut self, user: &str, key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        if user.starts_with("twokeys") {
            self.auth_calls.lock().unwrap().push(format!("publickey:{}", key.public_key.fingerprint(russh::keys::HashAlg::Sha256)));
            let known = self.keys.iter().any(|k| k.key_data() == key.public_key.key_data());
            if known && !self.accepted_keys.iter().any(|k| k.key_data() == key.public_key.key_data()) {
                self.accepted_keys.push(key.public_key.clone());
                if self.accepted_keys.len() == 2 && user == "twokeys" {
                    return Ok(Auth::Accept);
                }
                let next = if self.accepted_keys.len() == 2 { MethodKind::KeyboardInteractive } else { MethodKind::PublicKey };
                return Ok(Auth::Reject { proceed_with_methods: Some(MethodSet::from(&[next][..])), partial_success: true });
            }
            return Ok(Auth::Reject { proceed_with_methods: Some(MethodSet::from(&[MethodKind::PublicKey][..])), partial_success: false });
        }
        self.auth_calls.lock().unwrap().push("publickey".into());
        let known = self.keys.iter().any(|k| k.key_data() == key.public_key.key_data());
        Ok(if user == "keyotp" && known {
            Auth::Reject { proceed_with_methods: Some(MethodSet::from(&[MethodKind::KeyboardInteractive][..])), partial_success: true }
        } else if user == "tester" && known {
            Auth::Accept
        } else {
            reject()
        })
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
                let handler = common::sftp_fs::FsHandler::new(&self.sftp_root).with_transfer_gate(self.sftp_gate.clone());
                russh_sftp::server::run(channel.into_stream(), handler).await;
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
        // An unsolicited agent channel on a bastion must be rejected even if the
        // destination enabled forwarding, or the jump profile contained that option.
        probe_agent(_session.handle(), self.agent_events.clone());
        let Ok(mut tcp) = tokio::net::TcpStream::connect((host, port as u16)).await else {
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
        let bind = if address.is_empty() || address == "localhost" { "127.0.0.1" } else { address };
        let Ok(listener) = TcpListener::bind((bind, *port as u16)).await else { return Ok(false) };
        let address = self.forward_address.lock().unwrap().clone().unwrap_or_else(|| address.to_owned());
        let (handle, address, port) = (session.handle(), address, *port);
        tokio::spawn(async move {
            while let Ok((mut tcp, peer)) = listener.accept().await {
                let channel = handle.channel_open_forwarded_tcpip(address.clone(), port, peer.ip().to_string(), peer.port().into()).await;
                if let Ok(channel) = channel {
                    tokio::spawn(async move {
                        let _ = tokio::io::copy_bidirectional(&mut tcp, &mut channel.into_stream()).await;
                    });
                }
            }
        });
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, format!("pty {term} {cols}x{rows}\r\n").into_bytes())
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
            session.data(channel, format!("{}\n", SHELL_DIR_ANSWER).into_bytes())?;
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
        }
        Ok(())
    }

    async fn x11_request(
        &mut self,
        _channel: ChannelId,
        _single_connection: bool,
        protocol: &str,
        cookie: &str,
        _screen: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Like an X program on the server: connect back with the session's cookie, say "ping",
        // and keep what the display answers.
        let cookie: Vec<u8> = (0..cookie.len()).step_by(2).map(|i| u8::from_str_radix(&cookie[i..i + 2], 16).unwrap()).collect();
        let setup = [x11_setup(protocol.as_bytes(), &cookie), b"ping".to_vec()].concat();
        let handle = session.handle();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let channel = handle.channel_open_x11("127.0.0.1", 6010).await.unwrap();
            let mut stream = channel.into_stream();
            stream.write_all(&setup).await.unwrap();
            let mut reply = [0u8; 4];
            stream.read_exact(&mut reply).await.unwrap();
            X11_REPLY.lock().unwrap().extend_from_slice(&reply);
        });
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        // The shell is driven through the handler callbacks; release the channel object.
        self.channels.lock().unwrap().remove(&channel);
        self.shells.lock().unwrap().insert(channel);
        session.data(channel, b"welcome\r\n".to_vec())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if !self.shells.lock().unwrap().contains(&channel) {
            return Ok(());
        }
        if data == b"probe-agent" {
            probe_agent(session.handle(), self.agent_events.clone());
            return Ok(());
        }
        if data == b"hold-agent" {
            let handle = session.handle();
            let events = self.agent_events.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt as _;
                let channel = handle.channel_open_agent().await.unwrap();
                let mut agent = russh::keys::agent::client::AgentClient::connect(channel.into_stream());
                assert_eq!(agent.request_identities().await.unwrap().len(), 1);
                events.lock().unwrap().push("holding".into());
                let _ = agent.into_inner().read(&mut [0u8; 1]).await;
                events.lock().unwrap().push("relay-closed".into());
            });
            return Ok(());
        }
        session.data(channel, [b"echo:".as_slice(), data].concat())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, format!("resized {cols}x{rows}").into_bytes())
    }
}

struct Fixture {
    user_ca: Arc<Mutex<Option<PublicKey>>>,
    agent_events: Arc<Mutex<Vec<String>>>,
    allow_agent: Arc<std::sync::atomic::AtomicBool>,
    forward_address: Arc<Mutex<Option<String>>>,
    auth_calls: Arc<Mutex<Vec<String>>>,
    server_rt: tokio::runtime::Runtime,
    port: u16,
    host_key: PrivateKey,
    dir: PathBuf,
    known_hosts: PathBuf,
    net: NetRuntime,
}

fn random_key(alg: Algorithm) -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), alg).unwrap()
}

fn rsa_key() -> PrivateKey {
    let key = russh::keys::ssh_key::private::RsaKeypair::random(&mut rand::rng(), 2048).unwrap();
    PrivateKey::from(key)
}

fn probe_agent(handle: server::Handle, events: Arc<Mutex<Vec<String>>>) {
    tokio::spawn(async move {
        let result = match handle.channel_open_agent().await {
            Err(_) => "rejected",
            Ok(channel) => {
                let mut agent = russh::keys::agent::client::AgentClient::connect(channel.into_stream());
                let ids = agent.request_identities().await.unwrap();
                assert_eq!(ids.len(), 1);
                let signed = agent.sign_request(&ids[0], None, b"forwarded-signature".to_vec()).await.unwrap();
                assert!(signed.len() > b"forwarded-signature".len());
                "signed"
            }
        };
        events.lock().unwrap().push(result.into());
    });
}

fn wait_agent_event(fx: &Fixture, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fx.agent_events.lock().unwrap().iter().any(|event| event == expected) {
            return;
        }
        assert!(Instant::now() < deadline, "waiting for {expected}: {:?}", fx.agent_events.lock().unwrap());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn user_certificate(ca: &PrivateKey, key: &PrivateKey, user: &str, expired: bool) -> russh::keys::Certificate {
    use russh::keys::ssh_key::certificate::{Builder, CertType};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let mut builder =
        Builder::new_with_random_nonce(&mut rand::rng(), key.public_key(), now - 60, if expired { now - 1 } else { now + 3600 }).unwrap();
    builder.cert_type(CertType::User).unwrap().valid_principal(user).unwrap();
    builder.sign(ca).unwrap()
}

#[test]
fn user_certificate_files_authenticate_and_preserve_otp_and_rsa_policy() {
    let key = random_key(Algorithm::Ed25519);
    let ca = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("user-certificate", &[]);
    fx.trust_host();
    *fx.user_ca.lock().unwrap() = Some(ca.public_key().clone());
    let key_path = fx.write_key("identity", &key);
    let cert_path = fx.dir.join("identity-cert.pub");
    for explicit in [false, true] {
        std::fs::write(&cert_path, user_certificate(&ca, &key, "tester", false).to_openssh().unwrap()).unwrap();
        fx.auth_calls.lock().unwrap().clear();
        let h = fx.connect(ConnectParams {
            identity_file: Some(key_path.clone()),
            certificate_file: explicit.then(|| cert_path.clone()),
            ..fx.params()
        });
        expect_connected(&h);
        assert_eq!(fx.auth_calls.lock().unwrap().as_slice(), ["certificate"]);
        h.close();
    }
    std::fs::write(&cert_path, user_certificate(&ca, &key, "keyotp", false).to_openssh().unwrap()).unwrap();
    fx.auth_calls.lock().unwrap().clear();
    let h = fx.connect(ConnectParams {
        username: "keyotp".into(),
        identity_file: Some(key_path),
        certificate_file: Some(cert_path.clone()),
        password: Some("secret".into()),
        ..fx.params()
    });
    answer_key_otp(&h);
    assert!(!fx.auth_calls.lock().unwrap().iter().any(|v| v == "password"));
    h.close();

    let rsa = rsa_key();
    let rsa_path = fx.write_key("rsa", &rsa);
    std::fs::write(&cert_path, user_certificate(&ca, &rsa, "tester", false).to_openssh().unwrap()).unwrap();
    fx.auth_calls.lock().unwrap().clear();
    let h = fx.connect(ConnectParams {
        identity_file: Some(rsa_path),
        certificate_file: Some(cert_path),
        password: Some("secret".into()),
        ..fx.params()
    });
    expect_connected(&h);
    assert_eq!(fx.auth_calls.lock().unwrap().as_slice(), ["password"]);
}

#[test]
fn user_certificate_rejection_does_not_hide_bare_key_or_explicit_errors() {
    let key = random_key(Algorithm::Ed25519);
    let ca = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("certificate-rejection", &[&key]);
    fx.trust_host();
    *fx.user_ca.lock().unwrap() = Some(ca.public_key().clone());
    let key_path = fx.write_key("identity", &key);
    let cert_path = fx.dir.join("identity-cert.pub");
    for (user, expired) in [("wrong-principal", false), ("tester", true)] {
        std::fs::write(&cert_path, user_certificate(&ca, &key, user, expired).to_openssh().unwrap()).unwrap();
        fx.auth_calls.lock().unwrap().clear();
        let h = fx.connect(ConnectParams { identity_file: Some(key_path.clone()), ..fx.params() });
        expect_connected(&h);
        assert_eq!(fx.auth_calls.lock().unwrap().last().unwrap(), "publickey");
        h.close();
    }
    // An explicit rejected certificate must not silently submit the same key as a bare identity.
    fx.auth_calls.lock().unwrap().clear();
    let h = fx.connect(ConnectParams {
        identity_file: Some(key_path.clone()),
        certificate_file: Some(cert_path.clone()),
        password: Some("secret".into()),
        ..fx.params()
    });
    expect_connected(&h);
    assert!(!fx.auth_calls.lock().unwrap().iter().any(|v| v == "publickey"));
    h.close();
    // Parse failures are actionable connection errors, not unnoticed password fallback.
    std::fs::write(&cert_path, "not a certificate").unwrap();
    let h = fx.connect(ConnectParams {
        identity_file: Some(key_path),
        certificate_file: Some(cert_path),
        password: Some("secret".into()),
        ..fx.params()
    });
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
}

#[test]
fn explicit_user_certificate_can_use_a_bare_agent_identity() {
    let key = random_key(Algorithm::Ed25519);
    let ca = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("certificate-agent-bare", &[]);
    fx.trust_host();
    *fx.user_ca.lock().unwrap() = Some(ca.public_key().clone());
    let pipe = start_test_agent(&fx, &[&key], "certificate-agent-bare");
    let path = fx.dir.join("user-cert.pub");
    std::fs::write(&path, user_certificate(&ca, &key, "tester", false).to_openssh().unwrap()).unwrap();
    let h = fx.connect(ConnectParams { certificate_file: Some(path), use_agent: true, agent_pipe: Some(pipe), ..fx.params() });
    expect_connected(&h);
    assert_eq!(fx.auth_calls.lock().unwrap().as_slice(), ["certificate"]);
}

#[test]
fn agent_certificate_identities_precede_bare_keys_and_deduplicate_certificates() {
    // A rejected fixture must never fall back to the developer's real Unix agent.
    #[cfg(unix)]
    if std::env::var_os("PLAYTERM_CERT_AGENT_ISOLATED").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "agent_certificate_identities_precede_bare_keys_and_deduplicate_certificates", "--nocapture"])
            .env("PLAYTERM_CERT_AGENT_ISOLATED", "1")
            .env_remove("SSH_AUTH_SOCK")
            .status()
            .unwrap();
        assert!(result.success());
        return;
    }
    let key = random_key(Algorithm::Ed25519);
    let ca = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("agent-cert-identities", &[&key]);
    fx.trust_host();
    *fx.user_ca.lock().unwrap() = Some(ca.public_key().clone());
    let wrong = user_certificate(&ca, &key, "wrong-user", false);
    let good = user_certificate(&ca, &key, "tester", false);
    for (label, certs, expected) in [
        ("accepted", vec![wrong.clone(), wrong.clone(), good], vec!["certificate", "certificate"]),
        ("bare-fallback", vec![wrong.clone(), wrong], vec!["certificate", "publickey"]),
    ] {
        fx.auth_calls.lock().unwrap().clear();
        let pipe = certificate_agent::start(&fx, &key, certs, label, false);
        let h = fx.connect(ConnectParams { use_agent: true, agent_pipe: Some(pipe), ..fx.params() });
        expect_connected(&h);
        assert_eq!(*fx.auth_calls.lock().unwrap(), expected);
        h.close();
    }
    let otp = user_certificate(&ca, &key, "keyotp", false);
    let pipe = certificate_agent::start(&fx, &key, vec![otp], "otp", false);
    fx.auth_calls.lock().unwrap().clear();
    let h = fx.connect(ConnectParams {
        username: "keyotp".into(),
        use_agent: true,
        agent_pipe: Some(pipe),
        password: Some("secret".into()),
        ..fx.params()
    });
    answer_key_otp(&h);
    assert!(!fx.auth_calls.lock().unwrap().iter().any(|v| v == "password" || v == "publickey"));
    h.close();

    let pipe = certificate_agent::start(&fx, &key, vec![user_certificate(&ca, &key, "tester", false)], "refused", true);
    fx.auth_calls.lock().unwrap().clear();
    let h = fx.connect(ConnectParams {
        use_agent: true,
        agent_pipe: Some(pipe),
        default_identity_files: vec![fx.write_key("fallback-key", &key)],
        ..fx.params()
    });
    expect_connected(&h);
    assert_eq!(fx.auth_calls.lock().unwrap().as_slice(), ["publickey"]);
}

#[test]
fn agent_forwarding_requires_opt_in_and_isolated_destination() {
    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("agent-forwarding", &[]);
    fx.trust_host();
    let pipe = start_test_agent(&fx, &[&key], "forwarding");
    for enabled in [false, true] {
        fx.agent_events.lock().unwrap().clear();
        let h = fx.connect(ConnectParams {
            password: Some("secret".into()),
            agent_forwarding: enabled,
            agent_pipe: Some(pipe.clone()),
            ..fx.params()
        });
        expect_connected(&h);
        h.send(b"probe-agent".to_vec());
        wait_agent_event(&fx, if enabled { "signed" } else { "rejected" });
        assert_eq!(fx.agent_events.lock().unwrap().iter().filter(|v| *v == "requested").count(), usize::from(enabled));
        h.close();
    }
    fx.agent_events.lock().unwrap().clear();
    let jump = Fixture::new("agent-forwarding-jump", &[]);
    jump.trust_host();
    let h = fx.connect(ConnectParams {
        password: Some("secret".into()),
        agent_forwarding: true,
        agent_pipe: Some(pipe.clone()),
        jump: Some(Box::new(ConnectParams {
            password: Some("secret".into()),
            agent_forwarding: true,
            agent_pipe: Some(pipe.clone()),
            ..jump.params()
        })),
        ..fx.params()
    });
    expect_connected(&h);
    wait_agent_event(&jump, "rejected");
    h.send(b"probe-agent".to_vec());
    wait_agent_event(&fx, "signed");
    assert!(!jump.agent_events.lock().unwrap().iter().any(|v| v == "requested"));
    h.close();

    fx.allow_agent.store(false, std::sync::atomic::Ordering::SeqCst);
    let h = fx.connect(ConnectParams { password: Some("secret".into()), agent_forwarding: true, agent_pipe: Some(pipe), ..fx.params() });
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
}

#[test]
fn closing_ssh_aborts_idle_agent_relays() {
    use std::sync::atomic::Ordering;
    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("agent-forwarding-close", &[]);
    fx.trust_host();
    let (pipe, active) = certificate_agent::start_counted(&fx, &key, vec![], "forward-close", false);
    let h = fx.connect(ConnectParams { password: Some("secret".into()), agent_forwarding: true, agent_pipe: Some(pipe), ..fx.params() });
    expect_connected(&h);
    h.send(b"hold-agent".to_vec());
    wait_agent_event(&fx, "holding");
    assert!(active.load(Ordering::SeqCst) > 0);
    h.close();
    wait_agent_event(&fx, "relay-closed");
    let deadline = Instant::now() + Duration::from_secs(5);
    while active.load(Ordering::SeqCst) != 0 {
        assert!(Instant::now() < deadline, "local agent connection leaked after SSH close");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Clone)]
struct TestAgentPolicy {
    sign: bool,
}

impl russh::keys::agent::server::Agent for TestAgentPolicy {
    async fn confirm_request(&self, msg: russh::keys::agent::server::MessageType) -> bool {
        !matches!(msg, russh::keys::agent::server::MessageType::Sign) || self.sign
    }
}

#[cfg(windows)]
fn start_test_agent(fx: &Fixture, keys: &[&PrivateKey], label: &str) -> String {
    use futures::StreamExt as _;
    use russh::keys::agent::client::AgentClient;
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe = format!(r"\\.\pipe\playterm-two-keys-{}-{label}", std::process::id());
    fx.server_rt.block_on(async {
        let first = ServerOptions::new().first_pipe_instance(true).create(&pipe).unwrap();
        let connections = futures::stream::unfold((first, pipe.clone()), |(server, name)| async move {
            let accepted = server.connect().await.map(|()| server);
            let next = ServerOptions::new().create(&name).unwrap();
            Some((accepted, (next, name)))
        });
        tokio::spawn(russh::keys::agent::server::serve(connections.boxed(), TestAgentPolicy { sign: !label.starts_with("refuse") }));
        let mut client = AgentClient::connect_named_pipe(&pipe).await.unwrap();
        for key in keys {
            client.add_identity(key, &[]).await.unwrap();
        }
    });
    pipe
}

#[cfg(unix)]
fn start_test_agent(fx: &Fixture, keys: &[&PrivateKey], label: &str) -> String {
    use russh::keys::agent::client::AgentClient;
    use tokio::net::UnixListener;

    // macOS TMPDIR plus the fixture/label can exceed sockaddr_un.sun_path.
    // A private short directory also avoids predictable shared /tmp socket names.
    let socket_dir = tempfile::Builder::new().prefix("pta-").tempdir_in("/tmp").unwrap();
    let socket = socket_dir.path().join("agent");
    let policy = TestAgentPolicy { sign: !label.starts_with("refuse") };
    fx.server_rt.block_on(async {
        let listener = UnixListener::bind(&socket).unwrap();
        let connections = futures::stream::unfold(listener, |listener| async move {
            let accepted = listener.accept().await.map(|(stream, _)| stream);
            Some((accepted, listener))
        });
        tokio::spawn(async move {
            let _socket_dir = socket_dir;
            russh::keys::agent::server::serve(Box::pin(connections), policy).await
        });
        let mut client = AgentClient::connect_uds(&socket).await.unwrap();
        for key in keys {
            client.add_identity(key, &[]).await.unwrap();
        }
    });
    socket.to_string_lossy().into_owned()
}

impl Fixture {
    /// Server accepting password `secret` and the given public keys for user `tester`.
    fn new(name: &str, client_keys: &[&PrivateKey]) -> Self {
        Self::with_gate(name, client_keys, None)
    }

    fn with_gate(name: &str, client_keys: &[&PrivateKey], sftp_gate: Option<Arc<common::sftp_fs::TransferGate>>) -> Self {
        Self::with_host_identity(name, client_keys, sftp_gate, random_key(Algorithm::Ed25519), Vec::new())
    }

    fn with_host_identity(
        name: &str,
        client_keys: &[&PrivateKey],
        sftp_gate: Option<Arc<common::sftp_fs::TransferGate>>,
        host_key: PrivateKey,
        certificates: Vec<russh::keys::Certificate>,
    ) -> Self {
        Self::with_preferences(name, client_keys, sftp_gate, host_key, certificates, russh::Preferred::default())
    }

    fn with_preferences(
        name: &str,
        client_keys: &[&PrivateKey],
        sftp_gate: Option<Arc<common::sftp_fs::TransferGate>>,
        host_key: PrivateKey,
        certificates: Vec<russh::keys::Certificate>,
        preferred: russh::Preferred,
    ) -> Self {
        let server_rt = tokio::runtime::Runtime::new().unwrap();
        let config = Arc::new(server::Config {
            preferred,
            keys: vec![host_key.clone()],
            certificates,
            auth_rejection_time: Duration::from_millis(10),
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = server_rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("playterm-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sftp")).unwrap();
        let known_hosts = dir.join("known_hosts");

        let auth_calls = Arc::new(Mutex::new(Vec::new()));
        let user_ca = Arc::default();
        let agent_events = Arc::default();
        let allow_agent = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let forward_address = Arc::default();
        let server = TestServer {
            user_ca: Arc::clone(&user_ca),
            agent_events: Arc::clone(&agent_events),
            allow_agent: Arc::clone(&allow_agent),
            forward_address: Arc::clone(&forward_address),
            accepted_keys: Vec::new(),
            sftp_gate,
            auth_calls: auth_calls.clone(),
            keys: Arc::new(client_keys.iter().map(|k| k.public_key().clone()).collect()),
            sftp_root: dir.join("sftp"),
            channels: Arc::default(),
            shells: Arc::default(),
            scripts: Arc::default(),
        };
        server_rt.spawn(async move {
            let _ = { server }.run_on_socket(config, &listener).await;
        });

        Self {
            user_ca,
            agent_events,
            allow_agent,
            forward_address,
            auth_calls,
            server_rt,
            port,
            host_key,
            dir,
            known_hosts,
            net: NetRuntime::new().unwrap(),
        }
    }

    fn trust_host(&self) {
        known_hosts::learn_known_hosts_path("127.0.0.1", self.port, self.host_key.public_key(), &self.known_hosts).unwrap();
    }

    fn params(&self) -> ConnectParams {
        ConnectParams {
            protocol: Protocol::Ssh,
            host: "127.0.0.1".into(),
            port: self.port,
            username: "tester".into(),
            password: None,
            identity_file: None,
            default_identity_files: Vec::new(),
            use_agent: false,
            ssh_legacy_algorithms: false,
            certificate_file: None,
            agent_forwarding: false,
            agent_pipe: None,
            forwards: Vec::new(),
            serial: Default::default(),
            keepalive: Default::default(),
            rdp: Default::default(),
            desktop: (1024, 768),
            term_type: "xterm-256color".into(),
            cols: 80,
            rows: 24,
            known_hosts: self.known_hosts.clone(),
            connect_timeout: ConnectParams::DEFAULT_CONNECT_TIMEOUT,
            jump: None,
            x11: None,
        }
    }

    fn connect(&self, params: ConnectParams) -> SessionHandle {
        self.net.connect(params, Arc::new(|| {}))
    }

    fn write_key(&self, file: &str, key: &PrivateKey) -> PathBuf {
        let path = self.dir.join(file);
        std::fs::write(&path, key.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
        path
    }
}

#[test]
fn legacy_algorithms_require_site_opt_in_and_exchange_data() {
    use std::borrow::Cow;
    for (name, kex) in [("legacy-g14", russh::kex::DH_G14_SHA1), ("legacy-g1", russh::kex::DH_G1_SHA1)] {
        let preferred = russh::Preferred {
            kex: Cow::Owned(vec![kex]),
            cipher: Cow::Owned(vec![russh::cipher::AES_128_CBC]),
            mac: Cow::Owned(vec![russh::mac::HMAC_SHA1]),
            ..Default::default()
        };
        let fx = Fixture::with_preferences(name, &[], None, random_key(Algorithm::Ed25519), Vec::new(), preferred);
        fx.trust_host();
        let mut params = fx.params();
        params.password = Some("secret".into());
        let rejected = fx.connect(params.clone());
        assert!(matches!(skip_notices(&rejected), SessionEvent::Closed(Some(_))));
        params.ssh_legacy_algorithms = true;
        let accepted = fx.connect(params);
        assert!(matches!(skip_notices(&accepted), SessionEvent::Connected));
        accepted.send(b"legacy-echo\r\n".to_vec());
        assert!(read_until(&accepted, "legacy-echo").contains("legacy-echo"));
        accepted.close();
    }
}

fn next_event(h: &SessionHandle) -> SessionEvent {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(e) = h.try_recv() {
            return e;
        }
        assert!(Instant::now() < deadline, "timed out waiting for session event");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Collects terminal output until `needle` shows up. Notices are ignored.
fn read_until(h: &SessionHandle, needle: &str) -> String {
    let mut out = String::new();
    while !out.contains(needle) {
        match next_event(h) {
            SessionEvent::Data(d) => out.push_str(&String::from_utf8_lossy(&d)),
            SessionEvent::Notice(_) => {}
            other => panic!("unexpected event while waiting for {needle:?}: {other:?}; got so far {out:?}"),
        }
    }
    out
}

fn skip_notices(h: &SessionHandle) -> SessionEvent {
    loop {
        match next_event(h) {
            SessionEvent::Notice(_) => {}
            other => return other,
        }
    }
}

fn expect_prompt(h: &SessionHandle) -> AuthPrompt {
    match skip_notices(h) {
        SessionEvent::AuthPrompt(p) => p,
        other => panic!("expected a prompt, got {other:?}"),
    }
}

fn expect_connected(h: &SessionHandle) {
    match skip_notices(h) {
        SessionEvent::Connected => {}
        other => panic!("expected Connected, got {other:?}"),
    }
    read_until(h, "welcome");
}

// -------------------------------------------------------------------------------------------------
// Host keys and passwords

fn host_certificate_fixture(name: &str, principal: &str, expired: bool) -> (Fixture, PrivateKey) {
    use russh::keys::ssh_key::certificate::{Builder, CertType};
    let ca = random_key(Algorithm::Ed25519);
    let host = random_key(Algorithm::Ed25519);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let before = if expired { now - 1 } else { now + 3600 };
    let mut builder = Builder::new_with_random_nonce(&mut rand::rng(), host.public_key(), now - 60, before).unwrap();
    builder.cert_type(CertType::Host).unwrap().valid_principal(principal).unwrap();
    let certificate = builder.sign(&ca).unwrap();
    (Fixture::with_host_identity(name, &[], None, host, vec![certificate]), ca)
}

#[test]
fn host_certificate_negotiates_and_never_falls_back_to_tofu() {
    for scenario in ["valid", "wildcard", "wrong-ca", "expired", "wrong-host", "no-ca"] {
        let principal = match scenario {
            "wrong-host" => "wrong.example",
            "wildcard" => "127.0.*.?",
            _ => "127.0.0.1",
        };
        let (fx, ca) = host_certificate_fixture(&format!("host-cert-{scenario}"), principal, scenario == "expired");
        let ca = if scenario == "wrong-ca" { random_key(Algorithm::Ed25519) } else { ca };
        let trust = if scenario == "no-ca" {
            String::new()
        } else {
            format!("@cert-authority [127.0.0.1]:{} {}\n", fx.port, ca.public_key().to_openssh().unwrap())
        };
        std::fs::write(&fx.known_hosts, &trust).unwrap();
        let handle = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
        if matches!(scenario, "valid" | "wildcard") {
            expect_connected(&handle);
            handle.close();
        } else {
            // The server offers both certificate and bare key. Every certificate
            // failure, including absent CA trust, closes without a TOFU question.
            match skip_notices(&handle) {
                SessionEvent::Closed(Some(_)) => {}
                other => panic!("{scenario}: certificate failure did not close: {other:?}"),
            }
        }
        assert_eq!(std::fs::read_to_string(&fx.known_hosts).unwrap(), trust, "certificate validation never learns a bare key: {scenario}");
    }
}

#[test]
fn configured_ca_refuses_a_bare_key_downgrade_without_question() {
    let fx = Fixture::new("cert-downgrade", &[]);
    let ca = random_key(Algorithm::Ed25519);
    let trust = format!("@cert-authority [127.0.0.1]:{} {}\n", fx.port, ca.public_key().to_openssh().unwrap());
    std::fs::write(&fx.known_hosts, &trust).unwrap();
    let handle = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    assert!(matches!(skip_notices(&handle), SessionEvent::Closed(Some(_))));
    assert_eq!(std::fs::read_to_string(&fx.known_hosts).unwrap(), trust);
}

#[test]
fn first_connect_asks_for_host_key_then_runs_shell() {
    let fx = Fixture::new("first", &[]);
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });

    let SessionEvent::HostKeyCheck(check) = skip_notices(&h) else { panic!("expected host key check") };
    assert_eq!(check.port, fx.port);
    assert_eq!(check.algorithm, "ssh-ed25519");
    assert_eq!(check.fingerprint, fx.host_key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string());
    check.answer(true);

    assert!(matches!(skip_notices(&h), SessionEvent::Connected));
    let banner = read_until(&h, "welcome");
    assert!(banner.contains("pty xterm-256color 80x24"), "{banner:?}");

    h.send(b"hi".to_vec());
    read_until(&h, "echo:hi");
    h.resize(100, 30);
    read_until(&h, "resized 100x30");

    h.close();
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(None)));

    // The key was recorded; a second connection must not ask again.
    assert!(known_hosts::check_known_hosts_path("127.0.0.1", fx.port, fx.host_key.public_key(), &fx.known_hosts).unwrap());
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
}

#[test]
fn shell_directory_is_asked_on_a_second_channel() {
    let fx = Fixture::new("shell-dir", &[]);
    fx.trust_host();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    h.query_shell_dir();
    loop {
        match next_event(&h) {
            SessionEvent::ShellDir(dir) => {
                assert_eq!(dir.as_deref(), Some(SHELL_DIR_ANSWER));
                break;
            }
            SessionEvent::Data(_) | SessionEvent::Notice(_) => {}
            other => panic!("expected the shell directory, got {other:?}"),
        }
    }
    // Nothing of it shows in the terminal: the shell still answers as before.
    h.send(b"x".to_vec());
    read_until(&h, "echo:x");
}

#[test]
fn silent_server_times_out() {
    // Accepts TCP connections (the backlog) but never says a word of SSH.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let fx = Fixture::new("silent", &[]);
    let params = ConnectParams {
        port: silent.local_addr().unwrap().port(),
        connect_timeout: Duration::from_secs(1),
        jump: None,
        x11: None,
        ..fx.params()
    };
    let started = Instant::now();
    let h = fx.connect(params);
    let SessionEvent::Closed(Some(reason)) = skip_notices(&h) else { panic!("expected a timeout") };
    assert!(reason.contains("1"), "{reason}");
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(1) && took < Duration::from_secs(5), "{took:?}");
}

#[test]
fn time_spent_on_prompts_does_not_count() {
    let fx = Fixture::new("slow-user", &[]);
    let h = fx.connect(ConnectParams { connect_timeout: Duration::from_secs(1), ..fx.params() });
    // The user takes longer than the whole limit over the host key and then the password.
    let SessionEvent::HostKeyCheck(check) = skip_notices(&h) else { panic!("expected host key check") };
    std::thread::sleep(Duration::from_millis(1500));
    check.answer(true);
    let prompt = expect_prompt(&h);
    std::thread::sleep(Duration::from_millis(1500));
    prompt.answer(Some(vec!["secret".into()]));
    expect_connected(&h);
}

#[test]
fn host_key_of_another_type_is_a_changed_key() {
    // The host is on record with an ECDSA key; the server (or a man in the middle) only
    // offers Ed25519. That must be refused as a changed key, not asked as a first visit.
    let fx = Fixture::new("other-type", &[]);
    let recorded = random_key(Algorithm::Ecdsa { curve: russh::keys::EcdsaCurve::NistP256 });
    known_hosts::learn_known_hosts_path("127.0.0.1", fx.port, recorded.public_key(), &fx.known_hosts).unwrap();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    match skip_notices(&h) {
        SessionEvent::Closed(Some(reason)) => assert!(reason.contains("ecdsa") && reason.contains("ssh-ed25519"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn second_factor_is_not_the_password() {
    // Password accepted, then a one-time code: the saved password must not be sent as the
    // code, and the code prompt is not offered for saving as the password.
    let fx = Fixture::new("otp", &[]);
    fx.trust_host();
    let h = fx.connect(ConnectParams { username: "otp".into(), password: Some("secret".into()), ..fx.params() });
    let prompt = expect_prompt(&h);
    assert_eq!(prompt.fields.len(), 1);
    assert!(prompt.fields[0].label.contains("Verification code"), "{:?}", prompt.fields);
    assert!(!prompt.account_password, "a one-time code is not the account password");
    prompt.answer(Some(vec!["123456".into()]));
    expect_connected(&h);
}

fn answer_key_otp(h: &SessionHandle) {
    let prompt = expect_prompt(h);
    assert!(!prompt.account_password);
    assert!(prompt.fields[0].label.contains("Verification code"));
    prompt.answer(Some(vec!["123456".into()]));
    expect_connected(h);
}

#[test]
fn public_key_second_factor_retries_and_cancels_without_using_saved_password() {
    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("key-otp", &[&key]);
    fx.trust_host();
    let path = fx.write_key("identity", &key);
    let params = ConnectParams { username: "keyotp".into(), identity_file: Some(path), password: Some("secret".into()), ..fx.params() };
    let h = fx.connect(params.clone());
    let prompt = expect_prompt(&h);
    assert!(!prompt.account_password);
    assert_eq!(fx.auth_calls.lock().unwrap().as_slice(), ["publickey", "keyboard-interactive"]);
    prompt.answer(Some(vec!["wrong-code".into()]));
    answer_key_otp(&h);
    let h = fx.connect(params.clone());
    expect_prompt(&h).answer(None);
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    answer_key_otp(&fx.connect(params));
    assert!(!fx.auth_calls.lock().unwrap().iter().any(|m| m == "password"));
}

#[test]
fn wrong_password_falls_back_to_prompt() {
    let fx = Fixture::new("prompt", &[]);
    fx.trust_host();

    let h = fx.connect(ConnectParams { password: Some("wrong".into()), ..fx.params() });
    // The test server advertises keyboard-interactive but rejects it silently; the client
    // must then ask for a password instead of giving up.
    let prompt = expect_prompt(&h);
    assert_eq!(prompt.fields.len(), 1);
    assert!(!prompt.fields[0].echo);
    assert_eq!(prompt.user.as_deref(), Some("tester"), "a login prompt says whose password it wants");
    assert!(prompt.account_password, "the password itself may be saved");
    prompt.answer(Some(vec!["secret".into()]));
    expect_connected(&h);
}

#[test]
fn cancelled_prompt_closes_session() {
    let fx = Fixture::new("cancel", &[]);
    fx.trust_host();

    let h = fx.connect(fx.params());
    expect_prompt(&h).answer(None);
    let SessionEvent::Closed(Some(reason)) = skip_notices(&h) else { panic!("expected close with error") };
    assert!(reason.contains("취소"), "{reason}");
}

#[test]
fn changed_host_key_is_refused_without_prompt() {
    let fx = Fixture::new("changed", &[]);
    // Pretend we knew a different key for this host:port before.
    let old = random_key(Algorithm::Ed25519);
    known_hosts::learn_known_hosts_path("127.0.0.1", fx.port, old.public_key(), &fx.known_hosts).unwrap();

    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    let SessionEvent::Closed(Some(reason)) = skip_notices(&h) else { panic!("expected refusal") };
    assert!(reason.contains("호스트 키가 변경"), "{reason}");
}

#[test]
fn rejected_host_key_closes_session() {
    let fx = Fixture::new("reject", &[]);
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    let SessionEvent::HostKeyCheck(check) = skip_notices(&h) else { panic!("expected host key check") };
    check.answer(false);
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    assert!(!fx.known_hosts.exists());
}

// -------------------------------------------------------------------------------------------------
// Public keys

#[test]
fn key_files_authenticate_without_prompts() {
    let ed = random_key(Algorithm::Ed25519);
    let ecdsa = random_key(Algorithm::Ecdsa { curve: russh::keys::EcdsaCurve::NistP256 });
    let fx = Fixture::new("keyfile", &[&ed, &ecdsa]);
    fx.trust_host();

    let ed_path = fx.write_key("id_ed25519", &ed);
    let h = fx.connect(ConnectParams { identity_file: Some(ed_path), ..fx.params() });
    expect_connected(&h);

    let ecdsa_path = fx.write_key("id_ecdsa", &ecdsa);
    let h = fx.connect(ConnectParams { identity_file: Some(ecdsa_path), ..fx.params() });
    expect_connected(&h);
}

#[test]
fn local_rsa_files_are_blocked_by_algorithm_before_explicit_or_default_authentication() {
    let rsa = rsa_key();
    let ed = random_key(Algorithm::Ed25519);
    let wrong = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("rsa-file-policy", &[&rsa, &ed]);
    fx.trust_host();
    let wrong_path = fx.write_key("unaccepted-ed", &wrong);
    for name in ["id_rsa", "id_ed25519", "arbitrary-name"] {
        let path = fx.write_key(name, &rsa);
        let before = std::fs::read(&path).unwrap();
        for explicit in [true, false] {
            fx.auth_calls.lock().unwrap().clear();
            let handle = fx.connect(ConnectParams {
                identity_file: Some(if explicit { path.clone() } else { wrong_path.clone() }),
                default_identity_files: vec![path.clone()],
                ..fx.params()
            });
            let mut warned = false;
            loop {
                match next_event(&handle) {
                    SessionEvent::Notice(text) => warned |= text.contains("RUSTSEC-2023-0071"),
                    SessionEvent::AuthPrompt(prompt) => {
                        assert!(warned);
                        assert!(prompt.account_password);
                        let expected = if explicit { 0 } else { 1 };
                        assert_eq!(
                            fx.auth_calls.lock().unwrap().iter().filter(|call| call.starts_with("publickey")).count(),
                            expected,
                            "no RSA signature reaches the server"
                        );
                        prompt.answer(Some(vec!["secret".into()]));
                        break;
                    }
                    other => panic!("RSA file must be skipped before signing: {other:?}"),
                }
            }
            expect_connected(&handle);
            handle.close();
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
    // A non-RSA key named id_rsa is still allowed.
    let path = fx.write_key("id_rsa", &ed);
    let handle = fx.connect(ConnectParams { identity_file: Some(path), ..fx.params() });
    expect_connected(&handle);
    handle.close();
}

#[test]
fn partial_success_does_not_fall_back_to_a_local_rsa_signature() {
    let ed = random_key(Algorithm::Ed25519);
    let rsa = rsa_key();
    let fx = Fixture::new("rsa-partial-policy", &[&ed, &rsa]);
    fx.trust_host();
    let ed_path = fx.write_key("ed", &ed);
    let rsa_path = fx.write_key("id_rsa", &rsa);
    let options =
        ConnectParams { username: "twokeys".into(), identity_file: Some(ed_path), default_identity_files: vec![rsa_path], ..fx.params() };
    // A new attempt and a reconnect both preserve the policy and partial-success state.
    for _ in 0..2 {
        fx.auth_calls.lock().unwrap().clear();
        let handle = fx.connect(options.clone());
        assert!(matches!(skip_notices(&handle), SessionEvent::Closed(Some(_))));
        assert_eq!(
            fx.auth_calls.lock().unwrap().as_slice(),
            [format!("publickey:{}", ed.public_key().fingerprint(russh::keys::HashAlg::Sha256))]
        );
    }
}

#[test]
fn rsa_agent_paths_keep_local_file_policy() {
    // Do not consult the developer's real SSH_AUTH_SOCK after a test agent fails.
    if std::env::var_os("PLAYTERM_RSA_AGENT_WORKER").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "rsa_agent_paths_keep_local_file_policy", "--nocapture"])
            .env("PLAYTERM_RSA_AGENT_WORKER", "1")
            .env_remove("SSH_AUTH_SOCK")
            .status()
            .unwrap();
        assert!(result.success());
        return;
    }
    let rsa = rsa_key();
    let wrong = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("rsa-agents", &[&rsa]);
    fx.trust_host();
    let rsa_path = fx.write_key("renamed-rsa-key", &rsa);
    for mode in ["accept", "refuse", "wrong", "missing"] {
        let pipe = match mode {
            "accept" => start_test_agent(&fx, &[&rsa], "rsa-accept"),
            "refuse" => start_test_agent(&fx, &[&rsa], "refuse-rsa"),
            "wrong" => start_test_agent(&fx, &[&wrong], "rsa-wrong"),
            _ => {
                #[cfg(windows)]
                {
                    format!(r"\\.\pipe\playterm-missing-rsa-agent-{}", std::process::id())
                }
                #[cfg(unix)]
                {
                    fx.dir.join("missing-agent").to_string_lossy().into_owned()
                }
            }
        };
        fx.auth_calls.lock().unwrap().clear();
        let handle = fx.connect(ConnectParams {
            identity_file: Some(rsa_path.clone()),
            default_identity_files: vec![rsa_path.clone()],
            use_agent: true,
            ssh_legacy_algorithms: false,
            certificate_file: None,
            agent_forwarding: false,
            agent_pipe: Some(pipe),
            ..fx.params()
        });
        if mode == "accept" {
            expect_connected(&handle);
            assert_eq!(fx.auth_calls.lock().unwrap().as_slice(), ["publickey"]);
        } else {
            let prompt = expect_prompt(&handle);
            assert!(prompt.account_password);
            assert_eq!(
                fx.auth_calls.lock().unwrap().iter().filter(|call| call.starts_with("publickey")).count(),
                usize::from(mode == "wrong")
            );
            prompt.answer(Some(vec!["secret".into()]));
            expect_connected(&handle);
        }
        handle.close();
    }
}

#[test]
fn rsa_file_policy_applies_to_both_jump_and_target() {
    let rsa = rsa_key();
    let ed = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("rsa-jump-policy", &[&rsa, &ed]);
    fx.trust_host();
    let rsa_path = fx.write_key("rsa", &rsa);
    let ed_path = fx.write_key("ed", &ed);
    for rsa_on_jump in [true, false] {
        fx.auth_calls.lock().unwrap().clear();
        let jump = ConnectParams { identity_file: Some(if rsa_on_jump { rsa_path.clone() } else { ed_path.clone() }), ..fx.params() };
        let target = ConnectParams {
            identity_file: Some(if rsa_on_jump { ed_path.clone() } else { rsa_path.clone() }),
            jump: Some(Box::new(jump)),
            ..fx.params()
        };
        let handle = fx.connect(target);
        let prompt = expect_prompt(&handle);
        assert!(prompt.account_password);
        assert_eq!(fx.auth_calls.lock().unwrap().iter().filter(|call| call.starts_with("publickey")).count(), usize::from(!rsa_on_jump));
        prompt.answer(Some(vec!["secret".into()]));
        expect_connected(&handle);
        assert_eq!(
            fx.auth_calls.lock().unwrap().iter().filter(|call| call.starts_with("publickey") || call.as_str() == "password").count(),
            2
        );
        handle.close();
    }
}

#[test]
fn encrypted_rsa_is_refused_after_decryption_without_signing() {
    let rsa = rsa_key();
    let fx = Fixture::new("rsa-encrypted-policy", &[&rsa]);
    fx.trust_host();
    let path = fx.dir.join("disguised-encrypted-key");
    let encrypted = rsa.encrypt(&mut rand::rng(), "test-passphrase").unwrap();
    std::fs::write(&path, encrypted.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
    let handle = fx.connect(ConnectParams { identity_file: Some(path), ..fx.params() });
    let passphrase = expect_prompt(&handle);
    assert!(!passphrase.account_password);
    passphrase.answer(Some(vec!["test-passphrase".into()]));
    let password = expect_prompt(&handle);
    assert!(password.account_password);
    assert!(!fx.auth_calls.lock().unwrap().iter().any(|call| call.starts_with("publickey")));
    password.answer(None);
    assert!(matches!(skip_notices(&handle), SessionEvent::Closed(Some(_))));
}

#[test]
fn two_public_keys_continue_across_files_and_skip_duplicates() {
    let a = random_key(Algorithm::Ed25519);
    let b = random_key(Algorithm::Ed25519);
    let wrong = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("two-key-files", &[&a, &b]);
    fx.trust_host();
    let a_path = fx.write_key("a", &a);
    let duplicate_a = fx.write_key("a-copy", &a);
    let b_path = fx.write_key("b", &b);
    let wrong_path = fx.write_key("wrong", &wrong);
    for explicit in [true, false] {
        fx.auth_calls.lock().unwrap().clear();
        let h = fx.connect(ConnectParams {
            username: "twokeys".into(),
            identity_file: explicit.then(|| a_path.clone()),
            default_identity_files: vec![wrong_path.clone(), a_path.clone(), duplicate_a.clone(), wrong_path.clone(), b_path.clone()],
            ..fx.params()
        });
        expect_connected(&h);
        h.close();
        let calls = fx.auth_calls.lock().unwrap().clone();
        let expected = if explicit { [&a, &wrong, &b] } else { [&wrong, &a, &b] };
        assert_eq!(calls, expected.map(|k| format!("publickey:{}", k.public_key().fingerprint(russh::keys::HashAlg::Sha256))));
    }
}

#[test]
fn one_public_key_repeated_is_not_two_factors() {
    let a = random_key(Algorithm::Ed25519);
    let b = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("one-key-only", &[&a, &b]);
    fx.trust_host();
    let path = fx.write_key("a", &a);
    let h = fx.connect(ConnectParams {
        username: "twokeys".into(),
        identity_file: Some(path.clone()),
        default_identity_files: vec![path.clone(), fx.write_key("duplicate", &a), path],
        ..fx.params()
    });
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    assert_eq!(fx.auth_calls.lock().unwrap().len(), 1);
}

#[test]
fn two_public_keys_then_otp_preserve_cancel_retry_and_saved_password_protection() {
    let a = random_key(Algorithm::Ed25519);
    let b = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("two-key-otp", &[&a, &b]);
    fx.trust_host();
    let params = ConnectParams {
        username: "twokeysotp".into(),
        identity_file: Some(fx.write_key("a", &a)),
        default_identity_files: vec![fx.write_key("b", &b)],
        password: Some("123456".into()), // Correct factor must still prompt.
        ..fx.params()
    };
    let h = fx.connect(params.clone());
    let prompt = expect_prompt(&h);
    assert!(!prompt.account_password);
    prompt.answer(Some(vec!["wrong".into()]));
    answer_key_otp(&h);
    let h = fx.connect(params.clone());
    expect_prompt(&h).answer(None);
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    answer_key_otp(&fx.connect(params));
    assert!(!fx.auth_calls.lock().unwrap().iter().any(|s| s == "password"));
}

#[cfg(any(windows, unix))]
#[test]
fn two_public_keys_continue_within_agents_and_across_file_agent_sources() {
    let a = random_key(Algorithm::Ed25519);
    let b = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("two-key-agents", &[&a, &b]);
    fx.trust_host();
    let both = start_test_agent(&fx, &[&a, &b], "both");
    let only_a = start_test_agent(&fx, &[&a], "only-a");
    let refusing_b = start_test_agent(&fx, &[&b], "refuse");
    let a_path = fx.write_key("a", &a);
    let b_path = fx.write_key("b", &b);
    for (identity_file, agent, defaults) in [
        (None, &both, vec![]),
        (Some(a_path.clone()), &both, vec![]),
        (None, &only_a, vec![a_path.clone(), b_path.clone()]),
        (Some(a_path.clone()), &refusing_b, vec![b_path]),
    ] {
        fx.auth_calls.lock().unwrap().clear();
        let params = ConnectParams {
            username: "twokeys".into(),
            identity_file,
            use_agent: true,
            ssh_legacy_algorithms: false,
            certificate_file: None,
            agent_forwarding: false,
            agent_pipe: Some(agent.clone()),
            default_identity_files: defaults,
            ..fx.params()
        };
        // A fresh connection must get a fresh deduplication set, including reconnects.
        for _ in 0..2 {
            let h = fx.connect(params.clone());
            expect_connected(&h);
            h.close();
        }
        assert_eq!(fx.auth_calls.lock().unwrap().len(), 4, "each connection submits two unique keys");
    }
    let h = fx.connect(ConnectParams {
        username: "twokeys".into(),
        identity_file: Some(a_path.clone()),
        use_agent: true,
        ssh_legacy_algorithms: false,
        certificate_file: None,
        agent_forwarding: false,
        agent_pipe: Some(only_a),
        default_identity_files: vec![a_path],
        ..fx.params()
    });
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    answer_key_otp(&fx.connect(ConnectParams {
        username: "twokeysotp".into(),
        use_agent: true,
        ssh_legacy_algorithms: false,
        certificate_file: None,
        agent_forwarding: false,
        agent_pipe: Some(both),
        password: Some("123456".into()),
        ..fx.params()
    }));
}

#[cfg(unix)]
#[test]
fn two_public_keys_continue_between_unix_agents() {
    if let Some(dir) = std::env::var_os("PLAYTERM_TWO_AGENT_CHILD") {
        let dir = PathBuf::from(dir);
        let a = russh::keys::load_secret_key(dir.join("a"), None).unwrap();
        let b = russh::keys::load_secret_key(dir.join("b"), None).unwrap();
        let fx = Fixture::new("two-agent-child", &[&a, &b]);
        fx.trust_host();
        expect_connected(&fx.connect(ConnectParams {
            username: "twokeys".into(),
            use_agent: true,
            ssh_legacy_algorithms: false,
            certificate_file: None,
            agent_forwarding: false,
            agent_pipe: Some(std::env::var("PLAYTERM_FIRST_AGENT").unwrap()),
            ..fx.params()
        }));
        assert_eq!(
            fx.auth_calls.lock().unwrap().iter().filter(|call| call.starts_with("publickey") || call.as_str() == "password").count(),
            2
        );
        return;
    }
    let a = random_key(Algorithm::Ed25519);
    let b = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("two-agent-parent", &[&a, &b]);
    fx.write_key("a", &a);
    fx.write_key("b", &b);
    let first = start_test_agent(&fx, &[&a], "first");
    let second = start_test_agent(&fx, &[&a, &b], "second");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "two_public_keys_continue_between_unix_agents", "--nocapture"])
        .env("PLAYTERM_TWO_AGENT_CHILD", &fx.dir)
        .env("PLAYTERM_FIRST_AGENT", first)
        .env("SSH_AUTH_SOCK", second)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn two_public_keys_are_independent_for_each_jump_and_new_connection() {
    let a = random_key(Algorithm::Ed25519);
    let b = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("two-key-jump", &[&a, &b]);
    fx.trust_host();
    let direct = ConnectParams {
        username: "twokeys".into(),
        identity_file: Some(fx.write_key("a", &a)),
        default_identity_files: vec![fx.write_key("b", &b)],
        ..fx.params()
    };
    expect_connected(&fx.connect(ConnectParams { jump: Some(Box::new(direct.clone())), ..direct }));
    assert_eq!(fx.auth_calls.lock().unwrap().len(), 4);
}

#[test]
fn second_key_passphrase_cancel_aborts_but_skip_allows_the_next_source() {
    let a = random_key(Algorithm::Ed25519);
    let b = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("two-key-cancel", &[&a, &b]);
    fx.trust_host();
    let encrypted = b.encrypt(&mut rand::rng(), "open sesame").unwrap();
    let params = ConnectParams {
        username: "twokeys".into(),
        identity_file: Some(fx.write_key("a", &a)),
        default_identity_files: vec![fx.write_key("encrypted-b", &encrypted), fx.write_key("plain-b", &b)],
        password: Some("secret".into()),
        ..fx.params()
    };
    let h = fx.connect(params.clone());
    expect_prompt(&h).answer(None);
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    assert_eq!(fx.auth_calls.lock().unwrap().len(), 1, "only A was submitted before cancellation");
    fx.auth_calls.lock().unwrap().clear();
    let h = fx.connect(params);
    let prompt = expect_prompt(&h);
    assert!(prompt.can_skip && !prompt.account_password);
    prompt.skip();
    expect_connected(&h);
    assert_eq!(fx.auth_calls.lock().unwrap().iter().filter(|call| call.starts_with("publickey") || call.as_str() == "password").count(), 2);
}

#[test]
fn forwards_x11_to_the_display() {
    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("x11", &[&key]);
    fx.trust_host();
    let path = fx.write_key("id_ed25519", &key);
    // A fake X server; its display number is its port - 6000.
    let display = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let number = display.local_addr().unwrap().port() - 6000;
    let h = fx.connect(ConnectParams {
        identity_file: Some(path),
        // Trusted: this display (a fake one) cannot make untrusted cookies.
        x11: Some(playterm_net::X11Request { display: format!("127.0.0.1:{number}.0"), trusted: true }),
        ..fx.params()
    });
    expect_connected(&h);
    display.set_nonblocking(false).unwrap();
    let (mut x, _) = display.accept().unwrap();
    x.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    // The server's made-up cookie is gone: this display has no xauth entry, so none is passed.
    let expected = [x11_setup(b"", b""), b"ping".to_vec()].concat();
    let mut got = vec![0u8; expected.len()];
    x.read_exact(&mut got).unwrap();
    assert_eq!(got, expected);
    x.write_all(b"pong").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while X11_REPLY.lock().unwrap().as_slice() != b"pong" {
        assert!(Instant::now() < deadline, "the reply did not come back");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn connects_through_jump_hosts() {
    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("jump", &[&key]);
    fx.trust_host();
    let path = fx.write_key("id_ed25519", &key);
    let direct = ConnectParams { identity_file: Some(path), ..fx.params() };

    // The test server is its own bastion: SSH to it, then a direct-tcpip channel back to it
    // carries the second SSH session.
    let one_hop = ConnectParams { jump: Some(Box::new(direct.clone())), ..direct.clone() };
    expect_connected(&fx.connect(one_hop.clone()));

    // Two hops: A → B → target.
    let two_hops = ConnectParams { jump: Some(Box::new(one_hop)), ..direct.clone() };
    expect_connected(&fx.connect(two_hops));

    // A target the bastion cannot reach is reported, not hung on.
    let unreachable = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = unreachable.local_addr().unwrap().port();
    drop(unreachable);
    let h = fx.connect(ConnectParams { port, jump: Some(Box::new(direct)), ..fx.params() });
    match skip_notices(&h) {
        SessionEvent::Closed(Some(reason)) => assert!(reason.contains(&port.to_string()), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn unknown_key_then_default_key_then_password() {
    let accepted = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("order", &[&accepted]);
    fx.trust_host();

    // An explicit key the server rejects, a missing default, then the accepted default key.
    let rejected = fx.write_key("other", &random_key(Algorithm::Ed25519));
    let good = fx.write_key("id_ed25519", &accepted);
    let params =
        ConnectParams { identity_file: Some(rejected.clone()), default_identity_files: vec![fx.dir.join("missing"), good], ..fx.params() };
    expect_connected(&fx.connect(params));

    // Nothing usable: falls through to the password prompt.
    let params = ConnectParams { identity_file: Some(rejected), ..fx.params() };
    let h = fx.connect(params);
    expect_prompt(&h).answer(Some(vec!["secret".into()]));
    expect_connected(&h);
}

#[test]
fn encrypted_key_asks_for_passphrase() {
    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("passphrase", &[&key]);
    fx.trust_host();
    let encrypted = key.encrypt(&mut rand::rng(), "open sesame").unwrap();
    let path = fx.write_key("id_ed25519", &encrypted);

    let h = fx.connect(ConnectParams { identity_file: Some(path.clone()), ..fx.params() });
    let prompt = expect_prompt(&h);
    assert!(prompt.title.contains("id_ed25519"), "{}", prompt.title);
    prompt.answer(Some(vec!["wrong".into()]));
    // Wrong passphrase: asked again.
    expect_prompt(&h).answer(Some(vec!["open sesame".into()]));
    expect_connected(&h);

    // Only an explicit skip moves on to the password.
    let h = fx.connect(ConnectParams { identity_file: Some(path.clone()), ..fx.params() });
    expect_prompt(&h).skip();
    let password_prompt = expect_prompt(&h);
    assert!(password_prompt.account_password);
    password_prompt.answer(Some(vec!["secret".into()]));
    expect_connected(&h);

    // Cancellation must not try an agent, a default key or a saved password, even on a jump host.
    let default = fx.write_key("default", &key);
    let params = ConnectParams {
        identity_file: Some(path),
        default_identity_files: vec![default],
        use_agent: true,
        ssh_legacy_algorithms: false,
        certificate_file: None,
        agent_forwarding: false,
        password: Some("secret".into()),
        ..fx.params()
    };
    for jump in [false, true] {
        fx.auth_calls.lock().unwrap().clear();
        let connect = if jump { ConnectParams { jump: Some(Box::new(params.clone())), ..fx.params() } } else { params.clone() };
        let h = fx.connect(connect);
        expect_prompt(&h).answer(None);
        assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
        assert!(fx.auth_calls.lock().unwrap().is_empty(), "credentials sent after cancellation");
    }

    // A fresh connection after cancellation still works.
    let h = fx.connect(params);
    expect_prompt(&h).answer(Some(vec!["open sesame".into()]));
    expect_connected(&h);
}

#[test]
fn dropped_passphrase_prompt_and_exhausted_retries_abort() {
    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("passphrase-abort", &[&key]);
    fx.trust_host();
    let encrypted = key.encrypt(&mut rand::rng(), "open sesame").unwrap();
    let path = fx.write_key("encrypted", &encrypted);
    let params = ConnectParams { identity_file: Some(path), password: Some("secret".into()), ..fx.params() };
    let h = fx.connect(params.clone());
    drop(expect_prompt(&h));
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    let h = fx.connect(params);
    for _ in 0..3 {
        expect_prompt(&h).answer(Some(vec!["wrong".into()]));
    }
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    assert!(fx.auth_calls.lock().unwrap().is_empty());
}

/// A real loopback SSH server for native-window authentication checks. Only fake credentials.
/// Set PLAYTERM_GUI_FIXTURE_DIR to a fresh evidence directory, run with --ignored --nocapture,
/// then launch playterm with PLAYTERM_CONFIG_DIR=<directory> and the URL from connection.txt.
/// Create <directory>/stop to stop it. It also exits after 30 minutes.
#[test]
#[ignore = "interactive native-window fixture; starts a local SSH server"]
fn native_auth_fixture() {
    let dir = PathBuf::from(std::env::var_os("PLAYTERM_GUI_FIXTURE_DIR").expect("set PLAYTERM_GUI_FIXTURE_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let key = if std::env::var_os("PLAYTERM_GUI_RSA_POLICY").is_some() { rsa_key() } else { random_key(Algorithm::Ed25519) };
    let second_key = random_key(Algorithm::Ed25519);
    let two_keys = std::env::var_os("PLAYTERM_GUI_TWO_KEYS").is_some();
    let gate = Arc::new(common::sftp_fs::TransferGate::default());
    let (fx, host_ca) = if std::env::var_os("PLAYTERM_GUI_HOST_CERTIFICATE").is_some() {
        let (fx, ca) = host_certificate_fixture("native-host-certificate", "127.0.0.1", false);
        (fx, Some(ca))
    } else {
        (Fixture::with_gate("native-auth", &[&key, &second_key], Some(gate.clone())), None)
    };
    if two_keys {
        let agent = start_test_agent(&fx, &[&key, &second_key], "native-two-keys");
        std::fs::write(dir.join("agent.txt"), agent).unwrap();
    }
    std::fs::write(fx.dir.join("sftp").join("large.bin"), vec![7u8; 8 * 1024 * 1024]).unwrap();
    std::fs::write(dir.join("sftp-root.txt"), fx.dir.join("sftp").to_string_lossy().as_bytes()).unwrap();
    if let Some(ca) = host_ca {
        std::fs::write(
            dir.join("known_hosts"),
            format!("@cert-authority [127.0.0.1]:{} {}\n", fx.port, ca.public_key().to_openssh().unwrap()),
        )
        .unwrap();
    } else {
        known_hosts::learn_known_hosts_path("127.0.0.1", fx.port, fx.host_key.public_key(), dir.join("known_hosts")).unwrap();
    }
    let encrypted = key.encrypt(&mut rand::rng(), "open sesame").unwrap();
    std::fs::write(dir.join("encrypted_key"), encrypted.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
    std::fs::write(dir.join("plain_key"), key.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
    std::fs::write(dir.join("settings.toml"), "check_updates = false\nlanguage = \"en\"\n").unwrap();
    let user = if two_keys { "twokeysotp" } else { "tester" };
    std::fs::write(dir.join("connection.txt"), format!("ssh://{user}@127.0.0.1:{}", fx.port)).unwrap();
    println!("Native GUI fixture: {} (passphrase: open sesame; password: secret)", dir.display());
    let deadline = Instant::now() + Duration::from_secs(1800);
    let mut was_stalled = false;
    while Instant::now() < deadline && !dir.join("stop").exists() {
        let stalled = dir.join("stall").exists();
        if stalled != was_stalled {
            if stalled {
                gate.pause();
            } else {
                gate.resume();
            }
            was_stalled = stalled;
        }
        std::fs::write(dir.join("sftp-waiting.txt"), (stalled && gate.waiting.load(std::sync::atomic::Ordering::SeqCst)).to_string())
            .unwrap();
        std::fs::write(dir.join("auth-calls.txt"), fx.auth_calls.lock().unwrap().join("\n")).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
#[test]
fn agent_on_named_pipe_authenticates() {
    use futures::StreamExt as _;
    use russh::keys::agent::client::AgentClient;
    use tokio::net::windows::named_pipe::ServerOptions;

    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("agent", &[&key]);
    fx.trust_host();

    // An in-process ssh-agent speaking the OpenSSH agent protocol on a private pipe.
    let pipe = format!(r"\\.\pipe\playterm-test-agent-{}", std::process::id());
    fx.server_rt.block_on(async {
        let first = ServerOptions::new().first_pipe_instance(true).create(&pipe).unwrap();
        let connections = futures::stream::unfold((first, pipe.clone()), |(server, name)| async move {
            let accepted = server.connect().await.map(|()| server);
            let next = ServerOptions::new().create(&name).unwrap();
            Some((accepted, (next, name)))
        });
        tokio::spawn(russh::keys::agent::server::serve(connections.boxed(), ()));

        let mut client = AgentClient::connect_named_pipe(&pipe).await.unwrap();
        client.add_identity(&key, &[]).await.unwrap();
    });

    let params = ConnectParams { use_agent: true, agent_pipe: Some(pipe), ..fx.params() };
    let h = fx.connect(params.clone());
    expect_connected(&h);
    answer_key_otp(&fx.connect(ConnectParams { username: "keyotp".into(), password: Some("secret".into()), ..params }));
}

#[cfg(unix)]
#[test]
fn agent_on_unix_socket_authenticates() {
    use russh::keys::agent::client::AgentClient;
    use tokio::net::UnixListener;

    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("agent", &[&key]);
    fx.trust_host();

    // An in-process ssh-agent speaking the OpenSSH agent protocol on a private socket.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    fx.server_rt.block_on(async {
        let listener = UnixListener::bind(&socket).unwrap();
        let connections = futures::stream::unfold(listener, |listener| async move {
            let accepted = listener.accept().await.map(|(stream, _)| stream);
            Some((accepted, listener))
        });
        tokio::spawn(russh::keys::agent::server::serve(Box::pin(connections), ()));

        let mut client = AgentClient::connect_uds(&socket).await.unwrap();
        client.add_identity(&key, &[]).await.unwrap();
    });

    let socket = socket.to_string_lossy().into_owned();
    let params = ConnectParams { use_agent: true, agent_pipe: Some(socket), ..fx.params() };
    let h = fx.connect(params.clone());
    expect_connected(&h);
    answer_key_otp(&fx.connect(ConnectParams { username: "keyotp".into(), password: Some("secret".into()), ..params }));
}

// -------------------------------------------------------------------------------------------------
// SFTP

fn open_sftp(h: &SessionHandle) -> SftpConnection {
    h.open_sftp();
    loop {
        match next_event(h) {
            SessionEvent::Sftp(Ok(conn)) => return conn,
            SessionEvent::Sftp(Err(e)) => panic!("sftp failed: {e}"),
            SessionEvent::Data(_) | SessionEvent::Notice(_) => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
}

fn next_sftp(conn: &SftpConnection) -> SftpEvent {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(e) = conn.try_recv() {
            return e;
        }
        assert!(Instant::now() < deadline, "timed out waiting for sftp event");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits until transfer `id` ends and returns its final state.
fn transfer_result(conn: &SftpConnection, id: u64) -> TransferState {
    loop {
        if let SftpEvent::Transfer(u) = next_sftp(conn)
            && u.id == id
            && u.state != TransferState::Running
        {
            return u.state;
        }
    }
}

fn op_result(conn: &SftpConnection) -> Result<(), String> {
    loop {
        if let SftpEvent::OpDone { result, .. } = next_sftp(conn) {
            return result;
        }
    }
}

fn listing(conn: &SftpConnection, path: &str) -> (String, Vec<(String, bool)>) {
    conn.list(path);
    loop {
        if let SftpEvent::Listing { path, result } = next_sftp(conn) {
            let entries = result.expect("listing").into_iter().map(|e| (e.name, e.is_dir)).collect();
            return (path, entries);
        }
    }
}

#[test]
fn sftp_browse_transfer_and_manage() {
    let fx = Fixture::new("sftp", &[]);
    fx.trust_host();
    let root = fx.dir.join("sftp");
    std::fs::write(root.join("hello.txt"), "안녕 sftp").unwrap();
    std::fs::create_dir_all(root.join("docs").join("deep")).unwrap();
    std::fs::write(root.join("docs").join("deep").join("a.txt"), "aaa").unwrap();

    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);

    let (path, entries) = listing(&sftp, ".");
    assert_eq!(path, "/");
    assert_eq!(entries, vec![("docs".to_owned(), true), ("hello.txt".to_owned(), false)], "dirs first");

    // Download a file and a directory tree.
    let local = fx.dir.join("local");
    std::fs::create_dir_all(&local).unwrap();
    let id = sftp.download("/hello.txt", &local.join("hello.txt"), false);
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(local.join("hello.txt")).unwrap(), "안녕 sftp");

    let id = sftp.download("/docs", &local.join("docs"), true);
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(local.join("docs").join("deep").join("a.txt")).unwrap(), "aaa");

    // Upload a multi-chunk file and a directory tree.
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(local.join("big.bin"), &big).unwrap();
    let id = sftp.upload(&local.join("big.bin"), "/big.bin");
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), big);

    let id = sftp.upload(&local.join("docs"), "/docs-copy");
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(root.join("docs-copy").join("deep").join("a.txt")).unwrap(), "aaa");

    // Manage entries.
    sftp.mkdir("/new");
    op_result(&sftp).unwrap();
    sftp.rename("/new", "/renamed");
    op_result(&sftp).unwrap();
    sftp.remove("/docs-copy", true);
    op_result(&sftp).unwrap();
    sftp.remove("/missing.txt", false);
    assert!(op_result(&sftp).is_err());

    let (_, entries) = listing(&sftp, "/");
    let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["docs", "renamed", "big.bin", "hello.txt"]);
}

#[test]
fn sftp_cancel_removes_partial_upload() {
    let fx = Fixture::new("sftp-cancel", &[]);
    fx.trust_host();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);

    let local = fx.dir.join("huge.bin");
    std::fs::write(&local, vec![7u8; 64 * 1024 * 1024]).unwrap();
    let id = sftp.upload(&local, "/huge.bin");
    sftp.cancel(id);
    assert_eq!(transfer_result(&sftp, id), TransferState::Cancelled);
    assert!(!fx.dir.join("sftp").join("huge.bin").exists(), "partial upload must be removed");
}

#[cfg(unix)]
#[test]
fn sftp_folder_download_does_not_follow_local_links() {
    let fx = Fixture::new("sftp-links", &[]);
    fx.trust_host();
    let root = fx.dir.join("sftp");
    std::fs::create_dir_all(root.join("docs").join("deep")).unwrap();
    std::fs::write(root.join("docs").join("deep").join("a.txt"), "from the server").unwrap();
    std::fs::write(root.join("docs").join("b.txt"), "b").unwrap();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);

    // A folder elsewhere, and a link to it inside the download target.
    let outside = fx.dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("a.txt"), "precious").unwrap();
    let target = fx.dir.join("dl");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&outside, target.join("deep")).unwrap();
    let id = sftp.download("/docs", &target, true);
    match transfer_result(&sftp, id) {
        TransferState::Failed(e) => assert!(e.contains("deep"), "{e}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(outside.join("a.txt")).unwrap(), "precious", "nothing written through the link");

    // A link to a file elsewhere is not replaced either.
    let target = fx.dir.join("dl-file");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(outside.join("a.txt"), target.join("b.txt")).unwrap();
    let id = sftp.download("/docs", &target, true);
    assert!(matches!(transfer_result(&sftp, id), TransferState::Failed(_)));
    assert_eq!(std::fs::read_to_string(outside.join("a.txt")).unwrap(), "precious");

    // Plain folders already there are fine, and a target folder that is itself a link (the
    // user's choice) too.
    let target = fx.dir.join("dl-plain");
    std::fs::create_dir_all(target.join("deep")).unwrap();
    let id = sftp.download("/docs", &target, true);
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(target.join("deep").join("a.txt")).unwrap(), "from the server");
    let chosen = fx.dir.join("chosen-link");
    let real = fx.dir.join("real-target");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &chosen).unwrap();
    let id = sftp.download("/docs", &chosen, true);
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(real.join("b.txt")).unwrap(), "b");
}

/// A link at `link` to the folder `target`: a symbolic link, or on Windows a junction (which
/// needs no special rights).
fn dir_link(target: &std::path::Path, link: &std::path::Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(target).output().unwrap();
        assert!(status.status.success(), "mklink /J: {status:?}");
    }
}

#[test]
fn sftp_download_into_refuses_a_link_named_like_the_item() {
    let fx = Fixture::new("sftp-top-link", &[]);
    fx.trust_host();
    let root = fx.dir.join("sftp");
    std::fs::create_dir_all(root.join("reports").join("deep")).unwrap();
    std::fs::write(root.join("reports").join("a.txt"), "from the server").unwrap();
    std::fs::write(root.join("reports").join("deep").join("b.txt"), "b").unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);

    // The chosen folder holds a link named like the remote folder, to a folder elsewhere.
    let outside = fx.dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("a.txt"), "precious").unwrap();
    let chosen = fx.dir.join("chosen");
    std::fs::create_dir_all(&chosen).unwrap();
    dir_link(&outside, &chosen.join("reports"));
    dir_link(&outside, &chosen.join("empty"));
    let listing = || {
        let mut names: Vec<String> =
            std::fs::read_dir(&outside).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    };

    let id = sftp.download_into("/reports", &chosen, "reports", true);
    match transfer_result(&sftp, id) {
        TransferState::Failed(e) => assert!(e.contains("reports"), "{e}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // An empty remote folder is refused all the same.
    let id = sftp.download_into("/empty", &chosen, "empty", true);
    assert!(matches!(transfer_result(&sftp, id), TransferState::Failed(_)));
    assert_eq!(std::fs::read_to_string(outside.join("a.txt")).unwrap(), "precious");
    assert_eq!(listing(), ["a.txt"], "nothing written through the link");

    // A plain folder (new or already there) downloads as before.
    let plain = fx.dir.join("plain");
    std::fs::create_dir_all(plain.join("reports")).unwrap();
    let id = sftp.download_into("/reports", &plain, "reports", true);
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(plain.join("reports").join("deep").join("b.txt")).unwrap(), "b");
    let id = sftp.download_into("/reports/a.txt", &plain, "a.txt", false);
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(plain.join("a.txt")).unwrap(), "from the server");

    // The folder the user chose may itself be a link.
    let chosen_link = fx.dir.join("chosen-link");
    let real = fx.dir.join("real");
    std::fs::create_dir_all(&real).unwrap();
    dir_link(&real, &chosen_link);
    let id = sftp.download_into("/reports", &chosen_link, "reports", true);
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(real.join("reports").join("a.txt")).unwrap(), "from the server");

    // A server name that is not a plain name is refused.
    let id = sftp.download_into("/reports", &plain, "..", true);
    assert!(matches!(transfer_result(&sftp, id), TransferState::Failed(_)));
}

/// Files whose name marks them as an unfinished upload, under `dir`.
fn upload_parts(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect()
}

#[test]
fn sftp_upload_replaces_an_existing_file_only_when_complete() {
    let fx = Fixture::new("sftp-replace", &[]);
    fx.trust_host();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);
    let root = fx.dir.join("sftp");
    let existing = root.join("app.sh");
    std::fs::write(&existing, b"#!/bin/sh\necho old\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o750)).unwrap();
    }

    // A local file that cannot be opened: the remote one is untouched.
    let id = sftp.upload(&fx.dir.join("missing.sh"), "/app.sh");
    assert!(matches!(transfer_result(&sftp, id), TransferState::Failed(_)));
    assert_eq!(std::fs::read(&existing).unwrap(), b"#!/bin/sh\necho old\n");

    // Cancelled half way: the old file stays, the partial upload is gone.
    let huge = fx.dir.join("huge.sh");
    std::fs::write(&huge, vec![b'x'; 64 * 1024 * 1024]).unwrap();
    let id = sftp.upload(&huge, "/app.sh");
    sftp.cancel(id);
    assert_eq!(transfer_result(&sftp, id), TransferState::Cancelled);
    assert_eq!(std::fs::read(&existing).unwrap(), b"#!/bin/sh\necho old\n");
    assert_eq!(upload_parts(&root), Vec::<String>::new());

    // Complete: it replaces the old file (the server refuses to rename over it, as OpenSSH's
    // does) and keeps its permissions.
    let new = fx.dir.join("new.sh");
    std::fs::write(&new, b"#!/bin/sh\necho new\n").unwrap();
    let id = sftp.upload(&new, "/app.sh");
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read(&existing).unwrap(), b"#!/bin/sh\necho new\n");
    assert_eq!(upload_parts(&root), Vec::<String>::new());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777, 0o750);
    }
}

/// Every path under `dir` with its contents (folders as `None`), sorted.
fn tree(dir: &std::path::Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
            if p.is_dir() {
                stack.push(p);
                out.push((rel, None));
            } else {
                out.push((rel, Some(std::fs::read(&p).unwrap())));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn sftp_a_file_upload_never_moves_a_folder_of_that_name() {
    let fx = Fixture::new("sftp-file-onto-dir", &[]);
    fx.trust_host();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);
    let root = fx.dir.join("sftp");
    std::fs::create_dir_all(root.join("reports").join("2026")).unwrap();
    std::fs::write(root.join("reports").join("2026").join("q3.txt"), "kept").unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    let before = tree(&root);

    let local = fx.dir.join("local");
    std::fs::create_dir_all(&local).unwrap();
    for name in ["reports", "empty"] {
        std::fs::write(local.join(name), b"a plain file").unwrap();
        let id = sftp.upload(&local.join(name), &format!("/{name}"));
        match transfer_result(&sftp, id) {
            TransferState::Failed(e) => assert!(e.contains(name), "{e}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    assert_eq!(tree(&root), before, "the folders, what is in them, and no .part or .old left");

    // A plain file of that name next to them is still replaced, a new one created.
    std::fs::write(root.join("notes.txt"), "old").unwrap();
    std::fs::write(local.join("notes.txt"), "new").unwrap();
    let id = sftp.upload(&local.join("notes.txt"), "/notes.txt");
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(root.join("notes.txt")).unwrap(), "new");
    let id = sftp.upload(&local.join("notes.txt"), "/fresh.txt");
    assert_eq!(transfer_result(&sftp, id), TransferState::Finished);
    assert_eq!(std::fs::read_to_string(root.join("fresh.txt")).unwrap(), "new");
    assert_eq!(upload_parts(&root), Vec::<String>::new());
}

/// Leftover hidden files of uploads (`.part`) and replacements (`.old`) of `name` in `dir`,
/// with their contents.
fn leftovers(dir: &std::path::Path, name: &str) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!(".{name}.")))
        .map(|p| (p.extension().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
        .collect();
    out.sort();
    out
}

#[test]
fn sftp_a_failed_replacement_never_loses_the_old_file() {
    let fx = Fixture::new("sftp-replace-fault", &[]);
    fx.trust_host();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);
    let root = fx.dir.join("sftp");
    let target = root.join("replace-fault.sh");
    let new = fx.dir.join("new.sh");
    std::fs::write(&new, b"new").unwrap();
    let failed = |state: TransferState| match state {
        TransferState::Failed(e) => e,
        other => panic!("expected a failure, got {other:?}"),
    };

    // The upload cannot take the name (the plain rename and the one after moving the old file
    // aside fail): the old file is put back, the upload dropped.
    std::fs::write(&target, b"old").unwrap();
    common::sftp_fs::fail_renames("/replace-fault.sh", 2);
    let e = failed(transfer_result(&sftp, sftp.upload(&new, "/replace-fault.sh")));
    assert!(e.contains("replace-fault.sh"), "{e}");
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(leftovers(&root, "replace-fault.sh"), []);

    // Not even the old file can be put back (the connection is gone, say): nothing is deleted,
    // and the error says where the old file and the upload are.
    common::sftp_fs::fail_renames("/replace-fault.sh", 3);
    let e = failed(transfer_result(&sftp, sftp.upload(&new, "/replace-fault.sh")));
    assert!(!target.exists());
    assert_eq!(leftovers(&root, "replace-fault.sh"), [("old".to_owned(), b"old".to_vec()), ("part".to_owned(), b"new".to_vec())]);
    assert!(e.contains(".old") && e.contains(".part"), "{e}");
    for (kind, _) in leftovers(&root, "replace-fault.sh") {
        let leftover =
            std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().unwrap() == kind.as_str()).unwrap();
        std::fs::remove_file(leftover).unwrap();
    }

    // The old file cannot even be moved aside: it stays, the upload is dropped.
    std::fs::write(&target, b"old").unwrap();
    common::sftp_fs::fail_renames("/.replace-fault.sh.", 1);
    failed(transfer_result(&sftp, sftp.upload(&new, "/replace-fault.sh")));
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(leftovers(&root, "replace-fault.sh"), []);

    // Without failures: replaced, nothing left over.
    assert_eq!(transfer_result(&sftp, sftp.upload(&new, "/replace-fault.sh")), TransferState::Finished);
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert_eq!(leftovers(&root, "replace-fault.sh"), []);
}

#[test]
fn sftp_cancelled_download_keeps_the_existing_file() {
    let fx = Fixture::new("sftp-cancel-down", &[]);
    fx.trust_host();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);

    std::fs::write(fx.dir.join("sftp").join("huge.bin"), vec![7u8; 64 * 1024 * 1024]).unwrap();
    let local = fx.dir.join("mine.txt");
    std::fs::write(&local, "my file").unwrap();
    let id = sftp.download("/huge.bin", &local, false);
    sftp.cancel(id);
    assert_eq!(transfer_result(&sftp, id), TransferState::Cancelled);
    // The download went to a partial file of its own: the file that was there is untouched,
    // and nothing partial is left behind.
    assert_eq!(std::fs::read_to_string(&local).unwrap(), "my file");
    let leftovers: Vec<_> = std::fs::read_dir(&fx.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn wait_for_stalled_request(gate: &common::sftp_fs::TransferGate) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gate.waiting.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "no SFTP request reached the unresponsive server");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn transfer_within(sftp: &SftpConnection, id: u64, timeout: Duration) -> Option<TransferState> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(SftpEvent::Transfer(update)) = sftp.try_recv()
            && update.id == id
            && update.state != TransferState::Running
        {
            return Some(update.state);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn stalled_sftp_cancellation(upload: bool, opening: bool) {
    let gate = Arc::new(common::sftp_fs::TransferGate::default());
    let fx = Fixture::with_gate(&format!("stalled-{upload}-{opening}"), &[], Some(gate.clone()));
    fx.trust_host();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);
    let original = b"existing bytes must survive cancellation";
    let source = fx.dir.join("source.bin");
    let target = fx.dir.join("sftp").join("target.bin");
    let local = fx.dir.join("local.bin");
    std::fs::write(&source, vec![7u8; 8 * 1024 * 1024]).unwrap();
    std::fs::write(&target, if upload { original.as_slice() } else { &[7u8; 4096] }).unwrap();
    std::fs::write(&local, original).unwrap();
    gate.block_open.store(opening, std::sync::atomic::Ordering::SeqCst);
    gate.pause();
    let id = if upload { sftp.upload(&source, "/target.bin") } else { sftp.download("/target.bin", &local, false) };
    wait_for_stalled_request(&gate);
    sftp.cancel(id);
    let outcome = transfer_within(&sftp, id, Duration::from_secs(1));
    gate.resume();
    if outcome.is_none() {
        let _ = transfer_within(&sftp, id, Duration::from_secs(5));
    }
    assert_eq!(outcome, Some(TransferState::Cancelled), "cancellation must finish before the server resumes");
    assert_eq!(std::fs::read(if upload { &target } else { &local }).unwrap(), original);
    let deadline = Instant::now() + Duration::from_secs(5);
    while gate.opened.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "late OPEN did not complete after the server resumed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let part_dir = if upload { fx.dir.join("sftp") } else { fx.dir.clone() };
    while !upload_parts(&part_dir).is_empty() {
        assert!(Instant::now() < deadline, "partial file remained after the server resumed");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn stalled_upload_can_be_cancelled_without_losing_existing_file() {
    stalled_sftp_cancellation(true, false);
}

#[test]
fn stalled_download_can_be_cancelled_without_losing_existing_file() {
    stalled_sftp_cancellation(false, false);
}

#[test]
fn stalled_upload_open_can_be_cancelled_and_late_created_file_is_removed() {
    stalled_sftp_cancellation(true, true);
}

#[test]
fn stalled_download_open_can_be_cancelled_without_losing_existing_file() {
    stalled_sftp_cancellation(false, true);
}

#[test]
fn failed_exclusive_open_never_removes_an_unowned_partial_file() {
    let gate = Arc::new(common::sftp_fs::TransferGate::default());
    gate.refuse_create.store(true, std::sync::atomic::Ordering::SeqCst);
    let fx = Fixture::with_gate("refused-open", &[], Some(gate));
    fx.trust_host();
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
    let sftp = open_sftp(&h);
    let source = fx.dir.join("source");
    std::fs::write(&source, b"new").unwrap();
    std::fs::write(fx.dir.join("sftp/target"), b"existing").unwrap();
    assert!(matches!(transfer_result(&sftp, sftp.upload(&source, "/target")), TransferState::Failed(_)));
    assert_eq!(std::fs::read(fx.dir.join("sftp/target")).unwrap(), b"existing");
    let parts = upload_parts(&fx.dir.join("sftp"));
    assert_eq!(parts.len(), 1);
    assert_eq!(std::fs::read(fx.dir.join("sftp").join(&parts[0])).unwrap(), b"foreign partial file");
}

#[test]
fn server_disconnect_during_transfer_keeps_existing_files() {
    for upload in [true, false] {
        let gate = Arc::new(common::sftp_fs::TransferGate::default());
        let fx = Fixture::with_gate(&format!("disconnect-{upload}"), &[], Some(gate.clone()));
        fx.trust_host();
        let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
        expect_connected(&h);
        let sftp = open_sftp(&h);
        let source = fx.dir.join("source");
        let local = fx.dir.join("local");
        let remote = fx.dir.join("sftp/target");
        std::fs::write(&source, vec![7; 8 * 1024 * 1024]).unwrap();
        std::fs::write(&local, b"old local").unwrap();
        std::fs::write(&remote, b"old remote").unwrap();
        gate.pause();
        let id = if upload { sftp.upload(&source, "/target") } else { sftp.download("/target", &local, false) };
        wait_for_stalled_request(&gate);
        fx.server_rt.shutdown_background();
        let outcome = transfer_within(&sftp, id, Duration::from_secs(5));
        assert!(matches!(outcome, Some(TransferState::Failed(_))), "{outcome:?}");
        assert_eq!(std::fs::read(&local).unwrap(), b"old local");
        assert_eq!(std::fs::read(&remote).unwrap(), b"old remote");
    }
}

// -------------------------------------------------------------------------------------------------
// Port forwarding

#[test]
fn repeated_connect_close_releases_forwarding_listeners() {
    let fx = Fixture::new("reconnect-stress", &[]);
    fx.trust_host();
    for round in 0..100 {
        let port = free_port();
        let h = fx.connect(ConnectParams {
            password: Some("secret".into()),
            forwards: vec![rule(ForwardKind::Local, port, fx.port)],
            ..fx.params()
        });
        expect_connected(&h);
        h.close();
        assert!(matches!(skip_notices(&h), SessionEvent::Closed(None)), "round {round}");
        let deadline = Instant::now() + Duration::from_secs(1);
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        while std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            assert!(Instant::now() < deadline, "listener survived round {round}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// TCP echo server on the fixture's runtime; returns its port.
fn echo_server(fx: &Fixture) -> u16 {
    let listener = fx.server_rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    fx.server_rt.spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = tcp.into_split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

fn assert_echo(mut tcp: std::net::TcpStream, message: &[u8]) {
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.write_all(message).unwrap();
    let mut buf = vec![0u8; message.len()];
    tcp.read_exact(&mut buf).unwrap();
    assert_eq!(buf, message);
}

fn rule(kind: ForwardKind, bind_port: u16, dest_port: u16) -> ForwardRule {
    ForwardRule { kind, bind_host: String::new(), bind_port, dest_host: "127.0.0.1".into(), dest_port }
}

fn marker_server(fx: &Fixture, marker: u8) -> u16 {
    use tokio::io::AsyncWriteExt as _;
    let listener = fx.server_rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    fx.server_rt.spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                tcp.write_all(&[marker]).await.unwrap();
                let (mut read, mut write) = tcp.into_split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    port
}

#[test]
fn remote_forward_same_port_routes_distinct_bind_addresses_to_distinct_destinations() {
    let fx = Fixture::new("forward-same-port", &[]);
    fx.trust_host();
    let (first, second, port) = (marker_server(&fx, b'A'), marker_server(&fx, b'B'), free_port());
    let mut rules = vec![rule(ForwardKind::Remote, port, first), rule(ForwardKind::Remote, port, second)];
    rules[0].bind_host = "127.0.0.1".into();
    rules[1].bind_host = "::1".into();
    let handle = fx.connect(ConnectParams { password: Some("secret".into()), forwards: rules, ..fx.params() });
    expect_connected(&handle);
    for (address, marker) in [("127.0.0.1", b'A'), ("::1", b'B')] {
        let mut tcp = std::net::TcpStream::connect((address, port)).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut received = [0];
        tcp.read_exact(&mut received).unwrap();
        assert_eq!(received, [marker], "{address} must reach its own destination");
        assert_echo(tcp, b"routed independently");
    }
    handle.close();
}

#[test]
fn remote_forward_accepts_loopback_aliases_and_rejects_unmatched_reports() {
    for (label, reported) in [("empty", ""), ("localhost", "localhost"), ("unmatched", "203.0.113.1")] {
        let fx = Fixture::new(&format!("forward-report-{label}"), &[]);
        fx.trust_host();
        *fx.forward_address.lock().unwrap() = Some(reported.into());
        let (echo, port) = (echo_server(&fx), free_port());
        let mut forwarding = rule(ForwardKind::Remote, port, echo);
        forwarding.bind_host = "127.0.0.1".into();
        let handle = fx.connect(ConnectParams { password: Some("secret".into()), forwards: vec![forwarding], ..fx.params() });
        expect_connected(&handle);
        let mut tcp = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        if label == "unmatched" {
            tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            match tcp.read(&mut [0]) {
                Ok(0) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) => {}
                other => panic!("unmatched channel must be rejected promptly: {other:?}"),
            }
        } else {
            assert_echo(tcp, b"loopback alias");
        }
        handle.close();
    }
}

#[test]
fn local_remote_and_dynamic_forwarding() {
    let fx = Fixture::new("forward", &[]);
    fx.trust_host();
    let echo = echo_server(&fx);
    let (l_port, r_port, d_port) = (free_port(), free_port(), free_port());

    let params = ConnectParams {
        password: Some("secret".into()),
        forwards: vec![
            rule(ForwardKind::Local, l_port, echo),
            rule(ForwardKind::Remote, r_port, echo),
            rule(ForwardKind::Dynamic, d_port, 0),
        ],
        ..fx.params()
    };
    let h = fx.connect(params);
    expect_connected(&h);

    // -L: local port → server → echo.
    assert_echo(std::net::TcpStream::connect(("127.0.0.1", l_port)).unwrap(), b"through local forward");

    // -R: server port → client → echo.
    assert_echo(std::net::TcpStream::connect(("127.0.0.1", r_port)).unwrap(), b"through remote forward");

    // -D: SOCKS5 CONNECT to the echo server.
    let mut socks = std::net::TcpStream::connect(("127.0.0.1", d_port)).unwrap();
    socks.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    socks.write_all(&[5, 1, 0]).unwrap();
    let mut greeting = [0u8; 2];
    socks.read_exact(&mut greeting).unwrap();
    assert_eq!(greeting, [5, 0]);
    let mut request = vec![5, 1, 0, 1, 127, 0, 0, 1];
    request.extend_from_slice(&echo.to_be_bytes());
    socks.write_all(&request).unwrap();
    let mut reply = [0u8; 10];
    socks.read_exact(&mut reply).unwrap();
    assert_eq!(reply[1], 0, "SOCKS5 CONNECT succeeded");
    assert_echo(socks, b"through socks");

    // Closing the session stops the local listeners.
    h.close();
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(None)));
    std::thread::sleep(Duration::from_millis(200));
    assert!(std::net::TcpStream::connect(("127.0.0.1", l_port)).is_err());
}

#[test]
fn forward_to_unreachable_target_reports_notice() {
    let fx = Fixture::new("forward-fail", &[]);
    fx.trust_host();
    let l_port = free_port();
    let dead = free_port(); // nothing listens here
    let h = fx.connect(ConnectParams {
        password: Some("secret".into()),
        forwards: vec![rule(ForwardKind::Local, l_port, dead)],
        ..fx.params()
    });
    expect_connected(&h);

    let mut tcp = std::net::TcpStream::connect(("127.0.0.1", l_port)).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut buf = [0u8; 1];
    assert_eq!(tcp.read(&mut buf).unwrap_or(0), 0, "connection is closed");
    loop {
        if let SessionEvent::Notice(n) = next_event(&h)
            && n.contains("포워딩 연결 실패")
        {
            break;
        }
    }
}

#[test]
fn agent_forwarding_foreign_platform_path_uses_environment_agent() {
    const CHILD: &str = "PLAYTERM_FORWARD_PLATFORM_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let fx = Fixture::new("forward-platform-child", &[]);
        fx.trust_host();
        let foreign = if cfg!(windows) { "/tmp/foreign-agent.sock" } else { r"\\.\pipe\openssh-ssh-agent" };
        let h = fx.connect(ConnectParams {
            password: Some("secret".into()),
            agent_forwarding: true,
            agent_pipe: Some(foreign.into()),
            ..fx.params()
        });
        expect_connected(&h);
        h.send(b"probe-agent".to_vec());
        wait_agent_event(&fx, "signed");
        h.close();
        return;
    }
    let key = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("forward-platform-parent", &[]);
    let agent = start_test_agent(&fx, &[&key], "platform");
    // Isolate SSH_AUTH_SOCK from parallel tests and the user's real agent.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "agent_forwarding_foreign_platform_path_uses_environment_agent", "--nocapture"])
        .env(CHILD, "1")
        .env("SSH_AUTH_SOCK", agent)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
