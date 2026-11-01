//! Password vault: secrets encrypted with a key derived from a master password.
//!
//! - KDF: Argon2id (parameters and salt stored in the file)
//! - Cipher: XChaCha20-Poly1305 with a fresh random nonce on every save
//! - The KDF parameters and salt are bound as associated data, so tampering with the
//!   header makes decryption fail instead of silently weakening the key.
//! - Each secret records the target it was saved for (`ssh://user@host:port`), inside the
//!   encrypted part: the site list is plain text, and a password must not follow a site that
//!   was pointed somewhere else.
//! - Every change re-reads the file first and applies just that change, so two playterm
//!   windows do not undo each other's saved passwords.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use playterm_i18n::{t, tr};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::fsutil::write_atomic;

const FORMAT_VERSION: u32 = 1;
const KDF_NAME: &str = "argon2id";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
/// Refuse absurd KDF costs from a (possibly tampered) file: 1 GiB, 16 passes.
const MAX_M_COST_KIB: u32 = 1024 * 1024;
const MAX_T_COST: u32 = 16;

#[derive(Debug)]
pub enum VaultError {
    WrongPassword,
    Locked,
    AlreadyExists,
    Missing,
    EmptyPassword,
    Format(String),
    /// The file was replaced (another window reset and recreated the vault) since unlocking.
    Replaced,
    Io(io::Error),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::WrongPassword => f.write_str(t("마스터 비밀번호가 틀렸습니다", "Wrong master password")),
            VaultError::Locked => f.write_str(t("볼트가 잠겨 있습니다", "The password vault is locked")),
            VaultError::AlreadyExists => f.write_str(t("볼트가 이미 존재합니다", "The password vault already exists")),
            VaultError::Missing => f.write_str(t("볼트 파일이 없습니다", "The password vault file does not exist")),
            VaultError::EmptyPassword => {
                f.write_str(t("마스터 비밀번호는 비워둘 수 없습니다", "The master password cannot be empty"))
            }
            VaultError::Replaced => f.write_str(t(
                "다른 창에서 볼트를 새로 만들었습니다. 잠금을 다시 풀어 주세요",
                "The password vault was recreated in another window. Unlock it again",
            )),
            VaultError::Format(detail) => {
                f.write_str(&tr!("볼트 파일 형식 오류: {detail}", "Invalid password vault file: {detail}"))
            }
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
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
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

#[derive(Clone, Copy)]
struct Kdf {
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}

impl Kdf {
    const DEFAULT: Kdf = Kdf { m_cost: Params::DEFAULT_M_COST, t_cost: Params::DEFAULT_T_COST, p_cost: 1 };

    fn derive(&self, master: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, VaultError> {
        let params = Params::new(self.m_cost, self.t_cost, self.p_cost, Some(32))
            .map_err(|e| VaultError::Format(tr!("KDF 파라미터: {e}", "KDF parameters: {e}")))?;
        let mut key = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(master.as_bytes(), salt, key.as_mut())
            .map_err(|e| VaultError::Format(format!("KDF: {e}")))?;
        Ok(key)
    }

    fn aad(&self, salt: &[u8]) -> Vec<u8> {
        format!(
            "playterm-vault:v{FORMAT_VERSION}:{KDF_NAME}:{}:{}:{}:{}",
            self.m_cost,
            self.t_cost,
            self.p_cost,
            B64.encode(salt)
        )
        .into_bytes()
    }
}

struct Unlocked {
    key: Zeroizing<[u8; 32]>,
    kdf: Kdf,
    salt: [u8; SALT_LEN],
    secrets: BTreeMap<String, Zeroizing<String>>,
    targets: BTreeMap<String, String>,
}

impl Unlocked {
    /// The secrets in the vault file as it is now, decrypted with this key.
    fn read(&self, path: &Path) -> Result<Option<Decrypted>, VaultError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            // Deleted meanwhile: what is in memory is written back.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let file: VaultFile = toml::from_str(&text).map_err(|e| VaultError::Format(e.to_string()))?;
        let salt: [u8; SALT_LEN] = decode_fixed(&file.salt, "salt")?;
        let same_kdf = (file.m_cost, file.t_cost, file.p_cost) == (self.kdf.m_cost, self.kdf.t_cost, self.kdf.p_cost);
        if salt != self.salt || !same_kdf {
            return Err(VaultError::Replaced);
        }
        let contents = decrypt(&self.key, self.kdf, &salt, &file).map_err(|_| VaultError::Replaced)?;
        Ok(Some(contents))
    }
}

/// Secrets and their targets, as decrypted from the file.
type Decrypted = (BTreeMap<String, Zeroizing<String>>, BTreeMap<String, String>);

/// Decrypts the contents of `file` (whose salt is `salt`) with `key`.
fn decrypt(
    key: &[u8; 32],
    kdf: Kdf,
    salt: &[u8],
    file: &VaultFile,
) -> Result<Decrypted, VaultError> {
    let nonce: [u8; NONCE_LEN] = decode_fixed(&file.nonce, "nonce")?;
    let ciphertext = B64.decode(&file.ciphertext).map_err(|e| VaultError::Format(e.to_string()))?;
    let cipher = XChaCha20Poly1305::new(&Key::from(*key));
    let aad = kdf.aad(salt);
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(&XNonce::from(nonce), Payload { msg: &ciphertext, aad: &aad })
            .map_err(|_| VaultError::WrongPassword)?,
    );
    let text = std::str::from_utf8(&plaintext).map_err(|e| VaultError::Format(e.to_string()))?;
    let contents: Contents = toml::from_str(text).map_err(|e| VaultError::Format(e.to_string()))?;
    let secrets = contents.secrets.into_iter().map(|(k, v)| (k, Zeroizing::new(v))).collect();
    Ok((secrets, contents.targets))
}

/// The `vault.toml` file. Starts locked; secrets are only available after [`Vault::unlock`].
pub struct Vault {
    path: PathBuf,
    unlocked: Option<Unlocked>,
}

impl Vault {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), unlocked: None }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn is_unlocked(&self) -> bool {
        self.unlocked.is_some()
    }

    /// Creates a new, empty vault protected by `master` and leaves it unlocked.
    pub fn create(&mut self, master: &str) -> Result<(), VaultError> {
        if master.is_empty() {
            return Err(VaultError::EmptyPassword);
        }
        if self.exists() {
            return Err(VaultError::AlreadyExists);
        }
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|e| io::Error::other(e.to_string()))?;
        let kdf = Kdf::DEFAULT;
        let key = kdf.derive(master, &salt)?;
        self.unlocked = Some(Unlocked { key, kdf, salt, secrets: BTreeMap::new(), targets: BTreeMap::new() });
        self.save()
    }

    pub fn unlock(&mut self, master: &str) -> Result<(), VaultError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(VaultError::Missing),
            Err(e) => return Err(e.into()),
        };
        let file: VaultFile = toml::from_str(&text).map_err(|e| VaultError::Format(e.to_string()))?;
        if file.version != FORMAT_VERSION || file.kdf != KDF_NAME {
            return Err(VaultError::Format(tr!(
                "지원하지 않는 버전: v{} {}",
                "Unsupported version: v{} {}",
                file.version,
                file.kdf
            )));
        }
        if file.m_cost > MAX_M_COST_KIB || file.t_cost > MAX_T_COST {
            return Err(VaultError::Format(t("KDF 비용이 비정상적으로 큽니다", "The KDF cost is abnormally high").into()));
        }
        let salt: [u8; SALT_LEN] = decode_fixed(&file.salt, "salt")?;
        let kdf = Kdf { m_cost: file.m_cost, t_cost: file.t_cost, p_cost: file.p_cost };
        let key = kdf.derive(master, &salt)?;
        let (secrets, targets) = decrypt(&key, kdf, &salt, &file)?;
        self.unlocked = Some(Unlocked { key, kdf, salt, secrets, targets });
        Ok(())
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
            let missing: Vec<(String, String)> = u
                .secrets
                .keys()
                .filter(|id| !u.targets.contains_key(*id))
                .filter_map(|id| Some((id.clone(), target_of(id)?)))
                .collect();
            let changed = !missing.is_empty();
            u.targets.extend(missing);
            changed
        })
    }

    /// Applies `change` to the vault as it is on disk now (another window may have saved since)
    /// and writes it if `change` returns true.
    fn update(&mut self, change: impl FnOnce(&mut Unlocked) -> bool) -> Result<(), VaultError> {
        let unlocked = self.unlocked.as_mut().ok_or(VaultError::Locked)?;
        if let Some((secrets, targets)) = unlocked.read(&self.path)? {
            unlocked.secrets = secrets;
            unlocked.targets = targets;
        }
        if change(unlocked) {
            self.save()?;
        }
        Ok(())
    }

    /// Deletes the vault file and every stored secret. Used when the master password is lost.
    pub fn reset(&mut self) -> Result<(), VaultError> {
        self.lock();
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self) -> Result<(), VaultError> {
        let unlocked = self.unlocked.as_ref().ok_or(VaultError::Locked)?;
        let contents = ContentsRef {
            secrets: unlocked.secrets.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
            targets: &unlocked.targets,
        };
        let plaintext =
            Zeroizing::new(toml::to_string(&contents).map_err(|e| VaultError::Format(e.to_string()))?);

        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
        let cipher = XChaCha20Poly1305::new(&Key::from(*unlocked.key));
        let aad = unlocked.kdf.aad(&unlocked.salt);
        let ciphertext = cipher
            .encrypt(&XNonce::from(nonce), Payload { msg: plaintext.as_bytes(), aad: &aad })
            .map_err(|_| VaultError::Format(t("암호화 실패", "Encryption failed").into()))?;

        let file = VaultFile {
            version: FORMAT_VERSION,
            kdf: KDF_NAME.into(),
            m_cost: unlocked.kdf.m_cost,
            t_cost: unlocked.kdf.t_cost,
            p_cost: unlocked.kdf.p_cost,
            salt: B64.encode(unlocked.salt),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ciphertext),
        };
        let text = toml::to_string_pretty(&file).map_err(|e| VaultError::Format(e.to_string()))?;
        write_atomic(&self.path, text.as_bytes())?;
        Ok(())
    }
}

fn decode_fixed<const N: usize>(b64: &str, what: &str) -> Result<[u8; N], VaultError> {
    B64.decode(b64)
        .ok()
        .and_then(|v| <[u8; N]>::try_from(v).ok())
        .ok_or_else(|| VaultError::Format(tr!("{what} 값이 잘못되었습니다", "Invalid {what} value")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::new(dir.path().join("vault.toml"));
        (dir, v)
    }

    #[test]
    fn create_set_lock_unlock() {
        let (_dir, mut v) = vault();
        assert!(!v.exists());
        v.create("correct horse").unwrap();
        v.set("p1", "ssh://me@a:22", "s3cret!").unwrap();
        v.set("p2", "telnet://b:23", "한글비번").unwrap();
        v.lock();
        assert_eq!(v.get("p1"), None);
        assert!(matches!(v.set("p3", "x", "x"), Err(VaultError::Locked)));

        v.unlock("correct horse").unwrap();
        assert_eq!(v.get("p1"), Some("s3cret!"));
        assert_eq!(v.get("p2"), Some("한글비번"));
        assert_eq!(v.target("p1"), Some("ssh://me@a:22"));

        v.rebind("p1", "ssh://me@new:22").unwrap();
        v.lock();
        v.unlock("correct horse").unwrap();
        assert_eq!(v.target("p1"), Some("ssh://me@new:22"));

        v.remove("p1").unwrap();
        v.remove("p2").unwrap();
        v.lock();
        v.unlock("correct horse").unwrap();
        assert_eq!((v.get("p1"), v.get("p2"), v.target("p1")), (None, None, None));
    }

    #[test]
    fn wrong_password_and_no_plaintext_on_disk() {
        let (_dir, mut v) = vault();
        v.create("master").unwrap();
        // A long id: random ciphertext text could contain a short one like "p1" by chance.
        v.set("site-id-for-plaintext-check", "ssh://user@target-host-check:22", "very-secret-value").unwrap();
        let raw = std::fs::read_to_string(&v.path).unwrap();
        assert!(!raw.contains("very-secret-value"));
        assert!(!raw.contains("target-host-check"), "targets are encrypted too");
        assert!(!raw.contains("site-id-for-plaintext-check"), "secret ids are encrypted too");

        v.lock();
        assert!(matches!(v.unlock("nope"), Err(VaultError::WrongPassword)));
        assert!(!v.is_unlocked());
    }

    #[test]
    fn tampered_header_is_rejected() {
        let (_dir, mut v) = vault();
        v.create("master").unwrap();
        v.lock();
        let raw = std::fs::read_to_string(&v.path).unwrap();
        let tampered = raw.replace(&format!("t_cost = {}", Params::DEFAULT_T_COST), "t_cost = 1");
        assert_ne!(raw, tampered);
        std::fs::write(&v.path, tampered).unwrap();
        assert!(matches!(v.unlock("master"), Err(VaultError::WrongPassword)));
    }

    #[test]
    fn create_rules_and_reset() {
        let (_dir, mut v) = vault();
        assert!(matches!(v.create(""), Err(VaultError::EmptyPassword)));
        assert!(matches!(v.unlock("x"), Err(VaultError::Missing)));
        v.create("a").unwrap();
        assert!(matches!(v.create("b"), Err(VaultError::AlreadyExists)));
        v.reset().unwrap();
        assert!(!v.exists() && !v.is_unlocked());
        v.create("b").unwrap();
    }

    #[test]
    fn two_windows_keep_each_others_passwords() {
        let (dir, mut a) = vault();
        a.create("m").unwrap();
        let mut b = Vault::new(dir.path().join("vault.toml"));
        b.unlock("m").unwrap();
        // Each window saves a password the other one has not seen.
        a.set("from-a", "ssh://a", "1").unwrap();
        b.set("from-b", "ssh://b", "2").unwrap();
        a.remove("missing").unwrap();
        a.set("from-a-2", "ssh://a2", "3").unwrap();
        let mut c = Vault::new(dir.path().join("vault.toml"));
        c.unlock("m").unwrap();
        assert_eq!((c.get("from-a"), c.get("from-b"), c.get("from-a-2")), (Some("1"), Some("2"), Some("3")));

        // A vault recreated elsewhere is not overwritten with old contents.
        c.reset().unwrap();
        c.create("other").unwrap();
        assert!(matches!(a.set("x", "t", "y"), Err(VaultError::Replaced)));
    }

    #[test]
    fn older_secrets_get_their_target_once() {
        let (_dir, mut v) = vault();
        v.create("m").unwrap();
        v.set("p1", "ssh://kept", "1").unwrap();
        // As written by an older version: a secret without a target.
        v.unlocked.as_mut().unwrap().secrets.insert("old".into(), Zeroizing::new("2".into()));
        v.save().unwrap();
        v.bind_missing(|id| Some(format!("ssh://now-{id}"))).unwrap();
        assert_eq!((v.target("p1"), v.target("old")), (Some("ssh://kept"), Some("ssh://now-old")));
    }

    #[test]
    fn each_save_uses_a_new_nonce() {
        let (_dir, mut v) = vault();
        v.create("m").unwrap();
        let first = std::fs::read_to_string(&v.path).unwrap();
        v.set("a", "t", "1").unwrap();
        v.remove("a").unwrap();
        let second = std::fs::read_to_string(&v.path).unwrap();
        assert_ne!(first, second);
    }
}
