//! Names of things that differ between Windows, macOS and other systems, for the UI text.

use playterm_i18n::t;

/// The system's file manager: Explorer, Finder, or a generic name.
pub fn file_manager() -> &'static str {
    if cfg!(windows) {
        t!("platform.explorer")
    } else if cfg!(target_os = "macos") {
        "Finder"
    } else {
        t!("platform.the_file_manager")
    }
}

/// The SSH agents playterm talks to here.
pub fn ssh_agents() -> &'static str {
    if cfg!(windows) { "Pageant / Windows OpenSSH" } else { "ssh-agent" }
}

/// Where the mouse pointer is on the screen (physical pixels), and whether its primary button
/// is down, read from the system: egui stops seeing the pointer once it leaves the window, also
/// while a tab is dragged out of it. `None` where this is not implemented.
#[cfg(windows)]
pub fn global_pointer() -> Option<(eframe::egui::Pos2, bool)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_SWAPBUTTON};
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: plain Win32 calls; `p` outlives the call that fills it.
    unsafe {
        if GetCursorPos(&mut p) == 0 {
            return None;
        }
        // GetAsyncKeyState reads the physical buttons: with swapped buttons the primary is the right one.
        let primary = if GetSystemMetrics(SM_SWAPBUTTON) != 0 { VK_RBUTTON } else { VK_LBUTTON };
        let down = (GetAsyncKeyState(primary.into()) as u16) & 0x8000 != 0;
        Some((eframe::egui::pos2(p.x as f32, p.y as f32), down))
    }
}

#[cfg(not(windows))]
pub fn global_pointer() -> Option<(eframe::egui::Pos2, bool)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fit_the_platform() {
        assert!(!file_manager().is_empty());
        if cfg!(target_os = "macos") {
            assert_eq!(file_manager(), "Finder");
            assert!(!ssh_agents().contains("Windows") && !ssh_agents().contains("Pageant"));
        }
        if cfg!(windows) {
            assert!(ssh_agents().contains("Pageant"));
        }
    }
}
