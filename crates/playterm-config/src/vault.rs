//! Password vault: saved site passwords, encrypted with a random key kept in the OS keychain
//! (see [`crate::keystore`]), so connecting never asks for a master password.
//!
//! - Cipher: XChaCha20-Poly1305 with a fresh random nonce on every save
//! - The file header (format, the vault's id) is bound as associated data.
//! - Each secret records the target it was saved for (`ssh://user@host:port`), inside the
//!   encrypted part: the site list is plain text, and a password must not follow a site that
//!   was pointed somewhere else.
//! - Every change re-reads the file first and applies just that change, so two playterm
//!   windows do not undo each other's saved passwords.
//! - Vaults of older versions are encrypted with a key derived from a master password
//!   (Argon2id). [`Vault::migrate`] opens one with it once and rewrites it with a keychain key.
//! - A master password now only protects exported site lists: [`seal`] and [`unseal`].

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use playterm_i18n::{t, tr};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::fsutil::write_private_atomic;
use crate::keystore::{FileKeyStore, KeyStore};

/// Vaults encrypted with a master password (older versions).
const MASTER_VERSION: u32 = 1;
const KDF_NAME: &str = "argon2id";
/// Vaults encrypted with a key from the key store.
const LOCAL_VERSION: u32 = 2;
const LOCAL_KDF: &str = "keystore";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
/// Refuse absurd KDF costs from a (possibly tampered) file: 1 GiB, 16 passes.
const MAX_M_COST_KIB: u32 = 1024 * 1024;
const MAX_T_COST: u32 = 16;

/// Lock a stable sibling, never the data file which atomic saves replace. OS locks are
/// released on process exit; the lock file itself must never be unlinked.
fn lock_file(path: &Path) -> Result<std::fs::File, VaultError> {
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
                return Err(io::Error::new(io::ErrorKind::TimedOut, t!("vault.the_vault_is_busy_in_another_window_try")).into());
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

#[derive(Debug)]
pub enum VaultError {
    Cancelled,
    WrongPassword,
    Locked,
    AlreadyExists,
    Missing,
    EmptyPassword,
    /// The key of the vault is not in the keychain (or does not open it).
    KeyMissing,
    Format(String),
    /// The file was replaced (another window reset and recreated the vault) since unlocking.
    Replaced,
    Io(io::Error),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Cancelled => f.write_str(t!("vault.vault_operation_cancelled")),
            VaultError::WrongPassword => f.write_str(t!("vault.wrong_master_password")),
            VaultError::Locked => f.write_str(t!("vault.the_password_vault_is_locked")),
            VaultError::AlreadyExists => f.write_str(t!("vault.the_password_vault_already_exists")),
            VaultError::Missing => f.write_str(t!("vault.the_password_vault_file_does_not_exist")),
            VaultError::EmptyPassword => f.write_str(t!("vault.the_master_password_cannot_be_empty")),
            VaultError::KeyMissing => f.write_str(t!("vault.the_key_of_the_saved_passwords_is_not")),
            VaultError::Replaced => f.write_str(t!("vault.the_password_vault_was_recreated_in_another_window")),
            VaultError::Format(detail) => f.write_str(&tr!("vault.invalid_password_vault_file", detail = detail)),
            // Transparent, like the inner error.
            VaultError::Io(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for VaultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VaultError::Io(e) => e.source(),
            _ => None,
        }
    }
}

impl From<io::Error> for VaultError {
    fn from(e: io::Error) -> Self {
        VaultError::Io(e)
    }
}

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    kdf: String,
    #[serde(default)]
    m_cost: u32,
    #[serde(default)]
    t_cost: u32,
    #[serde(default)]
    p_cost: u32,
    /// The Argon2 salt; for a key-store vault, its id.
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize)]
struct ContentsRef<'a> {
    secrets: BTreeMap<&'a str, &'a str>,
    targets: &'a BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Contents {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    /// Secret id -> what it was saved for (absent in files from older versions).
    #[serde(default)]
    targets: BTreeMap<String, String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Kdf {
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}

impl Kdf {
    const DEFAULT: Kdf = Kdf { m_cost: 64 * 1024, t_cost: 3, p_cost: 1 };

    /// The costs read from a file, unless absurd.
    fn checked(m_cost: u32, t_cost: u32, p_cost: u32) -> Result<Self, VaultError> {
        if m_cost > MAX_M_COST_KIB || t_cost > MAX_T_COST {
            return Err(VaultError::Format(t!("vault.the_kdf_cost_is_abnormally_high").into()));
        }
        Ok(Kdf { m_cost, t_cost, p_cost })
    }

    fn derive(&self, master: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
        let params = Params::new(self.m_cost, self.t_cost, self.p_cost, Some(KEY_LEN))
            .map_err(|e| VaultError::Format(tr!("vault.kdf_parameters", e = e)))?;
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(master.as_bytes(), salt, key.as_mut())
            .map_err(|e| VaultError::Format(format!("KDF: {e}")))?;
        Ok(key)
    }
}

/// How a vault file's key is had.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Protection {
    /// Derived from a master password (older versions).
    Master(Kdf),
    /// Kept in the key store.
    Local,
}

impl Protection {
    fn of(file: &VaultFile) -> Result<Self, VaultError> {
        match (file.version, file.kdf.as_str()) {
            (MASTER_VERSION, KDF_NAME) => Ok(Protection::Master(Kdf::checked(file.m_cost, file.t_cost, file.p_cost)?)),
            (LOCAL_VERSION, LOCAL_KDF) => Ok(Protection::Local),
            (version, kdf) => Err(VaultError::Format(tr!("vault.unsupported_version_v", arg0 = version, arg1 = kdf))),
        }
    }

    fn aad(&self, salt: &[u8]) -> Vec<u8> {
        match self {
            Protection::Master(k) => {
                format!("playterm-vault:v{MASTER_VERSION}:{KDF_NAME}:{}:{}:{}:{}", k.m_cost, k.t_cost, k.p_cost, B64.encode(salt))
            }
            Protection::Local => format!("playterm-vault:v{LOCAL_VERSION}:{LOCAL_KDF}:{}", B64.encode(salt)),
        }
        .into_bytes()
    }
}

/// What kind of vault file there is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VaultKind {
    /// No vault yet: nothing saved.
    Missing,
    /// Encrypted with a master password by an older version: [`Vault::migrate`] it once.
    Master,
    /// Opens with the key in the key store ([`Vault::open`]).
    Local,
}

/// The key store id of the vault with this salt.
fn key_id(salt: &[u8]) -> String {
    salt.iter().map(|b| format!("{b:02x}")).collect()
}

fn random<const N: usize>() -> Result<[u8; N], VaultError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes)
}

#[derive(Clone)]
struct Unlocked {
    key: Zeroizing<[u8; KEY_LEN]>,
    protection: Protection,
    salt: [u8; SALT_LEN],
    secrets: BTreeMap<String, Zeroizing<String>>,
    targets: BTreeMap<String, String>,
}

impl Unlocked {
    /// The secrets in the vault file as it is now, decrypted with this key.
    fn read(&self, path: &Path) -> Result<Decrypted, VaultError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            // A reset in another process must not resurrect old credentials.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(VaultError::Replaced),
            Err(e) => return Err(e.into()),
        };
        let file: VaultFile = toml::from_str(&text).map_err(|e| VaultError::Format(e.to_string()))?;
        let salt: [u8; SALT_LEN] = decode_fixed(&file.salt, "salt")?;
        // Migrated or recreated in another window since.
        if salt != self.salt || Protection::of(&file).ok() != Some(self.protection) {
            return Err(VaultError::Replaced);
        }
        decrypt(&self.key, self.protection, &salt, &file).map_err(|_| VaultError::Replaced)
    }
}

/// Secrets and their targets, as decrypted from the file.
type Decrypted = (BTreeMap<String, Zeroizing<String>>, BTreeMap<String, String>);

/// Decrypts the contents of `file` (whose salt is `salt`) with `key`.
fn decrypt(key: &[u8; KEY_LEN], protection: Protection, salt: &[u8], file: &VaultFile) -> Result<Decrypted, VaultError> {
    let nonce: [u8; NONCE_LEN] = decode_fixed(&file.nonce, "nonce")?;
    let ciphertext = B64.decode(&file.ciphertext).map_err(|e| VaultError::Format(e.to_string()))?;
    let cipher = XChaCha20Poly1305::new(&Key::from(*key));
    let aad = protection.aad(salt);
    let plaintext = Zeroizing::new(
        cipher.decrypt(&XNonce::from(nonce), Payload { msg: &ciphertext, aad: &aad }).map_err(|_| VaultError::WrongPassword)?,
    );
    let text = std::str::from_utf8(&plaintext).map_err(|e| VaultError::Format(e.to_string()))?;
    let contents: Contents = toml::from_str(text).map_err(|e| VaultError::Format(e.to_string()))?;
    let secrets = contents.secrets.into_iter().map(|(k, v)| (k, Zeroizing::new(v))).collect();
    Ok((secrets, contents.targets))
}

/// The `vault.toml` file. Starts closed; secrets are only available after [`Vault::open`],
/// [`Vault::create`] or [`Vault::migrate`].
pub struct Vault {
    path: PathBuf,
    keys: Arc<dyn KeyStore>,
    unlocked: Option<Unlocked>,
}

impl Vault {
    /// A vault whose key is kept in a private file next to it (tests, and the fallback).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        Self::with_keys(path, Arc::new(FileKeyStore::new(dir)))
    }

    /// A vault whose key is kept in `keys` (the OS keychain in the app).
    pub fn with_keys(path: impl Into<PathBuf>, keys: Arc<dyn KeyStore>) -> Self {
        Self { path: path.into(), keys, unlocked: None }
    }

    /// The same file and key store, closed (for a computation on another thread).
    pub fn reopen(&self) -> Self {
        Self { path: self.path.clone(), keys: self.keys.clone(), unlocked: None }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn is_unlocked(&self) -> bool {
        self.unlocked.is_some()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the key had to be kept in a plain private file (no keychain on this system).
    pub fn key_in_file(&self) -> bool {
        self.keys.used_file_fallback()
    }

    /// What kind of vault the file is (cheap: no key is derived or loaded).
    pub fn kind(&self) -> Result<VaultKind, VaultError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(VaultKind::Missing),
            Err(e) => return Err(e.into()),
        };
        let file: VaultFile = toml::from_str(&text).map_err(|e| VaultError::Format(e.to_string()))?;
        Ok(match Protection::of(&file)? {
            Protection::Master(_) => VaultKind::Master,
            Protection::Local => VaultKind::Local,
        })
    }

    /// Creates a new, empty vault with a new key in the key store, and leaves it open.
    pub fn create(&mut self) -> Result<(), VaultError> {
        let _lock = lock_file(&self.path)?;
        if self.exists() {
            return Err(VaultError::AlreadyExists);
        }
        let unlocked = Unlocked {
            key: Zeroizing::new(random()?),
            protection: Protection::Local,
            salt: random()?,
            secrets: BTreeMap::new(),
            targets: BTreeMap::new(),
        };
        self.commit_local(&unlocked)?;
        self.unlocked = Some(unlocked);
        Ok(())
    }

    /// Opens a key-store vault with its key.
    pub fn open(&mut self) -> Result<(), VaultError> {
        let _lock = lock_file(&self.path)?;
        let file = self.read_file()?;
        if Protection::of(&file)? != Protection::Local {
            return Err(VaultError::Locked);
        }
        let salt: [u8; SALT_LEN] = decode_fixed(&file.salt, "salt")?;
        let stored = self.keys.load(&key_id(&salt))?.ok_or(VaultError::KeyMissing)?;
        let key: Zeroizing<[u8; KEY_LEN]> = Zeroizing::new(decode_fixed(&stored, "key").map_err(|_| VaultError::KeyMissing)?);
        let (secrets, targets) = decrypt(&key, Protection::Local, &salt, &file).map_err(|e| match e {
            VaultError::WrongPassword => VaultError::KeyMissing,
            e => e,
        })?;
        self.unlocked = Some(Unlocked { key, protection: Protection::Local, salt, secrets, targets });
        Ok(())
    }

    /// Opens a vault of an older version with its master password and rewrites it with a new
    /// key in the key store, so it is not asked for again.
    pub fn migrate(&mut self, master: &str) -> Result<(), VaultError> {
        self.migrate_cancellable(master, || true)
    }

    /// Checks permission to commit after the expensive derivation, before persisting anything.
    /// Returning false leaves both the data file and this vault's state unchanged.
    pub fn migrate_cancellable(&mut self, master: &str, commit: impl FnOnce() -> bool) -> Result<(), VaultError> {
        if master.is_empty() {
            return Err(VaultError::EmptyPassword);
        }
        let _lock = lock_file(&self.path)?;
        let file = self.read_file()?;
        let Protection::Master(kdf) = Protection::of(&file)? else {
            // Already migrated (in another window): it opens without the password.
            drop(_lock);
            return if commit() { self.open() } else { Err(VaultError::Cancelled) };
        };
        let salt: [u8; SALT_LEN] = decode_fixed(&file.salt, "salt")?;
        let key = kdf.derive(master, &salt)?;
        let (secrets, targets) = decrypt(&key, Protection::Master(kdf), &salt, &file)?;
        let unlocked = Unlocked { key: Zeroizing::new(random()?), protection: Protection::Local, salt: random()?, secrets, targets };
        if !commit() {
            return Err(VaultError::Cancelled);
        }
        self.commit_local(&unlocked)?;
        self.unlocked = Some(unlocked);
        Ok(())
    }

    /// Stores the key of a new key-store vault, then the file; the key goes again if the file
    /// cannot be written. Caller holds the lock.
    fn commit_local(&self, unlocked: &Unlocked) -> Result<(), VaultError> {
        let id = key_id(&unlocked.salt);
        self.keys.store(&id, &Zeroizing::new(B64.encode(*unlocked.key)))?;
        if let Err(e) = self.save_unlocked(unlocked) {
            let _ = self.keys.delete(&id);
            return Err(e);
        }
        Ok(())
    }

    fn read_file(&self) -> Result<VaultFile, VaultError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(VaultError::Missing),
            Err(e) => return Err(e.into()),
        };
        toml::from_str(&text).map_err(|e| VaultError::Format(e.to_string()))
    }

    /// Forgets the key and all decrypted secrets (they are zeroed on drop).
    pub fn lock(&mut self) {
        self.unlocked = None;
    }

    pub fn get(&self, id: &str) -> Option<&str> {
        self.unlocked.as_ref()?.secrets.get(id).map(|s| s.as_str())
    }

    /// What the secret `id` was saved for (see [`Vault::set`]); `None` for secrets saved by
    /// older versions until [`Vault::bind_missing`] records it.
    pub fn target(&self, id: &str) -> Option<&str> {
        self.unlocked.as_ref()?.targets.get(id).map(String::as_str)
    }

    /// Stores `secret` for `id`, to be used with `target` (e.g. `ssh://user@host:22`) only.
    pub fn set(&mut self, id: &str, target: &str, secret: &str) -> Result<(), VaultError> {
        self.update(|u| {
            u.secrets.insert(id.to_owned(), Zeroizing::new(secret.to_owned()));
            u.targets.insert(id.to_owned(), target.to_owned());
            true
        })
    }

    /// Moves the secret `id` to a new target (the user changed the site in playterm).
    pub fn rebind(&mut self, id: &str, target: &str) -> Result<(), VaultError> {
        self.update(|u| {
            let has = u.secrets.contains_key(id);
            if has {
                u.targets.insert(id.to_owned(), target.to_owned());
            }
            has
        })
    }

    pub fn remove(&mut self, id: &str) -> Result<(), VaultError> {
        self.update(|u| {
            u.targets.remove(id);
            u.secrets.remove(id).is_some()
        })
    }

    /// Records a target for secrets saved by older versions, which have none: `target_of` gives
    /// the site's current one (or `None` if the site is gone).
    pub fn bind_missing(&mut self, target_of: impl Fn(&str) -> Option<String>) -> Result<(), VaultError> {
        self.update(|u| {
            let missing: Vec<(String, String)> =
                u.secrets.keys().filter(|id| !u.targets.contains_key(*id)).filter_map(|id| Some((id.clone(), target_of(id)?))).collect();
            let changed = !missing.is_empty();
            u.targets.extend(missing);
            changed
        })
    }

    /// Applies `change` to the vault as it is on disk now (another window may have saved since)
    /// and writes it if `change` returns true.
    fn update(&mut self, change: impl FnOnce(&mut Unlocked) -> bool) -> Result<(), VaultError> {
        let mut unlocked = self.unlocked.as_ref().ok_or(VaultError::Locked)?.clone();
        let _lock = lock_file(&self.path)?;
        let (secrets, targets) = unlocked.read(&self.path)?;
        unlocked.secrets = secrets;
        unlocked.targets = targets;
        if change(&mut unlocked) {
            self.save_unlocked(&unlocked)?;
        }
        self.unlocked = Some(unlocked);
        Ok(())
    }

    /// Starts over without the stored secrets (the master password of an older vault is lost).
    /// The old file is kept as `vault.toml.bak` (replacing an older one), not deleted: a file
    /// that only looked like a wrong password (damaged by a crash, say) can still be recovered.
    pub fn reset(&mut self) -> Result<(), VaultError> {
        let _lock = lock_file(&self.path)?;
        self.lock();
        let mut backup = self.path.as_os_str().to_owned();
        backup.push(".bak");
        match std::fs::rename(&self.path, PathBuf::from(backup)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Caller holds the sibling lock across the entire read/modify/write transaction.
    fn save_unlocked(&self, unlocked: &Unlocked) -> Result<(), VaultError> {
        let contents =
            ContentsRef { secrets: unlocked.secrets.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(), targets: &unlocked.targets };
        let plaintext = Zeroizing::new(toml::to_string(&contents).map_err(|e| VaultError::Format(e.to_string()))?);
        let (nonce, ciphertext) = encrypt(&unlocked.key, &unlocked.protection.aad(&unlocked.salt), plaintext.as_bytes())?;
        let (version, kdf, cost) = match unlocked.protection {
            Protection::Master(k) => (MASTER_VERSION, KDF_NAME, k),
            Protection::Local => (LOCAL_VERSION, LOCAL_KDF, Kdf { m_cost: 0, t_cost: 0, p_cost: 0 }),
        };
        let file = VaultFile {
            version,
            kdf: kdf.into(),
            m_cost: cost.m_cost,
            t_cost: cost.t_cost,
            p_cost: cost.p_cost,
            salt: B64.encode(unlocked.salt),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ciphertext),
        };
        let text = toml::to_string_pretty(&file).map_err(|e| VaultError::Format(e.to_string()))?;
        write_private_atomic(&self.path, text.as_bytes())?;
        Ok(())
    }
}

/// Encrypts `plaintext` with a fresh random nonce.
fn encrypt(key: &[u8; KEY_LEN], aad: &[u8], plaintext: &[u8]) -> Result<([u8; NONCE_LEN], Vec<u8>), VaultError> {
    let nonce: [u8; NONCE_LEN] = random()?;
    let cipher = XChaCha20Poly1305::new(&Key::from(*key));
    let ciphertext = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: plaintext, aad })
        .map_err(|_| VaultError::Format(t!("vault.encryption_failed").into()))?;
    Ok((nonce, ciphertext))
}

fn decode_fixed<const N: usize>(b64: &str, what: &str) -> Result<[u8; N], VaultError> {
    B64.decode(b64)
        .ok()
        .and_then(|v| <[u8; N]>::try_from(v).ok())
        .ok_or_else(|| VaultError::Format(tr!("vault.invalid_value", what = what)))
}

// ---------------------------------------------------------------------------------------------
// Data sealed with a password (exported site lists)

const SEALED_FORMAT: &str = "playterm-sealed";
const SEALED_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct SealedFile {
    format: String,
    /// What the data is (`sites`): a file of one kind is not taken for another.
    kind: String,
    version: u32,
    kdf: String,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

fn sealed_aad(kind: &str, kdf: Kdf, salt: &[u8]) -> Vec<u8> {
    format!("{SEALED_FORMAT}:{kind}:v{SEALED_VERSION}:{KDF_NAME}:{}:{}:{}:{}", kdf.m_cost, kdf.t_cost, kdf.p_cost, B64.encode(salt))
        .into_bytes()
}

/// `plaintext` encrypted with a key derived from `password` (Argon2id), as the text of a file.
pub fn seal(kind: &str, password: &str, plaintext: &[u8]) -> Result<String, VaultError> {
    if password.is_empty() {
        return Err(VaultError::EmptyPassword);
    }
    let kdf = Kdf::DEFAULT;
    let salt: [u8; SALT_LEN] = random()?;
    let key = kdf.derive(password, &salt)?;
    let (nonce, ciphertext) = encrypt(&key, &sealed_aad(kind, kdf, &salt), plaintext)?;
    let file = SealedFile {
        format: SEALED_FORMAT.into(),
        kind: kind.into(),
        version: SEALED_VERSION,
        kdf: KDF_NAME.into(),
        m_cost: kdf.m_cost,
        t_cost: kdf.t_cost,
        p_cost: kdf.p_cost,
        salt: B64.encode(salt),
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(ciphertext),
    };
    toml::to_string_pretty(&file).map_err(|e| VaultError::Format(e.to_string()))
}

/// The data [`seal`]ed in `text`, if `password` is the one it was sealed with.
pub fn unseal(kind: &str, password: &str, text: &str) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    let file: SealedFile = toml::from_str(text).map_err(|e| VaultError::Format(e.to_string()))?;
    if file.format != SEALED_FORMAT || file.kind != kind || file.version != SEALED_VERSION || file.kdf != KDF_NAME {
        return Err(VaultError::Format(tr!("vault.unsupported_version_v", arg0 = file.version, arg1 = file.kind)));
    }
    let kdf = Kdf::checked(file.m_cost, file.t_cost, file.p_cost)?;
    let salt: [u8; SALT_LEN] = decode_fixed(&file.salt, "salt")?;
    let nonce: [u8; NONCE_LEN] = decode_fixed(&file.nonce, "nonce")?;
    let ciphertext = B64.decode(&file.ciphertext).map_err(|e| VaultError::Format(e.to_string()))?;
    let key = kdf.derive(password, &salt)?;
    let cipher = XChaCha20Poly1305::new(&Key::from(*key));
    let aad = sealed_aad(kind, kdf, &salt);
    let plaintext = cipher.decrypt(&XNonce::from(nonce), Payload { msg: &ciphertext, aad: &aad }).map_err(|_| VaultError::WrongPassword)?;
    Ok(Zeroizing::new(plaintext))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !path.exists() {
            assert!(Instant::now() < deadline, "timeout: {}", path.display());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // Spawn this exact test in a separate OS process. Fake passwords only.
    #[test]
    fn process_worker() {
        let Some(dir) = std::env::var_os("PLAYTERM_VAULT_TEST_DIR") else { return };
        let dir = PathBuf::from(dir);
        let name = std::env::var("PLAYTERM_VAULT_TEST_NAME").unwrap();
        let op = std::env::var("PLAYTERM_VAULT_TEST_OP").unwrap();
        let mut v = Vault::new(dir.join("vault.toml"));
        if op != "create" {
            v.open().unwrap();
        }
        std::fs::write(dir.join(format!("ready-{name}")), "").unwrap();
        wait_file(&dir.join(format!("start-{name}")));
        if op == "reset" {
            v.reset().unwrap();
        } else if op == "create" {
            let result = v.create();
            assert!(result.is_ok() || matches!(result, Err(VaultError::AlreadyExists)));
        } else {
            v.update(|u| {
                std::fs::write(dir.join(format!("entered-{name}")), "").unwrap();
                wait_file(&dir.join(format!("release-{name}")));
                if op == "remove" {
                    u.secrets.remove("old");
                    u.targets.remove("old");
                } else {
                    u.secrets.insert(name.clone(), Zeroizing::new("fake-secret".into()));
                    u.targets.insert(name.clone(), format!("ssh://{name}"));
                }
                true
            })
            .unwrap();
        }
        std::fs::write(dir.join(format!("done-{name}")), "").unwrap();
    }

    fn worker(dir: &Path, name: &str, op: &str) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "vault::tests::process_worker", "--nocapture"])
            .env("PLAYTERM_VAULT_TEST_DIR", dir)
            .env("PLAYTERM_VAULT_TEST_NAME", name)
            .env("PLAYTERM_VAULT_TEST_OP", op)
            .spawn()
            .unwrap()
    }

    fn signal(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), "").unwrap();
    }

    #[test]
    fn separate_processes_serialize_add_remove_and_reset() {
        for op in ["add", "remove", "reset"] {
            let (dir, mut v) = vault();
            v.create().unwrap();
            v.set("old", "ssh://old", "old-secret").unwrap();
            let mut a = worker(dir.path(), "a", "add");
            let mut b = worker(dir.path(), "b", op);
            for name in ["ready-a", "ready-b"] {
                wait_file(&dir.path().join(name));
            }
            signal(dir.path(), "start-a");
            wait_file(&dir.path().join("entered-a"));
            signal(dir.path(), "start-b");
            // B must not enter the transaction while A owns the OS lock.
            std::thread::sleep(Duration::from_millis(150));
            assert!(!dir.path().join("entered-b").exists());
            assert!(!dir.path().join("done-b").exists());
            signal(dir.path(), "release-b");
            signal(dir.path(), "release-a");
            wait_file(&dir.path().join("done-a"));
            wait_file(&dir.path().join("done-b"));
            assert!(a.wait().unwrap().success());
            assert!(b.wait().unwrap().success());
            if op == "reset" {
                assert!(!v.exists());
                assert!(matches!(v.set("resurrect", "t", "x"), Err(VaultError::Replaced)));
                v.create().unwrap();
                continue;
            }
            v.open().unwrap();
            assert_eq!(v.get("a"), Some("fake-secret"));
            if op == "add" {
                assert_eq!(v.get("b"), Some("fake-secret"));
            } else {
                assert_eq!(v.get("old"), None);
            }
        }
    }

    #[test]
    fn process_exit_releases_lock_and_concurrent_creates_have_one_winner() {
        let (dir, mut v) = vault();
        let mut a = worker(dir.path(), "a", "create");
        let mut b = worker(dir.path(), "b", "create");
        for name in ["ready-a", "ready-b"] {
            wait_file(&dir.path().join(name));
        }
        for name in ["start-a", "start-b"] {
            signal(dir.path(), name);
        }
        for name in ["done-a", "done-b"] {
            wait_file(&dir.path().join(name));
        }
        assert!(a.wait().unwrap().success());
        assert!(b.wait().unwrap().success());
        v.open().unwrap();
        let mut crashed = worker(dir.path(), "crash", "add");
        wait_file(&dir.path().join("ready-crash"));
        signal(dir.path(), "start-crash");
        wait_file(&dir.path().join("entered-crash"));
        crashed.kill().unwrap();
        crashed.wait().unwrap();
        v.set("survivor", "ssh://test", "fake").unwrap();
        assert_eq!(v.get("crash"), None);
        v.open().unwrap();
        assert_eq!(v.get("survivor"), Some("fake"));
    }

    #[test]
    fn lock_timeout_preserves_memory_and_file() {
        let (_dir, mut v) = vault();
        v.create().unwrap();
        v.set("old", "ssh://test", "fake").unwrap();
        let before = std::fs::read(&v.path).unwrap();
        let guard = lock_file(&v.path).unwrap();
        let started = Instant::now();
        assert!(matches!(v.set("new", "t", "fake"), Err(VaultError::Io(e)) if e.kind() == io::ErrorKind::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(v.get("new"), None);
        assert_eq!(std::fs::read(&v.path).unwrap(), before);
        drop(guard);
        v.set("new", "t", "fake").unwrap();
    }

    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::new(dir.path().join("vault.toml"));
        (dir, v)
    }

    #[test]
    fn create_set_lock_open_without_a_password() {
        let (_dir, mut v) = vault();
        assert!(!v.exists());
        assert_eq!(v.kind().unwrap(), VaultKind::Missing);
        v.create().unwrap();
        assert_eq!(v.kind().unwrap(), VaultKind::Local);
        v.set("p1", "ssh://me@a:22", "s3cret!").unwrap();
        v.set("p2", "telnet://b:23", "한글비번").unwrap();
        v.lock();
        assert_eq!(v.get("p1"), None);
        assert!(matches!(v.set("p3", "x", "x"), Err(VaultError::Locked)));

        v.open().unwrap();
        assert_eq!(v.get("p1"), Some("s3cret!"));
        assert_eq!(v.get("p2"), Some("한글비번"));
        assert_eq!(v.target("p1"), Some("ssh://me@a:22"));

        v.rebind("p1", "ssh://me@new:22").unwrap();
        v.lock();
        v.open().unwrap();
        assert_eq!(v.target("p1"), Some("ssh://me@new:22"));

        v.remove("p1").unwrap();
        v.remove("p2").unwrap();
        v.lock();
        v.open().unwrap();
        assert_eq!((v.get("p1"), v.get("p2"), v.target("p1")), (None, None, None));
    }

    #[test]
    fn a_vault_without_its_key_does_not_open() {
        let (dir, mut v) = vault();
        v.create().unwrap();
        v.set("p", "ssh://t", "secret").unwrap();
        v.lock();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "key") {
                std::fs::write(&path, B64.encode([9u8; KEY_LEN])).unwrap();
            }
        }
        assert!(matches!(v.open(), Err(VaultError::KeyMissing)));
        let mut other = Vault::with_keys(v.path().to_owned(), Arc::new(FileKeyStore::new(dir.path().join("elsewhere"))));
        assert!(matches!(other.open(), Err(VaultError::KeyMissing)));
        assert!(!v.is_unlocked() && !other.is_unlocked());
    }

    #[cfg(unix)]
    #[test]
    fn vault_and_key_writes_keep_mode_0600_including_migration() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.toml");
        let mut vault = Vault::new(&path);
        vault.create().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "key") {
                assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        vault.set("site", "ssh://test", "fake-password").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let (_legacy_dir, mut legacy) = legacy_vault(Kdf { m_cost: 19 * 1024, t_cost: 2, p_cost: 1 });
        std::fs::set_permissions(legacy.path(), std::fs::Permissions::from_mode(0o666)).unwrap();
        legacy.migrate("test-master").unwrap();
        assert_eq!(std::fs::metadata(legacy.path()).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn no_plaintext_on_disk() {
        let (_dir, mut v) = vault();
        v.create().unwrap();
        // A long id: random ciphertext text could contain a short one like "p1" by chance.
        v.set("site-id-for-plaintext-check", "ssh://user@target-host-check:22", "very-secret-value").unwrap();
        let raw = std::fs::read_to_string(&v.path).unwrap();
        assert!(!raw.contains("very-secret-value"));
        assert!(!raw.contains("target-host-check"), "targets are encrypted too");
        assert!(!raw.contains("site-id-for-plaintext-check"), "secret ids are encrypted too");
    }

    #[test]
    fn tampered_header_is_rejected() {
        let (_dir, mut v) = vault();
        v.create().unwrap();
        v.lock();
        let raw = std::fs::read_to_string(&v.path).unwrap();
        let tampered = raw.replace(&format!("version = {LOCAL_VERSION}"), &format!("version = {MASTER_VERSION}"));
        assert_ne!(raw, tampered);
        std::fs::write(&v.path, tampered).unwrap();
        assert!(v.open().is_err());

        let (_dir, mut legacy) = legacy_vault(Kdf::DEFAULT);
        let raw = std::fs::read_to_string(&legacy.path).unwrap();
        let tampered = raw.replace(&format!("t_cost = {}", Kdf::DEFAULT.t_cost), "t_cost = 1");
        assert_ne!(raw, tampered);
        std::fs::write(&legacy.path, tampered).unwrap();
        assert!(matches!(legacy.migrate("test-master"), Err(VaultError::WrongPassword)));
    }

    #[test]
    fn create_rules_and_reset() {
        let (_dir, mut v) = vault();
        assert!(matches!(v.open(), Err(VaultError::Missing)));
        assert!(matches!(v.migrate("x"), Err(VaultError::Missing)));
        v.create().unwrap();
        assert!(matches!(v.create(), Err(VaultError::AlreadyExists)));
        v.reset().unwrap();
        assert!(!v.exists() && !v.is_unlocked());
        assert!(v.path.with_file_name("vault.toml.bak").exists(), "the old vault is kept");
        v.create().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(v.path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn two_windows_keep_each_others_passwords() {
        let (dir, mut a) = vault();
        a.create().unwrap();
        let mut b = Vault::new(dir.path().join("vault.toml"));
        b.open().unwrap();
        // Each window saves a password the other one has not seen.
        a.set("from-a", "ssh://a", "1").unwrap();
        b.set("from-b", "ssh://b", "2").unwrap();
        a.remove("missing").unwrap();
        a.set("from-a-2", "ssh://a2", "3").unwrap();
        let mut c = Vault::new(dir.path().join("vault.toml"));
        c.open().unwrap();
        assert_eq!((c.get("from-a"), c.get("from-b"), c.get("from-a-2")), (Some("1"), Some("2"), Some("3")));

        // A vault recreated elsewhere is not overwritten with old contents.
        c.reset().unwrap();
        c.create().unwrap();
        assert!(matches!(a.set("x", "t", "y"), Err(VaultError::Replaced)));
    }

    #[test]
    fn older_secrets_get_their_target_once() {
        let (_dir, mut v) = vault();
        v.create().unwrap();
        v.set("p1", "ssh://kept", "1").unwrap();
        // As written by an older version: a secret without a target.
        v.unlocked.as_mut().unwrap().secrets.insert("old".into(), Zeroizing::new("2".into()));
        let _lock = lock_file(&v.path).unwrap();
        v.save_unlocked(v.unlocked.as_ref().unwrap()).unwrap();
        drop(_lock);
        v.bind_missing(|id| Some(format!("ssh://now-{id}"))).unwrap();
        assert_eq!((v.target("p1"), v.target("old")), (Some("ssh://kept"), Some("ssh://now-old")));
    }

    #[test]
    fn each_save_uses_a_new_nonce() {
        let (_dir, mut v) = vault();
        v.create().unwrap();
        let first = std::fs::read_to_string(&v.path).unwrap();
        v.set("a", "t", "1").unwrap();
        v.remove("a").unwrap();
        let second = std::fs::read_to_string(&v.path).unwrap();
        assert_ne!(first, second);
    }

    /// A vault as older versions wrote it: protected by the master password `test-master`.
    fn legacy_vault(kdf: Kdf) -> (tempfile::TempDir, Vault) {
        let (dir, vault) = vault();
        let salt = [7; SALT_LEN];
        let key = kdf.derive("test-master", &salt).unwrap();
        let unlocked = Unlocked {
            key,
            protection: Protection::Master(kdf),
            salt,
            secrets: BTreeMap::from([("site".into(), Zeroizing::new("saved-secret".into()))]),
            targets: BTreeMap::from([("site".into(), "ssh://test@localhost:22".into())]),
        };
        let _lock = lock_file(&vault.path).unwrap();
        vault.save_unlocked(&unlocked).unwrap();
        (dir, vault)
    }

    #[test]
    fn an_older_vault_migrates_once_with_its_master_password() {
        let (dir, mut vault) = legacy_vault(Kdf { m_cost: 19 * 1024, t_cost: 2, p_cost: 1 });
        assert_eq!(vault.kind().unwrap(), VaultKind::Master);
        assert!(matches!(vault.open(), Err(VaultError::Locked)));
        let old = std::fs::read(&vault.path).unwrap();
        assert!(matches!(vault.migrate("wrong"), Err(VaultError::WrongPassword)));
        assert_eq!(std::fs::read(&vault.path).unwrap(), old);
        assert!(!vault.is_unlocked());

        vault.migrate("test-master").unwrap();
        assert_eq!(vault.kind().unwrap(), VaultKind::Local);
        assert_eq!(vault.get("site"), Some("saved-secret"));
        assert_eq!(vault.target("site"), Some("ssh://test@localhost:22"));
        vault.set("next", "ssh://next", "next-secret").unwrap();

        // Another window (or the next start) opens it without the master password.
        let mut again = Vault::new(dir.path().join("vault.toml"));
        again.open().unwrap();
        assert_eq!((again.get("site"), again.get("next")), (Some("saved-secret"), Some("next-secret")));
        // A window that still asked for the master password just opens it.
        let mut late = Vault::new(dir.path().join("vault.toml"));
        late.migrate("whatever").unwrap();
        assert_eq!(late.get("next"), Some("next-secret"));
    }

    #[test]
    fn cancellation_never_persists_migration() {
        let (_dir, mut old) = legacy_vault(Kdf { m_cost: 19 * 1024, t_cost: 2, p_cost: 1 });
        let original = std::fs::read(&old.path).unwrap();
        assert!(matches!(old.migrate_cancellable("test-master", || false), Err(VaultError::Cancelled)));
        assert_eq!(std::fs::read(&old.path).unwrap(), original);
        assert!(!old.is_unlocked());
        let keys = std::fs::read_dir(old.path.parent().unwrap())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|e| e == "key"))
            .count();
        assert_eq!(keys, 0, "no key is stored for a cancelled migration");
    }

    #[test]
    fn excessive_kdf_costs_are_rejected_without_rewriting() {
        let (_dir, mut vault) = legacy_vault(Kdf::DEFAULT);
        let original = std::fs::read_to_string(&vault.path).unwrap();
        for (m, t) in [(MAX_M_COST_KIB + 1, 3), (64 * 1024, MAX_T_COST + 1)] {
            let mut file: VaultFile = toml::from_str(&original).unwrap();
            file.m_cost = m;
            file.t_cost = t;
            let tampered = toml::to_string(&file).unwrap();
            std::fs::write(&vault.path, &tampered).unwrap();
            assert!(matches!(vault.migrate("test-master"), Err(VaultError::Format(_))));
            assert_eq!(std::fs::read_to_string(&vault.path).unwrap(), tampered);
        }
    }

    #[test]
    fn sealed_data_opens_only_with_its_password_and_kind() {
        let text = seal("sites", "export-pass", "host = \"db01\"\npassword = \"hunter2\"".as_bytes()).unwrap();
        assert!(!text.contains("db01") && !text.contains("hunter2"));
        let plain = unseal("sites", "export-pass", &text).unwrap();
        assert_eq!(std::str::from_utf8(&plain).unwrap(), "host = \"db01\"\npassword = \"hunter2\"");
        assert!(matches!(unseal("sites", "wrong", &text), Err(VaultError::WrongPassword)));
        assert!(matches!(unseal("other", "export-pass", &text), Err(VaultError::Format(_))));
        assert!(matches!(seal("sites", "", b"x"), Err(VaultError::EmptyPassword)));
        let tampered = text.replace("kind = \"sites\"", "kind = \"other\"");
        assert!(unseal("other", "export-pass", &tampered).is_err());
    }

    /// Fake encrypted data for an actual-window migration check.
    #[test]
    #[ignore = "interactive vault fixture; set PLAYTERM_VAULT_GUI_DIR and optionally PLAYTERM_VAULT_GUI_SLOW"]
    fn native_vault_fixture() {
        let dir = PathBuf::from(std::env::var_os("PLAYTERM_VAULT_GUI_DIR").unwrap());
        let kdf = if std::env::var_os("PLAYTERM_VAULT_GUI_SLOW").is_some() {
            Kdf { m_cost: 512 * 1024, t_cost: 16, p_cost: 1 }
        } else {
            Kdf { m_cost: 19 * 1024, t_cost: 2, p_cost: 1 }
        };
        let (_temporary, vault) = legacy_vault(kdf);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(vault.path(), dir.join("vault.toml")).unwrap();
        println!("Fake GUI vault: {} (master: test-master)", dir.display());
    }
}
