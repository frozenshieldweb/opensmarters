// SPDX-License-Identifier: GPL-3.0-or-later
//! Navigation: typed [`Route`]s, the back stack ([`Router`]) and the command queue
//! screens use ([`Navigator`], available as `cx.nav`).
//!
//! Rules: Esc/Backspace = back everywhere (the top screen may consume it first, e.g. to
//! close a dialog or hide the OSD); side-rail sections reset the stack to
//! `[Home, section]`; signing in resets to `[Home]`.

pub mod route;
pub mod router;

pub use route::{PlayerRoute, RecordingsTab, Route, Section, SettingsPage};
pub use router::{Change, Entry, NavCommand, Navigator, Router};

use eframe::egui::{Context, Id};

fn back_label_key() -> Id {
    Id::unique("opensmarters.nav.back_label")
}

/// Publishes the back label of the page under the top of the stack (the app does this
/// after every navigation).
pub fn set_back_label(ctx: &Context, label: Option<&'static str>) {
    ctx.data_mut(|d| match label {
        Some(l) => {
            d.insert_temp(back_label_key(), l);
        }
        None => {
            d.remove_temp::<&'static str>(back_label_key());
        }
    });
}

/// Where Esc/Back leads from the current page, as a short label for its back button
/// ("Movies", "Home"; [`Route::back_label`]). `None` on the root.
pub fn back_label(ctx: &Context) -> Option<&'static str> {
    ctx.data(|d| d.get_temp::<&'static str>(back_label_key()))
}
