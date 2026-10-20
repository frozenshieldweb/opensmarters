//! Identifiers and small shared enums.

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier of a profile (account). A random UUID v4, never derived from the
/// server URL or credentials.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(Uuid);

impl ProfileId {
    /// A fresh random id.
    pub fn new_random() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an existing UUID.
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl fmt::Debug for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProfileId({})", self.0.hyphenated())
    }
}

impl FromStr for ProfileId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

impl ToSql for ProfileId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.hyphenated().to_string()))
    }
}

impl FromSql for ProfileId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

/// The three content sections of a provider. Used by favourites and category locks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// Live TV channels (`stream_id`).
    Live,
    /// Movies / VOD (`stream_id`).
    Movie,
    /// Series (`series_id`).
    Series,
}

impl ContentKind {
    /// All variants.
    pub const ALL: [Self; 3] = [Self::Live, Self::Movie, Self::Series];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Movie => "movie",
            Self::Series => "series",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "live" => Some(Self::Live),
            "movie" => Some(Self::Movie),
            "series" => Some(Self::Series),
            _ => None,
        }
    }
}

/// Implements `ToSql`/`FromSql` for a small enum stored as TEXT via `as_str`/`parse`.
macro_rules! sql_text_enum {
    ($ty:ty) => {
        impl rusqlite::types::ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
            }
        }

        impl rusqlite::types::FromSql for $ty {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let s = value.as_str()?;
                <$ty>::parse(s).ok_or(rusqlite::types::FromSqlError::InvalidType)
            }
        }
    };
}
pub(crate) use sql_text_enum;

sql_text_enum!(ContentKind);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_id_round_trips_as_text() {
        let id = ProfileId::new_random();
        let parsed: ProfileId = id
            .to_string()
            .parse()
            .ok()
            .unwrap_or(ProfileId::new_random());
        assert_eq!(parsed, id);
        assert!(format!("{id:?}").starts_with("ProfileId("));
    }

    #[test]
    fn content_kind_strings() {
        for k in ContentKind::ALL {
            assert_eq!(ContentKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ContentKind::parse("LIVE"), None);
    }
}
