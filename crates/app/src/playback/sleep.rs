// SPDX-License-Identifier: GPL-3.0-or-later
//! Sleep timer (stub). Phase 7 fills it in: stop after N minutes, at the end of this film
//! or episode, or at the end of this programme; fade out, then ask "Still watching?".

/// The sleep timer (`cx.sleep`).
#[derive(Debug, Default)]
pub struct SleepTimer {
    _todo: (),
}

/// Called once per frame. **Stub.**
pub fn on_frame(_cx: &mut crate::screens::AppCtx<'_>) {}
