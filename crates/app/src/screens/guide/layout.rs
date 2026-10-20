// SPDX-License-Identifier: GPL-3.0-or-later
//! EPG grid geometry (§6.9), pure and unit-tested: the time axis (6 pt per minute),
//! half-hour ruler slots aligned to the **viewer's** wall clock (correct across DST and
//! for :30/:45 offsets), visible-row ranges, and the keyboard model (left/right by
//! programme, up/down by channel keeping the time position).

use std::ops::Range;

use opensmarters_core::chrono::{DateTime, Duration, TimeZone, Timelike, Utc};

use crate::services::catalog::Programme;
use crate::services::session::guide::EpgRow;

/// Points per minute.
pub const PT_PER_MIN: f32 = 6.0;
/// Ruler slot length.
pub const SLOT_MINUTES: i64 = 30;
/// Row height.
pub const ROW_H: f32 = 64.0;
/// Gap between rows (and between cells).
pub const GAP: f32 = 2.0;
/// Row stride.
pub const ROW_STRIDE: f32 = ROW_H + GAP;
/// Channel column width.
pub const CHANNEL_W: f32 = 240.0;
/// Ruler height.
pub const RULER_H: f32 = 40.0;
/// Cells narrower than this show no text.
pub const MIN_TEXT_W: f32 = 48.0;

/// Maps instants to x positions: `x0` is where `origin` is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeAxis {
    /// Epoch seconds (fractional while scrolling) at `x0`.
    pub origin: f64,
    /// Screen x of `origin`.
    pub x0: f32,
}

impl TimeAxis {
    /// Points per second.
    pub const PT_PER_SEC: f64 = PT_PER_MIN as f64 / 60.0;

    /// The x of an instant.
    pub fn x(&self, t: DateTime<Utc>) -> f32 {
        self.x0 + ((t.timestamp() as f64 - self.origin) * Self::PT_PER_SEC) as f32
    }

    /// The instant at x (whole seconds).
    pub fn time_at(&self, x: f32) -> DateTime<Utc> {
        let secs = self.origin + f64::from(x - self.x0) / Self::PT_PER_SEC;
        DateTime::from_timestamp(secs.floor() as i64, 0).unwrap_or_default()
    }

    /// Seconds covered by `width` points.
    pub fn secs_for(width: f32) -> f64 {
        f64::from(width) / Self::PT_PER_SEC
    }
}

/// The start of the half-hour slot containing `t`, on `tz`'s wall clock.
pub fn floor_slot<Tz: TimeZone>(t: DateTime<Utc>, tz: &Tz) -> DateTime<Utc> {
    let local = t.with_timezone(tz);
    let into = i64::from(local.minute() % SLOT_MINUTES as u32) * 60 + i64::from(local.second());
    DateTime::from_timestamp(t.timestamp() - into, 0).unwrap_or(t)
}

/// Slot starts (local half hours) from the slot containing `from` up to `to`.
pub fn slot_starts<Tz: TimeZone>(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    tz: &Tz,
) -> Vec<DateTime<Utc>> {
    let mut out = Vec::new();
    let mut t = floor_slot(from, tz);
    while t < to && out.len() < 2000 {
        out.push(t);
        let next = t + Duration::minutes(SLOT_MINUTES);
        // Re-align after an offset change that isn't a multiple of 30 min.
        t = floor_slot(next + Duration::minutes(SLOT_MINUTES - 1), tz);
        if t <= *out.last().unwrap_or(&t) {
            t = next;
        }
    }
    out
}

/// Rows overlapping the viewport `[offset, offset + height)` (plus `overscan` rows each
/// side), clamped to `count`.
pub fn visible_rows(offset: f32, height: f32, count: usize, overscan: usize) -> Range<usize> {
    if count == 0 || height <= 0.0 {
        return 0..0;
    }
    let first = (offset.max(0.0) / ROW_STRIDE).floor() as usize;
    let last = ((offset.max(0.0) + height) / ROW_STRIDE).ceil() as usize;
    first.saturating_sub(overscan).min(count)..(last + overscan).min(count)
}

/// The row offset that brings row `i` fully into a viewport of `height` (unchanged when
/// it already is).
pub fn scroll_row_into_view(offset: f32, height: f32, i: usize) -> f32 {
    let top = i as f32 * ROW_STRIDE;
    let bottom = top + ROW_H;
    if top < offset {
        top
    } else if bottom > offset + height {
        (bottom - height).max(0.0)
    } else {
        offset
    }
}

/// The view start (epoch secs) that makes `[start, stop)` usefully visible in a window
/// of `view_secs`: unchanged when at least 20 minutes (or all) of it is on screen, else
/// its start lands 15 minutes in from the left edge. Clamped to `bounds`.
pub fn scroll_time_into_view(
    view_start: f64,
    view_secs: f64,
    start: i64,
    stop: i64,
    bounds: (f64, f64),
) -> f64 {
    let (s, e) = (start as f64, stop as f64);
    let view_end = view_start + view_secs;
    let visible = (e.min(view_end) - s.max(view_start)).max(0.0);
    let needed = (20.0 * 60.0f64).min(e - s).min(view_secs);
    if visible >= needed - 1.0 {
        return view_start;
    }
    let target = s - 15.0 * 60.0;
    target.clamp(bounds.0, (bounds.1 - view_secs).max(bounds.0))
}

/// The programme to land on in a row when moving vertically with the time cursor at `t`:
/// the one on air at `t`, else the next one, else the last one before.
pub fn programme_for_time(row: &EpgRow, t: DateTime<Utc>) -> Option<usize> {
    if row.programmes.is_empty() {
        return None;
    }
    if let Some(i) = row.index_at(t) {
        return Some(i);
    }
    let next = row.first_ending_after(t);
    if next < row.programmes.len() {
        Some(next)
    } else {
        Some(row.programmes.len() - 1)
    }
}

/// Index of the programme starting at `start` (epoch seconds).
pub fn index_of_start(row: &EpgRow, start: i64) -> Option<usize> {
    row.programmes
        .binary_search_by_key(&start, |p| p.start.timestamp())
        .ok()
}

/// The time cursor after focusing a programme, so ↑/↓ keep the position the user sees:
/// **now** for the programme on air (when now is in view — otherwise ↓ from the on-air
/// cell would land on a programme that already ended), else its start, or the left edge
/// when it started before the view.
pub fn cursor_for(p: &Programme, view_start: f64, now: DateTime<Utc>) -> DateTime<Utc> {
    let left = DateTime::from_timestamp(view_start.ceil() as i64, 0).unwrap_or(p.start);
    if p.start <= now && now < p.stop && now >= left {
        now
    } else if p.start < left && left < p.stop {
        left
    } else {
        p.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensmarters_core::chrono_tz::Tz;

    fn utc(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, d, h, m, 0)
            .single()
            .unwrap_or_default()
    }

    fn tz(name: &str) -> Tz {
        name.parse::<Tz>().unwrap_or(Tz::UTC)
    }

    #[test]
    fn axis_maps_six_points_per_minute() {
        let a = TimeAxis {
            origin: utc(20, 12, 0).timestamp() as f64,
            x0: 100.0,
        };
        assert!((a.x(utc(20, 12, 30)) - 280.0).abs() < 0.01);
        assert_eq!(a.time_at(280.0), utc(20, 12, 30));
        assert!((TimeAxis::secs_for(180.0) - 1800.0).abs() < 1e-6);
    }

    #[test]
    fn slots_follow_local_half_hours() {
        // Kathmandu is UTC+5:45: local half hours fall at :15 and :45 UTC.
        let k = tz("Asia/Kathmandu");
        let s = slot_starts(utc(20, 12, 20), utc(20, 13, 30), &k);
        let mins: Vec<u32> = s.iter().map(|t| t.minute()).collect();
        assert_eq!(mins, [15, 45, 15]);
        // St John's (UTC-2:30 in summer): :00/:30 local = :30/:00 UTC.
        let sj = tz("America/St_Johns");
        assert_eq!(floor_slot(utc(20, 12, 20), &sj), utc(20, 12, 0));
        assert_eq!(
            floor_slot(utc(20, 12, 20), &Tz::UTC),
            utc(20, 12, 0),
            "plain UTC"
        );
    }

    #[test]
    fn slots_across_dst_end() {
        // Paris leaves CEST on 25 Oct 2026 at 01:00Z: local 02:00–03:00 happens twice.
        let p = tz("Europe/Paris");
        let s = slot_starts(utc(24, 23, 0), utc(25, 2, 0), &p);
        assert_eq!(
            s.len(),
            6,
            "every 30 min in absolute time, no gap or overlap"
        );
        for w in s.windows(2) {
            assert_eq!((w[1] - w[0]).num_minutes(), 30);
        }
        // Lord Howe moves by 30 minutes; slots stay half-hourly.
        let lh = tz("Australia/Lord_Howe");
        let s = slot_starts(utc(3, 14, 0), utc(3, 17, 0), &lh);
        for w in s.windows(2) {
            assert_eq!((w[1] - w[0]).num_minutes(), 30);
        }
        for t in &s {
            assert_eq!(t.with_timezone(&lh).minute() % 30, 0);
        }
    }

    #[test]
    fn visible_rows_and_scrolling() {
        assert_eq!(visible_rows(0.0, 660.0, 10_000, 0), 0..10);
        assert_eq!(visible_rows(66.0 * 100.0 + 10.0, 660.0, 10_000, 2), 98..113);
        assert_eq!(visible_rows(0.0, 660.0, 3, 2), 0..3);
        assert_eq!(visible_rows(0.0, 660.0, 0, 2), 0..0);
        // Row 20 below a 660 pt viewport at the top: scroll so it is the last row.
        let off = scroll_row_into_view(0.0, 660.0, 20);
        assert!((off - (20.0 * ROW_STRIDE + ROW_H - 660.0)).abs() < 0.01);
        assert_eq!(scroll_row_into_view(off, 660.0, 15), off);
        assert_eq!(scroll_row_into_view(off, 660.0, 0), 0.0);
    }

    #[test]
    fn time_scrolling_keeps_useful_views() {
        let view = 1_000_000.0;
        let secs = 3.0 * 3600.0;
        let b = (0.0, 2_000_000.0);
        // Fully visible: unchanged.
        assert_eq!(
            scroll_time_into_view(view, secs, 1_000_600, 1_002_400, b),
            view
        );
        // A long programme that started before the view but shows 30 min: unchanged.
        assert_eq!(
            scroll_time_into_view(view, secs, 990_000, 1_001_800, b),
            view
        );
        // Off to the right: start lands 15 min in.
        assert_eq!(
            scroll_time_into_view(view, secs, 1_020_000, 1_021_800, b),
            1_020_000.0 - 900.0
        );
        // Clamped to the bounds.
        assert_eq!(
            scroll_time_into_view(view, secs, 100, 1900, (1000.0, 2_000_000.0)),
            1000.0
        );
    }

    fn prog(a: DateTime<Utc>, b: DateTime<Utc>) -> Programme {
        Programme {
            title: String::new(),
            description: String::new(),
            start: a,
            stop: b,
            has_archive: false,
        }
    }

    #[test]
    fn vertical_moves_keep_the_time_position() {
        let row = EpgRow::from_programmes(vec![
            prog(utc(20, 10, 0), utc(20, 11, 0)),
            prog(utc(20, 11, 0), utc(20, 12, 30)),
            prog(utc(20, 13, 0), utc(20, 14, 0)), // gap 12:30–13:00
        ]);
        assert_eq!(programme_for_time(&row, utc(20, 11, 45)), Some(1));
        assert_eq!(programme_for_time(&row, utc(20, 12, 45)), Some(2), "gap");
        assert_eq!(programme_for_time(&row, utc(20, 9, 0)), Some(0), "before");
        assert_eq!(programme_for_time(&row, utc(20, 23, 0)), Some(2), "after");
        assert_eq!(programme_for_time(&EpgRow::default(), utc(20, 9, 0)), None);
        assert_eq!(index_of_start(&row, utc(20, 13, 0).timestamp()), Some(2));
        assert_eq!(index_of_start(&row, utc(20, 13, 5).timestamp()), None);
        // Cursor: a programme that started before the view keeps the view's left edge
        // (here "now" is in the future, so no programme is on air in view).
        let left = utc(20, 11, 30).timestamp() as f64;
        let later = utc(20, 23, 0);
        assert_eq!(cursor_for(&row.programmes[1], left, later), utc(20, 11, 30));
        assert_eq!(cursor_for(&row.programmes[2], left, later), utc(20, 13, 0));
        // The on-air programme puts the cursor on now, not on the view's left edge
        // (QA-01: ↓ from the on-air cell must land on the next row's on-air programme).
        let now = utc(20, 12, 10);
        assert_eq!(cursor_for(&row.programmes[1], left, now), now);
        // …unless now is scrolled out of view to the left.
        let far_left = utc(20, 12, 20).timestamp() as f64;
        assert_eq!(
            cursor_for(&row.programmes[1], far_left, now),
            utc(20, 12, 20)
        );
    }
}
