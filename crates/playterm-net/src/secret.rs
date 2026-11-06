//! Owned connection passwords keep every application-owned clone zeroizing.

use std::ops::Deref;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Clone, Default)]
pub struct Password(Zeroizing<String>);

impl Password {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Transfers ownership without creating a plain String copy.
    pub fn into_zeroizing(self) -> Zeroizing<String> {
        self.0
    }
}

impl From<String> for Password {
    fn from(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
}

impl From<&str> for Password {
    fn from(value: &str) -> Self {
        Self::from(value.to_owned())
    }
}

impl From<Zeroizing<String>> for Password {
    fn from(value: Zeroizing<String>) -> Self {
        Self(value)
    }
}

impl Deref for Password {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Password {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Zeroize for Password {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

// The owned Zeroizing<String> field clears its allocation when Password drops.
impl ZeroizeOnDrop for Password {}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_and_transfers_keep_zeroizing_storage_and_redacted_debug() {
        fn zeroizes_on_drop<T: ZeroizeOnDrop>(_: &T) {}
        let password = Password::from("test-secret");
        let mut copy: Password = password.clone();
        zeroizes_on_drop(&password);
        zeroizes_on_drop(&copy);
        assert_eq!(format!("{copy:?}"), "<redacted>");
        copy.zeroize();
        assert!(copy.is_empty());
        assert_eq!(password.as_str(), "test-secret");
        let transferred: Zeroizing<String> = password.into_zeroizing();
        assert_eq!(transferred.as_str(), "test-secret");
    }
}
