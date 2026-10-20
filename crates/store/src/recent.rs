//! Recently watched live channels, for quick zapping and a "last channel" key.
//!
//! Each profile keeps at most [`StoreOptions::recent_live_limit`](crate::StoreOptions)
//! channels (default 50); older ones are trimmed on insert.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::{Result, map_fk};
use crate::favourites::ItemSnapshot;
use crate::ids::ProfileId;
use crate::text::validate_item_id;
use crate::time::Timestamp;

/// A recently watched live channel.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RecentChannel {
    /// Live `stream_id` (or the app's stable M3U id).
    pub stream_id: String,
    /// When it was last tuned to.
    pub last_watched: Timestamp,
    /// Display snapshot.
    pub snapshot: ItemSnapshot,
}

fn recent_from_row(row: &Row<'_>) -> rusqlite::Result<RecentChannel> {
    Ok(RecentChannel {
        stream_id: row.get(0)?,
        last_watched: row.get(1)?,
        snapshot: ItemSnapshot {
            name: row.get(2)?,
            icon_url: row.get(3)?,
            category_id: row.get(4)?,
        },
    })
}

/// Recent-channel operations. Obtain with [`Store::recent_live`].
#[derive(Clone, Copy, Debug)]
pub struct RecentLive<'a> {
    pub(crate) store: &'a Store,
}

impl RecentLive<'_> {
    /// Records that a channel was tuned to now (moves it to the front).
    pub fn touch(
        &self,
        profile: ProfileId,
        stream_id: &str,
        snapshot: &ItemSnapshot,
    ) -> Result<()> {
        let stream_id = validate_item_id("stream_id", stream_id)?;
        let snap = snapshot.sanitized();
        let keep = i64::try_from(self.store.recent_live_limit).unwrap_or(i64::MAX);
        let mut conn = self.store.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO recent_live (profile_id, stream_id, last_watched, seq, name, icon_url,
                                      category_id)
             VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(seq), 0) + 1 FROM recent_live),
                     ?4, ?5, ?6)
             ON CONFLICT (profile_id, stream_id) DO UPDATE SET
                 last_watched = excluded.last_watched,
                 seq = excluded.seq,
                 name = excluded.name,
                 icon_url = excluded.icon_url,
                 category_id = excluded.category_id",
            params![
                profile,
                stream_id,
                self.store.now(),
                snap.name,
                snap.icon_url,
                snap.category_id
            ],
        )
        .map_err(map_fk(profile))?;
        tx.execute(
            "DELETE FROM recent_live WHERE profile_id = ?1 AND seq NOT IN
                 (SELECT seq FROM recent_live WHERE profile_id = ?1 ORDER BY seq DESC LIMIT ?2)",
            params![profile, keep],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Most recent first.
    pub fn list(&self, profile: ProfileId, limit: usize) -> Result<Vec<RecentChannel>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT stream_id, last_watched, name, icon_url, category_id FROM recent_live
             WHERE profile_id = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![profile, i64::try_from(limit).unwrap_or(i64::MAX)],
            recent_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The channel before the current one (second most recent) — the "last channel" key.
    pub fn previous(&self, profile: ProfileId) -> Result<Option<RecentChannel>> {
        Ok(self
            .store
            .conn()
            .prepare_cached(
                "SELECT stream_id, last_watched, name, icon_url, category_id FROM recent_live
                 WHERE profile_id = ?1 ORDER BY seq DESC LIMIT 1 OFFSET 1",
            )?
            .query_row([profile], recent_from_row)
            .optional()?)
    }

    /// Forgets one channel. Returns `false` if it was not in the list.
    pub fn remove(&self, profile: ProfileId, stream_id: &str) -> Result<bool> {
        let n = self.store.conn().execute(
            "DELETE FROM recent_live WHERE profile_id = ?1 AND stream_id = ?2",
            params![profile, stream_id],
        )?;
        Ok(n > 0)
    }

    /// Clears the list. Returns the number of rows removed.
    pub fn clear(&self, profile: ProfileId) -> Result<usize> {
        Ok(self
            .store
            .conn()
            .execute("DELETE FROM recent_live WHERE profile_id = ?1", [profile])?)
    }
}
