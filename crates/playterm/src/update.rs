//! Updates from the GitHub releases of playterm.
//!
//! The latest release is looked up at start-up (Tools → Settings can turn that off) and with
//! Help → Check for updates. A newer one is offered in a dialog; installing it downloads this
//! PC's package (`playterm-<version>-windows-<arch>.zip`) and checks it against the release's
//! `SHA256SUMS.txt`, whose Ed25519 signature (`SHA256SUMS.txt.sig`, made by the release
//! workflow with `playterm-sign`) must match [`PUBLIC_KEY`]: a release or download that was
//! tampered with is refused, as is anything not newer than the running version (the version is
//! in the signed file name). The new `playterm.exe` replaces the running one, which Windows
//! allows by renaming it to `playterm.exe.old` (deleted at the next start); the update applies
//! when playterm is restarted.
//!
//! On macOS a newer release is only announced (when it has a package for this Mac,
//! `playterm-<version>-macos-<arch>.zip`) and downloaded from its release page: replacing the
//! executable inside the signed `playterm.app` would break the app's signature.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use eframe::egui;
use playterm_i18n::{t, tr};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Deserialize;

use crate::icon;

const LATEST_URL: &str = "https://api.github.com/repos/playok/playterm/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/playok/playterm/releases";
/// Ed25519 key that signs `SHA256SUMS.txt` of a release (its private half is the
/// `PLAYTERM_SIGNING_KEY` secret of the release workflow).
const PUBLIC_KEY: &str = "2d93c4d13001984a27a1dfcf424e1fa7dc1cc94e8b975644f2592ea2f97db399";
const SUMS: &str = "SHA256SUMS.txt";
const SUMS_SIG: &str = "SHA256SUMS.txt.sig";
const EXE: &str = "playterm.exe";
/// Largest API answer, checksum file or signature read.
const MAX_SMALL: u64 = 1024 * 1024;
/// Largest package downloaded, and largest exe unpacked from it.
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// `1.2.3` or `v1.2.3`; anything else (pre-releases too) is not a version we install.
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.strip_prefix('v').unwrap_or(s).split('.').map(|p| p.parse::<u64>().ok());
        let v = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(v)
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is x.y.z")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A release newer than the running version, with this PC's package.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    pub notes: String,
    pub page: String,
    package: String,
    package_url: String,
    sums_url: String,
    sig_url: String,
}

/// Whether a release can be installed from within playterm (otherwise its page is opened).
const AUTO_INSTALL: bool = cfg!(windows);

/// Operating-system part of the package name; `None` on systems releases have no package for
/// (Linux and other Unix systems), where no update is offered.
fn os() -> Option<&'static str> {
    if cfg!(windows) {
        Some("windows")
    } else if cfg!(target_os = "macos") {
        Some("macos")
    } else {
        None
    }
}

/// Architecture part of the package name.
fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" }
}

fn package_name(version: Version) -> Option<String> {
    os().map(|os| format!("playterm-{version}-{os}-{}.zip", arch()))
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// The release described by a GitHub `releases/latest` answer, if it is newer than `current`.
fn parse_latest(json: &str, current: Version) -> Result<Option<Release>, String> {
    let gh: GhRelease = serde_json::from_str(json).map_err(|e| format!("GitHub: {e}"))?;
    let Some(version) = Version::parse(&gh.tag_name) else { return Ok(None) };
    if gh.draft || gh.prerelease || version <= current {
        return Ok(None);
    }
    let url = |name: &str| gh.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    let Some(package) = package_name(version) else { return Ok(None) };
    let package_url = url(&package).ok_or_else(|| {
        tr!("{version} 릴리스에 이 PC 용 패키지({package})가 없습니다", "Release {version} has no package for this PC ({package})")
    })?;
    let (Some(sums_url), Some(sig_url)) = (url(SUMS), url(SUMS_SIG)) else {
        return Err(tr!(
            "{version} 릴리스에 서명된 체크섬({SUMS_SIG})이 없어 자동으로 설치할 수 없습니다",
            "Release {version} has no signed checksums ({SUMS_SIG}), so it cannot be installed automatically"
        ));
    };
    Ok(Some(Release {
        version,
        notes: gh.body.unwrap_or_default().replace('\r', ""),
        page: gh.html_url,
        package,
        package_url,
        sums_url,
        sig_url,
    }))
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Checks the signature of `SHA256SUMS.txt` against `public` (hex).
fn verify_sums(sums: &[u8], sig_hex: &str, public: &str) -> Result<(), String> {
    let bad = || {
        t(
            "체크섬 파일의 서명이 맞지 않습니다. 설치하지 않았습니다.",
            "The signature of the checksums does not match. Nothing was installed.",
        )
        .to_owned()
    };
    let sig = unhex(sig_hex.trim()).ok_or_else(bad)?;
    let public = unhex(public).expect("PUBLIC_KEY is hex");
    UnparsedPublicKey::new(&ED25519, public).verify(sums, &sig).map_err(|_| bad())
}

/// The SHA-256 that `sums` (lines of `<hex>  <name>`) gives for `name`.
fn expected_hash(sums: &str, name: &str) -> Option<Vec<u8>> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start();
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name).then(|| unhex(&hash.to_ascii_lowercase())).flatten().filter(|h| h.len() == 32)
    })
}

/// `playterm.exe` from the package.
fn extract_exe(package: &[u8]) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(package)).map_err(|e| format!("zip: {e}"))?;
    let file = zip.by_name(EXE).map_err(|e| format!("zip ({EXE}): {e}"))?;
    let mut exe = Vec::new();
    file.take(MAX_PACKAGE + 1).read_to_end(&mut exe).map_err(|e| format!("zip ({EXE}): {e}"))?;
    if exe.len() as u64 > MAX_PACKAGE || !exe.starts_with(b"MZ") {
        return Err(tr!("패키지의 {EXE} 가 올바르지 않습니다", "The {EXE} in the package is not valid"));
    }
    Ok(exe)
}

/// `exe` with `suffix` added to its file name.
fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Puts `new` in place of the running executable `exe`, which is moved aside to `<exe>.old`
/// (a running exe can be renamed but not overwritten). Undoes the move if that fails.
fn replace_exe(exe: &Path, new: &[u8]) -> Result<(), String> {
    let staged = sibling(exe, ".new");
    let write_err = |e: std::io::Error| {
        tr!(
            "{} 에 쓸 수 없습니다: {e}\n릴리스 페이지에서 직접 받아 주세요.",
            "Cannot write to {}: {e}\nPlease download it from the releases page.",
            exe.parent().unwrap_or(exe).display()
        )
    };
    std::fs::write(&staged, new).map_err(write_err)?;
    // An older .old may still be running (a window not restarted since the last update): then
    // it cannot be deleted, and this one gets a name of its own.
    let mut old = sibling(exe, ".old");
    if old.exists() && std::fs::remove_file(&old).is_err() {
        old = sibling(exe, &format!(".old-{}", std::process::id()));
    }
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&staged);
        return Err(write_err(e));
    }
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&staged);
        return Err(write_err(e));
    }
    Ok(())
}

/// Deletes executables left over by an earlier update (`<exe>.old*`, `<exe>.new`). Those still
/// running in another window stay until a later start.
pub fn clean_up() {
    let Ok(exe) = std::env::current_exe() else { return };
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else { return };
    let name = name.to_string_lossy().to_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().to_lowercase();
        if let Some(rest) = file.strip_prefix(&name)
            && (rest == ".new" || rest.starts_with(".old"))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// -- network --------------------------------------------------------------------------------------

fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    ureq::Agent::config_builder()
        // Windows' own TLS and certificate store (also trusts a company proxy's root).
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).root_certs(RootCerts::PlatformVerifier).build())
        .https_only(true)
        .user_agent(concat!("playterm/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn get_small(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut response = agent.get(url).header("Accept", "application/vnd.github+json").call().map_err(|e| format!("{url}: {e}"))?;
    response.body_mut().with_config().limit(MAX_SMALL).read_to_string().map_err(|e| format!("{url}: {e}"))
}

/// The latest release, if newer than the running version.
fn check() -> Result<Option<Release>, String> {
    parse_latest(&get_small(&agent(), LATEST_URL)?, Version::current())
}

/// Bytes of the package downloaded so far, and its size (0 = not known yet).
#[derive(Default)]
struct Progress {
    done: AtomicU64,
    total: AtomicU64,
}

/// Downloads, checks and installs `release`; returns the path of the new executable.
fn install(release: &Release, progress: &Progress, ctx: &egui::Context) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let agent = agent();
    let sums = get_small(&agent, &release.sums_url)?;
    verify_sums(sums.as_bytes(), &get_small(&agent, &release.sig_url)?, PUBLIC_KEY)?;
    let expected = expected_hash(&sums, &release.package)
        .ok_or_else(|| tr!("서명된 체크섬에 {} 가 없습니다", "The signed checksums do not list {}", release.package))?;

    let url = &release.package_url;
    let mut response = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    progress.total.store(response.body().content_length().unwrap_or(0), Ordering::Relaxed);
    let mut reader = response.body_mut().with_config().limit(MAX_PACKAGE).reader();
    let mut package = Vec::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("{url}: {e}"))?;
        if n == 0 {
            break;
        }
        package.extend_from_slice(&buf[..n]);
        progress.done.store(package.len() as u64, Ordering::Relaxed);
        ctx.request_repaint();
    }
    if ring::digest::digest(&ring::digest::SHA256, &package).as_ref() != expected.as_slice() {
        return Err(tr!(
            "받은 {} 의 SHA-256 이 서명된 체크섬과 다릅니다. 설치하지 않았습니다.",
            "The SHA-256 of the downloaded {} does not match the signed checksums. Nothing was installed.",
            release.package
        ));
    }
    replace_exe(&exe, &extract_exe(&package)?)?;
    Ok(exe)
}

// -- ui -------------------------------------------------------------------------------------------

enum Msg {
    Checked(Result<Option<Release>, String>),
    Installed(Result<PathBuf, String>),
}

enum Dialog {
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Version),
    Installed(Version, PathBuf),
    Failed(String),
}

/// What the app has to do after the update dialog.
pub enum Action {
    /// Don't offer this version again (saved in the settings).
    Skip(String),
    /// Start the new executable and close this window.
    Restart(PathBuf),
}

#[derive(Default)]
pub struct Updater {
    rx: Option<mpsc::Receiver<Msg>>,
    progress: Arc<Progress>,
    /// The check was asked for (Help menu): its outcome is shown even when there is nothing new.
    manual: bool,
    dialog: Option<Dialog>,
    /// A newer release, until it is installed: the menu bar offers it.
    available: Option<Release>,
}

impl Updater {
    /// Looks for a newer release in the background. `manual` shows the outcome either way.
    pub fn check(&mut self, ctx: &egui::Context, manual: bool) {
        if manual {
            self.dialog = Some(Dialog::Checking);
        }
        if self.rx.is_some() {
            self.manual |= manual;
            return;
        }
        self.manual = manual;
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Checked(check()));
            ctx.request_repaint();
        });
    }

    /// Takes in what the background thread found. `skipped` is the version the user skipped.
    pub fn poll(&mut self, skipped: &str) {
        let Some(msg) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        self.rx = None;
        match msg {
            Msg::Checked(Ok(Some(release))) => {
                let skip = !self.manual && release.version.to_string() == skipped;
                if !skip {
                    self.dialog = Some(Dialog::Available(release.clone()));
                }
                self.available = Some(release);
            }
            Msg::Checked(Ok(None)) => {
                if self.manual {
                    self.dialog = Some(Dialog::UpToDate);
                }
            }
            Msg::Checked(Err(e)) => {
                log::warn!("update check failed: {e}");
                if self.manual {
                    self.dialog = Some(Dialog::Failed(e));
                }
            }
            Msg::Installed(Ok(exe)) => {
                let version = self.available.take().map_or(Version::current(), |r| r.version);
                self.dialog = Some(Dialog::Installed(version, exe));
            }
            Msg::Installed(Err(e)) => {
                log::warn!("update failed: {e}");
                self.dialog = Some(Dialog::Failed(e));
            }
        }
        self.manual = false;
    }

    fn start_install(&mut self, ctx: &egui::Context, release: Release) {
        self.dialog = Some(Dialog::Downloading(release.version));
        self.progress = Arc::default();
        let progress = self.progress.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Installed(install(&release, &progress, &ctx)));
            ctx.request_repaint();
        });
    }

    /// A newer release offered while its dialog is closed (for the menu bar).
    pub fn pending(&self) -> Option<Version> {
        self.dialog.is_none().then(|| self.available.as_ref().map(|r| r.version)).flatten()
    }

    /// Opens the dialog of the newer release again.
    pub fn reopen(&mut self) {
        if let Some(release) = &self.available {
            self.dialog = Some(Dialog::Available(release.clone()));
        }
    }

    pub fn is_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// The update dialog, if open. `connected` is the number of connected sessions (closed by
    /// a restart).
    pub fn show(&mut self, ctx: &egui::Context, connected: usize) -> Option<Action> {
        let dialog = self.dialog.take()?;
        let mut keep = true;
        let mut action = None;
        let mut install = None;
        let close = egui::Modal::new(egui::Id::unique("update")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(t("업데이트", "Update"));
            ui.add_space(6.0);
            match &dialog {
                Dialog::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t("새 버전을 확인하는 중…", "Checking for a new version…"));
                    });
                    ui.add_space(8.0);
                    keep = !ui.add(icon::button(icon::CLOSE, t("닫기", "Close"))).clicked();
                }
                Dialog::UpToDate => {
                    ui.label(tr!("최신 버전({})을 쓰고 있습니다.", "You have the latest version ({}).", Version::current()));
                    ui.add_space(8.0);
                    keep = !ui.add(icon::primary(ui, icon::OK, t("확인", "OK"))).clicked();
                }
                Dialog::Available(release) => {
                    ui.label(tr!(
                        "playterm {} 이 나왔습니다 (지금 {}).",
                        "playterm {} is available (you have {}).",
                        release.version,
                        Version::current()
                    ));
                    if !release.notes.trim().is_empty() {
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                            ui.add(egui::Label::new(egui::RichText::new(release.notes.trim()).weak()).wrap());
                        });
                    }
                    ui.hyperlink_to(t("릴리스 페이지", "Release page"), &release.page);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if AUTO_INSTALL {
                            if ui.add(icon::primary(ui, icon::UPDATE, t("업데이트", "Update"))).clicked() {
                                install = Some(release.clone());
                            }
                        } else if ui.add(icon::button(icon::WEB_PAGE, t("다운로드 페이지 열기", "Open the download page"))).clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&release.page));
                            keep = false;
                        }
                        if ui.add(icon::button(icon::LATER, t("나중에", "Later"))).clicked() {
                            keep = false;
                        }
                        if ui.add(icon::button(icon::SKIP, t("이 버전 건너뛰기", "Skip this version"))).clicked() {
                            action = Some(Action::Skip(release.version.to_string()));
                            self.available = None;
                            keep = false;
                        }
                    });
                }
                Dialog::Downloading(version) => {
                    ui.label(tr!("playterm {version} 을 받는 중…", "Downloading playterm {version}…"));
                    let done = self.progress.done.load(Ordering::Relaxed);
                    let total = self.progress.total.load(Ordering::Relaxed);
                    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
                    let bar = if total > 0 {
                        egui::ProgressBar::new((done as f64 / total as f64) as f32).text(format!("{:.1} / {:.1} MB", mb(done), mb(total)))
                    } else {
                        egui::ProgressBar::new(0.0).text(format!("{:.1} MB", mb(done))).animate(true)
                    };
                    ui.add(bar);
                }
                Dialog::Installed(version, exe) => {
                    ui.label(tr!(
                        "playterm {version} 을 설치했습니다. 다시 시작하면 적용됩니다.",
                        "playterm {version} is installed. It takes effect when playterm restarts."
                    ));
                    if connected > 0 {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            tr!(
                                "지금 다시 시작하면 연결된 세션 {connected}개가 끊어집니다.",
                                "Restarting now closes {connected} connected session(s)."
                            ),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(icon::primary(ui, icon::RESTART, t("지금 다시 시작", "Restart now"))).clicked() {
                            action = Some(Action::Restart(exe.clone()));
                            keep = false;
                        }
                        if ui.add(icon::button(icon::LATER, t("나중에", "Later"))).clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Failed(error) => {
                    ui.label(t("업데이트하지 못했습니다:", "The update failed:"));
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    ui.hyperlink_to(t("릴리스 페이지", "Releases page"), RELEASES_PAGE);
                    ui.add_space(8.0);
                    keep = !ui.add(icon::primary(ui, icon::OK, t("확인", "OK"))).clicked();
                }
            }
        });
        if let Some(release) = install {
            self.start_install(ctx, release);
            return action;
        }
        // Escape / click outside closes, except while the download runs.
        let busy = matches!(dialog, Dialog::Downloading(_));
        if keep && (busy || !close.should_close()) {
            self.dialog = Some(dialog);
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    #[test]
    fn versions() {
        assert_eq!(Version::parse("v0.2.0"), Some(Version(0, 2, 0)));
        assert_eq!(Version::parse("1.10.3"), Some(Version(1, 10, 3)));
        assert!(Version(1, 10, 3) > Version(1, 9, 9));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("1.2.3-beta"), None);
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
        assert_eq!(unhex(PUBLIC_KEY).map(|k| k.len()), Some(32));
    }

    fn latest_json(tag: &str, assets: &[&str]) -> String {
        let assets: Vec<String> =
            assets.iter().map(|n| format!(r#"{{"name":"{n}","browser_download_url":"https://example.com/{n}"}}"#)).collect();
        format!(r#"{{"tag_name":"{tag}","html_url":"https://example.com/r","body":"notes\r\nline","assets":[{}]}}"#, assets.join(","))
    }

    #[test]
    fn latest_release() {
        let current = Version(0, 2, 0);
        let Some(os) = os() else {
            // No package for this system: a newer release is not offered.
            let any = ["windows", "macos"].map(|os| format!("playterm-0.3.0-{os}-{}.zip", arch()));
            let json = latest_json("v0.3.0", &[&any[0], &any[1], SUMS, SUMS_SIG]);
            assert_eq!(parse_latest(&json, current).unwrap(), None);
            return;
        };
        let package = format!("playterm-0.3.0-{os}-{}.zip", arch());
        let json = latest_json("v0.3.0", &[&package, SUMS, SUMS_SIG]);
        let release = parse_latest(&json, current).unwrap().unwrap();
        assert_eq!(release.version, Version(0, 3, 0));
        assert_eq!(release.package, package);
        assert_eq!(release.package_url, format!("https://example.com/{package}"));
        assert_eq!(release.notes, "notes\nline");
        // Not newer.
        assert_eq!(parse_latest(&latest_json("v0.2.0", &[&package, SUMS, SUMS_SIG]), current).unwrap(), None);
        // A package for another system is not one for this PC.
        let other = if os == "macos" { "windows" } else { "macos" };
        let elsewhere = format!("playterm-0.3.0-{other}-{}.zip", arch());
        assert!(parse_latest(&latest_json("v0.3.0", &[&elsewhere, SUMS, SUMS_SIG]), current).is_err());
        // Unsigned, or no package for this architecture.
        assert!(parse_latest(&latest_json("v0.3.0", &[&package, SUMS]), current).is_err());
        assert!(parse_latest(&latest_json("v0.3.0", &[SUMS, SUMS_SIG]), current).is_err());
    }

    #[test]
    fn signed_checksums() {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let hex = |b: &[u8]| b.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let public = hex(pair.public_key().as_ref());
        let hash = "ab".repeat(32);
        let sums = format!("{hash}  playterm-0.3.0-windows-x64.zip\r\n{}  other.zip\r\n", "cd".repeat(32));
        let sig = hex(pair.sign(sums.as_bytes()).as_ref());
        assert!(verify_sums(sums.as_bytes(), &format!("{sig}\n"), &public).is_ok());
        assert!(verify_sums(sums.replace("0.3.0", "0.4.0").as_bytes(), &sig, &public).is_err());
        assert!(verify_sums(sums.as_bytes(), &sig, PUBLIC_KEY).is_err());
        assert_eq!(expected_hash(&sums, "playterm-0.3.0-windows-x64.zip"), unhex(&hash));
        assert_eq!(expected_hash(&sums, "missing.zip"), None);
    }

    /// Asks GitHub for real (network): `cargo test -p playterm live_check -- --ignored`.
    #[test]
    #[ignore]
    fn live_check() {
        let latest = parse_latest(&get_small(&agent(), LATEST_URL).unwrap(), Version(0, 0, 1));
        eprintln!("{latest:?}");
        assert!(matches!(latest, Ok(Some(_)) | Err(_)), "any release is newer than 0.0.1");
    }

    #[test]
    fn package_and_replace() {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(EXE, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"MZ new").unwrap();
        zip.start_file("README.md", zip::write::SimpleFileOptions::default()).unwrap();
        let package = zip.finish().unwrap().into_inner();
        let exe = extract_exe(&package).unwrap();
        assert_eq!(exe, b"MZ new");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EXE);
        std::fs::write(&path, b"MZ old").unwrap();
        replace_exe(&path, &exe).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"MZ new");
        assert_eq!(std::fs::read(sibling(&path, ".old")).unwrap(), b"MZ old");
        assert!(!sibling(&path, ".new").exists());
    }
}
