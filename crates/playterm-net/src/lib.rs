//! Network transports for playterm (SSH, Telnet, serial, RDP).
//!
//! Every session runs as a task on a shared tokio runtime. The UI talks to it through a
//! [`SessionHandle`]: commands go in over a tokio channel, events come back over a std channel
//! (so the UI thread can poll without async), and a waker callback tells the UI to repaint.

mod deadline;
mod forward;
mod keepalive;
mod rdp;
mod serial;
mod session;
mod sftp;
mod ssh;
mod ssh_auth;
mod telnet;

use std::io;

pub use session::{
    AuthPrompt, ConnectParams, HostKeyCheck, PromptField, Protocol, SessionEvent, SessionHandle,
    Waker,
};
pub use forward::{ForwardKind, ForwardRule};
pub use keepalive::{KeepAlive, KeepAliveMode, decode_escapes};
pub use rdp::{RdpFrame, RdpInput, RdpMouseButton, RdpSettings, scancode};
pub use serial::{SerialFlow, SerialParity, SerialPortEntry, SerialSettings, list_ports};
pub use sftp::{
    RemoteEntry, SftpConnection, SftpEvent, TransferState, TransferUpdate, is_safe_local_name, remote_join, remote_parent,
};
pub use telnet::TelnetCodec;

pub struct NetRuntime {
    rt: tokio::runtime::Runtime,
}

impl NetRuntime {
    pub fn new() -> io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("playterm-net")
            .enable_all()
            .build()?;
        Ok(Self { rt })
    }

    /// Starts connecting in the background. Progress and data arrive as [`SessionEvent`]s.
    pub fn connect(&self, params: ConnectParams, waker: Waker) -> SessionHandle {
        let (handle, commands, sink) = SessionHandle::new(waker);
        if params.protocol == Protocol::Rdp {
            // Not `Send` (see `rdp`): a thread of its own.
            rdp::spawn(params, commands, sink);
            return handle;
        }
        self.rt.spawn(async move {
            let result = match params.protocol {
                Protocol::Ssh => ssh::run(params, commands, sink.clone()).await,
                Protocol::Telnet => telnet::run(params, commands, sink.clone()).await,
                Protocol::Serial => serial::run(params, commands, sink.clone()).await,
                Protocol::Rdp => unreachable!("started above"),
            };
            if let Err(e) = &result {
                log::warn!("session ended with error: {e:#}");
            }
            sink.send(SessionEvent::Closed(result.err().map(|e| format!("{e:#}"))));
        });
        handle
    }
}
