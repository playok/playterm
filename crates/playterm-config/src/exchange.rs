//! Site lists exported to a file and imported back (on another computer, say): the sites with
//! their saved passwords, all encrypted with a password chosen at export ([`crate::seal`]).

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::profiles::SessionProfile;
use crate::vault::{VaultError, seal, unseal};

/// What [`crate::seal`] calls these files.
const KIND: &str = "sites";
/// File extension of exported site lists.
pub const SITES_EXTENSION: &str = "ptsites";

/// A site and, if it saves one, its password.
pub struct ExportedSite {
    pub profile: SessionProfile,
    pub password: Option<Zeroizing<String>>,
}

#[derive(Serialize)]
struct ContentsRef<'a> {
    sites: Vec<SiteRef<'a>>,
}

#[derive(Serialize)]
struct SiteRef<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<&'a str>,
    profile: &'a SessionProfile,
}

#[derive(Deserialize)]
struct Contents {
    #[serde(default)]
    sites: Vec<Site>,
}

#[derive(Deserialize)]
struct Site {
    #[serde(default)]
    password: Option<String>,
    profile: SessionProfile,
}

/// The text of an exported site list, encrypted with `password`.
pub fn export_sites(password: &str, sites: &[ExportedSite]) -> Result<String, VaultError> {
    let contents = ContentsRef {
        sites: sites.iter().map(|s| SiteRef { password: s.password.as_ref().map(|p| p.as_str()), profile: &s.profile }).collect(),
    };
    let plaintext = Zeroizing::new(toml::to_string(&contents).map_err(|e| VaultError::Format(e.to_string()))?);
    seal(KIND, password, plaintext.as_bytes())
}

/// The sites of an exported list, if `password` is the one it was exported with.
pub fn import_sites(password: &str, text: &str) -> Result<Vec<ExportedSite>, VaultError> {
    let plaintext = unseal(KIND, password, text)?;
    let text = std::str::from_utf8(&plaintext).map_err(|e| VaultError::Format(e.to_string()))?;
    let contents: Contents = toml::from_str(text).map_err(|e| VaultError::Format(e.to_string()))?;
    Ok(contents.sites.into_iter().map(|s| ExportedSite { profile: s.profile, password: s.password.map(Zeroizing::new) }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_and_passwords_round_trip() {
        let mut web = SessionProfile::new().unwrap();
        web.name = "web".into();
        web.host = "web01".into();
        web.folder = "운영/DB".into();
        web.save_password = true;
        let mut serial = SessionProfile::new().unwrap();
        serial.name = "console".into();
        let sites = [
            ExportedSite { profile: web.clone(), password: Some(Zeroizing::new("p@ss \"quoted\"".into())) },
            ExportedSite { profile: serial.clone(), password: None },
        ];
        let text = export_sites("export-pass", &sites).unwrap();
        assert!(!text.contains("web01") && !text.contains("p@ss"));
        let back = import_sites("export-pass", &text).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].profile.to_toml().unwrap(), web.to_toml().unwrap());
        assert_eq!(back[0].password.as_deref().map(String::as_str), Some("p@ss \"quoted\""));
        assert_eq!(back[1].profile.to_toml().unwrap(), serial.to_toml().unwrap());
        assert!(back[1].password.is_none());
        assert!(matches!(import_sites("wrong", &text), Err(VaultError::WrongPassword)));
    }
}
