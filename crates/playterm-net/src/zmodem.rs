//! Receiving files with ZMODEM: `sz file` run on the server sends files through the terminal
//! connection (SSH, Telnet or serial), and they are saved into a folder on this PC.
//!
//! [`Detector`] spots the start of a transfer in the output of the remote side; from there a
//! [`Download`] takes all output until the transfer is over. It answers the sender, checks every
//! header and data block (CRC-16 or CRC-32), asks for a resend from the last good position after
//! an error, and writes each file under a temporary name first, so a broken transfer leaves no
//! half file behind. Only receiving is supported; a command the sender asks to run (ZCOMMAND) is
//! refused.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use playterm_i18n::{t, tr};

use crate::sftp::is_safe_local_name;

const ZPAD: u8 = b'*';
/// ZMODEM's escape byte; it is also CAN, five of which in a row cancel a transfer.
const ZDLE: u8 = 0x18;
const ZBIN: u8 = b'A';
const ZHEX: u8 = b'B';
const ZBIN32: u8 = b'C';
const XON: u8 = 0x11;
const XOFF: u8 = 0x13;

// Header types.
const ZRQINIT: u8 = 0;
const ZRINIT: u8 = 1;
const ZSINIT: u8 = 2;
const ZACK: u8 = 3;
const ZFILE: u8 = 4;
const ZNAK: u8 = 6;
const ZABORT: u8 = 7;
const ZFIN: u8 = 8;
const ZRPOS: u8 = 9;
const ZDATA: u8 = 10;
const ZEOF: u8 = 11;
const ZFERR: u8 = 12;
const ZCHALLENGE: u8 = 14;
const ZCAN: u8 = 16;
const ZFREECNT: u8 = 17;
const ZCOMMAND: u8 = 18;

// How a data block ends (after a ZDLE): what the sender expects next.
/// End of the frame; a header follows.
const ZCRCE: u8 = b'h';
/// More blocks follow, no answer wanted.
const ZCRCG: u8 = b'i';
/// More blocks follow; answer with ZACK.
const ZCRCQ: u8 = b'j';
/// End of the frame; answer with ZACK.
const ZCRCW: u8 = b'k';
/// Escaped 0x7f and 0xff.
const ZRUB0: u8 = b'l';
const ZRUB1: u8 = b'm';

// What this receiver can do (ZRINIT): full duplex, receiving while writing to disk, CRC-32.
const CANFDX: u8 = 0x01;
const CANOVIO: u8 = 0x02;
const CANFC32: u8 = 0x20;

/// Longest data block accepted (senders use up to 8 KiB).
const MAX_BLOCK: usize = 16 * 1024;
/// Without output from the sender for this long, the last answer is sent again...
const RETRY_AFTER: Duration = Duration::from_secs(10);
/// ...and after this long the transfer is given up.
const GIVE_UP_AFTER: Duration = Duration::from_secs(60);
/// How long to wait for the sender's closing "OO".
const OO_WAIT: Duration = Duration::from_secs(1);
/// After a cancel, what the sender had already sent is dropped until it is quiet this long...
const DRAIN_QUIET: Duration = Duration::from_millis(300);
/// ...or at most this long...
const DRAIN_MAX: Duration = Duration::from_secs(5);
/// ...or until this much came without a ZMODEM byte (it is not the transfer any more).
const DRAIN_KEEP: usize = 4096;

/// What cancels a ZMODEM transfer on the other side: CAN bytes, then backspaces that erase them
/// on a terminal that shows them.
pub const CANCEL: &[u8] = b"\x18\x18\x18\x18\x18\x18\x18\x18\x18\x18\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08";

// -- checksums --------------------------------------------------------------------------------

const fn crc16_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC16_TABLE: [u16; 256] = crc16_table();
static CRC32_TABLE: [u32; 256] = crc32_table();

/// CRC-16/XMODEM, continued from `crc` (start with 0).
fn crc16(crc: u16, data: &[u8]) -> u16 {
    data.iter().fold(crc, |crc, &b| (crc << 8) ^ CRC16_TABLE[usize::from((crc >> 8) as u8 ^ b)])
}

/// CRC-32 (IEEE) state, continued from `crc`: start with `!0` and invert the result.
fn crc32(crc: u32, data: &[u8]) -> u32 {
    data.iter().fold(crc, |crc, &b| (crc >> 8) ^ CRC32_TABLE[usize::from(crc as u8 ^ b)])
}

// -- frames -----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    kind: u8,
    /// A position (little endian) or flags (`data[3]` is ZF0).
    data: [u8; 4],
}

impl Header {
    fn new(kind: u8) -> Self {
        Self { kind, data: [0; 4] }
    }

    /// A header carrying a file position (ZMODEM positions are 32 bits).
    fn at(kind: u8, pos: u64) -> Self {
        Self { kind, data: (pos as u32).to_le_bytes() }
    }

    fn position(&self) -> u32 {
        u32::from_le_bytes(self.data)
    }

    /// The header in hex form, the form a receiver sends.
    fn hex(&self) -> Vec<u8> {
        let mut raw = [0u8; 7];
        raw[0] = self.kind;
        raw[1..5].copy_from_slice(&self.data);
        let crc = crc16(0, &raw[..5]);
        raw[5..].copy_from_slice(&crc.to_be_bytes());
        let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
        for b in raw {
            out.extend_from_slice(format!("{b:02x}").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        if self.kind != ZFIN && self.kind != ZACK {
            out.push(XON);
        }
        out
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Frame {
    /// A header; `crc32` if its data blocks carry CRC-32.
    Header(Header, bool),
    BadHeader,
    /// A data block and how it ended (ZCRCE, ZCRCG, ZCRCQ or ZCRCW).
    Block(Vec<u8>, u8),
    BadBlock,
}

/// Undoes ZDLE escaping, one byte at a time.
#[derive(Default)]
struct Unescape {
    after_zdle: bool,
}

enum Unescaped {
    Byte(u8),
    /// ZDLE plus the end of a data block.
    End(u8),
    Bad,
    /// An escape prefix or flow control, which carry no data.
    Nothing,
}

impl Unescape {
    fn push(&mut self, b: u8) -> Unescaped {
        if std::mem::take(&mut self.after_zdle) {
            return match b {
                // CANs in a row: a cancel (counted by the receiver).
                ZDLE => {
                    self.after_zdle = true;
                    Unescaped::Nothing
                }
                ZCRCE | ZCRCG | ZCRCQ | ZCRCW => Unescaped::End(b),
                ZRUB0 => Unescaped::Byte(0x7f),
                ZRUB1 => Unescaped::Byte(0xff),
                _ if b & 0x60 == 0x40 => Unescaped::Byte(b ^ 0x40),
                _ => Unescaped::Bad,
            };
        }
        match b {
            ZDLE => {
                self.after_zdle = true;
                Unescaped::Nothing
            }
            // Flow control from the line is not data (the sender escapes real ones).
            XON | XOFF | 0x91 | 0x93 => Unescaped::Nothing,
            _ => Unescaped::Byte(b),
        }
    }
}

enum Parse {
    /// Looking for the next header.
    Seek,
    /// `*` seen.
    Pad,
    /// `*` ZDLE seen.
    PadZdle,
    /// The hex digits of a hex header.
    Hex(Vec<u8>),
    /// A binary header.
    Bin { crc32: bool, bytes: Vec<u8>, esc: Unescape },
    /// A data block, then its CRC.
    Data { crc32: bool, bytes: Vec<u8>, esc: Unescape, end: Option<u8>, check: Vec<u8> },
}

/// Splits the sender's output into headers and data blocks.
struct Parser {
    state: Parse,
}

impl Parser {
    fn new() -> Self {
        Self { state: Parse::Seek }
    }

    /// Reads a data block next.
    fn expect_data(&mut self, crc32: bool) {
        self.state = Parse::Data { crc32, bytes: Vec::new(), esc: Unescape::default(), end: None, check: Vec::new() };
    }

    /// Between frames (not inside a header or block).
    fn idle(&self) -> bool {
        matches!(self.state, Parse::Seek)
    }

    fn push(&mut self, b: u8) -> Option<Frame> {
        let (next, frame) = match std::mem::replace(&mut self.state, Parse::Seek) {
            Parse::Seek => (if b == ZPAD { Parse::Pad } else { Parse::Seek }, None),
            Parse::Pad => (
                match b {
                    ZPAD => Parse::Pad,
                    ZDLE => Parse::PadZdle,
                    _ => Parse::Seek,
                },
                None,
            ),
            Parse::PadZdle => (
                match b {
                    ZHEX => Parse::Hex(Vec::with_capacity(14)),
                    ZBIN => Parse::Bin { crc32: false, bytes: Vec::with_capacity(7), esc: Unescape::default() },
                    ZBIN32 => Parse::Bin { crc32: true, bytes: Vec::with_capacity(9), esc: Unescape::default() },
                    _ => Parse::Seek,
                },
                None,
            ),
            Parse::Hex(mut digits) => {
                let b = b & 0x7f;
                if b == XON || b == XOFF {
                    (Parse::Hex(digits), None)
                } else {
                    digits.push(b);
                    if digits.len() < 14 { (Parse::Hex(digits), None) } else { (Parse::Seek, Some(hex_header(&digits))) }
                }
            }
            Parse::Bin { crc32: wide, mut bytes, mut esc } => match esc.push(b) {
                Unescaped::Nothing => (Parse::Bin { crc32: wide, bytes, esc }, None),
                Unescaped::Bad | Unescaped::End(_) => (Parse::Seek, Some(Frame::BadHeader)),
                Unescaped::Byte(x) => {
                    bytes.push(x);
                    if bytes.len() < if wide { 9 } else { 7 } {
                        (Parse::Bin { crc32: wide, bytes, esc }, None)
                    } else {
                        (Parse::Seek, Some(bin_header(&bytes, wide)))
                    }
                }
            },
            Parse::Data { crc32: wide, mut bytes, mut esc, end, mut check } => match (esc.push(b), end) {
                (Unescaped::Nothing, _) => (Parse::Data { crc32: wide, bytes, esc, end, check }, None),
                (Unescaped::Bad, _) | (Unescaped::End(_), Some(_)) => (Parse::Seek, Some(Frame::BadBlock)),
                (Unescaped::End(e), None) => (Parse::Data { crc32: wide, bytes, esc, end: Some(e), check }, None),
                (Unescaped::Byte(x), None) => {
                    bytes.push(x);
                    if bytes.len() > MAX_BLOCK {
                        (Parse::Seek, Some(Frame::BadBlock))
                    } else {
                        (Parse::Data { crc32: wide, bytes, esc, end, check }, None)
                    }
                }
                (Unescaped::Byte(x), Some(e)) => {
                    check.push(x);
                    if check.len() < if wide { 4 } else { 2 } {
                        (Parse::Data { crc32: wide, bytes, esc, end, check }, None)
                    } else {
                        let good = if wide {
                            !crc32(crc32(!0, &bytes), &[e]) == u32::from_le_bytes([check[0], check[1], check[2], check[3]])
                        } else {
                            crc16(crc16(0, &bytes), &[e]) == u16::from_be_bytes([check[0], check[1]])
                        };
                        (Parse::Seek, Some(if good { Frame::Block(bytes, e) } else { Frame::BadBlock }))
                    }
                }
            },
        };
        self.state = next;
        frame
    }
}

fn hex_header(digits: &[u8]) -> Frame {
    let value = |d: u8| (d as char).to_digit(16);
    let mut raw = [0u8; 7];
    for (i, pair) in digits.chunks(2).enumerate() {
        match (value(pair[0]), value(pair[1])) {
            (Some(hi), Some(lo)) => raw[i] = (hi * 16 + lo) as u8,
            _ => return Frame::BadHeader,
        }
    }
    if crc16(0, &raw[..5]) != u16::from_be_bytes([raw[5], raw[6]]) {
        return Frame::BadHeader;
    }
    Frame::Header(Header { kind: raw[0], data: [raw[1], raw[2], raw[3], raw[4]] }, false)
}

fn bin_header(bytes: &[u8], wide: bool) -> Frame {
    let good = if wide {
        !crc32(!0, &bytes[..5]) == u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]])
    } else {
        crc16(0, &bytes[..5]) == u16::from_be_bytes([bytes[5], bytes[6]])
    };
    if good { Frame::Header(Header { kind: bytes[0], data: [bytes[1], bytes[2], bytes[3], bytes[4]] }, wide) } else { Frame::BadHeader }
}

// -- finding the start ------------------------------------------------------------------------

/// The start of a hex header from the other side; `0` then means ZRQINIT (a sender), `1` ZRINIT
/// (a receiver).
const MARKER: &[u8] = b"**\x18B0";

/// A ZMODEM transfer starting in the output of the remote side (see [`Detector`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// `sz` on the server is sending files. `at` is where its first header begins in the chunk
    /// (0 if it began in an earlier one), `rest` where the output after its start begins.
    Download { at: usize, rest: usize },
    /// `rz` on the server waits for files to be sent to it, which is not supported.
    Upload { at: usize, rest: usize },
}

/// Finds the start of a ZMODEM transfer in the output of the remote side, also when it is split
/// between chunks.
#[derive(Default)]
pub struct Detector {
    matched: usize,
}

impl Detector {
    pub fn scan(&mut self, chunk: &[u8]) -> Option<Start> {
        for (i, &b) in chunk.iter().enumerate() {
            if self.matched == MARKER.len() {
                self.matched = 0;
                let at = (i + 1).saturating_sub(MARKER.len() + 1);
                match b {
                    b'0' => return Some(Start::Download { at, rest: i + 1 }),
                    b'1' => return Some(Start::Upload { at, rest: i + 1 }),
                    _ => {}
                }
            }
            self.matched = if b == MARKER[self.matched] {
                self.matched + 1
            } else if b == ZPAD {
                // "***": the last two may still start a header.
                if self.matched == 2 { 2 } else { 1 }
            } else {
                0
            };
        }
        None
    }
}

// -- receiving --------------------------------------------------------------------------------

/// How a download ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    /// The user cancelled it.
    Cancelled,
    Failed(String),
}

/// The file being received.
pub struct Progress<'a> {
    pub name: &'a str,
    pub received: u64,
    /// The size the sender announced.
    pub size: Option<u64>,
}

enum Stage {
    /// ZRINIT sent: waiting for a file, or the end.
    Init,
    /// ZSINIT came: waiting for its data block.
    Sinit,
    /// ZFILE came: waiting for the name and size.
    FileInfo,
    /// Taking the data of a file.
    Receiving,
    /// ZFIN answered: waiting for the sender's "OO".
    Finishing(Instant),
}

struct Incoming {
    name: String,
    part: PathBuf,
    out: BufWriter<File>,
    size: Option<u64>,
    received: u64,
}

/// A ZMODEM download in progress (see the module docs).
pub struct Download {
    dir: PathBuf,
    parser: Parser,
    stage: Stage,
    /// The data blocks after the last header carry CRC-32.
    crc32: bool,
    file: Option<Incoming>,
    saved: Vec<PathBuf>,
    outcome: Option<Outcome>,
    /// Output of the remote side after the transfer ended, for the terminal.
    passthrough: Vec<u8>,
    /// CAN bytes in a row; five cancel the transfer.
    cans: usize,
    /// 'O's of the closing "OO" seen.
    oo: usize,
    last_input: Instant,
    last_retry: Instant,
    /// The last header sent, sent again when the sender asks (ZNAK).
    last_sent: Vec<u8>,
    /// The user cancelled at this time: the rest of the transfer is being dropped.
    cancelling: Option<Instant>,
    /// While cancelling: what came after the last ZMODEM byte (the shell's prompt, at the end).
    drain: Vec<u8>,
}

/// The first header of a download, which [`Detector`] found.
const DOWNLOAD_START: &[u8] = b"**\x18B00";

impl Download {
    /// Starts receiving into `dir`, with the output that came after the start [`Detector`] found.
    /// Returns the download and what to send to the server.
    pub fn begin(dir: PathBuf, rest: &[u8], now: Instant) -> (Self, Vec<u8>) {
        let mut download = Self {
            dir,
            parser: Parser::new(),
            stage: Stage::Init,
            crc32: false,
            file: None,
            saved: Vec::new(),
            outcome: None,
            passthrough: Vec::new(),
            cans: 0,
            oo: 0,
            last_input: now,
            last_retry: now,
            last_sent: Vec::new(),
            cancelling: None,
            drain: Vec::new(),
        };
        let mut reply = download.feed(DOWNLOAD_START, now);
        reply.extend(download.feed(rest, now));
        (download, reply)
    }

    /// Takes output of the remote side. Returns what to send back.
    pub fn feed(&mut self, input: &[u8], now: Instant) -> Vec<u8> {
        let mut reply = Vec::new();
        for (i, &b) in input.iter().enumerate() {
            if self.outcome.is_some() {
                self.passthrough.extend_from_slice(&input[i..]);
                break;
            }
            self.last_input = now;
            if self.cancelling.is_some() {
                self.drain(&input[i..]);
                break;
            }
            self.byte(b, now, &mut reply);
        }
        reply
    }

    /// After a cancel: drops what the sender still sends, keeping what came after its last
    /// ZMODEM byte for the screen. That is a ZDLE (escaped data is full of them) before an
    /// escaped byte or the end of a block, all from 0x40 up; a shell echoing our CAN bytes
    /// sends them before other bytes. (A ZDLE at the end waits for the next byte.)
    fn drain(&mut self, input: &[u8]) {
        self.drain.extend_from_slice(input);
        let last = (0..self.drain.len())
            .rev()
            .find(|&i| self.drain[i] == ZDLE && self.drain.get(i + 1).is_some_and(|&next| next >= 0x40));
        if let Some(i) = last {
            self.drain.drain(..=i);
        }
        if self.drain.len() > DRAIN_KEEP {
            self.end_cancel();
        }
    }

    fn end_cancel(&mut self) {
        self.cancelling = None;
        self.outcome = Some(Outcome::Cancelled);
        let drain = std::mem::take(&mut self.drain);
        self.passthrough = text_tail(&drain).to_vec();
    }

    /// Repeats the last answer when the sender has gone quiet, and gives up after a while.
    /// Returns what to send.
    pub fn tick(&mut self, now: Instant) -> Vec<u8> {
        let mut reply = Vec::new();
        if self.outcome.is_some() {
            return reply;
        }
        if let Some(since) = self.cancelling {
            if now.duration_since(self.last_input) >= DRAIN_QUIET || now.duration_since(since) >= DRAIN_MAX {
                self.end_cancel();
            }
            return reply;
        }
        if let Stage::Finishing(since) = self.stage {
            if now.duration_since(since) >= OO_WAIT {
                self.outcome = Some(Outcome::Finished);
            }
            return reply;
        }
        if now.duration_since(self.last_input) >= GIVE_UP_AFTER {
            self.fail(t("서버의 응답이 없어 중단했습니다", "The server stopped answering").into(), true, &mut reply);
        } else if now.duration_since(self.last_input.max(self.last_retry)) >= RETRY_AFTER {
            self.last_retry = now;
            self.parser = Parser::new();
            match (&self.stage, &self.file) {
                (Stage::Receiving, Some(file)) => self.send(Header::at(ZRPOS, file.received), &mut reply),
                _ => self.send_rinit(&mut reply),
            }
        }
        reply
    }

    /// Stops the download (the user cancelled). Returns what tells the server to stop.
    /// What the sender had already sent is dropped until it goes quiet ([`Download::tick`]);
    /// then the outcome is [`Outcome::Cancelled`].
    pub fn cancel(&mut self, now: Instant) -> Vec<u8> {
        if self.outcome.is_none() && self.cancelling.is_none() {
            self.discard();
            self.cancelling = Some(now);
            self.last_input = now;
        }
        CANCEL.to_vec()
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// Output that came after the transfer ended; it belongs on the screen.
    pub fn take_passthrough(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.passthrough)
    }

    /// Files received completely, in order.
    pub fn saved(&self) -> &[PathBuf] {
        &self.saved
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn progress(&self) -> Option<Progress<'_>> {
        self.file.as_ref().map(|f| Progress { name: &f.name, received: f.received, size: f.size })
    }

    fn byte(&mut self, b: u8, now: Instant, reply: &mut Vec<u8>) {
        if b == ZDLE {
            self.cans += 1;
            if self.cans >= 5 {
                self.fail(t("서버가 전송을 취소했습니다", "The server cancelled the transfer").into(), false, reply);
                return;
            }
        } else {
            self.cans = 0;
        }
        if matches!(self.stage, Stage::Finishing(_)) && self.parser.idle() {
            if b == b'O' {
                self.oo += 1;
                if self.oo == 2 {
                    self.outcome = Some(Outcome::Finished);
                }
                return;
            }
            if matches!(b & 0x7f, b'\r' | b'\n' | XON | XOFF) {
                // The end of the sender's own header.
                return;
            }
            if b != ZPAD {
                // The shell is back without the "OO".
                self.outcome = Some(Outcome::Finished);
                self.passthrough.push(b);
                return;
            }
        }
        match self.parser.push(b) {
            Some(Frame::Header(header, crc32)) => self.header(header, crc32, now, reply),
            Some(Frame::BadHeader) => match (&self.stage, &self.file) {
                (Stage::Receiving, Some(file)) => self.send(Header::at(ZRPOS, file.received), reply),
                _ => self.send(Header::new(ZNAK), reply),
            },
            Some(Frame::Block(data, end)) => self.block(data, end, reply),
            Some(Frame::BadBlock) => match (&self.stage, &self.file) {
                (Stage::Receiving, Some(file)) => self.send(Header::at(ZRPOS, file.received), reply),
                (Stage::Sinit, _) => self.send(Header::new(ZNAK), reply),
                _ => self.send_rinit(reply),
            },
            None => {}
        }
    }

    fn header(&mut self, header: Header, crc32: bool, now: Instant, reply: &mut Vec<u8>) {
        self.crc32 = crc32;
        match header.kind {
            ZRQINIT => {
                if matches!(self.stage, Stage::Init) {
                    self.send_rinit(reply);
                }
            }
            ZSINIT => {
                self.stage = Stage::Sinit;
                self.parser.expect_data(crc32);
            }
            ZFILE => {
                // A new file (or the same one again) replaces one cut short.
                self.discard();
                self.stage = Stage::FileInfo;
                self.parser.expect_data(crc32);
            }
            ZDATA => match &self.file {
                Some(file) if header.position() == file.received as u32 => {
                    self.stage = Stage::Receiving;
                    self.parser.expect_data(crc32);
                }
                Some(file) => self.send(Header::at(ZRPOS, file.received), reply),
                None => self.send_rinit(reply),
            },
            ZEOF => {
                // An end at another position is ignored: it may have crossed a ZRPOS.
                if self.file.as_ref().is_some_and(|f| header.position() == f.received as u32) {
                    self.finish_file(reply);
                }
            }
            ZFIN => {
                self.send(Header::new(ZFIN), reply);
                self.stage = Stage::Finishing(now);
            }
            ZNAK => reply.extend_from_slice(&self.last_sent),
            ZFREECNT => self.send(Header { kind: ZACK, data: [0xff; 4] }, reply),
            ZCHALLENGE => self.send(Header { kind: ZACK, data: header.data }, reply),
            ZCOMMAND => self.fail(
                t("서버가 이 PC 에서 명령을 실행하라고 요청해 거부했습니다", "The server asked to run a command on this PC; refused")
                    .into(),
                true,
                reply,
            ),
            ZABORT | ZCAN | ZFERR => {
                self.fail(t("서버가 전송을 중단했습니다", "The server stopped the transfer").into(), false, reply)
            }
            _ => {}
        }
    }

    fn block(&mut self, data: Vec<u8>, end: u8, reply: &mut Vec<u8>) {
        match self.stage {
            Stage::Sinit => {
                self.send(Header::at(ZACK, 1), reply);
                self.stage = Stage::Init;
            }
            Stage::FileInfo => self.open_file(&data, reply),
            Stage::Receiving => {
                let Some(file) = &mut self.file else { return };
                if let Err(e) = file.out.write_all(&data) {
                    let message = tr!("{} 에 쓸 수 없습니다: {e}", "Cannot write {}: {e}", file.part.display());
                    self.fail(message, true, reply);
                    return;
                }
                file.received += data.len() as u64;
                let received = file.received;
                match end {
                    ZCRCG => self.parser.expect_data(self.crc32),
                    ZCRCQ => {
                        self.send(Header::at(ZACK, received), reply);
                        self.parser.expect_data(self.crc32);
                    }
                    ZCRCW => {
                        self.send(Header::at(ZACK, received), reply);
                        reply.push(XON);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// The ZFILE block: `name NUL size mtime mode ... NUL`.
    fn open_file(&mut self, info: &[u8], reply: &mut Vec<u8>) {
        let (raw_name, rest) = match info.iter().position(|&b| b == 0) {
            Some(i) => (&info[..i], &info[i + 1..]),
            None => (info, &[][..]),
        };
        let fields = rest.split(|&b| b == 0).next().unwrap_or_default();
        let size = std::str::from_utf8(fields).ok().and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse().ok());
        let name = local_name(raw_name);
        match create_part(&self.dir, &name) {
            Ok((part, file)) => {
                self.file = Some(Incoming { name, part, out: BufWriter::with_capacity(64 * 1024, file), size, received: 0 });
                self.stage = Stage::Receiving;
                self.send(Header::at(ZRPOS, 0), reply);
            }
            Err(e) => {
                let message = tr!("{} 에 파일을 만들 수 없습니다: {e}", "Cannot create a file in {}: {e}", self.dir.display());
                self.fail(message, true, reply);
            }
        }
    }

    /// The whole file arrived: gives it its name and asks for the next one.
    fn finish_file(&mut self, reply: &mut Vec<u8>) {
        let Some(file) = self.file.take() else { return };
        let Incoming { name, part, out, .. } = file;
        let result = out.into_inner().map_err(io::IntoInnerError::into_error).and_then(|f| f.sync_all()).and_then(|()| {
            let target = unique_path(&self.dir, &name);
            std::fs::rename(&part, &target).map(|()| target)
        });
        match result {
            Ok(target) => {
                self.saved.push(target);
                self.stage = Stage::Init;
                self.send_rinit(reply);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                self.fail(tr!("{name} 을(를) 저장할 수 없습니다: {e}", "Cannot save {name}: {e}"), true, reply);
            }
        }
    }

    fn send_rinit(&mut self, reply: &mut Vec<u8>) {
        self.send(Header { kind: ZRINIT, data: [0, 0, 0, CANFDX | CANOVIO | CANFC32] }, reply);
    }

    fn send(&mut self, header: Header, reply: &mut Vec<u8>) {
        self.last_sent = header.hex();
        reply.extend_from_slice(&self.last_sent);
    }

    /// Ends the download with an error; `tell_sender` cancels the transfer on the server too
    /// (not when the server ended it itself: the CAN bytes would reach its shell).
    fn fail(&mut self, message: String, tell_sender: bool, reply: &mut Vec<u8>) {
        self.discard();
        self.outcome = Some(Outcome::Failed(message));
        if tell_sender {
            reply.extend_from_slice(CANCEL);
        }
    }

    /// Drops the file being received.
    fn discard(&mut self) {
        if let Some(file) = self.file.take() {
            drop(file.out);
            let _ = std::fs::remove_file(&file.part);
        }
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        self.discard();
    }
}

/// The end of `bytes` that reads as text (printable ASCII, UTF-8, line ends, escape
/// sequences, echoed CAN), without the binary data before it.
fn text_tail(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let len = match bytes[i] {
            0x20..=0x7e | b'\r' | b'\n' | b'\t' | 0x1b | 0x07 | 0x08 | ZDLE => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 0,
        };
        if len == 1 || (len > 1 && bytes.get(i..i + len).is_some_and(|s| std::str::from_utf8(s).is_ok())) {
            i += len;
        } else {
            i += 1;
            start = i;
        }
    }
    &bytes[start..]
}

/// Moves a received file into `dir` under a name not taken there yet (copying it when `dir` is
/// on another drive). Returns where it went.
pub fn move_into(file: &Path, dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = file.file_name().ok_or_else(|| io::Error::other("no file name"))?.to_string_lossy();
    let target = unique_path(dir, &name);
    if std::fs::rename(file, &target).is_err() {
        if let Err(e) = std::fs::copy(file, &target) {
            let _ = std::fs::remove_file(&target);
            return Err(e);
        }
        std::fs::remove_file(file)?;
    }
    Ok(target)
}

/// A file name for this PC from the name the sender gave: its last part (a sender may send a
/// path), as UTF-8 or else CP949, with what Windows does not allow in a name replaced.
fn local_name(raw: &[u8]) -> String {
    let text = match std::str::from_utf8(raw) {
        Ok(text) => text.to_owned(),
        Err(_) => encoding_rs::EUC_KR.decode(raw).0.into_owned(),
    };
    let base = text.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut name: String = base
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*') { '_' } else { c })
        .take(200)
        .collect();
    name.truncate(name.trim_end_matches(['.', ' ']).len());
    if name.is_empty() {
        return "download".into();
    }
    if !is_safe_local_name(&name) {
        // A device name (`CON`, `nul.txt`).
        name.insert(0, '_');
    }
    name
}

/// A new temporary file in `dir` for receiving `name`.
fn create_part(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    std::fs::create_dir_all(dir)?;
    for i in 0..1000 {
        let part = dir.join(if i == 0 { format!(".{name}.part") } else { format!(".{name}.{i}.part") });
        match OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(file) => return Ok((part, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, name.to_owned()))
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`, ... when that exists: a download never replaces a
/// file.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ZRINIT this receiver sends, read as a position: its flags are in the last byte.
    const RINIT_FLAGS: u32 = u32::from_le_bytes([0, 0, 0, CANFDX | CANOVIO | CANFC32]);

    #[test]
    fn text_after_binary_data() {
        assert_eq!(text_tail(b"\x00\xff\x81ab\xea\xb0\x80\r\n\x1b[1m$ "), "ab가\r\n\x1b[1m$ ".as_bytes());
        assert_eq!(text_tail(b"plain"), b"plain");
        assert_eq!(text_tail(b"ab\xea\xb0"), b"", "a cut character is not text");
        assert_eq!(text_tail(b""), b"");
    }

    #[test]
    fn moving_keeps_existing_files() {
        let base = std::env::temp_dir().join(format!("playterm-zmodem-move-{}", std::process::id()));
        let (from, to) = (base.join("from"), base.join("to"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("a.txt"), b"old").unwrap();
        std::fs::write(from.join("a.txt"), b"new").unwrap();
        let moved = move_into(&from.join("a.txt"), &to).unwrap();
        assert_eq!(moved, to.join("a (1).txt"));
        assert_eq!(std::fs::read(&moved).unwrap(), b"new");
        assert_eq!(std::fs::read(to.join("a.txt")).unwrap(), b"old");
        assert!(!from.join("a.txt").exists());
        // A folder that does not exist yet is made.
        std::fs::write(from.join("b"), b"b").unwrap();
        assert_eq!(move_into(&from.join("b"), &to.join("sub")).unwrap(), to.join("sub").join("b"));
        std::fs::remove_dir_all(&base).unwrap();
    }

    // A small ZMODEM sender, to drive the receiver.

    fn escape(out: &mut Vec<u8>, data: &[u8]) {
        for &b in data {
            match b {
                ZDLE | 0x10 | XON | XOFF | 0x90 | 0x91 | 0x93 => out.extend_from_slice(&[ZDLE, b ^ 0x40]),
                0x7f => out.extend_from_slice(&[ZDLE, ZRUB0]),
                0xff => out.extend_from_slice(&[ZDLE, ZRUB1]),
                _ => out.push(b),
            }
        }
    }

    fn hex(kind: u8, data: [u8; 4]) -> Vec<u8> {
        Header { kind, data }.hex()
    }

    fn bin(kind: u8, pos: u32, wide: bool) -> Vec<u8> {
        let mut raw = vec![kind];
        raw.extend_from_slice(&pos.to_le_bytes());
        let mut out = vec![ZPAD, ZDLE, if wide { ZBIN32 } else { ZBIN }];
        let crc = if wide { (!crc32(!0, &raw)).to_le_bytes().to_vec() } else { crc16(0, &raw).to_be_bytes().to_vec() };
        raw.extend(crc);
        escape(&mut out, &raw);
        out
    }

    fn block(data: &[u8], end: u8, wide: bool) -> Vec<u8> {
        let mut out = Vec::new();
        escape(&mut out, data);
        out.extend_from_slice(&[ZDLE, end]);
        let crc = if wide {
            (!crc32(crc32(!0, data), &[end])).to_le_bytes().to_vec()
        } else {
            crc16(crc16(0, data), &[end]).to_be_bytes().to_vec()
        };
        escape(&mut out, &crc);
        out
    }

    /// The headers in what the receiver sent.
    fn headers(bytes: &[u8]) -> Vec<(u8, u32)> {
        let mut parser = Parser::new();
        bytes
            .iter()
            .filter_map(|&b| match parser.push(b) {
                Some(Frame::Header(h, _)) => Some((h.kind, h.position())),
                _ => None,
            })
            .collect()
    }

    fn file_info(name: &[u8], size: usize) -> Vec<u8> {
        let mut info = name.to_vec();
        info.push(0);
        info.extend_from_slice(format!("{size} 14712345670 100644 0 1 {size}").as_bytes());
        info.push(0);
        info
    }

    /// Feeds `bytes` in pieces of `step` bytes, as a network would.
    fn feed(download: &mut Download, bytes: &[u8], step: usize) -> Vec<u8> {
        let now = Instant::now();
        bytes.chunks(step.max(1)).flat_map(|c| download.feed(c, now)).collect()
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn all_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 256) as u8).collect()
    }

    /// Sends one file the way `sz` does: name, then data in blocks, then the end.
    fn send_file(download: &mut Download, name: &[u8], data: &[u8], wide: bool, step: usize) {
        let reply = feed(download, &[bin(ZFILE, 0, wide), block(&file_info(name, data.len()), ZCRCW, wide)].concat(), step);
        assert_eq!(headers(&reply), [(ZRPOS, 0)]);
        let mut stream = bin(ZDATA, 0, wide);
        let pieces: Vec<&[u8]> = data.chunks(1024).collect();
        let mut expected_acks = Vec::new();
        if data.is_empty() {
            stream.extend(block(b"", ZCRCE, wide));
        }
        let mut pos = 0;
        for (i, piece) in pieces.iter().enumerate() {
            pos += piece.len();
            let end = if i + 1 == pieces.len() {
                ZCRCE
            } else if i % 3 == 2 {
                expected_acks.push((ZACK, pos as u32));
                ZCRCQ
            } else {
                ZCRCG
            };
            stream.extend(block(piece, end, wide));
        }
        stream.extend(bin(ZEOF, data.len() as u32, wide));
        expected_acks.push((ZRINIT, RINIT_FLAGS));
        assert_eq!(headers(&feed(download, &stream, step)), expected_acks);
    }

    #[test]
    fn receives_files_and_hands_back_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let (mut download, reply) = Download::begin(dir.path().to_owned(), b"000000000000\r\x8a\x11", Instant::now());
        let rinit = headers(&reply);
        assert_eq!(rinit, [(ZRINIT, RINIT_FLAGS)]);

        // ZSINIT is acknowledged.
        let sinit = [bin(ZSINIT, 0, true), block(b"\0", ZCRCW, true)].concat();
        assert_eq!(headers(&feed(&mut download, &sinit, 64)), [(ZACK, 1)]);

        // CRC-32 and CRC-16, every byte value (escapes), whole and byte by byte.
        let big = all_bytes(5000);
        send_file(&mut download, b"/home/me/big.bin", &big, true, 4096);
        let small = all_bytes(700);
        send_file(&mut download, "한글 이름.txt".as_bytes(), &small, false, 1);
        send_file(&mut download, b"empty", b"", true, 7);
        assert_eq!(download.saved().len(), 3);

        let reply = feed(&mut download, &hex(ZFIN, [0; 4]), 5);
        assert_eq!(headers(&reply), [(ZFIN, 0)]);
        assert_eq!(download.outcome(), None, "waits for OO");
        download.feed(b"OO$ prompt", Instant::now());
        assert_eq!(download.outcome(), Some(&Outcome::Finished));
        assert_eq!(download.take_passthrough(), b"$ prompt");

        assert_eq!(files(dir.path()), ["big.bin", "empty", "한글 이름.txt"]);
        assert_eq!(std::fs::read(dir.path().join("big.bin")).unwrap(), big);
        assert_eq!(std::fs::read(dir.path().join("한글 이름.txt")).unwrap(), small);
    }

    #[test]
    fn errors_ask_for_a_resend_from_the_last_good_position() {
        let dir = tempfile::tempdir().unwrap();
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", Instant::now());
        let data = all_bytes(3000);
        feed(&mut download, &[bin(ZFILE, 0, true), block(&file_info(b"a.bin", 3000), ZCRCW, true)].concat(), 100);

        // The second block arrives damaged.
        let mut damaged = block(&data[1000..2000], ZCRCG, true);
        damaged[10] ^= 0x01;
        let stream = [bin(ZDATA, 0, true), block(&data[..1000], ZCRCG, true), damaged].concat();
        assert_eq!(headers(&feed(&mut download, &stream, 333)), [(ZRPOS, 1000)]);
        // Blocks still in flight are dropped until data from the right position comes.
        assert_eq!(headers(&feed(&mut download, &block(&data[2000..], ZCRCE, true), 50)), []);
        // Data from a wrong position is refused.
        assert_eq!(headers(&feed(&mut download, &bin(ZDATA, 2000, true), 50)), [(ZRPOS, 1000)]);
        // An end at a wrong position is ignored.
        assert_eq!(headers(&feed(&mut download, &bin(ZEOF, 3000, true), 50)), []);

        let resend = [bin(ZDATA, 1000, true), block(&data[1000..], ZCRCE, true), bin(ZEOF, 3000, true)].concat();
        assert_eq!(headers(&feed(&mut download, &resend, 77)), [(ZRINIT, RINIT_FLAGS)]);
        assert_eq!(std::fs::read(dir.path().join("a.bin")).unwrap(), data);

        // A damaged header gets a ZNAK, and the receiver repeats itself after one.
        let mut bad = hex(ZFIN, [0; 4]);
        bad[6] = b'9';
        assert_eq!(headers(&feed(&mut download, &bad, 50)), [(ZNAK, 0)]);
        assert_eq!(headers(&feed(&mut download, &hex(ZNAK, [0; 4]), 50)), [(ZNAK, 0)]);
    }

    #[test]
    fn cancelling_leaves_no_file() {
        // By the sender: five CAN.
        let dir = tempfile::tempdir().unwrap();
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", Instant::now());
        feed(&mut download, &[bin(ZFILE, 0, true), block(&file_info(b"a.bin", 3000), ZCRCW, true)].concat(), 100);
        feed(&mut download, &[bin(ZDATA, 0, true), block(&all_bytes(1000), ZCRCG, true)].concat(), 100);
        assert_eq!(download.progress().map(|p| (p.name.to_owned(), p.received, p.size)), Some(("a.bin".into(), 1000, Some(3000))));
        let reply = feed(&mut download, b"\x18\x18\x18\x18\x18\x18\x08\x08$ ", 100);
        assert!(matches!(download.outcome(), Some(Outcome::Failed(_))));
        assert!(reply.is_empty(), "no CAN back to a shell");
        assert_eq!(files(dir.path()), Vec::<String>::new());

        // By the user. What the sender had already sent is dropped, the prompt after it kept.
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", Instant::now());
        feed(&mut download, &[bin(ZFILE, 0, true), block(&file_info(b"a.bin", 3000), ZCRCW, true)].concat(), 100);
        feed(&mut download, &[bin(ZDATA, 0, true), block(&all_bytes(1000), ZCRCG, true)].concat(), 100);
        assert_eq!(download.cancel(Instant::now()), CANCEL);
        assert_eq!(files(dir.path()), Vec::<String>::new());
        assert_eq!(download.outcome(), None, "still dropping what was on its way");
        // (The shell echoes the CAN bytes it got after the sender ended.)
        let late = [block(&all_bytes(3000), ZCRCG, true), block(&all_bytes(500), ZCRCE, true), b"\r\n$ \x18\x18\x08 \x08".to_vec()].concat();
        assert!(feed(&mut download, &late, 1).is_empty());
        assert!(download.tick(Instant::now()).is_empty());
        assert_eq!(download.outcome(), None, "not quiet yet");
        download.tick(Instant::now() + Duration::from_secs(1));
        assert_eq!(download.outcome(), Some(&Outcome::Cancelled));
        let shown = download.take_passthrough();
        assert!(shown.ends_with(b"\r\n$ \x18\x18\x08 \x08") && shown.len() < 16, "{shown:?}");
        assert_eq!(files(dir.path()), Vec::<String>::new());

        // A sender that does not stop is given up on after a while.
        let start = Instant::now();
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", start);
        download.cancel(start);
        download.feed(&block(&all_bytes(1000), ZCRCG, true), start + Duration::from_secs(4));
        download.tick(start + Duration::from_secs(4));
        assert_eq!(download.outcome(), None);
        download.feed(&block(&all_bytes(1000), ZCRCG, true), start + Duration::from_secs(6));
        download.tick(start + Duration::from_secs(6));
        assert_eq!(download.outcome(), Some(&Outcome::Cancelled));

        // Dropped mid-file (the connection closed).
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", Instant::now());
        feed(&mut download, &[bin(ZFILE, 0, true), block(&file_info(b"a.bin", 3000), ZCRCW, true)].concat(), 100);
        drop(download);
        assert_eq!(files(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn a_command_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", Instant::now());
        let reply = feed(&mut download, &[bin(ZCOMMAND, 0, true), block(b"rm -rf /\0", ZCRCW, true)].concat(), 100);
        assert!(matches!(download.outcome(), Some(Outcome::Failed(_))));
        assert!(reply.ends_with(CANCEL));
    }

    #[test]
    fn existing_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"old").unwrap();
        std::fs::write(dir.path().join("a (1).txt"), b"old").unwrap();
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", Instant::now());
        send_file(&mut download, b"a.txt", b"new", true, 100);
        assert_eq!(download.saved(), [dir.path().join("a (2).txt")]);
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"old");
    }

    #[test]
    fn quiet_sender_gets_reminded_then_given_up() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", start);
        assert!(download.tick(start + Duration::from_secs(1)).is_empty());
        assert_eq!(headers(&download.tick(start + RETRY_AFTER)), [(ZRINIT, RINIT_FLAGS)]);
        assert!(download.tick(start + RETRY_AFTER + Duration::from_secs(1)).is_empty());
        assert!(download.tick(start + GIVE_UP_AFTER).ends_with(CANCEL));
        assert!(matches!(download.outcome(), Some(Outcome::Failed(_))));

        // No "OO" after ZFIN: done after a moment.
        let (mut download, _) = Download::begin(dir.path().to_owned(), b"000000000000\r\n", start);
        download.feed(&hex(ZFIN, [0; 4]), start);
        download.tick(start + OO_WAIT);
        assert_eq!(download.outcome(), Some(&Outcome::Finished));
    }

    #[test]
    fn detects_starts_across_chunks() {
        let mut d = Detector::default();
        assert_eq!(d.scan(b"$ sz a.txt\r\nrz\r***\x18B00000000000000\r\x8a\x11"), Some(Start::Download { at: 16, rest: 22 }));
        assert_eq!(d.scan(b"rz\r**"), None);
        assert_eq!(d.scan(b"\x18B0"), None);
        assert_eq!(d.scan(b"0000"), Some(Start::Download { at: 0, rest: 1 }));
        assert_eq!(d.scan(b"x**\x18B0100000023be50"), Some(Start::Upload { at: 1, rest: 7 }));
        assert_eq!(d.scan(b"**\x18B09 and ** \x18B00"), None);
    }

    #[test]
    fn names_from_the_sender_stay_in_the_folder() {
        assert_eq!(local_name(b"../../etc/passwd"), "passwd");
        assert_eq!(local_name(b"C:\\Windows\\win.ini"), "win.ini");
        assert_eq!(local_name(b"a:b?.txt"), "a_b_.txt");
        assert_eq!(local_name(b"CON"), "_CON");
        assert_eq!(local_name(b"nul.txt"), "_nul.txt");
        assert_eq!(local_name(b"dir/.."), "download");
        assert_eq!(local_name(b"name. . "), "name");
        assert_eq!(local_name(b""), "download");
        assert_eq!(local_name(&encoding_rs::EUC_KR.encode("한글.txt").0), "한글.txt");
    }
}
