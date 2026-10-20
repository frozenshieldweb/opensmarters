// SPDX-License-Identifier: GPL-3.0-or-later
//! Programme reminders set from the Guide: "tell me when this starts" (`notify`) or "switch
//! to the channel when it starts" (`switch`). One reminder per profile, channel and start
//! time; setting it again updates it.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::{Result, StoreError, map_fk};
use crate::ids::{ProfileId, sql_text_enum};
use crate::text::{
    MAX_TITLE_CHARS, MAX_URL_BYTES, sanitize_display, sanitize_opt, validate_item_id,
};
use crate::time::Timestamp;

/// Row id of a reminder.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReminderId(pub i64);

/// What happens when a reminder fires.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderAction {
    /// Show a prompt ("Starting now … Watch / Dismiss").
    #[default]
    Notify,
    /// Switch to the channel at the start time (after a short, cancellable countdown).
    Switch,
}

impl ReminderAction {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Switch => "switch",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "notify" => Some(Self::Notify),
            "switch" => Some(Self::Switch),
            _ => None,
        }
    }
}
sql_text_enum!(ReminderAction);

/// Input for [`Reminders::set`].
#[derive(Clone, Debug, PartialEq)]
pub struct NewReminder {
    /// Live channel key (Xtream `stream_id` or the app's M3U id). Never a URL.
    pub stream_id: String,
    /// Programme start.
    pub start_at: Timestamp,
    /// Programme end.
    pub end_at: Timestamp,
    /// Programme title (provider text: sanitised and capped).
    pub title: String,
    /// Channel name (provider text: sanitised and capped).
    pub channel_name: String,
    /// Channel logo URL (dropped if unusable).
    pub icon_url: Option<String>,
    /// What to do at the start time.
    pub action: ReminderAction,
}

/// A reminder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    /// Row id.
    pub id: ReminderId,
    /// Owning profile.
    pub profile: ProfileId,
    /// Live channel key.
    pub stream_id: String,
    /// Programme start.
    pub start_at: Timestamp,
    /// Programme end.
    pub end_at: Timestamp,
    /// Programme title.
    pub title: String,
    /// Channel name.
    pub channel_name: String,
    /// Channel logo URL.
    pub icon_url: Option<String>,
    /// What to do at the start time.
    pub action: ReminderAction,
    /// When it fired (`None` = still pending).
    pub fired_at: Option<Timestamp>,
    /// Created.
    pub created_at: Timestamp,
}

const COLUMNS: &str = "id, profile_id, stream_id, start_at, end_at, title, channel_name,
    icon_url, action, fired_at, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Reminder> {
    Ok(Reminder {
        id: ReminderId(row.get(0)?),
        profile: row.get(1)?,
        stream_id: row.get(2)?,
        start_at: row.get(3)?,
        end_at: row.get(4)?,
        title: row.get(5)?,
        channel_name: row.get(6)?,
        icon_url: row.get(7)?,
        action: row.get(8)?,
        fired_at: row.get(9)?,
        created_at: row.get(10)?,
    })
}

/// Reminder operations. Obtain with [`Store::reminders`].
#[derive(Clone, Copy, Debug)]
pub struct Reminders<'a> {
    pub(crate) store: &'a Store,
}

impl Reminders<'_> {
    /// Sets a reminder (or updates the one for the same channel and start time, which then
    /// fires again).
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] for a URL-like or empty channel id or an end before the
    /// start; [`StoreError::ProfileNotFound`].
    pub fn set(&self, profile: ProfileId, r: &NewReminder) -> Result<Reminder> {
        let stream_id = validate_item_id("stream_id", &r.stream_id)?;
        if r.end_at <= r.start_at {
            return Err(StoreError::invalid("end_at", "must be after the start"));
        }
        {
            let conn = self.store.conn();
            conn.execute(
                "INSERT INTO reminders (profile_id, stream_id, start_at, end_at, title,
                                        channel_name, icon_url, action, fired_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9)
                 ON CONFLICT (profile_id, stream_id, start_at) DO UPDATE SET
                     end_at = excluded.end_at,
                     title = excluded.title,
                     channel_name = excluded.channel_name,
                     icon_url = excluded.icon_url,
                     action = excluded.action,
                     fired_at = NULL",
                params![
                    profile,
                    stream_id,
                    r.start_at,
                    r.end_at,
                    sanitize_display(&r.title, MAX_TITLE_CHARS),
                    sanitize_display(&r.channel_name, MAX_TITLE_CHARS),
                    sanitize_opt(r.icon_url.as_deref(), MAX_URL_BYTES),
                    r.action,
                    self.store.now(),
                ],
            )
            .map_err(map_fk(profile))?;
        }
        self.find(profile, stream_id, r.start_at)?
            .ok_or_else(|| StoreError::invalid("id", "reminder vanished after insert"))
    }

    /// The reminder for a channel's programme, if set.
    pub fn find(
        &self,
        profile: ProfileId,
        stream_id: &str,
        start_at: Timestamp,
    ) -> Result<Option<Reminder>> {
        Ok(self
            .store
            .conn()
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM reminders
                 WHERE profile_id = ?1 AND stream_id = ?2 AND start_at = ?3"
            ))?
            .query_row(params![profile, stream_id, start_at], from_row)
            .optional()?)
    }

    /// Every reminder of a profile, earliest start first.
    pub fn list(&self, profile: ProfileId) -> Result<Vec<Reminder>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM reminders WHERE profile_id = ?1 ORDER BY start_at, id"
        ))?;
        let rows = stmt.query_map([profile], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Reminders that have not fired and whose programme has not ended at `now`, earliest
    /// start first.
    pub fn pending(&self, profile: ProfileId, now: Timestamp) -> Result<Vec<Reminder>> {
        let conn = self.store.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM reminders
             WHERE profile_id = ?1 AND fired_at IS NULL AND end_at > ?2
             ORDER BY start_at, id"
        ))?;
        let rows = stmt.query_map(params![profile, now], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Marks a reminder as fired. Returns `false` if it does not exist or already fired.
    pub fn mark_fired(&self, id: ReminderId) -> Result<bool> {
        let n = self.store.conn().execute(
            "UPDATE reminders SET fired_at = ?1 WHERE id = ?2 AND fired_at IS NULL",
            params![self.store.now(), id.0],
        )?;
        Ok(n > 0)
    }

    /// Changes what a reminder does.
    pub fn set_action(&self, id: ReminderId, action: ReminderAction) -> Result<bool> {
        let n = self.store.conn().execute(
            "UPDATE reminders SET action = ?1 WHERE id = ?2",
            params![action, id.0],
        )?;
        Ok(n > 0)
    }

    /// Deletes a reminder. Returns `false` if it did not exist.
    pub fn remove(&self, id: ReminderId) -> Result<bool> {
        let n = self
            .store
            .conn()
            .execute("DELETE FROM reminders WHERE id = ?1", [id.0])?;
        Ok(n > 0)
    }

    /// Deletes reminders whose programme ended before `before`. Returns how many.
    pub fn prune(&self, profile: ProfileId, before: Timestamp) -> Result<usize> {
        Ok(self.store.conn().execute(
            "DELETE FROM reminders WHERE profile_id = ?1 AND end_at < ?2",
            params![profile, before],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_round_trip() {
        for a in [ReminderAction::Notify, ReminderAction::Switch] {
            assert_eq!(ReminderAction::parse(a.as_str()), Some(a));
        }
        assert_eq!(ReminderAction::parse("nope"), None);
    }
}
