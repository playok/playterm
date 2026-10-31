//! Terminal fonts: every font family installed on this PC (system and per-user fonts), or any
//! font file the user picks, with Malgun Gothic behind it for Hangul the chosen font lacks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use eframe::egui::{self, FontData, FontDefinitions};

use crate::view::bold_family;

/// One face of a font file (collections such as `.ttc` hold several).
#[derive(Clone, Debug, PartialEq)]
struct Face {
    path: PathBuf,
    index: u32,
}

/// An installed font family and the faces the terminal uses from it.
#[derive(Clone, Debug, PartialEq)]
pub struct FontChoice {
    /// Display name (the Korean name when the font has one).
    pub name: String,
    /// The name saved in the settings: the English family name, which also lets start-up
    /// find the font's files without scanning every font.
    pub key: String,
    /// Every name of the family, lower-case, for matching a saved setting.
    names: Vec<String>,
    /// Glyphs have one width, as a terminal needs.
    pub monospace: bool,
    regular: Face,
    bold: Option<Face>,
}

impl FontChoice {
    /// The name to show: Korean when the UI is Korean and the font has a Korean name.
    pub fn display(&self) -> &str {
        if playterm_i18n::english() { &self.key } else { &self.name }
    }

    pub fn matches(&self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        self.names.contains(&name)
    }
}

const LANG_KOREAN: u16 = 0x0412;
const LANG_ENGLISH_US: u16 = 0x0409;

/// What one face says about itself.
#[derive(Clone, Debug)]
struct FaceInfo {
    face: Face,
    /// (Windows language id, family name)
    families: Vec<(u16, String)>,
    weight: u16,
    italic: bool,
    monospace: bool,
}

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

/// Memory-maps a font file (read-only). Installed fonts are not rewritten in place, and
/// Windows refuses to truncate or delete a file while a mapping of it exists.
fn map(path: &Path) -> Option<memmap2::Mmap> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: see above; the mapping is only read.
    unsafe { memmap2::Mmap::map(&file) }.ok()
}

/// Reads the family names, weight, slant and pitch of every face in a font file.
fn read_faces(path: &Path, data: &[u8]) -> Vec<FaceInfo> {
    use ttf_parser::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    (0..count)
        .filter_map(|index| {
            let face = ttf_parser::Face::parse(data, index).ok()?;
            let names_with = |id: u16| -> Vec<(u16, String)> {
                let mut out: Vec<(u16, String)> = Vec::new();
                for n in face.names().into_iter().filter(|n| n.name_id == id) {
                    if let Some(s) = n.to_string().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
                        && !out.iter().any(|(_, o)| *o == s)
                    {
                        out.push((n.language_id, s));
                    }
                }
                out
            };
            // The typographic family groups weights like "Segoe UI Semibold" under "Segoe UI".
            let mut families = names_with(TYPOGRAPHIC_FAMILY);
            if families.is_empty() {
                families = names_with(FAMILY);
            }
            if families.is_empty() {
                return None;
            }
            let advance = |c: char| face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g));
            let monospace = face.is_monospaced()
                || matches!((advance('i'), advance('M'), advance('W')), (Some(a), Some(b), Some(c)) if a == b && b == c);
            Some(FaceInfo {
                face: Face { path: path.to_owned(), index },
                families,
                weight: face.weight().to_number(),
                italic: face.is_italic() || face.is_oblique(),
                monospace,
            })
        })
        .collect()
}

/// Groups faces into families and picks each family's regular and bold face.
fn families_from(faces: Vec<FaceInfo>) -> Vec<FontChoice> {
    let mut groups: HashMap<String, Vec<FaceInfo>> = HashMap::new();
    for info in faces {
        let english = info.families.iter().find(|(lang, _)| *lang == LANG_ENGLISH_US).unwrap_or(&info.families[0]);
        groups.entry(english.1.to_lowercase()).or_default().push(info);
    }
    let mut out: Vec<FontChoice> = groups
        .into_values()
        .filter_map(|faces| {
            let regular = faces.iter().min_by_key(|f| (f.italic, f.weight.abs_diff(400)))?;
            let bold = faces
                .iter()
                .filter(|f| !f.italic && f.weight >= 600 && f.weight > regular.weight)
                .min_by_key(|f| f.weight.abs_diff(700));
            let all = faces.iter().flat_map(|f| f.families.iter());
            let key = all.clone().find(|(lang, _)| *lang == LANG_ENGLISH_US).unwrap_or(&regular.families[0]).1.clone();
            let name = all
                .clone()
                .find(|(lang, _)| *lang == LANG_KOREAN)
                .or_else(|| all.clone().find(|(lang, _)| *lang == LANG_ENGLISH_US))
                .unwrap_or(&regular.families[0])
                .1
                .clone();
            let mut names: Vec<String> = all.map(|(_, n)| n.to_lowercase()).collect();
            names.sort();
            names.dedup();
            Some(FontChoice {
                name,
                key,
                names,
                monospace: regular.monospace,
                regular: regular.face.clone(),
                bold: bold.map(|b| b.face.clone()),
            })
        })
        .collect();
    // Fixed-width fonts first (they suit a terminal), then by name.
    out.sort_by_cached_key(|c| (!c.monospace, c.name.to_lowercase()));
    out
}

fn read_files(files: &[(String, PathBuf)]) -> Vec<FaceInfo> {
    files.iter().filter_map(|(_, path)| map(path).map(|data| read_faces(path, &data))).flatten().collect()
}

static FONTS: OnceLock<Vec<FontChoice>> = OnceLock::new();

/// Every font family installed on this PC, scanned once per run. Reading hundreds of font
/// files that are not in the disk cache yet takes about a second, so it is spread over threads.
pub fn available() -> &'static [FontChoice] {
    FONTS.get_or_init(|| {
        let files = installed_files(&font_dirs());
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        let chunk = files.len().div_ceil(threads).max(1);
        let faces = std::thread::scope(|scope| {
            let workers: Vec<_> = files.chunks(chunk).map(|part| scope.spawn(|| read_files(part))).collect();
            workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
        });
        families_from(faces)
    })
}

/// Lower-case letters and digits only: "Cascadia Mono" → "cascadiamono".
fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Looks for `font` only in files named like it ("D2Coding" → `D2Coding-Ver1.3.ttc`,
/// "Consolas" → `consola.ttf`), so start-up need not read every installed font.
fn find_by_file_name(font: &str) -> Option<FontChoice> {
    let wanted = squash(font);
    // Five letters: "GulimChe" is in gulim.ttc, "BatangChe" in batang.ttc.
    let prefix: String = wanted.chars().take(5).collect();
    if prefix.chars().count() < 4 {
        return None;
    }
    let files: Vec<(String, PathBuf)> =
        installed_files(&font_dirs()).into_iter().filter(|(name, _)| squash(name).starts_with(&prefix)).collect();
    families_from(read_files(&files)).into_iter().find(|c| c.matches(font))
}

/// Whether a font setting is a file path rather than a family name.
pub fn is_file(font: &str) -> bool {
    let lower = font.to_lowercase();
    font.contains(['\\', '/']) && [".ttf", ".otf", ".ttc"].iter().any(|ext| lower.ends_with(ext))
}

/// The family in a font file the user picked (its regular and bold faces).
fn from_file(path: &Path) -> Option<FontChoice> {
    let data = map(path)?;
    families_from(read_faces(path, &data)).into_iter().next()
}

/// The family a font setting names (any of its names, any language), or a font file.
pub fn find(font: &str) -> Option<FontChoice> {
    if is_file(font) {
        return from_file(Path::new(font));
    }
    if FONTS.get().is_none()
        && let Some(found) = find_by_file_name(font)
    {
        return Some(found);
    }
    available().iter().find(|c| c.matches(font)).cloned()
}

/// Embedded bitmap tables (monochrome glyph images for small sizes in fonts such as Gulim,
/// Batang or MS Gothic).
const BITMAP_TABLES: [&[u8; 4]; 3] = [b"EBDT", b"EBLC", b"EBSC"];

fn read_u16(data: &[u8], at: usize) -> Option<usize> {
    data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
}

fn read_u32(data: &[u8], at: usize) -> Option<usize> {
    data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// Where face `index`'s table directory starts (collections list one per face).
fn face_offset(data: &[u8], index: u32) -> Option<usize> {
    if data.starts_with(b"ttcf") {
        read_u32(data, 12 + 4 * index as usize)
    } else {
        (index == 0).then_some(0)
    }
}

/// egui draws glyphs from embedded bitmap tables like color emoji, in their own color, so
/// such text ignores the terminal colors. Returns a copy of the font in which face `index`'s
/// bitmap tables are renamed out of the way (the outlines are used instead), or `None` when
/// it has none.
fn hide_bitmap_tables(data: &[u8], index: u32) -> Option<Vec<u8>> {
    let dir = face_offset(data, index)?;
    let tables = read_u16(data, dir + 4)?;
    let records: Vec<usize> = (0..tables)
        .map(|i| dir + 12 + 16 * i)
        .filter(|&rec| data.get(rec..rec + 4).is_some_and(|tag| BITMAP_TABLES.iter().any(|t| tag == *t)))
        .collect();
    if records.is_empty() {
        return None;
    }
    let mut copy = data.to_vec();
    for rec in records {
        // Lower-case tags are not registered tables, so parsers skip them.
        copy[rec..rec + 4].make_ascii_lowercase();
    }
    Some(copy)
}

fn load(face: &Face) -> std::io::Result<FontData> {
    let index = face.index;
    match map(&face.path) {
        Some(data) => Ok(match hide_bitmap_tables(&data, index) {
            Some(copy) => FontData { index, ..FontData::from_owned(copy) },
            None => FontData::from_blob(Arc::new(data), index),
        }),
        None => std::fs::read(&face.path).map(|bytes| {
            let bytes = hide_bitmap_tables(&bytes, index).unwrap_or(bytes);
            FontData { index, ..FontData::from_owned(bytes) }
        }),
    }
}

/// Installs `font` (a family name or a font file) as the terminal font. Falls back to
/// Consolas, and says so, when it cannot be found or loaded.
pub fn install(ctx: &egui::Context, font: &str) -> Result<(), String> {
    let mut error = None;
    let choice = find(font).or_else(|| {
        error = Some(playterm_i18n::tr!(
            "'{font}' 글꼴을 찾을 수 없어 Consolas 를 씁니다",
            "Font '{font}' not found, using Consolas"
        ));
        find("Consolas")
    });

    let mut defs = FontDefinitions::default();
    let add = |defs: &mut FontDefinitions, key: &str, face: &Face| match load(face) {
        Ok(data) => {
            defs.font_data.insert(key.to_owned(), Arc::new(data));
            true
        }
        Err(e) => {
            log::warn!("font {} not available: {e}", face.path.display());
            false
        }
    };

    let mut regular = Vec::new();
    let mut bold = Vec::new();
    if let Some(c) = &choice {
        if add(&mut defs, "term", &c.regular) {
            regular.push("term".to_owned());
        } else {
            error = Some(playterm_i18n::tr!("글꼴을 불러올 수 없습니다: {font}", "Cannot load the font: {font}"));
        }
        if let Some(b) = &c.bold
            && add(&mut defs, "term-bold", b)
        {
            bold.push("term-bold".to_owned());
        }
    }
    // A font without a bold face draws bold text with its regular face: falling through to
    // Malgun Gothic Bold would give proportional Latin letters (and `\` drawn as `₩`).
    if bold.is_empty() {
        bold.extend(regular.iter().cloned());
    }
    // Hangul and other scripts the terminal font lacks.
    let files = installed_files(&font_dirs());
    let lookup = |name: &str| files.iter().find(|(n, _)| n == name).map(|(_, p)| Face { path: p.clone(), index: 0 });
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
    let builtin = defs.families.get(&egui::FontFamily::Monospace).cloned().unwrap_or_default();
    let mono: Vec<String> = regular.into_iter().chain(builtin).collect();
    let mono_bold: Vec<String> = bold.into_iter().chain(mono.iter().cloned()).collect();
    defs.families.insert(egui::FontFamily::Monospace, mono);
    defs.families.insert(bold_family(), mono_bold);
    ctx.set_fonts(defs);
    error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(file: &str, index: u32, families: &[(u16, &str)], weight: u16, italic: bool, mono: bool) -> FaceInfo {
        FaceInfo {
            face: Face { path: PathBuf::from(file), index },
            families: families.iter().map(|(l, n)| (*l, n.to_string())).collect(),
            weight,
            italic,
            monospace: mono,
        }
    }

    #[test]
    fn groups_faces_into_families() {
        let d2 = [(LANG_ENGLISH_US, "D2Coding")];
        let malgun = [(LANG_ENGLISH_US, "Malgun Gothic"), (LANG_KOREAN, "맑은 고딕")];
        let fonts = families_from(vec![
            info("d2.ttc", 0, &d2, 400, false, true),
            info("d2.ttc", 1, &d2, 700, false, true),
            info("malgunbd.ttf", 0, &malgun, 700, false, false),
            info("malgun.ttf", 0, &malgun, 400, false, false),
            info("arial-i.ttf", 0, &[(LANG_ENGLISH_US, "Arial")], 400, true, false),
            info("arial.ttf", 0, &[(LANG_ENGLISH_US, "Arial")], 400, false, false),
            info("thin.ttf", 0, &[(LANG_ENGLISH_US, "Thin")], 100, false, false),
        ]);
        let names: Vec<&str> = fonts.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["D2Coding", "Arial", "Thin", "맑은 고딕"], "monospace first, then by name");
        assert_eq!(fonts[3].key, "Malgun Gothic", "settings keep the English name");

        let d2 = &fonts[0];
        assert_eq!((d2.regular.index, d2.bold.as_ref().map(|b| b.index)), (0, Some(1)));
        let arial = &fonts[1];
        assert!(arial.regular.path.ends_with("arial.ttf"), "upright before italic");
        assert!(arial.bold.is_none());
        let malgun = &fonts[3];
        assert!(malgun.regular.path.ends_with("malgun.ttf"));
        assert!(malgun.bold.as_ref().unwrap().path.ends_with("malgunbd.ttf"));
        assert!(malgun.matches("Malgun Gothic") && malgun.matches("맑은 고딕") && !malgun.matches("Malgun"));
        assert!(fonts[2].bold.is_none(), "a lone thin face is the regular one");
    }

    #[test]
    fn reads_real_fonts() {
        let consolas = Path::new(r"C:\Windows\Fonts\consola.ttf");
        let Some(data) = map(consolas) else { return };
        let faces = read_faces(consolas, &data);
        assert_eq!(faces.len(), 1);
        assert!(faces[0].families.iter().any(|(_, n)| n == "Consolas"));
        assert!(faces[0].monospace && !faces[0].italic && faces[0].weight == 400);
        assert!(read_faces(Path::new("x"), b"not a font").is_empty());
    }

    #[test]
    #[ignore = "scans the installed fonts; run with --ignored --nocapture to see the timing"]
    fn scan_installed_fonts() {
        let start = std::time::Instant::now();
        let fonts = available();
        let mono = fonts.iter().filter(|f| f.monospace).count();
        println!("{} families ({mono} monospace) in {:?}", fonts.len(), start.elapsed());
        for f in fonts.iter().filter(|f| f.monospace) {
            println!("  {} (bold: {})", f.name, f.bold.is_some());
        }
        assert!(find("consolas").is_some() && find("맑은 고딕").is_some());
    }

    #[test]
    fn finds_by_file_name_without_scanning() {
        assert_eq!(squash("Cascadia Mono"), "cascadiamono");
        assert_eq!(squash("consola.ttf"), "consolattf");
        if Path::new(r"C:\Windows\Fonts\consola.ttf").exists() {
            let c = find_by_file_name("Consolas").expect("consola.ttf");
            assert!(c.regular.path.ends_with("consola.ttf") && c.bold.is_some());
        }
        if Path::new(r"C:\Windows\Fonts\gulim.ttc").exists() {
            let g = find_by_file_name("GulimChe").expect("in gulim.ttc");
            assert!(g.monospace && g.matches("굴림체"));
        }
        assert!(find_by_file_name("Nope Font").is_none());
        assert!(find_by_file_name("맑은 고딕").is_none(), "non-Latin names need the full scan");
    }

    /// A font file with one face whose tables have the given tags (no table contents).
    fn sfnt(tags: &[&[u8; 4]]) -> Vec<u8> {
        let mut data = vec![0, 1, 0, 0];
        data.extend((tags.len() as u16).to_be_bytes());
        data.extend([0; 6]);
        for tag in tags {
            data.extend(*tag);
            data.extend([0; 12]);
        }
        data
    }

    #[test]
    fn hides_embedded_bitmaps() {
        let plain = sfnt(&[b"cmap", b"glyf"]);
        assert!(hide_bitmap_tables(&plain, 0).is_none(), "nothing to hide: the mapping is used as is");

        let gulim = sfnt(&[b"EBDT", b"EBLC", b"glyf"]);
        let fixed = hide_bitmap_tables(&gulim, 0).unwrap();
        assert_eq!(&fixed[12..16], b"ebdt");
        assert_eq!(&fixed[28..32], b"eblc");
        assert_eq!(&fixed[44..48], b"glyf");
        assert_eq!(fixed.len(), gulim.len());

        // A collection: only the requested face's directory changes.
        let face = sfnt(&[b"EBDT"]);
        let mut ttc = b"ttcf".to_vec();
        ttc.extend([0, 1, 0, 0]);
        ttc.extend(2u32.to_be_bytes());
        let first = 20u32;
        let second = first + face.len() as u32;
        ttc.extend(first.to_be_bytes());
        ttc.extend(second.to_be_bytes());
        ttc.extend(&face);
        ttc.extend(&face);
        let fixed = hide_bitmap_tables(&ttc, 1).unwrap();
        let tag = |at: u32| &fixed[at as usize + 12..at as usize + 16];
        assert_eq!((tag(first), tag(second)), (&b"EBDT"[..], &b"ebdt"[..]));
        assert!(hide_bitmap_tables(&ttc, 5).is_none(), "no such face");

        // The real Gulim, when installed, has bitmaps to hide and still parses afterwards.
        if let Ok(data) = std::fs::read(r"C:\Windows\Fonts\gulim.ttc") {
            let fixed = hide_bitmap_tables(&data, 1).expect("GulimChe has embedded bitmaps");
            let face = ttf_parser::Face::parse(&fixed, 1).unwrap();
            assert!(face.glyph_index('가').is_some());
        }
    }

    #[test]
    fn font_paths() {
        assert!(is_file(r"C:\Users\me\fonts\Hack-Regular.ttf"));
        assert!(is_file("D:/fonts/x.OTF"));
        assert!(!is_file("Consolas"));
        assert!(!is_file("my.ttf"), "a bare name is not a path");
    }
}
