//! Serial sessions without hardware: opening a port that does not exist must fail cleanly.
//! (Data transfer and BREAK are covered by the unit tests in `src/serial.rs`.)

use std::sync::Arc;
use std::time::{Duration, Instant};

use playterm_net::{ConnectParams, NetRuntime, Protocol, SerialSettings, SessionEvent};

fn params(port: &str) -> ConnectParams {
    ConnectParams {
        protocol: Protocol::Serial,
        host: port.into(),
        port: 0,
        username: String::new(),
        password: None,
        identity_file: None,
        default_identity_files: Vec::new(),
        use_agent: false,
        agent_pipe: None,
        forwards: Vec::new(),
        serial: SerialSettings { port: port.into(), baud: 115200, ..Default::default() },
        keepalive: Default::default(),
        term_type: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        known_hosts: std::env::temp_dir().join("playterm-serial-test-known_hosts"),
    }
}

fn closed_reason(net: &NetRuntime, port: &str) -> String {
    let h = net.connect(params(port), Arc::new(|| {}));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match h.try_recv() {
            Some(SessionEvent::Closed(Some(reason))) => return reason,
            Some(other) => panic!("unexpected event {other:?}"),
            None => {
                assert!(Instant::now() < deadline, "timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn missing_port_reports_error() {
    let net = NetRuntime::new().unwrap();
    let reason = closed_reason(&net, "COM250");
    assert!(reason.contains("COM250 을(를) 열 수 없습니다"), "{reason}");

    let reason = closed_reason(&net, "");
    assert!(reason.contains("시리얼 포트를 지정하세요"), "{reason}");
}

#[test]
fn listing_ports_does_not_fail() {
    // This machine may have no COM ports at all; listing must still work.
    let _ = playterm_net::list_ports();
}
