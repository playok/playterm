use playterm_i18n::t;
use serde::{Deserialize, Serialize};
use std::fmt;

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
            SerialParity::None => t!("common.none"),
            SerialParity::Odd => t!("serial.odd"),
            SerialParity::Even => t!("serial.even"),
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
            SerialFlow::None => t!("common.none"),
            SerialFlow::Hardware => t!("serial.hardware_rts_cts"),
            SerialFlow::Software => t!("serial.software_xon_xoff"),
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
}
