use std::io;
use std::path::{Path, PathBuf};

use playterm_net::{ForwardRule, KeepAlive, Protocol, SerialSettings};
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

/// One step of an automatic login: when a line ends with `expect`, type `send` and Enter.
/// `send` may contain `{user}` and `{password}` (the site's saved password).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoginStep {
    pub expect: String,
    pub send: String,
}

/// Answers to login prompts (Telnet and serial consoles, or commands after an SSH login).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoLogin {
    pub enabled: bool,
    #[serde(rename = "step")]
    pub steps: Vec<LoginStep>,
}

impl AutoLogin {
    /// The usual Unix/network-device login: user name, then password.
    pub fn standard_steps() -> Vec<LoginStep> {
        vec![
            LoginStep { expect: "login:".into(), send: "{user}".into() },
            LoginStep { expect: "Password:".into(), send: "{password}".into() },
        ]
    }

    /// Whether some step types the saved password.
    pub fn uses_password(&self) -> bool {
        self.enabled && self.steps.iter().any(|s| s.send.contains("{password}"))
    }
}

/// Color that marks a site's tabs (e.g. red for production).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabColor {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Gray,
}

impl TabColor {
    pub const ALL: [TabColor; 9] = [
        TabColor::Red,
        TabColor::Orange,
        TabColor::Yellow,
        TabColor::Green,
        TabColor::Teal,
        TabColor::Blue,
        TabColor::Purple,
        TabColor::Pink,
        TabColor::Gray,
    ];

    /// The color, readable on both the light and the dark theme.
    pub fn rgb(self) -> [u8; 3] {
        match self {
            TabColor::Red => [220, 60, 60],
            TabColor::Orange => [235, 130, 30],
            TabColor::Yellow => [215, 180, 20],
            TabColor::Green => [50, 170, 80],
            TabColor::Teal => [20, 160, 160],
            TabColor::Blue => [50, 120, 230],
            TabColor::Purple => [150, 90, 210],
            TabColor::Pink => [220, 80, 160],
            TabColor::Gray => [130, 130, 130],
        }
    }

    pub fn name(self) -> &'static str {
        use playterm_i18n::t;
        match self {
            TabColor::Red => t("빨강", "Red"),
            TabColor::Orange => t("주황", "Orange"),
            TabColor::Yellow => t("노랑", "Yellow"),
            TabColor::Green => t("초록", "Green"),
            TabColor::Teal => t("청록", "Teal"),
            TabColor::Blue => t("파랑", "Blue"),
            TabColor::Purple => t("보라", "Purple"),
            TabColor::Pink => t("분홍", "Pink"),
            TabColor::Gray => t("회색", "Gray"),
        }
    }
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
    pub auto_login: AutoLogin,
    /// Keeps the connection from timing out while idle.
    pub keepalive: KeepAlive,
    /// Color of the site's tabs; `None` for the usual look.
    pub tab_color: Option<TabColor>,
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
            charset: Charset::Auto,
            term_type: "xterm-256color".into(),
            save_password: false,
            identity_file: String::new(),
            use_agent: true,
            forwards: Vec::new(),
            serial: SerialSettings::default(),
            log: LogSettings::default(),
            auto_login: AutoLogin::default(),
            keepalive: KeepAlive::default(),
            tab_color: None,
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

    /// The profile as a TOML document (no password: profiles never hold one), to hand it to
    /// another playterm window.
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| e.to_string())
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// A copy with a fresh id, for "duplicate".
    pub fn duplicate(&self) -> Self {
        Self { id: random_id(), name: playterm_i18n::tr!("{} (복사본)", "{} (copy)", self.display_name()), ..self.clone() }
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

        let mut a = a;
        a.auto_login = AutoLogin { enabled: true, steps: AutoLogin::standard_steps() };
        a.keepalive = KeepAlive { mode: playterm_net::KeepAliveMode::Text, interval: 300, text: "\\r".into() };
        a.tab_color = Some(TabColor::Red);
        store.upsert(a.clone()).unwrap();
        assert_eq!(ProfileStore::load(&path).unwrap().get(&a.id), Some(&a), "login steps are saved");

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
        assert!(text.contains("tab_color = \"red\""), "{text}");

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
        assert_eq!(p.keepalive, KeepAlive::default(), "and get protocol keep-alives");
    }

    #[test]
    fn toml_round_trip() {
        let p = SessionProfile {
            name: "core".into(),
            host: "10.0.0.1".into(),
            username: "admin".into(),
            tab_color: Some(TabColor::Blue),
            auto_login: AutoLogin { enabled: true, steps: AutoLogin::standard_steps() },
            ..Default::default()
        };
        assert_eq!(SessionProfile::from_toml(&p.to_toml().unwrap()).unwrap(), p);
        assert!(SessionProfile::from_toml("host = 5").is_err());
    }

    #[test]
    fn duplicate_gets_new_id() {
        let p = SessionProfile { host: "h".into(), ..Default::default() };
        let d = p.duplicate();
        assert_ne!(p.id, d.id);
        assert_eq!(p.host, d.host);
    }
}
