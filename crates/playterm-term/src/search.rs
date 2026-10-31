//! Plain-text search through the screen and the scrollback.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::{RegexIter, RegexSearch};

use crate::terminal::Terminal;

/// A found occurrence: the first and last cell.
pub type SearchMatch = std::ops::RangeInclusive<Point>;

/// Which way to look from the current match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDirection {
    /// Up, towards older output.
    Older,
    /// Down, towards newer output.
    Newer,
}

/// A compiled search for some text. Matching ignores case unless the text has upper-case
/// letters (like `less` and Windows Terminal).
pub struct TextSearch {
    regex: RegexSearch,
}

/// Escapes regex syntax so the text is matched literally.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl TextSearch {
    /// `None` for empty text.
    pub fn new(text: &str) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        RegexSearch::new(&escape(text)).ok().map(|regex| Self { regex })
    }
}

impl Terminal {
    /// The next match in `direction` from `from` (not including it), wrapping around the
    /// whole history. Without `from`, `Older` starts at the bottom of the screen and `Newer`
    /// at the top of the history.
    pub fn find(&self, search: &mut TextSearch, from: Option<&SearchMatch>, direction: SearchDirection) -> Option<SearchMatch> {
        let term = self.term();
        match direction {
            SearchDirection::Older => {
                let origin = match from {
                    Some(m) => m.start().sub(term, Boundary::None, 1),
                    None => Point::new(Line(term.screen_lines() as i32 - 1), term.last_column()),
                };
                term.search_next(&mut search.regex, origin, Direction::Left, Side::Left, None)
            }
            SearchDirection::Newer => {
                let origin = match from {
                    Some(m) => m.start().add(term, Boundary::None, 1),
                    None => Point::new(Line(-(term.history_size() as i32)), Column(0)),
                };
                term.search_next(&mut search.regex, origin, Direction::Right, Side::Left, None)
            }
        }
    }

    /// Scrolls `m` into view and selects it (so it can be copied).
    pub fn show_match(&mut self, m: &SearchMatch) {
        let term = self.term_mut();
        term.scroll_to_point(*m.start());
        let mut selection = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
        selection.update(*m.end(), Side::Right);
        term.selection = Some(selection);
    }

    /// Matches on the part of the history that is on screen now.
    pub fn visible_matches(&self, search: &mut TextSearch) -> Vec<SearchMatch> {
        let term = self.term();
        let top = -(term.grid().display_offset() as i32);
        let start = Point::new(Line(top), Column(0));
        let end = Point::new(Line(top + term.screen_lines() as i32 - 1), term.last_column());
        RegexIter::new(start, end, Direction::Right, term, &mut search.regex).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Charset;

    fn at(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    fn term_with(text: &str) -> Terminal {
        let mut t = Terminal::new(20, 4, Charset::Utf8, 100);
        t.feed(text.as_bytes());
        t
    }

    #[test]
    fn finds_up_and_down_with_wrap() {
        // Six lines on a four-line screen: two are in the history.
        let t = term_with("alpha 1\r\nbeta\r\nalpha 2\r\ngamma\r\nalpha 3\r\n");
        let mut s = TextSearch::new("alpha").unwrap();

        let m3 = t.find(&mut s, None, SearchDirection::Older).unwrap();
        assert_eq!(*m3.start(), at(2, 0), "the newest first when going up");
        let m2 = t.find(&mut s, Some(&m3), SearchDirection::Older).unwrap();
        assert_eq!(*m2.start(), at(0, 0));
        let m1 = t.find(&mut s, Some(&m2), SearchDirection::Older).unwrap();
        assert_eq!(*m1.start(), at(-2, 0), "into the history");
        let wrapped = t.find(&mut s, Some(&m1), SearchDirection::Older).unwrap();
        assert_eq!(wrapped, m3);

        assert_eq!(t.find(&mut s, None, SearchDirection::Newer).unwrap(), m1);
        assert_eq!(t.find(&mut s, Some(&m1), SearchDirection::Newer).unwrap(), m2);
        assert_eq!(*m3.end(), at(2, 4));
    }

    #[test]
    fn literal_and_smart_case() {
        let t = term_with("axb a.b A.B\r\n");
        let mut dot = TextSearch::new("a.b").unwrap();
        assert_eq!(*t.find(&mut dot, None, SearchDirection::Newer).unwrap().start(), at(0, 4));
        let mut upper = TextSearch::new("A.B").unwrap();
        assert_eq!(*t.find(&mut upper, None, SearchDirection::Older).unwrap().start(), at(0, 8));
        let mut lower = TextSearch::new("a.b").unwrap();
        assert_eq!(*t.find(&mut lower, None, SearchDirection::Older).unwrap().start(), at(0, 8), "lower case matches both");
        let mut none = TextSearch::new("zzz").unwrap();
        assert!(t.find(&mut none, None, SearchDirection::Older).is_none());
        assert!(TextSearch::new("").is_none());
        assert!(TextSearch::new("(").is_some(), "regex syntax is literal");
    }

    #[test]
    fn hangul_and_visible_matches() {
        let mut t = term_with("로그 오류\r\nok\r\n오류 2\r\n");
        let mut s = TextSearch::new("오류").unwrap();
        let visible = t.visible_matches(&mut s);
        assert_eq!(visible.len(), 2);
        assert_eq!(*visible[0].start(), at(0, 5), "after a wide '로그 '");
        // Showing a match selects it.
        let m = visible[1].clone();
        t.show_match(&m);
        assert_eq!(t.selection_text().as_deref(), Some("오류"));
    }
}
