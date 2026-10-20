// SPDX-License-Identifier: GPL-3.0-or-later
//! What to play: [`PlaybackRequest`], built by the session (`Session::live_request`,
//! `movie_request`, `episode_request`, `catchup_request`, `resume_request`) and handed to
//! the player screen (`Route::Player(PlayerRoute::Request(..))`) or the live preview.
//!
//! Also the header plumbing: the profile's User-Agent and an M3U entry's `#EXTVLCOPT`
//! User-Agent/Referrer, with the Referrer only kept for the stream's own host.

use std::sync::Arc;

use opensmarters_player::{PlayableUrl, Referrer, UserAgent};
use opensmarters_store::PlaybackKind;

use crate::services::catalog::LiveChannel;

/// Kind of media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// A live channel.
    Live,
    /// A movie (VOD).
    Movie,
    /// A series episode.
    Episode,
    /// A catch-up (timeshift) programme.
    Catchup,
    /// A live recording or clip played from the recordings folder (a local file: seekable,
    /// finishes at the end, no reconnects, no provider connection). Downloaded films and
    /// episodes play as [`MediaKind::Movie`]/[`MediaKind::Episode`] so their resume point
    /// is shared with streaming.
    Recording,
}

impl MediaKind {
    /// Live streams reconnect on errors and have no scrubber.
    pub fn is_live(self) -> bool {
        matches!(self, MediaKind::Live)
    }

    /// The store's resume-point kind (`None` for live).
    pub fn playback_kind(self) -> Option<PlaybackKind> {
        match self {
            MediaKind::Live => None,
            MediaKind::Movie => Some(PlaybackKind::Movie),
            MediaKind::Episode => Some(PlaybackKind::Episode),
            MediaKind::Catchup => Some(PlaybackKind::Catchup),
            MediaKind::Recording => None,
        }
    }
}

/// Ids needed for progress saving, favourites and "next episode".
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackItem {
    /// Live channel.
    Live {
        /// Channel key (favourites/recents id).
        key: String,
        /// Channel number.
        number: Option<u64>,
    },
    /// Movie.
    Movie {
        /// VOD `stream_id`.
        stream_id: u64,
        /// `container_extension` (to rebuild the URL on resume).
        container_extension: Option<String>,
    },
    /// Episode.
    Episode {
        /// Episode id.
        episode_id: u64,
        /// Series id.
        series_id: u64,
        /// Season number.
        season: Option<u32>,
        /// Episode number.
        episode: Option<u32>,
        /// `container_extension`.
        container_extension: Option<String>,
    },
    /// Catch-up programme.
    Catchup {
        /// Live `stream_id`.
        stream_id: u64,
        /// Programme start (UTC, epoch seconds).
        start: i64,
        /// Duration in minutes.
        duration_minutes: u32,
    },
    /// A live recording or clip (store row id); its resume point lives in the recordings
    /// table.
    Recording {
        /// `recordings.id`.
        id: opensmarters_store::RecordingId,
    },
}

impl PlaybackItem {
    /// The store item id for resume points (`None` for live). Catch-up uses
    /// `"<stream_id>@<start>"`.
    pub fn progress_id(&self) -> Option<String> {
        match self {
            PlaybackItem::Live { .. } | PlaybackItem::Recording { .. } => None,
            PlaybackItem::Movie { stream_id, .. } => Some(stream_id.to_string()),
            PlaybackItem::Episode { episode_id, .. } => Some(episode_id.to_string()),
            PlaybackItem::Catchup {
                stream_id, start, ..
            } => Some(format!("{stream_id}@{start}")),
        }
    }
}

/// Per-stream HTTP headers for mpv.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamHeaders {
    /// User-Agent (profile preset, or the M3U entry's `http-user-agent`).
    pub user_agent: Option<UserAgent>,
    /// Referrer, only when it points at the stream's own host.
    pub referrer: Option<Referrer>,
}

/// The live channel list the player can zap through.
#[derive(Debug, Clone)]
pub struct ZapContext {
    /// Channels in display order (the current list/category).
    pub channels: Arc<Vec<LiveChannel>>,
    /// Index of the playing channel.
    pub index: usize,
}

impl ZapContext {
    /// The channel `delta` steps away (wrapping).
    pub fn step(&self, delta: isize) -> Option<(usize, &LiveChannel)> {
        let n = self.channels.len();
        if n == 0 {
            return None;
        }
        let i = (self.index as isize + delta).rem_euclid(n as isize) as usize;
        self.channels.get(i).map(|c| (i, c))
    }

    /// The channel with `number`.
    pub fn find_number(&self, number: u64) -> Option<(usize, &LiveChannel)> {
        self.channels
            .iter()
            .enumerate()
            .find(|(_, c)| c.number == Some(number))
    }
}

/// Everything the player needs to play one item.
#[derive(Debug, Clone)]
pub struct PlaybackRequest {
    /// Kind.
    pub kind: MediaKind,
    /// The validated URL (contains credentials; `Debug` is redacted).
    pub url: PlayableUrl,
    /// Title for the OSD (channel name, movie title, episode title).
    pub title: String,
    /// Secondary line (programme, "S2 · E5", year · genre).
    pub subtitle: Option<String>,
    /// Start position in seconds.
    pub resume_from: Option<f64>,
    /// Ids for progress/favourites.
    pub item: PlaybackItem,
    /// Poster/logo URL for history snapshots and idle screens.
    pub artwork: Option<String>,
    /// HTTP headers.
    pub headers: StreamHeaders,
    /// Live zapping context.
    pub zap: Option<ZapContext>,
    /// Known duration (seconds) from metadata, before mpv reports it.
    pub duration_hint: Option<f64>,
}

/// Builds the headers for a stream: `item_ua` (M3U `#EXTVLCOPT`) wins over the profile UA
/// when it is a valid header value; `item_referrer` is kept only if its host equals the
/// stream URL's host.
pub fn stream_headers(
    stream_url: &str,
    profile_ua: &str,
    item_ua: Option<&str>,
    item_referrer: Option<&str>,
) -> StreamHeaders {
    let user_agent = item_ua
        .and_then(|ua| UserAgent::new(ua.trim()).ok())
        .or_else(|| UserAgent::new(profile_ua).ok());
    let referrer = item_referrer.and_then(|r| {
        let same_host = same_host(stream_url, r);
        if !same_host {
            tracing::debug!("ignoring an M3U referrer for another host");
            return None;
        }
        Referrer::parse(r.trim()).ok()
    });
    StreamHeaders {
        user_agent,
        referrer,
    }
}

fn same_host(a: &str, b: &str) -> bool {
    let host = |s: &str| {
        opensmarters_core::url::Url::parse(s.trim())
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    };
    matches!((host(a), host(b)), (Some(x), Some(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_user_agent_wins_when_valid() {
        let h = stream_headers(
            "http://s.example/1.ts",
            "opensmarters/0.1",
            Some("VLC/3"),
            None,
        );
        assert_eq!(h.user_agent.as_ref().map(UserAgent::as_str), Some("VLC/3"));
        let h = stream_headers(
            "http://s.example/1.ts",
            "opensmarters/0.1",
            Some("bad\nua"),
            None,
        );
        assert_eq!(
            h.user_agent.as_ref().map(UserAgent::as_str),
            Some("opensmarters/0.1")
        );
    }

    #[test]
    fn referrer_only_for_the_streams_own_host() {
        let h = stream_headers(
            "http://s.example:8080/live/u/p/1.ts",
            "ua",
            None,
            Some("http://S.example/portal"),
        );
        assert!(h.referrer.is_some());
        let h = stream_headers(
            "http://s.example/live/1.ts",
            "ua",
            None,
            Some("http://tracker.example/"),
        );
        assert!(h.referrer.is_none());
        let h = stream_headers(
            "http://s.example/1.ts",
            "ua",
            None,
            Some("file:///etc/passwd"),
        );
        assert!(h.referrer.is_none());
    }

    #[test]
    fn progress_ids() {
        assert_eq!(
            PlaybackItem::Catchup {
                stream_id: 5,
                start: 100,
                duration_minutes: 30
            }
            .progress_id()
            .as_deref(),
            Some("5@100")
        );
        assert_eq!(
            PlaybackItem::Live {
                key: "1".into(),
                number: None
            }
            .progress_id(),
            None
        );
    }
}
