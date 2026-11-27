//! At most one blocking vault computation (a key derivation from a master password) per app,
//! including cancelled jobs still deriving: migrating an older vault, exporting and importing
//! site lists.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};

use eframe::egui;
use playterm_config::{Vault, VaultError};
use playterm_i18n::t;
use zeroize::Zeroizing;

const WORKING: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTING: u8 = 2;

#[derive(Default)]
pub(super) struct Worker {
    busy: Arc<AtomicBool>,
}

pub(super) struct Task<T = Vault> {
    state: Arc<AtomicU8>,
    result: mpsc::Receiver<Result<T, VaultError>>,
}

struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Worker {
    /// Opens `vault` (an older one) with its master password and rewrites it with a keychain key.
    pub fn migrate(&self, mut vault: Vault, master: Zeroizing<String>, ctx: egui::Context) -> io::Result<Task> {
        self.spawn(
            move |state| {
                let commit = || state.compare_exchange(WORKING, COMMITTING, Ordering::AcqRel, Ordering::Acquire).is_ok();
                let result = vault.migrate_cancellable(&master, commit);
                drop(master);
                result.map(|()| vault)
            },
            move || ctx.request_repaint(),
        )
    }

    /// Runs `compute` (which derives a key from a password) off the UI thread.
    pub fn run<T: Send + 'static>(
        &self,
        compute: impl FnOnce() -> Result<T, VaultError> + Send + 'static,
        ctx: egui::Context,
    ) -> io::Result<Task<T>> {
        self.spawn(move |_| compute(), move || ctx.request_repaint())
    }

    fn spawn<T: Send + 'static>(
        &self,
        compute: impl FnOnce(&AtomicU8) -> Result<T, VaultError> + Send + 'static,
        wake: impl FnOnce() + Send + 'static,
    ) -> io::Result<Task<T>> {
        if self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                t!("app.vault_worker.the_previous_vault_operation_is_finishing_try_again"),
            ));
        }
        let guard = BusyGuard(self.busy.clone());
        let state = Arc::new(AtomicU8::new(WORKING));
        let worker_state = state.clone();
        let (send, result) = mpsc::channel();
        std::thread::Builder::new().name("playterm-vault".into()).spawn(move || {
            let guard = guard;
            let outcome = compute(&worker_state);
            // Permit retries before delivering the completion to the UI.
            drop(guard);
            let _ = send.send(outcome);
            wake();
        })?;
        Ok(Task { state, result })
    }
}

impl<T> Task<T> {
    pub fn can_cancel(&self) -> bool {
        self.state.load(Ordering::Acquire) == WORKING
    }

    pub fn cancel(&self) -> bool {
        self.state.compare_exchange(WORKING, CANCELLED, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    pub fn poll(&self) -> Option<Result<T, VaultError>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(VaultError::Io(io::Error::other(t!("app.vault_worker.the_vault_worker_stopped_unexpectedly")))))
            }
        }
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_nonblocking_and_does_not_allow_parallel_derivations() {
        let worker = Worker::default();
        let (entered, started) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let task = worker
            .spawn(
                move |state| {
                    entered.send(()).unwrap();
                    held.recv().unwrap();
                    assert_eq!(state.load(Ordering::Acquire), CANCELLED);
                    Err::<(), _>(VaultError::Cancelled)
                },
                || {},
            )
            .unwrap();
        started.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(task.poll().is_none());
        assert!(task.cancel());
        assert!(worker.spawn(|_| Err::<(), _>(VaultError::Cancelled), || {}).is_err());
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = task.poll() {
                assert!(matches!(result, Err(VaultError::Cancelled)));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn commit_cannot_be_reported_as_cancelled() {
        let worker = Worker::default();
        let (entered, started) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let task = worker
            .spawn(
                move |state| {
                    assert!(state.compare_exchange(WORKING, COMMITTING, Ordering::AcqRel, Ordering::Acquire).is_ok());
                    entered.send(()).unwrap();
                    held.recv().unwrap();
                    Err::<(), _>(VaultError::Missing)
                },
                || {},
            )
            .unwrap();
        started.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(!task.can_cancel());
        assert!(!task.cancel());
        release.send(()).unwrap();
    }
}
