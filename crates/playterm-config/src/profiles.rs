use std::io;
use std::path::{Path, PathBuf};

use playterm_net::{ForwardRule, Protocol, SerialSettings};
use playterm_term::Charset;
use serde::{Deserialize, Serialize};

use crate::fsutil::{random_id, write_atomic};

/// Per-session output logging.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    /// Start logging automatically when the session connects.
    pub auto: bool,
    /// Raw bytes instead of cleaned-up text.
    pub raw: bool,
    /// Prefix text log lines with the time.
    pub timestamps: bool,
    /// Folder for log files; empty = the default log folder.
    pub dir: String,
}

/// A saved connection. Passwords are never stored here, only in the [`crate::Vault`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionProfile {
    pub id: String,
    pub name: String,
    /// Group shown in the session list; empty for top level.
    pub folder: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub charset: Charset,
    pub term_type: String,
    /// Whether the password for this profile is kept in the vault.
    pub save_password: bool,
    /// Private key file (OpenSSH or PuTTY .ppk); empty for none. `~` expands to the home dir.
    pub identity_file: String,
    /// Offer keys from a running SSH agent (Pageant, Windows OpenSSH agent).
    pub use_agent: bool,
    /// Port forwardings started with the session.
    #[serde(rename = "forward")]
    pub forwards: Vec<ForwardRule>,
    /// Line settings for serial sessions.
    pub serial: SerialSettings,
    pub log: LogSettings,
}

impl Default for SessionProfile {
    fn default() -> Self {
        Self {
            id: random_id(),
            name: String::new(),
            folder: String::new(),
            protocol: Protocol::Ssh,
            host: String::new(),
            port: Protocol::Ssh.default_port(),
            username: String::new(),
            charset: Charset::Utf8,
            term_type: "xterm-256color".into(),
            save_password: false,
            identity_file: String::new(),
            use_agent: true,
            forwards: Vec::new(),
            serial: SerialSettings::default(),
            log: LogSettings::default(),
        }
    }
}

impl SessionProfile {
    /// Name to show in lists: the explicit name, or `user@host`.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.clone()
        } else if self.protocol == Protocol::Serial {
            self.serial.port.clone()
        } else if self.username.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.username, self.host)
        }
    }

    /// A copy with a fresh id, for "duplicate".
    pub fn duplicate(&self) -> Self {
        Self { id: random_id(), name: format!("{} (복사본)", self.display_name()), ..self.clone() }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct ProfileFile {
    #[serde(default, rename = "session")]
    sessions: Vec<SessionProfile>,
}

/// The `sessions.toml` file.
pub struct ProfileStore {
    path: PathBuf,
    profiles: Vec<SessionProfile>,
}

impl ProfileStore {
    /// Loads profiles from `path`; a missing file is an empty store.
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let profiles = match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str::<ProfileFile>(&text)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))?
                    .sessions
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, profiles })
    }

    /// An empty store that will be written to `path` on the first change.
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), profiles: Vec::new() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn profiles(&self) -> &[SessionProfile] {
        &self.profiles
    }

    pub fn get(&self, id: &str) -> Option<&SessionProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Inserts or replaces (by id) and saves.
    pub fn upsert(&mut self, profile: SessionProfile) -> io::Result<()> {
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        self.profiles.retain(|p| p.id != id);
        self.save()
    }

    /// Sorted folder names (empty string = top level first).
    pub fn folders(&self) -> Vec<String> {
        let mut folders: Vec<String> = self.profiles.iter().map(|p| p.folder.trim().to_owned()).collect();
        folders.sort_by_key(|f| f.to_lowercase());
        folders.dedup();
        folders
    }

    fn save(&self) -> io::Result<()> {
        let file = ProfileFile { sessions: self.profiles.clone() };
        let text = toml::to_string_pretty(&file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(&self.path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("sessions.toml");

        let mut store = ProfileStore::load(&path).unwrap();
        assert!(store.profiles().is_empty());

        let a = SessionProfile {
            name: "router".into(),
            folder: "lab".into(),
            protocol: Protocol::Telnet,
            host: "10.0.0.1".into(),
            port: 23,
            charset: Charset::Cp949,
            ..Default::default()
        };
        let b = SessionProfile {
            host: "web01".into(),
            username: "deploy".into(),
            forwards: vec![ForwardRule::parse(playterm_net::ForwardKind::Local, "8080:db:5432").unwrap()],
            ..Default::default()
        };
        store.upsert(a.clone()).unwrap();
        store.upsert(b.clone()).unwrap();

        let serial = SessionProfile {
            protocol: Protocol::Serial,
            serial: SerialSettings { port: "COM3".into(), baud: 115200, ..Default::default() },
            log: LogSettings { auto: true, timestamps: true, ..Default::default() },
            ..Default::default()
        };
        assert_eq!(serial.display_name(), "COM3");
        store.upsert(serial.clone()).unwrap();
        let reloaded = ProfileStore::load(&path).unwrap();
        assert_eq!(reloaded.get(&serial.id), Some(&serial));
        store.remove(&serial.id).unwrap();

        let reloaded = ProfileStore::load(&path).unwrap();
        assert_eq!(reloaded.profiles(), &[a.clone(), b.clone()]);
        assert_eq!(reloaded.folders(), vec!["".to_owned(), "lab".to_owned()]);
        assert_eq!(b.display_name(), "deploy@web01");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("protocol = \"telnet\""), "{text}");
        assert!(text.contains("charset = \"cp949\""), "{text}");

        store.upsert(SessionProfile { name: "core".into(), ..a.clone() }).unwrap();
        store.remove(&b.id).unwrap();
        let reloaded = ProfileStore::load(&path).unwrap();
        assert_eq!(reloaded.profiles().len(), 1);
        assert_eq!(reloaded.profiles()[0].name, "core");
    }

    #[test]
    fn missing_fields_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.toml");
        std::fs::write(&path, "[[session]]\nid = \"x\"\nhost = \"h\"\n").unwrap();
        let store = ProfileStore::load(&path).unwrap();
        let p = &store.profiles()[0];
        assert_eq!((p.id.as_str(), p.port, p.protocol), ("x", 22, Protocol::Ssh));
        assert!(p.use_agent && p.identity_file.is_empty(), "files from older versions keep agent auth on");
    }

    #[test]
    fn duplicate_gets_new_id() {
        let p = SessionProfile { host: "h".into(), ..Default::default() };
        let d = p.duplicate();
        assert_ne!(p.id, d.id);
        assert_eq!(p.host, d.host);
    }
}
