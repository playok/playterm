//! User-visible text in Korean or English.
//!
//! Every message is written at its call site in both languages; the language chosen in the
//! settings picks one at run time:
//!
//! ```
//! use playterm_i18n::tr;
//! let name = "web01";
//! let text = tr!("'{name}' 사이트를 삭제할까요?", "Delete site '{name}'?");
//! assert_eq!(text, "'web01' 사이트를 삭제할까요?");
//! ```

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// Shows English text from now on (Korean otherwise, the default).
pub fn set_english(on: bool) {
    ENGLISH.store(on, Ordering::Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// The text for the current language, for places that need a `&'static str`.
pub fn t(ko: &'static str, en: &'static str) -> &'static str {
    if english() { en } else { ko }
}

/// Formats the Korean or the English text (`format!` syntax, inline arguments allowed) for
/// the current language.
#[macro_export]
macro_rules! tr {
    ($ko:literal, $en:literal $(,)?) => {
        if $crate::english() { ::std::format!($en) } else { ::std::format!($ko) }
    };
    ($ko:literal, $en:literal, $($arg:tt)+) => {
        if $crate::english() { ::std::format!($en, $($arg)+) } else { ::std::format!($ko, $($arg)+) }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test only: the language is global and tests run in parallel.
    #[test]
    fn switches_language() {
        let n = 3;
        assert_eq!(tr!("사이트 {n}개", "{n} sites"), "사이트 3개");
        assert_eq!(t("닫기", "Close"), "닫기");
        set_english(true);
        assert_eq!(tr!("사이트 {n}개", "{n} sites"), "3 sites");
        assert_eq!(tr!("{}개", "{} items", n + 1), "4 items");
        assert_eq!(t("닫기", "Close"), "Close");
        set_english(false);
    }
}
