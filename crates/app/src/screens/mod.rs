// SPDX-License-Identifier: GPL-3.0-or-later
//! Screens. One module (file or folder) per screen; each implements [`Screen`] and is
//! created from its [`Route`] by [`create`].
//!
//! ## The contract
//!
//! - [`Screen::ui`] draws the screen into the content area (right of the side rail for
//!   [`Chrome::Shell`], the whole window for [`Chrome::Fullscreen`]). It must not block:
//!   start work with [`crate::task::Loader`] and poll it here.
//! - [`Screen::on_enter`] runs when the screen becomes the top of the stack (first time
//!   and when returning to it); [`Screen::on_leave`] when it is covered or removed
//!   (`cx.next_route` tells you what comes next).
//! - [`Screen::on_input`] runs before the focus engine each frame: consume keys you own
//!   (digits, PageUp…) with `ctx.input_mut(|i| i.consume_key(..))`.
//! - [`Screen::on_back`] is offered Esc/Backspace first: return `true` if you used it (close
//!   a dialog, hide the OSD). Otherwise the router goes back.
//! - Navigate with `cx.nav` (push/replace/back/reset/section); play with
//!   [`AppCtx::play`].

pub mod catchup;
pub mod detail;
pub mod favourites;
pub mod guide;
pub mod home;
pub mod live;
pub mod movies;
pub mod multiscreen;
pub mod onboarding;
pub mod player;
pub mod profiles;
pub mod recordings;
pub mod search;
pub mod series;
pub mod settings;
pub mod shell;
pub mod welcome;

use std::sync::Arc;

use eframe::egui;
use opensmarters_core::chrono::{DateTime, Utc};
use tokio::runtime::Handle;

use crate::nav::{Navigator, PlayerRoute, Route, Section};
use crate::playback::{PlaybackController, PlaybackRequest};
use crate::services::{Prefs, Services, Session};
use crate::sessions::SessionManager;
use crate::ui::layout::Layout;
use crate::ui::widgets::Toasts;

/// Window chrome around a screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chrome {
    /// Side rail + content.
    Shell,
    /// The whole window (player, profiles, onboarding).
    Fullscreen,
}

/// Everything a screen can use (built by the app each frame).
pub struct AppCtx<'a> {
    /// The egui context.
    pub egui: egui::Context,
    /// App-wide services (runtime, store, images, redactor, paths).
    pub services: &'a Services,
    /// Navigation commands (applied after the frame).
    pub nav: &'a mut Navigator,
    /// Toasts.
    pub toasts: &'a mut Toasts,
    /// The shared player.
    pub playback: &'a mut PlaybackController,
    /// Preferences (call [`AppCtx::save_prefs`] after changing them).
    pub prefs: &'a mut Prefs,
    /// Profile sessions (open/switch).
    pub sessions: &'a mut SessionManager,
    /// Layout metrics for this frame.
    pub layout: Layout,
    /// Current time (UTC) for EPG progress.
    pub now: DateTime<Utc>,
    /// During `on_leave`: the route becoming the top.
    pub next_route: Option<Route>,
    /// Recordings and downloads (app-wide).
    pub dvr: &'a mut crate::dvr::Dvr,
    /// The sleep timer.
    pub sleep: &'a mut crate::playback::sleep::SleepTimer,
    /// App-wide prompts (reminders, "Still watching?").
    pub alerts: &'a mut crate::services::alerts::Alerts,
    /// Multiscreen tile players.
    pub multi: &'a mut crate::playback::multi::MultiView,
    pub(crate) prefs_dirty: &'a mut bool,
}

impl std::fmt::Debug for AppCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCtx")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl AppCtx<'_> {
    /// The active session, if a profile is open.
    pub fn session(&self) -> Option<Arc<Session>> {
        self.sessions.current().cloned()
    }

    /// The tokio runtime.
    pub fn rt(&self) -> &Handle {
        self.services.rt()
    }

    /// Persists the preferences in the background (and applies motion/zoom).
    pub fn save_prefs(&mut self) {
        *self.prefs_dirty = true;
    }

    /// Plays `req`: in the shared player and, if `fullscreen`, opens the player screen.
    pub fn play(&mut self, req: PlaybackRequest, fullscreen: bool) {
        let Some(session) = self.session() else {
            return;
        };
        if fullscreen {
            self.nav
                .push(Route::Player(PlayerRoute::Request(Arc::new(req))));
        } else {
            self.playback.play(session, req);
        }
    }
}

/// A screen. See the [module docs](self).
pub trait Screen {
    /// Draws the screen.
    fn ui(&mut self, cx: &mut AppCtx<'_>, ui: &mut egui::Ui);

    /// The screen became the top of the stack.
    fn on_enter(&mut self, _cx: &mut AppCtx<'_>) {}

    /// The screen is covered or removed.
    fn on_leave(&mut self, _cx: &mut AppCtx<'_>) {}

    /// Claim keys before the focus engine runs.
    fn on_input(&mut self, _cx: &mut AppCtx<'_>, _ctx: &egui::Context) {}

    /// Esc/Backspace: return true if consumed.
    fn on_back(&mut self, _cx: &mut AppCtx<'_>) -> bool {
        false
    }

    /// Window chrome.
    fn chrome(&self) -> Chrome {
        Chrome::Shell
    }

    /// Side-rail section to highlight (defaults to the route's section).
    fn section(&self) -> Option<Section> {
        None
    }
}

/// Builds the screen for a route.
pub fn create(route: &Route) -> Box<dyn Screen> {
    match route {
        Route::Welcome => Box::new(welcome::WelcomeScreen::new()),
        Route::Profiles => Box::new(profiles::ProfilesScreen::new()),
        Route::Onboarding { edit } => Box::new(onboarding::OnboardingScreen::new(*edit)),
        Route::Home => Box::new(home::HomeScreen::new()),
        Route::Live { category } => Box::new(live::LiveScreen::new(category.clone())),
        Route::Guide => Box::new(guide::GuideScreen::new()),
        Route::Movies { category } => Box::new(movies::MoviesScreen::new(category.clone())),
        Route::Series { category } => Box::new(series::SeriesScreen::new(category.clone())),
        Route::MovieDetail { stream_id } => Box::new(detail::DetailScreen::movie(*stream_id)),
        Route::SeriesDetail { series_id } => Box::new(detail::DetailScreen::series(*series_id)),
        Route::Catchup { stream_id } => Box::new(catchup::CatchupScreen::new(*stream_id)),
        Route::Search { query } => Box::new(search::SearchScreen::new(query.clone())),
        Route::Favourites => Box::new(favourites::FavouritesScreen::new()),
        Route::Recordings { tab } => Box::new(recordings::RecordingsScreen::new(*tab)),
        Route::Multiscreen => Box::new(multiscreen::MultiscreenScreen::new()),
        Route::Settings { page } => Box::new(settings::SettingsScreen::new(*page)),
        Route::Player(p) => Box::new(player::PlayerScreen::new(p.clone())),
    }
}
