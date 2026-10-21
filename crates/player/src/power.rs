// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeping the PC awake while something plays or records.
//!
//! The app draws the video itself (mpv only renders into it), so mpv's own screensaver
//! blocking doesn't apply: without this, Windows turns the display off and puts the PC to
//! sleep in the middle of a film. [`set_awake`] wraps `SetThreadExecutionState`, the
//! documented way for a media app to say so.

/// What the app needs from the power manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Awake {
    /// Nothing: the display may turn off and the PC may sleep as usual.
    #[default]
    Idle,
    /// The PC must not sleep (a recording or download runs), but the display may turn off.
    System,
    /// Video plays: the display stays on and the PC awake.
    Display,
}

/// Tells Windows what the app needs until the next call. The request belongs to the
/// calling thread, so call it from one thread (the UI thread) whenever the need changes;
/// Windows drops it when the thread exits. Returns whether Windows accepted it (always
/// `true` elsewhere, where it does nothing).
pub fn set_awake(need: Awake) -> bool {
    #[cfg(windows)]
    {
        const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
        const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;
        const ES_CONTINUOUS: u32 = 0x8000_0000;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetThreadExecutionState(flags: u32) -> u32;
        }
        let flags = ES_CONTINUOUS
            | match need {
                Awake::Idle => 0,
                Awake::System => ES_SYSTEM_REQUIRED,
                Awake::Display => ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
            };
        // SAFETY: plain Win32 call with documented flag values and no pointers; it only
        // changes this thread's power request. It returns the previous state, 0 on failure.
        unsafe { SetThreadExecutionState(flags) != 0 }
    }
    #[cfg(not(windows))]
    {
        let _ = need;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_request_is_accepted_and_can_be_released() {
        for need in [Awake::Display, Awake::System, Awake::Idle] {
            assert!(set_awake(need), "{need:?}");
        }
        assert_eq!(Awake::default(), Awake::Idle);
    }
}
