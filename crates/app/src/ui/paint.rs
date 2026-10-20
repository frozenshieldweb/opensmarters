// SPDX-License-Identifier: GPL-3.0-or-later
//! Painting helpers: the focus treatment (§5/§9.4), gradients and scrims (§4.6/§9.5),
//! cover-fit images with rounded corners, shadows.

use eframe::egui::{
    self, Color32, CornerRadius, Mesh, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, TextureId,
    Vec2, epaint::RectShape, pos2, vec2,
};

use crate::ui::theme::{self, color};

/// Paints the glow behind a focused element. Call **before** painting the element.
/// `t` is the animated focus amount (0..1).
pub fn focus_glow(painter: &Painter, rect: Rect, radius: u8, t: f32) {
    if t <= 0.0 {
        return;
    }
    painter.add(theme::focus_glow_shadow(t).as_shape(rect, CornerRadius::same(radius)));
}

/// Paints the focus ring: a 3 pt `focus` stroke outside the element with a 3 pt gap in
/// the page colour (so the ring reads on any image). Call **after** painting the element.
pub fn focus_ring(painter: &Painter, rect: Rect, radius: u8, t: f32) {
    if t <= 0.0 {
        return;
    }
    let a = (255.0 * t.clamp(0.0, 1.0)).round() as u8;
    let gap = Color32::from_rgba_unmultiplied(color::BG.r(), color::BG.g(), color::BG.b(), a);
    painter.rect_stroke(
        rect.expand(1.5),
        CornerRadius::same(radius.saturating_add(1)),
        Stroke::new(3.0, gap),
        StrokeKind::Middle,
    );
    let ring =
        Color32::from_rgba_unmultiplied(color::FOCUS.r(), color::FOCUS.g(), color::FOCUS.b(), a);
    painter.rect_stroke(
        rect.expand(4.5),
        CornerRadius::same(radius.saturating_add(4)),
        Stroke::new(3.0, ring),
        StrokeKind::Middle,
    );
}

/// Focus ring for a circle (avatars, round icon buttons).
pub fn focus_ring_circle(painter: &Painter, center: Pos2, radius: f32, t: f32) {
    if t <= 0.0 {
        return;
    }
    let a = (255.0 * t.clamp(0.0, 1.0)).round() as u8;
    let gap = Color32::from_rgba_unmultiplied(color::BG.r(), color::BG.g(), color::BG.b(), a);
    painter.circle_stroke(center, radius + 1.5, Stroke::new(3.0, gap));
    let ring =
        Color32::from_rgba_unmultiplied(color::FOCUS.r(), color::FOCUS.g(), color::FOCUS.b(), a);
    painter.circle_stroke(center, radius + 4.5, Stroke::new(3.0, ring));
}

/// A drop shadow (elevation token) behind `rect`.
pub fn shadow(painter: &Painter, rect: Rect, radius: u8, shadow: egui::Shadow) {
    painter.add(shadow.as_shape(rect, CornerRadius::same(radius)));
}

/// Horizontal gradient through colour stops `(t, colour)` with `t` in 0..1 left→right.
pub fn h_gradient(painter: &Painter, rect: Rect, stops: &[(f32, Color32)]) {
    gradient(painter, rect, stops, true);
}

/// Vertical gradient through colour stops `(t, colour)` with `t` in 0..1 top→bottom.
pub fn v_gradient(painter: &Painter, rect: Rect, stops: &[(f32, Color32)]) {
    gradient(painter, rect, stops, false);
}

fn gradient(painter: &Painter, rect: Rect, stops: &[(f32, Color32)], horizontal: bool) {
    if stops.len() < 2 || !rect.is_positive() {
        return;
    }
    let mut mesh = Mesh::default();
    for (i, (t, c)) in stops.iter().enumerate() {
        let t = t.clamp(0.0, 1.0);
        let (a, b) = if horizontal {
            let x = rect.left() + rect.width() * t;
            (pos2(x, rect.top()), pos2(x, rect.bottom()))
        } else {
            let y = rect.top() + rect.height() * t;
            (pos2(rect.left(), y), pos2(rect.right(), y))
        };
        mesh.colored_vertex(a, *c);
        mesh.colored_vertex(b, *c);
        if i > 0 {
            let base = u32::try_from(i * 2).unwrap_or(0);
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base + 1, base);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// `bg` with alpha `a` (0..1), unmultiplied.
pub fn bg_alpha(a: f32) -> Color32 {
    with_alpha(color::BG, a)
}

/// `c` (opaque) with alpha `a` (0..1), unmultiplied.
pub fn with_alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// `scrim-left` (§4.6): `bg @92%` at the left edge fading to transparent at 60% width,
/// eased with intermediate stops.
pub fn scrim_left(painter: &Painter, rect: Rect) {
    let w = rect.width();
    let r = Rect::from_min_size(rect.min, vec2(w * 0.6, rect.height()));
    h_gradient(
        painter,
        r,
        &[
            (0.0, bg_alpha(0.92)),
            (0.35, bg_alpha(0.75)),
            (0.7, bg_alpha(0.3)),
            (1.0, bg_alpha(0.0)),
        ],
    );
}

/// `scrim-bottom` (§4.6): transparent to `bg` over the bottom `fraction` of `rect`.
pub fn scrim_bottom(painter: &Painter, rect: Rect, fraction: f32) {
    let h = rect.height() * fraction;
    let r = Rect::from_min_max(pos2(rect.left(), rect.bottom() - h), rect.max);
    v_gradient(
        painter,
        r,
        &[
            (0.0, bg_alpha(0.0)),
            (0.4, bg_alpha(0.45)),
            (0.75, bg_alpha(0.85)),
            (1.0, bg_alpha(1.0)),
        ],
    );
}

/// OSD top scrim: black 80% → 72% at 50% → 0%.
pub fn scrim_osd_top(painter: &Painter, rect: Rect, height: f32, alpha: f32) {
    let r = Rect::from_min_size(rect.min, vec2(rect.width(), height));
    let k = |a: f32| Color32::from_black_alpha((a * alpha * 255.0) as u8);
    v_gradient(painter, r, &[(0.0, k(0.8)), (0.5, k(0.72)), (1.0, k(0.0))]);
}

/// OSD bottom scrim: black 0% → 60% → 78% → 85% (top → bottom).
pub fn scrim_osd_bottom(painter: &Painter, rect: Rect, height: f32, alpha: f32) {
    let r = Rect::from_min_max(pos2(rect.left(), rect.bottom() - height), rect.max);
    let k = |a: f32| Color32::from_black_alpha((a * alpha * 255.0) as u8);
    v_gradient(
        painter,
        r,
        &[
            (0.0, k(0.0)),
            (0.35, k(0.6)),
            (0.7, k(0.78)),
            (1.0, k(0.85)),
        ],
    );
}

/// UV rect that cover-fits an image of `image_size` into `target`, keeping `anchor`
/// (0..1 on each axis; `vec2(1.0, 0.5)` = anchored right) in view.
pub fn cover_uv(image_size: Vec2, target: Vec2, anchor: Vec2) -> Rect {
    if image_size.x <= 0.0 || image_size.y <= 0.0 || target.x <= 0.0 || target.y <= 0.0 {
        return Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
    }
    let ai = image_size.x / image_size.y;
    let ar = target.x / target.y;
    if ai > ar {
        let u_w = ar / ai;
        Rect::from_min_size(pos2((1.0 - u_w) * anchor.x, 0.0), vec2(u_w, 1.0))
    } else {
        let v_h = ai / ar;
        Rect::from_min_size(pos2(0.0, (1.0 - v_h) * anchor.y), vec2(1.0, v_h))
    }
}

/// UV rect and painted rect that fit an image inside `target` (letterboxed, centred).
pub fn contain_rect(image_size: Vec2, target: Rect) -> Rect {
    if image_size.x <= 0.0 || image_size.y <= 0.0 {
        return target;
    }
    let scale = (target.width() / image_size.x).min(target.height() / image_size.y);
    Rect::from_center_size(target.center(), image_size * scale)
}

/// Paints a texture into `rect` with rounded corners, sampling `uv`, multiplied by `tint`.
pub fn image_rounded(
    painter: &Painter,
    texture: TextureId,
    rect: Rect,
    uv: Rect,
    radius: u8,
    tint: Color32,
) {
    painter
        .add(RectShape::filled(rect, CornerRadius::same(radius), tint).with_texture(texture, uv));
}

/// Masks the four corners of `rect` with `fill` so square content (the video blit)
/// looks rounded with radius `r`. Paint it after the content.
pub fn round_corners_mask(painter: &Painter, rect: Rect, r: f32, fill: Color32) {
    if r <= 0.0 {
        return;
    }
    let steps = 8;
    let corners = [
        (
            rect.left_top(),
            pos2(rect.left() + r, rect.top() + r),
            std::f32::consts::PI,
        ),
        (
            rect.right_top(),
            pos2(rect.right() - r, rect.top() + r),
            std::f32::consts::PI * 1.5,
        ),
        (
            rect.right_bottom(),
            pos2(rect.right() - r, rect.bottom() - r),
            0.0,
        ),
        (
            rect.left_bottom(),
            pos2(rect.left() + r, rect.bottom() - r),
            std::f32::consts::FRAC_PI_2,
        ),
    ];
    for (corner, center, start) in corners {
        let mut mesh = Mesh::default();
        mesh.colored_vertex(corner, fill);
        for i in 0..=steps {
            let a = start + std::f32::consts::FRAC_PI_2 * (i as f32 / steps as f32);
            mesh.colored_vertex(center + vec2(a.cos(), a.sin()) * r, fill);
        }
        for i in 1..=steps {
            let i = u32::try_from(i).unwrap_or(0);
            mesh.add_triangle(0, i, i + 1);
        }
        painter.add(Shape::mesh(mesh));
    }
}

/// Linear interpolation between two colours (in gamma space; fine for UI states).
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

/// A progress bar: `track` with an `accent` fill for `fraction` (0..1).
pub fn progress_bar(painter: &Painter, rect: Rect, fraction: f32, fill: Color32, track: Color32) {
    let r = (rect.height() / 2.0).round().clamp(0.0, 255.0) as u8;
    painter.rect_filled(rect, CornerRadius::same(r), track);
    let f = fraction.clamp(0.0, 1.0);
    if f > 0.0 {
        let filled = Rect::from_min_size(
            rect.min,
            vec2((rect.width() * f).max(rect.height()), rect.height()),
        );
        painter.rect_filled(filled, CornerRadius::same(r), fill);
    }
}

/// A ring spinner (`accent` arc on a faint track), rotating once per second.
pub fn spinner(painter: &Painter, center: Pos2, radius: f32, time: f64, width: f32) {
    painter.circle_stroke(center, radius, Stroke::new(width, color::PROGRESS_TRACK));
    let start = (time % 1.0) as f32 * std::f32::consts::TAU;
    let sweep = std::f32::consts::FRAC_PI_2 * 1.2;
    let n = 24;
    let points: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = start + sweep * (i as f32 / n as f32);
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(width, color::ACCENT)));
}
