//! An opt-in check against an actual OpenSSH daemon, separate from the russh loopback tests.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use playterm_net::{AuthPrompt, ConnectParams, NetRuntime, Protocol, SessionEvent, SessionHandle, SftpEvent, TransferState};

fn params(known_hosts: PathBuf, port_variable: &str) -> ConnectParams {
    ConnectParams {
        protocol: Protocol::Ssh,
        host: "127.0.0.1".into(),
        port: std::env::var(port_variable).unwrap().parse().unwrap(),
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
        known_hosts,
        connect_timeout: Duration::from_secs(10),
        jump: None,
        x11: None,
    }
}

#[test]
#[ignore = "requires an isolated OpenSSH host-certificate server; see scripts/test-openssh.sh"]
fn openssh_trusted_host_certificate_without_tofu() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("known_hosts");
    let options = params(file.clone(), "PLAYTERM_OPENSSH_CERT_PORT");
    let ca = std::fs::read_to_string(std::env::var_os("PLAYTERM_OPENSSH_HOST_CA").unwrap()).unwrap();
    let trust = format!("@cert-authority [127.0.0.1]:{} {}\n", options.port, ca.trim());
    std::fs::write(&file, &trust).unwrap();
    let net = NetRuntime::new().unwrap();
    let handle = net.connect(options, Arc::new(|| {}));
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut verified = false;
    loop {
        assert!(Instant::now() < deadline, "OpenSSH certificate connection timed out");
        match handle.try_recv() {
            Some(SessionEvent::Connected) => break,
            Some(SessionEvent::HostKeyCheck(_)) => panic!("certificate trust must never ask TOFU"),
            Some(SessionEvent::Closed(error)) => panic!("certificate connection failed: {error:?}"),
            Some(SessionEvent::Notice(text)) => {
                verified |= text.contains("Verified SSH host certificate") || text.contains("SSH 호스트 인증서")
            }
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    assert!(verified, "the certificate validation notice must precede authentication");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), trust);
    handle.close();
}

#[test]
#[ignore = "requires an isolated OpenSSH server; see scripts/test-openssh.sh"]
fn openssh_key_shell_and_unicode_sftp() {
    let dir = tempfile::tempdir().unwrap();
    let net = NetRuntime::new().unwrap();
    let params = params(dir.path().join("known_hosts"), "PLAYTERM_OPENSSH_PORT");
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

fn next_auth_event(handle: &SessionHandle) -> SessionEvent {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "OpenSSH MFA event timeout");
        match handle.try_recv() {
            Some(SessionEvent::HostKeyCheck(check)) => check.answer(true),
            Some(SessionEvent::Notice(_)) | None => std::thread::sleep(Duration::from_millis(5)),
            Some(event) => return event,
        }
    }
}

fn mfa_prompt(handle: &SessionHandle) -> AuthPrompt {
    let SessionEvent::AuthPrompt(prompt) = next_auth_event(handle) else {
        panic!("OpenSSH MFA must ask instead of using the stored password");
    };
    assert!(!prompt.account_password, "the second factor must never overwrite the account password");
    assert!(!prompt.can_skip);
    assert_eq!(prompt.fields.len(), 1);
    assert!(!prompt.fields[0].echo);
    prompt
}

#[test]
#[ignore = "requires OpenSSH publickey,keyboard-interactive with PAM; see scripts/test-openssh.sh"]
fn openssh_public_key_mfa_does_not_autofill_or_save_second_factor() {
    let dir = tempfile::tempdir().unwrap();
    let net = NetRuntime::new().unwrap();
    let mut params = params(dir.path().join("known_hosts"), "PLAYTERM_OPENSSH_MFA_PORT");
    // Correct PAM credentials: an autofill regression would connect without asking and fail
    // this test. PAM is a static test factor; real OTP providers remain a separate matrix row.
    params.password = Some("local-test-only".into());
    let handle = net.connect(params.clone(), Arc::new(|| {}));
    mfa_prompt(&handle).answer(Some(vec!["wrong-test-factor".into()]));
    mfa_prompt(&handle).answer(Some(vec!["local-test-only".into()]));
    assert!(matches!(next_auth_event(&handle), SessionEvent::Connected));
    handle.close();

    let handle = net.connect(params.clone(), Arc::new(|| {}));
    mfa_prompt(&handle).answer(None);
    assert!(matches!(next_auth_event(&handle), SessionEvent::Closed(Some(_))));

    let handle = net.connect(params, Arc::new(|| {}));
    mfa_prompt(&handle).answer(Some(vec!["local-test-only".into()]));
    assert!(matches!(next_auth_event(&handle), SessionEvent::Connected));
    handle.close();
}

#[test]
#[ignore = "requires isolated OpenSSH publickey,publickey servers; see scripts/test-openssh.sh"]
fn openssh_two_public_keys_and_two_keys_then_mfa() {
    let dir = tempfile::tempdir().unwrap();
    let net = NetRuntime::new().unwrap();
    let mut base = params(dir.path().join("known_hosts"), "PLAYTERM_OPENSSH_TWO_KEY_PORT");
    let a = base.identity_file.clone().unwrap();
    let b = PathBuf::from(std::env::var_os("PLAYTERM_OPENSSH_KEY_B").unwrap());
    let wrong = PathBuf::from(std::env::var_os("PLAYTERM_OPENSSH_WRONG_KEY").unwrap());
    for (identity_file, default_identity_files, use_agent) in [
        (Some(a.clone()), vec![a.clone(), wrong.clone(), b.clone()], false),
        (None, vec![wrong.clone(), a.clone(), wrong, b.clone()], false),
        (None, vec![], true),
        (Some(a.clone()), vec![a.clone()], true),
    ] {
        let handle = net.connect(
            ConnectParams {
                identity_file,
                default_identity_files,
                use_agent,
                agent_pipe: use_agent.then(|| std::env::var("SSH_AUTH_SOCK").unwrap()),
                ..base.clone()
            },
            Arc::new(|| {}),
        );
        assert!(matches!(next_auth_event(&handle), SessionEvent::Connected), "two unique keys must complete authentication");
        handle.close();
    }
    // Neither a single key nor the same key supplied again is a second factor.
    for duplicates in [vec![], vec![a.clone(), a.clone()]] {
        let handle = net.connect(ConnectParams { default_identity_files: duplicates, ..base.clone() }, Arc::new(|| {}));
        assert!(matches!(next_auth_event(&handle), SessionEvent::Closed(Some(_))));
    }
    base.port = std::env::var("PLAYTERM_OPENSSH_TWO_KEY_MFA_PORT").unwrap().parse().unwrap();
    base.default_identity_files = vec![a, b];
    base.password = Some("local-test-only".into());
    let handle = net.connect(base.clone(), Arc::new(|| {}));
    mfa_prompt(&handle).answer(Some(vec!["wrong-test-factor".into()]));
    mfa_prompt(&handle).answer(Some(vec!["local-test-only".into()]));
    assert!(matches!(next_auth_event(&handle), SessionEvent::Connected));
    handle.close();
    let handle = net.connect(base.clone(), Arc::new(|| {}));
    mfa_prompt(&handle).answer(None);
    assert!(matches!(next_auth_event(&handle), SessionEvent::Closed(Some(_))));
    let handle = net.connect(base, Arc::new(|| {}));
    mfa_prompt(&handle).answer(Some(vec!["local-test-only".into()]));
    assert!(matches!(next_auth_event(&handle), SessionEvent::Connected));
    handle.close();
}
