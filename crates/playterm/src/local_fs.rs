//! Local file commands of the prompt: `ls`, `dir`, `cd`, `pwd`.

use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use playterm_i18n::{t, tr};
use unicode_width::UnicodeWidthStr;

use crate::console::pad;
use crate::profile_form::expand_home;

const DIR_COLOR: &str = "\x1b[1;34m";
const RESET: &str = "\x1b[0m";

/// The Downloads folder of the user (their home if Windows does not know it).
pub fn downloads() -> PathBuf {
    directories::UserDirs::new().and_then(|d| d.download_dir().map(Path::to_owned)).unwrap_or_else(|| home().join("Downloads"))
}

/// Where ZMODEM downloads are saved: the folder of the setting, or Downloads when it is empty.
pub fn zmodem_folder(setting: &str) -> PathBuf {
    match setting.trim() {
        "" => downloads(),
        dir => expand_home(dir),
    }
}

/// Starting folder of a prompt: the user's home.
pub fn home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_owned())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }))
}

/// `~\Documents` (Windows) or `~/Documents` style display of a folder (for the prompt).
pub fn display(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Resolves `arg` against `cwd`: `~` and `~\...` (home), drive letters (`D:`), absolute and
/// relative paths, with `.` and `..` removed (no `\\?\` prefix like `canonicalize`). Other
/// names that start with `~` (`~backup`, `~$doc.docx`) are ordinary relative names.
pub fn resolve(cwd: &Path, arg: &str) -> PathBuf {
    let arg = arg.trim().trim_matches('"');
    let home_relative = arg == "~" || arg.starts_with("~\\") || arg.starts_with("~/");
    let joined = if home_relative {
        expand_home(arg)
    } else if cfg!(windows) && arg.len() == 2 && arg.ends_with(':') {
        // A bare drive means its root.
        PathBuf::from(format!("{arg}\\"))
    } else {
        cwd.join(arg)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pop past the root.
                if out.file_name().is_some() {
                    out.pop();
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// `cd`: the new folder, or why not.
pub fn change_dir(cwd: &Path, arg: Option<&str>) -> Result<PathBuf, String> {
    let target = match arg {
        None => home(),
        Some(a) => resolve(cwd, a),
    };
    if target.is_dir() {
        Ok(target)
    } else if target.exists() {
        Err(tr!("cd: 폴더가 아닙니다: {}", "cd: not a folder: {}", target.display()))
    } else {
        Err(tr!("cd: 경로를 찾을 수 없습니다: {}", "cd: path not found: {}", target.display()))
    }
}

/// Case-insensitive `*` / `?` match.
fn wildcard(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) => a == b && go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    go(&p, &n)
}

struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
}

fn is_hidden(name: &str, meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        if meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
            return true;
        }
    }
    #[cfg(not(windows))]
    let _ = meta;
    name.starts_with('.')
}

/// Options of `ls` / `dir`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ListOptions {
    /// Details: time, size (`ls -l`, `dir`).
    pub long: bool,
    /// Include hidden files (`-a`, `dir /a`).
    pub all: bool,
    pub path: Option<String>,
}

/// The `ls` / `dir` listing of `opts.path` (default `cwd`), laid out for `cols` columns.
pub fn list(cwd: &Path, opts: &ListOptions, cols: usize) -> Result<String, String> {
    let arg = opts.path.as_deref().unwrap_or(".");
    let mut target = resolve(cwd, arg);
    // A wildcard in the last part filters that folder: `dir *.log`.
    let mut pattern = None;
    if let Some(name) = target.file_name().and_then(|n| n.to_str())
        && name.contains(['*', '?'])
    {
        pattern = Some(name.to_owned());
        target.pop();
    }
    let meta = std::fs::metadata(&target).map_err(|_| tr!("경로를 찾을 수 없습니다: {}", "Path not found: {}", target.display()))?;
    let mut entries = Vec::new();
    if meta.is_dir() {
        let read = std::fs::read_dir(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        for item in read.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let Ok(meta) = item.metadata() else { continue };
            if (!opts.all && is_hidden(&name, &meta)) || pattern.as_deref().is_some_and(|p| !wildcard(p, &name)) {
                continue;
            }
            entries.push(Entry { name, is_dir: meta.is_dir(), size: meta.len(), modified: meta.modified().ok() });
        }
    } else {
        // A single file.
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        entries.push(Entry { name, is_dir: false, size: meta.len(), modified: meta.modified().ok() });
    }
    // Folders first, then by name.
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));

    if opts.long {
        Ok(long_listing(&target, &entries))
    } else {
        Ok(short_listing(&entries, cols))
    }
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn long_listing(dir: &Path, entries: &[Entry]) -> String {
    let mut out = tr!("\x1b[90m{} 폴더\x1b[0m\n\n", "\x1b[90mFolder {}\x1b[0m\n\n", dir.display());
    let (mut files, mut dirs, mut total) = (0, 0, 0u64);
    for e in entries {
        let time = e
            .modified
            .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d  %H:%M").to_string())
            .unwrap_or_else(|| " ".repeat(17));
        if e.is_dir {
            dirs += 1;
            out.push_str(&format!("{time}    <DIR>          {DIR_COLOR}{}{RESET}\n", e.name));
        } else {
            files += 1;
            total += e.size;
            out.push_str(&format!("{time}    {:>14} {}\n", thousands(e.size), e.name));
        }
    }
    out.push_str(&tr!(
        "\x1b[90m파일 {files}개 ({} 바이트), 폴더 {dirs}개\x1b[0m",
        "\x1b[90m{files} files ({} bytes), {dirs} folders\x1b[0m",
        thousands(total)
    ));
    out
}

/// Names in columns, filled top to bottom like `ls`.
fn short_listing(entries: &[Entry], cols: usize) -> String {
    if entries.is_empty() {
        return t("\x1b[90m(비어 있음)\x1b[0m", "\x1b[90m(empty)\x1b[0m").into();
    }
    let label = |e: &Entry| if e.is_dir { format!("{}{}", e.name, std::path::MAIN_SEPARATOR) } else { e.name.clone() };
    let width = entries.iter().map(|e| label(e).width()).max().unwrap_or(1) + 2;
    let per_row = (cols.max(width) / width).max(1);
    let rows = entries.len().div_ceil(per_row);
    let mut out = String::new();
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..per_row {
            let Some(e) = entries.get(c * rows + r) else { break };
            let text = label(e);
            let last = c + 1 == per_row || (c + 1) * rows + r >= entries.len();
            let padded = if last { text.clone() } else { pad(&text, width) };
            if e.is_dir {
                // Color only the name, not the padding.
                line.push_str(&format!("{DIR_COLOR}{text}{RESET}{}", &padded[text.len()..]));
            } else {
                line.push_str(&padded);
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.pop();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        // Strip SGR sequences for assertions.
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    #[test]
    #[cfg(not(windows))]
    fn resolves_unix_paths() {
        let cwd = Path::new("/Users/me/work");
        assert_eq!(resolve(cwd, ".."), PathBuf::from("/Users/me"));
        assert_eq!(resolve(cwd, "../../../.."), PathBuf::from("/"));
        assert_eq!(resolve(cwd, "src/./a"), PathBuf::from("/Users/me/work/src/a"));
        assert_eq!(resolve(cwd, "D:"), PathBuf::from("/Users/me/work/D:"), "no drive letters");
        assert_eq!(resolve(cwd, "/x/../y"), PathBuf::from("/y"));
        assert_eq!(resolve(cwd, "~"), home());
        assert_eq!(resolve(cwd, "~/Documents"), home().join("Documents"));
        assert_eq!(resolve(cwd, "~backup"), PathBuf::from("/Users/me/work/~backup"));
        assert_eq!(display(&home().join("Documents")), "~/Documents");
    }

    #[test]
    #[cfg(windows)]
    fn resolves_paths() {
        let cwd = Path::new(r"C:\Users\me\work");
        assert_eq!(resolve(cwd, ".."), PathBuf::from(r"C:\Users\me"));
        assert_eq!(resolve(cwd, r"..\..\..\.."), PathBuf::from(r"C:\"));
        assert_eq!(resolve(cwd, "src/./a"), PathBuf::from(r"C:\Users\me\work\src\a"));
        assert_eq!(resolve(cwd, "D:"), PathBuf::from(r"D:\"));
        assert_eq!(resolve(cwd, r"E:\x\..\y"), PathBuf::from(r"E:\y"));
        assert_eq!(resolve(cwd, "~"), home());
        assert_eq!(resolve(cwd, r"~\Documents"), home().join("Documents"));
        assert_eq!(resolve(cwd, "~backup"), PathBuf::from(r"C:\Users\me\work\~backup"));
        assert_eq!(resolve(cwd, "~$report.docx"), PathBuf::from(r"C:\Users\me\work\~$report.docx"));
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*.LOG", "a.log"));
        assert!(wildcard("a?c*", "abcdef"));
        assert!(!wildcard("*.txt", "a.log"));
        assert!(wildcard("*", ""));
    }

    #[test]
    fn lists_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("하위폴더")).unwrap();
        std::fs::write(dir.path().join("b.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("a.log"), vec![0u8; 1234]).unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();

        let short = plain(&list(dir.path(), &ListOptions::default(), 80).unwrap());
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(short, format!("하위폴더{sep}  a.log      b.txt"));
        let narrow = plain(&list(dir.path(), &ListOptions::default(), 10).unwrap());
        assert_eq!(narrow, format!("하위폴더{sep}\na.log\nb.txt"));

        let all = plain(&list(dir.path(), &ListOptions { all: true, ..Default::default() }, 80).unwrap());
        assert!(all.contains(".hidden"));

        let long = plain(&list(dir.path(), &ListOptions { long: true, ..Default::default() }, 80).unwrap());
        assert!(long.contains("<DIR>") && long.contains("1,234 a.log"));
        assert!(long.ends_with("파일 2개 (1,239 바이트), 폴더 1개"));

        let opts = ListOptions { path: Some("*.log".into()), ..Default::default() };
        assert_eq!(plain(&list(dir.path(), &opts, 80).unwrap()), "a.log");

        let opts = ListOptions { path: Some("nope".into()), ..Default::default() };
        assert!(list(dir.path(), &opts, 80).is_err());
    }

    #[test]
    fn changes_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("f"), "").unwrap();
        assert_eq!(change_dir(dir.path(), Some("sub")).unwrap(), resolve(dir.path(), "sub"));
        assert!(change_dir(dir.path(), Some("f")).is_err());
        assert!(change_dir(dir.path(), Some("missing")).is_err());
        assert_eq!(change_dir(dir.path(), None).unwrap(), home());
    }

    #[test]
    fn number_format() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
