//! Names of things that differ between Windows, macOS and other systems, for the UI text.

use playterm_i18n::t;

/// The system's file manager: Explorer, Finder, or a generic name.
pub fn file_manager() -> &'static str {
    if cfg!(windows) {
        t("탐색기", "Explorer")
    } else if cfg!(target_os = "macos") {
        "Finder"
    } else {
        t("파일 관리자", "the file manager")
    }
}

/// The SSH agents playterm talks to here.
pub fn ssh_agents() -> &'static str {
    if cfg!(windows) { "Pageant / Windows OpenSSH" } else { "ssh-agent" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fit_the_platform() {
        assert!(!file_manager().is_empty());
        if cfg!(target_os = "macos") {
            assert_eq!(file_manager(), "Finder");
            assert!(!ssh_agents().contains("Windows") && !ssh_agents().contains("Pageant"));
        }
        if cfg!(windows) {
            assert!(ssh_agents().contains("Pageant"));
        }
    }
}
