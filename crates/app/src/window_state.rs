// SPDX-License-Identifier: GPL-3.0-or-later
//! Window size, position and maximised state, remembered between runs in the store's
//! settings table ([`WINDOW`]).
//!
//! - Saved in **physical pixels** with the monitor scale at the time, so TV mode / UI
//!   scale zoom (which change egui's points) never distort it; restored through the
//!   `ViewportBuilder` in native logical points (`px / scale`).
//! - While maximised, the *restored* size and position are kept (so un-maximising after a
//!   restart goes back to the normal window). Fullscreen and minimised states are never
//!   saved.
//! - Saves are debounced ([`SAVE_AFTER`]) and run off the UI thread; the app also saves
//!   once more on exit.
//! - Not used with `--ephemeral` (nothing is written) or `--window-size` (harness runs).
//! - Hostile or stale values are sanitised ([`WindowState::sanitized`]): a window is never
//!   restored smaller than the app's minimum or absurdly far away.

use std::time::{Duration, Instant};

use eframe::egui::{self, ViewportInfo};
use opensmarters_store::settings::Setting;
use serde::{Deserialize, Serialize};

/// The stored window geometry.
pub const WINDOW: Setting<Option<WindowState>> = Setting::new("app.window", || None);

/// Wait this long after the last move/resize before saving.
pub const SAVE_AFTER: Duration = Duration::from_millis(1200);

/// Smallest inner size restored (physical px at scale 1; the app's minimum window).
const MIN_W: f32 = 640.0;
const MIN_H: f32 = 400.0;
/// Largest plausible coordinate / size (px).
const MAX_PX: f32 = 16_384.0;

/// Window geometry in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// Outer left edge (px, virtual desktop).
    pub x: f32,
    /// Outer top edge (px).
    pub y: f32,
    /// Inner width (px).
    pub w: f32,
    /// Inner height (px).
    pub h: f32,
    /// Monitor scale (native pixels per point) when saved.
    pub scale: f32,
    /// Maximised (x/y/w/h are then the restored geometry).
    pub maximized: bool,
}

impl WindowState {
    /// The state if every value is plausible: finite, scale in 0.5..=4, size at least the
    /// app minimum and at most 16 384 px, position within ±16 384 px with the title bar
    /// reachable (`y ≥ -16`).
    pub fn sanitized(self) -> Option<Self> {
        let finite = [self.x, self.y, self.w, self.h, self.scale]
            .iter()
            .all(|v| v.is_finite());
        if !finite || !(0.5..=4.0).contains(&self.scale) {
            return None;
        }
        Some(Self {
            x: self.x.clamp(-MAX_PX, MAX_PX),
            y: self.y.clamp(-16.0, MAX_PX),
            w: self.w.clamp(MIN_W * self.scale.min(1.0), MAX_PX),
            h: self.h.clamp(MIN_H * self.scale.min(1.0), MAX_PX),
            ..self
        })
    }

    /// Inner size in native logical points (for `ViewportBuilder::with_inner_size`).
    pub fn inner_size_points(&self) -> egui::Vec2 {
        egui::vec2(self.w, self.h) / self.scale
    }

    /// Outer position in native logical points (for `ViewportBuilder::with_position`).
    pub fn position_points(&self) -> egui::Pos2 {
        egui::pos2(self.x, self.y) / self.scale
    }

    /// Applies the state to a viewport builder.
    pub fn apply(&self, builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
        builder
            .with_inner_size(self.inner_size_points())
            .with_position(self.position_points())
            .with_maximized(self.maximized)
    }
}

/// Watches the viewport each frame and says when to save.
#[derive(Debug, Default)]
pub struct WindowTracker {
    /// The last normal (not maximised) geometry seen.
    normal: Option<WindowState>,
    current: Option<WindowState>,
    saved: Option<WindowState>,
    changed_at: Option<Instant>,
}

impl WindowTracker {
    /// Starts from the restored state (so an unchanged window isn't saved again).
    pub fn new(restored: Option<WindowState>) -> Self {
        Self {
            normal: restored.map(|s| WindowState {
                maximized: false,
                ..s
            }),
            current: restored,
            saved: restored,
            changed_at: None,
        }
    }

    /// Feeds this frame's viewport info. `ppp` is egui's pixels per point (native × zoom).
    pub fn observe(&mut self, info: &ViewportInfo, ppp: f32) {
        if info.minimized == Some(true) || info.fullscreen == Some(true) {
            return;
        }
        let (Some(inner), Some(outer), Some(native)) = (
            info.inner_rect,
            info.outer_rect,
            info.native_pixels_per_point,
        ) else {
            return;
        };
        let maximized = info.maximized == Some(true);
        if !maximized {
            self.normal = Some(WindowState {
                x: outer.min.x * ppp,
                y: outer.min.y * ppp,
                w: inner.width() * ppp,
                h: inner.height() * ppp,
                scale: native,
                maximized: false,
            });
        }
        let Some(normal) = self.normal else {
            return;
        };
        let next = WindowState {
            maximized,
            scale: native,
            ..normal
        };
        if self.current.is_none_or(|c| !close(&c, &next)) {
            self.current = Some(next);
            self.changed_at = Some(Instant::now());
        }
    }

    /// The state to save now, if it changed and has been stable for [`SAVE_AFTER`].
    pub fn due(&mut self) -> Option<WindowState> {
        let at = self.changed_at?;
        if at.elapsed() < SAVE_AFTER {
            return None;
        }
        self.changed_at = None;
        self.take_unsaved()
    }

    /// The state to save at exit, if different from the last save.
    pub fn take_unsaved(&mut self) -> Option<WindowState> {
        let cur = self.current?;
        if self.saved.is_some_and(|s| close(&s, &cur)) {
            return None;
        }
        self.saved = Some(cur);
        Some(cur)
    }
}

fn close(a: &WindowState, b: &WindowState) -> bool {
    a.maximized == b.maximized
        && (a.x - b.x).abs() < 1.0
        && (a.y - b.y).abs() < 1.0
        && (a.w - b.w).abs() < 1.0
        && (a.h - b.h).abs() < 1.0
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use eframe::egui::{Rect, pos2, vec2};
    use opensmarters_store::{MemorySecretStore, Store, StoreOptions};

    use super::*;

    fn info(x: f32, y: f32, w: f32, h: f32, maximized: bool) -> ViewportInfo {
        ViewportInfo {
            inner_rect: Some(Rect::from_min_size(pos2(x + 8.0, y + 31.0), vec2(w, h))),
            outer_rect: Some(Rect::from_min_size(pos2(x, y), vec2(w + 16.0, h + 39.0))),
            native_pixels_per_point: Some(1.5),
            maximized: Some(maximized),
            minimized: Some(false),
            fullscreen: Some(false),
            ..ViewportInfo::default()
        }
    }

    #[test]
    fn sanitizes_hostile_values() {
        let ok = WindowState {
            x: 100.0,
            y: 50.0,
            w: 1600.0,
            h: 900.0,
            scale: 1.25,
            maximized: false,
        };
        assert_eq!(ok.sanitized(), Some(ok));
        assert!(WindowState { w: f32::NAN, ..ok }.sanitized().is_none());
        assert!(WindowState { scale: 0.0, ..ok }.sanitized().is_none());
        let tiny = WindowState {
            w: 3.0,
            h: 2.0,
            y: -5000.0,
            x: 1e9,
            ..ok
        }
        .sanitized()
        .unwrap();
        assert!(tiny.w >= 640.0 && tiny.h >= 400.0);
        assert!(tiny.y >= -16.0 && tiny.x <= 16_384.0);
    }

    #[test]
    fn tracks_in_pixels_and_keeps_the_restored_geometry_while_maximised() {
        let mut t = WindowTracker::new(None);
        // ppp 1.5 (native 1.5, no zoom): points → px × 1.5.
        t.observe(&info(100.0, 60.0, 1000.0, 600.0, false), 1.5);
        assert!(t.due().is_none(), "debounced");
        let s = t.take_unsaved().unwrap();
        assert_eq!((s.x, s.y, s.w, s.h), (150.0, 90.0, 1500.0, 900.0));
        assert!(!s.maximized);
        assert!(t.take_unsaved().is_none(), "unchanged: nothing to save");

        // Maximise: the normal geometry is kept, only the flag changes.
        t.observe(&info(0.0, 0.0, 1280.0, 700.0, true), 1.5);
        let m = t.take_unsaved().unwrap();
        assert!(m.maximized);
        assert_eq!((m.w, m.h), (1500.0, 900.0));
        assert!((m.inner_size_points().x - 1000.0).abs() < 0.01);
    }

    #[test]
    fn zoom_does_not_change_the_saved_size() {
        // TV mode: zoom 1.75 → egui reports smaller points, ppp is native × zoom.
        let mut a = WindowTracker::new(None);
        a.observe(&info(10.0, 10.0, 1000.0, 600.0, false), 1.5);
        let mut b = WindowTracker::new(None);
        b.observe(
            &info(10.0 / 1.75, 10.0 / 1.75, 1000.0 / 1.75, 600.0 / 1.75, false),
            1.5 * 1.75,
        );
        let (sa, sb) = (a.take_unsaved().unwrap(), b.take_unsaved().unwrap());
        assert!((sa.w - sb.w).abs() < 0.5 && (sa.x - sb.x).abs() < 0.5);
    }

    #[test]
    fn minimised_and_fullscreen_are_ignored() {
        let mut t = WindowTracker::new(None);
        let mut i = info(0.0, 0.0, 800.0, 600.0, false);
        i.fullscreen = Some(true);
        t.observe(&i, 1.0);
        assert!(t.take_unsaved().is_none());
        i.fullscreen = Some(false);
        i.minimized = Some(true);
        t.observe(&i, 1.0);
        assert!(t.take_unsaved().is_none());
    }

    #[test]
    fn persists_in_the_store() {
        let store = Store::open(
            StoreOptions::in_memory(),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap();
        let s = WindowState {
            x: 1.0,
            y: 2.0,
            w: 1300.0,
            h: 800.0,
            scale: 1.0,
            maximized: true,
        };
        store.settings().set(&WINDOW, &Some(s)).unwrap();
        assert_eq!(store.settings().get(&WINDOW).unwrap(), Some(s));
    }
}
