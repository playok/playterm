//! Serial (COM port) sessions.

use std::time::Duration;

use anyhow::Context as _;
use playterm_i18n::{t, tr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::keepalive::{IdleTimer, sleep_until};
use crate::session::{ConnectParams, EventSink, SessionCommand, SessionEvent};

/// How long the line is held in the break state (devices expect 250–500 ms).
const BREAK_DURATION: Duration = Duration::from_millis(300);

pub use playterm_types::{SerialFlow, SerialParity, SerialSettings};

fn serial_builder(settings: &SerialSettings) -> anyhow::Result<tokio_serial::SerialPortBuilder> {
    use tokio_serial::{DataBits, FlowControl, Parity, StopBits};
    let data_bits = match settings.data_bits {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        8 => DataBits::Eight,
        n => anyhow::bail!(tr!("serial.data_bits_must_be_5_to_8", n = n)),
    };
    let stop_bits = match settings.stop_bits {
        1 => StopBits::One,
        2 => StopBits::Two,
        n => anyhow::bail!(tr!("serial.stop_bits_must_be_1_or_2", n = n)),
    };
    let parity = match settings.parity {
        SerialParity::None => Parity::None,
        SerialParity::Odd => Parity::Odd,
        SerialParity::Even => Parity::Even,
    };
    let flow = match settings.flow {
        SerialFlow::None => FlowControl::None,
        SerialFlow::Hardware => FlowControl::Hardware,
        SerialFlow::Software => FlowControl::Software,
    };
    Ok(tokio_serial::new(&settings.port, settings.baud).data_bits(data_bits).stop_bits(stop_bits).parity(parity).flow_control(flow))
}

/// A serial port present on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialPortEntry {
    pub name: String,
    /// Human readable device description, if known.
    pub description: String,
}

/// Lists the COM ports currently present.
pub fn list_ports() -> Vec<SerialPortEntry> {
    let mut ports: Vec<SerialPortEntry> = tokio_serial::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let description = match p.port_type {
                tokio_serial::SerialPortType::UsbPort(usb) => usb.product.or(usb.manufacturer).unwrap_or_else(|| "USB".into()),
                tokio_serial::SerialPortType::BluetoothPort => "Bluetooth".into(),
                tokio_serial::SerialPortType::PciPort => "PCI".into(),
                tokio_serial::SerialPortType::Unknown => String::new(),
            };
            SerialPortEntry { name: p.port_name, description }
        })
        .collect();
    // COM2 before COM10.
    ports.sort_by_key(|p| {
        let digits: String = p.name.chars().filter(char::is_ascii_digit).collect();
        (digits.parse::<u32>().unwrap_or(u32::MAX), p.name.clone())
    });
    ports
}

/// A line that can signal BREAK (used by consoles for ROMMON / password recovery).
pub(crate) trait SendBreak {
    fn send_break(&mut self) -> impl Future<Output = std::io::Result<()>> + Send;
}

impl SendBreak for tokio_serial::SerialStream {
    async fn send_break(&mut self) -> std::io::Result<()> {
        use tokio_serial::SerialPort as _;
        self.set_break().map_err(std::io::Error::other)?;
        tokio::time::sleep(BREAK_DURATION).await;
        self.clear_break().map_err(std::io::Error::other)
    }
}

pub(crate) async fn run(params: ConnectParams, commands: mpsc::UnboundedReceiver<SessionCommand>, sink: EventSink) -> anyhow::Result<()> {
    let settings = &params.serial;
    if settings.port.is_empty() {
        anyhow::bail!(t!("serial.choose_a_serial_port"));
    }
    let stream = {
        use tokio_serial::SerialPortBuilderExt as _;
        serial_builder(settings)?.open_native_async().with_context(|| tr!("common.cannot_open", arg0 = settings.port))?
    };
    sink.send(SessionEvent::Connected);
    let idle = IdleTimer::new(&params.keepalive, false, &sink);
    serve(stream, commands, sink, idle).await
}

/// Pumps bytes between the line and the session until closed.
pub(crate) async fn serve<S>(
    mut stream: S,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    sink: EventSink,
    mut idle: IdleTimer,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + SendBreak + Unpin,
{
    let mut buf = vec![0u8; 4096];
    loop {
        tokio::select! {
            n = stream.read(&mut buf) => {
                let n = n.context(t!("serial.receive_error_the_device_may_have_been_disconnected"))?;
                if n == 0 {
                    // Some drivers report an empty read on timeout; the line is still open.
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
                sink.data(buf[..n].to_vec()).await;
            }
            cmd = commands.recv() => match cmd {
                Some(SessionCommand::Input(data)) => {
                    stream.write_all(&data).await?;
                    idle.touch();
                }
                Some(SessionCommand::SendBreak) => {
                    stream.send_break().await.context(t!("serial.cannot_send_break"))?;
                    sink.notice(t!("common.sent_break"));
                }
                Some(SessionCommand::Resize { .. } | SessionCommand::Rdp(_)) => {}
                Some(SessionCommand::ShellDir) => sink.send(SessionEvent::ShellDir(None)),
                Some(SessionCommand::OpenSftp) => {
                    sink.send(SessionEvent::Sftp(Err(t!("serial.sftp_is_not_available_in_serial_sessions")
                    .into())));
                }
                Some(SessionCommand::Close) | None => return Ok(()),
            },
            _ = sleep_until(idle.deadline()) => {
                if let Some(text) = &idle.text {
                    stream.write_all(text).await?;
                }
                idle.touch();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::DuplexStream;

    use super::*;
    use crate::keepalive::{KeepAlive, KeepAliveMode};
    use crate::session::SessionHandle;

    /// In-memory "serial line" that counts BREAK signals.
    struct FakeLine {
        io: DuplexStream,
        breaks: Arc<AtomicUsize>,
    }

    impl AsyncRead for FakeLine {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.io).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for FakeLine {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.io).poll_write(cx, buf)
        }
        fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.io).poll_flush(cx)
        }
        fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
        }
    }

    impl SendBreak for FakeLine {
        async fn send_break(&mut self) -> std::io::Result<()> {
            self.breaks.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    async fn next_event(h: &SessionHandle) -> SessionEvent {
        for _ in 0..500 {
            if let Some(e) = h.try_recv() {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out");
    }

    #[tokio::test(start_paused = true)]
    async fn types_keepalive_text_when_idle() {
        let (line, mut device) = tokio::io::duplex(1024);
        let (handle, commands, sink) = SessionHandle::new(Arc::new(|| {}));
        let keepalive = KeepAlive { mode: KeepAliveMode::Text, interval: 30, text: "\\x20\\b".into() };
        let idle = IdleTimer::new(&keepalive, false, &sink);
        let task = tokio::spawn(serve(FakeLine { io: line, breaks: Arc::default() }, commands, sink, idle));

        let mut buf = [0u8; 2];
        device.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b" \x08");
        let start = tokio::time::Instant::now();
        handle.send(b"x".to_vec());
        let mut buf = [0u8; 3];
        device.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"x \x08", "the next one comes a full period after the input");
        assert!(start.elapsed() >= Duration::from_secs(30));

        handle.close();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn pumps_data_and_breaks() {
        let (line, mut device) = tokio::io::duplex(1024);
        let breaks = Arc::new(AtomicUsize::new(0));
        let (handle, commands, sink) = SessionHandle::new(Arc::new(|| {}));
        let idle = IdleTimer::new(&KeepAlive::default(), false, &sink);
        let task = tokio::spawn(serve(FakeLine { io: line, breaks: breaks.clone() }, commands, sink, idle));

        device.write_all(b"Router>").await.unwrap();
        assert!(matches!(next_event(&handle).await, SessionEvent::Data(d) if d == b"Router>"));

        handle.send(b"enable\r".to_vec());
        let mut buf = [0u8; 7];
        device.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"enable\r");

        handle.send_break();
        assert!(matches!(next_event(&handle).await, SessionEvent::Notice(_)));
        assert_eq!(breaks.load(Ordering::Relaxed), 1);

        handle.close();
        task.await.unwrap().unwrap();
    }

    #[test]
    fn settings_validation_and_summary() {
        let s = SerialSettings { port: "COM3".into(), baud: 115200, ..Default::default() };
        assert_eq!(s.line_summary(), "115200 8N1");
        assert!(serial_builder(&s).is_ok());
        assert!(serial_builder(&SerialSettings { data_bits: 9, ..s.clone() }).is_err());
        assert!(serial_builder(&SerialSettings { stop_bits: 3, ..s }).is_err());
    }
}
