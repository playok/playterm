//! Session output logging.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use alacritty_terminal::vte::{Parser, Perform};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogMode {
    /// Exactly the bytes received, in the wire charset (escape sequences included).
    Raw,
    /// Printable output only, UTF-8, escape sequences removed.
    Text { timestamps: bool },
}

pub struct SessionLog {
    path: PathBuf,
    out: BufWriter<File>,
    mode: LogMode,
    parser: Parser,
    at_line_start: bool,
    scratch: String,
}

impl SessionLog {
    /// Opens (appending to) `path`, creating parent folders. Text logs start with a header line.
    pub fn create(path: &Path, mode: LogMode, title: &str) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = File::options().create(true).append(true).open(path)?;
        let mut log = Self {
            path: path.to_owned(),
            out: BufWriter::new(file),
            mode,
            parser: Parser::new(),
            at_line_start: true,
            scratch: String::new(),
        };
        if let LogMode::Text { .. } = mode {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            write!(log.out, "===== playterm 로그: {title} ({now}) =====\r\n")?;
        }
        Ok(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn mode(&self) -> LogMode {
        self.mode
    }

    /// Bytes as received from the remote side (before charset decoding).
    pub fn record_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.mode == LogMode::Raw {
            self.out.write_all(bytes)?;
        }
        Ok(())
    }

    /// The same data after decoding to UTF-8.
    pub fn record_decoded(&mut self, utf8: &[u8]) -> io::Result<()> {
        let LogMode::Text { timestamps } = self.mode else { return Ok(()) };
        self.scratch.clear();
        let mut printer = Printer { out: &mut self.scratch, at_line_start: &mut self.at_line_start, timestamps };
        self.parser.advance(&mut printer, utf8);
        self.out.write_all(self.scratch.as_bytes())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Collects printable characters and line breaks, dropping control and escape sequences.
struct Printer<'a> {
    out: &'a mut String,
    at_line_start: &'a mut bool,
    timestamps: bool,
}

impl Perform for Printer<'_> {
    fn print(&mut self, c: char) {
        if *self.at_line_start && self.timestamps {
            self.out.push_str(&chrono::Local::now().format("[%H:%M:%S] ").to_string());
        }
        *self.at_line_start = false;
        self.out.push(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // CR is dropped; LF ends the line (written as CRLF for Windows editors).
            b'\n' => {
                self.out.push_str("\r\n");
                *self.at_line_start = true;
            }
            b'\t' => self.out.push('\t'),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn text_log_strips_escape_sequences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("a.log");
        let mut log = SessionLog::create(&path, LogMode::Text { timestamps: false }, "router").unwrap();
        log.record_raw(b"ignored in text mode").unwrap();
        // Split in the middle of an escape sequence and a UTF-8 character.
        log.record_decoded(b"\x1b[1;32mOK\x1b[").unwrap();
        log.record_decoded("0m 한글\r\n\x1b]0;title\x07line2\r\n".as_bytes()).unwrap();
        log.flush().unwrap();
        let text = read(&path);
        assert!(text.starts_with("===== playterm 로그: router"), "{text}");
        assert!(text.ends_with("OK 한글\r\nline2\r\n"), "{text:?}");
    }

    #[test]
    fn timestamps_prefix_each_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let mut log = SessionLog::create(&path, LogMode::Text { timestamps: true }, "x").unwrap();
        log.record_decoded(b"one\r\ntwo\r\n\r\n").unwrap();
        log.flush().unwrap();
        let body: Vec<String> = read(&path).lines().skip(1).map(str::to_owned).collect();
        assert_eq!(body.len(), 3);
        for (line, word) in body.iter().zip(["one", "two"]) {
            assert!(line.starts_with('[') && line.ends_with(word), "{line}");
        }
        assert_eq!(body[2], "", "empty lines get no timestamp");
    }

    #[test]
    fn raw_log_keeps_bytes_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.log");
        let mut log = SessionLog::create(&path, LogMode::Raw, "x").unwrap();
        log.record_raw(&[0x1b, b'[', b'm', 0xC7, 0xD1]).unwrap();
        log.record_decoded(b"ignored in raw mode").unwrap();
        drop(log);
        let mut log = SessionLog::create(&path, LogMode::Raw, "x").unwrap();
        log.record_raw(b"!").unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), vec![0x1b, b'[', b'm', 0xC7, 0xD1, b'!']);
    }
}
