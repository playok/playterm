//! `ping` for the local prompt: ICMP echo requests through Windows' ICMP API (`IcmpSendEcho`,
//! `Icmp6SendEcho2`), which needs no administrator rights and no `ping.exe`, on a thread of its
//! own. Each answer arrives as a [`PingEvent`]; dropping the [`Ping`] stops it.

use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use playterm_i18n::{t, tr};

use crate::session::Waker;

/// Time between echo requests (from the start of one to the start of the next).
const INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_SIZE: u16 = 65_500;
pub const MAX_TIMEOUT_MS: u32 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

/// What `ping` was asked for (the options of Windows' `ping`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PingOptions {
    /// How many requests; `None` = until stopped (`-t`).
    pub count: Option<u32>,
    /// How long to wait for each answer.
    pub timeout_ms: u32,
    /// Bytes of data in each request.
    pub size: u16,
    /// Only IPv4 (`-4`) or only IPv6 (`-6`) addresses of the name.
    pub family: Option<Family>,
}

impl Default for PingOptions {
    fn default() -> Self {
        Self { count: Some(4), timeout_ms: 4000, size: 32, family: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PingEvent {
    /// The name was resolved; requests go to this address.
    Start(IpAddr),
    /// An answer.
    Reply { from: IpAddr, bytes: usize, time_ms: u32, ttl: Option<u8> },
    /// No answer in time.
    TimedOut,
    /// An error report instead of an answer (unreachable, TTL expired), from `from` if known.
    Failed { from: Option<IpAddr>, status: u32 },
    /// It could not go on (unknown name, the ICMP API failed).
    Error(String),
    /// All requests were sent.
    Done,
}

/// A ping in progress.
pub struct Ping {
    rx: mpsc::Receiver<PingEvent>,
    stop: Arc<AtomicBool>,
}

impl Ping {
    /// Starts pinging `host` (a name or an address); `waker` is called after each event.
    pub fn start(host: String, options: PingOptions, waker: Waker) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::Builder::new()
            .name("ping".into())
            .spawn(move || {
                let send = |event| {
                    let _ = tx.send(event);
                    waker();
                };
                run(&host, &options, &stopped, send);
            })
            .expect("spawn the ping thread");
        Self { rx, stop }
    }

    pub fn try_recv(&self) -> Option<PingEvent> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Ping {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run(host: &str, options: &PingOptions, stop: &AtomicBool, send: impl Fn(PingEvent)) {
    let addr = match resolve(host, options.family) {
        Ok(addr) => addr,
        Err(e) => {
            send(PingEvent::Error(e));
            return;
        }
    };
    send(PingEvent::Start(addr));
    let icmp = match imp::Icmp::open(addr) {
        Ok(icmp) => icmp,
        Err(e) => {
            send(PingEvent::Error(e));
            return;
        }
    };
    let data = payload(options.size);
    let mut sent = 0u32;
    while !stop.load(Ordering::Relaxed) && options.count.is_none_or(|n| sent < n) {
        let started = Instant::now();
        let event = icmp.echo(addr, &data, options.timeout_ms);
        sent += 1;
        if stop.load(Ordering::Relaxed) {
            return;
        }
        send(event);
        if options.count.is_some_and(|n| sent >= n) {
            break;
        }
        while !stop.load(Ordering::Relaxed) && started.elapsed() < INTERVAL {
            std::thread::sleep((INTERVAL.saturating_sub(started.elapsed())).min(Duration::from_millis(50)));
        }
    }
    if !stop.load(Ordering::Relaxed) {
        send(PingEvent::Done);
    }
}

/// The address to ping: `host` itself, or the first address of the name (IPv4 first, as most
/// network devices are reached that way, unless `-6`).
fn resolve(host: &str, family: Option<Family>) -> Result<IpAddr, String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<IpAddr> = (host, 0)
        .to_socket_addrs()
        .map_err(|e| tr!("{host} 을(를) 찾을 수 없습니다: {e}", "Cannot find {host}: {e}"))?
        .map(|a| a.ip())
        .collect();
    let pick = |v4: bool| addrs.iter().copied().find(|a| a.is_ipv4() == v4);
    match family {
        Some(Family::V4) => pick(true),
        Some(Family::V6) => pick(false),
        None => pick(true).or_else(|| pick(false)),
    }
    .ok_or_else(|| match family {
        Some(Family::V4) => tr!("{host} 에 IPv4 주소가 없습니다", "{host} has no IPv4 address"),
        Some(Family::V6) => tr!("{host} 에 IPv6 주소가 없습니다", "{host} has no IPv6 address"),
        None => tr!("{host} 을(를) 찾을 수 없습니다", "Cannot find {host}"),
    })
}

/// The data of a request, the same letters Windows' `ping` sends.
fn payload(size: u16) -> Vec<u8> {
    b"abcdefghijklmnopqrstuvw".iter().copied().cycle().take(size.into()).collect()
}

/// What an error status of the ICMP API means.
pub fn status_text(status: u32) -> String {
    match status {
        11002 => t("대상 네트워크에 연결할 수 없습니다", "Destination net unreachable").into(),
        11003 | 11040 => t("대상 호스트에 연결할 수 없습니다", "Destination host unreachable").into(),
        11004 => t("대상 프로토콜에 연결할 수 없습니다", "Destination protocol unreachable").into(),
        11005 => t("대상 포트에 연결할 수 없습니다", "Destination port unreachable").into(),
        11013 => t("전송 중 TTL 이 만료되었습니다", "TTL expired in transit").into(),
        11014 => t("재조립 중 TTL 이 만료되었습니다", "TTL expired during reassembly").into(),
        11009 => t("패킷이 너무 큽니다", "Packet too big").into(),
        11050 => t("일반 오류", "General failure").into(),
        _ => tr!("오류 (상태 {status})", "Error (status {status})"),
    }
}

/// Counts of a ping, for its summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PingStats {
    pub sent: u32,
    pub received: u32,
    pub min_ms: u32,
    pub max_ms: u32,
    total_ms: u64,
}

impl PingStats {
    /// Counts one answered or unanswered request (not [`PingEvent::Start`] and the like).
    pub fn record(&mut self, event: &PingEvent) {
        match event {
            PingEvent::Reply { time_ms, .. } => {
                self.sent += 1;
                if self.received == 0 {
                    (self.min_ms, self.max_ms) = (*time_ms, *time_ms);
                }
                self.received += 1;
                self.min_ms = self.min_ms.min(*time_ms);
                self.max_ms = self.max_ms.max(*time_ms);
                self.total_ms += u64::from(*time_ms);
            }
            PingEvent::TimedOut | PingEvent::Failed { .. } => self.sent += 1,
            _ => {}
        }
    }

    pub fn lost(&self) -> u32 {
        self.sent - self.received
    }

    /// Lost requests in percent.
    pub fn loss_percent(&self) -> u32 {
        (self.lost() * 100).checked_div(self.sent).unwrap_or(0)
    }

    pub fn average_ms(&self) -> u32 {
        self.total_ms.checked_div(u64::from(self.received)).unwrap_or(0) as u32
    }
}

#[cfg(windows)]
mod imp {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use playterm_i18n::tr;
    use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ICMP_ECHO_REPLY, ICMPV6_ECHO_REPLY_LH, IP_REQ_TIMED_OUT, IP_SUCCESS, Icmp6CreateFile, Icmp6ParseReplies,
        Icmp6SendEcho2, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET6, IN6_ADDR, IN6_ADDR_0, SOCKADDR_IN6, SOCKADDR_IN6_0};

    use super::PingEvent;

    /// An ICMP handle of the API (IPv4 or IPv6).
    pub struct Icmp(HANDLE);

    // The handle is only used from the thread that owns it.
    unsafe impl Send for Icmp {}

    impl Icmp {
        pub fn open(addr: IpAddr) -> Result<Self, String> {
            // SAFETY: plain calls without arguments; the result is checked.
            let handle = unsafe { if addr.is_ipv4() { IcmpCreateFile() } else { Icmp6CreateFile() } };
            if handle == INVALID_HANDLE_VALUE || handle.is_null() {
                // SAFETY: reads the calling thread's last error.
                let error = std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32);
                return Err(tr!("ICMP 를 사용할 수 없습니다: {error}", "ICMP is not available: {error}"));
            }
            Ok(Self(handle))
        }

        pub fn echo(&self, addr: IpAddr, data: &[u8], timeout_ms: u32) -> PingEvent {
            match addr {
                IpAddr::V4(ip) => self.echo4(ip, data, timeout_ms),
                IpAddr::V6(ip) => self.echo6(ip, data, timeout_ms),
            }
        }

        fn echo4(&self, ip: Ipv4Addr, data: &[u8], timeout_ms: u32) -> PingEvent {
            // Room for the reply structure, the echoed data and an ICMP error message; u64s
            // keep the structure (it holds a pointer) aligned.
            let mut buffer = vec![0u64; (size_of::<ICMP_ECHO_REPLY>() + data.len() + 64).div_ceil(8)];
            let size = (buffer.len() * 8) as u32;
            // SAFETY: `data` and `buffer` are valid for the sizes given; the reply is read only
            // from the buffer the call filled.
            let (count, reply) = unsafe {
                let count = IcmpSendEcho(
                    self.0,
                    u32::from_ne_bytes(ip.octets()),
                    data.as_ptr().cast(),
                    data.len() as u16,
                    std::ptr::null(),
                    buffer.as_mut_ptr().cast(),
                    size,
                    timeout_ms,
                );
                (count, *buffer.as_ptr().cast::<ICMP_ECHO_REPLY>())
            };
            let status = if count == 0 {
                // SAFETY: reads the calling thread's last error.
                unsafe { GetLastError() }
            } else {
                reply.Status
            };
            let from = (reply.Address != 0).then(|| IpAddr::V4(Ipv4Addr::from(reply.Address.to_ne_bytes())));
            match (status, from) {
                (IP_SUCCESS, Some(from)) => PingEvent::Reply {
                    from,
                    bytes: reply.DataSize.into(),
                    time_ms: reply.RoundTripTime,
                    ttl: Some(reply.Options.Ttl),
                },
                (IP_REQ_TIMED_OUT, _) => PingEvent::TimedOut,
                (status, from) => PingEvent::Failed { from, status },
            }
        }

        fn echo6(&self, ip: Ipv6Addr, data: &[u8], timeout_ms: u32) -> PingEvent {
            let sockaddr = |addr: [u8; 16]| SOCKADDR_IN6 {
                sin6_family: AF_INET6,
                sin6_port: 0,
                sin6_flowinfo: 0,
                sin6_addr: IN6_ADDR { u: IN6_ADDR_0 { Byte: addr } },
                Anonymous: SOCKADDR_IN6_0 { sin6_scope_id: 0 },
            };
            let source = sockaddr([0; 16]);
            let target = sockaddr(ip.octets());
            let mut buffer = vec![0u64; (size_of::<ICMPV6_ECHO_REPLY_LH>() + data.len() + 128).div_ceil(8)];
            let size = (buffer.len() * 8) as u32;
            // SAFETY: as in `echo4`; the call is synchronous (no event, no APC routine).
            let (count, reply) = unsafe {
                let count = Icmp6SendEcho2(
                    self.0,
                    std::ptr::null_mut(),
                    None,
                    std::ptr::null(),
                    &source,
                    &target,
                    data.as_ptr().cast(),
                    data.len() as u16,
                    std::ptr::null(),
                    buffer.as_mut_ptr().cast(),
                    size,
                    timeout_ms,
                );
                if count > 0 {
                    Icmp6ParseReplies(buffer.as_mut_ptr().cast(), size);
                }
                (count, buffer.as_ptr().cast::<ICMPV6_ECHO_REPLY_LH>().read_unaligned())
            };
            let status = if count == 0 {
                // SAFETY: reads the calling thread's last error.
                unsafe { GetLastError() }
            } else {
                reply.Status
            };
            let words = reply.Address.sin6_addr;
            let mut octets = [0u8; 16];
            for (i, word) in words.iter().enumerate() {
                octets[i * 2..i * 2 + 2].copy_from_slice(&word.to_ne_bytes());
            }
            let from = (octets != [0; 16]).then(|| IpAddr::V6(Ipv6Addr::from(octets)));
            match (status, from) {
                (IP_SUCCESS, Some(from)) => {
                    PingEvent::Reply { from, bytes: data.len(), time_ms: reply.RoundTripTime, ttl: None }
                }
                (IP_REQ_TIMED_OUT, _) => PingEvent::TimedOut,
                (status, from) => PingEvent::Failed { from, status },
            }
        }
    }

    impl Drop for Icmp {
        fn drop(&mut self) {
            // SAFETY: the handle came from IcmpCreateFile / Icmp6CreateFile and is closed once.
            unsafe { IcmpCloseHandle(self.0) };
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::net::IpAddr;

    use super::PingEvent;

    pub struct Icmp;

    impl Icmp {
        pub fn open(_: IpAddr) -> Result<Self, String> {
            Err(playterm_i18n::t("이 운영체제에서는 ping 을 지원하지 않습니다", "ping is not supported on this system").into())
        }

        pub fn echo(&self, _: IpAddr, _: &[u8], _: u32) -> PingEvent {
            PingEvent::TimedOut
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statistics() {
        let mut stats = PingStats::default();
        let reply = |time_ms| PingEvent::Reply { from: [10, 0, 0, 1].into(), bytes: 32, time_ms, ttl: Some(64) };
        for event in [PingEvent::Start([10, 0, 0, 1].into()), reply(12), PingEvent::TimedOut, reply(4), reply(20)] {
            stats.record(&event);
        }
        stats.record(&PingEvent::Failed { from: None, status: 11003 });
        assert_eq!((stats.sent, stats.received, stats.lost(), stats.loss_percent()), (5, 3, 2, 40));
        assert_eq!((stats.min_ms, stats.max_ms, stats.average_ms()), (4, 20, 12));
        assert_eq!(PingStats::default().loss_percent(), 0);
    }

    #[test]
    fn payload_and_names() {
        assert_eq!(payload(26), b"abcdefghijklmnopqrstuvwabc");
        assert!(payload(0).is_empty());
        assert_eq!(resolve("127.0.0.1", None), Ok([127, 0, 0, 1].into()));
        assert_eq!(resolve("[::1]", None), Ok(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)));
        assert!(resolve("127.0.0.1", Some(Family::V6)).is_err());
        assert!(status_text(11013).contains("TTL"));
    }

    /// Pings this PC (the loopback address answers without any network).
    #[cfg(windows)]
    #[test]
    fn pings_localhost() {
        let ping = Ping::start("127.0.0.1".into(), PingOptions { count: Some(2), ..Default::default() }, Arc::new(|| {}));
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !events.contains(&PingEvent::Done) && Instant::now() < deadline {
            match ping.try_recv() {
                Some(event) => events.push(event),
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert_eq!(events.first(), Some(&PingEvent::Start([127, 0, 0, 1].into())));
        let replies = events.iter().filter(|e| matches!(e, PingEvent::Reply { bytes: 32, .. })).count();
        assert_eq!(replies, 2, "{events:?}");
        assert_eq!(events.last(), Some(&PingEvent::Done));
    }
}
