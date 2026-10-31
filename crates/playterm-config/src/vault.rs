//! Password vault: secrets encrypted with a key derived from a master password.
//!
//! - KDF: Argon2id (parameters and salt stored in the file)
//! - Cipher: XChaCha20-Poly1305 with a fresh random nonce on every save
//! - The KDF parameters and salt are bound as associated data, so tampering with the
//!   header makes decryption fail instead of silently weakening the key.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
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

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("마스터 비밀번호가 틀렸습니다")]
    WrongPassword,
    #[error("볼트가 잠겨 있습니다")]
    Locked,
    #[error("볼트가 이미 존재합니다")]
    AlreadyExists,
    #[error("볼트 파일이 없습니다")]
    Missing,
    #[error("마스터 비밀번호는 비워둘 수 없습니다")]
    EmptyPassword,
    #[error("볼트 파일 형식 오류: {0}")]
    Format(String),
    #[error(transparent)]
    Io(#[from] io::Error),
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
}

#[derive(Deserialize)]
struct Contents {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
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
            .map_err(|e| VaultError::Format(format!("KDF 파라미터: {e}")))?;
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
        self.unlocked = Some(Unlocked { key, kdf, salt, secrets: BTreeMap::new() });
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
            return Err(VaultError::Format(format!("지원하지 않는 버전: v{} {}", file.version, file.kdf)));
        }
        if file.m_cost > MAX_M_COST_KIB || file.t_cost > MAX_T_COST {
            return Err(VaultError::Format("KDF 비용이 비정상적으로 큽니다".into()));
        }
        let salt: [u8; SALT_LEN] = decode_fixed(&file.salt, "salt")?;
        let nonce: [u8; NONCE_LEN] = decode_fixed(&file.nonce, "nonce")?;
        let ciphertext = B64.decode(&file.ciphertext).map_err(|e| VaultError::Format(e.to_string()))?;

        let kdf = Kdf { m_cost: file.m_cost, t_cost: file.t_cost, p_cost: file.p_cost };
        let key = kdf.derive(master, &salt)?;
        let cipher = XChaCha20Poly1305::new(&Key::from(*key));
        let aad = kdf.aad(&salt);
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(&XNonce::from(nonce), Payload { msg: &ciphertext, aad: &aad })
                .map_err(|_| VaultError::WrongPassword)?,
        );
        let text = std::str::from_utf8(&plaintext).map_err(|e| VaultError::Format(e.to_string()))?;
        let contents: Contents = toml::from_str(text).map_err(|e| VaultError::Format(e.to_string()))?;
        let secrets = contents.secrets.into_iter().map(|(k, v)| (k, Zeroizing::new(v))).collect();

        self.unlocked = Some(Unlocked { key, kdf, salt, secrets });
        Ok(())
    }

    /// Forgets the key and all decrypted secrets (they are zeroed on drop).
    pub fn lock(&mut self) {
        self.unlocked = None;
    }

    pub fn get(&self, id: &str) -> Option<&str> {
        self.unlocked.as_ref()?.secrets.get(id).map(|s| s.as_str())
    }

    pub fn set(&mut self, id: &str, secret: &str) -> Result<(), VaultError> {
        let unlocked = self.unlocked.as_mut().ok_or(VaultError::Locked)?;
        unlocked.secrets.insert(id.to_owned(), Zeroizing::new(secret.to_owned()));
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<(), VaultError> {
        let unlocked = self.unlocked.as_mut().ok_or(VaultError::Locked)?;
        if unlocked.secrets.remove(id).is_some() {
            self.save()?;
        }
        Ok(())
    }

    /// Drops secrets whose id no longer passes `keep` (e.g. deleted profiles).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) -> Result<(), VaultError> {
        let unlocked = self.unlocked.as_mut().ok_or(VaultError::Locked)?;
        let before = unlocked.secrets.len();
        unlocked.secrets.retain(|id, _| keep(id));
        if unlocked.secrets.len() != before {
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
        };
        let plaintext =
            Zeroizing::new(toml::to_string(&contents).map_err(|e| VaultError::Format(e.to_string()))?);

        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
        let cipher = XChaCha20Poly1305::new(&Key::from(*unlocked.key));
        let aad = unlocked.kdf.aad(&unlocked.salt);
        let ciphertext = cipher
            .encrypt(&XNonce::from(nonce), Payload { msg: plaintext.as_bytes(), aad: &aad })
            .map_err(|_| VaultError::Format("암호화 실패".into()))?;

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
        .ok_or_else(|| VaultError::Format(format!("{what} 값이 잘못되었습니다")))
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
        v.set("p1", "s3cret!").unwrap();
        v.set("p2", "한글비번").unwrap();
        v.lock();
        assert_eq!(v.get("p1"), None);
        assert!(matches!(v.set("p3", "x"), Err(VaultError::Locked)));

        v.unlock("correct horse").unwrap();
        assert_eq!(v.get("p1"), Some("s3cret!"));
        assert_eq!(v.get("p2"), Some("한글비번"));

        v.remove("p1").unwrap();
        v.retain(|id| id != "p2").unwrap();
        v.lock();
        v.unlock("correct horse").unwrap();
        assert_eq!((v.get("p1"), v.get("p2")), (None, None));
    }

    #[test]
    fn wrong_password_and_no_plaintext_on_disk() {
        let (_dir, mut v) = vault();
        v.create("master").unwrap();
        v.set("p1", "very-secret-value").unwrap();
        let raw = std::fs::read_to_string(&v.path).unwrap();
        assert!(!raw.contains("very-secret-value"));
        assert!(!raw.contains("p1"), "secret ids are encrypted too");

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
    fn each_save_uses_a_new_nonce() {
        let (_dir, mut v) = vault();
        v.create("m").unwrap();
        let first = std::fs::read_to_string(&v.path).unwrap();
        v.set("a", "1").unwrap();
        v.remove("a").unwrap();
        let second = std::fs::read_to_string(&v.path).unwrap();
        assert_ne!(first, second);
    }
}
