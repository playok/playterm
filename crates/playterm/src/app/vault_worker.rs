//! At most one blocking vault computation per app, including cancelled jobs still deriving.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};

use eframe::egui;
use playterm_config::{Vault, VaultError};
use playterm_i18n::t;
use zeroize::Zeroizing;

use crate::dialogs::VaultMode;

const WORKING: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTING: u8 = 2;

#[derive(Default)]
pub(super) struct Worker {
    busy: Arc<AtomicBool>,
}

pub(super) struct Task {
    state: Arc<AtomicU8>,
    result: mpsc::Receiver<Result<Vault, VaultError>>,
}

struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Worker {
    pub fn start(&self, path: PathBuf, mode: VaultMode, master: Zeroizing<String>, ctx: egui::Context) -> io::Result<Task> {
        self.spawn(
            move |state| {
                let mut vault = Vault::new(path);
                let commit = || state.compare_exchange(WORKING, COMMITTING, Ordering::AcqRel, Ordering::Acquire).is_ok();
                let result = match mode {
                    VaultMode::Create => vault.create_cancellable(&master, commit),
                    VaultMode::Unlock => vault.unlock_cancellable(&master, commit),
                };
                drop(master);
                result.map(|()| vault)
            },
            move || ctx.request_repaint(),
        )
    }

    fn spawn(
        &self,
        compute: impl FnOnce(&AtomicU8) -> Result<Vault, VaultError> + Send + 'static,
        wake: impl FnOnce() + Send + 'static,
    ) -> io::Result<Task> {
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

impl Task {
    pub fn can_cancel(&self) -> bool {
        self.state.load(Ordering::Acquire) == WORKING
    }

    pub fn cancel(&self) -> bool {
        self.state.compare_exchange(WORKING, CANCELLED, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    pub fn poll(&self) -> Option<Result<Vault, VaultError>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(VaultError::Io(io::Error::other(t!("app.vault_worker.the_vault_worker_stopped_unexpectedly")))))
            }
        }
    }
}

impl Drop for Task {
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
                    Err(VaultError::Cancelled)
                },
                || {},
            )
            .unwrap();
        started.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(task.poll().is_none());
        assert!(task.cancel());
        assert!(worker.spawn(|_| Err(VaultError::Cancelled), || {}).is_err());
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
                    Err(VaultError::Missing)
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
