//! Persistent configuration for playterm: saved session profiles, the encrypted password
//! vault and the application settings. All live as files in the user's config directory.

mod exchange;
mod fsutil;
mod keystore;
mod profiles;
mod settings;
mod vault;

pub use exchange::{ExportedSite, SITES_EXTENSION, export_sites, import_sites};
pub use keystore::{FileKeyStore, KeyStore, OsKeyStore};
pub use profiles::{
    AutoLogin, LogSettings, LoginStep, ProfileStore, SessionProfile, SiteTemplate, TabColor, TemplateLayout, folder_name, normalize_folder,
    parent_folder,
};
pub use settings::{AppSettings, ClipboardKeys, Language, Macro, PreventSleep, ShortcutKeys, SiteView, UiTheme};
pub use vault::{Vault, VaultError, VaultKind, seal, unseal};
