//! Serial (COM port) sessions.

use std::fmt;
use std::time::Duration;

use anyhow::Context as _;
use playterm_i18n::{t, tr};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::session::{ConnectParams, EventSink, SessionCommand, SessionEvent};

/// How long the line is held in the break state (devices expect 250–500 ms).
const BREAK_DURATION: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SerialParity {
    #[default]
    None,
    Odd,
    Even,
}

impl SerialParity {
    pub const ALL: [SerialParity; 3] = [SerialParity::None, SerialParity::Odd, SerialParity::Even];

    fn letter(self) -> char {
        match self {
            SerialParity::None => 'N',
            SerialParity::Odd => 'O',
            SerialParity::Even => 'E',
        }
    }
}

impl fmt::Display for SerialParity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SerialParity::None => t("없음", "None"),
            SerialParity::Odd => t("홀수", "Odd"),
            SerialParity::Even => t("짝수", "Even"),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SerialFlow {
    #[default]
    None,
    /// RTS/CTS
    Hardware,
    /// XON/XOFF
    Software,
}

impl SerialFlow {
    pub const ALL: [SerialFlow; 3] = [SerialFlow::None, SerialFlow::Hardware, SerialFlow::Software];
}

impl fmt::Display for SerialFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SerialFlow::None => t("없음", "None"),
            SerialFlow::Hardware => t("하드웨어 (RTS/CTS)", "Hardware (RTS/CTS)"),
            SerialFlow::Software => t("소프트웨어 (XON/XOFF)", "Software (XON/XOFF)"),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SerialSettings {
    /// Port name, e.g. `COM3`.
    pub port: String,
    pub baud: u32,
    /// 5–8
    pub data_bits: u8,
    pub parity: SerialParity,
    /// 1 or 2
    pub stop_bits: u8,
    pub flow: SerialFlow,
}

impl Default for SerialSettings {
    /// 9600 8N1, the usual console setting of network equipment.
    fn default() -> Self {
        Self { port: String::new(), baud: 9600, data_bits: 8, parity: SerialParity::None, stop_bits: 1, flow: SerialFlow::None }
    }
}

impl SerialSettings {
    pub const COMMON_BAUD_RATES: [u32; 9] = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 921600];

    /// `9600 8N1` style summary.
    pub fn line_summary(&self) -> String {
        format!("{} {}{}{}", self.baud, self.data_bits, self.parity.letter(), self.stop_bits)
    }

    fn builder(&self) -> anyhow::Result<tokio_serial::SerialPortBuilder> {
        use tokio_serial::{DataBits, FlowControl, Parity, StopBits};
        let data_bits = match self.data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            8 => DataBits::Eight,
            n => anyhow::bail!(tr!("데이터 비트는 5~8 이어야 합니다 ({n})", "Data bits must be 5 to 8 ({n})")),
        };
        let stop_bits = match self.stop_bits {
            1 => StopBits::One,
            2 => StopBits::Two,
            n => anyhow::bail!(tr!("정지 비트는 1 또는 2 여야 합니다 ({n})", "Stop bits must be 1 or 2 ({n})")),
        };
        let parity = match self.parity {
            SerialParity::None => Parity::None,
            SerialParity::Odd => Parity::Odd,
            SerialParity::Even => Parity::Even,
        };
        let flow = match self.flow {
            SerialFlow::None => FlowControl::None,
            SerialFlow::Hardware => FlowControl::Hardware,
            SerialFlow::Software => FlowControl::Software,
        };
        Ok(tokio_serial::new(&self.port, self.baud)
            .data_bits(data_bits)
            .stop_bits(stop_bits)
            .parity(parity)
            .flow_control(flow))
    }
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
                tokio_serial::SerialPortType::UsbPort(usb) => {
                    usb.product.or(usb.manufacturer).unwrap_or_else(|| "USB".into())
                }
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

pub(crate) async fn run(
    params: ConnectParams,
    commands: mpsc::UnboundedReceiver<SessionCommand>,
    sink: EventSink,
) -> anyhow::Result<()> {
    let settings = &params.serial;
    if settings.port.is_empty() {
        anyhow::bail!(t("시리얼 포트를 지정하세요", "Choose a serial port"));
    }
    let stream = {
        use tokio_serial::SerialPortBuilderExt as _;
        settings
            .builder()?
            .open_native_async()
            .with_context(|| tr!("{} 을(를) 열 수 없습니다", "Cannot open {}", settings.port))?
    };
    sink.send(SessionEvent::Connected);
    serve(stream, commands, sink).await
}

/// Pumps bytes between the line and the session until closed.
pub(crate) async fn serve<S>(
    mut stream: S,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    sink: EventSink,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + SendBreak + Unpin,
{
    let mut buf = vec![0u8; 4096];
    loop {
        tokio::select! {
            n = stream.read(&mut buf) => {
                let n = n.context(t(
                    "수신 오류 (장치가 분리되었을 수 있습니다)",
                    "Receive error (the device may have been disconnected)",
                ))?;
                if n == 0 {
                    // Some drivers report an empty read on timeout; the line is still open.
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
                sink.send(SessionEvent::Data(buf[..n].to_vec()));
            }
            cmd = commands.recv() => match cmd {
                Some(SessionCommand::Input(data)) => stream.write_all(&data).await?,
                Some(SessionCommand::SendBreak) => {
                    stream.send_break().await.context(t("BREAK 신호를 보낼 수 없습니다", "Cannot send BREAK"))?;
                    sink.notice(t("BREAK 신호를 보냈습니다", "Sent BREAK"));
                }
                Some(SessionCommand::Resize { .. }) => {}
                Some(SessionCommand::OpenSftp) => {
                    sink.send(SessionEvent::Sftp(Err(t(
                        "시리얼 세션에서는 SFTP를 쓸 수 없습니다",
                        "SFTP is not available in serial sessions",
                    )
                    .into())));
                }
                Some(SessionCommand::Close) | None => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::DuplexStream;

    use super::*;
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
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
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

    #[tokio::test]
    async fn pumps_data_and_breaks() {
        let (line, mut device) = tokio::io::duplex(1024);
        let breaks = Arc::new(AtomicUsize::new(0));
        let (handle, commands, sink) = SessionHandle::new(Arc::new(|| {}));
        let task = tokio::spawn(serve(FakeLine { io: line, breaks: breaks.clone() }, commands, sink));

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
        assert!(s.builder().is_ok());
        assert!(SerialSettings { data_bits: 9, ..s.clone() }.builder().is_err());
        assert!(SerialSettings { stop_bits: 3, ..s }.builder().is_err());
    }
}
