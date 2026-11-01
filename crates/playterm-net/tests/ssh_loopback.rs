//! End-to-end SSH tests against an in-process russh server on 127.0.0.1.

mod common;

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use playterm_net::{
    AuthPrompt, ConnectParams, ForwardKind, ForwardRule, NetRuntime, Protocol, SessionEvent, SessionHandle,
    SftpConnection, SftpEvent, TransferState,
};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey, known_hosts};
use russh::server::{self, Auth, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Pty};
use tokio::net::TcpListener;

#[derive(Clone)]
struct TestServer {
    /// Public keys accepted for user `tester`.
    keys: Arc<Vec<PublicKey>>,
    /// Directory served over SFTP.
    sftp_root: PathBuf,
    /// Session channels not yet claimed by a shell, so a subsystem request can take them.
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    /// Channels running the echo "shell"; other channels (SFTP, forwards) carry raw streams.
    shells: Arc<Mutex<std::collections::HashSet<ChannelId>>>,
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
                russh_sftp::server::run(channel.into_stream(), common::sftp_fs::FsHandler::new(&self.sftp_root)).await;
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
        // The shell is driven through the handler callbacks; release the channel object.
        self.channels.lock().unwrap().remove(&channel);
        self.shells.lock().unwrap().insert(channel);
        session.data(channel, b"welcome\r\n".to_vec())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if !self.shells.lock().unwrap().contains(&channel) {
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
        let dir = std::env::temp_dir().join(format!("playterm-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sftp")).unwrap();
        let known_hosts = dir.join("known_hosts");

        let server = TestServer {
            keys: Arc::new(client_keys.iter().map(|k| k.public_key().clone()).collect()),
            sftp_root: dir.join("sftp"),
            channels: Arc::default(),
            shells: Arc::default(),
        };
        server_rt.spawn(async move {
            let _ = { server }.run_on_socket(config, &listener).await;
        });

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
fn silent_server_times_out() {
    // Accepts TCP connections (the backlog) but never says a word of SSH.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let fx = Fixture::new("silent", &[]);
    let params = ConnectParams {
        port: silent.local_addr().unwrap().port(),
        connect_timeout: Duration::from_secs(1),
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

// -------------------------------------------------------------------------------------------------
// Port forwarding

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
