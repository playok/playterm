//! End-to-end SSH tests against an in-process russh server on 127.0.0.1.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use playterm_net::{AuthPrompt, ConnectParams, NetRuntime, Protocol, SessionEvent, SessionHandle};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey, known_hosts};
use russh::server::{self, Auth, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Pty};
use tokio::net::TcpListener;

#[derive(Clone)]
struct TestServer {
    /// Public keys accepted for user `tester`.
    keys: Arc<Vec<PublicKey>>,
}

impl server::Server for TestServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        self.clone()
    }
}

/// Like OpenSSH: after a failure, keep offering every method.
fn reject() -> Auth {
    let methods: &[MethodKind] = &[MethodKind::PublicKey, MethodKind::Password, MethodKind::KeyboardInteractive];
    Auth::Reject { proceed_with_methods: Some(MethodSet::from(methods)), partial_success: false }
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "tester" && password == "secret" { Auth::Accept } else { reject() })
    }

    async fn auth_publickey(&mut self, user: &str, key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        let known = self.keys.iter().any(|k| k.key_data() == key.public_key.key_data());
        Ok(if user == "tester" && known { Auth::Accept } else { reject() })
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

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.data(channel, b"welcome\r\n".to_vec())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
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

impl Fixture {
    /// Server accepting password `secret` and the given public keys for user `tester`.
    fn new(name: &str, client_keys: &[&PrivateKey]) -> Self {
        let host_key = random_key(Algorithm::Ed25519);
        let server_rt = tokio::runtime::Runtime::new().unwrap();
        let config = Arc::new(server::Config {
            keys: vec![host_key.clone()],
            auth_rejection_time: Duration::from_millis(10),
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = server_rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        let keys = Arc::new(client_keys.iter().map(|k| k.public_key().clone()).collect());
        server_rt.spawn(async move {
            let _ = TestServer { keys }.run_on_socket(config, &listener).await;
        });

        let dir = std::env::temp_dir().join(format!("playterm-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let known_hosts = dir.join("known_hosts");

        Self { server_rt, port, host_key, dir, known_hosts, net: NetRuntime::new().unwrap() }
    }

    fn trust_host(&self) {
        known_hosts::learn_known_hosts_path("127.0.0.1", self.port, self.host_key.public_key(), &self.known_hosts)
            .unwrap();
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
            agent_pipe: None,
            term_type: "xterm-256color".into(),
            cols: 80,
            rows: 24,
            known_hosts: self.known_hosts.clone(),
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
    assert!(
        known_hosts::check_known_hosts_path("127.0.0.1", fx.port, fx.host_key.public_key(), &fx.known_hosts).unwrap()
    );
    let h = fx.connect(ConnectParams { password: Some("secret".into()), ..fx.params() });
    expect_connected(&h);
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
    let rsa = rsa_key();
    let fx = Fixture::new("keyfile", &[&ed, &rsa]);
    fx.trust_host();

    let ed_path = fx.write_key("id_ed25519", &ed);
    let h = fx.connect(ConnectParams { identity_file: Some(ed_path), ..fx.params() });
    expect_connected(&h);

    // RSA must be signed with rsa-sha2-*, which the client picks from server-sig-algs.
    let rsa_path = fx.write_key("id_rsa", &rsa);
    let h = fx.connect(ConnectParams { identity_file: Some(rsa_path), ..fx.params() });
    expect_connected(&h);
}

#[test]
fn unknown_key_then_default_key_then_password() {
    let accepted = random_key(Algorithm::Ed25519);
    let fx = Fixture::new("order", &[&accepted]);
    fx.trust_host();

    // An explicit key the server rejects, a missing default, then the accepted default key.
    let rejected = fx.write_key("other", &random_key(Algorithm::Ed25519));
    let good = fx.write_key("id_ed25519", &accepted);
    let params = ConnectParams {
        identity_file: Some(rejected.clone()),
        default_identity_files: vec![fx.dir.join("missing"), good],
        ..fx.params()
    };
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

    // Cancelling the passphrase skips the key and moves on to the password.
    let h = fx.connect(ConnectParams { identity_file: Some(path), ..fx.params() });
    expect_prompt(&h).answer(None);
    let password_prompt = expect_prompt(&h);
    assert!(password_prompt.title.contains("비밀번호"), "{}", password_prompt.title);
    password_prompt.answer(Some(vec!["secret".into()]));
    expect_connected(&h);
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

    let h = fx.connect(ConnectParams { use_agent: true, agent_pipe: Some(pipe), ..fx.params() });
    expect_connected(&h);
}
