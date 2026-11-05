//! An opt-in check against an actual OpenSSH daemon, separate from the russh loopback tests.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use playterm_net::{ConnectParams, NetRuntime, Protocol, SessionEvent, SftpEvent, TransferState};

#[test]
#[ignore = "requires an isolated OpenSSH server; see scripts/test-openssh.sh"]
fn openssh_key_shell_and_unicode_sftp() {
    let dir = tempfile::tempdir().unwrap();
    let net = NetRuntime::new().unwrap();
    let params = ConnectParams {
        protocol: Protocol::Ssh,
        host: "127.0.0.1".into(),
        port: std::env::var("PLAYTERM_OPENSSH_PORT").unwrap().parse().unwrap(),
        username: std::env::var("PLAYTERM_OPENSSH_USER").unwrap(),
        identity_file: Some(PathBuf::from(std::env::var_os("PLAYTERM_OPENSSH_KEY").unwrap())),
        password: None,
        default_identity_files: vec![],
        use_agent: false,
        agent_pipe: None,
        forwards: vec![],
        serial: Default::default(),
        keepalive: Default::default(),
        rdp: Default::default(),
        desktop: (1024, 768),
        term_type: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        known_hosts: dir.path().join("known_hosts"),
        connect_timeout: Duration::from_secs(10),
        jump: None,
        x11: None,
    };
    let handle = net.connect(params, Arc::new(|| {}));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut connected = false;
    let mut output = String::new();
    let sftp = loop {
        assert!(Instant::now() < deadline, "OpenSSH connection/SFTP timeout: {output}");
        match handle.try_recv() {
            Some(SessionEvent::HostKeyCheck(check)) => check.answer(true), // isolated loopback only
            Some(SessionEvent::Connected) => {
                connected = true;
                handle.send(b"printf 'playterm-%s-%s\\n' openssh ok\r".to_vec());
                handle.open_sftp();
            }
            Some(SessionEvent::Data(bytes)) => output.push_str(&String::from_utf8_lossy(&bytes)),
            Some(SessionEvent::Sftp(Ok(sftp))) => break sftp,
            Some(SessionEvent::Notice(_)) | None => std::thread::sleep(Duration::from_millis(5)),
            event => panic!("OpenSSH failed: {event:?}"),
        }
    };
    assert!(connected);
    let data: Vec<u8> = (0..3_000_000).map(|i| (i % 251) as u8).collect();
    let local = dir.path().join("한글 데이터.bin");
    std::fs::write(&local, &data).unwrap();
    let remote = format!("한글-상호운용-{}.bin", std::process::id());
    let wait_transfer = |id| {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(Instant::now() < deadline, "OpenSSH transfer timeout");
            if let Some(SftpEvent::Transfer(update)) = sftp.try_recv()
                && update.id == id
                && update.state != TransferState::Running
            {
                assert_eq!(update.state, TransferState::Finished);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait_transfer(sftp.upload(&local, &remote));
    let download = dir.path().join("download.bin");
    wait_transfer(sftp.download(&remote, &download, false));
    assert_eq!(std::fs::read(download).unwrap(), data);
    sftp.remove(&remote, false);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "OpenSSH cleanup timeout");
        if let Some(SftpEvent::OpDone { result, .. }) = sftp.try_recv() {
            result.unwrap();
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    while !output.contains("playterm-openssh-ok") {
        assert!(Instant::now() < deadline, "OpenSSH shell did not reply");
        if let Some(SessionEvent::Data(bytes)) = handle.try_recv() {
            output.push_str(&String::from_utf8_lossy(&bytes));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    handle.close();
}
