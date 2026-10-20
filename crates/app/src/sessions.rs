// SPDX-License-Identifier: GPL-3.0-or-later
//! Opening and switching profiles (`cx.sessions`). One profile is open at a time; opening
//! another replaces it when the login succeeds.

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

    /// Cancels an open in progress.
    pub fn cancel(&mut self) {
        self.loader.cancel();
        self.opening = None;
    }

    /// Closes the current session (e.g. its profile was deleted).
    pub fn close(&mut self) {
        self.current = None;
    }

    /// Replaces the current session directly (tests, demo).
    pub fn set_current(&mut self, session: Arc<Session>) {
        self.current = Some(session);
    }

    /// Applies a finished open. Call once per frame.
    pub fn poll(&mut self, _ctx: &egui::Context) -> Option<SessionEvent> {
        if !self.loader.poll() {
            return None;
        }
        let result = self.loader.take()?;
        let (profile, _) = self.opening.take()?;
        match result {
            Ok(session) => {
                self.current = Some(Arc::clone(&session));
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
