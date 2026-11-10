//! Importing the sessions SecureCRT saves as `.ini` files (one file per session) in its
//! `Config\Sessions` folder; subfolders there become site folders.
//!
//! Each line is `T:"Name"=value`, the letter giving the type: `S` text, `D` a number in
//! hexadecimal, `B`/`Z` binary or a list (its data continues on the following, indented lines).
//! Only the plain settings are read. The passwords SecureCRT saved (encrypted) are not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use playterm_config::SessionProfile;
use playterm_i18n::{t, tr};
use playterm_net::{Protocol, SerialFlow, SerialParity, SerialSettings};
use playterm_term::Charset;

use crate::import_dialog::ImportedSession;
use crate::xshell::{LIMITS, Limits, Loaded, decode, session_files};

/// Values of a session file by name, lower-case, without the type letter: `S:"Hostname"=h` is
/// `hostname` → `h`, `D:"[SSH2] Port"=00000016` is `[ssh2] port` → `00000016`.
pub type Values = HashMap<String, String>;

/// Where SecureCRT keeps its sessions: `PLAYTERM_SECURECRT_DIR` (for testing), else its default
/// configuration folder on this system, if it exists. A configuration folder moved elsewhere
/// in SecureCRT's options is picked in the dialog.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PLAYTERM_SECURECRT_DIR") {
        return Some(PathBuf::from(dir));
    }
    let mut candidates = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("VanDyke").join("Config").join("Sessions"));
    }
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_owned()) {
        candidates.push(home.join("Library/Application Support/VanDyke/SecureCRT/Config/Sessions"));
        candidates.push(home.join(".vandyke/SecureCRT/Config/Sessions"));
    }
    candidates.into_iter().find(|d| d.is_dir())
}

/// The `T:"Name"=value` lines; continuation lines of lists and anything else are skipped.
pub fn parse(text: &str) -> Values {
    let mut values = Values::new();
    for line in text.lines() {
        // Continuation lines of a `B:`/`Z:` value are indented.
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let line = line.trim_end();
        let Some(rest) = line.get(1..).filter(|_| line.starts_with(['S', 'D', 'B', 'Z'])) else { continue };
        let Some(rest) = rest.strip_prefix(":\"") else { continue };
        let Some((name, value)) = rest.split_once("\"=") else { continue };
        values.insert(name.trim().to_lowercase(), value.trim().to_owned());
    }
    values
}

/// `Output Transformer Name`: `UTF-8`, a Korean code page, or `Default` (the PC's code page,
/// detected here from the output).
fn charset(name: &str) -> Charset {
    if let Some(charset) = Charset::from_label(name) {
        return charset;
    }
    let lower = name.trim().to_lowercase();
    if lower.contains("utf-8") || lower.contains("utf8") {
        Charset::Utf8
    } else if lower.contains("949") || lower.contains("euc-kr") || lower.contains("korean") {
        Charset::Cp949
    } else {
        Charset::Auto
    }
}

/// Whether the session file holds a password SecureCRT saved (encrypted).
fn has_saved_password(values: &Values) -> bool {
    ["password", "password v2"].iter().any(|k| values.get(*k).is_some_and(|v| !v.is_empty()))
}

/// Converts one session (`name` is the file name without `.ini`, `folder` its subfolder).
pub fn convert(name: &str, folder: &str, values: &Values) -> ImportedSession {
    let get = |key: &str| values.get(key).map(String::as_str).unwrap_or("");
    // `D:` values are hexadecimal.
    let number = |key: &str| u32::from_str_radix(get(key).trim(), 16).ok();
    let done = |profile: Result<SessionProfile, String>, notes: Vec<String>| ImportedSession {
        name: name.to_owned(),
        folder: folder.to_owned(),
        profile,
        notes,
    };
    let mut notes = Vec::new();
    if has_saved_password(values) {
        notes.push(t!("securecrt.the_password_securecrt_saved_encrypted_is_not_imported").into());
    }

    let protocol_name = get("protocol name");
    let protocol = match protocol_name.to_lowercase().as_str() {
        "" | "ssh2" => Protocol::Ssh,
        "telnet" => Protocol::Telnet,
        "serial" => Protocol::Serial,
        _ => {
            let other = protocol_name;
            return done(Err(tr!("common.the_protocol_is_not_supported", other = other)), notes);
        }
    };
    let base = match SessionProfile::new() {
        Ok(profile) => profile,
        Err(e) => return done(Err(e.to_string()), notes),
    };
    let mut p = SessionProfile { name: name.to_owned(), protocol, ..base };
    p.charset = charset(get("output transformer name"));
    // Xterm (and anything else) keeps the default terminal type.
    let emulation = get("emulation").to_lowercase();
    if ["vt100", "vt102", "vt220", "vt320", "linux", "ansi"].contains(&emulation.as_str()) {
        p.term_type = emulation;
    }

    if protocol == Protocol::Serial {
        let port = get("com port").trim().to_uppercase();
        if port.is_empty() {
            return done(Err(t!("common.the_serial_port_is_empty").into()), notes);
        }
        let mut s = SerialSettings { port, ..Default::default() };
        match number("baud rate") {
            Some(b) if b >= 110 => s.baud = b,
            _ => notes.push(tr!("common.could_not_read_the_speed_left_at_bps", arg0 = s.baud)),
        }
        if let Some(bits) = number("data bits").and_then(|b| u8::try_from(b).ok()).filter(|b| (5..=8).contains(b)) {
            s.data_bits = bits;
        }
        s.parity = match number("parity").unwrap_or(0) {
            0 => SerialParity::None,
            1 => SerialParity::Odd,
            2 => SerialParity::Even,
            _ => {
                notes.push(t!("common.mark_space_parity_is_not_supported_set_to").into());
                SerialParity::None
            }
        };
        // 0: 1 bit, 1: 1.5 bits, 2: 2 bits.
        s.stop_bits = if number("stop bits").unwrap_or(0) >= 1 { 2 } else { 1 };
        let on = |key: &str| number(key).is_some_and(|n| n != 0);
        s.flow = if on("cts flow") || on("rts flow") {
            SerialFlow::Hardware
        } else if on("xon flow") {
            SerialFlow::Software
        } else {
            SerialFlow::None
        };
        p.host = s.port.clone();
        p.port = 0;
        p.serial = s;
        return done(Ok(p), notes);
    }

    let host = get("hostname").trim();
    if host.is_empty() {
        return done(Err(t!("common.the_host_is_empty").into()), notes);
    }
    p.host = host.to_owned();
    let port_key = if protocol == Protocol::Ssh { "[ssh2] port" } else { "port" };
    p.port = number(port_key).and_then(|n| u16::try_from(n).ok()).filter(|&n| n > 0).unwrap_or(protocol.default_port());
    p.username = get("username").trim().to_owned();

    let firewall = get("firewall name").trim();
    if !firewall.is_empty() && !firewall.eq_ignore_ascii_case("none") {
        notes.push(tr!("securecrt.the_firewall_proxy_setting_is_not_imported", firewall = firewall));
    }

    if protocol == Protocol::Ssh {
        if p.username.is_empty() {
            notes.push(t!("common.no_user_name_enter_it_in_the_site").into());
        }
        // `C:\keys\id_rsa::rawkey`: the path, then how SecureCRT reads the key.
        let key = ["identity filename v2", "identity filename"]
            .iter()
            .map(|k| get(k).split("::").next().unwrap_or("").trim())
            .find(|k| !k.is_empty())
            .unwrap_or("");
        let global_key = number("use global public key").is_some_and(|n| n != 0);
        if !key.is_empty() && !global_key {
            p.identity_file = key.to_owned();
        } else if global_key {
            notes.push(t!("securecrt.securecrt_s_global_public_key_is_not_imported").into());
        }
        let forwards = ["port forward table v2", "reverse forward table v2"].iter().any(|k| number(k).is_some_and(|n| n > 0));
        if forwards {
            notes.push(t!("common.port_forwardings_are_not_imported").into());
        }
    }
    done(Ok(p), notes)
}

/// Not sessions: the folder settings SecureCRT keeps in every folder, and the default session
/// (the template for new ones) at the top.
fn is_session_file(folder: &str, path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    !name.starts_with("__") && !(folder.is_empty() && name == "default.ini")
}

/// Reads and converts every session under `dir` (as far as the limits allow).
pub fn load(dir: &Path) -> Result<Loaded, String> {
    load_with(dir, LIMITS)
}

fn load_with(dir: &Path, limits: Limits) -> Result<Loaded, String> {
    if !dir.is_dir() {
        return Err(tr!("common.folder_not_found", arg0 = dir.display()));
    }
    let (files, truncated) = session_files(dir, ".ini", limits);
    let sessions = files
        .into_iter()
        .filter(|(folder, path)| is_session_file(folder, path))
        .map(|(folder, path)| {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            match std::fs::read(&path) {
                Ok(bytes) => convert(&name, &folder, &parse(&decode(&bytes))),
                Err(e) => ImportedSession { name, folder, profile: Err(e.to_string()), notes: Vec::new() },
            }
        })
        .collect();
    Ok(Loaded { sessions, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As SecureCRT writes it (UTF-8 with a byte order mark), trimmed.
    const SSH: &str = "\u{FEFF}S:\"Username\"=admin\r\nS:\"Password V2\"=02:7b9c3f\r\nS:\"Hostname\"=db.example.com\r\n\
        S:\"Protocol Name\"=SSH2\r\nD:\"[SSH2] Port\"=00000862\r\nS:\"Firewall Name\"=None\r\n\
        S:\"Identity Filename V2\"=C:\\keys\\db_ed25519::rawkey\r\nD:\"Use Global Public Key\"=00000000\r\n\
        S:\"Emulation\"=Xterm\r\nS:\"Output Transformer Name\"=UTF-8\r\n\
        Z:\"Port Forward Table V2\"=00000001\r\n 3306|127.0.0.1|3306|db|0\r\nB:\"Keyword Set\"=00000002\r\n 00 01\r\n";

    #[test]
    fn imports_international_encoding_labels() {
        assert_eq!(charset("Shift_JIS"), Charset::ShiftJis);
        assert_eq!(charset("936"), Charset::Gbk);
        assert_eq!(charset("windows-1251"), Charset::Windows1251);
        assert_eq!(charset("unsupported"), Charset::Auto);
    }

    #[test]
    fn parses_typed_lines() {
        let v = parse(&decode(SSH.as_bytes()));
        assert_eq!(v.get("hostname").map(String::as_str), Some("db.example.com"));
        assert_eq!(v.get("[ssh2] port").map(String::as_str), Some("00000862"));
        assert_eq!(v.get("port forward table v2").map(String::as_str), Some("00000001"));
        assert!(!v.keys().any(|k| k.contains("3306")), "continuation lines are not values: {v:?}");
    }

    #[test]
    fn converts_ssh_without_the_password() {
        let r = convert("DB", "운영/DB", &parse(&decode(SSH.as_bytes())));
        let p = r.profile.unwrap();
        assert_eq!((p.name.as_str(), p.host.as_str(), p.port, p.username.as_str()), ("DB", "db.example.com", 2146, "admin"));
        assert_eq!((p.protocol, p.charset, p.identity_file.as_str()), (Protocol::Ssh, Charset::Utf8, r"C:\keys\db_ed25519"));
        assert_eq!(r.folder, "운영/DB");
        assert!(!format!("{p:?}").contains("7b9c3f"), "the saved password is not carried over");
        assert!(r.notes.iter().any(|n| n.contains("비밀번호") || n.contains("password")), "{:?}", r.notes);
        assert_eq!(r.notes.len(), 2, "the password and the forwardings are reported: {:?}", r.notes);
    }

    #[test]
    fn converts_telnet_serial_and_rejects_others() {
        let p = convert("r", "", &parse("S:\"Protocol Name\"=Telnet\nS:\"Hostname\"=10.0.0.1\nD:\"Port\"=00000017\nS:\"Output Transformer Name\"=Korean (949)\nS:\"Emulation\"=VT100\n"))
            .profile
            .unwrap();
        assert_eq!((p.protocol, p.port, p.charset, p.term_type.as_str()), (Protocol::Telnet, 23, Charset::Cp949, "vt100"));

        let serial = "S:\"Protocol Name\"=Serial\nS:\"Com Port\"=com3\nD:\"Baud Rate\"=0001c200\nD:\"Data Bits\"=00000008\n\
            D:\"Parity\"=00000002\nD:\"Stop Bits\"=00000000\nD:\"CTS Flow\"=00000001\n";
        let r = convert("console", "", &parse(serial));
        let p = r.profile.unwrap();
        assert_eq!((p.serial.port.as_str(), p.serial.baud, p.serial.parity), ("COM3", 115200, SerialParity::Even));
        assert_eq!((p.serial.stop_bits, p.serial.flow), (1, SerialFlow::Hardware));
        assert!(r.notes.is_empty(), "{:?}", r.notes);

        for other in ["SSH1", "RLogin", "Raw", "Telnet/SSL"] {
            let r = convert("x", "", &parse(&format!("S:\"Protocol Name\"={other}\nS:\"Hostname\"=h\n")));
            assert!(r.profile.unwrap_err().contains(other));
        }
        assert!(convert("x", "", &parse("S:\"Protocol Name\"=SSH2\n")).profile.is_err(), "no host");
        let global = "S:\"Hostname\"=h\nS:\"Username\"=u\nD:\"Use Global Public Key\"=00000001\nS:\"Identity Filename V2\"=C:\\k::rawkey\nS:\"Firewall Name\"=Jump\n";
        let r = convert("g", "", &parse(global));
        assert_eq!(r.profile.unwrap().identity_file, "");
        assert_eq!(r.notes.len(), 2, "the global key and the firewall: {:?}", r.notes);
    }

    #[test]
    fn loads_a_folder_tree_without_folder_data_or_the_default_session() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("운영").join("DB")).unwrap();
        std::fs::write(dir.path().join("web.ini"), "S:\"Hostname\"=web\nS:\"Username\"=u\n").unwrap();
        std::fs::write(dir.path().join("Default.ini"), "S:\"Hostname\"=\n").unwrap();
        std::fs::write(dir.path().join("__FolderData__.ini"), "Z:\"Folder List\"=00000001\n 운영\n").unwrap();
        std::fs::write(dir.path().join("운영").join("__FolderData__.ini"), "").unwrap();
        std::fs::write(dir.path().join("운영").join("DB").join("master.INI"), SSH).unwrap();
        // A session named "Default" inside a folder is a session like any other.
        std::fs::write(dir.path().join("운영").join("Default.ini"), "S:\"Hostname\"=d\nS:\"Username\"=u\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a session").unwrap();
        let loaded = load(dir.path()).unwrap();
        let found: Vec<(&str, &str)> = loaded.sessions.iter().map(|s| (s.folder.as_str(), s.name.as_str())).collect();
        assert_eq!(found, [("", "web"), ("운영", "Default"), ("운영/DB", "master")]);
        assert!(!loaded.truncated);
        assert!(load(&dir.path().join("missing")).is_err());
        assert!(load_with(dir.path(), Limits { files: 1, ..LIMITS }).unwrap().truncated);
    }
}
