use playterm_i18n::{t, tr};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    /// Listen locally, connect from the server (`ssh -L`).
    #[default]
    Local,
    /// Listen on the server, connect from here (`ssh -R`).
    Remote,
    /// Local SOCKS5 proxy, connections made from the server (`ssh -D`).
    Dynamic,
}

impl ForwardKind {
    pub const ALL: [ForwardKind; 3] = [ForwardKind::Local, ForwardKind::Remote, ForwardKind::Dynamic];

    pub fn letter(self) -> char {
        match self {
            ForwardKind::Local => 'L',
            ForwardKind::Remote => 'R',
            ForwardKind::Dynamic => 'D',
        }
    }
}

impl fmt::Display for ForwardKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ForwardKind::Local => t("로컬 (L)", "Local (L)"),
            ForwardKind::Remote => t("원격 (R)", "Remote (R)"),
            ForwardKind::Dynamic => t("동적 SOCKS (D)", "Dynamic SOCKS (D)"),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForwardRule {
    pub kind: ForwardKind,
    /// Listening address; empty = loopback (L/D) or the server's default (R).
    pub bind_host: String,
    pub bind_port: u16,
    /// Target; unused for dynamic forwarding.
    pub dest_host: String,
    pub dest_port: u16,
}

impl ForwardRule {
    /// Parses OpenSSH syntax: `[bind:]port:host:hostport` for L/R, `[bind:]port` for D.
    /// IPv6 addresses go in brackets.
    pub fn parse(kind: ForwardKind, spec: &str) -> Result<Self, String> {
        let parts = split_colons(spec);
        let port = |s: &str| s.parse::<u16>().map_err(|_| tr!("잘못된 포트 '{s}': {spec}", "Invalid port '{s}': {spec}"));
        let (bind, rest): (&str, &[String]) = match (kind, parts.len()) {
            (ForwardKind::Dynamic, 1) | (_, 3) => ("", &parts[..]),
            (ForwardKind::Dynamic, 2) | (_, 4) => (&parts[0], &parts[1..]),
            _ => return Err(tr!("형식이 잘못되었습니다: {spec}", "Invalid format: {spec}")),
        };
        let mut rule = ForwardRule { kind, bind_host: bind.to_owned(), bind_port: port(&rest[0])?, ..Default::default() };
        if kind != ForwardKind::Dynamic {
            rule.dest_host = rest[1].clone();
            rule.dest_port = port(&rest[2])?;
            if rule.dest_host.is_empty() {
                return Err(tr!("대상 호스트가 없습니다: {spec}", "Missing destination host: {spec}"));
            }
        }
        Ok(rule)
    }

    /// Local listening address for L/D rules.
    pub fn local_bind(&self) -> &str {
        if self.bind_host.is_empty() { "127.0.0.1" } else { &self.bind_host }
    }
}

impl fmt::Display for ForwardRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bind = match self.kind {
            ForwardKind::Remote if self.bind_host.is_empty() => tr!("(서버):{}", "(server):{}", self.bind_port),
            ForwardKind::Remote => tr!("(서버){}:{}", "(server){}:{}", self.bind_host, self.bind_port),
            _ => format!("{}:{}", self.local_bind(), self.bind_port),
        };
        match self.kind {
            ForwardKind::Dynamic => write!(f, "D {bind} (SOCKS5)"),
            k => write!(f, "{} {bind} → {}:{}", k.letter(), self.dest_host, self.dest_port),
        }
    }
}

/// Splits on ':' outside `[...]`, removing the brackets.
fn split_colons(spec: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut in_brackets = false;
    for c in spec.chars() {
        match c {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            ':' if !in_brackets => parts.push(String::new()),
            c => parts.last_mut().expect("never empty").push(c),
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_openssh_specs() {
        let r = ForwardRule::parse(ForwardKind::Local, "8080:db:5432").unwrap();
        assert_eq!((r.bind_host.as_str(), r.bind_port, r.dest_host.as_str(), r.dest_port), ("", 8080, "db", 5432));
        assert_eq!(r.to_string(), "L 127.0.0.1:8080 → db:5432");

        let r = ForwardRule::parse(ForwardKind::Remote, "0.0.0.0:9000:localhost:3000").unwrap();
        assert_eq!((r.bind_host.as_str(), r.bind_port), ("0.0.0.0", 9000));

        let r = ForwardRule::parse(ForwardKind::Local, "[::1]:8080:[fe80::1]:22").unwrap();
        assert_eq!((r.bind_host.as_str(), r.dest_host.as_str()), ("::1", "fe80::1"));

        let r = ForwardRule::parse(ForwardKind::Dynamic, "1080").unwrap();
        assert_eq!(r.to_string(), "D 127.0.0.1:1080 (SOCKS5)");
        assert_eq!(ForwardRule::parse(ForwardKind::Dynamic, "0.0.0.0:1080").unwrap().bind_host, "0.0.0.0");
    }

    #[test]
    fn rejects_bad_specs() {
        assert!(ForwardRule::parse(ForwardKind::Local, "8080").is_err());
        assert!(ForwardRule::parse(ForwardKind::Local, "x:db:1").is_err());
        assert!(ForwardRule::parse(ForwardKind::Local, "1::1").is_err());
        assert!(ForwardRule::parse(ForwardKind::Dynamic, "a:b:c").is_err());
    }
}
