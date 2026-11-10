//! Local command prompt shown in tabs that are not connected: a small line editor and the
//! commands it understands (`ssh`, `telnet`, `serial`, `open`, `ls`, ...).

use playterm_config::SessionProfile;
use playterm_i18n::{t, tr};
use playterm_net::ping::{self, Family, PingOptions};
use playterm_net::{ForwardKind, ForwardRule};
use playterm_term::Charset;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::local_fs::ListOptions;
use crate::profile_form::parse_url;

/// In the terminal's own foreground color (bold), which every color scheme keeps readable; a
/// fixed ANSI color is faint on some (cyan on light schemes, blue on dark ones: #163).
pub const PROMPT: &str = "\x1b[1mplayterm\x1b[0m> ";

/// The `help` text in the current language.
pub fn help() -> &'static str {
    t!("console.help")
}

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
    /// `ping`: the host and the options.
    Ping(String, PingOptions),
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
    s.parse().ok().filter(|&p: &u16| p > 0).ok_or_else(|| tr!("console.invalid_port", s = s))
}

/// Local user name, the default SSH user (like OpenSSH): `USERNAME` on Windows, `USER` elsewhere.
fn local_user() -> String {
    let var = if cfg!(windows) { "USERNAME" } else { "USER" };
    std::env::var(var).unwrap_or_default()
}

fn parse_ssh(args: &[String]) -> Result<SessionProfile, String> {
    let mut port = None;
    let mut user = None;
    let mut identity = String::new();
    let mut jump = String::new();
    let mut x11 = false;
    let mut x11_trusted = false;
    let mut agent_forwarding = false;
    let mut forwards = Vec::new();
    let mut charset = Charset::Auto;
    let mut target = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |what: &str| it.next().cloned().ok_or_else(|| tr!("console.missing_after", arg = arg, what = what));
        match arg.as_str() {
            "-p" => port = Some(parse_port(&value(t!("common.port"))?)?),
            "-l" => user = Some(value(t!("console.user_name"))?),
            "-i" => identity = value(t!("console.key_file"))?,
            "-J" => jump = value(t!("console.jump_host"))?,
            "-X" => x11 = true,
            "-Y" => (x11, x11_trusted) = (true, true),
            "-A" => agent_forwarding = true,
            "-a" => agent_forwarding = false,
            "-L" | "-R" | "-D" => {
                let kind = match arg.as_str() {
                    "-L" => ForwardKind::Local,
                    "-R" => ForwardKind::Remote,
                    _ => ForwardKind::Dynamic,
                };
                forwards.push(ForwardRule::parse(kind, &value(t!("console.forwarding_spec"))?)?);
            }
            "--cp949" | "--euc-kr" => charset = Charset::Cp949,
            a if a.starts_with('-') => return Err(tr!("console.unknown_option", a = a)),
            a if target.is_none() => target = Some(a.to_owned()),
            a => return Err(tr!("console.too_many_arguments", a = a)),
        }
    }
    let target = target.ok_or(t!("console.usage_ssh_options_user_host_port"))?;
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
    profile.jump_host = jump;
    profile.x11_forwarding = x11;
    profile.x11_trusted = x11_trusted;
    profile.agent_forwarding = agent_forwarding;
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
            a if a.starts_with('-') => return Err(tr!("console.unknown_option", a = a)),
            a => rest.push(a),
        }
    }
    let (host, port) = match rest.as_slice() {
        [host] => (*host, None),
        [host, port] => (*host, Some(parse_port(port)?)),
        _ => return Err(t!("console.usage_telnet_host_port").into()),
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
        _ => Err(t!("console.usage_serial_com3_speed").into()),
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
            _ => Err(t!("console.usage_rdp_user_host_port").into()),
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
        "ping" => parse_ping(rest),
        "exit" | "quit" | "close" => Parsed::Request(Request::Exit),
        other => Parsed::Error(tr!("console.unknown_command_type_help_for_a_list", other = other)),
    }
}

/// `ping [-t] [-n count] [-w ms] [-l size] [-4|-6] host`, the options of Windows' `ping`
/// (`/t` works too), and `-c count` as on Linux.
fn parse_ping(args: &[String]) -> Parsed {
    let usage = || Parsed::Error(t!("console.usage_ping_t_n_count_w_ms_l").into());
    let mut options = PingOptions::default();
    let mut host = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = arg.to_ascii_lowercase().replacen('/', "-", 1);
        let mut number = |max: u32, what: &str| {
            it.next()
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|n| (1..=max).contains(n))
                .ok_or_else(|| Parsed::Error(tr!("console.needs_from_1_to", flag = flag, max = max, what = what)))
        };
        match flag.as_str() {
            "-t" => options.count = None,
            "-n" | "-c" => match number(u32::MAX, t!("console.a_count")) {
                Ok(n) => options.count = Some(n),
                Err(e) => return e,
            },
            "-w" => match number(ping::MAX_TIMEOUT_MS, t!("console.milliseconds")) {
                Ok(ms) => options.timeout_ms = ms,
                Err(e) => return e,
            },
            "-l" => match number(ping::MAX_SIZE.into(), t!("console.a_size_in_bytes")) {
                Ok(size) => options.size = size as u16,
                Err(e) => return e,
            },
            "-4" => options.family = Some(Family::V4),
            "-6" => options.family = Some(Family::V6),
            _ if flag.starts_with('-') => {
                return Parsed::Error(tr!("console.ping_unknown_option", arg = arg));
            }
            _ if host.is_none() => host = Some(arg.clone()),
            _ => return usage(),
        }
    }
    match host {
        Some(host) => Parsed::Request(Request::Ping(host, options)),
        None => usage(),
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
            Some(_) if dash.is_some() => return Parsed::Error(tr!("console.unknown_option", a = a)),
            _ if opts.path.is_none() => opts.path = Some(a.clone()),
            _ => return Parsed::Error(t!("console.only_one_path_is_allowed").into()),
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

        let p = connect("ssh -J ops@bastion:2222 db01");
        assert_eq!((p.host.as_str(), p.jump_host.as_str()), ("db01", "ops@bastion:2222"));
        assert!(!p.x11_forwarding);
        let p = connect("ssh -X web");
        assert!(p.x11_forwarding && !p.x11_trusted, "-X is untrusted");
        let p = connect("ssh -Y -p 2222 web");
        assert!(p.x11_forwarding && p.x11_trusted);
        assert!(!connect("ssh web").agent_forwarding);
        assert!(connect("ssh -A web").agent_forwarding);
        assert!(!connect("ssh -A -a web").agent_forwarding);
        assert!(connect("ssh -a -A web").agent_forwarding);
        assert!(matches!(parse("ssh -J"), Parsed::Error(_)), "-J needs a host");

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
    fn ping_commands() {
        let ping = |line: &str| match parse(line) {
            Parsed::Request(Request::Ping(host, options)) => (host, options),
            other => panic!("{line}: {other:?}"),
        };
        assert_eq!(ping("ping router"), ("router".into(), PingOptions::default()));
        let (host, options) = ping("ping -n 2 -w 500 -l 1400 -4 10.0.0.1");
        assert_eq!(host, "10.0.0.1");
        assert_eq!(options, PingOptions { count: Some(2), timeout_ms: 500, size: 1400, family: Some(Family::V4) });
        assert_eq!(ping("PING /t -6 ::1").1.count, None);
        assert_eq!(ping("ping -c 3 host").1.count, Some(3));
        for bad in ["ping", "ping -n 0 host", "ping -n x host", "ping -l 70000 host", "ping -x host", "ping a b"] {
            assert!(matches!(parse(bad), Parsed::Error(_)), "{bad}");
        }
    }

    #[test]
    fn other_commands() {
        assert_eq!(parse("  "), Parsed::Empty);
        assert_eq!(parse("HELP"), Parsed::Help);
        assert_eq!(request("open"), Request::SiteManager);
        assert_eq!(request("open 웹 서버"), Request::OpenSite("웹 서버".into()));
        assert_eq!(request("sites 운영"), Request::Sites("운영".into()));
        assert_eq!(parse("ls"), Parsed::Ls(ListOptions::default()));
        assert_eq!(parse(r"ls -la C:\Windows"), Parsed::Ls(ListOptions { long: true, all: true, path: Some(r"C:\Windows".into()) }));
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
