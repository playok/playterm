//! Build-time catalog validation and code generation, shared with the tests.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Locale {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub locale: Locale,
    #[serde(default)]
    pub text: BTreeMap<String, String>,
    #[serde(default)]
    pub format: BTreeMap<String, String>,
}

#[derive(Debug, PartialEq)]
pub enum Part {
    Text(String),
    Argument(String),
}

/// Only named arguments are allowed in translations. Rust formatting belongs at the call
/// site, so translators can reorder arguments without changing their types or formatting.
pub fn parse_format(template: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' if chars.peek() == Some(&c) => {
                chars.next();
                literal.push(c);
            }
            '{' => {
                if !literal.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut literal)));
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err("unclosed argument".into()),
                    }
                }
                if !identifier(&name) {
                    return Err(format!("invalid argument {{{name}}}; use a named argument without a format specifier"));
                }
                parts.push(Part::Argument(name));
            }
            '}' => return Err("unmatched closing brace; use }} for a literal brace".into()),
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        parts.push(Part::Text(literal));
    }
    Ok(parts)
}

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_') && name != "_"
}

fn arguments(parts: &[Part]) -> BTreeSet<&str> {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Argument(name) => Some(name.as_str()),
            Part::Text(_) => None,
        })
        .collect()
}

pub fn normalize_locale(code: &str) -> String {
    code.trim().split(['.', '@']).next().unwrap_or_default().replace('_', "-").to_ascii_lowercase()
}

fn valid_locale(code: &str) -> bool {
    !code.is_empty() && code.split('-').all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_alphanumeric()))
}

pub fn generate(catalogs: &[Catalog]) -> Result<String, String> {
    let base = catalogs.iter().find(|catalog| catalog.locale.code == "en").ok_or("missing English catalog (en)")?;
    if !catalogs.iter().any(|catalog| catalog.locale.code == "ko") {
        return Err("missing Korean catalog (ko) required for existing preferences".into());
    }
    if !base.text.contains_key("language.follow_system") {
        return Err("missing language.follow_system".into());
    }
    let mut ordered: Vec<_> = catalogs.iter().collect();
    ordered.sort_by_key(|catalog| (catalog.locale.code != "en", &catalog.locale.code));
    let mut locale_ids = BTreeSet::new();
    let mut all_keys = BTreeSet::new();
    for key in base.text.keys().chain(base.format.keys()) {
        if !key.split('.').all(identifier) || !all_keys.insert(key) {
            return Err(format!("invalid or duplicate message key: {key}"));
        }
    }
    for catalog in &ordered {
        let code = &catalog.locale.code;
        if normalize_locale(code) != *code || !valid_locale(code) || code == "system" || catalog.locale.name.trim().is_empty() {
            return Err(format!("invalid locale metadata: {code}"));
        }
        for alias in std::iter::once(code).chain(catalog.locale.aliases.iter()) {
            let normalized = normalize_locale(alias);
            if !valid_locale(&normalized) || normalized == "system" || !locale_ids.insert(normalized) {
                return Err(format!("invalid or duplicate locale/alias: {alias}"));
            }
        }
        for (kind, values, source) in [("text", &catalog.text, &base.text), ("format", &catalog.format, &base.format)] {
            for (key, value) in values {
                let original = source.get(key).ok_or_else(|| format!("{code}: unknown {kind} key {key}"))?;
                if value.is_empty() && !original.is_empty() {
                    return Err(format!("{code}: empty translation for {key}; omit it to use English"));
                }
                if kind == "format" {
                    let expected = parse_format(original).map_err(|e| format!("en:{key}: {e}"))?;
                    let actual = parse_format(value).map_err(|e| format!("{code}:{key}: {e}"))?;
                    if arguments(&expected) != arguments(&actual) {
                        return Err(format!(
                            "{code}:{key}: arguments differ from English: {:?} != {:?}",
                            arguments(&actual),
                            arguments(&expected)
                        ));
                    }
                }
            }
        }
    }

    let mut out = String::from("// Generated from locales/*.toml; do not edit.\n");
    out.push_str("static LOCALES: &[Locale] = &[\n");
    for catalog in &ordered {
        let aliases: Vec<_> = catalog.locale.aliases.iter().map(|a| normalize_locale(a)).collect();
        writeln!(out, "Locale {{ code: {:?}, name: {:?}, aliases: &{:?} }},", catalog.locale.code, catalog.locale.name, aliases).unwrap();
    }
    out.push_str("];\nimpl Language {\n");
    out.push_str("pub const SYSTEM: Self = Self(0);\npub const ENGLISH: Self = Self(1);\n");
    writeln!(out, "pub const KOREAN: Self = Self({});", ordered.iter().position(|c| c.locale.code == "ko").unwrap() + 1).unwrap();
    writeln!(out, "pub const ALL: [Self; {}] = [", ordered.len() + 1).unwrap();
    for index in 0..=ordered.len() {
        writeln!(out, "Self({index}),").unwrap();
    }
    out.push_str("];\n}\nstatic TEXTS: &[&[&str]] = &[\n");
    for catalog in &ordered {
        out.push_str("&[\n");
        for (key, value) in &base.text {
            writeln!(out, "{:?},", catalog.text.get(key).unwrap_or(value)).unwrap();
        }
        out.push_str("],\n");
    }
    out.push_str("];\nstatic FORMATS: &[&[&[Part]]] = &[\n");
    for catalog in &ordered {
        out.push_str("&[\n");
        for (key, value) in &base.format {
            let source = parse_format(value)?;
            let args: Vec<_> = arguments(&source).into_iter().collect();
            out.push_str("&[");
            for part in parse_format(catalog.format.get(key).unwrap_or(value))? {
                match part {
                    Part::Text(text) => write!(out, "Part::Text({text:?}),").unwrap(),
                    Part::Argument(name) => write!(out, "Part::Argument({}),", args.iter().position(|arg| *arg == name).unwrap()).unwrap(),
                }
            }
            out.push_str("],\n");
        }
        out.push_str("],\n");
    }
    out.push_str("];\n");
    out.push_str("/// Look up a static message by its catalog key. Unknown keys fail to compile.\n#[macro_export]\nmacro_rules! t {\n");
    for (index, key) in base.text.keys().enumerate() {
        writeln!(out, "({key:?} $(,)?) => {{ $crate::__text({index}) }};").unwrap();
    }
    out.push_str("($($invalid:tt)*) => { compile_error!(\"unknown static translation key; add it to locales/en.toml [text]\") };\n}\n");
    out.push_str("/// Format a catalog message using explicit named arguments. Unknown keys and missing or extra arguments fail to compile.\n#[macro_export]\nmacro_rules! tr {\n");
    for (index, (key, value)) in base.format.iter().enumerate() {
        let parts = parse_format(value)?;
        let args = arguments(&parts);
        write!(out, "({key:?}").unwrap();
        for (i, name) in args.iter().enumerate() {
            write!(out, ", {name} = $arg{i}:expr").unwrap();
        }
        write!(out, " $(,)?) => {{ $crate::__format({index}, &[").unwrap();
        for i in 0..args.len() {
            write!(out, "::std::format!(\"{{}}\", $arg{i}),").unwrap();
        }
        out.push_str("]) };\n");
    }
    out.push_str("($($invalid:tt)*) => { compile_error!(\"unknown formatted translation key or incorrect arguments; use the named arguments in locales/en.toml in alphabetical order\") };\n}\n");
    Ok(out)
}
