//! ZMODEM file transfers through the terminal connection (SSH, Telnet or serial): `sz file` run
//! on the server sends files, which are saved into a folder on this PC, and `rz` takes files sent
//! from this PC ([`Upload`]).
//!
//! [`Detector`] spots the start of a transfer in the output of the remote side; from there a
//! [`Download`] takes all output until the transfer is over. It answers the sender, checks every
//! header and data block (CRC-16 or CRC-32), asks for a resend from the last good position after
//! an error, and writes each file under a temporary name first, so a broken transfer leaves no
//! half file behind. A command the sender asks to run (ZCOMMAND) is refused.
//!
//! An [`Upload`] offers the files the user picked one by one, starts each where the receiver
//! asks (ZRPOS: from the start, after an error, or to resume a file it already has a part of),
//! and streams the data in a window it keeps acknowledged (ZCRCQ), or stop-and-wait (ZCRCW) for
//! a receiver that cannot take data while it writes.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
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
const ZSKIP: u8 = 5;
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
/// The receiver wants every control character escaped.
const ESCCTL: u8 = 0x40;
/// ZFILE ZF0: a binary file (no newline conversion).
const ZCBIN: u8 = 1;

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

/// ZDLE-escapes `data` for the line: ZDLE, flow control, CR (a Telnet or terminal line may
/// change it), DEL and 0xff (Telnet's IAC); with `ctl` every control character.
fn escape(out: &mut Vec<u8>, data: &[u8], ctl: bool) {
    for &b in data {
        match b {
            ZDLE | 0x10 | XON | XOFF | 0x90 | 0x91 | 0x93 | b'\r' | 0x8d => out.extend_from_slice(&[ZDLE, b ^ 0x40]),
            0x7f => out.extend_from_slice(&[ZDLE, ZRUB0]),
            0xff => out.extend_from_slice(&[ZDLE, ZRUB1]),
            _ if ctl && b & 0x60 == 0 => out.extend_from_slice(&[ZDLE, b ^ 0x40]),
            _ => out.push(b),
        }
    }
}

/// A header in binary form (the form a sender sends), with CRC-32 if `wide`.
fn bin_frame(header: Header, wide: bool, ctl: bool) -> Vec<u8> {
    let mut raw = vec![header.kind];
    raw.extend_from_slice(&header.data);
    let mut out = vec![ZPAD, ZDLE, if wide { ZBIN32 } else { ZBIN }];
    let crc = if wide { (!crc32(!0, &raw)).to_le_bytes().to_vec() } else { crc16(0, &raw).to_be_bytes().to_vec() };
    raw.extend(crc);
    escape(&mut out, &raw, ctl);
    out
}

/// A data block ending with `end` (ZCRCE, ZCRCG, ZCRCQ or ZCRCW).
fn data_frame(data: &[u8], end: u8, wide: bool, ctl: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 8);
    escape(&mut out, data, ctl);
    out.extend_from_slice(&[ZDLE, end]);
    let crc =
        if wide { (!crc32(crc32(!0, data), &[end])).to_le_bytes().to_vec() } else { crc16(crc16(0, data), &[end]).to_be_bytes().to_vec() };
    escape(&mut out, &crc, ctl);
    out
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
    /// `rz` on the server waits for files to be sent to it (see [`Upload`]).
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

// -- sending ----------------------------------------------------------------------------------

/// Data per block sent (what `sz` sends; every receiver takes it).
const SEND_BLOCK: usize = 1024;
/// Streaming: a block asking for an acknowledgement (ZCRCQ) every this many bytes...
const ACK_EVERY: u64 = 4 * 1024;
/// ...and at most this much sent and not acknowledged.
const WINDOW: u64 = 128 * 1024;
/// Stop-and-wait (the receiver cannot take data while it writes): bytes per ZCRCW, unless it
/// names its buffer size.
const STOP_AND_WAIT: u64 = 8 * 1024;
/// ZRPOS for the same position this many times in a row: the line is too bad, give up.
const MAX_RESENDS: usize = 20;

/// The first header of an upload (ZRINIT), which [`Detector`] found.
const UPLOAD_START: &[u8] = b"**\x18B01";

enum UpStage {
    /// The receiver is waiting; the files are being picked ([`Upload::choose`]).
    Choosing,
    /// ZFILE sent: waiting for where to start (ZRPOS), or ZSKIP.
    Offered,
    /// Sending data.
    Sending,
    /// A block that waits for an acknowledgement (ZCRCW) was sent.
    WaitAck,
    /// ZEOF sent: waiting for the receiver to take the next file (ZRINIT).
    Eof,
    /// ZFIN sent: waiting for the receiver's ZFIN.
    Fin,
}

struct Outgoing {
    name: String,
    path: PathBuf,
    file: File,
    size: u64,
    /// Sent up to here...
    pos: u64,
    /// ...and the receiver has confirmed up to here.
    acked: u64,
    /// Since the last block that asked for an acknowledgement.
    since_ack: u64,
    /// The last ZRPOS and how many came in a row for it.
    resends: (u64, usize),
}

/// What became of a file of an [`Upload`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    Done(PathBuf),
    /// The receiver did not want it (e.g. it has a file of that name).
    Skipped(PathBuf),
}

/// A ZMODEM upload in progress: `rz` on the server takes the files picked on this PC (see the
/// module docs).
pub struct Upload {
    parser: Parser,
    stage: UpStage,
    /// The receiver takes CRC-32, wants control characters escaped, and how much data it takes
    /// before it must be asked for an acknowledgement (0: any amount, while it writes).
    crc32: bool,
    escctl: bool,
    streaming: bool,
    buffer: u64,
    queue: std::collections::VecDeque<PathBuf>,
    file: Option<Outgoing>,
    sent: Vec<Sent>,
    outcome: Option<Outcome>,
    passthrough: Vec<u8>,
    cans: usize,
    last_input: Instant,
    last_retry: Instant,
    /// The last header (and block) sent, sent again when the receiver is quiet or asks (ZNAK).
    last_sent: Vec<u8>,
    cancelling: Option<Instant>,
    drain: Vec<u8>,
}

impl Upload {
    /// Starts an upload with the output that came after the receiver's first header
    /// ([`Detector`]). It waits for the files ([`Upload::choose`]); returns what to send.
    pub fn begin(rest: &[u8], now: Instant) -> (Self, Vec<u8>) {
        let mut upload = Self {
            parser: Parser::new(),
            stage: UpStage::Choosing,
            crc32: false,
            escctl: false,
            streaming: true,
            buffer: 0,
            queue: Default::default(),
            file: None,
            sent: Vec::new(),
            outcome: None,
            passthrough: Vec::new(),
            cans: 0,
            last_input: now,
            last_retry: now,
            last_sent: Vec::new(),
            cancelling: None,
            drain: Vec::new(),
        };
        let mut reply = upload.feed(UPLOAD_START, now);
        reply.extend(upload.feed(rest, now));
        (upload, reply)
    }

    /// Whether the files are still to be picked.
    pub fn choosing(&self) -> bool {
        matches!(self.stage, UpStage::Choosing) && self.outcome.is_none() && self.cancelling.is_none()
    }

    /// The files to send, in order; none cancels. Returns what to send.
    pub fn choose(&mut self, files: Vec<PathBuf>, now: Instant) -> Vec<u8> {
        if !self.choosing() {
            return Vec::new();
        }
        if files.is_empty() {
            return self.cancel(now);
        }
        self.queue = files.into();
        self.last_input = now;
        self.last_retry = now;
        let mut reply = Vec::new();
        self.next_file(&mut reply);
        reply
    }

    /// Takes output of the remote side. Returns what to send.
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
            self.byte(b, &mut reply);
        }
        self.pump(&mut reply);
        reply
    }

    /// Sends more data if the window has room, repeats the last header when the receiver has
    /// gone quiet, and gives up after a while. Returns what to send.
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
        if self.choosing() {
            // The receiver waits as long as it likes (and gives up by itself).
            return reply;
        }
        self.pump(&mut reply);
        let quiet = now.duration_since(self.last_input);
        if quiet >= GIVE_UP_AFTER {
            self.fail(t("서버의 응답이 없어 중단했습니다", "The server stopped answering").into(), true, &mut reply);
        } else if now.duration_since(self.last_input.max(self.last_retry)) >= RETRY_AFTER {
            self.last_retry = now;
            match (&self.stage, self.file.as_ref().map(|f| f.acked)) {
                // Acknowledgements stopped coming: go on from the last one (a receiver that
                // is further asks for its position).
                (UpStage::Sending | UpStage::WaitAck, Some(acked)) => self.restart_at(acked, &mut reply),
                _ => reply.extend_from_slice(&self.last_sent),
            }
        }
        reply
    }

    /// Stops the upload (the user cancelled). Returns what tells the server to stop.
    pub fn cancel(&mut self, now: Instant) -> Vec<u8> {
        if self.outcome.is_none() && self.cancelling.is_none() {
            self.file = None;
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

    /// The files dealt with so far, in order.
    pub fn sent(&self) -> &[Sent] {
        &self.sent
    }

    /// The file being sent: how much of it the receiver has.
    pub fn progress(&self) -> Option<Progress<'_>> {
        self.file.as_ref().map(|f| Progress { name: &f.name, received: f.acked, size: Some(f.size) })
    }

    fn drain(&mut self, input: &[u8]) {
        self.drain.extend_from_slice(input);
        // The receiver's headers are text, so all of it after its last header's start.
        if let Some(i) = self.drain.windows(2).rposition(|w| w == [ZPAD, ZDLE] || w == [ZDLE, ZDLE]) {
            let end = self.drain[i..].iter().position(|&b| b == b'\n').map(|n| i + n + 1);
            match end {
                Some(end) => drop(self.drain.drain(..end)),
                None => self.drain.drain(..i).for_each(drop),
            }
        }
        if self.drain.len() > DRAIN_KEEP {
            self.end_cancel();
        }
    }

    fn end_cancel(&mut self) {
        self.cancelling = None;
        self.outcome = Some(Outcome::Cancelled);
        let drain = std::mem::take(&mut self.drain);
        let text: Vec<u8> = text_tail(&drain).iter().copied().filter(|&b| b != ZDLE && b != XON).collect();
        self.passthrough = text;
    }

    fn byte(&mut self, b: u8, reply: &mut Vec<u8>) {
        if b == ZDLE {
            self.cans += 1;
            if self.cans >= 5 {
                self.fail(t("서버가 전송을 취소했습니다", "The server cancelled the transfer").into(), false, reply);
                return;
            }
        } else {
            self.cans = 0;
        }
        if matches!(self.stage, UpStage::Fin) && self.parser.idle() && b != ZPAD && !matches!(b & 0x7f, b'\r' | b'\n' | XON | XOFF) {
            // The receiver is gone and its shell is back.
            self.finish(reply);
            self.passthrough.push(b);
            return;
        }
        match self.parser.push(b) {
            Some(Frame::Header(header, _)) => self.header(header, reply),
            Some(Frame::BadHeader) if !matches!(self.stage, UpStage::Choosing) => reply.extend_from_slice(&self.last_sent),
            _ => {}
        }
    }

    fn header(&mut self, header: Header, reply: &mut Vec<u8>) {
        match header.kind {
            ZRINIT => {
                let flags = header.data[3];
                self.crc32 = flags & CANFC32 != 0;
                self.escctl = flags & ESCCTL != 0;
                self.buffer = u16::from_le_bytes([header.data[0], header.data[1]]) as u64;
                self.streaming = self.buffer == 0 && flags & CANFDX != 0 && flags & CANOVIO != 0;
                match self.stage {
                    UpStage::Eof => {
                        if let Some(file) = self.file.take() {
                            self.sent.push(Sent::Done(file.path));
                        }
                        self.next_file(reply);
                    }
                    // It did not get the offer, or the end.
                    UpStage::Offered | UpStage::Fin => reply.extend_from_slice(&self.last_sent),
                    _ => {}
                }
            }
            ZRPOS => {
                let pos = header.position() as u64;
                if !matches!(self.stage, UpStage::Offered | UpStage::Sending | UpStage::WaitAck | UpStage::Eof) {
                    return;
                }
                let Some(file) = &mut self.file else { return };
                if pos > file.size {
                    self.fail(tr!("서버가 파일 크기를 넘는 위치({pos})를 요청했습니다", "The server asked for a position ({pos}) past the end of the file"), true, reply);
                    return;
                }
                if !matches!(self.stage, UpStage::Offered) {
                    file.resends = if file.resends.0 == pos { (pos, file.resends.1 + 1) } else { (pos, 1) };
                    if file.resends.1 > MAX_RESENDS {
                        self.fail(t("오류가 계속되어 중단했습니다", "Too many errors on the line").into(), true, reply);
                        return;
                    }
                }
                self.restart_at(pos, reply);
            }
            ZACK => {
                let pos = header.position() as u64;
                if let Some(file) = &mut self.file
                    && pos <= file.pos
                {
                    file.acked = file.acked.max(pos);
                    if matches!(self.stage, UpStage::WaitAck) && pos == file.pos {
                        self.stage = UpStage::Sending;
                        self.send(bin_frame(Header::at(ZDATA, pos), self.crc32, self.escctl), reply);
                    }
                }
            }
            ZSKIP => {
                if matches!(self.stage, UpStage::Offered | UpStage::Sending | UpStage::WaitAck | UpStage::Eof)
                    && let Some(file) = self.file.take()
                {
                    self.sent.push(Sent::Skipped(file.path));
                    self.next_file(reply);
                }
            }
            ZFIN => {
                if matches!(self.stage, UpStage::Fin) {
                    self.finish(reply);
                }
            }
            ZNAK => reply.extend_from_slice(&self.last_sent),
            ZABORT | ZCAN | ZFERR => self.fail(t("서버가 전송을 중단했습니다", "The server stopped the transfer").into(), false, reply),
            _ => {}
        }
    }

    /// Offers the next file, or ends the session when there is none.
    fn next_file(&mut self, reply: &mut Vec<u8>) {
        if let Some(path) = self.queue.pop_front() {
            let opened = File::open(&path).and_then(|file| {
                let meta = file.metadata()?;
                if meta.is_dir() {
                    return Err(io::Error::other(t("폴더는 보낼 수 없습니다", "a folder cannot be sent")));
                }
                Ok((file, meta))
            });
            let (file, meta) = match opened {
                Ok(f) => f,
                Err(e) => {
                    let name = path.display().to_string();
                    self.fail(tr!("{name} 을(를) 열 수 없습니다: {e}", "Cannot open {name}: {e}"), true, reply);
                    return;
                }
            };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let size = meta.len();
            let mtime = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            let left_bytes: u64 = size + self.queue.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum::<u64>();
            let mut info = name.as_bytes().to_vec();
            info.push(0);
            info.extend_from_slice(format!("{size} {mtime:o} 100644 0 {} {left_bytes}", self.queue.len() + 1).as_bytes());
            info.push(0);
            let frame = [
                bin_frame(Header { kind: ZFILE, data: [0, 0, 0, ZCBIN] }, self.crc32, self.escctl),
                data_frame(&info, ZCRCW, self.crc32, self.escctl),
            ]
            .concat();
            self.file = Some(Outgoing { name, path, file, size, pos: 0, acked: 0, since_ack: 0, resends: (0, 0) });
            self.stage = UpStage::Offered;
            self.send(frame, reply);
            return;
        }
        self.stage = UpStage::Fin;
        self.send(Header::new(ZFIN).hex(), reply);
    }

    /// Sends the data from `pos` on (where the receiver asked).
    fn restart_at(&mut self, pos: u64, reply: &mut Vec<u8>) {
        let Some(file) = &mut self.file else { return };
        if let Err(e) = file.file.seek(SeekFrom::Start(pos)) {
            let name = file.name.clone();
            self.fail(tr!("{name} 을(를) 읽을 수 없습니다: {e}", "Cannot read {name}: {e}"), true, reply);
            return;
        }
        file.pos = pos;
        file.acked = pos;
        file.since_ack = 0;
        self.stage = UpStage::Sending;
        self.send(bin_frame(Header::at(ZDATA, pos), self.crc32, self.escctl), reply);
    }

    /// Sends data while the window has room; at the end of the file, ZEOF.
    fn pump(&mut self, reply: &mut Vec<u8>) {
        if !matches!(self.stage, UpStage::Sending) || self.outcome.is_some() || self.cancelling.is_some() {
            return;
        }
        let (wide, ctl, streaming) = (self.crc32, self.escctl, self.streaming);
        let window = if streaming { WINDOW } else if self.buffer > 0 { self.buffer.min(STOP_AND_WAIT) } else { STOP_AND_WAIT };
        let Some(file) = &mut self.file else { return };
        let mut buf = vec![0u8; SEND_BLOCK];
        loop {
            if streaming && file.pos - file.acked >= WINDOW {
                return;
            }
            let want = (SEND_BLOCK as u64).min(file.size - file.pos).min(window.saturating_sub(file.since_ack).max(1)) as usize;
            let mut got = 0;
            while got < want {
                match file.file.read(&mut buf[got..want]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        let name = file.name.clone();
                        self.fail(tr!("{name} 을(를) 읽을 수 없습니다: {e}", "Cannot read {name}: {e}"), true, reply);
                        return;
                    }
                }
            }
            file.pos += got as u64;
            file.since_ack += got as u64;
            // A file that shrank while being sent ends where its data ends.
            let at_end = file.pos >= file.size || got < want;
            if at_end {
                file.size = file.pos;
                reply.extend(data_frame(&buf[..got], ZCRCE, wide, ctl));
                let eof = bin_frame(Header::at(ZEOF, file.pos), wide, ctl);
                self.stage = UpStage::Eof;
                self.send(eof, reply);
                return;
            }
            if !streaming && file.since_ack >= window {
                file.since_ack = 0;
                reply.extend(data_frame(&buf[..got], ZCRCW, wide, ctl));
                self.stage = UpStage::WaitAck;
                // Repeated from the last acknowledged position if it gets lost.
                self.last_sent = bin_frame(Header::at(ZDATA, file.acked), wide, ctl);
                return;
            }
            let end = if file.since_ack >= ACK_EVERY {
                file.since_ack = 0;
                ZCRCQ
            } else {
                ZCRCG
            };
            reply.extend(data_frame(&buf[..got], end, wide, ctl));
        }
    }

    fn finish(&mut self, reply: &mut Vec<u8>) {
        reply.extend_from_slice(b"OO");
        self.outcome = Some(Outcome::Finished);
    }

    fn send(&mut self, bytes: Vec<u8>, reply: &mut Vec<u8>) {
        reply.extend_from_slice(&bytes);
        self.last_sent = bytes;
    }

    fn fail(&mut self, message: String, tell_receiver: bool, reply: &mut Vec<u8>) {
        self.file = None;
        self.outcome = Some(Outcome::Failed(message));
        if tell_receiver {
            reply.extend_from_slice(CANCEL);
        }
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

    fn hex(kind: u8, data: [u8; 4]) -> Vec<u8> {
        Header { kind, data }.hex()
    }

    fn bin(kind: u8, pos: u32, wide: bool) -> Vec<u8> {
        bin_frame(Header::at(kind, pos as u64), wide, false)
    }

    fn block(data: &[u8], end: u8, wide: bool) -> Vec<u8> {
        data_frame(data, end, wide, false)
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

    // -- sending --

    /// Runs `upload` against `download` (this module's own receiver) until both are over;
    /// `damage` may change what the sender sends on its way.
    fn exchange(upload: &mut Upload, download: &mut Download, first: Vec<u8>, mut damage: impl FnMut(&mut Vec<u8>)) {
        let mut now = Instant::now();
        let mut to_receiver = first;
        for _ in 0..100_000 {
            damage(&mut to_receiver);
            let mut to_sender = Vec::new();
            for chunk in to_receiver.chunks(700) {
                to_sender.extend(download.feed(chunk, now));
            }
            to_sender.extend(download.tick(now));
            to_receiver = upload.feed(&to_sender, now);
            to_receiver.extend(upload.tick(now));
            if upload.outcome().is_some() && download.outcome().is_some() {
                return;
            }
            if to_receiver.is_empty() && to_sender.is_empty() {
                // Nothing moves: let time pass (retries).
                now += Duration::from_secs(1);
            }
        }
        panic!("the transfer did not end");
    }

    /// An upload to this module's receiver: `rz` answered by a [`Download`].
    fn start_upload(dir: &Path) -> (Upload, Download) {
        let now = Instant::now();
        let (download, rinit) = Download::begin(dir.to_owned(), b"000000000000\r\n", now);
        let Some(Start::Upload { rest, .. }) = Detector::default().scan(&rinit) else { panic!("no ZRINIT in {rinit:?}") };
        let (upload, reply) = Upload::begin(&rinit[rest..], now);
        assert!(reply.is_empty() && upload.choosing());
        (upload, download)
    }

    #[test]
    fn sends_files_to_the_receiver() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let big = all_bytes(300_000);
        let control: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        for (name, data) in [("big.bin", &big[..]), ("empty", &[][..]), ("control.bin", &control[..])] {
            std::fs::write(src.path().join(name), data).unwrap();
        }
        let (mut upload, mut download) = start_upload(dst.path());
        let first = upload.choose(["big.bin", "empty", "control.bin"].map(|n| src.path().join(n)).to_vec(), Instant::now());
        exchange(&mut upload, &mut download, first, |_| {});
        assert_eq!(upload.outcome(), Some(&Outcome::Finished));
        assert_eq!(download.outcome(), Some(&Outcome::Finished));
        assert_eq!(upload.sent().len(), 3);
        assert_eq!(std::fs::read(dst.path().join("big.bin")).unwrap(), big);
        assert_eq!(std::fs::read(dst.path().join("empty")).unwrap(), b"");
        assert_eq!(std::fs::read(dst.path().join("control.bin")).unwrap(), control);
    }

    #[test]
    fn damaged_data_is_sent_again() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let data = all_bytes(200_000);
        std::fs::write(src.path().join("a.bin"), &data).unwrap();
        let (mut upload, mut download) = start_upload(dst.path());
        let first = upload.choose(vec![src.path().join("a.bin")], Instant::now());
        // Every few rounds a byte of the data is changed on the way, and some are lost.
        let mut round = 0;
        exchange(&mut upload, &mut download, first, |bytes| {
            round += 1;
            if bytes.len() > 5000 && round % 3 == 0 {
                bytes[2500] ^= 0x55;
            }
            if bytes.len() > 9000 && round % 7 == 0 {
                bytes.drain(4000..4100);
            }
        });
        assert_eq!(upload.outcome(), Some(&Outcome::Finished));
        assert_eq!(std::fs::read(dst.path().join("a.bin")).unwrap(), data);
    }

    #[test]
    fn upload_cancels_and_fails_cleanly() {
        let dst = tempfile::tempdir().unwrap();
        // Nothing picked: the receiver is told to stop.
        let (mut upload, _) = start_upload(dst.path());
        let now = Instant::now();
        assert_eq!(upload.choose(Vec::new(), now), CANCEL);
        upload.feed(b"\r\n$ ", now);
        upload.tick(now + DRAIN_QUIET);
        assert_eq!(upload.outcome(), Some(&Outcome::Cancelled));
        assert_eq!(upload.take_passthrough(), b"\r\n$ ");

        // A file that cannot be read.
        let (mut upload, _) = start_upload(dst.path());
        assert!(upload.choose(vec![dst.path().join("missing")], now).ends_with(CANCEL));
        assert!(matches!(upload.outcome(), Some(Outcome::Failed(_))));

        // The receiver skips a file it has (ZSKIP): the next one goes.
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a"), b"aaa").unwrap();
        std::fs::write(src.path().join("b"), b"bbb").unwrap();
        let (mut upload, _) = start_upload(dst.path());
        upload.choose(vec![src.path().join("a"), src.path().join("b")], now);
        let reply = upload.feed(&hex(ZSKIP, [0; 4]), now);
        assert_eq!(headers(&reply), [(ZFILE, u32::from_le_bytes([0, 0, 0, ZCBIN]))]);
        assert_eq!(upload.sent(), [Sent::Skipped(src.path().join("a"))]);
        assert_eq!(upload.progress().unwrap().name, "b");

        // A quiet receiver is asked again, then given up.
        let start = Instant::now();
        assert!(upload.tick(start + RETRY_AFTER).windows(1).count() > 0);
        assert!(upload.tick(start + GIVE_UP_AFTER).ends_with(CANCEL));
        assert!(matches!(upload.outcome(), Some(Outcome::Failed(_))));
    }

    #[test]
    fn upload_resumes_where_the_receiver_asks() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a"), all_bytes(10_000)).unwrap();
        let dst = tempfile::tempdir().unwrap();
        let (mut upload, _) = start_upload(dst.path());
        let now = Instant::now();
        upload.choose(vec![src.path().join("a")], now);
        // The receiver has 6000 bytes already.
        let reply = upload.feed(&hex(ZRPOS, 6000u32.to_le_bytes()), now);
        let mut parser = Parser::new();
        let mut data = Vec::new();
        let mut found = Vec::new();
        for &b in &reply {
            match parser.push(b) {
                Some(Frame::Header(h, wide)) => {
                    found.push((h.kind, h.position()));
                    if h.kind == ZDATA {
                        parser.expect_data(wide);
                    }
                }
                Some(Frame::Block(block, end)) => {
                    data.extend(block);
                    if end == ZCRCG || end == ZCRCQ {
                        parser.expect_data(true);
                    }
                }
                _ => {}
            }
        }
        assert_eq!(found, [(ZDATA, 6000), (ZEOF, 10_000)]);
        assert_eq!(data, all_bytes(10_000)[6000..]);
    }

    /// `rz` / `sz` of lrzsz, when installed.
    fn lrzsz(program: &str, args: &[&str], dir: &Path) -> Option<std::process::Child> {
        use std::process::{Command, Stdio};
        let child = Command::new(program)
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(child) => Some(child),
            Err(_) => {
                eprintln!("{program} is not installed: skipped");
                None
            }
        }
    }

    /// The output of a child process, as it comes.
    fn reader(child: &mut std::process::Child) -> std::sync::mpsc::Receiver<Vec<u8>> {
        use std::io::Read;
        let mut out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        rx
    }

    #[test]
    fn sends_to_lrzsz_rz() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let big = all_bytes(1_000_000);
        std::fs::write(src.path().join("big.bin"), &big).unwrap();
        std::fs::write(src.path().join("small.txt"), b"hello\r\n\x18\x11\x13\xff").unwrap();
        // A part received earlier: `rz -r` asks to go on from its end.
        std::fs::write(dst.path().join("big.bin"), &big[..300_000]).unwrap();
        let Some(mut rz) = lrzsz("rz", &["-r"], dst.path()) else { return };
        let output = reader(&mut rz);
        let mut stdin = rz.stdin.take().unwrap();
        let mut detector = Detector::default();
        let mut upload: Option<Upload> = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let chunk = output.recv_timeout(Duration::from_millis(50)).unwrap_or_default();
            let now = Instant::now();
            let reply = match &mut upload {
                None => match detector.scan(&chunk) {
                    Some(Start::Upload { rest, .. }) => {
                        let (mut up, mut reply) = Upload::begin(&chunk[rest..], now);
                        reply.extend(up.choose(vec![src.path().join("big.bin"), src.path().join("small.txt")], now));
                        upload = Some(up);
                        reply
                    }
                    _ => Vec::new(),
                },
                Some(up) => [up.feed(&chunk, now), up.tick(now)].concat(),
            };
            if !reply.is_empty() {
                stdin.write_all(&reply).unwrap();
            }
            if upload.as_ref().is_some_and(|u| u.outcome().is_some()) {
                break;
            }
        }
        drop(stdin);
        let status = rz.wait().unwrap();
        let upload = upload.expect("rz sent no ZRINIT");
        assert_eq!(upload.outcome(), Some(&Outcome::Finished));
        assert!(status.success(), "rz: {status}");
        assert_eq!(std::fs::read(dst.path().join("big.bin")).unwrap(), big);
        assert_eq!(std::fs::read(dst.path().join("small.txt")).unwrap(), b"hello\r\n\x18\x11\x13\xff");
    }

    #[test]
    fn receives_from_lrzsz_sz() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let data = all_bytes(500_000);
        std::fs::write(src.path().join("a.bin"), &data).unwrap();
        let Some(mut sz) = lrzsz("sz", &["a.bin"], src.path()) else { return };
        let output = reader(&mut sz);
        let mut stdin = sz.stdin.take().unwrap();
        let mut detector = Detector::default();
        let mut download: Option<Download> = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let chunk = output.recv_timeout(Duration::from_millis(50)).unwrap_or_default();
            let now = Instant::now();
            let reply = match &mut download {
                None => match detector.scan(&chunk) {
                    Some(Start::Download { rest, .. }) => {
                        let (down, reply) = Download::begin(dst.path().to_owned(), &chunk[rest..], now);
                        download = Some(down);
                        reply
                    }
                    _ => Vec::new(),
                },
                Some(down) => [down.feed(&chunk, now), down.tick(now)].concat(),
            };
            if !reply.is_empty() {
                let _ = stdin.write_all(&reply);
            }
            if download.as_ref().is_some_and(|d| d.outcome().is_some()) {
                break;
            }
        }
        drop(stdin);
        sz.wait().unwrap();
        assert_eq!(download.expect("sz sent no ZRQINIT").outcome(), Some(&Outcome::Finished));
        assert_eq!(std::fs::read(dst.path().join("a.bin")).unwrap(), data);
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
