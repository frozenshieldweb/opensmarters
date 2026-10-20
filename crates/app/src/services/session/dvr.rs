// SPDX-License-Identifier: GPL-3.0-or-later
//! Stream addresses for the recording/download engine (`crate::dvr`), built **without**
//! the parental-lock check of the request builders (`movie_request`, `episode_request`,
//! `live_request`).
//!
//! Why: a queued download or a scheduled recording runs in the background, perhaps after
//! a restart or after the 30-minute re-lock ([`super::locks::RELOCK_IDLE`]), when nobody
//! is there to enter the PIN and the item's categories may not be indexed yet. Parental
//! control gates **starting** a job (the UI checks `check_unlocked` before it queues or
//! schedules anything, `dvr::ui`) and **showing or playing** what was saved
//! (`dvr::ui::play_saved`, the Recordings screen), not the transfer itself.
//!
//! **Only `crate::dvr` may call these.** Everything that plays goes through the checked
//! request builders. The URL logic is the same as theirs: the session's own Xtream URL
//! builder (or the M3U entry's address, validated as a `PlayableUrl`), and the same
//! headers (`stream_headers`).

use opensmarters_core::urls::LiveContainer as CoreContainer;
use opensmarters_player::PlayableUrl;
use opensmarters_store::LiveContainer;

use super::{Session, Source};
use crate::error::{AppError, AppResult};
use crate::playback::{StreamHeaders, stream_headers};
use crate::services::catalog::{ChannelSource, LiveChannel};

impl Session {
    /// A film's stream address for a download job (no lock check, see the module docs).
    ///
    /// # Errors
    ///
    /// Not an Xtream account, or the URL was rejected.
    pub(crate) fn dvr_movie_url(
        &self,
        stream_id: u64,
        container_extension: Option<&str>,
    ) -> AppResult<(PlayableUrl, StreamHeaders)> {
        let Source::Xtream { urls, .. } = &self.source else {
            return Err(AppError::msg("Films need an Xtream Codes account."));
        };
        let url = urls.movie(stream_id, container_extension);
        let headers = stream_headers(url.as_str(), &self.user_agent, None, None);
        Ok((PlayableUrl::parse(url.as_str())?, headers))
    }

    /// An episode's stream address for a download job (no lock check).
    ///
    /// # Errors
    ///
    /// Not an Xtream account, or the URL was rejected.
    pub(crate) fn dvr_episode_url(
        &self,
        episode_id: u64,
        container_extension: Option<&str>,
    ) -> AppResult<(PlayableUrl, StreamHeaders)> {
        let Source::Xtream { urls, .. } = &self.source else {
            return Err(AppError::msg("Series need an Xtream Codes account."));
        };
        let url = urls.episode(episode_id, container_extension);
        let headers = stream_headers(url.as_str(), &self.user_agent, None, None);
        Ok((PlayableUrl::parse(url.as_str())?, headers))
    }

    /// A live channel's stream address for a recording job (no lock check). On Xtream
    /// the `.ts` container when the account allows it (one continuous stream records
    /// more reliably than HLS), else the profile's container; M3U entries use their own
    /// address and `#EXTVLCOPT` headers, like `live_request`.
    ///
    /// # Errors
    ///
    /// The channel doesn't belong to this account's kind, or the URL was rejected.
    pub(crate) fn dvr_live_url(
        &self,
        channel: &LiveChannel,
    ) -> AppResult<(PlayableUrl, StreamHeaders)> {
        match (&self.source, &channel.source) {
            (Source::Xtream { urls, .. }, ChannelSource::Xtream { stream_id }) => {
                let container = if self.allowed_live_containers().contains(&LiveContainer::Ts) {
                    CoreContainer::Ts
                } else {
                    self.live_container()
                };
                let url = urls.live(*stream_id, container);
                let headers = stream_headers(url.as_str(), &self.user_agent, None, None);
                Ok((PlayableUrl::parse(url.as_str())?, headers))
            }
            (
                _,
                ChannelSource::M3u {
                    url,
                    user_agent,
                    referrer,
                },
            ) => {
                let playable = PlayableUrl::parse(url).map_err(|_| {
                    AppError::msg(
                        "This channel's address isn't a web stream, so it won't be recorded \
                         for your safety.",
                    )
                })?;
                let headers = stream_headers(
                    url,
                    &self.user_agent,
                    user_agent.as_deref(),
                    referrer.as_deref(),
                );
                Ok((playable, headers))
            }
            _ => Err(AppError::msg(
                "This channel can't be recorded with this account.",
            )),
        }
    }
}
