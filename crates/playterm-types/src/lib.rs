//! Serializable connection settings shared by configuration and network transports.
//! No runtime or transport implementation dependencies belong in this crate.
mod forward;
mod keepalive;
mod protocol;
mod rdp;
mod serial;

pub use forward::{ForwardKind, ForwardRule};
pub use keepalive::{KeepAlive, KeepAliveMode, decode_escapes};
pub use protocol::Protocol;
pub use rdp::RdpSettings;
pub use serial::{SerialFlow, SerialParity, SerialSettings};
