//! Persistent configuration for playterm: saved session profiles, the encrypted password
//! vault and the application settings. All live as files in the user's config directory.

mod fsutil;
mod profiles;
mod settings;
mod vault;

pub use profiles::{
    AutoLogin, LogSettings, LoginStep, ProfileStore, SessionProfile, SiteTemplate, TabColor, TemplateLayout, folder_name,
    normalize_folder, parent_folder,
};
pub use settings::{AppSettings, ClipboardKeys, Language, Macro, PreventSleep, SiteView, UiTheme};
pub use vault::{Vault, VaultError};
