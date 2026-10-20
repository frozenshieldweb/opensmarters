// SPDX-License-Identifier: GPL-3.0-or-later
//! Opening and switching profiles (`cx.sessions`). One profile is open at a time; opening
//! another replaces it when the login succeeds.
//!
//! A replaced or closed session is **closed** ([`Session::close`]): its queued EPG,
//! guide, look-up and image work is dropped and its background tasks stop before their
//! next provider request, so the old provider is never called again.
//!
//! The manager also reports user input to the open session every frame
//! ([`Session::note_activity`]): parental unlocks expire after
//! [`crate::services::session::RELOCK_IDLE`] without input.

use std::sync::Arc;

use eframe::egui;
use opensmarters_store::ProfileId;

use crate::services::{OpenFailure, Services, Session};
use crate::task::Loader;

/// What happened this frame.
#[derive(Debug)]
pub enum SessionEvent {
    /// A profile opened (it is now `current`).
    Opened(Arc<Session>),
    /// Opening failed.
    Failed(ProfileId),
}

/// The session state machine.
#[derive(Debug, Default)]
pub struct SessionManager {
    current: Option<Arc<Session>>,
    opening: Option<(ProfileId, String)>,
    loader: Loader<Result<Arc<Session>, OpenFailure>>,
    failure: Option<(ProfileId, OpenFailure)>,
}

/// True if this frame carries user input (keys, text, clicks, wheel, pointer moves).
fn has_user_input(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Key { .. }
                    | egui::Event::Text(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::PointerMoved(_)
                    | egui::Event::MouseWheel { .. }
                    | egui::Event::Touch { .. }
            )
        })
    })
}

impl SessionManager {
    /// A manager with nothing open.
    pub fn new() -> Self {
        Self::default()
    }

    /// The open session.
    pub fn current(&self) -> Option<&Arc<Session>> {
        self.current.as_ref()
    }

    /// The profile being opened (id, name).
    pub fn opening(&self) -> Option<&(ProfileId, String)> {
        self.opening.as_ref()
    }

    /// The last failure (for the profile picker / account status).
    pub fn failure(&self) -> Option<&(ProfileId, OpenFailure)> {
        self.failure.as_ref()
    }

    /// Clears the last failure.
    pub fn clear_failure(&mut self) {
        self.failure = None;
    }

    /// Starts opening a profile (logs in in the background).
    pub fn open(&mut self, services: &Services, profile: ProfileId, name: impl Into<String>) {
        self.opening = Some((profile, name.into()));
        self.failure = None;
        let services2 = services.clone();
        self.loader
            .start(services.rt(), services.egui(), move |_| async move {
                Ok(Session::open(services2, profile).await)
            });
    }

    /// "Refresh channels and films": drops every cached response of `session`'s profile
    /// (memory and disk) **and then** opens the profile again, in that order — the
    /// fresh session can never read the old cache, and the old session is closed when
    /// the new one replaces it. Use it instead of spawning `refresh()` next to
    /// [`SessionManager::open`] (those two race).
    pub fn reopen_fresh(&mut self, services: &Services, session: &Arc<Session>) {
        let profile = session.profile_id();
        self.opening = Some((profile, session.profile().name.clone()));
        self.failure = None;
        let services2 = services.clone();
        let old = Arc::clone(session);
        self.loader
            .start(services.rt(), services.egui(), move |_| async move {
                old.refresh().await;
                Ok(Session::open(services2, profile).await)
            });
    }

    /// Cancels an open in progress.
    pub fn cancel(&mut self) {
        self.loader.cancel();
        self.opening = None;
    }

    /// Closes the current session (e.g. its profile was deleted).
    pub fn close(&mut self) {
        if let Some(old) = self.current.take() {
            old.close();
        }
    }

    /// Replaces the current session directly (tests, demo). The previous one is closed.
    pub fn set_current(&mut self, session: Arc<Session>) {
        self.replace(session);
    }

    fn replace(&mut self, session: Arc<Session>) {
        let same = self
            .current
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &session));
        if let Some(old) = self.current.replace(session)
            && !same
        {
            old.close();
        }
    }

    /// Applies a finished open and reports user input to the open session. Call once per
    /// frame.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<SessionEvent> {
        if let Some(s) = &self.current
            && has_user_input(ctx)
        {
            s.note_activity();
        }
        if !self.loader.poll() {
            return None;
        }
        let result = self.loader.take()?;
        let (profile, _) = self.opening.take()?;
        match result {
            Ok(session) => {
                self.replace(Arc::clone(&session));
                Some(SessionEvent::Opened(session))
            }
            Err(f) => {
                tracing::info!(error = %f.error, "profile could not be opened");
                self.failure = Some((profile, f));
                Some(SessionEvent::Failed(profile))
            }
        }
    }
}
