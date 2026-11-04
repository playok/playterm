//! Minimal SFTP server backed by a local directory, for tests and the `dev_sshd` example.
//! Remote paths are absolute (`/a/b`) and map below `root`; `..` cannot escape it.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};

enum Open {
    File(fs::File),
    /// Directory listing, returned in one batch; `None` once sent.
    Dir(Option<Vec<File>>),
}

pub struct FsHandler {
    root: PathBuf,
    handles: HashMap<String, Open>,
    next: u64,
}

impl FsHandler {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), handles: HashMap::new(), next: 0 }
    }

    /// The local path for an SFTP path, always inside the root: on Windows a part like
    /// `..\..\x` or `C:\x` would otherwise leave it, so `\` and `:` cannot be part of a name.
    fn real(&self, path: &str) -> PathBuf {
        normalize(path)
            .split('/')
            .filter(|p| !p.is_empty())
            .map(|p| p.replace(['\\', ':'], "_"))
            .fold(self.root.clone(), |acc, part| acc.join(part))
    }

    fn insert(&mut self, open: Open) -> String {
        self.next += 1;
        let handle = self.next.to_string();
        self.handles.insert(handle.clone(), open);
        handle
    }
}

/// Resolves `.` and `..` lexically; the result always starts with `/`.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

fn attrs(meta: &fs::Metadata) -> FileAttributes {
    #[cfg(unix)]
    let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777;
    #[cfg(not(unix))]
    let mode = if meta.is_dir() { 0o755 } else { 0o644 };
    let kind = if meta.is_dir() { 0o040000 | mode } else { 0o100000 | mode };
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as u32);
    FileAttributes { size: Some(meta.len()), permissions: Some(kind), mtime, ..FileAttributes::empty() }
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

fn io_code(e: std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

fn stat(path: &Path) -> Result<FileAttributes, StatusCode> {
    fs::metadata(path).map(|m| attrs(&m)).map_err(io_code)
}

impl russh_sftp::server::Handler for FsHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _version: u32, _ext: HashMap<String, String>) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name { id, files: vec![File::dummy(normalize(&path))] })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        Ok(Attrs { id, attrs: stat(&self.real(&path))? })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        Ok(Attrs { id, attrs: stat(&self.real(&path))? })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        match self.handles.get(&handle) {
            Some(Open::File(f)) => Ok(Attrs { id, attrs: f.metadata().map(|m| attrs(&m)).map_err(io_code)? }),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let mut files = Vec::new();
        for entry in fs::read_dir(self.real(&path)).map_err(io_code)? {
            let entry = entry.map_err(io_code)?;
            let meta = entry.metadata().map_err(io_code)?;
            files.push(File::new(entry.file_name().to_string_lossy(), attrs(&meta)));
        }
        Ok(Handle { id, handle: self.insert(Open::Dir(Some(files))) })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir(files)) => match files.take() {
                Some(files) => Ok(Name { id, files }),
                None => Err(StatusCode::Eof),
            },
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(&mut self, id: u32, filename: String, flags: OpenFlags, _a: FileAttributes) -> Result<Handle, Self::Error> {
        let file = fs::OpenOptions::new()
            .read(flags.contains(OpenFlags::READ))
            .write(flags.contains(OpenFlags::WRITE))
            .append(flags.contains(OpenFlags::APPEND))
            .create(flags.contains(OpenFlags::CREATE))
            .truncate(flags.contains(OpenFlags::TRUNCATE))
            .open(self.real(&filename))
            .map_err(io_code)?;
        Ok(Handle { id, handle: self.insert(Open::File(file)) })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(SeekFrom::Start(offset)).map_err(io_code)?;
        let mut data = vec![0u8; len as usize];
        let n = f.read(&mut data).map_err(io_code)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(n);
        Ok(Data { id, data })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(SeekFrom::Start(offset)).map_err(io_code)?;
        f.write_all(&data).map_err(io_code)?;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _a: FileAttributes) -> Result<Status, Self::Error> {
        fs::create_dir(self.real(&path)).map_err(io_code)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        fs::remove_dir(self.real(&path)).map_err(io_code)?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        fs::remove_file(self.real(&filename)).map_err(io_code)?;
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, Self::Error> {
        // Like OpenSSH's sftp-server (SFTP v3): an existing target is not replaced.
        if fs::symlink_metadata(self.real(&to)).is_ok() {
            return Err(StatusCode::Failure);
        }
        fs::rename(self.real(&from), self.real(&to)).map_err(io_code)?;
        Ok(ok(id))
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, Self::Error> {
        #[cfg(unix)]
        if let Some(mode) = attrs.permissions {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(self.real(&path), fs::Permissions::from_mode(mode & 0o7777)).map_err(io_code)?;
        }
        #[cfg(not(unix))]
        let _ = (&path, attrs);
        Ok(ok(id))
    }
}
