//! Resource-based UI translations. Add a locale TOML file to `locales/` to add a language.
//! The build validates catalogs and generates language choices and checked message macros.
//!
//! ```
//! use playterm_i18n::{Language, set_language, t, tr};
//! set_language(Language::ENGLISH);
//! assert_eq!(t!("common.close"), "Close");
//! assert_eq!(tr!("app.broadcast.typing_into_all_tabs_other", others = 3),
//!            "Typing into all tabs · 3 other");
//! ```
//!
//! Unknown keys, missing arguments, and extra arguments are compile errors:
//!
//! ```compile_fail
//! playterm_i18n::t!("this.key.does_not_exist");
//! ```
//! ```compile_fail
//! playterm_i18n::tr!("app.broadcast.typing_into_all_tabs_other");
//! ```
//! ```compile_fail
//! playterm_i18n::tr!("app.broadcast.typing_into_all_tabs_other", others = 3, extra = 1);
//! ```

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

// Catalogs, including available languages and macro signatures, are generated together.
include!(concat!(env!("OUT_DIR"), "/catalog.rs"));

struct Locale {
    code: &'static str,
    name: &'static str,
    aliases: &'static [&'static str],
}

#[derive(Clone, Copy, Debug)]
enum Part {
    Text(&'static str),
    Argument(usize),
}

/// A language preference. IDs are internal; settings always serialize stable locale codes.
/// `ALL` includes System followed by every catalog, with English first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Language(usize);

impl Language {
    pub fn code(self) -> &'static str {
        if self == Self::SYSTEM { "system" } else { LOCALES[self.0 - 1].code }
    }

    /// Native language names come from the resource metadata and never require UI changes.
    pub fn name(self) -> &'static str {
        if self == Self::SYSTEM { t!("language.follow_system") } else { LOCALES[self.0 - 1].name }
    }

    /// Resolve BCP 47 / POSIX locale names, preferring the most specific catalog or alias.
    /// For example zh-Hant-TW tries zh-hant-tw, then zh-hant, then zh. English is the fallback.
    pub fn from_locale(locale: &str) -> Self {
        Self::from_registry(locale, LOCALES).unwrap_or(Self::ENGLISH)
    }

    fn from_registry(locale: &str, registry: &[Locale]) -> Option<Self> {
        let mut code = locale.trim().split(['.', '@']).next().unwrap_or_default().replace('_', "-").to_ascii_lowercase();
        loop {
            if let Some(index) = registry.iter().position(|locale| locale.code == code || locale.aliases.contains(&code.as_str())) {
                return Some(Self(index + 1));
            }
            let index = code.rfind('-')?;
            code.truncate(index);
        }
    }

    pub fn resolve(self) -> Self {
        static SYSTEM: OnceLock<Language> = OnceLock::new();
        if self == Self::SYSTEM { *SYSTEM.get_or_init(|| Self::from_locale(&sys_locale::get_locale().unwrap_or_default())) } else { self }
    }
}

impl Serialize for Language {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        // An unavailable language (for example after a downgrade) must not corrupt settings.
        Ok(if code == "system" { Self::SYSTEM } else { Self::from_locale(&code) })
    }
}

// Preserve the historical library default; the application applies its saved preference
// (System for new installations) before displaying any UI.
static LANGUAGE: AtomicUsize = AtomicUsize::new(Language::KOREAN.0);

pub fn set_language(language: Language) {
    LANGUAGE.store(language.resolve().0, Ordering::Relaxed);
}

pub fn language() -> Language {
    Language(LANGUAGE.load(Ordering::Relaxed))
}

/// Implementation detail for the checked `t!` macro. Prefer the macro at call sites.
#[doc(hidden)]
pub fn __text(message: usize) -> &'static str {
    TEXTS[language().0 - 1][message]
}

/// Implementation detail for the checked `tr!` macro. Arguments are evaluated once and
/// inserted as text, never parsed as formatting or translation syntax.
#[doc(hidden)]
pub fn __format(message: usize, args: &[String]) -> String {
    render(FORMATS[language().0 - 1][message], args)
}

fn render(parts: &[Part], args: &[String]) -> String {
    let capacity = parts
        .iter()
        .map(|part| match part {
            Part::Text(text) => text.len(),
            Part::Argument(index) => args[*index].len(),
        })
        .sum();
    let mut text = String::with_capacity(capacity);
    for part in parts {
        match part {
            Part::Text(literal) => text.push_str(literal),
            Part::Argument(index) => text.push_str(&args[*index]),
        }
    }
    text
}

#[cfg(test)]
#[path = "../catalog.rs"]
mod catalog;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_locale_names_and_fallback() {
        for locale in ["ko", "ko-KR", "ko_KR.UTF-8", "KO_kr@variant"] {
            assert_eq!(Language::from_locale(locale), Language::KOREAN);
        }
        for locale in ["en-US", "en_GB.UTF-8", "C", "POSIX", "", "unsupported"] {
            assert_eq!(Language::from_locale(locale), Language::ENGLISH);
        }
        let registry = [
            Locale { code: "en", name: "English", aliases: &[] },
            Locale { code: "zh", name: "中文", aliases: &[] },
            Locale { code: "zh-hant", name: "繁體中文", aliases: &["zh-tw"] },
        ];
        assert_eq!(Language::from_registry("zh_Hant_TW.UTF-8", &registry), Some(Language(3)));
        assert_eq!(Language::from_registry("zh-TW", &registry), Some(Language(3)));
        assert_eq!(Language::from_registry("zh-CN", &registry), Some(Language(2)));
        assert_ne!(Language::SYSTEM.resolve(), Language::SYSTEM);
        for language in Language::ALL.into_iter().skip(1) {
            assert_eq!(Language::from_locale(language.code()), language);
            assert_eq!(language.resolve(), language);
        }
    }

    #[test]
    fn renders_reordered_repeated_arguments_without_reinterpreting_values() {
        let args = ["{arg1} 🐱".into(), "서울".into()];
        let parts = [Part::Argument(1), Part::Text(" / "), Part::Argument(0), Part::Text(" / "), Part::Argument(1)];
        assert_eq!(render(&parts, &args), "서울 / {arg1} 🐱 / 서울");
    }

    #[test]
    fn serialized_preferences_use_codes_not_generated_indices() {
        #[derive(Serialize, Deserialize)]
        struct Settings {
            language: Language,
        }
        for language in Language::ALL {
            let text = toml::to_string(&Settings { language }).unwrap();
            assert!(text.contains(&format!("\"{}\"", language.code())));
            assert_eq!(toml::from_str::<Settings>(&text).unwrap().language, language);
        }
        assert_eq!(toml::from_str::<Settings>("language = 'missing-locale'").unwrap().language, Language::ENGLISH);
    }

    // Only this test mutates process-wide language state.
    #[test]
    fn switches_language_and_checks_generated_macros() {
        set_language(Language::ENGLISH);
        assert_eq!(t!("common.close"), "Close");
        assert_eq!(tr!("app.broadcast.typing_into_all_tabs_other", others = 3), "Typing into all tabs · 3 other");
        let count = std::cell::Cell::new(0);
        let _ = tr!(
            "app.broadcast.typing_into_all_tabs_other",
            others = {
                count.set(count.get() + 1);
                count.get()
            }
        );
        assert_eq!(count.get(), 1);
        set_language(Language::KOREAN);
        assert_eq!(t!("common.close"), "닫기");
    }

    fn catalogs() -> Vec<catalog::Catalog> {
        [include_str!("../locales/en.toml"), include_str!("../locales/ko.toml")]
            .into_iter()
            .map(|text| toml::from_str(text).unwrap())
            .collect()
    }

    #[test]
    fn new_catalog_generates_language_and_english_fallback_without_rust_changes() {
        let mut catalogs = catalogs();
        catalogs.push(
            toml::from_str(
                r#"
[locale]
code = "fr"
name = "Français"
aliases = ["fr-FR"]
[text]
"common.close" = "Fermer"
[format]
"app.broadcast.typing_into_all_tabs_other" = "{others} autres onglets"
"#,
            )
            .unwrap(),
        );
        let generated = catalog::generate(&catalogs).unwrap();
        assert!(generated.contains("pub const ALL: [Self; 4]"));
        assert!(generated.contains("Fermer"));
        assert!(generated.contains("fr-fr"));
        // Absent French translations use the English value in the generated table.
        assert_eq!(generated.matches("Choose where to connect.").count(), 2);
        // Korean must remain fully translated during this migration.
        assert_eq!(catalogs[0].text.keys().collect::<Vec<_>>(), catalogs[1].text.keys().collect::<Vec<_>>());
        assert_eq!(catalogs[0].format.keys().collect::<Vec<_>>(), catalogs[1].format.keys().collect::<Vec<_>>());
    }

    #[test]
    fn rejects_bad_catalogs_at_build_time() {
        let mut values = catalogs();
        values[1].text.insert("typo.key".into(), "오타".into());
        assert!(catalog::generate(&values).unwrap_err().contains("unknown text key"));
        let mut values = catalogs();
        values[1].format.insert("app.broadcast.typing_into_all_tabs_other".into(), "{wrong}".into());
        assert!(catalog::generate(&values).unwrap_err().contains("arguments differ"));
        let mut values = catalogs();
        values[1].locale.aliases.push("en-US".into());
        values[0].locale.aliases.push("EN_us".into());
        assert!(catalog::generate(&values).unwrap_err().contains("duplicate locale"));
        let mut values = catalogs();
        values[1].text.insert("common.close".into(), String::new());
        assert!(catalog::generate(&values).unwrap_err().contains("empty translation"));
        for invalid in ["{", "}", "{}", "{0}", "{name:?}", "{a{b}"] {
            assert!(catalog::parse_format(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            catalog::parse_format("{{{name}}}").unwrap(),
            vec![catalog::Part::Text("{".into()), catalog::Part::Argument("name".into()), catalog::Part::Text("}".into()),]
        );
    }
}
