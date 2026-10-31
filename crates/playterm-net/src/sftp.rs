//! SFTP over an SSH channel of an open session.
//!
//! Operations run on the network runtime; results come back as [`SftpEvent`]s that the UI
//! polls with [`SftpConnection::try_recv`], like session events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use playterm_i18n::{t, tr};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::session::Waker;

const CHUNK: usize = 256 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub mtime: Option<u32>,
    pub permissions: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    Running,
    Finished,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct TransferUpdate {
    pub id: u64,
    pub done: u64,
    pub total: u64,
    pub state: TransferState,
}

#[derive(Debug)]
pub enum SftpEvent {
    /// Directory listing; `path` is the canonical absolute path.
    Listing { path: String, result: Result<Vec<RemoteEntry>, String> },
    /// A mkdir/rename/remove finished.
    OpDone { what: String, result: Result<(), String> },
    Transfer(TransferUpdate),
}

/// Joins a remote directory and a name with `/`.
pub fn remote_join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

/// Parent of a remote path (`/` stays `/`).
pub fn remote_parent(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".to_owned(),
        Some((parent, _)) => parent.to_owned(),
    }
}

struct Inner {
    sftp: SftpSession,
    rt: tokio::runtime::Handle,
    events: std_mpsc::Sender<SftpEvent>,
    waker: Waker,
    next_id: AtomicU64,
    cancel: Mutex<HashMap<u64, Arc<AtomicBool>>>,
}

impl Inner {
    fn emit(&self, event: SftpEvent) {
        let _ = self.events.send(event);
        (self.waker)();
    }
}

/// UI handle to an SFTP session. Dropping it closes the SFTP channel.
pub struct SftpConnection {
    inner: Arc<Inner>,
    events: std_mpsc::Receiver<SftpEvent>,
}

impl std::fmt::Debug for SftpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpConnection").finish_non_exhaustive()
    }
}

impl SftpConnection {
    pub(crate) fn new(sftp: SftpSession, waker: Waker) -> Self {
        let (tx, rx) = std_mpsc::channel();
        let inner = Inner {
            sftp,
            rt: tokio::runtime::Handle::current(),
            events: tx,
            waker,
            next_id: AtomicU64::new(1),
            cancel: Mutex::new(HashMap::new()),
        };
        Self { inner: Arc::new(inner), events: rx }
    }

    pub fn try_recv(&self) -> Option<SftpEvent> {
        self.events.try_recv().ok()
    }

    fn spawn<F>(&self, f: impl FnOnce(Arc<Inner>) -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.inner.rt.spawn(f(self.inner.clone()));
    }

    /// Lists a directory; `"."` lists the login directory.
    pub fn list(&self, path: &str) {
        let path = path.to_owned();
        self.spawn(|inner| async move {
            let result = list(&inner.sftp, &path).await;
            let (path, result) = match result {
                Ok((canonical, entries)) => (canonical, Ok(entries)),
                Err(e) => (path, Err(format!("{e:#}"))),
            };
            inner.emit(SftpEvent::Listing { path, result });
        });
    }

    pub fn mkdir(&self, path: &str) {
        let path = path.to_owned();
        self.op(tr!("폴더 만들기 {path}", "Create folder {path}"), move |sftp| async move { Ok(sftp.create_dir(path).await?) });
    }

    pub fn rename(&self, from: &str, to: &str) {
        let (from, to) = (from.to_owned(), to.to_owned());
        self.op(tr!("이름 바꾸기 {from} → {to}", "Rename {from} → {to}"), move |sftp| async move {
            Ok(sftp.rename(from, to).await?)
        });
    }

    /// Removes a file, or a directory with everything in it.
    pub fn remove(&self, path: &str, is_dir: bool) {
        let path = path.to_owned();
        self.op(tr!("삭제 {path}", "Delete {path}"), move |sftp| async move {
            if is_dir { remove_tree(&sftp, &path).await } else { Ok(sftp.remove_file(path).await?) }
        });
    }

    fn op<F>(&self, what: String, f: impl FnOnce(SftpRef) -> F + Send + 'static)
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.spawn(|inner| async move {
            let result = f(SftpRef(inner.clone())).await.map_err(|e| format!("{e:#}"));
            inner.emit(SftpEvent::OpDone { what, result });
        });
    }

    /// Downloads a remote file or directory tree to `local` (the destination path itself).
    pub fn download(&self, remote: &str, local: &Path, is_dir: bool) -> u64 {
        let (remote, local) = (remote.to_owned(), local.to_owned());
        self.transfer(move |sftp, progress| async move {
            if is_dir {
                download_tree(&sftp, &remote, &local, progress).await
            } else {
                let total = sftp.metadata(remote.clone()).await?.len();
                progress.add_total(total);
                download_file(&sftp, &remote, &local, progress).await
            }
        })
    }

    /// Uploads a local file or directory tree to `remote` (the destination path itself).
    pub fn upload(&self, local: &Path, remote: &str) -> u64 {
        let (local, remote) = (local.to_owned(), remote.to_owned());
        self.transfer(move |sftp, progress| async move {
            if local.is_dir() {
                upload_tree(&sftp, &local, &remote, progress).await
            } else {
                progress.add_total(tokio::fs::metadata(&local).await?.len());
                upload_file(&sftp, &local, &remote, progress).await
            }
        })
    }

    pub fn cancel(&self, id: u64) {
        if let Some(flag) = self.inner.cancel.lock().expect("cancel map").get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    fn transfer<F>(&self, f: impl FnOnce(SftpRef, Progress) -> F + Send + 'static) -> u64
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.inner.cancel.lock().expect("cancel map").insert(id, cancelled.clone());
        self.spawn(move |inner| async move {
            let progress = Progress::new(id, inner.clone(), cancelled.clone());
            let result = f(SftpRef(inner.clone()), progress.clone()).await;
            let state = match result {
                Ok(()) => TransferState::Finished,
                Err(_) if cancelled.load(Ordering::Relaxed) => TransferState::Cancelled,
                Err(e) => TransferState::Failed(format!("{e:#}")),
            };
            progress.finish(state);
            inner.cancel.lock().expect("cancel map").remove(&id);
        });
        id
    }
}

/// Cheap clonable access to the session inside spawned tasks.
#[derive(Clone)]
struct SftpRef(Arc<Inner>);

impl std::ops::Deref for SftpRef {
    type Target = SftpSession;
    fn deref(&self) -> &SftpSession {
        &self.0.sftp
    }
}

#[derive(Clone)]
struct Progress {
    id: u64,
    inner: Arc<Inner>,
    cancelled: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    last_emit: Arc<Mutex<Instant>>,
}

impl Progress {
    fn new(id: u64, inner: Arc<Inner>, cancelled: Arc<AtomicBool>) -> Self {
        let p = Self {
            id,
            inner,
            cancelled,
            done: Arc::default(),
            total: Arc::default(),
            last_emit: Arc::new(Mutex::new(Instant::now())),
        };
        p.emit(TransferState::Running);
        p
    }

    fn add_total(&self, bytes: u64) {
        self.total.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Records progress; fails once the transfer was cancelled.
    fn advance(&self, bytes: usize) -> anyhow::Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            anyhow::bail!(t("취소됨", "Cancelled"));
        }
        self.done.fetch_add(bytes as u64, Ordering::Relaxed);
        let mut last = self.last_emit.lock().expect("progress clock");
        if last.elapsed() >= PROGRESS_INTERVAL {
            *last = Instant::now();
            drop(last);
            self.emit(TransferState::Running);
        }
        Ok(())
    }

    fn finish(&self, state: TransferState) {
        self.emit(state);
    }

    fn emit(&self, state: TransferState) {
        self.inner.emit(SftpEvent::Transfer(TransferUpdate {
            id: self.id,
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            state,
        }));
    }
}

async fn list(sftp: &SftpSession, path: &str) -> anyhow::Result<(String, Vec<RemoteEntry>)> {
    let canonical = sftp.canonicalize(path).await.with_context(|| tr!("{path} 경로를 찾을 수 없습니다", "Path not found: {path}"))?;
    let mut entries: Vec<RemoteEntry> = sftp
        .read_dir(canonical.clone())
        .await
        .with_context(|| tr!("{canonical} 을(를) 읽을 수 없습니다", "Cannot read {canonical}"))?
        .filter(|e| e.file_name() != "." && e.file_name() != "..")
        .map(|e| {
            let meta = e.metadata();
            RemoteEntry {
                name: e.file_name(),
                is_dir: meta.is_dir(),
                is_symlink: meta.is_symlink(),
                size: meta.size.unwrap_or(0),
                mtime: meta.mtime,
                permissions: meta.permissions,
            }
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok((canonical, entries))
}

async fn download_file(sftp: &SftpSession, remote: &str, local: &Path, progress: Progress) -> anyhow::Result<()> {
    let result = async {
        let mut src = sftp.open(remote).await.with_context(|| tr!("{remote} 을(를) 열 수 없습니다", "Cannot open {remote}"))?;
        let mut dst = tokio::fs::File::create(local)
            .await
            .with_context(|| tr!("{} 을(를) 만들 수 없습니다", "Cannot create {}", local.display()))?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = src.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await?;
            progress.advance(n)?;
        }
        dst.flush().await?;
        anyhow::Ok(())
    }
    .await;
    if result.is_err() {
        // Do not leave a truncated file that looks complete.
        let _ = tokio::fs::remove_file(local).await;
    }
    result
}

async fn upload_file(sftp: &SftpSession, local: &Path, remote: &str, progress: Progress) -> anyhow::Result<()> {
    let result = async {
        let mut src = tokio::fs::File::open(local)
            .await
            .with_context(|| tr!("{} 을(를) 열 수 없습니다", "Cannot open {}", local.display()))?;
        let mut dst = sftp.create(remote).await.with_context(|| tr!("{remote} 을(를) 만들 수 없습니다", "Cannot create {remote}"))?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = src.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await?;
            progress.advance(n)?;
        }
        dst.shutdown().await?;
        anyhow::Ok(())
    }
    .await;
    if result.is_err() {
        let _ = sftp.remove_file(remote).await;
    }
    result
}

/// Remote tree as (relative path, is_dir, size), parents before children.
async fn walk_remote(sftp: &SftpSession, root: &str) -> anyhow::Result<Vec<(String, bool, u64)>> {
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(rel) = stack.pop() {
        let dir = if rel.is_empty() { root.to_owned() } else { remote_join(root, &rel) };
        for e in sftp.read_dir(dir).await? {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let rel_child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let meta = e.metadata();
            if meta.is_dir() {
                stack.push(rel_child.clone());
            }
            out.push((rel_child, meta.is_dir(), meta.size.unwrap_or(0)));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

async fn download_tree(sftp: &SftpSession, remote: &str, local: &Path, progress: Progress) -> anyhow::Result<()> {
    let entries = walk_remote(sftp, remote).await?;
    progress.add_total(entries.iter().filter(|e| !e.1).map(|e| e.2).sum());
    tokio::fs::create_dir_all(local).await?;
    for (rel, is_dir, _) in entries {
        let target: PathBuf = rel.split('/').fold(local.to_owned(), |p, part| p.join(part));
        if is_dir {
            tokio::fs::create_dir_all(&target).await?;
        } else {
            download_file(sftp, &remote_join(remote, &rel), &target, progress.clone()).await?;
        }
    }
    Ok(())
}

async fn upload_tree(sftp: &SftpSession, local: &Path, remote: &str, progress: Progress) -> anyhow::Result<()> {
    // Collect first so the total is known before any byte moves.
    let mut files = Vec::new();
    let mut dirs = vec![String::new()];
    let mut stack = vec![(local.to_owned(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel_child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let meta = entry.metadata()?;
            if meta.is_dir() {
                dirs.push(rel_child.clone());
                stack.push((entry.path(), rel_child));
            } else {
                progress.add_total(meta.len());
                files.push((entry.path(), rel_child));
            }
        }
    }
    dirs.sort();
    for rel in dirs {
        let target = if rel.is_empty() { remote.to_owned() } else { remote_join(remote, &rel) };
        if !sftp.try_exists(target.clone()).await.unwrap_or(false) {
            sftp.create_dir(target.clone()).await.with_context(|| tr!("{target} 폴더를 만들 수 없습니다", "Cannot create folder {target}"))?;
        }
    }
    for (path, rel) in files {
        upload_file(sftp, &path, &remote_join(remote, &rel), progress.clone()).await?;
    }
    Ok(())
}

async fn remove_tree(sftp: &SftpSession, root: &str) -> anyhow::Result<()> {
    let entries = walk_remote(sftp, root).await?;
    // Deepest first so directories are empty when removed.
    for (rel, is_dir, _) in entries.iter().rev() {
        let path = remote_join(root, rel);
        if *is_dir { sftp.remove_dir(path).await? } else { sftp.remove_file(path).await? }
    }
    sftp.remove_dir(root).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths() {
        assert_eq!(remote_join("/home/a", "b"), "/home/a/b");
        assert_eq!(remote_join("/", "b"), "/b");
        assert_eq!(remote_parent("/home/a"), "/home");
        assert_eq!(remote_parent("/home/a/"), "/home");
        assert_eq!(remote_parent("/home"), "/");
        assert_eq!(remote_parent("/"), "/");
    }
}
