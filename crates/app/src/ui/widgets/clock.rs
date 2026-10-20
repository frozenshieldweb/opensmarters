// SPDX-License-Identifier: GPL-3.0-or-later
//! The clock (tabular digits) used in headers and the OSD.

use std::time::Duration;

use eframe::egui::{Align2, Color32, Context, FontId, Painter, Pos2, Rect};
use opensmarters_core::chrono::{Local, Timelike};

use crate::services::Prefs;

/// The local time formatted per the 12/24 h preference.
pub fn now_text(prefs: &Prefs) -> String {
    prefs.format_time(&Local::now())
}

/// Paints the clock and schedules a repaint at the next minute.
pub fn clock(
    ctx: &Context,
    painter: &Painter,
    pos: Pos2,
    align: Align2,
    prefs: &Prefs,
    font: FontId,
    color: Color32,
) -> Rect {
    let now = Local::now();
    let secs_left = 60 - u64::from(now.second());
    ctx.request_repaint_after(Duration::from_secs(secs_left.max(1)));
    painter.text(pos, align, prefs.format_time(&now), font, color)
}
