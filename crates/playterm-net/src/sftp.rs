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
use russh_sftp::protocol::OpenFlags;
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

/// Most entries a folder download lists, and the deepest folder it follows: a server could
/// otherwise list an endless tree.
const MAX_TREE_ENTRIES: usize = 100_000;
const MAX_TREE_DEPTH: usize = 64;

/// Whether `name` (one entry of a server's directory listing) can be used as a single file or
/// folder name on Windows. A server picks these names, so anything that could reach outside
/// the chosen folder is refused: `.`/`..`, separators, drive colons, control characters,
/// characters Windows does not allow, device names (`CON`, `NUL`, `COM1`, ...) and trailing
/// dots or spaces, which Windows drops.
pub fn is_safe_local_name(name: &str) -> bool {
    const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    if name.is_empty() || name == "." || name == ".." || name.ends_with(['.', ' ']) {
        return false;
    }
    if name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')) {
        return false;
    }
    // The device names are reserved with any extension too (`nul.txt`).
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    let numbered = (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit();
    !RESERVED.contains(&stem.as_str()) && !numbered
}

/// The file a download is written to before it replaces `local`: next to it, new, so a failed
/// or cancelled download never truncates or deletes a file that was already there.
fn part_path(local: &Path) -> PathBuf {
    use std::sync::atomic::AtomicUsize;
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let unique = format!("{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
    local.with_file_name(format!(".{name}.{unique}.part"))
}

async fn download_file(sftp: &SftpSession, remote: &str, local: &Path, progress: Progress) -> anyhow::Result<()> {
    let part = part_path(local);
    let result = async {
        let mut src = sftp.open(remote).await.with_context(|| tr!("{remote} 을(를) 열 수 없습니다", "Cannot open {remote}"))?;
        let mut dst = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await
            .with_context(|| tr!("{} 을(를) 만들 수 없습니다", "Cannot create {}", part.display()))?;
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
        drop(dst);
        // Complete: now it replaces the destination (the user chose it, or it is in the folder
        // they chose).
        tokio::fs::rename(&part, local)
            .await
            .with_context(|| tr!("{} 을(를) 만들 수 없습니다", "Cannot create {}", local.display()))?;
        anyhow::Ok(())
    }
    .await;
    if result.is_err() {
        // Only the partial file is ours to remove.
        let _ = tokio::fs::remove_file(&part).await;
    }
    result
}

/// A hidden name next to `remote` (same folder, so renames stay on one filesystem) ending in
/// `.{kind}`, with a random part nobody can prepare (say, as a link to elsewhere) in advance:
/// the file an upload is written to (`part`), or the old file while it is replaced (`old`).
fn remote_temp_path(remote: &str, kind: &str) -> anyhow::Result<String> {
    let (dir, name) = match remote.rsplit_once('/') {
        Some((dir, name)) => (format!("{dir}/"), name),
        None => (String::new(), remote),
    };
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("random: {e}"))?;
    let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{dir}.{name}.{token}.{kind}"))
}

/// Uploads `local` to `remote` through a temporary remote file that replaces it only once
/// complete: a failed or cancelled upload (or a local file that cannot be read) leaves a file
/// already at `remote` as it was, and removes only the temporary one.
async fn upload_file(sftp: &SftpSession, local: &Path, remote: &str, progress: Progress) -> anyhow::Result<()> {
    // Before anything is created on the server.
    let mut src = tokio::fs::File::open(local)
        .await
        .with_context(|| tr!("{} 을(를) 열 수 없습니다", "Cannot open {}", local.display()))?;
    let part = remote_temp_path(remote, "part")?;
    // A file being replaced keeps its permissions (an executable script, a private file): the
    // temporary file is created with them, so its data is never open to more than the old was.
    let permissions = sftp.metadata(remote).await.ok().and_then(|old| old.permissions).map(|mode| mode & 0o7777);
    let attributes = russh_sftp::protocol::FileAttributes { permissions, ..russh_sftp::protocol::FileAttributes::empty() };
    // Exclusive: never an existing file, nor a link to one.
    let flags = OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE;
    let mut dst = sftp
        .open_with_flags_and_attributes(part.as_str(), flags, attributes)
        .await
        .with_context(|| tr!("{remote} 을(를) 만들 수 없습니다", "Cannot create {remote}"))?;
    let result = async {
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
        progress.advance(0)?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => replace_remote(sftp, &part, remote).await,
        Err(e) => {
            // Only the temporary file is ours to remove.
            let _ = sftp.remove_file(part.as_str()).await;
            Err(e)
        }
    }
}

/// Renames the complete upload `part` onto `remote`, never deleting the file already there
/// before the new one has taken its name.
///
/// A plain rename comes first: servers that replace the target with it do so in one step. SFTP
/// v3 servers (OpenSSH's among them) refuse an existing target; the old file then moves aside
/// to a hidden backup name, the upload takes its place, and only then is the backup removed. If
/// the upload cannot take the name, the backup moves back, and the error says what is where.
async fn replace_remote(sftp: &SftpSession, part: &str, remote: &str) -> anyhow::Result<()> {
    let first = match sftp.rename(part, remote).await {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    if !sftp.try_exists(remote).await.unwrap_or(false) {
        // Not an existing target that got in the way: the upload is of no use then.
        let r = sftp.rename(part, remote).await;
        if r.is_err() {
            let _ = sftp.remove_file(part).await;
        }
        return r.with_context(|| tr!("{remote} 을(를) 만들 수 없습니다 ({first})", "Cannot create {remote} ({first})"));
    }
    let backup = remote_temp_path(remote, "old")?;
    if let Err(e) = sftp.rename(remote, backup.as_str()).await {
        // The old file stays where it is; the upload is dropped.
        let _ = sftp.remove_file(part).await;
        return Err(anyhow::anyhow!(tr!(
            "{remote} 을(를) 바꿀 수 없어 원래 파일을 그대로 두었습니다 ({e})",
            "Could not replace {remote}; the file there was left as it was ({e})"
        )));
    }
    if let Err(e) = sftp.rename(part, remote).await {
        // Put the old file back.
        return match sftp.rename(backup.as_str(), remote).await {
            Ok(()) => {
                let _ = sftp.remove_file(part).await;
                Err(anyhow::anyhow!(tr!(
                    "{remote} 을(를) 바꾸지 못해 원래 파일로 되돌렸습니다 ({e})",
                    "Could not replace {remote}; the file there was put back ({e})"
                )))
            }
            Err(e2) => Err(anyhow::anyhow!(tr!(
                "{remote} 을(를) 바꾸지 못했고({e}) 원래 파일도 되돌리지 못했습니다({e2}). 원래 파일은 {backup}, 올린 파일은 {part} 에 있습니다",
                "Could not replace {remote} ({e}) nor put the old file back ({e2}). The old file is at {backup}, the upload at {part}"
            ))),
        };
    }
    if let Err(e) = sftp.remove_file(backup.as_str()).await {
        return Err(anyhow::anyhow!(tr!(
            "{remote} 은(는) 바꿨지만 이전 파일의 백업 {backup} 을(를) 지우지 못했습니다 ({e})",
            "{remote} was replaced, but the backup of the old file, {backup}, could not be removed ({e})"
        )));
    }
    Ok(())
}

/// Remote tree as (relative path, is_dir, size), parents before children. The walk stops when
/// the tree is unreasonably large or deep. For a `download` every name is checked with
/// [`is_safe_local_name`] before anything is written, and cancelling stops the walk.
async fn walk_remote(
    sftp: &SftpSession,
    root: &str,
    download: Option<&Progress>,
) -> anyhow::Result<Vec<(String, bool, u64)>> {
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(rel) = stack.pop() {
        if let Some(progress) = download {
            progress.advance(0)?;
        }
        let depth = if rel.is_empty() { 0 } else { rel.split('/').count() };
        if depth > MAX_TREE_DEPTH {
            anyhow::bail!(tr!("폴더가 너무 깊습니다 ({}단계 넘음): {rel}", "Folders nested too deep (over {}): {rel}", MAX_TREE_DEPTH));
        }
        let dir = if rel.is_empty() { root.to_owned() } else { remote_join(root, &rel) };
        for e in sftp.read_dir(dir).await? {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            if name.contains('/') || (download.is_some() && !is_safe_local_name(&name)) {
                anyhow::bail!(tr!(
                    "이 PC 에 저장할 수 없는 이름이 있어 다운로드하지 않았습니다: {:?}",
                    "Not downloaded: a name cannot be saved on this PC: {:?}",
                    name
                ));
            }
            if out.len() >= MAX_TREE_ENTRIES {
                anyhow::bail!(tr!("항목이 너무 많습니다 ({}개 넘음)", "Too many entries (over {})", MAX_TREE_ENTRIES));
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

/// Whether `meta` is a link: a symbolic link, or on Windows a junction or other reparse point,
/// any of which can lead outside the folder it is in.
fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// Refuses a download target under `base` (the folder the user chose) whose path goes through
/// a link that already exists there: a link to a folder elsewhere would have the download
/// write, or replace files, outside `base`. `base` itself may be a link (the user picked it).
fn ensure_no_links(base: &Path, rel: &str) -> anyhow::Result<()> {
    let mut path = base.to_owned();
    for part in rel.split('/') {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if is_link(&meta) => anyhow::bail!(tr!(
                "{} 은(는) 다른 곳을 가리키는 링크라 그 안에 받지 않았습니다",
                "Not downloaded: {} is a link that leads elsewhere",
                path.display()
            )),
            Ok(_) => {}
            // Not there yet: nothing below it can be a link either.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(anyhow::Error::new(e).context(path.display().to_string())),
        }
    }
    Ok(())
}

async fn download_tree(sftp: &SftpSession, remote: &str, local: &Path, progress: Progress) -> anyhow::Result<()> {
    let entries = walk_remote(sftp, remote, Some(&progress)).await?;
    progress.add_total(entries.iter().filter(|e| !e.1).map(|e| e.2).sum());
    tokio::fs::create_dir_all(local).await?;
    for (rel, is_dir, _) in entries {
        // The parts were checked in `walk_remote`; this stays inside `local` either way.
        let target: PathBuf = rel.split('/').fold(local.to_owned(), |p, part| p.join(part));
        anyhow::ensure!(
            target.starts_with(local) && target.components().count() == local.components().count() + rel.split('/').count(),
            "unsafe path {rel:?}"
        );
        // Checked right before each use, so links made along the way count too.
        ensure_no_links(local, &rel)?;
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
    let entries = walk_remote(sftp, root, None).await?;
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
    fn server_names_that_could_leave_the_folder_are_refused() {
        for ok in ["file.txt", "폴더", ".bashrc", "a b", "console.d", "COM10", "nul-ish", "x.tar.gz"] {
            assert!(is_safe_local_name(ok), "{ok:?} is a plain name");
        }
        for bad in [
            "", ".", "..", r"..\..\Startup\x.bat", "../x", "a/b", r"a\b", r"C:\x", "C:", "x:stream",
            r"\\server\share", "nul", "NUL.txt", "con", "Com1", "lpt9.log", "aux ", "CONOUT$", "name.", "name ",
            "a\nb", "a\u{0}b", "what?", "*", "a<b", "a|b", "\"q\"",
        ] {
            assert!(!is_safe_local_name(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn upload_temporary_names_are_unpredictable() {
        let a = remote_temp_path("/srv/app.sh", "part").unwrap();
        let b = remote_temp_path("/srv/app.sh", "part").unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("/srv/.app.sh.") && a.ends_with(".part"), "{a}");
        assert_eq!(a.len(), "/srv/.app.sh.".len() + 32 + ".part".len(), "a 128-bit token");
        assert!(remote_temp_path("plain", "part").unwrap().starts_with(".plain."));
    }

    /// OpenSSH's own `sftp-server` on this machine, when it has one.
    fn openssh_sftp_server() -> Option<&'static str> {
        ["/usr/libexec/sftp-server", "/usr/lib/openssh/sftp-server", "/usr/lib/ssh/sftp-server", "/usr/libexec/openssh/sftp-server"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
    }

    /// Against OpenSSH's sftp-server (its rename does not replace an existing file): an upload
    /// replaces the file once complete, keeps its permissions, and a failed one leaves it alone.
    #[tokio::test]
    async fn uploads_replace_files_on_openssh() {
        let Some(server) = openssh_sftp_server() else {
            eprintln!("no OpenSSH sftp-server: skipped");
            return;
        };
        let mut child = tokio::process::Command::new(server)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stream = tokio::io::join(child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let sftp = SftpSession::new(stream).await.unwrap();
        let conn = SftpConnection::new(sftp, Arc::new(|| {}));
        let progress = || Progress::new(1, conn.inner.clone(), Arc::new(AtomicBool::new(false)));

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("app.sh");
        std::fs::write(&target, b"old").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o750)).unwrap();
        let remote = target.to_string_lossy().into_owned();
        // Leftover uploads (.part) or backups of replaced files (.old).
        let parts = || {
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter(|e| {
                    let name = e.as_ref().unwrap().file_name().to_string_lossy().into_owned();
                    name.ends_with(".part") || name.ends_with(".old")
                })
                .count()
        };

        assert!(upload_file(&conn.inner.sftp, &dir.path().join("missing"), &remote, progress()).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");

        let cancelled = Progress::new(2, conn.inner.clone(), Arc::new(AtomicBool::new(true)));
        let big = dir.path().join("big.local");
        std::fs::write(&big, vec![1u8; 3 * CHUNK]).unwrap();
        assert!(upload_file(&conn.inner.sftp, &big, &remote, cancelled).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(parts(), 0);

        let new = dir.path().join("new.local");
        std::fs::write(&new, b"new contents").unwrap();
        upload_file(&conn.inner.sftp, &new, &remote, progress()).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new contents");
        assert_eq!(parts(), 0);
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&target).unwrap().permissions()) & 0o777, 0o750);

        // A private file stays private: the temporary file is created with its mode, so the
        // data is never readable by others, not even while uploading.
        #[cfg(unix)]
        {
            let secret = dir.path().join("secret.txt");
            std::fs::write(&secret, b"x").unwrap();
            std::fs::set_permissions(&secret, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
            upload_file(&conn.inner.sftp, &new, &secret.to_string_lossy(), progress()).await.unwrap();
            assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&secret).unwrap().permissions()) & 0o777, 0o600);
        }

        // A new file is simply created.
        let fresh = dir.path().join("fresh.txt");
        upload_file(&conn.inner.sftp, &new, &fresh.to_string_lossy(), progress()).await.unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"new contents");
    }

    #[test]
    fn partial_downloads_go_next_to_the_target() {
        let target = std::env::temp_dir().join("Downloads").join("report.pdf");
        let target = target.as_path();
        let (a, b) = (part_path(target), part_path(target));
        assert_ne!(a, b, "each download has its own partial file");
        assert_eq!(a.parent(), target.parent());
        assert!(a.file_name().unwrap().to_string_lossy().starts_with(".report.pdf."));
    }

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
