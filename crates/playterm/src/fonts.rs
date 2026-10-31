//! Terminal fonts: a known list of coding fonts found on this PC (or any font file the user
//! picks), with Malgun Gothic behind it for Hangul the chosen font lacks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

use crate::view::bold_family;

/// One face of a font file (collections such as `.ttc` hold several).
#[derive(Clone, Debug, PartialEq)]
struct Face {
    path: PathBuf,
    index: u32,
}

impl Face {
    fn new(path: PathBuf) -> Self {
        Self { path, index: 0 }
    }
}

/// A terminal font that can be used: its regular and (if present) bold face.
#[derive(Clone, Debug, PartialEq)]
pub struct FontChoice {
    pub name: String,
    regular: Face,
    bold: Option<Face>,
}

impl FontChoice {
    /// A font file the user picked; a collection's own Regular/Bold faces are used.
    fn from_file(path: PathBuf) -> Self {
        let mut choice = Self { name: path.display().to_string(), regular: Face::new(path), bold: None };
        choice.use_collection_faces();
        choice
    }

    /// For a font collection (`.ttc`), picks its Regular and Bold faces by their names
    /// instead of assuming face 0 and a separate bold file.
    fn use_collection_faces(&mut self) {
        let Ok(file) = std::fs::File::open(&self.regular.path) else { return };
        // SAFETY: read-only mapping of an installed font, see `load`.
        let Ok(data) = (unsafe { memmap2::Mmap::map(&file) }) else { return };
        let Some((regular, bold)) = collection_faces(&data) else { return };
        let path = self.regular.path.clone();
        self.regular = Face { path: path.clone(), index: regular };
        if let Some(index) = bold {
            self.bold = Some(Face { path, index });
        }
    }
}

/// The Regular and Bold face indexes of a font collection, preferring faces whose family is
/// not a ligature variant. `None` for a single-face file.
fn collection_faces(data: &[u8]) -> Option<(u32, Option<u32>)> {
    let count = ttf_parser::fonts_in_collection(data)?;
    let name = |face: &ttf_parser::Face, ids: [u16; 2]| {
        ids.iter().find_map(|&id| face.names().into_iter().find(|n| n.name_id == id).and_then(|n| n.to_string()))
    };
    use ttf_parser::name_id::{FAMILY, SUBFAMILY, TYPOGRAPHIC_FAMILY, TYPOGRAPHIC_SUBFAMILY};
    let faces: Vec<(u32, String, String)> = (0..count)
        .filter_map(|i| {
            let face = ttf_parser::Face::parse(data, i).ok()?;
            let family = name(&face, [TYPOGRAPHIC_FAMILY, FAMILY]).unwrap_or_default().to_lowercase();
            let style = name(&face, [TYPOGRAPHIC_SUBFAMILY, SUBFAMILY]).unwrap_or_default().to_lowercase();
            Some((i, family, style))
        })
        .collect();
    let plain: Vec<&(u32, String, String)> = faces.iter().filter(|(_, f, _)| !f.contains("ligature")).collect();
    let pool = if plain.is_empty() { faces.iter().collect() } else { plain };
    let regular = pool.iter().find(|(_, _, s)| s == "regular").or(pool.first())?;
    let bold = pool.iter().find(|(_, f, s)| s == "bold" && *f == regular.1).or(pool.iter().find(|(_, _, s)| s == "bold"));
    Some((regular.0, bold.map(|b| b.0)))
}

/// How to find a known font among the installed font files (lower-case file names).
struct Known {
    name: &'static str,
    regular: fn(&str) -> bool,
    bold: fn(&str) -> bool,
}

const KNOWN: &[Known] = &[
    Known { name: "Consolas", regular: |f| f == "consola.ttf", bold: |f| f == "consolab.ttf" },
    Known { name: "Cascadia Mono", regular: |f| f == "cascadiamono.ttf", bold: |_| false },
    Known { name: "Cascadia Code", regular: |f| f == "cascadiacode.ttf", bold: |_| false },
    Known {
        name: "D2Coding",
        regular: |f| f.starts_with("d2coding") && !f.contains("bold") && !f.contains("ligature"),
        bold: |f| f.starts_with("d2coding") && f.contains("bold") && !f.contains("ligature"),
    },
    Known {
        name: "나눔고딕코딩",
        regular: |f| f == "nanumgothiccoding.ttf",
        bold: |f| f == "nanumgothiccoding-bold.ttf",
    },
    Known {
        name: "JetBrains Mono",
        regular: |f| f == "jetbrainsmono-regular.ttf",
        bold: |f| f == "jetbrainsmono-bold.ttf",
    },
    Known { name: "Lucida Console", regular: |f| f == "lucon.ttf", bold: |_| false },
    Known { name: "Courier New", regular: |f| f == "cour.ttf", bold: |f| f == "courbd.ttf" },
];

/// System fonts, then fonts installed for the current user only.
fn font_dirs() -> Vec<PathBuf> {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    let mut dirs = vec![PathBuf::from(windir).join("Fonts")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
    }
    dirs
}

/// Installed font files by lower-case file name, sorted so the first match is predictable.
fn installed_files(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().to_lowercase(), e.path()))
        .filter(|(name, _)| name.ends_with(".ttf") || name.ends_with(".otf") || name.ends_with(".ttc"))
        .collect();
    files.sort();
    files
}

fn choices_in(files: &[(String, PathBuf)]) -> Vec<FontChoice> {
    KNOWN
        .iter()
        .filter_map(|k| {
            let find = |pred: fn(&str) -> bool| files.iter().find(|(n, _)| pred(n)).map(|(_, p)| Face::new(p.clone()));
            Some(FontChoice { name: k.name.to_owned(), regular: find(k.regular)?, bold: find(k.bold) })
        })
        .collect()
}

/// The known coding fonts installed on this PC.
pub fn available() -> Vec<FontChoice> {
    let mut choices = choices_in(&installed_files(&font_dirs()));
    for c in &mut choices {
        if c.bold.is_none() {
            c.use_collection_faces();
        }
    }
    choices
}

/// Whether a font setting is a file path rather than a name from the list.
pub fn is_file(font: &str) -> bool {
    let lower = font.to_lowercase();
    font.contains(['\\', '/']) && [".ttf", ".otf", ".ttc"].iter().any(|ext| lower.ends_with(ext))
}

/// Memory-maps a font file instead of copying it to the heap: Malgun Gothic alone is ~12 MB
/// per weight, and mapped pages are shared with the system and only loaded when used.
fn load(path: &Path, index: u32) -> std::io::Result<FontData> {
    let file = std::fs::File::open(path)?;
    // SAFETY: the mapping is read-only. Windows refuses to truncate or delete a file while a
    // mapping of it exists, and installed fonts are not rewritten in place, so the bytes
    // cannot change underneath us.
    match unsafe { memmap2::Mmap::map(&file) } {
        Ok(map) => Ok(FontData::from_blob(Arc::new(map), index)),
        Err(_) => std::fs::read(path).map(|bytes| FontData { index, ..FontData::from_owned(bytes) }),
    }
}

/// Installs `font` (a name from [`available`] or a font file) as the terminal font. Falls
/// back to Consolas, and says so, when it cannot be loaded.
pub fn install(ctx: &egui::Context, font: &str) -> Result<(), String> {
    let files = installed_files(&font_dirs());
    let choices = available();
    let consolas = || choices.iter().find(|c| c.name == "Consolas").cloned();
    let (choice, mut error) = if is_file(font) {
        (Some(FontChoice::from_file(PathBuf::from(font))), None)
    } else {
        match choices.iter().find(|c| c.name == font) {
            Some(c) => (Some(c.clone()), None),
            None => (consolas(), Some(format!("'{font}' 글꼴이 설치되어 있지 않아 Consolas 를 씁니다"))),
        }
    };

    let mut defs = FontDefinitions::default();
    let mut loaded = HashMap::new();
    let mut add = |defs: &mut FontDefinitions, key: &str, face: &Face| -> bool {
        if loaded.contains_key(key) {
            return true;
        }
        match load(&face.path, face.index) {
            Ok(data) => {
                defs.font_data.insert(key.to_owned(), Arc::new(data));
                loaded.insert(key.to_owned(), ());
                true
            }
            Err(e) => {
                log::warn!("font {} not available: {e}", face.path.display());
                false
            }
        }
    };

    let mut regular = Vec::new();
    let mut bold = Vec::new();
    let main_ok = choice.as_ref().is_some_and(|c| add(&mut defs, "term", &c.regular));
    if main_ok {
        regular.push("term".to_owned());
        if let Some(b) = choice.as_ref().and_then(|c| c.bold.as_ref())
            && add(&mut defs, "term-bold", b)
        {
            bold.push("term-bold".to_owned());
        }
    } else {
        error = Some(format!("글꼴을 불러올 수 없어 Consolas 를 씁니다: {font}"));
        if let Some(c) = consolas() {
            if add(&mut defs, "term", &c.regular) {
                regular.push("term".to_owned());
            }
            if let Some(b) = &c.bold
                && add(&mut defs, "term-bold", b)
            {
                bold.push("term-bold".to_owned());
            }
        }
    }
    // A font without a bold face draws bold text with its regular face: falling through to
    // Malgun Gothic Bold would give proportional Latin letters (and `\` drawn as `₩`).
    if bold.is_empty() {
        bold.extend(regular.iter().cloned());
    }
    // Hangul and other scripts the terminal font lacks.
    let lookup = |name: &str| files.iter().find(|(n, _)| n == name).map(|(_, p)| Face::new(p.clone()));
    if let Some(f) = lookup("malgun.ttf")
        && add(&mut defs, "malgun", &f)
    {
        regular.push("malgun".to_owned());
    }
    if let Some(f) = lookup("malgunbd.ttf")
        && add(&mut defs, "malgun-bold", &f)
    {
        bold.push("malgun-bold".to_owned());
    }

    // egui's built-in monospace fonts stay as the last fallback (symbols, emoji).
    let builtin = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let mono: Vec<String> = regular.into_iter().chain(builtin).collect();
    let mono_bold: Vec<String> = bold.into_iter().chain(mono.iter().cloned()).collect();
    defs.families.insert(FontFamily::Monospace, mono);
    defs.families.insert(bold_family(), mono_bold);
    ctx.set_fonts(defs);
    error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(names: &[&str]) -> Vec<(String, PathBuf)> {
        names.iter().map(|n| (n.to_lowercase(), PathBuf::from(format!(r"C:\Fonts\{n}")))).collect()
    }

    #[test]
    fn finds_known_fonts() {
        let found = choices_in(&files(&[
            "consola.ttf",
            "consolab.ttf",
            "D2Coding-Ver1.3.2-20180524.ttf",
            "D2CodingBold-Ver1.3.2-20180524.ttf",
            "D2Coding-Ver1.3.2-20180524-ligature.ttf",
            "malgun.ttf",
            "CascadiaMono.ttf",
        ]));
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Consolas", "Cascadia Mono", "D2Coding"]);
        let d2 = &found[2];
        assert!(d2.regular.path.ends_with("D2Coding-Ver1.3.2-20180524.ttf"));
        assert!(d2.bold.as_ref().unwrap().path.ends_with("D2CodingBold-Ver1.3.2-20180524.ttf"));
        assert!(found[1].bold.is_none());
    }

    #[test]
    fn single_fonts_are_not_collections() {
        let consolas = Path::new(r"C:\Windows\Fonts\consola.ttf");
        if let Ok(data) = std::fs::read(consolas) {
            assert_eq!(collection_faces(&data), None);
        }
        assert_eq!(collection_faces(b"not a font"), None);
    }

    #[test]
    fn font_paths() {
        assert!(is_file(r"C:\Users\me\fonts\Hack-Regular.ttf"));
        assert!(is_file("D:/fonts/x.OTF"));
        assert!(!is_file("Consolas"));
        assert!(!is_file("my.ttf"), "a bare name is not a path");
    }
}
