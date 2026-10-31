//! Persistent configuration for playterm: saved session profiles, the encrypted password
//! vault and the application settings. All live as files in the user's config directory.

mod fsutil;
mod profiles;
mod settings;
mod vault;

pub use profiles::{AutoLogin, LogSettings, LoginStep, ProfileStore, SessionProfile, TabColor};
pub use settings::{AppSettings, Language, PreventSleep, UiTheme};
pub use vault::{Vault, VaultError};
