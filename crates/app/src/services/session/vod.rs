// SPDX-License-Identifier: GPL-3.0-or-later
//! Movies and series additions to [`Session`]: watch state for the detail pages and
//! poster grids, marking episodes watched/unwatched, the remembered browse category per
//! profile, and episode requests with display titles.
//!
//! Everything that touches the store runs on the blocking pool (never the UI thread).

use std::collections::{BTreeMap, HashMap};

use opensmarters_core::{CategoryId, Episode};
use opensmarters_store::settings::Setting;
use opensmarters_store::{
    ContentKind, PlaybackKind, PlaybackProgress, PlaybackSnapshot, ResumeThresholds,
};

use super::{Session, blocking};
use crate::error::AppResult;
use crate::playback::PlaybackRequest;
use crate::services::vod;

/// Last browsed category per profile and kind (`"<profile>:<movie|series>"` → id).
const LAST_CATEGORY: Setting<BTreeMap<String, String>> =
    Setting::new("app.vod_last_category", BTreeMap::new);

/// How many history rows the poster grids read for progress bars.
const HISTORY_ROWS: usize = 2000;

/// Watch state of one series.
#[derive(Debug, Clone, Default)]
pub struct SeriesWatch {
    /// Stored progress by episode id.
    pub episodes: HashMap<u64, PlaybackProgress>,
    /// The most recently watched episode.
    pub last: Option<PlaybackProgress>,
}

impl SeriesWatch {
    /// Progress of an episode.
    pub fn get(&self, episode_id: u64) -> Option<&PlaybackProgress> {
        self.episodes.get(&episode_id)
    }

    /// True if the episode was watched to the end (ever).
    pub fn watched(&self, episode_id: u64) -> bool {
        self.get(episode_id)
            .is_some_and(|p| p.finished || p.ever_finished)
    }
}

fn kind_key(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Live => "live",
        ContentKind::Movie => "movie",
        ContentKind::Series => "series",
    }
}

impl Session {
    /// The store's resume thresholds (no I/O).
    pub fn resume_thresholds(&self) -> ResumeThresholds {
        self.store().playback().thresholds()
    }

    /// A movie's resume point.
    ///
    /// # Errors
    ///
    /// Store errors.
    pub async fn movie_progress(&self, stream_id: u64) -> AppResult<Option<PlaybackProgress>> {
        let profile = self.profile_id();
        blocking(self.store(), move |s| {
            s.playback()
                .get(profile, PlaybackKind::Movie, &stream_id.to_string())
        })
        .await
    }

    /// Recent movie progress by `stream_id` (poster progress bars).
    ///
    /// # Errors
    ///
    /// Store errors.
    pub async fn movie_progress_map(&self) -> AppResult<HashMap<u64, PlaybackProgress>> {
        let profile = self.profile_id();
        blocking(self.store(), move |s| {
            Ok(s.playback()
                .history(profile, HISTORY_ROWS)?
                .into_iter()
                .filter(|p| p.kind == PlaybackKind::Movie)
                .filter_map(|p| p.item_id.parse::<u64>().ok().map(|id| (id, p)))
                .collect())
        })
        .await
    }

    /// Every stored episode of a series plus the most recently watched one.
    ///
    /// # Errors
    ///
    /// Store errors.
    pub async fn series_watch(&self, series_id: u64) -> AppResult<SeriesWatch> {
        let profile = self.profile_id();
        blocking(self.store(), move |s| {
            let sid = series_id.to_string();
            let pb = s.playback();
            let episodes = pb
                .series_progress(profile, &sid)?
                .into_iter()
                .filter_map(|p| p.item_id.parse::<u64>().ok().map(|id| (id, p)))
                .collect();
            let last = pb.last_episode(profile, &sid)?;
            Ok(SeriesWatch { episodes, last })
        })
        .await
    }

    /// Marks an episode watched (finished, and the latest of its series so "Continue"
    /// moves on) or unwatched (its resume point is forgotten).
    ///
    /// # Errors
    ///
    /// Store errors.
    #[allow(clippy::too_many_arguments)] // mirrors `series_episode_request`
    pub async fn set_episode_watched(
        &self,
        series_id: u64,
        series_name: &str,
        episode: &Episode,
        season: u32,
        number: u32,
        artwork: Option<&str>,
        watched: bool,
    ) -> AppResult<()> {
        let profile = self.profile_id();
        let id = episode.id.to_string();
        let snapshot = PlaybackSnapshot {
            title: episode_display_title(series_name, episode, number),
            poster_url: self.safe_url(artwork.or(episode.info.movie_image.as_deref())),
            series_id: Some(series_id.to_string()),
            season: Some(season),
            episode: Some(number),
            container_extension: episode.container_extension.clone(),
        };
        let hint = episode
            .info
            .duration_secs
            .map(|d| d as f64)
            .or_else(|| {
                episode
                    .info
                    .duration
                    .as_deref()
                    .and_then(vod::parse_duration)
                    .map(|d| d as f64)
            })
            .filter(|d| *d > 0.0);
        // A write: in order with progress saves and other writes.
        self.services()
            .write("mark an episode watched", move |s| {
                let pb = s.playback();
                if watched {
                    let existing = pb.get(profile, PlaybackKind::Episode, &id)?;
                    let duration = existing.as_ref().and_then(|p| p.duration_secs).or(hint);
                    let pos = duration.unwrap_or_else(|| {
                        existing.as_ref().map_or(1.0, |p| p.position_secs.max(1.0))
                    });
                    pb.record_progress(
                        profile,
                        PlaybackKind::Episode,
                        &id,
                        pos,
                        duration,
                        &snapshot,
                    )?;
                    pb.mark_finished(profile, PlaybackKind::Episode, &id)?;
                } else {
                    pb.remove(profile, PlaybackKind::Episode, &id)?;
                }
                Ok(())
            })
            .await
    }

    /// The category last browsed in Movies (`ContentKind::Movie`) or Series.
    pub async fn last_vod_category(&self, kind: ContentKind) -> Option<CategoryId> {
        let key = format!("{}:{}", self.profile_id(), kind_key(kind));
        // Read behind any queued `remember_vod_category` (FIFO writer).
        self.services()
            .write("read the last category", move |s| {
                Ok(s.settings().get(&LAST_CATEGORY)?.remove(&key))
            })
            .await
            .ok()
            .flatten()
            .map(CategoryId::from)
    }

    /// Remembers the browsed category (in the background).
    pub fn remember_vod_category(&self, kind: ContentKind, id: &CategoryId) {
        let key = format!("{}:{}", self.profile_id(), kind_key(kind));
        let value = id.as_str().to_owned();
        self.services().blocking("remember category", move |s| {
            let settings = s.settings();
            let mut map = settings.get(&LAST_CATEGORY)?;
            if map.get(&key) == Some(&value) {
                return Ok(());
            }
            map.insert(key, value);
            settings.set(&LAST_CATEGORY, &map)
        });
    }

    /// An episode request whose title reads well everywhere it is shown (player OSD,
    /// Continue watching): `"Series · Episode title"`, subtitle `"S1 · E3"`. `season` is
    /// the season key of the series info (episodes don't always state it), `number` the
    /// display number.
    ///
    /// # Errors
    ///
    /// Not an Xtream account, or the URL was rejected.
    #[allow(clippy::too_many_arguments)]
    pub fn series_episode_request(
        &self,
        series_id: u64,
        series_name: &str,
        episode: &Episode,
        season: u32,
        number: u32,
        artwork: Option<&str>,
        resume_from: Option<f64>,
    ) -> AppResult<PlaybackRequest> {
        let mut ep = episode.clone();
        ep.title = episode_display_title(series_name, episode, number);
        ep.season = Some(season);
        ep.episode_num = Some(number);
        let mut req = self.episode_request(series_id, "", &ep, artwork, resume_from)?;
        if req.duration_hint.is_none() {
            req.duration_hint = episode
                .info
                .duration
                .as_deref()
                .and_then(vod::parse_duration)
                .map(|d| d as f64);
        }
        Ok(req)
    }
}

/// `"Series · Episode title"` (the series name without tag/year).
pub(crate) fn episode_display_title(series_name: &str, episode: &Episode, number: u32) -> String {
    let series = vod::display_title(series_name).title;
    let title = vod::episode_title(&episode.title, series_name, number);
    if series.is_empty() {
        title
    } else {
        format!("{series} · {title}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_titles_for_episodes() {
        let ep = Episode {
            id: 1,
            episode_num: Some(2),
            title: "My Show - S01E02 - The Pilot".into(),
            container_extension: None,
            season: None,
            added: None,
            direct_source: None,
            info: Default::default(),
        };
        assert_eq!(
            episode_display_title("FR | My Show (2010)", &ep, 2),
            "My Show · The Pilot"
        );
        assert_eq!(
            episode_display_title("", &ep, 2),
            "My Show - S01E02 - The Pilot"
        );
        assert_eq!(kind_key(ContentKind::Series), "series");
    }
}
