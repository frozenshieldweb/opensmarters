// SPDX-License-Identifier: GPL-3.0-or-later
//! Breakpoints and page metrics (`docs/DESIGN.md` §3). Computed once per frame from the
//! root width (in points) and available as `cx.layout`.

use eframe::egui::{Rect, Vec2};

/// Layout breakpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    /// < 960 pt.
    Compact,
    /// 960–1439 pt (and TV mode at 1080p ≈ 1100 pt).
    Medium,
    /// ≥ 1440 pt.
    Wide,
}

/// Width of the expanded side rail.
pub const RAIL_EXPANDED: f32 = 232.0;
/// Width of the collapsed side rail.
pub const RAIL_COLLAPSED: f32 = 72.0;

/// Per-frame layout metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// Breakpoint.
    pub breakpoint: Breakpoint,
    /// The whole window in points.
    pub screen: Rect,
    /// The area right of the side rail (whole screen for fullscreen routes).
    pub content: Rect,
    /// Page margin (24 / 40 / 48).
    pub margin: f32,
    /// Poster card width (132 / 148 / 168).
    pub poster_width: f32,
    /// TV mode (zoomed, remote-first).
    pub tv: bool,
}

impl Layout {
    /// Metrics for a screen rect.
    pub fn new(screen: Rect, tv: bool) -> Self {
        let w = screen.width();
        let breakpoint = if w < 960.0 {
            Breakpoint::Compact
        } else if w < 1440.0 {
            Breakpoint::Medium
        } else {
            Breakpoint::Wide
        };
        let (margin, poster_width) = match breakpoint {
            Breakpoint::Compact => (24.0, 132.0),
            Breakpoint::Medium => (40.0, 148.0),
            Breakpoint::Wide => (48.0, 168.0),
        };
        Self {
            breakpoint,
            screen,
            content: screen,
            margin,
            poster_width,
            tv,
        }
    }

    /// The side rail's resting width (expanded on wide, collapsed otherwise).
    pub fn rail_width(&self) -> f32 {
        match self.breakpoint {
            Breakpoint::Wide => RAIL_EXPANDED,
            Breakpoint::Medium | Breakpoint::Compact => RAIL_COLLAPSED,
        }
    }

    /// True on the wide breakpoint.
    pub fn is_wide(&self) -> bool {
        self.breakpoint == Breakpoint::Wide
    }

    /// Poster size (2:3).
    pub fn poster_size(&self) -> Vec2 {
        Vec2::new(self.poster_width, (self.poster_width * 1.5).round())
    }

    /// Backdrop card size (16:9): 296×166 wide, 260×146 otherwise.
    pub fn backdrop_card_size(&self) -> Vec2 {
        if self.is_wide() {
            Vec2::new(296.0, 166.0)
        } else {
            Vec2::new(260.0, 146.0)
        }
    }

    /// Channel tile size: 240×135 wide, 208×117 otherwise.
    pub fn channel_tile_size(&self) -> Vec2 {
        if self.is_wide() {
            Vec2::new(240.0, 135.0)
        } else {
            Vec2::new(208.0, 117.0)
        }
    }

    /// TV safe area: 5% insets in TV mode, the whole screen otherwise.
    pub fn safe_area(&self) -> Rect {
        if self.tv {
            self.screen
                .shrink2(self.screen.size() * 0.05 * Vec2::new(1.0, 0.6))
        } else {
            self.screen
        }
    }

    /// Grid columns for cards of `card_width` with 16 pt gaps in `width`.
    pub fn grid_columns(width: f32, card_width: f32) -> usize {
        (((width + 16.0) / (card_width + 16.0)).floor() as usize).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    #[test]
    fn breakpoints() {
        let l = Layout::new(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1920.0, 1080.0)),
            false,
        );
        assert_eq!(l.breakpoint, Breakpoint::Wide);
        assert_eq!(l.rail_width(), RAIL_EXPANDED);
        let l = Layout::new(
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1097.0, 617.0)),
            true,
        );
        assert_eq!(l.breakpoint, Breakpoint::Medium);
        assert_eq!(l.poster_width, 148.0);
        assert_eq!(Layout::grid_columns(1000.0, 168.0), 5);
    }
}
