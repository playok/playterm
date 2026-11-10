//! X11 forwarding (`ssh -X`): the session asks the server for X11 forwarding with a made-up
//! cookie; X programs on the server then connect back through `x11` channels, which are joined
//! to the X server on this PC (VcXsrv or Xming on Windows, XQuartz on macOS, the desktop's on
//! Linux).
//!
//! Like OpenSSH, the cookie the server sees is not the real one: each connection's first packet
//! (the X11 connection setup) must carry it, or the connection is dropped — so only programs of
//! this session reach the display. It is then replaced by:
//!
//! - untrusted (`ssh -X`, the default): a cookie `xauth` generates as *untrusted*, with which
//!   the X server (SECURITY extension) keeps the remote programs from reading other windows or
//!   the keyboard. When none can be made (no `xauth`, or an X server without the extension),
//!   X11 connections are refused rather than given full access.
//! - trusted (`ssh -Y`, chosen by the user): the display's real cookie from `xauth`, or no
//!   authorization (an X server that asks for none). The server's programs then have the whole
//!   display, keyboard included.

use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use playterm_i18n::{t, tr};

pub const AUTH_PROTOCOL: &str = "MIT-MAGIC-COOKIE-1";
/// X11's first TCP port (display 0).
const X_TCP_PORT: u16 = 6000;
/// A setup packet larger than this is not a real one.
const MAX_AUTH: usize = 4096;

/// Where the X server listens, read from a `DISPLAY` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayTarget {
    /// `host:n`: TCP port 6000 + n.
    Tcp { host: String, display: u16 },
    /// `:n` (or `unix:n`): the local socket `/tmp/.X11-unix/Xn`.
    Local { display: u16 },
    /// `/path/to/socket:n` (XQuartz's launchd socket on macOS).
    Socket { path: String, display: u16 },
}

/// The display named by `value` (e.g. `localhost:0.0`, `:0`, `/private/tmp/…/org.xquartz:0`)
/// and its screen.
pub fn parse_display(value: &str) -> Result<(DisplayTarget, u32), String> {
    let value = value.trim();
    let bad = || tr!("x11.not_a_display_value_e_g_localhost_0", value = value);
    let (host, rest) = value.rsplit_once(':').ok_or_else(bad)?;
    let (display, screen) = match rest.split_once('.') {
        Some((d, s)) => (d, s.parse::<u32>().map_err(|_| bad())?),
        None => (rest, 0),
    };
    let display: u16 = display.parse().map_err(|_| bad())?;
    if display > u16::MAX - X_TCP_PORT {
        return Err(bad());
    }
    let target = match host {
        "" | "unix" => DisplayTarget::Local { display },
        h if h.starts_with('/') => DisplayTarget::Socket { path: h.to_owned(), display },
        h => DisplayTarget::Tcp { host: h.trim_matches(['[', ']']).to_owned(), display },
    };
    Ok((target, screen))
}

/// The display to use when none is set for the site: `DISPLAY` of this PC, or `localhost:0.0`
/// (where VcXsrv and Xming listen by default).
pub fn default_display() -> String {
    std::env::var("DISPLAY").ok().filter(|d| !d.trim().is_empty()).unwrap_or_else(|| "localhost:0.0".into())
}

/// What an X11 connection takes to the display in place of the session's made-up cookie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayAuth {
    /// This MIT-MAGIC-COOKIE-1 (untrusted, or the display's real one when trusted).
    Cookie(Vec<u8>),
    /// No authorization (trusted, and `xauth` has nothing for the display).
    Open,
    /// Connections are refused, for this reason (untrusted, and no untrusted cookie).
    Refuse(String),
}

/// The forwarding of one session: the display, the cookie shown to the server, and what
/// connections take to the display.
#[derive(Clone)]
pub struct X11Forward {
    pub display: String,
    target: DisplayTarget,
    pub screen: u32,
    fake: Vec<u8>,
    auth: DisplayAuth,
}

impl X11Forward {
    /// Prepares forwarding to `display`: a new random cookie for the server, and an untrusted
    /// cookie for the display (or with `trusted`, its real one).
    pub async fn new(display: &str, trusted: bool) -> anyhow::Result<Self> {
        let (target, screen) = parse_display(display).map_err(|e| anyhow!(e))?;
        let mut fake = vec![0u8; 16];
        getrandom::fill(&mut fake).map_err(|e| anyhow!("random: {e}"))?;
        let auth = if trusted {
            real_cookie(display).await.map_or(DisplayAuth::Open, DisplayAuth::Cookie)
        } else {
            match untrusted_cookie(display).await {
                Ok(cookie) => DisplayAuth::Cookie(cookie),
                Err(why) => DisplayAuth::Refuse(tr!("x11.no_untrusted_x11_cookie_could_be_made_so", why = why)),
            }
        };
        Ok(Self { display: display.to_owned(), target, screen, fake, auth })
    }

    /// For tests: forwarding with a known made-up cookie and display authorization.
    pub fn with_auth(display: &str, fake: Vec<u8>, auth: DisplayAuth) -> Result<Self, String> {
        let (target, screen) = parse_display(display)?;
        Ok(Self { display: display.to_owned(), target, screen, fake, auth })
    }

    /// Why X11 connections will be refused, if they will (to tell the user up front).
    pub fn refusal(&self) -> Option<&str> {
        match &self.auth {
            DisplayAuth::Refuse(why) => Some(why),
            _ => None,
        }
    }

    /// The cookie to send in the `x11-req`, in hex.
    pub fn fake_cookie_hex(&self) -> String {
        self.fake.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Joins one X11 connection from the server (`remote`) to the display, once its setup
    /// packet shows the session's cookie.
    pub async fn relay<S: AsyncRead + AsyncWrite + Unpin>(&self, mut remote: S) -> anyhow::Result<()> {
        let setup = read_setup(&mut remote).await?;
        if setup.auth_name != AUTH_PROTOCOL.as_bytes() || setup.auth_data != self.fake {
            bail!(tr!("x11.an_x11_connection_from_the_server_did_not"));
        }
        let first = match &self.auth {
            DisplayAuth::Cookie(cookie) => setup.with_auth(AUTH_PROTOCOL.as_bytes(), cookie),
            DisplayAuth::Open => setup.with_auth(b"", b""),
            DisplayAuth::Refuse(why) => bail!("{why}"),
        };
        match &self.target {
            DisplayTarget::Tcp { host, display } => {
                let port = X_TCP_PORT + display;
                let mut local = tokio::net::TcpStream::connect((host.as_str(), port)).await.with_context(|| no_server(&self.display))?;
                local.set_nodelay(true)?;
                local.write_all(&first).await?;
                tokio::io::copy_bidirectional(&mut remote, &mut local).await?;
            }
            #[cfg(unix)]
            DisplayTarget::Local { display } => {
                let path = format!("/tmp/.X11-unix/X{display}");
                let mut local = tokio::net::UnixStream::connect(&path).await.with_context(|| no_server(&self.display))?;
                local.write_all(&first).await?;
                tokio::io::copy_bidirectional(&mut remote, &mut local).await?;
            }
            #[cfg(unix)]
            DisplayTarget::Socket { path, .. } => {
                let mut local = tokio::net::UnixStream::connect(path).await.with_context(|| no_server(&self.display))?;
                local.write_all(&first).await?;
                tokio::io::copy_bidirectional(&mut remote, &mut local).await?;
            }
            #[cfg(not(unix))]
            DisplayTarget::Local { .. } | DisplayTarget::Socket { .. } => {
                bail!(tr!("x11.on_windows_give_the_x_server_as_a", arg0 = self.display));
            }
        }
        Ok(())
    }
}

fn no_server(display: &str) -> String {
    tr!("x11.cannot_reach_the_x_server_is_one_running", display = display)
}

/// The X11 connection setup packet a client sends first.
struct Setup {
    /// Byte order, version (the first 6 bytes, kept as they are).
    head: [u8; 6],
    big_endian: bool,
    auth_name: Vec<u8>,
    auth_data: Vec<u8>,
}

fn pad4(n: usize) -> usize {
    (4 - n % 4) % 4
}

async fn read_setup<S: AsyncRead + Unpin>(stream: &mut S) -> anyhow::Result<Setup> {
    let mut fixed = [0u8; 12];
    tokio::time::timeout(Duration::from_secs(30), stream.read_exact(&mut fixed)).await.context("X11 setup")??;
    let big_endian = match fixed[0] {
        b'B' => true,
        b'l' => false,
        _ => bail!("not an X11 connection"),
    };
    let num = |a: u8, b: u8| if big_endian { u16::from_be_bytes([a, b]) } else { u16::from_le_bytes([a, b]) } as usize;
    let (name_len, data_len) = (num(fixed[6], fixed[7]), num(fixed[8], fixed[9]));
    if name_len + data_len > MAX_AUTH {
        bail!("X11 setup too large");
    }
    let mut rest = vec![0u8; name_len + pad4(name_len) + data_len + pad4(data_len)];
    stream.read_exact(&mut rest).await?;
    let auth_name = rest[..name_len].to_vec();
    let start = name_len + pad4(name_len);
    let auth_data = rest[start..start + data_len].to_vec();
    let mut head = [0u8; 6];
    head.copy_from_slice(&fixed[..6]);
    Ok(Setup { head, big_endian, auth_name, auth_data })
}

impl Setup {
    /// The packet again with this authorization.
    fn with_auth(&self, name: &[u8], data: &[u8]) -> Vec<u8> {
        let num = |n: usize| if self.big_endian { (n as u16).to_be_bytes() } else { (n as u16).to_le_bytes() };
        let mut out = self.head.to_vec();
        out.extend_from_slice(&num(name.len()));
        out.extend_from_slice(&num(data.len()));
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name);
        out.extend(std::iter::repeat_n(0, pad4(name.len())));
        out.extend_from_slice(data);
        out.extend(std::iter::repeat_n(0, pad4(data.len())));
        out
    }
}

/// Windows must not search the current working directory or PATH for an executable.
fn xauth_program(configured: Option<std::ffi::OsString>) -> Result<std::path::PathBuf, String> {
    if let Some(path) = configured {
        let path = std::path::PathBuf::from(path);
        if !path.is_absolute() {
            return Err(t!("x11.playterm_xauth_must_be_an_absolute_path_to").into());
        }
        return Ok(path);
    }
    if cfg!(windows) { Err(t!("x11.set_playterm_xauth_to_the_absolute_path_of").into()) } else { Ok("xauth".into()) }
}

/// An untrusted cookie generated into its own file, as with OpenSSH's `ssh -X`.
async fn untrusted_cookie(display: &str) -> Result<Vec<u8>, String> {
    let program = xauth_program(std::env::var_os("PLAYTERM_XAUTH"))?;
    let display = display.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut name = [0u8; 8];
        getrandom::fill(&mut name).map_err(|e| e.to_string())?;
        let hex: String = name.iter().map(|b| format!("{b:02x}")).collect();
        let file = std::env::temp_dir().join(format!("playterm-xauth-{hex}"));
        let run = |args: &[&str]| std::process::Command::new(&program).arg("-q").arg("-f").arg(&file).args(args).output();
        // The cookie lasts while it is in use, and 12 hours after the last X connection ended.
        let generated = run(&["generate", &display, AUTH_PROTOCOL, "untrusted", "timeout", "43200"]);
        let listed = run(&["list"]);
        let _ = std::fs::remove_file(&file);
        let generated = generated.map_err(|e| tr!("x11.cannot_run_xauth", e = e))?;
        if !generated.status.success() {
            let err = String::from_utf8_lossy(&generated.stderr).trim().to_owned();
            return Err(tr!("x11.xauth_generate_failed", err = err));
        }
        let listed = listed.map_err(|e| e.to_string())?;
        cookie_from_xauth(&String::from_utf8_lossy(&listed.stdout)).ok_or_else(|| t!("x11.xauth_made_no_cookie").into())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The display's MIT-MAGIC-COOKIE-1 from `xauth list`, if `xauth` is there and knows it.
async fn real_cookie(display: &str) -> Option<Vec<u8>> {
    let program = xauth_program(std::env::var_os("PLAYTERM_XAUTH")).ok()?;
    let display = display.to_owned();
    let output =
        tokio::task::spawn_blocking(move || std::process::Command::new(program).arg("list").arg(&display).output()).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    cookie_from_xauth(&String::from_utf8_lossy(&output.stdout))
}

/// The first MIT-MAGIC-COOKIE-1 in `xauth list` output (`host/unix:0  MIT-MAGIC-COOKIE-1  hex`).
fn cookie_from_xauth(list: &str) -> Option<Vec<u8>> {
    list.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (_, proto, hex) = (fields.next()?, fields.next()?, fields.next()?);
        if proto != AUTH_PROTOCOL || hex.len() % 2 != 0 {
            return None;
        }
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xauth_requires_an_explicit_absolute_path_on_windows() {
        assert!(xauth_program(Some("xauth".into())).is_err());
        assert!(xauth_program(Some("./xauth".into())).is_err());
        let absolute = std::env::temp_dir().join("trusted-xauth");
        assert_eq!(xauth_program(Some(absolute.clone().into_os_string())).unwrap(), absolute);
        assert_eq!(xauth_program(None).is_err(), cfg!(windows));
    }

    #[test]
    fn reads_display_values() {
        assert_eq!(parse_display("localhost:0.0").unwrap(), (DisplayTarget::Tcp { host: "localhost".into(), display: 0 }, 0));
        assert_eq!(parse_display("10.0.0.5:10.1").unwrap(), (DisplayTarget::Tcp { host: "10.0.0.5".into(), display: 10 }, 1));
        assert_eq!(parse_display(":0").unwrap(), (DisplayTarget::Local { display: 0 }, 0));
        assert_eq!(parse_display("unix:1").unwrap().0, DisplayTarget::Local { display: 1 });
        assert_eq!(
            parse_display("/private/tmp/com.apple.launchd.x/org.xquartz:0").unwrap().0,
            DisplayTarget::Socket { path: "/private/tmp/com.apple.launchd.x/org.xquartz".into(), display: 0 }
        );
        for bad in ["", "localhost", "host:x", "host:0.y", "host:99999", "host:60000"] {
            assert!(parse_display(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn untrusted_without_xauth_refuses() {
        // Unless this machine has xauth and an X server at :0 that makes untrusted cookies,
        // nothing gets the display.
        let forward = X11Forward::new("127.0.0.1:59000", false).await.unwrap();
        assert!(forward.refusal().is_some(), "no untrusted cookie for a display that does not exist");
        let trusted = X11Forward::new("127.0.0.1:59000", true).await.unwrap();
        assert!(trusted.refusal().is_none());
    }

    #[test]
    fn reads_xauth_cookies() {
        let list = "mac/unix:0  MIT-MAGIC-COOKIE-1  00ff10ab\nother  XDM-AUTHORIZATION-1  1234\n";
        assert_eq!(cookie_from_xauth(list), Some(vec![0x00, 0xff, 0x10, 0xab]));
        assert_eq!(cookie_from_xauth("x  MIT-MAGIC-COOKIE-1  zz"), None);
        assert_eq!(cookie_from_xauth(""), None);
    }

    fn setup_packet(big: bool, name: &[u8], data: &[u8]) -> Vec<u8> {
        let setup = Setup {
            head: if big { [b'B', 0, 0, 11, 0, 0] } else { [b'l', 0, 11, 0, 0, 0] },
            big_endian: big,
            auth_name: Vec::new(),
            auth_data: Vec::new(),
        };
        setup.with_auth(name, data)
    }

    #[tokio::test]
    async fn replaces_the_cookie_and_relays() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // A display number whose port is this listener's (port - 6000 must fit).
        let Some(display) = port.checked_sub(X_TCP_PORT) else { return };
        let forward = X11Forward::with_auth(&format!("127.0.0.1:{display}.0"), vec![7; 16], DisplayAuth::Cookie(vec![9; 16])).unwrap();
        for big in [false, true] {
            let (mut remote, theirs) = tokio::io::duplex(4096);
            let f = forward.clone();
            let relay = tokio::spawn(async move { f.relay(theirs).await });
            remote.write_all(&[setup_packet(big, AUTH_PROTOCOL.as_bytes(), &[7; 16]), b"hello".to_vec()].concat()).await.unwrap();
            let (mut x, _) = listener.accept().await.unwrap();
            let mut got = vec![0u8; setup_packet(big, AUTH_PROTOCOL.as_bytes(), &[9; 16]).len() + 5];
            x.read_exact(&mut got).await.unwrap();
            assert_eq!(got, [setup_packet(big, AUTH_PROTOCOL.as_bytes(), &[9; 16]), b"hello".to_vec()].concat());
            x.write_all(b"reply").await.unwrap();
            let mut reply = [0u8; 5];
            remote.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"reply");
            drop(x);
            drop(remote);
            let _ = relay.await;
        }

        // A wrong cookie never reaches the display.
        let (mut remote, theirs) = tokio::io::duplex(4096);
        let f = forward.clone();
        let relay = tokio::spawn(async move { f.relay(theirs).await });
        remote.write_all(&setup_packet(false, AUTH_PROTOCOL.as_bytes(), &[1; 16])).await.unwrap();
        assert!(relay.await.unwrap().is_err());
        assert!(tokio::time::timeout(Duration::from_millis(200), listener.accept()).await.is_err());

        // Untrusted without an untrusted cookie: refused even with the right cookie.
        let refused = X11Forward::with_auth(&format!("127.0.0.1:{display}"), vec![7; 16], DisplayAuth::Refuse("no".into())).unwrap();
        assert_eq!(refused.refusal(), Some("no"));
        let (mut remote, theirs) = tokio::io::duplex(4096);
        let relay = tokio::spawn(async move { refused.relay(theirs).await });
        remote.write_all(&setup_packet(false, AUTH_PROTOCOL.as_bytes(), &[7; 16])).await.unwrap();
        assert!(relay.await.unwrap().is_err());
        assert!(tokio::time::timeout(Duration::from_millis(200), listener.accept()).await.is_err());

        // Trusted, and no real cookie: no authorization is passed on.
        let open = X11Forward::with_auth(&format!("127.0.0.1:{display}"), vec![7; 16], DisplayAuth::Open).unwrap();
        let (mut remote, theirs) = tokio::io::duplex(4096);
        tokio::spawn(async move { open.relay(theirs).await });
        remote.write_all(&setup_packet(false, AUTH_PROTOCOL.as_bytes(), &[7; 16])).await.unwrap();
        let (mut x, _) = listener.accept().await.unwrap();
        let mut got = vec![0u8; 12];
        x.read_exact(&mut got).await.unwrap();
        assert_eq!(got, setup_packet(false, b"", b""));
    }
}
