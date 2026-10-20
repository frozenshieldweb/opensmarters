// SPDX-License-Identifier: GPL-3.0-or-later
//! App-wide prompts drawn above every screen (stub): programme reminders ("Starting now
//! ... Watch / Dismiss"), the sleep timer's "Still watching?". Phase 7 fills it in, together
//! with the reminder scheduler.

/// Prompt queue and reminder scheduler state (`cx.alerts`).
#[derive(Debug, Default)]
pub struct Alerts {
    _todo: (),
}

/// Called once per frame before the screen (fire due reminders). **Stub.**
pub fn on_frame(_cx: &mut crate::screens::AppCtx<'_>) {}

/// Draws the prompts above the screen and the side rail. **Stub.**
pub fn show(_cx: &mut crate::screens::AppCtx<'_>, _ui: &mut eframe::egui::Ui) {}
