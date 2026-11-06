use serde::{Deserialize, Serialize};

/// Per-site RDP options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RdpSettings {
    /// Desktop size in pixels; 0 x 0 follows the size of the tab.
    pub width: u16,
    pub height: u16,
    /// Share the clipboard with the remote desktop.
    pub clipboard: bool,
    /// Play the remote desktop's sound on this PC (as mstsc's "Play on this computer").
    pub sound: bool,
    /// A folder of this PC shown in the remote desktop as a drive; empty shares none.
    pub drive_folder: String,
    /// The remote desktop can read the shared folder but not change it.
    pub drive_read_only: bool,
    /// Print from the remote desktop on this PC's default printer.
    pub printer: bool,
    /// RD Gateway to connect through, `host[:port]` (443 when left out); empty connects directly.
    pub gateway: String,
    /// User for the gateway; empty logs on to it with the site's user and password.
    pub gateway_user: String,
}

impl Default for RdpSettings {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            clipboard: true,
            sound: true,
            drive_folder: String::new(),
            drive_read_only: false,
            printer: false,
            gateway: String::new(),
            gateway_user: String::new(),
        }
    }
}

impl RdpSettings {
    /// Desktop sizes offered in the site editor; (0, 0) follows the tab.
    pub const SIZES: [(u16, u16); 6] = [(0, 0), (1920, 1080), (1600, 900), (1366, 768), (1280, 720), (1024, 768)];

    pub fn fits_tab(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The folder to share, if any.
    pub fn shared_folder(&self) -> Option<&str> {
        Some(self.drive_folder.trim()).filter(|f| !f.is_empty())
    }

    /// The gateway to connect through, if any.
    pub fn gateway(&self) -> Option<&str> {
        Some(self.gateway.trim()).filter(|g| !g.is_empty())
    }

    /// The gateway's own user, when it does not take the site's user and password.
    pub fn gateway_user(&self) -> Option<&str> {
        self.gateway().and(Some(self.gateway_user.trim()).filter(|u| !u.is_empty()))
    }
}
