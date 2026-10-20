// SPDX-License-Identifier: GPL-3.0-or-later
//! Typed domain models for the Xtream Codes API.
//!
//! All models deserialise **leniently** from the panel's JSON (see [`crate::de`]): wrong
//! types become `None`/empty, only missing ids make an item invalid. Field names follow
//! the wire names from `docs/PROTOCOL.md`, except where noted.
//!
//! List and detail models also implement `Serialize`; the output uses the wire field
//! names and epoch-second timestamps, so it deserialises back into the same value (useful
//! for caching). [`EpgListing`] is a decoded view and is not meant to round-trip.
//!
//! Passwords never appear in any model: the panel echoes `user_info.password` in the
//! login response, and that field is deliberately not deserialised.

mod account;
mod detail;
mod epg;
mod login;
mod streams;

pub use account::{ServerUrl, XtreamAccount};
pub use detail::{
    Episode, EpisodeInfo, MovieData, Season, SeriesDetails, SeriesInfo, VodDetails, VodInfo,
};
pub use epg::{
    ArchiveProgramme, EpgListing, archive_programmes, group_by_server_date, parse_epg_listings,
};
pub use login::{AccountStatus, LoginInfo, ServerInfo, UserInfo};
pub use streams::{Category, CategoryId, LiveStream, Series, VodStream};

/// Serialisation helpers that mirror the lenient deserialisers.
pub(crate) mod ser {
    use chrono::{DateTime, Utc};
    use serde::Serializer;

    /// `Option<DateTime<Utc>>` → `Option<i64>` epoch seconds.
    #[allow(clippy::ref_option)] // signature imposed by `serialize_with`
    pub(crate) fn opt_timestamp<S: Serializer>(
        value: &Option<DateTime<Utc>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(dt) => s.serialize_some(&dt.timestamp()),
            None => s.serialize_none(),
        }
    }
}
