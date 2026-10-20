// SPDX-License-Identifier: GPL-3.0-or-later
//! Where a job's bytes come from, and how a saved file is played.
//!
//! **Stream URLs are rebuilt from the profile every time a job starts** (the session's
//! request builders, or for an Xtream recording the session's own URL builder with the
//! `.ts` container) and live only in memory: rows hold ids, never URLs.
//!
//! Saved files play through `PlayableUrl::from_owned_media` (inside the recordings
//! folder, first bytes must be video): films and episodes as `Movie`/`Episode` (the
//! resume point is shared with streaming), live recordings and clips as `Recording`
//! (their resume point is `recordings.position_secs`).

use std::path::Path;
use std::sync::Arc;

use opensmarters_core::Episode;
use opensmarters_core::url::Url;
use opensmarters_core::urls::LiveContainer as CoreContainer;
use opensmarters_player::{PlayableUrl, ValidationError};
use opensmarters_store::{LiveContainer, PlaybackKind, Recording, RecordingKind, Store};

use super::files;
use crate::error::{AppError, AppResult};
use crate::playback::{MediaKind, PlaybackItem, PlaybackRequest, StreamHeaders, stream_headers};
use crate::services::Session;
use crate::services::catalog::LiveChannel;

/// A job's source: the stream URL (secret) and its headers.
#[derive(Debug, Clone)]
pub struct Source {
    /// The URL (contains credentials; `Debug` is redacted).
    pub url: PlayableUrl,
    /// User-Agent / Referrer.
    pub headers: StreamHeaders,
}

impl Source {
    /// The URL for the downloader.
    ///
    /// # Errors
    ///
    /// Never in practice (a `PlayableUrl` is a valid URL).
    pub fn http_url(&self) -> AppResult<Url> {
        Url::parse(self.url.as_str())
            .map_err(|_| AppError::msg("This address can't be downloaded."))
    }
}

/// The episode a download row describes (enough for the URL builder).
fn episode_of(r: &Recording) -> AppResult<Episode> {
    let id = r
        .item_id
        .parse::<u64>()
        .map_err(|_| AppError::msg("This episode can't be downloaded."))?;
    Ok(Episode {
        id,
        episode_num: r.episode,
        title: r.title.clone(),
        container_extension: r.container_extension.clone(),
        season: r.season,
        added: None,
        direct_source: None,
        info: Default::default(),
    })
}

/// The source of a film or episode download.
///
/// # Errors
///
/// Not an Xtream account, bad ids, a rejected URL.
pub fn download_source(session: &Session, r: &Recording) -> AppResult<Source> {
    let req = match r.kind {
        RecordingKind::Movie => {
            let id = r
                .item_id
                .parse::<u64>()
                .map_err(|_| AppError::msg("This film can't be downloaded."))?;
            session.movie_request(
                id,
                r.container_extension.as_deref(),
                &r.title,
                None,
                None,
                None,
            )?
        }
        RecordingKind::Episode => {
            let series = r
                .series_id
                .as_deref()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or_default();
            session.episode_request(series, "", &episode_of(r)?, None, None)?
        }
        _ => return Err(AppError::msg("Only films and episodes can be downloaded.")),
    };
    Ok(Source {
        url: req.url,
        headers: req.headers,
    })
}

/// The channel `key` and the source to record it from: on Xtream the `.ts` container
/// when the account allows it (one continuous stream records more reliably than HLS),
/// otherwise what the player would open (M3U entries: the playlist's own address; the
/// engine detects HLS by the content).
///
/// # Errors
///
/// The channel is gone, the catalogue can't load, the URL was rejected.
pub async fn live_source(session: &Arc<Session>, key: &str) -> AppResult<(LiveChannel, Source)> {
    let catalog = session.live_catalog().await?;
    let channel = catalog
        .get(key)
        .cloned()
        .ok_or_else(|| AppError::msg("This channel is no longer offered by your provider."))?;
    if let (Some(client), Some(login), Some(stream_id)) =
        (session.xtream(), session.login(), channel.stream_id())
        && session
            .allowed_live_containers()
            .contains(&LiveContainer::Ts)
    {
        let url = client.stream_urls(login).live(stream_id, CoreContainer::Ts);
        let headers = stream_headers(url.as_str(), session.user_agent(), None, None);
        let url = PlayableUrl::parse(url.as_str())?;
        return Ok((channel, Source { url, headers }));
    }
    let req = session.live_request(&channel, None)?;
    Ok((
        channel,
        Source {
            url: req.url,
            headers: req.headers,
        },
    ))
}

/// The resume point to use for a saved file (blocking: reads the store).
pub fn resume_point(store: &Store, r: &Recording) -> Option<f64> {
    let kind = match r.kind {
        RecordingKind::Movie => PlaybackKind::Movie,
        RecordingKind::Episode => PlaybackKind::Episode,
        RecordingKind::Live | RecordingKind::Clip => {
            // Start over when it was watched to (nearly) the end.
            return r
                .position_secs
                .filter(|p| *p > 5.0 && r.duration_secs.is_none_or(|d| *p < d - 30.0));
        }
    };
    store
        .playback()
        .resume_position(r.profile, kind, &r.item_id)
        .ok()
        .flatten()
}

/// A request that plays the saved file of `r` from `dir` (blocking: checks the file).
///
/// # Errors
///
/// No file yet, the file is missing or isn't video.
pub fn playback_request(
    dir: &Path,
    r: &Recording,
    resume: Option<f64>,
) -> AppResult<PlaybackRequest> {
    let name = r
        .file_name
        .as_deref()
        .ok_or_else(|| AppError::msg("This hasn't been saved yet."))?;
    let path = files::path_in(dir, name)
        .ok_or_else(|| AppError::msg("This file's name isn't valid, so it won't be played."))?;
    let url = PlayableUrl::from_owned_media(dir, &path).map_err(|e| match e {
        ValidationError::NotMedia => {
            AppError::msg("This file isn't a video opensmarters can play, so it won't be opened.")
        }
        _ => AppError::msg(
            "The file is missing. It may have been moved, renamed or deleted from the \
             recordings folder.",
        ),
    })?;
    let title = crate::ui::text::cap(&r.title, 160).into_owned();
    let (kind, item, subtitle) = match r.kind {
        RecordingKind::Movie => (
            MediaKind::Movie,
            PlaybackItem::Movie {
                stream_id: r.item_id.parse().unwrap_or_default(),
                container_extension: r.container_extension.clone(),
            },
            Some("Downloaded copy".to_owned()),
        ),
        RecordingKind::Episode => {
            let se = match (r.season, r.episode) {
                (Some(s), Some(e)) => format!("S{s} · E{e}"),
                (None, Some(e)) => format!("E{e}"),
                _ => String::new(),
            };
            let sub = [r.subtitle.clone().unwrap_or_default(), se]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            (
                MediaKind::Episode,
                PlaybackItem::Episode {
                    episode_id: r.item_id.parse().unwrap_or_default(),
                    series_id: r
                        .series_id
                        .as_deref()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_default(),
                    season: r.season,
                    episode: r.episode,
                    container_extension: r.container_extension.clone(),
                },
                Some(sub).filter(|s| !s.is_empty()),
            )
        }
        RecordingKind::Live | RecordingKind::Clip => (
            MediaKind::Recording,
            PlaybackItem::Recording { id: r.id },
            r.subtitle.clone().map(|s| {
                if r.kind == RecordingKind::Clip {
                    format!("{s} · Clip")
                } else {
                    format!("{s} · Recording")
                }
            }),
        ),
    };
    Ok(PlaybackRequest {
        kind,
        url,
        title,
        subtitle,
        resume_from: resume,
        item,
        artwork: r.icon_url.clone(),
        headers: StreamHeaders::default(),
        zap: None,
        duration_hint: r.duration_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_rows_rebuild_the_episode() {
        let r = Recording {
            id: opensmarters_store::RecordingId(3),
            profile: opensmarters_store::ProfileId::new_random(),
            kind: RecordingKind::Episode,
            state: opensmarters_store::RecordingState::Queued,
            item_id: "77".into(),
            series_id: Some("9".into()),
            season: Some(2),
            episode: Some(5),
            container_extension: Some("mkv".into()),
            title: "Ep".into(),
            subtitle: Some("Show".into()),
            icon_url: None,
            start_at: None,
            end_at: None,
            file_name: None,
            bytes_done: 0,
            bytes_total: None,
            duration_secs: None,
            position_secs: None,
            error: None,
            created_at: opensmarters_store::Timestamp::EPOCH,
            updated_at: opensmarters_store::Timestamp::EPOCH,
        };
        let e = episode_of(&r).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((e.id, e.season, e.episode_num), (77, Some(2), Some(5)));
        assert_eq!(e.container_extension.as_deref(), Some("mkv"));
        // No file yet: nothing to play.
        assert!(playback_request(Path::new("C:/nowhere"), &r, None).is_err());
    }
}
