// SPDX-License-Identifier: GPL-3.0-or-later
//! The operating system's motion preference (`docs/DESIGN.md` §7, reduced motion), read
//! with **safe Rust** (moved unchanged from the app's `platform` module so the installer
//! can share it):
//!
//! - [`animations_enabled`]: the OS "show animations" switch. On Windows this is
//!   `Windows.UI.ViewManagement.UISettings.AnimationsEnabled` (Settings → Accessibility →
//!   Visual effects → *Animation effects*, the same value as `SPI_GETCLIENTAREAANIMATION`),
//!   read through the `windows` crate's WinRT projection, whose methods are safe. Other
//!   platforms report `None` (unknown), and the manual setting applies.
//! - [`SystemMotion`]: polls that value every few seconds so turning animations off in
//!   Windows applies while the program runs.

use std::time::{Duration, Instant};

/// How often [`SystemMotion::poll`] asks the OS again.
pub const MOTION_POLL_EVERY: Duration = Duration::from_secs(3);

/// Whether the OS wants animations (`Some(false)` = the user turned them off). `None` if
/// unknown or unsupported.
pub fn animations_enabled() -> Option<bool> {
    #[cfg(windows)]
    {
        use windows::UI::ViewManagement::UISettings;
        match UISettings::new().and_then(|s| s.AnimationsEnabled()) {
            Ok(on) => Some(on),
            Err(e) => {
                tracing::debug!(error = %e, "UISettings.AnimationsEnabled unavailable");
                None
            }
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Caches [`animations_enabled`] and refreshes it every [`MOTION_POLL_EVERY`].
#[derive(Debug, Default)]
pub struct SystemMotion {
    checked: Option<Instant>,
    value: Option<bool>,
}

impl SystemMotion {
    /// The OS animation setting, re-read at most every [`MOTION_POLL_EVERY`].
    pub fn poll(&mut self) -> Option<bool> {
        if self
            .checked
            .is_none_or(|t| t.elapsed() >= MOTION_POLL_EVERY)
        {
            self.checked = Some(Instant::now());
            self.value = animations_enabled();
        }
        self.value
    }

    /// The last value read (no OS call).
    pub fn last(&self) -> Option<bool> {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_motion_caches_between_polls() {
        let mut m = SystemMotion::default();
        let first = m.poll();
        assert_eq!(m.last(), first);
        // A second poll within the interval doesn't ask the OS again.
        let t = m.checked;
        let _ = m.poll();
        assert_eq!(m.checked, t);
    }
}
