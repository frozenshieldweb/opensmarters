// SPDX-License-Identifier: GPL-3.0-or-later
//! **Afterglow**, opensmarters' design system for egui (`docs/DESIGN.md`), shared by the
//! app (`crates/app`) and the installer (`crates/installer`) so the two never drift apart.
//!
//! - [`theme`]: colour, spacing, radius, stroke, elevation and motion tokens; the font
//!   setup (Source Sans 3 and Fraunces with their variable-font coordinates, Phosphor
//!   icons, egui's fonts as fallbacks) and the egui style. Call [`theme::install`] once.
//! - [`icons`]: Phosphor Regular/Fill families and every glyph as a `&str` constant.
//! - [`paint`]: the focus treatment (ring + glow), gradients and scrims, cover-fit
//!   images, progress bar, spinner.
//! - [`motion`]: the reduced-motion flag (stored in the egui context) and animation
//!   helpers that honour it.
//!
//! ```ignore
//! afterglow::theme::install(ctx);
//! let painter = ui.painter();
//! afterglow::paint::focus_glow(painter, rect, afterglow::theme::radius::SM, 1.0);
//! painter.rect_filled(rect, afterglow::theme::radius::SM, afterglow::theme::color::ACCENT);
//! afterglow::paint::focus_ring(painter, rect, afterglow::theme::radius::SM, 1.0);
//! ```
//!
//! The app re-exports these modules as `crate::ui::{icons, motion, paint, theme}`.

#![forbid(unsafe_code)]

pub mod icons;
pub mod motion;
pub mod paint;
pub mod theme;
