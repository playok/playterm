//! Importing the sessions Xshell saves as `.xsh` files (INI text, one file per session) in its
//! Sessions folder; subfolders there become site folders.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use playterm_config::SessionProfile;
use playterm_i18n::{t, tr};
use playterm_net::{Protocol, SerialFlow, SerialParity, SerialSettings};
use playterm_term::Charset;

use crate::import_dialog::ImportedSession;

/// How much of a folder tree is read. A wrongly picked folder such as a whole drive or the
/// home folder must not hang the dialog, so folders and directory entries are counted too,
/// not only session files.
#[derive(Clone, Copy, Debug)]
struct Limits {
    /// Deepest subfolder read.
    depth: usize,
    /// Folders read.
    dirs: usize,
    /// Directory entries looked at (files and folders).
    entries: usize,
    /// Session files read.
    files: usize,
}

const LIMITS: Limits = Limits { depth: 8, dirs: 2_000, entries: 50_000, files: 5_000 };

/// The sessions found under a folder.
#[derive(Debug)]
pub struct Loaded {
    pub sessions: Vec<ImportedSession>,
    /// The folder held more than [`LIMITS`] allow: only part of it was read.
    pub truncated: bool,
}

/// Values of an `.xsh` file by (section, key), both lower-case.
pub type Values = HashMap<(String, String), String>;

/// Where Xshell keeps its sessions: `PLAYTERM_XSHELL_DIR` (for testing), else the newest of
/// `Documents\NetSarang Computer\<version>\Xshell\Sessions` (Xshell 6 and later) and
/// `%APPDATA%\NetSarang\Xshell\Sessions` (Xshell 5) that exists.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PLAYTERM_XSHELL_DIR") {
        return Some(PathBuf::from(dir));
    }
    let mut candidates = Vec::new();
    if let Some(docs) = directories::UserDirs::new().and_then(|d| d.document_dir().map(Path::to_owned)) {
        let base = docs.join("NetSarang Computer");
        let mut versions: Vec<(u32, PathBuf)> = std::fs::read_dir(&base)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Some((e.file_name().to_str()?.parse().ok()?, e.path())))
            .collect();
        versions.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
        candidates.extend(versions.into_iter().map(|(_, dir)| dir.join("Xshell").join("Sessions")));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("NetSarang").join("Xshell").join("Sessions"));
    }
    candidates.into_iter().find(|d| d.is_dir())
}

/// The text of an `.xsh` file: UTF-16 (Xshell 6 and later write it with a byte order mark),
/// UTF-8, or the Korean code page of older versions.
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], le: bool| {
        let units: Vec<u16> =
            bytes.as_chunks::<2>().0.iter().map(|&c| if le { u16::from_le_bytes(c) } else { u16::from_be_bytes(c) }).collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, true)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, false)
    } else {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => encoding_rs::EUC_KR.decode(bytes).0.into_owned(),
        }
    }
}

/// `[SECTION]` and `Key=Value` lines (`;` / `#` start a comment line).
pub fn parse(text: &str) -> Values {
    let mut values = Values::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_lowercase();
        } else if let Some((key, value)) = line.split_once('=') {
            values.insert((section.clone(), key.trim().to_lowercase()), value.trim().to_owned());
        }
    }
    values
}

/// `CodePage=65001` (UTF-8), `949` / `51949` (Korean); anything else is detected from the output.
fn charset(code_page: &str) -> Charset {
    match code_page.trim() {
        "65001" => Charset::Utf8,
        "949" | "51949" => Charset::Cp949,
        other => {
            let lower = other.to_lowercase();
            if lower.contains("utf-8") || lower.contains("utf8") {
                Charset::Utf8
            } else if lower.contains("949") || lower.contains("euc-kr") || lower.contains("korean") {
                Charset::Cp949
            } else {
                Charset::Auto
            }
        }
    }
}

/// Converts one session (`name` is the file name without `.xsh`, `folder` its subfolder).
pub fn convert(name: &str, folder: &str, values: &Values) -> ImportedSession {
    let get = |section: &str, key: &str| values.get(&(section.to_owned(), key.to_owned())).map(String::as_str).unwrap_or("");
    let number = |section: &str, key: &str| get(section, key).parse::<u32>().ok();
    let done = |profile: Result<SessionProfile, String>, notes: Vec<String>| ImportedSession {
        name: name.to_owned(),
        folder: folder.to_owned(),
        profile,
        notes,
    };
    let mut notes = Vec::new();

    let protocol = match get("connection", "protocol").to_lowercase().as_str() {
        "" | "ssh" => Protocol::Ssh,
        "telnet" => Protocol::Telnet,
        "serial" => Protocol::Serial,
        other => {
            let reason = tr!("{other} 프로토콜은 지원하지 않습니다", "The {other} protocol is not supported");
            return done(Err(reason), notes);
        }
    };
    let mut p = SessionProfile { name: name.to_owned(), protocol, ..Default::default() };
    p.charset = charset(get("terminal", "codepage"));
    let term = get("terminal", "type");
    if !term.is_empty() {
        p.term_type = term.to_owned();
    }
    if !get("connection:authentication", "password").is_empty() {
        notes.push(
            t("Xshell 이 암호화해 저장한 비밀번호는 가져오지 않습니다", "The password Xshell saved (encrypted) is not imported").into(),
        );
    }

    if protocol == Protocol::Serial {
        // The COM port: "COM3" or its number.
        let port = ["port", "comport"]
            .iter()
            .map(|k| get("connection:serial", k))
            .find(|v| !v.is_empty())
            .map(|v| if v.parse::<u32>().is_ok() { format!("COM{v}") } else { v.to_uppercase() })
            .unwrap_or_default();
        if port.is_empty() {
            return done(Err(t("시리얼 포트가 비어 있습니다", "The serial port is empty").into()), notes);
        }
        let mut s = SerialSettings { port, ..Default::default() };
        // Xshell stores some line settings as list positions rather than values; only values
        // that are plainly what they say are taken, the rest keep the defaults (9600 8N1).
        match number("connection:serial", "baudrate") {
            Some(b) if b >= 110 => s.baud = b,
            _ => notes.push(tr!(
                "통신 속도를 읽지 못해 {}bps 로 두었습니다. 확인하세요",
                "Could not read the speed, left at {} bps. Please check it",
                s.baud
            )),
        }
        if let Some(bits) = number("connection:serial", "databits").and_then(|b| u8::try_from(b).ok()).filter(|b| (5..=8).contains(b)) {
            s.data_bits = bits;
        }
        s.parity = match get("connection:serial", "parity") {
            "" | "0" => SerialParity::None,
            "1" => SerialParity::Odd,
            "2" => SerialParity::Even,
            _ => {
                notes.push(t("패리티를 None 으로 두었습니다. 확인하세요", "Parity left at None. Please check it").into());
                SerialParity::None
            }
        };
        s.flow = match get("connection:serial", "flowctrl") {
            "" | "0" => SerialFlow::None,
            _ => {
                notes.push(t("흐름 제어를 None 으로 두었습니다. 확인하세요", "Flow control left at None. Please check it").into());
                SerialFlow::None
            }
        };
        p.host = s.port.clone();
        p.port = 0;
        p.serial = s;
        return done(Ok(p), notes);
    }

    let mut host = get("connection", "host").to_owned();
    let mut user = get("connection:authentication", "username").to_owned();
    if let Some((u, h)) = host.rsplit_once('@').map(|(u, h)| (u.to_owned(), h.to_owned())) {
        if user.is_empty() {
            user = u;
        }
        host = h;
    }
    if host.is_empty() {
        return done(Err(t("호스트가 비어 있습니다", "The host is empty").into()), notes);
    }
    p.host = host;
    p.port = number("connection", "port").and_then(|n| u16::try_from(n).ok()).filter(|&n| n > 0).unwrap_or(protocol.default_port());

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
        let key = get("connection:authentication", "userkey");
        if !key.is_empty() {
            notes.push(tr!(
                "Xshell 사용자 키 '{key}' 는 Xshell 안에 있어 가져올 수 없습니다. 키를 내보내 사이트 관리에서 키 파일로 지정하세요",
                "Xshell user key '{key}' lives inside Xshell and cannot be imported. Export it and set it as the key file in the Site Manager"
            ));
        }
        let forwards = values
            .iter()
            .filter(|((section, _), _)| section.contains("tunnel") || section.contains("forward"))
            .any(|((_, key), value)| key == "count" && value.parse::<u32>().is_ok_and(|n| n > 0));
        if forwards {
            notes.push(t("포트 포워딩은 가져오지 않습니다", "Port forwardings are not imported").into());
        }
    }
    done(Ok(p), notes)
}

/// The `.xsh` files under `dir` with their subfolder ("" at the top, `a/b` below), sorted, and
/// whether `limits` stopped the walk before it read everything.
fn session_files(dir: &Path, limits: Limits) -> (Vec<(String, PathBuf)>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    let (mut dirs, mut entries_seen) = (0, 0);
    let mut stack = vec![(dir.to_owned(), String::new(), 0)];
    'walk: while let Some((path, folder, depth)) = stack.pop() {
        if dirs >= limits.dirs {
            truncated = true;
            break;
        }
        dirs += 1;
        let Ok(entries) = std::fs::read_dir(&path) else { continue };
        for entry in entries.flatten() {
            if entries_seen >= limits.entries || out.len() >= limits.files {
                truncated = true;
                break 'walk;
            }
            entries_seen += 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            // Not followed through links: they could lead out of the folder, or in circles.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth < limits.depth {
                    let sub = if folder.is_empty() { name } else { format!("{folder}/{name}") };
                    stack.push((path, sub, depth + 1));
                } else {
                    truncated = true;
                }
            } else if kind.is_file() && name.to_lowercase().ends_with(".xsh") {
                out.push((folder.clone(), path));
            }
        }
    }
    out.sort_by_key(|(folder, path)| (folder.to_lowercase(), path.to_string_lossy().to_lowercase()));
    (out, truncated)
}

/// Reads and converts every session under `dir` (as far as [`LIMITS`] allow).
pub fn load(dir: &Path) -> Result<Loaded, String> {
    load_with(dir, LIMITS)
}

fn load_with(dir: &Path, limits: Limits) -> Result<Loaded, String> {
    if !dir.is_dir() {
        return Err(tr!("폴더가 없습니다: {}", "Folder not found: {}", dir.display()));
    }
    let (files, truncated) = session_files(dir, limits);
    let sessions = files
        .into_iter()
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

    const SSH: &str = "[SessionInfo]\r\nVersion=7.0\r\n[CONNECTION]\r\nPort=2222\r\nHost=db.example.com\r\nProtocol=SSH\r\n\
        [CONNECTION:AUTHENTICATION]\r\nUserName=admin\r\nPassword=AbCdEf==\r\nUserKey=id_rsa_2048\r\n\
        [TERMINAL]\r\nType=xterm-256color\r\nCodePage=65001\r\n";

    fn utf16le(s: &str) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        out.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
        out
    }

    #[test]
    fn decodes_and_parses() {
        assert_eq!(decode(&utf16le("[A]\r\nk=웹서버")), "[A]\r\nk=웹서버");
        assert_eq!(decode(b"\xEF\xBB\xBFk=v"), "k=v");
        // CP949 bytes of "웹서버".
        assert_eq!(decode(b"k=\xC0\xA5\xBC\xAD\xB9\xF6"), "k=웹서버");

        let v = parse("; comment\n[Connection]\n Host = h \nempty=\n[TERMINAL]\nType=vt100\n");
        assert_eq!(v.get(&("connection".into(), "host".into())).map(String::as_str), Some("h"));
        assert_eq!(v.get(&("connection".into(), "empty".into())).map(String::as_str), Some(""));
        assert_eq!(v.get(&("terminal".into(), "type".into())).map(String::as_str), Some("vt100"));
    }

    #[test]
    fn converts_ssh() {
        let r = convert("DB", "운영/DB", &parse(&decode(&utf16le(SSH))));
        let p = r.profile.unwrap();
        assert_eq!((p.name.as_str(), p.host.as_str(), p.port, p.username.as_str()), ("DB", "db.example.com", 2222, "admin"));
        assert_eq!((p.charset, p.term_type.as_str(), r.folder.as_str()), (Charset::Utf8, "xterm-256color", "운영/DB"));
        assert_eq!(r.notes.len(), 2, "the password and the user key are reported: {:?}", r.notes);
    }

    #[test]
    fn converts_telnet_serial_and_rejects_others() {
        let p = convert("r", "", &parse("[CONNECTION]\nHost=10.0.0.1\nProtocol=TELNET\n[TERMINAL]\nCodePage=949")).profile.unwrap();
        assert_eq!((p.protocol, p.port, p.charset), (Protocol::Telnet, 23, Charset::Cp949));

        let serial = "[CONNECTION]\nProtocol=SERIAL\n[CONNECTION:SERIAL]\nPort=3\nBaudRate=115200\nDataBits=8\nParity=2\nFlowCtrl=0\n";
        let r = convert("console", "", &parse(serial));
        let p = r.profile.unwrap();
        assert_eq!((p.serial.port.as_str(), p.serial.baud, p.serial.parity), ("COM3", 115200, SerialParity::Even));
        assert!(r.notes.is_empty(), "{:?}", r.notes);
        // A speed stored as a list position is not taken as bits per second.
        let r = convert("c", "", &parse("[CONNECTION]\nProtocol=SERIAL\n[CONNECTION:SERIAL]\nComPort=COM1\nBaudRate=6\n"));
        assert_eq!((r.profile.as_ref().unwrap().serial.baud, r.notes.len()), (9600, 1));

        assert!(convert("x", "", &parse("[CONNECTION]\nHost=h\nProtocol=RLOGIN")).profile.is_err());
        assert!(convert("x", "", &parse("[CONNECTION]\nProtocol=SSH")).profile.is_err());
        let tunnels = "[CONNECTION]\nHost=h\n[CONNECTION:AUTHENTICATION]\nUserName=u\n[TUNNELING]\nCount=2\n";
        assert_eq!(convert("t", "", &parse(tunnels)).notes.len(), 1);
    }

    #[test]
    fn loads_a_folder_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("운영").join("DB")).unwrap();
        std::fs::write(dir.path().join("web.xsh"), "[CONNECTION]\nHost=web\n[CONNECTION:AUTHENTICATION]\nUserName=u\n").unwrap();
        std::fs::write(dir.path().join("운영").join("DB").join("master.XSH"), utf16le(SSH)).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a session").unwrap();
        let loaded = load(dir.path()).unwrap();
        let found: Vec<(&str, &str)> = loaded.sessions.iter().map(|s| (s.folder.as_str(), s.name.as_str())).collect();
        assert_eq!(found, [("", "web"), ("운영/DB", "master")]);
        assert!(!loaded.truncated);
        assert!(load(&dir.path().join("missing")).is_err());
    }

    /// A tree with `width` subfolders, each holding `width` session files named in reverse.
    fn wide_tree(width: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for d in 0..width {
            let sub = dir.path().join(format!("d{d}"));
            std::fs::create_dir(&sub).unwrap();
            for f in (0..width).rev() {
                std::fs::write(sub.join(format!("s{f}.xsh")), "[CONNECTION]\nHost=h\n").unwrap();
            }
        }
        dir
    }

    #[test]
    fn stops_at_the_limits_and_still_sorts() {
        let dir = wide_tree(4);
        let all = Limits { depth: 8, dirs: 100, entries: 1000, files: 1000 };
        let (files, truncated) = session_files(dir.path(), all);
        assert_eq!((files.len(), truncated), (16, false));

        // Folders without any session files count too: a big tree stops early.
        for limits in [Limits { dirs: 2, ..all }, Limits { entries: 6, ..all }, Limits { files: 3, ..all }, Limits { depth: 0, ..all }] {
            let (files, truncated) = session_files(dir.path(), limits);
            assert!(truncated, "{limits:?}");
            assert!(files.len() < 16, "{limits:?}");
            let mut sorted = files.clone();
            sorted.sort_by_key(|(folder, path)| (folder.to_lowercase(), path.to_string_lossy().to_lowercase()));
            assert_eq!(files, sorted, "sorted even when cut short: {limits:?}");
        }
        assert!(load_with(dir.path(), Limits { files: 3, ..all }).unwrap().truncated);
    }
}
