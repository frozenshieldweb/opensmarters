// SPDX-License-Identifier: GPL-3.0-or-later
//! Multiscreen layout math (pure, unit-tested): 1×2 for two slots, 2×2 for three or four,
//! and the "enlarged" layout (one big tile, the others in a column on the right). Tiles are
//! 16:9 and centred in the area.

use eframe::egui::{Rect, pos2, vec2};

/// Gap between tiles (room for the 3 pt focus ring and its 3 pt gap on both sides).
pub const GAP: f32 = 16.0;
/// The small column takes at most this share of the width in the enlarged layout.
const SMALL_SHARE: f32 = 0.26;

/// Columns and rows for `slots` slots.
pub fn grid(slots: usize) -> (usize, usize) {
    match slots {
        0 | 1 => (1, 1),
        2 => (2, 1),
        _ => (2, 2),
    }
}

/// The layout's name for the header ("1 × 2", "2 × 2").
pub fn label(slots: usize) -> String {
    let (cols, rows) = grid(slots);
    format!("{rows} \u{d7} {cols}")
}

/// The largest 16:9 rect inside `cell`, centred.
fn fit_16x9(cell: Rect) -> Rect {
    let w = cell.width().min(cell.height() * 16.0 / 9.0).max(0.0);
    Rect::from_center_size(cell.center(), vec2(w, w * 9.0 / 16.0))
}

/// The rect of each slot: the grid, or with `enlarged = Some(i)` slot `i` big on the left
/// and the others stacked on the right (in slot order).
pub fn tile_rects(area: Rect, slots: usize, enlarged: Option<usize>) -> Vec<Rect> {
    if slots == 0 || !area.is_positive() {
        return Vec::new();
    }
    match enlarged.filter(|&e| e < slots && slots > 1) {
        Some(big) => enlarged_rects(area, slots, big),
        None => grid_rects(area, slots),
    }
}

fn grid_rects(area: Rect, slots: usize) -> Vec<Rect> {
    let (cols, rows) = grid(slots);
    let (cf, rf) = (cols as f32, rows as f32);
    let cell_w = ((area.width() - GAP * (cf - 1.0)) / cf).max(0.0);
    let cell_h = ((area.height() - GAP * (rf - 1.0)) / rf).max(0.0);
    let tile = fit_16x9(Rect::from_min_size(area.min, vec2(cell_w, cell_h))).size();
    // Pack the tiles together and centre the block.
    let block = vec2(
        tile.x * cf + GAP * (cf - 1.0),
        tile.y * rf + GAP * (rf - 1.0),
    );
    let origin = area.center() - block / 2.0;
    (0..slots)
        .map(|i| {
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            Rect::from_min_size(
                pos2(origin.x + c * (tile.x + GAP), origin.y + r * (tile.y + GAP)),
                tile,
            )
        })
        .collect()
}

fn enlarged_rects(area: Rect, slots: usize, big: usize) -> Vec<Rect> {
    let small_n = (slots - 1) as f32;
    let max_big = area.height() * 16.0 / 9.0;
    let mut small_w = area.width() * SMALL_SHARE;
    let mut big_w = (area.width() - GAP - small_w).min(max_big);
    // The small column must fit next to the big tile: shrink it if needed.
    let col_h_max = big_w * 9.0 / 16.0;
    let small_h_max = ((col_h_max - GAP * (small_n - 1.0)) / small_n).max(0.0);
    if small_w * 9.0 / 16.0 > small_h_max {
        small_w = small_h_max * 16.0 / 9.0;
        big_w = (area.width() - GAP - small_w).min(max_big);
    }
    let big_size = vec2(big_w, big_w * 9.0 / 16.0);
    let small_size = vec2(small_w, small_w * 9.0 / 16.0);
    let total_w = big_size.x + GAP + small_size.x;
    let left = area.center().x - total_w / 2.0;
    let top = area.center().y - big_size.y / 2.0;
    let big_rect = Rect::from_min_size(pos2(left, top), big_size);
    let mut out = Vec::with_capacity(slots);
    let mut k = 0.0;
    for i in 0..slots {
        if i == big {
            out.push(big_rect);
        } else {
            out.push(Rect::from_min_size(
                pos2(big_rect.right() + GAP, top + k * (small_size.y + GAP)),
                small_size,
            ));
            k += 1.0;
        }
    }
    out
}

/// The unused fourth cell of a 2×2 grid showing three slots (for a note about the
/// connection limit), else `None`.
pub fn spare_cell(area: Rect, slots: usize) -> Option<Rect> {
    if slots != 3 {
        return None;
    }
    let rects = grid_rects(area, 4);
    rects.get(3).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(pos2(48.0, 104.0), vec2(1824.0, 900.0))
    }

    fn assert_16x9(r: Rect) {
        let ratio = r.width() / r.height();
        assert!((ratio - 16.0 / 9.0).abs() < 0.01, "{r:?} is not 16:9");
    }

    fn assert_inside(r: Rect, area: Rect) {
        assert!(area.expand(0.5).contains_rect(r), "{r:?} outside {area:?}");
    }

    fn assert_apart(rects: &[Rect]) {
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(
                    !a.expand(GAP / 2.0 - 0.5)
                        .intersects(b.expand(GAP / 2.0 - 0.5)),
                    "{a:?} and {b:?} are closer than the gap"
                );
            }
        }
    }

    #[test]
    fn grids_by_slot_count() {
        assert_eq!(grid(2), (2, 1));
        assert_eq!(grid(3), (2, 2));
        assert_eq!(grid(4), (2, 2));
        assert_eq!(label(2), "1 \u{d7} 2");
        assert_eq!(label(4), "2 \u{d7} 2");
    }

    #[test]
    fn side_by_side_and_quad_fill_the_area() {
        for slots in 2..=4 {
            let rects = tile_rects(area(), slots, None);
            assert_eq!(rects.len(), slots);
            for r in &rects {
                assert_16x9(*r);
                assert_inside(*r, area());
            }
            assert_apart(&rects);
        }
        // 1×2: two tiles side by side, as wide as possible.
        let two = tile_rects(area(), 2, None);
        assert!((two[0].top() - two[1].top()).abs() < 0.01);
        assert!((two[0].width() * 2.0 + GAP - area().width()).abs() < 0.5);
        // 2×2: as tall as possible, centred.
        let four = tile_rects(area(), 4, None);
        assert!((four[0].height() * 2.0 + GAP - area().height()).abs() < 0.5);
        assert!((four[0].left() + four[1].right() - 2.0 * area().center().x).abs() < 0.5);
        // Three slots keep the 2×2 cells and leave the fourth for a note.
        let three = tile_rects(area(), 3, None);
        assert_eq!(three[..], four[..3]);
        assert_eq!(spare_cell(area(), 3), Some(four[3]));
        assert_eq!(spare_cell(area(), 4), None);
    }

    #[test]
    fn enlarged_tile_is_big_and_the_others_fit_beside_it() {
        for slots in 2..=4 {
            for big in 0..slots {
                let rects = tile_rects(area(), slots, Some(big));
                assert_eq!(rects.len(), slots);
                for (i, r) in rects.iter().enumerate() {
                    assert_16x9(*r);
                    assert_inside(*r, area());
                    if i != big {
                        assert!(r.width() < rects[big].width() * 0.5, "slot {i} too big");
                        assert!(r.left() > rects[big].right(), "small tiles on the right");
                        assert!(r.bottom() <= rects[big].bottom() + 0.5);
                    }
                }
                assert_apart(&rects);
            }
        }
        // The small tiles keep their order.
        let r = tile_rects(area(), 4, Some(2));
        assert!(r[0].top() < r[1].top() && r[1].top() < r[3].top());
    }

    #[test]
    fn degenerate_input_is_harmless() {
        assert!(tile_rects(area(), 0, None).is_empty());
        assert!(tile_rects(Rect::NOTHING, 4, None).is_empty());
        // An enlarged index past the slots falls back to the grid.
        assert_eq!(tile_rects(area(), 2, Some(5)), tile_rects(area(), 2, None));
        let one = tile_rects(area(), 1, Some(0));
        assert_eq!(one.len(), 1);
        assert_16x9(one[0]);
        // A tiny area (TV safe area at an odd size) still gives non-negative rects.
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(40.0, 20.0));
        for r in tile_rects(tiny, 4, Some(0)) {
            assert!(r.width() >= 0.0 && r.height() >= 0.0);
        }
    }
}
