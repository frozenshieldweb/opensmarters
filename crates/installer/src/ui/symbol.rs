// SPDX-License-Identifier: GPL-3.0-or-later
//! The open-corners symbol (`assets/brand/symbol-gradient.svg`) drawn as egui meshes, and
//! the installer's signature moment: the symbol **fills with champagne light along its
//! diagonal** as the work progresses (LAUNCH_PROMPT §B3.3).
//!
//! Geometry comes straight from the SVG (1024 units): each of the two L-shaped pieces is a
//! bar 114.231 thick bent around a corner (outer radius 212.143, inner 97.912) with
//! rounded ends (16.319). We walk the bar's centre line and emit a triangle strip, so the
//! colour can vary per vertex: the champagne → rose gradient along the TL→BR diagonal, a
//! dim "at rest" version, and the lit part up to the progress, with a soft leading edge
//! and a few feathered layers of glow.

use eframe::egui::{self, Color32, Mesh, Painter, Pos2, Rect, Shape, vec2};

use afterglow::theme::color;

// Frame bounding box in SVG units.
const X0: f32 = 102.4;
const Y0: f32 = 184.32;
const FRAME_W: f32 = 819.2;
const FRAME_H: f32 = 655.36;
const THICK: f32 = 114.231;
const CAP: f32 = 16.319;
// Centre line of the top-left piece.
const BAR_X: f32 = 159.5155; // left bar centre x
const BAR_Y: f32 = 241.4355; // top bar centre y
const C1: (f32, f32) = (314.543, 396.463);
const RC: f32 = 155.0275;
const LEFT_END_Y: f32 = 619.051;
const TOP_END_X: f32 = 724.143;

const L1: f32 = LEFT_END_Y - C1.1; // left bar straight part
const L2: f32 = std::f32::consts::FRAC_PI_2 * RC; // corner arc
const L3: f32 = TOP_END_X - C1.0; // top bar straight part
const LEN: f32 = L1 + L2 + L3;

/// Champagne (the light) and muted rose: the symbol's gradient stops.
const CHAMPAGNE: Color32 = Color32::from_rgb(0xE6, 0xBE, 0x91);
const ROSE: Color32 = Color32::from_rgb(0xC9, 0x86, 0x8A);

/// Width / height of the symbol frame (5:4).
pub const ASPECT: f32 = FRAME_W / FRAME_H;

/// Point on the top-left piece's centre line at arc length `s` (extended straight beyond
/// both ends), and its outward normal.
fn centre(s: f32) -> ((f32, f32), (f32, f32)) {
    if s <= L1 {
        ((BAR_X, LEFT_END_Y - s), (-1.0, 0.0))
    } else if s <= L1 + L2 {
        let a = std::f32::consts::PI + (s - L1) / RC;
        let (sin, cos) = a.sin_cos();
        ((C1.0 + RC * cos, C1.1 + RC * sin), (cos, sin))
    } else {
        ((C1.0 + (s - L1 - L2), BAR_Y), (0.0, -1.0))
    }
}

/// Position along the gradient diagonal (0 at the top-left corner, 1 at the bottom-right).
fn diagonal(x: f32, y: f32) -> f32 {
    let (dx, dy) = (FRAME_W, FRAME_H);
    ((x - X0) * dx + (y - Y0) * dy) / (dx * dx + dy * dy)
}

/// The gradient colour at diagonal position `g`.
fn gradient(g: f32) -> Color32 {
    if g <= 0.36 {
        CHAMPAGNE
    } else {
        CHAMPAGNE.lerp_to_gamma(ROSE, ((g - 0.36) / 0.64).clamp(0.0, 1.0))
    }
}

fn scale(c: Color32, a: f32) -> Color32 {
    // Unmultiplied colour with alpha `a` (0..1).
    Color32::from_rgba_unmultiplied(
        c.r(),
        c.g(),
        c.b(),
        (a.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

/// One strip for both pieces: the bar offset outwards by `grow` (SVG units), with the
/// colour of each vertex from `paint(g, edge)` where `edge` is 0 on the bar's own outline
/// and 1 on the grown outline.
fn strips(rect: Rect, grow: f32, paint: &dyn Fn(f32, f32) -> Color32) -> Mesh {
    let k = rect.width() / FRAME_W;
    let to_screen = |x: f32, y: f32| -> Pos2 { rect.min + vec2((x - X0) * k, (y - Y0) * k) };
    let mut mesh = Mesh::default();
    let step = 3.0_f32;
    let start = -grow;
    let end = LEN + grow;
    let n = ((end - start) / step).ceil() as usize;
    for piece in 0..2 {
        let flip = |x: f32, y: f32| {
            if piece == 0 {
                (x, y)
            } else {
                (1024.0 - x, 1024.0 - y)
            }
        };
        let base = mesh.vertices.len() as u32;
        let rho = CAP + grow; // corner radius at the ends
        for i in 0..=n {
            let s = (start + i as f32 * step).min(end);
            let ((cx, cy), (nx, ny)) = centre(s);
            let d_end = (s - start).min(end - s).max(0.0);
            let half = THICK / 2.0 + grow;
            let hw = if d_end < rho {
                (half - rho) + (rho * rho - (rho - d_end) * (rho - d_end)).max(0.0).sqrt()
            } else {
                half
            };
            // Four vertices across: grown outer, outer, inner, grown inner. For the plain
            // fill (`grow` 0) outer == grown outer.
            let inner_hw = (hw - grow).clamp(0.0, THICK / 2.0);
            for (offset, edge) in [(hw, 1.0), (inner_hw, 0.0), (-inner_hw, 0.0), (-hw, 1.0)] {
                let (x, y) = flip(cx + nx * offset, cy + ny * offset);
                let g = diagonal(x, y);
                mesh.colored_vertex(to_screen(x, y), paint(g, edge));
            }
        }
        for i in 0..n as u32 {
            let a = base + i * 4;
            let b = a + 4;
            for j in 0..3 {
                mesh.add_triangle(a + j, a + j + 1, b + j);
                mesh.add_triangle(a + j + 1, b + j + 1, b + j);
            }
        }
    }
    mesh
}

/// The outline of each piece (outer edge forward, inner edge back), in screen points.
fn outlines(rect: Rect) -> [Vec<Pos2>; 2] {
    let k = rect.width() / FRAME_W;
    let to_screen = |x: f32, y: f32| -> Pos2 { rect.min + vec2((x - X0) * k, (y - Y0) * k) };
    let step = 3.0_f32;
    let n = (LEN / step).ceil() as usize;
    let mut out: [Vec<Pos2>; 2] = [Vec::new(), Vec::new()];
    for (piece, pts) in out.iter_mut().enumerate() {
        let flip = |x: f32, y: f32| {
            if piece == 0 {
                (x, y)
            } else {
                (1024.0 - x, 1024.0 - y)
            }
        };
        let mut outer = Vec::with_capacity(n + 1);
        let mut inner = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let s = (i as f32 * step).min(LEN);
            let ((cx, cy), (nx, ny)) = centre(s);
            let d_end = s.min(LEN - s).max(0.0);
            let half = THICK / 2.0;
            let hw = if d_end < CAP {
                (half - CAP) + (CAP * CAP - (CAP - d_end) * (CAP - d_end)).max(0.0).sqrt()
            } else {
                half
            };
            let (ox, oy) = flip(cx + nx * hw, cy + ny * hw);
            let (ix, iy) = flip(cx - nx * hw, cy - ny * hw);
            outer.push(to_screen(ox, oy));
            inner.push(to_screen(ix, iy));
        }
        pts.extend(outer);
        pts.extend(inner.into_iter().rev());
    }
    out
}

/// How the symbol looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Strength of the unlit symbol's hairline (0 = no frame at all, 1 = full).
    pub rest: f32,
    /// How far the light has filled it (0..1 along the diagonal).
    pub lit: f32,
    /// Glow around the lit part.
    pub glow: bool,
    /// Overall opacity (fades).
    pub opacity: f32,
}

/// Paints the symbol inside `rect` (its 5:4 frame).
pub fn paint(painter: &Painter, rect: Rect, look: Look) {
    let lit = look.lit.clamp(0.0, 1.0);
    // Soft leading edge, narrow enough that the fill reads as progress.
    let soft = 0.07;
    let light = |g: f32| -> f32 {
        if lit >= 1.0 {
            1.0
        } else if lit <= 0.0 {
            0.0
        } else {
            // Map lit 0..1 to an edge that starts before 0 and ends after 1 so both
            // extremes are fully dark / fully lit.
            let p = -soft + lit * (1.0 + 2.0 * soft);
            ((p - g) / soft * 0.5 + 0.5).clamp(0.0, 1.0)
        }
    };
    let o = look.opacity.clamp(0.0, 1.0);
    // Glow behind the lit part: a few feathered layers.
    if lit > 0.0 && look.glow {
        for (grow, alpha) in [(36.0, 0.10), (22.0, 0.14), (11.0, 0.20)] {
            let mesh = strips(rect, grow, &|g, edge| {
                let a = alpha * light(g) * (1.0 - edge) * o;
                scale(CHAMPAGNE, a)
            });
            painter.add(Shape::mesh(mesh));
        }
    }
    // At rest the symbol is an empty frame of dark glass with a champagne hairline: a
    // vessel waiting for light. The light then fills it.
    if look.rest > 0.0 {
        let glass = strips(rect, 0.0, &|_, _| scale(color::BG_DEEP, 0.62 * o));
        painter.add(Shape::mesh(glass));
    }
    let mesh = strips(rect, 0.0, &|g, _| {
        let l = light(g);
        let base = gradient(g);
        // The front of the light is a touch brighter (cream), like a glowing edge.
        let front = if lit > 0.0 && lit < 1.0 {
            let p = -soft + lit * (1.0 + 2.0 * soft);
            (1.0 - ((g - p).abs() / (soft * 1.5))).clamp(0.0, 1.0) * 0.5
        } else {
            0.0
        };
        let lit_c = base.lerp_to_gamma(color::CREAM, front);
        scale(lit_c, l * o)
    });
    painter.add(Shape::mesh(mesh));
    if look.rest > 0.0 {
        let w = (rect.width() / 120.0).clamp(0.75, 1.5);
        for pts in outlines(rect) {
            painter.add(Shape::closed_line(
                pts,
                egui::Stroke::new(w, scale(CHAMPAGNE, look.rest * o)),
            ));
        }
    }
}

/// Reduced motion: the symbol at rest with a plain progress bar inside its frame.
pub fn paint_with_bar(painter: &Painter, rect: Rect, rest: f32, fraction: f32, opacity: f32) {
    paint(
        painter,
        rect,
        Look {
            rest,
            lit: 0.0,
            glow: false,
            opacity,
        },
    );
    let k = rect.width() / FRAME_W;
    let inner = Rect::from_min_max(
        rect.min + vec2((216.631 - X0) * k, (298.551 - Y0) * k),
        rect.min + vec2((807.369 - X0) * k, (725.449 - Y0) * k),
    );
    let w = inner.width() * 0.72;
    let bar = Rect::from_center_size(inner.center(), vec2(w, 6.0));
    let track = scale(Color32::WHITE, 0.14 * opacity);
    let fill = scale(color::ACCENT, opacity);
    afterglow::paint::progress_bar(painter, bar, fraction, fill, track);
}

/// The 5:4 frame of a symbol `width` points wide centred on `center`.
pub fn frame(center: Pos2, width: f32) -> Rect {
    Rect::from_center_size(center, vec2(width, width / ASPECT))
}

/// For tests: the centre line is continuous and the pieces meet the SVG's end points.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centre_line_matches_the_svg() {
        let ((x, y), _) = centre(0.0);
        assert!((x - BAR_X).abs() < 0.01 && (y - LEFT_END_Y).abs() < 0.01);
        let ((x, y), _) = centre(LEN);
        assert!((x - TOP_END_X).abs() < 0.01 && (y - BAR_Y).abs() < 0.01);
        // Continuity at the arc's ends.
        for s in [L1, L1 + L2] {
            let (a, _) = centre(s - 0.001);
            let (b, _) = centre(s + 0.001);
            assert!((a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01, "{s}");
        }
        assert!((diagonal(X0, Y0)).abs() < 1e-6);
        assert!((diagonal(X0 + FRAME_W, Y0 + FRAME_H) - 1.0).abs() < 1e-6);
        assert!((ASPECT - 1.25).abs() < 1e-3);
    }

    #[test]
    fn the_mesh_is_well_formed() {
        let rect = frame(egui::pos2(100.0, 100.0), 120.0);
        let mesh = strips(rect, 0.0, &|_, _| Color32::WHITE);
        assert!(mesh.is_valid());
        assert!(
            mesh.vertices
                .iter()
                .all(|v| rect.expand(1.0).contains(v.pos))
        );
    }
}
