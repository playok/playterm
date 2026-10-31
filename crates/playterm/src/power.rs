//! Keeps Windows from going to sleep (and optionally turning the screen off) while sessions are
//! connected, so idle connections are not dropped. Closing the lid or choosing Sleep still works:
//! this only stops the idle timers.

use playterm_config::PreventSleep;

/// The execution state this (UI) thread has asked Windows for.
#[derive(Default)]
pub struct SleepGuard {
    current: Option<PreventSleep>,
}

impl SleepGuard {
    /// Asks for `want` (`Off` releases the request). Only calls Windows when it changes.
    pub fn set(&mut self, want: PreventSleep) {
        if self.current != Some(want) {
            apply(want);
            self.current = Some(want);
        }
    }

    /// Whether sleep is being held off right now.
    pub fn active(&self) -> bool {
        matches!(self.current, Some(PreventSleep::System | PreventSleep::Display))
    }
}

#[cfg(windows)]
fn apply(want: PreventSleep) {
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    let flags = match want {
        PreventSleep::Off => ES_CONTINUOUS,
        PreventSleep::System => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
        PreventSleep::Display => ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
    };
    // SAFETY: plain call; the state belongs to this thread and ends with it.
    if unsafe { SetThreadExecutionState(flags) } == 0 {
        log::warn!("SetThreadExecutionState({flags:#x}) failed");
    }
}

#[cfg(not(windows))]
fn apply(_want: PreventSleep) {}
