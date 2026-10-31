use std::path::Path;
use std::sync::mpsc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

use crate::charset::{Charset, InputEncoder, OutputDecoder};
use crate::detect::Detector;
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

/// What reached the screen while the charset was still being detected.
enum Segment {
    Remote(Vec<u8>),
    Local(String),
}

/// Most bytes kept for redrawing after detection; beyond this only new output is fixed.
const REPLAY_LIMIT: usize = 512 * 1024;
/// History lines carried over (as plain text) when a tab that already has content starts
/// detecting, e.g. a connection made from the local prompt.
const REPLAY_HISTORY_LINES: usize = 1000;

/// Charset detection in progress ([`Charset::Auto`]).
struct AutoDetect {
    detector: Detector,
    /// The screen before detection started, as plain text.
    prefix: String,
    /// Output since then, to redraw with the detected charset; `None` once too large.
    segments: Option<Vec<Segment>>,
    recorded: usize,
}

impl AutoDetect {
    fn record(&mut self, segment: Segment, len: usize) {
        self.recorded += len;
        if self.recorded > REPLAY_LIMIT {
            self.segments = None;
        } else if let Some(segments) = &mut self.segments {
            segments.push(segment);
        }
    }
}

/// One emulated terminal screen plus its wire-charset conversion.
pub struct Terminal {
    term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    decoder: OutputDecoder,
    encoder: InputEncoder,
    events: mpsc::Receiver<Event>,
    /// A sender for a new `Term` when the screen is rebuilt.
    listener: Listener,
    scrollback: usize,
    palette: Palette,
    size: Size,
    log: Option<SessionLog>,
    /// Events raised outside the VT parser (log errors).
    pending: Vec<TerminalEvent>,
    auto: Option<AutoDetect>,
    /// The charset [`Charset::Auto`] settled on.
    detected: Option<Charset>,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, charset: Charset, scrollback: usize) -> Self {
        let (tx, rx) = mpsc::channel();
        let size = Size { cols: cols.max(2), rows: rows.max(1) };
        let config = Config { scrolling_history: scrollback, ..Config::default() };
        let listener = Listener(tx);
        let mut terminal = Self {
            term: Term::new(config, &size, listener.clone()),
            parser: Processor::new(),
            decoder: OutputDecoder::new(charset),
            encoder: InputEncoder::new(charset),
            events: rx,
            listener,
            scrollback,
            palette: Palette::default(),
            size,
            log: None,
            pending: Vec::new(),
            auto: None,
            detected: None,
        };
        terminal.set_charset(charset);
        terminal
    }

    /// Feeds raw bytes received from the remote side.
    pub fn feed(&mut self, bytes: &[u8]) {
        let detected = match &mut self.auto {
            Some(auto) => {
                auto.record(Segment::Remote(bytes.to_vec()), bytes.len());
                auto.detector.push(bytes)
            }
            None => None,
        };
        self.feed_decoded(bytes);
        if let Some(charset) = detected {
            self.settle(charset);
        }
    }

    fn feed_decoded(&mut self, bytes: &[u8]) {
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
        if let Some(auto) = &mut self.auto {
            auto.record(Segment::Local(text.to_owned()), text.len());
        }
        self.parser.advance(&mut self.term, text.as_bytes());
    }

    /// Switches the wire charset (when a tab connects to a different host). With
    /// [`Charset::Auto`] the output is read as UTF-8 until it shows which charset it is in.
    pub fn set_charset(&mut self, charset: Charset) {
        self.decoder = OutputDecoder::new(charset);
        self.encoder = InputEncoder::new(charset);
        self.detected = None;
        self.auto = (charset == Charset::Auto).then(|| AutoDetect {
            detector: Detector::default(),
            prefix: self.screen_text(),
            segments: Some(Vec::new()),
            recorded: 0,
        });
    }

    /// The charset [`Charset::Auto`] detected, once it has.
    pub fn detected_charset(&self) -> Option<Charset> {
        self.detected
    }

    /// Uses `charset` from now on. Output already shown in the wrong one is redrawn.
    fn settle(&mut self, charset: Charset) {
        let Some(auto) = self.auto.take() else { return };
        self.detected = Some(charset);
        if charset == Charset::Utf8 {
            return; // what was shown was decoded as UTF-8 already
        }
        self.decoder = OutputDecoder::new(charset);
        self.encoder = InputEncoder::new(charset);
        if let Some(segments) = auto.segments {
            self.rebuild(&auto.prefix, segments);
        }
    }

    /// Redraws the screen from scratch: `prefix` (the earlier screen), then the recorded output.
    fn rebuild(&mut self, prefix: &str, segments: Vec<Segment>) {
        let mut kept = self.channel_events();
        let config = Config { scrolling_history: self.scrollback, ..Config::default() };
        self.term = Term::new(config, &self.size, self.listener.clone());
        self.parser = Processor::new();
        // The log has this output already; replaying must not write it twice.
        let log = self.log.take();
        self.parser.advance(&mut self.term, prefix.as_bytes());
        for segment in segments {
            match segment {
                Segment::Remote(bytes) => {
                    let utf8 = self.decoder.decode(&bytes).to_vec();
                    self.parser.advance(&mut self.term, &utf8);
                }
                Segment::Local(text) => self.parser.advance(&mut self.term, text.as_bytes()),
            }
        }
        self.log = log;
        // Replies the redraw produced were sent the first time; titles are kept.
        kept.extend(self.channel_events().into_iter().filter(|e| matches!(e, TerminalEvent::Title(_))));
        self.pending.extend(kept);
    }

    /// The history and screen up to the cursor as plain text lines (colors are not kept).
    fn screen_text(&self) -> String {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let first = (-(grid.history_size() as i32)).max(cursor.line.0 - REPLAY_HISTORY_LINES as i32);
        let mut out = String::new();
        for line in first..=cursor.line.0 {
            let row = &grid[Line(line)];
            let end = if line == cursor.line.0 { cursor.column.0 } else { self.size.cols };
            let text: String = (0..end.min(self.size.cols))
                .map(|c| &row[Column(c)])
                .filter(|cell| !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER))
                .map(|cell| if cell.c == '\0' { ' ' } else { cell.c })
                .collect();
            if line == cursor.line.0 {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end());
                out.push_str("\r\n");
            }
        }
        // Nothing but empty lines: a new tab.
        if out.trim().is_empty() { String::new() } else { out }
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

    /// The text on the cursor's line before the cursor: the prompt a program is waiting at.
    pub fn cursor_line_text(&self) -> String {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let row = &grid[cursor.line];
        let mut text = String::new();
        for col in 0..cursor.column.0.min(self.size.cols) {
            let cell = &row[Column(col)];
            if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                text.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
        }
        text
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

    /// Switches the color scheme (colors the remote set with OSC 4/10/11 still win).
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// Changes how many lines of history are kept; shrinking drops the oldest lines.
    pub fn set_scrollback(&mut self, lines: usize) {
        self.scrollback = lines;
        // This also re-announces the current title, which leaves the tab title unchanged.
        self.term.set_options(Config { scrolling_history: lines, ..Config::default() });
    }

    /// Drains pending terminal events, translating them for the host application.
    pub fn take_events(&mut self) -> Vec<TerminalEvent> {
        if let Some(log) = &mut self.log
            && let Err(e) = log.flush()
        {
            self.fail_log(e);
        }
        let mut out = std::mem::take(&mut self.pending);
        out.extend(self.channel_events());
        out
    }

    /// Events the terminal emulator raised, translated for the host.
    fn channel_events(&mut self) -> Vec<TerminalEvent> {
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
    use alacritty_terminal::term::TermMode;

    use super::*;

    fn row_text(t: &Terminal, line: i32) -> String {
        let grid = t.term().grid();
        (0..t.cols())
            .map(|c| &grid[Point::new(Line(line), Column(c))])
            .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
            .map(|cell| cell.c)
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
    fn cursor_line_text() {
        let mut t = Terminal::new(20, 3, Charset::Utf8, 100);
        t.feed("Welcome\r\n\x1b[1mrouter\x1b[0m login: ".as_bytes());
        assert_eq!(t.cursor_line_text(), "router login: ");
        t.feed(b"admin\r\n");
        assert_eq!(t.cursor_line_text(), "");
        t.feed("암호: ".as_bytes());
        assert_eq!(t.cursor_line_text(), "암호: ", "wide characters once each");
    }

    fn cp949(text: &str) -> Vec<u8> {
        encoding_rs::EUC_KR.encode(text).0.into_owned()
    }

    #[test]
    fn auto_detects_cp949_and_redraws() {
        let mut t = Terminal::new(30, 5, Charset::Auto, 100);
        t.feed_local("연결 중...\r\n");
        // A device-attributes query: answered once, not again when the screen is redrawn.
        t.feed(b"\x1b[c\x1b]0;router\x07");
        t.feed(&cp949("환영합니다\r\n"));
        assert_eq!(t.detected_charset(), Some(Charset::Cp949));
        assert_eq!(row_text(&t, 0), "연결 중...");
        assert_eq!(row_text(&t, 1), "환영합니다", "the first Hangul is redrawn correctly");
        let events = t.take_events();
        assert_eq!(events.iter().filter(|e| matches!(e, TerminalEvent::Reply(_))).count(), 1);
        assert!(events.contains(&TerminalEvent::Title(Some("router".into()))));
        // Output and input use CP949 from now on.
        t.feed(&cp949("암호: "));
        assert_eq!(row_text(&t, 2), "암호:");
        assert_eq!(t.encode_input("한"), cp949("한"));
    }

    #[test]
    fn auto_detects_utf8() {
        let mut t = Terminal::new(30, 3, Charset::Auto, 100);
        t.feed("안녕하세요\r\n".as_bytes());
        assert_eq!(t.detected_charset(), Some(Charset::Utf8));
        assert_eq!(row_text(&t, 0), "안녕하세요");
        assert_eq!(t.encode_input("한"), "한".as_bytes());
    }

    #[test]
    fn auto_keeps_earlier_screen_text() {
        // A tab with a local prompt connects; the prompt history stays after the redraw.
        let mut t = Terminal::new(30, 6, Charset::Utf8, 100);
        t.feed_local("playterm> ls\r\nfile.txt\r\nplayterm> telnet r\r\n");
        t.set_charset(Charset::Auto);
        assert_eq!(t.detected_charset(), None);
        t.feed(&cp949("사용자: "));
        assert_eq!(t.detected_charset(), Some(Charset::Cp949));
        assert_eq!(row_text(&t, 0), "playterm> ls");
        assert_eq!(row_text(&t, 2), "playterm> telnet r");
        assert_eq!(row_text(&t, 3), "사용자:");
        // A fixed charset ends detection.
        t.set_charset(Charset::Utf8);
        assert_eq!(t.detected_charset(), None);
    }

    #[test]
    fn scrollback_can_shrink_and_grow() {
        let mut t = Terminal::new(20, 5, Charset::Utf8, 100);
        for i in 0..50 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(t.term().grid().history_size(), 46);
        t.set_scrollback(10);
        assert_eq!(t.term().grid().history_size(), 10);
        t.set_scrollback(1000);
        for i in 0..50 {
            t.feed(format!("more {i}\r\n").as_bytes());
        }
        assert_eq!(t.term().grid().history_size(), 60);
        assert!(t.take_events().iter().all(|e| matches!(e, TerminalEvent::Title(None))));
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
