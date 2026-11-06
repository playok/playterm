//! Importing the sessions PuTTY saved in the registry
//! (`HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions`, one key per session).

use std::collections::HashMap;

use playterm_config::SessionProfile;
use playterm_i18n::{t, tr};
use playterm_net::{ForwardKind, ForwardRule, Protocol, SerialFlow, SerialParity, SerialSettings};
use playterm_term::Charset;

use crate::import_dialog::ImportedSession;

pub const SESSIONS_KEY: &str = r"Software\SimonTatham\PuTTY\Sessions";

/// The registry key to read: PuTTY's, or `PLAYTERM_PUTTY_KEY` (for testing).
pub fn sessions_key() -> String {
    std::env::var("PLAYTERM_PUTTY_KEY").unwrap_or_else(|_| SESSIONS_KEY.to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
// Read from the Windows registry; elsewhere only the tests make them.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub enum RegValue {
    Str(String),
    Dword(u32),
}

pub type Values = HashMap<String, RegValue>;

/// Session names are stored with `%XX` escapes of the bytes of the name in the system code
/// page (CP949 on Korean Windows), or UTF-8 in newer versions.
pub fn unescape(name: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut bytes = Vec::with_capacity(name.len());
    let raw = name.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        let escaped = match raw.get(i + 1..i + 3) {
            Some(&[hi, lo]) if raw[i] == b'%' => hex(hi).zip(hex(lo)).map(|(h, l)| h << 4 | l),
            _ => None,
        };
        match escaped {
            Some(b) => {
                bytes.push(b);
                i += 3;
            }
            None => {
                bytes.push(raw[i]);
                i += 1;
            }
        }
    }
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => encoding_rs::EUC_KR.decode(e.as_bytes()).0.into_owned(),
    }
}

/// `L8080=host:80,4R127.0.0.1:2222=localhost:22,D1080` → forwarding rules (bad entries noted).
fn parse_forwards(spec: &str, notes: &mut Vec<String>) -> Vec<ForwardRule> {
    let mut rules = Vec::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        // An optional address family prefix: 4 (IPv4 only) or 6 (IPv6 only).
        let entry = entry.trim_start_matches(['4', '6']);
        let mut chars = entry.chars();
        let kind = match chars.next() {
            Some('L') => ForwardKind::Local,
            Some('R') => ForwardKind::Remote,
            Some('D') => ForwardKind::Dynamic,
            _ => {
                notes.push(tr!("포트 포워딩 '{entry}' 을(를) 읽을 수 없습니다", "Cannot read port forwarding '{entry}'"));
                continue;
            }
        };
        let rest = chars.as_str();
        let rule = match (kind, rest.split_once('=')) {
            (ForwardKind::Dynamic, _) => ForwardRule::parse(kind, rest),
            (_, Some((src, dst))) => ForwardRule::parse(kind, &format!("{src}:{dst}")),
            (_, None) => Err(tr!("대상이 없습니다: {entry}", "Missing destination: {entry}")),
        };
        match rule {
            Ok(r) => rules.push(r),
            Err(e) => notes.push(tr!("포트 포워딩 건너뜀: {e}", "Port forwarding skipped: {e}")),
        }
    }
    rules
}

fn charset(code_page: &str) -> Charset {
    let cp = code_page.to_lowercase();
    if ["949", "euc-kr", "ks c 5601", "ksc", "korean"].iter().any(|k| cp.contains(k)) {
        Charset::Cp949
    } else if cp.contains("utf-8") || cp.contains("utf8") {
        Charset::Utf8
    } else {
        // Not set (or a code page we do not have): let the output decide.
        Charset::Auto
    }
}

/// Converts the registry values of one session.
pub fn convert(name: &str, values: &Values) -> ImportedSession {
    let text = |key: &str| match values.get(key) {
        Some(RegValue::Str(s)) => s.trim().to_owned(),
        _ => String::new(),
    };
    let number = |key: &str| match values.get(key) {
        Some(RegValue::Dword(n)) => Some(*n),
        _ => None,
    };
    let mut notes = Vec::new();
    let protocol = match text("Protocol").to_lowercase().as_str() {
        "" | "ssh" => Protocol::Ssh,
        "telnet" => Protocol::Telnet,
        "serial" => Protocol::Serial,
        other => {
            let reason = tr!("{other} 프로토콜은 지원하지 않습니다", "The {other} protocol is not supported");
            return ImportedSession { name: name.to_owned(), folder: String::new(), profile: Err(reason), notes };
        }
    };
    let base = match SessionProfile::new() {
        Ok(profile) => profile,
        Err(e) => return ImportedSession { name: name.to_owned(), folder: String::new(), profile: Err(e.to_string()), notes },
    };
    let mut p = SessionProfile { name: name.to_owned(), protocol, ..base };
    p.charset = charset(&text("LineCodePage"));
    let term = text("TerminalType");
    if !term.is_empty() {
        p.term_type = term;
    }

    if protocol == Protocol::Serial {
        let line = text("SerialLine");
        if line.is_empty() {
            let reason = t("시리얼 포트가 비어 있습니다", "The serial port is empty").into();
            return ImportedSession { name: name.to_owned(), folder: String::new(), profile: Err(reason), notes };
        }
        let mut s = SerialSettings { port: line.to_uppercase(), ..Default::default() };
        s.baud = number("SerialSpeed").filter(|&b| b > 0).unwrap_or(s.baud);
        s.data_bits = number("SerialDataBits").and_then(|b| u8::try_from(b).ok()).filter(|b| (5..=8).contains(b)).unwrap_or(8);
        // Stored in half bits: 2 = 1, 3 = 1.5, 4 = 2.
        s.stop_bits = if number("SerialStopHalfbits").unwrap_or(2) >= 3 { 2 } else { 1 };
        s.parity = match number("SerialParity").unwrap_or(0) {
            1 => SerialParity::Odd,
            2 => SerialParity::Even,
            0 => SerialParity::None,
            _ => {
                notes.push(
                    t("Mark/Space 패리티는 지원하지 않아 None 으로 설정했습니다", "Mark/Space parity is not supported, set to None").into(),
                );
                SerialParity::None
            }
        };
        s.flow = match number("SerialFlowControl").unwrap_or(1) {
            1 => SerialFlow::Software,
            2 => SerialFlow::Hardware,
            0 => SerialFlow::None,
            _ => {
                notes.push(
                    t("DSR/DTR 흐름 제어는 지원하지 않아 None 으로 설정했습니다", "DSR/DTR flow control is not supported, set to None")
                        .into(),
                );
                SerialFlow::None
            }
        };
        p.host = s.port.clone();
        p.port = 0;
        p.serial = s;
        return ImportedSession { name: name.to_owned(), folder: String::new(), profile: Ok(p), notes };
    }

    let mut host = text("HostName");
    let mut user = text("UserName");
    // "user@host" in the host field.
    if let Some((u, h)) = host.rsplit_once('@').map(|(u, h)| (u.to_owned(), h.to_owned())) {
        if user.is_empty() {
            user = u;
        }
        host = h;
    }
    if host.is_empty() {
        let reason = t("호스트가 비어 있습니다", "The host is empty").into();
        return ImportedSession { name: name.to_owned(), folder: String::new(), profile: Err(reason), notes };
    }
    p.host = host;
    p.port = number("PortNumber").and_then(|n| u16::try_from(n).ok()).filter(|&n| n > 0).unwrap_or(protocol.default_port());

    if protocol == Protocol::Ssh {
        if user.is_empty() {
            notes.push(
                t(
                    "사용자 이름이 없습니다. 가져온 뒤 사이트 관리에서 입력하세요",
                    "No user name. Enter it in the Site Manager after the import",
                )
                .into(),
            );
        }
        p.username = user;
        p.identity_file = text("PublicKeyFile");
        p.use_agent = number("TryAgent").unwrap_or(1) != 0;
        p.forwards = parse_forwards(&text("PortForwardings"), &mut notes);
    }
    ImportedSession { name: name.to_owned(), folder: String::new(), profile: Ok(p), notes }
}

/// Reads and converts every session under `key` (`Default Settings` is skipped).
pub fn load(key: &str) -> Result<Vec<ImportedSession>, String> {
    let mut sessions: Vec<ImportedSession> = registry::read_sessions(key)?
        .into_iter()
        .map(|(raw, values)| (unescape(&raw), values))
        .filter(|(name, _)| name != "Default Settings")
        .map(|(name, values)| convert(&name, &values))
        .collect();
    sessions.sort_by_key(|s| s.name.to_lowercase());
    Ok(sessions)
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
    };

    use super::{RegValue, Values};

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from RegOpenKeyExW and is closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// `None` when the key does not exist.
    fn open(parent: HKEY, path: &str) -> Result<Option<Key>, String> {
        let path = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated path and out pointer.
        match unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, KEY_READ, &mut key) } {
            ERROR_SUCCESS => Ok(Some(Key(key))),
            ERROR_FILE_NOT_FOUND => Ok(None),
            e => Err(playterm_i18n::tr!("레지스트리를 열 수 없습니다 (오류 {e})", "Cannot open the registry (error {e})")),
        }
    }

    fn subkeys(key: &Key) -> Vec<String> {
        let mut names = Vec::new();
        let mut buf = [0u16; 512];
        for index in 0.. {
            let mut len = buf.len() as u32;
            // SAFETY: buffer and length describe valid memory; optional pointers are null.
            let r = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    buf.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r != ERROR_SUCCESS {
                break; // ERROR_NO_MORE_ITEMS, or a name too long to be a PuTTY session
            }
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        names
    }

    fn values(key: &Key) -> Values {
        let mut out = Values::new();
        let mut name = [0u16; 16384];
        let mut data = vec![0u8; 64 * 1024];
        for index in 0.. {
            let mut name_len = name.len() as u32;
            let mut data_len = data.len() as u32;
            let mut kind = 0u32;
            // SAFETY: buffers and lengths describe valid memory.
            let r = unsafe {
                RegEnumValueW(key.0, index, name.as_mut_ptr(), &mut name_len, std::ptr::null(), &mut kind, data.as_mut_ptr(), &mut data_len)
            };
            if r == ERROR_NO_MORE_ITEMS {
                break;
            }
            if r != ERROR_SUCCESS {
                continue; // too large for the buffer: not a setting we read
            }
            let value_name = String::from_utf16_lossy(&name[..name_len as usize]);
            let bytes = &data[..data_len as usize];
            let value = match kind {
                REG_SZ | REG_EXPAND_SZ => {
                    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                    RegValue::Str(String::from_utf16_lossy(&units[..end]))
                }
                REG_DWORD if bytes.len() >= 4 => RegValue::Dword(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
                _ => continue,
            };
            out.insert(value_name, value);
        }
        out
    }

    /// Every session key under `path` (in HKEY_CURRENT_USER) with its values.
    pub fn read_sessions(path: &str) -> Result<Vec<(String, Values)>, String> {
        let Some(root) = open(HKEY_CURRENT_USER, path)? else { return Ok(Vec::new()) };
        let mut sessions = Vec::new();
        for name in subkeys(&root) {
            if let Some(key) = open(root.0, &name)? {
                sessions.push((name, values(&key)));
            }
        }
        Ok(sessions)
    }
}

#[cfg(not(windows))]
mod registry {
    pub fn read_sessions(_path: &str) -> Result<Vec<(String, super::Values)>, String> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, RegValue)]) -> Values {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn s(v: &str) -> RegValue {
        RegValue::Str(v.into())
    }

    #[test]
    fn unescapes_names() {
        assert_eq!(unescape("My%20Server"), "My Server");
        assert_eq!(unescape("a%25b%5Cc"), "a%b\\c");
        // CP949 bytes of "웹서버".
        assert_eq!(unescape("%C0%A5%BC%AD%B9%F6"), "웹서버");
        assert_eq!(unescape("%EC%9B%B9"), "웹", "UTF-8 bytes");
        assert_eq!(unescape("50%"), "50%", "a lone % stays");
        assert_eq!(unescape("x%zz"), "x%zz");
        assert_eq!(unescape("%가나"), "%가나", "non-ASCII after % does not split a character");
    }

    #[test]
    fn converts_ssh() {
        let v = values(&[
            ("Protocol", s("ssh")),
            ("HostName", s("admin@db.example.com")),
            ("PortNumber", RegValue::Dword(2222)),
            ("PublicKeyFile", s(r"C:\keys\db.ppk")),
            ("LineCodePage", s("CP949 (EUC-KR)")),
            ("TerminalType", s("xterm")),
            ("PortForwardings", s("L8080=localhost:80,4R127.0.0.1:9000=intra:9000,D1080,Lbad")),
            ("TryAgent", RegValue::Dword(0)),
        ]);
        let r = convert("DB", &v);
        let p = r.profile.unwrap();
        assert_eq!((p.name.as_str(), p.host.as_str(), p.port, p.username.as_str()), ("DB", "db.example.com", 2222, "admin"));
        assert_eq!((p.identity_file.as_str(), p.charset, p.term_type.as_str()), (r"C:\keys\db.ppk", Charset::Cp949, "xterm"));
        assert!(!p.use_agent);
        assert_eq!(p.forwards.len(), 3);
        assert_eq!(p.forwards[0], ForwardRule::parse(ForwardKind::Local, "8080:localhost:80").unwrap());
        assert_eq!(p.forwards[1].bind_host, "127.0.0.1");
        assert_eq!(p.forwards[2].kind, ForwardKind::Dynamic);
        assert_eq!(r.notes.len(), 1, "the bad forwarding is reported: {:?}", r.notes);
    }

    #[test]
    fn converts_telnet_serial_and_rejects_others() {
        let p = convert("router", &values(&[("Protocol", s("telnet")), ("HostName", s("10.0.0.1"))])).profile.unwrap();
        assert_eq!((p.protocol, p.port, p.charset), (Protocol::Telnet, 23, Charset::Auto));
        let utf8 = values(&[("HostName", s("h")), ("UserName", s("u")), ("LineCodePage", s("UTF-8"))]);
        assert_eq!(convert("u", &utf8).profile.unwrap().charset, Charset::Utf8);

        let v = values(&[
            ("Protocol", s("serial")),
            ("SerialLine", s("com3")),
            ("SerialSpeed", RegValue::Dword(115200)),
            ("SerialStopHalfbits", RegValue::Dword(4)),
            ("SerialParity", RegValue::Dword(2)),
            ("SerialFlowControl", RegValue::Dword(0)),
        ]);
        let p = convert("console", &v).profile.unwrap();
        assert_eq!((p.serial.port.as_str(), p.serial.baud, p.serial.stop_bits), ("COM3", 115200, 2));
        assert_eq!((p.serial.parity, p.serial.flow, p.host.as_str()), (SerialParity::Even, SerialFlow::None, "COM3"));

        assert!(convert("r", &values(&[("Protocol", s("rlogin")), ("HostName", s("h"))])).profile.is_err());
        assert!(convert("empty", &values(&[("Protocol", s("ssh"))])).profile.is_err());
        let no_user = convert("n", &values(&[("HostName", s("h"))]));
        assert!(no_user.profile.is_ok() && !no_user.notes.is_empty(), "SSH without a user is imported with a note");
    }
}
