//! Persistent configuration for playterm: saved session profiles, the encrypted password
//! vault and the application settings. All live as files in the user's config directory.

mod fsutil;
mod profiles;
mod settings;
mod vault;

pub use profiles::{AutoLogin, LogSettings, LoginStep, ProfileStore, SessionProfile};
pub use settings::{AppSettings, Language, UiTheme};
pub use vault::{Vault, VaultError};
