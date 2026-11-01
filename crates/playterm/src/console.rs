//! Local command prompt shown in tabs that are not connected: a small line editor and the
//! commands it understands (`ssh`, `telnet`, `serial`, `open`, `ls`, ...).

use playterm_config::SessionProfile;
use playterm_i18n::{t, tr};
use playterm_net::{ForwardKind, ForwardRule};
use playterm_term::Charset;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::local_fs::ListOptions;
use crate::profile_form::parse_url;

pub const PROMPT: &str = "\x1b[1;36mplayterm\x1b[0m> ";

/// The `help` text in the current language.
pub fn help() -> &'static str {
    t(HELP_KO, HELP_EN)
}

const HELP_KO: &str = "\
\x1b[1m사용할 수 있는 명령\x1b[0m
  ssh [-p 포트] [-l 사용자] [-i 키파일] [-L|-R 포트:호스트:포트] [-D 포트] [--cp949] [사용자@]호스트[:포트]
  telnet 호스트 [포트]              Telnet 접속 (--cp949: 한글 구형 장비)
  serial COM3 [속도]                시리얼 콘솔 (기본 9600 8N1)
  rdp [사용자@]호스트[:포트]         원격 데스크톱 (Windows RDP)
  open                              사이트 관리 창 열기
  open 이름|번호                    저장된 사이트에 접속 (번호는 sites 의 번호)
  sites [검색어]                    저장된 사이트 목록
  ls [-l] [-a] [경로]               파일 목록 (-l: 자세히, -a: 숨김 파일 포함, *.log 같은 패턴 가능)
  dir [/a] [경로]                   파일 목록 (자세히)
  cd [경로], pwd                    폴더 이동 (cd 만 쓰면 홈, cd .., cd D:) / 현재 폴더
  new                               새 사이트 만들기
  quick                             빠른 연결 창
  r, reconnect                      마지막 연결 다시 접속
  clear, cls                        화면 지우기
  exit                              탭 닫기
  help, ?                           이 도움말

  ←/→, Home/End: 커서 이동   ↑/↓: 이전 명령   Ctrl+C: 입력 취소   Ctrl+L: 화면 지우기";

const HELP_EN: &str = "\
\x1b[1mAvailable commands\x1b[0m
  ssh [-p port] [-l user] [-i keyfile] [-L|-R port:host:port] [-D port] [--cp949] [user@]host[:port]
  telnet host [port]                Telnet connection (--cp949: older Korean devices)
  serial COM3 [speed]               Serial console (default 9600 8N1)
  rdp [user@]host[:port]            Remote desktop (Windows RDP)
  open                              Open the Site Manager
  open name|number                  Connect to a saved site (number as listed by sites)
  sites [filter]                    List saved sites
  ls [-l] [-a] [path]               List files (-l: details, -a: include hidden files, patterns like *.log)
  dir [/a] [path]                   List files (details)
  cd [path], pwd                    Change folder (cd alone: home, cd .., cd D:) / current folder
  new                               Create a new site
  quick                             Quick Connect
  r, reconnect                      Reconnect to the last connection
  clear, cls                        Clear the screen
  exit                              Close the tab
  help, ?                           This help

  ←/→, Home/End: move cursor   ↑/↓: previous command   Ctrl+C: cancel input   Ctrl+L: clear screen";

/// Something a command asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Connect(Box<SessionProfile>),
    /// Connect a saved site by name or `ls` number.
    OpenSite(String),
    /// List saved sites matching the filter.
    Sites(String),
    SiteManager,
    NewSite,
    QuickConnect,
    Reconnect,
    Exit,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Empty,
    Help,
    Clear,
    Ls(ListOptions),
    Cd(Option<String>),
    Pwd,
    Request(Request),
    Error(String),
}

/// Splits a command line into words; `"..."` and `'...'` keep spaces.
fn split_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    args.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            None => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        args.push(cur);
    }
    args
}

fn parse_port(s: &str) -> Result<u16, String> {
    s.parse().ok().filter(|&p: &u16| p > 0).ok_or_else(|| tr!("잘못된 포트: {s}", "Invalid port: {s}"))
}

/// Local Windows user name, the default SSH user (like OpenSSH).
fn local_user() -> String {
    std::env::var("USERNAME").unwrap_or_default()
}

fn parse_ssh(args: &[String]) -> Result<SessionProfile, String> {
    let mut port = None;
    let mut user = None;
    let mut identity = String::new();
    let mut forwards = Vec::new();
    let mut charset = Charset::Auto;
    let mut target = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |what: &str| {
            it.next().cloned().ok_or_else(|| tr!("{arg} 다음에 {what}이(가) 필요합니다", "Missing {what} after {arg}"))
        };
        match arg.as_str() {
            "-p" => port = Some(parse_port(&value(t("포트", "port"))?)?),
            "-l" => user = Some(value(t("사용자 이름", "user name"))?),
            "-i" => identity = value(t("키 파일", "key file"))?,
            "-L" | "-R" | "-D" => {
                let kind = match arg.as_str() {
                    "-L" => ForwardKind::Local,
                    "-R" => ForwardKind::Remote,
                    _ => ForwardKind::Dynamic,
                };
                forwards.push(ForwardRule::parse(kind, &value(t("포워딩 설정", "forwarding spec"))?)?);
            }
            "--cp949" | "--euc-kr" => charset = Charset::Cp949,
            a if a.starts_with('-') => return Err(tr!("알 수 없는 옵션: {a}", "Unknown option: {a}")),
            a if target.is_none() => target = Some(a.to_owned()),
            a => return Err(tr!("인수가 너무 많습니다: {a}", "Too many arguments: {a}")),
        }
    }
    let target = target.ok_or(t("사용법: ssh [옵션] [사용자@]호스트[:포트]", "Usage: ssh [options] [user@]host[:port]"))?;
    let target = match (target.contains('@'), user) {
        (true, _) => target,
        (false, Some(u)) => format!("{u}@{target}"),
        (false, None) => format!("{}@{target}", local_user()),
    };
    let mut profile = parse_url(&format!("ssh://{target}"))?;
    if let Some(port) = port {
        profile.port = port;
    }
    profile.identity_file = identity;
    profile.forwards = forwards;
    profile.charset = charset;
    Ok(profile)
}

fn parse_telnet(args: &[String]) -> Result<SessionProfile, String> {
    let mut charset = Charset::Auto;
    let mut rest = Vec::new();
    for a in args {
        match a.as_str() {
            "--cp949" | "--euc-kr" => charset = Charset::Cp949,
            a if a.starts_with('-') => return Err(tr!("알 수 없는 옵션: {a}", "Unknown option: {a}")),
            a => rest.push(a),
        }
    }
    let (host, port) = match rest.as_slice() {
        [host] => (*host, None),
        [host, port] => (*host, Some(parse_port(port)?)),
        _ => return Err(t("사용법: telnet 호스트 [포트]", "Usage: telnet host [port]").into()),
    };
    let mut profile = parse_url(&format!("telnet://{host}"))?;
    if let Some(port) = port {
        profile.port = port;
    }
    profile.charset = charset;
    Ok(profile)
}

fn parse_serial(args: &[String]) -> Result<SessionProfile, String> {
    match args {
        [port] => parse_url(&format!("serial://{port}")),
        [port, baud] => parse_url(&format!("serial://{port}:{baud}")),
        _ => Err(t("사용법: serial COM3 [속도]", "Usage: serial COM3 [speed]").into()),
    }
}

pub fn parse(line: &str) -> Parsed {
    let args = split_args(line);
    let Some((cmd, rest)) = args.split_first() else { return Parsed::Empty };
    // Free-text argument (site names may contain spaces).
    let text = rest.join(" ");
    let connect = |r: Result<SessionProfile, String>| match r {
        Ok(p) => Parsed::Request(Request::Connect(Box::new(p))),
        Err(e) => Parsed::Error(e),
    };
    match cmd.to_lowercase().as_str() {
        "help" | "?" | "h" => Parsed::Help,
        "clear" | "cls" => Parsed::Clear,
        "ssh" => connect(parse_ssh(rest)),
        "telnet" => connect(parse_telnet(rest)),
        "serial" | "com" => connect(parse_serial(rest)),
        "rdp" | "mstsc" => connect(match rest {
            [target] => parse_url(&format!("rdp://{target}")),
            _ => Err(t("사용법: rdp [사용자@]호스트[:포트]", "Usage: rdp [user@]host[:port]").into()),
        }),
        "open" | "connect" | "o" if text.is_empty() => Parsed::Request(Request::SiteManager),
        "open" | "connect" | "o" => Parsed::Request(Request::OpenSite(text)),
        "ls" => parse_ls(rest, false),
        "dir" => parse_ls(rest, true),
        "cd" | "chdir" => match rest {
            [] => Parsed::Cd(None),
            _ => Parsed::Cd(Some(text)),
        },
        "pwd" => Parsed::Pwd,
        "sites" | "site" | "list" => Parsed::Request(Request::Sites(text)),
        "new" => Parsed::Request(Request::NewSite),
        "quick" => Parsed::Request(Request::QuickConnect),
        "r" | "reconnect" => Parsed::Request(Request::Reconnect),
        "exit" | "quit" | "close" => Parsed::Request(Request::Exit),
        other => Parsed::Error(tr!(
            "알 수 없는 명령: {other}  (help 로 목록 보기)",
            "Unknown command: {other}  (type help for a list)"
        )),
    }
}

/// `ls [-l] [-a] [경로]`, `dir [/a] [경로]` (dir is always detailed). A `/x` argument of
/// `dir` is an option only when it is a known flag; otherwise it is a path.
fn parse_ls(args: &[String], dir: bool) -> Parsed {
    let mut opts = ListOptions { long: dir, ..Default::default() };
    for a in args {
        let dash = a.strip_prefix('-');
        let slash = if dir { a.strip_prefix('/') } else { None };
        let known = |f: &str| !f.is_empty() && f.chars().all(|c| "laLA".contains(c));
        match dash.or(slash) {
            Some(f) if known(f) => {
                opts.long |= f.contains(['l', 'L']);
                opts.all |= f.contains(['a', 'A']);
            }
            Some(_) if dash.is_some() => return Parsed::Error(tr!("알 수 없는 옵션: {a}", "Unknown option: {a}")),
            _ if opts.path.is_none() => opts.path = Some(a.clone()),
            _ => return Parsed::Error(t("경로는 하나만 쓸 수 있습니다", "Only one path is allowed").into()),
        }
    }
    Parsed::Ls(opts)
}

/// Pads `s` with spaces to `width` terminal columns (Hangul counts as two).
pub fn pad(s: &str, width: usize) -> String {
    let w = s.width();
    if w >= width { s.to_owned() } else { format!("{s}{}", " ".repeat(width - w)) }
}

/// Single-line editor with history. Output (echo, redraws) is appended to `out`.
pub struct LineEditor {
    prompt: String,
    /// Terminal width, for lines that wrap.
    cols: usize,
    /// Row of the terminal cursor, counted from the row where the prompt starts.
    cursor_row: usize,
    line: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Index into `history` while browsing with ↑/↓.
    browsing: Option<usize>,
    /// The unfinished line, restored when browsing past the newest entry.
    draft: Vec<char>,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self {
            prompt: PROMPT.into(),
            cols: 80,
            cursor_row: 0,
            line: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            browsing: None,
            draft: Vec::new(),
        }
    }
}

impl LineEditor {
    /// Sets the prompt text (with SGR colors) used from the next prompt on.
    pub fn set_prompt(&mut self, prompt: String) {
        self.prompt = prompt;
    }

    /// Terminal width; lines longer than this wrap.
    pub fn set_width(&mut self, cols: usize) {
        self.cols = cols.max(1);
    }

    /// Where the terminal puts the next character after the prompt and the first `upto`
    /// characters of the line, as (row from the prompt's first row, column). The column
    /// equals the width when the row is full and the next character wraps.
    fn position(&self, upto: usize) -> (usize, usize) {
        let mut pos = (0, 0);
        for c in visible(&self.prompt).chars().chain(self.line[..upto].iter().copied()) {
            pos = advance(pos, c.width().unwrap_or(0), self.cols);
        }
        pos
    }

    /// Moves the cursor back to the start of the prompt and clears from there down.
    fn rewind(&mut self, out: &mut String) {
        if self.cursor_row > 0 {
            out.push_str(&format!("\x1b[{}A", self.cursor_row));
        }
        out.push_str("\r\x1b[J");
        self.cursor_row = 0;
    }

    /// Redraws the prompt and the line (which may wrap over several rows) and puts the
    /// cursor in place.
    fn redraw(&mut self, out: &mut String) {
        self.rewind(out);
        out.push_str(&self.prompt);
        out.extend(self.line.iter());
        let end = self.position(self.line.len());
        if self.cursor == self.line.len() {
            self.cursor_row = end.0;
            return;
        }
        // The character under the cursor starts the next row if it does not fit.
        let (mut row, mut col) = self.position(self.cursor);
        if col + self.line[self.cursor].width().unwrap_or(0) > self.cols {
            (row, col) = (row + 1, 0);
        }
        if end.0 > row {
            out.push_str(&format!("\x1b[{}A", end.0 - row));
        }
        out.push('\r');
        if col > 0 {
            out.push_str(&format!("\x1b[{col}C"));
        }
        self.cursor_row = row;
    }

    /// Prompt for a fresh line.
    pub fn prompt(&mut self, out: &mut String) {
        self.line.clear();
        self.cursor = 0;
        self.browsing = None;
        out.push_str(&self.prompt);
        self.cursor_row = self.position(0).0;
    }

    /// Removes the prompt and the line from the screen (something else takes over the row).
    pub fn erase(&mut self, out: &mut String) {
        self.rewind(out);
        self.line.clear();
        self.cursor = 0;
        self.browsing = None;
    }

    /// Puts the cursor after the end of the line, so output can follow it.
    fn move_to_end(&mut self, out: &mut String) {
        if self.cursor != self.line.len() {
            self.cursor = self.line.len();
            self.redraw(out);
        }
    }

    /// Typed or pasted text. Returns completed lines (a paste may hold several).
    pub fn text(&mut self, text: &str, out: &mut String) -> Vec<String> {
        let mut done = Vec::new();
        let mut last_cr = false;
        let mut changed = false;
        for c in text.chars() {
            match c {
                '\r' | '\n' => {
                    // "\r\n" is one Enter.
                    if !(c == '\n' && last_cr) {
                        if std::mem::take(&mut changed) {
                            self.redraw(out);
                        }
                        done.push(self.submit(out));
                    }
                }
                c if c.is_control() => {}
                c => {
                    self.line.insert(self.cursor, c);
                    self.cursor += 1;
                    changed = true;
                }
            }
            last_cr = c == '\r';
        }
        if changed {
            self.redraw(out);
        }
        done
    }

    fn submit(&mut self, out: &mut String) -> String {
        self.move_to_end(out);
        let line: String = self.line.drain(..).collect();
        self.cursor = 0;
        self.browsing = None;
        out.push_str("\r\n");
        self.cursor_row = 0;
        let trimmed = line.trim();
        if !trimmed.is_empty() && self.history.last().map(String::as_str) != Some(trimmed) {
            self.history.push(trimmed.to_owned());
        }
        line
    }

    fn set_line(&mut self, line: Vec<char>, out: &mut String) {
        self.line = line;
        self.cursor = self.line.len();
        self.redraw(out);
    }

    /// A key that produced control bytes (Enter, Backspace, arrows, Ctrl+...).
    /// Returns the submitted line on Enter. `Some("\x0c")`-style clears are reported as `clear`.
    pub fn key(&mut self, bytes: &[u8], out: &mut String) -> KeyResult {
        match bytes {
            b"\r" | b"\n" => return KeyResult::Line(self.submit(out)),
            [0x7f] | [0x08] => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.line.remove(self.cursor);
                    self.redraw(out);
                }
            }
            b"\x1b[3~" => {
                if self.cursor < self.line.len() {
                    self.line.remove(self.cursor);
                    self.redraw(out);
                }
            }
            [0x03] => {
                // Ctrl+C: drop the line, like a shell.
                self.move_to_end(out);
                out.push_str("^C\r\n");
                self.prompt(out);
            }
            [0x15] => self.set_line(Vec::new(), out),
            [0x0c] => return KeyResult::Clear,
            b"\x1b[D" | b"\x1bOD" if self.cursor > 0 => {
                self.cursor -= 1;
                self.redraw(out);
            }
            b"\x1b[C" | b"\x1bOC" if self.cursor < self.line.len() => {
                self.cursor += 1;
                self.redraw(out);
            }
            b"\x1b[H" | b"\x1bOH" | [0x01] => {
                self.cursor = 0;
                self.redraw(out);
            }
            b"\x1b[F" | b"\x1bOF" | [0x05] => {
                self.cursor = self.line.len();
                self.redraw(out);
            }
            b"\x1b[A" | b"\x1bOA" if !self.history.is_empty() => {
                let i = match self.browsing {
                    None => {
                        self.draft = self.line.clone();
                        self.history.len() - 1
                    }
                    Some(i) => i.saturating_sub(1),
                };
                self.browsing = Some(i);
                self.set_line(self.history[i].chars().collect(), out);
            }
            b"\x1b[B" | b"\x1bOB" => match self.browsing {
                Some(i) if i + 1 < self.history.len() => {
                    self.browsing = Some(i + 1);
                    self.set_line(self.history[i + 1].chars().collect(), out);
                }
                Some(_) => {
                    self.browsing = None;
                    let draft = std::mem::take(&mut self.draft);
                    self.set_line(draft, out);
                }
                None => {}
            },
            _ => {}
        }
        KeyResult::None
    }

}

/// `s` without its escape sequences (the part that takes up cells).
fn visible(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ parameters final-byte; other escapes are two characters.
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Cursor position after drawing a character `width` cells wide at `pos` on a `cols`-wide
/// screen with auto-wrap: a character that does not fit goes to the start of the next row.
fn advance(pos: (usize, usize), width: usize, cols: usize) -> (usize, usize) {
    let (row, col) = if pos.1 + width > cols { (pos.0 + 1, 0) } else { pos };
    (row, col + width)
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyResult {
    None,
    Line(String),
    Clear,
}

#[cfg(test)]
mod tests {
    use super::*;
    use playterm_net::Protocol;

    fn request(line: &str) -> Request {
        match parse(line) {
            Parsed::Request(r) => r,
            other => panic!("{line}: {other:?}"),
        }
    }

    fn connect(line: &str) -> SessionProfile {
        match request(line) {
            Request::Connect(p) => *p,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn splits_quoted_words() {
        assert_eq!(split_args(r#"open "웹 서버"  x"#), ["open", "웹 서버", "x"]);
        assert_eq!(split_args("  "), Vec::<String>::new());
        assert_eq!(split_args("a ''"), ["a", ""]);
    }

    #[test]
    fn ssh_commands() {
        let p = connect("ssh root@10.0.0.1");
        assert_eq!((p.protocol, p.host.as_str(), p.port, p.username.as_str()), (Protocol::Ssh, "10.0.0.1", 22, "root"));

        let p = connect("ssh -p 2222 -l admin -i ~/.ssh/k --cp949 host");
        assert_eq!((p.port, p.username.as_str(), p.identity_file.as_str()), (2222, "admin", "~/.ssh/k"));
        assert_eq!(p.charset, Charset::Cp949);

        let p = connect("ssh me@host:2200 -L 8080:db:5432 -D 1080");
        assert_eq!((p.port, p.forwards.len()), (2200, 2));

        assert_eq!(connect("ssh host").username, local_user());
        assert!(matches!(parse("ssh"), Parsed::Error(_)));
        assert!(matches!(parse("ssh -x host"), Parsed::Error(_)));
        assert!(matches!(parse("ssh -p"), Parsed::Error(_)));
    }

    #[test]
    fn telnet_and_serial_commands() {
        let p = connect("telnet router");
        assert_eq!((p.protocol, p.host.as_str(), p.port), (Protocol::Telnet, "router", 23));
        assert_eq!(connect("telnet router 2323").port, 2323);
        assert_eq!(connect("telnet router:2323").port, 2323);
        assert!(matches!(parse("telnet"), Parsed::Error(_)));

        let p = connect("serial COM3 115200");
        assert_eq!((p.serial.port.as_str(), p.serial.baud), ("COM3", 115200));
    }

    #[test]
    fn other_commands() {
        assert_eq!(parse("  "), Parsed::Empty);
        assert_eq!(parse("HELP"), Parsed::Help);
        assert_eq!(request("open"), Request::SiteManager);
        assert_eq!(request("open 웹 서버"), Request::OpenSite("웹 서버".into()));
        assert_eq!(request("sites 운영"), Request::Sites("운영".into()));
        assert_eq!(parse("ls"), Parsed::Ls(ListOptions::default()));
        assert_eq!(
            parse(r"ls -la C:\Windows"),
            Parsed::Ls(ListOptions { long: true, all: true, path: Some(r"C:\Windows".into()) })
        );
        assert_eq!(parse("dir /a *.log"), Parsed::Ls(ListOptions { long: true, all: true, path: Some("*.log".into()) }));
        assert_eq!(parse("dir /Windows"), Parsed::Ls(ListOptions { long: true, all: false, path: Some("/Windows".into()) }));
        assert!(matches!(parse("ls -z"), Parsed::Error(_)));
        assert!(matches!(parse("ls a b"), Parsed::Error(_)));
        assert_eq!(parse("cd"), Parsed::Cd(None));
        assert_eq!(parse(r#"cd "Program Files""#), Parsed::Cd(Some("Program Files".into())));
        assert_eq!(parse("pwd"), Parsed::Pwd);
        assert_eq!(request("r"), Request::Reconnect);
        assert!(matches!(parse("rm -rf /"), Parsed::Error(_)));
    }

    #[test]
    fn line_editing() {
        let mut ed = LineEditor::default();
        let mut out = String::new();
        assert!(ed.text("lx", &mut out).is_empty());
        ed.key(&[0x7f], &mut out);
        ed.text("s", &mut out);
        ed.key(b"\x1b[D", &mut out);
        ed.key(b"\x1b[D", &mut out);
        ed.text("한", &mut out);
        assert_eq!(ed.position(ed.line.len()), (0, "playterm> ".len() + 4));
        ed.key(b"\x1b[3~", &mut out);
        assert_eq!(ed.key(b"\r", &mut out), KeyResult::Line("한s".into()));

        // History.
        ed.text("ls", &mut out);
        ed.key(b"\r", &mut out);
        ed.text("dra", &mut out);
        ed.key(b"\x1b[A", &mut out);
        ed.key(b"\x1b[A", &mut out);
        assert_eq!(ed.line.iter().collect::<String>(), "한s");
        ed.key(b"\x1b[B", &mut out);
        ed.key(b"\x1b[B", &mut out);
        assert_eq!(ed.line.iter().collect::<String>(), "dra");

        // A paste with several lines.
        ed.key(&[0x15], &mut out);
        assert_eq!(ed.text("ls\r\nhelp\n", &mut out), ["ls", "help"]);
    }

    #[test]
    fn wrapped_lines() {
        let mut ed = LineEditor::default();
        ed.set_width(20);
        let mut out = String::new();
        ed.prompt(&mut out);
        // 10 prompt cells + 15 characters: the line wraps onto a second row.
        ed.text("abcdefghijklmno", &mut out);
        assert_eq!(ed.cursor_row, 1);

        // Left: redraw from the prompt's first row, cursor before "o" (row 1, column 4).
        out.clear();
        ed.key(b"\x1b[D", &mut out);
        assert!(out.starts_with("\x1b[1A\r\x1b[J"), "{out:?}");
        assert!(out.ends_with("\r\x1b[4C"), "{out:?}");
        assert_eq!(ed.cursor_row, 1);

        // Home: up one row to just after the prompt.
        out.clear();
        ed.key(b"\x1b[H", &mut out);
        assert!(out.ends_with("\x1b[1A\r\x1b[10C"), "{out:?}");
        assert_eq!(ed.cursor_row, 0);

        // Enter from the first row: move past the wrapped end before the new line.
        out.clear();
        assert_eq!(ed.key(b"\r", &mut out), KeyResult::Line("abcdefghijklmno".into()));
        assert!(out.ends_with("abcdefghijklmno\r\n"), "{out:?}");
        assert_eq!(ed.cursor_row, 0);
    }

    #[test]
    fn wide_characters_wrap_early() {
        let mut ed = LineEditor::default();
        ed.set_width(13);
        let mut out = String::new();
        ed.prompt(&mut out);
        // 10 + 2 = 12 cells; the next wide character does not fit in the 13th cell.
        ed.text("한", &mut out);
        assert_eq!(ed.position(1), (0, 12));
        ed.text("글", &mut out);
        assert_eq!((ed.position(2), ed.cursor_row), ((1, 2), 1));
        // A full row leaves the cursor on it until the next character arrives.
        let mut ed = LineEditor::default();
        ed.set_width(12);
        ed.prompt(&mut out);
        ed.text("ab", &mut out);
        assert_eq!((ed.position(2), ed.cursor_row), ((0, 12), 0));
    }

    #[test]
    fn prompt_width_ignores_colors() {
        assert_eq!(visible(PROMPT), "playterm> ");
        assert_eq!(visible("\x1b[1;36ma\x1b[0m \x1b[34m~\\x\x1b[0m> "), "a ~\\x> ");
    }
}
