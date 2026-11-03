//! Keeps Windows from going to sleep (and optionally turning the screen off) while sessions are
//! connected, so idle connections are not dropped. Closing the lid or choosing Sleep still works:
//! this only stops the idle timers.
//!
//! The screen saver is another matter: execution states do not stop it. [`ScreenSaverGuard`]
//! keeps it away by reporting a zero-distance mouse move when the PC has been idle for a while.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use playterm_config::PreventSleep;

/// The execution state this (UI) thread has asked Windows for.
/// Whether this system can hold off sleep and the screen saver (Windows only so far); elsewhere
/// the settings and the menu item are hidden.
pub const SUPPORTED: bool = cfg!(windows);

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
        SUPPORTED && matches!(self.current, Some(PreventSleep::System | PreventSleep::Display))
    }
}

/// While on, the screen saver (and the idle lock and screen-off that come with it) never starts.
#[derive(Default)]
pub struct ScreenSaverGuard {
    on: Arc<AtomicBool>,
    started: bool,
}

impl ScreenSaverGuard {
    /// How often the idle time is checked.
    const CHECK: Duration = Duration::from_secs(10);
    /// Idle time after which a nudge is sent. With [`Self::CHECK`] this stays under a minute,
    /// the shortest screen saver timeout Windows offers.
    const IDLE: Duration = Duration::from_secs(30);

    pub fn set(&mut self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
        if on && !self.started {
            self.started = true;
            let flag = self.on.clone();
            let spawned = std::thread::Builder::new().name("screensaver-guard".into()).spawn(move || {
                loop {
                    std::thread::sleep(Self::CHECK);
                    if flag.load(Ordering::Relaxed) && idle_time() >= Self::IDLE {
                        nudge();
                    }
                }
            });
            if let Err(e) = spawned {
                log::warn!("screen saver guard thread: {e}");
            }
        }
    }

    pub fn active(&self) -> bool {
        SUPPORTED && self.on.load(Ordering::Relaxed)
    }
}

/// Time since the last keyboard or mouse input anywhere on the desktop.
#[cfg(windows)]
fn idle_time() -> Duration {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: `info` is a valid LASTINPUTINFO with cbSize set.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Duration::ZERO;
    }
    // SAFETY: plain query. Both are 32-bit tick counts, so wrapping subtraction is right.
    let now = unsafe { GetTickCount() };
    Duration::from_millis(now.wrapping_sub(info.dwTime).into())
}

#[cfg(not(windows))]
fn idle_time() -> Duration {
    Duration::ZERO
}

/// A mouse "move" of zero distance: resets the idle timer, the pointer stays where it is.
#[cfg(windows)]
fn nudge() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, MOUSEEVENTF_MOVE, SendInput};
    let mut input = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
    input.Anonymous.mi.dwFlags = MOUSEEVENTF_MOVE;
    // SAFETY: one fully initialized INPUT, with its size.
    if unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } != 1 {
        // Fails on the lock screen or a UAC prompt, where it is not needed anyway.
        log::debug!("SendInput failed");
    }
}

#[cfg(not(windows))]
fn nudge() {}

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
