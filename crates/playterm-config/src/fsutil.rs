use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// Locks a stable sibling across a read/modify/atomic-replace transaction. Never unlink
/// the lock file: a second process may already have opened the same inode.
pub(crate) fn lock_config(path: &Path) -> io::Result<std::fs::File> {
    use std::time::{Duration, Instant};
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(name)?;
    let deadline = Instant::now() + Duration::from_millis(750);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    playterm_i18n::t(
                        "다른 창에서 설정을 사용 중입니다. 다시 시도하세요",
                        "The configuration is busy in another window. Try again",
                    ),
                ));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// Writes `contents` to `path` via a temporary file and rename, so a crash, power loss or full
/// disk never leaves a half-written config behind. The data is flushed to disk before the
/// rename, and each write has its own temporary file (two windows may save at once).
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_atomic_inner(path, contents, false)
}

/// Restricts vault data and the temporary file to its owner on Unix.
pub fn write_private_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_atomic_inner(path, contents, true)
}

fn open_temporary(path: &Path, private: bool) -> io::Result<(std::fs::File, PathBuf)> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", random_id()?));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(&tmp)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(e) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok((file, tmp.into()))
}

fn write_atomic_inner(path: &Path, contents: &[u8], private: bool) -> io::Result<()> {
    let (mut file, tmp) = open_temporary(path, private)?;
    let result = (|| {
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// 16 random hex characters, used as a stable profile id.
pub fn random_id() -> io::Result<String> {
    #[cfg(test)]
    if FAIL_ID_ENTROPY.get() {
        return Err(io::Error::other("injected OS entropy failure"));
    }
    random_id_with(|bytes| getrandom::fill(bytes).map_err(|e| io::Error::other(format!("OS random number generator unavailable: {e}"))))
}

#[cfg(test)]
thread_local! { static FAIL_ID_ENTROPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[cfg(test)]
pub(crate) fn with_id_entropy_failure(check: impl FnOnce()) {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAIL_ID_ENTROPY.set(self.0);
        }
    }
    let _restore = Restore(FAIL_ID_ENTROPY.replace(true));
    check();
}

fn random_id_with(fill: impl FnOnce(&mut [u8]) -> io::Result<()>) -> io::Result<String> {
    let mut bytes = [0u8; 8];
    fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_failure_is_an_error_without_an_id_or_panic() {
        let result = random_id_with(|_| Err(io::Error::other("injected entropy failure")));
        assert_eq!(result.unwrap_err().to_string(), "injected entropy failure");
    }

    #[cfg(unix)]
    #[test]
    fn private_temporary_and_replacement_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.toml");
        let (file, temporary) = open_temporary(&path, true).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        drop(file);
        std::fs::remove_file(temporary).unwrap();
        std::fs::write(&path, "old encrypted data").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        write_private_atomic(&path, b"new encrypted data").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(path).unwrap(), b"new encrypted data");
    }
}
