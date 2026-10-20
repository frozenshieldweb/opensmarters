// SPDX-License-Identifier: GPL-3.0-or-later
//! # opensmarters (app)
//!
//! The eframe/egui desktop app. Start with `docs/APP_ARCHITECTURE.md`; the short map:
//!
//! | Module | Role |
//! |---|---|
//! | [`app`] | The eframe `App`: frame loop, router, lifecycle |
//! | [`screens`] | One module per screen + the [`screens::Screen`] contract and [`screens::AppCtx`] |
//! | [`nav`] | Typed [`nav::Route`]s, back stack, `--route` syntax |
//! | [`focus`] | Spatial focus engine (keyboard / remote / mouse) |
//! | [`ui`] | Design tokens, icons, painting, widgets |
//! | [`services`] | Runtime, store, session (catalogue, EPG, favourites, parental), images |
//! | [`playback`] | Shared player, reconnect, progress, [`playback::PlaybackRequest`] |
//! | [`task`] | [`task::Loader`] for background work |
//! | [`sessions`] | Opening/switching profiles |
//! | [`logging`] | Redacted logging |
//! | [`cli`], [`harness`] | Command line and the dev/test harness |

#![forbid(unsafe_code)]

pub mod app;
pub mod cli;
pub mod error;
pub mod focus;
pub mod harness;
pub mod logging;
pub mod nav;
pub mod playback;
pub mod screens;
pub mod services;
pub mod sessions;
pub mod task;
pub mod ui;
