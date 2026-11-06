use playterm_i18n::{t, tr};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeepAliveMode {
    Off,
    /// SSH keepalive requests, Telnet `IAC NOP`. Serial lines have none.
    #[default]
    Protocol,
    /// Type [`KeepAlive::text`]: resets timeouts that only count user input.
    Text,
}

impl KeepAliveMode {
    pub const ALL: [KeepAliveMode; 3] = [KeepAliveMode::Off, KeepAliveMode::Protocol, KeepAliveMode::Text];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeepAlive {
    pub mode: KeepAliveMode,
    /// Seconds without anything sent before a keep-alive goes out.
    pub interval: u32,
    /// Text typed in [`KeepAliveMode::Text`]: ASCII with the escapes of [`decode_escapes`].
    pub text: String,
}

impl KeepAlive {
    pub const MIN_INTERVAL: u32 = 5;
    pub const MAX_INTERVAL: u32 = 3600;
    /// Space, then backspace: resets idle timers without changing a half-typed command.
    pub const DEFAULT_TEXT: &str = "\\x20\\b";

    pub fn period(&self) -> Duration {
        Duration::from_secs(self.interval.max(1).into())
    }
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self { mode: KeepAliveMode::Protocol, interval: 60, text: Self::DEFAULT_TEXT.into() }
    }
}

/// Turns `\r`, `\n`, `\t`, `\b`, `\e` (Esc), `\\` and `\xHH` into bytes. Other characters must
/// be printable ASCII, so the bytes are the same in every charset.
pub fn decode_escapes(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if !c.is_ascii() || c.is_ascii_control() {
                return Err(tr!(
                    "'{c}' 는 쓸 수 없습니다 (ASCII 문자와 \\x 이스케이프만)",
                    "'{c}' cannot be used (ASCII characters and \\x escapes only)"
                ));
            }
            out.push(c as u8);
            continue;
        }
        let byte = match chars.next() {
            Some('r') => b'\r',
            Some('n') => b'\n',
            Some('t') => b'\t',
            Some('b') => 0x08,
            Some('e') => 0x1b,
            Some('\\') => b'\\',
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                u8::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 2)
                    .ok_or_else(|| tr!("\\x 뒤에는 16진수 두 자리가 와야 합니다", "\\x must be followed by two hex digits"))?
            }
            Some(other) => {
                return Err(tr!("알 수 없는 이스케이프 \\{other}", "Unknown escape \\{other}"));
            }
            None => return Err(t("문자열이 \\ 로 끝납니다", "The text ends with \\").into()),
        };
        out.push(byte);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes() {
        assert_eq!(decode_escapes(KeepAlive::DEFAULT_TEXT).unwrap(), b" \x08");
        assert_eq!(decode_escapes("ab\\r\\n\\t\\e\\\\\\x7F").unwrap(), b"ab\r\n\t\x1b\\\x7f");
        assert_eq!(decode_escapes("").unwrap(), b"");
        assert!(decode_escapes("\\x4").is_err());
        assert!(decode_escapes("\\xZZ").is_err());
        assert!(decode_escapes("\\q").is_err());
        assert!(decode_escapes("a\\").is_err());
        assert!(decode_escapes("한글").is_err());
    }
}
