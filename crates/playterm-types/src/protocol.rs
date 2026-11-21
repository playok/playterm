use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Ssh,
    Telnet,
    TelnetTls,
    Raw,
    Rlogin,
    /// Local COM port; `host`/`port` are unused, see [`crate::SerialSettings`].
    Serial,
    /// Windows Remote Desktop; shown as a picture instead of a terminal.
    Rdp,
}

impl Protocol {
    pub const ALL: [Protocol; 7] =
        [Protocol::Ssh, Protocol::Telnet, Protocol::TelnetTls, Protocol::Raw, Protocol::Rlogin, Protocol::Serial, Protocol::Rdp];

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh => 22,
            Protocol::Telnet => 23,
            Protocol::TelnetTls => 992,
            Protocol::Raw => 23,
            Protocol::Rlogin => 513,
            Protocol::Serial => 0,
            Protocol::Rdp => 3389,
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Protocol::Ssh => "SSH",
            Protocol::Telnet => "Telnet",
            Protocol::TelnetTls => "Telnet/SSL",
            Protocol::Raw => "Raw TCP",
            Protocol::Rlogin => "rlogin",
            Protocol::Serial => "Serial",
            Protocol::Rdp => "RDP",
        })
    }
}
