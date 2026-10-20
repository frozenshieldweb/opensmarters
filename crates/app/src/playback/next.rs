// SPDX-License-Identifier: GPL-3.0-or-later
//! "Next episode": when an episode starts, the controller makes sure it knows the
//! episode's series (`Session::series_info`, cached) so that, when the episode
//! **finishes**, the player can offer the following one ([`PlaybackController::up_next`])
//! with a countdown and chain on ([`PlaybackController::play_up_next`]).
//!
//! It works wherever the episode was started from (detail page, Home's Continue
//! watching, Favourites, Search): the series is looked up from the request's
//! `PlaybackItem::Episode::series_id`. The order follows the store's `next_episode` rule
//! (next number, then the next season; specials chain into season 1; gaps are skipped).

use std::sync::{Arc, PoisonError};

use opensmarters_core::{Episode, SeriesInfo};

use super::{PlaybackController, PlaybackItem, PlaybackRequest, PlaybackStatus};
use crate::services::vod;

/// The series an episode belongs to.
#[derive(Debug, Clone)]
pub struct EpisodeQueue {
    /// `series_id`.
    pub series_id: u64,
    /// Series name (provider string).
    pub series_name: String,
    /// Series cover (fallback artwork).
    pub artwork: Option<String>,
    /// Seasons and episodes.
    pub info: Arc<SeriesInfo>,
}

/// The episode offered after the one that finished.
#[derive(Debug, Clone)]
pub struct UpNext {
    /// The episode that finished (keys the countdown).
    pub after: u64,
    /// The next episode.
    pub episode: Episode,
    /// Its season key.
    pub season: u32,
    /// Its display number.
    pub number: u32,
    /// `"S1·E4"` / `"Special 2"`.
    pub code: String,
    /// Clean episode title.
    pub title: String,
    /// Series name (display).
    pub series: String,
    /// Still or series artwork.
    pub artwork: Option<String>,
}

fn playing_episode(req: Option<&PlaybackRequest>) -> Option<(u64, u64)> {
    match req.map(|r| &r.item) {
        Some(PlaybackItem::Episode {
            episode_id,
            series_id,
            ..
        }) => Some((*series_id, *episode_id)),
        _ => None,
    }
}

impl PlaybackController {
    /// Makes sure the series of the playing episode is known (called when a stream
    /// starts). The lookup runs in the background; it's usually a cache hit.
    pub(super) fn sync_episode_queue(&mut self) {
        let Some((series_id, _)) = playing_episode(self.request()) else {
            return;
        };
        if series_id == 0 {
            return;
        }
        let known = self
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|q| q.series_id == series_id);
        if known {
            return;
        }
        let Some(session) = self.session().cloned() else {
            return;
        };
        let slot = Arc::clone(&self.queue);
        let ctx = self.ctx.clone();
        session.services().spawn({
            let session = Arc::clone(&session);
            async move {
                match session.series_info(series_id).await {
                    Ok(info) => {
                        let queue = EpisodeQueue {
                            series_id,
                            series_name: vod::series_name(&info),
                            artwork: session.safe_url(info.info.cover.as_deref()),
                            info,
                        };
                        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(queue);
                        ctx.request_repaint();
                    }
                    Err(e) => tracing::debug!(error = %e, "series lookup for next episode failed"),
                }
            }
        });
    }

    /// Sets the series of the episode about to play (the detail page already has it,
    /// which saves a lookup).
    pub fn set_episode_queue(&mut self, queue: EpisodeQueue) {
        *self.queue.lock().unwrap_or_else(PoisonError::into_inner) = Some(queue);
    }

    /// The episode to offer once the playing episode has **finished** (`None` while it
    /// plays, for other media, and after the last episode).
    pub fn up_next(&self) -> Option<UpNext> {
        if self.status() != PlaybackStatus::Finished {
            return None;
        }
        self.next_episode_after_current()
    }

    /// The episode after the playing one, whatever the status.
    pub fn next_episode_after_current(&self) -> Option<UpNext> {
        let (series_id, episode_id) = playing_episode(self.request())?;
        let guard = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let q = guard.as_ref().filter(|q| q.series_id == series_id)?;
        let next_id = vod::next_episode_id(&q.info, episode_id)?;
        let (season, number, ep) = vod::locate_episode(&q.info, next_id)?;
        Some(UpNext {
            after: episode_id,
            episode: ep.clone(),
            season,
            number,
            code: vod::episode_code(season, number),
            title: vod::episode_title(&ep.title, &q.series_name, number),
            series: vod::display_title(&q.series_name).title.to_owned(),
            artwork: ep.info.movie_image.clone().or_else(|| q.artwork.clone()),
        })
    }

    /// Plays the next episode from the start. Returns false when there is none or its
    /// request can't be built.
    pub fn play_up_next(&mut self) -> bool {
        let Some(next) = self.next_episode_after_current() else {
            return false;
        };
        let Some(session) = self.session().cloned() else {
            return false;
        };
        let (series_id, name) = {
            let guard = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            match guard.as_ref() {
                Some(q) => (q.series_id, q.series_name.clone()),
                None => return false,
            }
        };
        // `next.artwork` is the episode still, else the series cover.
        match session.series_episode_request(
            series_id,
            &name,
            &next.episode,
            next.season,
            next.number,
            next.artwork.as_deref(),
            None,
        ) {
            Ok(req) => {
                self.play(session, req);
                true
            }
            Err(e) => {
                tracing::info!(error = %e, "next episode not playable");
                false
            }
        }
    }
}
