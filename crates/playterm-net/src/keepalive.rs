//! Keeping idle connections open.
//!
//! Two kinds of idle timeout cut sessions off:
//! - NAT routers, firewalls and SSH servers (`ClientAliveInterval`) drop connections that carry
//!   no traffic. Protocol keep-alives (SSH keepalive requests, Telnet `IAC NOP`) are enough.
//! - Devices and shells log out users who type nothing (Cisco `exec-timeout`, `TMOUT`).
//!   Only input resets those, so the session types a harmless text such as space + backspace.

use std::time::Duration;

use playterm_i18n::tr;
use tokio::time::Instant;

use crate::session::EventSink;

pub use playterm_types::{KeepAlive, KeepAliveMode, decode_escapes};

/// When the next keep-alive is due: a period after the last thing the user (or we) sent.
pub(crate) struct IdleTimer {
    period: Option<Duration>,
    last: Instant,
    /// What to type; `None` means a protocol keep-alive.
    pub text: Option<Vec<u8>>,
}

impl IdleTimer {
    /// `own_protocol`: the transport sends its protocol keep-alive from this timer (Telnet).
    pub fn new(keepalive: &KeepAlive, own_protocol: bool, sink: &EventSink) -> Self {
        let text = (keepalive.mode == KeepAliveMode::Text)
            .then(|| match decode_escapes(&keepalive.text) {
                Ok(bytes) => Some(bytes).filter(|b| !b.is_empty()),
                Err(e) => {
                    sink.notice(tr!("keepalive.keep_alive_text_error", e = e));
                    None
                }
            })
            .flatten();
        let active = match keepalive.mode {
            KeepAliveMode::Off => false,
            KeepAliveMode::Protocol => own_protocol,
            KeepAliveMode::Text => text.is_some(),
        };
        Self { period: active.then(|| keepalive.period()), last: Instant::now(), text }
    }

    /// Something was sent: the idle period starts again.
    pub fn touch(&mut self) {
        self.last = Instant::now();
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.period.map(|p| self.last + p)
    }
}

/// Sleeps until `deadline`, or forever without one.
pub(crate) async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_modes() {
        let (_h, _c, sink) = crate::session::SessionHandle::new(std::sync::Arc::new(|| {}));
        let ka = |mode, text: &str| KeepAlive { mode, interval: 30, text: text.into() };
        assert!(IdleTimer::new(&ka(KeepAliveMode::Off, ""), true, &sink).deadline().is_none());
        assert!(IdleTimer::new(&ka(KeepAliveMode::Protocol, ""), true, &sink).deadline().is_some());
        assert!(IdleTimer::new(&ka(KeepAliveMode::Protocol, ""), false, &sink).deadline().is_none());
        let typed = IdleTimer::new(&ka(KeepAliveMode::Text, "\\r"), false, &sink);
        assert_eq!((typed.deadline().is_some(), typed.text.as_deref()), (true, Some(&b"\r"[..])));
        assert!(IdleTimer::new(&ka(KeepAliveMode::Text, ""), true, &sink).deadline().is_none());
        assert!(IdleTimer::new(&ka(KeepAliveMode::Text, "\\q"), true, &sink).deadline().is_none());
    }
}
