//! Where session logs go and how they are named.

use std::path::{Path, PathBuf};

use playterm_term::LogMode;

use crate::profile_form::expand_home;

/// How a tab should log, decided when it is opened.
#[derive(Clone, Debug)]
pub struct LogPlan {
    pub dir: PathBuf,
    pub mode: LogMode,
}

/// `Documents\playterm-logs`, or a folder next to the config when there is no Documents.
pub fn default_log_dir() -> PathBuf {
    if let Some(docs) = directories::UserDirs::new().and_then(|d| d.document_dir().map(Path::to_owned)) {
        return docs.join("playterm-logs");
    }
    directories::ProjectDirs::from("", "", "playterm")
        .map(|d| d.data_dir().join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("playterm-logs"))
}

/// The configured folder, or the default one when empty.
pub fn log_dir(configured: &str) -> PathBuf {
    if configured.trim().is_empty() { default_log_dir() } else { expand_home(configured.trim()) }
}

/// Characters Windows does not allow in file names become `_`.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_owned();
    if cleaned.is_empty() { "session".to_owned() } else { cleaned }
}

/// `title_YYYYMMDD_HHMMSS.log`, without the folder.
pub fn file_name(title: &str) -> String {
    format!("{}_{}.log", sanitize(title), chrono::Local::now().format("%Y%m%d_%H%M%S"))
}

/// A new log file path in `dir` that does not exist yet.
pub fn new_log_path(dir: &Path, title: &str) -> PathBuf {
    let name = file_name(title);
    let mut path = dir.join(&name);
    let stem = name.trim_end_matches(".log").to_owned();
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}_{n}.log"));
        n += 1;
    }
    path
}

/// Shows a file selected in the file manager (Explorer, Finder), or opens a folder.
pub fn reveal(path: &Path) {
    if !path.is_file() {
        let _ = std::fs::create_dir_all(path);
    }
    let (program, args) = reveal_command(path, path.is_file());
    if let Err(e) = std::process::Command::new(program).args(&args).spawn() {
        log::warn!("{program} failed: {e}");
    }
}

/// The command that shows `path` in the file manager (`file`: select it in its folder).
fn reveal_command(path: &Path, file: bool) -> (&'static str, Vec<std::ffi::OsString>) {
    if cfg!(windows) {
        let arg = if file { format!("/select,{}", path.display()).into() } else { path.as_os_str().to_owned() };
        ("explorer", vec![arg])
    } else if cfg!(target_os = "macos") {
        // Always reveal (-R: select in Finder), never open: a folder named like a bundle
        // (`x.app`, e.g. downloaded over SFTP) would otherwise be launched, not shown.
        let _ = file;
        ("open", vec!["-R".into(), path.as_os_str().to_owned()])
    } else {
        // xdg-open cannot select a file: open its folder.
        let dir = if file { path.parent().unwrap_or(path) } else { path };
        ("xdg-open", vec![dir.as_os_str().to_owned()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_uses_the_platform_file_manager() {
        let file = Path::new("/tmp/x/a.log");
        let (program, args) = reveal_command(file, true);
        if cfg!(target_os = "macos") {
            assert_eq!((program, args), ("open", vec!["-R".into(), file.as_os_str().to_owned()]));
            // A folder is revealed too, not opened: `open x.app` would launch it.
            let bundle = Path::new("/tmp/x/evil.app");
            assert_eq!(reveal_command(bundle, false).1, vec!["-R".into(), bundle.as_os_str().to_owned()]);
        } else if cfg!(windows) {
            assert_eq!(program, "explorer");
            assert!(args[0].to_string_lossy().starts_with("/select,"));
        } else {
            assert_eq!((program, args), ("xdg-open", vec!["/tmp/x".into()]));
        }
    }

    #[test]
    fn file_names_are_safe_and_unique() {
        assert_eq!(sanitize(r#"root@db:22 <prod>"#), "root@db_22 _prod_");
        assert_eq!(sanitize("  ..  "), "session");
        assert!(file_name("COM3").starts_with("COM3_") && file_name("x").ends_with(".log"));

        let dir = tempfile::tempdir().unwrap();
        let first = new_log_path(dir.path(), "a/b");
        std::fs::write(&first, "").unwrap();
        let second = new_log_path(dir.path(), "a/b");
        assert_ne!(first, second);
        assert!(second.file_name().unwrap().to_string_lossy().starts_with("a_b_"));
    }
}
