//! Network transports for playterm (SSH, Telnet).
//!
//! Every session runs as a task on a shared tokio runtime. The UI talks to it through a
//! [`SessionHandle`]: commands go in over a tokio channel, events come back over a std channel
//! (so the UI thread can poll without async), and a waker callback tells the UI to repaint.

mod session;
mod ssh;
mod telnet;

use std::io;

pub use session::{
    AuthPrompt, ConnectParams, HostKeyCheck, PromptField, Protocol, SessionEvent, SessionHandle,
    Waker,
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
        self.rt.spawn(async move {
            let result = match params.protocol {
                Protocol::Ssh => ssh::run(params, commands, sink.clone()).await,
                Protocol::Telnet => telnet::run(params, commands, sink.clone()).await,
            };
            if let Err(e) = &result {
                log::warn!("session ended with error: {e:#}");
            }
            sink.send(SessionEvent::Closed(result.err().map(|e| format!("{e:#}"))));
        });
        handle
    }
}
