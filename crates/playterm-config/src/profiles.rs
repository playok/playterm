use std::io;
use std::path::{Path, PathBuf};

use playterm_term::Charset;
use playterm_types::{ForwardRule, KeepAlive, Protocol, RdpSettings, SerialSettings};
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
    /// A short word shown with the site's tabs, its connection line and the window title
    /// ("운영", "PROD"); empty for none.
    pub tab_label: String,
    /// SSH: the jump host (bastion) to go through: a saved site's name, or `user@host:port`;
    /// empty for a direct connection.
    pub jump_host: String,
    /// SSH: X11 forwarding (`ssh -X`), to `x11_display` (empty: this PC's DISPLAY, else
    /// `localhost:0.0`).
    pub x11_forwarding: bool,
    pub x11_display: String,
    /// Trusted X11 forwarding (`ssh -Y`): the server's X programs get the whole display,
    /// keyboard included. Off: an untrusted cookie, or no X connections when none can be made.
    pub x11_trusted: bool,
    /// A macro (by name, Tools → Macros) run once connected and logged in; empty: none.
    pub after_connect_macro: String,
    /// Options for RDP sites.
    pub rdp: RdpSettings,
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
            tab_label: String::new(),
            jump_host: String::new(),
            x11_forwarding: false,
            x11_display: String::new(),
            x11_trusted: false,
            after_connect_macro: String::new(),
            rdp: RdpSettings::default(),
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

/// How the sessions of a template are placed when it is opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TemplateLayout {
    /// Tabs of one group.
    #[default]
    Tabs,
    /// Side by side.
    Columns,
    /// One above the other.
    Rows,
    Grid,
}

impl TemplateLayout {
    pub const ALL: [Self; 4] = [Self::Tabs, Self::Columns, Self::Rows, Self::Grid];

    pub fn label(self) -> &'static str {
        use playterm_i18n::t;
        match self {
            Self::Tabs => t("탭으로", "As tabs"),
            Self::Columns => t("좌우로 나란히", "Side by side"),
            Self::Rows => t("위아래로", "Stacked"),
            Self::Grid => t("격자", "Grid"),
        }
    }
}

/// Sites opened together, under a name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteTemplate {
    pub id: String,
    pub name: String,
    /// Site ids, in opening order.
    pub sites: Vec<String>,
    pub layout: TemplateLayout,
}

impl Default for SiteTemplate {
    fn default() -> Self {
        Self { id: random_id(), name: String::new(), sites: Vec::new(), layout: TemplateLayout::Tabs }
    }
}

/// `" a / b//c "` -> `"a/b/c"`: a folder path, with `/` between the levels ("" = top level).
pub fn normalize_folder(path: &str) -> String {
    path.split('/').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
}

/// The folder that contains folder `path` ("" for a folder at the top level).
pub fn parent_folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// The last level of folder `path`: its name.
pub fn folder_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Whether folder `path` is `folder` or inside it.
fn in_folder(path: &str, folder: &str) -> bool {
    path == folder || path.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
}

/// `path` with its part `from` (a folder it is in) replaced by `to`.
fn moved(path: &str, from: &str, to: &str) -> String {
    let rest = path.strip_prefix(from).unwrap_or(path).trim_start_matches('/');
    normalize_folder(&format!("{to}/{rest}"))
}

#[derive(Default, Serialize, Deserialize)]
struct ProfileFile {
    /// Folders that exist on their own (also empty ones); folders of sites exist anyway.
    #[serde(default, rename = "folders", skip_serializing_if = "Vec::is_empty")]
    folders: Vec<String>,
    #[serde(default, rename = "session")]
    sessions: Vec<SessionProfile>,
    #[serde(default, rename = "template", skip_serializing_if = "Vec::is_empty")]
    templates: Vec<SiteTemplate>,
}

/// The `sessions.toml` file: the sites and the templates that open several of them.
pub struct ProfileStore {
    path: PathBuf,
    profiles: Vec<SessionProfile>,
    templates: Vec<SiteTemplate>,
    /// Folders made with "New folder" (see [`ProfileStore::folders`]).
    folders: Vec<String>,
}

impl ProfileStore {
    /// Loads profiles from `path`; a missing file is an empty store.
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<ProfileFile>(&text)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => ProfileFile::default(),
            Err(e) => return Err(e),
        };
        let folders = file.folders.iter().map(|f| normalize_folder(f)).filter(|f| !f.is_empty()).collect();
        let mut profiles = file.sessions;
        for p in &mut profiles {
            p.folder = normalize_folder(&p.folder);
        }
        Ok(Self { path, profiles, templates: file.templates, folders })
    }

    /// An empty store that will be written to `path` on the first change.
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), profiles: Vec::new(), templates: Vec::new(), folders: Vec::new() }
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
    pub fn upsert(&mut self, mut profile: SessionProfile) -> io::Result<()> {
        profile.folder = normalize_folder(&profile.folder);
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
        self.save()
    }

    /// Removes a site (from the templates too) and saves.
    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        self.profiles.retain(|p| p.id != id);
        for template in &mut self.templates {
            template.sites.retain(|s| s != id);
        }
        self.save()
    }

    pub fn templates(&self) -> &[SiteTemplate] {
        &self.templates
    }

    pub fn template(&self, id: &str) -> Option<&SiteTemplate> {
        self.templates.iter().find(|t| t.id == id)
    }

    /// Inserts or replaces (by id) a template and saves.
    pub fn upsert_template(&mut self, template: SiteTemplate) -> io::Result<()> {
        match self.templates.iter_mut().find(|t| t.id == template.id) {
            Some(existing) => *existing = template,
            None => self.templates.push(template),
        }
        self.save()
    }

    pub fn remove_template(&mut self, id: &str) -> io::Result<()> {
        self.templates.retain(|t| t.id != id);
        self.save()
    }

    /// Every folder path, sorted, the top level ("") first: folders made on their own, folders
    /// of sites, and the folders above them (`a/b` makes `a` too).
    pub fn folders(&self) -> Vec<String> {
        let mut all: std::collections::BTreeSet<String> = std::collections::BTreeSet::from([String::new()]);
        for path in self.folders.iter().chain(self.profiles.iter().map(|p| &p.folder)) {
            let mut path = normalize_folder(path);
            while !path.is_empty() {
                let parent = parent_folder(&path).to_owned();
                all.insert(path);
                path = parent;
            }
        }
        let mut folders: Vec<String> = all.into_iter().collect();
        folders.sort_by_key(|f| f.to_lowercase());
        folders
    }

    /// Makes folder `path` (inside its parent folder, made too if needed) and saves.
    pub fn create_folder(&mut self, path: &str) -> io::Result<()> {
        let path = normalize_folder(path);
        if path.is_empty() || self.folders().contains(&path) {
            return Ok(());
        }
        self.folders.push(path);
        self.save()
    }

    /// Renames or moves folder `from` to `to`: its sites and the folders inside it go along.
    /// Into an existing folder, the contents are merged.
    pub fn rename_folder(&mut self, from: &str, to: &str) -> io::Result<()> {
        let (from, to) = (normalize_folder(from), normalize_folder(to));
        if from.is_empty() || to.is_empty() || from == to || in_folder(&to, &from) {
            return Ok(());
        }
        for p in &mut self.profiles {
            if in_folder(&p.folder, &from) {
                p.folder = moved(&p.folder, &from, &to);
            }
        }
        for f in &mut self.folders {
            if in_folder(f, &from) {
                *f = moved(f, &from, &to);
            }
        }
        self.folders.sort();
        self.folders.dedup();
        self.save()
    }

    /// Removes folder `path`; nothing in it is deleted: its sites and the folders inside it move
    /// up to its parent folder.
    pub fn delete_folder(&mut self, path: &str) -> io::Result<()> {
        let path = normalize_folder(path);
        if path.is_empty() {
            return Ok(());
        }
        let parent = parent_folder(&path).to_owned();
        for p in &mut self.profiles {
            if in_folder(&p.folder, &path) {
                p.folder = moved(&p.folder, &path, &parent);
            }
        }
        self.folders.retain(|f| f != &path);
        for f in &mut self.folders {
            if in_folder(f, &path) {
                *f = moved(f, &path, &parent);
            }
        }
        self.folders.retain(|f| !f.is_empty());
        self.folders.sort();
        self.folders.dedup();
        self.save()
    }

    /// The sites in folder `path` and the folders inside it.
    pub fn sites_in_folder(&self, path: &str) -> usize {
        let path = normalize_folder(path);
        self.profiles.iter().filter(|p| in_folder(&p.folder, &path)).count()
    }

    /// Moves site `id` into `folder` and saves.
    pub fn move_site(&mut self, id: &str, folder: &str) -> io::Result<()> {
        let folder = normalize_folder(folder);
        match self.profiles.iter_mut().find(|p| p.id == id) {
            Some(p) if p.folder != folder => {
                p.folder = folder;
                self.save()
            }
            _ => Ok(()),
        }
    }

    fn save(&self) -> io::Result<()> {
        let file = ProfileFile { folders: self.folders.clone(), sessions: self.profiles.clone(), templates: self.templates.clone() };
        let text = toml::to_string_pretty(&file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(&self.path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn tab_label_is_saved_and_optional() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.toml");
        std::fs::write(&path, "[[session]]\nid = \"a\"\nname = \"old\"\nhost = \"h\"\n").unwrap();
        let mut store = ProfileStore::load(&path).unwrap();
        assert_eq!(store.get("a").unwrap().tab_label, "", "older files have no label");
        let mut p = store.get("a").unwrap().clone();
        p.tab_label = "운영".into();
        store.upsert(p).unwrap();
        assert_eq!(ProfileStore::load(&path).unwrap().get("a").unwrap().tab_label, "운영");
    }
    use super::*;

    #[test]
    fn folders_nest_and_survive_renames_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.toml");
        let mut store = ProfileStore::load(&path).unwrap();
        let site = |name: &str, folder: &str| SessionProfile { name: name.into(), folder: folder.into(), ..Default::default() };
        store.upsert(site("web1", " 운영 / 웹 ")).unwrap();
        store.upsert(site("db1", "운영/DB")).unwrap();
        store.upsert(site("lab", "")).unwrap();
        store.create_folder("개발/빈 폴더").unwrap();
        assert_eq!(store.folders(), ["", "개발", "개발/빈 폴더", "운영", "운영/DB", "운영/웹"]);
        assert_eq!(store.sites_in_folder("운영"), 2);

        // Empty folders are kept in the file.
        let reloaded = ProfileStore::load(&path).unwrap();
        assert_eq!(reloaded.folders(), store.folders());

        // A rename moves what is inside.
        store.rename_folder("운영", "prod").unwrap();
        let folder_of = |s: &ProfileStore, name: &str| s.profiles().iter().find(|p| p.name == name).unwrap().folder.clone();
        assert_eq!(folder_of(&store, "web1"), "prod/웹");
        assert_eq!(folder_of(&store, "db1"), "prod/DB");
        assert!(!store.folders().contains(&"운영".to_owned()));
        // Not into itself.
        store.rename_folder("prod", "prod/x").unwrap();
        assert_eq!(folder_of(&store, "web1"), "prod/웹");

        // A delete moves the contents up; no site is lost.
        store.delete_folder("prod").unwrap();
        assert_eq!(folder_of(&store, "web1"), "웹");
        assert_eq!(folder_of(&store, "db1"), "DB");
        store.delete_folder("개발").unwrap();
        assert_eq!(store.folders(), ["", "DB", "빈 폴더", "웹"]);
        assert_eq!(store.profiles().len(), 3);

        let id = store.profiles().iter().find(|p| p.name == "lab").unwrap().id.clone();
        store.move_site(&id, "빈 폴더").unwrap();
        assert_eq!(folder_of(&store, "lab"), "빈 폴더");
        assert_eq!(ProfileStore::load(&path).unwrap().folders(), store.folders());
    }

    #[test]
    fn folder_paths() {
        assert_eq!(normalize_folder(" a / b//c "), "a/b/c");
        assert_eq!(parent_folder("a/b/c"), "a/b");
        assert_eq!(parent_folder("a"), "");
        assert_eq!(folder_name("a/b/c"), "c");
        assert!(in_folder("a/b", "a") && in_folder("a", "a") && !in_folder("ab", "a"));
        assert_eq!(moved("a/b/c", "a/b", "x"), "x/c");
        assert_eq!(moved("a/b", "a/b", ""), "");
    }

    #[test]
    fn templates_are_saved_and_follow_site_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.toml");
        let mut store = ProfileStore::load(&path).unwrap();
        let a = SessionProfile { host: "a".into(), ..Default::default() };
        let b = SessionProfile { host: "b".into(), ..Default::default() };
        store.upsert(a.clone()).unwrap();
        store.upsert(b.clone()).unwrap();
        let template = SiteTemplate {
            name: "lab".into(),
            sites: vec![b.id.clone(), a.id.clone()],
            layout: TemplateLayout::Grid,
            ..Default::default()
        };
        store.upsert_template(template.clone()).unwrap();
        assert_eq!(ProfileStore::load(&path).unwrap().template(&template.id), Some(&template));

        store.remove(&b.id).unwrap();
        let reloaded = ProfileStore::load(&path).unwrap();
        assert_eq!(reloaded.template(&template.id).unwrap().sites, vec![a.id.clone()], "a deleted site leaves its templates");
        store.remove_template(&template.id).unwrap();
        assert!(ProfileStore::load(&path).unwrap().templates().is_empty());
        assert!(!std::fs::read_to_string(&path).unwrap().contains("[[template]]"), "no empty section");
    }

    #[test]
    fn legacy_connection_settings_keep_their_serialized_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.toml");
        std::fs::write(&path, include_str!("../tests/fixtures/sessions-v0.6.0.toml")).unwrap();
        let mut store = ProfileStore::load(&path).unwrap();
        let profile = store.get("v060").unwrap().clone();
        assert_eq!(profile.protocol, Protocol::Ssh);
        assert_eq!(profile.forwards[0].kind, playterm_types::ForwardKind::Remote);
        assert_eq!(profile.forwards[0].bind_host, "127.0.0.2");
        assert_eq!(profile.keepalive.mode, playterm_types::KeepAliveMode::Text);
        assert_eq!(profile.keepalive.text, "\\r");
        assert_eq!(profile.serial.parity, playterm_types::SerialParity::Even);
        assert_eq!(profile.serial.line_summary(), "115200 7E2");
        assert_eq!(profile.rdp.gateway(), Some("gateway.example:8443"));
        assert!(profile.rdp.drive_read_only && profile.rdp.printer);
        store.upsert(profile.clone()).unwrap();
        let reloaded = ProfileStore::load(&path).unwrap();
        assert_eq!(reloaded.get("v060"), Some(&profile));
        // Every explicitly stored legacy value keeps the same table/key names and representation.
        let before: toml::Value = toml::from_str(include_str!("../tests/fixtures/sessions-v0.6.0.toml")).unwrap();
        let after: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        for section in ["forward", "keepalive", "serial", "rdp"] {
            assert_eq!(before["session"][0][section], after["session"][0][section], "{section}");
        }
    }

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
            forwards: vec![ForwardRule::parse(playterm_types::ForwardKind::Local, "8080:db:5432").unwrap()],
            ..Default::default()
        };
        store.upsert(a.clone()).unwrap();
        store.upsert(b.clone()).unwrap();

        let mut a = a;
        a.auto_login = AutoLogin { enabled: true, steps: AutoLogin::standard_steps() };
        a.keepalive = KeepAlive { mode: playterm_types::KeepAliveMode::Text, interval: 300, text: "\\r".into() };
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
