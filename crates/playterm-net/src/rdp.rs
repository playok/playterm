//! RDP sessions (Windows Remote Desktop) on the IronRDP protocol crates.
//!
//! The connection runs TCP → TLS → NLA (CredSSP), then the active session decodes the screen into
//! an RGBA image and sends input as fast-path events. Each session runs on a thread of its own
//! with a single-threaded runtime; the Windows clipboard backend gets another thread, whose window
//! is served by a blocking message loop even while the session waits on the clipboard.
//!
//! The decoded image is shared with the UI, which copies only what changed since it last looked
//! (woken at most ~60 times per second). The server's TLS certificate is pinned like an SSH host
//! key: confirmed on first use, refused if it changes.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use ironrdp_cliprdr::CliprdrClient;
use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy, CliprdrBackendFactory};
use ironrdp_connector::connection_activation::ConnectionActivationState;
use ironrdp_connector::sspi::generator::NetworkRequest;
use ironrdp_connector::{
    BitmapConfig, ClientConnector, ConnectorError, ConnectorErrorKind, ConnectorResult, Credentials, DesktopSize,
    ServerName,
};
use ironrdp_core::WriteBuf;
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_dvc::DrdynvcClient;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::gcc::{ConnectionType, KeyboardType};
use ironrdp_pdu::input::MousePdu;
use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::rdp::capability_sets::{MajorPlatformType, RailSupportLevel, client_codecs_capabilities};
use ironrdp_pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput, fast_path};
use ironrdp_tokio::{FramedWrite as _, TokioFramed};
use playterm_i18n::{t, tr};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use smallvec::SmallVec;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::session::{ConnectParams, EventSink, PromptField, SessionCommand, SessionEvent};

/// Wrong passwords tried before giving up.
const MAX_PASSWORD_TRIES: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Shortest time between two screens sent to the UI.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Per-site RDP options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RdpSettings {
    /// Desktop size in pixels; 0 x 0 follows the size of the tab.
    pub width: u16,
    pub height: u16,
    /// Share the clipboard with the remote desktop.
    pub clipboard: bool,
}

impl Default for RdpSettings {
    fn default() -> Self {
        Self { width: 0, height: 0, clipboard: true }
    }
}

impl RdpSettings {
    /// Desktop sizes offered in the site editor; (0, 0) follows the tab.
    pub const SIZES: [(u16, u16); 6] = [(0, 0), (1920, 1080), (1600, 900), (1366, 768), (1280, 720), (1024, 768)];

    pub fn fits_tab(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// A changed part of the remote screen, for the UI.
pub struct RdpFrame {
    /// Size of the whole desktop.
    pub width: u16,
    pub height: u16,
    /// The changed rectangle: x, y, width, height (the whole desktop after a size change).
    pub rect: [u16; 4],
    /// `rect` pixels, RGBA (opaque), row by row.
    pub rgba: Vec<u8>,
}

/// The decoded remote screen, shared by the session (which draws into it) and the UI (which copies
/// the changed parts out).
pub(crate) struct SharedScreen {
    image: DecodedImage,
    /// Union of the changes not yet taken: left, top, right, bottom (exclusive).
    dirty: Option<[u16; 4]>,
}

impl Default for SharedScreen {
    fn default() -> Self {
        Self { image: DecodedImage::new(PixelFormat::RgbA32, 0, 0), dirty: None }
    }
}

/// The smallest rectangle (left, top, right, bottom) holding both.
fn union(a: [u16; 4], b: [u16; 4]) -> [u16; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// `rect` (left, top, right, bottom exclusive) of an RGBA image with rows of `stride` bytes.
fn copy_rect(data: &[u8], stride: usize, rect: [u16; 4]) -> Vec<u8> {
    let [left, top, right, bottom] = rect.map(usize::from);
    let mut rgba = Vec::with_capacity((right - left) * 4 * (bottom - top));
    for y in top..bottom {
        rgba.extend_from_slice(&data[y * stride + left * 4..y * stride + right * 4]);
    }
    rgba
}

impl SharedScreen {
    /// A new, black desktop of this size; the UI starts over with the whole picture.
    pub(crate) fn reset(&mut self, width: u16, height: u16) {
        self.image = DecodedImage::new(PixelFormat::RgbA32, width, height);
        self.dirty = Some([0, 0, width, height]);
    }

    /// Notes a changed `rect` (left, top, right, bottom exclusive) for the UI.
    pub(crate) fn changed(&mut self, rect: [u16; 4]) {
        let (width, height) = (self.image.width(), self.image.height());
        let rect = [rect[0].min(width), rect[1].min(height), rect[2].min(width), rect[3].min(height)];
        if rect[0] < rect[2] && rect[1] < rect[3] {
            self.dirty = Some(self.dirty.map_or(rect, |d| union(d, rect)));
        }
    }

    /// The changes since the last call, if any.
    pub(crate) fn take(&mut self) -> Option<RdpFrame> {
        let rect = self.dirty.take()?;
        let rgba = copy_rect(self.image.data(), self.image.stride(), rect);
        let [left, top, right, bottom] = rect;
        Some(RdpFrame {
            width: self.image.width(),
            height: self.image.height(),
            rect: [left, top, right - left, bottom - top],
            rgba,
        })
    }
}

fn lock(screen: &Mutex<SharedScreen>) -> MutexGuard<'_, SharedScreen> {
    screen.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RdpMouseButton {
    Left,
    Right,
    Middle,
}

/// Input for the remote desktop, in desktop pixel coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RdpInput {
    /// A key by its PS/2 set-1 scan code; `0xE0xx` for extended keys (arrows, Insert, right Ctrl, ...).
    Key { scancode: u16, pressed: bool },
    /// Characters, typed as Unicode keyboard events (layout and IME independent).
    Text(String),
    MouseMove { x: u16, y: u16 },
    MouseButton { button: RdpMouseButton, pressed: bool, x: u16, y: u16 },
    /// Wheel rotation in 1/120 notches; positive turns away from the user (up / right).
    Wheel { delta: i16, horizontal: bool, x: u16, y: u16 },
    /// The desktop should take this size (when it follows the tab).
    Resize { width: u16, height: u16 },
}

pub mod scancode {
    //! Set-1 scan codes of keys the input code sends by name.
    pub const ESCAPE: u16 = 0x01;
    pub const BACKSPACE: u16 = 0x0E;
    pub const TAB: u16 = 0x0F;
    pub const ENTER: u16 = 0x1C;
    pub const LEFT_CTRL: u16 = 0x1D;
    pub const LEFT_SHIFT: u16 = 0x2A;
    pub const LEFT_ALT: u16 = 0x38;
    pub const RIGHT_ALT: u16 = 0xE038;
    pub const C: u16 = 0x2E;
    pub const V: u16 = 0x2F;
    pub const X: u16 = 0x2D;
}

/// Appends the fast-path events for `input` (everything but [`RdpInput::Resize`]).
fn fast_path(input: &RdpInput, out: &mut SmallVec<[FastPathInputEvent; 2]>) {
    let mouse = |flags: PointerFlags, units: i16, x: u16, y: u16| {
        FastPathInputEvent::MouseEvent(MousePdu { flags, number_of_wheel_rotation_units: units, x_position: x, y_position: y })
    };
    match input {
        RdpInput::Key { scancode, pressed } => {
            let mut flags = KeyboardFlags::empty();
            if !pressed {
                flags |= KeyboardFlags::RELEASE;
            }
            if scancode & 0xFF00 == 0xE000 {
                flags |= KeyboardFlags::EXTENDED;
            }
            out.push(FastPathInputEvent::KeyboardEvent(flags, (scancode & 0xFF) as u8));
        }
        RdpInput::Text(text) => {
            for unit in text.encode_utf16() {
                out.push(FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::empty(), unit));
                out.push(FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::RELEASE, unit));
            }
        }
        RdpInput::MouseMove { x, y } => out.push(mouse(PointerFlags::MOVE, 0, *x, *y)),
        RdpInput::MouseButton { button, pressed, x, y } => {
            let mut flags = match button {
                RdpMouseButton::Left => PointerFlags::LEFT_BUTTON,
                RdpMouseButton::Right => PointerFlags::RIGHT_BUTTON,
                RdpMouseButton::Middle => PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
            };
            if *pressed {
                flags |= PointerFlags::DOWN;
            }
            out.push(mouse(flags, 0, *x, *y));
        }
        RdpInput::Wheel { delta, horizontal, x, y } => {
            let flags = if *horizontal { PointerFlags::HORIZONTAL_WHEEL } else { PointerFlags::VERTICAL_WHEEL };
            // The wire field is 9-bit two's complement.
            out.push(mouse(flags, (*delta).clamp(-255, 255), *x, *y));
        }
        RdpInput::Resize { .. } => {}
    }
}

/// Text typed through the terminal path (the command bar, simultaneous input): Enter, Tab,
/// Backspace, Esc and Ctrl+C become keys, the rest Unicode characters.
fn bytes_to_inputs(bytes: &[u8]) -> Vec<RdpInput> {
    let key = |scancode: u16, out: &mut Vec<RdpInput>| {
        out.push(RdpInput::Key { scancode, pressed: true });
        out.push(RdpInput::Key { scancode, pressed: false });
    };
    let mut out = Vec::new();
    let mut text = String::new();
    let mut prev = '\0';
    for ch in String::from_utf8_lossy(bytes).chars() {
        let special = match ch {
            '\n' if prev == '\r' => {
                prev = ch;
                continue;
            }
            '\r' | '\n' => Some(scancode::ENTER),
            '\t' => Some(scancode::TAB),
            '\x08' | '\x7f' => Some(scancode::BACKSPACE),
            '\x1b' => Some(scancode::ESCAPE),
            '\x03' => Some(scancode::C),
            c if c.is_control() => {
                prev = ch;
                continue;
            }
            _ => None,
        };
        prev = ch;
        match special {
            None => text.push(ch),
            Some(code) => {
                if !text.is_empty() {
                    out.push(RdpInput::Text(std::mem::take(&mut text)));
                }
                if ch == '\x03' {
                    out.push(RdpInput::Key { scancode: scancode::LEFT_CTRL, pressed: true });
                    key(code, &mut out);
                    out.push(RdpInput::Key { scancode: scancode::LEFT_CTRL, pressed: false });
                } else {
                    key(code, &mut out);
                }
            }
        }
    }
    if !text.is_empty() {
        out.push(RdpInput::Text(text));
    }
    out
}

/// `DOMAIN\user` into (domain, user); `user@domain` is passed on as it is.
fn split_user(user: &str) -> (Option<&str>, &str) {
    match user.split_once('\\') {
        Some((domain, name)) if !domain.is_empty() => (Some(domain), name),
        _ => (None, user),
    }
}

/// Desktop sizes must be even; most servers want a width divisible by 4.
fn desktop_size(width: u16, height: u16) -> (u16, u16) {
    ((width.clamp(320, 8192) / 4) * 4, (height.clamp(240, 8192) / 2) * 2)
}

/// This PC's name, as the remote side shows it (at most 15 characters).
fn client_name() -> String {
    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "playterm".into());
    name.chars().take(15).collect()
}

/// `SHA256:` and the unpadded base64 of the certificate's SHA-256, like SSH fingerprints.
fn fingerprint(der: &[u8]) -> String {
    format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(sha2::Sha256::digest(der)))
}

/// What `known_rdp_hosts` says about `host:port`.
#[derive(Debug, PartialEq, Eq)]
enum Known {
    Match,
    Unknown,
    /// The line holding a different certificate.
    Changed(usize),
}

fn check_known(file: &Path, target: &str, fp: &str) -> Known {
    let Ok(text) = std::fs::read_to_string(file) else { return Known::Unknown };
    for (i, line) in text.lines().enumerate() {
        if let Some((host, known)) = line.split_once(' ')
            && host == target
        {
            return if known.trim() == fp { Known::Match } else { Known::Changed(i + 1) };
        }
    }
    Known::Unknown
}

fn learn_known(file: &Path, target: &str, fp: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
    writeln!(f, "{target} {fp}")
}

/// Kerberos would need a KDC; without one CredSSP uses NTLM.
struct NoKdc;

impl ironrdp_tokio::NetworkClient for NoKdc {
    async fn send(&mut self, _: &NetworkRequest) -> ConnectorResult<Vec<u8>> {
        Err(ironrdp_connector::general_err!("Kerberos (KDC) is not supported; NTLM is used instead"))
    }
}

/// Clipboard backend messages, into the session loop.
#[derive(Debug)]
struct ClipboardProxy(mpsc::UnboundedSender<ClipboardMessage>);

impl ClipboardMessageProxy for ClipboardProxy {
    fn send_clipboard_message(&self, message: ClipboardMessage) {
        let _ = self.0.send(message);
    }
}

/// The Windows clipboard backend on a thread of its own. Its hidden window must keep handling
/// messages while the session thread is busy: when a local program pastes remote data, the window
/// waits for the session to fetch it.
#[cfg(windows)]
struct ClipboardThread {
    factory: Box<dyn CliprdrBackendFactory + Send>,
    thread_id: u32,
}

#[cfg(windows)]
impl ClipboardThread {
    fn start(proxy: ClipboardProxy) -> anyhow::Result<Self> {
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("playterm-rdp-clipboard".into()).spawn(move || {
            let clipboard = match ironrdp_cliprdr_native::WinClipboard::new(proxy) {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            // SAFETY: no preconditions.
            let thread_id = unsafe { GetCurrentThreadId() };
            if ready_tx.send(Ok((clipboard.backend_factory(), thread_id))).is_err() {
                return;
            }
            // SAFETY: a plain blocking message loop on this thread, until WM_QUIT (0) or an error (-1);
            // `msg` is written by GetMessageW before use.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            // The window goes away on the thread that made it.
            drop(clipboard);
        })?;
        let (factory, thread_id) = ready_rx
            .recv()
            .map_err(|_| anyhow!("clipboard thread ended"))?
            .map_err(|e| anyhow!(e))?;
        Ok(Self { factory, thread_id })
    }
}

#[cfg(windows)]
impl Drop for ClipboardThread {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
        // SAFETY: posts to the thread's message queue (created with its window); fails harmlessly if
        // the thread is already gone.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
    }
}

enum Outcome {
    Ended,
    BadPassword,
}

/// Starts the session on its own thread; ends with [`SessionEvent::Closed`].
pub(crate) fn spawn(params: ConnectParams, commands: mpsc::UnboundedReceiver<SessionCommand>, sink: EventSink) {
    let thread_sink = sink.clone();
    let started = std::thread::Builder::new().name("playterm-rdp".into()).spawn(move || {
        let sink = thread_sink;
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(run(params, commands, sink.clone())));
        if let Err(e) = &result {
            log::warn!("RDP session ended with error: {e:#}");
        }
        sink.send(SessionEvent::Closed(result.err().map(|e| format!("{e:#}"))));
    });
    if let Err(e) = started {
        sink.send(SessionEvent::Closed(Some(e.to_string())));
    }
}

async fn run(
    params: ConnectParams,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    sink: EventSink,
) -> anyhow::Result<()> {
    // The clipboard window lives as long as the session (across password retries).
    let (clip_tx, mut clip_rx) = mpsc::unbounded_channel();
    #[cfg(windows)]
    let clipboard = if params.rdp.clipboard {
        match ClipboardThread::start(ClipboardProxy(clip_tx)) {
            Ok(c) => Some(c),
            Err(e) => {
                sink.notice(tr!("클립보드 공유를 시작하지 못했습니다: {e}", "Clipboard sharing could not start: {e}"));
                None
            }
        }
    } else {
        None
    };
    #[cfg(windows)]
    let factory = clipboard.as_ref().map(|c| &*c.factory);
    #[cfg(not(windows))]
    let factory: Option<&(dyn CliprdrBackendFactory + Send)> = {
        drop(clip_tx);
        None
    };
    let has_clipboard = factory.is_some();

    let mut password = params.password.clone().map(Zeroizing::new);
    for _ in 0..MAX_PASSWORD_TRIES {
        let current = match password.take() {
            Some(p) => p,
            None => {
                let target = if params.username.is_empty() {
                    params.host.clone()
                } else {
                    format!("{}@{}", params.username, params.host)
                };
                let answers = sink
                    .prompt(
                        tr!("{target} 비밀번호", "Password for {target}"),
                        String::new(),
                        vec![PromptField { label: t("비밀번호", "Password").into(), echo: false }],
                        Some(&params.username),
                    )
                    .await
                    .ok_or_else(|| anyhow!(t("인증이 취소되었습니다", "Authentication cancelled")))?;
                Zeroizing::new(answers.into_iter().next().unwrap_or_default())
            }
        };
        let clip = if has_clipboard { Some(&mut clip_rx) } else { None };
        match session(&params, &current, factory, &mut commands, clip, &sink).await? {
            Outcome::Ended => return Ok(()),
            Outcome::BadPassword => sink.notice(t("인증 실패, 다시 시도하세요.", "Authentication failed, try again.")),
        }
    }
    bail!(t("인증 실패: 시도 횟수를 초과했습니다", "Authentication failed: too many attempts"))
}

/// Status codes in a CredSSP error that mean "wrong user name or password".
const BAD_CREDENTIALS: [u32; 5] = [
    0xC000_0064, // STATUS_NO_SUCH_USER
    0xC000_006A, // STATUS_WRONG_PASSWORD
    0xC000_006D, // STATUS_LOGON_FAILURE
    0x8009_030C, // SEC_E_LOGON_DENIED
    0xC007_030C, // SEC_E_LOGON_DENIED as some servers (IronRDP) report it
];

/// How a failed connection should be handled.
enum Failure {
    /// Wrong user name or password: ask again.
    BadPassword,
    /// Anything else, with a message for the user.
    Error(String),
}

fn classify(e: &ConnectorError) -> Failure {
    let err = match e.kind() {
        ConnectorErrorKind::Credssp(err) => err,
        ConnectorErrorKind::AccessDenied => return Failure::BadPassword,
        _ => return Failure::Error(e.to_string()),
    };
    let status = err.nstatus.map(|s| s.0);
    if status.is_some_and(|s| BAD_CREDENTIALS.contains(&s))
        || err.error_type == ironrdp_connector::sspi::ErrorKind::LogonDenied
    {
        return Failure::BadPassword;
    }
    let reason = match status {
        Some(0xC000_0072) => Some(t("계정이 사용 중지되어 있습니다", "The account is disabled")),
        Some(0xC000_0234) => Some(t("계정이 잠겨 있습니다", "The account is locked out")),
        Some(0xC000_0071) => Some(t("비밀번호가 만료되었습니다", "The password has expired")),
        Some(0xC000_0224) => Some(t("비밀번호를 바꿔야 합니다 (서버에서 먼저 변경하세요)", "The password must be changed (change it on the server first)")),
        Some(0xC000_006E) => Some(t("계정 제한으로 로그온할 수 없습니다", "Logon is not allowed by an account restriction")),
        Some(0xC000_015B) => Some(t("이 계정은 원격 로그온 권한이 없습니다", "The account is not allowed to log on remotely")),
        _ => None,
    };
    match reason {
        Some(r) => Failure::Error(r.into()),
        None => Failure::Error(tr!("NLA 인증 오류: {err}", "NLA authentication error: {err}")),
    }
}

fn connector_config(params: &ConnectParams, password: &str) -> anyhow::Result<ironrdp_connector::Config> {
    let (domain, user) = split_user(&params.username);
    let (width, height) = if params.rdp.fits_tab() { params.desktop } else { (params.rdp.width, params.rdp.height) };
    let (width, height) = desktop_size(width, height);
    let codecs = client_codecs_capabilities(&[]).map_err(|e| anyhow!(e))?;
    Ok(ironrdp_connector::Config {
        desktop_size: DesktopSize { width, height },
        monitor_layout: None,
        desktop_scale_factor: 0,
        desktop_physical_size: None,
        desktop_orientation: None,
        device_scale_factor: None,
        monitor_extended_layout: None,
        relative_mouse_input: false,
        enable_tls: true,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        credentials: Credentials::UsernamePassword { username: user.to_owned(), password: password.to_owned() },
        domain: domain.map(str::to_owned),
        client_build: 0,
        client_name: client_name(),
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: 0,
        connection_type: ConnectionType::Lan,
        ime_file_name: String::new(),
        bitmap: Some(BitmapConfig { lossy_compression: true, color_depth: 32, codecs }),
        dig_product_id: String::new(),
        client_dir: r"C:\Windows\System32\mstscax.dll".into(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::empty(),
        platform: MajorPlatformType::WINDOWS,
        hardware_id: None,
        request_data: None,
        autologon: false,
        enable_audio_playback: false,
        enable_audio_capture: false,
        performance_flags: PerformanceFlags::default(),
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        compression_type: Some(CompressionType::K64),
        enable_server_pointer: true,
        // The remote pointer is drawn into the screen; the local one is hidden over it.
        pointer_software_rendering: true,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
    })
}

trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite> AsyncReadWrite for T {}
type Upgraded = TokioFramed<Box<dyn AsyncReadWrite + Unpin + Send + Sync>>;

/// TCP, TLS (with the certificate check) and the connection sequence up to the active session.
async fn connect(
    params: &ConnectParams,
    password: &str,
    clipboard: Option<&(dyn CliprdrBackendFactory + Send)>,
    sink: &EventSink,
) -> anyhow::Result<Result<(ironrdp_connector::ConnectionResult, Upgraded), ConnectorError>> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((params.host.as_str(), params.port)))
        .await
        .context(t("연결 시간 초과", "Connection timed out"))?
        .with_context(|| tr!("{}:{} 에 연결할 수 없습니다", "Cannot connect to {}:{}", params.host, params.port))?;
    stream.set_nodelay(true)?;
    let client_addr = stream.local_addr()?;
    let mut framed = TokioFramed::new(stream);

    let drdynvc = DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let mut connector = ClientConnector::new(connector_config(params, password)?, client_addr).with_static_channel(drdynvc);
    if let Some(factory) = clipboard {
        connector.attach_static_channel(ironrdp_cliprdr::Cliprdr::new(factory.build_cliprdr_backend()));
    }

    let should_upgrade = match ironrdp_tokio::connect_begin(&mut framed, &mut connector).await {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    let (stream, leftover) = framed.into_inner();
    // Self-signed certificates are the norm for RDP: accepted here, then pinned below.
    let (tls, cert) = ironrdp_tls::upgrade_with_certificate_validation(
        stream,
        &params.host,
        ironrdp_tls::CertificateValidation::DangerouslyAcceptInvalidCertificate,
    )
    .await
    .context(t("TLS 연결 실패", "TLS connection failed"))?;
    check_certificate(params, &cert, sink).await?;

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let erased: Box<dyn AsyncReadWrite + Unpin + Send + Sync> = Box::new(tls);
    let mut framed = TokioFramed::new_with_leftover(erased, leftover);
    let server_public_key = ironrdp_tls::extract_tls_server_public_key(&cert)
        .ok_or_else(|| anyhow!("unable to extract the TLS server public key"))?
        .to_owned();
    let outcome = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoKdc,
        ServerName::new(params.host.clone()),
        server_public_key,
        None,
    )
    .await;
    Ok(outcome.and_then(|o| o.into_connected()).map(|result| (result, framed)))
}

/// Trust on first use, like SSH host keys: a new certificate is confirmed by the user and
/// remembered in `known_rdp_hosts`; a changed one stops the connection.
async fn check_certificate(params: &ConnectParams, cert: &x509_cert::Certificate, sink: &EventSink) -> anyhow::Result<()> {
    use x509_cert::der::Encode as _;
    let der = cert.to_der().context("certificate")?;
    let fp = fingerprint(&der);
    let file = params.known_hosts.with_file_name("known_rdp_hosts");
    let target = format!("{}:{}", params.host, params.port);
    match check_known(&file, &target, &fp) {
        Known::Match => Ok(()),
        Known::Changed(line) => bail!(tr!(
            "경고: 서버 인증서가 변경되었습니다! 중간자 공격일 수 있습니다. \
             정상적인 변경이라면 {} 의 {line}번째 줄을 삭제하세요.",
            "WARNING: the server certificate has changed! This could be a man-in-the-middle attack. \
             If the change is legitimate, delete line {line} of {}.",
            file.display()
        )),
        Known::Unknown => {
            let accepted = sink.confirm_certificate(&params.host, params.port, fp.clone()).await;
            if !accepted {
                bail!(t("서버 인증서를 신뢰하지 않아 연결하지 않았습니다", "The server certificate was not trusted"));
            }
            if let Err(e) = learn_known(&file, &target, &fp) {
                sink.notice(tr!("known_rdp_hosts 저장 실패: {e}", "Could not save known_rdp_hosts: {e}"));
            }
            Ok(())
        }
    }
}

async fn session(
    params: &ConnectParams,
    password: &str,
    clipboard: Option<&(dyn CliprdrBackendFactory + Send)>,
    commands: &mut mpsc::UnboundedReceiver<SessionCommand>,
    mut clip_rx: Option<&mut mpsc::UnboundedReceiver<ClipboardMessage>>,
    sink: &EventSink,
) -> anyhow::Result<Outcome> {
    let (result, framed) = match connect(params, password, clipboard, sink).await? {
        Ok(connected) => connected,
        Err(e) => match classify(&e) {
            Failure::BadPassword => return Ok(Outcome::BadPassword),
            Failure::Error(message) => bail!(message),
        },
    };
    sink.send(SessionEvent::Connected);

    let (mut reader, mut writer) = ironrdp_tokio::split_tokio_framed(framed);
    let screen = sink.screen();
    lock(&screen).reset(result.desktop_size.width, result.desktop_size.height);
    let activation_factory = result.activation_factory;
    let mut stage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();

    let mut frames = tokio::time::interval(FRAME_INTERVAL);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut housekeeping = tokio::time::interval(Duration::from_secs(5));
    // The screen changed since the UI was last woken.
    let mut changed = true;
    let mut closing: Option<tokio::time::Instant> = None;
    let mut ended_by_server = true;
    let mut commands_open = true;

    let reason = 'session: loop {
        let outputs = tokio::select! {
            pdu = reader.read_pdu() => {
                let (action, payload) = match pdu {
                    Ok(p) => p,
                    // The server closed the connection after our goodbye.
                    Err(_) if closing.is_some() => break 'session None,
                    Err(e) => return Err(anyhow!(e).context(t("수신 오류", "Receive error"))),
                };
                stage.process(&mut lock(&screen).image, action, &payload, None)?
            }
            _ = frames.tick(), if changed => {
                changed = false;
                sink.wake();
                Vec::new()
            }
            () = async { tokio::time::sleep_until(closing.unwrap_or_else(tokio::time::Instant::now)).await },
                if closing.is_some() => break 'session None,
            command = commands.recv(), if commands_open => match command {
                Some(SessionCommand::Rdp(inputs)) => input(&mut stage, &screen, &inputs)?,
                Some(SessionCommand::Input(bytes)) => input(&mut stage, &screen, &bytes_to_inputs(&bytes))?,
                Some(SessionCommand::Resize { .. }) => Vec::new(),
                Some(SessionCommand::SendBreak) => {
                    sink.notice(t("RDP 세션에서는 BREAK 신호를 지원하지 않습니다", "RDP sessions do not support BREAK"));
                    Vec::new()
                }
                Some(SessionCommand::OpenSftp) => {
                    sink.send(SessionEvent::Sftp(Err(t(
                        "RDP 세션에서는 SFTP를 쓸 수 없습니다",
                        "SFTP is not available in RDP sessions",
                    )
                    .into())));
                    Vec::new()
                }
                // The tab closed or dropped the session (reconnect): no more commands come.
                Some(SessionCommand::Close) | None => {
                    commands_open = false;
                    ended_by_server = false;
                    // A polite goodbye, then at most two seconds for the server to hang up.
                    closing = Some(tokio::time::Instant::now() + Duration::from_secs(2));
                    stage.graceful_shutdown()?
                }
            },
            message = async {
                match clip_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => match message {
                Some(message) => clipboard_message(&mut stage, message)?,
                // The clipboard backend is gone: carry on without it.
                None => {
                    clip_rx = None;
                    Vec::new()
                }
            },
            _ = housekeeping.tick() => match stage.get_svc_processor_mut::<CliprdrClient>() {
                Some(cliprdr) => {
                    let messages = cliprdr.drive_timeouts()?;
                    let frame = stage.process_svc_processor_messages(messages)?;
                    if frame.is_empty() { Vec::new() } else { vec![ActiveStageOutput::ResponseFrame(frame)] }
                }
                None => Vec::new(),
            },
        };

        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => {
                    writer.write_all(&frame).await.context(t("전송 오류", "Send error"))?;
                }
                ActiveStageOutput::GraphicsUpdate(r) => {
                    // Inclusive in the PDU; exclusive here.
                    lock(&screen).changed([r.left, r.top, r.right.saturating_add(1), r.bottom.saturating_add(1)]);
                    changed = true;
                }
                ActiveStageOutput::Terminate(reason) => break 'session Some(reason),
                ActiveStageOutput::DeactivateAll => {
                    // Deactivation-Reactivation Sequence (e.g. after a resize): a fresh activation.
                    let mut activation = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = ironrdp_tokio::single_sequence_step_read(&mut reader, &mut activation, &mut buf)
                            .await
                            .map_err(|e| anyhow!("{e}"))?;
                        if written.size().is_some() {
                            writer.write_all(buf.filled()).await?;
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                            ..
                        } = activation.connection_activation_state()
                        {
                            lock(&screen).reset(desktop_size.width, desktop_size.height);
                            changed = true;
                            stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id: activation.io_channel_id(),
                                    user_channel_id: activation.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                }
                                .build(),
                            );
                            stage.set_share_id(share_id);
                            stage.set_enable_server_pointer(enable_server_pointer);
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    };
    if let Some(reason) = reason.filter(|_| ended_by_server) {
        sink.notice(tr!("원격 데스크톱 종료: {}", "Remote desktop ended: {}", reason.description()));
    }
    Ok(Outcome::Ended)
}

/// Keyboard and mouse into session output (frames to send).
fn input(stage: &mut ActiveStage, screen: &Mutex<SharedScreen>, inputs: &[RdpInput]) -> anyhow::Result<Vec<ActiveStageOutput>> {
    let mut outputs = Vec::new();
    let mut events: SmallVec<[FastPathInputEvent; 2]> = SmallVec::new();
    for i in inputs {
        if let RdpInput::Resize { width, height } = i {
            let (width, height) = desktop_size(*width, *height);
            // Servers without the display control channel keep their size.
            if let Some(frame) = stage.encode_resize(u32::from(width), u32::from(height), Some(100), None) {
                outputs.push(ActiveStageOutput::ResponseFrame(frame?));
            }
        } else {
            fast_path(i, &mut events);
        }
    }
    if !events.is_empty() {
        outputs.extend(stage.process_fastpath_input(&mut lock(screen).image, &events)?);
    }
    Ok(outputs)
}

/// The local clipboard changed or the remote side asked for data.
fn clipboard_message(stage: &mut ActiveStage, message: ClipboardMessage) -> anyhow::Result<Vec<ActiveStageOutput>> {
    let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else { return Ok(Vec::new()) };
    let messages = match message {
        ClipboardMessage::SendInitiateCopy(formats) => cliprdr.initiate_copy(&formats)?,
        ClipboardMessage::SendInitiateFileCopy(files) => cliprdr.initiate_file_copy(files)?,
        ClipboardMessage::SendFormatData(response) => cliprdr.submit_format_data(response)?,
        ClipboardMessage::SendInitiatePaste(format) => cliprdr.initiate_paste(format)?,
        ClipboardMessage::SendFileContentsRequest(request) => cliprdr.request_file_contents(request)?,
        ClipboardMessage::SendFileContentsResponse(response) => cliprdr.submit_file_contents(response)?,
        ClipboardMessage::Error(e) => {
            log::warn!("clipboard: {e}");
            return Ok(Vec::new());
        }
    };
    let frame = stage.process_svc_processor_messages(messages)?;
    Ok(vec![ActiveStageOutput::ResponseFrame(frame)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(inputs: &[RdpInput]) -> Vec<FastPathInputEvent> {
        let mut out = SmallVec::new();
        for i in inputs {
            fast_path(i, &mut out);
        }
        out.into_vec()
    }

    #[test]
    fn keys_and_text() {
        let e = events(&[
            RdpInput::Key { scancode: 0xE048, pressed: true },
            RdpInput::Key { scancode: 0x1E, pressed: false },
            RdpInput::Text("가😀".into()),
        ]);
        assert_eq!(e[0], FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED, 0x48));
        assert_eq!(e[1], FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE, 0x1E));
        // One BMP character and a surrogate pair: three code units, each pressed and released.
        assert_eq!(e.len(), 2 + 6);
        assert_eq!(e[2], FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::empty(), 0xAC00));
        assert_eq!(e[3], FastPathInputEvent::UnicodeKeyboardEvent(KeyboardFlags::RELEASE, 0xAC00));
    }

    #[test]
    fn mouse() {
        let e = events(&[
            RdpInput::MouseButton { button: RdpMouseButton::Right, pressed: true, x: 5, y: 6 },
            RdpInput::Wheel { delta: -400, horizontal: false, x: 1, y: 2 },
        ]);
        let FastPathInputEvent::MouseEvent(button) = &e[0] else { panic!() };
        assert_eq!(button.flags, PointerFlags::RIGHT_BUTTON | PointerFlags::DOWN);
        assert_eq!((button.x_position, button.y_position), (5, 6));
        let FastPathInputEvent::MouseEvent(wheel) = &e[1] else { panic!() };
        assert_eq!((wheel.flags, wheel.number_of_wheel_rotation_units), (PointerFlags::VERTICAL_WHEEL, -255));
    }

    #[test]
    fn typed_bytes() {
        let k = |scancode, pressed| RdpInput::Key { scancode, pressed };
        assert_eq!(
            bytes_to_inputs(b"ls -l\r\n"),
            vec![RdpInput::Text("ls -l".into()), k(scancode::ENTER, true), k(scancode::ENTER, false)]
        );
        assert_eq!(
            bytes_to_inputs(b"\x03"),
            vec![k(scancode::LEFT_CTRL, true), k(scancode::C, true), k(scancode::C, false), k(scancode::LEFT_CTRL, false)]
        );
        assert_eq!(bytes_to_inputs("한글\x01".as_bytes()), vec![RdpInput::Text("한글".into())]);
    }

    #[test]
    fn users_and_sizes() {
        assert_eq!(split_user(r"CORP\kim"), (Some("CORP"), "kim"));
        assert_eq!(split_user("kim@corp.local"), (None, "kim@corp.local"));
        assert_eq!(split_user("kim"), (None, "kim"));
        assert_eq!(desktop_size(1023, 767), (1020, 766));
        assert_eq!(desktop_size(10, 10), (320, 240));
    }

    #[test]
    fn shared_screen_copies_changed_parts() {
        let mut screen = SharedScreen::default();
        assert!(screen.take().is_none(), "no desktop yet");
        // A new size: the whole picture.
        screen.reset(4, 3);
        let f = screen.take().unwrap();
        assert_eq!((f.width, f.height, f.rect, f.rgba.len()), (4, 3, [0, 0, 4, 3], 48));
        assert!(screen.take().is_none(), "nothing new");
        // Two changes before the UI looks: their union; off-screen parts are cut.
        screen.changed([0, 0, 1, 1]);
        screen.changed([2, 1, 9, 2]);
        screen.changed([5, 5, 6, 6]);
        let f = screen.take().unwrap();
        assert_eq!(f.rect, [0, 0, 4, 2]);
        assert_eq!(f.rgba.len(), 4 * 2 * 4);

        let image: Vec<u8> = (0..4 * 3 * 4).map(|i| i as u8).collect(); // 4 x 3 pixels
        let part = copy_rect(&image, 16, [1, 1, 3, 3]);
        assert_eq!(part.len(), 2 * 2 * 4);
        // Row 1 of the region starts at pixel (1, 2) of the image.
        assert_eq!(&part[8..12], &image[36..40]);
    }

    #[test]
    fn known_certificates() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known_rdp_hosts");
        let fp = fingerprint(b"cert one");
        assert!(fp.starts_with("SHA256:"));
        assert_eq!(check_known(&file, "srv:3389", &fp), Known::Unknown);
        learn_known(&file, "srv:3389", &fp).unwrap();
        learn_known(&file, "other:3389", &fingerprint(b"x")).unwrap();
        assert_eq!(check_known(&file, "srv:3389", &fp), Known::Match);
        assert_eq!(check_known(&file, "srv:3389", &fingerprint(b"cert two")), Known::Changed(1));
        assert_eq!(check_known(&file, "srv:3390", &fp), Known::Unknown, "the port is part of the name");
    }
}
