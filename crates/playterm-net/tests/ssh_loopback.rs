//! End-to-end SSH tests against an in-process russh server on 127.0.0.1.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use playterm_net::{ConnectParams, NetRuntime, Protocol, SessionEvent, SessionHandle};
use russh::keys::{Algorithm, PrivateKey, known_hosts};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Pty};
use tokio::net::TcpListener;

#[derive(Clone)]
struct TestServer;

impl server::Server for TestServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        self.clone()
    }
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == "tester" && password == "secret" {
            return Ok(Auth::Accept);
        }
        // Like OpenSSH: keep offering password (and keyboard-interactive) after a failure.
        let methods: &[MethodKind] = &[MethodKind::Password, MethodKind::KeyboardInteractive];
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
    _server_rt: tokio::runtime::Runtime,
    port: u16,
    key: PrivateKey,
    known_hosts: PathBuf,
    net: NetRuntime,
}

fn random_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

impl Fixture {
    fn new(name: &str) -> Self {
        let key = random_key();
        let server_rt = tokio::runtime::Runtime::new().unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key.clone()],
            auth_rejection_time: Duration::from_millis(10),
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = server_rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        server_rt.spawn(async move {
            let _ = TestServer.run_on_socket(config, &listener).await;
        });

        let known_hosts = std::env::temp_dir()
            .join(format!("playterm-test-{name}-{}", std::process::id()))
            .join("known_hosts");
        let _ = std::fs::remove_file(&known_hosts);

        Self { _server_rt: server_rt, port, key, known_hosts, net: NetRuntime::new().unwrap() }
    }

    fn connect(&self, password: Option<&str>) -> SessionHandle {
        let params = ConnectParams {
            protocol: Protocol::Ssh,
            host: "127.0.0.1".into(),
            port: self.port,
            username: "tester".into(),
            password: password.map(Into::into),
            term_type: "xterm-256color".into(),
            cols: 80,
            rows: 24,
            known_hosts: self.known_hosts.clone(),
        };
        self.net.connect(params, Arc::new(|| {}))
    }
}

fn next_event(h: &SessionHandle) -> SessionEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
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

#[test]
fn first_connect_asks_for_host_key_then_runs_shell() {
    let fx = Fixture::new("first");
    let h = fx.connect(Some("secret"));

    let SessionEvent::HostKeyCheck(check) = skip_notices(&h) else { panic!("expected host key check") };
    assert_eq!(check.port, fx.port);
    assert_eq!(check.algorithm, "ssh-ed25519");
    assert_eq!(check.fingerprint, fx.key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string());
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
    assert!(known_hosts::check_known_hosts_path("127.0.0.1", fx.port, fx.key.public_key(), &fx.known_hosts).unwrap());
    let h = fx.connect(Some("secret"));
    assert!(matches!(skip_notices(&h), SessionEvent::Connected));
}

#[test]
fn wrong_password_falls_back_to_prompt() {
    let fx = Fixture::new("prompt");
    known_hosts::learn_known_hosts_path("127.0.0.1", fx.port, fx.key.public_key(), &fx.known_hosts).unwrap();

    let h = fx.connect(Some("wrong"));
    // The test server advertises keyboard-interactive but rejects it silently; the client
    // must then ask for a password instead of giving up.
    let prompt = match skip_notices(&h) {
        SessionEvent::AuthPrompt(p) => p,
        other => panic!("expected password prompt, got {other:?}"),
    };
    assert_eq!(prompt.fields.len(), 1);
    assert!(!prompt.fields[0].echo);
    prompt.answer(Some(vec!["secret".into()]));
    assert!(matches!(skip_notices(&h), SessionEvent::Connected));
    read_until(&h, "welcome");
}

#[test]
fn cancelled_prompt_closes_session() {
    let fx = Fixture::new("cancel");
    known_hosts::learn_known_hosts_path("127.0.0.1", fx.port, fx.key.public_key(), &fx.known_hosts).unwrap();

    let h = fx.connect(None);
    let prompt = match skip_notices(&h) {
        SessionEvent::AuthPrompt(p) => p,
        other => panic!("expected password prompt, got {other:?}"),
    };
    prompt.answer(None);
    let SessionEvent::Closed(Some(reason)) = skip_notices(&h) else { panic!("expected close with error") };
    assert!(reason.contains("취소"), "{reason}");
}

#[test]
fn changed_host_key_is_refused_without_prompt() {
    let fx = Fixture::new("changed");
    // Pretend we knew a different key for this host:port before.
    known_hosts::learn_known_hosts_path("127.0.0.1", fx.port, random_key().public_key(), &fx.known_hosts).unwrap();

    let h = fx.connect(Some("secret"));
    let SessionEvent::Closed(Some(reason)) = skip_notices(&h) else { panic!("expected refusal") };
    assert!(reason.contains("호스트 키가 변경"), "{reason}");
}

#[test]
fn rejected_host_key_closes_session() {
    let fx = Fixture::new("reject");
    let h = fx.connect(Some("secret"));
    let SessionEvent::HostKeyCheck(check) = skip_notices(&h) else { panic!("expected host key check") };
    check.answer(false);
    assert!(matches!(skip_notices(&h), SessionEvent::Closed(Some(_))));
    assert!(!fx.known_hosts.exists());
}
