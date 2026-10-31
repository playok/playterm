use std::path::Path;
use std::sync::mpsc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

use crate::charset::{Charset, InputEncoder, OutputDecoder};
use crate::log::{LogMode, SessionLog};
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
    /// Writing the session log failed; logging has stopped.
    LogFailed(String),
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
    log: Option<SessionLog>,
    /// Events raised outside the VT parser (log errors).
    pending: Vec<TerminalEvent>,
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
            log: None,
            pending: Vec::new(),
        }
    }

    /// Feeds raw bytes received from the remote side.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut log_error = None;
        if let Some(log) = &mut self.log
            && let Err(e) = log.record_raw(bytes)
        {
            log_error = Some(e);
        }
        let utf8 = self.decoder.decode(bytes);
        if let Some(log) = &mut self.log
            && let Err(e) = log.record_decoded(utf8)
        {
            log_error = Some(e);
        }
        self.parser.advance(&mut self.term, utf8);
        if let Some(e) = log_error {
            self.fail_log(e);
        }
    }

    /// Starts writing received output to `path` (appending if it exists).
    pub fn start_log(&mut self, path: &Path, mode: LogMode, title: &str) -> std::io::Result<()> {
        self.stop_log();
        self.log = Some(SessionLog::create(path, mode, title)?);
        Ok(())
    }

    /// Stops logging; returns the file that was written.
    pub fn stop_log(&mut self) -> Option<std::path::PathBuf> {
        let mut log = self.log.take()?;
        let _ = log.flush();
        Some(log.path().to_owned())
    }

    pub fn log(&self) -> Option<&SessionLog> {
        self.log.as_ref()
    }

    fn fail_log(&mut self, error: std::io::Error) {
        if let Some(log) = self.log.take() {
            self.pending.push(TerminalEvent::LogFailed(format!("{}: {error}", log.path().display())));
        }
    }

    /// Feeds locally generated UTF-8 text (status messages), bypassing the wire charset.
    pub fn feed_local(&mut self, text: &str) {
        self.parser.advance(&mut self.term, text.as_bytes());
    }

    /// Switches the wire charset (when a tab connects to a different host).
    pub fn set_charset(&mut self, charset: Charset) {
        self.decoder = OutputDecoder::new(charset);
        self.encoder = InputEncoder::new(charset);
    }

    /// Turns off what a remote program may have left on: alternate screen, origin mode and
    /// scroll region, line-drawing character set, insert mode, application keys, mouse
    /// reporting, bracketed paste, hidden cursor, colors. The cursor stays where it is.
    pub fn reset_modes(&mut self) {
        // Back to the normal screen first; that also restores its cursor.
        self.feed_local("\x1b[?1049l");
        let cursor = self.term.grid().cursor.point;
        // Origin mode and the scroll region reset move the cursor home, so put it back after.
        self.feed_local(
            "\x1b[?6l\x1b[r\x1b(B\x0f\x1b[4l\x1b[?7h\x1b[?25h\x1b[0m\x1b[?1l\x1b>\x1b[?2004l\
             \x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l",
        );
        self.feed_local(&format!("\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1));
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

    /// Drops the scrollback history (the visible screen stays).
    pub fn clear_scrollback(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.term.grid_mut().clear_history();
        self.term.selection = None;
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
        if let Some(log) = &mut self.log
            && let Err(e) = log.flush()
        {
            self.fail_log(e);
        }
        let mut out = std::mem::take(&mut self.pending);
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
    use alacritty_terminal::term::TermMode;

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
    fn reset_modes_keeps_cursor() {
        let mut t = Terminal::new(20, 5, Charset::Utf8, 100);
        t.feed(b"one\r\ntwo");
        // A full-screen program: alternate screen, scroll region, origin mode, line drawing,
        // insert mode, mouse reporting.
        t.feed(b"\x1b[?1049h\x1b[2;4r\x1b[?6h\x1b(0\x1b[4h\x1b[?1000h\x1b[?2004h");
        t.reset_modes();
        assert!(!t.term().mode().intersects(TermMode::MOUSE_MODE | TermMode::BRACKETED_PASTE | TermMode::INSERT));
        // Back on the normal screen, after "two"; "q" is a plain q again, not a line.
        t.feed(b"q");
        assert_eq!(row_text(&t, 0), "one");
        assert_eq!(row_text(&t, 1), "twoq");
        // Without a scroll region, new lines reach the bottom row.
        t.feed(b"\r\n3\r\n4\r\n5");
        assert_eq!(row_text(&t, 4), "5");
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

    #[test]
    fn logs_decoded_cp949_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let text_path = dir.path().join("text.log");
        let mut t = Terminal::new(20, 5, Charset::Cp949, 100);
        t.start_log(&text_path, LogMode::Text { timestamps: false }, "장비").unwrap();
        t.feed(&[0x1b, b'[', b'1', b'm', 0xC7, 0xD1, b'\r', b'\n']);
        t.feed_local("local notices are not logged\r\n");
        assert!(t.take_events().is_empty());
        assert_eq!(t.stop_log().as_deref(), Some(text_path.as_path()));
        t.feed(b"after stop\r\n");

        let text = std::fs::read_to_string(&text_path).unwrap();
        assert!(text.ends_with("\r\n한\r\n"), "{text:?}");
        assert!(!text.contains("local") && !text.contains("after"));
    }
}
