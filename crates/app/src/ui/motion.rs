// SPDX-License-Identifier: GPL-3.0-or-later
//! Motion preferences (`docs/DESIGN.md` §7): the reduced-motion flag lives in egui's
//! context data so every widget can read it.

use eframe::egui::{Context, Id, emath::easing};

fn key() -> Id {
    Id::unique("opensmarters.reduced_motion")
}

/// True if the user asked for reduced motion (no scale on focus, no shimmer, no shake,
/// fades ≤ 100 ms).
pub fn reduced(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(key())).unwrap_or(false)
}

/// Sets the reduced-motion flag.
pub fn set_reduced(ctx: &Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(key(), on));
}

/// `secs`, or 0 with reduced motion.
pub fn duration(ctx: &Context, secs: f32) -> f32 {
    if reduced(ctx) { 0.0 } else { secs }
}

/// A fade duration: `secs`, capped at 100 ms with reduced motion.
pub fn fade(ctx: &Context, secs: f32) -> f32 {
    if reduced(ctx) { secs.min(0.1) } else { secs }
}

/// Animates a bool with cubic-out easing over `secs` (honouring reduced motion).
pub fn animate_bool(ctx: &Context, id: Id, on: bool, secs: f32) -> f32 {
    ctx.animate_bool_with_time_and_easing(id, on, duration(ctx, secs), easing::cubic_out)
}

/// Animates a value over `secs` (honouring reduced motion).
pub fn animate_value(ctx: &Context, id: Id, target: f32, secs: f32) -> f32 {
    ctx.animate_value_with_time(id, target, duration(ctx, secs))
}
