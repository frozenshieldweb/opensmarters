// SPDX-License-Identifier: GPL-3.0-or-later
//! The UI kit: design tokens ([`theme`]), Phosphor [`icons`], painting helpers
//! ([`paint`]), text helpers ([`text`]), motion ([`motion`]), layout breakpoints
//! ([`layout`]), bundled artwork ([`assets`]) and reusable [`widgets`].
//!
//! [`theme`], [`icons`], [`paint`] and [`motion`] live in the shared `afterglow` crate
//! (`crates/afterglow`, also used by the installer) and are re-exported here unchanged.

pub mod assets;
pub use afterglow::{icons, motion, paint, theme};
pub mod layout;
pub mod text;
pub mod widgets;
