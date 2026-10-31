use std::sync::mpsc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

use crate::charset::{Charset, InputEncoder, OutputDecoder};
use crate::palette::Palette;

/// Something the terminal needs the host application to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Bytes to send back to the remote side (device attribute replies, cursor reports, ...).
    Reply(Vec<u8>),
    /// Window title set by the remote side; `None` resets to the default title.
    Title(Option<String>),
    Bell,
    /// Remote side asked to put text on the clipboard (OSC 52).
    ClipboardStore(String),
}

#[derive(Clone)]
pub struct Listener(mpsc::Sender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// One emulated terminal screen plus its wire-charset conversion.
pub struct Terminal {
    term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    decoder: OutputDecoder,
    encoder: InputEncoder,
    events: mpsc::Receiver<Event>,
    palette: Palette,
    size: Size,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, charset: Charset, scrollback: usize) -> Self {
        let (tx, rx) = mpsc::channel();
        let size = Size { cols: cols.max(2), rows: rows.max(1) };
        let config = Config { scrolling_history: scrollback, ..Config::default() };
        Self {
            term: Term::new(config, &size, Listener(tx)),
            parser: Processor::new(),
            decoder: OutputDecoder::new(charset),
            encoder: InputEncoder::new(charset),
            events: rx,
            palette: Palette::default(),
            size,
        }
    }

    /// Feeds raw bytes received from the remote side.
    pub fn feed(&mut self, bytes: &[u8]) {
        let utf8 = self.decoder.decode(bytes);
        self.parser.advance(&mut self.term, utf8);
    }

    /// Feeds locally generated UTF-8 text (status messages), bypassing the wire charset.
    pub fn feed_local(&mut self, text: &str) {
        self.parser.advance(&mut self.term, text.as_bytes());
    }

    /// Encodes user-typed text into the wire charset.
    pub fn encode_input(&self, text: &str) -> Vec<u8> {
        self.encoder.encode(text)
    }

    /// Returns `true` if the size actually changed.
    pub fn resize(&mut self, cols: usize, rows: usize) -> bool {
        let size = Size { cols: cols.max(2), rows: rows.max(1) };
        if size == self.size {
            return false;
        }
        self.size = size;
        self.term.resize(size);
        true
    }

    pub fn cols(&self) -> usize {
        self.size.cols
    }

    pub fn rows(&self) -> usize {
        self.size.rows
    }

    pub fn scroll_lines(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Grid point for a cell of the visible screen (row 0 = top of the viewport).
    pub fn viewport_point(&self, row: usize, col: usize) -> Point {
        let row = row.min(self.size.rows - 1) as i32;
        let offset = self.term.grid().display_offset() as i32;
        Point::new(Line(row - offset), Column(col.min(self.size.cols - 1)))
    }

    pub fn start_selection(&mut self, ty: SelectionType, point: Point, side: Side) {
        self.term.selection = Some(Selection::new(ty, point, side));
    }

    pub fn update_selection(&mut self, point: Point, side: Side) {
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, side);
        }
    }

    /// Selects the whole scrollback and screen.
    pub fn select_all(&mut self) {
        let top = Line(-(self.term.grid().history_size() as i32));
        let bottom = Line(self.size.rows as i32 - 1);
        let mut selection = Selection::new(SelectionType::Lines, Point::new(top, Column(0)), Side::Left);
        selection.update(Point::new(bottom, Column(self.size.cols - 1)), Side::Right);
        self.term.selection = Some(selection);
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// Selected text. Empty rows at the end (e.g. the unused bottom of the screen after
    /// "select all") collapse into a single trailing newline.
    pub fn selection_text(&self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        let mut text = self.term.selection_to_string()?;
        while text.ends_with("\n\n") {
            text.pop();
        }
        (!text.is_empty()).then_some(text)
    }

    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }

    pub fn term_mut(&mut self) -> &mut Term<Listener> {
        &mut self.term
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Drains pending terminal events, translating them for the host application.
    pub fn take_events(&mut self) -> Vec<TerminalEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::PtyWrite(text) => out.push(TerminalEvent::Reply(self.encoder.encode(&text))),
                Event::ColorRequest(index, format) => {
                    let rgb = self.palette.index(index, self.term.colors());
                    out.push(TerminalEvent::Reply(format(rgb).into_bytes()));
                }
                Event::Title(title) => out.push(TerminalEvent::Title(Some(title))),
                Event::ResetTitle => out.push(TerminalEvent::Title(None)),
                Event::Bell => out.push(TerminalEvent::Bell),
                Event::ClipboardStore(_, text) => out.push(TerminalEvent::ClipboardStore(text)),
                // Clipboard reads by the remote side are refused on purpose: they would let any
                // host silently exfiltrate the local clipboard.
                Event::ClipboardLoad(..) => {}
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::{Column, Line, Point};

    use super::*;

    fn row_text(t: &Terminal, line: i32) -> String {
        let grid = t.term().grid();
        (0..t.cols())
            .map(|c| grid[Point::new(Line(line), Column(c))].c)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn prints_text_and_moves_cursor() {
        let mut t = Terminal::new(20, 5, Charset::Utf8, 100);
        t.feed(b"hello\r\nworld");
        assert_eq!(row_text(&t, 0), "hello");
        assert_eq!(row_text(&t, 1), "world");
    }

    #[test]
    fn cp949_hangul_is_wide() {
        let mut t = Terminal::new(20, 5, Charset::Cp949, 100);
        t.feed(&[0xC7, 0xD1, b'A']);
        let grid = t.term().grid();
        let first = &grid[Point::new(Line(0), Column(0))];
        assert_eq!(first.c, '한');
        assert!(first.flags.contains(crate::Flags::WIDE_CHAR));
        assert_eq!(grid[Point::new(Line(0), Column(2))].c, 'A');
    }

    #[test]
    fn device_attributes_reply() {
        let mut t = Terminal::new(20, 5, Charset::Utf8, 100);
        t.feed(b"\x1b[c");
        let events = t.take_events();
        assert!(matches!(events.as_slice(), [TerminalEvent::Reply(r)] if r.starts_with(b"\x1b[?")));
    }

    #[test]
    fn selection_kinds() {
        let mut t = Terminal::new(20, 3, Charset::Utf8, 100);
        t.feed(b"hello world\r\nsecond line");

        t.start_selection(SelectionType::Simple, t.viewport_point(0, 0), Side::Left);
        assert!(!t.has_selection(), "a click without drag selects nothing");
        t.update_selection(t.viewport_point(0, 4), Side::Right);
        assert_eq!(t.selection_text().as_deref(), Some("hello"));

        t.start_selection(SelectionType::Semantic, t.viewport_point(0, 8), Side::Left);
        assert_eq!(t.selection_text().as_deref(), Some("world"));

        t.start_selection(SelectionType::Lines, t.viewport_point(1, 3), Side::Left);
        assert_eq!(t.selection_text().as_deref(), Some("second line\n"));

        t.select_all();
        assert_eq!(t.selection_text().as_deref(), Some("hello world\nsecond line\n"));
        t.clear_selection();
        assert_eq!(t.selection_text(), None);
    }

    #[test]
    fn hangul_selection() {
        let mut t = Terminal::new(20, 2, Charset::Utf8, 100);
        t.feed("가나 다라".as_bytes());
        t.start_selection(SelectionType::Simple, t.viewport_point(0, 0), Side::Left);
        t.update_selection(t.viewport_point(0, 3), Side::Right);
        assert_eq!(t.selection_text().as_deref(), Some("가나"));
    }

    #[test]
    fn title_event() {
        let mut t = Terminal::new(20, 5, Charset::Utf8, 100);
        t.feed(b"\x1b]0;my title\x07");
        assert_eq!(t.take_events(), vec![TerminalEvent::Title(Some("my title".into()))]);
    }
}
