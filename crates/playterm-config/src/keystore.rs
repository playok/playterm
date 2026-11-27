//! Where the vault's key is kept, so saved passwords open without a master password: the
//! OS keychain (macOS Keychain, Windows Credential Manager, the Secret Service on Linux), or a
//! private file next to the vault where there is none.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use zeroize::Zeroizing;

use crate::fsutil::write_private_atomic;

/// Keeps the vault keys (base64 text), each under the id of its vault.
pub trait KeyStore: Send + Sync {
    fn load(&self, id: &str) -> io::Result<Option<Zeroizing<String>>>;
    fn store(&self, id: &str, key: &str) -> io::Result<()>;
    fn delete(&self, id: &str) -> io::Result<()>;
    /// Whether a key was stored in a plain private file because there is no keychain here.
    fn used_file_fallback(&self) -> bool {
        false
    }
}

/// Keys as private files (`vault-<id>.key`, mode 0600) in a folder: the fallback without a
/// keychain, and the store the tests use.
pub struct FileKeyStore {
    dir: PathBuf,
}

impl FileKeyStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, id: &str) -> io::Result<PathBuf> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid vault id"));
        }
        Ok(self.dir.join(format!("vault-{id}.key")))
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self, id: &str) -> io::Result<Option<Zeroizing<String>>> {
        match std::fs::read_to_string(self.path(id)?) {
            Ok(text) => Ok(Some(Zeroizing::new(text.trim().to_owned()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn store(&self, id: &str, key: &str) -> io::Result<()> {
        write_private_atomic(&self.path(id)?, key.as_bytes())
    }

    fn delete(&self, id: &str) -> io::Result<()> {
        match std::fs::remove_file(self.path(id)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// The OS keychain. Only where a desktop may have none (Linux and other Unix without a Secret
/// Service) are new keys kept in a [`FileKeyStore`] in `fallback_dir` instead, and that is
/// reported ([`KeyStore::used_file_fallback`]). On macOS and Windows a keychain that cannot
/// store the key (access denied, say) is an error: the key is never quietly left in a file.
/// Keys already in such a file are still read (a vault set up by a test fixture).
pub struct OsKeyStore {
    fallback: FileKeyStore,
    used_fallback: AtomicBool,
}

/// Whether this system may lack a keychain, so a file stands in for it.
const FILE_FALLBACK: bool = cfg!(all(unix, not(target_os = "macos")));

const SERVICE: &str = "playterm";

impl OsKeyStore {
    pub fn new(fallback_dir: &Path) -> Self {
        Self { fallback: FileKeyStore::new(fallback_dir), used_fallback: AtomicBool::new(false) }
    }

    fn entry(id: &str) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, &format!("vault-{id}"))
    }
}

fn other(e: keyring::Error) -> io::Error {
    io::Error::other(e.to_string())
}

impl KeyStore for OsKeyStore {
    fn load(&self, id: &str) -> io::Result<Option<Zeroizing<String>>> {
        let keychain = Self::entry(id).and_then(|entry| entry.get_password());
        match keychain {
            Ok(key) => Ok(Some(Zeroizing::new(key))),
            Err(keyring::Error::NoEntry) => self.fallback.load(id),
            // No keychain here: the key may be in the fallback file.
            Err(e) => match self.fallback.load(id)? {
                Some(key) => Ok(Some(key)),
                None => Err(other(e)),
            },
        }
    }

    fn store(&self, id: &str, key: &str) -> io::Result<()> {
        match Self::entry(id).and_then(|entry| entry.set_password(key)) {
            Ok(()) => Ok(()),
            Err(e @ (keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_))) if FILE_FALLBACK => {
                log::warn!("no keychain ({e}); keeping the vault key in a private file");
                self.fallback.store(id, key)?;
                self.used_fallback.store(true, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(io::Error::other(format!("keychain: {e}"))),
        }
    }

    fn used_file_fallback(&self) -> bool {
        self.used_fallback.load(Ordering::Relaxed)
    }

    fn delete(&self, id: &str) -> io::Result<()> {
        match Self::entry(id).and_then(|entry| entry.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => log::warn!("could not delete the vault key from the keychain: {e}"),
        }
        self.fallback.delete(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trip_and_rejects_odd_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(dir.path());
        assert!(store.load("abc123").unwrap().is_none());
        store.store("abc123", "a2V5").unwrap();
        assert_eq!(store.load("abc123").unwrap().as_deref().map(String::as_str), Some("a2V5"));
        store.delete("abc123").unwrap();
        store.delete("abc123").unwrap();
        assert!(store.load("abc123").unwrap().is_none());
        assert!(store.store("../x", "k").is_err());
    }

    /// Uses the real keychain of this computer (a throwaway entry, deleted again).
    #[test]
    #[ignore = "touches the OS keychain"]
    fn os_keychain_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = OsKeyStore::new(dir.path());
        let id = "0123456789abcdef0123456789abcdef";
        store.store(id, "dGVzdC1rZXk=").unwrap();
        assert_eq!(store.load(id).unwrap().as_deref().map(String::as_str), Some("dGVzdC1rZXk="));
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none(), "kept in the keychain, not a file");
        store.delete(id).unwrap();
        assert!(store.load(id).unwrap().is_none());
    }
}
