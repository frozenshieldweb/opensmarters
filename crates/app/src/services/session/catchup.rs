// SPDX-License-Identifier: GPL-3.0-or-later
//! Catch-up extensions of the session: the effective timeshift format (the profile's,
//! or one chosen in this session after the alternate format worked), requests with an
//! explicit format (for the one-time fallback), and the fallback policy itself
//! ([`Fallback`]).
//!
//! Real panels may answer `503` to every timeshift URL (see `docs/PROGRESS.md`): the UI
//! offers the other format **once** and only then gives up with a plain message.

use std::sync::PoisonError;

use opensmarters_core::ArchiveProgramme;
use opensmarters_player::PlayableUrl;
use opensmarters_store::{ProfileUpdate, TimeshiftFormat};

use super::{Session, Source, core_timeshift};
use crate::error::{AppError, AppResult};
use crate::playback::{MediaKind, PlaybackItem, PlaybackRequest, stream_headers};
use crate::services::catalog::LiveChannel;

/// The other timeshift format.
pub fn alternate(f: TimeshiftFormat) -> TimeshiftFormat {
    match f {
        TimeshiftFormat::Standard => TimeshiftFormat::SmartersCompat,
        TimeshiftFormat::SmartersCompat => TimeshiftFormat::Standard,
    }
}

/// A short user-facing name of a format.
pub fn format_label(f: TimeshiftFormat) -> &'static str {
    match f {
        TimeshiftFormat::Standard => "standard",
        TimeshiftFormat::SmartersCompat => "IPTV Smarters–style",
    }
}

/// The resume-point id of a catch-up programme (`"<stream_id>@<start>"`), as saved by the
/// player.
pub fn progress_id(stream_id: u64, programme: &ArchiveProgramme) -> String {
    format!("{stream_id}@{}", programme.start.timestamp())
}

/// Parses a catch-up resume-point id back into `(stream_id, start_timestamp)`.
pub fn parse_progress_id(id: &str) -> Option<(u64, i64)> {
    let (sid, start) = id.split_once('@')?;
    Some((sid.parse().ok()?, start.parse().ok()?))
}

/// What to offer after a catch-up attempt (pure policy, unit-tested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Nothing to do.
    None,
    /// It failed with the usual format: offer the other one (once).
    TryAlternate(TimeshiftFormat),
    /// The alternate format worked: offer to save it to the profile.
    OfferSave(TimeshiftFormat),
    /// Both formats failed: catch-up isn't available right now.
    GiveUp,
}

/// One catch-up playback attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// Resume-point id of the programme.
    pub item: String,
    /// Format used.
    pub format: TimeshiftFormat,
    /// This attempt is the one-time fallback.
    pub is_fallback: bool,
}

impl Attempt {
    /// The next step after this attempt failed.
    pub fn on_failure(&self) -> Fallback {
        if self.is_fallback {
            Fallback::GiveUp
        } else {
            Fallback::TryAlternate(alternate(self.format))
        }
    }

    /// The next step after this attempt started playing; `profile_format` is what the
    /// profile has saved.
    pub fn on_success(&self, profile_format: TimeshiftFormat) -> Fallback {
        if self.is_fallback && self.format != profile_format {
            Fallback::OfferSave(self.format)
        } else {
            Fallback::None
        }
    }
}

impl Session {
    /// The timeshift format catch-up uses now: the one chosen in this session (after the
    /// alternate format worked) or the profile's setting.
    pub fn timeshift_format(&self) -> TimeshiftFormat {
        self.ext_state()
            .timeshift_override
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unwrap_or(self.profile.settings.timeshift_format)
    }

    /// The profile's saved timeshift format (ignores the session choice).
    pub fn saved_timeshift_format(&self) -> TimeshiftFormat {
        self.ext_state()
            .timeshift_saved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unwrap_or(self.profile.settings.timeshift_format)
    }

    /// Uses `format` for catch-up for the rest of this session; with `persist`, also
    /// saves it to the profile (in the background).
    pub fn set_timeshift_format(&self, format: TimeshiftFormat, persist: bool) {
        *self
            .ext_state()
            .timeshift_override
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(format);
        if persist {
            *self
                .ext_state()
                .timeshift_saved
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(format);
            let id = self.profile.id;
            let mut settings = self.profile.settings.clone();
            settings.timeshift_format = format;
            self.services.blocking("save timeshift format", move |s| {
                s.profiles()
                    .update(
                        id,
                        ProfileUpdate {
                            settings: Some(settings),
                            ..ProfileUpdate::default()
                        },
                    )
                    .map(|_| ())
            });
        }
    }

    /// A catch-up request with an explicit timeshift format (see
    /// [`Session::catchup_request`], which uses the profile's).
    ///
    /// # Errors
    ///
    /// [`AppError::Locked`] when any of the channel's categories is locked (parental
    /// control); not an Xtream channel, or the URL was rejected.
    pub fn catchup_request_with(
        &self,
        channel: &LiveChannel,
        programme: &ArchiveProgramme,
        format: TimeshiftFormat,
        resume_from: Option<f64>,
    ) -> AppResult<PlaybackRequest> {
        let (Source::Xtream { urls, .. }, Some(stream_id)) = (&self.source, channel.stream_id())
        else {
            return Err(AppError::msg("Catch-up needs an Xtream Codes account."));
        };
        self.check_unlocked(
            opensmarters_store::ContentKind::Live,
            &channel.key,
            Some(&channel.categories),
        )?;
        let url = urls.timeshift(
            stream_id,
            programme,
            core_timeshift(format),
            self.live_container(),
        );
        Ok(PlaybackRequest {
            kind: MediaKind::Catchup,
            headers: stream_headers(url.as_str(), &self.user_agent, None, None),
            url: PlayableUrl::parse(url.as_str())?,
            title: crate::ui::text::cap(&programme.title, 160).into_owned(),
            subtitle: Some(channel.name.clone()),
            resume_from,
            item: PlaybackItem::Catchup {
                stream_id,
                start: programme.start.timestamp(),
                duration_minutes: programme.duration_minutes(),
            },
            artwork: self.safe_url(channel.logo.as_deref()),
            zap: None,
            duration_hint: Some(f64::from(programme.duration_minutes()) * 60.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_is_offered_once_then_gives_up() {
        let first = Attempt {
            item: "1@100".into(),
            format: TimeshiftFormat::Standard,
            is_fallback: false,
        };
        assert_eq!(
            first.on_failure(),
            Fallback::TryAlternate(TimeshiftFormat::SmartersCompat)
        );
        assert_eq!(first.on_success(TimeshiftFormat::Standard), Fallback::None);
        let second = Attempt {
            format: TimeshiftFormat::SmartersCompat,
            is_fallback: true,
            ..first.clone()
        };
        assert_eq!(second.on_failure(), Fallback::GiveUp);
        assert_eq!(
            second.on_success(TimeshiftFormat::Standard),
            Fallback::OfferSave(TimeshiftFormat::SmartersCompat)
        );
        // Already saved (e.g. the profile was switched meanwhile): nothing to offer.
        assert_eq!(
            second.on_success(TimeshiftFormat::SmartersCompat),
            Fallback::None
        );
        assert_eq!(
            alternate(alternate(TimeshiftFormat::Standard)),
            TimeshiftFormat::Standard
        );
    }

    #[test]
    fn progress_ids_round_trip() {
        assert_eq!(
            parse_progress_id("42@1700000000"),
            Some((42, 1_700_000_000))
        );
        assert_eq!(parse_progress_id("42"), None);
        assert_eq!(parse_progress_id("x@1"), None);
    }
}
