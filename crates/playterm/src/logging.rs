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

/// Shows a file selected in Explorer, or opens a folder.
pub fn reveal(path: &Path) {
    let mut cmd = std::process::Command::new("explorer");
    if path.is_file() {
        cmd.arg(format!("/select,{}", path.display()));
    } else {
        let _ = std::fs::create_dir_all(path);
        cmd.arg(path);
    }
    if let Err(e) = cmd.spawn() {
        log::warn!("explorer failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
