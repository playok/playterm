//! Persistent configuration for playterm: saved session profiles and the encrypted
//! password vault. Both live as files in the user's config directory.

mod fsutil;
mod profiles;
mod vault;

pub use profiles::{ProfileStore, SessionProfile};
pub use vault::{Vault, VaultError};
