//! Inspection and explicit removal of remembered host identities.
//! Writers lock a stable sibling, so atomic replacement never bypasses the lock.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use russh::keys::{HashAlg, PublicKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustKind {
    Ssh,
    Rdp,
}

#[derive(Clone, Debug)]
pub struct TrustedHost {
    pub kind: TrustKind,
    pub line: usize,
    pub host: String,
    pub algorithm: String,
    pub fingerprint: String,
    raw: String,
}

pub(crate) fn read(path: &Path) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

pub(crate) fn lock(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut sibling = path.as_os_str().to_owned();
    sibling.push(".lock");
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(sibling)?;
    let deadline = Instant::now() + Duration::from_millis(750);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "Host identities are busy in another window; retry"));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

fn path_for(ssh_file: &Path, kind: TrustKind) -> PathBuf {
    match kind {
        TrustKind::Ssh => ssh_file.to_owned(),
        TrustKind::Rdp => ssh_file.with_file_name("known_rdp_hosts"),
    }
}

/// Lists ordinary, hashed and marked SSH entries, plus remembered RDP certificates.
/// Unparseable non-comment lines remain visible and removable as invalid entries.
pub fn trusted_hosts(ssh_file: &Path) -> io::Result<Vec<TrustedHost>> {
    let mut entries = Vec::new();
    for kind in [TrustKind::Ssh, TrustKind::Rdp] {
        for (index, raw) in read(&path_for(ssh_file, kind))?.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<_> = line.split_whitespace().collect();
            let marked = kind == TrustKind::Ssh && fields[0].starts_with('@');
            let start = usize::from(marked);
            let host = fields.get(start).copied().unwrap_or(line).to_owned();
            let (algorithm, fingerprint) = match kind {
                TrustKind::Rdp => ("Certificate SHA-256".into(), fields.get(1).copied().unwrap_or("Invalid entry").into()),
                TrustKind::Ssh => {
                    let key = fields
                        .get(start + 1)
                        .zip(fields.get(start + 2))
                        .and_then(|(algorithm, value)| PublicKey::from_openssh(&format!("{algorithm} {value}")).ok());
                    match key {
                        Some(key) => {
                            let marker = if marked { format!("{} ", fields[0]) } else { String::new() };
                            (format!("{marker}{}", key.algorithm()), key.fingerprint(HashAlg::Sha256).to_string())
                        }
                        None => ("Invalid entry".into(), line.into()),
                    }
                }
            };
            entries.push(TrustedHost { kind, line: index + 1, host, algorithm, fingerprint, raw: raw.into() });
        }
    }
    Ok(entries)
}

/// Removes only the exact line the user reviewed. A concurrent edit requires refreshing.
pub fn remove_trusted_host(ssh_file: &Path, entry: &TrustedHost) -> io::Result<()> {
    let path = path_for(ssh_file, entry.kind);
    let _lock = lock(&path)?;
    let text = read(&path)?;
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let at = entry.line - 1;
    if lines.get(at).map(|s| s.trim_end_matches(['\r', '\n'])) != Some(entry.raw.as_str()) {
        return Err(io::Error::other("The entry changed in another window; refresh before deleting"));
    }
    let remaining: String = lines.into_iter().enumerate().filter_map(|(i, line)| (i != at).then_some(line)).collect();
    write_atomic(&path, remaining.as_bytes())
}

pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{suffix}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_fingerprints_and_only_removes_the_reviewed_line() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join("known_hosts");
        let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
        let line = format!("jump.example {}", key.public_key().to_openssh().unwrap());
        std::fs::write(&ssh, format!("# keep comment\r\n{line}\r\ninvalid\r\n")).unwrap();
        std::fs::write(dir.path().join("known_rdp_hosts"), "desk:3389 SHA256:fake\n").unwrap();
        let entries = trusted_hosts(&ssh).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].fingerprint, key.public_key().fingerprint(HashAlg::Sha256).to_string());
        assert_eq!(entries[2].kind, TrustKind::Rdp);
        remove_trusted_host(&ssh, &entries[0]).unwrap();
        assert_eq!(read(&ssh).unwrap(), "# keep comment\r\ninvalid\r\n");
        assert!(remove_trusted_host(&ssh, &entries[0]).is_err(), "never delete a different line after an edit");
        assert_eq!(read(&ssh).unwrap(), "# keep comment\r\ninvalid\r\n");
        remove_trusted_host(&ssh, &entries[2]).unwrap();
        assert!(read(&dir.path().join("known_rdp_hosts")).unwrap().is_empty());
    }
}
