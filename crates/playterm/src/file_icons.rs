//! Small painted icons for the SFTP file list: a folder, or a page with a colored band that
//! tells the kind of file. Painted with shapes (no icon font), so they look the same everywhere
//! and follow the light/dark theme.

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, pos2, vec2};
use playterm_net::RemoteEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Folder,
    Image,
    Archive,
    Code,
    Script,
    Config,
    Text,
    Document,
    Media,
    Executable,
    Other,
}

impl FileKind {
    /// Color of the band on the page; `None` for plain pages (text lines instead).
    fn band(self) -> Option<Color32> {
        Some(match self {
            FileKind::Image => Color32::from_rgb(60, 170, 90),
            FileKind::Archive => Color32::from_rgb(150, 90, 200),
            FileKind::Code => Color32::from_rgb(60, 120, 220),
            FileKind::Script => Color32::from_rgb(230, 130, 40),
            FileKind::Config => Color32::from_rgb(30, 150, 150),
            FileKind::Document => Color32::from_rgb(210, 70, 60),
            FileKind::Media => Color32::from_rgb(210, 80, 150),
            FileKind::Executable => Color32::from_rgb(200, 60, 60),
            FileKind::Folder | FileKind::Text | FileKind::Other => return None,
        })
    }
}

/// The kind of a remote entry, from its type, name and permissions.
pub fn kind(entry: &RemoteEntry) -> FileKind {
    if entry.is_dir {
        return FileKind::Folder;
    }
    let name = entry.name.to_ascii_lowercase();
    let ext = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext,
        _ => "",
    };
    let by_ext = match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" | "ico" | "tif" | "tiff" | "heic" => FileKind::Image,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "lz4" | "deb" | "rpm" | "jar" | "war" | "iso" | "img" => {
            FileKind::Archive
        }
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "py" | "js" | "ts" | "jsx" | "tsx" | "go" | "java" | "kt" | "rb" | "php" | "cs"
        | "swift" | "lua" | "pl" | "sql" | "html" | "htm" | "css" | "scss" | "vue" => FileKind::Code,
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => FileKind::Script,
        "conf" | "cfg" | "ini" | "yaml" | "yml" | "toml" | "json" | "xml" | "env" | "properties" | "service" => FileKind::Config,
        "txt" | "md" | "log" | "csv" | "rst" => FileKind::Text,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "hwp" | "odt" => FileKind::Document,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "mp4" | "mkv" | "avi" | "mov" | "webm" => FileKind::Media,
        _ => FileKind::Other,
    };
    if by_ext != FileKind::Other {
        return by_ext;
    }
    match name.as_str() {
        "dockerfile" | "makefile" | "cmakelists.txt" => return FileKind::Code,
        // Dotfiles without an extension (.bashrc, .profile, .gitignore) are settings.
        n if n.starts_with('.') && ext.is_empty() => return FileKind::Config,
        _ => {}
    }
    if entry.permissions.is_some_and(|m| m & 0o111 != 0) { FileKind::Executable } else { FileKind::Other }
}

/// Paints the icon for `kind` in a square as tall as a line of text.
pub fn icon(ui: &mut egui::Ui, kind: FileKind, symlink: bool) {
    let side = ui.text_style_height(&egui::TextStyle::Body).max(14.0);
    let (rect, _) = ui.allocate_exact_size(vec2(side, side), egui::Sense::hover());
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    // Work in a 16-unit grid scaled to the square.
    let s = side / 16.0;
    let p = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(p(x0, y0), p(x1, y1));

    if kind == FileKind::Folder {
        let back = Color32::from_rgb(214, 152, 40);
        let front = Color32::from_rgb(240, 186, 70);
        painter.rect_filled(r(1.0, 2.5, 7.0, 5.5), 1.0 * s, back);
        painter.rect_filled(r(1.0, 4.0, 15.0, 14.0), 1.5 * s, back);
        painter.rect_filled(r(1.0, 5.5, 15.0, 14.0), 1.5 * s, front);
    } else {
        let (fill, edge, fold) = if dark {
            (Color32::from_rgb(222, 224, 228), Color32::from_rgb(150, 152, 158), Color32::from_rgb(176, 178, 184))
        } else {
            (Color32::WHITE, Color32::from_rgb(130, 132, 138), Color32::from_rgb(200, 202, 208))
        };
        let page = vec![p(3.0, 1.0), p(10.0, 1.0), p(13.0, 4.0), p(13.0, 15.0), p(3.0, 15.0)];
        painter.add(Shape::convex_polygon(page, fill, Stroke::new(1.0, edge)));
        painter.add(Shape::convex_polygon(vec![p(10.0, 1.0), p(13.0, 4.0), p(10.0, 4.0)], fold, Stroke::NONE));
        match kind.band() {
            Some(color) => {
                painter.rect_filled(r(3.5, 9.0, 12.5, 13.5), 0.5 * s, color);
            }
            None => {
                let line = Stroke::new(1.0 * s, edge);
                for y in [7.0, 9.5, 12.0] {
                    painter.line_segment([p(5.0, y), p(11.0, y)], line);
                }
            }
        }
    }

    if symlink {
        // A small arrow in a circle at the bottom left: "points elsewhere".
        let center: Pos2 = p(4.5, 11.5);
        painter.circle(center, 3.6 * s, ui.visuals().extreme_bg_color, Stroke::new(1.0, ui.visuals().text_color()));
        let arrow = Stroke::new(1.2 * s, ui.visuals().text_color());
        painter.line_segment([p(3.0, 13.0), p(6.2, 9.8)], arrow);
        painter.line_segment([p(6.2, 9.8), p(4.0, 9.8)], arrow);
        painter.line_segment([p(6.2, 9.8), p(6.2, 12.0)], arrow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, permissions: Option<u32>) -> RemoteEntry {
        RemoteEntry { name: name.into(), is_dir, is_symlink: false, size: 0, mtime: None, permissions }
    }

    #[test]
    fn kinds() {
        assert_eq!(kind(&entry("logs", true, None)), FileKind::Folder);
        assert_eq!(kind(&entry("photo.JPG", false, None)), FileKind::Image);
        assert_eq!(kind(&entry("backup.tar.gz", false, None)), FileKind::Archive);
        assert_eq!(kind(&entry("main.rs", false, None)), FileKind::Code);
        assert_eq!(kind(&entry("deploy.sh", false, Some(0o755))), FileKind::Script);
        assert_eq!(kind(&entry("nginx.conf", false, None)), FileKind::Config);
        assert_eq!(kind(&entry(".bashrc", false, None)), FileKind::Config);
        assert_eq!(kind(&entry("Dockerfile", false, None)), FileKind::Code);
        assert_eq!(kind(&entry("syslog.log", false, None)), FileKind::Text);
        assert_eq!(kind(&entry("report.pdf", false, None)), FileKind::Document);
        assert_eq!(kind(&entry("talk.mp4", false, None)), FileKind::Media);
        assert_eq!(kind(&entry("server", false, Some(0o755))), FileKind::Executable);
        assert_eq!(kind(&entry("data", false, Some(0o644))), FileKind::Other);
        assert_eq!(kind(&entry(".env", false, None)), FileKind::Config, "a dotfile named like an extension");
    }
}
