//! Test RDP server for playterm: NLA (CredSSP) login, a test pattern that follows the mouse and
//! the desktop size the client asks for, and every input event logged to stdout.
//!
//! `cargo run --release -- [--port 3389] [--user tester] [--password secret]`

use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use ironrdp_server::tokio::sync::Notify;
use ironrdp_server::{
    BitmapUpdate, Credentials, DesktopSize, DisplayUpdate, KeyboardEvent, MouseEvent, PixelFormat, RdpServer,
    RdpServerDisplay, RdpServerDisplayUpdates, RdpServerInputHandler, TlsIdentityCtx,
};

#[derive(Default)]
struct State {
    mouse: (u16, u16),
    keys: u32,
    size: (u16, u16),
    /// A size the client asked for, to announce before the next picture.
    resize: Option<(u16, u16)>,
}

struct Shared {
    state: Mutex<State>,
    changed: Notify,
}

struct Input(Arc<Shared>);

impl RdpServerInputHandler for Input {
    fn keyboard(&mut self, event: KeyboardEvent) {
        match &event {
            KeyboardEvent::UnicodePressed(u) => {
                println!("key: unicode {:?} ({u:#06x})", char::from_u32(u32::from(*u)).unwrap_or('?'));
            }
            KeyboardEvent::Pressed { code, extended } => {
                println!("key: scancode {}{code:#04x} down", if *extended { "E0 " } else { "" });
            }
            KeyboardEvent::Released { code, extended } => {
                println!("key: scancode {}{code:#04x} up", if *extended { "E0 " } else { "" });
            }
            _ => {}
        }
        if matches!(event, KeyboardEvent::UnicodePressed(_) | KeyboardEvent::Pressed { .. }) {
            self.0.state.lock().unwrap().keys += 1;
        }
        self.0.changed.notify_one();
    }

    fn mouse(&mut self, event: MouseEvent) {
        match event {
            MouseEvent::Move { x, y } => self.0.state.lock().unwrap().mouse = (x, y),
            other => println!("mouse: {other:?}"),
        }
        self.0.changed.notify_one();
    }
}

/// The test pattern: a gradient, a white border, a box whose color changes with each key press
/// and a yellow crosshair at the pointer.
fn render(st: &State) -> BitmapUpdate {
    let (w, h) = (st.size.0.max(1), st.size.1.max(1));
    let mut data = vec![0u8; usize::from(w) * usize::from(h) * 4];
    let hue = [(220, 60, 60), (60, 170, 90), (60, 120, 220), (230, 130, 40)][(st.keys % 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = (usize::from(y) * usize::from(w) + usize::from(x)) * 4;
            let mut px = ((u32::from(x) * 255 / u32::from(w)) as u8, (u32::from(y) * 255 / u32::from(h)) as u8, 96);
            if x > 40 && x < 240 && y > 40 && y < 160 {
                px = hue;
            }
            if x < 4 || y < 4 || x >= w - 4 || y >= h - 4 {
                px = (255, 255, 255);
            }
            let (mx, my) = st.mouse;
            if (x == mx && y.abs_diff(my) < 20) || (y == my && x.abs_diff(mx) < 20) {
                px = (255, 255, 0);
            }
            // ARGB32: A, R, G, B in memory.
            data[i..i + 4].copy_from_slice(&[255, px.0, px.1, px.2]);
        }
    }
    BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(w).unwrap(),
        height: NonZeroU16::new(h).unwrap(),
        format: PixelFormat::ARgb32,
        data: data.into(),
        stride: NonZeroUsize::new(usize::from(w) * 4).unwrap(),
    }
}

struct Updates {
    shared: Arc<Shared>,
    first: bool,
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> anyhow::Result<Option<DisplayUpdate>> {
        if !std::mem::take(&mut self.first) {
            self.shared.changed.notified().await;
        }
        let mut st = self.shared.state.lock().unwrap();
        if let Some((width, height)) = st.resize.take() {
            println!("resizing the desktop to {width}x{height}");
            st.size = (width, height);
            // The picture at the new size follows.
            self.shared.changed.notify_one();
            return Ok(Some(DisplayUpdate::Resize(DesktopSize { width, height })));
        }
        Ok(Some(DisplayUpdate::Bitmap(render(&st))))
    }
}

struct Display(Arc<Shared>);

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        let (width, height) = self.0.state.lock().unwrap().size;
        DesktopSize { width, height }
    }

    async fn request_initial_size(&mut self, client_size: DesktopSize) -> DesktopSize {
        println!("client asked for {}x{}", client_size.width, client_size.height);
        self.0.state.lock().unwrap().size = (client_size.width, client_size.height);
        client_size
    }

    async fn updates(&mut self) -> anyhow::Result<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(Updates { shared: self.0.clone(), first: true }))
    }

    fn request_layout(&mut self, layout: ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout) {
        if let Some(monitor) = layout.monitors().first() {
            let (w, h) = monitor.dimensions();
            println!("client asked to resize to {w}x{h}");
            self.0.state.lock().unwrap().resize = Some((w as u16, h as u16));
            self.0.changed.notify_one();
        }
    }
}

fn arg(name: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned()).unwrap_or_else(|| default.into())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let port: u16 = arg("--port", "3389").parse().context("--port")?;
    let user = arg("--user", "tester");
    let password = arg("--password", "secret");

    // A fixed self-signed certificate in the temp folder, so known_rdp_hosts keeps matching.
    let dir = std::env::temp_dir().join("playterm-dev-rdpd");
    std::fs::create_dir_all(&dir)?;
    let (cert_path, key_path) = (dir.join("cert.pem"), dir.join("key.pem"));
    if !cert_path.exists() || !key_path.exists() {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
        std::fs::write(&cert_path, cert.cert.pem())?;
        std::fs::write(&key_path, cert.signing_key.serialize_pem())?;
    }
    let identity = TlsIdentityCtx::init_from_paths(&cert_path, &key_path)?;

    let shared = Arc::new(Shared {
        state: Mutex::new(State { size: (1024, 768), ..Default::default() }),
        changed: Notify::new(),
    });
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], port))
        .with_hybrid(identity.make_acceptor()?, identity.pub_key.clone())
        .with_input_handler(Input(shared.clone()))
        .with_display_handler(Display(shared.clone()))
        .with_honor_client_desktop_size(true)
        .build();
    server.set_credentials(Some(Credentials { username: user.clone(), password: password.clone(), domain: None }));
    println!("dev-rdpd listening on 127.0.0.1:{port} ({user} / {password}, NLA)");
    server.run().await
}
