use std::io::{self, Write as _};
use std::path::Path;

/// Writes `contents` to `path` via a temporary file and rename, so a crash, power loss or full
/// disk never leaves a half-written config behind. The data is flushed to disk before the
/// rename, and each write has its own temporary file (two windows may save at once).
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", random_id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// 16 random hex characters, used as a stable profile id.
pub fn random_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("OS random number generator unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
