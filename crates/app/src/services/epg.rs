// SPDX-License-Identifier: GPL-3.0-or-later
//! Now/next EPG lookups for lists (Live TV, Home's "Live now", Favourites, Search, the
//! player banner).
//!
//! There is **one** EPG cache per session: the guide's bounded row cache
//! ([`super::session::guide`]). [`super::Session::epg`] / [`super::Session::now_next`]
//! read it and, on a miss, queue a short "now/next" request in the guide's bounded
//! queue (at most [`super::session::guide::ROW_FETCHES`] requests at a time, most
//! recently shown rows first, requests for rows that scrolled away dropped). XMLTV
//! profiles read the session's XMLTV guide (no request per channel).
//!
//! Lookups never block. Expired rows keep showing their old programmes while they are
//! refreshed (stale-while-revalidate); failed rows are retried after [`RETRY_AFTER`].

use std::sync::Arc;
use std::time::Duration;

use super::catalog::Programme;
use super::session::guide::EpgRow;

/// How long a channel's EPG stays fresh.
pub const EPG_TTL: Duration = super::session::guide::ROW_TTL;
/// When to retry a failed fetch.
pub const RETRY_AFTER: Duration = super::session::guide::ROW_RETRY;
/// Concurrent EPG requests to the provider (shared with the Guide).
pub const MAX_CONCURRENT_FETCHES: usize = super::session::guide::ROW_FETCHES;

/// Result of a non-blocking lookup.
#[derive(Debug, Clone)]
pub enum EpgState {
    /// Being fetched (show a skeleton line).
    Loading,
    /// The provider has no guide for this channel.
    Missing,
    /// The schedule (programmes sorted by start; possibly stale while it refreshes).
    Ready(Arc<EpgRow>),
}

impl EpgState {
    /// The programmes, if ready.
    pub fn programmes(&self) -> Option<&[Programme]> {
        match self {
            EpgState::Ready(r) => Some(&r.programmes),
            _ => None,
        }
    }
}
