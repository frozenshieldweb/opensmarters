// SPDX-License-Identifier: GPL-3.0-or-later
//! Navigation: typed [`Route`]s, the back stack ([`Router`]) and the command queue
//! screens use ([`Navigator`], available as `cx.nav`).
//!
//! Rules: Esc/Backspace = back everywhere (the top screen may consume it first, e.g. to
//! close a dialog or hide the OSD); side-rail sections reset the stack to
//! `[Home, section]`; signing in resets to `[Home]`.

pub mod route;
pub mod router;

pub use route::{PlayerRoute, Route, Section, SettingsPage};
pub use router::{Change, Entry, NavCommand, Navigator, Router};
